//! 本地假模型回归：验证真实派发、补答、落库路径，不调用外部模型。

use super::*;
use crate::agent::protocol::{ContractFieldDef, ContractFieldType, ContractSchema};
use std::sync::{Arc, Mutex};

pub(crate) struct MockReviewer {
    pub root: std::path::PathBuf,
    pub db: DbState,
    pub requests: Arc<Mutex<Vec<Value>>>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for MockReviewer {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl MockReviewer {
    pub async fn new(responses: Vec<Value>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let app = axum::Router::new().route(
            "/{*path}",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                let index = {
                    let mut requests = captured.lock().unwrap();
                    requests.push(body);
                    requests.len() - 1
                };
                let output = responses[index.min(responses.len() - 1)].to_string();
                async move {
                    let chunk = json!({"choices":[{"delta":{"content":output}}]});
                    (
                        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
                    )
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let root = std::env::temp_dir().join(format!("retired-impact-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = DbState(Arc::new(Mutex::new(
            crate::db::init_db(&root.join("test.db")).unwrap(),
        )));
        {
            let conn = db.0.lock().unwrap();
            conn.execute("INSERT INTO platforms (id,name,base_url,api_key,created_at) VALUES ('mock','mock',?1,'test-only','now')",
                params![format!("http://{address}/v1")]).unwrap();
            conn.execute("INSERT INTO models (id,platform_id,name,display_name,created_at) VALUES ('mock','mock','test-model','mock','now')", []).unwrap();
        }
        Self {
            root,
            db,
            requests,
            server,
        }
    }

    async fn wait_child(&self, id: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let (status, result): (String, Option<String>) = self
                    .db
                    .0
                    .lock()
                    .unwrap()
                    .query_row(
                        "SELECT status,result_json FROM agent_child_runs WHERE id=?1",
                        params![id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .unwrap();
                if status != "running" {
                    assert_eq!(status, "succeeded", "{result:?}");
                    return serde_json::from_str(&result.unwrap()).unwrap();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("local mock reviewer did not finish")
    }
}

fn verdict(blocking: bool) -> Value {
    json!({"findings": if blocking { vec![json!({"id":"F1","severity":"blocking",
        "finality":"blocking_regardless","location":"src/consumer.rs:1", "current":"旧调用",
        "why":"调用方参数与新签名不一致", "suggestion":"同步修复调用方", "evidence_strength":"confirmed"})] } else { vec![] },
        "confidence":"confirmed", "evidence_refs":[], "missing_evidence":[], "cve_refs":[],
        "scorecard":{"correctness":90,"risk":90,"style":90,"test_coverage":null,"null_reason":"无绑定验证"}})
}

fn old_snapshot(scope: bool) -> PlanReviewerDispatchSnapshot {
    let params = json!({"question":"审查当前改动", "evidence_excerpt": "需求和代码 diff",
        "impact_pack_refs":["agent_logs/old.json:10"]});
    let schema = ContractSchema::new(vec![
        ContractFieldDef {
            name: "consumer_verdicts".into(),
            field_type: ContractFieldType::StringArray,
            description: "旧消费点裁决".into(),
            required: true,
            allowed_values: None,
        },
        ContractFieldDef::must_new(
            "cve_refs",
            ContractFieldType::StringArray,
            "保留的其他义务",
            true,
        ),
    ]);
    let mut snapshot = PlanReviewerDispatchSnapshot::new("mock", "flash", "auto", "审查者", params)
        .with_custom_schema(schema);
    if scope {
        snapshot.params["evidence_scope_refs"] = json!(["src/other.rs"]);
    }
    PlanReviewerDispatchSnapshot::from_json_str(&snapshot.to_json_string()).unwrap()
}

#[tokio::test]
async fn old_checkpoint_snapshot_accepts_missing_consumer_verdicts_and_keeps_blockers() {
    for blocking in [false, true] {
        let mock = MockReviewer::new(vec![verdict(blocking)]).await;
        let snapshot = old_snapshot(false);
        let (_tx, rx) = watch::channel(0u64);
        let id = snapshot
            .spawn(
                None,
                &mock.db,
                "parent",
                "retired-impact-test",
                &mock.root,
                rx,
                None,
            )
            .await
            .unwrap();
        let result = mock.wait_child(&id).await;
        let requests = mock.requests.lock().unwrap();
        assert_eq!(requests.len(), 1, "缺少旧字段不能补审");
        assert!(!requests[0]["messages"]
            .to_string()
            .contains("consumer_verdicts"));
        let parsed: ChildReviewResult = serde_json::from_value(result.clone()).unwrap();
        assert_eq!(parsed.unwithdrawn_blocking_count(), u32::from(blocking));
        assert!(!result
            .as_object()
            .unwrap()
            .contains_key("consumer_coverage_incomplete"));
        let stored: String = mock
            .db
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT contract_schema FROM agent_child_runs WHERE id=?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!stored.contains("consumer_verdicts"));
        assert!(stored.contains("cve_refs"));
        assert!(
            snapshot
                .custom_schema
                .fields
                .iter()
                .any(|f| f.name == "consumer_verdicts"),
            "旧快照原件保留"
        );
    }
}

#[tokio::test]
async fn explicit_evidence_scope_still_requires_coverage_after_consumer_retirement() {
    let first = verdict(false);
    let mut second = first.clone();
    second["evidence_scope_verdicts"] = json!(["src/other.rs => NA: 背景材料"]);
    let mock = MockReviewer::new(vec![first, second]).await;
    let snapshot = old_snapshot(true);
    let (_tx, rx) = watch::channel(0u64);
    let id = snapshot
        .spawn(
            None,
            &mock.db,
            "parent",
            "retired-scope-test",
            &mock.root,
            rx,
            None,
        )
        .await
        .unwrap();
    let result = mock.wait_child(&id).await;
    assert_eq!(
        mock.requests.lock().unwrap().len(),
        2,
        "其他范围对账仍须补答"
    );
    assert_eq!(
        result["evidence_scope_verdicts"][0],
        "src/other.rs => NA: 背景材料"
    );
}
