pub mod checkpoint;
pub mod context;
pub mod mcp;
pub mod router;
pub mod run_state;
pub mod task_store;
pub mod types;
pub mod utils;

use crate::agent::checkpoint::{
    load_checkpoint, recovery_decision, save_checkpoint, AgentCheckpoint, RecoveryDecision,
};
use crate::agent::mcp::*;
use crate::agent::run_state::{AgentRunEvent, AgentRunMachine, AgentRunState};
use crate::agent::types::*;
use crate::agent::utils::*;
use crate::db::DbState;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tauri::Emitter;
use tauri::{AppHandle, Manager, State};
use tokio::sync::{mpsc, Mutex};
use tokio::time::{sleep, Duration};

// 维护每个 session_id 对应的干预消息接收通道
pub fn intervention_channels() -> &'static Arc<Mutex<HashMap<String, mpsc::Sender<String>>>> {
    static CHANNELS: OnceLock<Arc<Mutex<HashMap<String, mpsc::Sender<String>>>>> = OnceLock::new();
    CHANNELS.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

#[tauri::command]
pub async fn send_human_intervention(session_id: String, message: String) -> Result<(), String> {
    let channels = intervention_channels().lock().await;
    if let Some(sender) = channels.get(&session_id) {
        sender.send(message).await.map_err(|e| e.to_string())?;
        Ok(())
    } else {
        Err("任务已结束或通道不存在".to_string())
    }
}

/// Agent 每个任务最多执行步数（可按项目需求调整）
const MAX_STEPS: usize = 300;

/// 推进后端运行状态，并向前端发送独立于旧进度事件的权威状态快照。
/// 旧的 agent-progress-* 事件保持不变，以便前端逐步迁移。
fn advance_run_state(
    app: &AppHandle,
    session_id: &str,
    machine: &mut AgentRunMachine,
    event: AgentRunEvent,
) -> Result<(), String> {
    let snapshot = machine.transition(event).map_err(|err| err.to_string())?;
    app.emit(&format!("agent-run-state-{}", session_id), snapshot)
        .map_err(|err| err.to_string())?;
    Ok(())
}

