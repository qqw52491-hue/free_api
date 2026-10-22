use super::*;
use crate::agent::{context::SandwichContext, path_guard};

struct Fixture {
    base: PathBuf,
    primary: PathBuf,
    reference: PathBuf,
    session: String,
    other: String,
    conn: Connection,
}
impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!("session-directory-{}", Uuid::new_v4()));
        // 按正式授权的契约使用物理路径，兼容系统临时目录别名和 Windows 路径前缀。
        std::fs::create_dir_all(&base).unwrap();
        let base = base.canonicalize().unwrap();
        let primary = base.join("primary");
        let reference = base.join("reference");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&reference).unwrap();
        std::fs::write(reference.join("keep.txt"), "keep").unwrap();
        let conn = crate::db::init_db(&base.join("test.db")).unwrap();
        let session = Uuid::new_v4().to_string();
        let other = Uuid::new_v4().to_string();
        for id in [&session, &other] {
            conn.execute("INSERT INTO chat_sessions (id,title,model_id,created_at,updated_at) VALUES (?1,'test','model','now','now')", params![id]).unwrap();
            workspace_access::bind_conversation(id, &primary, vec![]);
        }
        Self {
            base,
            primary,
            reference,
            session,
            other,
            conn,
        }
    }
    fn set(&self, access: DirectoryAccess) {
        set_row(
            &self.conn,
            &self.session,
            self.reference.to_str().unwrap(),
            access,
        )
        .unwrap();
    }
    fn read(&self, session: &str) -> bool {
        path_guard::ensure_readable_path(
            self.reference.join("keep.txt").to_str().unwrap(),
            &self.primary,
            Some(session),
            false,
        )
        .is_ok()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        workspace_access::revoke_session(&self.session);
        workspace_access::revoke_session(&self.other);
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

#[test]
fn session_permissions_are_live_isolated_and_write_includes_read() {
    let f = Fixture::new();
    assert!(!f.read(&f.session));
    assert!(!f.read(&f.other));
    f.set(DirectoryAccess::ReadOnly);
    assert!(f.read(&f.session));
    assert!(!f.read(&f.other));
    let target = f.reference.join("new.txt");
    assert!(path_guard::ensure_allowed_path(
        target.to_str().unwrap(),
        &f.primary,
        Some(&f.session),
        false
    )
    .unwrap_err()
    .contains("只读"));
    f.set(DirectoryAccess::ReadWrite);
    assert!(f.read(&f.session));
    assert!(path_guard::ensure_allowed_path(
        target.to_str().unwrap(),
        &f.primary,
        Some(&f.session),
        false
    )
    .is_ok());
    // 项目配置刷新不能把临时对话权限改掉。
    workspace_access::update_project(&f.session, &[]);
    assert!(f.read(&f.session));
    remove_row(&f.conn, &f.session, f.reference.to_str().unwrap()).unwrap();
    assert!(!f.read(&f.session));
    assert_eq!(
        std::fs::read_to_string(f.reference.join("keep.txt")).unwrap(),
        "keep"
    );
}

#[test]
fn session_permissions_persist_and_rebind_on_resume_without_affecting_another_session() {
    let f = Fixture::new();
    f.set(DirectoryAccess::ReadOnly);
    workspace_access::revoke_session(&f.session);
    assert!(!f.read(&f.session));
    let reopened = crate::db::init_db(&f.base.join("test.db")).unwrap();
    super::super::directories::bind_session_directories(&reopened, &f.session, &f.primary, false)
        .unwrap();
    assert!(f.read(&f.session));
    assert!(!f.read(&f.other));
    let config = config_row(&reopened, &f.session).unwrap();
    assert_eq!(config.root_path, f.primary.to_string_lossy());
    assert_eq!(config.directories.len(), 1);
    assert_eq!(config.directories[0].access, DirectoryAccess::ReadOnly);
}

#[test]
fn malformed_paths_unknown_sessions_and_project_sessions_cannot_mutate_session_permissions() {
    let f = Fixture::new();
    assert!(set_row(
        &f.conn,
        "unknown",
        f.reference.to_str().unwrap(),
        DirectoryAccess::ReadWrite
    )
    .is_err());
    assert!(set_row(&f.conn, &f.session, ".", DirectoryAccess::ReadOnly).is_err());
    assert!(set_row(
        &f.conn,
        &f.session,
        f.primary.to_str().unwrap(),
        DirectoryAccess::ReadOnly
    )
    .is_err());
    assert!(set_row(
        &f.conn,
        &f.session,
        f.reference.join("keep.txt").to_str().unwrap(),
        DirectoryAccess::ReadOnly
    )
    .is_err());
    assert!(set_row(
        &f.conn,
        &f.session,
        f.base.join("missing").to_str().unwrap(),
        DirectoryAccess::ReadOnly
    )
    .is_err());
    let project =
        super::super::create_project_row(&f.conn, "project", f.primary.to_str().unwrap()).unwrap();
    let session =
        super::super::create_project_session_row(&f.conn, &project.id, "project session", "model")
            .unwrap();
    assert!(config_row(&f.conn, &session.id)
        .unwrap_err()
        .contains("项目"));
    assert!(set_row(
        &f.conn,
        &session.id,
        f.reference.to_str().unwrap(),
        DirectoryAccess::ReadWrite
    )
    .is_err());
    assert!(read_session_directories(&f.conn, &f.session)
        .unwrap()
        .is_empty());
    assert!(serde_json::from_str::<DirectoryAccess>("\"write_only\"").is_err());
}

#[test]
fn offline_directory_can_be_removed_and_session_deletion_revokes_runtime_only() {
    let f = Fixture::new();
    f.set(DirectoryAccess::ReadOnly);
    std::fs::rename(&f.reference, f.base.join("offline")).unwrap();
    remove_row(&f.conn, &f.session, f.reference.to_str().unwrap()).unwrap();
    assert!(read_session_directories(&f.conn, &f.session)
        .unwrap()
        .is_empty());
    std::fs::rename(f.base.join("offline"), &f.reference).unwrap();
    f.set(DirectoryAccess::ReadWrite);
    set_row(
        &f.conn,
        &f.other,
        f.reference.to_str().unwrap(),
        DirectoryAccess::ReadOnly,
    )
    .unwrap();
    // 注入测试目录，绝不触碰真实应用目录或用户数据库。
    crate::db::delete_session_completely(&f.conn, &f.session, Some(&f.base)).unwrap();
    assert!(workspace_access::session_primary(&f.session).is_none());
    assert!(!f.read(&f.session));
    assert!(f.read(&f.other));
    assert!(read_session_directories(&f.conn, &f.session)
        .unwrap()
        .is_empty());
    assert_eq!(
        std::fs::read_to_string(f.reference.join("keep.txt")).unwrap(),
        "keep"
    );
}

#[test]
fn live_permission_context_refresh_keeps_original_task_plan_and_contract() {
    let f = Fixture::new();
    f.set(DirectoryAccess::ReadOnly);
    let mut ctx = SandwichContext::new("system".into(), "original goal".into());
    ctx.current_goal = "current goal".into();
    ctx.project_resource_manifest = Some("immutable prefix".into());
    ctx.review_contract = Some(serde_json::from_value(serde_json::json!({"scope_statement":"keep", "obligations":[{"id":"verify","question":"check","status":"open"}]})).unwrap());
    let mut cp = crate::agent::checkpoint::AgentCheckpoint::new(f.session.clone(), &ctx);
    cp.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
    cp.submitted_plan = Some(crate::agent::types::PlanSubmission {
        plan_summary: "keep plan".into(),
        ..Default::default()
    });
    let plan = serde_json::to_value(&cp.submitted_plan).unwrap();
    let contract = ctx.review_contract.clone();
    assert!(workspace_access::sync_context(
        &f.session, &f.primary, &mut ctx
    ));
    assert!(ctx
        .project_resource_manifest
        .as_ref()
        .unwrap()
        .contains("只读"));
    f.set(DirectoryAccess::ReadWrite);
    assert!(workspace_access::sync_context(
        &f.session, &f.primary, &mut ctx
    ));
    assert!(ctx
        .project_resource_manifest
        .as_ref()
        .unwrap()
        .contains("可读写"));
    assert!(workspace_access::scope_write_authorized(
        &ctx.resource_scope.bindings,
        f.reference.join("new.txt").to_str().unwrap()
    ));
    assert_eq!(ctx.user_goal, "original goal");
    assert_eq!(ctx.current_goal, "current goal");
    assert_eq!(ctx.review_contract, contract);
    assert_eq!(cp.plan_phase, crate::agent::checkpoint::PlanPhase::Approved);
    assert_eq!(serde_json::to_value(&cp.submitted_plan).unwrap(), plan);
    remove_row(&f.conn, &f.session, f.reference.to_str().unwrap()).unwrap();
    workspace_access::sync_context(&f.session, &f.primary, &mut ctx);
    assert!(!workspace_access::scope_write_authorized(
        &ctx.resource_scope.bindings,
        f.reference.join("new.txt").to_str().unwrap()
    ));
    assert!(ctx
        .project_resource_manifest
        .as_ref()
        .unwrap()
        .starts_with("immutable prefix"));
}

#[cfg(unix)]
#[test]
fn session_symlink_grants_stay_bound_to_canonical_directory_not_new_link_target() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let alias = f.base.join("alias");
    symlink(&f.reference, &alias).unwrap();
    let dirs = set_row(
        &f.conn,
        &f.session,
        alias.to_str().unwrap(),
        DirectoryAccess::ReadOnly,
    )
    .unwrap();
    assert_eq!(
        dirs[0].path,
        f.reference.canonicalize().unwrap().to_string_lossy()
    );
    assert!(f.read(&f.session));
    let outside = f.base.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "secret").unwrap();
    symlink(&outside, f.reference.join("escape")).unwrap();
    assert!(path_guard::ensure_readable_path(
        f.reference.join("escape/secret.txt").to_str().unwrap(),
        &f.primary,
        Some(&f.session),
        false
    )
    .is_err());
}

