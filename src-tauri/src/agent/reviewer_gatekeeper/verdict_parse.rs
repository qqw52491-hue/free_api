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


/// 只做字段类型/非空校验和明确别名转换，不判断缺陷语义。
/// 禁止把解析失败的缺陷列表当成 []，或把未知字段对象当成空白缺陷。
pub(crate) fn parse_reviewer_json(
    raw_text: &str,
    model_id: &str,
    diff_hash: &str,
    duration_ms: u64,
) -> Result<ReviewResult, String> {
    use serde_json::Value;
    let value: Value = serde_json::from_str(&extract_json_str(raw_text))
        .map_err(|e| format!("JSON 语法错误：{e}"))?;
    if !value.is_object() {
        return Err("终审判决必须是 JSON 对象".into());
    }
    let verdict = review_text(&value, &["verdict"], "verdict")?;
    if !matches!(
        verdict.as_str(),
        "passed" | "rejected" | "reject" | "fail" | "blocking"
    ) {
        return Err("verdict 必须为 passed 或 rejected".into());
    }
    let score = value
        .get("score")
        .and_then(Value::as_u64)
        .filter(|s| *s <= 100)
        .ok_or("score 缺失或类型/范围错误：必须为 0..100 的整数")? as u32;
    let summary = review_text(
        &value,
        &["summary", "conclusion", "verdict_reason", "decision_basis"],
        "summary",
    )?;
    let items = value
        .get("blocking_issues")
        .and_then(Value::as_array)
        .ok_or("blocking_issues 缺失或类型错误：必须显式提供数组，无缺陷时为 []")?;
    let mut blocking_issues = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let path = format!("blocking_issues[{index}]");
        if !item.is_object() {
            return Err(format!("{path} 必须为对象"));
        }
        let mut issue = review_text(
            item,
            &["issue", "detail", "description", "title"],
            &format!("{path}.issue"),
        )?;
        // 标题和正文往往分列，保留补充说明，避免转换时只剩一个标题。
        for key in [
            "title",
            "detail",
            "description",
            "analysis",
            "why_blocking",
            "impact",
        ] {
            if let Some(text) = item.get(key).and_then(Value::as_str) {
                if !text.trim().is_empty() && !issue.contains(text) {
                    issue.push_str(&format!("\n{key}: {text}"));
                }
            }
        }
        let requirement = review_text(
            item,
            &[
                "requirement",
                "required_fix",
                "required_action",
                "required_evidence",
            ],
            &format!("{path}.requirement"),
        )?;
        let file = review_file(item, &path)?;
        blocking_issues.push(BlockingIssue {
            file,
            issue,
            requirement,
        });
    }
    if verdict != "passed" && blocking_issues.is_empty() {
        return Err("verdict 表示拒绝，但 blocking_issues 为空：请提供缺陷或纠正 verdict".into());
    }
    let mut nit_issues = Vec::new();
    if let Some(items) = value.get("nit_issues") {
        let items = items.as_array().ok_or("nit_issues 类型错误：必须为数组")?;
        for (index, item) in items.iter().enumerate() {
            let path = format!("nit_issues[{index}]");
            let suggestion = review_text(item, &["suggestion"], &format!("{path}.suggestion"))?;
            nit_issues.push(NitIssue {
                file: review_file(item, &path)?,
                suggestion,
            });
        }
    }
    let missing_evidence = match value.get("missing_evidence") {
        Some(v) => serde_json::from_value::<Vec<String>>(v.clone())
            .map_err(|e| format!("missing_evidence 类型错误：必须为字符串数组（{e}）"))?,
        None => Vec::new(),
    };
    Ok(ReviewResult {
        // 有有效阻塞项时仍以阻塞项为准，不能因 verdict=passed 放行。
        verdict: if blocking_issues.is_empty() {
            "passed"
        } else {
            "rejected"
        }
        .into(),
        score,
        blocking_issues,
        nit_issues,
        summary,
        duration_ms,
        model_id: model_id.into(),
        diff_hash: diff_hash.into(),
        missing_evidence,
    })
}

fn review_text(value: &serde_json::Value, keys: &[&str], path: &str) -> Result<String, String> {
    for key in keys {
        if let Some(v) = value.get(key) {
            return v
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("{path}（字段 {key}）必须为非空字符串"));
        }
    }
    Err(format!("{path} 缺失；可用字段：{}", keys.join(" / ")))
}

fn review_file(item: &serde_json::Value, path: &str) -> Result<String, String> {
    // 全局/报告级缺陷允许不指定文件；显式给出的文件必须是字符串。
    for key in ["file", "path", "location"] {
        if let Some(value) = item.get(key) {
            return value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{path}.file（字段 {key}）必须为字符串"));
        }
    }
    Ok(String::new())
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

#[cfg(test)]
mod feedback_preservation_tests {
    use super::*;

    #[test]
    fn historical_aliases_preserve_blocking_feedback() {
        for key in ["required_fix", "required_action", "required_evidence"] {
            let mut item = json!({"title":"证据缺失", "detail":"报告缺少授权分支 diff"});
            item[key] = json!("附上 diff 和行号");
            let raw = json!({"verdict":"fail", "score":62,
                "conclusion":"当前证据不足", "blocking_issues":[item]});
            let result = parse_reviewer_json(&raw.to_string(), "mock", "hash", 12).unwrap();
            assert!(!result.is_passed());
            assert!(result.blocking_issues[0].issue.contains("证据缺失"));
            assert!(result.blocking_issues[0]
                .issue
                .contains("报告缺少授权分支 diff"));
            assert_eq!(result.blocking_issues[0].requirement, "附上 diff 和行号");
            assert_eq!(result.summary, "当前证据不足");
            assert_eq!(result.score, 62);
        }
    }

    #[test]
    fn malformed_verdicts_cannot_become_empty_blockers_or_success() {
        for (patch, field) in [
            (
                json!({"blocking_issues":[{"title":"报告有问题"}]}),
                "blocking_issues[0].requirement",
            ),
            (json!({"blocking_issues":[{}]}), "blocking_issues[0].issue"),
            (json!({"blocking_issues":"并非数组"}), "blocking_issues"),
            (json!({"blocking_issues":null}), "blocking_issues"),
            (
                json!({"blocking_issues":[{"issue":42,"requirement":"修正"}]}),
                "blocking_issues[0].issue",
            ),
            (json!({"score":null}), "score"),
            (json!({"score":101}), "score"),
            (json!({"summary":null}), "summary"),
            (json!({"nit_issues":[{}]}), "nit_issues[0].suggestion"),
            (json!({"missing_evidence":{}}), "missing_evidence"),
            (json!({"verdict":null}), "verdict"),
        ] {
            let mut raw = json!({"verdict":"passed", "score":90, "summary":"完成",
                "blocking_issues":[], "nit_issues":[]});
            raw.as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            let error = parse_reviewer_json(&raw.to_string(), "mock", "hash", 0).unwrap_err();
            assert!(error.contains(field), "{error}");
        }
        for raw in [
            r#"{"verdict":"reject","blocking_issues":[]}"#,
            r#"{"score":90,"summary":"完成"}"#,
        ] {
            assert!(parse_reviewer_json(raw, "mock", "hash", 0).is_err());
        }
    }
}
