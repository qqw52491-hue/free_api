use super::actions::{execute, Capabilities, Profile, Session};
use crate::{db::DbState, llm::ModelConfig};
use axum::{routing::post, Json, Router};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
};

struct Fixture {
    root: PathBuf,
    db: DbState,
    model: ModelConfig,
    requests: Arc<Mutex<Vec<Value>>>,
    server: tokio::task::JoinHandle<()>,
    interrupt: tokio::sync::watch::Sender<u64>,
}

impl Fixture {
    async fn new(actions: Vec<Value>) -> Self {
        let root =
            std::env::temp_dir().join(format!("free-api-execution-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("original")).unwrap();
        std::fs::create_dir_all(root.join("copy")).unwrap();
        for dir in ["original", "copy"] {
            std::fs::write(root.join(dir).join("data.txt"), "original").unwrap();
        }
        let db = DbState(Arc::new(Mutex::new(
            crate::db::init_db(&root.join("test.db")).unwrap(),
        )));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let actions = Arc::new(Mutex::new(VecDeque::from(actions)));
        let router = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            let actions = actions.clone();
            async move {
                seen.lock().unwrap().push(body);
                let action = actions.lock().unwrap().pop_front().expect("unexpected model call");
                let body = format!("data: {}\n\ndata: [DONE]\n\n", json!({"choices":[{"delta":{"content":action.to_string()},"finish_reason":"stop"}]}));
                axum::response::Response::builder().header("Content-Type", "text/event-stream")
                    .body(axum::body::Body::from(body)).unwrap()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let model = ModelConfig {
            model_id: "shared-execution-test".into(),
            platform_name: "local".into(),
            base_url: format!("http://{address}/v1"),
            api_key: "test-only".into(),
            model_name: "mock".into(),
            max_tokens: 8192,
            temperature: 0.0,
            is_ollama: false,
            is_lmstudio: false,
            context_window: 32768,
            provider_capabilities: "{}".into(),
            timeout_secs: 5,
            requires_alternating_roles: 0,
        };
        let (interrupt, _) = tokio::sync::watch::channel(0);
        Self {
            root,
            db,
            model,
            requests,
            server,
            interrupt,
        }
    }

    fn session(&self, writer: bool, command: &str) -> Session {
        let parent = uuid::Uuid::new_v4().to_string();
        let child = crate::agent::child_reviewer::begin_child_run(
            &self.db,
            &parent,
            &parent,
            &self.root,
            "mock",
            0,
            if writer { "writer" } else { "worker" },
            None,
        )
        .unwrap();
        let capabilities = if writer {
            Capabilities::Writer {
                workspace: self.root.join("copy"),
                scope: String::new(),
            }
        } else {
            Capabilities::Scout {
                workspace: self.root.join("original"),
                scope: String::new(),
            }
        };
        Session::new(
            self.db.clone(),
            None,
            parent,
            child,
            self.model.clone(),
            "auto".into(),
            self.interrupt.subscribe(),
            vec![json!({"role":"system","content":"执行测试任务"})],
            capabilities,
            Profile {
                kind: if writer { "writer" } else { "worker" },
                max_steps: 6,
                timeout_secs: 10,
                call_timeout_secs: 5,
                output_tokens: 1200,
                observation_chars: 40000,
                action_chars: 8000,
                finish_on_budget: !writer,
                test_command: command.into(),
            },
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn finish() -> Value {
    json!({"action":"finish","findings":["已核对"],"confidence":"supported"})
}

#[tokio::test]
async fn shared_engine_preserves_read_only_and_isolated_write_capabilities() {
    let write = json!({"action":"write_file","params":{"path":"data.txt","content":"changed"}});
    let f = Fixture::new(vec![write.clone(), finish(), write, finish()]).await;
    let scout = execute(f.session(false, "")).await;
    assert_eq!(scout.output.status, "succeeded");
    assert!(serde_json::to_string(&scout.messages)
        .unwrap()
        .contains("动作被拒绝"));
    assert_eq!(
        std::fs::read_to_string(f.root.join("original/data.txt")).unwrap(),
        "original"
    );
    let writer = execute(f.session(true, "")).await;
    assert_eq!(writer.output.status, "succeeded");
    assert!(writer.output.test_evidence.is_none());
    assert_eq!(writer.output.changes["data.txt"].content, "original");
    assert_eq!(
        std::fs::read_to_string(f.root.join("copy/data.txt")).unwrap(),
        "changed"
    );
    assert_eq!(
        std::fs::read_to_string(f.root.join("original/data.txt")).unwrap(),
        "original"
    );
    assert_eq!(f.requests.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn explicit_verification_survives_early_finish_and_keeps_real_failure() {
    let f = Fixture::new(vec![
        finish(),
        json!({"action":"run_command","params":{"command":"node --test failing.test.mjs"}}),
        finish(),
    ])
    .await;
    std::fs::write(
        f.root.join("copy/failing.test.mjs"),
        "throw new Error('expected failure');",
    )
    .unwrap();
    let session = execute(f.session(true, "node --test failing.test.mjs")).await;
    let evidence = session.output.test_evidence.unwrap();
    assert_eq!(evidence.0, "node --test failing.test.mjs");
    assert!(
        !evidence.1,
        "a failed process must not be reported as passed"
    );
    let requests = f.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[1]["messages"]
        .to_string()
        .contains("尚未执行本任务指定的测试命令"));
}
