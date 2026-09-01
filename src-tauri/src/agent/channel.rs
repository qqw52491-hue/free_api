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
use axum::extract::{Path as AxumPath, Query as AxumQuery, State as AxumState};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Html;
use axum::response::sse::{Event as SseEvent, Sse};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{AppHandle, Emitter, Listener, Manager};
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
    /// 动作里程碑汇报步长：每 N 个动作推一条"正在干啥"；0 = 关闭
    #[serde(default = "default_action_milestone")]
    pub action_milestone_steps: u32,
    /// 阶段里程碑（环境就绪/失败恢复/救援/交付）是否推送
    #[serde(default = "default_true")]
    pub phase_pushes: bool,
    /// 网页控制台对外访问基础地址（内网穿透域名，如 https://xxx.example.com）；
    /// 空 = 自动探测局域网 IPv4。用于生成扫码直达的二维码
    #[serde(default)]
    pub console_base_url: Option<String>,
}

fn default_action_milestone() -> u32 {
    15
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
            action_milestone_steps: default_action_milestone(),
            phase_pushes: true,
            console_base_url: None,
        }
    }
}

pub fn channel_config_path() -> PathBuf {
    // 与 Node 桥一致：CHANNEL_CONFIG_PATH 可指向独立配置文件（多实例隔离/测试）
    if let Ok(path) = std::env::var("CHANNEL_CONFIG_PATH") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
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
    pub paused_form: Option<serde_json::Value>,
    pub last_progress: Option<String>,
    pub event_count: u64,
    pub started_at: String,
    pub updated_at: String,
    pub result: Option<String>,
    pub error: Option<String>,
    /// 本次 run 实际生效的 LLM 瞬时重试次数（提交时快照）
    pub llm_retries: u32,
    /// 动作里程碑步长（提交时快照；0 = 该任务不按动作汇报）
    pub report_every_actions: u32,
    /// 本任务是否推送阶段里程碑（提交时快照）
    pub phase_push: bool,
    /// 最近一次里程碑状态描述（仅环境就绪/失败恢复/外部救援/开始交付时更新，
    /// 桥据此推送阶段汇报）
    pub phase: Option<String>,
    /// 最新状态描述（每次状态机迁移都更新，「状态」查询可见）
    pub state_detail: Option<String>,
    /// 任务来源: "channel" (微信/通道派发) | "desktop" (桌面端会话)
    #[serde(default = "default_run_source")]
    pub source: String,
}

fn default_run_source() -> String {
    "channel".to_string()
}

pub(crate) fn runs_store() -> &'static Mutex<HashMap<String, RunRecord>> {
    static RUNS: OnceLock<Mutex<HashMap<String, RunRecord>>> = OnceLock::new();
    RUNS.get_or_init(|| Mutex::new(HashMap::new()))
}

// ---------------------------------------------------------------- 实时事件总线
// 网页端 SSE 推送的数据源：run 的进度/状态/终态事件在此广播，
// handler_events 订阅后按 session 过滤下发（网页端 EventSource 无法带 header，
// token 走 query 参数；这比 WebSocket 更贴合本机 HTTP + 内网穿透隧道场景）。

type SessionEvent = (String, Value);

fn event_bus() -> &'static tokio::sync::broadcast::Sender<SessionEvent> {
    static BUS: OnceLock<tokio::sync::broadcast::Sender<SessionEvent>> = OnceLock::new();
    BUS.get_or_init(|| tokio::sync::broadcast::channel(512).0)
}

