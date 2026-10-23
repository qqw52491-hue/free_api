use super::*;
use crate::agent::{checkpoint::{AgentCheckpoint, save_checkpoint, load_checkpoint}, context::SandwichContext};
use crate::agent::child_reviewer::*;
use crate::agent::review_team::tests::TestDb;

fn checkpoint(db: &DbState, session: &str) -> AgentCheckpoint {
    let context = SandwichContext::new("sys".into(), "review control fixture".into());
    let mut cp = AgentCheckpoint::new(session.into(), &context);
    save_checkpoint(db, &mut cp).unwrap();
    cp
}
fn child(f: &TestDb, session: &str, model: &str) -> String {
    begin_child_run_extended(&f.db, "parent", session, &f.root, model, 0, "review", None, None, None,
        &ChildRunIdentity::fresh("plan_review")).unwrap()
}

#[tokio::test]
async fn stopping_review_during_terminal_step_preserves_atomic_success_and_failure() {
    use crate::agent::finalization::{commit_failure, commit_finalization};
    for failed in [false, true] {
        let f = TestDb::new();
        let session = uuid::Uuid::new_v4().to_string();
        f.db.0.lock().unwrap().execute(
            "INSERT INTO chat_sessions (id,title,model_id,created_at,updated_at) VALUES (?1,'fixture','m','now','now')",
            [&session],
        ).unwrap();
        let context = SandwichContext::new("sys".into(), "review control fixture".into());
        let mut cp = AgentCheckpoint::new(session.clone(), &context);
        save_checkpoint(&f.db, &mut cp).unwrap();
        let version = cp.version;
        let (_parent, rx) = watch::channel(0);
        let id = uuid::Uuid::new_v4().to_string();
        let _guard = register(&f.db, &session, &id, "slow", false, rx).unwrap();
        // 终审等待中使用真实停止入口，主循环仍持有停止前的快照，直接进入终态提交。
        stop_member(&f.db, &session, &id).unwrap();
        assert!(cp.excluded_review_models.is_empty());
        let status = if failed { "failed" } else { "succeeded" };
        if failed {
            cp.mark_failed("fixture failure".into(), &context);
            commit_failure(&f.db, &mut cp, "run", &session, "goal", &[],
                &json!({"summary":"fixture failure"}), "fixture failure").unwrap();
        } else {
            cp.mark_done(&context);
            commit_finalization(&f.db, &mut cp, "run", &session, "goal", &[],
                &json!({"summary":"done"}), "done", "m").unwrap();
        }
        assert_eq!(cp.version, version + 1, "控制面排除不推进执行快照版本");
        assert_eq!(cp.excluded_review_models, BTreeSet::from(["slow".into()]));
        let restored = load_checkpoint(&f.db, &session).unwrap().unwrap();
        assert_eq!(restored.phase, cp.phase);
        assert_eq!(restored.excluded_review_models, cp.excluded_review_models);
        let conn = f.db.0.lock().unwrap();
        let actual: String = conn.query_row("SELECT status FROM agent_run_traces WHERE id='run'", [], |r| r.get(0)).unwrap();
        assert_eq!(actual, status);
        let (messages, count): (i64, i64) = conn.query_row(
            "SELECT (SELECT COUNT(*) FROM chat_messages WHERE session_id=?1),message_count FROM chat_sessions WHERE id=?1",
            [&session], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!((messages, count), (1, 1));
        let history: String = conn.query_row(
            "SELECT snapshot_json FROM agent_checkpoint_history WHERE session_id=?1", [&session], |r| r.get(0),
        ).unwrap();
        let history: AgentCheckpoint = serde_json::from_str(&history).unwrap();
        assert_eq!(history.phase, cp.phase);
        assert_eq!(history.excluded_review_models, cp.excluded_review_models);
    }
}

#[tokio::test]
async fn review_exclusion_does_not_bypass_genuine_terminal_version_conflicts() {
    let f = TestDb::new();
    let session = uuid::Uuid::new_v4().to_string();
    let context = SandwichContext::new("sys".into(), "fixture".into());
    let mut cp = AgentCheckpoint::new(session.clone(), &context);
    save_checkpoint(&f.db, &mut cp).unwrap();
    let (_parent, rx) = watch::channel(0);
    let id = uuid::Uuid::new_v4().to_string();
    let _guard = register(&f.db, &session, &id, "slow", false, rx).unwrap();
    stop_member(&f.db, &session, &id).unwrap();
    // 另一执行保存实际推进快照，不能因控制字段可合并而盲目覆盖这个版本。
    let mut other = load_checkpoint(&f.db, &session).unwrap().unwrap();
    other.step_id = 17;
    save_checkpoint(&f.db, &mut other).unwrap();
    cp.mark_done(&context);
    let before = serde_json::to_value(&cp).unwrap();
    let error = crate::agent::finalization::commit_finalization(
        &f.db, &mut cp, "run", &session, "goal", &[], &json!({}), "done", "m",
    ).unwrap_err();
    assert!(error.contains("乐观锁冲突"));
    assert_eq!(serde_json::to_value(&cp).unwrap(), before, "失败提交不修改调用方内存");
    let restored = load_checkpoint(&f.db, &session).unwrap().unwrap();
    assert_eq!(restored.step_id, 17);
    assert_eq!(restored.version, other.version);
    assert!(restored.excluded_review_models.contains("slow"));
    let conn = f.db.0.lock().unwrap();
    for table in ["agent_run_traces", "chat_messages", "agent_checkpoint_history"] {
        let count: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0, "{table} 不得留下半截终态");
    }
}

