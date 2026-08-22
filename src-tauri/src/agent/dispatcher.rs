use crate::agent::mcp::PluginRegistry;
use crate::agent::mcp_catalog::invalidate_plugin;
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

/// `finish` 是 Agent 协议的终态动作，不是本地工具或 MCP 工具。
/// 统一判定可防止某个调用路径把已完成任务派发为未知 action。
pub fn is_finish_instruction(instruction: &AgentInstruction) -> bool {
    instruction.get_tool().eq_ignore_ascii_case("finish")
        || instruction.get_action().eq_ignore_ascii_case("finish")
}

/// MCP 服务总名本身不是可调用工具。调用必须指定 `服务/具体工具`，或以服务名配合不同的具体 action。
/// 该检查只拒绝无效形态，不从参数中猜测或迁移任何工具、cwd、path 或 project。
fn mcp_service_requires_concrete_tool(tool: &str, action: &str) -> bool {
    !tool.contains('/')
        && (action.trim().is_empty() || tool.trim().eq_ignore_ascii_case(action.trim()))
}

pub fn is_write_or_side_effect_tool(inst: &AgentInstruction) -> bool {
    let tool = inst.get_tool().to_ascii_lowercase();
    let action = inst.get_action().to_ascii_lowercase();

    // 显式排除网络抓取/搜索/文件读取等纯只读操作
    if tool == "web_fetch"
        || tool == "web_search"
        || tool == "read_file"
        || tool == "search_files"
        || tool == "ripgrep_search"
        || tool.starts_with("read_")
        || tool.starts_with("get_")
        || tool.starts_with("list_")
        || tool.starts_with("search_")
        || action.starts_with("read_")
        || action.starts_with("get_")
        || action.starts_with("list_")
        || action.starts_with("search_")
    {
        return false;
    }

    // 显式匹配写盘/Excel/代码修改/数据库写入/系统脚本执行等写操作
    tool.contains("excel")
        || tool.contains("write")
        || tool.contains("edit")
        || tool.contains("shell")
        || tool.contains("bash")
        || tool.contains("exec")
        || (tool == "task_store"
            && (action.contains("create")
                || action.contains("upsert")
                || action.contains("update")))
        || action.starts_with("write")
        || action.starts_with("edit")
        || action.starts_with("create")
        || action.starts_with("delete")
        || action.starts_with("save")
        || action.starts_with("exec")
        || action.starts_with("run")
}

/// 面向 AgentPanel 的轻量工具生命周期事件。只发送参数和输出摘要；完整大对象
/// 仍由 task_store / 文件系统持久化，不能被事件流或模型上下文重复承载。
#[derive(Debug, Clone, Serialize)]
pub struct AgentToolEvent {
    pub event_type: String,
    pub step_id: usize,
    pub description: String,
    pub tool: String,
    pub action: String,
    pub route: Option<String>,
    pub success: Option<bool>,
    pub params_summary: String,
    pub result_summary: String,
    pub duration_ms: Option<u128>,
    pub timestamp: String,
}

pub fn event_summary(value: &str, limit: usize) -> String {
    let normalized = value.trim();
    if normalized.chars().count() <= limit {
        normalized.to_string()
    } else {
        format!("{}…", normalized.chars().take(limit).collect::<String>())
    }
}

pub fn emit_tool_event(
    app: &AppHandle,
    session_id: &str,
    step_id: usize,
    inst: &AgentInstruction,
    event_type: &str,
    route: Option<String>,
    success: Option<bool>,
    result: &str,
    duration_ms: Option<u128>,
) {
    let payload = AgentToolEvent {
        event_type: event_type.to_string(),
        step_id,
        description: inst.description.clone(),
        tool: inst.get_tool(),
        action: inst.get_action(),
        route,
        success,
        params_summary: event_summary(&inst.get_params().to_string(), 8000),
        result_summary: event_summary(result, 16000),
        duration_ms,
        timestamp: Utc::now().to_rfc3339(),
    };
    let _ = app.emit(&format!("agent-tool-event-{}", session_id), payload);
}

