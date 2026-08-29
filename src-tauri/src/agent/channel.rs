// 微信通道层 (WeChat Channel)：
// - 本机 axum HTTP API（默认 127.0.0.1:8790），供 Node 微信桥调用：建会话、派任务、
//   轮询进度、应答 ask_human、抢方向盘(ReplaceGoal)与停止(Cancel)。
// - 一组 Tauri 命令（wechat_bridge_* / wechat_channel_*）供「微信绑定」页面代理访问
//   Node 桥（扫码登录二维码、桥状态、配置读写）。
// - 任务执行完全复用 run_agent_main_loop_in_workspace：项目会话走项目工作区，
//   未绑定会话兜底到 scratch_dir，主循环零改动。
// 鉴权：所有请求必须携带 X-Channel-Token，与桥共用同一份 channel-config.json。

use crate::agent::main_loop::run_agent_main_loop_in_workspace;
use crate::agent::router::ModelRoutingConfig;
use crate::db::DbState;
use axum::extract::{Path as AxumPath, State as AxumState};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{AppHandle, Listener, Manager};
use tokio::sync::Mutex as AsyncMutex;

const DEFAULT_API_PORT: u16 = 8790;
const DEFAULT_BRIDGE_PORT: u16 = 8791;
const SCRATCH_PROJECT_NAME: &str = "wechat-scratch";
const DEFAULT_HEARTBEAT_MINUTES: u64 = 30;
const MAX_RUN_RECORDS: usize = 200;
const MAX_GOAL_PREVIEW_CHARS: usize = 120;
const MAX_RESULT_CHARS: usize = 24_000;

// ---------------------------------------------------------------- 配置

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    /// 桥 ↔ 本机 API 的共享令牌
    pub token: String,
    /// 本机 axum API 端口
    #[serde(default = "default_api_port")]
    pub port: u16,
    /// Node 桥 HTTP 端口
    #[serde(default = "default_bridge_port")]
    pub bridge_port: u16,
    /// 未绑定任务的工作区落点
    #[serde(default)]
    pub scratch_dir: String,
    /// 创建会话时登记的模型（也是默认路由基线）
    #[serde(default)]
    pub model_id: String,
    /// 路由三槽：pro / flash / vision（模型 UUID，空则沿用会话模型）
    #[serde(default)]
    pub pro: String,
    #[serde(default)]
    pub flash: String,
    #[serde(default)]
    pub vision: String,
    /// 心跳汇报间隔（分钟）
    #[serde(default = "default_heartbeat_minutes")]
    pub heartbeat_minutes: u64,
    /// 首次联系推送的帮助文案
    #[serde(default = "default_help_text")]
    pub help_text: String,
    /// 应用启动时自动拉起微信桥
    #[serde(default = "default_true")]
    pub auto_start_bridge: bool,
    /// 微信发送者白名单（wxid 列表）；空表 = 自动锁定第一条消息的发送者为主人
    #[serde(default)]
    pub allowlist: Vec<String>,
    /// 微信任务的瞬时 LLM 请求重试次数；None = 跟随系统默认（3，上限 100）。
    /// API 不稳定时调高：评测路径基线为 20。
    #[serde(default)]
    pub llm_retries: Option<u32>,
}

fn default_api_port() -> u16 {
    DEFAULT_API_PORT
}
fn default_bridge_port() -> u16 {
    DEFAULT_BRIDGE_PORT
}
fn default_heartbeat_minutes() -> u64 {
    DEFAULT_HEARTBEAT_MINUTES
}
fn default_help_text() -> String {
    // 完整指令面板由桥内置（COMMAND_PANEL，随桥能力同步），此处只是可自定义的开场白
    "🤖 SandwichAgent 微信通道已就绪。".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            token: uuid::Uuid::new_v4().to_string(),
            port: default_api_port(),
            bridge_port: default_bridge_port(),
            scratch_dir: String::new(),
            model_id: String::new(),
            pro: String::new(),
            flash: String::new(),
            vision: String::new(),
            heartbeat_minutes: default_heartbeat_minutes(),
            help_text: default_help_text(),
            auto_start_bridge: true,
            allowlist: Vec::new(),
            llm_retries: None,
        }
    }
}

