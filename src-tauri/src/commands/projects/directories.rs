//! 用户显式配置的项目附加目录；不复制、扫描或修改目录内容。
use super::*;
use crate::agent::workspace_access::{DirectoryAccess, ProjectDirectory};

pub(crate) fn list_project_directories_row(
    conn: &Connection,
    project_id: &str,
) -> Result<Vec<ProjectDirectory>, String> {
    get_project(conn, project_id)?;
    let mut stmt = conn.prepare("SELECT canonical_path, access FROM agent_project_directories WHERE project_id=?1 ORDER BY canonical_path")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![project_id], |row| {
            let access: String = row.get(1)?;
            Ok(ProjectDirectory {
                path: row.get(0)?,
                access: if access == "read_write" {
                    DirectoryAccess::ReadWrite
                } else {
                    DirectoryAccess::ReadOnly
                },
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

pub(crate) fn set_project_directory_row(
    conn: &Connection,
    project_id: &str,
    raw: &str,
    access: DirectoryAccess,
) -> Result<Vec<ProjectDirectory>, String> {
    let project = get_project(conn, project_id)?;
    let primary = canonical_directory(&project.root_path)?;
    let path = canonical_extra_directory(raw, &primary)?;
    let text = path.to_string_lossy().into_owned();
    conn.execute("INSERT INTO agent_project_directories (project_id, canonical_path, access, updated_at) VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT(project_id, canonical_path) DO UPDATE SET access=excluded.access, updated_at=excluded.updated_at",
        params![project_id, text, access.as_str(), Utc::now().to_rfc3339()]).map_err(|e| e.to_string())?;
    refresh_project_directories(conn, project_id)
}

pub(super) fn canonical_extra_directory(raw: &str, primary: &Path) -> Result<PathBuf, String> {
    if !Path::new(raw.trim()).is_absolute() {
        return Err("附加目录必须使用绝对路径，请选择本机目录。".into());
    }
    let path = canonical_directory(raw)?;
    let primary = primary.canonicalize().unwrap_or_else(|_| primary.into());
    if path == primary || path.starts_with(&primary) {
        return Err(
            "该目录已属于主工作区（可读写），无需重复添加；附加目录不会覆盖主工作区权限。".into(),
        );
    }
    Ok(path)
}

pub(crate) fn remove_project_directory_row(
    conn: &Connection,
    project_id: &str,
    path: &str,
) -> Result<Vec<ProjectDirectory>, String> {
    get_project(conn, project_id)?;
    // 使用保存的路径作键，目录离线/被删除后也能移除授权；无需 canonicalize。
    conn.execute(
        "DELETE FROM agent_project_directories WHERE project_id=?1 AND canonical_path=?2",
        params![project_id, path],
    )
    .map_err(|e| e.to_string())?;
    refresh_project_directories(conn, project_id)
}

fn refresh_project_directories(
    conn: &Connection,
    project_id: &str,
) -> Result<Vec<ProjectDirectory>, String> {
    let dirs = list_project_directories_row(conn, project_id)?;
    crate::agent::workspace_access::update_project(project_id, &dirs);
    Ok(dirs)
}

pub(crate) fn bind_session_directories(
    conn: &Connection,
    session_id: &str,
    workspace: &Path,
    is_project: bool,
) -> Result<(), String> {
    let access = if is_project {
        if let Some(project) = project_for_session(conn, session_id)? {
            Some((
                project.id.clone(),
                list_project_directories_row(conn, &project.id)?,
            ))
        } else {
            None
        }
    } else {
        None
    };
    if let Some(access) = access {
        crate::agent::workspace_access::bind_session(session_id, workspace, Some(access));
    } else {
        let dirs = super::session_directories::read_session_directories(conn, session_id)?;
        crate::agent::workspace_access::bind_conversation(session_id, workspace, dirs);
    }
    Ok(())
}

#[tauri::command]
pub async fn list_agent_project_directories(
    state: State<'_, DbState>,
    project_id: String,
) -> Result<Vec<ProjectDirectory>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    list_project_directories_row(&conn, &project_id)
}

#[tauri::command]
pub async fn set_agent_project_directory(
    state: State<'_, DbState>,
    project_id: String,
    path: String,
    access: DirectoryAccess,
) -> Result<Vec<ProjectDirectory>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    set_project_directory_row(&conn, &project_id, &path, access)
}

#[tauri::command]
pub async fn remove_agent_project_directory(
    state: State<'_, DbState>,
    project_id: String,
    path: String,
) -> Result<Vec<ProjectDirectory>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    remove_project_directory_row(&conn, &project_id, &path)
}

#[cfg(test)]
mod tests;
