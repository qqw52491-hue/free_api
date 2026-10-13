//! 单条指令的同步命令执行内核。

use crate::agent::mcp::PluginRegistry;
use crate::agent::mcp_catalog::invalidate_plugin;
use crate::agent::plan_tree_dispatch::{
    run_plan_tree_sync, validate_controlled_parallel_batch, MAX_CONTROLLED_PARALLEL_BRANCHES,
};
use crate::agent::retrieval;
use crate::agent::task_store;
use crate::agent::types::{AgentInstruction, DispatchResult};
use crate::agent::utils::{run_osascript, run_shell};
use crate::agent::web_search;
use crate::db::DbState;
use chrono::Utc;
use reqwest::Url;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

use super::*;

/// MCP 出站路径归一：外部进程不认识宿主侧的 "tmp/" 快捷路由（会按自己的 cwd
/// 解析到错误位置），也不该猜"相对谁"。把 path 类参数值翻译成绝对路径再透传：
///   tmp/x 或 ./tmp/x  → 会话专属临时目录；
///   其他相对路径形态   → 当前工作区根（自由会话回落到会话 tmp，与内部工具一致）；
///   绝对路径/URL/纯标识符（无分隔符无扩展名）→ 原样透传，不碰契约。
pub(crate) fn looks_like_path_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k.contains("path")
        || k.contains("file")
        || k.contains("dir")
        || k.contains("folder")
        || k.contains("output")
        || k.contains("save")
        || k.contains("dest")
        || matches!(k.as_str(), "src" | "source" | "target" | "root" | "workspace" | "filename")
}

pub(crate) fn route_path_value(raw: &str, session_tmp: &std::path::Path, effective_ws: &std::path::Path) -> Option<String> {
    let v = raw.trim();
    // 显式作用域 URI（宿主私有 scheme，非真 URL——只做前缀剥头，容忍 2/3 斜杠）：
    //   session-tmp://x → 会话临时目录；workspace://x → 当前工作区根。
    // 放在 :// 通用放行之前，我们自己的 scheme 必须被翻译。
    for (scheme, root) in [("session-tmp://", session_tmp), ("workspace://", effective_ws)] {
        if let Some(rest) = v.strip_prefix(scheme) {
            let rel = rest.trim_start_matches('/');
            if rel.is_empty() || rel.split('/').any(|seg| seg == "..") {
                return None;
            }
            return Some(root.join(rel).to_string_lossy().into_owned());
        }
    }
    if v.is_empty() || v.contains("://") || v.starts_with('~') {
        return None;
    }
    // Windows 盘符/UNC 已是绝对形态
    if v.len() >= 2 && v.as_bytes()[1] == b':' && v.as_bytes()[0].is_ascii_alphabetic() {
        return None;
    }
    if let Some(rel) = v.strip_prefix("./tmp/").or_else(|| v.strip_prefix("tmp/")) {
        if rel.is_empty() || rel.split('/').any(|seg| seg == "..") {
            return None;
        }
        return Some(session_tmp.join(rel).to_string_lossy().into_owned());
    }
    let v = v.strip_prefix("./").unwrap_or(v);
    let p = std::path::Path::new(v);
    if p.is_absolute() {
        return None;
    }
    // 相对形态必须像路径（含分隔符或扩展名）；纯标识符（如 "presentation_1"）不动
    if !v.contains('/') && !v.contains('\\') && p.extension().is_none() {
        return None;
    }
    if v.split(['/', '\\']).any(|seg| seg == "..") {
        return None;
    }
    Some(effective_ws.join(v).to_string_lossy().into_owned())
}

pub(crate) fn normalize_mcp_path_params(
    v: &mut serde_json::Value,
    session_tmp: &std::path::Path,
    effective_ws: &std::path::Path,
) -> usize {
    match v {
        serde_json::Value::Object(map) => {
            let mut n = 0;
            for (key, val) in map.iter_mut() {
                let rewritten = match val {
                    serde_json::Value::String(s) if looks_like_path_key(key) => {
                        route_path_value(s, session_tmp, effective_ws)
                    }
                    _ => None,
                };
                if let Some(new_val) = rewritten {
                    if let serde_json::Value::String(s) = val {
                        *s = new_val;
                    }
                    n += 1;
                } else {
                    n += normalize_mcp_path_params(val, session_tmp, effective_ws);
                }
            }
            n
        }
        serde_json::Value::Array(items) => items
            .iter_mut()
            .map(|i| normalize_mcp_path_params(i, session_tmp, effective_ws))
            .sum(),
        _ => 0,
    }
}

