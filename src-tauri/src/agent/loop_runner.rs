use crate::agent::checkpoint::{save_checkpoint, AgentCheckpoint};
use crate::agent::child_reviewer::{result_as_dispatch_output, spawn_child_reviewer};
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
use std::path::Path;
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, watch};

const REPEATED_FAILURE_BLOCKED_ROUTE: &str = "repeated_deterministic_failure";

#[derive(Debug, Clone)]
pub struct StepExecutionOutcome {
    pub instruction: AgentInstruction,
    pub result: DispatchResult,
    pub token_usage: Option<crate::agent::types::TokenUsage>,
    pub thinking_duration_ms: u64,
    pub tool_duration_ms: u64,
}

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
    workspace_dir: &Path,
    goal: &str,
    log_context: Option<bool>,
    transient_llm_request_retries: u32,
    final_session_id: &String,
    step_id: usize,
    context: &mut crate::agent::context::SandwichContext,
    mut checkpoint: &mut AgentCheckpoint,
    mut run_machine: &mut AgentRunMachine,
    rx: &mut mpsc::Receiver<HumanControl>,
    interrupt_rx: &mut watch::Receiver<u64>,
    resume_instruction: Option<AgentInstruction>,
    run_trace_steps: &mut Vec<serde_json::Value>,
) -> Result<StepExecutionOutcome, String> {
    let mut retry_count = 0;
    let max_retries = 3;
    let mut planning_retry_count = 0;
    const MAX_PLANNING_RETRIES: usize = 1;
    let mut last_token_usage: Option<crate::agent::types::TokenUsage>;
    let mut last_thinking_duration_ms: u64 = 0;
    let mut last_tool_duration_ms: u64;

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
        let tool_start = Instant::now();
        let dispatch_result = run_tool_phase(
            app,
            state,
            registry_state,
            model_routing,
            run_id,
            workspace_dir,
            final_session_id,
            step_id,
            &inst,
            context,
            checkpoint,
            run_machine,
            rx,
            interrupt_rx,
        )
        .await?;
        let tool_duration_ms = tool_start.elapsed().as_millis() as u64;
        if dispatch_result.success || inst.get_action() == "finish" {
            return Ok(StepExecutionOutcome {
                instruction: inst,
                result: dispatch_result,
                token_usage: None,
                thinking_duration_ms: 0,
                tool_duration_ms,
            });
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

        let planning_start = Instant::now();
        let (mut inst, token_usage, thinking_text, history_estimate_at_request) = match call_llm(
            &context,
            &state,
            routing.model_id.clone(),
            &routing.tier.to_string(),
            Some(&app),
            step_id,
            &final_session_id,
            log_context.unwrap_or(false),
            transient_llm_request_retries,
            model_routing.reasoning_effort_for(&routing.tier),
            can_early_challenge_finish,
            interrupt_rx,
        )
        .await
        {
            Ok(r) => {
                last_thinking_duration_ms = planning_start.elapsed().as_millis() as u64;
                last_token_usage = Some(r.1.clone());
                r
            }
            Err(e) if e == crate::llm::STREAM_INTERRUPTED_ERROR => {
                let reason = take_pending_interrupt(rx)
                    .unwrap_or_else(|| "用户请求中断当前模型输出。".to_string());
                return Ok(StepExecutionOutcome {
                    instruction: AgentInstruction::default(),
                    result: DispatchResult {
                        success: true,
                        stdout: reason,
                        stderr: String::new(),
                        route: RUN_INTERRUPTED_ROUTE.to_string(),
                    },
                    token_usage: None,
                    thinking_duration_ms: last_thinking_duration_ms,
                    tool_duration_ms: 0,
                });
            }
            Err(e) => {
                // 🛑 如果是网络/API 传输层彻底失败，直接快速终止
                if e.contains("LLM 请求最终失败")
                    || e.contains("请求失败")
                    || e.contains("API 错误")
                {
                    crate::agent::advance_run_state(
                        &app,
                        &final_session_id,
                        &mut run_machine,
                        AgentRunEvent::PlanningFailed { reason: e.clone() },
                    )?;
                    return Err(format!("❌ LLM 请求失败 (已达重试上限): {}", e));
                }

                // 🛡️ 容错自愈：对于模型自言自语/纯文本格式异常，允许在对话上下文中进行 1 次严格引导重试
                if planning_retry_count < MAX_PLANNING_RETRIES {
                    planning_retry_count += 1;
                    crate::agent::advance_run_state(
                        &app,
                        &final_session_id,
                        &mut run_machine,
                        AgentRunEvent::RetryPlanning,
                    )?;
                    let _ = app.emit(
                        &format!("agent-log-{}", final_session_id),
                        format!(
                            "🔄 AI 输出了非结构化文本，正在自动注入引导提示重新生成 ({}/{})...",
                            planning_retry_count, MAX_PLANNING_RETRIES
                        ),
                    );
                    context.add_error_feedback(&format!(
                        "你的上一轮输出未能通过 JSON 协议校验（{}）。\n请严格遵循系统契约，仅输出一个合法的 JSON 工具指令（例如 task_store、finish 等）！",
                        e
                    ));
                    checkpoint.record_recoverable_error(format!("规划格式异常: {}", e), &context);
                    save_checkpoint(state.inner(), &mut checkpoint)?;
                    continue;
                }

                crate::agent::advance_run_state(
                    &app,
                    &final_session_id,
                    &mut run_machine,
                    AgentRunEvent::PlanningFailed { reason: e.clone() },
                )?;
                let _ = app.emit(
                    &format!("agent-log-{}", final_session_id),
                    format!("❌ AI 规划失败: {}", e),
                );
                return Err(format!("❌ AI 规划失败: {}", e));
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

        // 📊 逐调用 usage telemetry 已下沉到 call_llm 内每个请求结果旁
        //（成功 / parse_error / request_error / interrupted 各占一行），
        // 完整上下文与 JSON 修复重发不再共用一行成功快照。

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
            return Ok(StepExecutionOutcome {
                instruction: inst,
                result: DispatchResult {
                    success: true,
                    stdout: reason,
                    stderr: String::new(),
                    route: RUN_INTERRUPTED_ROUTE.to_string(),
                },
                token_usage: last_token_usage,
                thinking_duration_ms: last_thinking_duration_ms,
                tool_duration_ms: 0,
            });
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

        let tool_start = Instant::now();
        let dispatch_result = run_tool_phase(
            app,
            state,
            registry_state,
            model_routing,
            run_id,
            workspace_dir,
            final_session_id,
            step_id,
            &inst,
            &mut *context,
            &mut *checkpoint,
            &mut *run_machine,
            rx,
            interrupt_rx,
        )
        .await?;
        last_tool_duration_ms = tool_start.elapsed().as_millis() as u64;

        // 安全边界三：工具已返回，保留结果但不让旧回合继续规划。
        if let Some(reason) = take_pending_interrupt(rx) {
            return Ok(StepExecutionOutcome {
                instruction: inst,
                result: DispatchResult {
                    success: true,
                    stdout: reason,
                    stderr: String::new(),
                    route: RUN_INTERRUPTED_ROUTE.to_string(),
                },
                token_usage: last_token_usage,
                thinking_duration_ms: last_thinking_duration_ms,
                tool_duration_ms: last_tool_duration_ms,
            });
        }
        // --- C. 判断执行结果 ---
        if dispatch_result.success || inst.get_action() == "finish" {
            break (inst, dispatch_result); // 成功！
        } else {
            let repeat_failure_blocked = dispatch_result.route == REPEATED_FAILURE_BLOCKED_ROUTE;
            if !repeat_failure_blocked {
                context.record_failed_attempt(
                    step_id,
                    inst.get_tool(),
                    inst.get_action(),
                    &inst.get_params(),
                    dispatch_result.stderr.clone(),
                    None,
                );
            }
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
            let total_dur = last_thinking_duration_ms + last_tool_duration_ms;
            let metrics_val = json!({
                "prompt_tokens": last_token_usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
                "completion_tokens": last_token_usage.as_ref().map(|u| u.completion_tokens).unwrap_or(0),
                "total_tokens": last_token_usage.as_ref().map(|u| u.total_tokens).unwrap_or(0),
                "cached_tokens": last_token_usage.as_ref().map(|u| u.cached_tokens).unwrap_or(0),
                "cache_hit_rate": last_token_usage.as_ref().map(|u| u.cache_hit_rate).unwrap_or(0.0),
                "tokens_per_second": last_token_usage.as_ref().map(|u| u.tokens_per_second).unwrap_or(0.0),
                "ttft_ms": last_token_usage.as_ref().map(|u| u.ttft_ms).unwrap_or(0),
                "thinking_duration_ms": last_thinking_duration_ms,
                "tool_duration_ms": last_tool_duration_ms,
                "total_duration_ms": total_dur,
            });

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
                "metrics": metrics_val.clone(),
                "duration_ms": total_dur,
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
                    "metrics": metrics_val,
                }),
            )
            .ok();

            if let Ok(raw_json) = serde_json::to_string(&inst) {
                context.add_assistant_raw_message(&raw_json);
            }

            if repeat_failure_blocked {
                context.add_error_feedback(&error_detail);
                break (inst, dispatch_result);
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
                            std::path::PathBuf::from("src-tauri/prompts/tools")
                                .join(&tool_filename),
                            std::path::PathBuf::from("prompts/tools").join(&tool_filename),
                            std::path::PathBuf::from("../src-tauri/prompts/tools")
                                .join(&tool_filename),
                            resource_dir.join("prompts/tools").join(&tool_filename),
                            std::path::PathBuf::from("src-tauri/prompts/skills")
                                .join(&tool_filename),
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

    Ok(StepExecutionOutcome {
        instruction,
        result,
        token_usage: last_token_usage,
        thinking_duration_ms: last_thinking_duration_ms,
        tool_duration_ms: last_tool_duration_ms,
    })
}

/// 执行持久化状态机的 RunTool 阶段。
///
/// 调用前为未知或可能有副作用的操作创建 pending checkpoint；人工等待也以
/// WaitUser 阶段保存。所有旧进度事件仍由该处理器原样发送。
pub async fn run_tool_phase(
    app: &AppHandle,
    state: &State<'_, DbState>,
    registry_state: &State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    model_routing: &crate::agent::router::ModelRoutingConfig,
    parent_run_id: &str,
    workspace_dir: &Path,
    final_session_id: &String,
    step_id: usize,
    inst: &AgentInstruction,
    context: &mut crate::agent::context::SandwichContext,
    mut checkpoint: &mut AgentCheckpoint,
    mut run_machine: &mut AgentRunMachine,
    rx: &mut mpsc::Receiver<HumanControl>,
    interrupt_rx: &mut watch::Receiver<u64>,
) -> Result<DispatchResult, String> {
    // --- B. 执行动作 ---
    if let Some(reason) =
        context.repeated_failure_blocker(&inst.get_tool(), &inst.get_action(), &inst.get_params())
    {
        return Ok(DispatchResult {
            success: false,
            stdout: String::new(),
            stderr: reason,
            route: REPEATED_FAILURE_BLOCKED_ROUTE.to_string(),
        });
    }

    // --- B0. 重复读取硬拦截：同工具+同路径第 3 次读取且内容未变 → 拒绝并给抓手 ---
    if let Some(block_msg) = context.check_repeat_read_dispatch(inst, workspace_dir) {
        let _ = app.emit(
            &format!("agent-log-{}", final_session_id),
            "🛑 重复读取已拦截：内容未变化，强制切换策略".to_string(),
        );
        return Ok(DispatchResult {
            success: false,
            stdout: String::new(),
            stderr: block_msg,
            route: "host_rejection:repeat_read".to_string(),
        });
    }

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
    // --- B2. 受限 child reviewer：异步派发，不阻塞主循环。结果在后续 step 中由 main_loop 的 poll_completed_child_runs 注入。 ---
    let dispatch_result = if inst.get_tool().eq_ignore_ascii_case("child_reviewer") {
        let routing = crate::agent::router::AgentRouter::route_for_role(
            context,
            model_routing,
            step_id,
            crate::agent::router::roles::REVIEWER,
        );
        let model_tier = routing.tier.to_string();
        let reasoning_effort = model_routing.reasoning_effort_for(&routing.tier).to_string();
        let child_params = inst.get_params();
        // 角色卡：派发参数可携带 "role" 命中角色池 → 钉死专属模型 + 注入人设
        let mut reviewer_persona = String::new();
        let mut reviewer_model = routing.model_id.clone();
        if let Some(role_name) = child_params.get("role").and_then(|v| v.as_str()) {
            match crate::agent::tools::role_manager::resolve_enabled_card(state.inner(), role_name) {
                Ok(Some(card)) => {
                    reviewer_persona = crate::agent::tools::role_manager::persona_block(&card);
                    if !card.model_id.is_empty() {
                        reviewer_model = card.model_id.clone();
                    }
                }
                Ok(None) => {
                    return Ok(DispatchResult {
                        stdout: String::new(),
                        stderr: format!("角色卡不存在或已禁用：{}；可用角色见角色池菜单，或先用 role_manager.upsert 创建。", role_name),
                        success: false,
                        route: "host_rejection:role_card_missing".to_string(),
                    });
                }
                Err(error) => {
                    return Ok(DispatchResult { stdout: String::new(), stderr: error, success: false, route: "host_rejection:role_card_lookup".to_string() });
                }
            }
        }
        match spawn_child_reviewer(
            Some(app.clone()),
            state.inner(),
            parent_run_id,
            final_session_id,
            workspace_dir,
            &reviewer_model,
            &model_tier,
            &reasoning_effort,
            &child_params,
            &reviewer_persona,
            interrupt_rx.clone(),
        )
        .await
        {
            Ok(child_run_id) => DispatchResult {
                stdout: format!(
                    "子审查任务已异步派发，child_run_id: {}。需要结论时调用 wait_jobs(job_ids:[\"{}\"]) 挂起等待，返回值中将直接给出完整审查结论；若先执行其他独立操作，结果也会在后续步骤自动注入。",
                    child_run_id, child_run_id
                ),
                stderr: String::new(),
                success: true,
                route: "child_reviewer:spawned".to_string(),
            },
            Err(error) => DispatchResult {
                stdout: String::new(),
                stderr: error,
                success: false,
                route: "child_reviewer:rejected_or_failed".to_string(),
            },
        }
    // --- B2.6 持续监听：注册周期巡检，外部变化以 watch_event 经 durable inbox 自动注入 ---
    } else if inst.get_tool().eq_ignore_ascii_case("watch_start")
        || inst.get_action().eq_ignore_ascii_case("watch_start")
    {
        let params = inst.get_params();
        let watch_outcome = async {
            let request = crate::agent::watch_engine::parse_watch_request(&params)?;
            crate::agent::watch_engine::begin_watch(
                state.inner(),
                parent_run_id,
                final_session_id,
                &request,
                Some(workspace_dir),
            )
            .await
        };
        match watch_outcome.await {
            Ok(watch_id) => {
                if let Some(watch_state) =
                    crate::agent::watch_engine::load_watch_state(state.inner(), &watch_id)
                {
                    let db_clone = crate::db::DbState(state.inner().0.clone());
                    let session_clone = final_session_id.clone();
                    let watch_clone = watch_id.clone();
                    let workspace_clone = workspace_dir.to_path_buf();
                    tokio::spawn(crate::agent::watch_engine::run_watch_loop(
                        db_clone,
                        Some(app.clone()),
                        session_clone,
                        watch_clone,
                        watch_state.interval_secs,
                        watch_state.last_snapshot,
                        Some(workspace_clone),
                    ));
                }
                let _ = app.emit(
                    &format!("agent-log-{}", final_session_id),
                    format!("👁️ 持续监听已启动: {}", watch_id),
                );
                DispatchResult {
                    stdout: format!(
                        "持续监听已启动，watch_id: {}。此后每个巡检周期内若目标发生变化，变化事件会作为 watch_event 自动注入你的上下文（无需轮询读取）；若长时间无变化则不会产生任何事件。任务收尾前请调用 watch_stop 释放该监听，否则 finish 会被拦截。",
                        watch_id
                    ),
                    stderr: String::new(),
                    success: true,
                    route: "watch:started".to_string(),
                }
            }
            Err(error) => DispatchResult {
                stdout: String::new(),
                stderr: error,
                success: false,
                route: "watch:rejected".to_string(),
            },
        }
    } else if inst.get_tool().eq_ignore_ascii_case("watch_stop")
        || inst.get_action().eq_ignore_ascii_case("watch_stop")
    {
        let params = inst.get_params();
        let stop_outcome = match params.get("watch_id").and_then(|v| v.as_str()) {
            Some(watch_id) => {
                crate::agent::watch_engine::stop_watch(state.inner(), parent_run_id, watch_id)
                    .map(|rows| format!("已停止监听 {}（{} 行）", watch_id, rows))
            }
            None => crate::agent::watch_engine::stop_all_watches(state.inner(), parent_run_id)
                .map(|rows| format!("已停止全部持续监听（{} 行）", rows)),
        };
        match stop_outcome {
            Ok(summary) => DispatchResult {
                stdout: summary,
                stderr: String::new(),
                success: true,
                route: "watch:stopped".to_string(),
            },
            Err(error) => DispatchResult {
                stdout: String::new(),
                stderr: error,
                success: false,
                route: "watch:stop_failed".to_string(),
            },
        }
    // --- B2.2 L3 隔离写入 worker：私有副本上改码跑测试，diff 回执，root 仍是真实工作区唯一写入者 ---
    } else if inst.get_tool().eq_ignore_ascii_case("child_writer")
        || inst.get_action().eq_ignore_ascii_case("child_writer")
    {
        let routing = crate::agent::router::AgentRouter::route_for_role(
            context,
            model_routing,
            step_id,
            crate::agent::router::roles::WRITER,
        );
        let reasoning_effort = model_routing.reasoning_effort_for(&routing.tier).to_string();
        let writer_params = inst.get_params();
        let mut writer_persona = String::new();
        let mut writer_model = routing.model_id.clone();
        let mut effective_params = writer_params.clone();
        if let Some(role_name) = writer_params.get("role").and_then(|v| v.as_str()) {
            match crate::agent::tools::role_manager::resolve_enabled_card(state.inner(), role_name) {
                Ok(Some(card)) => {
                    writer_persona = crate::agent::tools::role_manager::persona_block(&card);
                    if !card.model_id.is_empty() {
                        writer_model = card.model_id.clone();
                    }
                    effective_params = crate::agent::tools::role_manager::merge_writer_defaults(&card, &writer_params);
                }
                Ok(None) => {
                    return Ok(DispatchResult {
                        stdout: String::new(),
                        stderr: format!("角色卡不存在或已禁用：{}。", role_name),
                        success: false,
                        route: "host_rejection:role_card_missing".to_string(),
                    });
                }
                Err(error) => {
                    return Ok(DispatchResult { stdout: String::new(), stderr: error, success: false, route: "host_rejection:role_card_lookup".to_string() });
                }
            }
        }
        match crate::agent::child_writer::spawn_child_writer(
            Some(app.clone()),
            state.inner(),
            parent_run_id,
            final_session_id,
            workspace_dir,
            &writer_model,
            &reasoning_effort,
            &effective_params,
            writer_persona,
            interrupt_rx.clone(),
        )
        .await
        {
            Ok(child_run_id) => DispatchResult {
                stdout: format!(
                    "写入 worker 已在隔离副本上开工，child_run_id: {}。它完成修改并自行运行你指定的测试命令后，会以结构化回执（changed_files 含逐文件 diff、test_status、findings）回到你的上下文；用 wait_jobs(job_ids:[\"{}\"]) 回收。注意：你（root）才是真实工作区的唯一写入者——审阅其 diff 后由你用常规写工具落实修改并最终验证。",
                    child_run_id, child_run_id
                ),
                stderr: String::new(),
                success: true,
                route: "child_writer:spawned".to_string(),
            },
            Err(error) => DispatchResult {
                stdout: String::new(),
                stderr: error,
                success: false,
                route: "child_writer:rejected_or_failed".to_string(),
            },
        }
    // --- B2.1 L2 只读侦察 worker：异步派发独立动作-观测循环，结果经 durable inbox 回收 ---
    } else if inst.get_tool().eq_ignore_ascii_case("child_worker")
        || inst.get_action().eq_ignore_ascii_case("child_worker")
    {
        let routing = crate::agent::router::AgentRouter::route_for_role(
            context,
            model_routing,
            step_id,
            crate::agent::router::roles::SCOUT,
        );
        let reasoning_effort = model_routing.reasoning_effort_for(&routing.tier).to_string();
        let worker_params = inst.get_params();
        let mut scout_persona = String::new();
        let mut scout_model = routing.model_id.clone();
        if let Some(role_name) = worker_params.get("role").and_then(|v| v.as_str()) {
            match crate::agent::tools::role_manager::resolve_enabled_card(state.inner(), role_name) {
                Ok(Some(card)) => {
                    scout_persona = crate::agent::tools::role_manager::persona_block(&card);
                    if !card.model_id.is_empty() {
                        scout_model = card.model_id.clone();
                    }
                }
                Ok(None) => {
                    return Ok(DispatchResult {
                        stdout: String::new(),
                        stderr: format!("角色卡不存在或已禁用：{}。", role_name),
                        success: false,
                        route: "host_rejection:role_card_missing".to_string(),
                    });
                }
                Err(error) => {
                    return Ok(DispatchResult { stdout: String::new(), stderr: error, success: false, route: "host_rejection:role_card_lookup".to_string() });
                }
            }
        }
        match crate::agent::child_worker::spawn_child_worker(
            Some(app.clone()),
            state.inner(),
            parent_run_id,
            final_session_id,
            workspace_dir,
            &scout_model,
            &reasoning_effort,
            &worker_params,
            scout_persona,
            interrupt_rx.clone(),
        )
        .await
        {
            Ok(child_run_id) => DispatchResult {
                stdout: format!(
                    "侦察 worker 已异步派发，child_run_id: {}。worker 拥有独立上下文并自行执行只读侦察（读文件/列目录/只读命令），完成后以结构化结论（findings/confidence/evidence_refs/missing_evidence）回到你的上下文；需要结论时调用 wait_jobs(job_ids:[\"{}\"]) 挂起等待，或先推进其他独立操作。finish 前须确认其已结束。",
                    child_run_id, child_run_id
                ),
                stderr: String::new(),
                success: true,
                route: "child_worker:spawned".to_string(),
            },
            Err(error) => DispatchResult {
                stdout: String::new(),
                stderr: error,
                success: false,
                route: "child_worker:rejected_or_failed".to_string(),
            },
        }
    // --- B2.5 特殊工具：wait_jobs 反应式零 Token 挂起等待 ---
    } else if inst.get_tool().eq_ignore_ascii_case("wait_jobs")
        || inst.get_tool().eq_ignore_ascii_case("wait_job")
        || inst.get_action().eq_ignore_ascii_case("wait_jobs")
    {
        let params = inst.get_params();
        let target_ids: Option<Vec<String>> = params
            .get("job_ids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            });
        let timeout_secs = params
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(45);

        let _ = app.emit(
            &format!("agent-log-{}", final_session_id),
            format!("⏳ 正在挂起等待后台异步任务完成 (超时限制: {}s)...", timeout_secs),
        );
        let _ = app.emit(
            &format!("agent-progress-{}", final_session_id),
            json!({
                "type": "agent_waiting_jobs",
                "message": "正在挂起等待后台任务完成...",
                "job_ids": target_ids.clone().unwrap_or_default(),
            }),
        );

        let mut rx_watch = interrupt_rx.clone();
        match crate::agent::job_engine::wait_for_jobs(
            state.inner(),
            parent_run_id,
            target_ids.as_deref(),
            timeout_secs,
            Some(&mut rx_watch),
        )
        .await
        {
            Ok(completed_jobs) => {
                let mut out = String::new();
                if completed_jobs.is_empty() {
                    out.push_str("等待结束：当前无新完成的后台任务，或所有后台任务已结束。");
                } else {
                    out.push_str(&format!("✅ 已成功等待并接收到 {} 个后台任务结果：\n", completed_jobs.len()));
                    for job in completed_jobs {
                        out.push_str(&format!(
                            "\n- 【任务 ID: {} | 类型: {}】状态: {}, 耗时: {}ms\n  描述: {}\n  输出: {}\n",
                            job.job_id,
                            job.job_type,
                            job.status,
                            job.duration_ms,
                            job.title,
                            if job.result_json.is_empty() { "（无附加输出）" } else { &job.result_json }
                        ));
                    }
                }
                DispatchResult {
                    stdout: out,
                    stderr: String::new(),
                    success: true,
                    route: "wait_jobs:resolved".to_string(),
                }
            }
            Err(err) => DispatchResult {
                stdout: String::new(),
                stderr: err,
                success: false,
                route: "wait_jobs:interrupted_or_failed".to_string(),
            },
        }
    // --- B3. 特殊拦截：AI 主动挂起呼叫人类 ---
    } else if inst.get_tool().eq_ignore_ascii_case("ask_human") || inst.get_action().eq_ignore_ascii_case("ask") {
        let params = inst.get_params();
        let form_val = params.get("form");
        let ask_msg = params
            .get("message")
            .or_else(|| form_val.and_then(|f| f.get("title")))
            .or_else(|| form_val.and_then(|f| f.get("description")))
            .and_then(|v| v.as_str())
            .unwrap_or("AI 请求人类协助与指示")
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
                "form": form_val.cloned().unwrap_or(serde_json::Value::Null)
            }),
        )
        .ok();

        println!("⏸️ [阻塞点] 模型触发 ask_human 挂起，正在死等前端人类唤醒...");

        // 真正的阻塞点：死等人类回复！
        // rx 被我们在外层定义过了，这里可以直接 `.recv().await`
        checkpoint.prepared_instruction = Some(inst.clone());
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
            context
                .turns_history
                .push(crate::agent::context::types::ChatMessage {
                    role: "user".to_string(),
                    content: serde_json::json!(no_progress_notice),
                });
            println!("🔁 [No-Progress] 注入无进展提示: {}", inst.get_tool());
        }
        // 检索零命中偏航：换措辞换关键词连续零命中不会触发上面的重复检测
        // （签名各不相同）；这里按（工具族×作用域）组计数，只提示一次，
        // 同样只注入事实提示，不修改、不拒绝、不强制 finish。
        if let Some(zero_hit_notice) = context.zero_hit.record(
            &inst.get_tool(),
            &inst.get_params(),
            dispatch_result.success,
            &output,
        ) {
            context
                .turns_history
                .push(crate::agent::context::types::ChatMessage {
                    role: "user".to_string(),
                    content: serde_json::json!(zero_hit_notice),
                });
            println!("🎯 [Zero-Hit] 注入检索零命中偏航提示: {}", inst.get_tool());
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