/// 执行单条指令的内部路由逻辑
fn execute_command_inner(
    db: &DbState,
    session_id: &str,
    tool: &str,
    action: &str,
    params: &serde_json::Value,
    registry: &mut PluginRegistry,
) -> DispatchResult {
    let mut final_action = action.to_string();

    // --- 兼容性修复：如果 action 为空但 params 里有参数，尝试从 params 里捞 action ---
    if final_action.is_empty() {
        if let Some(a) = params.get("action").and_then(|v| v.as_str()) {
            final_action = a.to_string();
        } else if let Some(a) = params.get("command").and_then(|v| v.as_str()) {
            final_action = a.to_string();
        }
    }

    if tool.eq_ignore_ascii_case("task_store") {
        return task_store::dispatch(db, session_id, &final_action, params);
    }

    let (plugin_name, tool_name) = if tool.contains('/') {
        let parts: Vec<&str> = tool.split('/').collect();
        (parts[0].to_string(), parts[1].to_string())
    } else {
        (tool.to_string(), final_action.to_string())
    };

    if let Some(plugin) = registry.get_mut(&plugin_name) {
        println!("执行动作调用外部插件: {}/{}", plugin_name, tool_name);
        match plugin.call_tool(&tool_name, params.clone()) {
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

    // 专项检测：model 幻觉成了内置函数调用格式（tool_name / tool_input）
    let is_function_calling_hallucination = params.get("tool_name").is_some()
        || params.get("tool_input").is_some()
        || params.get("function_call").is_some()
        || final_action == "none"
        || final_action.is_empty();

    if is_function_calling_hallucination {
        return DispatchResult {
            stdout: String::new(),
            stderr: format!(
                "❌ 【幻觉纠正】你输出了错误的函数调用格式（tool_name/tool_input）！\n\
                这不是我们的协议。正确格式必须是：\n\
                {{\"tool\": \"动态 MCP 工具\", \"command\": {{\"action\": \"具体动作\"}}}}\n\
                或者任务完成时：{{\"tool\": \"finish\", \"command\": {{}}}}\n\
                绝对禁止输出 tool_name / tool_input / function_call 等字段！"
            ),
            success: false,
            route: "hallucination_detected".to_string(),
        };
    }

    DispatchResult {
        stdout: String::new(),
        stderr: format!("❌ 未知 action='{}', params={:?}", final_action, params),
        success: false,
        route: "unknown".to_string(),
    }
}

// 根据指令执行具体动作
pub fn run_agent_step(
    db: &DbState,
    session_id: &str,
    instruction: &AgentInstruction,
    registry: &mut PluginRegistry,
) -> DispatchResult {
    let tool_name = instruction.get_tool().to_lowercase();

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
            // 提取单步的 action
            let step_action = cmd_val
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();

            println!("   ➡️  Step {}: 执行动作 '{}'...", step_idx, step_action);

            // 执行单步
            let res =
                execute_command_inner(db, session_id, &tool_name, &step_action, cmd_val, registry);

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

    let result = tokio::task::spawn_blocking(move || {
        // 在阻塞线程内部加锁
        let mut registry = futures::executor::block_on(registry_arc.lock());
        run_agent_step(&db, &sid, &instruction, &mut registry)
    })
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

#[tauri::command]
pub async fn run_agent_main_loop(
    app: AppHandle,
    state: State<'_, DbState>,
    registry_state: State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    model_routing: crate::agent::router::ModelRoutingConfig,
    goal: String,
    _auto_pilot: bool,
    session_id: Option<String>,
    log_context: Option<bool>,
) -> Result<(), String> {
    let final_session_id = session_id.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis().to_string())
            .unwrap_or_else(|_| "default".to_string())
    });
    let mut run_machine = AgentRunMachine::new();
    advance_run_state(
        &app,
        &final_session_id,
        &mut run_machine,
        AgentRunEvent::Start,
    )?;
    // 1. 自动加载插件（强制探测模式）
    {
        let cwd = std::env::current_dir().unwrap_or_default();
        println!("========================================");
        println!(
            "🔍 [插件探测] 强制执行开始！当前工作目录 cwd = {}",
            cwd.display()
        );

        let mut registry = registry_state.lock().await;
        registry.clients.clear();
        let mut all_clients = std::collections::HashMap::new();

        // 多路探测：涵盖开发模式、生产模式、src-tauri 作为 cwd 的情况
        let search_paths: Vec<std::path::PathBuf> = vec![
            cwd.join("plugins"),       // cwd/plugins（项目根目录）
            cwd.join("../plugins"),    // cwd/../plugins（从 src-tauri 向上）
            cwd.join("../../plugins"), // 再向上一层保险
            app.path()
                .app_config_dir()
                .unwrap_or_default()
                .join("plugins"), // 系统配置路径
            app.path()
                .resource_dir()
                .unwrap_or_default()
                .join("plugins"), // 打包后资源路径
        ];

        for path in &search_paths {
            println!(
                "  探测路径: {} → {}",
                path.display(),
                if path.exists() {
                    "✅ 存在"
                } else {
                    "❌ 不存在"
                }
            );
        }

        for path in search_paths {
            if path.exists() {
                let reg = PluginRegistry::load_from_dir(&path);
                for (name, client) in reg.clients {
                    println!("🔌 成功挂载插件: {} (来自 {})", name, path.display());
                    all_clients.insert(name, client);
                }
            }
        }
        println!("📊 插件初始化完成，最终可用插件总数: {}", all_clients.len());
        println!("========================================");
        registry.clients = all_clients;
    }

    // 2. 初始化三明治上下文：加载系统提示词 + 动态注入 MCP 工具
    let prompt_path = app
        .path()
        .resource_dir()
        .unwrap_or_default()
        .join("prompts/core_logic.md");
    let base_prompt = std::fs::read_to_string(&prompt_path)
        .or_else(|_| std::fs::read_to_string("prompts/core_logic.md"))
        .unwrap_or_else(|_| "你是一个全自动 Web Agent。".to_string());

    // 2. 动态扫描所有内置工具 (Built-in Tools)
    let task_store_menu = r#"
## 会话任务数据区（内部工具，不是 MCP）
当采集、解析或批处理会产生后续仍要使用的结构化中间数据时，先写入 task_store；不要把完整数据放入 memories、历史或 prompt。
调用格式：tool 固定为 "task_store"，action 为下列动作，参数放在 command 或 params 对象中。
- create_resource：创建数据集。参数 kind、label，可选 resource_id、schema、summary、cursor。
- upsert_records：实时写入或更新记录。参数 resource_id、records 数组；每条记录宜提供 id、record_key 或 key，以保证幂等更新；可同时写 summary、cursor、status。
- list_resources / get_resource：读取数据集目录和元数据。
- list_records：分页读取数据行。参数 resource_id，可选 limit（最多 100）、offset。
- get_record：读取一条记录。参数 resource_id、record_key。
- update_resource：更新资源 label、summary、status、cursor 或 schema。
数据行会在工具返回时立刻持久化，并按当前 session 隔离；模型默认只看到资源目录与摘要，需要具体数据时再分页查询。
"#;
    let mut local_tools_menu = String::from("\n## 内置本地工具 (Built-in Tools)\n");
    let tools_dir = app
        .path()
        .resource_dir()
        .unwrap_or_default()
        .join("prompts/tools");
    let tools_dir_dev = std::path::PathBuf::from("prompts/tools");

    let entries = std::fs::read_dir(&tools_dir)
        .or_else(|_| std::fs::read_dir(&tools_dir_dev))
        .unwrap_or_else(|_| return std::fs::read_dir(".").unwrap()); // 容错处理

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("md") {
            let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if let Ok(content) = std::fs::read_to_string(&path) {
                // 抓取简短的 summary 标签内容
                let summary = content
                    .find("<summary>")
                    .and_then(|start| {
                        content
                            .find("</summary>")
                            .map(|end| &content[start + 9..end])
                    })
                    .unwrap_or("无详细描述");
                local_tools_menu.push_str(&format!("- {}: {}\n", name, summary));
            }
        }
    }

    // 3. 查询所有 MCP Server 的工具列表，追加到菜单里
    let mcp_tools_menu = {
        let mut registry = registry_state.lock().await;
        registry.format_tools_menu()
    };

    let system_prompt = format!(
        "{}\n{}\n{}\n{}",
        base_prompt, task_store_menu, local_tools_menu, mcp_tools_menu
    );
    println!(
        "--- 最终系统提示词 (级联菜单模式) ---\n{}\n------------------",
        system_prompt
    );
    let mut context = context::SandwichContext::new(system_prompt, goal.clone());
    // checkpoint 是恢复的权威来源：只还原可持久化事实，连接、插件与工具详情由 Runtime 重建。
    let mut checkpoint = match load_checkpoint(state.inner(), &final_session_id)? {
        Some(existing) if !existing.phase.is_terminal() && existing.data.user_goal == goal => {
            existing.restore_context(&mut context);
            existing
        }
        Some(existing) => {
            // 同一 session 已结束或目标改变时，以乐观锁 revision 覆盖为新的运行快照。
            let mut fresh = AgentCheckpoint::new(final_session_id.clone(), &context);
            fresh.version = existing.version;
            fresh
        }
        None => AgentCheckpoint::new(final_session_id.clone(), &context),
    };
    // 数据区本体是权威来源；每次启动都刷新目录，checkpoint 仅保留这份目录快照。
    context.set_task_resources(task_store::list_resource_refs(
        state.inner(),
        &final_session_id,
    )?);
    checkpoint.refresh_context(&context);
    save_checkpoint(state.inner(), &mut checkpoint)?;
    // 仅在从 phase=RunTool 且尚无 pending 的 checkpoint 恢复时使用。
    let mut recovered_instruction: Option<AgentInstruction> = None;
    let mut action_history: std::collections::VecDeque<String> = std::collections::VecDeque::new();

    // 注册人类干预接收器
    let (tx, mut rx) = mpsc::channel::<String>(5);
    {
        let mut channels = intervention_channels().lock().await;
        channels.insert(final_session_id.clone(), tx);
    }

    advance_run_state(
        &app,
        &final_session_id,
        &mut run_machine,
        AgentRunEvent::RuntimeReady,
    )?;
    // WaitUser 与 pending 调用不能从普通上下文可靠反推；恢复时显式读取 checkpoint。
    match recovery_decision(&checkpoint) {
        RecoveryDecision::ResumePlanning | RecoveryDecision::Terminal => {}
        RecoveryDecision::ResumeTool => {
            recovered_instruction = checkpoint.prepared_instruction.clone();
            if recovered_instruction.is_none() {
                return Err("checkpoint phase=run_tool 但缺少 prepared instruction".to_string());
            }
        }
        RecoveryDecision::ResumeWaiting
        | RecoveryDecision::RequireHumanReview
        | RecoveryDecision::RetryPending
        | RecoveryDecision::ProbePending => {
            let reason = match checkpoint.pending.as_ref() {
                Some(pending) => format!(
                    "系统从 checkpoint 恢复：工具 {} / 动作 {} 的结果不确定（call_id: {}，已尝试 {} 次）。为避免重复副作用，请确认是否继续。",
                    pending.tool, pending.action, pending.call_id, pending.attempts
                ),
                None => checkpoint
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "系统恢复后正在等待人类确认。".to_string()),
            };

            checkpoint.wait_for_human(reason.clone(), &context);
            save_checkpoint(state.inner(), &mut checkpoint)?;
            advance_run_state(
                &app,
                &final_session_id,
                &mut run_machine,
                AgentRunEvent::InstructionReady {
                    action: "checkpoint_recovery".to_string(),
                },
            )?;
            advance_run_state(
                &app,
                &final_session_id,
                &mut run_machine,
                AgentRunEvent::WaitForHuman {
                    question: reason.clone(),
                },
            )?;
            app.emit(
                &format!("agent-progress-{}", final_session_id),
                json!({ "type": "agent_paused", "message": reason }),
            )
            .map_err(|error| error.to_string())?;

            let reply = rx
                .recv()
                .await
                .unwrap_or_else(|| "人类未回复，通道异常关闭".to_string());
            context.add_error_feedback(&format!("【checkpoint 恢复后的人工确认】{}", reply));
            checkpoint.begin_planning(checkpoint.step_id, &context);
            save_checkpoint(state.inner(), &mut checkpoint)?;
            advance_run_state(
                &app,
                &final_session_id,
                &mut run_machine,
                AgentRunEvent::HumanReplyReceived,
            )?;
        }
    }

    // durable cursor 是调度权威：每个普通步骤提交后推进 step_id；恢复时也从这里继续。
    // 不再使用启动瞬间捕获范围的 for-loop，避免 checkpoint 更新后调度器仍沿旧游标运行。
    while checkpoint.step_id < MAX_STEPS {
        let step_id = checkpoint.step_id;
        let resume_instruction = recovered_instruction.take();
        if resume_instruction.is_none() {
            checkpoint.begin_planning(step_id, &context);
            save_checkpoint(state.inner(), &mut checkpoint)?;
        }
        advance_run_state(
            &app,
            &final_session_id,
            &mut run_machine,
            AgentRunEvent::BeginPlanning { step_id },
        )?;
        app.emit(
            &format!("agent-log-{}", final_session_id),
            format!("正在规划第 {} 步...", step_id + 1),
        )
        .map_err(|e| e.to_string())?;

        // ======== [核心注入：人类干预监听点] ========
        while let Ok(human_msg) = rx.try_recv() {
            println!("🚨 收到人类干预指令: {}", human_msg);
            let intervention_format = format!(
                "🚨【系统最高警报：人类干预】🚨\n\
                请立即放弃原计划并彻底服从以下人类最新指令：\n\
                “{}”\n\
                要求：在下一轮 reflection 中承认收到指令，并在 todo_update 中重写计划！",
                human_msg
            );
            context
                .turns_history
                .push(crate::agent::context::ChatMessage {
                    role: "user".to_string(),
                    content: serde_json::json!(intervention_format),
                });
            app.emit(
                &format!("agent-log-{}", final_session_id),
                "👑 人类干预指令已成功注入 AI 视野！",
            )
            .ok();
        }
        // ==========================================

        // 修复：绝不可以在这里清空 current_observation，因为这会导致 AI 看不到刚获取的页面数据
        // ================================================================
        // 统一重试循环：整个"规划 + 执行"作为一个原子操作，失败就重试
        // ================================================================
        let (instruction, result) = run_call_llm_and_run_tool_phases(
            &app,
            &state,
            &registry_state,
            &model_routing,
            &goal,
            log_context,
            &final_session_id,
            step_id,
            &mut context,
            &mut checkpoint,
            &mut run_machine,
            &mut rx,
            resume_instruction,
        )
        .await?;

        // ================================================================
        // 以下是成功跳出规划+执行重试循环后的处理逻辑
        // ================================================================

        // --- 网关层硬拦截：死循环检测 ---
        action_history.push_back(instruction.get_action().to_lowercase());
        if action_history.len() > 4 {
            action_history.pop_front();
        }

        // 连续 3 次 extract 才触发（放宽阈值，避免合理的"extract→等待→extract"被误判）
        let is_loop = if action_history.len() >= 3 {
            let len = action_history.len();
            action_history[len - 3] == "extract"
                && action_history[len - 2] == "extract"
                && action_history[len - 1] == "extract"
        } else {
            false
        };

        // extract → scroll → extract 这个死循环模式依然保留
        let is_loop_3 = if action_history.len() >= 3 {
            let len = action_history.len();
            let last_three = (
                &action_history[len - 3],
                &action_history[len - 2],
                &action_history[len - 1],
            );
            last_three.0 == "extract" && last_three.1 == "scroll" && last_three.2 == "extract"
        } else {
            false
        };

        if is_loop || is_loop_3 {
            let error_detail = "【🚨 网关强制阻断】: 系统检测到你陷入了认知死胡同（连续无意义的 extract 循环）！当前页面的目标元素大概率不存在，或者你理解错了页面状态！请立刻通过 `back` 返回上一页，或者更改搜索关键词重新 `goto`，严禁再次执行 extract！";
            context.add_error_feedback(error_detail);
            app.emit(
                &format!("agent-log-{}", final_session_id),
                "🚨 网关层拦截：发现违规连续 extract 操作，强制注入纠偏机制！",
            )
            .ok();
            action_history.clear(); // 阻断后清空历史，重新积累
        }

        // --- 更新任务规划 + 核心记忆 ---
        if !instruction.todo_update.is_empty() {
            context.todo_list = instruction.todo_update.clone();
        }
        if !instruction.memories_update.is_empty() {
            context.update_memories(instruction.memories_update.clone());
            println!("📝 AI 更新了核心记忆: {:?}", instruction.memories_update);
        }
        if let Some(ref summary) = instruction.progress_summary {
            context.progress_summary = summary.clone();
            println!("📝 AI 更新了进度摘要: {}", summary);
        }

        // --- 核心增强：历史截断与缓存优化 ---
        if instruction.clear_history.unwrap_or(false) {
            println!("🧹 AI 主动触发了历史清理 (clear_history: true)!");
            context.turns_history.clear();
        }

        // --- 核心增强：Token 优化与冷存储检索 ---
        // AI 在本轮请求 "require_memory: true" 或触发了清空历史，下一轮组装消息时就会塞入全部 Fact 内容
        context.carry_memories = instruction.require_memory.unwrap_or(false)
            || instruction.clear_history.unwrap_or(false);

        // --- 预加载下一轮工具说明书 ---
        let next_tool = instruction
            .next_tool_hint
            .clone()
            .unwrap_or_else(|| instruction.get_action());
        context.active_tool = Some(next_tool.clone());

        // --- 加载下一轮工具说明书 ---
        let tool_filename = format!("{}.md", context.active_tool.as_deref().unwrap_or("core"));
        let resource_dir = app.path().resource_dir().unwrap_or_default();
        let tool_path_production = resource_dir.join("prompts/tools").join(&tool_filename);
        let tool_path_dev = std::path::PathBuf::from("prompts/tools").join(&tool_filename);

        if let Ok(tool_md) = std::fs::read_to_string(&tool_path_production)
            .or_else(|_| std::fs::read_to_string(&tool_path_dev))
        {
            context.active_tool_detail = tool_md;
        } else {
            let plugin_name = next_tool.split('/').next().unwrap_or(&next_tool);
            let detail = {
                let mut registry = registry_state.lock().await;
                if registry.plugin_names().contains(&plugin_name) {
                    registry.format_tool_detail(plugin_name)
                } else {
                    String::new()
                }
            };
            if detail.is_empty() {
                context.active_tool_detail.clear();
            } else {
                context.active_tool_detail = detail;
            }
        }

        // --- 刷新独立任务数据区目录，再把轻量引用写进 checkpoint ---
        // 数据行早已由 task_store 自己实时持久化，绝不复制到 context。
        context.set_task_resources(task_store::list_resource_refs(
            state.inner(),
            &final_session_id,
        )?);

        // --- 记录历史 + 更新观测 ---
        let final_stdout = result.stdout.clone();
        let output_text = if result.success {
            final_stdout.clone()
        } else {
            format!("❌ {}\n{}", result.stderr, final_stdout)
        };

        context.add_step(&instruction, &output_text);

        context.current_observation = output_text.clone();
        // 工具结果已经进入可恢复上下文。先提交 durable phase/cursor，再通知前端；
        // 这样崩溃恢复不会把已经完成的普通步骤当作待执行，也不会把 finish 误续跑。
        let is_finish =
            instruction.tool.as_deref() == Some("finish") || instruction.get_action() == "finish";
        if is_finish {
            checkpoint.mark_done(&context);
        } else {
            checkpoint.complete_step(step_id, &context);
        }
        save_checkpoint(state.inner(), &mut checkpoint)?;

        // --- 前端通知 + 任务完成判定（合并） ---
        app.emit(&format!("agent-context-{}", final_session_id), &context)
            .map_err(|e| e.to_string())?;

        if is_finish {
            advance_run_state(
                &app,
                &final_session_id,
                &mut run_machine,
                AgentRunEvent::BeginFinalizing,
            )?;
            // finish 指令：直接发 complete，不走普通的 step_done
            // 尝试从 command 里解析 finish 的结构化内容
            let finish_payload = instruction
                .command
                .clone()
                .unwrap_or(serde_json::Value::Null);

            app.emit(
                &format!("agent-progress-{}", final_session_id),
                json!({
                    "type": "complete",
                    "message": "任务已由 AI 标记完成",
                    "success": true,
                    "output": output_text,      // 原始 JSON 字符串（兼容旧逻辑）
                    "finish": finish_payload,   // 结构化成果对象，含 details/summary/artifacts
                }),
            )
            .map_err(|e| e.to_string())?;
            break;
        }

        // 普通步骤：发 step_done / step_error
        app.emit(
            &format!("agent-progress-{}", final_session_id),
            json!({
                "type": if result.success { "step_done" } else { "step_error" },
                "step_id": step_id,
                "description": instruction.description,
                "output": output_text
            }),
        )
        .map_err(|e| e.to_string())?;

        sleep(Duration::from_millis(500)).await;
    }

    if run_machine.state() == AgentRunState::Finalizing {
        advance_run_state(
            &app,
            &final_session_id,
            &mut run_machine,
            AgentRunEvent::Succeed {
                summary: "任务已完成".to_string(),
            },
        )?;
    } else if !run_machine.state().is_terminal() {
        let terminal_error = format!("任务在达到最大步数 {} 后未完成", MAX_STEPS);
        advance_run_state(
            &app,
            &final_session_id,
            &mut run_machine,
            AgentRunEvent::Fail {
                reason: terminal_error.clone(),
            },
        )?;
        checkpoint.mark_failed(terminal_error, &context);
        save_checkpoint(state.inner(), &mut checkpoint)?;
    }
    {
        let mut channels = intervention_channels().lock().await;
        channels.remove(&final_session_id);
    }

    Ok(())
}

