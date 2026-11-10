use super::*;
use std::sync::{Arc, Mutex};
fn fixture() -> DbState {
    let db = DbState(Arc::new(Mutex::new(
        crate::db::init_db(&std::path::PathBuf::from(":memory:")).unwrap(),
    )));
    db.0.lock().unwrap().execute("INSERT INTO agent_child_runs(id,parent_run_id,parent_session_id,workspace_root,model_id,status,started_at,kind) VALUES ('child','parent','session','/tmp','mock','running','now','worker')",[]).unwrap();
    db
}

#[test]
fn supported_grants_intersect_the_same_way_as_runtime_authorization() {
    let db=fixture();
    let cases=[json!(null),json!([]),json!(["read_file"]),json!(["local_file"]),json!(["local_file.read"]),json!(["child_task"]),json!(["send_message"]),json!(["my_mcp/tool"]),json!(["my_mcp/other"])];
    let calls=[("local_file","read"),("local_file","write"),("child_task","send"),("child_task","dispatch"),("child_task","result"),("my_mcp/tool","inspect"),("my_mcp/tool","change"),("my_mcp/tool_extra","inspect")];
    let authorized=|config:&ExecutionConfig,tool:&str,action:&str| {
        db.0.lock().unwrap().execute("UPDATE agent_child_runs SET execution_config_json=?1 WHERE id='child'",[serde_json::to_string(config).unwrap()]).unwrap();
        authorize_call(&db,"child",tool,action,&json!({})).is_ok()
    };
    for requested in &cases {for limit in &cases {
        let original=ExecutionConfig::parse(&json!({"execution":{"allowed_tools":requested}}),"task").unwrap();
        let ceiling=ExecutionConfig::parse(&json!({"execution":{"allowed_tools":limit}}),"task").unwrap();
        let mut result=original.clone();result.restrict_tools(&ceiling);
        for (tool,action) in calls {
            assert_eq!(authorized(&result,tool,action),authorized(&original,tool,action)&&authorized(&ceiling,tool,action),"requested={requested}, ceiling={limit}, call={tool}.{action}");
        }
    }}
}

#[test]
fn nested_registration_cannot_regain_tools_and_does_not_copy_parent_workflow() {
    let db=fixture();
    db.0.lock().unwrap().execute("UPDATE agent_child_runs SET execution_config_json=?1 WHERE id='child'",[json!({"allowed_tools":["local_file","my_mcp/tool","send_message"],"workflow":{"plan_mode":"plan_approval"},"stages":{"final_review":true}}).to_string()]).unwrap();
    let mut parent="child".to_string();
    for (index,tools) in [json!(["read_file","my_mcp/tool","child_task"]),json!(["local_file","my_mcp/tool","my_mcp/other","run_command","child_task"]),json!([]),json!(null)].into_iter().enumerate() {
        let mut identity=crate::agent::child_reviewer::ChildRunIdentity::fresh("task");
        identity.execution_config=Some(json!({"allowed_tools":tools,"workflow":{"completion":"handoff"},"stages":{"final_review":false}}).to_string());
        let id=crate::agent::child_reviewer::begin_child_run_extended(&db,&parent,"session",&std::env::temp_dir(),"mock",0,"task",None,None,None,&identity).unwrap();
        let saved=config(&db,&id).unwrap();
        assert_eq!(saved.workflow.completion.as_deref(),Some("handoff"));
        assert!(saved.workflow.plan_mode.is_none(),"parent workflow is not a child default");
        assert_eq!(saved.stages.final_review,Some(false));
        assert_eq!(authorize(&db,&id,"read_file").is_ok(),index<2);
        assert_eq!(authorize_call(&db,&id,"my_mcp/tool","inspect",&json!({})).is_ok(),index<2);
        assert!(authorize(&db,&id,"write_file").is_err());
        assert!(authorize(&db,&id,"run_command").is_err());
        assert_eq!(authorize_call(&db,&id,"my_mcp/tool","change",&json!({})).is_ok(),index<2);
        assert!(authorize_call(&db,&id,"my_mcp/other","inspect",&json!({})).is_err());
        assert_eq!(authorize_call(&db,&id,"child_task","send",&json!({})).is_ok(),index<2);
        assert!(authorize_call(&db,&id,"child_task","result",&json!({})).is_ok());
        parent=id;
    }
}

