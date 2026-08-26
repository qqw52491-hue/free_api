use crate::agent::checkpoint::{save_checkpoint, AgentCheckpoint};
use crate::agent::context::resource_scope::ResourceOperation;
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
    run_id: &str,
    goal: &str,
    log_context: Option<bool>,
    final_session_id: &String,
    step_id: usize,
    context: &mut crate::agent::context::SandwichContext,
    mut checkpoint: &mut AgentCheckpoint,
    mut run_machine: &mut AgentRunMachine,
    rx: &mut mpsc::Receiver<HumanControl>,
    interrupt_rx: &mut watch::Receiver<u64>,
    resume_instruction: Option<AgentInstruction>,
    run_trace_steps: &mut Vec<serde_json::Value>,
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
        let routing =
            crate::agent::router::AgentRouter::route_for_step(&context, &model_routing, step_id);
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
        let open_todos_for_early_check = crate::agent::main_loop::finish_blockers(
            &context.todo_list,
            context.review_contract.as_ref(),
            false,
            context.side_effects.len(),
            context.change_verification.as_ref(),
        );
        let can_early_challenge_finish =
            !checkpoint.finish_reviewed && !open_todos_for_early_check.is_empty();

        let (mut inst, token_usage, thinking_text, history_estimate_at_request) = match call_llm(
            &context,
            &state,
            routing.model_id.clone(),
            Some(&app),
            step_id,
            &final_session_id,
            log_context.unwrap_or(false),
            model_routing.reasoning_effort_for(&routing.tier),
            can_early_challenge_finish,
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
                        format!(
                            "🔑 API 认证失败 [模型: {}]: API Key 无效或已过期，请在设置中检查 API Key。\n上游原始错误: {}",
                            routing.model_id, e
                        )
                    } else if e.contains("402") || e.contains("insufficient_quota") {
                        format!(
                            "💳 账户额度不足 [模型: {}]: API 账户余额已耗尽，请在设置中切换模型或充值。\n上游原始错误: {}",
                            routing.model_id, e
                        )
                    } else if e.contains("403") {
                        format!(
                            "🚫 权限不足 [模型: {}]: 当前 API Key 无权访问所选模型。\n上游原始错误: {}",
                            routing.model_id, e
                        )
                    } else if e.contains("404") || e.contains("model_not_found") {
                        format!(
                            "🔍 模型未找到 [模型: {}]: 所选模型名称在上游服务商处不存在。\n上游原始错误: {}",
                            routing.model_id, e
                        )
                    } else {
                        format!("❌ API 致命错误 [模型: {}]: {}", routing.model_id, e)
                    };

                    return Err(friendly_reason);
                }

                retry_count += 1;
                crate::agent::advance_run_state(
                    &app,
                    &final_session_id,
                    &mut run_machine,
                    AgentRunEvent::PlanningFailed { reason: e.clone() },
                )?;

                app.emit(
                    &format!("agent-log-{}", final_session_id),
                    format!(
                        "🔄 规划解析失败(第{}次)，正在注入修正提示让 AI 重新输出...",
                        retry_count
                    ),
                )
                .map_err(|e| e.to_string())?;
                context.add_error_feedback(&format!(
                    "你的输出未能通过 JSON 校验，解析报错为: {}\n请严格按照 System Prompt 要求输出纯合法 JSON！",
                    e
                ));
                checkpoint.record_recoverable_error(format!("JSON 解析失败: {}", e), &context);
                save_checkpoint(state.inner(), &mut checkpoint)?;
                crate::agent::advance_run_state(
                    &app,
                    &final_session_id,
                    &mut run_machine,
                    AgentRunEvent::RetryPlanning,
                )?;

                if retry_count >= max_retries {
                    let terminal_error = format!("AI 持续输出非法格式，任务终止: {}", e);
                    return Err(terminal_error);
                }
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

        // 真实 usage 只能与发出该完整上下文请求时的历史快照成对保存。
        // 隔离 JSON 修复请求返回 None，保留已有完整上下文基线。
        if token_usage.prompt_tokens > 0 {
            if let Some(history_tokens) = history_estimate_at_request {
                context.last_reported_prompt_tokens = Some(token_usage.prompt_tokens as usize);
                context.last_reported_history_estimate_tokens = Some(history_tokens);
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

            // 🌟 立即将失败尝试的完整报错写入 SQLite (agent_step_logs & FTS5)，确保错误历史有据可查
            let _ = crate::agent::run_trace::save_step_output_with_meta(
                state.inner(),
                final_session_id,
                step_id,
                &inst.get_tool(),
                &inst.description,
                "failed",
                &error_detail,
            );

            // 🌟 立即将失败尝试写入复盘轨迹 run_trace_steps 并持久化到 agent_run_traces，保证页面刷新后红色的失败步骤不丢失
            run_trace_steps.push(json!({
                "id": format!("{}-attempt-{}", step_id, retry_count),
                "step_id": step_id,
                "description": format!("{} (失败重试 #{})", inst.description, retry_count),
                "reflection": inst.reflection.clone(),
                "thought": inst.thought.clone(),
                "tool": inst.get_tool(),
                "action": inst.get_action(),
                "command": inst.get_params(),
                "params": inst.get_params(),
                "plan_id": plan_id,
                "plan_title": plan_title,
                "status": "failed",
                "output": error_detail,
                "timestamp": chrono::Utc::now().timestamp_millis(),
            }));
            let _ = crate::agent::run_trace::upsert_snapshot(
                state.inner(),
                run_id,
                final_session_id,
                goal,
                "running",
                run_trace_steps,
                None,
            );

            // 🌟 立即向前端广播步骤失败事件，确保错误尝试保留在时间线上供用户查看
            app.emit(
                &format!("agent-progress-{}", final_session_id),
                json!({
                    "type": "step_error",
                    "step_id": step_id,
                    "description": inst.description.clone(),
                    "tool": inst.get_tool(),
                    "output": error_detail,
                }),
            )
            .ok();

            if let Ok(raw_json) = serde_json::to_string(&inst) {
                context.add_assistant_raw_message(&raw_json);
            }

            // 【修复：重试时的工具盲区】如果 AI 尝试使用的工具尚未在历史流中，立即为其追加说明书，防止它在 retry 时因为看不到语法而继续瞎猜报错。
            let attempted_tool = if !inst.get_tool().is_empty() {
                inst.get_tool()
            } else {
                inst.get_action()
            };
            let plugin_name = attempted_tool
                .split('/')
                .next()
                .unwrap_or(&attempted_tool)
                .to_string();

            if attempted_tool != "finish" && attempted_tool != "core" && !attempted_tool.is_empty()
            {
                context.active_tool = Some(attempted_tool.clone());
                // 服务级能力目录不能替代具体 MCP 子工具的参数 schema。
                // 失败后只为当前失败的完整工具名披露一次精确规格，不改写模型参数。
                if !context.has_tool_manual(&attempted_tool) {
                    let mut tool_detail = {
                        let registry = registry_state.inner().lock().await;
                        let exact = registry.format_tool_detail(&attempted_tool);
                        if exact.contains("未找到工具") {
                            String::new()
                        } else {
                            exact
                        }
                    };

                    // 非 MCP 原生工具或目录中不存在的工具，仍按原路径查找本地手册。
                    if tool_detail.is_empty() {
                        let tool_filename = format!("{}.md", plugin_name);
                        let resource_dir = app.path().resource_dir().unwrap_or_default();
                        let candidate_paths = [
                            std::path::PathBuf::from("src-tauri/prompts/tools").join(&tool_filename),
                            std::path::PathBuf::from("prompts/tools").join(&tool_filename),
                            std::path::PathBuf::from("../src-tauri/prompts/tools").join(&tool_filename),
                            resource_dir.join("prompts/tools").join(&tool_filename),
                            std::path::PathBuf::from("src-tauri/prompts/skills").join(&tool_filename),
                            std::path::PathBuf::from("prompts/skills").join(&tool_filename),
                            std::path::PathBuf::from("../src-tauri/prompts/skills")
                                .join(&tool_filename),
                            resource_dir.join("prompts/skills").join(&tool_filename),
                        ];
                        for cp in &candidate_paths {
                            if let Ok(tool_md) = std::fs::read_to_string(cp) {
                                tool_detail = tool_md;
                                break;
                            }
                        }
                    }

                    if !tool_detail.trim().is_empty() {
                        context.turns_history.push(crate::agent::context::types::ChatMessage {
                            role: "user".to_string(),
                            content: json!(format!(
                                "【工具/Skill 操作手册: {}】\n{}\n(已写入历史流，后续调用可直接参考此规格)",
                                attempted_tool,
                                tool_detail.trim()
                            )),
                        });
                        context.fetched_tools.insert(attempted_tool.clone());
                        println!(
                            "🔄 [重试修正] 为 AI 紧急追加了工具说明书至历史流: {}",
                            attempted_tool
                        );
                    }
                }
            }

            if retry_count >= max_retries {
                println!(
                    "⚠️ [工具重试超限] 工具重试达到 {} 次上限，不再强行中断任务，将错误信息作为 Observation 返回给主 Agent 决策",
                    max_retries
                );
                // 将重试超限的错误详情注入上下文，以便模型在下一步决策中反思并调整策略或进行总结交付
                context.add_error_feedback(&format!(
                    "{}\n\n⚠️ 注意：该工具动作连续重试 {} 次均失败，请勿再重复调用相同参数！请调整方案、使用其他工具或根据已知信息进行总结。",
                    error_detail, max_retries
                ));
                break (inst, dispatch_result);
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

    // --- B1. 协议终态：finish 不创建 pending、不派发工具，由主循环在终态自检裁决后统一收敛状态与发送事件。 ---
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
        // 不在此处盲目广播 "succeeded"，交由 main_loop 的 finish guard 判定是否为 finish_challenged 统一发射事件
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
        let is_write_tool = is_write_or_side_effect_tool(inst);
        if is_write_tool {
            if let Some(scope_guard) = context
                .resource_scope
                .guard_known_outside_write(&inst.get_tool(), &inst.get_params())
            {
                let result = DispatchResult {
                    success: false,
                    stdout: String::new(),
                    stderr: format!(
                        "{}\nNo tool call was issued; original parameters were not changed.",
                        scope_guard.render()
                    ),
                    route: "scope_guard:outside".to_string(),
                };
                emit_tool_event(
                    app,
                    final_session_id,
                    step_id,
                    inst,
                    "failed",
                    Some(result.route.clone()),
                    Some(false),
                    &result.stderr,
                    Some(tool_started_at.elapsed().as_millis()),
                );
                return Ok(result);
            }
        }

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
        let registry = {
            let guard = registry_arc.lock().await;
            (*guard).clone()
        };

        let mut tool_task = tokio::task::spawn_blocking(move || {
            println!("执行动作: {:?}", inst_c);
            run_agent_step(&db, &sid, &inst_c, &registry)
        });

        let mut dispatch_result = tokio::select! {
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
        let output = if dispatch_result.success {
            dispatch_result.stdout.clone()
        } else {
            dispatch_result.stderr.clone()
        };
        if let Some(scope_observation) = context.resource_scope.observe_tool_result(
            &inst.get_tool(),
            &inst.get_params(),
            &output,
            if is_write_tool {
                ResourceOperation::Write
            } else {
                ResourceOperation::Read
            },
        ) {
            let annotation = scope_observation.render();
            if dispatch_result.success {
                dispatch_result.stdout = format!("{}\n\n{}", dispatch_result.stdout, annotation);
            } else {
                dispatch_result.stderr = format!("{}\n\n{}", dispatch_result.stderr, annotation);
            }
        }
        // 无进展重复检测：只在"相同工具+相同参数+相同输出/失败"反复发生时注入
        // 策略级提示；不修改、不补全、不拒绝模型提交的参数。
        if let Some(no_progress_notice) = context.no_progress.record(
            &inst.get_tool(),
            &inst.get_params(),
            dispatch_result.success,
            &output,
        ) {
            context.turns_history.push(crate::agent::context::types::ChatMessage {
                role: "user".to_string(),
                content: serde_json::json!(no_progress_notice),
            });
            println!("🔁 [No-Progress] 注入无进展提示: {}", inst.get_tool());
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
