//! Web 视图与会话列表/轨迹详情 handler。

use super::*;

/// 自托管更新目录：app 数据根/updates/（publish_update.sh 落点）。
fn updates_dir() -> PathBuf {
    channel_config_path()
        .parent()
        .map(|p| p.join("updates"))
        .unwrap_or_else(|| PathBuf::from("updates"))
}

/// GET /update/latest.json：Tauri updater 的 manifest 端点（免鉴权——客户端拿不到 token）。
/// 文件不存在 = 还没发布过任何版本，回 404 让 updater 静默当"已是最新"。
pub(crate) async fn handler_update_manifest() -> impl axum::response::IntoResponse {
    let path = updates_dir().join("latest.json");
    match std::fs::read_to_string(&path) {
        Ok(body) => (
            StatusCode::OK,
            [("content-type", "application/json")],
            body,
        ),
        Err(_) => (
            StatusCode::NOT_FOUND,
            [("content-type", "application/json")],
            json!({ "error": "no update published" }).to_string(),
        ),
    }
}

/// GET /update/files/{name}：分发签名升级包本体。文件名白名单字符防目录穿越。
pub(crate) async fn handler_update_file(
    AxumPath(name): AxumPath<String>,
) -> impl axum::response::IntoResponse {
    let safe = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if !safe || name.starts_with('.') {
        return Err(StatusCode::BAD_REQUEST);
    }
    let path = updates_dir().join(&name);
    // canonicalize 双重校验：解析后必须仍在 updates/ 内
    let base = updates_dir().canonicalize().unwrap_or_else(|_| updates_dir());
    let resolved = path.canonicalize().unwrap_or_else(|_| path.clone());
    if !resolved.starts_with(&base) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let bytes = std::fs::read(&resolved).map_err(|_| StatusCode::NOT_FOUND)?;
    Ok((
        [("content-type", "application/octet-stream")],
        bytes,
    ))
}

/// GET / 与 /web：内嵌网页控制台入口（no-cache 保证升级二进制后页面同步更新）
pub(crate) async fn handler_web_home() -> impl axum::response::IntoResponse {
    // 门禁提示里的配置文件路径按本机实际位置渲染（macOS 是 Library/Application Support，
    // 模板里的 ~/.config/ 在 mac 上是错的）
    let page = CHANNEL_WEB_PAGE.replace(
        "{{CONFIG_PATH}}",
        &channel_config_path().display().to_string(),
    );
    (
        [(axum::http::header::CACHE_CONTROL, "no-cache")],
        Html(page),
    )
}

#[derive(Deserialize)]
pub(crate) struct SessionsQuery {
    pub(crate) project_id: Option<String>,
    /// scratch = 仅临时任务（未绑定项目，或绑定到 scratch 兜底项目的会话）
    pub(crate) scope: Option<String>,
}

/// 会话列表（含项目绑定与最近一次 trace 概要），网页端左栏/工作区卡片数据源。
/// 纯 SQL 便于单测；在途实时状态由 apply_run_overlay 在处理器里合并。
pub(crate) fn list_channel_sessions(
    conn: &rusqlite::Connection,
    project_id: Option<&str>,
    scratch_only: bool,
) -> Result<Vec<Value>, String> {
    // scratch 兜底项目（wechat-scratch）绑定的会话也算临时任务
    let scratch_project_id: Option<String> = conn
        .query_row(
            "SELECT id FROM agent_projects WHERE name=?1 LIMIT 1",
            rusqlite::params![SCRATCH_PROJECT_NAME],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    let where_clause = "(:pid IS NULL OR aps.project_id = :pid) \
                        AND (:scratch = 0 OR aps.project_id IS NULL OR aps.project_id = :scratch_pid)";
    let sql = format!(
        "SELECT s.id, s.title, s.model_id, s.created_at, s.updated_at, s.message_count,
                aps.project_id, p.name AS project_name, p.root_path,
                t.id AS trace_id, t.status AS trace_status, t.goal AS trace_goal,
                t.started_at AS trace_started_at, t.updated_at AS trace_updated_at,
                t.completed_at AS trace_completed_at
         FROM chat_sessions s
         LEFT JOIN agent_project_sessions aps ON aps.session_id = s.id
         LEFT JOIN agent_projects p ON p.id = aps.project_id
         LEFT JOIN agent_run_traces t ON t.id = (
             SELECT t2.id FROM agent_run_traces t2
             WHERE t2.session_id = s.id
             ORDER BY t2.started_at DESC, t2.id DESC LIMIT 1
         )
         WHERE {where_clause}
         ORDER BY COALESCE(t.updated_at, s.updated_at) DESC
         LIMIT 200"
    );
    let mut statement = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mut query = statement
        .query_map(
            rusqlite::named_params! {
                ":pid": project_id,
                ":scratch": scratch_only as i64,
                ":scratch_pid": scratch_project_id,
            },
            |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "title": row.get::<_, String>(1)?,
                    "model_id": row.get::<_, String>(2)?,
                    "created_at": row.get::<_, String>(3)?,
                    "updated_at": row.get::<_, String>(4)?,
                    "message_count": row.get::<_, i64>(5)?,
                    "project_id": row.get::<_, Option<String>>(6)?,
                    "project_name": row.get::<_, Option<String>>(7)?,
                    "root_path": row.get::<_, Option<String>>(8)?,
                    "trace_id": row.get::<_, Option<String>>(9)?,
                    "status": row.get::<_, Option<String>>(10)?,
                    "goal": row.get::<_, Option<String>>(11)?,
                    "trace_started_at": row.get::<_, Option<String>>(12)?,
                    "trace_updated_at": row.get::<_, Option<String>>(13)?,
                    "trace_completed_at": row.get::<_, Option<String>>(14)?,
                }))
            },
        )
        .map_err(|e| e.to_string())?;
    let mut sessions = Vec::new();
    for row in query.by_ref() {
        sessions.push(row.map_err(|e| e.to_string())?);
    }
    Ok(sessions)
}

