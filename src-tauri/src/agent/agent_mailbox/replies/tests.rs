use super::*;
use crate::agent::{
    checkpoint::{save_checkpoint_and_consume, AgentCheckpoint, AppliedReceipt},
    context::SandwichContext,
};
use std::sync::{Arc, Barrier, Mutex};

struct Fixture {
    path: PathBuf,
    db: DbState,
    root: Actor,
    b: Actor,
    c: Actor,
}
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("free-api-readonly-reply-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("data.txt"), "original").unwrap();
        let db = DbState(Arc::new(Mutex::new(
            crate::db::init_db(&path.join("test.db")).unwrap(),
        )));
        let root = Actor::new(
            &uuid::Uuid::new_v4().to_string(),
            &uuid::Uuid::new_v4().to_string(),
        );
        db.0.lock().unwrap().execute("INSERT INTO chat_sessions(id,title,model_id,created_at,updated_at,message_count) VALUES (?1,'test','model','now','now',0)",[&root.session]).unwrap();
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
        let b = child();
        let c = child();
        db.0.lock().unwrap().execute("UPDATE agent_child_runs SET status='succeeded',result_json=?2,applied_at='old' WHERE id=?1",params![b.run,json!({"findings":["old-answer"],"confidence":"supported"}).to_string()]).unwrap();
        Self {
            path,
            db,
            root,
            b,
            c,
        }
    }
    fn question(&self) -> Value {
        json!({"target":self.b.run,"message":"为什么保留旧接口？","data":{"test_id":"reply-test"}})
    }
    fn commit(&self) -> Result<(), String> {
        let context = SandwichContext::new("system".into(), "goal".into());
        let mut checkpoint =
            crate::agent::checkpoint::load_checkpoint(&self.db, &self.root.session)?
                .unwrap_or_else(|| AgentCheckpoint::new(self.root.session.clone(), &context));
        checkpoint.mark_done(&context);
        crate::agent::finalization::commit_finalization(
            &self.db,
            &mut checkpoint,
            &self.root.run,
            &self.root.session,
            "goal",
            &[],
            &json!({"summary":"done"}),
            "done",
            "model",
        )
    }
    fn apply(&self, run: &str) {
        let result = read_result(&self.db, &self.root, &json!({"child_run_id":run})).unwrap();
        let mut context = SandwichContext::new("system".into(), "goal".into());
        context.add_host_feedback(&result.to_string());
        let mut checkpoint = AgentCheckpoint::new(self.root.session.clone(), &context);
        save_checkpoint_and_consume(
            &self.db,
            &mut checkpoint,
            &context,
            &[AppliedReceipt {
                run_id: run.into(),
                verdict: None,
            }],
        )
        .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
fn output() -> crate::agent::execution::actions::Output {
    crate::agent::execution::actions::Output {
        status: "succeeded".into(),
        findings: vec!["因为旧客户端仍依赖此接口".into()],
        confidence: "supported".into(),
        ..Default::default()
    }
}

#[test]
fn admission_completion_and_receipt_consumption_share_one_authoritative_ledger() {
    let f = Fixture::new();
    db_status(&f.db, &f.b.run, "running");
    let historical = super::super::send(
        &f.db,
        &f.c,
        &json!({"target":f.b.run,"message":"old-context"}),
        None,
        false,
    )
    .unwrap();
    db_status(&f.db, &f.b.run, "succeeded");
    let (body, ticket) = register(&f.db, &f.c, &f.question(), "question-1").unwrap();
    let ticket = ticket.unwrap();
    let reader = Actor::new(&ticket.run, &format!("child:{}", ticket.run));
    let old =
        super::super::receipt(&f.db, &reader, historical["message_id"].as_str().unwrap()).unwrap();
    assert_eq!(old["payload"]["message"], "old-context");
    assert_eq!(body["execution_status"], "queued");
    let team = family_list(&f.db, &f.root).unwrap();
    let reply = team
        .iter()
        .find(|row| row["child_run_id"] == ticket.run)
        .unwrap();
    assert_eq!(reply["purpose"], "readonly_reply");
    assert_eq!(reply["resumed_from"], f.b.run);
    assert!(crate::agent::child_reviewer::has_running_child_runs(
        &f.db,
        &f.root.run
    ));
    assert_eq!(f.commit().unwrap_err(), PENDING_REPLY);
    assert!(root_active(&f.db, &f.root.run));
    finish(&f.db, &ticket, output(), vec![], 12).unwrap();
    assert_eq!(
        f.commit().unwrap_err(),
        PENDING_REPLY,
        "a result not yet consumed must still hold completion"
    );
    f.apply(&ticket.run);
    f.commit().unwrap();
    let duplicate = register(&f.db, &f.c, &f.question(), "question-1").unwrap();
    assert!(duplicate.1.is_none());
    assert_eq!(duplicate.0["reply_run_id"], ticket.run);
    assert!(register(&f.db, &f.c, &f.question(), "late-request")
        .unwrap_err()
        .contains("已结束"));
    let inbox = pending(&f.db, &f.c).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0]["payload"]["data"]["read_only_reply"], true);
    let original = read_result(&f.db, &f.c, &json!({"child_run_id":f.b.run})).unwrap();
    assert!(original["text"].as_str().unwrap().contains("old-answer"));
    finish(&f.db, &ticket, output(), vec![], 99).unwrap();
    assert_eq!(
        pending(&f.db, &f.c).unwrap().len(),
        1,
        "late duplicate completion must not emit another answer"
    );
}

