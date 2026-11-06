use super::*;
use crate::agent::{
    checkpoint::{load_checkpoint, AgentCheckpoint, AgentPhase},
    context::SandwichContext,
    finalization::{FinalizationReceipt, ReceiptReview, ReceiptVerification},
};
use std::{path::PathBuf, sync::Arc};

struct Fixture {
    root: PathBuf,
    db: DbState,
    binding: TaskBinding,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("general-task-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = DbState(Arc::new(Mutex::new(
            crate::db::init_db(&root.join("test.db")).unwrap(),
        )));
        let parent = format!("parent-{}", uuid::Uuid::new_v4());
        crate::agent::conversation::begin_turn(&db, &parent, "model", "parent goal").unwrap();
        let id = crate::agent::child_reviewer::begin_child_run_extended(
            &db,
            "parent-run",
            &parent,
            &root,
            "model",
            0,
            "task",
            Some(1),
            None,
            None,
            &crate::agent::child_reviewer::ChildRunIdentity {
                task_id: "logical-task".into(),
                ..crate::agent::child_reviewer::ChildRunIdentity::fresh("task")
            },
        )
        .unwrap();
        let binding = TaskBinding {
            parent_session: parent,
            parent_run: "parent-run".into(),
            child_session: format!("task-{id}"),
            child_run: id,
        };
        crate::agent::conversation::begin_turn(&db, &binding.child_session, "model", "child goal")
            .unwrap();
        Self { root, db, binding }
    }
    fn finish_readonly(&self) {
        let context = SandwichContext::new("system".into(), "child goal".into());
        let mut cp = AgentCheckpoint::new(self.binding.child_session.clone(), &context);
        cp.pending_finalization = Some(FinalizationReceipt {
        handoff: false,
            receipt_id: "host-receipt".into(),
            diff_hash: "empty".into(),
            scope_revision: 0,
            contract_fingerprint: String::new(),
            acceptance_fingerprint: "host-fingerprint".into(),
            evidence_index_fingerprint: None,
            review_policy_fingerprint: String::new(),
            frozen_verification_run: None,
            step_id: 1,
            verification: ReceiptVerification::NotRequired,
            review: ReceiptReview::NotRequired,
            material_notices: vec![],
            blockers: vec![],
            risk_flags: vec![],
            finish_payload: json!({"summary":"read-only findings"}),
            created_at: chrono::Utc::now().to_rfc3339(),
        });
        cp.mark_done(&context);
        // 使用正式终态事务写检查点、轨迹和子会话消息；不制造父会话的成功消息。
        crate::agent::finalization::commit_finalization(
            &self.db,
            &mut cp,
            &self.binding.child_run,
            &self.binding.child_session,
            "child goal",
            &[],
            &json!({"summary":"read-only findings","finalization_receipt":"host-receipt"}),
            "read-only findings",
            "model",
        )
        .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        crate::agent::workspace_access::revoke_session(&self.binding.parent_session);
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = std::fs::remove_dir_all(crate::agent::path_guard::get_session_tmp_dir(
            &self.binding.parent_session,
        ));
    }
}

#[test]
fn accepts_complete_goals_without_specialist_business_fields_and_validates_aliases() {
    use crate::agent::{
        tools::input_validation::{instruction_rejection, validate_native},
        types::AgentInstruction,
    };
    let f = Fixture::new();
    let minimal = json!({"task":"独立调查并修复兼容问题".repeat(100)});
    assert!(validate_native("child_task", "dispatch", &minimal).is_ok());
    assert!(validate_native("child_task", "", &json!({"task":3})).is_err());
    assert!(validate_native("child_task", "", &json!({"task":"x".repeat(65_537)})).is_err());
    let inst:AgentInstruction=serde_json::from_value(json!({"tool":"core","action":"child_task","params":{"action":"result","child_run_id":f.binding.child_run}})).unwrap();
    assert!(instruction_rejection(&f.db, &inst).is_none());
    assert_eq!(request_action("RESULT", &json!({})), "result");
    assert_eq!(
        request_action("CHILD_TASK", &json!({"action":"RESULT"})),
        "result"
    );
}

