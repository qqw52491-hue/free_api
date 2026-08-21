use crate::agent::mcp::PluginRegistry;
use crate::agent::retrieval;
use crate::agent::task_store;
use crate::agent::types::{AgentInstruction, DispatchResult};
use crate::agent::utils::{run_osascript, run_shell};
use crate::agent::web_search;
use crate::db::DbState;
use chrono::Utc;
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
/// 它不会拒绝读取，而是把无范围或过大的请求改写为一个可继续扩展的局部窗口。
fn guard_code_read_range(
    tool_name: &str,
    params: serde_json::Value,
) -> (serde_json::Value, Option<String>) {
    let reads_text = matches!(
        tool_name.to_ascii_lowercase().as_str(),
        "read_text_file" | "read_file" | "get_file_content"
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
        .and_then(|value| value.as_u64());
    let requested_end = obj
        .get("endLine")
        .or_else(|| obj.get("end_line"))
        .and_then(|value| value.as_u64());
    let start_line = requested_start.unwrap_or(1).max(1);
    let max_end = start_line.saturating_add(MAX_CODE_READ_LINES - 1);
    let end_line = requested_end.unwrap_or(max_end).min(max_end);
    let constrained =
        requested_start.is_none() || requested_end.map(|end| end > max_end).unwrap_or(true);

    if !constrained {
        return (serde_json::Value::Object(obj), None);
    }

    obj.insert("startLine".to_string(), json!(start_line));
    obj.insert("endLine".to_string(), json!(end_line));
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
        return dispatch_model_manager(db, action, params);
    }

    // 4. 外部 MCP 插件工具 (支持 "插件名/工具名" 标准格式 或 "插件名" + action)
    let (plugin_name, tool_name) = if tool.contains('/') {
        let parts: Vec<&str> = tool.split('/').collect();
        (parts[0].to_string(), parts[1].to_string())
    } else {
        (tool.to_string(), action.to_string())
    };

    if let Some(plugin) = registry.get(&plugin_name) {
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

// ==================== MODEL MANAGER DISPATCHER ====================

fn dispatch_model_manager(
    db: &DbState,
    action: &str,
    params: &serde_json::Value,
) -> DispatchResult {
    let conn = match db.0.lock() {
        Ok(c) => c,
        Err(e) => {
            return DispatchResult {
                stdout: String::new(),
                stderr: format!("数据库锁获取失败: {}", e),
                success: false,
                route: "model_manager".to_string(),
            }
        }
    };

    let act = if !action.is_empty() {
        action
    } else {
        params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list_specs")
    };

    match act {
        "list_specs" => {
            match crate::db::get_all_model_specs(&conn) {
                Ok(specs) => DispatchResult {
                    stdout: serde_json::to_string_pretty(&specs).unwrap_or_else(|_| "[]".to_string()),
                    stderr: String::new(),
                    success: true,
                    route: "model_manager:list_specs".to_string(),
                },
                Err(e) => DispatchResult {
                    stdout: String::new(),
                    stderr: format!("查询模型规格库失败: {}", e),
                    success: false,
                    route: "model_manager:list_specs".to_string(),
                },
            }
        }
        "upsert_spec" => {
            let name = match params.get("name").and_then(|v| v.as_str()) {
                Some(n) => n.to_string(),
                None => {
                    return DispatchResult {
                        stdout: String::new(),
                        stderr: "缺少必填参数: name (规格名称，如 DeepSeek-V4)".to_string(),
                        success: false,
                        route: "model_manager:upsert_spec".to_string(),
                    }
                }
            };
            let context_window = params.get("context_window").and_then(|v| v.as_i64()).unwrap_or(128000);
            let max_tokens = params.get("max_tokens").and_then(|v| v.as_i64()).unwrap_or(8192);
            let supports_thinking = params.get("supports_thinking").and_then(|v| v.as_bool()).unwrap_or(false);
            let default_temperature = params.get("default_temperature").and_then(|v| v.as_f64()).unwrap_or(0.7);
            let description = params.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let id = params.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let created_at = chrono::Utc::now().to_rfc3339();

            let spec = crate::db::ModelSpec {
                id,
                name: name.clone(),
                context_window,
                max_tokens,
                supports_thinking,
                default_temperature,
                description,
                created_at,
            };

            match crate::db::upsert_model_spec_record(&conn, &spec) {
                Ok(_) => DispatchResult {
                    stdout: format!("✅ 成功保存模型规格: {} (上下文: {} tokens, 最大输出: {} tokens)", name, context_window, max_tokens),
                    stderr: String::new(),
                    success: true,
                    route: "model_manager:upsert_spec".to_string(),
                },
                Err(e) => DispatchResult {
                    stdout: String::new(),
                    stderr: format!("保存模型规格失败: {}", e),
                    success: false,
                    route: "model_manager:upsert_spec".to_string(),
                },
            }
        }
        "delete_spec" => {
            let target = match params.get("name").or_else(|| params.get("id")).or_else(|| params.get("id_or_name")).and_then(|v| v.as_str()) {
                Some(t) => t,
                None => {
                    return DispatchResult {
                        stdout: String::new(),
                        stderr: "缺少必填参数: name, id 或 id_or_name".to_string(),
                        success: false,
                        route: "model_manager:delete_spec".to_string(),
                    }
                }
            };
            let confirm = params.get("confirm").and_then(|v| v.as_bool()).unwrap_or(false);
            if !confirm {
                return DispatchResult {
                    stdout: String::new(),
                    stderr: format!("⚠️ 安全保护：删除模型规格 [{}] 是破坏性操作。必须显式提供 confirm: true 才能执行。", target),
                    success: false,
                    route: "model_manager:delete_spec".to_string(),
                };
            }

            match crate::db::delete_model_spec_record(&conn, target) {
                Ok(count) => {
                    if count > 0 {
                        DispatchResult {
                            stdout: format!("✅ 已成功删除模型规格: {}", target),
                            stderr: String::new(),
                            success: true,
                            route: "model_manager:delete_spec".to_string(),
                        }
                    } else {
                        DispatchResult {
                            stdout: String::new(),
                            stderr: format!("未找到名称或ID为 [{}] 的模型规格", target),
                            success: false,
                            route: "model_manager:delete_spec".to_string(),
                        }
                    }
                }
                Err(e) => DispatchResult {
                    stdout: String::new(),
                    stderr: format!("删除模型规格失败: {}", e),
                    success: false,
                    route: "model_manager:delete_spec".to_string(),
                },
            }
        }
        "list_platforms" => {
            let mut stmt = match conn.prepare("SELECT id, name, base_url, created_at FROM platforms ORDER BY created_at DESC") {
                Ok(s) => s,
                Err(e) => {
                    return DispatchResult {
                        stdout: String::new(),
                        stderr: format!("查询平台失败: {}", e),
                        success: false,
                        route: "model_manager:list_platforms".to_string(),
                    }
                }
            };
            let platforms = stmt.query_map([], |row| {
                let pid: String = row.get(0)?;
                let pname: String = row.get(1)?;
                let purl: String = row.get(2)?;
                Ok(json!({ "id": pid, "name": pname, "base_url": purl }))
            }).and_then(|iter| iter.collect::<Result<Vec<_>, _>>());

            match platforms {
                Ok(list) => DispatchResult {
                    stdout: serde_json::to_string_pretty(&list).unwrap_or_else(|_| "[]".to_string()),
                    stderr: String::new(),
                    success: true,
                    route: "model_manager:list_platforms".to_string(),
                },
                Err(e) => DispatchResult {
                    stdout: String::new(),
                    stderr: format!("解析平台数据失败: {}", e),
                    success: false,
                    route: "model_manager:list_platforms".to_string(),
                },
            }
        }
        "add_model" => {
            let platform_query = match params.get("platform_id").or_else(|| params.get("platform_name")).and_then(|v| v.as_str()) {
                Some(p) => p,
                None => {
                    return DispatchResult {
                        stdout: String::new(),
                        stderr: "缺少必填参数: platform_id 或 platform_name".to_string(),
                        success: false,
                        route: "model_manager:add_model".to_string(),
                    }
                }
            };

            let platform_id: String = match conn.query_row(
                "SELECT id FROM platforms WHERE id = ?1 OR name = ?1",
                rusqlite::params![platform_query],
                |r| r.get(0),
            ) {
                Ok(id) => id,
                Err(_) => {
                    return DispatchResult {
                        stdout: String::new(),
                        stderr: format!("未找到平台 [{}]，请先检查平台名称或使用 list_platforms 查询", platform_query),
                        success: false,
                        route: "model_manager:add_model".to_string(),
                    }
                }
            };

            let model_name = match params.get("model_name").or_else(|| params.get("name")).and_then(|v| v.as_str()) {
                Some(n) => n,
                None => {
                    return DispatchResult {
                        stdout: String::new(),
                        stderr: "缺少必填参数: model_name (API模型标识)".to_string(),
                        success: false,
                        route: "model_manager:add_model".to_string(),
                    }
                }
            };

            // 如果指定了 spec_name，自动从规格库补齐参数
            let spec_opt: Option<(i64, i64, f64)> = if let Some(sname) = params.get("spec_name").and_then(|v| v.as_str()) {
                conn.query_row(
                    "SELECT context_window, max_tokens, default_temperature FROM model_specs WHERE name = ?1 OR id = ?1",
                    rusqlite::params![sname],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                ).ok()
            } else {
                None
            };

            let context_window = params.get("context_window").and_then(|v| v.as_i64())
                .or(spec_opt.map(|s| s.0))
                .unwrap_or(128000);
            let max_tokens = params.get("max_tokens").and_then(|v| v.as_i64())
                .or(spec_opt.map(|s| s.1))
                .unwrap_or(4096);
            let temperature = params.get("temperature").and_then(|v| v.as_f64())
                .or(spec_opt.map(|s| s.2))
                .unwrap_or(0.7);
            let display_name = params.get("display_name").and_then(|v| v.as_str()).unwrap_or(model_name);

            let id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().to_rfc3339();

            match conn.execute(
                "INSERT INTO models (id, platform_id, name, display_name, max_tokens, temperature, enabled, status, latency_ms, context_window, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 'unknown', 0, ?7, ?8)",
                rusqlite::params![id, platform_id, model_name, display_name, max_tokens, temperature, context_window, now],
            ) {
                Ok(_) => DispatchResult {
                    stdout: format!("✅ 成功添加模型: [{}] (显示名: {}, 上下文: {} tokens, 输出上限: {} tokens)", model_name, display_name, context_window, max_tokens),
                    stderr: String::new(),
                    success: true,
                    route: "model_manager:add_model".to_string(),
                },
                Err(e) => DispatchResult {
                    stdout: String::new(),
                    stderr: format!("添加模型失败: {}", e),
                    success: false,
                    route: "model_manager:add_model".to_string(),
                },
            }
        }
        "delete_model" => {
            let model_query = match params.get("model_id").or_else(|| params.get("model_name")).or_else(|| params.get("name")).and_then(|v| v.as_str()) {
                Some(m) => m,
                None => {
                    return DispatchResult {
                        stdout: String::new(),
                        stderr: "缺少必填参数: model_id 或 model_name".to_string(),
                        success: false,
                        route: "model_manager:delete_model".to_string(),
                    }
                }
            };
            let confirm = params.get("confirm").and_then(|v| v.as_bool()).unwrap_or(false);
            if !confirm {
                return DispatchResult {
                    stdout: String::new(),
                    stderr: format!("⚠️ 安全保护：删除模型 [{}] 将连带删除配置。若确认删除，请在参数中附加 {{\"confirm\": true}}。", model_query),
                    success: false,
                    route: "model_manager:delete_model".to_string(),
                };
            }

            match conn.execute("DELETE FROM models WHERE id = ?1 OR name = ?1", rusqlite::params![model_query]) {
                Ok(count) => {
                    if count > 0 {
                        DispatchResult {
                            stdout: format!("✅ 已成功删除模型: {}", model_query),
                            stderr: String::new(),
                            success: true,
                            route: "model_manager:delete_model".to_string(),
                        }
                    } else {
                        DispatchResult {
                            stdout: String::new(),
                            stderr: format!("未找到模型 [{}]", model_query),
                            success: false,
                            route: "model_manager:delete_model".to_string(),
                        }
                    }
                }
                Err(e) => DispatchResult {
                    stdout: String::new(),
                    stderr: format!("删除模型失败: {}", e),
                    success: false,
                    route: "model_manager:delete_model".to_string(),
                },
            }
        }
        _ => DispatchResult {
            stdout: String::new(),
            stderr: format!("❌ 未知的 model_manager 动作 action='{}'。支持的动作: list_specs, upsert_spec, delete_spec, list_platforms, add_model, delete_model", act),
            success: false,
            route: "model_manager:unknown".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{init_db, DbState};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

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
        let res = dispatch_model_manager(&db, "list_specs", &p_list);
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
        let res = dispatch_model_manager(&db, "upsert_spec", &p_upsert);
        assert!(res.success, "upsert_spec should succeed: {}", res.stderr);

        // 3. 测试 delete_spec 无 confirm 拦截
        let p_del_no_confirm = serde_json::json!({ "id_or_name": "Custom-Test-Model" });
        let res = dispatch_model_manager(&db, "delete_spec", &p_del_no_confirm);
        assert!(
            !res.success,
            "delete_spec without confirm must fail for safety"
        );
        assert!(res.stderr.contains("必须显式提供 confirm: true"));

        // 4. 测试 delete_spec 带 confirm: true
        let p_del_confirm =
            serde_json::json!({ "id_or_name": "Custom-Test-Model", "confirm": true });
        let res = dispatch_model_manager(&db, "delete_spec", &p_del_confirm);
        assert!(
            res.success,
            "delete_spec with confirm must succeed: {}",
            res.stderr
        );

        // 5. 测试 list_platforms
        let p_platforms = serde_json::json!({});
        let res = dispatch_model_manager(&db, "list_platforms", &p_platforms);
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
    }
}
