use super::*;
use crate::agent::types::{ChildErrorKind, TokenUsage};
use crate::commands::projects::read_project_resource_excerpts_for_session;
use crate::db::DbState;
use crate::llm::{self, ModelConfig};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;
use uuid::Uuid;

/// 方案入口、子审查请求和审查输出共用同一预算，完整保留引用，不截断路径。
pub(crate) fn normalize_review_evidence_refs(references: &[String]) -> Result<Vec<String>, String> {
    for (index, reference) in references.iter().enumerate() {
        if reference.trim().chars().any(char::is_control) {
            return Err(format!("evidence_refs 第 {} 条含控制字符；引用须为单行字符串。", index + 1));
        }
    }
    crate::agent::utils::evidence_refs::normalize_evidence_refs(references)
}


pub fn parse_child_review_request(params: &Value) -> Result<ChildReviewRequest, String> {
    // 模型常把不用的字段显式传 null（如 evidence_excerpt: null），#[serde(default)] 不处理显式 null：
    // 先把 null 字段当作未传；真有类型错误时把真实原因告诉模型，不再统一报「必须是 JSON 对象」。
    let mut cleaned = params.clone();
    if let Some(obj) = cleaned.as_object_mut() {
        obj.retain(|_, v| !v.is_null());
    }
    let raw: RawChildReviewRequest = serde_json::from_value(cleaned).map_err(|e| {
        format!(
            "child_reviewer 参数格式不合法：{}。需要 JSON 对象；question/evidence_excerpt 是字符串，evidence_refs/resource_ids 是字符串数组；不用的字段请直接省略。",
            e
        )
    })?;
    let question = normalize_question(&raw.question, MAX_QUESTION_CHARS, "question")?;
    if raw.evidence_excerpt.chars().count() > MAX_EVIDENCE_CHARS {
        return Err(format!(
            "child_reviewer.evidence_excerpt 最多 {} 个字符；请先由父 Agent 收缩证据。",
            MAX_EVIDENCE_CHARS
        ));
    }

    let mut refs = normalize_review_evidence_refs(&raw.evidence_refs)?;
    refs.sort();
    let mut resource_ids = BTreeSet::new();
    for resource_id in raw.resource_ids {
        resource_ids.insert(normalize_nonempty(&resource_id, 160, "resource_ids")?);
    }
    if resource_ids.len() > MAX_CHILD_RESOURCE_IDS {
        return Err(format!(
            "child_reviewer.resource_ids 最多 {} 个。",
            MAX_CHILD_RESOURCE_IDS
        ));
    }
    // 受管资源 ID 会成为审查输出允许引用的一部分，入口一并计入同一预算。
    let combined_refs: Vec<String> = refs.iter().chain(resource_ids.iter()).cloned().collect();
    normalize_review_evidence_refs(&combined_refs)?;
    if raw.evidence_excerpt.trim().is_empty() && refs.is_empty() && resource_ids.is_empty() {
        return Err(
            "child_reviewer 必须携带证据：三选一传入 evidence_refs（你已读取过的文件路径，如 README.md、package.json）、evidence_excerpt（关键代码/文档摘录）或 resource_ids（受管资源 ID）。\n若尚未读取任何文件，请先用 read_text_file/grep 取证，再派发审查——空手派发审查没有意义。".to_string(),
        );
    }

    Ok(ChildReviewRequest {
        question,
        evidence_excerpt: raw.evidence_excerpt.trim().to_string(),
        evidence_refs: refs,
        resource_ids: resource_ids.into_iter().collect(),
    })
}

