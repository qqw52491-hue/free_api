use crate::agent::types::DispatchResult;
use crate::db::DbState;
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub id: String,
    pub session_id: String,
    pub run_id: Option<String>,
    pub display_name: String,
    pub format: String,
    pub mime_type: Option<String>,
    pub kind: String,
    pub canonical_path: String,
    pub size_bytes: i64,
    pub sha256: Option<String>,
    pub origin: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 获取受管产物的根存储目录
pub fn get_managed_artifacts_root() -> PathBuf {
    let base = dirs_next::data_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("free-api-chat").join("artifacts")
}

/// 任务成功后自动登记运行期间新生成的工作区文件（Excel/PDF/文档等）。
/// 不依赖模型自觉调用 register_file——微信"完成后回推文件"链路因此不再断在模型忘记登记上。
/// 返回本次登记成功的产物记录。
pub fn auto_register_run_artifacts(
    db: &DbState,
    session_id: &str,
    run_id: Option<&str>,
    workspace: &std::path::Path,
    started_at: chrono::DateTime<chrono::Utc>,
) -> Vec<ArtifactRecord> {
    const MAX_AUTO_ARTIFACTS: usize = 5;
    const MAX_AUTO_BYTES: u64 = 25 * 1024 * 1024; // 与微信回推上限一致
    const SKIP_DIRS: &[&str] = &[
        ".git", ".attachments", "node_modules", "target", "dist",
        ".venv", "__pycache__", ".rpa_profile", ".worktrees",
    ];
    const MAX_DEPTH: usize = 3;

    fn walk(
        dir: &std::path::Path,
        depth: usize,
        since: std::time::SystemTime,
        out: &mut Vec<(std::path::PathBuf, std::time::SystemTime)>,
    ) {
        if depth > 3 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                walk(&path, depth + 1, since, out);
            } else if meta.len() <= 25 * 1024 * 1024 {
                let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                if mtime >= since {
                    out.push((path, mtime));
                }
            }
        }
    }

    let since: std::time::SystemTime = started_at.into();
    let mut candidates = Vec::new();
    walk(workspace, 0, since, &mut candidates);
    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    candidates.truncate(MAX_AUTO_ARTIFACTS);

    let mut registered = Vec::new();
    for (path, _) in candidates {
        if let Ok(record) =
            register_workspace_artifact(db, session_id, run_id, &path.to_string_lossy())
        {
            registered.push(record);
        }
    }
    registered
}

/// 清理并获取安全的文件名
fn sanitize_filename(input: &str, fallback_format: &str) -> (String, String) {
    let clean = input
        .trim()
        .replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_");
    let (name_part, format_part) = if let Some(dot_idx) = clean.rfind('.') {
        let ext = &clean[dot_idx + 1..].to_ascii_lowercase();
        let name = &clean[..dot_idx];
        (name.to_string(), ext.to_string())
    } else {
        (clean, fallback_format.to_ascii_lowercase().replace('.', ""))
    };

    let safe_name = if name_part.is_empty() {
        format!("artifact_{}", Utc::now().timestamp_millis())
    } else {
        name_part
    };
    let safe_ext = if format_part.is_empty() {
        "txt".to_string()
    } else {
        format_part
    };

    (format!("{}.{}", safe_name, safe_ext), safe_ext)
}

/// 根据文件格式推断 MIME 类型与卡片 Kind
fn infer_mime_and_kind(format: &str) -> (String, String) {
    match format.to_ascii_lowercase().as_str() {
        "md" | "markdown" => (
            "text/markdown;charset=utf-8".to_string(),
            "document".to_string(),
        ),
        "html" | "htm" => ("text/html;charset=utf-8".to_string(), "web".to_string()),
        "json" => (
            "application/json;charset=utf-8".to_string(),
            "chart".to_string(),
        ),
        "pdf" => ("application/pdf".to_string(), "document".to_string()),
        "docx" => (
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_string(),
            "document".to_string(),
        ),
        "xlsx" => (
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string(),
            "file".to_string(),
        ),
        "png" => ("image/png".to_string(), "image".to_string()),
        "jpg" | "jpeg" => ("image/jpeg".to_string(), "image".to_string()),
        "svg" => ("image/svg+xml".to_string(), "image".to_string()),
        "rs" | "ts" | "js" | "py" | "vue" | "css" | "sql" => {
            ("text/plain;charset=utf-8".to_string(), "file".to_string())
        }
        _ => ("text/plain;charset=utf-8".to_string(), "file".to_string()),
    }
}