// 使用原生临时目录路径作为用户输入：macOS 可含 /var 别名，Windows 可不含 canonicalize 的扩展前缀。
#[test]
fn native_directory_spelling_uses_persisted_physical_key_for_restore_and_offline_revocation() {
    let f = Fixture::new();
    let raw_base = std::env::temp_dir().join(f.base.file_name().unwrap());
    let raw_reference = raw_base.join("reference").join(".");
    let file = f.reference.join("中文 reference.txt");
    std::fs::write(&file, "native directory evidence").unwrap();
    let granted = set_row(&f.conn, &f.session, raw_reference.to_str().unwrap(), DirectoryAccess::ReadOnly).unwrap();
    let key = granted[0].path.clone();
    assert_eq!(Path::new(&key), f.reference.canonicalize().unwrap());
    assert!(path_guard::ensure_readable_path(
        raw_reference.join("中文 reference.txt").to_str().unwrap(),
        &raw_base.join("primary"), Some(&f.session), false,
    ).is_ok());
    assert!(!f.read(&f.other));
    workspace_access::revoke_session(&f.session);
    super::super::directories::bind_session_directories(&f.conn, &f.session, &f.primary, false).unwrap();
    assert!(f.read(&f.session));
    assert!(path_guard::ensure_allowed_path(file.to_str().unwrap(), &f.primary, Some(&f.session), false).is_err());
    std::fs::remove_dir_all(&f.reference).unwrap();
    assert!(remove_row(&f.conn, &f.session, &key).unwrap().is_empty(), "目录离线时仍按保存键撤权");
    std::fs::create_dir(&f.reference).unwrap();
    std::fs::write(f.reference.join("keep.txt"), "recreated").unwrap();
    assert!(!f.read(&f.session), "重建同名目录不能恢复已撤销的授权");
}

