//! Agent 工具事件结构与前端事件推送。

use crate::agent::mcp::PluginRegistry;
use crate::agent::mcp_catalog::invalidate_plugin;
use crate::agent::plan_tree_dispatch::{
    run_plan_tree_sync, validate_controlled_parallel_batch, MAX_CONTROLLED_PARALLEL_BRANCHES,
};
use crate::agent::retrieval;
use crate::agent::task_store;
use crate::agent::types::{AgentInstruction, DispatchResult};
use crate::agent::utils::{run_osascript, run_shell};
use crate::agent::web_search;
use crate::db::DbState;
use chrono::Utc;
use reqwest::Url;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

use super::*;

/// 面向 AgentPanel 的轻量工具生命周期事件。只发送参数和输出摘要；完整大对象
/// 仍由 task_store / 文件系统持久化，不能被事件流或模型上下文重复承载。
#[derive(Debug, Clone, Serialize)]
pub struct AgentToolEvent {
    pub event_type: String,
    pub step_id: usize,
    pub description: String,
    pub tool: String,
    pub action: String,
    pub route: Option<String>,
    pub success: Option<bool>,
    pub params_summary: String,
    pub result_summary: String,
    pub duration_ms: Option<u128>,
    pub timestamp: String,
    /// 工具直供的结构化视图（kind 判别联合）：有值时前端渲染类型化卡片，
    /// 无值回退 params/result_summary 的文本启发式路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<serde_json::Value>,
}

pub fn event_summary(value: &str, limit: usize) -> String {
    let normalized = value.trim();
    if normalized.chars().count() <= limit {
        normalized.to_string()
    } else {
        format!("{}…", normalized.chars().take(limit).collect::<String>())
    }
}

pub fn emit_tool_event(
    app: &AppHandle,
    session_id: &str,
    step_id: usize,
    inst: &AgentInstruction,
    event_type: &str,
    route: Option<String>,
    success: Option<bool>,
    result: &str,
    duration_ms: Option<u128>,
    view: Option<&serde_json::Value>,
) {
    let payload = AgentToolEvent {
        event_type: event_type.to_string(),
        step_id,
        description: inst.description.clone(),
        tool: inst.get_tool(),
        action: inst.get_action(),
        route,
        success,
        params_summary: event_summary(&inst.get_params().to_string(), 8000),
        result_summary: event_summary(result, 16000),
        duration_ms,
        timestamp: Utc::now().to_rfc3339(),
        view: view.cloned(),
    };
    let _ = app.emit(&format!("agent-tool-event-{}", session_id), payload);
}