/// 🌟 智能规范化 MCP 参数（自动解包嵌套层级，自动映射 path/filePath 等常见别名）
fn normalize_mcp_params(params: &serde_json::Value) -> serde_json::Value {
    let mut obj = match params {
        serde_json::Value::Object(map) => map.clone(),
        _ => return params.clone(),
    };

    // 1. 如果大模型把参数多包了一层 (如 command: {...} 或 args: {...} 或 params: {...})，自动解包展开
    if obj.len() == 1 {
        if let Some(inner) = obj
            .get("command")
            .or_else(|| obj.get("args"))
            .or_else(|| obj.get("params"))
        {
            if let Some(inner_map) = inner.as_object() {
                obj = inner_map.clone();
            }
        }
    }

    // 2. 常见文件路径别名自动规范化 (filePath / file_path / file / filename / dirPath / dir_path -> path)
    if !obj.contains_key("path") {
        if let Some(v) = obj
            .get("filePath")
            .or_else(|| obj.get("file_path"))
            .or_else(|| obj.get("file"))
            .or_else(|| obj.get("filename"))
            .or_else(|| obj.get("dirPath"))
            .or_else(|| obj.get("dir_path"))
            .or_else(|| obj.get("directory"))
        {
            obj.insert("path".to_string(), v.clone());
        }
    }

    // 3. 针对 shell 命令的自适应兼容：若 command 为包含空格的整句命令行且未显式指定 args 或 shell_mode，自动开启 shell_mode: true
    if let Some(serde_json::Value::String(cmd_str)) = obj.get("command") {
        if cmd_str.contains(' ')
            && !obj.contains_key("args")
            && !obj.contains_key("shell_mode")
            && !obj.contains_key("shell")
        {
            obj.insert("shell_mode".to_string(), serde_json::json!(true));
        }
    }

    serde_json::Value::Object(obj)
}

const MAX_CODE_READ_LINES: u64 = 240;

/// 为常见源码读取工具补上可见、稳定的行范围护栏。
/// 它不会拒绝读取，而是把无范围、过大或参数别名（offset/limit）的请求改写为一个可继续扩展的局部窗口。
fn guard_code_read_range(
    tool_name: &str,
    params: serde_json::Value,
) -> (serde_json::Value, Option<String>) {
    let lower_tool = tool_name.to_ascii_lowercase();
    let reads_text = matches!(
        lower_tool.as_str(),
        "read_text_file" | "read_file" | "get_file_content" | "get_code_snippet"
    );
    let serde_json::Value::Object(mut obj) = params else {
        return (params, None);
    };
    if !reads_text || !obj.contains_key("path") {
        return (serde_json::Value::Object(obj), None);
    }

    let requested_start = obj
        .get("startLine")
        .or_else(|| obj.get("start_line"))
        .or_else(|| obj.get("offset"))
        .and_then(|value| value.as_u64());
    let requested_end = obj
        .get("endLine")
        .or_else(|| obj.get("end_line"))
        .and_then(|value| value.as_u64())
        .or_else(|| {
            obj.get("limit").and_then(|lim| {
                let s = requested_start.unwrap_or(1).max(1);
                lim.as_u64().map(|l| s.saturating_add(l).saturating_sub(1))
            })
        });

    let start_line = requested_start.unwrap_or(1).max(1);
    let max_end = start_line.saturating_add(MAX_CODE_READ_LINES - 1);
    let end_line = requested_end.unwrap_or(max_end).min(max_end);
    let constrained = requested_start.is_none()
        || requested_end.is_none()
        || requested_end.map(|end| end > max_end).unwrap_or(true);

    obj.insert("startLine".to_string(), json!(start_line));
    obj.insert("endLine".to_string(), json!(end_line));
    if obj.contains_key("start_line") {
        obj.insert("start_line".to_string(), json!(start_line));
    }
    if obj.contains_key("end_line") {
        obj.insert("end_line".to_string(), json!(end_line));
    }

    if !constrained {
        return (serde_json::Value::Object(obj), None);
    }

    let path = obj
        .get("path")
        .and_then(|value| value.as_str())
        .unwrap_or("<unknown>");
    let notice = format!(
        "[代码阅读护栏] {} 的请求已限定为第 {}–{} 行（最多 {} 行）。这是正常预算边界而非工具故障；如需更多证据，请按函数/符号、错误码或 diff hunk 选择相邻范围继续读取，而非重复全文请求。",
        path, start_line, end_line, MAX_CODE_READ_LINES
    );
    (serde_json::Value::Object(obj), Some(notice))
}