#[tokio::test]
async fn terminal_merge_keeps_new_task_separate_from_previous_exclusions() {
    let f = TestDb::new();
    let session = uuid::Uuid::new_v4().to_string();
    f.db.0.lock().unwrap().execute(
        "INSERT INTO chat_sessions (id,title,model_id,created_at,updated_at) VALUES (?1,'fixture','m','now','now')", [&session],
    ).unwrap();
    let previous = checkpoint(&f.db, &session);
    let (_parent, rx) = watch::channel(0);
    let id = uuid::Uuid::new_v4().to_string();
    let _guard = register(&f.db, &session, &id, "slow", false, rx).unwrap();
    stop_member(&f.db, &session, &id).unwrap();
    let context = SandwichContext::new("sys".into(), "new task".into());
    let mut next = AgentCheckpoint::new(session.clone(), &context);
    next.version = previous.version;
    assert_ne!(next.data.history_task_id, previous.data.history_task_id);
    next.mark_done(&context);
    crate::agent::finalization::commit_finalization(
        &f.db, &mut next, "run", &session, "goal", &[], &json!({}), "done", "m",
    ).unwrap();
    assert!(load_checkpoint(&f.db, &session).unwrap().unwrap().excluded_review_models.is_empty());
    assert!(ensure_allowed(&f.db, &session, "slow").is_ok());
    assert!(stop_member(&f.db, &session, &id).is_err(), "旧审查不能污染新任务");
}

#[tokio::test]
async fn individual_stop_persists_exclusion_preserves_peers_and_rejects_late_success() {
    let f = TestDb::new();
    let session = uuid::Uuid::new_v4().to_string();
    let mut stale = checkpoint(&f.db, &session);
    let a = child(&f, &session, "slow");
    let b = child(&f, &session, "fast");
    let (parent, rx) = watch::channel(0);
    let guard_a = register(&f.db, &session, &a, "slow", true, rx.clone()).unwrap();
    let guard_b = register(&f.db, &session, &b, "fast", true, rx.clone()).unwrap();
    let mut a_rx = guard_a.interrupt();
    let b_rx = guard_b.interrupt();
    stop_member(&f.db, &session, &a).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), a_rx.changed()).await.unwrap().unwrap();
    assert!(!b_rx.has_changed().unwrap());
    assert_eq!(*parent.borrow(), 0, "主 Agent 的停止信号未被修改");
    let receipts = poll_completed_delivery_receipts(&f.db, "parent", Some(&[a.clone()])).unwrap();
    assert_eq!(receipts.len(), 1, "取消必须有可消费的回执，齐套等待才会收口");
    assert_eq!(receipts[0].status, "cancelled");
    assert!(!crate::agent::types::ChildErrorKind::Cancelled.is_failover_eligible());
    finish_child_run(&f.db, &a, "succeeded", 1, 1, 1, None, Some("{}"));
    let statuses: Vec<String> = f.db.0.lock().unwrap().prepare("SELECT status FROM agent_child_runs ORDER BY id").unwrap()
        .query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    assert!(statuses.contains(&"cancelled".into()) && statuses.contains(&"running".into()));
    assert!(ensure_allowed(&f.db, &session, "slow").is_err());
    assert!(ensure_allowed(&f.db, &session, "fast").is_ok());
    // 正在执行的旧内存快照保存不能吞掉控制面新增的排除。
    stale.step_id = 9;
    save_checkpoint(&f.db, &mut stale).unwrap();
    let restored = load_checkpoint(&f.db, &session).unwrap().unwrap();
    assert!(restored.excluded_review_models.contains("slow"));
    assert_eq!(restored.step_id, 9);
    // 同任务续跑保留，新任务重置；旧的按钮也不能污染新任务。
    let mut resumed = restored.clone();
    resumed.restore_task_for_entry(&mut SandwichContext::new("sys".into(), "继续".into()), "继续当前任务");
    assert!(resumed.excluded_review_models.contains("slow"));
    let mut fresh = AgentCheckpoint::new(session.clone(), &SandwichContext::new("sys".into(), "new task".into()));
    fresh.version = restored.version;
    save_checkpoint(&f.db, &mut fresh).unwrap();
    assert!(ensure_allowed(&f.db, &session, "slow").is_ok());
    assert!(stop_member(&f.db, &session, &a).is_err());
    assert!(excluded_models(&f.db, &session).unwrap().is_empty());
}

