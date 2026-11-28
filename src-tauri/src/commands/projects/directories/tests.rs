use super::*;
use crate::agent::{path_guard, workspace_access};

#[test]
fn directory_permissions_persist_rebind_revoke_and_cascade_without_touching_files() {
    let base = std::env::temp_dir().join(format!("directory-config-{}", Uuid::new_v4()));
    // 按正式授权的契约使用物理路径，兼容系统临时目录别名和 Windows 路径前缀。
    std::fs::create_dir_all(&base).unwrap();
    let base = base.canonicalize().unwrap();
    let primary = base.join("primary");
    let reference = base.join("reference");
    std::fs::create_dir_all(&primary).unwrap();
    std::fs::create_dir_all(&reference).unwrap();
    let file = reference.join("reference.txt");
    std::fs::write(&file, "keep me").unwrap();
    let db_path = base.join("test.db");
    let conn = crate::db::init_db(&db_path).unwrap();
    let project = create_project_row(&conn, "config", primary.to_str().unwrap()).unwrap();
    let session = create_project_session_row(&conn, &project.id, "session", "model").unwrap();
    bind_session_directories(&conn, &session.id, &primary, true).unwrap();
    assert!(path_guard::ensure_readable_path(
        file.to_str().unwrap(),
        &primary,
        Some(&session.id),
        false
    )
    .is_err());
    assert!(set_project_directory_row(&conn, &project.id, ".", DirectoryAccess::ReadOnly).is_err());
    assert!(set_project_directory_row(
        &conn,
        &project.id,
        primary.to_str().unwrap(),
        DirectoryAccess::ReadOnly
    )
    .is_err());
    set_project_directory_row(
        &conn,
        &project.id,
        reference.to_str().unwrap(),
        DirectoryAccess::ReadOnly,
    )
    .unwrap();
    assert!(path_guard::ensure_readable_path(
        file.to_str().unwrap(),
        &primary,
        Some(&session.id),
        false
    )
    .is_ok());
    assert!(path_guard::ensure_allowed_path(
        file.to_str().unwrap(),
        &primary,
        Some(&session.id),
        false
    )
    .unwrap_err()
    .contains("只读"));
    set_project_directory_row(
        &conn,
        &project.id,
        reference.to_str().unwrap(),
        DirectoryAccess::ReadWrite,
    )
    .unwrap();
    assert!(path_guard::ensure_allowed_path(
        file.to_str().unwrap(),
        &primary,
        Some(&session.id),
        false
    )
    .is_ok());
    assert_eq!(
        list_project_directories_row(&conn, &project.id)
            .unwrap()
            .len(),
        1
    );
    drop(conn);
    workspace_access::bind_session(&session.id, &primary, None);
    let conn = crate::db::init_db(&db_path).unwrap();
    bind_session_directories(&conn, &session.id, &primary, true).unwrap();
    assert!(path_guard::ensure_allowed_path(
        file.to_str().unwrap(),
        &primary,
        Some(&session.id),
        false
    )
    .is_ok());
    let other = create_project_row(&conn, "other", base.to_str().unwrap()).unwrap();
    let other_session = create_project_session_row(&conn, &other.id, "other", "model").unwrap();
    assert!(list_project_directories_row(&conn, &other.id)
        .unwrap()
        .is_empty());
    bind_session_directories(&conn, &other_session.id, &base, true).unwrap();
    assert!(workspace_access::directories(Some(&other_session.id), &base).is_empty());
    remove_project_directory_row(&conn, &project.id, reference.to_str().unwrap()).unwrap();
    assert!(path_guard::ensure_readable_path(
        file.to_str().unwrap(),
        &primary,
        Some(&session.id),
        false
    )
    .is_err());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep me");
    set_project_directory_row(
        &conn,
        &project.id,
        reference.to_str().unwrap(),
        DirectoryAccess::ReadOnly,
    )
    .unwrap();
    std::fs::remove_dir_all(&reference).unwrap();
    remove_project_directory_row(&conn, &project.id, reference.to_str().unwrap()).unwrap();
    std::fs::create_dir(&reference).unwrap();
    set_project_directory_row(
        &conn,
        &project.id,
        reference.to_str().unwrap(),
        DirectoryAccess::ReadOnly,
    )
    .unwrap();
    conn.execute(
        "DELETE FROM agent_projects WHERE id=?1",
        params![project.id],
    )
    .unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_project_directories",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(n, 0);
    workspace_access::revoke_project(&project.id);
    assert!(workspace_access::directories(Some(&session.id), &primary).is_empty());
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn unknown_access_value_is_rejected_instead_of_granting_write() {
    assert!(serde_json::from_str::<DirectoryAccess>("\"write\"").is_err());
    assert!(serde_json::from_str::<DirectoryAccess>("\"read_write\"").is_ok());
}

#[tokio::test(start_paused = true)]
async fn project_additional_write_lock_is_atomic_at_start_and_on_live_permission_changes() {
    let base = std::env::temp_dir().join(format!("project-directory-lock-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&base).unwrap();
    let base = base.canonicalize().unwrap();
    let first = base.join("first");
    let second = base.join("second");
    let third = base.join("third");
    for dir in [&first, &second, &third] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let conn = crate::db::init_db(&base.join("test.db")).unwrap();
    let p1 = create_project_row(&conn, "first", first.to_str().unwrap()).unwrap();
    let p2 = create_project_row(&conn, "second", second.to_str().unwrap()).unwrap();
    let a = create_project_session_row(&conn, &p1.id, "first", "model").unwrap();
    let b = create_project_session_row(&conn, &p2.id, "second", "model").unwrap();
    bind_session_directories(&conn, &a.id, &first, true).unwrap();
    bind_session_directories(&conn, &b.id, &second, true).unwrap();
    let lock = crate::agent::workspace_lock::acquire_workspace_lock;

    // Both configurations may be saved while idle. Both runs cannot write the same root.
    for project in [&p1, &p2] {
        set_project_directory_row(&conn, &project.id, third.to_str().unwrap(), DirectoryAccess::ReadWrite).unwrap();
    }
    let a_guard = lock(&first, &a.id, "project-a", "first").await.unwrap();
    assert!(lock(&second, &b.id, "project-b", "second").await.is_err());

    // A live conflicting grant is rejected without persisting or publishing new authority.
    remove_project_directory_row(&conn, &p2.id, third.to_str().unwrap()).unwrap();
    let b_guard = lock(&second, &b.id, "project-b", "second").await.unwrap();
    let denied = set_project_directory_row(&conn, &p2.id, third.to_str().unwrap(), DirectoryAccess::ReadWrite).unwrap_err();
    assert!(denied.contains("可写范围重叠"));
    assert!(list_project_directories_row(&conn, &p2.id).unwrap().is_empty());
    assert!(workspace_access::directories(Some(&b.id), &second).is_empty());

    // Revocation takes effect for permissions now; in-flight work keeps the lease until exit.
    remove_project_directory_row(&conn, &p1.id, third.to_str().unwrap()).unwrap();
    assert!(workspace_access::directories(Some(&a.id), &first).is_empty());
    assert!(set_project_directory_row(&conn, &p2.id, third.to_str().unwrap(), DirectoryAccess::ReadWrite).is_err());
    drop(a_guard);
    set_project_directory_row(&conn, &p2.id, third.to_str().unwrap(), DirectoryAccess::ReadWrite).unwrap();
    assert!(lock(&third, &a.id, "project-c", "third").await.is_err());

    drop(b_guard);
    workspace_access::revoke_session(&a.id);
    workspace_access::revoke_session(&b.id);
    drop(conn);
    std::fs::remove_dir_all(base).unwrap();
}