pub fn channel_config_path() -> PathBuf {
    let home = dirs_next::data_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join("free-api-chat").join("channel-config.json")
}

pub fn load_channel_config() -> Result<ChannelConfig, String> {
    let path = channel_config_path();
    if !path.exists() {
        let config = ChannelConfig::default();
        save_channel_config(&config)?;
        return Ok(config);
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut config: ChannelConfig =
        serde_json::from_str(&raw).map_err(|e| format!("channel-config.json 解析失败: {e}"))?;
    if config.token.trim().is_empty() {
        config.token = uuid::Uuid::new_v4().to_string();
        save_channel_config(&config)?;
    }
    Ok(config)
}

pub fn save_channel_config(config: &ChannelConfig) -> Result<(), String> {
    let path = channel_config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let raw = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(&path, raw).map_err(|e| e.to_string())
}

fn default_scratch_dir() -> PathBuf {
    let home = dirs_next::data_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join("free-api-chat").join("wechat-scratch")
}

fn effective_scratch_dir(config: &ChannelConfig) -> PathBuf {
    let raw = config.scratch_dir.trim();
    if raw.is_empty() {
        return default_scratch_dir();
    }
    let expanded = if let Some(stripped) = raw.strip_prefix("~/") {
        dirs_next::home_dir().unwrap_or_default().join(stripped)
    } else {
        PathBuf::from(raw)
    };
    expanded
        .canonicalize()
        .unwrap_or_else(|_| {
            let _ = std::fs::create_dir_all(&expanded);
            expanded
        })
}

// ---------------------------------------------------------------- 运行账本

#[derive(Debug, Clone, Serialize)]
pub struct RunRecord {
    pub run_id: String,
    pub session_id: String,
    pub goal_preview: String,
    /// running | waiting_human | done | failed | cancelled
    pub status: String,
    pub paused_reason: Option<String>,
    pub last_progress: Option<String>,
    pub event_count: u64,
    pub started_at: String,
    pub updated_at: String,
    pub result: Option<String>,
    pub error: Option<String>,
    /// 本次 run 实际生效的 LLM 瞬时重试次数（提交时快照）
    pub llm_retries: u32,
    /// 最近一次里程碑状态描述（仅环境就绪/失败恢复/外部救援/开始交付时更新，
    /// 桥据此推送阶段汇报）
    pub phase: Option<String>,
    /// 最新状态描述（每次状态机迁移都更新，「状态」查询可见）
    pub state_detail: Option<String>,
}

fn runs_store() -> &'static Mutex<HashMap<String, RunRecord>> {
    static RUNS: OnceLock<Mutex<HashMap<String, RunRecord>>> = OnceLock::new();
    RUNS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_ts() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn truncate_chars(raw: &str, max: usize) -> String {
    raw.chars().take(max).collect()
}

/// 运行账本快照（供调度器轮询终态）
pub(crate) fn runs_store_snapshot() -> Vec<RunRecord> {
    runs_store()
        .lock()
        .expect("runs store poisoned")
        .values()
        .cloned()
        .collect()
}

/// scratch 目录解析的 crate 级入口（调度器复用）
pub(crate) fn effective_scratch_dir_pub(config: &ChannelConfig) -> std::path::PathBuf {
    effective_scratch_dir(config)
}

fn upsert_run(record: RunRecord) {
    let mut runs = runs_store().lock().expect("runs store poisoned");
    if runs.len() >= MAX_RUN_RECORDS && !runs.contains_key(&record.run_id) {
        // 淘汰最旧的已完成记录
        if let Some(oldest) = runs
            .values()
            .filter(|r| !matches!(r.status.as_str(), "running" | "waiting_human"))
            .min_by_key(|r| r.started_at.clone())
            .map(|r| r.run_id.clone())
        {
            runs.remove(&oldest);
        }
    }
    let run_key = record.run_id.clone();
    runs.insert(run_key, record);
}

fn patch_run(run_id: &str, patch: impl FnOnce(&mut RunRecord)) {
    let mut runs = runs_store().lock().expect("runs store poisoned");
    if let Some(record) = runs.get_mut(run_id) {
        record.updated_at = now_ts();
        patch(record);
    }
}

// ---------------------------------------------------------------- Axum 状态与鉴权

pub struct ChannelState {
    pub app: AppHandle,
    pub config: Arc<std::sync::RwLock<ChannelConfig>>,
}

fn authorized(headers: &HeaderMap, config: &ChannelConfig) -> bool {
    headers
        .get("x-channel-token")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == config.token)
        .unwrap_or(false)
}