/// 带当前 agent 的一次性图片通道；普通工具仍进入原同步路由。
pub(crate) fn execute_command_with_images(
    db: &DbState,
    session_id: &str,
    tool: &str,
    action: &str,
    params: &serde_json::Value,
    registry: &PluginRegistry,
    workspace_root: Option<&std::path::Path>,
    pending: Option<&crate::agent::tools::view_image::PendingImageQueue>,
) -> DispatchResult {
    if tool.eq_ignore_ascii_case("view_image") || action.eq_ignore_ascii_case("view_image") {
        if let Some(pending) = pending {
            return crate::agent::tools::view_image::dispatch(params, workspace_root, session_id, pending);
        }
    }
    execute_command_inner(db, session_id, tool, action, params, registry, workspace_root)
}

pub(crate) fn execute_command_inner(
    db: &DbState,
    session_id: &str,
    tool: &str,
    action: &str,
    params: &serde_json::Value,
    registry: &PluginRegistry,
    workspace_root: Option<&std::path::Path>,
) -> DispatchResult {
    // 单步、流水线及并行叶子共用纯参数预检，先报错再做原有安全裁决。
    if let Some(result) = crate::agent::tools::input_validation::native_rejection(tool, action, params) {
        return result;
    }
    // 0. 宿主层安全防护：
    // 项目专属会话禁止切换工作区（锁定在项目根目录，防止越界串扰）
    let lower_tool = tool.to_ascii_lowercase();
    let lower_action = action.to_ascii_lowercase();
    // 核心内置工具的 action 走归一小写（模型偶发 "Read"/"Write" 首字母大写
    // 不影响内置路由）；MCP 插件路径保留 action 原样——它承载子工具原名。
    let core_action = lower_action.as_str();
    if lower_tool.contains("switch_workspace")
        || lower_tool.contains("switchworkspace")
        || lower_tool.contains("set_workspace")
        || lower_tool.contains("change_workspace")
        || lower_action.contains("switch_workspace")
        || lower_action.contains("switchworkspace")
        || lower_action.contains("set_workspace")
    {
        if is_project_session(db, session_id) {
            return DispatchResult { view: None,
                stdout: String::new(),
                stderr: "❌ [宿主安全保护] 当前为专属项目会话，工作区已锁定在项目根目录，禁止越界切换。如需操作外部项目，请在通用临时会话中操作。".to_string(),
                success: false,
                route: "host_rejection:project_session_workspace_locked".to_string(),
            };
        }
        // 通用临时会话：放行给 MCP 插件进行工作区切换！
    }
    // 1. 核心原生工具 A: get_step_output (精准取回截断历史原文)
    if tool.eq_ignore_ascii_case("get_step_output") {
        return task_store::dispatch(db, session_id, "get_step_output", params);
    }

    // 2. 核心原生工具 B: task_store (会话任务数据区结构化操作)
    if tool.eq_ignore_ascii_case("task_store") {
        let final_action = if action.is_empty() {
            params
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("list_resources")
        } else {
            core_action
        };
        return task_store::dispatch(db, session_id, final_action, params);
    }

    // 2.5 项目受管资源：服务端按 session→project→resource 三重边界复核。
    if tool.eq_ignore_ascii_case("project_resources")
        || tool.eq_ignore_ascii_case("register_project_resource")
    {
        let final_action = if tool.eq_ignore_ascii_case("register_project_resource") {
            "register_resource"
        } else {
            core_action
        };
        return dispatch_project_resources(db, session_id, final_action, params);
    }

    // 3. 核心原生工具 C: search (跨执行日志 FTS5 与实时代码 ripgrep 联邦检索)
    if tool.eq_ignore_ascii_case("search") {
        return retrieval::dispatch_search(db, session_id, params, workspace_root);
    }

    // 3.1 核心原生工具 C2: capability_search (内置/MCP/Skill/指令/角色卡统一能力检索；
    //     目录截断后的发现层——模型不知确切名称时先搜这里再 next_tool_hint 精确加载)
    if tool.eq_ignore_ascii_case("capability_search")
        || tool.eq_ignore_ascii_case("search_capabilities")
    {
        return crate::agent::capability_index::search(db, registry, workspace_root, params);
    }

    // 3.5 核心原生工具 D: web_search / web_fetch (互联网全网检索与网页正文 Markdown 提取)
    if tool.eq_ignore_ascii_case("web_search") || tool.eq_ignore_ascii_case("web_fetch") {
        return web_search::dispatch(tool, params);
    }

    // 3.6 核心原生工具 E: model_manager (自主管理模型规格与渠道绑定，支持增删改查与安全确认)
    if tool.eq_ignore_ascii_case("model_manager") {
        return crate::agent::tools::model_manager::dispatch(db, core_action, params);
    }

    // 3.7 核心原生工具 F: mcp_manager (MCP 外部插件接入、批量导入与能力卡维护)
    if tool.eq_ignore_ascii_case("mcp_manager") {
        let act = action.to_ascii_lowercase();
        // 变更类危险动作（add_http, delete, toggle, quick_connect, install_ref）必须经过用户明确确认
        if act.contains("add") || act.contains("delete") || act.contains("toggle") || act.contains("connect") || act.contains("install") {
            let user_approved = params
                .get("user_confirmed")
                .and_then(|v| v.as_bool())
                .or_else(|| params.get("confirm").and_then(|v| v.as_bool()))
                .unwrap_or(false);
            if !user_approved {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: "❌ [权限安全拦截] 严禁在执行代码或排错任务中擅自调用 mcp_manager 添加、修改或重连 MCP 插件！\n💡 关键提醒：当前已连接的 MCP 工具（如 remote-hub 下的 read_text_file, edit_file, ripgrep_search 等）已经全部就绪且立即可用！请直接以 {\"tool\": \"remote-hub/具体工具名\", \"command\": {...}} 格式调用，绝不需要也不允许调用 mcp_manager 或派发空步骤去‘加载’或‘重连’！".to_string(),
                    success: false,
                    route: "mcp_manager:permission_denied".to_string(),
                };
            }
        }
        return crate::agent::tools::mcp_manager::dispatch(db, core_action, params);
    }

    // 3.8 核心原生工具 G: skill_manager (工作区自定义技能与提示词库维护)
    if tool.eq_ignore_ascii_case("skill_manager") {
        return crate::agent::tools::skill_manager::dispatch(db, core_action, params);
    }
    // 3.9 核心原生工具 H: role_manager (专精角色池维护——角色卡=人设+模型绑定+作用域)
    if tool.eq_ignore_ascii_case("role_manager") {
        return crate::agent::tools::role_manager::dispatch(db, core_action, params);
    }

    // 3.9 核心原生工具 H: schedule_manager (定时任务管理——对话内直接建计划)
    if tool.eq_ignore_ascii_case("schedule_manager") {
        return crate::agent::tools::schedule_manager::dispatch(db, core_action, params);
    }

    // 3.10 核心原生工具 I: project_manager (项目工作区管理)
    if tool.eq_ignore_ascii_case("project_manager") {
        return crate::agent::tools::project_manager::dispatch(db, core_action, params);
    }

    // 3.11 核心原生工具 J: system_info (本机运行状态)
    if tool.eq_ignore_ascii_case("system_info") {
        return crate::agent::tools::system_info::dispatch(db, core_action, params);
    }

    // 3.12 核心原生工具 K: wechat_manager (微信通道状态与主动知会)
    if tool.eq_ignore_ascii_case("wechat_manager") {
        return crate::agent::tools::wechat_manager::dispatch(db, core_action, params);
    }

    // 3.9 核心原生工具 H: artifact_manager (受管产物物理写入、元数据登记与产物账本管理)
    if tool.eq_ignore_ascii_case("artifact_manager") || tool.eq_ignore_ascii_case("artifact_store")
    {
        return crate::agent::artifact_manager::dispatch(db, session_id, core_action, params);
    }

    // 3.13 核心原生工具 L: local_file (工作区与临时目录双白名单本地文件读写/精准局部替换)
    if tool.eq_ignore_ascii_case("local_file") {
        return crate::agent::tools::local_file::dispatch(db, session_id, core_action, params, workspace_root);
    }

    // 3.14 核心原生工具 M: local_shell (工作区物理边界与超时保护本地命令行执行)
    if tool.eq_ignore_ascii_case("local_shell") {
        return crate::agent::tools::local_shell::dispatch(db, session_id, core_action, params, workspace_root);
    }

    // 4. 外部 MCP 插件工具 (支持 "插件名/工具名" 标准格式 或 "插件名" + action)
    let (plugin_name, mut tool_name) = if tool.contains('/') {
        let parts: Vec<&str> = tool.split('/').collect();
        (parts[0].to_string(), parts[1].to_string())
    } else {
        (tool.to_string(), action.to_string())
    };

    if let Some(plugin) = registry.get(&plugin_name) {
        // 容错与大小写纠偏：通过 cached_tools 查找真实注册的原生工具名称，
        // 避免因模型输出全小写或首字母大写不一致引发 FastMCP/下游服务端报 Unknown tool。
        let cached = plugin.last_known_tools();
        if let Some(matched) = cached.iter().find(|t| t.name.eq_ignore_ascii_case(&tool_name)) {
            if matched.name != tool_name {
                println!("[mcp] 纠偏工具名大小写: '{}' -> '{}' (插件: {})", tool_name, matched.name, plugin_name);
                tool_name = matched.name.clone();
            }
        }
        if mcp_service_requires_concrete_tool(tool, action) {
            return DispatchResult { view: None,
                stdout: String::new(),
                stderr: format!(
                    "❌ [工具契约] MCP 服务‘{}’是服务名称而非具体工具。\n💡 正确调用示范: {{\"tool\": \"{}/具体工具名\", \"command\": {{...}}}}（例如: \"{}/read_text_file\" 或 \"{}/ripgrep_search\"）。请勿遗漏斜杠与子工具名。",
                    plugin_name, plugin_name, plugin_name, plugin_name
                ),
                success: false,
                route: "host_rejection:mcp_service_requires_subtool".to_string(),
            };
        }
        // MCP 子工具参数必须逐字按模型提供的契约透传；宿主不得补全、别名映射或截断。
        // 唯一例外：tmp/ 与裸相对路径是宿主侧的路由约定——外部进程按其自身 cwd
        // 解析会落到错误位置，这里统一翻译成绝对路径（模型写相对或绝对形式都正确）。
        let mut clean_params = params.clone();
        {
            let session_tmp = crate::agent::path_guard::get_session_tmp_dir(session_id);
            let effective_ws = workspace_root
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| session_tmp.clone());
            let rewritten =
                normalize_mcp_path_params(&mut clean_params, &session_tmp, &effective_ws);
            if rewritten > 0 {
                println!(
                    "[mcp] 路径参数归一 {} 处（tmp/→{}, 相对→{}）",
                    rewritten,
                    session_tmp.display(),
                    effective_ws.display()
                );
            }
        }
        println!(
            "执行动作调用外部插件: {}/{} (会话: {}, 参数: {:?})",
            plugin_name, tool_name, session_id, clean_params
        );
        let call_res = if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.block_on(plugin.call_tool_for_session(
                Some(session_id),
                &tool_name,
                clean_params.clone(),
            ))
        } else {
            futures::executor::block_on(plugin.call_tool_for_session(
                Some(session_id),
                &tool_name,
                clean_params.clone(),
            ))
        };
        match call_res {
            Ok(out) => {
                let is_error = out
                    .get("isError")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let content = format_mcp_model_output(&out);
                let view = Some(mcp_raw_result_view(
                    &plugin_name,
                    &tool_name,
                    &clean_params,
                    &out,
                ));
                if is_error {
                    return DispatchResult { view,
                        stdout: String::new(),
                        stderr: content,
                        success: false,
                        route: format!("plugin:{}/{}", plugin_name, tool_name),
                    };
                } else {
                    return DispatchResult { view,
                        stdout: content,
                        stderr: String::new(),
                        success: true,
                        route: format!("plugin:{}/{}", plugin_name, tool_name),
                    };
                }
            }
            Err(e) => {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("插件调用异常: {:?}", e),
                    success: false,
                    route: format!("plugin:{}/{}", plugin_name, tool_name),
                };
            }
        }
    }

    // 5. 针对将 parallel 误写为独立工具的精准纠错
    if tool == "parallel" || core_action == "parallel" {
        return DispatchResult { view: None,
            stdout: String::new(),
            stderr: "❌ 'parallel' 不是独立工具名，而是顶层并行执行模式字段。\n正确格式示例：{\"plan_title\": \"...\", \"parallel_reason\": \"...\", \"parallel\": [{\"tool\": \"工具1\", \"command\": {...}}, {\"tool\": \"工具2\", \"command\": {...}}]}。\n请重新按标准 JSON 格式输出！".to_string(),
            success: false,
            route: "unknown".to_string(),
        };
    }

    // 6. 未知工具或格式错误报错（协助模型无硬编码纠错）。
    // 小目录全列便于直接纠错；大目录只给"插件名+数量"，否则一条报错
    // 会把整个工具目录重新灌进上下文，绕过菜单的全局限流。
    const UNKNOWN_TOOL_FULL_LIST_LIMIT: usize = 40;
    let mut mcp_tool_hints = Vec::new();
    let mut mcp_tool_count = 0usize;
    for (p_name, p_client) in &registry.clients {
        let tools = crate::agent::mcp_catalog::cached_tools(&p_client.config)
            .unwrap_or_else(|| p_client.last_known_tools());
        mcp_tool_count += tools.len();
        if !tools.is_empty() {
            // 插件内工具名同样排序：缓存目录顺序随刷新漂移会让同一错误的
            // 提示文本字节不稳定，破坏可复现性与前缀缓存复用。
            let mut names: Vec<String> = tools
                .iter()
                .map(|t| format!("{}/{}", p_name, t.name))
                .collect();
            names.sort();
            mcp_tool_hints.push((p_name.clone(), names));
        }
    }
    mcp_tool_hints.sort_by(|a, b| a.0.cmp(&b.0));

    let mcp_guide = if mcp_tool_hints.is_empty() {
        "（当前未连接任何 MCP 插件）".to_string()
    } else if mcp_tool_count <= UNKNOWN_TOOL_FULL_LIST_LIMIT {
        let lines = mcp_tool_hints
            .iter()
            .map(|(name, tools)| format!("- {}: {}", name, tools.join(", ")))
            .collect::<Vec<_>>();
        format!("\n当前已连接的 MCP 插件及可用工具：\n{}", lines.join("\n"))
    } else {
        let lines = mcp_tool_hints
            .iter()
            .map(|(name, tools)| format!("- {}（{} 个工具）", name, tools.len()))
            .collect::<Vec<_>>();
        format!(
            "\n当前已连接的 MCP 插件（共 {} 个工具，清单过大不逐项列出）：\n{}\n用 capability_search 检索正确工具名，或 next_tool_hint: \"插件名/工具名\" 取回完整 Schema。",
            mcp_tool_count,
            lines.join("\n")
        )
    };

    let builtin_list = crate::agent::prompt_assembly::build_builtin_tools_compact_list();
    // 顶层独立动作被误当工具调用：模型熟悉的 {"tool","action","params"} 形状
    // 不适用于方案提交/交付预检这类状态转换动作，报错须点破格式而非只说"拼写错"。
    let standalone_hint = if matches!(tool, "submit_plan" | "prepare_finish") {
        format!(
            "\n提示：'{}' 是顶层独立动作而非工具——请单独输出 {{\"{}\": {{...}}}}，不要附带 tool/action/params。",
            tool, tool
        )
    } else {
        String::new()
    };
    DispatchResult { view: None,
        stdout: String::new(),
        stderr: format!(
            "❌ 未知工具 tool='{}' (action='{}')。请检查拼写！{}\n可用原生内置工具：{}；{}\n外部 MCP 工具调用格式为：{{\"tool\": \"插件名/工具名\", \"command\": {{...}}}}。",
            tool,
            action,
            standalone_hint,
            builtin_list,
            mcp_guide
        ),
        success: false,
        route: "unknown".to_string(),
    }
}

