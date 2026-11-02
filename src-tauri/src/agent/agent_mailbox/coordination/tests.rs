use super::*;
use std::sync::{Arc, Barrier, Mutex};
struct Fixture {
    db: DbState,
    path: std::path::PathBuf,
    root: Actor,
    a: Actor,
    b: Actor,
}
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("inbox-coordination-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        let db = DbState(Arc::new(Mutex::new(
            crate::db::init_db(&path.join("test.db")).unwrap(),
        )));
        let root = Actor::new("root", "session");
        crate::agent::run_trace::upsert_snapshot(
            &db,
            &root.run,
            &root.session,
            "goal",
            "running",
            &[],
            None,
        )
        .unwrap();
        let child = || {
            let id = crate::agent::child_reviewer::begin_child_run(
                &db,
                &root.run,
                &root.session,
                &path,
                "model",
                0,
                "worker",
                None,
            )
            .unwrap();
            Actor::new(&id, &format!("child:{id}"))
        };
        let a = child();
        let b = child();
        Self {
            db,
            path,
            root,
            a,
            b,
        }
    }
    fn end(&self, actor: &Actor) {
        self.db.0.lock().unwrap().execute("UPDATE agent_child_runs SET status='succeeded',result_json='{\"findings\":[\"original\"]}' WHERE id=?1",[&actor.run]).unwrap();
    }
    fn send(&self, a: &Actor, b: &Actor, key: &str) -> Value {
        send(
            &self.db,
            a,
            &json!({"target":b.run,"message":key}),
            Some(key),
            false,
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn both_members_can_end_after_sending_without_losing_either_inbox() {
    let f = Fixture::new();
    let ab = f.send(&f.a, &f.b, "ab");
    let ba = f.send(&f.b, &f.a, "ba");
    f.end(&f.a);
    f.end(&f.b);
    assert!(pending_on(&f.db.0.lock().unwrap(), &f.root.run).unwrap());
    let tickets = claim(&f.db).unwrap();
    assert_eq!(tickets.len(), 2);
    assert!(claim(&f.db).unwrap().is_empty());
    for t in tickets {
        let mut messages = vec![];
        assert_eq!(deliver_specialist(&f.db, &t.run, &mut messages).unwrap(), 1);
        assert!(serde_json::to_string(&messages)
            .unwrap()
            .contains(if t.original == f.a.run { "ba" } else { "ab" }));
        let original: String =
            f.db.0
                .lock()
                .unwrap()
                .query_row(
                    "SELECT status FROM agent_child_runs WHERE id=?1",
                    [&t.original],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(original, "succeeded");
    }
    for (actor, id) in [
        (&f.a, ab["message_id"].as_str().unwrap()),
        (&f.b, ba["message_id"].as_str().unwrap()),
    ] {
        assert_eq!(receipt(&f.db, actor, id).unwrap()["delivery"], "in_context");
    }
    assert!(!pending_on(&f.db.0.lock().unwrap(), &f.root.run).unwrap());
}

#[test]
fn concurrent_claimers_start_only_one_associated_execution() {
    let f = Fixture::new();
    f.end(&f.b);
    f.send(&f.a, &f.b, "race");
    let gate = Arc::new(Barrier::new(3));
    let count = std::thread::scope(|s| {
        let mut handles = vec![];
        for _ in 0..2 {
            let g = gate.clone();
            let db = &f.db;
            handles.push(s.spawn(move || {
                g.wait();
                claim(db).unwrap().len()
            }));
        }
        gate.wait();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .sum::<usize>()
    });
    assert_eq!(count, 1);
}

#[test]
fn received_history_and_acknowledgement_roll_back_together() {
    let f = Fixture::new();
    f.end(&f.b);
    let msg = f.send(&f.a, &f.b, "persist");
    let t = claim(&f.db).unwrap().remove(0);
    f.db.0.lock().unwrap().execute_batch("CREATE TRIGGER reject_receipt BEFORE UPDATE OF delivered_at ON agent_messages BEGIN SELECT RAISE(ABORT,'reject receipt'); END;").unwrap();
    let mut history = vec![];
    assert!(deliver_specialist(&f.db, &t.run, &mut history).is_err());
    assert!(history.is_empty());
    assert_eq!(
        pending(&f.db, &Actor::new(&t.run, &format!("child:{}", t.run)))
            .unwrap()
            .len(),
        1
    );
    assert!(
        receipt(&f.db, &f.a, msg["message_id"].as_str().unwrap()).unwrap()["delivered_at"]
            .is_null()
    );
}

#[test]
fn progress_preserves_recipient_and_reports_failed_claim_without_restarting() {
    let f = Fixture::new();
    f.end(&f.b);
    f.send(&f.a, &f.b, "fail");
    let t = claim(&f.db).unwrap().remove(0);
    f.db.0
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_child_runs SET status='failed' WHERE id=?1",
            [&t.run],
        )
        .unwrap();
    assert!(claim(&f.db).unwrap().is_empty());
    let view = list(&f.db, &f.root, 0).unwrap();
    assert_eq!(view["items"][0]["recipient_run_id"], f.b.run);
    assert_eq!(view["items"][0]["execution_run_id"], t.run);
    assert_eq!(view["items"][0]["delivery_status"], "not_received");
    assert!(!pending_on(&f.db.0.lock().unwrap(), &f.root.run).unwrap());
}

#[test]
fn terminal_root_does_not_admit_or_wake_new_messages() {
    let f = Fixture::new();
    f.end(&f.b);
    f.db.0
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_run_traces SET status='succeeded' WHERE id=?1",
            [&f.root.run],
        )
        .unwrap();
    assert!(send(
        &f.db,
        &f.a,
        &json!({"target":f.b.run,"message":"late"}),
        Some("late"),
        false
    )
    .is_err());
    assert!(claim(&f.db).unwrap().is_empty());
}

#[test]
fn late_mail_survives_old_execution_completion_and_reply_returns_to_ended_sender() {
    let f = Fixture::new();
    f.end(&f.b);
    f.send(&f.a, &f.b, "question");
    f.end(&f.a);
    let first = claim(&f.db).unwrap().remove(0);
    deliver_specialist(&f.db, &first.run, &mut vec![]).unwrap();
    // A second accepted letter is assigned while the first run prepares finish.
    f.send(&f.root, &f.b, "late material");
    assert!(claim(&f.db).unwrap().is_empty());
    let reader = Actor::new(&first.run, &format!("child:{}", first.run));
    f.send(&reader, &f.a, "direct answer");
    let output = crate::agent::execution::actions::Output {
        status: "succeeded".into(),
        findings: vec!["answered".into()],
        ..Default::default()
    };
    super::super::replies::finish(&f.db, &first, output, vec![], 1).unwrap();
    let next = claim(&f.db).unwrap();
    assert_eq!(next.len(), 2);
    let mut recipients = std::collections::HashSet::new();
    for ticket in next {
        assert_ne!(ticket.run, first.run);
        recipients.insert(ticket.original);
        let mut history = vec![];
        assert_eq!(
            deliver_specialist(&f.db, &ticket.run, &mut history).unwrap(),
            1
        );
    }
    assert!(recipients.contains(&f.a.run));
    assert!(recipients.contains(&f.b.run));
    let root_mail = pending(&f.db, &f.root).unwrap();
    assert!(
        root_mail.is_empty(),
        "direct answer does not route through root"
    );
}

#[tokio::test]
async fn real_start_failure_retains_claim_and_does_not_spin_or_change_original_result() {
    let f = Fixture::new();
    f.end(&f.b);
    let mut changes = crate::agent::job_engine::job_watch_receiver();
    // No configured model: real coordinator + supervisor must report a failure.
    f.send(&f.a, &f.b, "missing model");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let status: Option<String> =
                f.db.0
                    .lock()
                    .unwrap()
                    .query_row(
                        "SELECT status FROM agent_child_runs WHERE purpose='readonly_inbox'",
                        [],
                        |r| r.get(0),
                    )
                    .optional()
                    .unwrap();
            if status.as_deref() == Some("failed") {
                break;
            }
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(claim(&f.db).unwrap().is_empty());
    let view = list(&f.db, &f.root, 0).unwrap();
    assert_eq!(view["items"][0]["delivery_status"], "not_received");
    let result: String =
        f.db.0
            .lock()
            .unwrap()
            .query_row(
                "SELECT result_json FROM agent_child_runs WHERE id=?1",
                [&f.b.run],
                |r| r.get(0),
            )
            .unwrap();
    assert!(result.contains("original"));
}

#[test]
fn capacity_deferred_mail_remains_pending_until_a_slot_is_available() {
    let f = Fixture::new();
    f.end(&f.b);
    f.send(&f.a, &f.b, "deferred");
    // Fill the existing worker allowance, without inventing a second limit.
    for n in 1..crate::agent::child_reviewer::MAX_CHILDREN_PER_KIND_PER_RUN {
        f.db.0.lock().unwrap().execute("INSERT INTO agent_child_runs(id,parent_run_id,parent_session_id,workspace_root,model_id,status,started_at,kind) VALUES (?1,?2,?3,?4,'model','running','now','worker')",params![format!("busy-{n}"),f.root.run,f.root.session,f.path.to_string_lossy()]).unwrap();
    }
    assert!(claim(&f.db).unwrap().is_empty());
    assert!(pending_on(&f.db.0.lock().unwrap(), &f.root.run).unwrap());
    let view = list(&f.db, &f.root, 0).unwrap();
    assert_eq!(view["items"][0]["delivery_status"], "pending_start");
    f.end(&f.a);
    assert_eq!(claim(&f.db).unwrap().len(), 1);
}

#[test]
fn worker_send_to_ended_member_replies_directly_and_all_receipts_allow_root_completion() {
    use crate::agent::{
        checkpoint::{save_checkpoint_and_consume, AgentCheckpoint, AppliedReceipt},
        context::SandwichContext,
        execution::runtime::{self, Snapshot},
    };
    let f = Fixture::new();
    crate::agent::conversation::begin_turn(&f.db, &f.root.session, "model", "communication test")
        .unwrap();
    // This is a deterministic production-entry integration test. Without a
    // Tokio executor, claim is advanced explicitly and no model is started.
    assert!(tokio::runtime::Handle::try_current().is_err());
    let config = json!({"allowed_tools":["send_message"]}).to_string();
    f.db.0
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_child_runs SET execution_config_json=?1 WHERE id IN (?2,?3)",
            params![config, f.a.run, f.b.run],
        )
        .unwrap();
    f.end(&f.a);
    let original: String =
        f.db.0
            .lock()
            .unwrap()
            .query_row(
                "SELECT result_json FROM agent_child_runs WHERE id=?1",
                [&f.a.run],
                |r| r.get(0),
            )
            .unwrap();
    let send_action = |actor: &Actor, target: &str, content: Value, step| {
        runtime::authorize(&f.db, &actor.run, "send_message").unwrap();
        let (_tx, mut rx) = tokio::sync::watch::channel(0u64);
        let raw = futures::executor::block_on(super::super::specialist_action(
            &f.db,
            &actor.run,
            "send_message",
            &json!({"target":target,"data":content}),
            step,
            &mut rx,
            60,
            None,
        ))
        .unwrap()
        .unwrap();
        serde_json::from_str::<Value>(&raw).unwrap()
    };
    let question = send_action(
        &f.b,
        &f.a.run,
        json!({"question":"琥珀和青石各是多少？"}),
        1,
    );
    assert!(pending_on(&f.db.0.lock().unwrap(), &f.root.run).unwrap());
    let tickets = claim(&f.db).unwrap();
    assert_eq!(tickets.len(), 1);
    let ticket = &tickets[0];
    assert!(ticket.inbox);
    assert_eq!(ticket.original, f.a.run);
    assert_eq!(
        runtime::config(&f.db, &ticket.run).unwrap().allowed_tools,
        Some(vec!["send_message".into()])
    );
    assert!(runtime::authorize(&f.db, &ticket.run, "run_command").is_err());
    assert!(runtime::authorize(&f.db, &ticket.run, "write_file").is_err());
    assert!(claim(&f.db).unwrap().is_empty());

    let mut snapshot = Snapshot::new(&ticket.run, "worker", json!({"messages":[]}));
    let mut history = vec![];
    assert_eq!(
        super::super::deliver_specialist_with_snapshot(
            &f.db,
            &ticket.run,
            &mut history,
            Some(&mut snapshot),
        )
        .unwrap(),
        1
    );
    assert!(serde_json::to_string(&history).unwrap().contains(&f.b.run));
    let answerer = Actor::new(&ticket.run, &format!("child:{}", ticket.run));
    let answer = send_action(&answerer, &f.b.run, json!({"琥珀":17,"青石":29}), 1);
    snapshot.phase = "ended".into();
    snapshot.save(&f.db).unwrap();
    super::super::replies::finish(
        &f.db,
        ticket,
        crate::agent::execution::actions::Output {
            status: "succeeded".into(),
            findings: vec!["sent directly".into()],
            ..Default::default()
        },
        history,
        1,
    )
    .unwrap();
    // A send-based inbox finish must not synthesize a second answer or notify
    // the root as if the root were the data recipient.
    assert!(pending(&f.db, &f.root).unwrap().is_empty());
    assert_eq!(pending(&f.db, &f.b).unwrap().len(), 1);
    let payload = receipt(&f.db, &f.b, answer["message_id"].as_str().unwrap()).unwrap();
    let data = &payload["payload"]["data"];
    assert_eq!(
        data["琥珀"].as_i64().unwrap() + data["青石"].as_i64().unwrap(),
        46
    );
    let mut b_snapshot = Snapshot::load(&f.db, &f.b.run, "worker").unwrap().unwrap();
    let mut b_history = vec![];
    assert_eq!(
        super::super::deliver_specialist_with_snapshot(
            &f.db,
            &f.b.run,
            &mut b_history,
            Some(&mut b_snapshot),
        )
        .unwrap(),
        1
    );
    assert_eq!(
        super::super::deliver_specialist_with_snapshot(
            &f.db,
            &f.b.run,
            &mut b_history,
            Some(&mut b_snapshot),
        )
        .unwrap(),
        0
    );
    crate::agent::child_reviewer::finish_child_run_with_history(
        &f.db,
        &f.b.run,
        "succeeded",
        1,
        0,
        0,
        None,
        Some("{\"findings\":[\"17+29=46\"]}"),
        Some(&serde_json::to_string(&b_history).unwrap()),
    );
    assert_eq!(
        receipt(&f.db, &f.b, question["message_id"].as_str().unwrap()).unwrap()["delivery"],
        "in_context"
    );
    assert_eq!(
        receipt(&f.db, &f.b, answer["message_id"].as_str().unwrap()).unwrap()["delivery"],
        "in_context"
    );
    assert!(claim(&f.db).unwrap().is_empty());

    let context = SandwichContext::new("system".into(), "communication test".into());
    let mut checkpoint = AgentCheckpoint::new(f.root.session.clone(), &context);
    let receipts = [&f.a.run, &f.b.run, &ticket.run]
        .into_iter()
        .map(|id| AppliedReceipt {
            run_id: id.clone(),
            verdict: None,
        })
        .collect::<Vec<_>>();
    save_checkpoint_and_consume(&f.db, &mut checkpoint, &context, &receipts).unwrap();
    checkpoint.mark_done(&context);
    crate::agent::finalization::commit_finalization(
        &f.db,
        &mut checkpoint,
        &f.root.run,
        &f.root.session,
        "communication test",
        &[],
        &json!({"summary":"46"}),
        "46",
        "model",
    )
    .unwrap();
    let conn = f.db.0.lock().unwrap();
    let after: String = conn
        .query_row(
            "SELECT result_json FROM agent_child_runs WHERE id=?1",
            [&f.a.run],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(after, original);
    assert!(!pending_on(&conn, &f.root.run).unwrap());
    let uncollected:i64=conn.query_row("SELECT COUNT(*) FROM agent_child_runs WHERE parent_run_id=?1 AND (status!='succeeded' OR applied_at IS NULL)",[&f.root.run],|r|r.get(0)).unwrap();
    assert_eq!(uncollected, 0);
    assert_eq!(
        conn.query_row(
            "SELECT status FROM agent_run_traces WHERE id=?1",
            [&f.root.run],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "succeeded"
    );
}