pub(crate) fn notify_session_event(session_id: &str, kind: &str) {
    // progress 类事件走主循环热路径（实测可达 10+/s），SSE 端合并到 ≤3/s；
    // state/提问/终态类低频且关键，即时放行。
    static LAST_PROGRESS: OnceLock<Mutex<HashMap<String, std::time::Instant>>> = OnceLock::new();
    if kind == "progress" {
        let map = LAST_PROGRESS.get_or_init(|| Mutex::new(HashMap::new()));
        let now = std::time::Instant::now();
        let mut guard = map.lock().expect("notify lock poisoned");
        if let Some(last) = guard.get(session_id) {
            if now.duration_since(*last) < std::time::Duration::from_millis(300) {
                return;
            }
        }
        guard.insert(session_id.to_string(), now);
    }
    let _ = event_bus().send((
        session_id.to_string(),
        json!({ "type": kind, "at": now_ts() }),
    ));
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
    action_milestone_steps: Option<u32>,
    phase_pushes: Option<bool>,
    console_base_url: Option<String>,
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
        let _ = conn.execute("DELETE FROM agent_task_records WHERE session_id=?1", rusqlite::params![id]);
        let _ = conn.execute("DELETE FROM agent_task_resources WHERE session_id=?1", rusqlite::params![id]);
        let _ = conn.execute("DELETE FROM agent_checkpoints WHERE run_id=?1", rusqlite::params![id]);
        let _ = conn.execute("DELETE FROM agent_run_traces WHERE session_id=?1", rusqlite::params![id]);
        let _ = conn.execute("DELETE FROM agent_step_logs WHERE session_id=?1", rusqlite::params![id]);
        let _ = conn.execute("DELETE FROM agent_memory_nodes WHERE session_id=?1", rusqlite::params![id]);
        let _ = conn.execute("DELETE FROM agent_lesson_recalls WHERE session_id=?1", rusqlite::params![id]);
        let _ = conn.execute("DELETE FROM chat_messages WHERE session_id=?1", rusqlite::params![id]);
        let _ = conn.execute("DELETE FROM project_sessions WHERE session_id=?1", rusqlite::params![id]);
        let _ = conn.execute("DELETE FROM chat_sessions WHERE id=?1", rusqlite::params![id]);
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
pub(crate) struct SessionRunGuard {
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
                    record.result = result;
                });
                notify_session_event(&self.session_id, if was_cancelled { "cancelled" } else { "done" });
                let _ = self.app.emit(
                    "channel-run-done",
                    json!({ "run_id": self.run_id, "session_id": self.session_id, "status": "done", "at": now_ts() }),
                );
            }
            Err(error) => {
                patch_run(&self.run_id, |record| {
                    if record.status != "cancelled" {
                        record.status = "failed".to_string();
                        record.error = Some(truncate_chars(error, 2_000));
                    }
                    record.paused_reason = None;
                    record.paused_form = None;
                });
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
            false,
            Some(spawn_session.clone()),
            None,
            llm_retries,
            None,
            workspace_override,
            Some(spawn_run.clone()),
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
const CHANNEL_WEB_PAGE: &str = concat!(
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
const TRACE_FIELD_TRUNCATE: usize = 6_000;

/// 会话列表（含项目绑定与最近一次 trace 概要），网页端左栏/工作区卡片数据源。
/// 纯 SQL 便于单测；在途实时状态由 apply_run_overlay 在处理器里合并。
fn list_channel_sessions(
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
fn apply_run_overlay(mut sessions: Vec<Value>) -> Vec<Value> {
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
fn sanitize_trace_steps(steps: &mut Value) {
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
fn percent_decode(input: &str) -> String {
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
fn percent_encode_component(s: &str) -> String {
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

fn channel_router(state: Arc<ChannelState>) -> Router {
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
    // 1. 优先立即强杀托管的 Node 子进程，避免 Node 阻塞在微信长轮询时前端无响应
    let child = bridge_child_slot()
        .lock()
        .expect("child slot poisoned")
        .take();
    if let Some(mut process) = child {
        let _ = process.kill().await;
    }
    // 2. 尝试向可能孤立的端口发送优雅停止请求（忽略失败）
    let _ = bridge_call(&config, "POST", "/bridge/stop", None).await;
    cache_bridge_status(&json!({ "running": false, "logged_in": false }));
    Ok(json!({ "ok": true, "message": "微信桥已停止" }))
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
    if let Some(v) = config.action_milestone_steps {
        current.action_milestone_steps = v.clamp(0, 1000);
    }
    if let Some(v) = config.console_base_url.clone() {
        let trimmed = v.trim().to_string();
        current.console_base_url = if trimmed.is_empty() { None } else { Some(trimmed) };
    }
    if let Some(v) = config.phase_pushes {
        current.phase_pushes = v;
    }
    save_channel_config(&current)?;
    sync_shared_config(&current);
    Ok(config_value(&current))
}

/// 探测本机局域网 IPv4（UDP connect 不发包，仅取路由选定的本地地址）
fn lan_ipv4_addresses() -> Vec<String> {
    let mut out = Vec::new();
    let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") else {
        return out;
    };
    // 对多个公网锚点做路由探测，多网卡/多网段都能覆盖
    for anchor in ["8.8.8.8:80", "1.1.1.1:80", "114.114.114.114:80"] {
        if socket.connect(anchor).is_ok() {
            if let Ok(addr) = socket.local_addr() {
                let ip = addr.ip().to_string();
                if !ip.starts_with("127.") && !out.contains(&ip) {
                    out.push(ip);
                }
            }
            // connect 会改 socket 的对端，重置后继续
            let _ = socket.connect("0.0.0.0:0");
        }
    }
    out
}

/// 网页控制台二维码：默认二维码指向局域网地址（扫码免输 token）；
/// 配置了 console_base_url（内网穿透域名）则优先用它。
#[tauri::command]
pub async fn wechat_channel_console_qr() -> Result<Value, String> {
    let config = channel_config_shared().read().expect("config poisoned").clone();
    let port = config.port;
    let token = config.token.clone();

    let mut urls: Vec<String> = Vec::new();
    let custom = config
        .console_base_url
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.trim_end_matches('/').to_string());
    if let Some(base) = &custom {
        urls.push(format!("{base}/?token={token}"));
    }
    for ip in lan_ipv4_addresses() {
        let url = format!("http://{ip}:{port}/?token={token}");
        if !urls.contains(&url) {
            urls.push(url);
        }
    }
    if urls.is_empty() {
        urls.push(format!("http://127.0.0.1:{port}/?token={token}"));
    }

    // 主二维码 = 第一个候选（自定义穿透地址优先，否则局域网）
    use qrcode::render::svg::Color;
    let code = qrcode::QrCode::with_error_correction_level(urls[0].as_bytes(), qrcode::EcLevel::M)
        .map_err(|e| format!("二维码生成失败: {e}"))?;
    let svg = code
        .render::<Color>()
        .min_dimensions(220, 220)
        .quiet_zone(true)
        .build();

    Ok(json!({
        "svg": svg,
        "primary": urls[0],
        "urls": urls,
        "source": if custom.is_some() { "custom" } else { "lan" },
    }))
}

#[tauri::command]
pub async fn wechat_channel_runs() -> Result<Value, String> {
    let mut runs: Vec<RunRecord> = runs_store()
        .lock()
        .expect("runs store poisoned")
        .values()
        .filter(|r| r.source == "channel")
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
            paused_form: None,
            last_progress: None,
            event_count: 0,
            started_at: "2026-01-01T00:00:00+00:00".to_string(),
            updated_at: "2026-01-01T00:00:00+00:00".to_string(),
            result: None,
            error: None,
            llm_retries: 3,
            report_every_actions: 15,
            phase_push: true,
            phase: None,
            state_detail: None,
            source: "channel".to_string(),
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
                paused_form: None,
                last_progress: None,
                event_count: 0,
                started_at: "2025-01-01T00:00:00+00:00".to_string(),
                updated_at: "2025-01-01T00:00:00+00:00".to_string(),
                result: None,
                error: None,
                llm_retries: 3,
                report_every_actions: 15,
                phase_push: true,
                phase: None,
                state_detail: None,
                source: "channel".to_string(),
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
        assert_eq!(parsed.action_milestone_steps, 15);
        assert!(parsed.phase_pushes);
        // 缺失字段/未知字段都应容忍
        let parsed2: ChannelConfig =
            serde_json::from_str(r#"{"token":"t","unknown_field":1}"#).unwrap();
        assert_eq!(parsed2.token, "t");
        assert_eq!(parsed2.port, DEFAULT_API_PORT);
    }

    // ------------------------------------------------ 网页控制台：会话列表与 trace

    fn web_test_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE chat_sessions (
                id TEXT PRIMARY KEY, title TEXT NOT NULL, model_id TEXT NOT NULL,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                message_count INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE agent_projects (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, root_path TEXT NOT NULL);
            CREATE TABLE agent_project_sessions (
                project_id TEXT NOT NULL, session_id TEXT NOT NULL, created_at TEXT NOT NULL);
            CREATE TABLE agent_run_traces (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL, goal TEXT, status TEXT NOT NULL,
                steps_json TEXT NOT NULL, completion_json TEXT,
                started_at TEXT NOT NULL, updated_at TEXT NOT NULL, completed_at TEXT);",
        )
        .unwrap();
        conn
    }

    fn insert_session(conn: &rusqlite::Connection, id: &str, updated_at: &str) {
        conn.execute(
            "INSERT INTO chat_sessions (id, title, model_id, created_at, updated_at, message_count)
             VALUES (?1, ?1, 'model-a', '2026-08-30T00:00:00+00:00', ?2, 0)",
            rusqlite::params![id, updated_at],
        )
        .unwrap();
    }

    fn insert_trace(conn: &rusqlite::Connection, id: &str, session_id: &str, status: &str, updated_at: &str) {
        conn.execute(
            "INSERT INTO agent_run_traces (id, session_id, goal, status, steps_json, started_at, updated_at)
             VALUES (?1, ?2, '目标', ?3, '[]', ?4, ?4)",
            rusqlite::params![id, session_id, status, updated_at],
        )
        .unwrap();
    }

    #[test]
    fn session_listing_filters_and_orders() {
        let conn = web_test_conn();
        conn.execute(
            "INSERT INTO agent_projects (id, name, root_path) VALUES ('p1', 'free_api', '/tmp/free_api')",
            [],
        )
        .unwrap();
        insert_session(&conn, "s-project", "2026-08-30T01:00:00+00:00");
        conn.execute(
            "INSERT INTO agent_project_sessions (project_id, session_id, created_at)
             VALUES ('p1', 's-project', '2026-08-30T01:00:00+00:00')",
            [],
        )
        .unwrap();
        insert_trace(&conn, "t-project", "s-project", "succeeded", "2026-08-30T01:05:00+00:00");
        insert_session(&conn, "s-plain-new", "2026-08-30T02:00:00+00:00");
        insert_trace(&conn, "t-plain", "s-plain-new", "failed", "2026-08-30T02:10:00+00:00");
        insert_session(&conn, "s-bare", "2026-08-30T00:30:00+00:00");

        // 全量：按最近活动排序（s-plain-new > s-project > s-bare）
        let all = list_channel_sessions(&conn, None, false).unwrap();
        let ids: Vec<&str> = all
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["s-plain-new", "s-project", "s-bare"]);
        let plain = all.iter().find(|s| s["id"] == "s-plain-new").unwrap();
        assert_eq!(plain["status"], "failed");
        assert_eq!(plain["trace_id"], "t-plain");
        assert!(plain["project_id"].is_null());
        // 无 trace 的会话也要返回（status 为 null）
        let bare = all.iter().find(|s| s["id"] == "s-bare").unwrap();
        assert!(bare["status"].is_null());

        // 项目过滤
        let project = list_channel_sessions(&conn, Some("p1"), false).unwrap();
        assert_eq!(project.len(), 1);
        assert_eq!(project[0]["id"], "s-project");
        assert_eq!(project[0]["project_name"], "free_api");

        // 临时任务过滤：只含未绑定项目的会话
        let scratch = list_channel_sessions(&conn, None, true).unwrap();
        let scratch_ids: Vec<&str> = scratch.iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert_eq!(scratch_ids, vec!["s-plain-new", "s-bare"]);
    }

    #[test]
    fn run_overlay_marks_active_sessions() {
        let conn = web_test_conn();
        insert_session(&conn, "ov-session", "2026-08-30T01:00:00+00:00");
        insert_trace(&conn, "ov-trace", "ov-session", "succeeded", "2026-08-30T01:05:00+00:00");
        let sessions = list_channel_sessions(&conn, None, false).unwrap();
        assert_eq!(sessions[0]["status"], "succeeded");

        let run_id = format!("ov-run-{}", uuid::Uuid::new_v4());
        upsert_run(RunRecord {
            run_id: run_id.clone(),
            session_id: "ov-session".to_string(),
            goal_preview: "goal".to_string(),
            status: "waiting_human".to_string(),
            paused_reason: Some("需要确认删除哪些文件？".to_string()),
            paused_form: None,
            last_progress: None,
            event_count: 0,
            started_at: "2026-08-30T02:00:00+00:00".to_string(),
            updated_at: "2026-08-30T02:00:00+00:00".to_string(),
            result: None,
            error: None,
            llm_retries: 3,
            report_every_actions: 15,
            phase_push: true,
            phase: None,
            state_detail: None,
                source: "channel".to_string(),
            });
        let overlaid = apply_run_overlay(sessions);
        assert_eq!(overlaid[0]["status"], "waiting_human");
        assert_eq!(overlaid[0]["paused_reason"], "需要确认删除哪些文件？");
        assert_eq!(overlaid[0]["run_id"], run_id);
        let _ = runs_store().lock().unwrap().remove(&run_id);
    }

    #[test]
    fn percent_decode_handles_ascii_utf8_and_invalid() {
        assert_eq!(percent_decode("plain.txt"), "plain.txt");
        assert_eq!(percent_decode("%E6%8A%A5%E5%91%8A.txt"), "报告.txt");
        assert_eq!(percent_decode("a%2Fb%20c"), "a/b c");
        // 非法序列原样保留
        assert_eq!(percent_decode("abc%zz"), "abc%zz");
        assert_eq!(percent_decode("trail%"), "trail%");
    }

    #[test]
    fn trace_steps_sanitizer_truncates_long_fields() {
        let long_output = "x".repeat(TRACE_FIELD_TRUNCATE + 500);
        let mut steps = json!([
            { "tool": "read_file", "output": long_output, "thought": "短思考" },
            { "tool": "run_command", "output": "ok" }
        ]);
        sanitize_trace_steps(&mut steps);
        let entries = steps.as_array().unwrap();
        let sanitized = entries[0]["output"].as_str().unwrap();
        assert!(sanitized.chars().count() <= TRACE_FIELD_TRUNCATE + 20);
        assert!(sanitized.ends_with("……（已截断，完整内容见桌面端）"));
        assert_eq!(entries[0]["thought"], "短思考");
        assert_eq!(entries[1]["output"], "ok");
    }
}

// handler_web_home 是 async 但无 IO，直接验证页面已内嵌且非空
#[cfg(test)]
mod web_page_tests {
    use super::*;
    #[test]
    fn embedded_page_present() {
        assert!(CHANNEL_WEB_PAGE.contains("远程控制"));
        assert!(CHANNEL_WEB_PAGE.len() > 5_000);
    }
}