pub(crate) const MCP_RAW_RESULTS_KIND: &str = "mcp_raw_results";

pub(crate) fn mcp_raw_result_view(
    plugin: &str,
    tool: &str,
    arguments: &serde_json::Value,
    result: &serde_json::Value,
) -> serde_json::Value {
    json!({
        "kind": MCP_RAW_RESULTS_KIND,
        "items": [{
            "plugin": plugin,
            "tool": tool,
            "arguments": arguments.clone(),
            "result": result.clone(),
        }],
    })
}

pub(crate) fn is_mcp_raw_results_view(view: &serde_json::Value) -> bool {
    view.get("kind").and_then(|k| k.as_str()) == Some(MCP_RAW_RESULTS_KIND)
        && view.get("items").is_some_and(|i| i.is_array())
}

pub(crate) fn append_mcp_raw_results(
    target: &mut Vec<serde_json::Value>,
    view: Option<&serde_json::Value>,
) {
    let Some(view) = view else {
        return;
    };
    if !is_mcp_raw_results_view(view) {
        return;
    }
    if let Some(items) = view.get("items").and_then(|i| i.as_array()) {
        target.extend(items.iter().cloned());
    }
}

pub(crate) fn mcp_raw_results_view(items: Vec<serde_json::Value>) -> Option<serde_json::Value> {
    if items.is_empty() {
        return None;
    }
    Some(json!({
        "kind": MCP_RAW_RESULTS_KIND,
        "items": items,
    }))
}

