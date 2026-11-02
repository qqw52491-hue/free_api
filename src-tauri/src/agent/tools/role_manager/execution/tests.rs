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
        json!({"profile":"writer","execution":{"workflow":{"plan_mode":"cross_review"}}}),
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