#[test]
fn formal_subtask_delivery_is_separate_from_parent_and_recovers_without_reexecution() {
    let f = Fixture::new();
    f.finish_readonly();
    let body = recover_finished(
        &f.db,
        &f.binding.parent_session,
        "resumed-parent",
        &f.binding.child_run,
    )
    .unwrap()
    .unwrap();
    assert_eq!(body["delivery_accepted"], true);
    assert_eq!(body["verification"]["status"], "not_required");
    assert_eq!(
        find_task(&f.db, &f.binding.parent_session, "logical-task").unwrap(),
        Some(f.binding.child_run.clone())
    );
    assert!(result(&f.db, "another-session", &f.binding.child_run).is_err());
    let cp = load_checkpoint(&f.db, &f.binding.child_session)
        .unwrap()
        .unwrap();
    assert_eq!(cp.phase, AgentPhase::Done);
    let conn = f.db.0.lock().unwrap();
    let parent_messages: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM chat_messages WHERE session_id=?1 AND role='assistant'",
            [&f.binding.parent_session],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(parent_messages, 0);
    let child_messages: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM chat_messages WHERE session_id=?1 AND role='assistant'",
            [&f.binding.child_session],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(child_messages, 1);
    drop(conn);
    let mut child_cp = cp;
    child_cp
        .data
        .side_effects
        .push(crate::agent::context::SideEffectRecord {
            step_id: 0,
            tool: "local_file".into(),
            description: "write".into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            affected_paths: vec!["src/a.rs".into()],
        });
    crate::agent::checkpoint::save_checkpoint(&f.db, &mut child_cp).unwrap();
    let mut parent_context = SandwichContext::new("sys".into(), "parent goal".into());
    import_effects(&f.db, &f.binding.child_run, 3, &mut parent_context).unwrap();
    import_effects(&f.db, &f.binding.child_run, 3, &mut parent_context).unwrap();
    assert_eq!(parent_context.side_effects.len(), 1);
    assert_eq!(
        parent_context.side_effects[0].affected_paths,
        vec!["src/a.rs"]
    );
}

#[test]
fn returned_ok_or_claimed_success_without_ready_receipt_is_not_delivery() {
    for status in ["running", "succeeded", "failed", "cancelled"] {
        let f = Fixture::new();
        crate::agent::run_trace::upsert_snapshot(
            &f.db,
            &f.binding.child_run,
            &f.binding.child_session,
            "goal",
            status,
            &[],
            Some(&json!({"summary":"完成了","finalization_receipt":"invented"})),
        )
        .unwrap();
        let body = settle(&f.db, &f.binding, &Ok(()), 1).unwrap();
        assert_eq!(body["delivery_accepted"], false, "{status}");
        assert_eq!(body["execution_ended"], true);
        assert_eq!(
            body["execution_status"],
            if status == "cancelled" {
                "cancelled"
            } else {
                "failed"
            }
        );
    }
}