#[tokio::test]
async fn finish_stop_and_parent_stop_have_separate_signals_and_session_checks() {
    let f = TestDb::new();
    let session = uuid::Uuid::new_v4().to_string();
    checkpoint(&f.db, &session);
    let (parent, rx) = watch::channel(0);
    let id = uuid::Uuid::new_v4().to_string();
    let other = uuid::Uuid::new_v4().to_string();
    let g1 = register(&f.db, &session, &id, "slow", false, rx.clone()).unwrap();
    let g2 = register(&f.db, &session, &other, "fast", false, rx).unwrap();
    assert!(stop_member(&f.db, "another-session", &id).is_err());
    assert!(excluded_models(&f.db, &session).unwrap().is_empty());
    stop_member(&f.db, &session, &id).unwrap();
    assert!(g1.interrupt().has_changed().unwrap());
    assert!(!g2.interrupt().has_changed().unwrap());
    assert!(register(&f.db, &session, "new-review", "slow", false, parent.subscribe()).is_err());
    let mut other_rx = g2.interrupt();
    parent.send_modify(|g| *g += 1);
    tokio::time::timeout(std::time::Duration::from_secs(1), other_rx.changed()).await.unwrap().unwrap();
    drop(g1);
    assert!(stop_member(&f.db, &session, &id).is_err(), "已收束的审查不能借旧 ID 再发停止");
}

#[tokio::test]
async fn exclusion_is_unioned_across_control_updates_without_erasing_completed_opinions() {
    let f = TestDb::new();
    let session = uuid::Uuid::new_v4().to_string();
    let stale = checkpoint(&f.db, &session);
    let (tx, rx) = watch::channel(0);
    let a = child(&f, &session, "a");
    let b = child(&f, &session, "b");
    let _a = register(&f.db, &session, &a, "a", true, rx.clone()).unwrap();
    let _b = register(&f.db, &session, &b, "b", true, rx).unwrap();
    // 完成与点击交错时，不把已有有效回执改成取消。
    finish_child_run(&f.db, &a, "succeeded", 1, 2, 3, None, Some("{\"findings\":[{\"id\":\"F1\",\"severity\":\"blocking\"}]}"));
    stop_member(&f.db, &session, &a).unwrap();
    stop_member(&f.db, &session, &b).unwrap();
    let mut stale = stale;
    save_checkpoint(&f.db, &mut stale).unwrap();
    assert_eq!(stale.excluded_review_models, BTreeSet::from(["a".into(), "b".into()]));
    let (status, raw): (String, String) = f.db.0.lock().unwrap().query_row(
        "SELECT status,result_json FROM agent_child_runs WHERE id=?1", [&a], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(status, "succeeded");
    assert!(raw.contains("blocking"));
    assert_eq!(*tx.borrow(), 0);
}

#[tokio::test]
async fn real_silent_child_is_cancelled_without_waiting_for_its_provider() {
    let mock = crate::agent::child_reviewer::retirement_tests::MockReviewer::silent().await;
    let session = uuid::Uuid::new_v4().to_string();
    checkpoint(&mock.db, &session);
    let (_tx, rx) = watch::channel(0);
    let id = spawn_child_reviewer(None, &mock.db, "parent", &session, &mock.root, "mock", "pro", "low",
        &json!({"question":"核对合成调用，不修改文件","evidence_excerpt":"本机回归夹具"}), "", None, rx.clone(), &ChildRunIdentity::fresh("plan_review")).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while mock.requests.lock().unwrap().is_empty() { tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
    }).await.unwrap();
    stop_member(&mock.db, &session, &id).unwrap();
    assert_eq!(mock.wait_terminal(&id).await.0, "cancelled");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while active().lock().unwrap().contains_key(&id) { tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
    }).await.unwrap();
    assert!(spawn_child_reviewer(None, &mock.db, "parent", &session, &mock.root, "mock", "pro", "low",
        &json!({"question":"再次审查","evidence_excerpt":"fixture"}), "", None, rx, &ChildRunIdentity::fresh("stage_review")).await.is_err());
    assert_eq!(mock.requests.lock().unwrap().len(), 1, "后续审查没有再次请求已排除模型");
}