#[test]
fn actual_concurrent_admission_and_terminal_commit_cannot_both_win() {
    let f = Fixture::new();
    let gate = Arc::new(Barrier::new(3));
    let (admission, completion) = std::thread::scope(|scope| {
        let g = gate.clone();
        let fixture = &f;
        let first = scope.spawn(move || {
            g.wait();
            register(&fixture.db, &fixture.root, &fixture.question(), "race")
        });
        let g = gate.clone();
        let fixture = &f;
        let second = scope.spawn(move || {
            g.wait();
            fixture.commit()
        });
        gate.wait();
        (first.join().unwrap(), second.join().unwrap())
    });
    match (admission, completion) {
        (Ok((_, Some(_))), Err(error)) => assert_eq!(error, PENDING_REPLY),
        (Err(error), Ok(())) => assert!(error.contains("主任务已结束")),
        other => panic!(
            "admission and closure disagreed: {:?}",
            (other.0.is_ok(), other.1)
        ),
    }
}

#[test]
fn cross_task_cancelled_targets_stale_senders_and_retry_conflicts_are_not_admitted() {
    let f = Fixture::new();
    let outsider = crate::agent::child_reviewer::begin_child_run(
        &f.db,
        "other-root",
        "other-session",
        &f.path,
        "model",
        0,
        "worker",
        None,
    )
    .unwrap();
    assert!(register(
        &f.db,
        &f.c,
        &json!({"target":outsider,"message":"question"}),
        "cross"
    )
    .is_err());
    db_status(&f.db, &f.b.run, "cancelled");
    assert!(register(&f.db, &f.c, &f.question(), "cancelled").is_err());
    db_status(&f.db, &f.b.run, "succeeded");
    db_status(&f.db, &f.c.run, "succeeded");
    assert!(register(&f.db, &f.c, &f.question(), "stale").is_err());
    db_status(&f.db, &f.c.run, "running");
    register(&f.db, &f.c, &f.question(), "key").unwrap();
    let mut changed = f.question();
    changed["message"] = json!("different");
    assert!(register(&f.db, &f.c, &changed, "key")
        .unwrap_err()
        .contains("重试键"));
}
fn db_status(db: &DbState, id: &str, status: &str) {
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE agent_child_runs SET status=?2 WHERE id=?1",
            params![id, status],
        )
        .unwrap();
}

#[tokio::test]
async fn peer_question_runs_read_only_answers_once_and_retains_original_delivery() {
    use axum::{routing::post, Json, Router};
    let f = Fixture::new();
    let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = seen.clone();
    let router=Router::new().route("/v1/chat/completions",post(move |Json(body):Json<Value>|{let seen=captured.clone();async move{
        let mut requests=seen.lock().unwrap();requests.push(body);
        let action=match requests.len(){
            1=>json!({"action":"write_file","params":{"path":"data.txt","content":"bad"}}),
            2=>json!({"action":"run_command","params":{"command":"node --eval process.exit()"}}),
            3=>json!({"action":"ask_agent","params":{"target":"parent","message":"spawn"}}),
            _=>json!({"action":"finish","findings":["旧客户端依赖旧接口，建议修改交原负责人。"],"confidence":"supported"})
        };
        axum::response::Response::builder().header("Content-Type","text/event-stream").body(axum::body::Body::from(format!("data: {}\n\ndata: [DONE]\n\n",json!({"choices":[{"delta":{"content":action.to_string()},"finish_reason":"stop"}]})))).unwrap()
    }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    {
        let conn = f.db.0.lock().unwrap();
        conn.execute("INSERT INTO platforms(id,name,base_url,api_key,created_at,requires_alternating_roles) VALUES ('local','local',?1,'test','now',0)",[format!("http://{address}/v1")]).unwrap();
        conn.execute("INSERT INTO models(id,platform_id,name,display_name,created_at) VALUES ('model','local','mock','mock','now')",[]).unwrap();
    }
    let (interrupt, _) = tokio::sync::watch::channel(0u64);
    crate::agent::control::stream_interrupt_channels()
        .lock()
        .await
        .insert(f.root.session.clone(), interrupt);
    let mut changes = crate::agent::job_engine::job_watch_receiver();
    let accepted = ask(&f.db, &f.c, &f.question(), "live", None).unwrap();
    let run = accepted["reply_run_id"].as_str().unwrap().to_string();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status: String =
                f.db.0
                    .lock()
                    .unwrap()
                    .query_row(
                        "SELECT status FROM agent_child_runs WHERE id=?1",
                        [&run],
                        |r| r.get(0),
                    )
                    .unwrap();
            if !matches!(status.as_str(), "queued" | "running") {
                assert_eq!(status, "succeeded");
                break;
            }
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    crate::agent::control::stream_interrupt_channels()
        .lock()
        .await
        .remove(&f.root.session);
    server.abort();
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[0]["messages"].to_string().contains("reply-test"));
    assert!(requests[1]["messages"]
        .to_string()
        .contains("本次动作未执行"));
    assert!(requests[2]["messages"]
        .to_string()
        .contains("本次动作未执行"));
    drop(requests);
    assert_eq!(
        std::fs::read_to_string(f.path.join("data.txt")).unwrap(),
        "original"
    );
    let inbox = pending(&f.db, &f.c).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0]["payload"]["data"]["reply_to"], f.b.run);
    let receipt =
        super::super::receipt(&f.db, &f.c, accepted["request_id"].as_str().unwrap()).unwrap();
    assert_eq!(receipt["delivery"], "in_context");
    let previous = read_result(&f.db, &f.c, &json!({"child_run_id":f.b.run})).unwrap();
    assert!(previous["text"].as_str().unwrap().contains("old-answer"));
}

