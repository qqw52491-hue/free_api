use super::*;
use crate::agent::{context::SandwichContext, path_guard};
use serde_json::json;
use std::sync::{Arc, Mutex};

struct Fixture {
    base: PathBuf,
    primary: PathBuf,
    reference: PathBuf,
    session: String,
    project: String,
}
impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!("directory-access-{}", uuid::Uuid::new_v4()));
        // 按正式授权的契约使用物理路径，兼容系统临时目录别名和 Windows 路径前缀。
        std::fs::create_dir_all(&base).unwrap();
        let base = base.canonicalize().unwrap();
        let primary = base.join("primary");
        let reference = base.join("reference");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&reference).unwrap();
        let session = uuid::Uuid::new_v4().to_string();
        let project = uuid::Uuid::new_v4().to_string();
        bind_session(&session, &primary, Some((project.clone(), vec![])));
        Self {
            base,
            primary,
            reference,
            session,
            project,
        }
    }
    fn set(&self, access: DirectoryAccess) {
        update_project(
            &self.project,
            &[ProjectDirectory {
                path: self.reference.to_string_lossy().into(),
                access,
            }],
        );
    }
    fn db(&self) -> crate::db::DbState {
        crate::db::DbState(Arc::new(Mutex::new(
            crate::db::init_db(&self.base.join("test.db")).unwrap(),
        )))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        bind_session(&self.session, &self.primary, None);
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

#[test]
fn controlled_tools_read_only_write_and_revocation() {
    let f = Fixture::new();
    let db = f.db();
    f.set(DirectoryAccess::ReadOnly);
    let file = f.reference.join("probe.txt");
    std::fs::write(&file, "reference_symbol").unwrap();
    let params = json!({"path": file, "start_line": 1, "end_line": 1});
    assert!(
        crate::agent::tools::local_file::dispatch(
            &db,
            &f.session,
            "read",
            &params,
            Some(&f.primary)
        )
        .success
    );
    assert!(
        crate::agent::tools::local_file::dispatch(
            &db,
            &f.session,
            "list",
            &json!({"path": f.reference}),
            Some(&f.primary)
        )
        .success
    );
    let denied = crate::agent::tools::local_file::dispatch(
        &db,
        &f.session,
        "write",
        &json!({"path": file, "content": "changed"}),
        Some(&f.primary),
    );
    assert!(!denied.success);
    assert!(denied.stderr.contains("只读"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "reference_symbol");
    f.set(DirectoryAccess::ReadWrite);
    let edited = crate::agent::tools::local_file::dispatch(
        &db,
        &f.session,
        "edit",
        &json!({"path": file, "target_content": "reference_symbol", "replacement_content": "changed"}),
        Some(&f.primary),
    );
    assert!(edited.success, "{}", edited.stderr);
    assert!(
        crate::agent::tools::local_file::dispatch(
            &db,
            &f.session,
            "write",
            &json!({"path": f.reference.join("new.txt"), "content": "new"}),
            Some(&f.primary)
        )
        .success
    );
    f.set(DirectoryAccess::ReadOnly);
    assert!(path_guard::ensure_allowed_path(
        file.to_str().unwrap(),
        &f.primary,
        Some(&f.session),
        false
    )
    .is_err());
    update_project(&f.project, &[]);
    assert!(
        !crate::agent::tools::local_file::dispatch(
            &db,
            &f.session,
            "read",
            &params,
            Some(&f.primary)
        )
        .success
    );
}

#[test]
fn deepest_rule_wins_and_other_sessions_or_roots_do_not_inherit_access() {
    let f = Fixture::new();
    let nested = f.reference.join("nested");
    std::fs::create_dir(&nested).unwrap();
    let target = nested.join("new.txt");
    let outer = ProjectDirectory {
        path: f.reference.to_string_lossy().into(),
        access: DirectoryAccess::ReadWrite,
    };
    let inner = ProjectDirectory {
        path: nested.to_string_lossy().into(),
        access: DirectoryAccess::ReadOnly,
    };
    update_project(&f.project, &[outer.clone(), inner.clone()]);
    assert!(path_guard::ensure_readable_path(
        target.to_str().unwrap(),
        &f.primary,
        Some(&f.session),
        false
    )
    .is_ok());
    assert!(path_guard::ensure_allowed_path(
        target.to_str().unwrap(),
        &f.primary,
        Some(&f.session),
        false
    )
    .is_err());
    assert!(path_guard::ensure_readable_path(
        target.to_str().unwrap(),
        &f.primary,
        Some("other"),
        false
    )
    .is_err());
    assert!(path_guard::ensure_readable_path(
        target.to_str().unwrap(),
        &f.base.join("other"),
        Some(&f.session),
        false
    )
    .is_err());
    update_project(
        &f.project,
        &[
            ProjectDirectory {
                access: DirectoryAccess::ReadOnly,
                ..outer
            },
            ProjectDirectory {
                access: DirectoryAccess::ReadWrite,
                ..inner
            },
        ],
    );
    assert!(path_guard::ensure_allowed_path(
        target.to_str().unwrap(),
        &f.primary,
        Some(&f.session),
        false
    )
    .is_ok());
}

#[cfg(unix)]
#[test]
fn symlink_escape_dangling_links_and_replaced_roots_do_not_migrate_permissions() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    f.set(DirectoryAccess::ReadWrite);
    let outside = f.base.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "secret").unwrap();
    symlink(&outside, f.reference.join("escape")).unwrap();
    assert!(path_guard::ensure_readable_path(
        f.reference.join("escape/secret.txt").to_str().unwrap(),
        &f.primary,
        Some(&f.session),
        false
    )
    .is_err());
    symlink(outside.join("new.txt"), f.reference.join("dangling")).unwrap();
    assert!(path_guard::ensure_allowed_path(
        f.reference.join("dangling").to_str().unwrap(),
        &f.primary,
        Some(&f.session),
        false
    )
    .is_err());
    std::fs::remove_dir_all(&f.reference).unwrap();
    symlink(&outside, &f.reference).unwrap();
    assert!(path_guard::ensure_readable_path(
        f.reference.join("secret.txt").to_str().unwrap(),
        &f.primary,
        Some(&f.session),
        false
    )
    .is_err());
    assert!(path_guard::ensure_allowed_path(
        f.reference.join("new.txt").to_str().unwrap(),
        &f.primary,
        Some(&f.session),
        false
    )
    .is_err());
}