/// 原子安全写入受管产物并登记持久化账本
pub fn save_managed_artifact(
    db: &DbState,
    session_id: &str,
    run_id: Option<&str>,
    title: &str,
    content: &str,
    format_opt: Option<&str>,
    kind_opt: Option<&str>,
    origin_opt: Option<&str>,
) -> Result<ArtifactRecord, String> {
    let now = Utc::now().to_rfc3339();
    let artifact_id = format!(
        "art_{}",
        &uuid::Uuid::new_v4().to_string().replace('-', "")[..16]
    );
    let fallback_format = format_opt.unwrap_or("md");
    let (filename, format) = sanitize_filename(title, fallback_format);
    let (mime_type, default_kind) = infer_mime_and_kind(&format);
    let kind = kind_opt.unwrap_or(&default_kind);
    let origin = origin_opt.unwrap_or("managed_generated");

    // 1. 确定会话受管目录
    let root = get_managed_artifacts_root();
    let session_dir = root.join(session_id).join(&artifact_id);
    std::fs::create_dir_all(&session_dir).map_err(|e| format!("无法创建产物存储目录: {}", e))?;

    let session_dir_canonical = session_dir
        .canonicalize()
        .map_err(|e| format!("无法规范化产物目录: {}", e))?;

    // 2. 临时文件原子写入并 fsync
    let temp_file_path = session_dir.join(format!(".tmp_{}", uuid::Uuid::new_v4()));
    let target_file_path = session_dir.join(&filename);

    {
        let mut file = std::fs::File::create(&temp_file_path)
            .map_err(|e| format!("无法创建临时产物文件: {}", e))?;
        file.write_all(content.as_bytes())
            .map_err(|e| format!("写入产物失败: {}", e))?;
        file.sync_all()
            .map_err(|e| format!("同步产物到磁盘失败: {}", e))?;
    }

    // 3. 原子重命名到正式文件名
    std::fs::rename(&temp_file_path, &target_file_path)
        .map_err(|e| format!("原子重命名产物文件失败: {}", e))?;

    // 4. Canonicalize 物理路径并做防逃逸边界检查
    let canonical_path = target_file_path
        .canonicalize()
        .map_err(|e| format!("无法获取产物绝对物理路径: {}", e))?;

    if !canonical_path.starts_with(&session_dir_canonical) {
        return Err("产物物理路径超出受管边界，拒绝登记".to_string());
    }

    let size_bytes = std::fs::metadata(&canonical_path)
        .map(|m| m.len() as i64)
        .unwrap_or(content.len() as i64);

    let canonical_str = canonical_path.to_string_lossy().to_string();

    let record = ArtifactRecord {
        id: artifact_id.clone(),
        session_id: session_id.to_string(),
        run_id: run_id.map(|s| s.to_string()),
        display_name: filename,
        format,
        mime_type: Some(mime_type),
        kind: kind.to_string(),
        canonical_path: canonical_str,
        size_bytes,
        sha256: None,
        origin: origin.to_string(),
        status: "ready".to_string(),
        created_at: now.clone(),
        updated_at: now,
    };

    // 5. 写入 SQLite 持久化账本
    let conn = db.0.lock().unwrap();
    conn.execute(
        "INSERT INTO agent_artifacts (
            id, session_id, run_id, display_name, format, mime_type, kind,
            canonical_path, size_bytes, sha256, origin, status, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            record.id,
            record.session_id,
            record.run_id,
            record.display_name,
            record.format,
            record.mime_type,
            record.kind,
            record.canonical_path,
            record.size_bytes,
            record.sha256,
            record.origin,
            record.status,
            record.created_at,
            record.updated_at,
        ],
    )
    .map_err(|e| format!("记录产物账本失败: {}", e))?;

    Ok(record)
}

