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
    let resource_id = crate::commands::projects::register_attachment_for_session(
        &conn,
        &session_id,
        std::path::Path::new(req.path.trim()),
        req.original_name.as_deref().unwrap_or("attachment"),
    )
    .map_err(|e| bad_request(&e))?;
    match resource_id {
        Some(id) => Ok(Json(json!({ "resource_id": id }))),
        None => Err(bad_request("该会话未关联项目，无法登记附件（请先绑定项目）")),
    }
}

/// 会话最近产物（供微信回推文件）
pub(crate) async fn handler_session_artifacts(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let db = state.app.state::<DbState>();
    let records = crate::agent::artifact_manager::list_session_artifacts(db.inner(), &session_id)
        .map_err(internal_error)?;
    let mut records = records;
    records.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    records.truncate(5);
    let artifacts: Vec<Value> = records
        .into_iter()
        .map(|record| {
            json!({
                "id": record.id,
                "display_name": record.display_name,
                "path": record.canonical_path,
                "size_bytes": record.size_bytes,
                "mime_type": record.mime_type,
                "kind": record.kind,
                "created_at": record.created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "artifacts": artifacts })))
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