/// 执行单条指令的内部路由逻辑（4大核心原生工具 + MCP 插件路由，彻底杜绝历史兼容包袱）
pub fn execute_command_inner(
    db: &DbState,
    session_id: &str,
    tool: &str,
    action: &str,
    params: &serde_json::Value,
    registry: &PluginRegistry,
) -> DispatchResult {
    // 0. 宿主层硬拦截 (Host Security Rejection Layer)：
    // 严禁模型自作主张越界切换工作区（例如试图调用 switch_workspace、set_workspace 等已被禁用的特权命令）
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
        return DispatchResult {
            stdout: String::new(),
            stderr: "❌ [宿主层安全拦截 (Host Security Rejection)] switch_workspace / 切换工作区已被宿主安全策略禁止。当前任务严格限定在初始工作区上下文中执行。请使用 search 或 read_file 等只读工具在当前工作区内完成代码定位，严禁越界切换。".to_string(),
            success: false,
            route: "host_rejection:forbidden_workspace_switch".to_string(),
        };
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

    // 3. 核心原生工具 C: search (跨执行日志 FTS5 与实时代码 ripgrep 联邦检索)
    if tool.eq_ignore_ascii_case("search") {
        return retrieval::dispatch_search(db, session_id, params);
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
        return crate::agent::tools::mcp_manager::dispatch(db, action, params);
    }

    // 3.8 核心原生工具 G: skill_manager (工作区自定义技能与提示词库维护)
    if tool.eq_ignore_ascii_case("skill_manager") {
        return crate::agent::tools::skill_manager::dispatch(db, action, params);
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
            return DispatchResult {
                stdout: String::new(),
                stderr: format!(
                    "❌ [工具契约] MCP 服务‘{}’不是具体工具。请显式调用‘{}/具体工具’，或保留 tool=‘{}’并将 action 设为具体工具名；不要把子工具名只放在参数里。宿主不会自动补全或改写 tool、cwd、path、project 参数。",
                    plugin_name, plugin_name, plugin_name
                ),
                success: false,
                route: "host_rejection:mcp_service_requires_subtool".to_string(),
            };
        }
        let normalized_params = normalize_mcp_params(params);
        let (clean_params, read_guard_notice) =
            guard_code_read_range(&tool_name, normalized_params);
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
                let content = read_guard_notice
                    .map(|notice| format!("{}\n\n{}", notice, content))
                    .unwrap_or(content);

                if is_error {
                    return DispatchResult {
                        stdout: String::new(),
                        stderr: content,
                        success: false,
                        route: format!("plugin:{}/{}", plugin_name, tool_name),
                    };
                } else {
                    return DispatchResult {
                        stdout: content,
                        stderr: String::new(),
                        success: true,
                        route: format!("plugin:{}/{}", plugin_name, tool_name),
                    };
                }
            }
            Err(e) => {
                return DispatchResult {
                    stdout: String::new(),
                    stderr: format!("插件调用异常: {:?}", e),
                    success: false,
                    route: format!("plugin:{}/{}", plugin_name, tool_name),
                };
            }
        }
    }

    // 5. 未知工具或格式错误报错
    DispatchResult {
        stdout: String::new(),
        stderr: format!(
            "❌ 未知工具 tool='{}' (action='{}')。请检查拼写！可用原生内置工具：finish, ask_human, model_manager, web_search, web_fetch, search, get_step_output, task_store；外部 MCP 工具格式为：{{\"tool\": \"插件名/工具名\", \"command\": {{...}}}}。",
            tool, action
        ),
        success: false,
        route: "unknown".to_string(),
    }
}