/// 将工作区内已写入的真实文件自动登记入产物账本
pub fn register_workspace_artifact(
    db: &DbState,
    session_id: &str,
    run_id: Option<&str>,
    raw_path: &str,
) -> Result<ArtifactRecord, String> {
    let clean = raw_path.trim().trim_matches('"').trim_matches('\'');
    if clean.is_empty() || clean == "[object Object]" {
        return Err("无效的文件路径".to_string());
    }

    let mut path_buf = PathBuf::from(clean);
    if !path_buf.exists() {
        // 1. 优先尝试从当前会话专属临时受管目录 (session_tmp) 解析快捷路径（如 "tmp/xxx", "./tmp/xxx" 或直接文件名）
        let session_tmp = crate::agent::path_guard::get_session_tmp_dir(session_id);
        let stripped = clean.strip_prefix("./").unwrap_or(clean);
        let sub = stripped.strip_prefix("tmp/").unwrap_or(stripped);
        let candidate_tmp = session_tmp.join(sub);
        if candidate_tmp.exists() {
            path_buf = candidate_tmp;
        } else if let Ok(current_dir) = std::env::current_dir() {
            let candidate = current_dir.join(clean);
            if candidate.exists() {
                path_buf = candidate;
            } else if let Some(parent) = current_dir.parent() {
                let candidate_parent = parent.join(clean);
                if candidate_parent.exists() {
                    path_buf = candidate_parent;
                }
            }
        }
    }

    if !path_buf.exists() {
        return Err(format!("文件不存在: {}", clean));
    }

    let canonical = path_buf
        .canonicalize()
        .map_err(|e| format!("无法规范化路径: {}", e))?;
    let canonical_str = canonical.to_string_lossy().to_string();
    let filename = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("file")
        .to_ascii_lowercase();

    let (mime_type, kind) = infer_mime_and_kind(&ext);
    let size_bytes = std::fs::metadata(&canonical)
        .map(|m| m.len() as i64)
        .unwrap_or(0);
    let now = Utc::now().to_rfc3339();
    let artifact_id = format!(
        "art_{}",
        &uuid::Uuid::new_v4().to_string().replace('-', "")[..16]
    );

    let record = ArtifactRecord {
        id: artifact_id,
        session_id: session_id.to_string(),
        run_id: run_id.map(|s| s.to_string()),
        display_name: filename,
        format: ext,
        mime_type: Some(mime_type),
        kind,
        canonical_path: canonical_str,
        size_bytes,
        sha256: None,
        origin: "workspace_write".to_string(),
        status: "ready".to_string(),
        created_at: now.clone(),
        updated_at: now,
    };

    let conn = db.0.lock().unwrap();
    // 如果该文件路径已登记且有效，更新 run_id 和 updated_at，不重复插入多条
    let existing_id: Option<String> = conn
        .query_row(
            "SELECT id FROM agent_artifacts WHERE session_id = ?1 AND canonical_path = ?2 AND status != 'deleted' LIMIT 1",
            params![session_id, record.canonical_path],
            |row| row.get(0),
        )
        .ok();

    if let Some(eid) = existing_id {
        conn.execute(
            "UPDATE agent_artifacts SET run_id = ?1, size_bytes = ?2, updated_at = ?3 WHERE id = ?4",
            params![record.run_id, record.size_bytes, record.updated_at, eid],
        ).ok();
        drop(conn);
        return get_artifact_record(db, &eid);
    }

    conn.execute(
        "INSERT INTO agent_artifacts (
            id, session_id, run_id, display_name, format, mime_type, kind,
            canonical_path, size_bytes, sha256, origin, status, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            record.id,
            record.session_id,
            record.run_id,
            record.display_name,
            record.format,
            record.mime_type,
            record.kind,
            record.canonical_path,
            record.size_bytes,
            record.sha256,
            record.origin,
            record.status,
            record.created_at,
            record.updated_at,
        ],
    )
    .map_err(|e| format!("登记工作区产物失败: {}", e))?;

    Ok(record)
}

/// 查询会话下的所有有效产物记录
pub fn list_session_artifacts(
    db: &DbState,
    session_id: &str,
) -> Result<Vec<ArtifactRecord>, String> {
    list_artifacts_by_scope(db, session_id, None)
}

