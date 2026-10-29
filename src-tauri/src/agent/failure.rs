//! 失败停止与成功交付是不同终态：未完成的账本原样保留，不借 finish 核销。
use crate::agent::{
    checkpoint::AgentCheckpoint,
    context::SandwichContext,
    run_state::{AgentRunEvent, AgentRunMachine},
    types::{DispatchResult, FailTaskRequest},
};
use crate::db::DbState;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

pub(crate) fn failure_action_result(request: &FailTaskRequest) -> DispatchResult {
    let reason = request.reason.trim();
    DispatchResult {
        view: None,
        success: false,
        stdout: String::new(),
        stderr: if reason.is_empty() {
            "fail_task.reason 必须是非空字符串；本次未停止任务、未执行附带工具。".into()
        } else {
            reason.into()
        },
        route: if reason.is_empty() {
            "input_validation:fail_task".into()
        } else {
            "task:failed".into()
        },
    }
}

/// 纯落账层便于回归：不调用任何成功验收/义务核销函数。
pub(crate) fn persist_failed_run(
    db: &DbState,
    session_id: &str,
    run_id: &str,
    goal: &str,
    steps: &[Value],
    checkpoint: &mut AgentCheckpoint,
    context: &SandwichContext,
    reason: &str,
) -> Result<String, String> {
    let message = format!("任务未完成，已停止。\n原因：{}\n已有改动、证据和未完成事项已保留；处理原因后可续跑，审查要求不会自动解除。", reason);
    let mut failed = checkpoint.clone();
    failed.pending_task_failure = None;
    failed.pending_finalization = None;
    failed.mark_failed(reason.to_string(), context);
    crate::agent::finalization::commit_failure(
        db,
        &mut failed,
        run_id,
        session_id,
        goal,
        steps,
        &json!({"summary": message, "reason": reason, "status": "failed"}),
        &message,
    )?;
    *checkpoint = failed;
    Ok(message)
}

/// 先中断真实请求，再收停台账/后台进程/监听；所有失败来源共用。
pub(crate) async fn stop_run_runtime(db: &DbState, session_id: &str, run_id: &str) -> Vec<String> {
    crate::agent::control::notify_stream_interrupt(session_id).await;
    let mut warnings = Vec::new();
    if let Err(e) = crate::agent::child_reviewer::cancel_running_child_runs(db, run_id) {
        warnings.push(format!("子任务收停失败：{e}"));
    }
    if let Err(e) = crate::agent::job_engine::cancel_all_running_jobs(db, run_id) {
        warnings.push(format!("后台任务收停失败：{e}"));
    }
    crate::agent::control::intervention_channels()
        .lock()
        .await
        .remove(session_id);
    crate::agent::control::stream_interrupt_channels()
        .lock()
        .await
        .remove(session_id);
    crate::agent::control::clear_stage(session_id);
    warnings
}