#[test]
fn runtime_context_update_keeps_task_contract_plan_and_review_state() {
    let f = Fixture::new();
    f.set(DirectoryAccess::ReadOnly);
    let mut ctx = SandwichContext::new("sys".into(), "original goal".into());
    ctx.current_goal = "active task".into();
    ctx.review_contract = Some(serde_json::from_value(json!({"scope_statement": "current", "obligations": [{"id":"check", "question":"verify", "status":"open"}]})).unwrap());
    ctx.project_resource_manifest = Some("cached immutable prefix".into());
    ctx.resource_scope =
        crate::agent::context::resource_scope::ResourceScopeContract::bind_primary_filesystem(
            f.primary.to_str().unwrap(),
            ResourceScopeSource::SessionSelected,
        );
    let mut cp = crate::agent::checkpoint::AgentCheckpoint::new(f.session.clone(), &ctx);
    cp.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
    cp.submitted_plan = Some(crate::agent::types::PlanSubmission {
        plan_summary: "keep plan".into(),
        ..Default::default()
    });
    let plan = serde_json::to_value(&cp.submitted_plan).unwrap();
    let contract = ctx.review_contract.clone();
    assert!(sync_context(&f.session, &f.primary, &mut ctx));
    assert!(!sync_context(&f.session, &f.primary, &mut ctx));
    let target = f.reference.join("new.txt");
    let params = json!({"path": target});
    assert!(ctx
        .resource_scope
        .guard_known_outside_write("write_file", &params)
        .is_some());
    f.set(DirectoryAccess::ReadWrite);
    assert!(sync_context(&f.session, &f.primary, &mut ctx));
    assert!(ctx
        .resource_scope
        .guard_known_outside_write("write_file", &params)
        .is_none());
    cp.refresh_context(&ctx);
    let saved = serde_json::to_string(&cp).unwrap();
    let saved: crate::agent::checkpoint::AgentCheckpoint = serde_json::from_str(&saved).unwrap();
    let mut resumed = SandwichContext::new("new".into(), "resume".into());
    // 与生产入口一致：先从项目选择绑定主目录，再恢复 checkpoint 的任务上下文。
    resumed.resource_scope =
        crate::agent::context::resource_scope::ResourceScopeContract::bind_primary_filesystem(
            f.primary.to_str().unwrap(),
            ResourceScopeSource::SessionSelected,
        );
    saved.restore_context(&mut resumed);
    f.set(DirectoryAccess::ReadOnly);
    sync_context(&f.session, &f.primary, &mut resumed);
    assert!(resumed
        .resource_scope
        .guard_known_outside_write("write_file", &params)
        .is_some());
    update_project(&f.project, &[]);
    sync_context(&f.session, &f.primary, &mut resumed);
    assert!(resumed
        .resource_scope
        .bindings
        .iter()
        .all(|b| b.source != ResourceScopeSource::ProjectConfigured));
    assert_eq!(resumed.current_goal, "active task");
    assert_eq!(resumed.user_goal, "original goal");
    assert_eq!(resumed.review_contract, contract);
    assert_eq!(serde_json::to_value(&saved.submitted_plan).unwrap(), plan);
    assert_eq!(
        saved.plan_phase,
        crate::agent::checkpoint::PlanPhase::Approved
    );
    assert_eq!(
        resumed
            .project_resource_manifest
            .as_ref()
            .unwrap()
            .matches(MARKER)
            .count(),
        1
    );
    assert!(resumed
        .project_resource_manifest
        .as_ref()
        .unwrap()
        .starts_with("cached immutable prefix"));
}

