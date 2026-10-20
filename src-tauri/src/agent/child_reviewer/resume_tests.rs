//! 续接审查真实流请求、补正、落库和回执回归；只调用本机假模型。
use super::retirement_tests::MockReviewer;
use super::*;
use crate::agent::protocol::{ContractFieldDef, ContractFieldType, ContractSchema};

fn schema() -> ContractSchema {
    ContractSchema::new(vec![ContractFieldDef {
        name: "root_cause_verdict".into(),
        field_type: ContractFieldType::String,
        description: "病根核对".into(),
        required: true,
        allowed_values: None,
    }])
    .with_strict_closed_set(false)
}

fn response() -> Value {
    json!({"findings":[],"confidence":"supported","evidence_refs":[],
        "missing_evidence":[],"root_cause_verdict":"explains","decision":"approved",
        "revised_plan_summary":"新模型列表作为唯一渲染源，失败回退原列表",
        "objection_verdicts":[{"finding_id":"F1","verdict":"withdraw",
            "reason":"修订方案已将列表渲染和搜索共同切换为拉取结果，原阻塞不再成立"}],
        "evidence_scope_verdicts":["src/model.vue => HOLDS"],
        "custom_result":{"checked":"list display"}})
}

async fn start(mock: &MockReviewer, schema: ContractSchema, rx: watch::Receiver<u64>) -> String {
    let identity = ChildRunIdentity {
        purpose: Some("plan_review".into()),
        contract_schema: Some(serde_json::to_string(&schema).unwrap()),
        ..ChildRunIdentity::fresh("review")
    };
    let id = begin_child_run_extended(
        &mock.db,
        "resume-run",
        "resume-session",
        &mock.root,
        "mock",
        0,
        "review",
        None,
        None,
        Some("previous-review"),
        &identity,
    )
    .unwrap();
    let config = ModelConfig::from_db_state(&mock.db, "mock").unwrap();
    let messages = vec![
        json!({"role":"system","content":"输出完整审查 JSON"}),
        json!({"role":"user","content":format!("核对父 Agent 修订方案。\n{}",schema.render_prompt_instructions())}),
    ];
    tokio::spawn(run_resumed_review(
        DbState(mock.db.0.clone()),
        id.clone(),
        "resume-session".into(),
        config,
        "auto".into(),
        messages,
        Some(schema),
        rx,
    ));
    id
}

#[tokio::test]
async fn resumed_review_preserves_full_contract_through_poll_and_history() {
    let raw = response();
    let mock = MockReviewer::new(vec![raw.clone()]).await;
    let (_tx, rx) = watch::channel(0);
    let id = start(&mock, schema(), rx).await;
    let (status, result) = mock.wait_terminal(&id).await;
    assert_eq!(status, "succeeded");
    for key in [
        "root_cause_verdict",
        "decision",
        "revised_plan_summary",
        "evidence_scope_verdicts",
        "custom_result",
    ] {
        assert_eq!(result[key], raw[key], "丢失扩展字段 {key}");
    }
    let receipts = poll_completed_delivery_receipts(&mock.db, "resume-run", None).unwrap();
    assert_eq!(receipts.len(), 1);
    receipts[0].validate_against_schema(&schema()).unwrap();
    assert_eq!(
        receipts[0].payload.as_ref().unwrap()["decision"],
        "approved"
    );
    assert_eq!(
        receipts[0]
            .as_reviewer_delivery()
            .unwrap()
            .withdrawn_findings,
        vec!["F1"]
    );
    let history: String = mock
        .db
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT message_history_json FROM agent_child_runs WHERE id=?1",
            [&id],
            |r| r.get(0),
        )
        .unwrap();
    let history: Vec<Value> = serde_json::from_str(&history).unwrap();
    let last = history.last().unwrap();
    assert_eq!(last["role"], "assistant");
    assert_eq!(
        serde_json::from_str::<Value>(last["content"].as_str().unwrap()).unwrap(),
        raw
    );
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn resumed_review_repairs_missing_and_wrong_type_in_same_context() {
    let good = response();
    let mut missing = good.clone();
    missing
        .as_object_mut()
        .unwrap()
        .remove("root_cause_verdict");
    let mut wrong = good.clone();
    wrong["root_cause_verdict"] = json!(["explains"]);
    let mock = MockReviewer::new(vec![missing, wrong, good]).await;
    let (_tx, rx) = watch::channel(0);
    let id = start(&mock, schema(), rx).await;
    let (status, result) = mock.wait_terminal(&id).await;
    assert_eq!(status, "succeeded");
    assert_eq!(result["root_cause_verdict"], "explains");
    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    for request in &requests[1..] {
        let text = request["messages"].to_string();
        assert!(text.contains("root_cause_verdict"));
        assert!(text.contains("不代表方案被驳回"));
        assert!(text.contains("核对父 Agent 修订方案"));
    }
}

