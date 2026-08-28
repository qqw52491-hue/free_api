use crate::db::{ChatSession, DbState};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::State;
use uuid::Uuid;

const MAX_PROJECT_NAME_CHARS: usize = 120;
const MAX_RESOURCE_LABEL_CHARS: usize = 200;
const MAX_RESOURCE_PREVIEW_CHARS: usize = 120_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProject {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProjectResource {
    pub resource_id: String,
    pub project_id: String,
    pub label: String,
    pub relative_path: String,
    pub canonical_path: String,
    pub kind: String,
    pub size_bytes: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectResourcePreview {
    pub resource: AgentProjectResource,
    pub content: String,
    pub truncated: bool,
}

fn canonical_directory(raw_path: &str) -> Result<PathBuf, String> {
    let raw = raw_path.trim();
    if raw.is_empty() {
        return Err("项目根目录不能为空".to_string());
    }
    let canonical = PathBuf::from(raw)
        .canonicalize()
        .map_err(|_| "项目根目录不存在或无法访问".to_string())?;
    if !canonical.is_dir() {
        return Err("项目根目录必须是目录".to_string());
    }
    Ok(canonical)
}

fn normalize_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("项目名称不能为空".to_string());
    }
    if name.chars().count() > MAX_PROJECT_NAME_CHARS {
        return Err(format!(
            "项目名称不能超过 {} 个字符",
            MAX_PROJECT_NAME_CHARS
        ));
    }
    Ok(name.to_string())
}

fn resource_kind(path: &Path) -> String {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "rs" | "ts" | "tsx" | "js" | "jsx" | "vue" | "py" | "go" | "java" | "kt" | "c" | "h"
        | "cpp" | "hpp" | "cs" | "swift" | "php" | "rb" | "sh" => "code".to_string(),
        "md" | "txt" | "json" | "yaml" | "yml" | "toml" | "xml" | "csv" => "text".to_string(),
        "pdf" => "pdf".to_string(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => "image".to_string(),
        _ => "file".to_string(),
    }
}

