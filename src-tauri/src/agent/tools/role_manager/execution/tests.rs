use super::*;
use std::sync::{Arc, Mutex};
fn db() -> DbState {
    DbState(Arc::new(Mutex::new(
        crate::db::init_db(&std::path::PathBuf::from(":memory:")).unwrap(),
    )))
}
fn card() -> Value {
    json!({"name":"configured_agent","label":"configured","allowed_kinds":"task,worker","execution_defaults":{"profile":"task","execution":{"allowed_tools":["local_file.read","send_message"],"workflow":{"plan_mode":"cross_review","completion":"delivery"}}}})
}

#[test]
fn independent_permissions_and_workflow_do_not_require_a_role_profile() {
    let db=db();
    let saved=upsert_card(&db,&json!({"name":"permissions_only","allowed_kinds":"worker","execution_defaults":{"execution":{"allowed_tools":["read_file"]}}})).unwrap();
    let stored:Value=serde_json::from_str(&saved.execution_defaults).unwrap();
    assert!(stored.get("profile").is_none());
    let merged=merge_execution_defaults(&db,&json!({"role":"permissions_only","task":"inspect"}),None).unwrap();
    assert_eq!(merged["profile"],"worker");
    assert_eq!(merged["execution"]["allowed_tools"],json!(["read_file"]));
    assert!(merged["execution"]["workflow"]["plan_mode"].is_null());
    let global=crate::agent::child_task::defaults::ChildWorkflowDefaults {plan_mode:Some("plan_approval".into()),completion:Some("delivery".into()),..Default::default()};
    let effective=global.apply(&merged).unwrap();
    assert_eq!(effective["execution"]["workflow"]["plan_mode"],"plan_approval");
    assert_eq!(effective["execution"]["allowed_tools"],json!(["read_file"]));
}

#[test]
fn withdrawing_only_role_workflow_preserves_permission_ceiling_and_model() {
    let db=db();let mut original=card();original["model_id"]=json!("chosen");
    upsert_card(&db,&original).unwrap();
    original["execution_defaults"]["execution"]["workflow"]=json!({});
    let updated=upsert_card(&db,&original).unwrap();
    assert_eq!(updated.model_id,"chosen");
    let merged=merge_execution_defaults(&db,&json!({"role":"configured_agent","execution":{"allowed_tools":["read_file","write_file"]}}),None).unwrap();
    assert_eq!(merged["execution"]["allowed_tools"],json!(["read_file"]));
    assert!(merged["execution"]["workflow"]["plan_mode"].is_null());
    assert!(upsert_card(&db,&json!({"name":"invalid","execution_defaults":{"profile":"unknown"}})).is_err());
}
#[test]
fn page_defaults_round_trip_and_drive_effective_dispatch_values() {
    let db = db();
    let saved = upsert_card(&db, &card()).unwrap();
    assert!(!saved.execution_defaults.is_empty());
    let effective = merge_execution_defaults(
        &db,
        &json!({"role":"configured_agent","task":"investigate"}),
        None,
    )
    .unwrap();
    assert_eq!(effective["profile"], "task");
    assert_eq!(
        effective["execution"]["workflow"]["plan_mode"],
        "cross_review"
    );
    let worker=merge_execution_defaults(&db,&json!({"role":"configured_agent","profile":"worker","task":"investigate"}),None).unwrap();
    assert_eq!(worker["execution"]["workflow"]["plan_mode"],"cross_review");
    assert_eq!(worker["execution"]["workflow"]["completion"],"delivery");
    let listed = list_cards(&db, true).unwrap();
    assert_eq!(
        listed
            .iter()
            .find(|c| c.name == saved.name)
            .unwrap()
            .execution_defaults,
        saved.execution_defaults
    );
}
#[test]
fn explicit_workflow_overrides_defaults_but_cannot_expand_role_tool_permissions() {
    let db = db();
    upsert_card(&db, &card()).unwrap();
    let effective=merge_execution_defaults(&db,&json!({"role":"configured_agent","execution":{"allowed_tools":["read_file","write_file","run_command"],"workflow":{"completion":"handoff"}}}),None).unwrap();
    assert_eq!(
        effective["execution"]["allowed_tools"],
        json!(["read_file"])
    );
    assert_eq!(effective["execution"]["workflow"]["completion"], "handoff");
    assert_eq!(
        effective["execution"]["workflow"]["plan_mode"],
        "cross_review"
    );
    let empty = merge_execution_defaults(
        &db,
        &json!({"role":"configured_agent","execution":{"allowed_tools":[]}}),
        None,
    )
    .unwrap();
    assert_eq!(empty["execution"]["allowed_tools"], json!([]));
}

#[test]
fn broad_dispatch_tool_retains_only_actions_within_the_role_ceiling() {
    let db = db();
    upsert_card(&db, &card()).unwrap();
    let effective = merge_execution_defaults(&db,
        &json!({"role":"configured_agent","execution":{"allowed_tools":["local_file","run_command"]}}), None).unwrap();
    let config = ExecutionConfig::parse(&effective, "task").unwrap();
    assert!(
        config.permits("read_file"),
        "the intersection includes the role's read action"
    );
    assert!(!config.permits("write_file"));
    assert!(!config.permits("run_command"));
    assert!(!config.permits("send_message"));
    assert_eq!(config.workflow.plan_mode.as_deref(), Some("cross_review"));
}

