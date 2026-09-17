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

/// 执行单条指令的内部路由逻辑（4大核心原生工具 + MCP 插件路由，彻底杜绝历史兼容包袱）
pub(crate) fn execute_command_inner(
    db: &DbState,
    session_id: &str,
    tool: &str,
    action: &str,
    params: &serde_json::Value,
    registry: &PluginRegistry,
    workspace_root: Option<&std::path::Path>,
) -> DispatchResult {
    // 0. 宿主层安全防护：
    // 项目专属会话禁止切换工作区（锁定在项目根目录，防止越界串扰）
    let lower_tool = tool.to_ascii_lowercase();
    let lower_action = action.to_ascii_lowercase();
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
            action
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
            action
        };
        return dispatch_project_resources(db, session_id, final_action, params);
    }

    // 3. 核心原生工具 C: search (跨执行日志 FTS5 与实时代码 ripgrep 联邦检索)
    if tool.eq_ignore_ascii_case("search") {
        return retrieval::dispatch_search(db, session_id, params, workspace_root);
    }

    // 3.5 核心原生工具 D: web_search / web_fetch (互联网全网检索与网页正文 Markdown 提取)
    if tool.eq_ignore_ascii_case("web_search") || tool.eq_ignore_ascii_case("web_fetch") {
        return web_search::dispatch(tool, params);
    }

    // 3.6 核心原生工具 E: model_manager (自主管理模型规格与渠道绑定，支持增删改查与安全确认)
    if tool.eq_ignore_ascii_case("model_manager") {
        return crate::agent::tools::model_manager::dispatch(db, action, params);
    }

    // 3.7 核心原生工具 F: mcp_manager (MCP 外部插件接入、批量导入与能力卡维护)
    if tool.eq_ignore_ascii_case("mcp_manager") {
        let act = action.to_ascii_lowercase();
        // 变更类危险动作（add_http, delete, toggle, quick_connect）必须经过用户明确确认
        if act.contains("add") || act.contains("delete") || act.contains("toggle") || act.contains("connect") {
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
        return crate::agent::tools::mcp_manager::dispatch(db, action, params);
    }

    // 3.8 核心原生工具 G: skill_manager (工作区自定义技能与提示词库维护)
    if tool.eq_ignore_ascii_case("skill_manager") {
        return crate::agent::tools::skill_manager::dispatch(db, action, params);
    }
    // 3.9 核心原生工具 H: role_manager (专精角色池维护——角色卡=人设+模型绑定+作用域)
    if tool.eq_ignore_ascii_case("role_manager") {
        return crate::agent::tools::role_manager::dispatch(db, action, params);
    }

    // 3.9 核心原生工具 H: schedule_manager (定时任务管理——对话内直接建计划)
    if tool.eq_ignore_ascii_case("schedule_manager") {
        return crate::agent::tools::schedule_manager::dispatch(db, action, params);
    }

    // 3.10 核心原生工具 I: project_manager (项目工作区管理)
    if tool.eq_ignore_ascii_case("project_manager") {
        return crate::agent::tools::project_manager::dispatch(db, action, params);
    }

    // 3.11 核心原生工具 J: system_info (本机运行状态)
    if tool.eq_ignore_ascii_case("system_info") {
        return crate::agent::tools::system_info::dispatch(db, action, params);
    }

    // 3.12 核心原生工具 K: wechat_manager (微信通道状态与主动知会)
    if tool.eq_ignore_ascii_case("wechat_manager") {
        return crate::agent::tools::wechat_manager::dispatch(db, action, params);
    }

    // 3.9 核心原生工具 H: artifact_manager (受管产物物理写入、元数据登记与产物账本管理)
    if tool.eq_ignore_ascii_case("artifact_manager") || tool.eq_ignore_ascii_case("artifact_store")
    {
        return crate::agent::artifact_manager::dispatch(db, session_id, action, params);
    }

    // 3.13 核心原生工具 L: local_file (工作区与临时目录双白名单本地文件读写/精准局部替换)
    if tool.eq_ignore_ascii_case("local_file") {
        return crate::agent::tools::local_file::dispatch(db, session_id, action, params, workspace_root);
    }

    // 3.14 核心原生工具 M: local_shell (工作区物理边界与超时保护本地命令行执行)
    if tool.eq_ignore_ascii_case("local_shell") {
        return crate::agent::tools::local_shell::dispatch(db, session_id, action, params, workspace_root);
    }

    // 4. 外部 MCP 插件工具 (支持 "插件名/工具名" 标准格式 或 "插件名" + action)
    let (plugin_name, tool_name) = if tool.contains('/') {
        let parts: Vec<&str> = tool.split('/').collect();
        (parts[0].to_string(), parts[1].to_string())
    } else {
        (tool.to_string(), action.to_string())
    };

    if let Some(plugin) = registry.get(&plugin_name) {
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
        let clean_params = params.clone();
        println!(
            "执行动作调用外部插件: {}/{} (会话: {}, 参数: {:?})",
            plugin_name, tool_name, session_id, clean_params
        );
        let call_res = if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.block_on(plugin.call_tool_for_session(
                Some(session_id),
                &tool_name,
                clean_params,
            ))
        } else {
            futures::executor::block_on(plugin.call_tool_for_session(
                Some(session_id),
                &tool_name,
                clean_params,
            ))
        };
        match call_res {
            Ok(out) => {
                let is_error = out
                    .get("isError")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let content = out
                    .get("content")
                    .and_then(|v| v.as_array())
                    .and_then(|arr| arr.get(0))
                    .and_then(|m| m.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if is_error {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: content,
                        success: false,
                        route: format!("plugin:{}/{}", plugin_name, tool_name),
                    };
                } else {
                    return DispatchResult { view: None,
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
    if tool == "parallel" || action == "parallel" {
        return DispatchResult { view: None,
            stdout: String::new(),
            stderr: "❌ 'parallel' 不是独立工具名，而是顶层并行执行模式字段。\n正确格式示例：{\"plan_title\": \"...\", \"parallel_reason\": \"...\", \"parallel\": [{\"tool\": \"工具1\", \"command\": {...}}, {\"tool\": \"工具2\", \"command\": {...}}]}。\n请重新按标准 JSON 格式输出！".to_string(),
            success: false,
            route: "unknown".to_string(),
        };
    }

    // 6. 未知工具或格式错误报错（动态生成当前所有已连接 MCP 插件及子工具清单，协助模型无硬编码纠错）
    let mut mcp_tool_hints = Vec::new();
    for (p_name, p_client) in &registry.clients {
        let tools = crate::agent::mcp_catalog::cached_tools(&p_client.config)
            .unwrap_or_else(|| p_client.last_known_tools());
        let tool_names = tools
            .iter()
            .map(|t| format!("{}/{}", p_name, t.name))
            .collect::<Vec<_>>();
        if !tool_names.is_empty() {
            mcp_tool_hints.push(format!("- {}: {}", p_name, tool_names.join(", ")));
        }
    }

    let mcp_guide = if mcp_tool_hints.is_empty() {
        "（当前未连接任何 MCP 插件）".to_string()
    } else {
        format!(
            "\n当前已连接的 MCP 插件及可用工具：\n{}",
            mcp_tool_hints.join("\n")
        )
    };

    let builtin_list = crate::agent::prompt_assembly::build_builtin_tools_compact_list();
    DispatchResult { view: None,
        stdout: String::new(),
        stderr: format!(
            "❌ 未知工具 tool='{}' (action='{}')。请检查拼写！\n可用原生内置工具：{}；{}\n外部 MCP 工具调用格式为：{{\"tool\": \"插件名/工具名\", \"command\": {{...}}}}。",
            tool,
            action,
            builtin_list,
            mcp_guide
        ),
        success: false,
        route: "unknown".to_string(),
    }
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