/// 执行一个可恢复步骤的 CallLlm 与 RunTool 阶段。
///
/// 该 handler 封装模型规划、工具派发、人工等待、重试和救援；每个外部副作用
/// 都由 checkpoint 在调用前后记录。主调度器只负责启动步骤和提交已完成结果。
async fn run_call_llm_and_run_tool_phases(
    app: &AppHandle,
    state: &State<'_, DbState>,
    registry_state: &State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    model_routing: &crate::agent::router::ModelRoutingConfig,
    _goal: &String,
    log_context: Option<bool>,
    final_session_id: &String,
    step_id: usize,
    context: &mut context::SandwichContext,
    mut checkpoint: &mut AgentCheckpoint,
    mut run_machine: &mut AgentRunMachine,
    rx: &mut mpsc::Receiver<String>,
    resume_instruction: Option<AgentInstruction>,
) -> Result<(AgentInstruction, DispatchResult), String> {
    let mut retry_count = 0;
    let max_retries = 3;

    // LLM 回复已落盘但工具尚未开始时，恢复直接进入同一工具处理器。
    // 这样不会为同一个决策额外消耗一次模型调用，也不会绕过工具前的 pending 提交。
    if let Some(inst) = resume_instruction {
        advance_run_state(
            &app,
            &final_session_id,
            &mut run_machine,
            AgentRunEvent::InstructionReady {
                action: inst.get_action(),
            },
        )?;
        let dispatch_result = run_tool_phase(
            app,
            state,
            registry_state,
            final_session_id,
            step_id,
            &inst,
            context,
            checkpoint,
            run_machine,
            rx,
        )
        .await?;
        if dispatch_result.success || inst.get_action() == "finish" {
            return Ok((inst, dispatch_result));
        }

        let error_detail = format!("【❌ 恢复后工具执行失败】: {}", dispatch_result.stderr);
        advance_run_state(
            &app,
            &final_session_id,
            &mut run_machine,
            AgentRunEvent::ToolFailed {
                reason: error_detail.clone(),
            },
        )?;
        context.add_error_feedback(&error_detail);
        checkpoint.record_recoverable_error(error_detail, context);
        save_checkpoint(state.inner(), checkpoint)?;
        advance_run_state(
            &app,
            &final_session_id,
            &mut run_machine,
            AgentRunEvent::RetryPlanning,
        )?;
    }

    let (instruction, result) = loop {
        // --- 🚦 调用网关：动态路由 ---
        let routing = crate::agent::router::AgentRouter::route(&context, &model_routing);
        let _ = app.emit(
            &format!("agent-log-{}", final_session_id),
            format!(
                "🚦 [网关] {} → {} ({})",
                routing.tier, routing.model_id, routing.reason
            ),
        );

        // --- A. 请求 AI 规划 ---
        let (mut inst, token_usage, thinking_text) = match call_llm(
            &context,
            &state,
            routing.model_id.clone(),
            Some(&app),
            step_id,
            &final_session_id,
            log_context.unwrap_or(false),
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                retry_count += 1;
                advance_run_state(
                    &app,
                    &final_session_id,
                    &mut run_machine,
                    AgentRunEvent::PlanningFailed { reason: e.clone() },
                )?;
                // 🔑 关键修复：无论解析失败还是成功，都要让前端看到这一步
                let err_desc = format!("⚠️ AI 输出格式错误（第{}次）", retry_count);
                app.emit(
                    &format!("agent-progress-{}", final_session_id),
                    json!({
                        "type": "step_new",
                        "step": {
                            "id": step_id,
                            "description": err_desc,
                            "reflection": "",
                            "thought": e.clone(),
                            "tool": "error",
                            "command": "",
                            "status": "error",
                            "output": e.clone()
                        }
                    }),
                )
                .ok();
                app.emit(
                    &format!("agent-progress-{}", final_session_id),
                    json!({
                        "type": "step_error",
                        "step_id": step_id,
                        "description": format!("AI 输出格式错误（第{}次）", retry_count),
                        "output": format!("JSON 解析失败: {}\n\n原始输出请查看终端日志", e)
                    }),
                )
                .ok();
                if retry_count >= max_retries {
                    app.emit(
                        &format!("agent-progress-{}", final_session_id),
                        json!({
                            "type": "complete",
                            "message": format!("AI 持续输出非法 JSON，任务终止: {}", e),
                            "success": false
                        }),
                    )
                    .ok();
                    let terminal_error = format!("AI 无法解析 JSON: {}", e);
                    advance_run_state(
                        &app,
                        &final_session_id,
                        &mut run_machine,
                        AgentRunEvent::Fail {
                            reason: terminal_error.clone(),
                        },
                    )?;
                    checkpoint.mark_failed(terminal_error.clone(), &context);
                    save_checkpoint(state.inner(), &mut checkpoint)?;
                    return Err(terminal_error);
                }
                app.emit(
                    &format!("agent-log-{}", final_session_id),
                    format!("🔄 格式错误(第{}次)，正在自我修复...", retry_count),
                )
                .map_err(|er| er.to_string())?;
                context.add_error_feedback(&e);
                advance_run_state(
                    &app,
                    &final_session_id,
                    &mut run_machine,
                    AgentRunEvent::RetryPlanning,
                )?;
                continue;
            }
        };

        inst.description = format!("[{}] {}", routing.tier, inst.description);
        // LLM 的结构化决策已经完整到达；在任何 UI 通知或工具调用之前提交它。
        checkpoint.record_instruction(inst.clone(), context);
        save_checkpoint(state.inner(), checkpoint)?;
        advance_run_state(
            &app,
            &final_session_id,
            &mut run_machine,
            AgentRunEvent::InstructionReady {
                action: inst.get_action(),
            },
        )?;

        // 📊 发送 Token 用量统计到前端
        app.emit(
            &format!("agent-progress-{}", final_session_id),
            json!({
                "type": "token_usage",
                "step_id": step_id,
                "prompt_tokens": token_usage.prompt_tokens,
                "completion_tokens": token_usage.completion_tokens,
                "total_tokens": token_usage.total_tokens,
                "context_window": token_usage.context_window,
                "usage_percent": token_usage.usage_percent,
                "summary": token_usage.summary()
            }),
        )
        .map_err(|e| e.to_string())?;
        println!("{}", token_usage.summary());

        // 🧠 发送最终思考完成事件到前端
        if !thinking_text.is_empty() {
            app.emit(
                &format!("agent-progress-{}", final_session_id),
                json!({
                    "type": "thinking",
                    "step_id": step_id,
                    "content": &thinking_text,
                    "done": true
                }),
            )
            .map_err(|e| e.to_string())?;
            app.emit(
                &format!("agent-log-{}", final_session_id),
                format!("🧠 AI思考完毕 (共{}字)", thinking_text.len()),
            )
            .map_err(|e| e.to_string())?;
        }

        // 通知前端：发现了新计划（含反思）
        if !inst.reflection.is_empty() {
            app.emit(
                &format!("agent-log-{}", final_session_id),
                format!("🪞 AI 反思: {}", inst.reflection),
            )
            .map_err(|e| e.to_string())?;
        }
        app.emit(
            &format!("agent-log-{}", final_session_id),
            format!("🤖 AI 规划了新动作: {}", inst.description),
        )
        .map_err(|e| e.to_string())?;
        app.emit(
            &format!("agent-progress-{}", final_session_id),
            json!({
                "type": "step_new",
                "step": {
                    "id": step_id,
                    "description": inst.description.clone(),
                    "reflection": inst.reflection.clone(),
                    "thought": inst.thought.clone(),
                    "tool": inst.get_action(),
                    "command": inst.get_params().to_string(),
                    "status": "pending",
                    "output": ""
                }
            }),
        )
        .map_err(|e| e.to_string())?;

        let dispatch_result = run_tool_phase(
            app,
            state,
            registry_state,
            final_session_id,
            step_id,
            &inst,
            &mut *context,
            &mut *checkpoint,
            &mut *run_machine,
            rx,
        )
        .await?;

        // --- C. 判断执行结果 ---
        if dispatch_result.success || inst.get_action() == "finish" {
            break (inst, dispatch_result); // 成功！
        } else {
            retry_count += 1;
            let error_detail = format!("【❌ 执行失败】: {}", dispatch_result.stderr);
            advance_run_state(
                &app,
                &final_session_id,
                &mut run_machine,
                AgentRunEvent::ToolFailed {
                    reason: error_detail.clone(),
                },
            )?;

            if let Ok(raw_json) = serde_json::to_string(&inst) {
                context.add_assistant_raw_message(&raw_json);
            }

            // 【修复：重试时的工具盲区】如果 AI 尝试使用的工具和当前加载的说明书不一致，立即为其加载正确的说明书，防止它在 retry 时因为看不到语法而继续瞎猜报错。
            let attempted_tool = inst.get_action();
            if context.active_tool.as_deref() != Some(&attempted_tool) && attempted_tool != "finish"
            {
                context.active_tool = Some(attempted_tool.clone());
                let tool_filename = format!("{}.md", attempted_tool);
                let resource_dir = app.path().resource_dir().unwrap_or_default();
                let tool_path_production = resource_dir.join("prompts/tools").join(&tool_filename);
                let tool_path_dev = std::path::PathBuf::from("prompts/tools").join(&tool_filename);

                if let Ok(tool_md) = std::fs::read_to_string(&tool_path_production)
                    .or_else(|_| std::fs::read_to_string(&tool_path_dev))
                {
                    context.active_tool_detail = tool_md;
                    println!(
                        "🔄 [重试修正] 为 AI 紧急加载了工具说明书: {}",
                        attempted_tool
                    );
                } else {
                    // 尝试从 MCP 插件中获取
                    let plugin_name = attempted_tool.split('/').next().unwrap_or(&attempted_tool);
                    let detail = {
                        let mut registry =
                            futures::executor::block_on(registry_state.inner().lock());
                        if registry.plugin_names().contains(&plugin_name) {
                            registry.format_tool_detail(plugin_name)
                        } else {
                            String::new()
                        }
                    };
                    if !detail.is_empty() {
                        context.active_tool_detail = detail;
                        println!(
                            "🔄 [重试修正] 为 AI 紧急加载了 MCP 插件说明书: {}",
                            attempted_tool
                        );
                    }
                }
            }

            if retry_count >= max_retries {
                // 最终失败：通知前端完成（带错误）
                app.emit(
                    &format!("agent-progress-{}", final_session_id),
                    json!({
                        "type": "complete",
                        "message": format!("执行多次失败，任务终止: {}", error_detail),
                        "success": false
                    }),
                )
                .ok();
                advance_run_state(
                    &app,
                    &final_session_id,
                    &mut run_machine,
                    AgentRunEvent::Fail {
                        reason: error_detail.clone(),
                    },
                )?;
                checkpoint.mark_failed(error_detail.clone(), &context);
                save_checkpoint(state.inner(), &mut checkpoint)?;
                return Err(error_detail);
            }

            app.emit(
                &format!("agent-log-{}", final_session_id),
                format!(
                    "🔄 执行失败(第{}次)，正注入错误反馈给 AI 重试...",
                    retry_count
                ),
            )
            .map_err(|e| e.to_string())?;
            context.add_error_feedback(&error_detail);

            checkpoint.record_recoverable_error(error_detail.clone(), &context);
            save_checkpoint(state.inner(), &mut checkpoint)?;
            continue; // 带着错误信息，重新规划！
        }
    };

    Ok((instruction, result))
}