pub(crate) fn build_managed_resource_evidence(
    db: &DbState,
    session_id: &str,
    request: &ChildReviewRequest,
) -> Result<(String, Vec<String>), String> {
    let manual_chars = request.evidence_excerpt.chars().count();
    let available_chars = MAX_EVIDENCE_CHARS.saturating_sub(manual_chars);
    if !request.resource_ids.is_empty() && available_chars == 0 {
        return Err("child_reviewer 证据摘录已占满预算，不能再读取受管资源。".to_string());
    }
    let excerpts = if request.resource_ids.is_empty() {
        Vec::new()
    } else {
        let conn = db.0.lock().map_err(|error| error.to_string())?;
        read_project_resource_excerpts_for_session(
            &conn,
            session_id,
            &request.resource_ids,
            available_chars,
        )?
    };

    let mut allowed_refs = request.evidence_refs.clone();
    let mut evidence_excerpt = request.evidence_excerpt.clone();
    for excerpt in excerpts {
        let separator = if evidence_excerpt.is_empty() { "" } else { "\n\n" };
        let remaining_chars = MAX_EVIDENCE_CHARS.saturating_sub(evidence_excerpt.chars().count());
        let plain_header = format!("受管资源 [{}] {}：\n", excerpt.resource_id, excerpt.relative_path);
        let plain_header_chars = separator.chars().count() + plain_header.chars().count();
        let needs_truncation = excerpt.truncated
            || excerpt.content.chars().count() > remaining_chars.saturating_sub(plain_header_chars);
        let header = if needs_truncation {
            format!(
                "受管资源 [{}] {}（已按预算截断）：\n",
                excerpt.resource_id, excerpt.relative_path
            )
        } else {
            plain_header
        };
        let header_chars = separator.chars().count() + header.chars().count();
        if header_chars >= remaining_chars {
            return Err("child_reviewer 受管资源元数据超过剩余证据预算。".to_string());
        }
        let content_budget = remaining_chars - header_chars;
        let content: String = excerpt.content.chars().take(content_budget).collect();
        allowed_refs.push(excerpt.resource_id);
        evidence_excerpt.push_str(separator);
        evidence_excerpt.push_str(&header);
        evidence_excerpt.push_str(&content);
    }
    debug_assert!(evidence_excerpt.chars().count() <= MAX_EVIDENCE_CHARS);
    allowed_refs.sort();
    allowed_refs.dedup();
    normalize_review_evidence_refs(&allowed_refs)?;
    Ok((evidence_excerpt, allowed_refs))
}

pub(crate) fn normalize_nonempty(value: &str, max_chars: usize, field: &str) -> Result<String, String> {

    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(format!("child_reviewer 缺少 {}。", field));
    }
    if normalized.chars().count() > max_chars {
        return Err(format!(
            "child_reviewer.{} 最多 {} 个字符。",
            field, max_chars
        ));
    }
    if normalized.chars().any(char::is_control) {
        return Err(format!("child_reviewer.{} 含不可接受的控制字符。", field));
    }
    Ok(normalized.to_string())
}

/// question 专用归一化：换行/制表等控制符折叠为单空格继续走，不打回。
/// 实测 27 次「含不可接受的控制字符」全是模型写多行问题被格式拒收——
/// 约束本意是单行展示，压平即可满足，拒绝是白烧一步重试。
pub(crate) fn normalize_question(value: &str, max_chars: usize, field: &str) -> Result<String, String> {
    let flattened: String = value
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ");
    if flattened.is_empty() {
        return Err(format!("child_reviewer 缺少 {}。", field));
    }
    if flattened.chars().count() > max_chars {
        return Err(format!(
            "child_reviewer.{} 最多 {} 个字符。",
            field, max_chars
        ));
    }
    Ok(flattened)
}

pub fn parse_child_review_rebuttal_output(
    raw: &str,
) -> Result<(
    Vec<ReviewFinding>,
    String,
    Vec<String>,
    Vec<String>,
    String,
    Vec<ObjectionVerdict>,
    Vec<String>,
), String> {
    let (findings, confidence, refs, missing, routing, _, verdicts, withdrawn) = parse_controlled_output(raw, &[])?;
    Ok((findings, confidence, refs, missing, routing, verdicts, withdrawn))
}

/// 宽松 JSON 提取：审查模型在大上下文下常把 JSON 包进 ```json 围栏、前后加一句
/// 说明文字，或与思考文本混合。实测（09-07 pro 实弹）：审查员 33 次 invalid_output
/// 大多源于此，每次触发整链重派烧真 token。策略：优先原样解析；失败则剥围栏、
/// 截取首个 '{' 到最后一个 '}' 的片段再解析。截取后仍解析失败才算真 invalid。
pub(crate) fn extract_json_object(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end < start {
        return None;
    }
    Some(&trimmed[start..=end])
}