fn unauthorized() -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "缺少或无效的 X-Channel-Token"})),
    )
}

// ---------------------------------------------------------------- 请求/响应结构

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
    vision: Option<String>,
}

#[derive(Deserialize)]
struct TaskReq {
    session_id: String,
    goal: String,
    model_routing: Option<PartialRouting>,
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
    port: Option<u16>,
    bridge_port: Option<u16>,
    scratch_dir: Option<String>,
    model_id: Option<String>,
    pro: Option<String>,
    flash: Option<String>,
    vision: Option<String>,
    heartbeat_minutes: Option<u64>,
    help_text: Option<String>,
    auto_start_bridge: Option<bool>,
    allowlist: Option<Vec<String>>,
    llm_retries: Option<u32>,
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
        return Err(bad_request("会话不存在，请先创建会话"));
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
    }
    if routing.pro.is_empty() {
        routing.pro = model_id_of_session(&conn, &session_id)?;
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
pub(crate) enum SubmitError {
    InFlight,
    Other(String),
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
        last_progress: None,
        event_count: 0,
        started_at: started_at.clone(),
        updated_at: started_at,
        result: None,
        error: None,
        llm_retries: llm_retries
            .unwrap_or(crate::agent::main_loop::DEFAULT_TRANSIENT_LLM_REQUEST_RETRIES)
            .clamp(1, 100),
        phase: None,
        state_detail: None,
    });

    // 进度订阅：主循环的 agent-progress-{session} 事件原样转写进运行账本，
    // agent_paused（ask_human 阻塞）单独标记，供桥推送「卡住提问」。
    let listen_session = session_id.clone();
    let listen_run = run_id.clone();
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
            patch_run(&listen_run, |record| {
                record.event_count += 1;
                if let Some(text) = description {
                    record.last_progress = Some(text);
                }
                if let Some(reason) = paused_reason {
                    record.status = "waiting_human".to_string();
                    record.paused_reason = Some(reason);
                } else if record.status == "waiting_human" {
                    // 收到后续进度说明已恢复运行
                    record.status = "running".to_string();
                    record.paused_reason = None;
                }
            });
        },
    );

    // 状态机订阅：state_detail 每次迁移都更新；phase 仅里程碑状态更新（供桥推送阶段汇报）
    let state_session = session_id.clone();
    let state_run = run_id.clone();
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
        },
    );

    let spawn_app = app.clone();
    let spawn_run = run_id.clone();
    let spawn_session = session_id.clone();
    let spawn_goal = goal;
    tauri::async_runtime::spawn(async move {
        let db = spawn_app.state::<DbState>();
        let registry =
            spawn_app.state::<Arc<AsyncMutex<crate::agent::mcp::PluginRegistry>>>();
        let outcome = run_agent_main_loop_in_workspace(
            spawn_app.clone(),
            db,
            registry,
            routing,
            spawn_goal,
            false,
            Some(spawn_session.clone()),
            None,
            llm_retries,
            None,
            workspace_override,
        )
        .await;
        let _ = app.unlisten(event_id);
        let _ = app.unlisten(state_event_id);
        match outcome {
            Ok(()) => {
                let result = last_assistant_message(&spawn_app, &spawn_session);
                let was_cancelled = runs_store()
                    .lock()
                    .expect("runs store poisoned")
                    .get(&spawn_run)
                    .map(|r| r.status == "cancelled")
                    .unwrap_or(false);
                patch_run(&spawn_run, |record| {
                    if !was_cancelled {
                        record.status = "done".to_string();
                    }
                    record.paused_reason = None;
                    record.result = result;
                });
            }
            Err(error) => {
                patch_run(&spawn_run, |record| {
                    if record.status != "cancelled" {
                        record.status = "failed".to_string();
                        record.error = Some(truncate_chars(&error, 2_000));
                    }
                    record.paused_reason = None;
                });
            }
        }
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
        record.status = "cancelled".to_string();
        record.paused_reason = None;
    });
    Ok(Json(json!({"ok": true, "session_id": session_id})))
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
    save_channel_config(&config).map_err(internal_error)?;
    *state.config.write().expect("config poisoned") = config.clone();
    Ok(Json(config_value(&config)))
}