#[tokio::test]
async fn real_silent_finish_stops_independently_and_is_not_redispatched_to_backup() {
    use crate::agent::{reviewer_gatekeeper::*, router::ModelRoutingConfig};
    use tauri::Listener;
    let mock = crate::agent::child_reviewer::retirement_tests::MockReviewer::silent().await;
    let session = uuid::Uuid::new_v4().to_string();
    let mut context = SandwichContext::new("sys".into(), "生成并核对报告".into());
    let baseline = GitBaseline::capture(&mock.root);
    std::fs::write(mock.root.join("report.md"), "需要审查的报告").unwrap();
    context.record_side_effect(1, "write_file".into(), "report.md".into());
    let mut cp = AgentCheckpoint::new(session.clone(), &context);
    save_checkpoint(&mock.db, &mut cp).unwrap();
    let mut routing = ModelRoutingConfig::default();
    routing.role_models.insert("reviewer".into(), "mock".into());
    routing.reviewer_backup_model = "backup".into();
    mock.db.0.lock().unwrap().execute("INSERT INTO models (id,platform_id,name,display_name,created_at) VALUES ('backup','mock','backup','backup','now')", []).unwrap();
    let app = tauri::test::mock_app();
    let events = std::sync::Arc::new(Mutex::new(Vec::<Value>::new()));
    let out = events.clone();
    app.listen(format!("agent-progress-{session}"), move |event| {
        if let Ok(value) = serde_json::from_str(event.payload()) { out.lock().unwrap().push(value); }
    });
    let (parent, mut rx) = watch::channel(0);
    let mut triage = crate::agent::triage::TriageVerdict::fallback("fixture");
    triage.final_review_mode = "artifact_quality".into();
    let review = evaluate_finish_diff(app.handle(), &mock.db, &context, &routing, 1, &mock.root, &baseline,
        chrono::Utc::now(), &session, &mut rx, None, None, &[], None, None, &[], None, Some(&triage), None, Some("本轮报告"));
    let cancel = async {
        loop {
            let id = active().lock().unwrap().iter().find(|(_, entry)| entry.session == session && !entry.child).map(|(id, _)| id.clone());
            if !mock.requests.lock().unwrap().is_empty() {
                if let Some(id) = id { stop_member(&mock.db, &session, &id).unwrap(); break; }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async { tokio::join!(review, cancel) }).await.unwrap();
    assert_eq!(result.verdict, "cancelled");
    assert!(!result.is_passed());
    assert_eq!(*parent.borrow(), 0);
    assert_eq!(mock.requests.lock().unwrap().len(), 1, "人工停止不触发备用重派");
    assert!(events.lock().unwrap().iter().any(|event| event["phase"] == "cancelled"));
    let again = evaluate_finish_diff(app.handle(), &mock.db, &context, &routing, 2, &mock.root, &baseline,
        chrono::Utc::now(), &session, &mut rx, None, None, &[], None, None, &[], None, Some(&triage), None, Some("再次核对报告")).await;
    assert_eq!(again.verdict, "cancelled");
    assert_eq!(mock.requests.lock().unwrap().len(), 1, "再次终审不调用已排除的模型");
}