/// 按会话或回合作用域查询有效产物记录
pub fn list_artifacts_by_scope(
    db: &DbState,
    session_id: &str,
    run_id: Option<&str>,
) -> Result<Vec<ArtifactRecord>, String> {
    let conn = db.0.lock().unwrap();
    if let Some(r_id) = run_id {
        let mut stmt = conn
            .prepare(
                "SELECT id, session_id, run_id, display_name, format, mime_type, kind,
                    canonical_path, size_bytes, sha256, origin, status, created_at, updated_at
             FROM agent_artifacts
             WHERE session_id = ?1 AND run_id = ?2 AND status != 'deleted'
             ORDER BY created_at ASC",
            )
            .map_err(|e| format!("查询产物列表失败: {}", e))?;
        let rows = stmt
            .query_map(params![session_id, r_id], |row| {
                Ok(ArtifactRecord {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    run_id: row.get(2)?,
                    display_name: row.get(3)?,
                    format: row.get(4)?,
                    mime_type: row.get(5)?,
                    kind: row.get(6)?,
                    canonical_path: row.get(7)?,
                    size_bytes: row.get(8)?,
                    sha256: row.get(9)?,
                    origin: row.get(10)?,
                    status: row.get(11)?,
                    created_at: row.get(12)?,
                    updated_at: row.get(13)?,
                })
            })
            .map_err(|e| format!("读取产物行失败: {}", e))?;
        let mut result = Vec::new();
        for row in rows {
            if let Ok(item) = row {
                result.push(item);
            }
        }
        Ok(result)
    } else {
        let mut stmt = conn
            .prepare(
                "SELECT id, session_id, run_id, display_name, format, mime_type, kind,
                    canonical_path, size_bytes, sha256, origin, status, created_at, updated_at
             FROM agent_artifacts
             WHERE session_id = ?1 AND status != 'deleted'
             ORDER BY created_at ASC",
            )
            .map_err(|e| format!("查询产物列表失败: {}", e))?;
        let rows = stmt
            .query_map(params![session_id], |row| {
                Ok(ArtifactRecord {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    run_id: row.get(2)?,
                    display_name: row.get(3)?,
                    format: row.get(4)?,
                    mime_type: row.get(5)?,
                    kind: row.get(6)?,
                    canonical_path: row.get(7)?,
                    size_bytes: row.get(8)?,
                    sha256: row.get(9)?,
                    origin: row.get(10)?,
                    status: row.get(11)?,
                    created_at: row.get(12)?,
                    updated_at: row.get(13)?,
                })
            })
            .map_err(|e| format!("读取产物行失败: {}", e))?;
        let mut result = Vec::new();
        for row in rows {
            if let Ok(item) = row {
                result.push(item);
            }
        }
        Ok(result)
    }
}

/// 根据 ID 查询单个产物记录
pub fn get_artifact_record(db: &DbState, artifact_id: &str) -> Result<ArtifactRecord, String> {
    let conn = db.0.lock().unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT id, session_id, run_id, display_name, format, mime_type, kind,
                    canonical_path, size_bytes, sha256, origin, status, created_at, updated_at
             FROM agent_artifacts
             WHERE id = ?1",
        )
        .map_err(|e| format!("查询产物失败: {}", e))?;

    stmt.query_row(params![artifact_id], |row| {
        Ok(ArtifactRecord {
            id: row.get(0)?,
            session_id: row.get(1)?,
            run_id: row.get(2)?,
            display_name: row.get(3)?,
            format: row.get(4)?,
            mime_type: row.get(5)?,
            kind: row.get(6)?,
            canonical_path: row.get(7)?,
            size_bytes: row.get(8)?,
            sha256: row.get(9)?,
            origin: row.get(10)?,
            status: row.get(11)?,
            created_at: row.get(12)?,
            updated_at: row.get(13)?,
        })
    })
    .map_err(|e| format!("未找到产物记录 (ID: {}): {}", artifact_id, e))
}

/// 安全读取产物文件文本正文
pub fn read_artifact_content(db: &DbState, artifact_id: &str) -> Result<String, String> {
    let record = get_artifact_record(db, artifact_id)?;
    if record.status == "deleted" {
        return Err("该产物已被删除".to_string());
    }

    let path = Path::new(&record.canonical_path);
    if !path.exists() {
        return Err(format!("产物文件在磁盘上丢失: {}", record.canonical_path));
    }

    let metadata = std::fs::metadata(path).map_err(|e| format!("获取文件信息失败: {}", e))?;
    if metadata.len() > 10 * 1024 * 1024 {
        return Err("文件过大（超过 10MB），建议在系统默认程序中打开".to_string());
    }

    std::fs::read_to_string(path).map_err(|e| format!("读取文件内容失败: {}", e))
}