fn config_value(config: &ChannelConfig) -> Value {
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

fn channel_router(state: Arc<ChannelState>) -> Router {
    Router::new()
        .route("/channel/projects", get(handler_projects))
        .route("/channel/session", post(handler_session))
        .route("/channel/task", post(handler_task))
        .route("/channel/runs", get(handler_run_list))
        .route("/channel/run/{run_id}", get(handler_run_status))
        .route("/channel/run/{run_id}/answer", post(handler_run_answer))
        .route("/channel/run/{run_id}/goal", post(handler_run_goal))
        .route("/channel/run/{run_id}/cancel", post(handler_run_cancel))
        .route("/channel/config", get(handler_config_get).put(handler_config_put))
        .route("/channel/models", get(handler_models))
        .route(
            "/channel/session/{session_id}/attachment",
            post(handler_session_attachment),
        )
        .route(
            "/channel/session/{session_id}/artifacts",
            get(handler_session_artifacts),
        )
        .with_state(state)
}

// ---------------------------------------------------------------- 全局共享配置
// 页面保存配置走 Tauri 命令，而 axum 处理器读的是 ChannelState 里的快照；
// 两边必须共享同一份 Arc<RwLock<ChannelConfig>>，否则保存后运行中的服务仍在用
// 应用启动时加载的旧值（表现为「未配置默认模型」）。

static APP_HANDLE: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();

/// 桥 watchdog 抑制标志：页面「停止桥」置位（用户主动关闭，不自动复活），
/// 手动「启动桥」复位。自动启动/看门狗均尊重该标志。
static BRIDGE_WATCHDOG_SUPPRESSED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// setup 时暂存 AppHandle，供无 State/AppHandle 上下文的原生工具（schedule_manager.run_now 等）取用
pub(crate) fn stash_app_handle(app: tauri::AppHandle) {
    let _ = APP_HANDLE.set(app);
}

pub(crate) fn app_handle() -> Option<tauri::AppHandle> {
    APP_HANDLE.get().cloned()
}

static BRIDGE_STATUS_CACHE: std::sync::OnceLock<
    std::sync::Mutex<Option<(std::time::Instant, Value)>>,
> = std::sync::OnceLock::new();

/// bridge_call 成功时缓存状态快照，供 wechat_manager.status 离线查询
fn cache_bridge_status(value: &Value) {
    let cache = BRIDGE_STATUS_CACHE.get_or_init(|| std::sync::Mutex::new(None));
    if let Ok(mut slot) = cache.lock() {
        *slot = Some((std::time::Instant::now(), value.clone()));
    }
}

/// 最近 5 分钟内的桥状态快照（过期返回 None，避免误报在线）
pub(crate) fn cached_bridge_status() -> Option<Value> {
    let cache = BRIDGE_STATUS_CACHE.get_or_init(|| std::sync::Mutex::new(None));
    let slot = cache.lock().ok()?;
    let (at, value) = slot.as_ref()?;
    if at.elapsed() < std::time::Duration::from_secs(300) {
        Some(value.clone())
    } else {
        None
    }
}

pub(crate) fn channel_config_shared() -> &'static Arc<std::sync::RwLock<ChannelConfig>> {
    static CONFIG: OnceLock<Arc<std::sync::RwLock<ChannelConfig>>> = OnceLock::new();
    CONFIG.get_or_init(|| {
        let config = match load_channel_config() {
            Ok(config) => config,
            Err(error) => {
                eprintln!("[channel] 配置加载失败，使用默认配置: {error}");
                ChannelConfig::default()
            }
        };
        Arc::new(std::sync::RwLock::new(config))
    })
}