/// 执行持久化状态机的 RunTool 阶段。
///
/// 调用前为未知或可能有副作用的操作创建 pending checkpoint；人工等待也以
/// WaitUser 阶段保存。所有旧进度事件仍由该处理器原样发送。
async fn run_tool_phase(
    app: &AppHandle,
    state: &State<'_, DbState>,
    registry_state: &State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    final_session_id: &String,
    step_id: usize,
    inst: &AgentInstruction,
    context: &mut context::SandwichContext,
    mut checkpoint: &mut AgentCheckpoint,
    mut run_machine: &mut AgentRunMachine,
    rx: &mut mpsc::Receiver<String>,
) -> Result<DispatchResult, String> {
    // --- B. 执行动作 ---
    app.emit(
        &format!("agent-log-{}", final_session_id),
        format!("▶ 步骤 {}: {}", step_id + 1, inst.description),
    )
    .map_err(|e| e.to_string())?;
    app.emit(
        &format!("agent-progress-{}", final_session_id),
        json!({
            "type": "step_start",
            "step_id": step_id,
            "description": &inst.description
        }),
    )
    .map_err(|e| e.to_string())?;

    // --- B1. 特殊拦截：AI 主动挂起呼叫人类 ---
    let dispatch_result = if inst.get_tool().to_lowercase() == "ask_human" {
        let ask_msg = inst
            .get_params()
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("AI遇到了麻烦，请求人类协助！")
            .to_string();

        app.emit(
            &format!("agent-log-{}", final_session_id),
            format!("⏸️ AI 已挂起，等待人类协助: {}", ask_msg),
        )
        .ok();

        // 通知前端进入暂停状态
        app.emit(
            &format!("agent-progress-{}", final_session_id),
            json!({
                "type": "agent_paused",
                "message": ask_msg
            }),
        )
        .ok();

        println!("⏸️ [阻塞点] 模型触发 ask_human 挂起，正在死等前端人类唤醒...");

        // 真正的阻塞点：死等人类回复！
        // rx 被我们在外层定义过了，这里可以直接 `.recv().await`
        checkpoint.wait_for_human(ask_msg.clone(), &context);
        save_checkpoint(state.inner(), &mut checkpoint)?;
        advance_run_state(
            &app,
            &final_session_id,
            &mut run_machine,
            AgentRunEvent::WaitForHuman {
                question: ask_msg.clone(),
            },
        )?;
        let reply = rx
            .recv()
            .await
            .unwrap_or_else(|| "人类未回复，通道异常关闭".to_string());
        context.add_error_feedback(&format!("【人类回复】{}", reply));
        checkpoint.begin_planning(step_id, &context);
        save_checkpoint(state.inner(), &mut checkpoint)?;
        advance_run_state(
            &app,
            &final_session_id,
            &mut run_machine,
            AgentRunEvent::HumanReplyReceived,
        )?;

        println!("▶️ [唤醒] 人类回复: {}", reply);

        app.emit(
            &format!("agent-log-{}", final_session_id),
            format!("▶️ AI 已被唤醒，人类回复: {}", reply),
        )
        .ok();

        // 伪造一个成功的执行结果，把人类的话当作工具输出返回给大模型
        crate::agent::types::DispatchResult {
            success: true,
            stdout: format!("人类的回复是: {}", reply),
            stderr: String::new(),
            route: "ask_human_resolved".to_string(),
        }
    } else {
        // 外部副作用前先持久化 pending；未知/写操作默认 HumanReview。
        let recovery_policy = checkpoint.prepare_tool(inst.clone(), &context);
        if recovery_policy.requires_pending() {
            save_checkpoint(state.inner(), &mut checkpoint)?;
        }

        // 具体和本地工具交互 (非 ask_human 走普通逻辑)
        let inst_c = inst.clone();
        let registry_arc = registry_state.inner().clone();
        let db = state.inner().clone();
        let sid = final_session_id.clone();
        tokio::task::spawn_blocking(move || {
            let mut registry = futures::executor::block_on(registry_arc.lock());
            println!("执行动作: {:?}", inst_c);
            run_agent_step(&db, &sid, &inst_c, &mut registry)
        })
        .await
        .map_err(|e| e.to_string())?
    };

    Ok(dispatch_result)
}