#[test]
fn inherited_authority_tracks_revocation_and_parent_output_references_keep_source() {
    use crate::agent::workspace_access::{self, DirectoryAccess, ProjectDirectory};
    let f = Fixture::new();
    let extra = f.root.join("extra");
    std::fs::create_dir_all(&extra).unwrap();
    let extra = extra.canonicalize().unwrap();
    workspace_access::bind_conversation(
        &f.binding.parent_session,
        &f.root,
        vec![ProjectDirectory {
            path: extra.to_string_lossy().into(),
            access: DirectoryAccess::ReadOnly,
        }],
    );
    let guard = TaskGuard::enter(f.binding.clone());
    assert!(is_delegating(&f.binding.parent_session));
    let parent_tmp = crate::agent::path_guard::get_session_tmp_dir(&f.binding.parent_session);
    let child_tmp = crate::agent::path_guard::get_session_tmp_dir(&f.binding.child_session);
    assert!(child_tmp.starts_with(&parent_tmp));
    assert_ne!(child_tmp, parent_tmp);
    std::fs::create_dir_all(&parent_tmp).unwrap();
    let evidence = parent_tmp.join("parent-evidence.txt");
    std::fs::write(&evidence, "reference").unwrap();
    assert!(crate::agent::path_guard::ensure_readable_path(
        evidence.to_str().unwrap(),
        &f.root,
        Some(&f.binding.child_session),
        false
    )
    .is_ok());
    assert!(crate::agent::path_guard::ensure_allowed_path(
        evidence.to_str().unwrap(),
        &f.root,
        Some(&f.binding.child_session),
        false
    )
    .is_err());
    assert_eq!(
        workspace_access::access_for_canonical(Some(&f.binding.child_session), &f.root, &extra),
        Some(DirectoryAccess::ReadOnly)
    );
    let context = SandwichContext::new("sys".into(), "child".into());
    let mut cp = AgentCheckpoint::new(f.binding.child_session.clone(), &context);
    crate::agent::checkpoint::save_checkpoint(&f.db, &mut cp).unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&extra)
        .status()
        .unwrap()
        .success());
    let next = vec![ProjectDirectory {
        path: extra.to_string_lossy().into(),
        access: DirectoryAccess::ReadWrite,
    }];
    {
        let conn = f.db.0.lock().unwrap();
        crate::agent::reviewer_gatekeeper::capture_new_writable_directories_on(
            &conn,
            &f.binding.parent_session,
            &f.root,
            &next,
        )
        .unwrap();
    }
    let saved = load_checkpoint(&f.db, &f.binding.child_session)
        .unwrap()
        .unwrap();
    assert!(saved
        .task_additional_git_baselines
        .contains_key(extra.to_str().unwrap()));
    workspace_access::bind_conversation(&f.binding.parent_session, &f.root, vec![]);
    assert_eq!(
        workspace_access::access_for_canonical(Some(&f.binding.child_session), &f.root, &extra),
        None
    );
    let id = crate::agent::run_trace::save_step_output_in_group(
        &f.db,
        &f.binding.parent_session,
        2,
        "local_file",
        "parent observation",
        "success",
        "source body",
        None,
    )
    .unwrap();
    let reply = crate::agent::task_store::dispatch(
        &f.db,
        &f.binding.child_session,
        "get_output",
        &json!({"output_id":id}),
    );
    assert!(reply.success, "{}", reply.stderr);
    let body: Value = serde_json::from_str(&reply.stdout).unwrap();
    assert_eq!(body["raw_output"], "source body");
    assert_eq!(body["source"]["delegated_parent_reference"], true);
    assert_eq!(body["source"]["session_id"], f.binding.parent_session);
    drop(guard);
    let reply = crate::agent::task_store::dispatch(
        &f.db,
        &f.binding.child_session,
        "get_output",
        &json!({"output_id":id}),
    );
    assert!(!reply.success);
}

#[tokio::test]
async fn parallel_children_hold_shared_project_lock_until_last_exit() {
    let f = Fixture::new();
    let root = crate::agent::workspace_lock::acquire_workspace_lock(
        &f.root, &f.binding.parent_session, &f.binding.parent_run, "parent",
    ).await.unwrap();
    let first = crate::agent::workspace_lock::acquire_delegation_lock(
        &f.root, &f.binding.parent_session, &f.binding.parent_run,
    ).await.unwrap().unwrap();
    let second = crate::agent::workspace_lock::acquire_delegation_lock(
        &f.root, &f.binding.parent_session, &f.binding.parent_run,
    ).await.unwrap().unwrap();
    drop(root);
    drop(first);
    let competing = crate::agent::workspace_lock::acquire_workspace_lock(
        &f.root, "other-session", "other-run", "other",
    );
    assert!(tokio::time::timeout(std::time::Duration::from_millis(150), competing).await.is_err());
    drop(second);
    let replacement = crate::agent::workspace_lock::acquire_workspace_lock(
        &f.root, "other-session", "other-run", "other",
    ).await.unwrap();
    drop(replacement);
}