pub(crate) fn sync_shared_config(config: &ChannelConfig) {
    *channel_config_shared()
        .write()
        .expect("config poisoned") = config.clone();
}

/// 在 Tauri setup 时启动本机通道 API；失败只打日志，不阻断应用启动。
pub fn spawn_channel_server(app: AppHandle) {
    stash_app_handle(app.clone());
    let config = channel_config_shared().read().expect("config poisoned").clone();
    let port = config.port;
    let state = Arc::new(ChannelState {
        app,
        config: channel_config_shared().clone(),
    });
    tauri::async_runtime::spawn(async move {
        let addr = format!("127.0.0.1:{port}");
        match tokio::net::TcpListener::bind(&addr).await {
            Ok(listener) => {
                println!("[channel] 微信通道 API 已监听 http://{addr}");
                if let Err(error) = axum::serve(listener, channel_router(state)).await {
                    eprintln!("[channel] API 服务退出: {error}");
                }
            }
            Err(error) => eprintln!(
                "[channel] 端口 127.0.0.1:{port} 绑定失败（可能已有应用实例在运行）: {error}"
            ),
        }
    });

    // 自动拉起微信桥（auto_start_bridge=false 可关闭；失败不影响应用）
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let auto_start = channel_config_shared()
            .read()
            .expect("config poisoned")
            .auto_start_bridge;
        if !auto_start {
            println!("[channel] 微信桥自动启动已关闭，可在微信绑定页手动启动");
            return;
        }
        match ensure_bridge_running().await {
            Ok(value) => println!("[channel] 微信桥已自动启动: {value}"),
            Err(error) => eprintln!("[channel] 微信桥自动启动失败（可在微信绑定页手动启动）: {error}"),
        }
    });

    // 看门狗：低频探活，桥意外退出时自动复活（用户手动停止则保持安静）
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            if BRIDGE_WATCHDOG_SUPPRESSED.load(std::sync::atomic::Ordering::SeqCst) {
                continue;
            }
            let auto_start = channel_config_shared()
                .read()
                .map(|config| config.auto_start_bridge)
                .unwrap_or(false);
            if !auto_start {
                continue;
            }
            if let Err(error) = ensure_bridge_running().await {
                eprintln!("[channel] 看门狗拉起微信桥失败: {error}");
            }
        }
    });
}

// ---------------------------------------------------------------- 页面用 Tauri 命令（桥代理 + 配置）

static BRIDGE_CHILD: OnceLock<Mutex<Option<tokio::process::Child>>> = OnceLock::new();

fn bridge_child_slot() -> &'static Mutex<Option<tokio::process::Child>> {
    BRIDGE_CHILD.get_or_init(|| Mutex::new(None))
}

fn bridge_base(config: &ChannelConfig) -> String {
    format!("http://127.0.0.1:{}", config.bridge_port)
}

fn bridge_http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(6))
        .build()
        .expect("reqwest client")
}

