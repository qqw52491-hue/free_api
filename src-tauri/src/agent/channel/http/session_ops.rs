//! 会话与任务入口 handler：建会话、删会话、下发任务、scratch 项目兜底。

use super::*;

#[derive(Deserialize)]
pub(crate) struct SessionReq {
    pub(crate) project_id: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) model_id: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct PartialRouting {
    pub(crate) pro: Option<String>,
    pub(crate) flash: Option<String>,
    pub vision: Option<String>,
    /// 上下文压缩策略：deep_coding（长程）| cost_saving（省流）| ultra_fast（极速）
    pub(crate) context_budget_mode: Option<String>,
    /// 路由模式：rules | scored
    pub(crate) routing_mode: Option<String>,
    /// 角色级覆盖：系统角色名（planner/executor/...）→ 档位或模型 UUID
    role_models: Option<std::collections::BTreeMap<String, String>>,
    /// 审查员跨家族自动优选开关：缺省/不传 = 关闭（跟随常规回退链）；
    /// true = 恢复自动挑选异构家族模型的旧行为。
    pub(crate) auto_cross_family_reviewer: Option<bool>,
}

#[derive(Deserialize)]
pub(crate) struct TaskReq {
    pub(crate) session_id: String,
    pub(crate) goal: String,
    pub(crate) model_routing: Option<PartialRouting>,
    #[serde(default)]
    pub(crate) plan_mode: Option<String>,
}

pub(crate) async fn handler_projects(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let conn = db_conn(&state.app)?;
    let mut stmt = conn
        .prepare("SELECT id, name, root_path FROM agent_projects ORDER BY name")
        .map_err(internal_error)?;
    let projects = stmt
        .query_map([], |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "root_path": row.get::<_, String>(2)?,
            }))
        })
        .map_err(internal_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(internal_error)?;
    Ok(Json(json!({ "projects": projects })))
}

pub(crate) async fn handler_session(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    Json(req): Json<SessionReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let model_id = req
        .model_id
        .clone()
        .or_else(|| Some(config.model_id.clone()))
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| bad_request("未配置默认模型：请在微信绑定页面填写模型后再派任务"))?;
    let title = req
        .title
        .clone()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "微信会话".to_string());
    let conn = db_conn(&state.app)?;
    let session = match req.project_id.clone() {
        Some(project_id) if !project_id.trim().is_empty() => {
            crate::commands::projects::create_project_session_row(
                &conn,
                project_id.trim(),
                &title,
                &model_id,
            )
        }
        _ => {
            // 未绑定任务：普通会话（无项目归属），工作区由运行时覆盖到 scratch 目录
            crate::commands::projects::create_plain_session_row(&conn, &title, &model_id)
        }
    }
    .map_err(|e| bad_request(&e))?;
    Ok(Json(json!({
        "session_id": session.id,
        "title": session.title,
        "model_id": session.model_id,
    })))
}

pub(crate) async fn handler_delete_session(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let db = state.app.state::<DbState>();
    let id = session_id.trim();
    if let Ok(conn) = db.0.lock() {
        let app_dir = state.app.path().app_data_dir().ok();
        let _ = crate::db::delete_session_completely(&conn, id, app_dir.as_deref());
    }
    crate::agent::retrieval::clear_session_search_index(id);
    notify_session_event(id, "deleted");
    let _ = state.app.emit("channel-run-done", json!({ "session_id": id, "status": "deleted", "at": now_ts() }));
    Ok(Json(json!({ "ok": true, "session_id": id })))
}

/// 确保 scratch 项目存在并返回其 id（未绑定任务的兜底工作区）
pub(crate) fn ensure_scratch_project(
    conn: &rusqlite::Connection,
    scratch_dir: &PathBuf,
) -> Result<String, String> {
    let root = scratch_dir.to_string_lossy().to_string();
    std::fs::create_dir_all(scratch_dir).map_err(|e| format!("创建临时工作区失败: {e}"))?;
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM agent_projects WHERE root_path=?1 LIMIT 1",
            rusqlite::params![root],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let project = crate::commands::projects::create_project_row(
        conn,
        SCRATCH_PROJECT_NAME,
        &root,
    )?;
    Ok(project.id)
}

