//! axum HTTP 层：会话/任务/事件流/追踪/工件等 handler 与路由表。
//! 从 channel.rs 原样抽出，行为不变。

use super::*;

#[derive(Deserialize)]
struct SessionReq {
    project_id: Option<String>,
    title: Option<String>,
    model_id: Option<String>,
}

#[derive(Deserialize)]
struct PartialRouting {
    pro: Option<String>,
    flash: Option<String>,
    pub vision: Option<String>,
    /// 上下文压缩策略：deep_coding（长程）| cost_saving（省流）| ultra_fast（极速）
    context_budget_mode: Option<String>,
    /// 路由模式：rules | scored
    routing_mode: Option<String>,
    /// 角色级覆盖：系统角色名（planner/executor/...）→ 档位或模型 UUID
    role_models: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Deserialize)]
struct TaskReq {
    session_id: String,
    goal: String,
    model_routing: Option<PartialRouting>,
    #[serde(default)]
    plan_mode: Option<String>,
}

#[derive(Deserialize)]
struct AnswerReq {
    text: String,
}

#[derive(Deserialize)]
struct GoalReq {
    goal: String,
}

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
}

// ---------------------------------------------------------------- 处理器

async fn handler_projects(
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

async fn handler_session(
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

async fn handler_delete_session(
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

async fn handler_task(
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

/// 提交失败的类型：会话在途（调用方映射 409）或其他原因
pub enum SubmitError {
    InFlight,
    Other(String),
}

/// 统一的任务提交入口（HTTP 处理器与定时调度器共用）：
/// 登记 RunRecord → 订阅进度事件 → 后台 spawn 主循环。
/// 同一会话同时只允许一个在途任务（含 waiting_human）。
pub struct SessionRunGuard {
    app: AppHandle,
    session_id: String,
    run_id: String,
    goal: String,
    event_id: tauri::EventId,
    state_event_id: tauri::EventId,
}

impl SessionRunGuard {
    pub(crate) fn new(
        app: &AppHandle,
        session_id: &str,
        run_id: &str,
        goal: &str,
        llm_retries: u32,
    ) -> Self {
        let started_at = now_ts();
        let existing = {
            let runs = runs_store().lock().expect("runs store poisoned");
            runs.get(run_id).cloned()
        };
        let source = existing
            .as_ref()
            .map(|r| r.source.clone())
            .unwrap_or_else(|| "desktop".to_string());
        let report_every_actions = existing
            .as_ref()
            .map(|r| r.report_every_actions)
            .unwrap_or(0);
        let phase_push = existing
            .as_ref()
            .map(|r| r.phase_push)
            .unwrap_or(false);

        upsert_run(RunRecord {
            run_id: run_id.to_string(),
            session_id: session_id.to_string(),
            goal_preview: truncate_chars(goal, MAX_GOAL_PREVIEW_CHARS),
            status: "running".to_string(),
            paused_reason: None,
            paused_form: None,
            last_progress: None,
            event_count: 0,
            started_at: started_at.clone(),
            updated_at: started_at,
            result: None,
            error: None,
            llm_retries: llm_retries.clamp(1, 100),
            report_every_actions,
            phase_push,
            phase: None,
            state_detail: None,
            source,
        });
        let _ = app.emit(
            "channel-run-started",
            json!({ "run_id": run_id, "session_id": session_id, "goal": goal, "at": now_ts() }),
        );
        notify_session_event(session_id, "start");

        let listen_session = session_id.to_string();
        let listen_run = run_id.to_string();
        let event_id = app.listen(
            format!("agent-progress-{listen_session}"),
            move |event: tauri::Event| {
                let Ok(payload) = serde_json::from_str::<Value>(event.payload()) else {
                    return;
                };
                let description = payload
                    .get("description")
                    .and_then(Value::as_str)
                    .or_else(|| payload.get("message").and_then(Value::as_str))
                    .map(|s| truncate_chars(s, 400));
                let paused = payload.get("type").and_then(Value::as_str) == Some("agent_paused");
                let is_real_step = matches!(
                    payload.get("type").and_then(Value::as_str),
                    Some("step_done") | Some("step_error") | Some("complete")
                );
                let paused_reason = if paused {
                    Some(
                        payload
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("Agent 请求人工协助。")
                            .to_string(),
                    )
                } else {
                    None
                };
                let paused_form = if paused {
                    payload.get("form").filter(|v| !v.is_null()).cloned()
                } else {
                    None
                };
                patch_run(&listen_run, |record| {
                    if is_real_step {
                        record.event_count += 1;
                    }
                    if let Some(text) = description {
                        record.last_progress = Some(text);
                    }
                    if let Some(reason) = paused_reason {
                        record.status = "waiting_human".to_string();
                        record.paused_reason = Some(reason);
                        record.paused_form = paused_form;
                    } else if record.status == "waiting_human" {
                        record.status = "running".to_string();
                        record.paused_reason = None;
                        record.paused_form = None;
                    }
                });
                notify_session_event(&listen_session, if paused { "waiting_human" } else { "progress" });
            },
        );

        let state_session = session_id.to_string();
        let state_run = run_id.to_string();
        let state_event_id = app.listen(
            format!("agent-run-state-{state_session}"),
            move |event: tauri::Event| {
                let Ok(payload) = serde_json::from_str::<Value>(event.payload()) else {
                    return;
                };
                let message = payload
                    .get("message")
                    .and_then(Value::as_str)
                    .map(|text| truncate_chars(text, 200));
                let state = payload
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_lowercase();
                let milestone = ["runtime", "recovering", "rescuing", "finalizing"]
                    .iter()
                    .any(|marker| state.contains(marker));
                patch_run(&state_run, |record| {
                    if let Some(text) = &message {
                        record.state_detail = Some(text.clone());
                    }
                    if milestone {
                        record.phase = Some(
                            message.unwrap_or_else(|| format!("进入阶段 {state}")),
                        );
                    }
                });
                notify_session_event(&state_session, "state");
            },
        );

        Self {
            app: app.clone(),
            session_id: session_id.to_string(),
            run_id: run_id.to_string(),
            goal: goal.to_string(),
            event_id,
            state_event_id,
        }
    }

    pub(crate) fn finish(self, outcome: &Result<(), String>) {
        let _ = self.app.unlisten(self.event_id);
        let _ = self.app.unlisten(self.state_event_id);
        match outcome {
            Ok(()) => {
                let result = last_assistant_message(&self.app, &self.session_id);
                let was_cancelled = runs_store()
                    .lock()
                    .expect("runs store poisoned")
                    .get(&self.run_id)
                    .map(|r| r.status == "cancelled")
                    .unwrap_or(false);
                patch_run(&self.run_id, |record| {
                    if !was_cancelled {
                        record.status = "done".to_string();
                    }
                    record.paused_reason = None;
                    record.paused_form = None;
                    record.result = result.clone();
                });
                notify_session_event(&self.session_id, if was_cancelled { "cancelled" } else { "done" });
                let _ = self.app.emit(
                    "channel-run-done",
                    json!({ "run_id": self.run_id, "session_id": self.session_id, "status": "done", "at": now_ts() }),
                );

                // 若开启了微信通道，主动向主人微信推送任务完成结果
                if !was_cancelled {
                    if let Ok(config) = load_channel_config() {
                        if config.auto_start_bridge {
                            let push_text = if let Some(ref text) = result {
                                let preview = if text.chars().count() > 300 {
                                    format!("{}...", text.chars().take(300).collect::<String>())
                                } else {
                                    text.clone()
                                };
                                format!("🏁 任务已完成：\n\n{}", preview)
                            } else {
                                "🏁 任务已完成。".to_string()
                            };
                            tauri::async_runtime::spawn(async move {
                                notify_wechat(&config, &push_text).await;
                            });
                        }
                    }
                }
            }
            Err(error) => {
                let err_msg = truncate_chars(error, 2_000);
                patch_run(&self.run_id, |record| {
                    if record.status != "cancelled" {
                        record.status = "failed".to_string();
                        record.error = Some(err_msg.clone());
                    }
                    record.paused_reason = None;
                    record.paused_form = None;
                });

                // 若开启了微信通道，主动向主人微信推送任务失败告警
                if let Ok(config) = load_channel_config() {
                    if config.auto_start_bridge {
                        let push_text = format!("❌ 任务执行失败：\n{}", truncate_chars(&err_msg, 200));
                        tauri::async_runtime::spawn(async move {
                            notify_wechat(&config, &push_text).await;
                        });
                    }
                }
                let db = self.app.state::<DbState>();
                let existing_steps =
                    crate::agent::run_trace::load_run_steps(db.inner(), &self.run_id)
                        .unwrap_or_default();
                let _ = crate::agent::run_trace::upsert_snapshot(
                    db.inner(),
                    &self.run_id,
                    &self.session_id,
                    &self.goal,
                    "failed",
                    &existing_steps,
                    Some(&json!({ "summary": error })),
                );
                notify_session_event(&self.session_id, "failed");
                let _ = self.app.emit(
                    "channel-run-done",
                    json!({ "run_id": self.run_id, "session_id": self.session_id, "status": "failed", "at": now_ts() }),
                );
            }
        }
    }
}

/// 统一的任务提交入口（HTTP 处理器与定时调度器共用）：
/// 登记 RunRecord → 订阅进度事件 → 后台 spawn 主循环。
/// 同一会话同时只允许一个在途任务（含 waiting_human）。
pub(crate) fn register_and_spawn_run(
    app: AppHandle,
    session_id: String,
    goal: String,
    routing: ModelRoutingConfig,
    workspace_override: Option<std::path::PathBuf>,
    llm_retries: Option<u32>,
    _action_milestone_steps: u32,
    _phase_push: bool,
    plan_mode: Option<String>,
) -> Result<String, SubmitError> {
    {
        let runs = runs_store().lock().expect("runs store poisoned");
        if runs.values().any(|r| {
            r.session_id == session_id && matches!(r.status.as_str(), "running" | "waiting_human")
        }) {
            return Err(SubmitError::InFlight);
        }
    }

    let run_id = uuid::Uuid::new_v4().to_string();
    let started_at = now_ts();
    println!(
        "[channel] run {run_id} 启动 session={session_id} 重试上限={}",
        llm_retries
            .unwrap_or(crate::agent::main_loop::DEFAULT_TRANSIENT_LLM_REQUEST_RETRIES)
            .clamp(1, 100)
    );

    upsert_run(RunRecord {
        run_id: run_id.clone(),
        session_id: session_id.clone(),
        goal_preview: truncate_chars(&goal, MAX_GOAL_PREVIEW_CHARS),
        status: "running".to_string(),
        paused_reason: None,
        paused_form: None,
        last_progress: None,
        event_count: 0,
        started_at: started_at.clone(),
        updated_at: started_at.clone(),
        result: None,
        error: None,
        llm_retries: llm_retries
            .unwrap_or(crate::agent::main_loop::DEFAULT_TRANSIENT_LLM_REQUEST_RETRIES)
            .clamp(1, 100),
        report_every_actions: _action_milestone_steps,
        phase_push: _phase_push,
        phase: None,
        state_detail: None,
        source: "channel".to_string(),
    });

    // 立即持久化该轮任务初始状态（用户消息气泡由 SessionRunGuard::new 在 channel-run-started 前写入）
    let db_state = app.state::<DbState>();
    let _ = crate::agent::run_trace::upsert_snapshot(
        db_state.inner(),
        &run_id,
        &session_id,
        &goal,
        "running",
        &[],
        None,
    );

    let spawn_app = app.clone();
    let spawn_run = run_id.clone();
    let spawn_session = session_id.clone();
    let spawn_goal = goal;
    let spawn_plan_mode = plan_mode
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| "cross_review".to_string()); // 手机/通道端默认启用 cross_review 交叉审查模式
    tauri::async_runtime::spawn(async move {
        let db = spawn_app.state::<DbState>();
        let registry =
            spawn_app.state::<Arc<AsyncMutex<crate::agent::mcp::PluginRegistry>>>();
        let _ = run_agent_main_loop_in_workspace(
            spawn_app.clone(),
            db,
            registry,
            routing,
            spawn_goal.clone(),
            true, // 手机端任务默认 auto_pilot: true，放手让 Agent 自主闭环
            Some(spawn_session.clone()),
            None,
            llm_retries,
            None,
            workspace_override,
            Some(spawn_run.clone()),
            Some(spawn_plan_mode),
        )
        .await;
    });

    Ok(run_id)
}

async fn handler_models(
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
async fn handler_role_cards(
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
struct EventsQuery {
    token: Option<String>,
    /// 不传 = 订阅所有会话事件（首页列表刷新用）
    session_id: Option<String>,
}

#[derive(Deserialize)]
struct ArtifactRawQuery {
    token: Option<String>,
    /// dl=1 时以附件形式下发（浏览器触发下载），否则 inline 预览
    dl: Option<String>,
}

/// GET /channel/events?token=&session_id= ：SSE 实时推送（EventSource 无法带 header，
/// token 走 query；进度/状态/终态即时下发，网页端收到后拉取最新数据渲染）。
async fn handler_events(
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
struct AttachmentReq {
    path: String,
    original_name: Option<String>,
}

/// 微信发来的媒体文件登记为项目受管资源（复制进 .attachments/ 隔离目录，返回句柄）
async fn handler_session_attachment(
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
async fn handler_session_artifacts(
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

fn model_id_of_session(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<String, (StatusCode, Json<Value>)> {
    conn.query_row(
        "SELECT model_id FROM chat_sessions WHERE id=?1",
        rusqlite::params![session_id],
        |row| row.get::<_, String>(0),
    )
    .map_err(|_| bad_request("会话缺少模型，请先在通道配置中填写默认模型"))
}

fn last_assistant_message(app: &AppHandle, session_id: &str) -> Option<String> {
    let conn = db_conn(app).ok()?;
    conn.query_row(
        "SELECT content FROM chat_messages WHERE session_id=?1 AND role='assistant' ORDER BY created_at DESC, id DESC LIMIT 1",
        rusqlite::params![session_id],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .map(|content| truncate_chars(&content, MAX_RESULT_CHARS))
}

async fn handler_run_status(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(run_id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let runs = runs_store().lock().expect("runs store poisoned");
    match runs.get(&run_id) {
        Some(record) => Ok(Json(serde_json::to_value(record).unwrap_or_else(|_| json!({})))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "任务不存在或已被清理"})),
        )),
    }
}

async fn handler_run_list(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let mut runs: Vec<RunRecord> = runs_store()
        .lock()
        .expect("runs store poisoned")
        .values()
        .cloned()
        .collect();
    runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    Ok(Json(json!({ "runs": runs })))
}

async fn handler_run_answer(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(run_id): AxumPath<String>,
    Json(req): Json<AnswerReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let session_id = {
        let runs = runs_store().lock().expect("runs store poisoned");
        runs.get(&run_id)
            .map(|r| r.session_id.clone())
            .ok_or_else(|| not_found("任务不存在"))?
    };
    let text = req.text.trim().to_string();
    if text.is_empty() {
        return Err(bad_request("回答内容不能为空"));
    }
    let answer_state = state.app.state::<DbState>();
    crate::agent::control::send_human_intervention(answer_state, session_id.clone(), text)
        .await
        .map_err(|e| bad_request(&e))?;
    patch_run(&run_id, |record| {
        record.status = "running".to_string();
        record.paused_reason = None;
        record.paused_form = None;
    });
    Ok(Json(json!({"ok": true, "session_id": session_id})))
}

async fn handler_run_goal(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(run_id): AxumPath<String>,
    Json(req): Json<GoalReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let session_id = {
        let runs = runs_store().lock().expect("runs store poisoned");
        runs.get(&run_id)
            .map(|r| r.session_id.clone())
            .ok_or_else(|| not_found("任务不存在"))?
    };
    let goal = req.goal.trim().to_string();
    if goal.is_empty() {
        return Err(bad_request("新目标不能为空"));
    }
    crate::agent::control::replace_agent_goal(session_id.clone(), goal)
        .await
        .map_err(|e| bad_request(&e))?;
    patch_run(&run_id, |record| {
        record.status = "running".to_string();
        record.paused_reason = None;
        record.paused_form = None;
    });
    Ok(Json(json!({"ok": true, "session_id": session_id})))
}

async fn handler_run_cancel(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(run_id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let session_id = {
        let runs = runs_store().lock().expect("runs store poisoned");
        runs.get(&run_id)
            .map(|r| r.session_id.clone())
            .ok_or_else(|| not_found("任务不存在"))?
    };
    crate::agent::control::cancel_agent_run(session_id.clone())
        .await
        .map_err(|e| bad_request(&e))?;
    patch_run(&run_id, |record| {
        // 已到终态（done/failed/cancelled）的任务不再覆盖，避免取消请求把完成态改写
        if matches!(record.status.as_str(), "running" | "waiting_human") {
            record.status = "cancelled".to_string();
        }
        record.paused_reason = None;
        record.paused_form = None;
    });
    Ok(Json(json!({"ok": true, "session_id": session_id})))
}

// ===== 评测报告在线浏览（与评测落盘共用 CWD/agent_evaluations 约定，见 evaluation.rs） =====

async fn handler_eval_list(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let dir = std::env::current_dir()
        .map_err(|e| internal_error(format!("无法获取工作目录: {e}")))?
        .join("agent_evaluations");
    let mut items: Vec<(std::time::SystemTime, Value)> = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(&dir) {
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

async fn handler_eval_detail(
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
    let dir = std::env::current_dir()
        .map_err(|e| internal_error(format!("无法获取工作目录: {e}")))?
        .join("agent_evaluations");
    let md_path = dir.join(format!("{eval_id}.md"));
    let (path, kind) = if md_path.is_file() {
        (md_path, "markdown")
    } else {
        (dir.join(format!("{eval_id}.json")), "json")
    };
    let content = std::fs::read_to_string(&path)
        .map_err(|_| not_found("评测报告不存在或已清理"))?;
    Ok(Json(json!({ "id": eval_id, "kind": kind, "content": content })))
}

async fn handler_config_get(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    Ok(Json(config_value(&config)))
}

async fn handler_config_put(
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
        "config_path": channel_config_path().to_string_lossy(),
    })
}

// ---------------------------------------------------------------- 小工具

fn db_conn<'a>(
    app: &'a AppHandle,
) -> Result<std::sync::MutexGuard<'a, rusqlite::Connection>, (StatusCode, Json<Value>)> {
    let state: tauri::State<'a, DbState> = app.state::<DbState>();
    let db: &'a DbState = state.inner();
    db.0.lock().map_err(internal_error)
}

fn bad_request(message: &str) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message })))
}

fn not_found(message: &str) -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_FOUND, Json(json!({ "error": message })))
}

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": error.to_string() })),
    )
}

