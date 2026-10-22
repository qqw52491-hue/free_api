//! 数据查询 handler：模型/角色卡/事件流/工件/评测报告/配置读写。

use super::*;

#[derive(Deserialize)]
pub struct ConfigPutReq {
    pub port: Option<u16>,
    pub bridge_port: Option<u16>,
    pub scratch_dir: Option<String>,
    pub model_id: Option<String>,
    pub pro: Option<String>,
    pub flash: Option<String>,
    pub vision: Option<String>,
    pub heartbeat_minutes: Option<u64>,
    pub help_text: Option<String>,
    pub auto_start_bridge: Option<bool>,
    pub allowlist: Option<Vec<String>>,
    pub llm_retries: Option<u32>,
    pub action_milestone_steps: Option<u32>,
    pub phase_pushes: Option<bool>,
    pub console_base_url: Option<String>,
    pub auto_start_tunnel: Option<bool>,
    pub tunnel_hostname: Option<String>,
    pub tunnel_dir: Option<String>,
}

// ---------------------------------------------------------------- 处理器

pub(crate) async fn handler_models(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let models = crate::commands::get_all_models_with_platform(state.app.state::<DbState>())
        .await
        .map_err(internal_error)?;
    Ok(Json(json!({ "models": models })))
}

/// 启用的角色卡（网页端「角色」选择器数据源；钉死 = 按作用域写 role_models）
pub(crate) async fn handler_role_cards(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let db = state.app.state::<DbState>();
    let cards = crate::agent::tools::role_manager::list_cards(db.inner(), true)
        .map_err(internal_error)?;
    let cards: Vec<Value> = cards
        .into_iter()
        .map(|card| {
            json!({
                "name": card.name,
                "label": card.label,
                "model_id": card.model_id,
                "scope_hints": card.scope_hints,
            })
        })
        .collect();
    Ok(Json(json!({ "role_cards": cards })))
}

#[derive(Deserialize)]
pub(crate) struct EventsQuery {
    pub(crate) token: Option<String>,
    /// 不传 = 订阅所有会话事件（首页列表刷新用）
    pub(crate) session_id: Option<String>,
}

/// GET /channel/events?token=&session_id= ：SSE 实时推送（EventSource 无法带 header，
/// token 走 query；进度/状态/终态即时下发，网页端收到后拉取最新数据渲染）。
pub(crate) async fn handler_events(
    AxumState(state): AxumState<Arc<ChannelState>>,
    AxumQuery(query): AxumQuery<EventsQuery>,
) -> Result<Sse<impl futures::Stream<Item = Result<SseEvent, std::convert::Infallible>>>, (StatusCode, Json<Value>)>
{
    let config = state.config.read().expect("config poisoned").clone();
    let token_ok = query
        .token
        .as_deref()
        .map(|v| v == config.token)
        .unwrap_or(false);
    if !token_ok {
        return Err(unauthorized());
    }
    let filter_session = query.session_id.clone();
    let rx = event_bus().subscribe();
    let stream = futures::stream::unfold(
        (rx, filter_session),
        move |(mut rx, filter_session)| async move {
            loop {
                match rx.recv().await {
                    Ok((session_id, payload)) => {
                        if let Some(want) = &filter_session {
                            if &session_id != want {
                                continue;
                            }
                        }
                        let data =
                            json!({ "session_id": session_id, "event": payload }).to_string();
                        return Some((
                            Ok::<_, std::convert::Infallible>(SseEvent::default().data(data)),
                            (rx, filter_session),
                        ));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    );
    Ok(Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default()))
}

#[derive(Deserialize)]
pub(crate) struct AttachmentReq {
    pub(crate) path: String,
    pub(crate) original_name: Option<String>,
}

/// 微信发来的媒体文件登记为项目受管资源（复制进 .attachments/ 隔离目录，返回句柄）
pub(crate) async fn handler_session_attachment(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    Json(req): Json<AttachmentReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let conn = db_conn(&state.app)?;
    let resource_id = register_channel_attachment(
        &conn, &session_id, std::path::Path::new(req.path.trim()),
        req.original_name.as_deref().unwrap_or("attachment"), &effective_scratch_dir(&config),
    ).map_err(|e| bad_request(&e))?;
    match resource_id {
        Some(id) => Ok(Json(json!({ "resource_id": id }))),
        None => Err(bad_request("该会话未关联项目，无法登记附件（请先绑定项目）")),
    }
}

/// 未绑定项目的微信附件使用既有临时工作区，仍按受管资源读取。
fn register_channel_attachment(
    conn: &rusqlite::Connection,
    session_id: &str,
    path: &std::path::Path,
    name: &str,
    scratch: &std::path::PathBuf,
) -> Result<Option<String>, String> {
    let metadata = std::fs::metadata(path).map_err(|e| format!("无法读取收到的附件: {e}"))?;
    if !metadata.is_file() { return Err("附件路径不是文件".into()); }
    if metadata.len() > super::upload::MAX_UPLOAD_BYTES as u64 { return Err("附件超过 25MB 上限".into()); }
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    if crate::commands::projects::project_for_session(&tx, session_id)?.is_none() {
        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM chat_sessions WHERE id=?1)", [session_id], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if !exists { return Err("附件会话不存在".into()); }
        let project_id = ensure_scratch_project(&tx, scratch)?;
        tx.execute("INSERT INTO agent_project_sessions (project_id, session_id, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![project_id, session_id, now_ts()]).map_err(|e| e.to_string())?;
    }
    let resource = crate::commands::projects::register_attachment_for_session(&tx, session_id, path, name)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(resource)
}

#[derive(Deserialize, Default)]
pub(crate) struct ArtifactsQuery {
    run_id: Option<String>,
}

/// 可选按本次运行取产物，避免把同会话的旧文件再次推送。
pub(crate) async fn handler_session_artifacts(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    AxumQuery(query): AxumQuery<ArtifactsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) { return Err(unauthorized()); }
    let db = state.app.state::<DbState>();
    channel_artifacts(db.inner(), &session_id, query.run_id.as_deref())
        .map(Json).map_err(internal_error)
}

fn channel_artifacts(db: &DbState, session_id: &str, run_id: Option<&str>) -> Result<Value, String> {
    let mut records = crate::agent::artifact_manager::list_artifacts_by_scope(db, session_id, run_id)?;
    records.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let total = records.len();
    records.truncate(5);
    let artifacts: Vec<Value> = records.into_iter().map(|record| json!({
        "id": record.id, "run_id": record.run_id, "display_name": record.display_name,
        "path": record.canonical_path, "size_bytes": record.size_bytes, "mime_type": record.mime_type,
        "kind": record.kind, "created_at": record.created_at,
    })).collect();
    Ok(json!({ "artifacts": artifacts, "total": total, "has_more": total > artifacts.len() }))
}

pub(crate) fn eval_report_dirs(app: &AppHandle) -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join("agent_evaluations"));
    }
    if let Ok(conn) = db_conn(app) {
        if let Ok(mut stmt) = conn.prepare(
            "SELECT root_path FROM agent_projects WHERE root_path IS NOT NULL AND root_path != ''",
        ) {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                for p in rows.flatten() {
                    let d = std::path::PathBuf::from(p).join("agent_evaluations");
                    if d.is_dir() && !dirs.contains(&d) {
                        dirs.push(d);
                    }
                }
            }
        }
    }
    dirs
}