pub(crate) fn take_mcp_durable_output(
    view: &mut Option<serde_json::Value>,
) -> Option<String> {
    let is_exact = view.as_ref().is_some_and(is_mcp_raw_results_view);
    if !is_exact {
        return None;
    }
    view.take().map(|v| {
        serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
    })
}

/// task_store 回查侧投影：持久化的是完整 sidecar，模型回读默认拿到可读形态。
/// raw=true 时调用方跳过本投影，原始 JSON（含 _meta/二进制）仍可完整取证。
pub(crate) fn project_mcp_durable_output(raw: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    if !is_mcp_raw_results_view(&value) {
        return None;
    }
    let items = value.get("items")?.as_array()?;
    let mut out = String::new();
    out.push_str(&format!("【MCP 证据包投影：{} 次调用】\n", items.len()));
    for (index, item) in items.iter().enumerate() {
        let plugin = item.get("plugin").and_then(|v| v.as_str()).unwrap_or("?");
        let tool = item.get("tool").and_then(|v| v.as_str()).unwrap_or("?");
        let args = item
            .get("arguments")
            .map(|a| a.to_string())
            .unwrap_or_else(|| "{}".to_string());
        out.push_str(&format!(
            "── #{} {}/{} ──\n参数: {}\n",
            index + 1,
            plugin,
            tool,
            args
        ));
        if let Some(result) = item.get("result") {
            out.push_str(&format_mcp_model_output(result));
        }
        out.push('\n');
    }
    out.push_str("（注：_meta、二进制负载与未投影字段仍完整保留在该 output_id 原始记录中；确需原始 JSON 时以参数 raw=true 重新回查。）");
    Some(out)
}