#[tokio::test]
async fn stale_child_lease_cannot_release_replaced_lock() {
    let f = Fixture::new();
    let stale = crate::agent::workspace_lock::acquire_delegation_lock(
        &f.root, &f.binding.parent_session, &f.binding.parent_run,
    ).await.unwrap().unwrap();
    crate::agent::workspace_lock::force_release_workspace_lock(
        Some(f.root.to_string_lossy().into_owned()), None,
    ).await.unwrap();
    let current = crate::agent::workspace_lock::acquire_delegation_lock(
        &f.root, &f.binding.parent_session, &f.binding.parent_run,
    ).await.unwrap().unwrap();
    drop(stale);
    let competing = crate::agent::workspace_lock::acquire_workspace_lock(
        &f.root, "other-session", "other-run", "other",
    );
    assert!(tokio::time::timeout(std::time::Duration::from_millis(150), competing).await.is_err());
    drop(current);
    let replacement = crate::agent::workspace_lock::acquire_workspace_lock(
        &f.root, "other-session", "other-run", "other",
    ).await.unwrap();
    drop(replacement);
}

#[tokio::test]
async fn delegation_first_holds_lock_and_same_session_cannot_preempt_active_lease() {
    let f = Fixture::new();
    let (one, two) = tokio::join!(
        crate::agent::workspace_lock::acquire_delegation_lock(&f.root, &f.binding.parent_session, &f.binding.parent_run),
        crate::agent::workspace_lock::acquire_delegation_lock(&f.root, &f.binding.parent_session, &f.binding.parent_run),
    );
    let one = one.unwrap().unwrap();
    let two = two.unwrap().unwrap();
    drop(one);
    let competing = crate::agent::workspace_lock::acquire_workspace_lock(
        &f.root, &f.binding.parent_session, "new-parent-run", "replacement",
    );
    assert!(tokio::time::timeout(std::time::Duration::from_millis(1750), competing).await.is_err());
    drop(two);
    let replacement = crate::agent::workspace_lock::acquire_workspace_lock(
        &f.root, &f.binding.parent_session, "new-parent-run", "replacement",
    ).await.unwrap();
    drop(replacement);
}

#[test]
fn general_children_still_obey_total_capacity() {
    let f = Fixture::new();
    for _ in 1..crate::agent::child_reviewer::MAX_TOTAL_CHILDREN_PER_RUN {
        crate::agent::child_reviewer::begin_child_run(
            &f.db, &f.binding.parent_run, &f.binding.parent_session, &f.root, "model", 0, "task",
            Some(i64::from(crate::agent::child_reviewer::MAX_TOTAL_CHILDREN_PER_RUN)),
        ).unwrap();
    }
    let rejected = crate::agent::child_reviewer::begin_child_run(
        &f.db, &f.binding.parent_run, &f.binding.parent_session, &f.root, "model", 0, "task",
        Some(i64::from(crate::agent::child_reviewer::MAX_TOTAL_CHILDREN_PER_RUN)),
    );
    assert!(rejected.is_err());
}

#[test]
fn multiple_general_children_can_register_and_keep_separate_results() {
    let f = Fixture::new();
    let second = crate::agent::child_reviewer::begin_child_run(
        &f.db, &f.binding.parent_run, &f.binding.parent_session, &f.root, "model", 0, "task",
        Some(i64::from(crate::agent::child_reviewer::MAX_TOTAL_CHILDREN_PER_RUN)),
    ).unwrap();
    assert_ne!(second, f.binding.child_run);
    assert_eq!(status(&f.db, &f.binding.parent_session, &f.binding.child_run).unwrap()["execution_status"], "running");
    assert_eq!(status(&f.db, &f.binding.parent_session, &second).unwrap()["execution_status"], "running");
    f.finish_readonly();
    settle(&f.db, &f.binding, &Ok(()), 1).unwrap();
    assert_eq!(status(&f.db, &f.binding.parent_session, &second).unwrap()["execution_status"], "running");
}