use rusqlite::OptionalExtension;

pub(crate) async fn handler_task(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    Json(req): Json<TaskReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let goal = req.goal.trim().to_string();
    if goal.is_empty() {
        return Err(bad_request("任务内容不能为空"));
    }
    let session_id = req.session_id.trim().to_string();

    let conn = db_conn(&state.app)?;
    let session_exists: Option<String> = conn
        .query_row(
            "SELECT id FROM chat_sessions WHERE id=?1",
            rusqlite::params![session_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(internal_error)?;
    if session_exists.is_none() {
        // 自愈：若会话在桌面端被清理或重启导致丢失，自动为通道重新创建该 session_id 对应的临时会话，避免死锁报错
        let default_model = if !config.model_id.trim().is_empty() {
            config.model_id.clone()
        } else {
            conn.query_row("SELECT id FROM models ORDER BY id LIMIT 1", [], |r| r.get::<_, String>(0)).unwrap_or_default()
        };
        let now = chrono::Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT OR IGNORE INTO chat_sessions (id, title, model_id, created_at, updated_at, message_count) VALUES (?1, ?2, ?3, ?4, ?5, 0)",
            rusqlite::params![session_id, "微信 · 自动自愈会话", default_model, now, now],
        );
    }
    let has_project = crate::commands::projects::project_for_session(&conn, &session_id)
        .map_err(internal_error)?
        .is_some();

    // 路由配置：默认取通道配置，允许请求覆盖三槽
    let mut routing = ModelRoutingConfig {
        pro: config.pro.clone(),
        flash: config.flash.clone(),
        vision: config.vision.clone(),
        ..Default::default()
    };
    if let Some(overrides) = &req.model_routing {
        if let Some(pro) = overrides.pro.clone() {
            routing.pro = pro;
        }
        if let Some(flash) = overrides.flash.clone() {
            routing.flash = flash;
        }
        if let Some(vision) = overrides.vision.clone() {
            routing.vision = vision;
        }
        if let Some(mode) = overrides.context_budget_mode.clone() {
            routing.context_budget_mode = mode;
        }
        if let Some(mode) = overrides.routing_mode.clone() {
            routing.routing_mode = mode;
        }
        if let Some(role_models) = overrides.role_models.clone() {
            routing.role_models = role_models
                .into_iter()
                .filter(|(_, model)| !model.trim().is_empty())
                .collect();
        }
        if let Some(v) = overrides.auto_cross_family_reviewer {
            routing.auto_cross_family_reviewer = v;
        }
    }
    if routing.pro.is_empty() {
        routing.pro = if !config.model_id.trim().is_empty() {
            config.model_id.clone()
        } else {
            model_id_of_session(&conn, &session_id)?
        };
    }
    if routing.flash.is_empty() {
        routing.flash = routing.pro.clone();
    }
    if routing.vision.is_empty() {
        routing.vision = routing.pro.clone();
    }
    drop(conn);

    // 项目会话沿用项目工作区；未绑定会话落到 scratch 目录
    let workspace_override = if has_project {
        None
    } else {
        Some(effective_scratch_dir(&config))
    };

    let effective_retries = config
        .llm_retries
        .unwrap_or(crate::agent::main_loop::DEFAULT_TRANSIENT_LLM_REQUEST_RETRIES)
        .clamp(1, 100);
    let run_id = register_and_spawn_run(
        state.app.clone(),
        session_id.clone(),
        goal,
        routing,
        workspace_override,
        config.llm_retries,
        config.action_milestone_steps,
        config.phase_pushes,
        req.plan_mode,
    )
    .map_err(|error| match error {
        SubmitError::InFlight => (
            StatusCode::CONFLICT,
            Json(json!({"error": "该会话已有在途任务", "session_id": session_id})),
        ),
        SubmitError::Other(message) => bad_request(&message),
    })?;

    Ok(Json(json!({
        "run_id": run_id,
        "session_id": session_id,
        "workspace": if has_project { "project" } else { "scratch" },
        "llm_retries": effective_retries,
    })))
}