// ---------------------------------------------------------------- 网页控制台
// ZCode 风格的浏览器控制台：工作区/临时任务选择 → 会话列表 → 执行流实时视图。
// 模块化管理（HTML 骨架 / CSS 样式 / JS 逻辑独立分拆在 mobile_web/ 目录下，编译期零开销嵌入二进制）
pub(crate) const CHANNEL_WEB_PAGE: &str = concat!(
    r#"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover, maximum-scale=1, user-scalable=no">
<meta name="color-scheme" content="dark">
<title>free_api 远程控制</title>
<style>
"#,
    include_str!("mobile_web/style.css"),
    r#"
</style>
</head>
<body>
"#,
    include_str!("mobile_web/template.html"),
    r#"
<script>
"#,
    include_str!("mobile_web/app.js"),
    r#"
</script>
</body>
</html>"#
);

/// GET / 与 /web：内嵌网页控制台入口（no-cache 保证升级二进制后页面同步更新）
async fn handler_web_home() -> impl axum::response::IntoResponse {
    (
        [(axum::http::header::CACHE_CONTROL, "no-cache")],
        Html(CHANNEL_WEB_PAGE),
    )
}

#[derive(Deserialize)]
struct SessionsQuery {
    project_id: Option<String>,
    /// scratch = 仅临时任务（未绑定项目，或绑定到 scratch 兜底项目的会话）
    scope: Option<String>,
}