#[test]
fn progress_and_long_results_are_bounded_and_child_data_follows_parent_cleanup() {
    let f = Fixture::new();
    f.finish_readonly();
    let body = settle(&f.db, &f.binding, &Ok(()), 1).unwrap();
    let mut long = body.clone();
    long["delivery"] = json!({"details":"证据".repeat(10_000)});
    let projected: Value = serde_json::from_str(&projection(&long)).unwrap();
    assert_eq!(projected["delivery"]["truncated"], true);
    assert_eq!(
        projected["read_full"]["params"]["child_run_id"],
        f.binding.child_run
    );
    let completed = relay_progress(
        &f.binding.child_run,
        json!({"type":"complete","success":true,"output":"done"}),
    )
    .unwrap();
    assert_eq!(completed["type"], "child_task_progress");
    assert!(completed.get("success").is_none());
    let pause = relay_progress(
        &f.binding.child_run,
        json!({"type":"agent_paused","message":"需要批准","form":{"kind":"plan_approval"}}),
    )
    .unwrap();
    assert_eq!(pause["event"]["form"]["kind"], "plan_approval");
    assert!(relay_progress(
        &f.binding.child_run,
        json!({"type":"thinking_chunk","text":"x"})
    )
    .is_none());

    let review_run = crate::agent::child_reviewer::begin_child_run(
        &f.db,
        &f.binding.child_run,
        &f.binding.child_session,
        &f.root,
        "model",
        0,
        "review",
        None,
    )
    .unwrap();
    let review_session = format!("task-{review_run}");
    crate::agent::conversation::begin_turn(
        &f.db,
        &review_session,
        "model",
        "【审查材料/历史：user】核对证据",
    )
    .unwrap();
    let conn = f.db.0.lock().unwrap();
    crate::db::delete_session_completely(&conn, &f.binding.parent_session, Some(&f.root)).unwrap();
    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM chat_sessions WHERE id IN (?1, ?2)",
            [&f.binding.child_session, &review_session],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0, "删除父任务后，登记过的子会话不应遗留在普通列表");
    assert!(
        f.root.join("test.db").exists(),
        "共享工作区不得作为子会话数据删除"
    );
}

#[test]
fn handoff_receipt_is_separate_from_formal_delivery_acceptance() {
    let f=Fixture::new();f.finish_readonly();
    f.db.0.lock().unwrap().execute("UPDATE agent_child_runs SET execution_config_json='{\"workflow\":{\"completion\":\"handoff\"}}' WHERE id=?1",[&f.binding.child_run]).unwrap();
    assert_eq!(settle(&f.db,&f.binding,&Ok(()),1).unwrap()["execution_status"],"failed","a delivery receipt cannot be reused as a handoff receipt");
    let mut cp=load_checkpoint(&f.db,&f.binding.child_session).unwrap().unwrap();cp.pending_finalization.as_mut().unwrap().handoff=true;crate::agent::checkpoint::save_checkpoint(&f.db,&mut cp).unwrap();
    let body=settle(&f.db,&f.binding,&Ok(()),1).unwrap();assert_eq!(body["execution_status"],"succeeded");assert_eq!(body["handoff_accepted"],true);assert_eq!(body["delivery_accepted"],false);
}