/// 把在途 RunRecord 的实时状态合并进会话列表：
/// running/waiting_human 覆盖 trace 终态，并附带 run_id 供网页端应答/接管/取消。
pub(crate) fn apply_run_overlay(mut sessions: Vec<Value>) -> Vec<Value> {
    let runs = runs_store().lock().expect("runs store poisoned");
    for session in sessions.iter_mut() {
        let Some(session_id) = session.get("id").and_then(Value::as_str) else {
            continue;
        };
        let latest = runs
            .values()
            .filter(|run| run.session_id == session_id)
            .max_by(|a, b| a.started_at.cmp(&b.started_at))
            .cloned();
        let Some(run) = latest else {
            continue;
        };
        session["run_id"] = json!(run.run_id);
        if matches!(run.status.as_str(), "running" | "waiting_human") {
            session["status"] = json!(run.status);
            if run.status == "waiting_human" {
                session["paused_reason"] = json!(run.paused_reason);
                session["paused_form"] = json!(run.paused_form);
            }
        }
    }
    sessions
}

/// GET /channel/sessions?project_id=&scope=scratch
pub(crate) async fn handler_sessions(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumQuery(query): AxumQuery<SessionsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let conn = db_conn(&state.app)?;
    let scratch_only = query.scope.as_deref() == Some("scratch");
    let sessions = list_channel_sessions(&conn, query.project_id.as_deref(), scratch_only)
        .map_err(|e| bad_request(&e))?;
    drop(conn);
    let sessions = apply_run_overlay(sessions);
    Ok(Json(json!({ "sessions": sessions })))
}

/// GET /channel/session/{session_id}/traces：会话全部 run trace 的元数据（不含 steps）
pub(crate) async fn handler_session_traces(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let db = state.app.state::<DbState>();
    let records = crate::agent::run_trace::list_session_traces(db.inner(), session_id.trim())
        .map_err(internal_error)?;
    let traces: Vec<Value> = records
        .into_iter()
        .map(|mut trace| {
            let step_count = trace
                .get("steps")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            trace["step_count"] = json!(step_count);
            if let Some(object) = trace.as_object_mut() {
                object.remove("steps");
            }
            trace
        })
        .collect();
    Ok(Json(json!({ "traces": traces })))
}

/// 详情端点对步骤长文本字段统一截断，避免长程任务的 trace 响应过大
pub(crate) fn sanitize_trace_steps(steps: &mut Value) {
    let Some(entries) = steps.as_array_mut() else {
        return;
    };
    for entry in entries.iter_mut() {
        let Some(object) = entry.as_object_mut() else {
            continue;
        };
        for key in [
            "output",
            "thought",
            "reflection",
            "description",
            "action",
            "command",
            "params",
        ] {
            let oversized = object
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|text| text.chars().count() > TRACE_FIELD_TRUNCATE);
            if oversized {
                let truncated: String = object[key]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(TRACE_FIELD_TRUNCATE)
                    .collect();
                object.insert(
                    key.to_string(),
                    json!(format!("{truncated}……（已截断，完整内容见桌面端）")),
                );
            }
        }
    }
}

/// GET /channel/trace/{trace_id}：单次 run 的完整执行流（steps）
pub(crate) async fn handler_trace_detail(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(trace_id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let conn = db_conn(&state.app)?;
    let row = conn
        .query_row(
            "SELECT id, session_id, goal, status, steps_json, completion_json,
                    started_at, updated_at, completed_at
             FROM agent_run_traces WHERE id=?1",
            rusqlite::params![trace_id.trim()],
            |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "session_id": row.get::<_, String>(1)?,
                    "goal": row.get::<_, String>(2)?,
                    "status": row.get::<_, String>(3)?,
                    "steps_json": row.get::<_, String>(4)?,
                    "completion": row.get::<_, Option<String>>(5)?,
                    "started_at": row.get::<_, String>(6)?,
                    "updated_at": row.get::<_, String>(7)?,
                    "completed_at": row.get::<_, Option<String>>(8)?,
                }))
            },
        )
        .optional()
        .map_err(internal_error)?;
    let mut trace = row.ok_or_else(|| not_found("执行记录不存在"))?;
    // 行优先：增量持久化后 steps_json 只在启动/终态整写，中途读取须看 agent_run_steps
    let trace_id_owned = trace["id"].as_str().unwrap_or_default().to_string();
    let row_steps: Vec<Value> = conn
        .prepare("SELECT step_json FROM agent_run_steps WHERE run_id=?1 ORDER BY seq ASC")
        .and_then(|mut stmt| {
            stmt.query_map(rusqlite::params![trace_id_owned], |r| r.get::<_, String>(0))
                .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
        })
        .unwrap_or_default()
        .into_iter()
        .filter_map(|raw| serde_json::from_str::<Value>(&raw).ok())
        .collect();
    let mut steps = if !row_steps.is_empty() {
        Value::Array(row_steps)
    } else {
        let steps_json = trace["steps_json"].as_str().unwrap_or("[]").to_string();
        serde_json::from_str::<Value>(&steps_json).unwrap_or_else(|_| json!([]))
    };
    sanitize_trace_steps(&mut steps);
    let completion: Value = trace["completion"]
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or(Value::Null);
    if let Some(object) = trace.as_object_mut() {
        object.remove("steps_json");
        object.remove("completion");
        object.insert("steps".to_string(), steps);
        object.insert("completion".to_string(), completion);
    }
    Ok(Json(json!({ "trace": trace })))
}