/// trace 文本字段在网页详情端点的截断上限（步骤可能带全量工具输出）
pub(crate) const TRACE_FIELD_TRUNCATE: usize = 6_000;

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
async fn handler_sessions(
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
async fn handler_session_traces(
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
async fn handler_trace_detail(
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
    let steps_json = trace["steps_json"].as_str().unwrap_or("[]").to_string();
    let mut steps = serde_json::from_str::<Value>(&steps_json).unwrap_or_else(|_| json!([]));
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

/// 网页端附件上限（与微信桥回推上限一致）
const MAX_UPLOAD_BYTES: usize = 25 * 1024 * 1024;

/// 极简 percent-decode（文件名经 X-File-Name 头传输，JS 侧 encodeURIComponent）
pub(crate) fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = if bytes[i] == b'%' && i + 2 < bytes.len() {
            bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|s| u8::from_str_radix(s, 16).ok())
        } else {
            None
        };
        if let Some(byte) = hex {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// POST /channel/session/{session_id}/attachment-upload
/// 浏览器给不了本地路径，网页端直接收字节流（raw body + X-File-Name），
/// 落临时文件后走既有登记逻辑（复制进项目 .attachments/ 隔离目录）。
/// 附件仅支持项目会话——临时会话没有 .attachments 落点（与微信桥约束一致）。
async fn handler_session_attachment_upload(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    if body.is_empty() {
        return Err(bad_request("附件内容为空"));
    }
    if body.len() > MAX_UPLOAD_BYTES {
        return Err(bad_request("附件超过 25MB 上限"));
    }
    let original_name = headers
        .get("x-file-name")
        .and_then(|v| v.to_str().ok())
        .map(percent_decode)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| bad_request("缺少 X-File-Name 文件名头"))?;
    let session_id = session_id.trim().to_string();
    let bound: bool = {
        let conn = db_conn(&state.app)?;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_project_sessions WHERE session_id=?1)",
            rusqlite::params![session_id],
            |row| row.get(0),
        )
        .map_err(internal_error)?
    };
    if !bound {
        return Err(bad_request("附件仅支持项目对话：请选择项目后再上传"));
    }
    let tmp = std::env::temp_dir().join(format!("free-api-upload-{}", uuid::Uuid::new_v4()));
    tokio::fs::write(&tmp, &body).await.map_err(internal_error)?;
    let registered = {
        let conn = db_conn(&state.app)?;
        crate::commands::projects::register_attachment_for_session(
            &conn,
            &session_id,
            &tmp,
            &original_name,
        )
    };
    let _ = std::fs::remove_file(&tmp);
    let resource_id = registered
        .map_err(|e| bad_request(&e))?
        .ok_or_else(|| bad_request("该会话未绑定项目，无法登记附件"))?;
    Ok(Json(json!({
        "resource_id": resource_id,
        "name": original_name,
        "size_bytes": body.len(),
    })))
}