pub fn run_plan_tree_sync(
    db: &DbState,
    session_id: &str,
    plan_node: &crate::agent::plan_tree::PlanNode,
    registry: &PluginRegistry,
) -> DispatchResult {
    let db_clone = db.clone();
    let sid = session_id.to_string();
    let reg_clone = registry.clone();

    let invoker: crate::agent::plan_tree::ToolInvoker = Arc::new(move |tool, args| {
        let db_c = db_clone.clone();
        let sid_c = sid.clone();
        let reg_c = reg_clone.clone();

        Box::pin(async move {
            let action = args
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // 修复: 并行分支在 tokio worker 线程上调用 invoker，而 execute_command_inner
            // 对 MCP 插件使用 handle.block_on，在 runtime 内嵌套 block_on 会直接 panic。
            // 用 spawn_blocking 把同步分发放到 blocking 线程执行，消除 runtime 嵌套。
            let res = tokio::task::spawn_blocking(move || {
                execute_command_inner(&db_c, &sid_c, &tool, &action, &args, &reg_c)
            })
            .await
            .map_err(|e| format!("工具执行任务崩溃: {}", e))?;
            if res.success {
                let parsed_val: serde_json::Value = serde_json::from_str(&res.stdout)
                    .unwrap_or_else(|_| serde_json::json!({ "output": res.stdout }));
                Ok(parsed_val)
            } else {
                Err(if !res.stderr.is_empty() {
                    res.stderr
                } else {
                    "工具执行返回失败".to_string()
                })
            }
        })
    });

    let report = futures::executor::block_on(crate::agent::plan_tree::execute_plan_tree(
        plan_node, invoker, None,
    ));

    let success = report.success;
    let feedback = report.to_llm_feedback();

    DispatchResult {
        stdout: feedback.clone(),
        stderr: if success { String::new() } else { feedback },
        success,
        route: if success {
            "plan_tree_success".to_string()
        } else {
            "plan_tree_failed".to_string()
        },
    }
}

const MAX_CONTROLLED_PARALLEL_BRANCHES: usize = 2;

/// 并行仅接受少量、已经由模型明确给出的独立分支。
/// 该校验不推断、补充或改写任何 MCP 子步骤参数。
fn validate_controlled_parallel_batch(parallel_steps: &[serde_json::Value]) -> Result<(), String> {
    match parallel_steps.len() {
        0 => Err("parallel 必须至少包含一个已确定的独立分支；未知范围请使用单步取证。".to_string()),
        1..=MAX_CONTROLLED_PARALLEL_BRANCHES => Ok(()),
        actual => Err(format!(
            "parallel 单批最多允许 {} 个独立分支，当前为 {}。请先串行收缩范围与工具契约，再保留至多两个互不依赖、参数已确定的分支。宿主不会改写子步骤参数。",
            MAX_CONTROLLED_PARALLEL_BRANCHES, actual
        )),
    }
}

