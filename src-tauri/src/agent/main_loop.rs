use crate::agent::checkpoint::{
    load_checkpoint, recovery_decision, save_checkpoint, AgentCheckpoint, AgentPhase,
    RecoveryDecision,
};
use crate::agent::context;
use crate::agent::control::{
    finish_interrupted_run, intervention_channels, stream_interrupt_channels,
    take_pending_interrupt, HumanControl,
};
use crate::agent::dispatcher::is_finish_instruction;
use crate::agent::loop_runner::run_call_llm_and_run_tool_phases;
use crate::agent::mcp::{McpClient, PluginRegistry};
use crate::agent::run_state::{AgentRunEvent, AgentRunMachine, AgentRunState};
use crate::agent::task_store;
use crate::agent::types::{AgentInstruction, RUN_INTERRUPTED_ROUTE};
use crate::db::DbState;
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, watch};
use tokio::time::{sleep, Duration};

/// Agent 每个任务最多执行步数（可按项目需求调整）
const MAX_STEPS: usize = 300;

/// 推进后端运行状态，并向前端发送独立于旧进度事件的权威状态快照。
/// 旧的 agent-progress-* 事件保持不变，以便前端逐步迁移。
pub fn advance_run_state(
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
    // 每次用户发送任务（包括在同一会话中继续对话）都是一个独立的可复盘运行。
    // 该轨迹只服务历史 UI，不参与 checkpoint 恢复或模型上下文。
    let run_id = uuid::Uuid::new_v4().to_string();
    let mut run_trace_steps: Vec<serde_json::Value> = Vec::new();
    let mut run_machine = AgentRunMachine::new();
    advance_run_state(
        &app,
        &final_session_id,
        &mut run_machine,
        AgentRunEvent::Start,
    )?;
    if let Err(error) = crate::agent::run_trace::upsert_snapshot(
        state.inner(),
        &run_id,
        &final_session_id,
        &goal,
        "running",
        &run_trace_steps,
        None,
    ) {
        eprintln!("[run-trace] 无法创建运行轨迹: {error}");
    }
    // Skill、插件与 shell 工具共享本轮运行时工作目录；恢复时会重新读取并按哈希复用未变更的 Skill。
    let workspace_dir = std::env::current_dir().unwrap_or_default();
    // 1. 智能 MCP 插件同步：首次任务全量初始化，后续任务仅增量保活（彻底消除每次 spawn 的秒级延迟）
    let _ = app.emit(
        &format!("agent-progress-{}", final_session_id),
        json!({
            "type": "action_phase",
            "phase": "mcp",
            "description": "🔍 正在同步 MCP 插件与存活状态..."
        }),
    );
    {
        let cwd = workspace_dir.clone();
        let mut registry = registry_state.lock().await;
        let is_first_init = registry.clients.is_empty();

        if is_first_init {
            // 首次：多路扫描 YAML 插件目录，全量 spawn
            println!("🔍 [插件探测] 首次初始化，扫描 YAML 插件目录...");
            let search_paths: Vec<std::path::PathBuf> = vec![
                cwd.join("plugins"),
                cwd.join("../plugins"),
                cwd.join("../../plugins"),
                app.path()
                    .app_config_dir()
                    .unwrap_or_default()
                    .join("plugins"),
                app.path()
                    .resource_dir()
                    .unwrap_or_default()
                    .join("plugins"),
            ];
            for path in search_paths {
                if path.exists() {
                    let reg = PluginRegistry::load_from_dir(&path);
                    for (name, client) in reg.clients {
                        println!("🔌 挂载 YAML 插件: {}", name);
                        registry.clients.insert(name, client);
                    }
                }
            }
        }

        // DB 管理插件：每次增量同步（新增 → spawn，禁用 → 移除，已有保留）
        let managed_plugins = crate::commands::load_managed_mcp_plugins(state.inner())?;
        for managed in managed_plugins {
            let name = managed.config.name.clone();
            if !managed.enabled {
                if registry.clients.remove(&name).is_some() {
                    println!("⏸️ 卸载已禁用 DB 插件: {}", name);
                }
                continue;
            }
            if !registry.clients.contains_key(&name) {
                // 新增插件 → spawn
                match McpClient::spawn(managed.config) {
                    Ok(client) => {
                        let client_arc = Arc::new(client);
                        let c = client_arc.clone();
                        let n = name.clone();
                        tokio::spawn(async move {
                            let _ = c.initialize().await;
                            if let Ok(tools) = c.list_tools().await {
                                println!("🔌 DB 插件就绪: {} ({} 个工具)", n, tools.len());
                            }
                        });
                        println!("🔌 挂载新 DB 插件: {}", name);
                        registry.clients.insert(name, client_arc);
                    }
                    Err(error) => eprintln!("❌ DB 插件启动失败 [{}]: {}", name, error),
                }
            }
        }

        println!(
            "📊 MCP 插件就绪: {} 个（首次初始化: {}）",
            registry.clients.len(),
            is_first_init
        );
    }

    // 2. 初始化三明治上下文：加载系统提示词 + 可视化/HTML沙箱 Skill + 动态注入 MCP 工具
    let _ = app.emit(
        &format!("agent-progress-{}", final_session_id),
        json!({
            "type": "action_phase",
            "phase": "skills",
            "description": "📚 正在装配系统提示词与 Skill 缓存..."
        }),
    );
    let core_logic_embedded = include_str!("../../prompts/core_logic.md");

    let prompt_path = app
        .path()
        .resource_dir()
        .unwrap_or_default()
        .join("prompts/core_logic.md");
    let base_prompt = std::fs::read_to_string(&prompt_path)
        .or_else(|_| std::fs::read_to_string("src-tauri/prompts/core_logic.md"))
        .or_else(|_| std::fs::read_to_string("prompts/core_logic.md"))
        .unwrap_or_else(|_| core_logic_embedded.to_string());

    // 2. 动态扫描所有内置工具 (Built-in Tools)
    let task_store_menu = r#"
## 会话任务数据区 task_store 动作详情
当采集、解析或批处理产生结构化中间数据时，写入 task_store（tool: "task_store"），参数放在 command 对象中：
- create_resource：创建数据集。参数 kind、label，可选 resource_id、schema、summary、cursor。
- upsert_records：实时写入或更新记录。参数 resource_id、records 数组；每条记录宜提供 id 或 record_key，以保证幂等更新；可同时写 summary、cursor、status。
- list_resources / get_resource：读取数据集目录和元数据。
- list_records：分页读取数据行。参数 resource_id，可选 limit（最多 100）、offset。
- get_record：读取一条记录。参数 resource_id、record_key。
- update_resource：更新资源 label、summary、status、cursor 或 schema。
数据行会在工具返回时立刻持久化，并按当前 session 隔离；模型默认只看到资源目录与摘要，需要具体数据时再分页查询。
"#;
    let mut local_tools_menu = String::from("\n## 内置本地工具 (Built-in Tools)\n");
    let candidate_dirs = [
        std::path::PathBuf::from("src-tauri/prompts/tools"),
        std::path::PathBuf::from("prompts/tools"),
        std::path::PathBuf::from("../src-tauri/prompts/tools"),
        app.path()
            .resource_dir()
            .unwrap_or_default()
            .join("prompts/tools"),
    ];

    let mut found_any = false;
    for dir in &candidate_dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut read_count = 0;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("md") {
                    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        let summary = content
                            .find("<summary>")
                            .and_then(|start| {
                                content
                                    .find("</summary>")
                                    .map(|end| &content[start + 9..end])
                            })
                            .unwrap_or("无详细描述");
                        local_tools_menu.push_str(&format!("- {}: {}\n", name, summary));
                        read_count += 1;
                    }
                }
            }
            if read_count > 0 {
                found_any = true;
                break;
            }
        }
    }
    if !found_any {
        local_tools_menu.push_str("- finish: 终态交付动作\n- ask_human: 询问人类或请求确认\n- model_manager: 模型规格与渠道配置管理工具\n- mcp_manager: 外部 MCP 插件接入与能力卡管理工具\n- skill_manager: 工作区 Skill 与自定义提示词管理工具\n- web_search: 全网实时搜索引擎\n- web_fetch: 网页正文 Markdown 提取工具\n- task_store: 结构化数据存储与分页检索\n- search: 代码与历史日志联邦检索\n");
    }

    // 3. 从当前工作区向上读取 AGENTS.md / HUB.md；内容未变时走进程内哈希缓存。
    let workspace_skill_context =
        crate::agent::skill_context::workspace_skill_context(&workspace_dir);
    // 4. 用户在能力管理页维护的条目只作为补充上下文；读取失败不能阻断正常 Agent 运行。
    let custom_instruction_context = crate::commands::load_enabled_custom_instruction_context(
        state.inner(),
    )
    .unwrap_or_else(|error| {
        eprintln!("加载自定义提示词与 Skill 失败: {}", error);
        String::new()
    });
    let conversation_model_id = if !model_routing.pro.trim().is_empty() {
        model_routing.pro.clone()
    } else if !model_routing.flash.trim().is_empty() {
        model_routing.flash.clone()
    } else {
        model_routing.vision.clone()
    };
    let model_cfg_res = crate::llm::ModelConfig::from_db(&state, &conversation_model_id);
    let maybe_model_cfg = model_cfg_res.as_ref().ok();

    // 5. 查询所有 MCP Server 的工具列表并提前预热/升级当前会话的智能能力卡
    let mcp_tools_menu = {
        let registry = registry_state.lock().await;
        registry
            .prime_tool_catalog_for_session(&final_session_id, maybe_model_cfg)
            .await;
        registry.prewarm_all_for_session(&final_session_id);
        registry.format_tools_menu()
    };

    // 6. 扫描 prompts/skills/ 目录，生成动态 Skill 目录摘要（不含已固定编译的内置 Skill）
    let skill_catalog = {
        let resource_dir = app.path().resource_dir().unwrap_or_default();
        let skills_dir_prod = resource_dir.join("prompts/skills");
        let skills_dir_dev = std::path::PathBuf::from("prompts/skills");
        let skills_dir = if skills_dir_prod.exists() {
            skills_dir_prod
        } else {
            skills_dir_dev
        };
        // 全量动态扫描 prompts/skills/ 目录下所有 Skill 手册并提取/升级智能能力卡
        crate::agent::skill_context::skill_catalog_string(&skills_dir, &[], maybe_model_cfg)
    };

    let system_prompt = format!(
        "{}{}{}{}\n{}\n{}\n{}",
        base_prompt,
        workspace_skill_context,
        custom_instruction_context,
        skill_catalog,
        task_store_menu,
        local_tools_menu,
        mcp_tools_menu
    );
    println!(
        "--- 最终系统提示词 (级联菜单模式) ---\n{}\n------------------",
        system_prompt
    );
    // 每轮都是独立 Agent 运行，但同一 session_id 始终归属同一个长期对话。
    // 崩溃恢复会沿用既有 checkpoint，不能重复追加用户消息；只有新轮次才写入消息历史。
    let existing_checkpoint = load_checkpoint(state.inner(), &final_session_id)?;
    let is_resuming_existing =
        !crate::agent::conversation::should_start_new_turn(existing_checkpoint.as_ref());
    if !is_resuming_existing {
        crate::agent::conversation::begin_turn(
            state.inner(),
            &final_session_id,
            &conversation_model_id,
            &goal,
        )?;
    }
    let mut context = context::SandwichContext::new(system_prompt, goal.clone());
    // 注入当前路由模型的上下文窗口大小与压缩配置，用于历史压缩水位动态计算与双层智能归档
    if let Ok(cfg) = &model_cfg_res {
        context.context_window_tokens = cfg.context_window as usize;
        context.compaction_model_config = Some(cfg.clone());
    } else {
        context.context_window_tokens = 32_768;
    }
    context.turns_history =
        crate::agent::conversation::recent_history(state.inner(), &final_session_id)?;
    // 审查任务在首次模型调用前注入轻量项目地图。它只包含目录、入口和 Git 元数据，
    // 不读取源码正文；恢复中的运行沿用 checkpoint，避免反复追加同一张地图。
    if !is_resuming_existing {
        if let Some(review_map) = crate::agent::project_map::review_bootstrap(&goal, &workspace_dir)
        {
            context.turns_history.push(context::types::ChatMessage {
                role: "user".to_string(),
                content: json!(review_map),
            });
        }
    }
    context.sync_fetched_tools_from_history();
    // checkpoint 是恢复的权威来源：只要上一轮任务尚未到终态 (Done/Failed)，一律无缝续接！
    let mut checkpoint = match existing_checkpoint {
        Some(existing) if !existing.phase.is_terminal() => {
            existing.restore_context(&mut context);
            let mut cp = existing;
            if cp.phase == AgentPhase::WaitUser {
                if !goal.trim().is_empty() && cp.data.user_goal != goal {
                    context.add_error_feedback(&format!("【人类回复】{}", goal));
                }
                // 保持原本的任务主目标不变
                context.user_goal = cp.data.user_goal.clone();
                cp.begin_planning(cp.step_id, &context);
            }
            cp
        }
        Some(existing) => {
            // 同一 session 已达终态时，以乐观锁 revision 覆盖为新的运行快照。
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
    let (tx, mut rx) = mpsc::channel::<HumanControl>(5);
    let (interrupt_tx, mut interrupt_rx) = watch::channel(0u64);
    {
        let mut channels = intervention_channels().lock().await;
        channels.insert(final_session_id.clone(), tx);
    }
    stream_interrupt_channels()
        .lock()
        .await
        .insert(final_session_id.clone(), interrupt_tx);

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

            let reply = match rx.recv().await {
                Some(HumanControl::Answer(reply)) => reply,
                Some(HumanControl::ReplaceGoal(next_goal)) => {
                    let reason = format!("用户提交了新的任务目标：{}", next_goal);
                    finish_interrupted_run(
                        &app,
                        state.inner(),
                        &final_session_id,
                        &run_id,
                        &goal,
                        &run_trace_steps,
                        &mut checkpoint,
                        &context,
                        &mut run_machine,
                        reason,
                    )
                    .await?;
                    return Ok(());
                }
                Some(HumanControl::Cancel) => {
                    finish_interrupted_run(
                        &app,
                        state.inner(),
                        &final_session_id,
                        &run_id,
                        &goal,
                        &run_trace_steps,
                        &mut checkpoint,
                        &context,
                        &mut run_machine,
                        "用户请求停止当前任务。".to_string(),
                    )
                    .await?;
                    return Ok(());
                }
                None => "人类未回复，通道异常关闭".to_string(),
            };
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

        // 安全边界一：尚未请求模型时直接收束旧运行。
        if let Some(reason) = take_pending_interrupt(&mut rx) {
            finish_interrupted_run(
                &app,
                state.inner(),
                &final_session_id,
                &run_id,
                &goal,
                &run_trace_steps,
                &mut checkpoint,
                &context,
                &mut run_machine,
                reason,
            )
            .await?;
            return Ok(());
        }

        // 修复：绝不可以在这里清空 current_observation，因为这会导致 AI 看不到刚获取的页面数据
        // ================================================================
        // 统一重试循环：整个"规划 + 执行"作为一个原子操作，失败就重试
        // ================================================================
        let (mut instruction, result) = match run_call_llm_and_run_tool_phases(
            &app,
            &state,
            &registry_state,
            &model_routing,
            &run_id,
            &goal,
            log_context,
            &final_session_id,
            step_id,
            &mut context,
            &mut checkpoint,
            &mut run_machine,
            &mut rx,
            &mut interrupt_rx,
            resume_instruction,
            &mut run_trace_steps,
        )
        .await
        {
            Ok(res) => res,
            Err(error) => {
                let terminal_error = format!("任务执行失败: {}", error);
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
                let failure_completion = json!({ "summary": terminal_error });
                let _ = crate::agent::run_trace::upsert_snapshot(
                    state.inner(),
                    &run_id,
                    &final_session_id,
                    &goal,
                    "failed",
                    &run_trace_steps,
                    Some(&failure_completion),
                );
                let _ = app.emit(
                    &format!("agent-progress-{}", final_session_id),
                    json!({
                        "type": "complete",
                        "message": terminal_error,
                        "success": false
                    }),
                );
                return Err(error);
            }
        };
        if result.route == RUN_INTERRUPTED_ROUTE {
            finish_interrupted_run(
                &app,
                state.inner(),
                &final_session_id,
                &run_id,
                &goal,
                &run_trace_steps,
                &mut checkpoint,
                &context,
                &mut run_machine,
                result.stdout,
            )
            .await?;
            return Ok(());
        }

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
            let rejections = context.update_memories(instruction.memories_update.clone());
            if !rejections.is_empty() {
                context.turns_history.push(crate::agent::context::types::ChatMessage {
                    role: "user".to_string(),
                    content: serde_json::json!(format!(
                        "【⚠️ 记忆安全提示】\n以下记忆项未能存入核心记忆卡：\n{}\n（请勿在记忆中保存明文密钥或超长文本；长数据请存入 TaskStore 数据区）",
                        rejections.iter().map(|r| format!("- {}", r)).collect::<Vec<_>>().join("\n")
                    )),
                });
            }
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

        // --- 加载工具说明书 (Write-Once 按需追加至历史流并去重，稳吃 99% 前缀缓存) ---
        let tool_key = next_tool
            .split('/')
            .next()
            .unwrap_or(&next_tool)
            .to_string();
        if !tool_key.is_empty() && tool_key != "finish" && tool_key != "core" {
            if !context.has_tool_manual(&tool_key) {
                let tool_filename = format!("{}.md", tool_key);
                let resource_dir = app.path().resource_dir().unwrap_or_default();
                let candidate_paths = [
                    std::path::PathBuf::from("src-tauri/prompts/tools").join(&tool_filename),
                    std::path::PathBuf::from("prompts/tools").join(&tool_filename),
                    std::path::PathBuf::from("../src-tauri/prompts/tools").join(&tool_filename),
                    resource_dir.join("prompts/tools").join(&tool_filename),
                    std::path::PathBuf::from("src-tauri/prompts/skills").join(&tool_filename),
                    std::path::PathBuf::from("prompts/skills").join(&tool_filename),
                    std::path::PathBuf::from("../src-tauri/prompts/skills").join(&tool_filename),
                    resource_dir.join("prompts/skills").join(&tool_filename),
                ];

                let mut tool_detail = String::new();
                for cp in &candidate_paths {
                    if let Ok(content) = std::fs::read_to_string(cp) {
                        tool_detail = content;
                        break;
                    }
                }
                if tool_detail.is_empty() {
                    let registry = registry_state.lock().await;
                    if registry.plugin_names().contains(&tool_key.as_str()) {
                        tool_detail = registry.format_tool_detail(&tool_key);
                    }
                }

                if !tool_detail.trim().is_empty() {
                    context.turns_history.push(crate::agent::context::types::ChatMessage {
                        role: "user".to_string(),
                        content: json!(format!(
                            "【工具/Skill 操作手册: {}】\n{}\n(已写入历史流，后续调用可直接参考此规格)",
                            tool_key,
                            tool_detail.trim()
                        )),
                    });
                    context.fetched_tools.insert(tool_key.clone());
                    println!("📖 [Tool Manifest Cache] 首次引入工具手册 [{}] 并作为事件追加至历史流 (已标记 fetched)", tool_key);
                }
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

        let outcome_str = if result.success {
            "succeeded"
        } else {
            "failed"
        };
        let output_id = crate::agent::run_trace::save_step_output_with_meta(
            state.inner(),
            &final_session_id,
            step_id,
            &instruction.get_tool(),
            &instruction.description,
            outcome_str,
            &output_text,
        )
        .unwrap_or_else(|e| {
            eprintln!("⚠️ 无法保存 step_output: {}", e);
            "unknown_output_id".to_string()
        });

        // 🌟 工具执行失败时，自动沉淀到「避坑教训库 (Failed Attempts)」，供后续步骤长效避坑
        if !result.success {
            let error_summary = if !result.stderr.trim().is_empty() {
                result.stderr.trim().to_string()
            } else {
                result.stdout.lines().take(3).collect::<Vec<_>>().join(" ")
            };
            let short_error = if error_summary.chars().count() > 160 {
                format!("{}...", error_summary.chars().take(160).collect::<String>())
            } else {
                error_summary
            };
            context.record_failed_attempt(
                step_id,
                instruction.get_tool(),
                instruction.get_action(),
                short_error,
                Some(output_id.clone()),
            );
        }

        context.add_step(&instruction, &output_text, &output_id);

        // ── 防虚假勾选：将当步真实 output_id 回填到标记为 done 但缺少证据的 Todo（需在 output_id 已知后执行）
        if !instruction.todo_update.is_empty() {
            for item in &mut instruction.todo_update {
                if item.status == "done" && item.evidence_output_id.is_none() {
                    item.evidence_output_id = Some(output_id.clone());
                }
            }
            // 证据关联后同步更新 todo_list
            context.todo_list = instruction.todo_update.clone();
        }

        // ── Lesson 血缘召回记录：若当步工具是 search，将命中的 lesson 类型结果写入血缘表 ───────
        if instruction.get_tool().eq_ignore_ascii_case("search") && result.success {
            if let Ok(search_result) = serde_json::from_str::<serde_json::Value>(&result.stdout) {
                let recalled_lesson_ids: Vec<String> = search_result
                    .get("hits")
                    .and_then(|h| h.as_array())
                    .map(|hits| {
                        hits.iter()
                            .filter_map(|h| {
                                let kind = h.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                                if kind == "lesson" || kind == "summary" {
                                    h.get("id_or_path")
                                        .and_then(|id| id.as_str())
                                        .map(String::from)
                                } else {
                                    None
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if !recalled_lesson_ids.is_empty() {
                    crate::agent::lesson::record_lesson_recall(
                        state.inner(),
                        &final_session_id,
                        step_id,
                        &recalled_lesson_ids,
                    );
                }
            }
        }

        // ── Lesson 显式血缘因果反馈打分 ────────────────────────────────────
        if let Some(ref applied) = instruction.applied_lessons {
            if !applied.is_empty() {
                crate::agent::lesson::feedback_on_applied_lessons(
                    state.inner(),
                    &final_session_id,
                    step_id,
                    applied,
                    result.success,
                );
            }
        }
        // ──────────────────────────────────────────────────────────────────

        // --- 记录不可逆副作用安全台账 (Side-Effects Ledger) ---
        // 关键不可逆操作（如写入文件、修改数据库、执行写操作命令等）成功执行后记录台账，
        // 纯只读命令（如 git log/show/diff, search 等）不入台账；参数保持完整合法 JSON，绝不使用 ... 截断元数据
        if result.success
            && crate::agent::context::AuditLedger::is_mutating_side_effect(
                &instruction.get_tool(),
                &instruction.get_params(),
            )
        {
            let safe_params = crate::agent::context::AuditLedger::format_side_effect_params(
                &instruction.get_params(),
            );
            context.record_side_effect(
                step_id,
                instruction.get_tool(),
                format!("{}: {}", instruction.description, safe_params),
            );
        }

        context.current_observation = output_text.clone();
        // 复盘轨迹使用与 step_new 相同的语义字段，但与 Agent prompt/checkpoint 隔离。
        // 这使历史会话既能恢复思考、计划和工具调用，又不会放大运行时上下文。
        run_trace_steps.push(json!({
            "id": step_id,
            "description": instruction.description.clone(),
            "reflection": instruction.reflection.clone(),
            "thought": instruction.thought.clone(),
            "tool": instruction.get_tool(),
            "action": instruction.get_action(),
            "command": instruction.get_params(),
            "params": instruction.get_params(),
            "plan_id": context.active_plan_id().unwrap_or_default(),
            "plan_title": context.active_plan_title().unwrap_or(&context.user_goal),
            "status": if result.success { "succeeded" } else { "failed" },
            "output": output_text.clone(),
            "timestamp": Utc::now().timestamp_millis(),
        }));
        if let Err(error) = crate::agent::run_trace::upsert_snapshot(
            state.inner(),
            &run_id,
            &final_session_id,
            &goal,
            "running",
            &run_trace_steps,
            None,
        ) {
            eprintln!("[run-trace] 无法保存步骤快照: {error}");
        }
        // 工具结果已经进入可恢复上下文。先提交 durable phase/cursor，再通知前端；
        // 这样崩溃恢复不会把已经完成的普通步骤当作待执行，也不会把 finish 误续跑。
        let is_finish = is_finish_instruction(&instruction);
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

            let persisted_summary = finish_payload
                .get("summary")
                .and_then(|value| value.as_str())
                .or_else(|| {
                    finish_payload
                        .get("details")
                        .and_then(|value| value.as_str())
                })
                .unwrap_or(&output_text)
                .to_string();
            crate::agent::conversation::complete_turn(
                state.inner(),
                &final_session_id,
                &conversation_model_id,
                &persisted_summary,
            )?;
            // 完成区域展示模型自己的最终回复；不再额外生成固定模板的“任务总结”Markdown 文件。
            let emitted_finish = match finish_payload {
                serde_json::Value::Object(object) => serde_json::Value::Object(object),
                _ => json!({ "summary": persisted_summary }),
            };
            if let Err(error) = crate::agent::run_trace::upsert_snapshot(
                state.inner(),
                &run_id,
                &final_session_id,
                &goal,
                "succeeded",
                &run_trace_steps,
                Some(&emitted_finish),
            ) {
                eprintln!("[run-trace] 无法保存完成快照: {error}");
            }
            app.emit(
                &format!("agent-progress-{}", final_session_id),
                json!({
                    "type": "complete",
                    "message": "任务已由 AI 标记完成",
                    "success": true,
                    "output": output_text,      // 原始 JSON 字符串（兼容旧逻辑）
                    "finish": emitted_finish,   // 结构化成果对象，含 details/summary/artifacts
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
        checkpoint.mark_failed(terminal_error.clone(), &context);
        save_checkpoint(state.inner(), &mut checkpoint)?;
        let failure_completion = json!({ "summary": terminal_error });
        if let Err(error) = crate::agent::run_trace::upsert_snapshot(
            state.inner(),
            &run_id,
            &final_session_id,
            &goal,
            "failed",
            &run_trace_steps,
            Some(&failure_completion),
        ) {
            eprintln!("[run-trace] 无法保存失败快照: {error}");
        }
    }
    {
        let mut channels = intervention_channels().lock().await;
        channels.remove(&final_session_id);
        stream_interrupt_channels()
            .lock()
            .await
            .remove(&final_session_id);
    }

    Ok(())
}