/// 极简 percent-encode（Content-Disposition 的 UTF-8 文件名用；保留 RFC 3986 非保留字符）
pub(crate) fn percent_encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// GET /channel/artifact/{id}/raw?token=&dl=1 ：产物原始字节（inline 预览 / attachment 下载）。
/// <a> 链接带不了 header，token 允许走 query（与 SSE 一致）。
async fn handler_artifact_raw(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(artifact_id): AxumPath<String>,
    AxumQuery(query): AxumQuery<ArtifactRawQuery>,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    let token_ok = authorized(&headers, &config)
        || query
            .token
            .as_deref()
            .map(|v| v == config.token)
            .unwrap_or(false);
    if !token_ok {
        return Err(unauthorized());
    }
    let db = state.app.state::<DbState>();
    let record = crate::agent::artifact_manager::get_artifact_record(db.inner(), artifact_id.trim())
        .map_err(|e| not_found(&e))?;
    if record.status == "deleted" {
        return Err(not_found("该产物已被删除"));
    }
    let path = std::path::PathBuf::from(&record.canonical_path);
    if !path.exists() {
        return Err(not_found("产物文件在磁盘上丢失"));
    }
    let bytes = tokio::fs::read(&path).await.map_err(internal_error)?;
    if bytes.len() > 50 * 1024 * 1024 {
        return Err(bad_request("文件超过 50MB，请从桌面端工作区直接获取"));
    }
    let mime = record.mime_type.clone().unwrap_or_else(|| "application/octet-stream".into());
    let encoded = percent_encode_component(&record.display_name);
    let disposition = if query.dl.as_deref() == Some("1") {
        format!("attachment; filename*=UTF-8''{encoded}")
    } else {
        format!("inline; filename*=UTF-8''{encoded}")
    };
    let mut response = (
        StatusCode::OK,
        bytes,
    )
        .into_response();
    if let Ok(v) = axum::http::HeaderValue::from_str(&mime) {
        response.headers_mut().insert(axum::http::header::CONTENT_TYPE, v);
    }
    if let Ok(v) = axum::http::HeaderValue::from_str(&disposition) {
        response.headers_mut().insert(axum::http::header::CONTENT_DISPOSITION, v);
    }
    Ok(response)
}