#[test]
fn code_search_permission_check_precedes_reused_cache() {
    let f = Fixture::new();
    let db = f.db();
    f.set(DirectoryAccess::ReadOnly);
    std::fs::write(f.reference.join("probe.rs"), "fn reference_symbol() {}\n").unwrap();
    let params = json!({"query":"reference_symbol", "kinds":["code"], "path": f.reference});
    let result =
        crate::agent::retrieval::dispatch_search(&db, &f.session, &params, Some(&f.primary));
    assert!(result.success, "{}", result.stderr);
    assert!(result.stdout.contains("reference_symbol"));
    {
        let conn = db.0.lock().unwrap();
        conn.execute("INSERT INTO agent_run_traces (id, session_id, goal, status, steps_json, started_at, updated_at) VALUES (?1,?2,'g','running','[]','2026-01-01','2026-01-01')", rusqlite::params![f.project, f.session]).unwrap();
        conn.execute("INSERT INTO agent_run_steps (run_id, seq, step_json) VALUES (?1,0,?2)", rusqlite::params![f.project, json!({"id":1,"action":"search","status":"succeeded","params":params,"output":result.stdout,"output_id":"cached-directory-search"}).to_string()]).unwrap();
    }
    let cached =
        crate::agent::retrieval::dispatch_search(&db, &f.session, &params, Some(&f.primary));
    assert_eq!(cached.route, "retrieval-cache");
    update_project(&f.project, &[]);
    let denied =
        crate::agent::retrieval::dispatch_search(&db, &f.session, &params, Some(&f.primary));
    assert!(!denied.success);
    assert_eq!(denied.route, "search:path_denied");
    assert!(denied.stdout.is_empty());
}

#[test]
fn image_reads_inherit_read_only_reference_access() {
    let f = Fixture::new();
    f.set(DirectoryAccess::ReadOnly);
    let file = f.reference.join("probe.png");
    std::fs::write(&file, b"image-test").unwrap();
    assert!(crate::agent::tools::view_image::resolve_and_encode_image(
        file.to_str().unwrap(),
        &f.primary,
        &f.session
    )
    .is_ok());
    update_project(&f.project, &[]);
    assert!(crate::agent::tools::view_image::resolve_and_encode_image(
        file.to_str().unwrap(),
        &f.primary,
        &f.session
    )
    .is_err());
}

