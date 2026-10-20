use super::*;
use crate::agent::review_contract::{apply_review_contract_update, ReviewContract};
use crate::agent::utils::evidence_refs::MAX_EVIDENCE_REFERENCE_CHARS;
use crate::db::DbState;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn contract(refs: &[String]) -> ReviewContract {
    serde_json::from_value(json!({"scope_statement":"核验本次改动", "obligations":[{
        "id":"proof","question":"证据是否支持结论","status":"evidenced",
        "closure_form":"evidence","evidence_refs":refs
    }]}))
    .unwrap()
}

fn verification(refs: &[String]) -> ChangeVerification {
    ChangeVerification {
        status: "verified".into(),
        evidence_refs: refs.to_vec(),
        waived_reason: None,
    }
}

fn plan(refs: &[String]) -> Result<Vec<String>, String> {
    let plan = serde_json::from_value(json!({"plan_summary":"核查实际证据与最终结论的对应关系",
        "target_files":[],"evidence_refs":refs}))
    .unwrap();
    crate::agent::plan_gate::validate_plan_submission(&plan, true).map(|v| v.1)
}

#[test]
fn plan_contract_and_verification_preserve_wide_refs_with_the_same_budget() {
    let refs: Vec<String> = (0..12)
        .map(|i| format!("src/{}/file_{i}.rs:12-30", "长目录/".repeat(70)))
        .collect();
    let mut submitted = refs.clone();
    submitted.push(format!(" {} ", refs[0]));
    let planned = plan(&submitted).unwrap();
    let mut current = None;
    apply_review_contract_update(&mut current, contract(&submitted)).unwrap();
    let mut verified = None;
    apply_change_verification_update(
        &mut verified,
        verification(&submitted),
        &EvidenceWindow::default(),
    )
    .unwrap();
    assert_eq!(planned, refs);
    assert_eq!(current.as_ref().unwrap().obligations[0].evidence_refs, refs);
    assert_eq!(verified.as_ref().unwrap().evidence_refs, refs);
    let saved = serde_json::to_string(&(current, verified)).unwrap();
    let restored: (Option<ReviewContract>, Option<ChangeVerification>) =
        serde_json::from_str(&saved).unwrap();
    assert_eq!(restored.0.unwrap().obligations[0].evidence_refs, refs);
    assert_eq!(restored.1.unwrap().evidence_refs, refs);
}

#[test]
fn unicode_budget_includes_separators_and_rejection_is_atomic() {
    let exact = vec!["界".repeat(MAX_EVIDENCE_REFERENCE_CHARS - 2), "a".into()];
    assert!(plan(&exact).is_ok());
    let mut current = None;
    apply_review_contract_update(&mut current, contract(&exact)).unwrap();
    let old_contract = current.clone();
    let mut verified = None;
    apply_change_verification_update(
        &mut verified,
        verification(&exact),
        &EvidenceWindow::default(),
    )
    .unwrap();
    let old_verified = verified.clone();
    let over = vec![exact[0].clone(), "ab".into()];
    for message in [
        plan(&over).unwrap_err(),
        apply_review_contract_update(&mut current, contract(&over)).unwrap_err(),
        apply_change_verification_update(
            &mut verified,
            verification(&over),
            &EvidenceWindow::default(),
        )
        .unwrap_err(),
    ] {
        assert!(
            message.contains("16384") && message.contains("容量"),
            "{message}"
        );
    }
    assert_eq!(current, old_contract);
    assert_eq!(verified, old_verified);
    assert!(plan(&[String::new()]).is_err());
    assert!(apply_review_contract_update(&mut None, contract(&[" ".into()])).is_err());
    assert!(apply_change_verification_update(
        &mut None,
        verification(&[" ".into()]),
        &EvidenceWindow::default()
    )
    .is_err());
}

#[test]
fn merged_contract_has_one_total_budget_without_losing_existing_obligations() {
    let mut current = None;
    apply_review_contract_update(&mut current, contract(&["a".repeat(9000)])).unwrap();
    let previous = current.clone();
    let extra: ReviewContract = serde_json::from_value(json!({"obligations":[{
        "id":"other", "question":"另一项结论", "status":"evidenced", "evidence_refs":["b".repeat(9000)]
    }]})).unwrap();
    assert!(apply_review_contract_update(&mut current, extra)
        .unwrap_err()
        .contains("16384"));
    assert_eq!(current, previous);
}

fn db() -> DbState {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE agent_step_logs(output_id TEXT PRIMARY KEY,session_id TEXT,step_id INTEGER,raw_output TEXT,created_at TEXT);
        CREATE TABLE agent_run_traces(id TEXT PRIMARY KEY,session_id TEXT);
        CREATE TABLE agent_run_steps(run_id TEXT,seq INTEGER,step_json TEXT,PRIMARY KEY(run_id,seq));").unwrap();
    DbState(Arc::new(Mutex::new(conn)))
}

fn put(db: &DbState, id: &str, session: &str, run: &str, step: usize, seq: usize, status: &str) {
    let conn = db.0.lock().unwrap();
    conn.execute("INSERT INTO agent_step_logs VALUES (?1,?2,?3,'exit=0; preserved output','2026-10-20T06:00:00Z')",params![id,session,step]).unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO agent_run_traces VALUES (?1,?2)",
        params![run, session],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO agent_run_steps VALUES (?1,?2,?3)",
        params![
            run,
            seq,
            json!({"id":step,"output_id":id,"status":status,"tool":"local_shell","action":"exec"})
                .to_string()
        ],
    )
    .unwrap();
}

