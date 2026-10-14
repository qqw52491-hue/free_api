//! 评审裁决 JSON 解析与机械预检违规枚举。

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


/// 稳健解析 Reviewer 返回的 JSON
pub(crate) fn parse_reviewer_json(
    raw_text: &str,
    model_id: &str,
    diff_hash: &str,
    duration_ms: u64,
) -> ReviewResult {
    let clean = extract_json_str(raw_text);
    if let Ok(mut parsed) = serde_json::from_str::<ReviewResult>(&clean) {
        parsed.duration_ms = duration_ms;
        parsed.model_id = model_id.to_string();
        parsed.diff_hash = diff_hash.to_string();
        // 依据契约以 blocking_issues 为唯一定论
        if parsed.blocking_issues.is_empty() {
            parsed.verdict = "passed".to_string();
        } else {
            parsed.verdict = "rejected".to_string();
        }
        return parsed;
    }

    // 尝试轻量容错提取
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&clean) {
        let score = v.get("score").and_then(|s| s.as_u64()).unwrap_or(80) as u32;
        let summary = v.get("summary").and_then(|s| s.as_str()).unwrap_or("审查完成").to_string();
        let blocking_issues = v.get("blocking_issues")
            .and_then(|b| serde_json::from_value::<Vec<BlockingIssue>>(b.clone()).ok())
            .unwrap_or_default();
        let nit_issues = v.get("nit_issues")
            .and_then(|n| serde_json::from_value::<Vec<NitIssue>>(n.clone()).ok())
            .unwrap_or_default();
        let missing_evidence = v.get("missing_evidence")
            .and_then(|m| serde_json::from_value::<Vec<String>>(m.clone()).ok())
            .unwrap_or_default();

        let verdict = if blocking_issues.is_empty() { "passed" } else { "rejected" }.to_string();
        return ReviewResult {
            verdict,
            score,
            blocking_issues,
            nit_issues,
            summary,
            duration_ms,
            model_id: model_id.to_string(),
            diff_hash: diff_hash.to_string(),
            missing_evidence,
        };
    }

    // JSON 解析彻底失败：Fail-Open 避免卡死
    ReviewResult::failed_open("Reviewer 输出非标准 JSON，未获得有效审查结论", model_id, diff_hash)
}

pub(crate) fn extract_json_str(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(start) = trimmed.find("```json") {
        if let Some(end) = trimmed[start + 7..].find("```") {
            return trimmed[start + 7..start + 7 + end].trim().to_string();
        }
    }
    if let Some(start) = trimmed.find("```") {
        if let Some(end) = trimmed[start + 3..].find("```") {
            return trimmed[start + 3..start + 3 + end].trim().to_string();
        }
    }
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if start <= end {
            return trimmed[start..=end].to_string();
        }
    }
    trimmed.to_string()
}

/// 机械门禁零 Token 预检（在调用大模型审查前执行）：
/// 检查实际修改的文件是否超出了当前活跃目标块的预定边界 (Scope)。
pub fn mechanical_precheck_violations(
    workspace: &Path,
    active_block: Option<&crate::agent::target_block::TargetBlock>,
) -> Vec<String> {
    let mut violations = Vec::new();
    if let Some(block) = active_block {
        if !block.scope.is_empty() {
            if let Some(files_str) = git_cmd(workspace, &["diff", "--name-only", "HEAD"]) {
                let modified: Vec<String> = files_str
                    .lines()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty() && !is_noise_path(s))
                    .collect();
                let bad = block.find_scope_violations(&modified);
                for file in bad {
                    violations.push(format!(
                        "文件 [{}] 发生改动，但超出了当前目标块 [{}] 的预定允许范围 (Scope: {:?})",
                        file, block.id, block.scope
                    ));
                }
            }
        }
    }
    violations
}