// 根据指令执行具体动作
pub fn run_agent_step(
    db: &DbState,
    session_id: &str,
    instruction: &AgentInstruction,
    registry: &PluginRegistry,
) -> DispatchResult {
    let tool_name = instruction.get_tool().to_lowercase();
    if is_finish_instruction(instruction) {
        return DispatchResult {
            stdout: instruction
                .command
                .as_ref()
                .and_then(|value| value.get("summary").or_else(|| value.get("details")))
                .and_then(|value| value.as_str())
                .unwrap_or("任务已完成")
                .to_string(),
            stderr: String::new(),
            success: true,
            route: "finish".to_string(),
        };
    }

    // --- 场景 A0: 树形/并行计划树 PlanTree Execution ---
    if let Some(ref par_cmds) = instruction.parallel {
        if let Err(error) = validate_controlled_parallel_batch(par_cmds) {
            return DispatchResult {
                stdout: String::new(),
                stderr: error,
                success: false,
                route: "parallel_contract_rejected".to_string(),
            };
        }
        println!(
            "🌳 [PlanTree: {}] 检测到 parallel 并行指令，启动 {} 路并发执行...",
            session_id,
            par_cmds.len()
        );
        let plan_node =
            crate::agent::plan_tree::PlanNode::Obj(crate::agent::plan_tree::PlanNodeObj {
                parallel: Some(
                    par_cmds
                        .iter()
                        .map(|c| {
                            serde_json::from_value(c.clone()).unwrap_or_else(|_| {
                                crate::agent::plan_tree::PlanNode::Obj(
                                    serde_json::from_value(c.clone()).unwrap_or_default(),
                                )
                            })
                        })
                        .collect(),
                ),
                ..Default::default()
            });
        return run_plan_tree_sync(db, session_id, &plan_node, registry);
    }
    if let Some(ref tree_val) = instruction.plan_tree {
        if let Ok(plan_node) =
            serde_json::from_value::<crate::agent::plan_tree::PlanNode>(tree_val.clone())
        {
            println!(
                "🌳 [PlanTree: {}] 检测到 plan_tree 复合树形计划，启动树形解释器...",
                session_id
            );
            return run_plan_tree_sync(db, session_id, &plan_node, registry);
        }
    }

    // --- 核心修复：兼容 root.commands 和 root.command.commands ---
    let mut batch_commands = instruction.commands.clone();
    if batch_commands.is_empty() {
        if let Some(cmd_val) = &instruction.command {
            if let Some(cmds) = cmd_val.get("commands").and_then(|v| v.as_array()) {
                batch_commands = cmds.clone();
            }
        }
    }

    // --- 场景 A: 组合指令流水线 Batch Execution ---
    if !batch_commands.is_empty() {
        println!(
            "🚀 [Pipeline: {}] 正在按顺序执行 {} 个组合动作...",
            session_id,
            batch_commands.len()
        );
        let mut total_stdout = String::new();

        for (idx, cmd_val) in batch_commands.iter().enumerate() {
            let step_idx = idx + 1;
            // 提取单步的 tool 与 action (优先使用子命令自带的 tool，缺省退回根 tool_name)
            let step_tool = cmd_val
                .get("tool")
                .and_then(|v| v.as_str())
                .unwrap_or(&tool_name);
            let step_action = cmd_val
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();

            println!(
                "   ➡️  Step {}: 执行 [{}] 动作 '{}'...",
                step_idx, step_tool, step_action
            );

            // 执行单步
            let res =
                execute_command_inner(db, session_id, step_tool, &step_action, cmd_val, registry);

            // 拼接输出结果
            if !res.stdout.is_empty() {
                total_stdout.push_str(&format!("\n[Step {} Result]: {}", step_idx, res.stdout));
            }

            // --- 核心修复：Fail-Fast 快速失败机制 ---
            if !res.success {
                println!(
                    "   🚫 Step {} 执行失败，流水线立即熔断！原因: {}",
                    step_idx, res.stderr
                );
                return DispatchResult {
                    stdout: total_stdout,
                    stderr: format!("流水线在第 {} 步崩溃: {}", step_idx, res.stderr),
                    success: false,
                    route: format!("pipeline_fail_at_{}", step_idx),
                };
            }
        }

        return DispatchResult {
            stdout: total_stdout,
            stderr: String::new(),
            success: true,
            route: "pipeline_success".to_string(),
        };
    }

    // --- 场景 B: 传统单步指令 Single Execution ---
    let mut action = instruction.get_action().trim().to_lowercase();

    // --- 幻觉纠偏：如果 action 为空但 extra_fields 里有 tool_name，尝试打捞 ---
    if action.is_empty() {
        if let Some(name) = instruction
            .extra_fields
            .get("tool_name")
            .and_then(|v| v.as_str())
        {
            action = name.to_lowercase();
        }
    }

    let params = instruction.get_params();
    execute_command_inner(db, session_id, &tool_name, &action, &params, registry)
}

