//! 合成运行产物经真实终审入口交给审查员，不依赖供应商或真实会话。
use super::*;
use crate::agent::child_reviewer::retirement_tests::MockReviewer;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn pass() -> Value {
    json!({"verdict":"passed","score":90,"summary":"变化与本次结论无关，可以交付",
        "blocking_issues":[],"nit_issues":[]})
}

struct Scenario {
    mock: MockReviewer,
    workspace: std::path::PathBuf,
    baseline: GitBaseline,
    context: SandwichContext,
    checkpoint: AgentCheckpoint,
    routing: ModelRoutingConfig,
    start: chrono::DateTime<Utc>,
    session: String,
}

impl Drop for Scenario {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

impl Scenario {
    async fn new(
        responses: Vec<Value>,
        continuous: bool,
        stop: Option<watch::Sender<u64>>,
    ) -> Self {
        let workspace =
            std::env::temp_dir().join(format!("material-decision-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(workspace.join("runtime")).unwrap();
        std::fs::write(workspace.join("runtime/status.json"), r#"{"count":0}"#).unwrap();
        let baseline = GitBaseline::non_git();
        let changed_workspace = workspace.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let mock = MockReviewer::with_request_hook(responses, move || {
            let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
            if continuous || call == 1 {
                std::fs::write(
                    changed_workspace.join("runtime/status.json"),
                    format!("{{\"count\":{call}}}"),
                )
                .unwrap();
            }
            if let Some(tx) = &stop {
                let _ = tx.send(1);
            }
        })
        .await;
        let mut context =
            SandwichContext::new("system".into(), "只读分析运行状态，交付结论".into());
        context.record_side_effect(1, "local_shell".into(), "运行查询的保守台账".into());
        context.change_verification = Some(ChangeVerification {
            status: "waived".into(),
            evidence_refs: vec![],
            waived_reason: Some("只读报告，由终审核对".into()),
        });
        let checkpoint = AgentCheckpoint::new("session".into(), &context);
        let mut routing = ModelRoutingConfig::default();
        routing.role_models.insert("reviewer".into(), "mock".into());
        Self {
            mock,
            workspace,
            baseline,
            context,
            checkpoint,
            routing,
            start: Utc::now() - chrono::Duration::seconds(1),
            session: format!("material-{}", uuid::Uuid::new_v4()),
        }
    }

    async fn evaluate(&mut self, rx: &mut watch::Receiver<u64>) -> CompletionEvaluation {
        let app = tauri::test::mock_app();
        let instruction: AgentInstruction = serde_json::from_value(json!({"tool":"finish",
            "command":{"summary":"运行状态诊断报告"}}))
        .unwrap();
        evaluate_completion(
            app.handle(),
            &self.mock.db,
            &mut self.context,
            &mut self.checkpoint,
            "material-run",
            &self.session,
            10,
            &self.workspace,
            &self.baseline,
            self.start,
            &self.routing,
            rx,
            &instruction,
            CompletionMode::Finish,
            false,
            false,
        )
        .await
    }
}

#[tokio::test]
async fn material_changes_are_reviewed_and_real_blockers_are_preserved() {
    for blocking in [false, true] {
        let verdict = if blocking {
            json!({"verdict":"rejected","score":90,"summary":"新材料使报告断言失实",
            "blocking_issues":[{"file":"runtime/status.json","issue":"报告断言与当前结果矛盾","requirement":"纠正断言并补充证据"}],"nit_issues":[]})
        } else {
            pass()
        };
        let mut scenario = Scenario::new(vec![pass(), verdict], false, None).await;
        let (_tx, mut rx) = watch::channel(0);
        let evaluation = scenario.evaluate(&mut rx).await;
        assert_eq!(
            evaluation.receipt.is_ready(),
            !blocking,
            "{:?}",
            evaluation.receipt
        );
        assert_eq!(
            evaluation
                .review_result
                .as_ref()
                .unwrap()
                .blocking_issues
                .len(),
            usize::from(blocking)
        );
        let requests = scenario.mock.requests.lock().unwrap();
        assert_eq!(requests.len(), 2, "变化只补充一次现有审查");
        let refreshed = requests[1]["messages"].to_string();
        for text in [
            "审查期间材料变化",
            "runtime/status.json",
            "此前材料",
            "当前材料",
            "变化本身不构成 blocking",
            "不要求逐项填表",
        ] {
            assert!(refreshed.contains(text), "缺少变化事实或裁量边界：{text}");
        }
        drop(requests);
        assert!(evaluation
            .receipt
            .material_notices
            .iter()
            .any(|s| s.contains("runtime/status.json")));
        if !blocking {
            let current = scenario.baseline.collect_review_snapshot(
                &scenario.workspace,
                &scenario.context,
                scenario.start,
            );
            assert_eq!(evaluation.receipt.diff_hash, current.hash);
            scenario.checkpoint.pending_finalization = Some(evaluation.receipt);
            let cached = scenario.evaluate(&mut rx).await;
            assert!(cached.receipt.is_ready());
            assert_eq!(
                scenario.mock.requests.lock().unwrap().len(),
                2,
                "已复核的稳定材料可以复用"
            );
        }
    }
}

#[tokio::test]
async fn continuously_changing_material_is_disclosed_without_an_endless_review_loop() {
    let mut scenario = Scenario::new(vec![pass()], true, None).await;
    let (_tx, mut rx) = watch::channel(0);
    let evaluation = scenario.evaluate(&mut rx).await;
    assert!(
        evaluation.receipt.is_ready(),
        "变化本身不合成 blocking finding"
    );
    assert_eq!(scenario.mock.requests.lock().unwrap().len(), 2);
    assert!(evaluation
        .receipt
        .material_notices
        .iter()
        .any(|s| s.contains("未取得独立审查结论")));
    let current = scenario.baseline.collect_review_snapshot(
        &scenario.workspace,
        &scenario.context,
        scenario.start,
    );
    assert!(
        !evaluation
            .receipt
            .is_fresh(&current.hash, scenario.context.resource_scope.revision, ""),
        "不能把最后未审的版本登记为已审版本"
    );
    scenario.checkpoint.pending_finalization = Some(evaluation.receipt);
    let next = scenario.evaluate(&mut rx).await;
    assert!(next.receipt.is_ready());
    assert_eq!(
        scenario.mock.requests.lock().unwrap().len(),
        4,
        "旧的不稳定收据不能跳过审查"
    );
}

#[tokio::test]
async fn ignored_runtime_output_in_clean_git_fallback_has_exact_change_paths() {
    let mut scenario = Scenario::new(vec![pass()], false, None).await;
    std::fs::write(scenario.workspace.join(".gitignore"), "runtime/\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "baseline",
        ],
    ] {
        assert!(std::process::Command::new("git")
            .current_dir(&scenario.workspace)
            .args(args)
            .status()
            .unwrap()
            .success());
    }
    scenario.baseline = GitBaseline::capture(&scenario.workspace);
    let (_tx, mut rx) = watch::channel(0);
    let evaluation = scenario.evaluate(&mut rx).await;
    assert!(evaluation.receipt.is_ready());
    let requests = scenario.mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let text = requests[1]["messages"].to_string();
    assert!(text.contains("runtime/status.json") && text.contains("1 个文件指纹变化"));
}

#[tokio::test]
async fn stop_during_material_change_does_not_launch_another_review() {
    let (tx, mut rx) = watch::channel(0);
    let mut scenario = Scenario::new(vec![pass()], false, Some(tx)).await;
    let evaluation = scenario.evaluate(&mut rx).await;
    assert!(!evaluation.receipt.is_ready());
    assert!(evaluation
        .receipt
        .blockers
        .iter()
        .any(|s| s.contains("停止或替换任务")));
    assert_eq!(scenario.mock.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn invalid_review_after_material_change_is_not_treated_as_approval() {
    let mut scenario = Scenario::new(
        vec![pass(), json!({"invalid":"missing verdict"})],
        false,
        None,
    )
    .await;
    let (_tx, mut rx) = watch::channel(0);
    let evaluation = scenario.evaluate(&mut rx).await;
    assert!(!evaluation.receipt.is_ready());
    assert!(evaluation
        .review_result
        .as_ref()
        .unwrap()
        .is_service_failure());
    assert!(scenario.checkpoint.pending_task_failure.is_none());
    assert!(evaluation.receipt.blockers.iter().any(|message| message.contains("ask_human")));
}

#[tokio::test]
async fn failed_host_recheck_is_not_erased_by_a_changed_material_retry() {
    let mut scenario = Scenario::new(
        vec![json!({"tool":"verify","id":"check"}), pass(), pass()],
        false,
        None,
    )
    .await;
    scenario.context.review_contract = Some(serde_json::from_value(json!({
        "scope_statement":"验证诊断依据", "obligations":[{
            "id":"check", "question":"验证命令实际结果", "status":"evidenced", "evidence_refs":["step_1"],
            "closure_form":"command", "verify_command":"printf x >> verification-count; test $(wc -c < verification-count) -eq 1"
        }]
    })).unwrap());
    let (_tx, mut rx) = watch::channel(0);
    let evaluation = scenario.evaluate(&mut rx).await;
    assert!(!evaluation.receipt.is_ready());
    assert_eq!(
        std::fs::read_to_string(scenario.workspace.join("verification-count")).unwrap(),
        "xx",
        "初次宿主验证通过，审查员实际请求的第二次执行失败"
    );
    assert_eq!(
        scenario.mock.requests.lock().unwrap().len(),
        2,
        "失败不能被第三次空白审查抹掉"
    );
    assert!(evaluation
        .receipt
        .blockers
        .iter()
        .any(|s| s.contains("复验未通过")));
}