pub(crate) fn review_output_rejection_feedback(reason: &str, attempt: u32, limit: u32) -> String {
    let next_action = if attempt >= limit {
        "已达到提交校验重试上限，子审查任务以 invalid_output 结束。"
    } else {
        "请按上述具体错误修正并重新输出完整审查 JSON。"
    };
    format!(
        "【系统】审查结果提交校验未通过（第 {}/{} 次）：{}\n本次未形成有效审查结论，不代表方案被驳回。{}",
        attempt, limit, reason, next_action
    )
}

pub(crate) fn parse_controlled_output(
    raw: &str,
    allowed_refs: &[String],
) -> Result<(
    Vec<ReviewFinding>,
    String,
    Vec<String>,
    Vec<String>,
    String,
    Option<ReviewScorecard>,
    Vec<ObjectionVerdict>,
    Vec<String>,
), String> {
    let output: RawChildReviewOutput = serde_json::from_str(raw)
        .or_else(|original_error| match extract_json_object(raw) {
            Some(fragment) => serde_json::from_str(fragment),
            None => Err(original_error),
        })
        .map_err(|error| format!("child_reviewer 审查 JSON 解析失败：{}。请修正对应字段或 JSON 语法。", error))?;
    if output.findings.len() > 5 {
        return Err(format!("child_reviewer.findings 实际 {} 项，最多 5 项。请缩减或合并为至多 5 项。", output.findings.len()));
    }
    if !matches!(output.confidence.as_str(), "confirmed" | "supported" | "unverified") {
        return Err(format!("child_reviewer.confidence 实际为 {:?}，允许值是 confirmed、supported 或 unverified。", output.confidence));
    }
    if output.missing_evidence.len() > 3 {
        return Err(format!("child_reviewer.missing_evidence 实际 {} 项，最多 3 项。请缩减或合并为至多 3 项。", output.missing_evidence.len()));
    }

    let allowed_refs_set = allowed_refs.iter().collect::<BTreeSet<_>>();
    let mut output_refs = BTreeSet::new();
    for reference in normalize_review_evidence_refs(&output.evidence_refs)? {
        if !allowed_refs.is_empty() && !allowed_refs_set.contains(&reference) {
            return Err(format!(
                "child_reviewer.evidence_refs 引用 {:?} 不在父 Agent 显式传入的列表中。请原样使用完整引用（包括末尾说明），允许列表：{}。",
                reference, serde_json::to_string(allowed_refs).unwrap_or_default()
            ));
        }
        output_refs.insert(reference);
    }
    for missing in &output.missing_evidence {
        normalize_nonempty(missing, MAX_MISSING_EVIDENCE_CHARS, "missing_evidence")?;
    }

    let mut structured_findings = Vec::new();
    let mut has_blocking = false;

    for (idx, raw_f) in output.findings.into_iter().enumerate() {
        let default_id = format!("F{}", idx + 1);
        let finding = match raw_f {
            FlexibleRawFinding::Structured(s) => {
                let id = s.id.unwrap_or_else(|| default_id.clone());
                let id = if id.trim().is_empty() { default_id.clone() } else { id.trim().to_string() };
                let location = s.location.unwrap_or_default().trim().to_string();
                let severity = s.severity.ok_or_else(|| format!(
                    "child_reviewer.findings[{idx}].severity 缺失，必须明确提供 blocking、suggestion 或 question。"
                ))?.trim().to_lowercase();
                if !matches!(severity.as_str(), "blocking" | "suggestion" | "question") {
                    return Err(format!(
                        "child_reviewer.findings[{idx}].severity 实际为 {severity:?}，允许值是 blocking、suggestion 或 question；请由审查员明确分类，不会自动转换风险等级。"
                    ));
                }
                // blocking 终态相关性：regardless（中间态即错）| final_only（仅最终交付前必须解决）
                let finality = s
                    .finality
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase();
                let finality = match finality.as_str() {
                    "final_only" | "blocking_if_final" => "final_only".to_string(),
                    _ => "regardless".to_string(),
                };
                if severity == "blocking" {
                    has_blocking = true;
                }
                let current = s.current.unwrap_or_default().trim().to_string();
                let mut why = s.why.unwrap_or_default().trim().to_string();
                if why.is_empty() {
                    why = current.clone();
                }
                if let Some(title) = s.title.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    if why.is_empty() {
                        why = title.to_string();
                    } else if !why.contains(title) {
                        why = format!("{title}\n{why}");
                    }
                }
                if why.is_empty() {
                    return Err(format!(
                        "child_reviewer.findings[{idx}] 缺少缺陷说明：why/description、current 或 title 至少提供一项非空文本。"
                    ));
                }
                let suggestion = s.suggestion.unwrap_or_default().trim().to_string();
                let evidence_strength = s.evidence_strength.unwrap_or_else(|| output.confidence.clone());
                ReviewFinding {
                    id,
                    severity,
                    finality,
                    location,
                    current: current.clone(),
                    why,
                    suggestion,
                    evidence_strength,
                }
            }
            FlexibleRawFinding::Text(text) => {
                let text = normalize_nonempty(&text, MAX_FINDING_CHARS, "findings")?;
                ReviewFinding {
                    id: default_id,
                    severity: "question".to_string(),
                    finality: "regardless".to_string(),
                    location: String::new(),
                    current: String::new(),
                    why: text,
                    suggestion: String::new(),
                    evidence_strength: output.confidence.clone(),
                }
            }
        };
        structured_findings.push(finding);
    }

    // 🌟 推导返回路由 routing（全部撤回或无 blocking 时直接 certain）
    let routing = if let Some(r) = output.routing {
        if matches!(r.as_str(), "certain" | "needs_alignment" | "blocked") {
            r
        } else if has_blocking || output.confidence == "unverified" {
            "needs_alignment".to_string()
        } else if !output.missing_evidence.is_empty() {
            "blocked".to_string()
        } else {
            "certain".to_string()
        }
    } else if has_blocking || output.confidence == "unverified" {
        "needs_alignment".to_string()
    } else if !output.missing_evidence.is_empty() {
        "blocked".to_string()
    } else {
        "certain".to_string()
    };

    let scorecard = output.scorecard.map(|sc| {
        ReviewScorecard::with_reason(
            sc.correctness.unwrap_or(80),
            sc.risk.unwrap_or(80),
            sc.style.unwrap_or(85),
            sc.test_coverage,
            sc.null_reason,
        )
    });

    let objection_verdicts: Vec<ObjectionVerdict> = output
        .objection_verdicts
        .into_iter()
        .map(|raw| {
            let reason = raw.reason.unwrap_or_default().trim().to_string();
            // 撤回必须有据：理由非空且达到最低信息量，否则按 maintain 处理。
            // 这是结构性底线（挡模板化橡皮擦）；"理由是否真的引用了父Agent的
            // 修订内容"是语义判断，由审查方法约束 + 事后 objection_stats 观测兜底。
            let reason_is_substantive = reason.chars().count() >= 12;
            ObjectionVerdict {
                finding_id: raw.finding_id.unwrap_or_default().trim().to_string(),
                // 缺失/非法 verdict 或理由空洞 一律按 maintain 处理：裁决不明确时绝不当撤回
                verdict: match raw.verdict.as_deref().map(|s| s.trim().to_ascii_lowercase()) {
                    Some(v) if v == "withdraw" && reason_is_substantive => v,
                    Some(v) if v == "maintain" => v,
                    _ => "maintain".to_string(),
                },
                reason,
            }
        })
        .collect();
    let mut withdrawn_findings = resolve_withdrawn_ids(&structured_findings, &objection_verdicts);
    for v in &objection_verdicts {
        if v.verdict.eq_ignore_ascii_case("withdraw") && !v.finding_id.is_empty() {
            if !withdrawn_findings.iter().any(|id| id == &v.finding_id) {
                withdrawn_findings.push(v.finding_id.clone());
            }
        }
    }

    Ok((
        structured_findings,
        output.confidence,
        output_refs.into_iter().collect(),
        output.missing_evidence.into_iter().map(|s| s.trim().to_string()).collect(),
        routing,
        scorecard,
        objection_verdicts,
        withdrawn_findings,
    ))
}