#[tauri::command]
pub async fn dispatch_agent_step(
    instruction_json: String,
    state: State<'_, DbState>,
    registry_state: State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    session_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let instruction: AgentInstruction =
        serde_json::from_str(&instruction_json).map_err(|e| e.to_string())?;

    // --- Strict JSON Schema Mutual Exclusivity Check ---
    let mut modes = 0;
    if instruction.tool.is_some() || instruction.action.is_some() { modes += 1; }
    if instruction.parallel.is_some() { modes += 1; }
    if instruction.plan_tree.is_some() { modes += 1; }
    if !instruction.commands.is_empty() { modes += 1; }

    if modes > 1 {
        return Ok(json!({
            "route": "schema_validation_failed",
            "success": false,
            "stdout": "",
            "stderr": "请选择单一步 (tool/action)、parallel 并行数组 或 plan_tree 计划树之一，严禁在同一 JSON 中混用！"
        }));
    }

    let registry_arc = registry_state.inner().clone();
    let db = state.inner().clone();
    let sid = session_id.unwrap_or_else(|| "default".to_string());

    let registry = {
        let guard = registry_arc.lock().await;
        (*guard).clone()
    };

    let result =
        tokio::task::spawn_blocking(move || run_agent_step(&db, &sid, &instruction, &registry))
            .await
            .map_err(|e| e.to_string())?;

    Ok(
        json!({ "route": result.route, "success": result.success, "stdout": result.stdout, "stderr": result.stderr }),
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{init_db, DbState};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    #[test]
    fn controlled_parallel_allows_at_most_two_explicit_branches_without_parameter_rewrite() {
        let one = vec![serde_json::json!({"tool": "remote-http-mcp/read_text_file", "command": {"path": "/repo/a.rs"}})];
        let two = vec![
            serde_json::json!({"tool": "remote-http-mcp/read_text_file", "command": {"cwd": "/repo", "path": "a.rs"}}),
            serde_json::json!({"tool": "remote-http-mcp/read_text_file", "command": {"cwd": "/repo", "path": "b.rs"}}),
        ];
        let three = vec![serde_json::json!({}), serde_json::json!({}), serde_json::json!({})];

        assert!(validate_controlled_parallel_batch(&one).is_ok());
        assert!(validate_controlled_parallel_batch(&two).is_ok());
        assert!(validate_controlled_parallel_batch(&three).unwrap_err().contains("最多允许 2"));
        assert_eq!(two[0]["command"]["cwd"], serde_json::json!("/repo"));
        assert_eq!(two[1]["command"]["path"], serde_json::json!("b.rs"));
    }

    #[test]
    fn mcp_service_name_requires_an_explicit_subtool_without_parameter_inference() {
        assert!(mcp_service_requires_concrete_tool("remote-http-mcp", ""));
        assert!(mcp_service_requires_concrete_tool("remote-http-mcp", "remote-http-mcp"));
        assert!(!mcp_service_requires_concrete_tool(
            "remote-http-mcp/read_text_file",
            ""
        ));
        assert!(!mcp_service_requires_concrete_tool(
            "remote-http-mcp",
            "read_text_file"
        ));
    }

    #[test]
    fn code_read_guard_bounds_unscoped_and_large_requests() {
        let (unscoped, notice) = guard_code_read_range(
            "read_text_file",
            json!({"path": "src/agent/context/sampling.rs"}),
        );
        assert_eq!(unscoped["startLine"], 1);
        assert_eq!(unscoped["endLine"], MAX_CODE_READ_LINES);
        assert!(notice.unwrap().contains("正常预算边界"));

        let (large, notice) = guard_code_read_range(
            "read_text_file",
            json!({"path": "src/agent/context/sampling.rs", "startLine": 500, "endLine": 1000}),
        );
        assert_eq!(large["startLine"], 500);
        assert_eq!(large["endLine"], 739);
        assert!(notice.is_some());

        let (bounded, notice) = guard_code_read_range(
            "read_text_file",
            json!({"path": "src/agent/context/sampling.rs", "startLine": 40, "endLine": 100}),
        );
        assert_eq!(bounded["endLine"], 100);
        assert!(notice.is_none());
    }

    #[test]
    fn test_model_manager_crud_and_safety_confirm() {
        let conn = init_db(&PathBuf::from(":memory:")).expect("init memory db");
        let db = DbState(Arc::new(Mutex::new(conn)));

        // 1. 测试 list_specs
        let p_list = serde_json::json!({});
        let res = crate::agent::tools::model_manager::dispatch(&db, "list_specs", &p_list);
        assert!(res.success, "list_specs should succeed: {}", res.stderr);
        assert!(
            res.stdout.contains("DeepSeek-V3") || res.stdout.contains("DeepSeek"),
            "should contain default specs"
        );

        // 2. 测试 upsert_spec
        let p_upsert = serde_json::json!({
            "name": "Custom-Test-Model",
            "context_window": 65536,
            "max_tokens": 8192,
            "supports_thinking": true,
            "default_temperature": 0.6,
            "description": "A unit test model spec"
        });
        let res = crate::agent::tools::model_manager::dispatch(&db, "upsert_spec", &p_upsert);
        assert!(res.success, "upsert_spec should succeed: {}", res.stderr);

        // 3. 测试 delete_spec 无 confirm 拦截
        let p_del_no_confirm = serde_json::json!({ "name": "Custom-Test-Model" });
        let res = crate::agent::tools::model_manager::dispatch(&db, "delete_spec", &p_del_no_confirm);
        assert!(
            !res.success,
            "delete_spec without confirm must fail for safety"
        );
        assert!(res.stderr.contains("confirm: true"));

        // 4. 测试 delete_spec 带 confirm: true
        let p_del_confirm =
            serde_json::json!({ "name": "Custom-Test-Model", "confirm": true });
        let res = crate::agent::tools::model_manager::dispatch(&db, "delete_spec", &p_del_confirm);
        assert!(
            res.success,
            "delete_spec with confirm must succeed: {}",
            res.stderr
        );

        // 5. 测试 list_platforms
        let p_platforms = serde_json::json!({});
        let res = crate::agent::tools::model_manager::dispatch(&db, "list_platforms", &p_platforms);
        assert!(res.success, "list_platforms should succeed: {}", res.stderr);
    }

    #[test]
    fn test_prompts_tools_files_exist() {
        let candidate_dirs = [
            std::path::PathBuf::from("src-tauri/prompts/tools"),
            std::path::PathBuf::from("prompts/tools"),
            std::path::PathBuf::from("../src-tauri/prompts/tools"),
        ];

        let mut found_dir = None;
        for d in &candidate_dirs {
            if d.exists() && d.is_dir() {
                found_dir = Some(d.clone());
                break;
            }
        }

        let dir = found_dir.expect("prompts/tools directory must exist in candidate paths");
        let model_manager_path = dir.join("model_manager.md");
        assert!(
            model_manager_path.exists(),
            "model_manager.md must exist in {:?}",
            dir
        );
        let content = std::fs::read_to_string(&model_manager_path).expect("read model_manager.md");
        assert!(
            content.contains("<summary>"),
            "model_manager.md must contain <summary>"
        );
        assert!(
            content.contains("upsert_spec"),
            "model_manager.md must describe upsert_spec"
        );

        let mcp_manager_path = dir.join("mcp_manager.md");
        assert!(mcp_manager_path.exists(), "mcp_manager.md must exist in {:?}", dir);
        let mcp_content = std::fs::read_to_string(&mcp_manager_path).expect("read mcp_manager.md");
        assert!(mcp_content.contains("add_http"), "mcp_manager.md must describe add_http");

        let skill_manager_path = dir.join("skill_manager.md");
        assert!(skill_manager_path.exists(), "skill_manager.md must exist in {:?}", dir);
        let skill_content = std::fs::read_to_string(&skill_manager_path).expect("read skill_manager.md");
        assert!(skill_content.contains("add"), "skill_manager.md must describe add");
    }

    #[test]
    fn test_mcp_and_skill_manager_crud() {
        let conn = init_db(&PathBuf::from(":memory:")).expect("init memory db");
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        // 1. MCP Manager List & Toggle & Delete
        let res_list = crate::agent::tools::mcp_manager::dispatch(&db, "list", &serde_json::json!({}));
        assert!(res_list.success);

        // Delete without confirm
        let res_del_fail = crate::agent::tools::mcp_manager::dispatch(&db, "delete", &serde_json::json!({ "name": "fake_mcp" }));
        assert!(!res_del_fail.success);
        assert!(res_del_fail.stderr.contains("confirm: true"));

        // Delete with confirm
        let res_del_ok = crate::agent::tools::mcp_manager::dispatch(&db, "delete", &serde_json::json!({ "name": "fake_mcp", "confirm": true }));
        assert!(res_del_ok.success);

        // 2. Skill Manager Add & List & Delete
        let res_add_skill = crate::agent::tools::skill_manager::dispatch(&db, "add", &serde_json::json!({
            "name": "单元测试技能",
            "content": "编写高覆盖率的 Rust 与 Vue 单元测试",
            "kind": "skill",
            "description": "用于编写单元测试"
        }));
        assert!(res_add_skill.success);
        assert!(res_add_skill.stdout.contains("单元测试技能"));

        let res_skill_list = crate::agent::tools::skill_manager::dispatch(&db, "list", &serde_json::json!({}));
        assert!(res_skill_list.success);
        assert!(res_skill_list.stdout.contains("单元测试技能"));

        let res_skill_del = crate::agent::tools::skill_manager::dispatch(&db, "delete", &serde_json::json!({ "name": "单元测试技能", "confirm": true }));
        assert!(res_skill_del.success);
    }

    #[test]
    fn test_host_security_rejection_forbidden_workspace_switch() {
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        )));
        let registry = PluginRegistry::new();
        let p = serde_json::json!({ "path": "/some/other/workspace" });

        let res = execute_command_inner(&db, "test-session", "switch_workspace", "", &p, &registry);
        assert!(!res.success);
        assert!(res.stderr.contains("宿主层安全拦截"));
        assert_eq!(res.route, "host_rejection:forbidden_workspace_switch");

        let res2 = execute_command_inner(&db, "test-session", "vscode_switchworkspace", "", &p, &registry);
        assert!(!res2.success);
        assert!(res2.stderr.contains("宿主层安全拦截"));
    }

    #[test]
    fn test_code_read_guard_supports_offset_and_snippet() {
        // 1. 预算内的 offset + limit (50..149 共 100 行 <= 240)
        let p_within = serde_json::json!({
            "path": "src/main.rs",
            "offset": 50,
            "limit": 100
        });
        let (guarded_within, notice_within) = guard_code_read_range("get_code_snippet", p_within);
        assert_eq!(guarded_within.get("startLine").and_then(|v| v.as_u64()), Some(50));
        assert_eq!(guarded_within.get("endLine").and_then(|v| v.as_u64()), Some(149));
        assert!(notice_within.is_none(), "within-budget range should not need truncation notice");

        // 2. 超出预算的 offset + limit (50..5000)
        let p_over = serde_json::json!({
            "path": "src/main.rs",
            "offset": 50,
            "limit": 5000
        });
        let (guarded_over, notice_over) = guard_code_read_range("get_code_snippet", p_over);
        assert_eq!(guarded_over.get("startLine").and_then(|v| v.as_u64()), Some(50));
        assert_eq!(guarded_over.get("endLine").and_then(|v| v.as_u64()), Some(289)); // 50 + 240 - 1
        assert!(notice_over.is_some(), "over-budget range must include truncation guard notice");
    }
}
