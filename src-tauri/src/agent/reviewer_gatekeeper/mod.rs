use crate::agent::context::SandwichContext;
use crate::agent::router::{AgentRouter, ModelRoutingConfig, roles};
use crate::agent::tools::role_manager::resolve_enabled_card;
use crate::db::DbState;
use crate::llm::{self, ModelConfig};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;

/// 用户在「模型请求重试」设置里配的总尝试次数（与主循环同一处来源；1=不重试）。
/// 终审的超时/调用报错/非 JSON 重试都以它为准：调到 0（即 1 次尝试）就直接返回失败。
pub(crate) static LLM_REQUEST_ATTEMPTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(3);

pub(crate) fn review_retry_allowed() -> bool {
    LLM_REQUEST_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed) > 1
}

mod baseline;
mod evidence;
mod review_types;
mod git_utils;
mod finish_diff;
mod verdict_parse;

pub(crate) use baseline::*;
pub(crate) use evidence::*;
pub(crate) use review_types::*;
pub(crate) use git_utils::*;
pub(crate) use finish_diff::*;
pub(crate) use verdict_parse::*;
const MAX_DIFF_CHARS: usize = 25_000;

/// Git 任务起点基准快照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitBaseline {
    pub is_git: bool,
    pub base_head: Option<String>,
}

/// 终态代码审查结果
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewResult {
    pub verdict: String, // "passed" | "rejected" | "skipped" | "failed_open"
    pub score: u32,      // 0-100 仅供遥测与 UI 显示
    pub blocking_issues: Vec<BlockingIssue>,
    pub nit_issues: Vec<NitIssue>,
    pub summary: String,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub model_id: String,
    #[serde(default)]
    pub diff_hash: String,
    /// 未能核实的疑点/证据缺口（预算耗尽、产物不可达、材料缺页等）——
    /// 让"我看不见/没查完"从 summary 散文变成结构化可观测信号。
    #[serde(default)]
    pub missing_evidence: Vec<String>,
}

#[cfg(test)]
mod tests;
