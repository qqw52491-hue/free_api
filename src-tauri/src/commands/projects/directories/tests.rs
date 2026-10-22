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
