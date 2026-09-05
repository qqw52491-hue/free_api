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
use crate::llm::ModelConfig;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
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
        let open_todos_for_early_check = crate::agent::finish_guard::finish_blockers(
            &context.todo_list,
            context.review_contract.as_ref(),
            false,
            0, // 避免把待闭合验证当作流式早期中断条件，允许模型在 finish 时同时递交 change_verification_update
            None,
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
                        if exact.contains("未找到工具") || exact.contains("离线") || exact.contains("连接失败") {
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

/// 前端下发方案审批结论时使用的结构化标记，形如 `<!--plan-decision:{"approved":true}-->`。
/// 采用 HTML 注释形态，既不会干扰 Markdown 方案正文的渲染，也不会被模型当作指令文本。
const PLAN_DECISION_MARKER_PREFIX: &str = "<!--plan-decision:";
const PLAN_DECISION_MARKER_SUFFIX: &str = "-->";

/// 方案审批的结构化结论（标记载荷的类型化契约）。
///
/// 与前端 `AgentHumanInterventionCard` / 移动端 app.js 的下发格式保持一致；
/// 结构变化必须双向同步，后端解析用本结构而非裸 JSON Value，避免字段拼写漂移静默失效。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct PlanDecision {
    /// true = 批准放行；false = 打回重新推演
    pub approved: bool,
    /// 打回时用户附带的修改意见（可选）
    #[serde(default)]
    pub note: Option<String>,
}

/// 从结构化标记中提取类型化的审批结论。
pub(crate) fn extract_plan_decision(reply: &str) -> Option<PlanDecision> {
    let start = reply.find(PLAN_DECISION_MARKER_PREFIX)?;
    let rest = &reply[start + PLAN_DECISION_MARKER_PREFIX.len()..];
    let end = rest.find(PLAN_DECISION_MARKER_SUFFIX)?;
    serde_json::from_str::<PlanDecision>(rest[..end].trim()).ok()
}

/// 剥离回复末尾的结构化审批标记，用于日志展示与持久化摘要。
pub(crate) fn strip_plan_decision_marker(reply: &str) -> String {
    match reply.find(PLAN_DECISION_MARKER_PREFIX) {
        Some(start) => {
            let head = &reply[..start];
            let rest = &reply[start + PLAN_DECISION_MARKER_PREFIX.len()..];
            match rest.find(PLAN_DECISION_MARKER_SUFFIX) {
                // 标记之后若还有正文（理论上不应出现），保留尾部内容
                Some(end) => format!("{}{}", head.trim_end(), rest[end + PLAN_DECISION_MARKER_SUFFIX.len()..].trim_end()),
                None => reply.to_string(),
            }
        }
        None => reply.to_string(),
    }
}

/// 从人类回复中解析方案审批结论。
///
/// 返回 `Some(true)` = 批准放行；`Some(false)` = 打回重新推演；`None` = 无法判定。
///
/// 判定顺序：
/// 1. 优先解析前端下发的结构化标记，这是权威来源，**不受用户自然语言内容干扰**；
/// 2. 无标记时（CLI / 直接调用等非 UI 通道）回退到关键词嗅探；
/// 3. 仍无法判定时返回 `None`，由调用方保持门禁关闭，绝不默认放行。
pub(crate) fn parse_plan_approval_decision(reply: &str) -> Option<bool> {
    // 1. 结构化标记：权威来源。即便用户补充意见里写了"打回修改"等字样也不会误判。
    if let Some(decision) = extract_plan_decision(reply) {
        return Some(decision.approved);
    }

    // 2. 回退：否定信号优先于肯定信号，避免"我不太同意但先按这个来"被判为批准
    let negative = [
        "【方案打回修改】", "打回修改", "\"approved\":false", "\"confirm\":false",
        "打回", "不同意", "拒绝", "不行", "别写", "不要写", "重新推演", "重新审查", "暂不批准",
    ];
    if negative.iter().any(|m| reply.contains(m)) {
        return Some(false);
    }
    let positive = [
        "【方案已批准】", "批准方案", "同意该方案", "方案通过",
        "批准", "同意", "通过", "好的", "好", "可以", "行", "没问题", "继续", "开始", "开始执行",
        "ok", "yes", "approve", "go", "proceed",
    ];
    let reply_lower = reply.to_ascii_lowercase();
    if positive.iter().any(|m| reply_lower.contains(m)) {
        return Some(true);
    }

    // 3. 无法判定：交由调用方保持门禁关闭
    None
}

/// 统合前置写入门禁判定（支持全自动、双 Agent 交叉审查、人工方案审批三种模式）
/// - 若放行返回 None，并适时将 plan_phase 置为 Approved；
/// - 若拦截返回 Some(DispatchResult)。
pub(crate) fn check_pre_write_gatekeeper(
    checkpoint: &mut AgentCheckpoint,
    context: &crate::agent::context::SandwichContext,
    db: &DbState,
    parent_run_id: &str,
    step_id: usize,
    is_resume_child: bool,
) -> Option<DispatchResult> {
    if checkpoint.plan_phase.is_approved() {
        return None;
    }

    match checkpoint.plan_mode {
        crate::agent::checkpoint::PlanExecutionMode::PlanApproval => {
            let stderr = if is_resume_child {
                crate::agent::gate_messages::plan_approval_resume_child_blocked()
            } else {
                crate::agent::gate_messages::plan_approval_need_human_approval()
            };
            Some(DispatchResult {
                success: false,
                stdout: String::new(),
                stderr,
                route: "plan_gatekeeper:need_human_approval".to_string(),
            })
        }
        crate::agent::checkpoint::PlanExecutionMode::CrossReview => {
            // 先探测后台是否仍有审查任务在跑：用于区分「结论还在路上」与「派发压根没产生任务」。
            // 若不区分，后者会让模型只会反复调用 wait_jobs 收空信箱，而 consumed 永远置不上位，写入被无限拦截。
            let reviewer_running =
                crate::agent::child_reviewer::has_running_child_runs(db, parent_run_id);
            if checkpoint.plan_reviewer_retry_count == 0 {
                Some(DispatchResult {
                    success: false,
                    stdout: String::new(),
                    stderr: crate::agent::gate_messages::cross_review_need_reviewer_dispatch(),
                    route: "plan_gatekeeper:need_reviewer_dispatch".to_string(),
                })
            } else if reviewer_running
                || (!checkpoint.plan_reviewer_consumed
                    && crate::agent::child_reviewer::has_pending_review_results(db, parent_run_id))
            {
                Some(DispatchResult {
                    success: false,
                    stdout: String::new(),
                    stderr: crate::agent::gate_messages::cross_review_need_wait_reviewer_jobs(),
                    route: "plan_gatekeeper:need_wait_reviewer_jobs".to_string(),
                })
            } else if !checkpoint.plan_reviewer_consumed {
                // 已消耗过审查轮次，但后台既没有在跑的任务、也没有待回收的结论 ——
                // 说明上一次派发并未真正启动（角色卡未授权/不存在、参数非法等）。
                // 此时必须引导模型重新派发，否则它会一直 wait_jobs 收空信箱而永久无法写入。
                Some(DispatchResult {
                    success: false,
                    stdout: String::new(),
                    stderr: crate::agent::gate_messages::cross_review_reviewer_result_missing(),
                    route: "plan_gatekeeper:reviewer_result_missing".to_string(),
                })
            } else if let Some(ref scorecard) = checkpoint.last_review_scorecard {
                if !scorecard.passes_gate() {
                    let reason = scorecard.gate_rejection_reason().unwrap_or_else(|| "审查评分未达放行门槛".to_string());
                    Some(DispatchResult {
                        success: false,
                        stdout: String::new(),
                        stderr: crate::agent::gate_messages::cross_review_scorecard_failed(&reason),
                        route: "plan_gatekeeper:scorecard_failed".to_string(),
                    })
                } else {
                    checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
                    None
                }
            } else {
                checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
                None
            }
        }
        crate::agent::checkpoint::PlanExecutionMode::Auto => {
            if !is_resume_child && step_id == 0 && context.turns_history.is_empty() {
                Some(DispatchResult {
                    success: false,
                    stdout: String::new(),
                    stderr: crate::agent::gate_messages::auto_mode_need_plan(),
                    route: "plan_gatekeeper:need_plan".to_string(),
                })
            } else {
                checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
                None
            }
        }
    }
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

    // --- B1.5 P4 连续批发派发解析失败降级熔断检查 ---
    if let Err(breaker_msg) = context.check_batch_dispatch_allowed(inst.has_dispatches()) {
        return Ok(DispatchResult {
            stdout: String::new(),
            stderr: breaker_msg,
            success: false,
            route: "host_rejection:batch_circuit_breaker".to_string(),
        });
    }

    // --- B1.8 前置方案门禁拦截：未获得方案审批前，禁止直接调用破坏性修改工具或派发写入类子 Agent ---
    let is_write_tool = is_write_or_side_effect_tool(inst)
        || inst.get_tool().eq_ignore_ascii_case("child_writer")
        || inst.get_action().eq_ignore_ascii_case("child_writer");

    if is_write_tool {
        if let Some(rejection) = check_pre_write_gatekeeper(
            checkpoint,
            context,
            state.inner(),
            parent_run_id,
            step_id,
            false,
        ) {
            emit_tool_event(
                app,
                final_session_id,
                step_id,
                inst,
                "failed",
                Some(rejection.route.clone()),
                Some(false),
                &rejection.stderr,
                Some(tool_started_at.elapsed().as_millis()),
            );
            return Ok(rejection);
        }
    }

    // --- B2. 受限 child reviewer：异步派发，不阻塞主循环。结果在后续 step 中由 main_loop 的 poll_completed_child_runs 注入。 ---
    let dispatch_result = if inst.get_tool().eq_ignore_ascii_case("child_reviewer") {
        // 记录派发前的消费状态：若本次派发一个任务都没起来，需原样回滚，避免白耗一轮审查预算。
        let pre_dispatch_consumed = checkpoint.plan_reviewer_consumed;
        // 方案审查轮数上限只约束批准前的方案审查讨论；
        // 已批准后的 child_reviewer 属于交付验证等其他用途，不消耗该预算、也不改变方案阶段。
        if !checkpoint.plan_phase.is_approved() {
            if checkpoint.plan_reviewer_retry_count
                >= crate::agent::checkpoint::MAX_PLAN_REVIEWER_RETRIES
            {
                let limit_msg = "【审查轮数已达上限（2 轮）】方案审查讨论已达最大重试上限，请停止继续派发审查子 Agent。\n请综合已有审查建议，整理最终技术方案并调用 ask_human（kind: 'plan_approval'）呈报给用户进行人工审批。".to_string();
                let result = DispatchResult {
                    stdout: String::new(),
                    stderr: limit_msg,
                    success: false,
                    route: "plan_gatekeeper:reviewer_retry_limit".to_string(),
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

            // 仅在尚未批准时进入审查中阶段；若已批准，保留 Approved 状态，避免后续补充审查打回 Reviewing 阻塞修改
            if !checkpoint.plan_phase.is_approved() {
                checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Reviewing;
            }
            checkpoint.plan_reviewer_retry_count =
                checkpoint.plan_reviewer_retry_count.saturating_add(1);
            checkpoint.plan_reviewer_consumed = false;
        }

        let routing = crate::agent::router::AgentRouter::route_for_role(
            context,
            model_routing,
            step_id,
            crate::agent::router::roles::REVIEWER,
        );
        let model_tier = routing.tier.to_string();
        let reasoning_effort = model_routing.reasoning_effort_for(&routing.tier).to_string();
        let items = inst.get_dispatch_items();
        let total = items.len();
        let is_batch = total > 1;

        let mut item_reports = Vec::new();
        let mut spawned_ids = Vec::new();
        let mut success_count = 0;
        let mut fail_count = 0;

        for (idx, child_params) in items.iter().enumerate() {
            let item_num = idx + 1;
            let mut reviewer_persona = String::new();
            let mut reviewer_model = routing.model_id.clone();
            let mut effective_params = child_params.clone();
            if let Some(obj) = effective_params.as_object_mut() {
                obj.remove("max_concurrency");
            }

            if let Some(role_name) = child_params.get("role").and_then(|v| v.as_str()) {
                match crate::agent::tools::role_manager::resolve_enabled_card(state.inner(), role_name) {
                    Ok(Some(card)) => {
                        if !card.allows_kind("reviewer") {
                            item_reports.push(format!(
                                "[{}/{}] ❌ 角色卡「{}」未授权挂载为 reviewer（允许类型: {}）。",
                                item_num, total, role_name, card.allowed_kinds
                            ));
                            fail_count += 1;
                            continue;
                        }
                        reviewer_persona = crate::agent::tools::role_manager::persona_block(&card);
                        if !card.model_id.is_empty() {
                            reviewer_model = card.model_id.clone();
                        }
                        if card.max_concurrency > 0 {
                            effective_params["max_concurrency"] = json!(card.max_concurrency.min(crate::agent::child_reviewer::MAX_TOTAL_CHILDREN_PER_RUN as i64));
                        }
                    }
                    Ok(None) => {
                        item_reports.push(format!(
                            "[{}/{}] ❌ 角色卡不存在或已禁用：{}；可用角色见角色池菜单，或先用 role_manager.upsert 创建。",
                            item_num, total, role_name
                        ));
                        fail_count += 1;
                        continue;
                    }
                    Err(error) => {
                        item_reports.push(format!(
                            "[{}/{}] ❌ 角色卡查询失败：{}。",
                            item_num, total, error
                        ));
                        fail_count += 1;
                        continue;
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
                &effective_params,
                &reviewer_persona,
                interrupt_rx.clone(),
            )
            .await
            {
                Ok(child_run_id) => {
                    let question_str = effective_params.get("question").and_then(|v| v.as_str()).unwrap_or("");
                    let question_brief = if question_str.chars().count() > 40 {
                        format!("{}...", question_str.chars().take(40).collect::<String>())
                    } else {
                        question_str.to_string()
                    };
                    item_reports.push(format!(
                        "[{}/{}] ✅ 已派发 child_run_id={} 审查任务: {}",
                        item_num, total, child_run_id, question_brief
                    ));
                    spawned_ids.push(child_run_id);
                    success_count += 1;
                }
                Err(error) => {
                    item_reports.push(format!(
                        "[{}/{}] ❌ 派发失败：{}",
                        item_num, total, error
                    ));
                    fail_count += 1;
                }
            }
        }

        // 一个审查任务都没起来（角色卡未授权/不存在、参数非法等）时回滚本轮审查预算：
        // 否则失败也会消耗仅有的 2 轮额度，额度耗尽后派发被限流、结论又永远收不到，
        // 交叉审查模式将陷入「既派不出、也写不了」的永久阻塞。
        if success_count == 0 && !checkpoint.plan_phase.is_approved() {
            checkpoint.rollback_plan_review_dispatch(pre_dispatch_consumed);
        }

        if !is_batch {
            if success_count == 1 {
                let child_run_id = &spawned_ids[0];
                let mut stdout = format!(
                    "子审查任务已异步派发，child_run_id: {}。需要结论时调用 wait_jobs(job_ids:[\"{}\"]) 挂起等待，返回值中将直接给出完整审查结论；若先执行其他独立操作，结果也会在后续步骤自动注入。",
                    child_run_id, child_run_id
                );
                if inst.is_mixed_dispatch_format() {
                    stdout = format!("⚠️ 检测到混合格式（同时包含 dispatches 与单发字段），系统已优先采用 dispatches。\n\n{}", stdout);
                }
                DispatchResult {
                    stdout,
                    stderr: String::new(),
                    success: true,
                    route: "child_reviewer:spawned".to_string(),
                }
            } else {
                let err_msg = item_reports.into_iter().next().unwrap_or_else(|| "派发失败".to_string());
                DispatchResult {
                    stdout: String::new(),
                    stderr: err_msg,
                    success: false,
                    route: "child_reviewer:rejected_or_failed".to_string(),
                }
            }
        } else {
            let mut summary = format!(
                "已批量处理 {} 个审查任务派发请求（{} 成功，{} 失败）：\n{}",
                total, success_count, fail_count, item_reports.join("\n")
            );
            if !spawned_ids.is_empty() {
                let ids_formatted: Vec<String> = spawned_ids.iter().map(|id| format!("\"{}\"", id)).collect();
                summary.push_str(&format!(
                    "\n\n已成功派发的审查任务均在后台异步执行；需要结论时调用 wait_jobs(job_ids:[{}]) 挂起等待，或先推进其他独立操作。",
                    ids_formatted.join(", ")
                ));
            }
            if inst.is_mixed_dispatch_format() {
                summary = format!("⚠️ 检测到混合格式（同时包含 dispatches 与单发字段），系统已优先采用 dispatches 批量派发。\n\n{}", summary);
            }
            let breaker_alert = context.record_batch_dispatch_outcome(success_count > 0);
            if success_count > 0 {
                DispatchResult {
                    stdout: summary,
                    stderr: String::new(),
                    success: true,
                    route: format!("child_reviewer:batch:{}ok{}fail", success_count, fail_count),
                }
            } else {
                let mut err = format!("批量审查派发全部失败（共 {} 项）：\n{}", total, item_reports.join("\n"));
                if let Some(alert) = breaker_alert {
                    err = format!("{}\n\n{}", err, alert);
                }
                DispatchResult {
                    stdout: String::new(),
                    stderr: err,
                    success: false,
                    route: "child_reviewer:rejected_or_failed".to_string(),
                }
            }
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
        let items = inst.get_dispatch_items();
        let total = items.len();
        let is_batch = total > 1;

        let mut item_reports = Vec::new();
        let mut spawned_ids = Vec::new();
        let mut success_count = 0;
        let mut fail_count = 0;

        for (idx, writer_params) in items.iter().enumerate() {
            let item_num = idx + 1;
            let mut writer_persona = String::new();
            let mut writer_model = routing.model_id.clone();
            let mut effective_params = writer_params.clone();
            if let Some(obj) = effective_params.as_object_mut() {
                obj.remove("max_concurrency");
            }

            if let Some(role_name) = writer_params.get("role").and_then(|v| v.as_str()) {
                match crate::agent::tools::role_manager::resolve_enabled_card(state.inner(), role_name) {
                    Ok(Some(card)) => {
                        if !card.allows_kind("writer") {
                            item_reports.push(format!(
                                "[{}/{}] ❌ 角色卡「{}」未授权挂载为 writer（允许类型: {}）。",
                                item_num, total, role_name, card.allowed_kinds
                            ));
                            fail_count += 1;
                            continue;
                        }
                        writer_persona = crate::agent::tools::role_manager::persona_block(&card);
                        if !card.model_id.is_empty() {
                            writer_model = card.model_id.clone();
                        }
                        effective_params = crate::agent::tools::role_manager::merge_writer_defaults(&card, writer_params);
                        if let Some(obj) = effective_params.as_object_mut() {
                            obj.remove("max_concurrency");
                        }
                        if !card.hard_scope.is_empty() {
                            effective_params["hard_scope"] = json!(card.hard_scope);
                        }
                        if card.max_concurrency > 0 {
                            effective_params["max_concurrency"] = json!(card.max_concurrency.min(crate::agent::child_reviewer::MAX_TOTAL_CHILDREN_PER_RUN as i64));
                        }
                    }
                    Ok(None) => {
                        item_reports.push(format!(
                            "[{}/{}] ❌ 角色卡不存在或已禁用：{}。",
                            item_num, total, role_name
                        ));
                        fail_count += 1;
                        continue;
                    }
                    Err(error) => {
                        item_reports.push(format!(
                            "[{}/{}] ❌ 角色卡查询失败：{}。",
                            item_num, total, error
                        ));
                        fail_count += 1;
                        continue;
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
                Ok(child_run_id) => {
                    let task_str = effective_params.get("task").and_then(|v| v.as_str()).unwrap_or("");
                    let task_brief = if task_str.chars().count() > 40 {
                        format!("{}...", task_str.chars().take(40).collect::<String>())
                    } else {
                        task_str.to_string()
                    };
                    item_reports.push(format!(
                        "[{}/{}] ✅ 已派发 child_run_id={} 写入任务: {}",
                        item_num, total, child_run_id, task_brief
                    ));
                    spawned_ids.push(child_run_id);
                    success_count += 1;
                }
                Err(error) => {
                    item_reports.push(format!(
                        "[{}/{}] ❌ 派发失败：{}",
                        item_num, total, error
                    ));
                    fail_count += 1;
                }
            }
        }

        if !is_batch {
            if success_count == 1 {
                let child_run_id = &spawned_ids[0];
                let writer_params = &items[0];
                let mut stdout = format!(
                    "写入 worker 已在隔离副本上开工，child_run_id: {}。它完成修改并自行运行你指定的测试命令后，会以结构化回执（changed_files 含逐文件 diff、test_status、findings）回到你的上下文；用 wait_jobs(job_ids:[\"{}\"]) 回收。注意：你（root）才是真实工作区的唯一写入者——审阅其 diff 后由你用常规写工具落实修改并最终验证。",
                    child_run_id, child_run_id
                );
                let task_str = writer_params.get("task").and_then(|v| v.as_str()).unwrap_or("");
                let has_excerpts = writer_params.get("context_excerpts").is_some();
                if crate::agent::types::is_task_thin(task_str) && !has_excerpts {
                    stdout = format!("⚠️ 本次派发任务描述较简略，子 Agent 可能缺乏必要背景，建议在后续派发时携带 context_excerpts（包含 background、file_hints 或 todo_slice）。\n\n{}", stdout);
                }
                if inst.is_mixed_dispatch_format() {
                    stdout = format!("⚠️ 检测到混合格式（同时包含 dispatches 与单发字段），系统已优先采用 dispatches。\n\n{}", stdout);
                }
                DispatchResult {
                    stdout,
                    stderr: String::new(),
                    success: true,
                    route: "child_writer:spawned".to_string(),
                }
            } else {
                let err_msg = item_reports.into_iter().next().unwrap_or_else(|| "派发失败".to_string());
                DispatchResult {
                    stdout: String::new(),
                    stderr: err_msg,
                    success: false,
                    route: "child_writer:rejected_or_failed".to_string(),
                }
            }
        } else {
            let mut summary = format!(
                "已批量处理 {} 个写入 worker 派发请求（{} 成功，{} 失败）：\n{}",
                total, success_count, fail_count, item_reports.join("\n")
            );
            if !spawned_ids.is_empty() {
                let ids_formatted: Vec<String> = spawned_ids.iter().map(|id| format!("\"{}\"", id)).collect();
                summary.push_str(&format!(
                    "\n\n所有已派发的写入 worker 均在各自的私有副本上独立执行；完成后请调用 wait_jobs(job_ids:[{}]) 回收 diff 审查。",
                    ids_formatted.join(", ")
                ));
            }
            if inst.is_mixed_dispatch_format() {
                summary = format!("⚠️ 检测到混合格式（同时包含 dispatches 与单发字段），系统已优先采用 dispatches 批量派发。\n\n{}", summary);
            }
            let breaker_alert = context.record_batch_dispatch_outcome(success_count > 0);
            if success_count > 0 {
                DispatchResult {
                    stdout: summary,
                    stderr: String::new(),
                    success: true,
                    route: format!("child_writer:batch:{}ok{}fail", success_count, fail_count),
                }
            } else {
                let mut err = format!("批量写入 worker 派发全部失败（共 {} 项）：\n{}", total, item_reports.join("\n"));
                if let Some(alert) = breaker_alert {
                    err = format!("{}\n\n{}", err, alert);
                }
                DispatchResult {
                    stdout: String::new(),
                    stderr: err,
                    success: false,
                    route: "child_writer:rejected_or_failed".to_string(),
                }
            }
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
        let items = inst.get_dispatch_items();
        let total = items.len();
        let is_batch = total > 1;

        let mut item_reports = Vec::new();
        let mut spawned_ids = Vec::new();
        let mut success_count = 0;
        let mut fail_count = 0;

        for (idx, worker_params) in items.iter().enumerate() {
            let item_num = idx + 1;
            let mut scout_persona = String::new();
            let mut scout_model = routing.model_id.clone();
            let mut effective_worker_params = worker_params.clone();
            if let Some(obj) = effective_worker_params.as_object_mut() {
                obj.remove("max_concurrency");
            }

            if let Some(role_name) = worker_params.get("role").and_then(|v| v.as_str()) {
                match crate::agent::tools::role_manager::resolve_enabled_card(state.inner(), role_name) {
                    Ok(Some(card)) => {
                        if !card.allows_kind("worker") {
                            item_reports.push(format!(
                                "[{}/{}] ❌ 角色卡「{}」未授权挂载为 worker（允许类型: {}）。",
                                item_num, total, role_name, card.allowed_kinds
                            ));
                            fail_count += 1;
                            continue;
                        }
                        scout_persona = crate::agent::tools::role_manager::persona_block(&card);
                        if !card.model_id.is_empty() {
                            scout_model = card.model_id.clone();
                        }
                        if !card.hard_scope.is_empty() {
                            effective_worker_params["hard_scope"] = json!(card.hard_scope);
                        }
                        if card.max_concurrency > 0 {
                            effective_worker_params["max_concurrency"] = json!(card.max_concurrency.min(crate::agent::child_reviewer::MAX_TOTAL_CHILDREN_PER_RUN as i64));
                        }
                    }
                    Ok(None) => {
                        item_reports.push(format!(
                            "[{}/{}] ❌ 角色卡不存在或已禁用：{}。",
                            item_num, total, role_name
                        ));
                        fail_count += 1;
                        continue;
                    }
                    Err(error) => {
                        item_reports.push(format!(
                            "[{}/{}] ❌ 角色卡查询失败：{}。",
                            item_num, total, error
                        ));
                        fail_count += 1;
                        continue;
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
                &effective_worker_params,
                scout_persona,
                interrupt_rx.clone(),
            )
            .await
            {
                Ok(child_run_id) => {
                    let task_str = effective_worker_params.get("task").and_then(|v| v.as_str()).unwrap_or("");
                    let task_brief = if task_str.chars().count() > 40 {
                        format!("{}...", task_str.chars().take(40).collect::<String>())
                    } else {
                        task_str.to_string()
                    };
                    item_reports.push(format!(
                        "[{}/{}] ✅ 已派发 child_run_id={} 任务: {}",
                        item_num, total, child_run_id, task_brief
                    ));
                    spawned_ids.push(child_run_id);
                    success_count += 1;
                }
                Err(error) => {
                    item_reports.push(format!(
                        "[{}/{}] ❌ 派发失败：{}",
                        item_num, total, error
                    ));
                    fail_count += 1;
                }
            }
        }

        if !is_batch {
            if success_count == 1 {
                let child_run_id = &spawned_ids[0];
                let worker_params = &items[0];
                let mut stdout = format!(
                    "侦察 worker 已异步派发，child_run_id: {}。worker 拥有独立上下文并自行执行只读侦察（读文件/列目录/只读命令），完成后以结构化结论（findings/confidence/evidence_refs/missing_evidence）回到你的上下文；需要结论时调用 wait_jobs(job_ids:[\"{}\"]) 挂起等待，或先推进其他独立操作。finish 前须确认其已结束。",
                    child_run_id, child_run_id
                );
                let task_str = worker_params.get("task").and_then(|v| v.as_str()).unwrap_or("");
                let has_excerpts = worker_params.get("context_excerpts").is_some();
                if crate::agent::types::is_task_thin(task_str) && !has_excerpts {
                    stdout = format!("⚠️ 本次派发任务描述较简略，子 Agent 可能缺乏必要背景，建议在后续派发时携带 context_excerpts（包含 background、file_hints 或 todo_slice）。\n\n{}", stdout);
                }
                if inst.is_mixed_dispatch_format() {
                    stdout = format!("⚠️ 检测到混合格式（同时包含 dispatches 与单发字段），系统已优先采用 dispatches。\n\n{}", stdout);
                }
                DispatchResult {
                    stdout,
                    stderr: String::new(),
                    success: true,
                    route: "child_worker:spawned".to_string(),
                }
            } else {
                let err_msg = item_reports.into_iter().next().unwrap_or_else(|| "派发失败".to_string());
                DispatchResult {
                    stdout: String::new(),
                    stderr: err_msg,
                    success: false,
                    route: "child_worker:rejected_or_failed".to_string(),
                }
            }
        } else {
            let mut summary = format!(
                "已批量处理 {} 个 worker 派发请求（{} 成功，{} 失败）：\n{}",
                total, success_count, fail_count, item_reports.join("\n")
            );
            if !spawned_ids.is_empty() {
                let ids_formatted: Vec<String> = spawned_ids.iter().map(|id| format!("\"{}\"", id)).collect();
                summary.push_str(&format!(
                    "\n\n已成功派发的 worker 均在后台异步执行；需要其结论时调用 wait_jobs(job_ids:[{}]) 挂起等待，或先推进其他独立操作。",
                    ids_formatted.join(", ")
                ));
            }
            if inst.is_mixed_dispatch_format() {
                summary = format!("⚠️ 检测到混合格式（同时包含 dispatches 与单发字段），系统已优先采用 dispatches 批量派发。\n\n{}", summary);
            }
            let breaker_alert = context.record_batch_dispatch_outcome(success_count > 0);
            if success_count > 0 {
                DispatchResult {
                    stdout: summary,
                    stderr: String::new(),
                    success: true,
                    route: format!("child_worker:batch:{}ok{}fail", success_count, fail_count),
                }
            } else {
                let mut err = format!("批量派发全部失败（共 {} 项）：\n{}", total, item_reports.join("\n"));
                if let Some(alert) = breaker_alert {
                    err = format!("{}\n\n{}", err, alert);
                }
                DispatchResult {
                    stdout: String::new(),
                    stderr: err,
                    success: false,
                    route: "child_worker:rejected_or_failed".to_string(),
                }
            }
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
                if completed_jobs
                    .iter()
                    .any(|j| j.job_type == "child_reviewer" || j.job_type.contains("reviewer"))
                {
                    checkpoint.plan_reviewer_consumed = true;
                }
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
    // --- B2.6 特殊工具：resume_child 追问已完成的子任务 ---
    } else if inst.get_tool().eq_ignore_ascii_case("resume_child")
        || inst.get_action().eq_ignore_ascii_case("resume_child")
    {
        let params = inst.get_params();
        let target_child_id = params
            .get("child_run_id")
            .or_else(|| params.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let followup_msg = params
            .get("message")
            .or_else(|| params.get("task"))
            .or_else(|| params.get("query"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let test_command_override = params
            .get("test_command")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string());

        if target_child_id.is_empty() {
            DispatchResult {
                stdout: String::new(),
                stderr: "❌ resume_child 缺少必要参数 child_run_id。".to_string(),
                success: false,
                route: "host_rejection:missing_child_run_id".to_string(),
            }
        } else if followup_msg.is_empty() {
            DispatchResult {
                stdout: String::new(),
                stderr: "❌ resume_child 缺少必要参数 message（对该子任务的追问或补充要求）。".to_string(),
                success: false,
                route: "host_rejection:missing_followup_message".to_string(),
            }
        } else {
            let child_data: Result<(String, String, Option<String>, Option<String>, String, String, Option<String>), String> = {
                let conn = state.0.lock().map_err(|e| e.to_string())?;
                conn.query_row(
                    "SELECT status, kind, workspace_copy, message_history_json, model_id, workspace_root, result_json FROM agent_child_runs WHERE id = ?1",
                    rusqlite::params![target_child_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, Option<String>>(6)?,
                        ))
                    },
                ).map_err(|e| format!("未找到子任务「{}」记录: {}", target_child_id, e))
            };

            match child_data {
                Err(err) => DispatchResult {
                    stdout: String::new(),
                    stderr: err,
                    success: false,
                    route: "host_rejection:child_not_found".to_string(),
                },
                Ok((status, kind, ws_copy, hist_json, child_model, root_str, prev_result_json)) => {
                    if status != "succeeded" {
                        DispatchResult {
                            stdout: String::new(),
                            stderr: format!("❌ 仅能追问已成功的子任务（该任务当前状态为「{}」）；如需继续请直接派发新任务。", status),
                            success: false,
                            route: "host_rejection:status_not_succeeded".to_string(),
                        }
                    } else if kind == "review" || kind == "reviewer" {
                        // 🌟 Reviewer 纯上下文对质分支：只读审查者无工作区副本，纯继承脱水历史进行反驳研判
                        let mut messages: Vec<Value> = match hist_json.as_deref() {
                            Some(raw) => serde_json::from_str(raw).unwrap_or_default(),
                            None => Vec::new(),
                        };
                        if messages.is_empty() {
                            DispatchResult {
                                stdout: String::new(),
                                stderr: "❌ 该审查任务未留存历史对话记录，无法恢复对质。".to_string(),
                                success: false,
                                route: "host_rejection:missing_history".to_string(),
                            }
                        } else {
                            messages.push(json!({
                                "role": "user",
                                "content": format!(
                                    "【父 Agent 反驳与对质】\n{}\n\n【对质研判规则】：\n请针对上述反驳与证据进行重新研判，输出更新后的审查 JSON：\n1. 若父 Agent 反驳与测试证据成立，请将该项缺陷撤回（从 findings 中剔除，并在 missing_evidence 说明已撤回原 finding ID）；\n2. 若仍坚持该缺陷，必须在 why 中给出全新证据与理由；\n3. 重新推导 routing（certain/needs_alignment/blocked）。",
                                    followup_msg
                                )
                            }));

                            let routing = crate::agent::router::AgentRouter::route_for_role(
                                context,
                                model_routing,
                                step_id,
                                crate::agent::router::roles::REVIEWER,
                            );
                            let model_to_use = if !child_model.is_empty() {
                                child_model
                            } else {
                                routing.model_id.clone()
                            };
                            let reasoning_effort = model_routing.reasoning_effort_for(&routing.tier).to_string();

                            let new_child_run_id = match crate::agent::child_reviewer::begin_child_run_extended(
                                state.inner(),
                                parent_run_id,
                                final_session_id,
                                workspace_dir,
                                &model_to_use,
                                0,
                                "review",
                                None,
                                None,
                                Some(target_child_id),
                            ) {
                                Ok(id) => id,
                                Err(err) => {
                                    return Ok(DispatchResult {
                                        stdout: String::new(),
                                        stderr: format!("❌ 创建审查对质任务失败: {}", err),
                                        success: false,
                                        route: "host_rejection:begin_run_failed".to_string(),
                                    });
                                }
                            };

                            let mut config = match ModelConfig::from_db_state(state.inner(), &model_to_use) {
                                Ok(cfg) => cfg,
                                Err(err) => {
                                    return Ok(DispatchResult {
                                        stdout: String::new(),
                                        stderr: format!("❌ 加载模型配置失败: {}", err),
                                        success: false,
                                        route: "host_rejection:model_config_failed".to_string(),
                                    });
                                }
                            };
                            config.max_tokens = 900;
                            if !checkpoint.plan_phase.is_approved() {
                                checkpoint.plan_reviewer_retry_count += 1;
                                checkpoint.plan_reviewer_consumed = false;
                            }

                            let db_clone = DbState(state.0.clone());
                            let new_child_run_id_clone = new_child_run_id.clone();
                            let _parent_session_id_clone = final_session_id.to_string();
                            let mut interrupt_rx_clone = interrupt_rx.clone();
                            let _app_clone = app.clone();

                            tokio::spawn(async move {
                                let started_at = std::time::Instant::now();
                                let mut on_stream_event = |_: crate::llm::StreamEvent<'_>| -> bool { true };
                                let call = crate::llm::stream_with_retry(
                                    &config,
                                    &messages,
                                    &reasoning_effort,
                                    &mut interrupt_rx_clone,
                                    1,
                                    &mut on_stream_event,
                                );
                                let stream_result = match tokio::time::timeout(
                                    std::time::Duration::from_secs(45),
                                    call,
                                ).await {
                                    Ok(Ok(res)) => res,
                                    Ok(Err(_e)) => {
                                        crate::agent::child_reviewer::finish_child_run(
                                            &db_clone,
                                            &new_child_run_id_clone,
                                            "failed",
                                            started_at.elapsed().as_millis() as u64,
                                            0,
                                            Some("request_or_stream"),
                                            None,
                                        );
                                        return;
                                    }
                                    Err(_) => {
                                        crate::agent::child_reviewer::finish_child_run(
                                            &db_clone,
                                            &new_child_run_id_clone,
                                            "timed_out",
                                            started_at.elapsed().as_millis() as u64,
                                            0,
                                            Some("timeout"),
                                            None,
                                        );
                                        return;
                                    }
                                };

                                let duration_ms = started_at.elapsed().as_millis() as u64;
                                let completion_tokens = stream_result.usage.map(|u| u.completion_tokens).unwrap_or(0);
                                let (findings, confidence, evidence_refs, missing_evidence, routing) = 
                                    match crate::agent::child_reviewer::parse_child_review_rebuttal_output(&stream_result.full_response) {
                                        Ok(res) => res,
                                        Err(_) => {
                                            crate::agent::child_reviewer::finish_child_run(
                                                &db_clone,
                                                &new_child_run_id_clone,
                                                "invalid_output",
                                                duration_ms,
                                                completion_tokens,
                                                Some("invalid_output"),
                                                None,
                                            );
                                            return;
                                        }
                                    };

                                let review_res = crate::agent::child_reviewer::ChildReviewResult {
                                    child_run_id: new_child_run_id_clone.clone(),
                                    status: "succeeded".to_string(),
                                    findings,
                                    confidence,
                                    evidence_refs,
                                    missing_evidence,
                                    duration_ms,
                                    completion_tokens,
                                    error_kind: None,
                                    raw_result_json: None,
                                    kind: Some("review".to_string()),
                                    routing: Some(routing),
                                    withdrawn_findings: vec![],
                                    scorecard: None,
                                };

                                let res_json = serde_json::to_string(&review_res).ok();
                                crate::agent::child_reviewer::finish_child_run_with_history(
                                    &db_clone,
                                    &new_child_run_id_clone,
                                    "succeeded",
                                    duration_ms,
                                    completion_tokens,
                                    None,
                                    res_json.as_deref(),
                                    serde_json::to_string(&messages).ok().as_deref(),
                                );
                            });

                            DispatchResult {
                                stdout: format!(
                                    "✅ 审查专家对质已成功启动：新 child_run_id={}（纯上下文继承唤醒，正在重新研判反驳）。你可以继续执行其他工作，或使用 wait_jobs 等待结果。",
                                    new_child_run_id
                                ),
                                stderr: String::new(),
                                success: true,
                                route: "resume_child:reviewer_rebuttal_spawned".to_string(),
                            }
                        }
                    } else {
                        // 校验工作区物理存在性（写入/侦察类 worker 必须依赖工作区副本）
                        let ws_path = match ws_copy {
                            Some(ref p) if PathBuf::from(p).exists() => PathBuf::from(p),
                            _ => PathBuf::new(),
                        };
                        if ws_path.as_os_str().is_empty() {
                            DispatchResult {
                                stdout: String::new(),
                                stderr: "❌ 该子任务的工作副本已随父运行结束回收，无法继续追问，请重新派发新任务。".to_string(),
                                success: false,
                                route: "host_rejection:workspace_recycled".to_string(),
                            }
                        } else {
                            let mut messages: Vec<Value> = match hist_json.as_deref() {
                                Some(raw) => serde_json::from_str(raw).unwrap_or_default(),
                                None => Vec::new(),
                            };
                            if messages.is_empty() {
                                DispatchResult {
                                    stdout: String::new(),
                                    stderr: "❌ 该子任务未留存历史对话记录，无法恢复。".to_string(),
                                    success: false,
                                    route: "host_rejection:missing_history".to_string(),
                                }
                            } else {
                                // 注入追问消息
                                messages.push(json!({
                                    "role": "user",
                                    "content": format!("【追问与补充任务】\n{}", followup_msg)
                                }));

                                let routing = crate::agent::router::AgentRouter::route_for_role(
                                    context,
                                    model_routing,
                                    step_id,
                                    if kind == "writer" {
                                        crate::agent::router::roles::WRITER
                                    } else {
                                        crate::agent::router::roles::SCOUT
                                    },
                                );
                                let model_to_use = if !child_model.is_empty() {
                                    child_model
                                } else {
                                    routing.model_id.clone()
                                };
                                let reasoning_effort = model_routing.reasoning_effort_for(&routing.tier).to_string();

                                let new_child_run_id = match crate::agent::child_reviewer::begin_child_run_extended(
                                    state.inner(),
                                    parent_run_id,
                                    final_session_id,
                                    workspace_dir,
                                    &model_to_use,
                                    0,
                                    &kind,
                                    None,
                                    Some(ws_path.to_string_lossy().as_ref()),
                                    Some(target_child_id),
                                ) {
                                    Ok(id) => id,
                                    Err(err) => {
                                        return Ok(DispatchResult {
                                            stdout: String::new(),
                                            stderr: format!("❌ 创建追问子任务失败: {}", err),
                                            success: false,
                                            route: "host_rejection:begin_run_failed".to_string(),
                                        });
                                    }
                                };

                                let config = match ModelConfig::from_db_state(state.inner(), &model_to_use) {
                                    Ok(cfg) => cfg,
                                    Err(err) => {
                                        return Ok(DispatchResult {
                                            stdout: String::new(),
                                            stderr: format!("❌ 加载模型配置失败: {}", err),
                                            success: false,
                                            route: "host_rejection:model_config_failed".to_string(),
                                        });
                                    }
                                };

                                if kind == "writer" {
                                    // 🛑 前置方案门禁：恢复(resume_child)写入型子任务等同于开启新的写入路径，
                                    // 未获批准前必须拦截，防止绕过派发门禁（B1.8）通过旧 writer 子任务改代码。
                                    if let Some(rejection) = check_pre_write_gatekeeper(
                                        checkpoint,
                                        context,
                                        state.inner(),
                                        parent_run_id,
                                        step_id,
                                        true,
                                    ) {
                                        emit_tool_event(
                                            app,
                                            final_session_id,
                                            step_id,
                                            inst,
                                            "failed",
                                            Some(rejection.route.clone()),
                                            Some(false),
                                            &rejection.stderr,
                                            Some(tool_started_at.elapsed().as_millis()),
                                        );
                                        return Ok(rejection);
                                    }

                                    let inherited_test_cmd = prev_result_json
                                        .as_deref()
                                        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                                        .and_then(|v| v.get("test_command").and_then(|s| s.as_str()).map(|s| s.to_string()))
                                        .filter(|s| !s.trim().is_empty());
                                    let test_cmd = test_command_override
                                        .or(inherited_test_cmd)
                                        .unwrap_or_else(|| "cargo test".to_string());
                                    let writer_req = crate::agent::child_writer::WriterRequest {
                                        task: followup_msg.to_string(),
                                        test_command: test_cmd,
                                        focus_paths: Vec::new(),
                                        context_excerpts: None,
                                        hard_scope: String::new(),
                                    };
                                    let max_steps = crate::agent::child_writer::MAX_WRITER_STEPS / 2;
                                    let parent_root = PathBuf::from(&root_str);
                                    tokio::spawn(crate::agent::child_writer::run_writer_loop(
                                        DbState(state.0.clone()),
                                        Some(app.clone()),
                                        final_session_id.to_string(),
                                        new_child_run_id.clone(),
                                        parent_root,
                                        ws_path,
                                        config,
                                        reasoning_effort,
                                        writer_req,
                                        String::new(),
                                        interrupt_rx.clone(),
                                        Some(messages),
                                        Some(max_steps),
                                    ));

                                    DispatchResult {
                                        stdout: format!(
                                            "✅ 写入 worker 追问已成功启动：新 child_run_id={}（继承上轮历史与工作区副本，步数上限减半为 {} 步）。你可以继续执行其他工作，或使用 wait_jobs 等待结果。",
                                            new_child_run_id, max_steps
                                        ),
                                        stderr: String::new(),
                                        success: true,
                                        route: "resume_child:writer_spawned".to_string(),
                                    }
                                } else {
                                    let worker_req = crate::agent::child_worker::WorkerRequest {
                                        task: followup_msg.to_string(),
                                        focus_paths: Vec::new(),
                                        context_excerpts: None,
                                        hard_scope: String::new(),
                                    };
                                    let max_steps = crate::agent::child_worker::MAX_WORKER_STEPS / 2;
                                    tokio::spawn(crate::agent::child_worker::run_worker_loop(
                                        DbState(state.0.clone()),
                                        Some(app.clone()),
                                        final_session_id.to_string(),
                                        new_child_run_id.clone(),
                                        ws_path,
                                        config,
                                        reasoning_effort,
                                        worker_req,
                                        String::new(),
                                        interrupt_rx.clone(),
                                        Some(messages),
                                        Some(max_steps),
                                    ));

                                    DispatchResult {
                                        stdout: format!(
                                            "✅ 侦察 worker 追问已成功启动：新 child_run_id={}（继承上轮历史，步数上限减半为 {} 步）。你可以继续执行其他工作，或使用 wait_jobs 等待结果。",
                                            new_child_run_id, max_steps
                                        ),
                                        stderr: String::new(),
                                        success: true,
                                        route: "resume_child:worker_spawned".to_string(),
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    // --- B3. 特殊拦截：AI 主动挂起呼叫人类 ---
    } else if inst.get_tool().eq_ignore_ascii_case("ask_human") || inst.get_action().eq_ignore_ascii_case("ask") {
        let params = inst.get_params();
        let form_val = params.get("form");
        // 与前端 normalizeHumanForm 的别名表保持一致（plan_approval / plan / approval），
        // 防止前端渲染出审批卡片而后端不认，导致用户批准后门禁仍然关闭。
        let is_plan_approval_kind = form_val
            .and_then(|f| f.get("kind"))
            .and_then(|k| k.as_str())
            .map(|k| {
                let normalized = k.trim().to_ascii_lowercase();
                normalized == "plan_approval" || normalized == "plan" || normalized == "approval"
            })
            .unwrap_or(false);

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

        // 方案审批挂起时，将阶段设为 AwaitingApproval，供前端及 Checkpoint 呈现等待审批状态
        if is_plan_approval_kind {
            checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::AwaitingApproval;
        }

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
        if is_plan_approval_kind {
            match parse_plan_approval_decision(&reply) {
                Some(false) => {
                    // 人类提出修改意见，打回重新推演审查
                    checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Reviewing;
                }
                Some(true) => {
                    // 人类在方案审批卡片中批准放行
                    checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
                    // 摘要中剥离结构化标记，避免协议噪声污染持久化内容
                    let clean_reply = strip_plan_decision_marker(&reply);
                    checkpoint.approved_plan_summary =
                        Some(clean_reply.chars().take(500).collect());
                }
                None => {
                    // “问一次就好，一次审批全程放权”
                    // 若用户已有非空回复且未包含打回否定词，直接放行写入，彻底避免连环死循环轰炸用户
                    let trimmed = reply.trim();
                    if !trimmed.is_empty()
                        && trimmed != "人类未回复，通道异常关闭"
                        && trimmed != "我再想想"
                    {
                        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
                        let clean_reply = strip_plan_decision_marker(&reply);
                        checkpoint.approved_plan_summary =
                            Some(clean_reply.chars().take(500).collect());
                        app.emit(
                            &format!("agent-log-{}", final_session_id),
                            "✅ 方案审批已放行（收到用户非反对回复，门禁解除）".to_string(),
                        )
                        .ok();
                    } else {
                        let msg = "【方案审批结论无法判定】未从回复中识别出明确的批准或打回意图，\
                                   前置写入门禁保持关闭。请重新呈报方案请用户明确确认。";
                        context.add_error_feedback(msg);
                        app.emit(
                            &format!("agent-log-{}", final_session_id),
                            msg.to_string(),
                        )
                        .ok();
                    }
                }
            }
        }
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
            Some(control) = rx.recv() => {
                let interrupt_reason = match control {
                    HumanControl::Cancel => "用户请求停止当前任务。".to_string(),
                    HumanControl::ReplaceGoal(goal) => format!("用户提交了新的任务目标：{}（已中断前序工具）", goal),
                    HumanControl::Answer(_) => "用户请求中断当前任务。".to_string(),
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
