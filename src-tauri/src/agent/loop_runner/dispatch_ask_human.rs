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

        if is_plan_approval_kind {
            if let Some(notice) = ask_human_recent_gate_notice(ctx.checkpoint, ctx.step_id) {
                ctx.context.add_host_feedback(&notice);
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

        // 历史提问次数和文本相似度只作建议，不推断此前已有答案或本次不应询问。
        if !is_plan_approval_kind {
            if !crate::agent::checkpoint::is_credential_inquiry_kind(&ask_kind) {
                let budget = crate::agent::channel::config::max_human_inquiry_budget();
                if let Some(notice) = ctx.checkpoint.inquiry_budget_notice(budget) {
                    ctx.context.add_host_feedback(&notice);
                }
            }
            if let Some(notice) = ctx.checkpoint.similar_question_notice(&ask_msg) {
                ctx.context.add_host_feedback(&notice);
            }
            ctx.checkpoint.record_asked_question(&ask_msg);
        }

        // 人工仲裁与放行上下文识别：
        // 明确带有 plan_approval/plan/approval kind，或门禁阻断/审查耗尽显式指示仲裁。
        let asks_git_operation = ctx.checkpoint.git_destructive_pending.iter().any(|command| ask_msg.contains(command));
        let is_approval_context = is_plan_approval_kind || (!asks_git_operation && ( ctx.checkpoint.gate_human_escape_directed
            || ctx.checkpoint.stage_review_blockers_open > 0
            || ctx.checkpoint.last_review_blocking_count > 0
            || (form_val.is_none()
                && ctx.checkpoint.plan_reviewer_retry_count
                    >= crate::agent::checkpoint::max_plan_reviewer_rounds())));

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
                "description": params.get("message").and_then(Value::as_str).unwrap_or("")
            }))
        } else {
            None
        };

        if is_approval_context {
            if let Err(reject_msg) = normalize_approval_description(
                &mut final_form_val, params.get("message").and_then(Value::as_str),
            ) {
                emit_tool_event(ctx.app, ctx.final_session_id, ctx.step_id, inst, "failed",
                    Some("plan_approval_content_missing".into()), Some(false), &reject_msg,
                    Some(ctx.tool_started_at.elapsed().as_millis()), None);
                return Ok(ArmOutcome::Early(DispatchResult { view: None, stdout: String::new(),
                    stderr: reject_msg, success: false,
                    route: "host_rejection:plan_approval_content_missing".into() }));
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
        let (reply, received_human_reply) = match ctx.rx.recv().await {
            Some(HumanControl::Answer(reply, cursor)) => {
                ctx.context.acknowledge_chat(cursor);
                (reply, true)
            },
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
            None => ("人类未回复，通道异常关闭".to_string(), false),
        };
        ctx.context.add_error_feedback(&format!("【人类回复】{}", reply));
        // 按钮是明确结构化操作；自由文本留给主模型正常下一轮理解。
        let explicit_plan = (received_human_reply && is_approval_context).then(|| parse_plan_approval_decision(&reply)).flatten();
        if let Some(approved) = explicit_plan {
            let clean_reply = strip_plan_decision_marker(&reply);
            ctx.checkpoint.apply_explicit_plan_decision(ctx.state.inner(), ctx.context, approved, &waiver_question, &clean_reply);
        }
        if received_human_reply {
            ctx.checkpoint.register_human_reply(&ask_msg, &reply, is_approval_context && explicit_plan.is_none());
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

/// 只保证审批正文可展示，不根据长度推断内容是否足够；标题不冒充方案正文。
fn normalize_approval_description(form: &mut Option<Value>, message: Option<&str>) -> Result<(), String> {
    let obj = form.as_mut().and_then(Value::as_object_mut)
        .ok_or("审批表单应为对象，本次尚未请求人工审批。")?;
    if obj.get("description").and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty()) {
        return Ok(());
    }
    if let Some(text) = message.map(str::trim).filter(|s| !s.is_empty()) {
        obj.insert("description".into(), json!(text));
        return Ok(());
    }
    Err("【方案审批内容缺失】审批卡片没有可展示的正文；请在 form.description 或 message 写明请求批准的操作。本次尚未请求人工审批。".into())
}

#[cfg(test)]
mod approval_content_tests {
    use super::*;
    #[test]
    fn short_real_body_is_displayed_but_title_and_missing_body_are_not_authorization() {
        let mut form = Some(json!({"kind":"plan_approval","title":"批准方案"}));
        assert!(normalize_approval_description(&mut form, None).is_err());
        assert!(normalize_approval_description(&mut form, Some("  ")).is_err());
        normalize_approval_description(&mut form, Some("修空值判断")).unwrap();
        assert_eq!(form.as_ref().unwrap()["description"], "修空值判断");
        normalize_approval_description(&mut form, Some("另一个请求")).unwrap();
        assert_eq!(form.as_ref().unwrap()["description"], "修空值判断", "已有正文不被覆盖");
        let mut invalid = Some(json!([]));
        assert!(normalize_approval_description(&mut invalid, Some("正文")).is_err());
    }
}
