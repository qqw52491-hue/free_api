//! 人工介入臂：ask_human 挂起、仲裁表单与人类回复裁决。

use crate::agent::checkpoint::{save_checkpoint, AgentCheckpoint};
use crate::agent::child_reviewer::result_as_dispatch_output;
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

use super::*;


pub(super) async fn dispatch_ask_human(
    ctx: &mut DispatchCtx<'_>,
    inst: &AgentInstruction,
) -> Result<ArmOutcome, String> {
    Ok(ArmOutcome::Done({
        let params = inst.get_params();
        let form_val = params.get("form");
        let ask_kind = form_val
            .and_then(|f| f.get("kind"))
            .or_else(|| params.get("kind"))
            .or_else(|| params.get("command").and_then(|c| c.get("kind")))
            .and_then(|k| k.as_str())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();

        // 与前端 normalizeHumanForm 的别名表保持一致（plan_approval / plan / approval），
        // 显式声明为审批/门禁放行类型的表单
        let is_plan_approval_kind =
            ask_kind == "plan_approval" || ask_kind == "plan" || ask_kind == "approval";

        // 【方向 3：冷却作用域收窄至 approval 类】
        // 门禁冷却（evaluate_ask_human_cooldown）核心目的在于防止模型刚被前置写入门禁拦截就立刻甩锅让人类审批放行。
        // - plan_approval / 门禁放行类：冷却生效（必须先按拦截意见行动，严禁立即甩锅绕门禁）。
        // - 缺参数、缺凭据、偏好或普通信息提问（非 approval 类，含过渡期 missing_parameter）：永远放行，不受冷却限制！
        if is_plan_approval_kind {
            if let Some(cooldown_msg) = evaluate_ask_human_cooldown(ctx.checkpoint, ctx.step_id) {
                crate::agent::dispatcher::emit_tool_event(
                    ctx.app,
                    ctx.final_session_id,
                    ctx.step_id,
                    inst,
                    "failed",
                    Some("ask_human_cooldown".to_string()),
                    Some(false),
                    &cooldown_msg,
                    Some(ctx.tool_started_at.elapsed().as_millis()),
                    None,
                );
                return Ok(ArmOutcome::Early(DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: cooldown_msg,
                    success: false,
                    route: "host_rejection:ask_human_cooldown".to_string(),
                }));
            }
        }

        let ask_msg = params
            .get("message")
            .or_else(|| form_val.and_then(|f| f.get("title")))
            .or_else(|| form_val.and_then(|f| f.get("description")))
            .and_then(|v| v.as_str())
            .unwrap_or("AI 请求人类协助与指示")
            .to_string();
        // 人工特赦带上「当时问了什么」：只记「确认执行」这类回复，审查员看不出批准的是什么。
        let waiver_question: String = form_val
            .and_then(|f| f.get("description"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(ask_msg.as_str())
            .chars()
            .take(500)
            .collect();

        // 【防问个没完：信息类提问全局配额兜底 (默认 5 次，可由配置调节；私密凭据类提问豁免)】
        // 提问信息类在同一任务里只允许问有限次数，超出直接拦截并强制采用默认值推进；且同一问题只允许问一次。
        if !is_plan_approval_kind {
            let is_credential = crate::agent::checkpoint::is_credential_inquiry_kind(&ask_kind);
            if !is_credential {
                let budget = crate::agent::channel::config::max_human_inquiry_budget();
                if let Some(quota_err) = ctx.checkpoint.check_inquiry_quota_exceeded(budget) {
                    crate::agent::dispatcher::emit_tool_event(
                        ctx.app,
                        ctx.final_session_id,
                        ctx.step_id,
                        inst,
                        "failed",
                        Some("ask_human_quota_exhausted".to_string()),
                        Some(false),
                        &quota_err,
                        Some(ctx.tool_started_at.elapsed().as_millis()),
                        None,
                    );
                    return Ok(ArmOutcome::Early(DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: quota_err,
                        success: false,
                        route: "host_rejection:ask_human_quota_exhausted".to_string(),
                    }));
                }
            }

            if let Some(dup_err) = ctx.checkpoint.check_duplicate_question(&ask_msg) {
                crate::agent::dispatcher::emit_tool_event(
                    ctx.app,
                    ctx.final_session_id,
                    ctx.step_id,
                    inst,
                    "failed",
                    Some("ask_human_duplicate".to_string()),
                    Some(false),
                    &dup_err,
                    Some(ctx.tool_started_at.elapsed().as_millis()),
                    None,
                );
                return Ok(ArmOutcome::Early(DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: dup_err,
                    success: false,
                    route: "host_rejection:ask_human_duplicate".to_string(),
                }));
            }
            ctx.checkpoint.record_asked_question(&ask_msg);
        }

        // 人工仲裁与放行上下文识别：
        // 明确带有 plan_approval/plan/approval kind，或门禁阻断/审查耗尽显式指示仲裁。
        let is_approval_context = is_plan_approval_kind
            || ctx.checkpoint.gate_human_escape_directed
            || ctx.checkpoint.stage_review_blockers_open > 0
            || ctx.checkpoint.last_review_blocking_count > 0
            || (form_val.is_none()
                && ctx.checkpoint.plan_reviewer_retry_count
                    >= crate::agent::checkpoint::max_plan_reviewer_rounds());

        ctx.app.emit(
            &format!("agent-log-{}", ctx.final_session_id),
            format!("⏸️ AI 已挂起，等待人类协助: {}", ask_msg),
        )
        .ok();

        // 若处于仲裁/审批上下文但模型未提供 form，自动补齐 plan_approval 结构化表单，
        // 使得前端直接渲染【批准方案】【打回修改】等可点击卡片，提升人机协作效率。
        let mut final_form_val: Option<serde_json::Value> = if form_val.is_some() {
            form_val.cloned()
        } else if is_approval_context {
            Some(json!({
                "kind": "plan_approval",
                "title": "方案与安全审查人工仲裁",
                "description": ask_msg
            }))
        } else {
            None
        };

        // 模型把方案正文写进 message、form.description 留空是高频格式摩擦（白烧一步）：
        // message 足够长时直接搬进 description 展示，不必打回。
        if is_approval_context {
            if let Some(obj) = final_form_val.as_mut().and_then(|f| f.as_object_mut()) {
                let cur = obj
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .chars()
                    .count();
                if cur < 30 && ask_msg.trim().chars().count() >= 30 {
                    obj.insert("description".to_string(), json!(ask_msg.trim()));
                }
            }
        }

        // 【审批内容必填】plan_approval 卡片的可审阅区域渲染的是 form.description；
        // 模型只给标题不带正文时，人类面对的就是一张"裸审批卡"——无法判断在批什么。
        // 机械校验（非语义）：最终将要展示的正文必须达到最低信息量（≥30 字符，
        // 约一句完整方案的长度），不足则拒收并要求补齐方案正文后重发——
        // 不消耗挂起/人工通道。
        if is_approval_context {
            let effective_desc = final_form_val
                .as_ref()
                .and_then(|f| f.get("description"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            const MIN_APPROVAL_DESC_CHARS: usize = 30;
            if effective_desc.chars().count() < MIN_APPROVAL_DESC_CHARS {
                let reject_msg = format!(
                    "【方案审批内容缺失】kind=plan_approval 的审批卡片必须携带可审阅的方案正文，\
                     当前 description 仅 {} 字符（要求 ≥ {}）。\
                     请把完整方案（目标 / 改动范围 target_files / 根因与证据 / 验证方式）写入 \
                     form.description 字段（未提供 form 时写入 message 字段）后重新调用 ask_human；\
                     人类无法批准一份看不见内容的方案。",
                    effective_desc.chars().count(),
                    MIN_APPROVAL_DESC_CHARS
                );
                crate::agent::dispatcher::emit_tool_event(
                    ctx.app,
                    ctx.final_session_id,
                    ctx.step_id,
                    inst,
                    "failed",
                    Some("plan_approval_content_missing".to_string()),
                    Some(false),
                    &reject_msg,
                    Some(ctx.tool_started_at.elapsed().as_millis()),
                    None,
                );
                return Ok(ArmOutcome::Early(DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: reject_msg,
                    success: false,
                    route: "host_rejection:plan_approval_content_missing".to_string(),
                }));
            }
        }

        // 通知前端进入暂停状态
        ctx.app.emit(
            &format!("agent-progress-{}", ctx.final_session_id),
            json!({
                "type": "agent_paused",
                "message": ask_msg,
                "form": final_form_val.unwrap_or(serde_json::Value::Null)
            }),
        )
        .ok();

        println!("⏸️ [阻塞点] 模型触发 ask_human 挂起，正在死等前端人类唤醒...");

        // 方案审批与仲裁挂起时，将阶段设为 AwaitingApproval，供前端及 Checkpoint 呈现等待审批状态
        if is_approval_context {
            ctx.checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::AwaitingApproval;
        }

        // 真正的阻塞点：死等人类回复！
        // ctx.rx 被我们在外层定义过了，这里可以直接 `.recv().await`
        // git 破坏命令确认链路：若问题原文引用了挂起的命令指纹，标记为
        // 「本次所问即该命令」——回复回来后按指纹核销（语义上确认与否归人类，
        // 机械上只认命令原文在场）。
        ctx.checkpoint.git_destructive_mark_asked(&ask_msg);
        ctx.checkpoint.prepared_instruction = Some(inst.clone());
        ctx.checkpoint.wait_for_human(ask_msg.clone(), ctx.context);
        save_checkpoint(ctx.state.inner(), ctx.checkpoint)?;
        crate::agent::advance_run_state(
            ctx.app,
            ctx.final_session_id,
            ctx.run_machine,
            AgentRunEvent::WaitForHuman {
                question: ask_msg.clone(),
            },
        )?;
        crate::agent::control::mark_stage(ctx.final_session_id, "ask_human:waiting_reply");
        let reply = match ctx.rx.recv().await {
            Some(HumanControl::Answer(reply)) => reply,
            Some(HumanControl::ReplaceGoal(goal)) => {
                return Ok(ArmOutcome::Early(DispatchResult { view: None,
                    success: true,
                    stdout: format!("用户提交了新的任务目标：{}", goal),
                    stderr: String::new(),
                    route: RUN_INTERRUPTED_ROUTE.to_string(),
                }))
            }
            Some(HumanControl::Cancel) => {
                return Ok(ArmOutcome::Early(DispatchResult { view: None,
                    success: true,
                    stdout: "用户请求停止当前任务。".to_string(),
                    stderr: String::new(),
                    route: RUN_INTERRUPTED_ROUTE.to_string(),
                }))
            }
            None => "人类未回复，通道异常关闭".to_string(),
        };
        // git 破坏命令核销：非拒绝回复 → asked 指纹确认为已放行，重发即通过
        let git_confirmed = ctx.checkpoint.git_destructive_resolve_reply(&reply);
        if git_confirmed > 0 {
            ctx.app.emit(
                &format!("agent-log-{}", ctx.final_session_id),
                format!("✅ 主人已确认 {} 条 git 破坏命令，原样重发即可放行", git_confirmed),
            )
            .ok();
        }
        ctx.context.add_error_feedback(&format!("【人类回复】{}", reply));
        if is_approval_context {
            match parse_plan_approval_decision(&reply) {
                Some(false) => {
                    // 人类提出修改意见，打回重新推演审查
                    ctx.checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Reviewing;
                }
                Some(true) => {
                    // 人类在方案审批卡片中批准放行
                    ctx.checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
                    // 未经独立审查披露：没有任何审查结论交付即获人工放行时打标——
                    // 交付与报告必须如实声明，不许静默。判定用 deliveries 而非
                    // consumed：全失败批次也计「齐套」，consumed 会漏标（实测）。
                    ctx.checkpoint.plan_unreviewed_release =
                        ctx.checkpoint.plan_review_deliveries.is_empty();
                    if ctx.checkpoint.plan_unreviewed_release {
                        ctx.app.emit(
                            &format!("agent-log-{}", ctx.final_session_id),
                            "⚠️ 方案经人工放行，但未收到任何独立审查结论（审查未派发或全部失败）".to_string(),
                        )
                        .ok();
                    }
                    ctx.checkpoint.gate_human_escape_directed = false;
                    ctx.checkpoint.recent_gate_block_step = None;
                    if ctx.checkpoint.stage_review_blockers_open > 0 {
                        ctx.checkpoint.stage_review_blockers_open = 0;
                        ctx.checkpoint.stage_review_blocker_stage = None;
                    }
                    if ctx.checkpoint.last_review_blocking_count > 0 {
                        ctx.checkpoint.last_review_blocking_count = 0;
                    }
                    // 摘要中剥离结构化标记，避免协议噪声污染持久化内容
                    let clean_reply = strip_plan_decision_marker(&reply);
                    ctx.checkpoint.approved_plan_summary =
                        Some(clean_reply.chars().take(500).collect());
                    ctx.checkpoint.record_human_waiver(&format!("问: {} → 答: {}", waiver_question, clean_reply));
                    ctx.checkpoint.last_reviewed_diff_hash = None;
                    ctx.checkpoint.last_review_result = None;
                    ctx.checkpoint.reviewer_retry_count = 0;
                    ctx.app.emit(
                        &format!("agent-log-{}", ctx.final_session_id),
                        "✅ 方案审批与人工仲裁已放行（收到用户批准，写入门禁解除）".to_string(),
                    )
                    .ok();
                }
                None => {
                    // “问一次就好，一次审批全程放权”
                    // 若用户已有非空回复且未包含打回否定词，直接放行写入，彻底避免连环死循环轰炸用户
                    let trimmed = reply.trim();
                    if !trimmed.is_empty()
                        && trimmed != "人类未回复，通道异常关闭"
                        && trimmed != "我再想想"
                    {
                        ctx.checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
                        ctx.checkpoint.plan_unreviewed_release =
                            ctx.checkpoint.plan_review_deliveries.is_empty();
                        if ctx.checkpoint.plan_unreviewed_release {
                            ctx.app.emit(
                                &format!("agent-log-{}", ctx.final_session_id),
                                "⚠️ 方案经人工放行，但未收到任何独立审查结论（审查未派发或全部失败）".to_string(),
                            )
                            .ok();
                        }
                        ctx.checkpoint.gate_human_escape_directed = false;
                        ctx.checkpoint.recent_gate_block_step = None;
                        if ctx.checkpoint.stage_review_blockers_open > 0 {
                            ctx.checkpoint.stage_review_blockers_open = 0;
                            ctx.checkpoint.stage_review_blocker_stage = None;
                        }
                        if ctx.checkpoint.last_review_blocking_count > 0 {
                            ctx.checkpoint.last_review_blocking_count = 0;
                        }
                        let clean_reply = strip_plan_decision_marker(&reply);
                        ctx.checkpoint.approved_plan_summary =
                            Some(clean_reply.chars().take(500).collect());
                        ctx.checkpoint.record_human_waiver(&format!("问: {} → 答: {}", waiver_question, clean_reply));
                        ctx.checkpoint.last_reviewed_diff_hash = None;
                        ctx.checkpoint.last_review_result = None;
                        ctx.checkpoint.reviewer_retry_count = 0;
                        ctx.app.emit(
                            &format!("agent-log-{}", ctx.final_session_id),
                            "✅ 方案审批与人工仲裁已放行（收到用户非反对回复，门禁解除）".to_string(),
                        )
                        .ok();
                    } else {
                        let msg = "【方案审批结论无法判定】未从回复中识别出明确的批准或打回意图，\
                                   前置写入门禁保持关闭。请重新呈报方案请用户明确确认。";
                        ctx.context.add_error_feedback(msg);
                        ctx.app.emit(
                            &format!("agent-log-{}", ctx.final_session_id),
                            msg.to_string(),
                        )
                        .ok();
                    }
                }
            }
        } else {
            let clean_reply = strip_plan_decision_marker(&reply);
            let reply_lower = clean_reply.to_ascii_lowercase();
            let has_explicit_waiver_intent = reply_lower.contains("force_pass")
                || reply_lower.contains("放行")
                || reply_lower.contains("特赦")
                || reply_lower.contains("豁免")
                || reply_lower.contains("确认执行")
                || (reply_lower.contains("保留") && reply_lower.contains("调试"));
            if has_explicit_waiver_intent {
                ctx.checkpoint.record_human_waiver(&format!("问: {} → 答: {}", waiver_question, clean_reply));
                ctx.checkpoint.last_reviewed_diff_hash = None;
                ctx.checkpoint.last_review_result = None;
                ctx.checkpoint.reviewer_retry_count = 0;
                if ctx.checkpoint.stage_review_blockers_open > 0 {
                    ctx.checkpoint.stage_review_blockers_open = 0;
                    ctx.checkpoint.stage_review_blocker_stage = None;
                }
                if ctx.checkpoint.last_review_blocking_count > 0 {
                    ctx.checkpoint.last_review_blocking_count = 0;
                }
            }
        }
        ctx.checkpoint.begin_planning(ctx.step_id, ctx.context);
        save_checkpoint(ctx.state.inner(), ctx.checkpoint)?;
        crate::agent::advance_run_state(
            ctx.app,
            ctx.final_session_id,
            ctx.run_machine,
            AgentRunEvent::HumanReplyReceived,
        )?;

        println!("▶️ [唤醒] 人类回复: {}", reply);

        ctx.app.emit(
            &format!("agent-log-{}", ctx.final_session_id),
            format!("▶️ AI 已被唤醒，人类回复: {}", reply),
        )
        .ok();

        // 伪造一个成功的执行结果，把人类的话当作工具输出返回给大模型
        crate::agent::types::DispatchResult { view: None,
            success: true,
            stdout: format!("人类的回复是: {}", reply),
            stderr: String::new(),
            route: "ask_human_resolved".to_string(),
        }
    }))
}