fn resolve_bridge_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("FREE_API_WECHAT_BRIDGE_DIR") {
        let dir = PathBuf::from(dir);
        if dir.join("index.js").exists() {
            return Some(dir);
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("wechat-bridge"));
        if let Some(parent) = cwd.parent() {
            candidates.push(parent.join("wechat-bridge"));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut ancestor = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..4 {
            if let Some(dir) = ancestor {
                candidates.push(dir.join("wechat-bridge"));
                ancestor = dir.parent().map(|p| p.to_path_buf());
            }
        }
    }
    candidates
        .into_iter()
        .find(|dir| dir.join("index.js").exists())
}

async fn bridge_call(
    config: &ChannelConfig,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let url = format!("{}{}", bridge_base(config), path);
    let client = bridge_http();
    let mut request = match method {
        "POST" => client.post(&url),
        _ => client.get(&url),
    };
    if let Some(value) = &body {
        request = request.json(value);
    }
    let response = request
        .header("x-channel-token", &config.token)
        .send()
        .await
        .map_err(|_| format!("微信桥未响应（{url}）。请先在页面启动桥。"))?;
    let value: Value = response.json().await.map_err(|e| e.to_string())?;
    if path.starts_with("/bridge/status") {
        cache_bridge_status(&value);
    }
    Ok(value)
}

/// 主动推送文本到主人微信（经桥转发；桥未运行时只记日志不报错）
pub(crate) async fn notify_wechat(config: &ChannelConfig, text: &str) {
    let payload = json!({ "text": text });
    if let Err(error) = bridge_call(config, "POST", "/bridge/notify", Some(payload)).await {
        eprintln!("[channel] 微信推送失败: {error}");
    }
}

/// 确保微信桥在运行：已就绪直接返回；否则拉起 node 子进程并等待就绪。
/// 供页面「启动桥」按钮与应用启动时的自动拉起共用；重复调用不会起第二个进程。
async fn ensure_bridge_running() -> Result<Value, String> {
    let config = load_channel_config()?;
    if let Ok(status) = bridge_call(&config, "GET", "/bridge/status", None).await {
        return Ok(json!({ "already_running": true, "status": status }));
    }
    // 已持有存活的子进程（仍在启动中）时不再重复 spawn，避免双实例抢端口。
    // guard 的检查与清理全部收在同步块内，跨 await 前必然释放（否则 future 非 Send）。
    let child_still_starting = {
        let mut slot = bridge_child_slot().lock().expect("child slot poisoned");
        match slot.as_mut().map(|process| process.try_wait()) {
            Some(Ok(None)) => true,  // 子进程存活，仍在启动中
            Some(Ok(Some(_))) => {
                slot.take(); // 子进程已退出，清理句柄后重新拉起
                false
            }
            _ => false,
        }
    };
    if child_still_starting {
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if let Ok(status) = bridge_call(&config, "GET", "/bridge/status", None).await {
                return Ok(json!({ "already_running": false, "status": status }));
            }
        }
        return Err("桥进程启动中但未在 10 秒内就绪，请查看桥日志".to_string());
    }
    let dir = resolve_bridge_dir().ok_or(
        "未找到 wechat-bridge 目录（可设 FREE_API_WECHAT_BRIDGE_DIR 指向该目录）",
    )?;
    let index_js = dir.join("index.js");
    // 桥日志落盘：之前 piped 后被丢弃，排障困难；现在追加写入数据目录
    let log_path = channel_config_path()
        .parent()
        .map(|p| p.join("wechat-bridge.log"))
        .ok_or_else(|| "无法定位桥日志目录".to_string())?;
    let stdout_log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("打开桥日志失败: {e}"))?;
    let stderr_log = stdout_log
        .try_clone()
        .map_err(|e| format!("复用桥日志句柄失败: {e}"))?;
    let child = tokio::process::Command::new("node")
        .arg(&index_js)
        .env("CHANNEL_CONFIG_PATH", channel_config_path())
        .stdout(std::process::Stdio::from(stdout_log))
        .stderr(std::process::Stdio::from(stderr_log))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("启动 node 桥失败: {e}"))?;
    *bridge_child_slot().lock().expect("child slot poisoned") = Some(child);

    // 等待桥就绪（最多 10s）
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if let Ok(status) = bridge_call(&config, "GET", "/bridge/status", None).await {
            return Ok(json!({ "already_running": false, "status": status }));
        }
    }
    Err("桥进程已启动但未在 10 秒内就绪，请查看桥日志".to_string())
}