#[derive(Deserialize)]
struct PreviewQuery {
    token: Option<String>,
}

/// GET /channel/artifact/{id}/preview?token= ：解析引擎产出的预览文本
/// （xlsx → 表结构+数据预览，pdf/docx → 全文截断，文本 → 原文；图片由 raw 端点直出）
async fn handler_artifact_preview(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(artifact_id): AxumPath<String>,
    AxumQuery(query): AxumQuery<PreviewQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    let token_ok = authorized(&headers, &config)
        || query
            .token
            .as_deref()
            .map(|v| v == config.token)
            .unwrap_or(false);
    if !token_ok {
        return Err(unauthorized());
    }
    let db = state.app.state::<DbState>();
    let record = crate::agent::artifact_manager::get_artifact_record(db.inner(), artifact_id.trim())
        .map_err(|e| not_found(&e))?;
    let is_image = record
        .mime_type
        .as_deref()
        .map(|m| m.starts_with("image/"))
        .unwrap_or(false);
    if is_image {
        return Ok(Json(json!({ "kind": "image", "name": record.display_name })));
    }
    let path = std::path::PathBuf::from(&record.canonical_path);
    if !path.exists() {
        return Err(not_found("产物文件在磁盘上丢失"));
    }
    match crate::agent::file_parser::parse_file(&path, Some(12_000)) {
        Ok(content) => {
            let text = crate::agent::file_parser::format_for_prompt(&content, &record.display_name);
            Ok(Json(json!({ "kind": "text", "name": record.display_name, "text": text })))
        }
        Err(e) => Ok(Json(json!({
            "kind": "unsupported",
            "name": record.display_name,
            "text": format!("该格式（{}）暂不支持网页内预览，请点击下载后本地查看：{}", record.display_name, e),
        }))),
    }
}