/// 打开产物文件
pub fn open_artifact(
    app: &tauri::AppHandle,
    db: &DbState,
    artifact_id: &str,
) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;

    let record = get_artifact_record(db, artifact_id)?;
    if record.status == "deleted" {
        return Err("产物已被删除".to_string());
    }

    let path = Path::new(&record.canonical_path);
    if !path.exists() {
        return Err(format!("文件不存在: {}", record.canonical_path));
    }

    app.opener()
        .open_path(&record.canonical_path, None::<&str>)
        .map_err(|e| format!("在系统中打开文件失败: {}", e))?;

    Ok(())
}

/// 在文件夹中定位产物文件
pub fn reveal_artifact(
    app: &tauri::AppHandle,
    db: &DbState,
    artifact_id: &str,
) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;

    let record = get_artifact_record(db, artifact_id)?;
    if record.status == "deleted" {
        return Err("产物已被删除".to_string());
    }

    let path = Path::new(&record.canonical_path);
    if !path.exists() {
        return Err(format!("文件不存在: {}", record.canonical_path));
    }

    app.opener()
        .reveal_item_in_dir(&record.canonical_path)
        .map_err(|e| format!("在文件管理器中定位失败: {}", e))?;

    Ok(())
}

/// 安全删除产物记录与磁盘文件
pub fn delete_artifact(db: &DbState, artifact_id: &str) -> Result<(), String> {
    let record = get_artifact_record(db, artifact_id)?;
    let now = Utc::now().to_rfc3339();

    // 1. 删除磁盘文件或所在单体目录
    let path = Path::new(&record.canonical_path);
    if path.exists() {
        let _ = std::fs::remove_file(path);
        if let Some(parent) = path.parent() {
            // 如果父目录只包含该产物且名字等于 artifact_id，清理该目录
            if parent.file_name().and_then(|n| n.to_str()) == Some(&record.id) {
                let _ = std::fs::remove_dir_all(parent);
            }
        }
    }

    // 2. 标记状态为 deleted
    let conn = db.0.lock().unwrap();
    conn.execute(
        "UPDATE agent_artifacts SET status = 'deleted', updated_at = ?1 WHERE id = ?2",
        params![now, artifact_id],
    )
    .map_err(|e| format!("更新产物状态失败: {}", e))?;

    Ok(())
}

