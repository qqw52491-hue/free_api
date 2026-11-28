//! User-managed local Codex connections and project selection.
use crate::db::DbState;
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub(crate) const PURPOSE: &str = "external_codex";
pub(crate) const EXECUTOR: &str = "project_external";

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ExternalAgent {
    pub id: String,
    pub name: String,
    pub command: String,
    pub enabled: bool,
}

#[derive(Deserialize)]
pub(crate) struct ExternalAgentInput {
    id: Option<String>,
    name: String,
    command: String,
    enabled: bool,
}

fn read(row: &Row<'_>) -> rusqlite::Result<ExternalAgent> {
    Ok(ExternalAgent {
        id: row.get(0)?,
        name: row.get(1)?,
        command: row.get(2)?,
        enabled: row.get(3)?,
    })
}

fn save(db: &DbState, agent: ExternalAgentInput) -> Result<ExternalAgent, String> {
    let name = agent.name.trim().to_owned();
    let command = agent.command.trim().to_owned();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err("连接名称须为 1 至 80 个可显示字符。".into());
    }
    if !Path::new(&command).is_absolute() || command.chars().any(char::is_control) {
        return Err("请填写 Codex 可执行文件的绝对路径。".into());
    }
    let conn = db.writer()?;
    let id = if let Some(id) = agent.id {
        let changed = conn
            .execute(
                "UPDATE external_agents SET name=?2,command=?3,enabled=?4 WHERE id=?1",
                params![id, name, command, agent.enabled],
            )
            .map_err(|e| e.to_string())?;
        if changed == 0 {
            return Err("这条外部 Agent 连接已不存在。".into());
        }
        id
    } else {
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO external_agents(id,name,command,enabled) VALUES (?1,?2,?3,?4)",
            params![id, name, command, agent.enabled],
        )
        .map_err(|e| e.to_string())?;
        id
    };
    Ok(ExternalAgent {
        id,
        name,
        command,
        enabled: agent.enabled,
    })
}

fn select(db: &DbState, project: &str, agent: Option<&str>) -> Result<(), String> {
    let conn = db.writer()?;
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_projects WHERE id=?1)",
            [project],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !exists {
        return Err("项目已不存在。".into());
    }
    if let Some(id) = agent {
        let changed = conn
            .execute(
                "INSERT INTO agent_project_external_agents(project_id,agent_id)
             SELECT ?1,id FROM external_agents WHERE id=?2 AND enabled=1
             ON CONFLICT(project_id) DO UPDATE SET agent_id=excluded.agent_id",
                params![project, id],
            )
            .map_err(|e| e.to_string())?;
        if changed == 0 {
            return Err("外部 Agent 不存在或已停用。".into());
        }
    } else {
        conn.execute(
            "DELETE FROM agent_project_external_agents WHERE project_id=?1",
            [project],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(crate) fn active_for_session(
    db: &DbState,
    session: &str,
) -> Result<Option<(ExternalAgent, String)>, String> {
    let conn = db.reader()?;
    conn.query_row(
        "SELECT a.id,a.name,a.command,a.enabled,p.root_path
         FROM agent_project_sessions s JOIN agent_projects p ON p.id=s.project_id
         JOIN agent_project_external_agents b ON b.project_id=p.id
         JOIN external_agents a ON a.id=b.agent_id AND a.enabled=1 WHERE s.session_id=?1",
        [session],
        |row| Ok((read(row)?, row.get(4)?)),
    )
    .optional()
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn list_external_agents(
    state: tauri::State<'_, DbState>,
) -> Result<Vec<ExternalAgent>, String> {
    let conn = state.reader()?;
    let mut stmt = conn
        .prepare(
            "SELECT id,name,command,enabled FROM external_agents ORDER BY name COLLATE NOCASE,id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], read).map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn save_external_agent(
    state: tauri::State<'_, DbState>,
    agent: ExternalAgentInput,
) -> Result<ExternalAgent, String> {
    save(state.inner(), agent)
}

#[tauri::command]
pub(crate) async fn get_project_external_agent(
    state: tauri::State<'_, DbState>,
    project_id: String,
) -> Result<Option<String>, String> {
    state
        .reader()?
        .query_row(
            "SELECT agent_id FROM agent_project_external_agents WHERE project_id=?1",
            [project_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn set_project_external_agent(
    state: tauri::State<'_, DbState>,
    project_id: String,
    agent_id: Option<String>,
) -> Result<(), String> {
    select(state.inner(), &project_id, agent_id.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn external_connection_is_shared_but_selection_is_explicit_and_project_scoped() {
        let conn = crate::db::init_db(&Path::new(":memory:").to_path_buf()).unwrap();
        conn.execute_batch(
            "INSERT INTO agent_projects(id,name,root_path,created_at,updated_at) VALUES
                ('one','one','/one','now','now'), ('two','two','/two','now','now');
             INSERT INTO chat_sessions(id,title,model_id,created_at,updated_at) VALUES
                ('a','a','','now','now'), ('b','b','','now','now');
             INSERT INTO agent_project_sessions(session_id,project_id,created_at) VALUES
                ('a','one','now'), ('b','two','now');",
        )
        .unwrap();
        let db = DbState(Arc::new(Mutex::new(conn)));
        let path = std::env::current_exe()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let draft = |id, enabled| ExternalAgentInput {
            id,
            name: "Codex".into(),
            command: path.clone(),
            enabled,
        };
        let agent = save(&db, draft(None, true)).unwrap();
        assert!(active_for_session(&db, "a").unwrap().is_none());
        select(&db, "one", Some(&agent.id)).unwrap();
        assert_eq!(
            active_for_session(&db, "a").unwrap().unwrap().0.id,
            agent.id
        );
        assert!(active_for_session(&db, "b").unwrap().is_none());
        assert!(select(&db, "missing", Some(&agent.id)).is_err());

        save(&db, draft(Some(agent.id.clone()), false)).unwrap();
        assert!(select(&db, "two", Some(&agent.id)).is_err());
        assert!(active_for_session(&db, "a").unwrap().is_none());
        save(&db, draft(Some(agent.id.clone()), true)).unwrap();
        assert!(
            active_for_session(&db, "a").unwrap().is_some(),
            "re-enabling retains the binding"
        );
        select(&db, "one", None).unwrap();
        assert!(active_for_session(&db, "a").unwrap().is_none());
        select(&db, "one", Some(&agent.id)).unwrap();
        let conn = db.writer().unwrap();
        conn.execute("DELETE FROM agent_projects WHERE id='one'", [])
            .unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM agent_project_external_agents",
                [],
                |r| r.get::<_, usize>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM external_agents", [], |r| r
                .get::<_, usize>(0))
                .unwrap(),
            1
        );
    }
}