#[test]
fn shell_cwd_requires_write_access_but_this_is_not_script_sandboxing() {
    let f = Fixture::new();
    let db = f.db();
    f.set(DirectoryAccess::ReadOnly);
    let params = json!({"command":"echo workspace_access_test", "cwd":f.reference});
    let denied =
        crate::agent::tools::local_shell::dispatch(&db, &f.session, "", &params, Some(&f.primary));
    assert_eq!(denied.route, "local_shell:cwd_denied");
    assert!(denied.stderr.contains("只读"));
    f.set(DirectoryAccess::ReadWrite);
    let allowed =
        crate::agent::tools::local_shell::dispatch(&db, &f.session, "", &params, Some(&f.primary));
    assert!(allowed.success, "{}", allowed.stderr);
    assert!(allowed.stdout.contains("workspace_access_test"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parallel_file_branches_use_the_same_directory_permissions() {
    let f = Fixture::new();
    let db = f.db();
    f.set(DirectoryAccess::ReadOnly);
    let file = f.reference.join("probe.txt");
    std::fs::write(&file, "parallel-reference").unwrap();
    let registry = crate::agent::mcp::PluginRegistry::new();
    let read: crate::agent::plan_tree::PlanNode = serde_json::from_value(
        json!({"parallel":[{"tool":"local_file","args":{"action":"read","path":file}}]}),
    )
    .unwrap();
    let result = crate::agent::plan_tree_dispatch::run_plan_tree_sync(
        &db,
        &f.session,
        &read,
        &registry,
        Some(&f.primary),
    );
    assert!(result.success, "{}", result.stderr);
    assert!(result.stdout.contains("parallel-reference"));
    let write: crate::agent::plan_tree::PlanNode = serde_json::from_value(json!({"parallel":[{"tool":"local_file","args":{"action":"write","path":file,"content":"updated","force":true}}]})).unwrap();
    let denied = crate::agent::plan_tree_dispatch::run_plan_tree_sync(
        &db,
        &f.session,
        &write,
        &registry,
        Some(&f.primary),
    );
    assert!(!denied.success);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "parallel-reference"
    );
    f.set(DirectoryAccess::ReadWrite);
    assert!(
        crate::agent::plan_tree_dispatch::run_plan_tree_sync(
            &db,
            &f.session,
            &write,
            &registry,
            Some(&f.primary)
        )
        .success
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "updated");
}

#[test]
fn project_manager_delete_also_revokes_live_directory_grants() {
    let f = Fixture::new();
    let db = f.db();
    let file = f.reference.join("keep.txt");
    std::fs::write(&file, "keep").unwrap();
    let project_id;
    {
        let conn = db.0.lock().unwrap();
        let project = crate::commands::projects::create_project_row(
            &conn,
            "delete",
            f.primary.to_str().unwrap(),
        )
        .unwrap();
        let session = crate::commands::projects::create_project_session_row(
            &conn,
            &project.id,
            "session",
            "model",
        )
        .unwrap();
        crate::commands::projects::directories::set_project_directory_row(
            &conn,
            &project.id,
            f.reference.to_str().unwrap(),
            DirectoryAccess::ReadWrite,
        )
        .unwrap();
        crate::commands::projects::directories::bind_session_directories(
            &conn,
            &session.id,
            &f.primary,
            true,
        )
        .unwrap();
        bind_session(
            &f.session,
            &f.primary,
            Some((
                project.id.clone(),
                directories(Some(&session.id), &f.primary),
            )),
        );
        bind_session(&session.id, &f.primary, None);
        project_id = project.id;
    }
    assert!(path_guard::ensure_allowed_path(
        file.to_str().unwrap(),
        &f.primary,
        Some(&f.session),
        false
    )
    .is_ok());
    let result = crate::agent::tools::project_manager::dispatch(
        &db,
        "delete",
        &json!({"project_id": project_id, "confirm":true}),
    );
    assert!(result.success);
    assert!(directories(Some(&f.session), &f.primary).is_empty());
    assert!(path_guard::ensure_readable_path(
        file.to_str().unwrap(),
        &f.primary,
        Some(&f.session),
        false
    )
    .is_err());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep");
}
