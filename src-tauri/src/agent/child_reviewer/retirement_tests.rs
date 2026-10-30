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
        Self::with_request_hook(responses, || {}).await
    }

    pub async fn with_request_hook(
        responses: Vec<Value>,
        on_request: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self::with_response_barrier(responses, on_request, None, None).await
    }

    /// 所有成员的 HTTP 请求到齐才返回答案，防止串行调用也通过多人并发回归。
    pub(crate) async fn concurrent(responses: Vec<Value>, members: usize) -> Self {
        Self::with_response_barrier(
            responses,
            || {},
            Some(Arc::new(tokio::sync::Barrier::new(members))),
            None,
        )
        .await
    }

    pub(crate) async fn silent() -> Self {
        Self::with_response_barrier(vec![json!({})], || {}, None, Some(Duration::from_secs(3_600))).await
    }

    async fn with_response_barrier(
        responses: Vec<Value>,
        on_request: impl Fn() + Send + Sync + 'static,
        barrier: Option<Arc<tokio::sync::Barrier>>,
        delay: Option<Duration>,
    ) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let on_request = Arc::new(on_request);
        let app = axum::Router::new().route(
            "/{*path}",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                on_request();
                let index = {
                    let mut requests = captured.lock().unwrap();
                    requests.push(body);
                    requests.len() - 1
                };
                let output = responses[index.min(responses.len() - 1)].to_string();
                let barrier = barrier.clone();
                async move {
                    if let Some(delay) = delay { tokio::time::sleep(delay).await; }
                    if let Some(barrier) = barrier {
                        tokio::time::timeout(Duration::from_secs(10), barrier.wait())
                            .await
                            .expect("reviewer requests did not arrive concurrently");
                    }
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
        let (status, result) = self.wait_terminal(id).await;
        assert_eq!(status, "succeeded", "{result:?}");
        result
    }

    pub(crate) async fn wait_terminal(&self, id: &str) -> (String, Value) {
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
                    return (status, serde_json::from_str(&result.unwrap()).unwrap());
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

#[tokio::test]
async fn plan_reviewer_receives_directory_scope_and_reads_the_additional_project() {
    use crate::agent::workspace_access::{self, DirectoryAccess, ProjectDirectory};
    let extra = std::env::temp_dir().join(format!("extra review {}", Uuid::new_v4()));
    std::fs::create_dir_all(&extra).unwrap();
    let extra = extra.canonicalize().unwrap();
    let file = extra.join("same.txt");
    std::fs::write(&file, "TARGET_PROJECT_EVIDENCE").unwrap();
    let mut good = verdict(false);
    good["root_cause_verdict"] = json!("na");
    let mock = MockReviewer::new(vec![json!({"tool":"read_file","path":file}), good]).await;
    std::fs::write(mock.root.join("same.txt"), "SOURCE_PROJECT_OTHER_FILE").unwrap();
    let sid = format!("stage-multi-dir-{}", Uuid::new_v4());
    workspace_access::bind_conversation(&sid, &mock.root, vec![ProjectDirectory {
        path: extra.to_string_lossy().into_owned(), access: DirectoryAccess::ReadOnly,
    }]);
    let refs = crate::agent::reviewer_gatekeeper::resolve_evidence_refs(
        &mock.root, &sid, &[format!("{}:1", file.display())], &[], 3000,
    ).unwrap();
    assert!(refs.contains("TARGET_PROJECT_EVIDENCE") && refs.contains(file.to_str().unwrap()));
    let snapshot = PlanReviewerDispatchSnapshot::new("mock", "flash", "auto", "审查者",
        json!({"question":"核对附加工程的同名文件", "evidence_excerpt":refs, "plan_review":true}));
    let (_tx, rx) = watch::channel(0u64);
    let id = snapshot.spawn(None, &mock.db, "parent", &sid, &mock.root, rx, None).await.unwrap();
    let result = mock.wait_child(&id).await;
    assert_eq!(result["root_cause_verdict"], "na");
    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let first = requests[0]["messages"][1]["content"].as_str().unwrap();
    assert!(first.contains("默认工作区") && first.contains(extra.to_str().unwrap()) && first.contains("只读"));
    let observation = requests[1]["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap();
    assert!(observation.contains("TARGET_PROJECT_EVIDENCE") && observation.contains(file.to_str().unwrap()));
    assert!(!observation.contains("SOURCE_PROJECT_OTHER_FILE"));
    workspace_access::revoke_session(&sid);
    let _ = std::fs::remove_dir_all(extra);
}

#[tokio::test]
async fn wide_references_reach_async_reviewer_and_storage_without_losing_blockers() {
    let refs: Vec<String> = (0..12).map(|i| format!(
        "src/{}/file_{i}.rs:10-20", "nested_module/".repeat(18)
    )).collect();
    let submission = serde_json::from_value(json!({"plan_summary":"核验服务暂停与解包方案的实际证据",
        "target_files":[],"evidence_refs":refs})).unwrap();
    let (_, plan_refs) = crate::agent::plan_gate::validate_plan_submission(&submission, true).unwrap();
    let mut response = verdict(true);
    response["evidence_refs"] = json!(refs);
    response["root_cause_verdict"] = json!("na");
    let mock = MockReviewer::new(vec![response]).await;
    let snapshot = PlanReviewerDispatchSnapshot::new("mock", "flash", "auto", "审查员",
        json!({"question":"核验服务暂停与解包方案", "plan_review":true, "evidence_refs":plan_refs}));
    let (_tx, rx) = watch::channel(0u64);
    let id = snapshot.spawn(None, &mock.db, "wide-ref-run", "wide-ref-session", &mock.root, rx, None).await.unwrap();
    let result = mock.wait_child(&id).await;
    let returned: ChildReviewResult = serde_json::from_value(result).unwrap();
    assert_eq!(returned.unwithdrawn_blocking_count(), 1);
    let mut expected = refs;
    expected.sort();
    assert_eq!(returned.evidence_refs, expected);
    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "合法引用不应触发补答");
    assert!(requests[0]["messages"][0]["content"].as_str().unwrap().contains("本次验收对象：执行前方案"));
    for reference in &expected {
        assert!(requests[0]["messages"].as_array().unwrap().iter().any(|m|
            m["content"].as_str().is_some_and(|content| content.contains(reference))));
    }
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

#[tokio::test]
async fn missing_root_verdict_is_corrected_before_child_delivery() {
    let first = verdict(true);
    let mut second = first.clone();
    second["root_cause_verdict"] = json!("explains");
    let mock = MockReviewer::new(vec![first.clone(), second]).await;
    let snapshot = PlanReviewerDispatchSnapshot::new(
        "mock",
        "flash",
        "auto",
        "审查者",
        json!({"question":"审查授权修改", "evidence_excerpt":"代码差异", "plan_review":true}),
    )
    .with_purpose("plan_review");
    let (_tx, rx) = watch::channel(0u64);
    let id = snapshot
        .spawn(
            None,
            &mock.db,
            "parent",
            "root-correction-test",
            &mock.root,
            rx,
            None,
        )
        .await
        .unwrap();
    let result = mock.wait_child(&id).await;
    assert_eq!(result["root_cause_verdict"], "explains");
    assert_eq!(result["findings"][0]["severity"], "blocking");
    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let messages = requests[1]["messages"].as_array().unwrap();
    assert_eq!(messages[messages.len() - 2]["content"], first.to_string());
    let feedback = messages.last().unwrap()["content"].as_str().unwrap();
    assert!(feedback.contains("root_cause_verdict"));
    assert!(feedback.contains("重新输出完整审查 JSON"));
}

#[tokio::test]
async fn repeated_contract_errors_use_existing_budget_and_keep_raw_diagnostics() {
    let invalid = verdict(true);
    let mock = MockReviewer::new(vec![invalid.clone()]).await;
    let snapshot = PlanReviewerDispatchSnapshot::new(
        "mock",
        "flash",
        "auto",
        "审查者",
        json!({"question":"审查修改", "evidence_excerpt":"代码差异", "plan_review":true}),
    );
    let (_tx, rx) = watch::channel(0u64);
    let id = snapshot
        .spawn(
            None,
            &mock.db,
            "parent-invalid-schema",
            "schema-exhaustion-test",
            &mock.root,
            rx,
            None,
        )
        .await
        .unwrap();
    let (status, result) = mock.wait_terminal(&id).await;
    assert_eq!(status, "invalid_output");
    assert_eq!(
        mock.requests.lock().unwrap().len(),
        20,
        "沿用原有非合规上限"
    );
    assert!(result["validation_error"]
        .as_str()
        .unwrap()
        .contains("root_cause_verdict"));
    assert_eq!(result["raw_result_json"], invalid.to_string());
    assert!(
        result.get("root_cause_verdict").is_none(),
        "宿主不能代填结论"
    );
    let polled = poll_completed_child_runs(&mock.db, "parent-invalid-schema").unwrap();
    assert_eq!(polled.len(), 1);
    assert_eq!(polled[0].status, "invalid_output");
    assert!(polled[0].missing_evidence[0].contains("root_cause_verdict"));
}

#[tokio::test]
async fn custom_contract_type_and_closed_set_errors_are_correctable() {
    for bad in [
        json!({"audit_note":42}),
        json!({"audit_note":"已审", "undeclared":true}),
    ] {
        let mut good = verdict(false);
        good.as_object_mut().unwrap().remove("cve_refs");
        good["audit_note"] = json!("已审");
        let mut invalid = good.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .extend(bad.as_object().unwrap().clone());
        let mock = MockReviewer::new(vec![invalid, good]).await;
        let schema = ContractSchema::new(vec![ContractFieldDef::must_new(
            "audit_note",
            ContractFieldType::String,
            "审查备注",
            true,
        )]);
        let snapshot = PlanReviewerDispatchSnapshot::new(
            "mock",
            "flash",
            "auto",
            "审查者",
            json!({"question":"审查修改", "evidence_excerpt":"代码差异"}),
        )
        .with_custom_schema(schema);
        let (_tx, rx) = watch::channel(0u64);
        let id = snapshot
            .spawn(
                None,
                &mock.db,
                "parent",
                "custom-contract-test",
                &mock.root,
                rx,
                None,
            )
            .await
            .unwrap();
        let result = mock.wait_child(&id).await;
        assert_eq!(result["audit_note"], "已审");
        let requests = mock.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let feedback = requests[1]["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(feedback.contains(if bad.get("undeclared").is_some() {
            "undeclared"
        } else {
            "audit_note"
        }));
    }
}

#[tokio::test]
async fn incident_severity_requires_explicit_correction_and_alias_text_survives() {
    let invalid = json!({"confidence":"supported", "findings":[{
        "id":"F1", "severity":"medium", "file":"src/http.js:10",
        "title":"授权处理错误", "description":"授权失效不应被当作会话失效",
        "recommendation":"分别处理授权状态和登录状态"}]});
    let mut good = invalid.clone();
    good["findings"][0]["severity"] = json!("blocking");
    let mock = MockReviewer::new(vec![invalid, good]).await;
    let snapshot = PlanReviewerDispatchSnapshot::new(
        "mock",
        "flash",
        "auto",
        "审查者",
        json!({"question":"审查修改", "evidence_excerpt":"代码差异"}),
    );
    let (_tx, rx) = watch::channel(0u64);
    let id = snapshot
        .spawn(
            None,
            &mock.db,
            "parent",
            "severity-correction-test",
            &mock.root,
            rx,
            None,
        )
        .await
        .unwrap();
    let result = mock.wait_child(&id).await;
    assert_eq!(result["findings"][0]["severity"], "blocking");
    assert_eq!(result["findings"][0]["location"], "src/http.js:10");
    assert!(result["findings"][0]["why"]
        .as_str()
        .unwrap()
        .contains("授权失效不应被当作会话失效"));
    assert_eq!(
        result["findings"][0]["suggestion"],
        "分别处理授权状态和登录状态"
    );
    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let feedback = requests[1]["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(feedback.contains("findings[0].severity") && feedback.contains("medium"));
}

#[tokio::test]
async fn reviewer_default_forensics_passes_eight_calls_and_keeps_the_safety_cap() {
    for requested in [9usize, 31] {
        let mut good = verdict(false);
        good["root_cause_verdict"] = json!("na");
        let mut responses = vec![json!({"tool":"read_file", "path":"evidence.txt"}); requested];
        responses.push(good);
        let mock = MockReviewer::new(responses).await;
        std::fs::write(mock.root.join("evidence.txt"), "FORENSIC_BUDGET_EVIDENCE").unwrap();
        let snapshot = PlanReviewerDispatchSnapshot::new("mock", "flash", "auto", "审查者",
            json!({"question":"核对合成证据", "evidence_excerpt":"synthetic", "plan_review":true}));
        let (_tx, rx) = watch::channel(0u64);
        let id = snapshot.spawn(None, &mock.db, "parent", "forensic-budget", &mock.root, rx, None).await.unwrap();
        let result = mock.wait_child(&id).await;
        assert_eq!(result["root_cause_verdict"], "na");
        let requests = mock.requests.lock().unwrap();
        assert_eq!(requests.len(), requested + 1);
        let messages = requests.last().unwrap()["messages"].as_array().unwrap();
        let observations = messages.iter().filter_map(|m| m["content"].as_str())
            .filter(|s| s.starts_with("【取证结果 step ") && s.contains("FORENSIC_BUDGET_EVIDENCE")).count();
        assert_eq!(observations, requested.min(30), "有效取证应越过旧 8 次上限，并在 30 次收口");
        let last = messages.last().unwrap()["content"].as_str().unwrap();
        assert_eq!(last.contains("取证预算已用尽"), requested > 30);
    }
}

#[tokio::test]
async fn contract_correction_can_still_use_tools_and_explicit_na() {
    let first = verdict(false);
    let mut good = first.clone();
    good["root_cause_verdict"] = json!("na");
    let mock = MockReviewer::new(vec![
        first,
        json!({"tool":"read_file", "path":"evidence.txt"}),
        good,
    ])
    .await;
    std::fs::write(mock.root.join("evidence.txt"), "synthetic review evidence").unwrap();
    let snapshot = PlanReviewerDispatchSnapshot::new(
        "mock",
        "flash",
        "auto",
        "审查者",
        json!({"question":"审查修改", "evidence_excerpt":"代码差异", "plan_review":true}),
    );
    let (_tx, rx) = watch::channel(0u64);
    let id = snapshot
        .spawn(
            None,
            &mock.db,
            "parent",
            "schema-tools-test",
            &mock.root,
            rx,
            None,
        )
        .await
        .unwrap();
    let result = mock.wait_child(&id).await;
    assert_eq!(result["root_cause_verdict"], "na");
    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    let observation = requests[2]["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(observation.contains("取证结果 step 1/"));
    assert!(observation.contains("synthetic review evidence"));
}

#[tokio::test]
async fn current_review_accepts_no_scorecard_without_a_format_retry() {
    let mock = MockReviewer::new(vec![json!({
        "findings":[],"confidence":"confirmed","evidence_refs":[],"missing_evidence":[]
    })]).await;
    let snapshot = PlanReviewerDispatchSnapshot::new("mock", "flash", "auto", "基础审查员",
        json!({"question":"核验只读报告中的结论", "evidence_excerpt":"当前仅能确认网络吞吐上界，不宣称已经测得真实磁盘写速"}));
    let (_tx, rx) = watch::channel(0u64);
    let id = snapshot.spawn(None, &mock.db, "no-score-run", "no-score-session", &mock.root, rx, None).await.unwrap();
    let result = mock.wait_child(&id).await;
    assert!(result.get("scorecard").is_none_or(Value::is_null));
    assert_eq!(mock.requests.lock().unwrap().len(), 1, "缺评分不应制造格式补答");
    let requests = mock.requests.lock().unwrap();
    let system = requests[0]["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("普通 question 只作参考，不自动成为证明义务"));
    assert!(!system.contains("综合均分>=75"));
    assert!(!system.contains("findings 为 1–5"));
}

#[test]
fn old_review_resume_updates_host_policy_but_preserves_original_judgments_and_evidence() {
    let evidence = json!({"role":"user","content":"证据原文：代码注释中也可能出现 scorecard 或 QUESTION"});
    let result = json!({"role":"assistant","content":"{\"scorecard\":{\"correctness\":58},\"findings\":[{\"severity\":\"blocking\"}]}"});
    let old = vec![json!({"role":"system","content":
        "宿主规则\n0. 【核心强制】必须在顶级 JSON 中完整输出 scorecard 四维评分卡，综合均分>=75方可放行；\n1. findings 为 1–5 条结构化审查条目；若无确切位置 location，severity 必须标为 question\n病根核对、撤回裁决与证据范围仍须核验"}), evidence.clone(), result.clone()];
    let mut resumed = old.clone();
    refresh_review_instructions(&mut resumed);
    assert!(resumed[0]["content"].as_str().unwrap().contains("不作为放行条件"));
    assert!(resumed[0]["content"].as_str().unwrap().contains("病根核对、撤回裁决与证据范围仍须核验"));
    assert!(!resumed[0]["content"].as_str().unwrap().contains("综合均分>=75"));
    assert_eq!(resumed[1], evidence);
    assert_eq!(resumed[2], result);
    assert!(old[0]["content"].as_str().unwrap().contains("综合均分>=75"));
    let once = resumed.clone();
    refresh_review_instructions(&mut resumed);
    assert_eq!(resumed, once);
}