#[test]
fn failed_answer_persistence_keeps_work_pending_and_never_half_delivers() {
    let f = Fixture::new();
    let (_, ticket) = register(&f.db, &f.c, &f.question(), "persist").unwrap();
    let ticket = ticket.unwrap();
    f.db.0.lock().unwrap().execute_batch("CREATE TRIGGER reject_answer BEFORE INSERT ON agent_messages WHEN NEW.id LIKE '%:answer' BEGIN SELECT RAISE(ABORT,'test persistence failure'); END;").unwrap();
    assert!(finish(&f.db, &ticket, output(), vec![], 1).is_err());
    let status: String =
        f.db.0
            .lock()
            .unwrap()
            .query_row(
                "SELECT status FROM agent_child_runs WHERE id=?1",
                [&ticket.run],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(status, "queued");
    assert!(pending(&f.db, &f.c).unwrap().is_empty());
    assert_eq!(f.commit().unwrap_err(), PENDING_REPLY);
    f.db.0
        .lock()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_answer;")
        .unwrap();
    finish(&f.db, &ticket, output(), vec![], 2).unwrap();
    assert_eq!(pending(&f.db, &f.c).unwrap().len(), 1);
}

#[tokio::test]
async fn admitted_start_failure_is_reported_and_does_not_leave_a_running_attempt() {
    let f = Fixture::new();
    let mut changes = crate::agent::job_engine::job_watch_receiver();
    // No model row or live root controller: this exercises the real setup failure path.
    let accepted = ask(&f.db, &f.c, &f.question(), "unavailable", None).unwrap();
    let id = accepted["reply_run_id"].as_str().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let status: String =
                f.db.0
                    .lock()
                    .unwrap()
                    .query_row(
                        "SELECT status FROM agent_child_runs WHERE id=?1",
                        [id],
                        |r| r.get(0),
                    )
                    .unwrap();
            if !matches!(status.as_str(), "queued" | "running") {
                assert_eq!(status, "failed");
                break;
            }
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(pending(&f.db, &f.c).unwrap()[0]["payload"]["data"]["error_kind"].is_string());
    assert_eq!(f.commit().unwrap_err(), PENDING_REPLY);
    f.apply(id);
    f.commit().unwrap();
}

#[test]
fn generic_task_history_reads_existing_checkpoint_with_exact_source_and_no_control_state() {
    let f = Fixture::new();
    db_status(&f.db, &f.b.run, "succeeded");
    f.db.0
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_child_runs SET kind='task' WHERE id=?1",
            [&f.b.run],
        )
        .unwrap();
    let mut context = SandwichContext::new("system".into(), "original-goal".into());
    context.add_host_feedback("prior-observation");
    let mut checkpoint = AgentCheckpoint::new(format!("task-{}", f.b.run), &context);
    crate::agent::checkpoint::save_checkpoint(&f.db, &mut checkpoint).unwrap();
    let view = read_result(
        &f.db,
        &f.c,
        &json!({"child_run_id":f.b.run,"view":"history"}),
    )
    .unwrap();
    assert!(view["source_field"]
        .as_str()
        .unwrap()
        .starts_with("agent_checkpoints.data"));
    assert!(view["text"].as_str().unwrap().contains("original-goal"));
    assert!(view["text"].as_str().unwrap().contains("prior-observation"));
    assert!(!view["text"]
        .as_str()
        .unwrap()
        .contains("pending_finalization"));
    assert!(read_result(&f.db, &f.c, &json!({"child_run_id":f.b.run,"offset":-1})).is_err());
}
