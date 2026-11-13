use super::*;
use crate::agent::{
    checkpoint::AgentCheckpoint, context::SandwichContext, types::AgentInstruction,
};
use std::sync::{Arc, Mutex};

async fn tool(
    db: &DbState,
    id: &str,
    program: &ReviewProgram,
    inst: &AgentInstruction,
    interrupt: &mut watch::Receiver<u64>,
) -> crate::agent::types::DispatchResult {
    let registry = tokio::sync::Mutex::new(crate::agent::mcp::PluginRegistry::new());
    super::tool(db, id, program, inst, &registry, interrupt).await
}

struct Fixture {
    db: DbState,
    root: std::path::PathBuf,
    id: String,
    program: ReviewProgram,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("review-main-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("evidence.txt"), "original evidence").unwrap();
        let db = DbState(Arc::new(Mutex::new(
            crate::db::init_db(&root.join("test.db")).unwrap(),
        )));
        let id = begin_child_run_extended(
            &db,
            "parent-run",
            "owner",
            &root,
            "mock",
            0,
            "review",
            Some(3),
            None,
            None,
            &ChildRunIdentity::fresh("review"),
        )
        .unwrap();
        let program = ReviewProgram {
            parent_session: "owner".into(),
            workspace: root.clone(),
            messages: vec![json!({"role":"system","content":"review"})],
            allowed_refs: vec![],
            schema: None,
            has_verify_obligation: Some(false),
            role: Some(json!("reviewer")),
            role_label: None,
            model: "mock".into(),
            can_finalize: Some(false),
            evidence_scope_refs: vec![],
            final_fingerprint: None,
            verify_obligations: vec![],
            evidence_since: None,
            parent_step_id: None, parent_step_occurrence: None,
        };
        crate::agent::conversation::begin_turn(&db, "owner", "mock", "parent").unwrap();
        crate::agent::conversation::begin_turn(&db, &format!("task-{id}"), "mock", "review")
            .unwrap();
        prepare(&db, &id, &program).unwrap();
        Self {
            db,
            root,
            id,
            program,
        }
    }
    fn binding(&self) -> TaskBinding {
        TaskBinding {
            parent_session: "owner".into(),
            parent_run: "parent-run".into(),
            child_session: format!("task-{}", self.id),
            child_run: self.id.clone(),
        }
    }
    fn payload(&self) -> Value {
        json!({"summary":"review completed","findings":[],"confidence":"confirmed","evidence_refs":[],"missing_evidence":[],"root_cause_verdict":"na"})
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn review_program_roundtrips_and_preserves_permission_ceiling_and_legacy_resume_config() {
    let f = Fixture::new();
    f.db.0
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_child_runs SET execution_config_json=?2 WHERE id=?1",
            params![f.id, json!({"allowed_tools":["read_file"]}).to_string()],
        )
        .unwrap();
    let goal = prepare(&f.db, &f.id, &f.program).unwrap();
    assert!(goal.contains("finish"));
    assert_eq!(load(&f.db, &f.id).unwrap().unwrap().workspace, f.root);
    let config = crate::agent::execution::runtime::config(&f.db, &f.id).unwrap();
    assert_eq!(config.allowed_tools, Some(vec!["read_file".into()]));
    assert!(config.stages.for_child(true, None).plan == Some(false));
    assert!(crate::agent::execution::runtime::ExecutionConfig::parse(
        &json!({"execution":config}),
        "reviewer"
    )
    .is_ok());
}

#[tokio::test]
async fn review_of_delegated_task_shares_root_workspace_lock_without_releasing_other_holders() {
    use crate::agent::{
        child_task::{workspace_owner, TaskGuard},
        workspace_lock,
    };
    let f = Fixture::new();
    let session = format!("root-{}", f.id);
    let run = format!("run-{}", f.id);
    let child = format!("child-{}", f.id);
    let _binding = TaskGuard::enter(TaskBinding {
        parent_session: session.clone(),
        parent_run: run.clone(),
        child_session: child.clone(),
        child_run: f.id.clone(),
    });
    let root = workspace_lock::acquire_workspace_lock(&f.root, &session, &run, "root")
        .await
        .unwrap();
    let (owner, owner_run) = workspace_owner(&child, &f.id).unwrap();
    assert_eq!((&owner, &owner_run), (&session, &run));
    let reviewer1 = workspace_lock::acquire_delegation_lock(&f.root, &owner, &owner_run)
        .await
        .unwrap();
    let reviewer2 = workspace_lock::acquire_delegation_lock(&f.root, &owner, &owner_run)
        .await
        .unwrap();
    drop(reviewer1);
    drop(root);
    let still_shared = workspace_lock::acquire_delegation_lock(&f.root, &session, &run)
        .await
        .unwrap();
    drop(reviewer2);
    drop(still_shared);
}

#[test]
fn resumed_review_accepts_new_evidence_after_persistence_without_relaxing_initial_review() {
    let f = Fixture::new();
    let mut first = f.program.clone();
    first.allowed_refs = vec!["step_1: initial evidence".into()];
    first.evidence_since = Some(chrono::Utc::now());
    first.schema = Some(
        crate::agent::protocol::ContractSchema::new(vec![
            crate::agent::protocol::ContractFieldDef {
                name: "root_cause_verdict".into(),
                field_type: crate::agent::protocol::ContractFieldType::String,
                description: "病根核对".into(),
                required: true,
                allowed_values: None,
            },
        ])
        .with_strict_closed_set(false),
    );
    prepare(&f.db, &f.id, &first).unwrap();
    let mut payload = f.payload();
    payload["evidence_refs"] = json!(["step_1: initial evidence", "step_8: fresh verification"]);
    let initial = load(&f.db, &f.id).unwrap().unwrap();
    assert!(validate(&f.db, &f.id, &initial, &payload, 1)
        .unwrap_err()
        .contains("不在父 Agent"));
    let resumed_id = begin_child_run_extended(
        &f.db,
        "parent-run",
        "owner",
        &f.root,
        "mock",
        0,
        "review",
        Some(3),
        None,
        Some(&f.id),
        &ChildRunIdentity::fresh("rebuttal"),
    )
    .unwrap();
    let goal = prepare(&f.db, &resumed_id, &initial).unwrap();
    assert!(goal.contains("首次派发的引用列表不限制本轮"));
    let resumed = load(&f.db, &resumed_id).unwrap().unwrap();
    assert!(resumed.allowed_refs.is_empty());
    assert_eq!(resumed.evidence_since, first.evidence_since);
    assert_eq!(resumed.can_finalize, Some(false));
    let result = validate(&f.db, &resumed_id, &resumed, &payload, 1).unwrap();
    assert!(result["evidence_refs"]
        .as_array()
        .unwrap()
        .contains(&json!("step_8: fresh verification")));
    assert_eq!(result["plan_review_can_finalize"], false);
    let mut invalid = payload.clone();
    invalid
        .as_object_mut()
        .unwrap()
        .remove("root_cause_verdict");
    assert!(validate(&f.db, &resumed_id, &resumed, &invalid, 1).is_err());
    // Compare the same result against the existing legacy follow-up contract.
    let mut legacy_payload = payload.clone();
    legacy_payload.as_object_mut().unwrap().remove("summary");
    let legacy = build_review_delivery(
        &legacy_payload.to_string(),
        ReviewDeliveryContext {
            child_run_id: &resumed_id,
            allowed_refs: &[],
            schema: resumed.schema.as_ref(),
            has_verify_obligation: resumed.has_verify_obligation,
            kind: Some("review"),
            duration_ms: 1,
            completion_tokens: 0,
            prompt_tokens: 0,
            role: resumed.role.as_ref(),
            role_label: resumed.role_label.as_ref(),
            model: &resumed.model,
            plan_review_can_finalize: resumed.can_finalize,
        },
    )
    .unwrap();
    assert_eq!(result, legacy);
    assert_eq!(
        load(&f.db, &f.id).unwrap().unwrap().allowed_refs,
        first.allowed_refs
    );
}

#[tokio::test]
async fn general_dispatch_cannot_expand_review_into_write_shell_or_nested_agents() {
    let f = Fixture::new();
    let (_tx, mut interrupt) = watch::channel(0);
    for value in [
        json!({"tool":"local_file","action":"write","params":{"path":"evidence.txt","content":"overwrite"}}),
        json!({"tool":"child_writer","params":{"task":"write"}}),
        json!({"tool":"run_command","params":{"command":"echo changed > evidence.txt"}}),
        json!({"tool":"parallel","parallel":[{"tool":"local_file","action":"write","params":{"path":"evidence.txt","content":"changed"}}]}),
    ] {
        let inst: AgentInstruction = serde_json::from_value(value).unwrap();
        assert!(
            !tool(&f.db, &f.id, &f.program, &inst, &mut interrupt)
                .await
                .success
        );
    }
    assert_eq!(
        std::fs::read_to_string(f.root.join("evidence.txt")).unwrap(),
        "original evidence"
    );
    let read = serde_json::from_value(json!({"tool":"read_file","params":{"path":"evidence.txt"}}))
        .unwrap();
    let result = tool(&f.db, &f.id, &f.program, &read, &mut interrupt).await;
    assert!(result.success, "{}", result.stderr);
    assert!(result.stdout.contains("original evidence"));
}

#[test]
fn validated_review_cannot_forge_host_identity_or_multi_member_finalization() {
    let f = Fixture::new();
    let mut payload = f.payload();
    payload["child_run_id"] = json!("forged");
    payload["plan_review_can_finalize"] = json!(true);
    let result = validate(&f.db, &f.id, &f.program, &payload, 12).unwrap();
    assert_eq!(result["child_run_id"], f.id);
    assert_eq!(result["plan_review_can_finalize"], false);
    payload["confidence"] = json!("wrong");
    assert!(validate(&f.db, &f.id, &f.program, &payload, 12).is_err());
    let mut scoped = f.program.clone();
    scoped.evidence_scope_refs = vec!["src/a.rs".into()];
    prepare(&f.db, &f.id, &scoped).unwrap();
    assert!(validate(&f.db, &f.id, &scoped, &f.payload(), 1).is_err());
    assert!(validate(&f.db, &f.id, &scoped, &f.payload(), 1).is_err());
    assert_eq!(
        validate(&f.db, &f.id, &scoped, &f.payload(), 1).unwrap()
            ["evidence_scope_coverage_incomplete"],
        true
    );
}

#[test]
fn readonly_role_cannot_schedule_verification_or_authorization_through_instruction_metadata() {
    let inst:AgentInstruction=serde_json::from_value(json!({"tool":"read_file","params":{"path":"evidence.txt"},"review_contract_update":{"scope_statement":"injected","obligations":[{"id":"bad","question":"write","status":"open","verify_command":"echo changed > evidence.txt","closure_form":"command"}]}})).unwrap();
    let restricted = restrict_instruction(inst);
    assert!(restricted.review_contract_update.is_none());
    assert_eq!(restricted.get_tool(), "review_profile_reject");
    assert!(restricted.target_blocks_update.is_none());
}

#[tokio::test]
async fn review_delivery_requires_atomic_main_terminal_and_cancelled_record_is_not_overwritten() {
    let f = Fixture::new();
    let binding = f.binding();
    assert_eq!(
        settle(&f.db, &binding, &Ok(()), 1).unwrap()["status"],
        "failed"
    );
    for cancelled in [false, true] {
        let f = Fixture::new();
        let binding = f.binding();
        let mut context = SandwichContext::new("system".into(), "review".into());
        let mut cp = AgentCheckpoint::new(binding.child_session.clone(), &context);
        let inst = serde_json::from_value(json!({"tool":"finish","params":f.payload()})).unwrap();
        let app = tauri::test::mock_app();
        let (_tx, mut rx) = watch::channel(0);
        let evaluation = crate::agent::finalization::evaluate_completion(
            app.handle(),
            &f.db,
            &mut context,
            &mut cp,
            &f.id,
            &binding.child_session,
            1,
            &f.root,
            &crate::agent::reviewer_gatekeeper::GitBaseline::non_git(),
            chrono::Utc::now(),
            &Default::default(),
            &mut rx,
            &inst,
            crate::agent::finalization::CompletionMode::Finish,
            false,
            false,
        )
        .await;
        assert!(evaluation.receipt.is_ready(), "{:?}", evaluation.receipt);
        let receipt_id = evaluation.receipt.receipt_id.clone();
        cp.pending_finalization = Some(evaluation.receipt);
        cp.mark_done(&context);
        crate::agent::finalization::commit_finalization(
            &f.db,
            &mut cp,
            &f.id,
            &binding.child_session,
            "review",
            &[],
            &json!({"summary":"review done","finalization_receipt":receipt_id}),
            "review done",
            "mock",
        )
        .unwrap();
        // A committed model execution cannot undo an earlier explicit cancellation.
        if cancelled {
            crate::agent::review_control::cancel_child_on(&f.db.0.lock().unwrap(), &f.id).unwrap();
        }
        let body = settle(&f.db, &binding, &Ok(()), 10).unwrap();
        if !cancelled {
            assert_eq!(body["child_run_id"], f.id);
            assert_eq!(body["plan_review_can_finalize"], false);
        }
        let status: String =
            f.db.0
                .lock()
                .unwrap()
                .query_row(
                    "SELECT status FROM agent_child_runs WHERE id=?1",
                    [&f.id],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(status, if cancelled { "cancelled" } else { "succeeded" });
    }
}

#[tokio::test]
async fn failed_final_recheck_survives_a_passed_model_verdict() {
    let f = Fixture::new();
    let mut program = f.program.clone();
    program.final_fingerprint = Some("fingerprint".into());
    program.verify_obligations=vec![serde_json::from_value(json!({"id":"verify-one","question":"verify","status":"open","verify_command":"exit 7","closure_form":"command"})).unwrap()];
    prepare(&f.db, &f.id, &program).unwrap();
    let (_tx, mut interrupt) = watch::channel(0);
    let inst =
        serde_json::from_value(json!({"tool":"verify","params":{"id":"verify-one"}})).unwrap();
    assert!(
        tool(&f.db, &f.id, &program, &inst, &mut interrupt)
            .await
            .success
    );
    let result=validate(&f.db,&f.id,&program,&json!({"verdict":"passed","score":100,"summary":"fine","blocking_issues":[],"nit_issues":[],"missing_evidence":[]}),1).unwrap();
    let mut snapshot = Snapshot::load(&f.db, &f.id, "reviewer").unwrap().unwrap();
    snapshot.state["verification_pending"] = json!(true);
    snapshot.save(&f.db).unwrap();
    let retry = tool(&f.db, &f.id, &program, &inst, &mut interrupt).await;
    assert!(!retry.success);
    assert!(retry.stderr.contains("未知"));
    assert_eq!(result["verdict"], "rejected");
    assert!(!result["blocking_issues"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn reviewer_navigates_middle_steps_and_child_results_with_scoped_output_alias() {
    let f = Fixture::new();
    let now = chrono::Utc::now();
    let mut program = f.program.clone();
    program.evidence_since = Some(now - chrono::Duration::seconds(1));
    let proof = crate::agent::run_trace::save_step_output_with_meta(
        &f.db, "owner", 33, "local_shell", "middle proof", "succeeded", "1521 passed",
    ).unwrap();
    {
        let c=f.db.0.lock().unwrap();
        for (id,session) in [("parent-run","owner"),("descendant","task-descendant"),("outsider","other")] {
            c.execute("INSERT INTO agent_run_traces(id,session_id,goal,status,steps_json,started_at,updated_at) VALUES (?1,?2,'goal','running','[]',?3,?3)",params![id,session,now.to_rfc3339()]).unwrap();
        }
        for seq in 0..55 {
            c.execute("INSERT INTO agent_run_steps(run_id,seq,step_json) VALUES ('parent-run',?1,?2)",params![seq,json!({"id":seq,"action":"local_shell","description":format!("step {seq}"),"status":"succeeded","output_id":if seq==33 {proof.clone()} else {format!("proof-{seq}")}}).to_string()]).unwrap();
        }
        c.execute("INSERT INTO agent_child_runs(id,parent_run_id,parent_session_id,workspace_root,model_id,status,started_at,kind,result_json) VALUES ('descendant','parent-run','owner',?1,'mock','succeeded',?2,'task',?3)",params![f.root.to_string_lossy(),now.to_rfc3339(),json!({"summary":"child result"}).to_string()]).unwrap();
    }
    let (_tx,mut rx)=watch::channel(0);
    let run = |req: Value| serde_json::from_value::<AgentInstruction>(req).unwrap();
    let page=tool(&f.db,&f.id,&program,&run(json!({"tool":"list_execution_steps","params":{"after_seq":29,"limit":10}})),&mut rx).await;
    assert!(page.success,"{}",page.stderr);
    let page:Value=serde_json::from_str(&page.stdout).unwrap();
    assert_eq!(page["steps"][3]["output_id"],proof);
    assert_eq!(page["next_after_seq"],39);
    assert_eq!(page["has_more"],true);
    for name in ["read_step_output","get_step_output"] {
        let got=tool(&f.db,&f.id,&program,&run(json!({"tool":name,"params":{"output_id":proof}})),&mut rx).await;
        assert!(got.success,"{}",got.stderr);assert!(got.stdout.contains("1521 passed"));
        let returned: Value = serde_json::from_str(&got.stdout).unwrap();
        assert_eq!(returned["read_scope"]["kind"], "single_output");
        assert_eq!(returned["read_scope"]["output_id"], proof);
    }
    let children=tool(&f.db,&f.id,&program,&run(json!({"tool":"list_child_runs"})),&mut rx).await;
    assert!(children.success,"{}",children.stderr);assert!(children.stdout.contains("descendant"));
    let old_fingerprint:Value=serde_json::from_str(&children.stdout).unwrap();
    f.db.0.lock().unwrap().execute("UPDATE agent_child_runs SET result_json=?1 WHERE id='descendant'",[json!({"summary":"child result updated"}).to_string()]).unwrap();
    let changed=tool(&f.db,&f.id,&program,&run(json!({"tool":"list_child_runs","params":{"limit":1}})),&mut rx).await;
    assert!(changed.success,"{}",changed.stderr);
    let changed:Value=serde_json::from_str(&changed.stdout).unwrap();
    assert_ne!(old_fingerprint["state_fingerprint"],changed["state_fingerprint"]);
    assert_eq!(changed["has_more"],true);
    let child=tool(&f.db,&f.id,&program,&run(json!({"tool":"read_child_result","params":{"child_run_id":"descendant"}})),&mut rx).await;
    assert!(child.success,"{}",child.stderr);assert!(child.stdout.contains("child result"));
    let child_proof=crate::agent::run_trace::save_step_output_with_meta(&f.db,"task-descendant",1,"read_file","child","succeeded","child evidence").unwrap();
    let got=tool(&f.db,&f.id,&program,&run(json!({"tool":"get_step_output","params":{"run_id":"descendant","output_id":child_proof}})),&mut rx).await;
    assert!(got.success,"{}",got.stderr);assert!(got.stdout.contains("child evidence"));
    for req in [json!({"tool":"list_execution_steps","params":{"run_id":"outsider"}}),json!({"tool":"get_step_output","params":{"run_id":"outsider","output_id":proof}}),json!({"tool":"read_child_result","params":{"child_run_id":"outsider"}})] {
        assert!(!tool(&f.db,&f.id,&program,&run(req),&mut rx).await.success);
    }
}

#[tokio::test]
async fn parallel_review_reads_share_permissions_budget_and_branch_receipts() {
    let f = Fixture::new();
    let mut program = f.program.clone();
    program.evidence_since = Some(chrono::Utc::now() - chrono::Duration::seconds(1));
    program.final_fingerprint = Some("final-review".into());
    let mut config = crate::agent::execution::runtime::config(&f.db, &f.id).unwrap();
    config.allowed_tools = Some(vec!["read_file".into(), "get_step_output".into(), "verify".into()]);
    f.db.0.lock().unwrap().execute(
        "UPDATE agent_child_runs SET execution_config_json=?2 WHERE id=?1",
        params![f.id, serde_json::to_string(&config).unwrap()],
    ).unwrap();
    let proof = crate::agent::run_trace::save_step_output_with_meta(
        &f.db, "owner", 2, "read_file", "parent proof", "succeeded", "parent evidence",
    ).unwrap();
    let (_tx, mut rx) = watch::channel(0);
    let inst: AgentInstruction = serde_json::from_value(json!({"parallel":[
        {"id":"file","tool":"read_file","params":{"path":"evidence.txt"}},
        {"action":"get_step_output","args":{"output_id":proof}},
        {"tool":"get_step_output","params":{"run_id":"outsider","output_id":proof}},
        {"tool":"local_file","action":"write","params":{"path":"evidence.txt","content":"changed"}},
        {"tool":"verify"}
    ]})).unwrap();
    let mut result = tool(&f.db, &f.id, &program, &inst, &mut rx).await;
    assert!(!result.success);
    assert!(result.stderr.contains("original evidence"));
    assert!(result.stderr.contains("不能读取其他任务的回执"));
    let durable = crate::agent::dispatcher::take_tool_durable_output(&mut result, "");
    let select = |id| crate::agent::plan_tree::output::select_branch(
        &durable, &json!({"branch_id":id}),
    ).unwrap().unwrap().text;
    assert!(select("file").contains("original evidence"));
    assert_eq!(
        serde_json::from_str::<Value>(&select("$1")).unwrap()["content"],
        "parent evidence"
    );
    assert!(select("$2").contains("不能读取其他任务的回执"));
    assert!(select("$3").contains("执行配置未允许"));
    assert!(select("$4").contains("复验和提交结果需单独调用"));
    assert_eq!(std::fs::read_to_string(f.root.join("evidence.txt")).unwrap(), "original evidence");
    let snapshot = Snapshot::load(&f.db, &f.id, "reviewer").unwrap().unwrap();
    assert_eq!(snapshot.state["tool_steps"], 3);
    assert_ne!(snapshot.state["verification_pending"], true);

    let binding = f.binding();
    let now = chrono::Utc::now().to_rfc3339();
    f.db.0.lock().unwrap().execute(
        "INSERT INTO agent_run_traces(id,session_id,goal,status,steps_json,started_at,updated_at) VALUES (?1,?2,'review','running','[]',?3,?3)",
        params![binding.child_run, binding.child_session, now],
    ).unwrap();
    assert!(durable.contains(&format!("run_id={:?}", binding.child_run)));
    let batch_receipt = crate::agent::run_trace::save_step_output_with_meta(
        &f.db, &binding.child_session, 1, "parallel", "review batch", "failed", &durable,
    ).unwrap();
    let branch = serde_json::from_value(json!({"tool":"get_step_output","params":{
        "output_id":batch_receipt,"run_id":binding.child_run,"branch_id":"file"
    }})).unwrap();
    let result = tool(&f.db, &f.id, &program, &branch, &mut rx).await;
    assert!(result.success, "{}", result.stderr);
    assert!(result.stdout.contains("original evidence"));

    let read = json!({"tool":"read_file","params":{"path":"evidence.txt"}});
    let batch = serde_json::from_value(json!({"parallel":[read,read]})).unwrap();
    let result = tool(&f.db, &f.id, &program, &batch, &mut rx).await;
    assert!(result.success);
    assert!(result.stdout.contains("成功: 2, 失败: 0"));

    let budget = crate::agent::review_tools::forensic_budget();
    let mut snapshot = Snapshot::load(&f.db, &f.id, "reviewer").unwrap().unwrap();
    snapshot.state["tool_steps"] = json!(budget - 1);
    snapshot.save(&f.db).unwrap();
    let oversized = vec![read.clone(); crate::agent::plan_tree_dispatch::MAX_CONTROLLED_PARALLEL_BRANCHES + 1];
    let rejected = serde_json::from_value(json!({"parallel":oversized})).unwrap();
    let result = tool(&f.db, &f.id, &program, &rejected, &mut rx).await;
    assert!(result.stderr.contains("单批最多"));
    let result = tool(&f.db, &f.id, &program, &batch, &mut rx).await;
    assert!(result.stdout.contains("成功: 1, 失败: 1"));
    assert!(result.stdout.contains("只读取证预算已用尽"));
    assert_eq!(
        Snapshot::load(&f.db, &f.id, "reviewer").unwrap().unwrap().state["tool_steps"],
        budget
    );
}

#[tokio::test]
async fn scalar_review_tool_parameters_return_error_without_panicking_or_spending_budget() {
    let f = Fixture::new();
    let (_tx, mut rx) = watch::channel(0);
    for request in [
        json!({"tool":"run_command","command":"git status --short"}),
        json!({"tool":"run_command","params":"git status --short"}),
        json!({"tool":"run_command","params":["bad"]}),
        json!({"tool":"run_command","params":42}),
    ] {
        let inst: AgentInstruction = serde_json::from_value(request).unwrap();
        let result = tool(&f.db, &f.id, &f.program, &inst, &mut rx).await;
        assert!(!result.success);
        assert!(result.stderr.contains("JSON 对象"));
        let snapshot = Snapshot::load(&f.db, &f.id, "reviewer").unwrap().unwrap();
        assert_eq!(snapshot.state["tool_steps"].as_u64().unwrap_or(0), 0);
    }
    assert_eq!(
        std::fs::read_to_string(f.root.join("evidence.txt")).unwrap(),
        "original evidence"
    );
}

#[test]
fn reviewer_handoff_stages_are_scoped_and_do_not_disable_the_parent_workflow() {
    let f = Fixture::new();
    let context = SandwichContext::new("sys".into(), "integration test".into());
    let mut parent = AgentCheckpoint::new("owner".into(), &context);
    parent.stages.stage_review = Some(true);
    parent.stages.final_review = Some(true);
    let mut verdict = crate::agent::triage::TriageVerdict::fallback("fixture");
    verdict.final_review_mode = "operational_verification".into();
    parent.triage_verdict = Some(verdict);
    crate::agent::checkpoint::save_checkpoint(&f.db, &mut parent).unwrap();
    let mut program = f.program.clone();
    program.messages.push(json!({"role":"user","content":crate::agent::review_team::parent_workflow_context(&parent)}));
    let goal = prepare(&f.db, &f.id, &program).unwrap();
    assert!(goal.contains("被审查父任务的流程配置快照"));
    assert!(goal.contains("operational_verification"));
    let config = crate::agent::execution::runtime::config(&f.db, &f.id).unwrap();
    let child_stages = config.stages.for_child(true, None);
    assert_eq!(child_stages.final_review, Some(false));
    let prompt = child_stages.reviewer_prompt();
    assert!(prompt.contains("Reviewer 子任务的内部配置"));
    assert!(!prompt.contains("【宿主固定环节配置】"));
    let restored = crate::agent::checkpoint::load_checkpoint(&f.db, "owner")
        .unwrap()
        .unwrap();
    assert_eq!(restored.stages.final_review, Some(true));
    assert_eq!(restored.stages.stage_review, Some(true));
    assert_eq!(
        restored.triage_verdict.unwrap().final_review_mode,
        "operational_verification"
    );
}

#[tokio::test]
async fn reviewer_prepare_receives_the_same_review_payload_as_finish() {
    let f = Fixture::new();
    let app = tauri::test::mock_app();
    let baseline = crate::agent::reviewer_gatekeeper::GitBaseline::capture(&f.root);
    for prepare in [true, false] {
        let payload = f.payload();
        let inst: AgentInstruction = serde_json::from_value(if prepare {
            json!({"prepare_finish":payload})
        } else {
            json!({"tool":"finish", "params":payload})
        }).unwrap();
        let mut context = SandwichContext::new("system".into(), "review".into());
        let mut checkpoint = AgentCheckpoint::new(format!("task-{}", f.id), &context);
        let (_tx, mut rx) = tokio::sync::watch::channel(0);
        let result = crate::agent::finalization::evaluate_completion(
            app.handle(), &f.db, &mut context, &mut checkpoint, &f.id,
            &format!("task-{}", f.id), 1, &f.root, &baseline, chrono::Utc::now(),
            &crate::agent::router::ModelRoutingConfig::default(), &mut rx, &inst,
            if prepare { crate::agent::finalization::CompletionMode::Prepare }
            else { crate::agent::finalization::CompletionMode::Finish }, false, false,
        ).await;
        assert_eq!(result.receipt.finish_payload, payload);
        assert!(result.receipt.is_ready(), "prepare={prepare}: {:?}", result.receipt.blockers);
    }
}

#[tokio::test]
async fn configured_mcp_tools_are_shared_by_single_and_parallel_review_calls() {
    use crate::agent::mcp::{McpClient, PluginRegistry, PluginSource};

    let f = Fixture::new();
    let name = format!("review-test-{}", Uuid::new_v4());
    let calls = f.root.join("mcp-calls");
    let server = r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if 'id' not in req: continue
    if req['method'] == 'initialize':
        result = {'protocolVersion':'2024-11-05','capabilities':{'tools':{}},'serverInfo':{'name':'review-fixture','version':'1'}}
    elif req['method'] == 'tools/list':
        result = {'tools':[
            {'name':'lookup','inputSchema':{'type':'object'}},
            {'name':'unapproved','inputSchema':{'type':'object'},'annotations':{'readOnlyHint':True}},
        ]}
    elif req['method'] == 'tools/call':
        with open(sys.argv[1], 'a') as log: log.write(req['params']['name']+'\n')
        result = {'structuredContent':{'evidence':'MCP-REVIEW-EVIDENCE','args':req['params']['arguments']}}
    else:
        result = {}
    print(json.dumps({'jsonrpc':'2.0','id':req['id'],'result':result}), flush=True)
"#;
    let args = vec!["-c", server, calls.to_str().unwrap()];
    f.db.0.lock().unwrap().execute(
        "INSERT INTO mcp_plugins (id,name,command,args,created_at) VALUES (?1,?1,'python3',?2,'test')",
        params![name, serde_json::to_string(&args).unwrap()],
    ).unwrap();
    let config = crate::commands::mcp::enabled_mcp_config(&f.db, &name).unwrap();
    let client = Arc::new(McpClient::spawn(config.clone()).unwrap());
    client.list_tools().await.unwrap();
    let mut registry = PluginRegistry::new();
    registry.insert_with_source(name.clone(), client, PluginSource::ManagedDb).unwrap();
    let lookup = format!("{name}/lookup");
    let unapproved = format!("{name}/unapproved");
    let mut execution = crate::agent::execution::runtime::config(&f.db, &f.id).unwrap();
    let registry = tokio::sync::Mutex::new(registry);
    let (_tx, mut rx) = watch::channel(0);
    let single: AgentInstruction = serde_json::from_value(json!({
        "tool":lookup,"params":{"query":"record"}
    })).unwrap();
    let result = super::tool(&f.db, &f.id, &f.program, &single, &registry, &mut rx).await;
    assert!(!result.success && result.stderr.contains("未获审查取证授权"));
    assert!(!calls.exists());
    assert!(mcp_tools::set_configured_tools(&f.db, &config, vec!["nonexistent".into()]).is_err());
    mcp_tools::set_configured_tools(&f.db, &config, vec!["lookup".into()]).unwrap();

    {
        let registry = registry.lock().await;
        let (index, menu) = mcp_tools::menu(&f.db, &registry, &execution);
        assert_eq!(index, lookup);
        assert!(menu.contains(&lookup));
        assert!(!menu.contains(&unapproved));
        execution.allowed_tools = Some(vec!["read_file".into()]);
        assert_eq!(mcp_tools::menu(&f.db, &registry, &execution), (String::new(), String::new()));
    }
    let save_execution = |execution: &crate::agent::execution::runtime::ExecutionConfig| {
        f.db.0.lock().unwrap().execute(
            "UPDATE agent_child_runs SET execution_config_json=?2 WHERE id=?1",
            params![f.id, serde_json::to_string(execution).unwrap()],
        ).unwrap();
    };
    save_execution(&execution);
    let result = super::tool(&f.db, &f.id, &f.program, &single, &registry, &mut rx).await;
    assert!(!result.success && result.stderr.contains("执行配置未允许"));
    assert!(!calls.exists());
    execution.allowed_tools.as_mut().unwrap().push(format!("{lookup}.read"));
    save_execution(&execution);
    let action_alias = serde_json::from_value(json!({"tool":lookup,"action":"read"})).unwrap();
    let result = super::tool(&f.db, &f.id, &f.program, &action_alias, &registry, &mut rx).await;
    assert!(!result.success && result.stderr.contains("执行配置未允许"));
    assert!(!calls.exists());
    execution.allowed_tools.as_mut().unwrap().push(lookup.clone());
    save_execution(&execution);

    let result = super::tool(&f.db, &f.id, &f.program, &single, &registry, &mut rx).await;
    assert!(result.success, "{}", result.stderr);
    let payload: Value = serde_json::from_str(&result.stdout).unwrap();
    assert_eq!(payload["structuredContent"]["args"]["query"], "record");

    let batch = serde_json::from_value(json!({"parallel":[
        {"tool":lookup,"params":{"query":"batch"}},
        {"tool":unapproved},
        {"tool":"read_file","params":{"path":"evidence.txt"}}
    ]})).unwrap();
    let result = super::tool(&f.db, &f.id, &f.program, &batch, &registry, &mut rx).await;
    assert!(!result.success);
    assert!(result.stdout.contains("MCP-REVIEW-EVIDENCE"));
    assert!(result.stdout.contains("original evidence"));
    assert!(result.stdout.contains("未获审查取证授权"));

    mcp_tools::set_configured_tools(&f.db, &config, vec![]).unwrap();
    let result = super::tool(&f.db, &f.id, &f.program, &single, &registry, &mut rx).await;
    assert!(!result.success && result.stderr.contains("未获审查取证授权"));
    mcp_tools::set_configured_tools(&f.db, &config, vec!["lookup".into()]).unwrap();
    f.db.0.lock().unwrap().execute("UPDATE mcp_plugins SET enabled=0 WHERE name=?1", [&name]).unwrap();
    let result = super::tool(&f.db, &f.id, &f.program, &single, &registry, &mut rx).await;
    assert!(!result.success);
    f.db.0.lock().unwrap().execute("UPDATE mcp_plugins SET enabled=1 WHERE name=?1", [&name]).unwrap();
    let mut changed_config = config.clone();
    changed_config.args.push("changed".into());
    f.db.0.lock().unwrap().execute(
        "UPDATE mcp_plugins SET args=?2 WHERE name=?1",
        params![name, serde_json::to_string(&changed_config.args).unwrap()],
    ).unwrap();
    let result = super::tool(&f.db, &f.id, &f.program, &single, &registry, &mut rx).await;
    assert!(!result.success && result.stderr.contains("未获审查取证授权"));
    mcp_tools::set_configured_tools(&f.db, &changed_config, vec![]).unwrap();
    assert!(mcp_tools::configured_tools(&f.db, &changed_config).unwrap().is_empty());
    assert_eq!(std::fs::read_to_string(calls).unwrap(), "lookup\nlookup\n");
    let snapshot = Snapshot::load(&f.db, &f.id, "reviewer").unwrap().unwrap();
    assert_eq!(snapshot.state["tool_steps"], 3);
}

#[tokio::test]
async fn review_tree_overview_spans_branches_but_evidence_remains_scoped() {
    let f = Fixture::new();
    let now = chrono::Utc::now().to_rfc3339();
    {
        let conn = f.db.0.lock().unwrap();
        for (id, session) in [("root-run", "root-session"), ("parent-run", "owner"), ("sibling", "task-sibling"), ("other", "other-session")] {
            conn.execute("INSERT INTO agent_run_traces(id,session_id,goal,status,steps_json,started_at,updated_at) VALUES (?1,?2,'goal','running','[]',?3,?3)", params![id,session,now]).unwrap();
        }
        for id in ["parent-run", "sibling"] {
            conn.execute("INSERT INTO agent_child_runs(id,parent_run_id,parent_session_id,workspace_root,model_id,status,started_at,kind,result_json) VALUES (?1,'root-run','root-session',?2,'mock','succeeded',?3,'task','{\"summary\":\"private\"}')",params![id,f.root.to_string_lossy(),now]).unwrap();
        }
    }
    let (_tx, mut rx) = watch::channel(0);
    let request = |value| serde_json::from_value::<AgentInstruction>(value).unwrap();
    let result = tool(&f.db, &f.id, &f.program, &request(json!({"tool":"list_task_tree","params":{"limit":100}})), &mut rx).await;
    assert!(result.success, "{}", result.stderr);
    let page: Value = serde_json::from_str(&result.stdout).unwrap();
    let tasks = page["tasks"].as_array().unwrap();
    assert!(tasks.iter().any(|t| t["run_id"] == "root-run"));
    assert!(tasks.iter().any(|t| t["run_id"] == "sibling"));
    assert!(!tasks.iter().any(|t| t["run_id"] == "other"));
    assert!(tasks.iter().all(|t| t.get("execution_config").is_none()));
    let forbidden = tool(&f.db, &f.id, &f.program, &request(json!({"tool":"read_child_result","params":{"child_run_id":"sibling"}})), &mut rx).await;
    assert!(!forbidden.success);
    let parent = tool(&f.db, &f.id, &f.program, &request(json!({"tool":"list_execution_steps","params":{"run_id":"parent-run"}})), &mut rx).await;
    assert!(parent.success, "{}", parent.stderr);
}

#[test]
fn exhausted_instruction_recovery_is_recorded_as_invalid_output() {
    let f = Fixture::new();
    let mut snapshot = Snapshot::load(&f.db, &f.id, "reviewer").unwrap().unwrap();
    snapshot.state["terminal_failure_kind"] = json!("invalid_output");
    snapshot.save(&f.db).unwrap();
    settle(&f.db, &f.binding(), &Err("指令恢复已耗尽".into()), 1).unwrap();
    let conn = f.db.0.lock().unwrap();
    let kind: String = conn.query_row("SELECT error_kind FROM agent_child_runs WHERE id=?1", [&f.id], |row| row.get(0)).unwrap();
    assert_eq!(kind, "invalid_output");
}