pub(crate) fn channel_router(state: Arc<ChannelState>) -> Router {
    Router::new()
        .route("/", get(handler_web_home))
        .route("/web", get(handler_web_home))
        .route("/favicon.ico", get(|| async { axum::http::StatusCode::NO_CONTENT }))
        .route("/channel/projects", get(handler_projects))
        .route("/channel/sessions", get(handler_sessions))
        .route("/channel/session", post(handler_session))
        .route("/channel/session/{session_id}", delete(handler_delete_session))
        .route("/channel/task", post(handler_task))
        .route(
            "/channel/session/{session_id}/traces",
            get(handler_session_traces),
        )
        .route("/channel/trace/{trace_id}", get(handler_trace_detail))
        .route("/channel/runs", get(handler_run_list))
        .route("/channel/run/{run_id}", get(handler_run_status))
        .route("/channel/evals", get(handler_eval_list))
        .route("/channel/eval/{eval_id}", get(handler_eval_detail))
        .route("/channel/run/{run_id}/answer", post(handler_run_answer))
        .route("/channel/run/{run_id}/goal", post(handler_run_goal))
        .route("/channel/run/{run_id}/cancel", post(handler_run_cancel))
        .route("/channel/config", get(handler_config_get).put(handler_config_put))
        .route("/channel/models", get(handler_models))
        .route("/channel/role-cards", get(handler_role_cards))
        .route("/channel/events", get(handler_events))
        .route(
            "/channel/artifact/{artifact_id}/raw",
            get(handler_artifact_raw),
        )
        .route(
            "/channel/artifact/{artifact_id}/preview",
            get(handler_artifact_preview),
        )
        .route(
            "/channel/session/{session_id}/attachment",
            post(handler_session_attachment),
        )
        .route(
            "/channel/session/{session_id}/attachment-upload",
            post(handler_session_attachment_upload),
        )
        .route(
            "/channel/session/{session_id}/artifacts",
            get(handler_session_artifacts),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            MAX_UPLOAD_BYTES + 64 * 1024,
        ))
        .with_state(state)
}

// ---------------------------------------------------------------- 全局共享配置
// 页面保存配置走 Tauri 命令，而 axum 处理器读的是 ChannelState 里的快照；
// 两边必须共享同一份 Arc<RwLock<ChannelConfig>>，否则保存后运行中的服务仍在用
// 应用启动时加载的旧值（表现为「未配置默认模型」）。

pub(crate) static APP_HANDLE: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();

/// 桥 watchdog 抑制标志：页面「停止桥」置位（用户主动关闭，不自动复活），
/// 手动「启动桥」复位。自动启动/看门狗均尊重该标志。
pub(crate) static BRIDGE_WATCHDOG_SUPPRESSED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

