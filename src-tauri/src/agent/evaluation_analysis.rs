//! 评测指标分析引擎：从 evaluation.rs 抽出。
//!
//! 从原始模型日志与运行轨迹中提取结构化指标（工具步骤/失败率/并行分支/
//! 重复签名/资源作用域/token 用量），供正式报告与基线对比使用。

use super::evaluation::{
    AgentEvaluationReport, ComplexDeliveryAssessment, EvaluationMetrics, RepeatedActionSignature,
};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone)]
pub(crate) struct ParsedAction {
    label: String,
    signature: String,
    is_read_intent: bool,
    parallel_branches: usize,
    /// 本批适用的受控扇出上限（纯只读 4 / 含写入执行或未知工具 2；非并行批为 0）
    parallel_cap: usize,
    parallel_without_rationale: bool,
    plan_title: Option<String>,
}

pub(crate) fn analyze(
    logs: &[Value],
    traces: &[Value],
) -> (
    EvaluationMetrics,
    BTreeMap<String, usize>,
    Vec<RepeatedActionSignature>,
    Vec<String>,
) {
    let mut metrics = EvaluationMetrics {
        llm_call_count: logs.len(),
        ..Default::default()
    };
    let mut action_counts = BTreeMap::new();
    let mut signatures = BTreeMap::new();
    let mut first_timestamp: Option<DateTime<Utc>> = None;
    let mut last_timestamp: Option<DateTime<Utc>> = None;
    let mut previous_plan_title: Option<String> = None;

    for log in logs {
        let parse_status = log
            .get("parse_status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if parse_status == "success" {
            metrics.parse_success_count += 1;
        } else {
            metrics.parse_error_count += 1;
        }
        if let Some(usage) = log.get("usage") {
            metrics.max_prompt_tokens = metrics
                .max_prompt_tokens
                .max(number(usage, "prompt_tokens"));
            metrics.total_prompt_tokens += number(usage, "prompt_tokens");
            metrics.total_completion_tokens += number(usage, "completion_tokens");
            metrics.total_cached_tokens += number(usage, "cached_tokens");
            metrics.max_context_usage_percent = metrics
                .max_context_usage_percent
                .max(number_f64(usage, "usage_percent"));
        }
        let messages_chars = log
            .get("messages_sent_to_model")
            .map(|messages| messages.to_string().chars().count())
            .unwrap_or(0);
        metrics.max_messages_char_count = metrics.max_messages_char_count.max(messages_chars);
        let raw_response = log
            .get("raw_response")
            .and_then(Value::as_str)
            .unwrap_or_default();
        metrics.max_raw_response_char_count = metrics
            .max_raw_response_char_count
            .max(raw_response.chars().count());
        if let Some(action) = parse_action(raw_response) {
            *action_counts.entry(action.label).or_insert(0) += 1;
            if action.parallel_branches > 0 {
                metrics.parallel_batch_count += 1;
                metrics.max_parallel_branches =
                    metrics.max_parallel_branches.max(action.parallel_branches);
                if action.parallel_branches > action.parallel_cap {
                    metrics.parallel_cap_exceeded_count += 1;
                }
                if action.parallel_without_rationale {
                    metrics.parallel_without_rationale_count += 1;
                }
            }
            if let Some(plan_title) = action.plan_title {
                if previous_plan_title
                    .as_deref()
                    .is_some_and(|previous| previous != plan_title)
                {
                    metrics.plan_title_transition_count += 1;
                }
                previous_plan_title = Some(plan_title);
            }
            if action.is_read_intent {
                metrics.read_intent_count += 1;
                *signatures.entry(action.signature).or_insert(0) += 1;
            }
        }
        if let Some(timestamp) = log
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_time)
        {
            first_timestamp = Some(first_timestamp.map_or(timestamp, |old| old.min(timestamp)));
            last_timestamp = Some(last_timestamp.map_or(timestamp, |old| old.max(timestamp)));
        }
    }

    metrics.unique_read_intent_count = signatures.len();
    let repeated_action_signatures: Vec<RepeatedActionSignature> = signatures
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(signature, count)| RepeatedActionSignature { signature, count })
        .collect();
    metrics.repeated_read_intent_count = repeated_action_signatures
        .iter()
        .map(|entry| entry.count - 1)
        .sum();
    let mut failed_signatures = BTreeMap::new();
    for trace in traces {
        if let Some(steps) = trace.get("steps").and_then(Value::as_array) {
            for step in steps {
                count_trace_step(step, &mut metrics, &mut failed_signatures);
            }
        }
    }
    metrics.repeated_failed_action_count = failed_signatures
        .values()
        .filter(|count| **count > 1)
        .map(|count| count - 1)
        .sum();
    metrics.parse_success_rate_percent =
        percentage(metrics.parse_success_count, metrics.llm_call_count);
    metrics.cache_hit_rate_percent = if metrics.total_prompt_tokens > 0 {
        (metrics.total_cached_tokens as f64 / metrics.total_prompt_tokens as f64) * 100.0
    } else {
        0.0
    };
    metrics.observed_duration_seconds = match (first_timestamp, last_timestamp) {
        (Some(start), Some(end)) => (end - start).num_seconds().max(0),
        _ => 0,
    };

    metrics.tool_failure_rate_percent =
        percentage(metrics.tool_failure_count, metrics.tool_step_count);

    let mut warnings = Vec::new();
    if metrics.parse_error_count > 0 {
        warnings.push(format!(
            "检测到 {} 次 JSON 解析错误；应确认修复重试没有重新发送不必要的历史。",
            metrics.parse_error_count
        ));
    }
    if metrics.tool_failure_count > 0 {
        warnings.push(format!(
            "检测到 {} 个失败工具步骤；应按失败类型改变策略，避免机械重试。",
            metrics.tool_failure_count
        ));
    }
    if metrics.repeated_read_intent_count > 0 {
        warnings.push(format!(
            "检测到 {} 次重复的读取意图；请结合具体签名确认是否属于必要分页。",
            metrics.repeated_read_intent_count
        ));
    }
    if metrics.max_context_usage_percent >= 80.0 {
        warnings.push(format!(
            "上下文峰值使用率为 {:.1}%，已接近窗口上限。",
            metrics.max_context_usage_percent
        ));
    }
    if metrics.repeated_failed_action_count > 0 {
        warnings.push(format!(
            "检测到 {} 次相同失败调用重犯；应确认失败假设已被结构化吸收，而非仅在自然语言中重复解释。",
            metrics.repeated_failed_action_count
        ));
    }
    if metrics.parallel_without_rationale_count > 0 {
        warnings.push(format!(
            "检测到 {} 个并行批次未给出独立性理由；应确认每个分支的工具契约与待验证问题已经稳定。",
            metrics.parallel_without_rationale_count
        ));
    }
    if metrics.parallel_cap_exceeded_count > 0 {
        warnings.push(format!(
            "检测到 {} 个并行批次超过受控扇出上限（纯只读 4 / 含写入执行 2）；应复核宿主拒绝路径是否生效。",
            metrics.parallel_cap_exceeded_count
        ));
    }
    (metrics, action_counts, repeated_action_signatures, warnings)
}

