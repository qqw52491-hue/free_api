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
pub mod config;
pub mod http;
pub mod bridge;
pub mod runtime;
pub mod tunnel;
#[cfg(test)]
mod tests;

pub(crate) use config::*;
pub use config::{
    channel_config_path, load_channel_config, save_channel_config, ChannelConfig,
};

/// 门禁档位：按风险分级的审查强度预设（保险按保额定价，而非一刀全开或全关）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateProfile {
    /// 轻量：仅收尾审查 + 机械门禁（人在环的小改）
    L1,
    /// 标准：方案 + 阶段 + 收尾全开（生产默认，今日 24/24 收敛验证档）
    L2,
    /// 全量：L2 + 每次写入必审（高危变更；双审查员会签为未建增强项）
    L3,
}

pub fn parse_gate_profile(raw: Option<&str>) -> GateProfile {
    match raw.map(|s| s.trim().to_ascii_uppercase()).as_deref() {
        Some("L1") => GateProfile::L1,
        Some("L3") => GateProfile::L3,
        _ => GateProfile::L2,
    }
}

/// 活动档位：环境变量 FREE_API_GATE_PROFILE 优先，其次 channel-config 的
/// gate_profile 字段，默认 L2。
pub fn active_gate_profile() -> GateProfile {
    if let Ok(v) = std::env::var("FREE_API_GATE_PROFILE") {
        return parse_gate_profile(Some(&v));
    }
    let configured = channel_config_shared()
        .read()
        .ok()
        .and_then(|c| c.gate_profile.clone());
    parse_gate_profile(configured.as_deref())
}

/// 档位 → 阶段审查有效开关：L1 关闭，L2/L3 按配置（默认开）。
pub fn profile_stage_review_enabled(
    profile: GateProfile,
    config_enabled: Option<bool>,
) -> bool {
    profile != GateProfile::L1 && config_enabled.unwrap_or(true)
}

/// 档位 → 阶段审查触发阈值：L3 每次写入必审（1），其余按配置（默认 6）。
pub fn profile_stage_review_max_files(profile: GateProfile, config_max: Option<u32>) -> u32 {
    match profile {
        GateProfile::L3 => 1,
        _ => config_max.unwrap_or(6),
    }
}
pub use bridge::{
    wechat_bridge_login_start, wechat_bridge_qr, wechat_bridge_relogin, wechat_bridge_start,
    wechat_bridge_status, wechat_bridge_stop, wechat_channel_config_get,
    wechat_channel_config_set, wechat_channel_console_qr, wechat_channel_runs,
};
pub(crate) use http::{ensure_scratch_project, register_and_spawn_run};
pub(crate) use bridge::notify_wechat;
pub(crate) use runtime::channel_config_shared;
pub(crate) use http::{BRIDGE_WATCHDOG_SUPPRESSED, config_value, ConfigPutReq, channel_router, APP_HANDLE};
pub(crate) use bridge::ensure_bridge_running;
pub(crate) use runtime::{app_handle, cache_bridge_status, cached_bridge_status, sync_shared_config};
pub(crate) use http::{SessionRunGuard, SubmitError};
pub use runtime::spawn_channel_server;

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

/// 会话事件统一分发中枢：一处调用，三端齐达——
///  ① SSE event_bus → 手机/网页控制台（EventSource）
///  ② Tauri emit channel-session-event → 桌面端打开中的会话/侧栏
/// （微信推送属语义决策不进本中枢，由各业务点显式调 notify_wechat）
///
/// 历史教训：事件点手工挑通道发 → 新事件忘发其中一路 = 某端静默不刷新
/// （run 中人工应答只进内存通道、桌面气泡页永不可见的实测 bug 即此类）。
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
    let payload = json!({ "type": kind, "at": now_ts() });
    let _ = event_bus().send((session_id.to_string(), payload.clone()));
    if let Some(app) = runtime::app_handle() {
        use tauri::Emitter;
        let _ = app.emit(
            "channel-session-event",
            json!({ "session_id": session_id, "kind": kind, "at": payload["at"] }),
        );
    }
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