#[cfg(unix)]
#[test]
fn retargeting_a_grant_alias_or_its_physical_root_does_not_move_permission_after_rebind() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let alias = f.base.join("chosen-alias");
    let outside = f.base.join("ungranted");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), "ungranted data").unwrap();
    symlink(&f.reference, &alias).unwrap();
    let granted = set_row(&f.conn, &f.session, alias.to_str().unwrap(), DirectoryAccess::ReadWrite).unwrap();
    assert_eq!(Path::new(&granted[0].path), f.reference);
    std::fs::remove_file(&alias).unwrap();
    symlink(&outside, &alias).unwrap();
    let rebind = || {
        workspace_access::revoke_session(&f.session);
        super::super::directories::bind_session_directories(&f.conn, &f.session, &f.primary, false).unwrap();
    };
    rebind();
    assert!(f.read(&f.session), "原物理目录的权限保留");
    assert!(path_guard::ensure_readable_path(alias.join("keep.txt").to_str().unwrap(), &f.primary, Some(&f.session), false).is_err());
    std::fs::remove_dir_all(&f.reference).unwrap();
    symlink(&outside, &f.reference).unwrap();
    rebind();
    assert!(!f.read(&f.session), "重新绑定时也不能把旧物理目录授权迁移到新链接目标");
    assert!(!workspace_access::scope_write_authorized(
        &{
            let mut context = SandwichContext::new("sys".into(), "goal".into());
            workspace_access::sync_context(&f.session, &f.primary, &mut context);
            context.resource_scope.bindings
        },
        f.reference.join("new.txt").to_str().unwrap(),
    ));
}