/// 内置 Agent 工具 `artifact_manager` 的派发逻辑
pub fn dispatch(
    db: &DbState,
    session_id: &str,
    action: &str,
    params: &serde_json::Value,
) -> DispatchResult {
    if let Some(result) = crate::agent::tools::input_validation::native_rejection("artifact_manager", action, params) {
        return result;
    }
    let sub_action = if action.is_empty() {
        params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("create_document")
    } else {
        action
    };

    match sub_action {
        "create_document" | "save_artifact" | "write_document" => {
            let title = params.get("title")
                .or_else(|| params.get("name"))
                .or_else(|| params.get("filename"))
                .and_then(|v| v.as_str())
                .unwrap_or("deliverable.md");
            
            let content = params.get("content")
                .or_else(|| params.get("content_markdown"))
                .or_else(|| params.get("markdown"))
                .or_else(|| params.get("code"))
                .or_else(|| params.get("html"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let format = params.get("format")
                .or_else(|| params.get("ext"))
                .and_then(|v| v.as_str());

            let kind = params.get("kind").and_then(|v| v.as_str());

            match save_managed_artifact(db, session_id, None, title, content, format, kind, Some("managed_generated")) {
                Ok(rec) => {
                    let result_payload = json!({
                        "artifact_id": rec.id,
                        "name": rec.display_name,
                        "format": rec.format,
                        "kind": rec.kind,
                        "size_bytes": rec.size_bytes,
                        "canonical_path": rec.canonical_path,
                        "status": rec.status,
                        "message": format!("交付物文件「{}」已安全写入受管目录并登记入账 (ID: {})", rec.display_name, rec.id)
                    });
                    DispatchResult { view: None,
                        stdout: serde_json::to_string_pretty(&result_payload).unwrap_or_default(),
                        stderr: String::new(),
                        success: true,
                        route: "artifact_manager:create_document".to_string(),
                    }
                }
                Err(err) => DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("❌ [产物管理错误] 写入交付物失败: {}", err),
                    success: false,
                    route: "artifact_manager:create_document".to_string(),
                },
            }
        }
        "register_file" | "register_workspace_file" => {
            // 登记工作区中已写入的真实文件（如 excel 插件生成的 xlsx）入产物账本；
            // 不登记则任务完成时桥/桌面端查不到产物，微信回推文件链路断在最后一环
            let path = params.get("path")
                .or_else(|| params.get("file"))
                .or_else(|| params.get("filepath"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if path.trim().is_empty() {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: "❌ [产物管理错误] 必须提供 path 参数（工作区内文件路径）".to_string(),
                    success: false,
                    route: "artifact_manager:register_file".to_string(),
                };
            }

            match register_workspace_artifact(db, session_id, None, path) {
                Ok(rec) => {
                    let result_payload = json!({
                        "artifact_id": rec.id,
                        "name": rec.display_name,
                        "format": rec.format,
                        "kind": rec.kind,
                        "size_bytes": rec.size_bytes,
                        "canonical_path": rec.canonical_path,
                        "status": rec.status,
                        "message": format!("工作区文件「{}」已登记入产物账本，任务完成时将自动回推给用户 (ID: {})", rec.display_name, rec.id),
                    });
                    DispatchResult { view: None,
                        stdout: serde_json::to_string_pretty(&result_payload).unwrap_or_default(),
                        stderr: String::new(),
                        success: true,
                        route: "artifact_manager:register_file".to_string(),
                    }
                }
                Err(err) => DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("❌ [产物管理错误] 登记工作区产物失败: {}", err),
                    success: false,
                    route: "artifact_manager:register_file".to_string(),
                },
            }
        }
        "list_artifacts" | "list" => {
            match list_session_artifacts(db, session_id) {
                Ok(records) => {
                    DispatchResult { view: None,
                        stdout: serde_json::to_string_pretty(&records).unwrap_or_default(),
                        stderr: String::new(),
                        success: true,
                        route: "artifact_manager:list_artifacts".to_string(),
                    }
                }
                Err(err) => DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("❌ [产物管理错误] 查询产物失败: {}", err),
                    success: false,
                    route: "artifact_manager:list_artifacts".to_string(),
                },
            }
        }
        "read_artifact" | "get" => {
            let artifact_id = params.get("artifact_id")
                .or_else(|| params.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if artifact_id.is_empty() {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: "❌ [产物管理错误] 必须提供 artifact_id 参数".to_string(),
                    success: false,
                    route: "artifact_manager:read_artifact".to_string(),
                };
            }

            match read_artifact_content(db, artifact_id) {
                Ok(content) => DispatchResult { view: None,
                    stdout: content,
                    stderr: String::new(),
                    success: true,
                    route: "artifact_manager:read_artifact".to_string(),
                },
                Err(err) => DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("❌ [产物管理错误] 读取产物失败: {}", err),
                    success: false,
                    route: "artifact_manager:read_artifact".to_string(),
                },
            }
        }
        unknown => DispatchResult { view: None,
            stdout: String::new(),
            stderr: format!("❌ [产物管理错误] 未知的 action: '{}'。支持的 action: create_document, register_file, list_artifacts, read_artifact", unknown),
            success: false,
            route: "artifact_manager:unknown_action".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> DbState {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE agent_artifacts (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL, run_id TEXT,
                display_name TEXT NOT NULL, format TEXT NOT NULL, mime_type TEXT,
                kind TEXT NOT NULL, canonical_path TEXT NOT NULL,
                size_bytes INTEGER NOT NULL, sha256 TEXT,
                origin TEXT NOT NULL DEFAULT 'managed_generated',
                status TEXT NOT NULL DEFAULT 'ready',
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL);",
        )
        .unwrap();
        DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)))
    }

    #[test]
    fn register_file_dispatch_records_workspace_binary() {
        let db = test_db();
        let dir = std::env::temp_dir().join(format!("art-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("整理结果.xlsx");
        std::fs::write(&file, b"PK\x03\x04 fake xlsx bytes").unwrap();

        let result = dispatch(
            &db,
            "sess-1",
            "register_file",
            &serde_json::json!({ "path": file.to_string_lossy() }),
        );
        assert!(result.success, "登记应成功: {}", result.stderr);

        let record: (String, String, String, i64) = db
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT display_name, format, origin, size_bytes FROM agent_artifacts WHERE session_id='sess-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(record.0, "整理结果.xlsx");
        assert_eq!(record.1, "xlsx");
        assert_eq!(record.2, "workspace_write");
        assert_eq!(record.3 as u64, std::fs::metadata(&file).unwrap().len());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn register_file_dispatch_rejects_missing_path() {
        let db = test_db();
        let result = dispatch(&db, "sess-1", "register_file", &serde_json::json!({}));
        assert!(!result.success);
        assert!(result.stderr.contains("path"));
    }

    #[test]
    fn auto_register_picks_new_files_skips_hidden_and_stale() {
        let db = test_db();
        let ws = std::env::temp_dir().join(format!("art-auto-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(ws.join(".git")).unwrap();
        std::fs::write(ws.join("课程表.xlsx"), b"PK new file").unwrap();
        std::fs::write(ws.join("报告.md"), "# 新报告").unwrap();
        std::fs::write(ws.join(".git").join("config"), b"junk").unwrap();
        std::fs::write(ws.join(".hidden"), b"junk").unwrap();

        // since 取未来 30s 前：本轮新建文件（mtime=now）全部入选；
        // 早于 since 的旧输入件由 walk 的 mtime >= since 判断排除（不引入 mtime 测试依赖）
        let since = chrono::Utc::now() - chrono::Duration::seconds(30);
        let registered = auto_register_run_artifacts(&db, "sess-auto", Some("run-1"), &ws, since);

        let names: Vec<String> = registered.iter().map(|a| a.display_name.clone()).collect();
        assert_eq!(names.len(), 2, "应登记两个新文件: {names:?}");
        assert!(names.contains(&"课程表.xlsx".to_string()));
        assert!(names.contains(&"报告.md".to_string()));
        assert!(!names.iter().any(|n| n.contains("hidden")));
        assert!(!names.iter().any(|n| n.contains("config")));
        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn register_file_dispatch_resolves_tmp_path() {
        let db = test_db();
        let session = "sess-tmp-test";
        let session_tmp = crate::agent::path_guard::get_session_tmp_dir(session);
        std::fs::create_dir_all(&session_tmp).unwrap();
        let test_file = session_tmp.join("taiyuan_weather_5days.xlsx");
        std::fs::write(&test_file, b"fake excel bytes").unwrap();

        // 模拟 Agent 传入 tmp/ 相对路径登记产物
        let result = dispatch(&db, session, "register_file", &serde_json::json!({
            "path": "tmp/taiyuan_weather_5days.xlsx"
        }));
        assert!(result.success, "应能成功解析 tmp/ 路径: {}", result.stderr);
        assert!(result.stdout.contains("taiyuan_weather_5days.xlsx"));
        assert!(result.stdout.contains("已登记入产物账本"));

        let _ = std::fs::remove_dir_all(&session_tmp);
    }
    #[test]
    fn malformed_document_creates_no_file_or_record_but_explicit_empty_is_valid() {
        let db = test_db();
        let session = format!("artifact-input-{}", uuid::Uuid::new_v4());
        let dir = get_managed_artifacts_root().join(&session);
        for params in [serde_json::json!({}), serde_json::json!({"content":null}), serde_json::json!({"content":12})] {
            let result = dispatch(&db, &session, "create_document", &params);
            assert!(!result.success);
            assert_eq!(result.route, crate::agent::tools::input_validation::INVALID_INPUT_ROUTE);
        }
        assert!(!dir.exists(), "无效正文不得创建产物目录");
        let item_count:i64 = db.0.lock().unwrap().query_row("SELECT COUNT(*) FROM agent_artifacts", [], |r| r.get(0)).unwrap();
        assert_eq!(item_count, 0);
        let result = dispatch(&db, &session, "create_document", &serde_json::json!({"content":""}));
        assert!(result.success, "{}", result.stderr);
        let size:i64 = db.0.lock().unwrap().query_row("SELECT size_bytes FROM agent_artifacts", [], |r| r.get(0)).unwrap();
        assert_eq!(size, 0);
        std::fs::remove_dir_all(dir).ok();
    }

}
