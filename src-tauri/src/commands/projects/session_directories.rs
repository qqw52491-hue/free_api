//! 用户在当前临时对话设置中显式授权；与项目配置隔离，不改变任务/审查状态。
use super::*;
use crate::agent::workspace_access::{self, DirectoryAccess, ProjectDirectory};

#[derive(Debug, Serialize)]
pub struct SessionDirectoryConfig {
    pub root_path: String,
    pub directories: Vec<ProjectDirectory>,
}

fn ensure_free_session(conn: &Connection, session: &str) -> Result<(), String> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM chat_sessions WHERE id=?1)",
            params![session],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !exists {
        return Err("当前对话不存在，请重新打开对话后配置目录。".into());
    }
    if project_for_session(conn, session)?.is_some() {
        return Err("项目对话请在项目的附加目录权限中配置。".into());
    }
    Ok(())
}

pub(super) fn read_session_directories(
    conn: &Connection,
    session: &str,
) -> Result<Vec<ProjectDirectory>, String> {
    let mut stmt = conn.prepare("SELECT canonical_path, access FROM agent_session_directories WHERE session_id=?1 ORDER BY canonical_path").map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![session], |r| {
            let access: String = r.get(1)?;
            Ok(ProjectDirectory {
                path: r.get(0)?,
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

fn session_workspace(session: &str) -> PathBuf {
    // 正在运行时保留该 run 的显式 workspace override；未运行时使用原有会话 tmp 路由。
    workspace_access::session_primary(session)
        .unwrap_or_else(|| crate::agent::path_guard::get_session_tmp_dir(session))
}
fn config_row(conn: &Connection, session: &str) -> Result<SessionDirectoryConfig, String> {
    ensure_free_session(conn, session)?;
    Ok(SessionDirectoryConfig {
        root_path: session_workspace(session).to_string_lossy().into_owned(),
        directories: read_session_directories(conn, session)?,
    })
}
fn refresh(conn: &Connection, session: &str) -> Result<Vec<ProjectDirectory>, String> {
    let dirs = read_session_directories(conn, session)?;
    let primary = session_workspace(session);
    workspace_access::bind_conversation(session, &primary, dirs.clone());
    Ok(dirs)
}
fn set_row(
    conn: &Connection,
    session: &str,
    raw: &str,
    access: DirectoryAccess,
) -> Result<Vec<ProjectDirectory>, String> {
    ensure_free_session(conn, session)?;
    let path = super::directories::canonical_extra_directory(raw, &session_workspace(session))?;
    conn.execute("INSERT INTO agent_session_directories (session_id, canonical_path, access, updated_at) VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT(session_id, canonical_path) DO UPDATE SET access=excluded.access, updated_at=excluded.updated_at",
        params![session, path.to_string_lossy(), access.as_str(), Utc::now().to_rfc3339()]).map_err(|e| e.to_string())?;
    refresh(conn, session)
}
fn remove_row(
    conn: &Connection,
    session: &str,
    path: &str,
) -> Result<Vec<ProjectDirectory>, String> {
    ensure_free_session(conn, session)?;
    // 离线目录也能撤销授权；这里只删除授权记录，不碰目录文件。
    conn.execute(
        "DELETE FROM agent_session_directories WHERE session_id=?1 AND canonical_path=?2",
        params![session, path],
    )
    .map_err(|e| e.to_string())?;
    refresh(conn, session)
}

#[tauri::command]
pub async fn get_agent_session_directory_config(
    state: State<'_, DbState>,
    session_id: String,
) -> Result<SessionDirectoryConfig, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    config_row(&conn, &session_id)
}
#[tauri::command]
pub async fn set_agent_session_directory(
    state: State<'_, DbState>,
    session_id: String,
    path: String,
    access: DirectoryAccess,
) -> Result<Vec<ProjectDirectory>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    set_row(&conn, &session_id, &path, access)
}
#[tauri::command]
pub async fn remove_agent_session_directory(
    state: State<'_, DbState>,
    session_id: String,
    path: String,
) -> Result<Vec<ProjectDirectory>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    remove_row(&conn, &session_id, &path)
}

#[cfg(test)]
mod tests;
