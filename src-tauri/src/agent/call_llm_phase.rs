//! CallLlm 阶段执行器：模型规划、结构化决策确认与工具派发编排。
//! 指令补正耗尽时保留错误类型上传，工具纠错与传输恢复独立处理。

#[cfg(test)]
mod tests;

use crate::agent::checkpoint::{save_checkpoint, AgentCheckpoint};
use crate::agent::child_reviewer::result_as_dispatch_output;
use crate::agent::context::resource_scope::ResourceOperation;
use crate::agent::control::{take_pending_interrupt, HumanControl};
use crate::agent::dispatcher::{
    emit_tool_event, is_finish_instruction, is_write_or_side_effect_tool, run_agent_step,
};
use crate::agent::mcp::PluginRegistry;
use crate::agent::run_state::{AgentRunEvent, AgentRunMachine};
use crate::agent::types::{AgentInstruction, DispatchResult, StepExecutionError, RUN_INTERRUPTED_ROUTE};
use crate::agent::utils::call_llm;
use crate::db::DbState;
use crate::llm::ModelConfig;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, watch};
use crate::agent::loop_runner::{run_tool_phase, StepExecutionOutcome, REPEATED_FAILURE_BLOCKED_ROUTE};
use crate::agent::plan_gate::check_pre_write_gatekeeper;

