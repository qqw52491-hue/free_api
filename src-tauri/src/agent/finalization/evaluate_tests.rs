use super::*;
use crate::agent::change_verification::ChangeVerification;
use crate::agent::reviewer_gatekeeper::BlockingIssue;

fn context() -> SandwichContext {
    SandwichContext::new("system".into(), "goal".into())
}

#[test]
fn complete_delivery_body_preserves_markdown_and_versions_both_fields() {
    let c = context();
    let cp = checkpoint(&c);
    for (payload, expected) in [
        (json!({"summary":"摘要","details":"# 正文\n\n**结论**"}), "摘要\n\n# 正文\n\n**结论**"),
        (json!({"details":"# 只有正文"}), "# 只有正文"),
        (json!({"summary":"相同","details":"相同"}), "相同"),
        (json!({"summary":"仅摘要"}), "仅摘要"),
    ] {
        assert_eq!(delivery_body(&payload).as_deref(), Some(expected));
    }
    let a = json!({"summary":"检查完成","details":"发现问题 A"});
    let b = json!({"summary":"检查完成","details":"未发现问题"});
    assert_ne!(acceptance_fingerprint(&c, &cp, &a), acceptance_fingerprint(&c, &cp, &b));
}

#[tokio::test]
async fn changed_details_reach_real_review_and_cannot_reuse_prepare_receipt() {
    use crate::agent::child_reviewer::retirement_tests::MockReviewer;
    let pass = json!({"verdict":"passed","score":90,"blocking_issues":[],"nit_issues":[],"summary":"ok"});
    let mock = MockReviewer::new(vec![pass]).await;
    let workspace = mock.root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let baseline = GitBaseline::capture(&workspace);
    std::fs::write(workspace.join("report.md"), "报告实物").unwrap();
    let mut c = context();
    c.record_side_effect(1, "local_file".into(), "report.md".into());
    c.change_verification = Some(ChangeVerification { status:"waived".into(),
        evidence_refs:vec![], waived_reason:Some("静态报告，终审核对内容".into()) });
    let mut cp = checkpoint(&c);
    let mut routing = ModelRoutingConfig::default();
    routing.role_models.insert("reviewer".into(), "mock".into());
    let app = tauri::test::mock_app();
    let (_tx, mut rx) = watch::channel(0);
    let session = format!("details-review-{}", uuid::Uuid::new_v4());
    let start = Utc::now() - chrono::Duration::seconds(1);
    for (index, details) in ["# 原结论\n\n发现问题 A", "# 新结论\n\n问题 A 已解决", "# 新结论\n\n问题 A 已解决"].into_iter().enumerate() {
        let instruction: AgentInstruction = serde_json::from_value(if index == 0 {
            json!({"prepare_finish":{"summary":"检查完成","details":details}})
        } else if index == 1 {
            json!({"tool":"finish","command":"","params":{"summary":"检查完成","details":details}})
        } else {
            json!({"tool":"finish","params":{"summary":"检查完成","details":details}})
        }).unwrap();
        let evaluation = evaluate_completion(app.handle(), &mock.db, &mut c, &mut cp,
            "details-run", &session, 10+index, &workspace, &baseline, start, &routing, &mut rx,
            &instruction, if index==0 { CompletionMode::Prepare } else { CompletionMode::Finish }, false, false).await;
        assert!(evaluation.receipt.is_ready(), "{:?}", evaluation.receipt);
        assert_eq!(evaluation.receipt.finish_payload["details"], details);
        cp.pending_finalization = Some(evaluation.receipt);
        assert_eq!(mock.requests.lock().unwrap().len(), if index==0 {1} else {2});
    }
    let requests = mock.requests.lock().unwrap();
    for (request, text) in requests.iter().zip(["# 原结论\n\n发现问题 A", "# 新结论\n\n问题 A 已解决"]) {
        let messages = request["messages"].as_array().unwrap();
        assert!(messages.iter().any(|m| m["content"].as_str().is_some_and(|s| s.contains(&format!("检查完成\n\n{text}")))));
    }
}
fn checkpoint(c: &SandwichContext) -> AgentCheckpoint {
    AgentCheckpoint::new("session".into(), c)
}