#[test]
fn fixed_adapter_keeps_its_own_workflow_and_uses_the_same_role_permission_ceiling() {
    let db = db();
    upsert_card(
        &db,
        &json!({"name":"legacy_reader","allowed_kinds":"worker"}),
    )
    .unwrap();
    assert_eq!(
        merge_execution_defaults(&db, &json!({"role":"legacy_reader"}), None).unwrap()["profile"],
        "worker"
    );
    upsert_card(&db, &card()).unwrap();
    let effective = merge_execution_defaults(
        &db,
        &json!({"role":"configured_agent","task":"read"}),
        Some("worker"),
    )
    .unwrap();
    assert_eq!(effective["profile"], "worker");
    assert!(effective["execution"]["workflow"]["plan_mode"].is_null());
    assert_eq!(
        effective["execution"]["allowed_tools"],
        json!(["local_file.read", "send_message"])
    );
}
#[test]
fn old_clients_preserve_defaults_and_explicit_clear_removes_them() {
    let db = db();
    let original = upsert_card(&db, &card()).unwrap();
    let updated = upsert_card(&db, &json!({"name":"configured_agent","label":"changed"})).unwrap();
    assert_eq!(updated.execution_defaults, original.execution_defaults);
    let cleared = upsert_card(
        &db,
        &json!({"name":"configured_agent","execution_defaults":""}),
    )
    .unwrap();
    assert!(cleared.execution_defaults.is_empty());
}
#[test]
fn invalid_page_combinations_are_rejected_before_saving_and_leave_previous_values() {
    let db = db();
    let original = upsert_card(&db, &card()).unwrap();
    for invalid in [
        json!({"profile":"writer","execution":{"workspace":"isolated","workflow":{"plan_mode":"cross_review"}}}),
        json!({"profile":"task","execution":{"workflow":{"completion":"guess"}}}),
        json!({"profile":"task","execution":{"allowed_tools":"read_file"}}),
    ] {
        assert!(upsert_card(
            &db,
            &json!({"name":"configured_agent","execution_defaults":invalid})
        )
        .is_err());
        assert_eq!(
            resolve_enabled_card(&db, "configured_agent")
                .unwrap()
                .unwrap()
                .execution_defaults,
            original.execution_defaults
        );
    }
}
#[test]
fn persisted_generic_role_contract_is_checked_against_the_actual_finish_payload() {
    let db = db();
    let schema = json!({"fields":[{"name":"checked_files","field_type":"number","required":true,"description":"count"}]});
    db.0.lock().unwrap().execute("INSERT INTO agent_child_runs(id,parent_run_id,parent_session_id,workspace_root,model_id,status,started_at,kind,contract_schema) VALUES ('task','parent','session','/tmp','model','running','now','task',?1)",[schema.to_string()]).unwrap();
    assert!(crate::agent::execution::runtime::validate_output(
        &db,
        "task",
        &json!({"summary":"done"})
    )
    .is_err());
    assert!(crate::agent::execution::runtime::validate_output(
        &db,
        "task",
        &json!({"summary":"done","checked_files":3})
    )
    .is_ok());
}

#[test]
fn generic_only_role_is_visible_in_the_same_catalog_used_for_dispatch() {
    let db = db();
    let mut value = card();
    value["allowed_kinds"] = json!("task");
    upsert_card(&db, &value).unwrap();
    let menu = build_role_cards_menu_filtered(&db, &[]).unwrap();
    assert!(menu.contains("configured_agent"));
    assert!(menu.contains("默认执行组合"));
    assert!(menu.contains("cross_review"));
}

#[test]
fn role_menu_uses_the_actual_completion_default_for_each_profile() {
    let db = db();
    for profile in ["task", "worker"] {
        let mut value = card();
        value["name"] = json!(format!("default_{profile}"));
        value["allowed_kinds"] = json!(profile);
        value["execution_defaults"] = json!({"profile":profile});
        upsert_card(&db, &value).unwrap();
        let effective = merge_execution_defaults(&db, &json!({"role":value["name"],"task":"check"}), None).unwrap();
        let config = ExecutionConfig::parse(&effective, profile).unwrap();
        assert!(config.workflow.completion.is_none());
    }
    let menu = build_role_cards_menu_filtered(&db, &[]).unwrap();
    assert!(menu.contains("默认执行组合 task / auto / delivery"));
    assert!(menu.contains("默认执行组合 worker / auto / handoff"));
}

#[test]
fn writer_alias_resolves_role_workspace_before_executor_selection() {
    let db = db();
    upsert_card(&db, &json!({"name":"isolated_writer", "allowed_kinds":"writer",
        "execution_defaults":{"profile":"writer","execution":{"workspace":"isolated",
            "workflow":{"completion":"handoff"}}}})).unwrap();
    let request = crate::agent::child_task::presets::requested(
        json!({"role":"isolated_writer","task":"implement"}), Some("writer")).unwrap();
    let effective = merge_execution_defaults(&db, &request, None).unwrap();
    assert_eq!(effective["execution"]["workspace"], "isolated");
    assert!(!crate::agent::execution::runtime::uses_main_executor("writer", &effective).unwrap());
    let shared = crate::agent::child_task::presets::requested(
        json!({"role":"isolated_writer","task":"implement","execution":{"workspace":"shared"}}),
        Some("writer")).unwrap();
    let shared = merge_execution_defaults(&db, &shared, None).unwrap();
    assert!(crate::agent::execution::runtime::uses_main_executor("writer", &shared).unwrap());
}