fn count_trace_step(
    step: &Value,
    metrics: &mut EvaluationMetrics,
    failed_signatures: &mut BTreeMap<String, usize>,
) {
    let Some(object) = step.as_object() else {
        return;
    };
    let has_tool_shape = ["tool", "action", "route", "result", "success"]
        .iter()
        .any(|key| object.contains_key(*key));
    if let Some(output) = object.get("output").and_then(Value::as_str) {
        if output.contains("[Resource scope]") {
            if output.contains("relation: outside") {
                metrics.resource_scope_outside_count += 1;
            }
            if output.contains("relation: unknown") {
                metrics.resource_scope_unknown_count += 1;
            }
        }
    }
    if let Some(revision) = object
        .get("resource_scope_revision")
        .and_then(Value::as_u64)
    {
        metrics.scope_rebind_count = metrics
            .scope_rebind_count
            .max(revision.saturating_sub(1) as usize);
    }
    if has_tool_shape {
        metrics.tool_step_count += 1;
        let failed = object.get("success").and_then(Value::as_bool) == Some(false)
            || object.get("status").and_then(Value::as_str) == Some("failed")
            || object
                .get("result")
                .and_then(|value| value.get("success"))
                .and_then(Value::as_bool)
                == Some(false);
        if failed {
            metrics.tool_failure_count += 1;
            let tool = object
                .get("tool")
                .or_else(|| object.get("action"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let params = object
                .get("params")
                .or_else(|| object.get("command"))
                .map(Value::to_string)
                .unwrap_or_default();
            let signature = format!(
                "{} | {}",
                tool,
                params.chars().take(360).collect::<String>()
            );
            *failed_signatures.entry(signature).or_insert(0) += 1;
        }
    }
    for value in object.values() {
        if let Some(items) = value.as_array() {
            for item in items {
                count_trace_step(item, metrics, failed_signatures);
            }
        }
    }
}

pub(crate) fn parse_action(raw_response: &str) -> Option<ParsedAction> {
    let value = serde_json::from_str::<Value>(raw_response).ok()?;
    let parallel_steps = value
        .get("parallel")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let parallel_branches = parallel_steps.len();
    let parallel_cap = if parallel_branches > 0 {
        crate::agent::plan_tree_dispatch::controlled_parallel_cap_for_tools(
            parallel_steps
                .iter()
                .map(crate::agent::plan_tree_dispatch::parallel_branch_label_from_json),
        )
    } else {
        0
    };
    let tool = value
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let action = value
        .get("action")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("command")
                .and_then(|command| command.get("action"))
                .and_then(Value::as_str)
        })
        .unwrap_or_default();
    let label = if parallel_branches > 0 {
        format!("parallel:{}", parallel_branches)
    } else if action.is_empty() {
        tool.to_string()
    } else {
        format!("{}:{}", tool, action)
    };
    if label.is_empty() {
        return None;
    }
    let normalized = label.to_ascii_lowercase();
    let is_read_intent = ["read", "search", "list", "grep", "get_step_output", "diff"]
        .iter()
        .any(|needle| normalized.contains(needle));
    let command = value
        .get("command")
        .map(Value::to_string)
        .unwrap_or_default();
    let parallel_without_rationale = parallel_branches > 0
        && value
            .get("parallel_reason")
            .and_then(Value::as_str)
            .is_none_or(|reason| reason.trim().is_empty());
    let plan_title = value
        .get("plan_title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string);
    let signature = if is_read_intent {
        format!(
            "{} | {}",
            label,
            command.chars().take(360).collect::<String>()
        )
    } else {
        label.clone()
    };
    Some(ParsedAction {
        label,
        signature,
        is_read_intent,
        parallel_branches,
        parallel_cap,
        parallel_without_rationale,
        plan_title,
    })
}

/// 从最后一个 finish 指令只提取复杂诊断交付的结构性边界。
///
/// 评测报告只保存布尔值和缺失段落，不保存模型的 summary 正文，
/// 避免将用户内容或敏感工具输出重新写入评测产物。
pub(crate) fn assess_complex_delivery(logs: &[Value]) -> ComplexDeliveryAssessment {
    let Some(summary) = logs.iter().rev().find_map(|log| {
        log.get("raw_response")
            .and_then(Value::as_str)
            .and_then(finish_summary)
    }) else {
        return ComplexDeliveryAssessment::default();
    };

    assess_complex_delivery_summary(&summary)
}

/// 运行期 finish 拦截复用的摘要检查；调用方只消费布尔结果，不保存或回显 summary 正文。
pub(crate) fn assess_complex_delivery_summary(summary: &str) -> ComplexDeliveryAssessment {
    let mut assessment = ComplexDeliveryAssessment {
        finish_found: true,
        ..Default::default()
    };
    assessment.verified_scope_present =
        contains_any(summary, &["已核验范围", "已验证范围", "核验范围", "验证范围"]);
    assessment.assumptions_or_unverified_present =
        contains_any(summary, &["未核验范围", "未验证范围", "关键假设", "未核验", "假设与未核验"]);
    assessment.conclusion_strength_present =
        (summary.contains("结论强度") || summary.contains("置信度") || summary.contains("结论"))
            && contains_any(summary, &["已证实", "部分支持", "未验证", "confirmed", "likely", "hypothesis"]);
    let root_cause_window = root_cause_claim_window(summary);
    assessment.root_cause_calibrated = root_cause_window.is_none_or(|window| {
        (window.contains("证据") || window.contains("依据"))
            && contains_any(&window, &["已证实", "部分支持", "未验证", "confirmed", "likely", "hypothesis"])
    });
    assessment.compliant = assessment.verified_scope_present
        && assessment.assumptions_or_unverified_present
        && assessment.conclusion_strength_present
        && assessment.root_cause_calibrated;
    assessment
}

/// 交付文本自身是否宣称了主根因结论（与窗口检测共用同一组标记词）。
pub(crate) fn summary_makes_root_cause_claim(summary: &str) -> bool {
    root_cause_claim_window(summary).is_some()
}

/// 截取主根因附近的短窗口，避免文末的全局免责声明替代该结论自己的证据与强度。
fn root_cause_claim_window(summary: &str) -> Option<String> {
    let markers = [
        "主根因",
        "根因判断",
        "根因结论",
        "根因分析",
        "核心根因",
        "故障根因",
        "根本原因",
        "根因排查",
    ];
    let marker_pos = markers
        .iter()
        .filter_map(|marker| summary.find(marker))
        .min()?;
    let slice = &summary[marker_pos..];
    // 如果后面有同级或高级标题（如 \n# 、\n## 、\n### ）或分割线，切到该位置；跳过紧跟 marker 的分割线
    let next_major_section = slice
        .find("\n# ")
        .or_else(|| slice.find("\n## "))
        .or_else(|| slice.find("\n### "))
        .or_else(|| slice.find("\n---"));

    let section = match next_major_section {
        Some(idx) if idx > 50 => &slice[..idx],
        _ => slice,
    };
    Some(section.chars().take(2500).collect())
}

fn finish_summary(raw_response: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(raw_response).ok()?;
    let tool = value
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let action = value
        .get("action")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("command")
                .and_then(|command| command.get("action"))
                .and_then(Value::as_str)
        })
        .unwrap_or_default();
    if tool != "finish" && action != "finish" {
        return None;
    }
    value
        .get("command")
        .and_then(Value::as_object)
        .and_then(|command| command.get("summary"))
        .and_then(Value::as_str)
        .or_else(|| value.get("summary").and_then(Value::as_str))
        .map(str::to_string)
}