#[tauri::command]
pub async fn wechat_bridge_start() -> Result<Value, String> {
    // 手动启动解除看门狗抑制（用户重新启用）
    BRIDGE_WATCHDOG_SUPPRESSED.store(false, std::sync::atomic::Ordering::SeqCst);
    ensure_bridge_running().await
}

#[tauri::command]
pub async fn wechat_bridge_status() -> Result<Value, String> {
    let config = load_channel_config()?;
    let bridge = bridge_call(&config, "GET", "/bridge/status", None)
        .await
        .unwrap_or_else(|e| json!({ "running": false, "error": e }));
    Ok(json!({
        "bridge": bridge,
        "config": config_value(&config),
    }))
}

#[tauri::command]
pub async fn wechat_bridge_login_start() -> Result<Value, String> {
    let config = load_channel_config()?;
    bridge_call(&config, "POST", "/bridge/login/start", None).await
}

#[tauri::command]
pub async fn wechat_bridge_qr() -> Result<Value, String> {
    let config = load_channel_config()?;
    bridge_call(&config, "GET", "/bridge/qr", None).await
}

#[tauri::command]
pub async fn wechat_bridge_relogin() -> Result<Value, String> {
    // 一键重新扫码：桥内部停监听→清凭据→发起新登录，进程不退出
    let config = load_channel_config()?;
    bridge_call(&config, "POST", "/bridge/relogin", None).await
}

#[tauri::command]
pub async fn wechat_bridge_stop() -> Result<Value, String> {
    let config = load_channel_config()?;
    BRIDGE_WATCHDOG_SUPPRESSED.store(true, std::sync::atomic::Ordering::SeqCst);
    let result = bridge_call(&config, "POST", "/bridge/stop", None).await;
    let mut child = bridge_child_slot()
        .lock()
        .expect("child slot poisoned")
        .take();
    if let Some(process) = child.as_mut() {
        let _ = process.kill().await;
    }
    drop(child);
    result
}

#[tauri::command]
pub async fn wechat_channel_config_get() -> Result<Value, String> {
    // 以磁盘为准读取（兼容手改文件），并同步进运行中服务的共享配置
    let config = load_channel_config()?;
    sync_shared_config(&config);
    Ok(config_value(&config))
}

#[tauri::command]
pub async fn wechat_channel_config_set(config: ConfigPutReq) -> Result<Value, String> {
    let mut current = load_channel_config()?;
    if let Some(v) = config.port {
        current.port = v;
    }
    if let Some(v) = config.bridge_port {
        current.bridge_port = v;
    }
    if let Some(v) = config.scratch_dir {
        current.scratch_dir = v.trim().to_string();
    }
    if let Some(v) = config.model_id {
        current.model_id = v.trim().to_string();
    }
    if let Some(v) = config.pro {
        current.pro = v.trim().to_string();
    }
    if let Some(v) = config.flash {
        current.flash = v.trim().to_string();
    }
    if let Some(v) = config.vision {
        current.vision = v.trim().to_string();
    }
    if let Some(v) = config.heartbeat_minutes {
        current.heartbeat_minutes = v.clamp(1, 24 * 60);
    }
    if let Some(v) = config.help_text {
        current.help_text = v;
    }
    if let Some(v) = config.auto_start_bridge {
        current.auto_start_bridge = v;
    }
    if let Some(v) = config.allowlist {
        current.allowlist = v.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    }
    if let Some(v) = config.llm_retries {
        current.llm_retries = Some(v.clamp(1, 100));
    }
    save_channel_config(&current)?;
    sync_shared_config(&current);
    Ok(config_value(&current))
}

