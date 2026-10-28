//! 评审结果类型与 diff 噪声过滤。

use super::*;
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


/// 审查阻塞性缺陷结构
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BlockingIssue {
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub issue: String,
    #[serde(default)]
    pub requirement: String,
}

/// 次要改进建议结构（不阻碍交付）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NitIssue {
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub suggestion: String,
}

impl ReviewResult {
    pub fn is_passed(&self) -> bool {
        !self.is_service_failure() && self.blocking_issues.is_empty()
    }

    pub fn is_service_failure(&self) -> bool {
        matches!(self.verdict.as_str(), "failed_open" | "cancelled")
    }

    pub(crate) fn cancelled(reason: &str, model_id: &str, diff_hash: &str) -> Self {
        let mut result = Self::failed_open(reason, model_id, diff_hash);
        result.verdict = "cancelled".into();
        result.summary = reason.into();
        result
    }

    pub fn skipped_empty() -> Self {
        Self {
            verdict: "passed".to_string(),
            score: 100,
            blocking_issues: Vec::new(),
            nit_issues: Vec::new(),
            summary: "任务未涉及代码文件修改，免除 Diff 审查".to_string(),
            duration_ms: 0,
            model_id: "none".to_string(),
            diff_hash: String::new(),
            missing_evidence: Vec::new(),
        }
    }

    pub fn failed_open(reason: &str, model_id: &str, diff_hash: &str) -> Self {
        Self {
            verdict: "failed_open".to_string(),
            score: 0,
            blocking_issues: Vec::new(),
            nit_issues: Vec::new(),
            summary: format!("终审通道失败，审查未完成: {}", reason),
            duration_ms: 0,
            model_id: model_id.to_string(),
            diff_hash: diff_hash.to_string(),
            missing_evidence: Vec::new(),
        }
    }

    /// 分流判决 mechanical_only 的直通结果：终审只需机械校验，不调 LLM。
    pub fn mechanical_pass(summary: &str) -> Self {
        Self {
            verdict: "passed".to_string(),
            score: 100,
            blocking_issues: Vec::new(),
            nit_issues: Vec::new(),
            summary: summary.to_string(),
            duration_ms: 0,
            model_id: "triage:mechanical_only".to_string(),
            diff_hash: String::new(),
            missing_evidence: Vec::new(),
        }
    }

    pub fn rejected_with_scope_drift(reason: &str, diff_hash: &str) -> Self {
        Self {
            verdict: "rejected".to_string(),
            score: 0,
            blocking_issues: vec![BlockingIssue {
                file: "global".to_string(),
                issue: reason.to_string(),
                requirement: "结合本轮工具记录核实计划外差异的来源、任务关联和内容风险；归属未确认时不得要求撤销共享工作区修改，已确认的本轮扩围应说明原因并更新方案范围".to_string(),
            }],
            nit_issues: Vec::new(),
            summary: reason.to_string(),
            duration_ms: 0,
            model_id: "scope_drift_guard".to_string(),
            diff_hash: diff_hash.to_string(),
            missing_evidence: Vec::new(),
        }
    }
}

/// 判定是否为二进制文件（Excel/Word/PDF/图片/压缩包等）
pub fn is_binary_file(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.ends_with(".xlsx")
        || p.ends_with(".xls")
        || p.ends_with(".docx")
        || p.ends_with(".doc")
        || p.ends_with(".pptx")
        || p.ends_with(".pdf")
        || p.ends_with(".zip")
        || p.ends_with(".tar")
        || p.ends_with(".gz")
        || p.ends_with(".png")
        || p.ends_with(".jpg")
        || p.ends_with(".jpeg")
        || p.ends_with(".ico")
        || p.ends_with(".webp")
        || p.ends_with(".mp4")
        || p.ends_with(".mp3")
}

/// 自动审查材料排除运行日志、lockfile、构建产物、依赖库等。
/// 材料展示、变更路径与完整内容指纹共用此规则。
pub fn is_noise_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    crate::agent::utils::runtime_logs::is_runtime_log_path(path)
        || p.contains("package-lock.json")
        || p.contains("pnpm-lock.yaml")
        || p.contains("yarn.lock")
        || p.contains("cargo.lock")
        || p.contains("node_modules/")
        || p.contains("target/")
        || p.contains("dist/")
        || p.contains(".git/")
        || p.contains(".turbo/")
}

/// 过滤 Git Diff 中的噪音文件段落
pub fn filter_diff_noise(raw_diff: &str) -> String {
    let mut result = Vec::new();
    let mut current_chunk = Vec::new();
    let mut is_noisy = false;

    for line in raw_diff.lines() {
        if line.starts_with("diff --git ") {
            if !current_chunk.is_empty() && !is_noisy {
                result.extend(current_chunk);
            }
            current_chunk = vec![line];
            is_noisy = is_noise_path(line);
        } else {
            current_chunk.push(line);
        }
    }
    if !current_chunk.is_empty() && !is_noisy {
        result.extend(current_chunk);
    }
    result.join("\n")
}