#[test]
fn nested_delegation_depth_and_workspace_owner_are_tracked_per_tree() {
    let token = uuid::Uuid::new_v4().to_string();
    let root = format!("depth-root-{token}");
    let root_run = format!("depth-run-{token}");
    let mut parent_session = root.clone();
    let mut parent_run = root_run.clone();
    let mut guards = Vec::new();
    assert_eq!(delegation_depth(&root).unwrap(), 0);
    for depth in 1..=MAX_DELEGATION_DEPTH {
        assert_eq!(check_delegation_depth(&parent_session).unwrap(), depth - 1);
        let binding = TaskBinding {parent_session:parent_session.clone(),parent_run:parent_run.clone(),
            child_session:format!("depth-{depth}-{token}"),child_run:format!("run-{depth}-{token}")};
        guards.push(TaskGuard::enter(binding.clone()));
        parent_session = binding.child_session;
        parent_run = binding.child_run;
        assert_eq!(delegation_depth(&parent_session).unwrap(), depth);
        assert_eq!(workspace_owner(&parent_session,&parent_run).unwrap(), (root.clone(),root_run.clone()));
    }
    assert!(check_delegation_depth(&parent_session).unwrap_err().contains("上限 5"));
    while let Some(guard) = guards.pop() { drop(guard); }
}

#[test]
fn desktop_tree_is_session_scoped_and_returns_real_parent_and_depth() {
    let f = Fixture::new();
    let root_id = &f.binding.child_run;
    let grandchild = crate::agent::child_reviewer::begin_child_run(&f.db, root_id, &f.binding.child_session, &f.root, "model",0,"task",None).unwrap();
    let grand_session = format!("task-{grandchild}");
    let great = crate::agent::child_reviewer::begin_child_run(&f.db, &grandchild, &grand_session, &f.root,"model",0,"task",None).unwrap();
    let nodes = visible_tasks(&f.db,&f.binding.parent_session,None).unwrap();
    assert!(nodes.iter().any(|(id,owner,root,depth)| id==root_id && owner==&f.binding.parent_session && root=="parent-run" && *depth==1));
    assert!(nodes.iter().any(|(id,owner,root,depth)| id==&great && owner==&grand_session && root=="parent-run" && *depth==3));
    assert!(visible_tasks(&f.db,"unrelated-session",Some(&great)).unwrap().is_empty());
    assert!(visible_tasks(&f.db,&f.binding.child_session,Some(&great)).unwrap().iter().any(|(_,_,root,depth)| root==root_id && *depth==2));
}

#[test]
fn nested_artifact_path_survives_parent_binding_cleanup() {
    let f = Fixture::new();
    let first = TaskGuard::enter(f.binding.clone());
    let id = crate::agent::child_reviewer::begin_child_run(&f.db, &f.binding.child_run, &f.binding.child_session, &f.root, "model", 0, "task", None).unwrap();
    let child_session = format!("task-{id}");
    let expected = crate::agent::path_guard::get_session_tmp_dir(&f.binding.child_session).join("delegated").join(&child_session);
    assert_eq!(result(&f.db, &f.binding.child_session, &id).unwrap()["artifact_base"], json!(expected));
    drop(first);
    assert_eq!(result(&f.db, &f.binding.child_session, &id).unwrap()["artifact_base"], json!(expected));
    let binding = TaskBinding {parent_session:f.binding.child_session.clone(),parent_run:f.binding.child_run.clone(),child_session,child_run:id.clone()};
    assert_eq!(settle(&f.db, &binding, &Ok(()), 0).unwrap()["artifact_base"], json!(expected));
    assert_eq!(result(&f.db, &f.binding.child_session, &id).unwrap()["artifact_base"], json!(expected));
}

#[test]
fn nested_progress_keeps_descendant_identity_across_hops() {
    let nested = relay_progress("grandchild", json!({"type":"agent_paused","message":"需要批准"})).unwrap();
    let relayed = relay_progress("child", nested.clone()).unwrap();
    assert_eq!(relayed, nested);
    assert_eq!(relayed["child_run_id"], "grandchild");
    assert_eq!(relayed["type"], "child_task_progress");
}

