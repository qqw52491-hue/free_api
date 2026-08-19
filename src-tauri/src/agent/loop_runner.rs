use crate::agent::checkpoint::{save_checkpoint, AgentCheckpoint};
use crate::agent::control::{take_pending_interrupt, HumanControl};
use crate::agent::dispatcher::{
    emit_tool_event, is_finish_instruction, is_write_or_side_effect_tool, run_agent_step,
};
use crate::agent::mcp::PluginRegistry;
use crate::agent::run_state::{AgentRunEvent, AgentRunMachine};
use crate::agent::types::{AgentInstruction, DispatchResult, RUN_INTERRUPTED_ROUTE};
use crate::agent::utils::call_llm;
use crate::db::DbState;
use serde_json::json;
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, watch};

/// 执行一个可恢复步骤的 CallLlm 与 RunTool 阶段。
///
/// 该 handler 封装模型规划、工具派发、人工等待、重试和救援；每个外部副作用
/// 都由 checkpoint 在调用前后记录。主调度器只负责启动步骤和提交已完成结果。
pub async fn run_call_llm_and_run_tool_phases(
    app: &AppHandle,
    state: &State<'_, DbState>,
    registry_state: &State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    model_routing: &crate::agent::router::ModelRoutingConfig,
    _goal: &String,
    log_context: Option<bool>,
    final_session_id: &String,
    step_id: usize,
    context: &mut crate::agent::context::SandwichContext,
    mut checkpoint: &mut AgentCheckpoint,
    mut run_machine: &mut AgentRunMachine,
    rx: &mut mpsc::Receiver<HumanControl>,
    interrupt_rx: &mut watch::Receiver<u64>,
    resume_instruction: Option<AgentInstruction>,
) -> Result<(AgentInstruction, DispatchResult), String> {
    let mut retry_count = 0;
    let max_retries = 3;

    // LLM 回复已落盘但工具尚未开始时，恢复直接进入同一工具处理器。
    // 这样不会为同一个决策额外消耗一次模型调用，也不会绕过工具前的 pending 提交。
    if let Some(inst) = resume_instruction {
        crate::agent::advance_run_state(
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
        crate::agent::advance_run_state(
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
        crate::agent::advance_run_state(
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
        let _ = app.emit(
            &format!("agent-progress-{}", final_session_id),
            json!({
                "type": "action_phase",
                "phase": "calling_llm",
                "description": format!("🧠 正在调用 {} 进行决策规划...", routing.model_id),
                "model_id": routing.model_id
            }),
        );
        let (mut inst, token_usage, thinking_text) = match call_llm(
            &context,
            &state,
            routing.model_id.clone(),
            Some(&app),
            step_id,
            &final_session_id,
            log_context.unwrap_or(false),
            model_routing.reasoning_effort_for(&routing.tier),
            interrupt_rx,
        )
        .await
        {
            Ok(r) => r,
            Err(e) if e == crate::llm::STREAM_INTERRUPTED_ERROR => {
                let reason = take_pending_interrupt(rx)
                    .unwrap_or_else(|| "用户请求中断当前模型输出。".to_string());
                return Ok((
                    AgentInstruction::default(),
                    DispatchResult {
                        success: true,
                        stdout: reason,
                        stderr: String::new(),
                        route: RUN_INTERRUPTED_ROUTE.to_string(),
                    },
                ));
            }
            Err(e) => {
                // 🛑 智能拦截不可重试的致命错误（401 Key失效、402 余额不足、403 权限不足、404 模型不存在等）
                if crate::llm::is_non_retryable_error(&e) {
                    let friendly_reason = if e.contains("401")
                        || e.contains("invalid_api_key")
                        || e.contains("Incorrect API key")
                    {
                        "🔑 API 认证失败：API Key 无效或已过期，请在设置中检查 API Key".to_string()
                    } else if e.contains("402") || e.contains("insufficient_quota") {
                        "💳 账户额度不足：API 账户余额已耗尽，请充值后重试".to_string()
                    } else if e.contains("403") {
                        "🚫 权限不足：当前 API Key 无权访问所选模型".to_string()
                    } else if e.contains("404") || e.contains("model_not_found") {
                        "🔍 模型未找到：所选模型名称在上游服务商处不存在".to_string()
                    } else {
                        format!("❌ API 致命错误: {}", e)
                    };

                    app.emit(
                        &format!("agent-progress-{}", final_session_id),
                        json!({
                            "type": "complete",
                            "message": friendly_reason,
                            "success": false
                        }),
                    )
                    .ok();
                    crate::agent::advance_run_state(
                        &app,
                        &final_session_id,
                        &mut run_machine,
                        AgentRunEvent::Fail {
                            reason: friendly_reason.clone(),
                        },
                    )?;
                    checkpoint.mark_failed(friendly_reason.clone(), &context);
                    save_checkpoint(state.inner(), &mut checkpoint)?;
                    return Err(friendly_reason);
                }

                retry_count += 1;
                crate::agent::advance_run_state(
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
                    let terminal_error = format!("AI 持续输出非法格式，任务终止: {}", e);
                    app.emit(
                        &format!("agent-progress-{}", final_session_id),
                        json!({
                            "type": "complete",
                            "message": terminal_error,
                            "success": false
                        }),
                    )
                    .ok();
                    crate::agent::advance_run_state(
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
                crate::agent::advance_run_state(
                    &app,
                    &final_session_id,
                    &mut run_machine,
                    AgentRunEvent::RetryPlanning,
                )?;
                continue;
            }
        };

        let model_tier = routing.tier.to_string();
        inst.description = inst.description.trim().to_string();
        if inst.description.is_empty() {
            inst.description = "执行下一步操作".to_string();
        }
        let (plan_id, plan_title) = context.resolve_plan(&inst);

        // 🌟 捕获大模型输出的全局精炼任务标题并实时持久化到 SQLite
        if let Some(session_title) = &inst.session_title {
            let clean = session_title.trim();
            if !clean.is_empty() && clean.chars().count() <= 25 {
                if let Ok(conn) = state.0.lock() {
                    let _ = conn.execute(
                        "UPDATE chat_sessions SET title=?1 WHERE id=?2",
                        rusqlite::params![clean, final_session_id],
                    );
                }
                app.emit(
                    &format!("agent-progress-{}", final_session_id),
                    json!({
                        "type": "session_title_updated",
                        "title": clean,
                    }),
                )
                .ok();
            }
        }

        // LLM 的结构化决策已经完整到达；在任何 UI 通知或工具调用之前提交它。
        checkpoint.record_instruction(inst.clone(), context);
        save_checkpoint(state.inner(), checkpoint)?;
        crate::agent::advance_run_state(
            &app,
            &final_session_id,
            &mut run_machine,
            AgentRunEvent::InstructionReady {
                action: inst.get_action(),
            },
        )?;

        // 📊 发送 Token 用量与速度指标统计到前端
        app.emit(
            &format!("agent-progress-{}", final_session_id),
            json!({
                "type": "token_usage",
                "step_id": step_id,
                "prompt_tokens": token_usage.prompt_tokens,
                "completion_tokens": token_usage.completion_tokens,
                "total_tokens": token_usage.total_tokens,
                "cached_tokens": token_usage.cached_tokens,
                "cache_hit_rate": token_usage.cache_hit_rate,
                "context_window": token_usage.context_window,
                "usage_percent": token_usage.usage_percent,
                "ttft_ms": token_usage.ttft_ms,
                "total_duration_ms": token_usage.total_duration_ms,
                "tokens_per_second": token_usage.tokens_per_second,
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

        // 安全边界二：模型结果已返回、工具尚未派发时丢弃旧指令。
        if let Some(reason) = take_pending_interrupt(rx) {
            return Ok((
                inst,
                DispatchResult {
                    success: true,
                    stdout: reason,
                    stderr: String::new(),
                    route: RUN_INTERRUPTED_ROUTE.to_string(),
                },
            ));
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
                    "tool": inst.get_tool(),
                    "action": inst.get_action(),
                    "command": inst.get_params(),
                    "params": inst.get_params(),
                    "plan_id": plan_id,
                    "plan_title": plan_title,
                    "model_tier": model_tier,
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

        // 安全边界三：工具已返回，保留结果但不让旧回合继续规划。
        if let Some(reason) = take_pending_interrupt(rx) {
            return Ok((
                inst,
                DispatchResult {
                    success: true,
                    stdout: reason,
                    stderr: String::new(),
                    route: RUN_INTERRUPTED_ROUTE.to_string(),
                },
            ));
        }
        // --- C. 判断执行结果 ---
        if dispatch_result.success || inst.get_action() == "finish" {
            break (inst, dispatch_result); // 成功！
        } else {
            retry_count += 1;
            let error_detail = format!("【❌ 执行失败】: {}", dispatch_result.stderr);
            crate::agent::advance_run_state(
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

            // 【修复：重试时的工具盲区】如果 AI 尝试使用的工具尚未在历史流中，立即为其追加说明书，防止它在 retry 时因为看不到语法而继续瞎猜报错。
            let attempted_tool = inst.get_action();
            let tool_key = attempted_tool
                .split('/')
                .next()
                .unwrap_or(&attempted_tool)
                .to_string();
            if attempted_tool != "finish" && attempted_tool != "core" && !tool_key.is_empty() {
                context.active_tool = Some(tool_key.clone());
                if !context.fetched_tools.contains(&tool_key) {
                    let tool_filename = format!("{}.md", tool_key);
                    let resource_dir = app.path().resource_dir().unwrap_or_default();
                    let tool_path_production =
                        resource_dir.join("prompts/tools").join(&tool_filename);
                    let tool_path_dev =
                        std::path::PathBuf::from("prompts/tools").join(&tool_filename);

                    let mut tool_detail = String::new();
                    if let Ok(tool_md) = std::fs::read_to_string(&tool_path_production)
                        .or_else(|_| std::fs::read_to_string(&tool_path_dev))
                    {
                        tool_detail = tool_md;
                    } else {
                        // 尝试从 MCP 插件中获取
                        let detail = {
                            let registry =
                                futures::executor::block_on(registry_state.inner().lock());
                            if registry.plugin_names().contains(&tool_key.as_str()) {
                                registry.format_tool_detail(&tool_key)
                            } else {
                                String::new()
                            }
                        };
                        if !detail.is_empty() {
                            tool_detail = detail;
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
                        println!(
                            "🔄 [重试修正] 为 AI 紧急追加了工具说明书至历史流: {}",
                            tool_key
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
                crate::agent::advance_run_state(
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
pub async fn run_tool_phase(
    app: &AppHandle,
    state: &State<'_, DbState>,
    registry_state: &State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    final_session_id: &String,
    step_id: usize,
    inst: &AgentInstruction,
    context: &mut crate::agent::context::SandwichContext,
    mut checkpoint: &mut AgentCheckpoint,
    mut run_machine: &mut AgentRunMachine,
    rx: &mut mpsc::Receiver<HumanControl>,
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
    let tool_started_at = Instant::now();
    emit_tool_event(
        app,
        final_session_id,
        step_id,
        inst,
        "started",
        None,
        None,
        "",
        None,
    );

    // --- B1. 协议终态：finish 不创建 pending、不派发工具，直接交由主循环完成 checkpoint 和状态收敛。 ---
    if is_finish_instruction(inst) {
        let result = DispatchResult {
            stdout: inst
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
        emit_tool_event(
            app,
            final_session_id,
            step_id,
            inst,
            "succeeded",
            Some(result.route.clone()),
            Some(true),
            &result.stdout,
            Some(tool_started_at.elapsed().as_millis()),
        );
        return Ok(result);
    }
    // --- B2. 特殊拦截：AI 主动挂起呼叫人类 ---
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
                "message": ask_msg,
                "form": inst.get_params().get("form").cloned().unwrap_or(serde_json::Value::Null)
            }),
        )
        .ok();

        println!("⏸️ [阻塞点] 模型触发 ask_human 挂起，正在死等前端人类唤醒...");

        // 真正的阻塞点：死等人类回复！
        // rx 被我们在外层定义过了，这里可以直接 `.recv().await`
        checkpoint.wait_for_human(ask_msg.clone(), &context);
        save_checkpoint(state.inner(), &mut checkpoint)?;
        crate::agent::advance_run_state(
            &app,
            &final_session_id,
            &mut run_machine,
            AgentRunEvent::WaitForHuman {
                question: ask_msg.clone(),
            },
        )?;
        let reply = match rx.recv().await {
            Some(HumanControl::Answer(reply)) => reply,
            Some(HumanControl::ReplaceGoal(goal)) => {
                return Ok(DispatchResult {
                    success: true,
                    stdout: format!("用户提交了新的任务目标：{}", goal),
                    stderr: String::new(),
                    route: RUN_INTERRUPTED_ROUTE.to_string(),
                })
            }
            Some(HumanControl::Cancel) => {
                return Ok(DispatchResult {
                    success: true,
                    stdout: "用户请求停止当前任务。".to_string(),
                    stderr: String::new(),
                    route: RUN_INTERRUPTED_ROUTE.to_string(),
                })
            }
            None => "人类未回复，通道异常关闭".to_string(),
        };
        context.add_error_feedback(&format!("【人类回复】{}", reply));
        checkpoint.begin_planning(step_id, &context);
        save_checkpoint(state.inner(), &mut checkpoint)?;
        crate::agent::advance_run_state(
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
        let is_write_tool = is_write_or_side_effect_tool(inst);

        let mut tool_task = tokio::task::spawn_blocking(move || {
            let registry = {
                let guard = futures::executor::block_on(registry_arc.lock());
                (*guard).clone()
            };
            println!("执行动作: {:?}", inst_c);
            run_agent_step(&db, &sid, &inst_c, &registry)
        });

        let dispatch_result = tokio::select! {
            res = &mut tool_task => {
                res.map_err(|e| e.to_string())?
            }
            control = rx.recv() => {
                let interrupt_reason = match control {
                    Some(HumanControl::Cancel) => "用户请求停止当前任务。".to_string(),
                    Some(HumanControl::ReplaceGoal(goal)) => format!("用户提交了新的任务目标：{}（已中断前序工具）", goal),
                    Some(HumanControl::Answer(_)) | None => "用户请求中断当前任务。".to_string(),
                };

                if is_write_tool {
                    // 写盘/事务类工具：给予最多 1.5 秒安全落盘缓冲期，避免破坏正在保存的文件
                    app.emit(
                        &format!("agent-log-{}", final_session_id),
                        "⏳ 收到停止指令，正在给予写盘/事务操作 1.5s 安全保存缓冲期...".to_string(),
                    ).ok();

                    tokio::select! {
                        res = &mut tool_task => {
                            app.emit(
                                &format!("agent-log-{}", final_session_id),
                                "✅ 写盘操作已安全完成，任务即将终止。".to_string(),
                            ).ok();
                            res.map_err(|e| e.to_string())?
                        }
                        _ = tokio::time::sleep(tokio::time::Duration::from_millis(1500)) => {
                            app.emit(
                                &format!("agent-log-{}", final_session_id),
                                "⚠️ 写盘操作耗时超 1.5s，已强制安全终止。".to_string(),
                            ).ok();
                            DispatchResult {
                                success: false,
                                stdout: interrupt_reason,
                                stderr: String::new(),
                                route: RUN_INTERRUPTED_ROUTE.to_string(),
                            }
                        }
                    }
                } else {
                    // 只读/网络/查询类工具：0 毫秒立刻中断退出
                    DispatchResult {
                        success: false,
                        stdout: interrupt_reason,
                        stderr: String::new(),
                        route: RUN_INTERRUPTED_ROUTE.to_string(),
                    }
                }
            }
        };

        if dispatch_result.route == RUN_INTERRUPTED_ROUTE {
            return Ok(dispatch_result);
        }
        dispatch_result
    };

    let event_type = if dispatch_result.success {
        "succeeded"
    } else {
        "failed"
    };
    let result_text = if !dispatch_result.stdout.is_empty() {
        dispatch_result.stdout.as_str()
    } else {
        dispatch_result.stderr.as_str()
    };
    emit_tool_event(
        app,
        final_session_id,
        step_id,
        inst,
        event_type,
        Some(dispatch_result.route.clone()),
        Some(dispatch_result.success),
        result_text,
        Some(tool_started_at.elapsed().as_millis()),
    );

    Ok(dispatch_result)
}