#[tauri::command]
pub async fn wechat_channel_runs() -> Result<Value, String> {
    let mut runs: Vec<RunRecord> = runs_store()
        .lock()
        .expect("runs store poisoned")
        .values()
        .cloned()
        .collect();
    runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    runs.truncate(20);
    serde_json::to_value(runs).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn test_config() -> ChannelConfig {
        ChannelConfig {
            token: "test-token".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn authorized_accepts_only_matching_token() {
        let config = test_config();
        let mut headers = HeaderMap::new();
        assert!(!authorized(&headers, &config));
        headers.insert("x-channel-token", HeaderValue::from_static("wrong"));
        assert!(!authorized(&headers, &config));
        headers.insert("x-channel-token", HeaderValue::from_static("test-token"));
        assert!(authorized(&headers, &config));
    }

    #[test]
    fn run_store_upsert_patch_and_eviction() {
        let key = format!("test-run-{}", uuid::Uuid::new_v4());
        let record = RunRecord {
            run_id: key.clone(),
            session_id: "session-x".to_string(),
            goal_preview: "goal".to_string(),
            status: "running".to_string(),
            paused_reason: None,
            last_progress: None,
            event_count: 0,
            started_at: "2026-01-01T00:00:00+00:00".to_string(),
            updated_at: "2026-01-01T00:00:00+00:00".to_string(),
            result: None,
            error: None,
            llm_retries: 3,
            phase: None,
            state_detail: None,
        };
        upsert_run(record);
        patch_run(&key, |r| {
            r.event_count = 5;
            r.last_progress = Some("进行中".to_string());
        });
        {
            let runs = runs_store().lock().unwrap();
            let stored = runs.get(&key).expect("record should exist");
            assert_eq!(stored.event_count, 5);
            assert_eq!(stored.last_progress.as_deref(), Some("进行中"));
        }
        // 淘汰路径：填满后最旧已完成记录应被挤出
        for i in 0..(MAX_RUN_RECORDS + 10) {
            upsert_run(RunRecord {
                run_id: format!("evict-{i}-{}", std::process::id()),
                session_id: "session-evict".to_string(),
                goal_preview: "old".to_string(),
                status: "done".to_string(),
                paused_reason: None,
                last_progress: None,
                event_count: 0,
                started_at: "2025-01-01T00:00:00+00:00".to_string(),
                updated_at: "2025-01-01T00:00:00+00:00".to_string(),
                result: None,
                error: None,
                llm_retries: 3,
                phase: None,
                state_detail: None,
            });
        }
        let runs = runs_store().lock().unwrap();
        assert!(runs.len() <= MAX_RUN_RECORDS);
        drop(runs);
        runs_store().lock().unwrap().remove(&key);
    }

    #[test]
    fn scratch_dir_defaults_under_data_dir() {
        let default_dir = effective_scratch_dir_pub(&ChannelConfig::default());
        assert!(default_dir.ends_with("wechat-scratch"));
    }

    #[test]
    fn config_serde_roundtrip_keeps_optional_fields() {
        let raw = serde_json::to_string(&ChannelConfig::default()).unwrap();
        let parsed: ChannelConfig = serde_json::from_str(&raw).unwrap();
        assert!(parsed.llm_retries.is_none());
        assert!(parsed.allowlist.is_empty());
        assert!(parsed.auto_start_bridge);
        // 缺失字段/未知字段都应容忍
        let parsed2: ChannelConfig =
            serde_json::from_str(r#"{"token":"t","unknown_field":1}"#).unwrap();
        assert_eq!(parsed2.token, "t");
        assert_eq!(parsed2.port, DEFAULT_API_PORT);
    }
}
