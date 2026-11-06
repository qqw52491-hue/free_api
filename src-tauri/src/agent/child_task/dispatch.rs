//! Dispatch identity uses declared inputs, before live role defaults or focus
//! collection. Replaying a registration must not depend on a changed workspace.
use serde_json::{json, Value};

pub(crate) fn key(task: &str, step: usize, params: &Value, slot: Option<usize>) -> String {
    let identity = json!([params, slot]).to_string();
    format!(
        "general:v2:{task}:{step}:{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, identity.as_bytes())
    )
}

/// Adopt an old registration only when its full persisted input agrees. The
/// old key alone omitted fields and cannot prove the dispatch is the same.
pub(crate) fn find_registered(
    db: &crate::db::DbState,
    session: &str,
    key: &str,
    legacy_key: &str,
    effective_params: &Value,
    slot: Option<usize>,
) -> Result<Option<String>, String> {
    if let Some(id) = super::find_task(db, session, key)? {
        return Ok(Some(id));
    }
    // Old batch keys had no slot identity: never adopt a first item as a sibling.
    if slot.is_some_and(|i| i > 0) {
        return Ok(None);
    }
    let Some(id) = super::find_task(db, session, legacy_key)? else {
        return Ok(None);
    };
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let raw: Option<String> = conn.query_row(
        "SELECT execution_checkpoint_json FROM agent_child_runs WHERE id=?1 AND parent_session_id=?2",
        rusqlite::params![id, session], |r| r.get(0),
    ).map_err(|e| e.to_string())?;
    let saved = raw.and_then(|s| serde_json::from_str::<Value>(&s).ok());
    let mut saved_params = saved
        .as_ref()
        .map(|s| s["state"]["dispatch_input"]["params"].clone())
        .unwrap_or(Value::Null);
    let mut current = effective_params.clone();
    // Collected workspace context is derived, not the identity of a request.
    for value in [&mut saved_params, &mut current] {
        if let Some(object) = value.as_object_mut() {
            object.remove("specialized_context");
        }
    }
    Ok((saved_params.is_object() && saved_params == current).then_some(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{
        checkpoint::{
            load_checkpoint, recovery_decision, save_checkpoint, AgentCheckpoint, RecoveryDecision,
        },
        context::SandwichContext,
        types::{AgentInstruction, DispatchResult},
    };
    use crate::db::DbState;
    use std::sync::{Arc, Mutex};

    struct Fixture {
        root: std::path::PathBuf,
        db: DbState,
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("dispatch-regression-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let db = DbState(Arc::new(Mutex::new(
                crate::db::init_db(&root.join("test.db")).unwrap(),
            )));
            crate::agent::tools::role_manager::ensure_default_role_cards(&db.0.lock().unwrap())
                .unwrap();
            Self { root, db }
        }
        fn register(&self, parent: &str, key: &str, params: &Value) -> Result<String, String> {
            crate::agent::child_reviewer::begin_child_run_extended(
                &self.db,
                parent,
                "owner",
                &self.root,
                "mock",
                0,
                "task",
                Some(6),
                None,
                None,
                &crate::agent::child_reviewer::ChildRunIdentity {
                    task_id: key.into(),
                    dispatch_input: Some(json!({"params": params}).to_string()),
                    ..crate::agent::child_reviewer::ChildRunIdentity::fresh("task")
                },
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn declared_materials_and_duplicate_batch_slots_have_distinct_registrations() {
        let f = Fixture::new();
        let original = json!({"task":"inspect","profile":"worker","role":"system_worker"});
        let first_key = key("task", 2, &original, Some(0));
        let first = f.register("parent", &first_key, &original).unwrap();
        for (field, value) in [
            ("model_id", json!("another-model")),
            ("focus_paths", json!(["src/b.rs"])),
            ("context_excerpts", json!({"background":"another module"})),
            ("test_command", json!("cargo test module_b")),
            ("custom_inputs", json!({"scope":"b"})),
        ] {
            let mut changed = original.clone();
            changed[field] = value;
            let changed_key = key("task", 2, &changed, Some(0));
            assert_ne!(changed_key, first_key, "{field}");
            assert!(super::super::find_task(&f.db, "owner", &changed_key)
                .unwrap()
                .is_none());
        }
        let second_key = key("task", 2, &original, Some(1));
        let second = f.register("parent", &second_key, &original).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            super::super::find_task(&f.db, "owner", &first_key).unwrap(),
            Some(first)
        );
        assert_ne!(key("task", 3, &original, Some(0)), first_key);
        assert_ne!(key("another-task", 2, &original, Some(0)), first_key);
        let reordered: Value =
            serde_json::from_str(r#"{"role":"system_worker","profile":"worker","task":"inspect"}"#)
                .unwrap();
        assert_eq!(key("task", 2, &reordered, Some(0)), first_key);
    }

    #[test]
    fn legacy_adoption_requires_the_full_input_and_owner() {
        let f = Fixture::new();
        let params =
            json!({"task":"inspect","focus_paths":["src/a.rs"],"test_command":"cargo test a"});
        let id = f.register("parent", "legacy-key", &params).unwrap();
        assert_eq!(
            find_registered(&f.db, "owner", "new-key", "legacy-key", &params, None).unwrap(),
            Some(id)
        );
        let mut changed = params.clone();
        changed["focus_paths"] = json!(["src/b.rs"]);
        assert!(
            find_registered(&f.db, "owner", "new-key", "legacy-key", &changed, None)
                .unwrap()
                .is_none()
        );
        assert!(
            find_registered(&f.db, "other-owner", "new-key", "legacy-key", &params, None)
                .unwrap()
                .is_none()
        );
        assert!(
            find_registered(&f.db, "owner", "new-key", "legacy-key", &params, Some(1))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn nested_dispatch_batch_checkpoint_keeps_slots_after_restore() {
        let f = Fixture::new();
        let context = SandwichContext::new("system".into(), "goal".into());
        let instruction: AgentInstruction = serde_json::from_value(json!({"tool":"child_worker", "command":{"dispatches":[{"task":"A"},{"task":"B"}]}})).unwrap();
        let mut checkpoint = AgentCheckpoint::new("owner".into(), &context);
        checkpoint.begin_child_dispatch_batch(&instruction, &context).unwrap();
        checkpoint.child_dispatch_batch.as_mut().unwrap().results[0] = Some(DispatchResult { success:true, stdout:"first".into(), stderr:String::new(), route:"child_task:spawned".into(), view:None });
        save_checkpoint(&f.db, &mut checkpoint).unwrap();
        let mut restored = load_checkpoint(&f.db, "owner").unwrap().unwrap();
        let recovered = restored.prepared_instruction.clone().unwrap();
        assert_eq!(recovered.get_dispatch_items().len(), 2);
        restored.begin_child_dispatch_batch(&recovered, &context).unwrap();
        let slots = &restored.child_dispatch_batch.as_ref().unwrap().results;
        assert!(slots[0].is_some());
        assert!(slots[1].is_none());
    }

    #[test]
    fn batch_checkpoint_recovers_remaining_slots_and_registration_before_receipt() {
        let f = Fixture::new();
        let context = SandwichContext::new("system".into(), "goal".into());
        let instruction: AgentInstruction =
            serde_json::from_value(json!({"tool":"child_worker","dispatches":[
                {"task":"inspect","focus_paths":["src/a.rs"]},
                {"task":"inspect","focus_paths":["src/b.rs"]},
                {"task":"inspect","focus_paths":["src/c.rs"]}
            ]}))
            .unwrap();
        let items = instruction.get_dispatch_items();
        let mut checkpoint = AgentCheckpoint::new("owner".into(), &context);
        checkpoint
            .begin_child_dispatch_batch(&instruction, &context)
            .unwrap();
        let item: AgentInstruction =
            serde_json::from_value(json!({"tool":"child_task","params":items[0]})).unwrap();
        checkpoint
            .prepare_child_dispatch(&item, &context, Some(0), true)
            .unwrap();
        let first_key = key(&context.history_task_id, 0, &items[0], Some(0));
        let first = f.register("parent", &first_key, &items[0]).unwrap();
        checkpoint.child_dispatch_batch.as_mut().unwrap().results[0] = Some(DispatchResult {
            success: true,
            stdout: json!({"child_run_id":first}).to_string(),
            stderr: String::new(),
            route: "child_task:spawned".into(),
            view: None,
        });
        checkpoint
            .prepare_child_dispatch(&item, &context, Some(1), true)
            .unwrap();
        save_checkpoint(&f.db, &mut checkpoint).unwrap();
        // The second registration commits, but the process dies before saving
        // its parent-side receipt. Replay must adopt it, not launch a duplicate.
        let second_key = key(&context.history_task_id, 0, &items[1], Some(1));
        let second = f.register("parent", &second_key, &items[1]).unwrap();
        let mut restored = load_checkpoint(&f.db, "owner").unwrap().unwrap();
        assert_eq!(recovery_decision(&restored), RecoveryDecision::ResumeTool);
        let recovered = restored.prepared_instruction.clone().unwrap();
        assert_eq!(recovered.get_dispatch_items(), items);
        restored
            .begin_child_dispatch_batch(&recovered, &context)
            .unwrap();
        assert!(restored.child_dispatch_batch.as_ref().unwrap().results[0].is_some());
        assert!(restored.child_dispatch_batch.as_ref().unwrap().results[1].is_none());
        assert_eq!(
            find_registered(&f.db, "owner", &second_key, "unused", &items[1], Some(1)).unwrap(),
            Some(second)
        );
        let third_key = key(&context.history_task_id, 0, &items[2], Some(2));
        f.register("parent", &third_key, &items[2]).unwrap();
        let count: i64 =
            f.db.0
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM agent_child_runs", [], |r| r.get(0))
                .unwrap();
        assert_eq!(count, 3);
        // Unknown specialised side effects still require their original recovery
        // path. A shared registration must not label a patch apply as replayable.
        restored
            .prepare_child_dispatch(&item, &context, Some(2), false)
            .unwrap();
        assert_eq!(
            recovery_decision(&restored),
            RecoveryDecision::RequireHumanReview
        );
        restored.complete_step(0, &context);
        assert!(restored.child_dispatch_batch.is_none());
        let mut old = serde_json::to_value(&restored).unwrap();
        old.as_object_mut().unwrap().remove("child_dispatch_batch");
        assert!(serde_json::from_value::<AgentCheckpoint>(old)
            .unwrap()
            .child_dispatch_batch
            .is_none());
    }

    #[test]
    fn shared_role_quota_serializes_racing_registrations_and_releases_terminal_slots() {
        let f = Arc::new(Fixture::new());
        f.db.0
            .lock()
            .unwrap()
            .execute(
                "UPDATE agent_role_cards SET max_concurrency=1 WHERE name='system_worker'",
                [],
            )
            .unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let handles:Vec<_>=(0..2).map(|i| {
            let f=f.clone(); let barrier=barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                f.register("parent",&format!("race-{i}"),&json!({"task":"work","role":"system_worker","profile":if i==0 {"worker"}else{"writer"}}))
            })
        }).collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert!(results
            .iter()
            .find_map(|r| r.as_ref().err())
            .unwrap()
            .contains("system_worker"));
        // A different role or parent run has a separate quota.
        f.register("parent", "writer", &json!({"role":"system_writer"}))
            .unwrap();
        f.register(
            "another-parent",
            "other-parent",
            &json!({"role":"system_worker"}),
        )
        .unwrap();
        let id = results.iter().find_map(|r| r.as_ref().ok()).unwrap();
        f.db.0
            .lock()
            .unwrap()
            .execute(
                "UPDATE agent_child_runs SET status='queued' WHERE id=?1",
                [id],
            )
            .unwrap();
        assert!(f
            .register("parent", "queued-reject", &json!({"role":"system_worker"}))
            .is_err());
        f.db.0
            .lock()
            .unwrap()
            .execute(
                "UPDATE agent_child_runs SET status='succeeded' WHERE id=?1",
                [id],
            )
            .unwrap();
        f.register("parent", "replacement", &json!({"role":"system_worker"}))
            .unwrap();
        f.db.0
            .lock()
            .unwrap()
            .execute(
                "UPDATE agent_role_cards SET max_concurrency=0 WHERE name='system_worker'",
                [],
            )
            .unwrap();
        f.register("parent", "unlimited-role", &json!({"role":"system_worker"}))
            .unwrap();
        for i in 0..3 {
            f.register("parent", &format!("fill-{i}"), &json!({"role":"system_worker"})).unwrap();
        }
        let error = f.register("parent", "over-total", &json!({"role":"system_worker"})).unwrap_err();
        assert!(error.contains("全局看门上限已满"));
    }
}
