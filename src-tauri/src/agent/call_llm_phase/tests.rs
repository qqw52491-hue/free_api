//! 经正式模型请求与工具分发入口核对补正结果，不调用外部模型。
use super::*;
use crate::agent::{
    context::{ChatMessage, SandwichContext},
    review_team::tests::TestDb,
};
use std::sync::{Arc, Mutex};

struct Fixture {
    data: TestDb,
    requests: Arc<Mutex<Vec<Value>>>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Fixture {
    async fn new(responses: Vec<Value>) -> Self {
        let data = TestDb::new();
        std::fs::write(data.root.join("evidence.txt"), "existing evidence").unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let server = axum::Router::new().route(
            "/{*path}",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                let index = {
                    let mut requests = captured.lock().unwrap();
                    requests.push(body);
                    requests.len() - 1
                };
                let chunk = responses[index.min(responses.len() - 1)].clone();
                async move {
                    if chunk["fixture_delay"].as_bool().unwrap_or(false) {
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    }
                    (
                        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
                    )
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });
        {
            let conn = data.db.0.lock().unwrap();
            conn.execute("INSERT INTO platforms (id,name,base_url,api_key,created_at) VALUES ('mock','mock',?1,'fixture','now')",
                rusqlite::params![format!("http://{address}/v1")]).unwrap();
            conn.execute("INSERT INTO models (id,platform_id,name,display_name,max_tokens,created_at) VALUES ('mock','mock','fixture','mock',8192,'now')", []).unwrap();
        }
        Self {
            data,
            requests,
            server,
        }
    }

    async fn run(
        &self,
    ) -> Result<
        (
            AgentInstruction,
            crate::agent::types::TokenUsage,
            String,
            Option<usize>,
        ),
        crate::agent::types::StepExecutionError,
    > {
        let (_interrupt, mut interrupt) = watch::channel(0);
        self.run_with_interrupt(&mut interrupt).await
    }

    async fn run_with_interrupt(
        &self,
        interrupt: &mut watch::Receiver<u64>,
    ) -> Result<
        (
            AgentInstruction,
            crate::agent::types::TokenUsage,
            String,
            Option<usize>,
        ),
        crate::agent::types::StepExecutionError,
    > {
        let app = tauri::test::mock_app();
        app.manage(self.data.db.clone());
        let session = uuid::Uuid::new_v4().to_string();
        let mut context = SandwichContext::new("fixture system".into(), "只读核对已有证据".into());
        context.turns_history.push(ChatMessage {
            role: "user".into(),
            content: json!("已完成的观察：evidence.txt 存在，后续核对不得遗失这条历史。"),
        });
        call_llm(
            &context,
            &app.state::<DbState>(),
            "mock".into(),
            "flash",
            None,
            1,
            &session,
            false,
            0,
            "auto",
            false,
            interrupt,
        )
        .await
    }
}

fn response(content: &str, finish: Option<&str>) -> Value {
    json!({"choices":[{"delta":{"content":content},"finish_reason":finish}],
        "usage":{"prompt_tokens":100,"completion_tokens":10,"total_tokens":110}})
}

fn read(path: &str) -> String {
    json!({"description":"核对夹具证据", "tool":"local_file",
        "command":{"action":"read","path":path}})
    .to_string()
}

#[tokio::test]
async fn generation_exhaustion_does_not_restart_the_inner_budget() {
    let reasoning_only = json!({"choices":[{"delta":{"reasoning_content":"fixture reasoning"},"finish_reason":"stop"}]} );
    for chunk in [
        response(&read("evidence.txt"), Some("length")),
        reasoning_only,
        response("{", Some("stop")),
    ] {
        let f = Fixture::new(vec![chunk]).await;
        let error = f.run().await.err().expect("不完整响应不得成为指令");
        assert!(
            error.instruction_recovery_exhausted(),
            "外层需要明确收到内层恢复已耗尽的事实：{error}"
        );
        assert_eq!(
            f.requests.lock().unwrap().len(),
            2,
            "内层初试和补正用完后，外层不得重新开启同一恢复预算"
        );
        let conn = f.data.db.0.lock().unwrap();
        let calls: Vec<(i64, String)> = conn
            .prepare("SELECT request_try,outcome FROM agent_llm_calls ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls.iter().map(|r| r.0).collect::<Vec<_>>(), vec![1, 2]);
        assert!(calls.iter().all(|r| r.1 != "instruction_ready"));
    }
}

#[tokio::test]
async fn generation_correction_preserves_history_and_dispatches_the_complete_instruction() {
    let f = Fixture::new(vec![
        response("{", Some("stop")),
        response(&read("evidence.txt"), Some("stop")),
    ])
    .await;
    let (instruction, _, _, _) = f.run().await.unwrap();
    let result = run_agent_step(
        &f.data.db,
        "fixture",
        &instruction,
        &PluginRegistry::new(),
        Some(&f.data.root),
    );
    assert!(result.success, "{}", result.stderr);
    assert!(result.stdout.contains("existing evidence"));
    let requests = f.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests.iter() {
        assert_eq!(request["max_tokens"], 8192);
        assert!(request["messages"].to_string().contains("已完成的观察"));
    }
    assert!(requests[1]["messages"]
        .to_string()
        .contains("未通过协议校验"));
}

#[tokio::test]
async fn interrupt_remains_distinct_from_exhausted_generation_and_is_not_retried() {
    let f = Fixture::new(vec![json!({"fixture_delay":true})]).await;
    let (sender, mut interrupt) = watch::channel(0);
    let stop = async {
        while f.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        sender.send_modify(|generation| *generation += 1);
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::join!(f.run_with_interrupt(&mut interrupt), stop)
    })
    .await
    .unwrap();
    let error = result.err().unwrap();
    assert_eq!(error.message(), crate::llm::STREAM_INTERRUPTED_ERROR);
    assert!(!error.instruction_recovery_exhausted());
    assert_eq!(f.requests.lock().unwrap().len(), 1);
}