#[test]
fn output_handles_resolve_real_origin_without_rerunning_or_scanning_id_as_step() {
    let db = db();
    put(&db, "fresh_out_step_999", "s", "run", 13, 22, "succeeded");
    let uuid = uuid::Uuid::new_v4().to_string();
    put(&db, &uuid, "s", "run", 13, 23, "failed");
    let source = EvidenceLookup {
        db: &db,
        session_id: "s",
        run_id: "run",
    };
    let window = EvidenceWindow {
        last_write_step: Some(12),
        current_step: 15,
    };
    for reference in [
        "output_id:fresh_out_step_999",
        "output_id = `fresh_out_step_999`",
        "{\"output_id\":\"fresh_out_step_999\"}",
    ] {
        let mut current = None;
        apply_change_verification_with_source(
            &mut current,
            verification(&[reference.into()]),
            &window,
            Some(&source),
        )
        .unwrap();
        assert!(current.unwrap().is_resolved());
    }
    let mut handle_only = None;
    apply_change_verification_with_source(
        &mut handle_only,
        verification(&[uuid]),
        &window,
        Some(&source),
    )
    .unwrap();
    assert!(
        handle_only.is_some(),
        "非零退出也是已完成的观测；是否满足目标仍由终审判定"
    );
    let mut current = None;
    apply_change_verification_with_source(
        &mut current,
        verification(&["step_13".into()]),
        &window,
        Some(&source),
    )
    .unwrap();
    let conn = db.0.lock().unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM agent_step_logs", [], |r| r
            .get::<_, usize>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row(
            "SELECT raw_output FROM agent_step_logs WHERE output_id='fresh_out_step_999'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "exit=0; preserved output"
    );
}

#[test]
fn old_foreign_unknown_ambiguous_and_unfinished_outputs_cannot_close_new_writes() {
    let db = db();
    for (id, session, run, step, seq, status) in [
        ("old", "s", "run", 10, 1, "succeeded"),
        ("same", "s", "run", 12, 2, "succeeded"),
        ("future", "s", "run", 15, 3, "succeeded"),
        ("running", "s", "run", 13, 4, "running"),
        ("other-run", "s", "old-run", 13, 0, "succeeded"),
        ("other-session", "t", "foreign-run", 13, 0, "succeeded"),
        ("ambiguous", "s", "run", 13, 5, "succeeded"),
        ("fresh", "s", "run", 13, 6, "succeeded"),
    ] {
        put(&db, id, session, run, step, seq, status);
    }
    db.0.lock()
        .unwrap()
        .execute(
            "INSERT INTO agent_run_steps VALUES ('old-run',1,?1)",
            [json!({"id":13,"output_id":"ambiguous","status":"succeeded"}).to_string()],
        )
        .unwrap();
    let source = EvidenceLookup {
        db: &db,
        session_id: "s",
        run_id: "run",
    };
    let window = EvidenceWindow {
        last_write_step: Some(12),
        current_step: 15,
    };
    let mut current = Some(verification(&["step_13".into()]));
    let previous = current.clone();
    for id in [
        "old",
        "same",
        "future",
        "running",
        "other-run",
        "other-session",
        "ambiguous",
        "missing",
    ] {
        let refs = vec![format!(
            "output_id:{id} (声称 run_id=run, step_13, created_at=2099-01-01)"
        )];
        assert!(
            apply_change_verification_with_source(
                &mut current,
                verification(&refs),
                &window,
                Some(&source)
            )
            .is_err(),
            "{id}"
        );
        assert_eq!(current, previous);
    }
    assert!(apply_change_verification_with_source(
        &mut current,
        verification(&["output_id:fresh".into(), "output_id:old".into()]),
        &window,
        Some(&source)
    )
    .is_err());
}

#[test]
fn host_state_update_accepts_a_handle_and_retains_it_across_checkpoint_restore() {
    let db = db();
    put(&db, "proof", "s", "run", 13, 20, "succeeded");
    let mut context = crate::agent::context::SandwichContext::new("system".into(), "核验".into());
    context.record_side_effect(12, "local_file".into(), "src/a.rs".into());
    let instruction = serde_json::from_value(json!({"tool":"finish","command":{"summary":"完成"},
        "change_verification_update":{"status":"verified","evidence_refs":["output_id:proof"]}}))
    .unwrap();
    let source = EvidenceLookup {
        db: &db,
        session_id: "s",
        run_id: "run",
    };
    crate::agent::finish_guard::apply_instruction_state_updates_with_source(
        &mut context,
        &instruction,
        14,
        &source,
    );
    assert_eq!(
        context.change_verification.as_ref().unwrap().evidence_refs,
        ["output_id:proof"]
    );
    let checkpoint = crate::agent::checkpoint::AgentCheckpoint::new("s".into(), &context);
    let restored: crate::agent::checkpoint::AgentCheckpoint =
        serde_json::from_str(&serde_json::to_string(&checkpoint).unwrap()).unwrap();
    assert_eq!(
        restored.data.change_verification,
        context.change_verification
    );
    context.record_side_effect(14, "local_file".into(), "later write".into());
    crate::agent::finish_guard::apply_instruction_state_updates_with_source(
        &mut context,
        &instruction,
        15,
        &source,
    );
    assert!(
        context.change_verification.is_none(),
        "后续写入仍使旧验证失效"
    );
}
