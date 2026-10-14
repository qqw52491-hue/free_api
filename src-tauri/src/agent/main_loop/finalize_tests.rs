use super::*;
use crate::agent::finalization::{FinalizationReceipt, ReceiptReview, ReceiptVerification};
use std::sync::{Arc, Mutex};

fn db() -> DbState {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE agent_checkpoints (run_id TEXT PRIMARY KEY, phase TEXT NOT NULL, snapshot_json TEXT NOT NULL, version INTEGER NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE agent_checkpoint_history (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, anchor_message_id TEXT NOT NULL, phase TEXT NOT NULL, snapshot_json TEXT NOT NULL, created_at TEXT NOT NULL);
CREATE TABLE agent_run_traces (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, goal TEXT NOT NULL, status TEXT NOT NULL, steps_json TEXT, completion_json TEXT, started_at TEXT NOT NULL, updated_at TEXT NOT NULL, completed_at TEXT);
CREATE TABLE chat_sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL, model_id TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, message_count INTEGER NOT NULL DEFAULT 0);
CREATE TABLE chat_messages (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, role TEXT NOT NULL, content TEXT NOT NULL, attachments TEXT, created_at TEXT NOT NULL, model_id TEXT);
INSERT INTO chat_sessions VALUES ('s','t','m','now','now',0);").unwrap();
    DbState(Arc::new(Mutex::new(conn)))
}
fn ready(c: &SandwichContext, cp: &AgentCheckpoint) -> FinalizationReceipt {
    FinalizationReceipt {
        receipt_id: "r".into(),
        diff_hash: "h".into(),
        scope_revision: c.resource_scope.revision,
        contract_fingerprint: String::new(),
        acceptance_fingerprint: crate::agent::finalization::acceptance_fingerprint(
            c,
            cp,
            Some("done"),
        ),
        step_id: 42,
        verification: ReceiptVerification::Verified {
            evidence: "host exit 0".into(),
        },
        review: ReceiptReview::Passed {
            score: 84,
            model_id: "m".into(),
            summary: "ok".into(),
        },
        blockers: vec![],
        risk_flags: vec![],
        finish_payload: json!({"summary":"done"}),
        created_at: "now".into(),
    }
}
fn commit(
    db: &DbState,
    cp: &mut AgentCheckpoint,
    c: &mut SandwichContext,
) -> Result<serde_json::Value, String> {
    commit_completed_run(
        db,
        cp,
        c,
        "run",
        "s",
        "goal",
        &[],
        &AgentInstruction::default(),
        "done",
        "m",
        None,
    )
}
fn context() -> SandwichContext {
    SandwichContext::new("system".into(), "goal".into())
}
fn no_terminal_rows(db: &DbState) {
    let conn = db.0.lock().unwrap();
    for table in [
        "agent_run_traces",
        "chat_messages",
        "agent_checkpoint_history",
    ] {
        let n: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {}", table), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "{table}");
    }
}
#[test]
fn production_commit_refuses_missing_blocked_old_and_stale_receipts_without_marking_done() {
    let db = db();
    let mut c = context();
    let mut cp = AgentCheckpoint::new("s".into(), &c);
    let phase = cp.phase.clone();
    assert!(commit(&db, &mut cp, &mut c).is_err());
    assert_eq!(cp.phase, phase);
    let mut receipt = ready(&c, &cp);
    receipt.verification = ReceiptVerification::Pending;
    cp.pending_finalization = Some(receipt);
    cp.finish_reviewed = true;
    cp.finish_challenge_count = 100;
    assert!(commit(&db, &mut cp, &mut c).is_err());
    assert_eq!(cp.phase, phase);
    let mut old = ready(&c, &cp);
    old.acceptance_fingerprint.clear();
    cp.pending_finalization = Some(old);
    assert!(commit(&db, &mut cp, &mut c).is_err());
    cp.pending_finalization = Some(ready(&c, &cp));
    c.resource_scope.revision += 1;
    assert!(commit(&db, &mut cp, &mut c)
        .unwrap_err()
        .contains("发生变化"));
    cp.pending_finalization = Some(ready(&c, &cp));
    c.review_contract = Some(
        serde_json::from_value(json!({"scope_statement":"corrected","obligations":[]})).unwrap(),
    );
    assert!(commit(&db, &mut cp, &mut c)
        .unwrap_err()
        .contains("发生变化"));
    cp.pending_finalization = Some(ready(&c, &cp));
    c.record_side_effect(43, "local_file".into(), "later write".into());
    assert!(commit(&db, &mut cp, &mut c)
        .unwrap_err()
        .contains("发生变化"));
    assert_eq!(cp.phase, phase);
    no_terminal_rows(&db);
}
#[test]
fn production_commit_preserves_memory_and_database_when_transaction_conflicts() {
    use crate::agent::target_block::TargetBlock;
    let db = db();
    let mut c = context();
    c.block_manager
        .set_blocks(vec![TargetBlock::new("b", "repair")]);
    let mut cp = AgentCheckpoint::new("s".into(), &c);
    crate::agent::checkpoint::save_checkpoint(&db, &mut cp).unwrap();
    cp.pending_finalization = Some(ready(&c, &cp));
    db.0.lock()
        .unwrap()
        .execute("UPDATE agent_checkpoints SET version=version+5", [])
        .unwrap();
    let before_cp = serde_json::to_value(&cp).unwrap();
    let before_c = serde_json::to_value(&c).unwrap();
    assert!(commit(&db, &mut cp, &mut c)
        .unwrap_err()
        .contains("乐观锁冲突"));
    assert_eq!(before_cp, serde_json::to_value(&cp).unwrap());
    assert_eq!(before_c, serde_json::to_value(&c).unwrap());
    no_terminal_rows(&db);
}
#[test]
fn production_commit_advances_block_only_after_ready_and_discloses_waiver() {
    use crate::agent::target_block::TargetBlock;
    let db = db();
    let mut c = context();
    c.block_manager
        .set_blocks(vec![TargetBlock::new("b", "repair")]);
    let mut cp = AgentCheckpoint::new("s".into(), &c);
    let mut receipt = ready(&c, &cp);
    receipt.verification = ReceiptVerification::Waived {
        reason: "环境没有自动测试，终审核对替代证据".into(),
    };
    cp.pending_finalization = Some(receipt);
    // 完成步骤的展示记账不应使验收指纹失效。
    c.active_target_block_mut().unwrap().on_step_finish();
    let emitted = commit(&db, &mut cp, &mut c).unwrap();
    assert!(c.active_target_block().is_none());
    assert_eq!(cp.phase.as_str(), "done");
    assert!(emitted["summary"].as_str().unwrap().contains("验证披露"));
    let conn = db.0.lock().unwrap();
    let status: String = conn
        .query_row("SELECT status FROM agent_run_traces", [], |r| r.get(0))
        .unwrap();
    assert_eq!(status, "succeeded");
    let msg: String = conn
        .query_row("SELECT content FROM chat_messages", [], |r| r.get(0))
        .unwrap();
    assert!(msg.contains("验证披露"));
}