fn map_project(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentProject> {
    Ok(AgentProject {
        id: row.get(0)?,
        name: row.get(1)?,
        root_path: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

fn map_resource(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentProjectResource> {
    Ok(AgentProjectResource {
        resource_id: row.get(0)?,
        project_id: row.get(1)?,
        label: row.get(2)?,
        relative_path: row.get(3)?,
        canonical_path: row.get(4)?,
        kind: row.get(5)?,
        size_bytes: row.get(6)?,
        created_at: row.get(7)?,
    })
}

fn get_project(conn: &Connection, project_id: &str) -> Result<AgentProject, String> {
    conn.query_row(
        "SELECT id, name, root_path, created_at, updated_at FROM agent_projects WHERE id=?1",
        params![project_id],
        map_project,
    )
    .map_err(|_| "项目不存在或已删除".to_string())
}

fn create_project_row(
    conn: &Connection,
    name: &str,
    root_path: &str,
) -> Result<AgentProject, String> {
    let name = normalize_name(name)?;
    let canonical_root = canonical_directory(root_path)?;
    let canonical_root = canonical_root.to_string_lossy().to_string();
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO agent_projects (id, name, root_path, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, name, canonical_root, now, now],
    )
    .map_err(|error| format!("创建项目失败: {}", error))?;
    get_project(conn, &id)
}

fn create_project_session_row(
    conn: &Connection,
    project_id: &str,
    title: &str,
    model_id: &str,
) -> Result<ChatSession, String> {
    get_project(conn, project_id)?;
    let title = title.trim();
    if title.is_empty() {
        return Err("会话标题不能为空".to_string());
    }
    let model_id = model_id.trim();
    if model_id.is_empty() {
        return Err("请选择会话模型".to_string());
    }
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO chat_sessions (id, title, model_id, created_at, updated_at, message_count) VALUES (?1, ?2, ?3, ?4, ?5, 0)",
        params![id, title, model_id, now, now],
    )
    .map_err(|error| format!("创建项目会话失败: {}", error))?;
    conn.execute(
        "INSERT INTO agent_project_sessions (project_id, session_id, created_at) VALUES (?1, ?2, ?3)",
        params![project_id, id, now],
    )
    .map_err(|error| format!("关联项目会话失败: {}", error))?;
    Ok(ChatSession {
        id,
        title: title.to_string(),
        model_id: model_id.to_string(),
        created_at: now.clone(),
        updated_at: now,
        message_count: 0,
    })
}

fn canonical_project_file(
    conn: &Connection,
    project_id: &str,
    raw_path: &str,
) -> Result<(AgentProject, PathBuf), String> {
    let project = get_project(conn, project_id)?;
    let root = PathBuf::from(&project.root_path)
        .canonicalize()
        .map_err(|_| "项目根目录已不存在或无法访问".to_string())?;
    let path = PathBuf::from(raw_path.trim());
    let target = if path.is_relative() {
        root.join(path)
    } else {
        path
    };
    let requested = target
        .canonicalize()
        .map_err(|_| "资源文件不存在或无法访问".to_string())?;
    if !requested.is_file() {
        return Err("项目资源必须是可读取的普通文件".to_string());
    }
    if !requested.starts_with(&root) {
        return Err("资源文件位于项目根目录外，已拒绝登记".to_string());
    }
    Ok((project, requested))
}

fn list_project_resources_row(
    conn: &Connection,
    project_id: &str,
) -> Result<Vec<AgentProjectResource>, String> {
    get_project(conn, project_id)?;
    let mut statement = conn
        .prepare(
            "SELECT resource_id, project_id, label, relative_path, canonical_path, kind, size_bytes, created_at
             FROM agent_project_resources WHERE project_id=?1 ORDER BY created_at DESC",
        )
        .map_err(|error| error.to_string())?;
    let resources = statement
        .query_map(params![project_id], map_resource)
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string());
    resources
}

fn add_project_resource_row(
    conn: &Connection,
    project_id: &str,
    raw_path: &str,
) -> Result<AgentProjectResource, String> {
    let (project, canonical_path) = canonical_project_file(conn, project_id, raw_path)?;
    let metadata = std::fs::metadata(&canonical_path)
        .map_err(|error| format!("无法读取资源元数据: {}", error))?;
    let relative_path = canonical_path
        .strip_prefix(PathBuf::from(&project.root_path))
        .map_err(|_| "无法计算项目内相对路径".to_string())?
        .to_string_lossy()
        .to_string();
    let fallback_label = canonical_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("未命名文件");
    let label: String = fallback_label
        .chars()
        .take(MAX_RESOURCE_LABEL_CHARS)
        .collect();
    let canonical_path_text = canonical_path.to_string_lossy().to_string();
    let existing_id: Option<String> = conn
        .query_row(
            "SELECT resource_id FROM agent_project_resources WHERE project_id=?1 AND canonical_path=?2",
            params![project_id, canonical_path_text],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let resource_id = existing_id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO agent_project_resources (resource_id, project_id, label, relative_path, canonical_path, kind, size_bytes, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(project_id, canonical_path) DO UPDATE SET
            label=excluded.label, relative_path=excluded.relative_path, kind=excluded.kind, size_bytes=excluded.size_bytes",
        params![
            resource_id,
            project_id,
            label,
            relative_path,
            canonical_path_text,
            resource_kind(&canonical_path),
            metadata.len() as i64,
            now,
        ],
    )
    .map_err(|error| format!("登记项目资源失败: {}", error))?;
    conn.query_row(
        "SELECT resource_id, project_id, label, relative_path, canonical_path, kind, size_bytes, created_at
         FROM agent_project_resources WHERE resource_id=?1",
        params![resource_id],
        map_resource,
    )
    .map_err(|error| error.to_string())
}

pub(crate) fn project_for_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<AgentProject>, String> {
    conn.query_row(
        "SELECT p.id, p.name, p.root_path, p.created_at, p.updated_at
         FROM agent_projects p INNER JOIN agent_project_sessions ps ON ps.project_id=p.id
         WHERE ps.session_id=?1",
        params![session_id],
        map_project,
    )
    .optional()
    .map_err(|error| error.to_string())
}

pub(crate) fn project_workspace_for_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<PathBuf>, String> {
    let Some(project) = project_for_session(conn, session_id)? else {
        return Ok(None);
    };
    canonical_directory(&project.root_path).map(Some)
}

pub(crate) fn project_resources_for_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<AgentProjectResource>, String> {
    let Some(project) = project_for_session(conn, session_id)? else {
        return Ok(Vec::new());
    };
    list_project_resources_row(conn, &project.id)
}

pub(crate) fn add_project_resource_for_session(
    conn: &Connection,
    session_id: &str,
    raw_path: &str,
    label_override: Option<&str>,
) -> Result<AgentProjectResource, String> {
    let Some(project) = project_for_session(conn, session_id)? else {
        return Err("当前会话未关联项目，无法登记项目受管资源。".to_string());
    };
    let (project, canonical_path) = canonical_project_file(conn, &project.id, raw_path)?;
    let metadata = std::fs::metadata(&canonical_path)
        .map_err(|error| format!("无法读取资源元数据: {}", error))?;
    let relative_path = canonical_path
        .strip_prefix(PathBuf::from(&project.root_path))
        .map_err(|_| "无法计算项目内相对路径".to_string())?
        .to_string_lossy()
        .to_string();
    let fallback_label = canonical_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("未命名文件");
    let label: String = label_override
        .unwrap_or(fallback_label)
        .trim()
        .chars()
        .take(MAX_RESOURCE_LABEL_CHARS)
        .collect();
    let label = if label.is_empty() {
        fallback_label.to_string()
    } else {
        label
    };
    let canonical_path_text = canonical_path.to_string_lossy().to_string();
    let existing_id: Option<String> = conn
        .query_row(
            "SELECT resource_id FROM agent_project_resources WHERE project_id=?1 AND canonical_path=?2",
            params![project.id, canonical_path_text],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let resource_id = existing_id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO agent_project_resources (resource_id, project_id, label, relative_path, canonical_path, kind, size_bytes, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(project_id, canonical_path) DO UPDATE SET
            label=excluded.label, relative_path=excluded.relative_path, kind=excluded.kind, size_bytes=excluded.size_bytes",
        params![
            resource_id,
            project.id,
            label,
            relative_path,
            canonical_path_text,
            resource_kind(&canonical_path),
            metadata.len() as i64,
            now,
        ],
    )
    .map_err(|error| format!("登记项目资源失败: {}", error))?;
    conn.query_row(
        "SELECT resource_id, project_id, label, relative_path, canonical_path, kind, size_bytes, created_at
         FROM agent_project_resources WHERE resource_id=?1",
        params![resource_id],
        map_resource,
    )
    .map_err(|error| error.to_string())
}

pub(crate) fn read_project_resource_for_session(
    conn: &Connection,
    session_id: &str,
    resource_id: &str,
) -> Result<ProjectResourcePreview, String> {
    let Some(project) = project_for_session(conn, session_id)? else {
        return Err("当前会话未关联项目，无法读取项目资源".to_string());
    };
    let resource = conn
        .query_row(
            "SELECT resource_id, project_id, label, relative_path, canonical_path, kind, size_bytes, created_at
             FROM agent_project_resources WHERE project_id=?1 AND resource_id=?2",
            params![project.id, resource_id],
            map_resource,
        )
        .map_err(|_| "项目资源不存在或不属于当前会话项目".to_string())?;
    let (_, canonical_path) = canonical_project_file(conn, &project.id, &resource.canonical_path)?;
    let content = std::fs::read_to_string(canonical_path)
        .map_err(|_| "该资源不是可预览的 UTF-8 文本文件".to_string())?;
    let total_chars = content.chars().count();
    let preview: String = content.chars().take(MAX_RESOURCE_PREVIEW_CHARS).collect();
    Ok(ProjectResourcePreview {
        resource,
        truncated: total_chars > MAX_RESOURCE_PREVIEW_CHARS,
        content: preview,
    })
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectResourceExcerpt {
    pub resource_id: String,
    pub relative_path: String,
    pub content: String,
    pub truncated: bool,
}

pub(crate) fn read_project_resource_excerpts_for_session(
    conn: &Connection,
    session_id: &str,
    resource_ids: &[String],
    max_total_chars: usize,
) -> Result<Vec<ProjectResourceExcerpt>, String> {
    if resource_ids.is_empty() || resource_ids.len() > 2 {
        return Err("child investigator 必须指定 1–2 个受管资源 ID".to_string());
    }
    if max_total_chars == 0 {
        return Err("child investigator 资源摘录预算必须大于零".to_string());
    }
    let Some(project) = project_for_session(conn, session_id)? else {
        return Err("当前会话未关联项目，无法读取项目资源".to_string());
    };

    let mut seen = std::collections::BTreeSet::new();
    let mut remaining = max_total_chars;
    let mut excerpts = Vec::with_capacity(resource_ids.len());
    for raw_resource_id in resource_ids {
        if remaining == 0 {
            return Err("child investigator 受管资源总摘录超过预算".to_string());
        }
        let resource_id = raw_resource_id.trim();
        if resource_id.is_empty() || resource_id.chars().count() > 160 || !seen.insert(resource_id) {
            return Err("child investigator 的资源 ID 无效或重复".to_string());
        }
        let resource = conn
            .query_row(
                "SELECT resource_id, project_id, label, relative_path, canonical_path, kind, size_bytes, created_at
                 FROM agent_project_resources WHERE project_id=?1 AND resource_id=?2",
                params![project.id, resource_id],
                map_resource,
            )
            .map_err(|_| "项目资源不存在或不属于当前会话项目".to_string())?;
        let (_, canonical_path) = canonical_project_file(conn, &project.id, &resource.canonical_path)?;
        let metadata = std::fs::metadata(&canonical_path)
            .map_err(|_| "项目资源当前不可读取".to_string())?;
        if metadata.len() > 256 * 1024 {
            return Err("child investigator 单个受管资源不能超过 256 KiB".to_string());
        }
        let content = std::fs::read_to_string(canonical_path)
            .map_err(|_| "child investigator 只支持 UTF-8 文本资源".to_string())?;
        let total_chars = content.chars().count();
        let excerpt: String = content.chars().take(remaining).collect();
        let excerpt_chars = excerpt.chars().count();
        remaining = remaining.saturating_sub(excerpt_chars);
        excerpts.push(ProjectResourceExcerpt {
            resource_id: resource.resource_id,
            relative_path: resource.relative_path,
            truncated: excerpt_chars < total_chars,
            content: excerpt,
        });
    }
    Ok(excerpts)
}

#[tauri::command]
pub async fn list_agent_projects(state: State<'_, DbState>) -> Result<Vec<AgentProject>, String> {

    let conn = state.0.lock().map_err(|error| error.to_string())?;
    let mut statement = conn
        .prepare("SELECT id, name, root_path, created_at, updated_at FROM agent_projects ORDER BY updated_at DESC")
        .map_err(|error| error.to_string())?;
    let projects = statement
        .query_map([], map_project)
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string());
    projects
}

#[tauri::command]
pub async fn create_agent_project(
    state: State<'_, DbState>,
    name: String,
    root_path: String,
) -> Result<AgentProject, String> {
    let conn = state.0.lock().map_err(|error| error.to_string())?;
    create_project_row(&conn, &name, &root_path)
}

#[tauri::command]
pub async fn delete_agent_project(
    state: State<'_, DbState>,
    project_id: String,
) -> Result<(), String> {
    let conn = state.0.lock().map_err(|error| error.to_string())?;
    let affected = conn
        .execute(
            "DELETE FROM agent_projects WHERE id=?1",
            params![project_id],
        )
        .map_err(|error| error.to_string())?;
    if affected == 0 {
        return Err("项目不存在或已删除".to_string());
    }
    Ok(())
}

#[tauri::command]
pub async fn create_agent_project_session(
    state: State<'_, DbState>,
    project_id: String,
    title: String,
    model_id: String,
) -> Result<ChatSession, String> {
    let conn = state.0.lock().map_err(|error| error.to_string())?;
    create_project_session_row(&conn, &project_id, &title, &model_id)
}

#[tauri::command]
pub async fn list_agent_project_sessions(
    state: State<'_, DbState>,
    project_id: String,
) -> Result<Vec<ChatSession>, String> {
    let conn = state.0.lock().map_err(|error| error.to_string())?;
    get_project(&conn, &project_id)?;
    let mut statement = conn
        .prepare(
            "SELECT s.id, s.title, s.model_id, s.created_at, s.updated_at, s.message_count
             FROM chat_sessions s INNER JOIN agent_project_sessions ps ON ps.session_id=s.id
             WHERE ps.project_id=?1 ORDER BY s.updated_at DESC",
        )
        .map_err(|error| error.to_string())?;
    let sessions = statement
        .query_map(params![project_id], |row| {
            Ok(ChatSession {
                id: row.get(0)?,
                title: row.get(1)?,
                model_id: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
                message_count: row.get(5)?,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string());
    sessions
}

#[tauri::command]
pub async fn list_agent_project_resources(
    state: State<'_, DbState>,
    project_id: String,
) -> Result<Vec<AgentProjectResource>, String> {
    let conn = state.0.lock().map_err(|error| error.to_string())?;
    list_project_resources_row(&conn, &project_id)
}

#[tauri::command]
pub async fn add_agent_project_resource(
    state: State<'_, DbState>,
    project_id: String,
    path: String,
) -> Result<AgentProjectResource, String> {
    let conn = state.0.lock().map_err(|error| error.to_string())?;
    add_project_resource_row(&conn, &project_id, &path)
}

#[tauri::command]
pub async fn remove_agent_project_resource(
    state: State<'_, DbState>,
    project_id: String,
    resource_id: String,
) -> Result<(), String> {
    let conn = state.0.lock().map_err(|error| error.to_string())?;
    let affected = conn
        .execute(
            "DELETE FROM agent_project_resources WHERE project_id=?1 AND resource_id=?2",
            params![project_id, resource_id],
        )
        .map_err(|error| error.to_string())?;
    if affected == 0 {
        return Err("项目资源不存在或不属于该项目".to_string());
    }
    Ok(())
}

#[tauri::command]
pub async fn read_agent_project_resource(
    state: State<'_, DbState>,
    project_id: Option<String>,
    resource_id: String,
) -> Result<ProjectResourcePreview, String> {
    let conn = state.0.lock().map_err(|error| error.to_string())?;
    let resource = if let Some(pid) = project_id.as_deref().filter(|s| !s.is_empty()) {
        conn.query_row(
            "SELECT resource_id, project_id, label, relative_path, canonical_path, kind, size_bytes, created_at
             FROM agent_project_resources WHERE project_id=?1 AND resource_id=?2",
            params![pid, resource_id],
            map_resource,
        )
    } else {
        conn.query_row(
            "SELECT resource_id, project_id, label, relative_path, canonical_path, kind, size_bytes, created_at
             FROM agent_project_resources WHERE resource_id=?1",
            params![resource_id],
            map_resource,
        )
    }
    .map_err(|_| "项目资源不存在或已被删除".to_string())?;
    let (_, canonical_path) = canonical_project_file(&conn, &resource.project_id, &resource.canonical_path)?;
    let (content, truncated) = match std::fs::read_to_string(&canonical_path) {
        Ok(text) => {
            let total = text.chars().count();
            let is_trunc = total > MAX_RESOURCE_PREVIEW_CHARS;
            let preview: String = text.chars().take(MAX_RESOURCE_PREVIEW_CHARS).collect();
            (preview, is_trunc)
        }
        Err(_) => {
            // 尝试使用多格式解析引擎提取 (Excel / Word / PDF / Zip 等)
            if let Ok(extracted) = crate::agent::file_parser::parse_file(&canonical_path, Some(MAX_RESOURCE_PREVIEW_CHARS)) {
                let prompt_text = crate::agent::file_parser::format_for_prompt(&extracted, &resource.label);
                let total = prompt_text.chars().count();
                let is_trunc = total > MAX_RESOURCE_PREVIEW_CHARS;
                let preview: String = prompt_text.chars().take(MAX_RESOURCE_PREVIEW_CHARS).collect();
                (preview, is_trunc)
            } else {
                (
                    format!(
                        "[数据文件: {}，大小: {:.1} KB]\n该文件已作为项目受管数据，可在 Agent 会话中由相关数据工具或脚本直接处理与分析。",
                        resource.label, (resource.size_bytes as f64) / 1024.0
                    ),
                    false,
                )
            }
        }
    };
    Ok(ProjectResourcePreview {
        resource,
        truncated,
        content,
    })
}

#[tauri::command]
pub async fn get_agent_session_project(
    state: State<'_, DbState>,
    session_id: String,
) -> Result<Option<AgentProject>, String> {
    let conn = state.0.lock().map_err(|error| error.to_string())?;
    project_for_session(&conn, &session_id)
}

#[tauri::command]
pub async fn get_agent_session_project_resources(
    state: State<'_, DbState>,
    session_id: String,
) -> Result<Vec<AgentProjectResource>, String> {
    let conn = state.0.lock().map_err(|error| error.to_string())?;
    project_resources_for_session(&conn, &session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_resources_are_scoped_to_the_canonical_project_root_and_session() {
        let base = std::env::temp_dir().join(format!("free-api-projects-{}", Uuid::new_v4()));
        let root = base.join("project");
        let outside = base.join("outside.txt");
        let nested = root.join("docs").join("notes.md");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, "project-scoped resource").unwrap();
        std::fs::write(&outside, "outside resource").unwrap();
        let db_path = base.join("test.db");
        let conn = crate::db::init_db(&db_path).unwrap();

        let project = create_project_row(&conn, "项目 A", root.to_string_lossy().as_ref()).unwrap();
        let session = create_project_session_row(&conn, &project.id, "会话 A", "model-a").unwrap();
        let resource =
            add_project_resource_row(&conn, &project.id, nested.to_string_lossy().as_ref())
                .unwrap();

        assert_eq!(resource.relative_path, "docs/notes.md");
        assert_eq!(
            project_for_session(&conn, &session.id).unwrap().unwrap().id,
            project.id
        );
        assert_eq!(
            project_resources_for_session(&conn, &session.id)
                .unwrap()
                .len(),
            1
        );
        assert!(
            add_project_resource_row(&conn, &project.id, outside.to_string_lossy().as_ref())
                .is_err()
        );

        drop(conn);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn child_resource_excerpts_require_same_project_session_and_valid_ids() {
        let base = std::env::temp_dir().join(format!("free-api-project-excerpts-{}", Uuid::new_v4()));
        let root_a = base.join("project-a");
        let root_b = base.join("project-b");
        let resource_a = root_a.join("a.md");
        let resource_b = root_b.join("b.md");
        std::fs::create_dir_all(&root_a).unwrap();
        std::fs::create_dir_all(&root_b).unwrap();
        std::fs::write(&resource_a, "alpha resource").unwrap();
        std::fs::write(&resource_b, "bravo resource").unwrap();
        let conn = crate::db::init_db(&base.join("test.db")).unwrap();
        let project_a = create_project_row(&conn, "项目 A", root_a.to_string_lossy().as_ref()).unwrap();
        let project_b = create_project_row(&conn, "项目 B", root_b.to_string_lossy().as_ref()).unwrap();
        let session_a = create_project_session_row(&conn, &project_a.id, "会话 A", "model-a").unwrap();
        let resource_a = add_project_resource_row(&conn, &project_a.id, resource_a.to_string_lossy().as_ref()).unwrap();
        let resource_b = add_project_resource_row(&conn, &project_b.id, resource_b.to_string_lossy().as_ref()).unwrap();

        let own = vec![resource_a.resource_id.clone()];
        assert_eq!(
            read_project_resource_excerpts_for_session(&conn, &session_a.id, &own, 100)
                .unwrap()
                .len(),
            1
        );
        assert!(read_project_resource_excerpts_for_session(
            &conn,
            &session_a.id,
            &[resource_b.resource_id.clone()],
            100,
        )
        .unwrap_err()
        .contains("不属于当前会话项目"));
        assert!(read_project_resource_excerpts_for_session(&conn, "detached-session", &own, 100)
            .unwrap_err()
            .contains("未关联项目"));
        assert!(read_project_resource_excerpts_for_session(
            &conn,
            &session_a.id,
            &[resource_a.resource_id.clone(), resource_a.resource_id.clone()],
            100,
        )
        .is_err());
        assert!(read_project_resource_excerpts_for_session(
            &conn,
            &session_a.id,
            &["one".to_string(), "two".to_string(), "three".to_string()],
            100,
        )
        .is_err());

        drop(conn);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn child_resource_excerpts_truncate_and_recanonicalize_every_read() {
        let base = std::env::temp_dir().join(format!("free-api-project-excerpt-budget-{}", Uuid::new_v4()));
        let root = base.join("project");
        let first_path = root.join("first.md");
        let second_path = root.join("second.md");
        let outside_path = base.join("outside.md");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&first_path, "abcdef").unwrap();
        std::fs::write(&second_path, "second resource").unwrap();
        std::fs::write(&outside_path, "outside resource").unwrap();
        let conn = crate::db::init_db(&base.join("test.db")).unwrap();
        let project = create_project_row(&conn, "项目预算", root.to_string_lossy().as_ref()).unwrap();
        let session = create_project_session_row(&conn, &project.id, "预算会话", "model-a").unwrap();
        let first = add_project_resource_row(&conn, &project.id, first_path.to_string_lossy().as_ref()).unwrap();
        let second = add_project_resource_row(&conn, &project.id, second_path.to_string_lossy().as_ref()).unwrap();

        let single = read_project_resource_excerpts_for_session(
            &conn,
            &session.id,
            &[first.resource_id.clone()],
            4,
        )
        .unwrap();
        assert_eq!(single[0].content, "abcd");
        assert!(single[0].truncated);
        assert!(read_project_resource_excerpts_for_session(
            &conn,
            &session.id,
            &[first.resource_id.clone(), second.resource_id.clone()],
            4,
        )
        .unwrap_err()
        .contains("总摘录超过预算"));

        #[cfg(unix)]
        {
            std::fs::remove_file(&first_path).unwrap();
            std::os::unix::fs::symlink(&outside_path, &first_path).unwrap();
            assert!(read_project_resource_excerpts_for_session(
                &conn,
                &session.id,
                &[first.resource_id],
                100,
            )
            .unwrap_err()
            .contains("项目根目录外"));
        }

        drop(conn);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn deleting_a_project_cascades_resources_and_session_mapping_but_keeps_chat_history() {
        let base = std::env::temp_dir().join(format!("free-api-project-delete-{}", Uuid::new_v4()));
        let root = base.join("project");
        let resource_path = root.join("readme.md");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&resource_path, "registered project resource").unwrap();
        let conn = crate::db::init_db(&base.join("test.db")).unwrap();

        let project = create_project_row(&conn, "项目 B", root.to_string_lossy().as_ref()).unwrap();
        let session = create_project_session_row(&conn, &project.id, "会话 B", "model-b").unwrap();
        add_project_resource_row(&conn, &project.id, resource_path.to_string_lossy().as_ref())
            .unwrap();

        conn.execute(
            "DELETE FROM agent_projects WHERE id=?1",
            params![project.id],
        )
        .unwrap();

        assert!(project_for_session(&conn, &session.id).unwrap().is_none());
        assert!(project_resources_for_session(&conn, &session.id)
            .unwrap()
            .is_empty());
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM agent_project_resources", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM chat_sessions WHERE id=?1",
                params![session.id],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );

        drop(conn);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn adding_project_resource_via_session_registers_cross_session_resource() {
        let base = std::env::temp_dir().join(format!("free-api-session-resource-{}", Uuid::new_v4()));
        let root = base.join("project");
        let spec_path = root.join("docs").join("database_v2_spec.md");
        std::fs::create_dir_all(spec_path.parent().unwrap()).unwrap();
        std::fs::write(&spec_path, "# Database V2 Spec\nTables: users, orders").unwrap();
        let conn = crate::db::init_db(&base.join("test.db")).unwrap();

        let project = create_project_row(&conn, "电商系统", root.to_string_lossy().as_ref()).unwrap();
        let session_a = create_project_session_row(&conn, &project.id, "设计会话 A", "model-a").unwrap();
        let session_b = create_project_session_row(&conn, &project.id, "开发会话 B", "model-b").unwrap();

        // 会话 A 调用 add_project_resource_for_session 注册新文件
        let registered = add_project_resource_for_session(
            &conn,
            &session_a.id,
            "docs/database_v2_spec.md",
            Some("V2数据库设计规范"),
        )
        .unwrap();

        assert_eq!(registered.relative_path, "docs/database_v2_spec.md");
        assert_eq!(registered.label, "V2数据库设计规范");

        // 会话 B 立刻能看到会话 A 注册的项目资源
        let resources_b = project_resources_for_session(&conn, &session_b.id).unwrap();
        assert_eq!(resources_b.len(), 1);
        assert_eq!(resources_b[0].resource_id, registered.resource_id);
        assert_eq!(resources_b[0].label, "V2数据库设计规范");

        // 会话 B 可以读取内容
        let preview = read_project_resource_for_session(&conn, &session_b.id, &registered.resource_id).unwrap();
        assert!(preview.content.contains("# Database V2 Spec"));

        drop(conn);
        let _ = std::fs::remove_dir_all(base);
    }
}