pub(crate) async fn handler_eval_list(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let dirs = eval_report_dirs(&state.app);
    let mut items: Vec<(std::time::SystemTime, Value)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for dir in dirs {
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(stem) = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .and_then(|name| name.strip_suffix(".json").map(str::to_string))
            else {
                continue;
            };
            if !seen.insert(stem.clone()) {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            let md_path = dir.join(format!("{stem}.md"));
            items.push((
                modified,
                json!({
                    "id": stem,
                    "has_markdown": md_path.is_file(),
                    "modified_secs": modified
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                }),
            ));
        }
    }
    items.sort_by(|a, b| b.0.cmp(&a.0));
    let evals: Vec<Value> = items.into_iter().map(|(_, v)| v).take(50).collect();
    Ok(Json(json!({ "evals": evals })))
}

// 评测报告 ID 白名单：仅 ASCII 字母数字与 - _ .，且禁止 ".."（防路径穿越，fail-closed）
pub(crate) fn is_safe_eval_id(eval_id: &str) -> bool {
    !eval_id.is_empty()
        && !eval_id.contains("..")
        && eval_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

pub(crate) async fn handler_eval_detail(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(eval_id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    if !is_safe_eval_id(&eval_id) {
        return Err(bad_request("非法的评测报告 ID"));
    }
    let mut resolved: Option<(std::path::PathBuf, &'static str)> = None;
    for dir in eval_report_dirs(&state.app) {
        let md_path = dir.join(format!("{eval_id}.md"));
        if md_path.is_file() {
            resolved = Some((md_path, "markdown"));
            break;
        }
        let json_path = dir.join(format!("{eval_id}.json"));
        if json_path.is_file() {
            resolved = Some((json_path, "json"));
        }
    }
    let Some((path, kind)) = resolved else {
        return Err(not_found("评测报告不存在或已清理"));
    };
    let content = std::fs::read_to_string(&path)
        .map_err(|_| not_found("评测报告不存在或已清理"))?;
    Ok(Json(json!({ "id": eval_id, "kind": kind, "content": content })))
}

pub(crate) async fn handler_config_get(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    Ok(Json(config_value(&config)))
}

pub(crate) async fn handler_config_put(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    Json(req): Json<ConfigPutReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    if let Some(v) = req.port {
        config.port = v;
    }
    if let Some(v) = req.bridge_port {
        config.bridge_port = v;
    }
    if let Some(v) = req.scratch_dir {
        config.scratch_dir = v.trim().to_string();
    }
    if let Some(v) = req.model_id {
        config.model_id = v.trim().to_string();
    }
    if let Some(v) = req.pro {
        config.pro = v.trim().to_string();
    }
    if let Some(v) = req.flash {
        config.flash = v.trim().to_string();
    }
    if let Some(v) = req.vision {
        config.vision = v.trim().to_string();
    }
    if let Some(v) = req.heartbeat_minutes {
        config.heartbeat_minutes = v.clamp(1, 24 * 60);
    }
    if let Some(v) = req.help_text {
        config.help_text = v;
    }
    if let Some(v) = req.auto_start_bridge {
        config.auto_start_bridge = v;
    }
    if let Some(v) = req.allowlist {
        config.allowlist = v.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    }
    if let Some(v) = req.llm_retries {
        config.llm_retries = Some(v.clamp(1, 100));
    }
    if let Some(v) = req.action_milestone_steps {
        config.action_milestone_steps = v.clamp(0, 1000);
    }
    if let Some(v) = req.phase_pushes {
        config.phase_pushes = v;
    }
    save_channel_config(&config).map_err(internal_error)?;
    *state.config.write().expect("config poisoned") = config.clone();
    Ok(Json(config_value(&config)))
}

pub(crate) fn config_value(config: &ChannelConfig) -> Value {
    json!({
        "port": config.port,
        "bridge_port": config.bridge_port,
        "scratch_dir": effective_scratch_dir(config).to_string_lossy(),
        "model_id": config.model_id,
        "pro": config.pro,
        "flash": config.flash,
        "vision": config.vision,
        "heartbeat_minutes": config.heartbeat_minutes,
        "help_text": config.help_text,
        "auto_start_bridge": config.auto_start_bridge,
        "allowlist": config.allowlist,
        "llm_retries": config.llm_retries,
        "action_milestone_steps": config.action_milestone_steps,
        "phase_pushes": config.phase_pushes,
        "console_base_url": config.console_base_url,
        "auto_start_tunnel": config.auto_start_tunnel,
        "tunnel_hostname": config.tunnel_hostname,
        "tunnel_dir": config.tunnel_dir,
        "tunnel_status": crate::agent::channel::tunnel::tunnel_status(),
        "config_path": channel_config_path().to_string_lossy(),
    })
}

// ---------------------------------------------------------------- 小工具

#[cfg(test)]
mod file_transfer_tests {
    use super::*;

    #[test]
    fn unbound_wechat_attachment_is_copied_to_scratch_and_readable_as_a_resource() {
        let base = std::env::temp_dir().join(format!("wechat-files-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let base = base.canonicalize().unwrap();
        let conn = crate::db::init_db(&base.join("test.db")).unwrap();
        let session = crate::commands::projects::create_plain_session_row(&conn, "微信 · 临时", "model").unwrap();
        let incoming = base.join("source.txt");
        std::fs::write(&incoming, "小文件正文🙂").unwrap();
        let scratch = base.join("scratch");
        let resource = register_channel_attachment(&conn, &session.id, &incoming, "报告.txt", &scratch).unwrap().unwrap();
        std::fs::remove_file(&incoming).unwrap(); // 原始下载路径消失后，受管副本仍可读取
        let preview = crate::commands::projects::read_project_resource_for_session(&conn, &session.id, &resource).unwrap();
        assert_eq!(preview.content, "小文件正文🙂");
        assert!(std::path::Path::new(&preview.resource.canonical_path).starts_with(scratch.join(".attachments")));
        assert!(register_channel_attachment(&conn, &session.id, &base, "directory", &scratch).is_err());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn returned_files_are_scoped_to_the_run_and_session_and_disclose_truncation() {
        let base = std::env::temp_dir().join(format!("wechat-artifacts-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(crate::db::init_db(&base.join("test.db")).unwrap())));
        for (index, session, run) in [(0, "s", "old"), (1, "other", "r"), (2, "s", "r"),
            (3, "s", "r"), (4, "s", "r"), (5, "s", "r"), (6, "s", "r"), (7, "s", "r")] {
            let path = base.join(format!("{index}.txt"));
            std::fs::write(&path, "output").unwrap();
            crate::agent::artifact_manager::register_workspace_artifact(&db, session, Some(run), path.to_str().unwrap()).unwrap();
        }
        let response = channel_artifacts(&db, "s", Some("r")).unwrap();
        assert_eq!(response["total"], 6);
        assert_eq!(response["has_more"], true);
        let files = response["artifacts"].as_array().unwrap();
        assert_eq!(files.len(), 5);
        assert!(files.iter().all(|file| file["run_id"] == "r" && file["display_name"] != "1.txt"));
        assert_eq!(channel_artifacts(&db, "s", Some("missing")).unwrap()["total"], 0);
        assert_eq!(channel_artifacts(&db, "s", None).unwrap()["total"], 7);
        let _ = std::fs::remove_dir_all(base);
    }
}