fn contains_any(text: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| text.contains(marker))
}

fn number(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn number_f64(value: &Value, key: &str) -> f64 {
    value
        .get(key)
        .and_then(Value::as_f64)
        .or_else(|| {
            value
                .get(key)
                .and_then(Value::as_i64)
                .map(|number| number as f64)
        })
        .unwrap_or(0.0)
}

pub(crate) fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

fn percentage(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        (part as f64 / total as f64) * 100.0
    }
}

pub(crate) fn verdict(
    metrics: &EvaluationMetrics,
    warnings: &[String],
    trace_status: &str,
) -> String {
    if metrics.llm_call_count == 0 {
        return "inconclusive".to_string();
    }
    let has_failure = metrics.parse_error_count > 0 || metrics.tool_failure_count > 0;
    let unresolved_failure = has_failure && trace_status != "succeeded";
    if unresolved_failure
        || metrics.repeated_failed_action_count > 0
        || metrics.max_parallel_branches > 6
    {
        return "needs_attention".to_string();
    }
    if has_failure || warnings.is_empty() {
        if has_failure {
            "review_recommended".to_string()
        } else {
            "passes_observed_guardrails".to_string()
        }
    } else {
        "review_recommended".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics_with_single_tool_failure() -> EvaluationMetrics {
        EvaluationMetrics {
            llm_call_count: 1,
            tool_failure_count: 1,
            ..EvaluationMetrics::default()
        }
    }

    #[test]
    fn succeeded_trace_with_single_changed_strategy_failure_is_review_recommended() {
        let metrics = metrics_with_single_tool_failure();
        assert_eq!(
            verdict(&metrics, &["工具失败已记录".to_string()], "succeeded"),
            "review_recommended"
        );
    }

    #[test]
    fn non_succeeded_trace_with_failure_still_needs_attention() {
        let metrics = metrics_with_single_tool_failure();
        assert_eq!(
            verdict(&metrics, &["工具失败已记录".to_string()], "cancelled"),
            "needs_attention"
        );
    }

    #[test]
    fn repeated_failure_still_needs_attention_after_succeeded_trace() {
        let mut metrics = metrics_with_single_tool_failure();
        metrics.repeated_failed_action_count = 1;
        assert_eq!(
            verdict(&metrics, &["相同失败调用重犯".to_string()], "succeeded"),
            "needs_attention"
        );
    }

    fn finish_log(summary: &str) -> Vec<Value> {
        vec![serde_json::json!({
            "raw_response": serde_json::json!({
                "tool": "finish",
                "command": { "summary": summary },
            })
            .to_string(),
        })]
    }

    #[test]
    fn complex_delivery_passes_only_when_all_three_boundary_sections_exist() {
        let logs = finish_log(
            "# 审查报告\n\n### 主根因\n- 部分支持：A。直接证据：a.rs 的相关调用链。\n\n### 已核验范围\n- A\n\n### 未核验范围或关键假设\n- B\n\n### 结论强度\n- 已证实：A；部分支持：B；未验证：C",
        );
        let assessment = assess_complex_delivery(&logs);
        assert!(assessment.finish_found);
        assert!(assessment.verified_scope_present);
        assert!(assessment.assumptions_or_unverified_present);
        assert!(assessment.conclusion_strength_present);
        assert!(assessment.compliant);
    }

    #[test]
    fn complex_delivery_flags_missing_uncertainty_boundary() {
        let logs = finish_log(
            "# 审查报告\n\n### 主根因\n- 已证实：A。直接证据：a.rs。\n\n### 已核验范围\n- A\n\n### 结论强度\n- 已证实：A",
        );
        let assessment = assess_complex_delivery(&logs);
        assert!(assessment.finish_found);
        assert!(assessment.verified_scope_present);
        assert!(!assessment.assumptions_or_unverified_present);
        assert!(assessment.conclusion_strength_present);
        assert!(!assessment.compliant);
    }

    #[test]
    fn complex_delivery_rejects_a_root_cause_without_local_evidence_and_strength() {
        let logs = finish_log(
            "# 审查报告\n\n### 主根因\n- registry 泄漏。\n\n### 已核验范围\n- A\n\n### 未核验范围或关键假设\n- B\n\n### 结论强度\n- 已证实：A；部分支持：B；未验证：C",
        );
        let assessment = assess_complex_delivery(&logs);
        assert!(!assessment.root_cause_calibrated);
        assert!(!assessment.compliant);
    }

    #[test]
    fn complex_delivery_accepts_direct_factual_evidence_and_realistic_layout() {
        let text = "# 案件数据导入全局拖拽遮罩卡住与残留问题排查与修复\n\n### 已核验范围\n- `case-transfer-widget.js`\n\n### 未核验范围与结论强度\n- 未修改其他无关业务页面\n- 结论强度：**【已证实】**。\n\n---\n\n### 主根因\n结论强度：**已证实**\n直接事实证据：\n1. case-transfer-widget.js:568-572 事件冒泡导致失衡\n";
        let logs = finish_log(text);
        let assessment = assess_complex_delivery(&logs);
        assert!(assessment.verified_scope_present);
        assert!(assessment.assumptions_or_unverified_present);
        assert!(assessment.conclusion_strength_present);
        assert!(assessment.root_cause_calibrated);
        assert!(assessment.compliant);
    }
    #[test]
    fn complex_delivery_uses_latest_finish_even_if_later_logs_are_not_finish() {
        let mut logs = finish_log(
            "# 审查报告\n\n### 根因结论\n- 已证实：A。直接证据：a.rs。\n\n### 已验证范围\n- A\n\n### 关键假设\n- B\n\n### 结论强度\n- 已证实：A",
        );
        logs.push(serde_json::json!({
            "raw_response": serde_json::json!({
                "tool": "filesystem",
                "action": "read_text_file",
                "command": { "path": "later.rs" },
            })
            .to_string(),
        }));
        assert!(assess_complex_delivery(&logs).compliant);
    }
}