/// run_tool 看门狗包装：与 call_llm 同款外层封顶。派发臂大多有内部自限时
/// （wait_jobs 默认 180s / exec 有 timeout_secs / 子任务 spawn 为同步建账），
/// 但挂死点也可能落在自限时罩不住的地方——实弹已观测到 child_reviewer
/// 派发在 spawn 内部永久冻结、checkpoint 停在 run_tool 相位、零子运行落账，
/// 只能靠 900s 会话级看门狗连整场一起收割。此处封顶把挂死收敛为
/// 「本步失败+带阶段面包屑的反馈」，循环继续而非冻死整场。
/// 阈值须高于 wait_jobs 合法挂起时长（默认 180s + 回执处理），env 可调。
fn tool_watchdog_secs() -> u64 {
    std::env::var("FREE_API_TOOL_WATCHDOG_SECS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|v| *v >= 60)
        .unwrap_or(480)
}

async fn run_tool_phase_guarded(
    app: &AppHandle,
    state: &State<'_, DbState>,
    registry_state: &State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    model_routing: &crate::agent::router::ModelRoutingConfig,
    run_id: &str,
    workspace_dir: &Path,
    final_session_id: &String,
    step_id: usize,
    inst: &AgentInstruction,
    context: &mut crate::agent::context::SandwichContext,
    checkpoint: &mut AgentCheckpoint,
    run_machine: &mut AgentRunMachine,
    rx: &mut mpsc::Receiver<HumanControl>,
    interrupt_rx: &mut watch::Receiver<u64>,
) -> Result<DispatchResult, String> {
    let secs = tool_watchdog_secs();
    let group = crate::agent::work_history::WorkGroup::capture(final_session_id, context, step_id);
    crate::agent::work_history::prepare_group(state.inner(), group.as_ref());
    match tokio::time::timeout(
        std::time::Duration::from_secs(secs),
        crate::agent::work_history::in_group(group, run_tool_phase(
            app,
            state,
            registry_state,
            model_routing,
            run_id,
            workspace_dir,
            final_session_id,
            step_id,
            inst,
            context,
            checkpoint,
            run_machine,
            rx,
            interrupt_rx,
        )),
    )
    .await
    {
        Err(_elapsed) => {
            let stage = crate::agent::control::read_stage(final_session_id)
                .unwrap_or_else(|| "unknown".to_string());
            let route = "tool_watchdog:timeout".to_string();
            // 挂死定位证据必须即时持久化——会话可能随后被 900s 停滞看门狗收割，
            // 只靠 stderr 反馈的话面包屑会随会话一起蒸发。
            crate::agent::telemetry::record_gate_fire(
                state.inner(),
                &crate::agent::telemetry::GateFireRecord {
                    run_id,
                    session_id: final_session_id,
                    step_id: step_id as i64,
                    gate: "tool_watchdog",
                    route: &format!("{}|stage={}", route, stage),
                },
            );
            Ok(DispatchResult {
                view: None,
                success: false,
                stdout: String::new(),
                stderr: format!(
                    "工具派发看门狗超时（{}s 未返回，stage={}）——该次派发可能内部挂死，已按本步失败处理避免会话冻结。若是异步子任务派发，请改用 wait_jobs 主动等待结论；若同一调用再次超时，请换用其他取证路径或 ask_human 报告卡住点。",
                    secs, stage
                ),
                route,
            })
        }
        Ok(r) => r,
    }
}

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
) -> Result<StepExecutionOutcome, StepExecutionError> {
    // 【shell 批改探针·前】步骤开始前快照 git 脏文件数；主循环在步骤结束后取差值。
    // 覆盖单发/批量派发/重试所有路径，防止 shell_exec 批量改文件绕过阶段审查。
    context.shell_probe_pre_dirty_paths =
        crate::agent::review_tools::git_dirty_file_paths(workspace_dir);
    context.shell_probe_pre_dirty = context
        .shell_probe_pre_dirty_paths
        .as_ref()
        .map(|p| p.len() as u32);
    context.shell_probe_pre_head = crate::agent::review_tools::git_head_sha(workspace_dir);
    context.shell_probe_pre_stash_count =
        crate::agent::review_tools::git_stash_count(workspace_dir);

    // 步骤间对账：本 session 写入的文件在上一步快照之间是否还在——
    // porcelain 差值只看步骤内，跨步骤被外部删/还原的文件只能靠「台账期望 vs
    // 文件系统实测」抓（别的 agent/用户手动动了我的工作区）。
    if !context.written_file_digests.is_empty() {
        let mut tampered: Vec<String> = Vec::new();
        let mut committed_out: Vec<String> = Vec::new();
        let mut external_modified: Vec<String> = Vec::new();
        for (rel, sha) in context.written_file_digests.iter() {
            let cur = crate::agent::review_tools::git_worktree_blob(workspace_dir, rel);
            match cur {
                // 文件消失：看 HEAD 是否已持有该内容指纹——是 = 被提交收走（合法，
                // 内容可经 git show 取回），否 = 被删除（未提交内容真实蒸发）
                None => {
                    if crate::agent::review_tools::git_head_blob(workspace_dir, rel)
                        .is_some_and(|h| &h == sha)
                    {
                        committed_out.push(rel.clone());
                    } else {
                        tampered.push(rel.clone());
                    }
                }
                Some(c) if &c == sha => {}
                // 内容变了：已是 HEAD 版本 = 被还原（未提交增量蒸发）；否则外部修改
                // （协作常态，仅换指纹基线不报警）
                Some(c) => {
                    if crate::agent::review_tools::git_head_blob(workspace_dir, rel)
                        .is_some_and(|h| h == c)
                    {
                        tampered.push(rel.clone());
                    } else {
                        external_modified.push(rel.clone());
                    }
                }
            }
        }
        for rel in committed_out {
            context.written_file_digests.remove(&rel);
        }
        for rel in external_modified {
            if let Some(c) = crate::agent::review_tools::git_worktree_blob(workspace_dir, &rel) {
                context.written_file_digests.insert(rel, c);
            }
        }
        if !tampered.is_empty() {
            let msg = format!(
                "⚠️ 本 session 写入的文件被外部销毁（删除或还原为 HEAD）：{}。\
                 若非本人操作请核对并发进程/其他 agent；内容无提交副本，已不可经 git 找回。",
                tampered.join(", ")
            );
            app.emit(&format!("agent-log-{}", final_session_id), msg.clone()).ok();
            context.git_dirty_lost.push(format!(
                "step{step_id} 步骤间对账: 本 session 写入文件被外部销毁: {}",
                tampered.join(", ")
            ));
        }
    }
    // 状态卡遥测的步号口径：与 run_trace 步骤 id / 取证台账 / 避坑台账同基准。
    context.current_step_id = step_id;
    let mut retry_count = 0;
    let max_retries = 3;
    let mut planning_retry_count = 0;
    const MAX_PLANNING_RETRIES: usize = 1;
    let mut last_token_usage: Option<crate::agent::types::TokenUsage> = None;
    let mut last_thinking_duration_ms: u64 = 0;
    let mut last_thinking_text = String::new();
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
        let dispatch_result = run_tool_phase_guarded(
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
                thinking_text: String::new(),
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

    // 附件观察先落检查点，再按实际发送投影选择规划模型。
    // 用户主动 view_image 仍走原有一次性队列，不在此处转述。
    if crate::agent::attachment_vision::has_unobserved_images(context) {
        let _ = app.emit(
            &format!("agent-log-{final_session_id}"),
            "正在观察新附件图片，保存结果后继续任务。",
        );
    }
    let observation_started = Instant::now();
    let image_directory =
        crate::agent::path_guard::get_session_tmp_dir(final_session_id).join("image-observations");
    match crate::agent::attachment_vision::prepare(
        state.inner(),
        context,
        model_routing,
        final_session_id,
        step_id,
        &image_directory,
        interrupt_rx,
    )
    .await
    {
        Ok(true) => {
            checkpoint.refresh_context(context);
            save_checkpoint(state.inner(), checkpoint)?;
            let note = if crate::agent::attachment_vision::history_requires_vision(context) {
                "附件图片观察未完成，已保留原图并继续原有视觉处理。"
            } else {
                "附件图片观察已保存，后续按任务阶段选择模型；原图可随时回看。"
            };
            let _ = app.emit(&format!("agent-log-{final_session_id}"), note);
        }
        Ok(false) => {}
        Err(error) if error == crate::llm::STREAM_INTERRUPTED_ERROR => {
            return Ok(StepExecutionOutcome {
                instruction: AgentInstruction::default(),
                result: DispatchResult {
                    view: None,
                    success: true,
                    stdout: take_pending_interrupt(rx)
                        .unwrap_or_else(|| "用户请求中断图片观察。".into()),
                    stderr: String::new(),
                    route: RUN_INTERRUPTED_ROUTE.into(),
                },
                token_usage: None,
                thinking_duration_ms: observation_started.elapsed().as_millis() as u64,
                tool_duration_ms: 0,
                thinking_text: String::new(),
            });
        }
        Err(error) => return Err(error.into()),
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
        // 统一由后置终态门禁 (finish_guard) 权威仲裁与自愈，严禁在流式前序 60 字符早退掐断大模型输出
        let can_early_challenge_finish = false;

        let planning_start = Instant::now();
        // 外层看门狗：单次 call_llm 内部含请求级 timeout 与瞬时重试，但挂死点
        // 也可能落在两者都罩不住的地方（通道漏唤醒、select 陷阱、上游 socket
        // 僵死）。不设上限的等待会让会话僵尸化越过一切内部时限——实弹已两次
        // 观测到。封顶取「单请求超时×(重试+1)+裕量」，超时即按普通调用失败
        // 返回错误，让主循环以失败步继续心跳而非永久冻结。
        let llm_watchdog_secs = {
            let base = 120u64.max(
                crate::llm::ModelConfig::from_db(&state, &routing.model_id)
                    .map(|c| c.timeout_secs)
                    .unwrap_or(120),
            );
            base.saturating_mul(transient_llm_request_retries as u64 + 1)
                .saturating_add(180)
        };
        let tier_name = routing.tier.to_string();
        // 规则文案事件化：首次激活/恢复缺事件/压缩重发在同一点补齐，
        // 补齐后尾部状态卡只投影动态状态行，规则文本进可缓存前缀。
        context.ensure_rule_events();
        let llm_call = call_llm(
            &context,
            &state,
            routing.model_id.clone(),
            &tier_name,
            Some(&app),
            step_id,
            &final_session_id,
            log_context.unwrap_or(false),
            transient_llm_request_retries,
            model_routing.reasoning_effort_for(&routing.tier),
            can_early_challenge_finish,
            interrupt_rx,
        );
        let llm_call_result = match tokio::time::timeout(
            std::time::Duration::from_secs(llm_watchdog_secs),
            llm_call,
        )
        .await
        {
            Err(_elapsed) => Err(format!(
                "LLM 调用看门狗超时（{}s 无任何产出）——请求可能僵死，已按失败处理避免会话冻结。",
                llm_watchdog_secs
            ).into()),
            Ok(r) => r,
        }
        .map(|r| {
            last_thinking_duration_ms = planning_start.elapsed().as_millis() as u64;
            last_token_usage = Some(r.1.clone());
            last_thinking_text = r.2.clone();
            r
        });
        // view_image 一次性视觉注入核销：pending_view_images 已随本次请求
        // 组装发出（成败同义——载荷已消费），此处清空。派发臂本次推入的新图
        // 在本行之后才进队列，不受影响，留给下一次调用。
        context.pending_view_images.clear();
        let (mut inst, token_usage, thinking_text, history_estimate_at_request) = match llm_call_result
        {
            Err(e) if e.message() == crate::llm::STREAM_INTERRUPTED_ERROR => {
                let reason = take_pending_interrupt(rx)
                    .unwrap_or_else(|| "用户请求中断当前模型输出。".to_string());
                return Ok(StepExecutionOutcome {
                    instruction: AgentInstruction::default(),
                    result: DispatchResult { view: None,
                        success: true,
                        stdout: reason,
                        stderr: String::new(),
                        route: RUN_INTERRUPTED_ROUTE.to_string(),
                    },
                    token_usage: None,
                    thinking_duration_ms: last_thinking_duration_ms,
                    tool_duration_ms: 0,
                    thinking_text: last_thinking_text.clone(),
                });
            }
            Err(e) => {
                // 指令补正已在 call_llm 内消耗完；保留类型传给主循环，
                // 本层及整阶段重试都不能重新开启同一生成预算。
                if e.instruction_recovery_exhausted() {
                    crate::agent::advance_run_state(
                        &app,
                        final_session_id,
                        &mut run_machine,
                        AgentRunEvent::PlanningFailed { reason: e.to_string() },
                    )?;
                    return Err(e);
                }
                let e = e.to_string();
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
                    return Err(format!("❌ LLM 请求失败 (已达重试上限): {}", e).into());
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
                            "🔄 未获得有效工具指令，正在按错误原因重新生成 ({}/{})...",
                            planning_retry_count, MAX_PLANNING_RETRIES
                        ),
                    );
                    context.add_error_feedback(
                        &crate::agent::utils::llm_client::instruction_recovery_feedback(&e),
                    );
                    checkpoint.record_recoverable_error(format!("规划响应异常: {}", e), &context);
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
                return Err(format!("❌ AI 规划失败: {}", e).into());
            }
            Ok(r) => r,
        };

        let model_tier = routing.tier.to_string();
        inst.description = inst.description.trim().to_string();
        if inst.description.is_empty() {
            inst.description = "执行下一步操作".to_string();
        }
        let (plan_id, plan_title) = context.resolve_plan(&inst);
        // 保存宿主已解析的身份，恢复时不重复消费 new_plan。
        inst.plan_id = Some(plan_id.clone());
        inst.plan_title = Some(plan_title.clone());
        inst.new_plan = Some(false);

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
            // 累计用量按「每次实际 LLM 调用」落账：含步骤内的规划重试，
            // 口径比主循环 outcome 定案处更全（那里只见得到最后一次调用）。
            context.run_prompt_tokens = context
                .run_prompt_tokens
                .saturating_add(token_usage.prompt_tokens.max(0) as u64);
            context.run_completion_tokens = context
                .run_completion_tokens
                .saturating_add(token_usage.completion_tokens.max(0) as u64);
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
                result: DispatchResult { view: None,
                    success: true,
                    stdout: reason,
                    stderr: String::new(),
                    route: RUN_INTERRUPTED_ROUTE.to_string(),
                },
                token_usage: last_token_usage,
                thinking_duration_ms: last_thinking_duration_ms,
                tool_duration_ms: 0,
                thinking_text: thinking_text.clone(),
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
                    "finding": inst.finding.clone(),
                    "finding_kind": inst.finding_kind.clone(),
                    "thinking": crate::agent::run_trace::clip_thinking_for_trace(&thinking_text),
                    "tool": inst.get_tool(),
                    "action": inst.get_action(),
                    "declared_action": inst.declared_action(),
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
        let dispatch_result = run_tool_phase_guarded(
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
                result: DispatchResult { view: None,
                    success: true,
                    stdout: reason,
                    stderr: String::new(),
                    route: RUN_INTERRUPTED_ROUTE.to_string(),
                },
                token_usage: last_token_usage,
                thinking_duration_ms: last_thinking_duration_ms,
                tool_duration_ms: last_tool_duration_ms,
                thinking_text: thinking_text.clone(),
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
            let mut error_detail = format!("【❌ 执行失败】: {}", dispatch_result.stderr);
            crate::agent::advance_run_state(
                &app,
                &final_session_id,
                &mut run_machine,
                AgentRunEvent::ToolFailed {
                    reason: error_detail.clone(),
                },
            )?;

            // Retry feedback stays bounded, but a partial failure must retain successful
            // siblings and raw MCP evidence before the next model request is made.
            let mut archival_result = dispatch_result.clone();
            let durable_output = crate::agent::dispatcher::take_tool_durable_output(&mut archival_result, &error_detail);
            let failed_output_id = crate::agent::run_trace::save_step_output_in_group(
                state.inner(),
                final_session_id,
                step_id,
                &inst.get_tool(),
                &inst.description,
                "failed",
                &durable_output,
                crate::agent::work_history::WorkGroup::capture(final_session_id, context, step_id).as_ref(),
            ).map_err(|e| eprintln!("无法保存失败尝试的原始输出: {e}")).ok();
            if let Some(id) = &failed_output_id {
                error_detail.push_str(&format!("\noutput_id: {id}\n完整执行回执已保存，可用 search_step_output 或 get_step_output 按需回查已执行的分支结果。"));
            }

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
                "finding": inst.finding.clone(),
                "finding_kind": inst.finding_kind.clone(),
                "thinking": crate::agent::run_trace::clip_thinking_for_trace(&thinking_text),
                "tool": inst.get_tool(),
                "action": inst.get_action(),
                "declared_action": inst.declared_action(),
                "command": inst.get_params(),
                "params": inst.get_params(),
                "plan_id": plan_id,
                "plan_title": plan_title,
                "status": "failed",
                "output": error_detail,
                "output_id": failed_output_id,
                "metrics": metrics_val.clone(),
                "duration_ms": total_dur,
                "timestamp": chrono::Utc::now().timestamp_millis(),
            }));
            // 行表同步落库：快照 upsert 只写 steps_json，缺了这行 agent_run_steps
            // 就没有失败步——重启/行表驱动的步骤条里拦截交锋整段蒸发，且后续
            // 步骤 seq 相对快照 id 偏移（实测偏移 2 导致 read 拦截步隐形）。
            if let Some(step_value) = run_trace_steps.last() {
                if let Err(e) = crate::agent::run_trace::append_step(
                    state.inner(),
                    run_id,
                    (run_trace_steps.len() - 1) as i64,
                    step_value,
                ) {
                    eprintln!("[run-trace] 无法持久化失败尝试步骤: {e}");
                }
            }
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
                    "output_id": failed_output_id,
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
        thinking_text: last_thinking_text,
    })
}