pub(crate) async fn finish_failed_run<R: tauri::Runtime>(
    app: &AppHandle<R>,
    db: &DbState,
    session_id: &str,
    run_id: &str,
    goal: &str,
    steps: &[Value],
    checkpoint: &mut AgentCheckpoint,
    context: &SandwichContext,
    machine: &mut AgentRunMachine,
    reason: String,
) -> Result<(), String> {
    let persisted = persist_failed_run(
        db, session_id, run_id, goal, steps, checkpoint, context, &reason,
    );
    // 即使数据库故障也必须收停，不把失败变成悬挂运行。
    let warnings = stop_run_runtime(db, session_id, run_id).await;
    let mut message = match &persisted {
        Ok(message) => message.clone(),
        Err(e) => format!(
            "任务未完成，已停止。\n原因：{reason}\n失败记录保存失败：{e}；请勿把这次当作成功交付。"
        ),
    };
    for warning in warnings {
        message.push_str(&format!("\n⚠️ {warning}"));
    }
    // 兼容旧 UI：complete 先填失败报告，权威终态随后广播（终态后 UI 忽略迟到进度）。
    let _ = app.emit(
        &format!("agent-progress-{session_id}"),
        json!({"type":"complete", "message":message, "success":false}),
    );
    let snapshot = machine
        .transition(AgentRunEvent::Fail {
            reason: message.clone(),
        })
        .map_err(|e| e.to_string())?;
    let _ = app.emit(&format!("agent-run-state-{session_id}"), snapshot);
    crate::agent::channel::notify_session_event(session_id, "message");
    persisted.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{
        checkpoint::{AgentPhase, PlanPhase},
        types::{AgentInstruction, TodoItem},
    };
    use std::sync::{Arc, Mutex};

    fn fixture() -> (
        std::path::PathBuf,
        DbState,
        SandwichContext,
        AgentCheckpoint,
    ) {
        let dir = std::env::temp_dir().join(format!("failure-exit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = crate::db::init_db(&dir.join("test.db")).unwrap();
        conn.execute("INSERT INTO chat_sessions (id,title,model_id,created_at,updated_at) VALUES ('s1','test','','2026-01-01','2026-01-01')", []).unwrap();
        let db = DbState(Arc::new(Mutex::new(conn)));
        let mut context = SandwichContext::new("sys".into(), "original goal".into());
        context.todo_list.push(TodoItem {
            id: 1,
            status: "pending".into(),
            description: "未完成验证".into(),
            evidence_output_id: None,
        });
        context.review_contract = Some(
            serde_json::from_value(json!({"scope_statement":"existing scope", "obligations":[{
                "id":"verify_build", "question":"构建通过", "status":"open", "evidence_refs":[],
                "verify_command":"cargo check", "closure_form":"command"
            }]}))
            .unwrap(),
        );
        let checkpoint = AgentCheckpoint::new("s1".into(), &context);
        (dir, db, context, checkpoint)
    }

    #[test]
    fn fail_task_is_explicit_and_empty_reason_is_only_a_parameter_error() {
        assert_eq!(
            failure_action_result(&FailTaskRequest::default()).route,
            "input_validation:fail_task"
        );
        assert_eq!(
            failure_action_result(&FailTaskRequest {
                reason: " 401 Unauthorized ".into()
            })
            .route,
            "task:failed"
        );
        let normal: AgentInstruction = serde_json::from_value(
            json!({"tool":"local_file", "description":"无法完成", "command":{"action":"read"}}),
        )
        .unwrap();
        assert!(normal.fail_task.is_none(), "不通过关键词猜测失败退出");
        let alias: AgentInstruction = serde_json::from_value(
            json!({"tool":"fail_task", "params":{"reason":"401 Unauthorized"}}),
        )
        .unwrap();
        let normalized = alias.unwrap_standalone_tool_alias().unwrap();
        assert_eq!(normalized.fail_task.unwrap().reason, "401 Unauthorized");
    }

    #[test]
    fn failure_mixed_with_write_drops_the_write_entry() {
        let mut inst: AgentInstruction = serde_json::from_value(json!({
            "fail_task":{"reason":"审查通道失效"}, "tool":"local_shell", "command":{"command":"touch should-not-exist"}
        })).unwrap();
        assert!(inst.strip_entry_when_status_action());
        assert!(!inst.has_executable_entry());
        assert!(inst.command.is_none());
        assert!(inst.validate_executable_shape().is_ok());
    }

    #[test]
    fn failure_commits_with_open_obligations_and_explicit_resume_preserves_them() {
        let (dir, db, mut context, mut cp) = fixture();
        let original_contract = context.review_contract.clone();
        let original_phase = cp.plan_phase;
        cp.stage_review_blockers_open = 2;
        let modified = dir.join("modified.rs");
        std::fs::write(&modified, "existing user-approved changes").unwrap();
        let message = persist_failed_run(
            &db,
            "s1",
            "run-1",
            "original goal",
            &[json!({"id":42})],
            &mut cp,
            &context,
            "401 Unauthorized",
        )
        .unwrap();
        assert!(message.contains("未完成"));
        assert_eq!(cp.phase, AgentPhase::Failed);
        assert_eq!(cp.plan_phase, original_phase);
        assert_eq!(
            std::fs::read_to_string(&modified).unwrap(),
            "existing user-approved changes"
        );
        {
            let conn = db.0.lock().unwrap();
            let (status, completion): (String, String) = conn
                .query_row(
                    "SELECT status,completion_json FROM agent_run_traces WHERE id='run-1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(status, "failed");
            assert_eq!(
                serde_json::from_str::<Value>(&completion).unwrap()["reason"],
                "401 Unauthorized"
            );
            let count: i64 = conn
                .query_row(
                    "SELECT message_count FROM chat_sessions WHERE id='s1'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1);
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM agent_checkpoint_history WHERE session_id='s1'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1);
            let saved: String = conn
                .query_row(
                    "SELECT snapshot_json FROM agent_checkpoints WHERE run_id='s1'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            cp = serde_json::from_str(&saved).unwrap();
        }
        cp.restore_context(&mut context);
        assert_eq!(context.review_contract, original_contract);
        assert_eq!(context.todo_list[0].status, "pending");
        cp.resume_failed_task(&mut context, "凭据已修复，继续原任务");
        assert_ne!(cp.phase, AgentPhase::Failed);
        assert_ne!(cp.plan_phase, PlanPhase::Approved);
        assert_eq!(cp.stage_review_blockers_open, 2);
        assert_eq!(context.review_contract, original_contract);
        assert_eq!(context.todo_list[0].status, "pending");
        assert!(cp.pending_task_failure.is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn database_write_error_rolls_back_entire_failure_and_in_memory_version() {
        let (dir, db, context, mut cp) = fixture();
        let before = serde_json::to_value(&cp).unwrap();
        db.0.lock().unwrap().execute_batch("CREATE TRIGGER reject_failure BEFORE INSERT ON chat_messages BEGIN SELECT RAISE(ABORT,'injected storage failure'); END;").unwrap();
        assert!(persist_failed_run(
            &db,
            "s1",
            "run-1",
            "goal",
            &[],
            &mut cp,
            &context,
            "request failed"
        )
        .is_err());
        assert_eq!(serde_json::to_value(&cp).unwrap(), before);
        let conn = db.0.lock().unwrap();
        for table in [
            "agent_checkpoints",
            "agent_run_traces",
            "chat_messages",
            "agent_checkpoint_history",
        ] {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0, "{table} must roll back");
        }
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn runtime_stop_interrupts_requests_cancels_only_own_jobs_and_rejects_late_completion() {
        let (dir, db, _, _) = fixture();
        let sid = format!("cleanup-{}", uuid::Uuid::new_v4());
        let (tx, mut rx) = tokio::sync::watch::channel(0u64);
        crate::agent::control::stream_interrupt_channels()
            .lock()
            .await
            .insert(sid.clone(), tx);
        let (tx, _rx) = tokio::sync::mpsc::channel(2);
        crate::agent::control::intervention_channels()
            .lock()
            .await
            .insert(sid.clone(), tx);
        let job = crate::agent::job_engine::begin_job(&db, "run-1", &sid, "watch", "own job", "{}")
            .unwrap();
        let other = crate::agent::job_engine::begin_job(
            &db,
            "run-other",
            "other",
            "watch",
            "other job",
            "{}",
        )
        .unwrap();
        let child = crate::agent::child_reviewer::begin_child_run(
            &db, "run-1", &sid, &dir, "m1", 0, "review", None,
        )
        .unwrap();
        assert!(stop_run_runtime(&db, &sid, "run-1").await.is_empty());
        assert_eq!(*rx.borrow_and_update(), 1);
        assert!(!crate::agent::control::stream_interrupt_channels()
            .lock()
            .await
            .contains_key(&sid));
        assert!(!crate::agent::control::intervention_channels()
            .lock()
            .await
            .contains_key(&sid));
        crate::agent::job_engine::finish_job(&db, &job, "succeeded", 20, Some("late result"), None)
            .unwrap();
        let conn = db.0.lock().unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM agent_background_jobs WHERE id=?1",
                [job],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "cancelled");
        let status: String = conn
            .query_row(
                "SELECT status FROM agent_background_jobs WHERE id=?1",
                [other],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "running");
        let status: String = conn
            .query_row(
                "SELECT status FROM agent_child_runs WHERE id=?1",
                [child],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "cancelled");
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn runtime_shell_cancellation_kills_the_registered_process_group() {
        use std::os::unix::process::CommandExt;
        let (dir, db, _, _) = fixture();
        // Only this test's isolated process group is touched; never the application.
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 60"])
            .process_group(0)
            .spawn()
            .unwrap();
        crate::agent::job_engine::begin_job(
            &db,
            "run-shell",
            "s1",
            "shell",
            "sleep",
            &json!({"pid":child.id()}).to_string(),
        )
        .unwrap();
        crate::agent::job_engine::cancel_all_running_jobs(&db, "run-shell").unwrap();
        let mut stopped = false;
        for _ in 0..50 {
            if child.try_wait().unwrap().is_some() {
                stopped = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !stopped {
            let _ = child.kill();
        }
        let _ = child.wait();
        assert!(stopped, "must stop the actual process, not just its DB row");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stage_dispatch_snapshot_round_trip_keeps_own_question_and_review_contract() {
        use crate::agent::{
            child_reviewer::PlanReviewerDispatchSnapshot,
            protocol::{
                ContractFieldDef, ContractFieldType, ContractSchema, DispatchEnvelope, FocusDomain,
            },
        };
        let mut envelope = DispatchEnvelope::reviewer("run-1", "s1", "primary", json!({"question":"阶段审查原题", "evidence_excerpt":"真实证据", "custom_inputs":{"plan_summary":"原方案"}}))
            .with_purpose("stage_review").with_batch_id("batch-stage")
            .with_persona("original reviewer persona").with_tier("flash").with_effort("high")
            .with_custom_schema(ContractSchema::new(vec![ContractFieldDef::must_new("cve_refs",ContractFieldType::StringArray,"CVE 编号",true)]));
        envelope.focus_domains = vec![FocusDomain::GitDiff, FocusDomain::LastVerifyLog];
        envelope.specialized_context = Some("原角色领域要求".into());
        let snapshot = PlanReviewerDispatchSnapshot::from_envelope(&envelope);
        let reloaded =
            PlanReviewerDispatchSnapshot::from_json_str(&snapshot.to_json_string()).unwrap();
        assert_eq!(snapshot, reloaded);
        assert_eq!(reloaded.params, envelope.input_payload);
        assert_eq!(reloaded.custom_schema, envelope.custom_schema);
        assert_eq!(reloaded.purpose.as_deref(), Some("stage_review"));
        assert_eq!(reloaded.batch_id.as_deref(), Some("batch-stage"));
        assert_eq!(
            reloaded.focus_domains,
            vec![FocusDomain::GitDiff, FocusDomain::LastVerifyLog]
        );
        assert_eq!(
            reloaded.specialized_context.as_deref(),
            Some("原角色领域要求")
        );
    }

    #[test]
    fn failed_terminal_review_is_not_a_pass_but_semantic_rejection_remains_repairable() {
        use crate::agent::reviewer_gatekeeper::ReviewResult;
        let failed = ReviewResult::failed_open("402 insufficient balance", "m1", "hash");
        assert!(!failed.is_passed());
        assert!(failed.is_service_failure());
        let rejected = ReviewResult::rejected_with_scope_drift("scope changed", "hash");
        assert!(!rejected.is_passed());
        assert!(!rejected.is_service_failure());
        assert!(ReviewResult::mechanical_pass("no semantic review needed").is_passed());
    }
}
