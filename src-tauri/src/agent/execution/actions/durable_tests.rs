use super::*;
use std::sync::{Arc, Mutex};
struct Fixture {
    db: DbState,
    dir: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("durable-agent-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = DbState(Arc::new(Mutex::new(
            crate::db::init_db(&dir.join("test.db")).unwrap(),
        )));
        crate::agent::run_trace::upsert_snapshot(
            &db,
            "parent",
            "session",
            "goal",
            "running",
            &[],
            None,
        )
        .unwrap();
        for id in ["child", "peer"] {
            db.0.lock().unwrap().execute("INSERT INTO agent_child_runs(id,parent_run_id,parent_session_id,workspace_root,model_id,status,started_at,kind) VALUES (?1,'parent','session',?2,'mock','running','now','worker')",rusqlite::params![id,dir.to_string_lossy()]).unwrap();
        }
        Self { db, dir }
    }
    fn session(&self) -> Session {
        let (_tx, rx) = watch::channel(0u64);
        let model = ModelConfig {
            model_id: "mock".into(),
            platform_name: "test".into(),
            base_url: "http://127.0.0.1:1".into(),
            api_key: "test".into(),
            model_name: "mock".into(),
            max_tokens: 1000,
            temperature: 0.0,
            is_ollama: false,
            is_lmstudio: false,
            context_window: 32000,
            provider_capabilities: "{}".into(),
            timeout_secs: 1,
            requires_alternating_roles: 0,
        };
        Session::new(
            self.db.clone(),
            None,
            "session".into(),
            "child".into(),
            model,
            "auto".into(),
            rx,
            vec![json!({"role":"user","content":"original task"})],
            Capabilities::Writer {
                workspace: self.dir.clone(),
                scope: String::new(),
            },
            Profile {
                kind: "worker",
                max_steps: 20,
                timeout_secs: 60,
                call_timeout_secs: 1,
                output_tokens: 1000,
                observation_chars: 40000,
                action_chars: 8000,
                finish_on_budget: true,
                test_command: String::new(),
            },
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
#[tokio::test]
async fn crash_after_message_registration_reuses_the_original_message_without_a_model_call() {
    let f = Fixture::new();
    let mut s = f.session();
    s.output.steps_used = 4;
    let raw =
        json!({"action":"send_message","params":{"target":"peer","message":"once"}}).to_string();
    let parsed = super::super::protocol::parse_action(&raw).unwrap();
    let ControlFlow::Continue(_) = apply_action(s, raw, parsed, Instant::now()).await else {
        panic!("unexpected end")
    };
    // Simulate a crash after send committed, before the observation checkpoint.
    let recovered = f.session();
    assert_eq!(recovered.output.steps_used, 4);
    let ControlFlow::Continue(recovered) = turn(recovered, Instant::now()).await else {
        panic!("replay failed")
    };
    let count: i64 =
        f.db.0
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM agent_messages WHERE sender_run_id='child'",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(count, 1);
    assert!(serde_json::to_string(&recovered.messages)
        .unwrap()
        .contains("duplicate"));
}
#[tokio::test]
async fn uncertain_write_is_retained_without_reexecution_or_model_call() {
    let f = Fixture::new();
    let mut s = f.session();
    s.snapshot.state["pending"] = json!(
        json!({"action":"write_file","params":{"path":"unsafe.txt","content":"bad"}}).to_string()
    );
    s.persist("prepared").unwrap();
    let restored = f.session();
    let finished = execute(restored).await;
    assert_eq!(finished.output.status, "interrupted");
    assert!(!f.dir.join("unsafe.txt").exists());
    assert!(finished.snapshot.state["pending"].is_string());
}
#[tokio::test]
async fn changed_permissions_are_checked_after_loading_the_checkpoint() {
    let f = Fixture::new();
    let mut s = f.session();
    let raw =
        json!({"action":"write_file","params":{"path":"denied.txt","content":"bad"}}).to_string();
    s.persist("ready").unwrap();
    f.db.0.lock().unwrap().execute("UPDATE agent_child_runs SET execution_config_json='{\"allowed_tools\":[]}' WHERE id='child'",[]).unwrap();
    let restored = f.session();
    let parsed = super::super::protocol::parse_action(&raw).unwrap();
    let ControlFlow::Continue(next) = apply_action(restored, raw, parsed, Instant::now()).await
    else {
        panic!("unexpected end")
    };
    assert!(!f.dir.join("denied.txt").exists());
    assert!(serde_json::to_string(&next.messages)
        .unwrap()
        .contains("未允许"));
}
#[tokio::test]
async fn completed_execution_returns_saved_result_without_starting_another_model_call() {
    let f = Fixture::new();
    let mut s = f.session();
    s.output.findings = vec!["saved answer".into()];
    let ControlFlow::Break(_) = s.end("succeeded", None) else {
        unreachable!()
    };
    let recovered = execute(f.session()).await;
    assert_eq!(recovered.output.status, "succeeded");
    assert_eq!(recovered.output.findings, vec!["saved answer"]);
}