#[tokio::test]
async fn resumed_review_exhaustion_keeps_diagnostics_without_fabricating_approval() {
    let mut missing = response();
    missing
        .as_object_mut()
        .unwrap()
        .remove("root_cause_verdict");
    let mock = MockReviewer::new(vec![missing.clone()]).await;
    let (_tx, rx) = watch::channel(0);
    let id = start(&mock, schema(), rx).await;
    let (status, result) = mock.wait_terminal(&id).await;
    assert_eq!(status, "invalid_output");
    assert_eq!(
        mock.requests.lock().unwrap().len(),
        MAX_REVIEW_OUTPUT_ATTEMPTS as usize
    );
    let result: ChildReviewResult = serde_json::from_value(result).unwrap();
    assert_eq!(result.error_kind.as_deref(), Some("invalid_output"));
    let receipts = poll_completed_delivery_receipts(&mock.db, "resume-run", None).unwrap();
    let receipt = &receipts[0];
    assert!(!receipt.is_success());
    assert!(receipt
        .missing_evidence()
        .join(" ")
        .contains("root_cause_verdict"));
    let saved: String = mock
        .db
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT message_history_json FROM agent_child_runs WHERE id=?1",
            [&id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(saved.contains("root_cause_verdict"));

    // 真实回执消费者和主运行失败落账：不输出内部 JSON、不解除未完成事项。
    let mut context = crate::agent::context::SandwichContext::new("sys".into(), "刷新列表".into());
    context.todo_list.push(crate::agent::types::TodoItem {
        id: 1,
        status: "pending".into(),
        description: "验证列表展示".into(),
        evidence_output_id: None,
    });
    let mut checkpoint =
        crate::agent::checkpoint::AgentCheckpoint::new("resume-session".into(), &context);
    checkpoint.stage_review_blockers_open = 1;
    let (_tx, rx) = watch::channel(0);
    let env = crate::agent::child_receipt::ReceiptEnv {
        app: None,
        db: &mock.db,
        parent_run_id: "resume-run",
        session_id: "resume-session",
        workspace_dir: &mock.root,
        interrupt_rx: rx,
        reviewer_backup_model: "",
    };
    let effects = crate::agent::child_receipt::reduce_child_receipt(
        &env,
        &mut checkpoint,
        &mut context,
        receipt,
    )
    .await;
    let reason = checkpoint.pending_task_failure.clone().unwrap();
    assert!(reason.contains("审查未完成"));
    assert!(reason.contains("无法完成交付"));
    assert!(reason.contains(&id));
    assert!(reason.chars().count() < 300);
    for text in [&reason, &effects.model_text] {
        assert!(!text.contains("\"findings\""));
        assert!(!text.contains("\"decision\""));
        assert!(!text.contains("诊断：{"));
    }
    mock.db.0.lock().unwrap().execute("INSERT INTO chat_sessions (id,title,model_id,created_at,updated_at) VALUES ('resume-session','test','','now','now')",[]).unwrap();
    let report = crate::agent::failure::persist_failed_run(
        &mock.db,
        "resume-session",
        "resume-run",
        "刷新列表",
        &[],
        &mut checkpoint,
        &context,
        &reason,
    )
    .unwrap();
    assert!(report.contains("任务未完成"));
    assert!(!report.contains("\"findings\""));
    assert_eq!(checkpoint.stage_review_blockers_open, 1);
    assert_eq!(context.todo_list[0].status, "pending");
    let (state, completion): (String, String) = mock
        .db
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT status,completion_json FROM agent_run_traces WHERE id='resume-run'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "failed");
    assert_eq!(
        serde_json::from_str::<Value>(&completion).unwrap()["summary"],
        report
    );
}

#[tokio::test]
async fn resumed_review_keeps_real_blocker_and_host_metadata() {
    let mut raw = response();
    raw["findings"] = json!([{"id":"F2","severity":"blocking","finality":"regardless",
        "location":"src/model.vue:1","current":"仍显示旧列表","why":"拉取结果没有成为实际列表数据源",
        "suggestion":"切换渲染源","evidence_strength":"supported"}]);
    raw["status"] = json!("cancelled");
    raw["child_run_id"] = json!("spoofed");
    raw["decision"] = json!("rebuttal");
    let mock = MockReviewer::new(vec![raw]).await;
    let (_tx, rx) = watch::channel(0);
    let id = start(&mock, schema(), rx).await;
    let (status, result) = mock.wait_terminal(&id).await;
    assert_eq!(status, "succeeded", "完成审查不等于方案通过");
    assert_eq!(result["child_run_id"], id);
    assert_eq!(result["status"], "succeeded");
    assert_eq!(result["decision"], "rebuttal");
    let result: ChildReviewResult = serde_json::from_value(result).unwrap();
    assert_eq!(result.unwithdrawn_blocking_count(), 1);
}

#[tokio::test]
async fn resumed_review_cancel_during_correction_does_not_approve_or_reissue() {
    let mut raw = response();
    raw.as_object_mut().unwrap().remove("root_cause_verdict");
    let (tx, rx) = watch::channel(0u64);
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let mock = MockReviewer::with_request_hook(vec![raw], move || {
        if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
            tx.send_replace(1);
        }
    })
    .await;
    let id = start(&mock, schema(), rx).await;
    let (status, _) = mock.wait_terminal(&id).await;
    assert_eq!(status, "cancelled");
    assert_eq!(mock.requests.lock().unwrap().len(), 2);
}