#[test]
fn session_search_revocation_is_checked_before_cached_evidence_is_returned() {
    use serde_json::json;
    let f = Fixture::new();
    f.set(DirectoryAccess::ReadOnly);
    std::fs::write(
        f.reference.join("probe.rs"),
        "fn session_reference_symbol() {}\n",
    )
    .unwrap();
    let db = crate::db::DbState(std::sync::Arc::new(std::sync::Mutex::new(
        crate::db::init_db(&f.base.join("test.db")).unwrap(),
    )));
    let params = json!({"query":"session_reference_symbol", "kinds":["code"], "path":f.reference});
    let result =
        crate::agent::retrieval::dispatch_search(&db, &f.session, &params, Some(&f.primary));
    assert!(result.success, "{}", result.stderr);
    assert!(result.stdout.contains("session_reference_symbol"));
    let run = Uuid::new_v4().to_string();
    f.conn.execute("INSERT INTO agent_run_traces (id, session_id, goal, status, steps_json, started_at, updated_at) VALUES (?1,?2,'g','running','[]','now','now')", params![run, f.session]).unwrap();
    f.conn.execute("INSERT INTO agent_run_steps (run_id, seq, step_json) VALUES (?1,0,?2)", params![run,json!({"id":1,"action":"search","status":"succeeded","params":params,"output":result.stdout,"output_id":"cached-session-search"}).to_string()]).unwrap();
    let cached =
        crate::agent::retrieval::dispatch_search(&db, &f.session, &params, Some(&f.primary));
    assert_eq!(cached.route, "retrieval-cache");
    remove_row(&f.conn, &f.session, f.reference.to_str().unwrap()).unwrap();
    let denied =
        crate::agent::retrieval::dispatch_search(&db, &f.session, &params, Some(&f.primary));
    assert!(!denied.success);
    assert_eq!(denied.route, "search:path_denied");
    assert!(denied.stdout.is_empty());
}

#[test]
fn session_file_image_and_shell_cwd_tools_use_the_shared_permission_table() {
    use serde_json::json;
    let f = Fixture::new();
    f.set(DirectoryAccess::ReadOnly);
    let db = crate::db::DbState(std::sync::Arc::new(std::sync::Mutex::new(
        crate::db::init_db(&f.base.join("test.db")).unwrap(),
    )));
    let file = f.reference.join("keep.txt");
    let read = crate::agent::tools::local_file::dispatch(
        &db,
        &f.session,
        "read",
        &json!({"path":file}),
        Some(&f.primary),
    );
    assert!(read.success, "{}", read.stderr);
    assert!(read.stdout.contains("keep"));
    let denied = crate::agent::tools::local_file::dispatch(
        &db,
        &f.session,
        "write",
        &json!({"path":file,"content":"changed"}),
        Some(&f.primary),
    );
    assert!(!denied.success);
    assert!(denied.stderr.contains("只读"));
    let image = f.reference.join("image.png");
    std::fs::write(&image, b"image-test").unwrap();
    assert!(crate::agent::tools::view_image::resolve_and_encode_image(
        image.to_str().unwrap(),
        &f.primary,
        &f.session
    )
    .is_ok());
    let command = json!({"command":"echo session_directory_test", "cwd":f.reference});
    let shell =
        crate::agent::tools::local_shell::dispatch(&db, &f.session, "", &command, Some(&f.primary));
    assert_eq!(shell.route, "local_shell:cwd_denied");
    f.set(DirectoryAccess::ReadWrite);
    let shell =
        crate::agent::tools::local_shell::dispatch(&db, &f.session, "", &command, Some(&f.primary));
    assert!(shell.success, "{}", shell.stderr);
    assert!(shell.stdout.contains("session_directory_test"));
    remove_row(&f.conn, &f.session, f.reference.to_str().unwrap()).unwrap();
    assert!(crate::agent::tools::view_image::resolve_and_encode_image(
        image.to_str().unwrap(),
        &f.primary,
        &f.session
    )
    .is_err());
    assert_eq!(std::fs::read_to_string(file).unwrap(), "keep");
}