#[test]
fn dotted_mcp_tool_names_are_not_expanded_as_native_action_aliases() {
    let config=ExecutionConfig::parse(&json!({"execution":{"allowed_tools":["my_mcp/tool"]}}),"task").unwrap();
    assert!(!config.permits("my_mcp/tool.inspect"));
    assert!(!config.permits("my_mcp/tool_extra"));
    assert!(config.permits("my_mcp/tool"));
}
#[test]
fn dynamic_configuration_is_typed_and_unsupported_workflows_are_explicit() {
    assert!(ExecutionConfig::parse(
        &json!({"execution":{"allowed_tools":"read_file"}}),
        "worker"
    )
    .is_err());
    assert!(ExecutionConfig::parse(
        &json!({"execution":{"workflow":{"discussion":false}}}),
        "worker"
    )
    .is_err());
    assert!(ExecutionConfig::parse(
        &json!({"execution":{"workflow":{"plan_mode":"cross_review","completion":"delivery"}}}),
        "task"
    )
    .is_ok());
    assert!(ExecutionConfig::parse(
        &json!({"execution":{"workspace":"isolated","workflow":{"completion":"delivery"}}}),
        "writer"
    )
    .is_err());
    let c = ExecutionConfig::parse(&json!({"execution":{"allowed_tools":[]}}), "worker").unwrap();
    assert!(!c.permits("run_command"));
    assert!(c.permits("finish"));
}
#[test]
fn tools_and_actions_are_restricted_by_the_registered_configuration() {
    let db = fixture();
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE agent_child_runs SET execution_config_json=?1 WHERE id='child'",
            [json!({"allowed_tools":["local_file.read","send_message"]}).to_string()],
        )
        .unwrap();
    assert!(authorize_call(&db, "child", "local_file", "", &json!({"path":"x"})).is_ok());
    assert!(authorize_call(
        &db,
        "child",
        "local_file",
        "write",
        &json!({"path":"x","content":"bad"})
    )
    .is_err());
    assert!(authorize(&db, "child", "read_file").is_ok());
    assert!(authorize(&db, "child", "write_file").is_err());
    assert!(authorize(&db, "child", "send_message").is_ok());
    assert!(authorize(&db, "child", "run_command").is_err());
    assert!(authorize_call(&db, "child", "child_task", "list", &json!({})).is_ok());
    assert!(authorize_call(
        &db,
        "child",
        "child_task",
        "dispatch",
        &json!({"task":"new"})
    )
    .is_err());
    assert!(authorize(&db, "unregistered-main", "run_command").is_ok());
}
#[test]
fn independent_checkpoints_reject_stale_writes_and_preserve_prepared_action_identity() {
    let db = fixture();
    let mut a = Snapshot::new(
        "child",
        "worker",
        json!({"pending":"stable-call","messages":[{"role":"user","content":"evidence"}]}),
    );
    a.save(&db).unwrap();
    let mut stale = a.clone();
    a.state["pending"] = Value::Null;
    a.save(&db).unwrap();
    stale.state["pending"] = json!("wrong");
    assert!(stale.save(&db).is_err());
    assert_eq!(stale.version, 1);
    let saved = Snapshot::load(&db, "child", "worker").unwrap().unwrap();
    assert_eq!(saved.version, 2);
    assert!(saved.state["pending"].is_null());
    assert!(Snapshot::load(&db, "child", "writer").is_err());
}
#[test]
fn acknowledged_mail_and_recoverable_context_commit_together() {
    let db = fixture();
    db.0.lock().unwrap().execute("INSERT INTO agent_messages(id,session_id,root_run_id,sender_run_id,sender_session_id,sender_kind,target_run_id,target_session_id,payload_json,created_at) VALUES ('mail','session','parent','parent','session','agent','child','child:child','{\"message\":\"evidence\"}','now')",[]).unwrap();
    let mut snapshot = Snapshot::new("child", "worker", json!({"messages":[],"pending":null}));
    let mut history = vec![];
    db.0.lock().unwrap().execute_batch("CREATE TRIGGER reject_snapshot BEFORE UPDATE OF execution_checkpoint_json ON agent_child_runs BEGIN SELECT RAISE(ABORT,'snapshot failure'); END;").unwrap();
    assert!(
        crate::agent::agent_mailbox::deliver_specialist_with_snapshot(
            &db,
            "child",
            &mut history,
            Some(&mut snapshot)
        )
        .is_err()
    );
    assert!(history.is_empty());
    assert_eq!(snapshot.version, 0);
    let pending: i64 =
        db.0.lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM agent_messages WHERE delivered_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(pending, 1);
    db.0.lock()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_snapshot")
        .unwrap();
    assert_eq!(
        crate::agent::agent_mailbox::deliver_specialist_with_snapshot(
            &db,
            "child",
            &mut history,
            Some(&mut snapshot)
        )
        .unwrap(),
        1
    );
    let restored = Snapshot::load(&db, "child", "worker").unwrap().unwrap();
    assert!(restored.state["messages"].to_string().contains("evidence"));
    assert_eq!(snapshot.version, 1);
}
#[test]
fn configuration_identity_survives_resume_and_reviewer_failover() {
    let db = fixture();
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE agent_child_runs SET execution_config_json=?1 WHERE id='child'",
            [json!({"allowed_tools":["read_file"]}).to_string()],
        )
        .unwrap();
    let original = crate::agent::child_reviewer::load_run_identity(&db, "child").unwrap();
    let next = crate::agent::child_reviewer::ChildRunIdentity::inherited(
        &original,
        Some("rebuttal".into()),
    );
    assert_eq!(original.execution_config, next.execution_config);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lower_level_dispatch_cannot_bypass_the_same_permission_in_composed_steps() {
    let db = fixture();
    let dir = std::env::temp_dir().join(format!("runtime-permissions-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    db.0.lock().unwrap().execute("UPDATE agent_child_runs SET execution_config_json='{\"allowed_tools\":[\"local_file.read\"]}' WHERE id='child'",[]).unwrap();
    let leaf = json!({"tool":"local_file","action":"write","params":{"path":"denied.txt","content":"bad"}});
    for value in [
        leaf.clone(),
        json!({"parallel":[leaf.clone()]}),
        json!({"commands":[leaf.clone()]}),
    ] {
        let instruction = serde_json::from_value(value).unwrap();
        let result = crate::agent::dispatcher::run_agent_step_with_images(
            &db,
            "session",
            &instruction,
            &crate::agent::mcp::PluginRegistry::new(),
            Some(&dir),
            &Default::default(),
            Some("child"),
            None,
        );
        assert!(!result.success);
        assert!(!dir.join("denied.txt").exists());
    }
    let _ = std::fs::remove_dir_all(dir);
}
#[test]
fn child_registration_intersects_dynamic_permissions_and_preserves_role_bounds() {
    let db = fixture();
    db.0.lock().unwrap().execute("UPDATE agent_child_runs SET execution_config_json='{\"allowed_tools\":[\"read_file\"]}' WHERE id='child'",[]).unwrap();
    let mut identity = crate::agent::child_reviewer::ChildRunIdentity::fresh("worker");
    identity.execution_config =
        Some(json!({"allowed_tools":["read_file","run_command"]}).to_string());
    let id = crate::agent::child_reviewer::begin_child_run_extended(
        &db,
        "child",
        "session",
        &std::env::temp_dir(),
        "mock",
        0,
        "worker",
        None,
        None,
        None,
        &identity,
    )
    .unwrap();
    assert!(authorize(&db, &id, "read_file").is_ok());
    assert!(authorize(&db, &id, "run_command").is_err());
}

#[test]
fn broad_child_tool_is_narrowed_to_the_parent_action_during_registration() {
    let db = fixture();
    db.0.lock()
        .unwrap()
        .execute(
            "UPDATE agent_child_runs SET execution_config_json=?1 WHERE id='child'",
            [json!({"allowed_tools":["local_file.read"]}).to_string()],
        )
        .unwrap();
    let mut identity = crate::agent::child_reviewer::ChildRunIdentity::fresh("task");
    identity.execution_config = Some(json!({"allowed_tools":["local_file"],"stages":{"plan":true,"final_review":false},"workflow":{"plan_mode":"plan_approval","completion":"delivery"}}).to_string());
    let id = crate::agent::child_reviewer::begin_child_run_extended(
        &db,
        "child",
        "session",
        &std::env::temp_dir(),
        "mock",
        0,
        "task",
        None,
        None,
        None,
        &identity,
    )
    .unwrap();
    assert!(authorize_call(&db, &id, "local_file", "read", &json!({"path":"a"})).is_ok());
    assert!(authorize_call(
        &db,
        &id,
        "local_file",
        "write",
        &json!({"path":"a","content":"x"})
    )
    .is_err());
    assert!(authorize(&db, &id, "run_command").is_err());
    let saved = config(&db, &id).unwrap();
    assert_eq!(saved.workflow.plan_mode.as_deref(), Some("plan_approval"));
    assert_eq!(saved.stages.plan, Some(true));
    assert_eq!(saved.stages.final_review, Some(false));
}

#[test]
fn permission_intersection_preserves_aliases_empty_and_unrestricted_meanings() {
    for (requested, ceiling, expected) in [
        (json!(null), json!(["read_file"]), json!(["read_file"])),
        (json!(["local_file"]), json!(null), json!(["local_file"])),
        (json!([]), json!(["local_file"]), json!([])),
        (json!(["local_file"]), json!([]), json!([])),
        (
            json!(["local_file"]),
            json!(["read_file", "local_file.list"]),
            json!(["read_file", "local_file.list"]),
        ),
        (
            json!(["local_file.read"]),
            json!(["local_file"]),
            json!(["local_file.read"]),
        ),
        (
            json!(["local_shell"]),
            json!(["run_command"]),
            json!(["run_command"]),
        ),
        (
            json!(["send_message"]),
            json!(["child_task.send"]),
            json!(["send_message"]),
        ),
        (
            json!(["my_mcp/read", "my_mcp/write"]),
            json!(["my_mcp/read"]),
            json!(["my_mcp/read"]),
        ),
    ] {
        let mut config =
            ExecutionConfig::parse(&json!({"execution":{"allowed_tools":requested}}), "task")
                .unwrap();
        let limit = ExecutionConfig::parse(&json!({"execution":{"allowed_tools":ceiling}}), "task")
            .unwrap();
        config.restrict_tools(&limit);
        assert_eq!(
            serde_json::to_value(config.allowed_tools).unwrap(),
            expected,
            "requested={requested}, ceiling={ceiling}"
        );
    }
}

#[test]
fn inbox_snapshot_cannot_acknowledge_another_executions_messages() {
    let db = fixture();
    let mut snapshot = Snapshot::new("different-child", "worker", json!({}));
    let mut history = vec![];
    assert!(
        crate::agent::agent_mailbox::deliver_specialist_with_snapshot(
            &db,
            "child",
            &mut history,
            Some(&mut snapshot)
        )
        .unwrap_err()
        .contains("归属不一致")
    );
    assert!(history.is_empty());
    assert_eq!(snapshot.version, 0);
}

#[test]
fn configured_discussion_requires_actual_plan_approval_and_model_binding_keeps_reviewers_independent(
) {
    let config = ExecutionConfig::parse(
        &json!({"execution":{"workflow":{"plan_mode":"cross_review","completion":"handoff"}}}),
        "task",
    )
    .unwrap();
    assert!(workflow_blocker(&config, false).is_some());
    assert!(workflow_blocker(&config, true).is_none());
    assert!(workflow_blocker(&Default::default(), false).is_none());
    let mut routing = crate::agent::router::ModelRoutingConfig::default();
    routing
        .role_models
        .insert("reviewer".into(), "independent".into());
    bind_primary_model(&mut routing, "chosen");
    assert_eq!(routing.pro, "chosen");
    assert_eq!(routing.flash, "chosen");
    assert_eq!(routing.role_models["planner"], "chosen");
    assert_eq!(routing.role_models["reviewer"], "independent");
}

#[test]
fn ordinary_presets_use_the_main_executor_and_specialised_modes_are_explicit() {
    for profile in ["task","worker","writer"] {
        assert!(uses_main_executor(profile,&json!({})).unwrap());
        let config=ExecutionConfig::parse(&json!({"execution":{"workflow":{"plan_mode":"cross_review","completion":"delivery"}}}),profile).unwrap();
        assert!(config.permits("my_mcp/tool"));
        assert!(config.permits("run_command"));
        assert!(config.permits("capability_search"));
    }
    assert!(!uses_main_executor("writer",&json!({"execution":{"workspace":"isolated"}})).unwrap());
    assert!(!uses_main_executor("reviewer",&json!({})).unwrap());
    assert!(uses_main_executor("worker",&json!({"execution":{"workspace":"isolated"}})).is_err());
}

#[test]
fn shared_main_protocol_uses_the_same_explicit_action_permissions() {
    let db=fixture();
    db.0.lock().unwrap().execute("UPDATE agent_child_runs SET execution_config_json=?1 WHERE id='child'",[json!({"allowed_tools":["read_file","send_message"]}).to_string()]).unwrap();
    assert!(authorize_call(&db,"child","local_file","read",&json!({})).is_ok());
    assert!(authorize_call(&db,"child","child_task","send",&json!({})).is_ok());
    assert!(authorize_call(&db,"child","local_file","write",&json!({})).is_err());
    assert!(authorize_call(&db,"child","my_mcp/tool","",&json!({})).is_err());
}

#[test]
fn explicit_dispatch_test_is_bound_to_the_existing_verification_contract() {
    let db=fixture();
    let mut snapshot=Snapshot::new("child","task",json!({"dispatch_input":{"params":{"test_command":"cargo test --lib"}}}));
    snapshot.save(&db).unwrap();
    let mut context=crate::agent::context::SandwichContext::new("system".into(),"goal".into());
    bind_registered_requirements(&db,"child",&mut context).unwrap();
    let contract=context.review_contract.unwrap();
    assert_eq!(contract.obligations[0].verify_command.as_deref(),Some("cargo test --lib"));
    assert!(contract.has_executable_verification());
}