fn mcp_block_descriptor(block: &serde_json::Value, index: usize) -> String {
    let type_name = block
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("<missing>");
    format!(
        "[MCP 内容块 #{} 类型={}：完整原始块已持久化到本步骤 output_id]",
        index, type_name
    )
}

pub(crate) fn format_mcp_model_output(result: &serde_json::Value) -> String {
    if let Some(object) = result.as_object() {
        let simple_top_level = object
            .keys()
            .all(|key| key == "content" || key == "isError")
            && object
                .get("isError")
                .is_none_or(serde_json::Value::is_boolean);
        if simple_top_level {
            if let Some(blocks) = object.get("content").and_then(serde_json::Value::as_array) {
                if let [block] = blocks.as_slice() {
                    if let Some(block_object) = block.as_object() {
                        let simple_text_block = block_object
                            .keys()
                            .all(|key| key == "type" || key == "text")
                            && block_object
                                .get("type")
                                .is_none_or(|kind| kind.as_str() == Some("text"));
                        if simple_text_block {
                            if let Some(text) =
                                block_object.get("text").and_then(serde_json::Value::as_str)
                            {
                                return text.to_string();
                            }
                        }
                    }
                }
            }
        }
    }

    let mut pieces: Vec<String> = Vec::new();
    let mut has_text_payload = false;
    if let Some(blocks) = result.get("content").and_then(serde_json::Value::as_array) {
        for (index, block) in blocks.iter().enumerate() {
            let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or_default();
            match block_type {
                "text" => {
                    if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                        pieces.push(text.to_string());
                        has_text_payload = true;
                    } else {
                        pieces.push(mcp_block_descriptor(block, index));
                    }
                }
                "resource" => {
                    let resource = block.get("resource").cloned().unwrap_or_else(|| json!({}));
                    let uri = resource.get("uri").and_then(|v| v.as_str()).unwrap_or_default();
                    let mime = resource
                        .get("mimeType")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    if let Some(text) = resource.get("text").and_then(|v| v.as_str()) {
                        pieces.push(format!("[MCP resource uri={} mimeType={}]\n{}", uri, mime, text));
                        has_text_payload = true;
                    } else {
                        let blob_chars = resource
                            .get("blob")
                            .and_then(|v| v.as_str())
                            .map(|b| b.chars().count())
                            .unwrap_or(0);
                        pieces.push(format!(
                            "[MCP resource uri={} mimeType={} blob_chars={}：完整内容已持久化到本步骤 output_id]",
                            uri, mime, blob_chars
                        ));
                    }
                }
                "resource_link" => {
                    let field = |key: &str| {
                        block.get(key).and_then(|v| v.as_str()).unwrap_or_default()
                    };
                    pieces.push(format!(
                        "[MCP resource_link name={} title={} uri={} mimeType={} description={}]",
                        field("name"),
                        field("title"),
                        field("uri"),
                        field("mimeType"),
                        field("description")
                    ));
                }
                "image" | "audio" => {
                    let mime = block
                        .get("mimeType")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let data_chars = block
                        .get("data")
                        .and_then(|v| v.as_str())
                        .map(|d| d.chars().count())
                        .unwrap_or(0);
                    let annotations = if block.get("annotations").is_some() {
                        " annotations=present"
                    } else {
                        ""
                    };
                    pieces.push(format!(
                        "[MCP {} mimeType={} base64_chars={}{}：完整内容已持久化到本步骤 output_id]",
                        block_type, mime, data_chars, annotations
                    ));
                }
                _ => pieces.push(mcp_block_descriptor(block, index)),
            }
        }
    }
    if !has_text_payload {
        if let Some(structured) = result.get("structuredContent") {
            pieces.push(
                serde_json::to_string_pretty(structured).unwrap_or_else(|_| structured.to_string()),
            );
        }
    }
    if pieces.is_empty() {
        return "（MCP 工具未返回模型可见 content；完整原始结果已持久化到本步骤 output_id）".to_string();
    }
    pieces.join("\n\n")
}

#[tauri::command]
pub async fn execute_command(tool: String, command: String) -> Result<serde_json::Value, String> {
    let (stdout, stderr, success) = if tool == "osascript" {
        run_osascript(&command)
    } else {
        run_shell(&command)
    };
    Ok(json!({ "success": success, "stdout": stdout.trim(), "stderr": stderr.trim() }))
}