#[tokio::test]
async fn review_logging_during_finish_keeps_receipt_valid_and_real_changes_reach_reviewer() {
    use crate::agent::child_reviewer::retirement_tests::MockReviewer;
    for change_real_file in [false, true] {
        let workspace = std::env::temp_dir().join(format!("finish-runtime-{}", uuid::Uuid::new_v4()));
        for dir in ["agent_logs", "src-tauri/agent_logs", "config"] {
            std::fs::create_dir_all(workspace.join(dir)).unwrap();
        }
        std::fs::write(workspace.join(".gitignore"), "agent_logs/\n").unwrap();
        std::fs::write(workspace.join("config/settings.json"), r#"{"enabled":true}"#).unwrap();
        for args in [
            vec!["init", "-q"], vec!["add", "."],
            vec!["-c", "user.name=test", "-c", "user.email=test@example.com", "commit", "-qm", "baseline"],
        ] {
            assert!(std::process::Command::new("git").current_dir(&workspace).args(args).status().unwrap().success());
        }
        let baseline = GitBaseline::capture(&workspace);
        let changed_workspace = workspace.clone();
        let mock = MockReviewer::with_request_hook(vec![json!({"verdict":"passed","score":90,
            "summary":"核查报告与证据后通过","blocking_issues":[],"nit_issues":[]})], move || {
            // 在真实终审请求进行中合成宿主写日志；不改变 cwd、不写真实会话日志。
            for path in ["agent_logs/review.json", "src-tauri/agent_logs/review.json"] {
                std::fs::write(changed_workspace.join(path), r#"[{"content":"终审记录"}]"#).unwrap();
            }
            if change_real_file {
                std::fs::write(changed_workspace.join("config/settings.json"), r#"{"enabled":false}"#).unwrap();
            }
        }).await;
        let mut c = context();
        c.record_side_effect(1, "local_shell".into(), "SQLite SELECT 查询的保守台账".into());
        c.change_verification = Some(ChangeVerification {status:"waived".into(),
            evidence_refs:vec![], waived_reason:Some("只读诊断，终审核对报告".into())});
        let mut cp = checkpoint(&c);
        let mut routing = ModelRoutingConfig::default();
        routing.role_models.insert("reviewer".into(), "mock".into());
        let app = tauri::test::mock_app();
        let (_tx, mut rx) = watch::channel(0);
        let session = format!("runtime-review-{}", uuid::Uuid::new_v4());
        let start = Utc::now() - chrono::Duration::seconds(1);
        let instruction: AgentInstruction = serde_json::from_value(json!({"tool":"finish",
            "command":{"summary":"只读诊断报告：核对数据库和源码后给出结论"}})).unwrap();
        let result = evaluate_completion(app.handle(), &mock.db, &mut c, &mut cp,
            "runtime-run", &session, 10, &workspace, &baseline, start, &routing, &mut rx,
            &instruction, CompletionMode::Finish, false, false).await;
        assert!(result.receipt.review.is_accepted(), "本地审查员确实通过");
        assert!(result.receipt.is_ready(), "审查员可认可变化：{:?}", result.receipt);
        assert_eq!(mock.requests.lock().unwrap().len(), if change_real_file { 2 } else { 1 });
        if change_real_file {
            let requests = mock.requests.lock().unwrap();
            let refreshed = requests[1]["messages"].as_array().unwrap().iter()
                .filter_map(|m| m["content"].as_str()).collect::<Vec<_>>().join("\n");
            assert!(refreshed.contains("审查期间材料变化"));
            assert!(refreshed.contains("config/settings.json"));
            assert!(refreshed.contains("\"enabled\":true") && refreshed.contains("\"enabled\":false"));
        }
        if !change_real_file {
            cp.pending_finalization = Some(result.receipt);
            std::fs::write(workspace.join("agent_logs/review.json"), "追加一条运行记录").unwrap();
            let reused = evaluate_completion(app.handle(), &mock.db, &mut c, &mut cp,
                "runtime-run", &session, 11, &workspace, &baseline, start, &routing, &mut rx,
                &instruction, CompletionMode::Finish, false, false).await;
            assert!(reused.receipt.is_ready(), "写日志后仍可复用就绪收据");
            assert_eq!(mock.requests.lock().unwrap().len(), 1, "不能因日志变化再次调用终审");
        }
        let requests = mock.requests.lock().unwrap();
        assert!(!requests[0]["messages"].to_string().contains("--- 文件内容: agent_logs/"));
        assert!(!requests[0]["messages"].to_string().contains("--- 文件内容: src-tauri/agent_logs/"));
        drop(requests);
        std::fs::remove_dir_all(workspace).unwrap();
    }
}

#[tokio::test]
async fn existing_evidence_reaches_finish_without_reexecuting_its_command() {
    use crate::agent::child_reviewer::retirement_tests::MockReviewer;
    for changed in [false, true] {
        let session = format!("verification-reuse-{}", uuid::Uuid::new_v4());
        let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID,
            json!(["verification-evidence-v2", session, "first-capture"]).to_string().as_bytes()).to_string();
        let mock = MockReviewer::new(vec![
            json!({"tool":"evidence_find", "query":"产物"}),
            json!({"tool":"evidence_inspect", "evidence_id":id}),
            json!({"tool":"read_step_output", "output_id":"prior-validation"}),
            json!({"tool":"evidence_compare", "evidence_id":id}),
            json!({"verdict":if changed {"rejected"} else {"passed"}, "score":85,
                "summary":"依据原始回执和当前对比结果裁决", "nit_issues":[],
                "blocking_issues":if changed {vec![json!({"file":"artifact.txt", "issue":"验证后的文件发生变化", "requirement":"针对当前版本补验"})]} else {vec![]}}),
        ]).await;
        let workspace = mock.root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let baseline = GitBaseline::capture(&workspace);
        let start = Utc::now() - chrono::Duration::seconds(1);
        std::fs::write(workspace.join("artifact.txt"), b"original artifact").unwrap();
        let command = "printf x >> validation-count; test -s artifact.txt";
        let verified = crate::agent::host_verify::run_command(&workspace, command, 10).await;
        assert!(verified.passed);
        {
            let conn = mock.db.0.lock().unwrap();
            let raw = format!("command={command}\nexit={}\n{}", verified.exit_code, verified.output_tail);
            conn.execute("INSERT INTO agent_step_logs(output_id,session_id,step_id,raw_output,created_at) VALUES ('prior-validation',?1,2,?2,?3)",
                rusqlite::params![session,raw,Utc::now().to_rfc3339()]).unwrap();
            crate::agent::retrieval::index_step_fts(&conn,"prior-validation",&session,2,"local_shell","exec","succeeded",&raw).unwrap();
        }
        let recorded = crate::agent::task_store::dispatch_in_workspace(&mock.db, &session, "evidence_record",
            &json!({"purpose":"产物完整性验证", "file_path":"artifact.txt", "command":command,
                "output_id":"prior-validation", "idempotency_key":"first-capture"}), Some(&workspace));
        assert!(recorded.success, "{}", recorded.stderr);
        let record: Value = serde_json::from_str(&recorded.stdout).unwrap();
        assert_eq!(record["evidence"]["evidence_id"], id);
        if changed { std::fs::write(workspace.join("artifact.txt"), b"changed after validation").unwrap(); }
        let mut c = context();
        c.record_side_effect(1, "local_file".into(), "artifact.txt".into());
        c.change_verification = Some(ChangeVerification {status:"verified".into(),
            evidence_refs:vec!["output_id:prior-validation".into()], waived_reason:None});
        let mut cp = checkpoint(&c);
        let mut routing = ModelRoutingConfig::default();
        routing.role_models.insert("reviewer".into(), "mock".into());
        let app = tauri::test::mock_app();
        let (_tx, mut rx) = watch::channel(0);
        let instruction: AgentInstruction = serde_json::from_value(json!({"tool":"finish", "command":{
            "summary":"交付产物，引用已有验证并核对其适用性"
        }})).unwrap();
        let evaluation = evaluate_completion(app.handle(), &mock.db, &mut c, &mut cp, "reuse-run", &session,
            10, &workspace, &baseline, start, &routing, &mut rx, &instruction, CompletionMode::Finish, false, false).await;
        assert_eq!(evaluation.receipt.is_ready(), !changed, "{:?}", evaluation.receipt);
        assert_eq!(std::fs::read_to_string(workspace.join("validation-count")).unwrap(), "x",
            "登记、终审取证与对比不应重新执行旧命令");
        let requests = mock.requests.lock().unwrap();
        assert_eq!(requests.len(), 5);
        assert!(requests[0]["messages"][1]["content"].as_str().unwrap().contains("可选证据登记索引"));
        let observation = |index: usize| requests[index]["messages"].as_array().unwrap()
            .last().unwrap()["content"].as_str().unwrap().to_string();
        assert!(observation(1).contains(&id));
        assert!(observation(2).contains("current_task"));
        assert!(observation(3).contains("exit=0"));
        assert!(observation(4).contains(if changed {"change_detected"} else {"no_change_detected"}));
        drop(requests);
        if !changed {
            cp.pending_finalization = Some(evaluation.receipt);
            let cached = evaluate_completion(app.handle(), &mock.db, &mut c, &mut cp, "reuse-run", &session,
                11, &workspace, &baseline, start, &routing, &mut rx, &instruction, CompletionMode::Finish, false, false).await;
            assert!(cached.receipt.is_ready());
            assert_eq!(mock.requests.lock().unwrap().len(), 5, "相同材料可复用就绪收据");
            crate::agent::task_store::dispatch(&mock.db, &session, "evidence_record", &json!({
                "subject":"产物", "purpose":"补充新证据", "reported_outcome":"failed"
            }));
            let refreshed = evaluate_completion(app.handle(), &mock.db, &mut c, &mut cp, "reuse-run", &session,
                12, &workspace, &baseline, start, &routing, &mut rx, &instruction, CompletionMode::Finish, false, false).await;
            assert!(refreshed.receipt.is_ready());
            let requests = mock.requests.lock().unwrap();
            assert_eq!(requests.len(), 6, "新证据必须进入审查，不能在就绪收据处被跳过");
            assert!(requests[5]["messages"][1]["content"].as_str().unwrap().contains("补充新证据"));
            assert_eq!(std::fs::read_to_string(workspace.join("validation-count")).unwrap(), "x");
        }
    }
}

#[tokio::test]
async fn rejected_review_reuses_real_frozen_execution_but_changed_artifact_runs_again() {
    use crate::agent::child_reviewer::retirement_tests::MockReviewer;
    let rejection = json!({"verdict":"rejected", "score":60, "summary":"交付说明不清晰",
        "blocking_issues":[{"file":"artifact.txt","issue":"说明缺失","requirement":"补充交付说明"}],
        "nit_issues":[]});
    let accepted = json!({"verdict":"passed", "score":93, "summary":"已核对旧执行证据和新说明",
        "blocking_issues":[], "nit_issues":[]});
    let mock = MockReviewer::new(vec![rejection, accepted.clone(), accepted.clone(), accepted]).await;
    let workspace = mock.root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let baseline = GitBaseline::capture(&workspace);
    let start = Utc::now() - chrono::Duration::seconds(1);
    std::fs::write(workspace.join("artifact.txt"), b"version-one").unwrap();
    let command = "printf x >> frozen-count; test -s artifact.txt";
    let mut c = context();
    c.record_side_effect_with_paths(1, "local_file".into(), "artifact.txt".into(), vec!["artifact.txt".into()]);
    c.change_verification = Some(ChangeVerification {
        status:"verified".into(), evidence_refs:vec!["step_1".into()], waived_reason:None,
    });
    c.review_contract = Some(serde_json::from_value(json!({
        "scope_statement":"交付安装包",
        "obligations":[{
            "id":"frozen", "question":"当前产物完整性",
            "status":"evidenced", "evidence_refs":["step_1"], "closure_form":"command",
            "verify_command":command
        }]
    })).unwrap());
    let mut cp = checkpoint(&c);
    let mut routing = ModelRoutingConfig::default();
    routing.role_models.insert("reviewer".into(), "mock".into());
    let app = tauri::test::mock_app();
    let (_tx, mut rx) = watch::channel(0);
    let finish = |summary: &str| -> AgentInstruction {
        serde_json::from_value(json!({"tool":"finish","command":{"summary":summary}})).unwrap()
    };
    let first = evaluate_completion(app.handle(), &mock.db, &mut c, &mut cp,
        "frozen-run", "frozen-session", 10, &workspace, &baseline, start, &routing,
        &mut rx, &finish("初稿"), CompletionMode::Finish, false, false).await;
    assert!(!first.receipt.is_ready(), "Reviewer 阻塞应保留");
    assert!(first.receipt.frozen_verification_run.is_some(),
        "打回也必须保留真实宿主冻结验证回执");
    assert_eq!(std::fs::read_to_string(workspace.join("frozen-count")).unwrap(), "x");
    cp.pending_finalization = Some(first.receipt);
    cp.stage_review_blockers_open = 1;
    let blocked = evaluate_completion(app.handle(), &mock.db, &mut c, &mut cp,
        "frozen-run", "frozen-session", 11, &workspace, &baseline, start, &routing,
        &mut rx, &finish("等待阶段阻塞解除"), CompletionMode::Finish, false, false).await;
    assert!(!blocked.receipt.is_ready());
    assert!(blocked.receipt.frozen_verification_run.is_some(),
        "中途被阶段 Blocking 拦下不应丢失旧的真实验证回执");
    assert_eq!(mock.requests.lock().unwrap().len(), 1,
        "未解决的阶段阻塞应继续生效，不应放行 Reviewer");
    assert_eq!(std::fs::read_to_string(workspace.join("frozen-count")).unwrap(), "x");
    cp.pending_finalization = Some(blocked.receipt);
    cp.stage_review_blockers_open = 0;
    // 新登记的审查证据只会刷新 Reviewer，不应无故使真实冻结验证重跑。
    let registered = crate::agent::task_store::dispatch(&mock.db, "frozen-session", "evidence_record",
        &json!({"subject":"交付说明", "purpose":"补充说明依据"}));
    assert!(registered.success, "{}", registered.stderr);
    let second = evaluate_completion(app.handle(), &mock.db, &mut c, &mut cp,
        "frozen-run", "frozen-session", 11, &workspace, &baseline, start, &routing,
        &mut rx, &finish("补充交付说明"), CompletionMode::Finish, false, false).await;
    assert!(second.receipt.is_ready(), "{:?}", second.receipt);
    assert_eq!(std::fs::read_to_string(workspace.join("frozen-count")).unwrap(), "x",
        "只补说明或登记证据后须重新审查，而非无故重跑已通过的冻结命令");
    let second_prompt = mock.requests.lock().unwrap()[1]["messages"][1]["content"]
        .as_str().unwrap().to_string();
    assert!(second_prompt.contains("旧契约验证宿主执行回执"));
    assert!(second_prompt.contains("补充说明依据"));
    assert!(second.receipt.frozen_verification_run.is_some());
    cp.pending_finalization = Some(second.receipt);

    std::fs::write(workspace.join("artifact.txt"), b"version-two-different").unwrap();
    let third = evaluate_completion(app.handle(), &mock.db, &mut c, &mut cp,
        "frozen-run", "frozen-session", 12, &workspace, &baseline, start, &routing,
        &mut rx, &finish("修改产物后重新交付"), CompletionMode::Finish, false, false).await;
    assert!(third.receipt.is_ready(), "{:?}", third.receipt);
    assert_eq!(std::fs::read_to_string(workspace.join("frozen-count")).unwrap(), "xx",
        "产物变更不得机械沿用旧验证");
    cp.pending_finalization = Some(third.receipt);
    c.review_contract.as_mut().unwrap().scope_statement = "修改后的验收范围".into();
    let fourth = evaluate_completion(app.handle(), &mock.db, &mut c, &mut cp,
        "frozen-run", "frozen-session", 13, &workspace, &baseline, start, &routing,
        &mut rx, &finish("验收契约变化后重新交付"), CompletionMode::Finish, false, false).await;
    assert!(fourth.receipt.is_ready(), "{:?}", fourth.receipt);
    assert_eq!(std::fs::read_to_string(workspace.join("frozen-count")).unwrap(), "xxx",
        "验收契约变化不得复用之前的冻结验证");
    assert_eq!(mock.requests.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn failed_frozen_verification_is_never_reused_from_a_rejected_receipt() {
    use crate::agent::child_reviewer::retirement_tests::MockReviewer;
    let mock = MockReviewer::new(vec![]).await;
    let workspace = mock.root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let baseline = GitBaseline::capture(&workspace);
    let start = Utc::now() - chrono::Duration::seconds(1);
    std::fs::write(workspace.join("artifact.txt"), b"artifact").unwrap();
    let mut c = context();
    c.record_side_effect(1, "local_file".into(), "artifact.txt".into());
    c.change_verification = Some(ChangeVerification {
        status:"verified".into(), evidence_refs:vec!["step_1".into()], waived_reason:None
    });
    c.review_contract = Some(serde_json::from_value(json!({
        "scope_statement":"验收安装包",
        "obligations":[{
            "id":"frozen", "question":"命令必须通过", "status":"evidenced",
            "evidence_refs":["step_1"], "closure_form":"command",
            "verify_command":"printf x >> frozen-count; false"
        }]
    })).unwrap());
    let mut cp = checkpoint(&c);
    let app = tauri::test::mock_app();
    let (_tx, mut rx) = watch::channel(0);
    let instruction: AgentInstruction = serde_json::from_value(json!({
        "tool":"finish","command":{"summary":"自述已验证"}
    })).unwrap();
    for (step, count) in [(10, "x"), (11, "xx")] {
        let evaluation = evaluate_completion(app.handle(), &mock.db, &mut c, &mut cp,
            "frozen-run", "frozen-session", step, &workspace, &baseline, start,
            &ModelRoutingConfig::default(), &mut rx, &instruction,
            CompletionMode::Finish, false, false).await;
        assert!(!evaluation.receipt.is_ready());
        assert!(evaluation.receipt.frozen_verification_run.is_none(),
            "失败的冻结命令不应产生可复用的宿主通过收据");
        assert_eq!(std::fs::read_to_string(workspace.join("frozen-count")).unwrap(), count);
        cp.pending_finalization = Some(evaluation.receipt);
    }
    assert!(mock.requests.lock().unwrap().is_empty(),
        "宿主冻结命令失败不得由 Reviewer 自述覆盖");
}

#[tokio::test]
async fn evidence_registration_does_not_bypass_frozen_verification() {
    use crate::agent::child_reviewer::retirement_tests::MockReviewer;
    let mock = MockReviewer::new(vec![json!({})]).await;
    let workspace = mock.root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let baseline = GitBaseline::capture(&workspace);
    let start = Utc::now() - chrono::Duration::seconds(1);
    std::fs::write(workspace.join("artifact.txt"), "artifact").unwrap();
    let recorded = crate::agent::task_store::dispatch(&mock.db, "frozen-session", "evidence_record", &json!({
        "purpose":"自述旧验证通过", "subject":"产物", "reported_outcome":"passed"
    }));
    assert!(recorded.success);
    let mut c = context();
    c.record_side_effect(1, "local_file".into(), "artifact.txt".into());
    c.change_verification = Some(ChangeVerification {status:"verified".into(),
        evidence_refs:vec!["step_2".into()], waived_reason:None});
    c.review_contract = Some(serde_json::from_value(json!({"scope_statement":"验证交付产物", "obligations":[{
        "id":"frozen", "question":"当前产物通过冻结检查", "status":"evidenced",
        "evidence_refs":["step_2"], "closure_form":"command",
        "verify_command":"printf x >> frozen-checks; false"
    }]})).unwrap());
    let mut cp = checkpoint(&c);
    let app = tauri::test::mock_app();
    let (_tx, mut rx) = watch::channel(0);
    let instruction: AgentInstruction = serde_json::from_value(json!({"tool":"finish", "command":{"summary":"交付产物"}})).unwrap();
    let evaluation = evaluate_completion(app.handle(), &mock.db, &mut c, &mut cp, "frozen-run", "frozen-session",
        10, &workspace, &baseline, start, &ModelRoutingConfig::default(), &mut rx,
        &instruction, CompletionMode::Finish, false, false).await;
    assert!(!evaluation.receipt.is_ready());
    assert_eq!(std::fs::read_to_string(workspace.join("frozen-checks"))
        .unwrap_or_else(|error| panic!("{error}; {:?}", evaluation.receipt)), "x");
    assert!(mock.requests.lock().unwrap().is_empty(), "冻结验证失败不能被自述证据或模型判决覆盖");
}
fn review(blocking: bool) -> ReviewResult {
    let mut r = ReviewResult::skipped_empty();
    r.model_id = "reviewer".into();
    r.diff_hash = "same-material".into();
    if blocking {
        r.blocking_issues.push(BlockingIssue {
            file: "src/a.rs".into(),
            issue: "真实缺陷".into(),
            requirement: "修复后重新验证".into(),
        });
    }
    r
}
#[test]
fn reviewed_flag_and_retry_count_never_erase_remaining_facts() {
    let mut c = context();
    for step in 1..=39 {
        c.record_side_effect(step, "local_shell".into(), "write or unknown shell".into());
    }
    let mut cp = checkpoint(&c);
    cp.finish_reviewed = true;
    cp.finish_challenge_count = 100;
    assert!(
        change_verification_blocker(c.side_effects.len(), c.change_verification.as_ref()).is_some()
    );
    assert_eq!(
        verification_receipt(true, c.change_verification.as_ref(), &[]),
        ReceiptVerification::Pending
    );
    for retries in [0, 1, 2, 3, 100] {
        assert!(!review_acceptance(&review(true), retries, false).is_accepted());
    }
}
#[test]
fn human_arbitration_is_explicit_and_never_converts_service_failure_to_pass() {
    assert!(!review_acceptance(&review(true), 2, true).is_accepted());
    assert!(matches!(
        review_acceptance(&review(true), 3, true),
        ReceiptReview::DeliveredWithWarnings { .. }
    ));
    assert!(review_acceptance(&review(false), 0, false).is_accepted());
    let failed = ReviewResult::failed_open("401 / timeout", "model", "hash");
    assert!(!review_acceptance(&failed, 100, true).is_accepted());
}
#[test]
fn verifier_failure_is_not_overridden_by_an_older_verified_claim() {
    let verified = ChangeVerification {
        status: "verified".into(),
        evidence_refs: vec!["step_40".into()],
        waived_reason: None,
    };
    let failures =
        vec!["宿主命令 mvn test 在 /tmp/ws 退出 1：[ERROR] No tests matching pattern".into()];
    assert!(matches!(
        verification_receipt(true, Some(&verified), &failures),
        ReceiptVerification::Failed { .. }
    ));
    assert!(matches!(
        verification_receipt(true, Some(&verified), &[]),
        ReceiptVerification::Verified { .. }
    ));
    assert_eq!(
        verification_receipt(false, None, &[]),
        ReceiptVerification::NotRequired
    );
}
#[test]
fn acceptance_version_ignores_display_bookkeeping_but_tracks_new_writes_and_claim_corrections() {
    use crate::agent::target_block::TargetBlock;
    let mut c = context();
    c.block_manager
        .set_blocks(vec![TargetBlock::new("b1", "repair")]);
    let cp = checkpoint(&c);
    let fp = acceptance_fingerprint(&c, &cp, &json!({"summary":"交付"}));
    c.current_observation = "new output / artifact registration".into();
    c.add_host_feedback("display-only");
    c.active_target_block_mut().unwrap().on_step_finish();
    assert_eq!(fp, acceptance_fingerprint(&c, &cp, &json!({"summary":"交付"})));
    c.record_side_effect(40, "local_file".into(), "source changed".into());
    assert_ne!(fp, acceptance_fingerprint(&c, &cp, &json!({"summary":"交付"})));
    let written = acceptance_fingerprint(&c, &cp, &json!({"summary":"交付"}));
    c.change_verification = Some(ChangeVerification {
        status: "waived".into(),
        evidence_refs: vec![],
        waived_reason: Some("无测试工具，由终审核对".into()),
    });
    assert_ne!(written, acceptance_fingerprint(&c, &cp, &json!({"summary":"交付"})));
}
#[test]
fn acceptance_version_tracks_stage_defects_scope_and_summary() {
    use crate::agent::target_block::TargetBlock;
    let mut c = context();
    c.block_manager
        .set_blocks(vec![TargetBlock::new("b1", "repair")]);
    let mut cp = checkpoint(&c);
    let fp = acceptance_fingerprint(&c, &cp, &json!({"summary":"summary"}));
    assert_ne!(
        fp,
        acceptance_fingerprint(&c, &cp, &json!({"summary":"different result"}))
    );
    cp.stage_review_blockers_open = 1;
    assert_ne!(fp, acceptance_fingerprint(&c, &cp, &json!({"summary":"summary"})));
    cp.stage_review_blockers_open = 0;
    c.active_target_block_mut()
        .unwrap()
        .scope
        .push("another.rs".into());
    assert_ne!(fp, acceptance_fingerprint(&c, &cp, &json!({"summary":"summary"})));
}
#[test]
fn successful_review_does_not_consume_a_stop_signal_as_completion() {
    let (tx, rx) = watch::channel(0);
    let generation = *rx.borrow();
    assert!(!interrupted_since(&rx, generation));
    let accepted = review_acceptance(&review(false), 0, false);
    assert!(accepted.is_accepted());
    tx.send(1).unwrap();
    assert!(interrupted_since(&rx, generation));
    tx.send(2).unwrap();
    assert!(interrupted_since(&rx, generation));
}
#[test]
fn pending_or_failed_verification_never_produces_a_ready_receipt() {
    let mut receipt:FinalizationReceipt=serde_json::from_value(json!({
        "receipt_id":"test","diff_hash":"h","scope_revision":0,"contract_fingerprint":"",
        "step_id":42,"verification":{"status":"pending"},"review":{"status":"passed","score":84,"model_id":"m","summary":"ok"},
        "blockers":[],"risk_flags":[],"created_at":"now"
    })).unwrap();
    assert!(!receipt.is_ready());
    receipt.verification = ReceiptVerification::Failed {
        details: "exit 1".into(),
    };
    assert!(!receipt.is_ready());
    receipt.verification = ReceiptVerification::Verified {
        evidence: "host exit 0".into(),
    };
    assert!(receipt.is_ready());
    receipt.review = ReceiptReview::Pending;
    assert!(!receipt.is_ready());
}
#[test]
fn inferred_verification_is_scoped_to_the_task_repository_and_subproject() {
    use crate::agent::change_verification::infer_adaptive_verification_command_in;
    let dir = std::env::temp_dir().join(format!("finish-scope-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(dir.join("module/src")).unwrap();
    std::fs::write(
        dir.join("module/Cargo.toml"),
        "[package]\nname='module'\nversion='0.1.0'\n",
    )
    .unwrap();
    let cmd = infer_adaptive_verification_command_in(&dir, &["module/src/main.rs".into()]).unwrap();
    assert!(cmd.contains("/module' && cargo check"), "{cmd}");
    assert!(infer_adaptive_verification_command_in(&dir, &["../outside/a.rs".into()]).is_none());
    assert!(infer_adaptive_verification_command_in(&dir, &["/outside/a.rs".into()]).is_none());
    assert!(infer_adaptive_verification_command_in(&dir, &["README.md".into()]).is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn frozen_suite_success_saves_real_receipt_without_model_recopy_and_failure_does_not() {
    let mut c = context();
    c.record_side_effect(39, "local_file".into(), "write".into());
    let mut result = crate::agent::host_verify::ObligationVerifyResult {
        obligation_id: "package".into(),
        command: "unzip -l package.zip | grep lic".into(),
        working_dir: "/tmp/ws".into(),
        exit_code: "1".into(),
        output_tail: "not found".into(),
        passed: false,
    };
    assert!(!save_successful_obligation_receipt(&mut c, 42, &[]));
    assert!(c.change_verification.is_none());
    assert!(!save_successful_obligation_receipt(
        &mut c,
        42,
        std::slice::from_ref(&result)
    ));
    assert!(c.change_verification.is_none());
    result.passed = true;
    result.exit_code = "0".into();
    assert!(save_successful_obligation_receipt(&mut c, 43, &[result]));
    assert!(c.change_verification.as_ref().unwrap().is_resolved());
    c.record_side_effect(44, "local_file".into(), "new write".into());
    assert!(c.change_verification.is_none());
}

#[test]
fn evaluation_payload_recovers_only_the_known_finish_frame_and_preserves_the_instruction() {
    let fixtures: Vec<Value> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test/fixtures/finish-summary-fragments.json"
    )))
    .unwrap();
    for fixture in fixtures {
        let raw = fixture["raw"].as_str().unwrap();
        let expected = fixture["expected"].as_str().unwrap();
        for prepare in [false, true] {
            let instruction: AgentInstruction = serde_json::from_value(if prepare {
                json!({"prepare_finish":{"summary":raw}})
            } else {
                json!({"tool":"finish","command":{"summary":raw}})
            })
            .unwrap();
            let original = serde_json::to_value(&instruction).unwrap();
            let result = delivery_payload(&instruction);
            assert_eq!(result["summary"], expected, "{}", fixture["name"]);
            assert_eq!(serde_json::to_value(&instruction).unwrap(), original);
        }
    }
}

#[tokio::test]
async fn completed_report_todos_do_not_infer_a_requirement_to_write_code() {
    use crate::agent::child_reviewer::{retirement_tests::MockReviewer, ReviewScorecard};
    use crate::agent::types::TodoItem;
    let mock = MockReviewer::new(vec![json!({})]).await;
    for description in [
        "检查修复方案，不修改项目",
        "解释实现原理",
        "review fix proposal",
        "compare refactor options",
    ] {
        let mut c = context();
        c.todo_list.push(TodoItem {
            id: 1,
            status: "done".into(),
            description: description.into(),
            evidence_output_id: None,
        });
        let mut cp = checkpoint(&c);
        cp.plan_phase = crate::agent::checkpoint::PlanPhase::Reviewing;
        cp.last_review_scorecard = Some(ReviewScorecard::new(80, 48, 85, Some(75)));
        let baseline = GitBaseline::capture(&mock.root);
        let (blockers, _, _) = collect_checks(
            &mock.db,
            &c,
            &cp,
            "report-run",
            "report-session",
            1,
            &mock.root,
            &baseline,
            Utc::now(),
            false,
            false,
        )
        .await;
        assert!(blockers.is_empty(), "{description}: {blockers:?}");
        cp.stage_review_blockers_open = 1;
        let (blockers, _, _) = collect_checks(
            &mock.db,
            &c,
            &cp,
            "report-run",
            "report-session",
            1,
            &mock.root,
            &baseline,
            Utc::now(),
            false,
            false,
        )
        .await;
        assert!(
            blockers.iter().any(|b| b.contains("阶段审查仍有 1 条")),
            "真实阶段缺陷仍应阻塞: {blockers:?}"
        );
    }
}

#[test]
fn new_current_requirements_change_acceptance_identity_without_changing_frozen_anchor() {
    let mut context = SandwichContext::new("sys".into(), "original".into());
    let checkpoint = AgentCheckpoint::new("s".into(), &context);
    let payload = json!({"summary":"done"});
    let before = acceptance_fingerprint(&context, &checkpoint, &payload);
    crate::agent::conversation::append_live_goal(&mut context, "also export a PDF");
    assert_eq!(context.user_goal, "original");
    assert_ne!(before, acceptance_fingerprint(&context, &checkpoint, &payload));
}

mod advisory_workflow_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Fixture {
        root: std::path::PathBuf,
        db: DbState,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("advisory-workflow-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(root.join("workspace")).unwrap();
            let db = DbState(Arc::new(Mutex::new(crate::db::init_db(&root.join("test.db")).unwrap())));
            Self { root, db }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.root); }
    }

    #[tokio::test]
    async fn pending_tracking_and_unmatched_evidence_reach_review_without_a_form_gate() {
        let f = Fixture::new();
        let workspace = f.root.join("workspace");
        let mut c = context();
        c.todo_list = serde_json::from_value(json!([
            {"id":1,"status":"pending","description":"总结调查结论"}
        ])).unwrap();
        c.record_side_effect(1, "local_file".into(), "report".into());
        c.review_contract = Some(serde_json::from_value(json!({"scope_statement":"报告", "obligations":[
            {"id":"ui","question":"真人操作是否核验？","status":"open","closure_form":"human"},
            {"id":"refs","question":"引用核对","status":"evidenced","evidence_refs":["missing.rs:9-10"]}
        ]})).unwrap());
        let mut cp = checkpoint(&c);
        let baseline = GitBaseline::capture(&workspace);
        let (blockers, notices, _) = collect_checks(&f.db, &c, &cp, "run", "session", 2,
            &workspace, &baseline, Utc::now(), false, true).await;
        assert!(blockers.is_empty(), "{blockers:?}");
        let text = notices.join("\n");
        assert!(text.contains("总结调查结论") && text.contains("真人操作是否核验") && text.contains("missing.rs"), "{text}");
        assert_eq!(c.todo_list[0].status, "pending");
        assert_eq!(c.review_contract.as_ref().unwrap().obligations[0].status, "open");
        cp.stage_review_blockers_open = 1;
        let (blockers, _, _) = collect_checks(&f.db, &c, &cp, "run", "session", 2,
            &workspace, &baseline, Utc::now(), false, false).await;
        assert!(blockers.iter().any(|b| b.contains("阶段审查")), "实质审查阻塞不被降为建议");
    }

    #[tokio::test]
    async fn absent_declaration_does_not_run_inferred_checks_but_explicit_failure_still_blocks() {
        let f = Fixture::new();
        let workspace = f.root.join("workspace");
        // 若自动执行推导的 cargo check，此 manifest 会失败；本例只检验宿主行为，免模型调用。
        std::fs::write(workspace.join("Cargo.toml"), "invalid manifest").unwrap();
        let baseline = GitBaseline::capture(&workspace);
        let mut c = context();
        c.record_side_effect_with_paths(1, "local_file".into(), "write".into(), vec!["src/lib.rs".into()]);
        assert!(crate::agent::change_verification::infer_adaptive_verification_command_in(
            &workspace, &c.new_affected_paths()).is_some());
        let mut cp = checkpoint(&c);
        let mut triage = crate::agent::triage::TriageVerdict::fallback("fixture");
        triage.final_review_mode = "mechanical_only".into();
        cp.triage_verdict = Some(triage);
        let app = tauri::test::mock_app();
        let (_tx, mut rx) = watch::channel(0);
        let instruction: AgentInstruction = serde_json::from_value(json!({"tool":"finish","params":{"summary":"本例未测试"}})).unwrap();
        let session = format!("advisory-{}", uuid::Uuid::new_v4());
        let result = evaluate_completion(app.handle(), &f.db, &mut c, &mut cp, "run", &session,
            2, &workspace, &baseline, Utc::now(), &ModelRoutingConfig::default(), &mut rx,
            &instruction, CompletionMode::Finish, false, false).await;
        assert!(result.receipt.is_ready(), "{:?}", result.receipt);
        assert!(matches!(result.receipt.verification, ReceiptVerification::Waived { .. }));
        assert!(result.receipt.frozen_verification_run.is_none(), "无真实执行不得生成通过回执");
        c.review_contract = Some(serde_json::from_value(json!({"scope_statement":"实际检查", "obligations":[
            {"id":"explicit","question":"命令通过？","status":"open","verify_command":"printf explicit-failure; exit 1"}
        ]})).unwrap());
        let failed = evaluate_completion(app.handle(), &f.db, &mut c, &mut cp, "run", &session,
            3, &workspace, &baseline, Utc::now(), &ModelRoutingConfig::default(), &mut rx,
            &instruction, CompletionMode::Finish, false, false).await;
        assert!(!failed.receipt.is_ready());
        assert!(failed.receipt.blockers.join("\n").contains("explicit-failure"));
    }

    #[test]
    fn early_filename_is_a_review_observation_not_an_instruction_to_create_a_copy() {
        let f = Fixture::new();
        let c = context();
        let mut cp = checkpoint(&c);
        cp.approved_target_files = Some(vec!["early-name.xlsx".into()]);
        let mut blockers = vec![];
        let mut notices = vec![];
        collect_scope_and_targets(&c, &cp, &f.root.join("workspace"), "session", Utc::now(),
            false, None, &mut blockers, &mut notices);
        assert!(blockers.is_empty());
        assert!(notices.join("\n").contains("early-name.xlsx"));
        assert!(notices.join("\n").contains("不证明未完成"));
    }
}

#[tokio::test]
async fn snapshot_executor_version_prevents_reusing_old_success_but_keeps_current_receipts() {
    let workspace = std::env::temp_dir().join(format!("verify-version-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&workspace).unwrap();
    let c = context();
    let command = "sh \"$FREE_API_VERIFY_SCRIPT\"";
    let obligations: Vec<crate::agent::review_contract::ReviewObligation> = serde_json::from_value(json!([
        {"id":"v", "question":"verify", "verify_command":command, "verify_script":"exit 0"}
    ])).unwrap();
    let results = crate::agent::host_verify::run_obligation_verifications(&workspace, &obligations).await;
    assert!(results[0].passed);
    let frozen = FrozenVerificationRun {
        executor_version: crate::agent::host_verify::OBLIGATION_EXECUTOR_VERSION,
        run_id:"run".into(), workspace_root:workspace.canonicalize().unwrap().to_string_lossy().into_owned(),
        material_fingerprint:"material".into(), contract_fingerprint:"contract".into(),
        scope_revision:c.resource_scope.revision, effects_fingerprint:verification_effects_fingerprint(&c),
        executed_at:Utc::now().to_rfc3339(), executed_step_id:1, results,
    };
    let mut prior: FinalizationReceipt = serde_json::from_value(json!({
        "receipt_id":"prior", "diff_hash":"material", "scope_revision":c.resource_scope.revision,
        "contract_fingerprint":"contract", "step_id":1, "verification":{"status":"pending"},
        "review":{"status":"pending"}, "blockers":[], "risk_flags":[], "finish_payload":{},
        "created_at":Utc::now().to_rfc3339(), "frozen_verification_run":frozen
    })).unwrap();
    assert!(reusable_frozen_verification(Some(&prior), &c, "run", &workspace,
        "material", "contract", &obligations).is_some());
    let mut legacy = serde_json::to_value(prior.frozen_verification_run.as_ref().unwrap()).unwrap();
    legacy.as_object_mut().unwrap().remove("executor_version");
    prior.frozen_verification_run = Some(serde_json::from_value(legacy).unwrap());
    assert!(reusable_frozen_verification(Some(&prior), &c, "run", &workspace,
        "material", "contract", &obligations).is_none(), "旧错误执行器的成功不能沿用");
    std::fs::remove_dir_all(workspace).unwrap();
}