#[tokio::test]
async fn nested_delegation_shares_root_lock_until_last_lease_exits() {
    let f = Fixture::new();
    let root = crate::agent::workspace_lock::acquire_workspace_lock(&f.root, &f.binding.parent_session, &f.binding.parent_run, "parent").await.unwrap();
    let guard = TaskGuard::enter(f.binding.clone());
    let (session, run) = workspace_owner(&f.binding.child_session, &f.binding.child_run).unwrap();
    let nested = tokio::time::timeout(std::time::Duration::from_secs(2), crate::agent::workspace_lock::acquire_delegation_lock(&f.root, &session, &run)).await.unwrap().unwrap().unwrap();
    drop(root);
    let competing = crate::agent::workspace_lock::acquire_workspace_lock(&f.root, "other", "other-run", "other");
    assert!(tokio::time::timeout(std::time::Duration::from_millis(150), competing).await.is_err());
    drop(nested);
    drop(guard);
    let replacement = tokio::time::timeout(std::time::Duration::from_secs(2), crate::agent::workspace_lock::acquire_workspace_lock(&f.root, "other", "other-run", "other")).await.unwrap().unwrap();
    drop(replacement);
}

#[test]
fn status_projects_rich_subagent_metadata_and_recent_steps() {
    let f = Fixture::new();
    {
        let conn = f.db.0.lock().unwrap();
        conn.execute(
            "UPDATE agent_child_runs SET purpose='worker', model_id='claude-sonnet-4-20250514', execution_checkpoint_json=?2 WHERE id=?1",
            params![
                f.binding.child_run,
                json!({
                    "state": {
                        "dispatch_input": {
                            "params": {
                                "task": "排查子任务状态面板的样式与多层级树形排序",
                                "profile": "worker",
                                "role": "system_worker",
                                "role_label": "侦察探针"
                            }
                        }
                    }
                })
                .to_string()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_run_steps (run_id, seq, step_json) VALUES (?1, 0, ?2), (?1, 1, ?3)",
            params![
                f.binding.child_run,
                json!({
                    "id": 1,
                    "tool": "local_file",
                    "action": "read",
                    "status": "succeeded",
                    "description": "读取 child_task.rs 状态投影实现",
                    "params": { "path": "src-tauri/src/agent/child_task.rs" }
                })
                .to_string(),
                json!({
                    "id": 2,
                    "tool": "search",
                    "action": "rg",
                    "status": "running",
                    "description": "检索前端 control_child_task 调用点",
                    "params": { "query": "control_child_task" }
                })
                .to_string(),
            ],
        )
        .unwrap();
    }
    let body = status(&f.db, &f.binding.parent_session, &f.binding.child_run).unwrap();
    assert_eq!(body["purpose"], "worker");
    assert_eq!(body["role"], "system_worker");
    assert_eq!(body["role_label"], "侦察探针");
    assert_eq!(body["model_id"], "claude-sonnet-4-20250514");
    assert_eq!(body["task"], "排查子任务状态面板的样式与多层级树形排序");
    assert_eq!(body["latest_tool"], "search");
    assert_eq!(body["latest_step"], "检索前端 control_child_task 调用点");
    assert_eq!(body["total_steps"], 2);
    let steps = body["recent_steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["seq"], 1);
    assert_eq!(steps[0]["tool"], "local_file");
    assert_eq!(steps[0]["target"], "src-tauri/src/agent/child_task.rs");
    assert_eq!(steps[1]["seq"], 2);
    assert_eq!(steps[1]["tool"], "search");
    assert_eq!(steps[1]["target"], "control_child_task");
    assert!(body.get("task_truncated").is_none());

    let long_task = "长目标".repeat(120);
    f.db.0
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_child_runs SET execution_checkpoint_json=?2 WHERE id=?1",
            params![
                f.binding.child_run,
                json!({
                    "state": {
                        "dispatch_input": {
                            "params": {
                                "task": long_task,
                                "profile": "worker"
                            }
                        }
                    }
                })
                .to_string()
            ],
        )
        .unwrap();
    let truncated_body = status(&f.db, &f.binding.parent_session, &f.binding.child_run).unwrap();
    assert_eq!(truncated_body["task_truncated"], true);
    assert_eq!(truncated_body["task"].as_str().unwrap().chars().count(), 300);
}
