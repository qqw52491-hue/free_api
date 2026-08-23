use crate::agent::evaluation_analysis::{analyze, verdict, parse_time, parse_action};
use crate::agent::run_trace;
use crate::db::DbState;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;

pub const READ_ONLY_REGRESSION_GOAL: &str = r#"你是一名证据驱动的只读代码审查员。请审查当前工作区中 Agent 的运行链路，重点判断它在审查较大项目时，是否仍会出现无效的重复读取、截断后机械回查、错误重试放大、上下文反弹或不必要并行。\n\n本任务是回归评测：严格只读。不得修改、创建、删除或重命名任何文件；不得提交 Git；不得执行有副作用的命令；不得调用会改变 MCP 全局工作目录的操作。宿主不会替你补全或覆盖任何 MCP 工具参数，因此每次调用都必须根据工具契约明确提供所需路径、cwd 或项目参数。\n\n请沿着“用户目标 → 模型规划 → 工具调用 → 工具结果写入 → 截断/采样 → 历史组装 → 工件回查 → 错误或解析重试”的链路取证。先建立最小项目地图，再按符号、行窗口或已知调用关系定向阅读。对尚未收缩范围的未知问题默认串行；只有每个分支都对应固定待验证问题，且任一结果不会改变另一分支的读取或解释时才可并行。安全窗口分页是正常取证，不是失败；结论所需证据已经充分时立即收尾，不要为了覆盖所有文件继续读取。\n\n最终交付一份简洁审查结论，逐条标明：已缓解的路径、仍可能出现的问题、触发条件、具体代码证据（文件与符号或行范围）、影响、优先级和最小可验证修复建议。不得把截断片段当作完整证据，不得虚构运行时行为。"#;

/// 复杂取消一致性审查的独立评测场景标识。
pub const COMPLEX_CANCELLATION_DIAGNOSTIC_SCENARIO: &str = "complex_cancellation_diagnostic";

/// 该场景测试跨模块的状态机取证能力，而不是要求模型通读整个项目。
/// 它只用于独立后台评测，绝不注入普通会话的系统提示词。
pub const COMPLEX_CANCELLATION_DIAGNOSTIC_GOAL: &str = r#"你是一名证据驱动的并发与状态一致性审查员。请严格只读审查当前工作区中 Agent 任务的“取消、暂停与恢复”链路：当用户主动停止任务、Agent 进入 ask_human 暂停、后台评测达到时限、或进程在工具调用后恢复时，系统是否能保证旧任务不会继续写入状态、不会被错误恢复、不会与新任务交叉污染？

不得修改、创建、删除或重命名任何文件；不得提交 Git；不得执行有副作用的命令；不得改变 MCP 全局工作目录。宿主不会替你补全或覆盖 MCP 工具参数，每次调用都必须按工具契约显式提供 cwd、path 或项目参数。

先建立最小状态地图：列出状态持有者、会话/任务 ID、事件名、持久化记录与每条边界的责任模块。然后追踪四条具体路径：（A）用户显式取消；（B）ask_human 暂停；（C）后台评测超时取消；（D）工具调用后进程恢复。只有状态地图已经将各路径收缩为固定待验证问题后，才可并行阅读互不依赖的分支；路径不同本身不构成并行理由。

对每条路径给出“触发事件 → 状态变化 → 取消/恢复信号 → 持久化/清理 → 后续写入阻断”的证据链；每个核心结论至少引用两个相关文件、函数或行窗口。明确区分代码已证明的保证、仍依赖时序而未证明的假设、以及可能的竞态或遗留写入风险。若线索不一致，回到状态地图或补读必要调用方，不得猜测。

最终交付：状态地图、四条路径的证据链、风险、反证或不确定性、优先级和最小可验证修复建议。证据足够时立即收束，不要为了通读更多文件而继续探索。"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvaluationReport {
    pub schema_version: u32,
    pub generated_at: String,
    pub session_id: String,
    pub evaluation_id: Option<String>,
    #[serde(default)]
    pub routing: Option<EvaluationRoutingMetadata>,
    pub log_path: String,
    pub trace_status: String,
    pub metrics: EvaluationMetrics,
    #[serde(default)]
    pub baseline: Option<EvaluationBaselineComparison>,
    pub action_counts: BTreeMap<String, usize>,
    pub repeated_action_signatures: Vec<RepeatedActionSignature>,
    pub warnings: Vec<String>,
    pub verdict: String,
    pub report_json_path: String,
    pub report_markdown_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EvaluationMetrics {
    pub llm_call_count: usize,
    pub parse_success_count: usize,
    pub parse_error_count: usize,
    pub parse_success_rate_percent: f64,
    pub tool_step_count: usize,
    pub tool_failure_count: usize,
    pub tool_failure_rate_percent: f64,
    pub parallel_batch_count: usize,
    pub max_parallel_branches: usize,
    pub read_intent_count: usize,
    #[serde(default)]
    pub unique_read_intent_count: usize,
    pub repeated_read_intent_count: usize,
    #[serde(default)]
    pub repeated_failed_action_count: usize,
    #[serde(default)]
    pub plan_title_transition_count: usize,
    #[serde(default)]
    pub parallel_without_rationale_count: usize,
    #[serde(default)]
    pub resource_scope_outside_count: usize,
    #[serde(default)]
    pub resource_scope_unknown_count: usize,
    #[serde(default)]
    pub scope_rebind_count: usize,
    pub max_prompt_tokens: i64,
    pub total_prompt_tokens: i64,
    pub total_completion_tokens: i64,
    pub total_cached_tokens: i64,
    pub cache_hit_rate_percent: f64,
    pub max_context_usage_percent: f64,
    pub max_messages_char_count: usize,
    pub max_raw_response_char_count: usize,
    pub observed_duration_seconds: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepeatedActionSignature {
    pub signature: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationRoutingMetadata {
    pub preset: String,
    pub reasoning_model_id: String,
    pub execution_model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationBaselineComparison {
    pub baseline_evaluation_id: Option<String>,
    pub baseline_generated_at: String,
    pub llm_call_delta: i64,
    pub parse_error_delta: i64,
    pub tool_failure_delta: i64,
    pub repeated_read_intent_delta: i64,
    #[serde(default)]
    pub repeated_failed_action_delta: i64,
    #[serde(default)]
    pub plan_title_transition_delta: i64,
    #[serde(default)]
    pub parallel_without_rationale_delta: i64,
    pub max_parallel_branches_delta: i64,
    pub max_prompt_tokens_delta: i64,
    pub max_context_usage_percent_delta: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvaluationRun {
    pub evaluation_id: String,
    pub scenario_id: String,
    pub session_id: String,
    pub goal: String,
    pub started_at: String,
}

pub fn begin_agent_evaluation(session_id: &str) -> Result<AgentEvaluationRun, String> {
    begin_agent_evaluation_for_scenario(session_id, None)
}

pub fn begin_agent_evaluation_for_scenario(
    session_id: &str,
    requested_scenario: Option<&str>,
) -> Result<AgentEvaluationRun, String> {
    let scenario_id = requested_scenario.unwrap_or("read_only_regression");
    let (evaluation_prefix, goal) = match scenario_id {
        "read_only_regression" => ("agent-readonly", READ_ONLY_REGRESSION_GOAL),
        COMPLEX_CANCELLATION_DIAGNOSTIC_SCENARIO => (
            "agent-complex-cancellation",
            COMPLEX_CANCELLATION_DIAGNOSTIC_GOAL,
        ),
        other => return Err(format!("未知的只读评测场景：{}", other)),
    };
    let project_dir = std::env::current_dir().map_err(|error| error.to_string())?;
    let run = AgentEvaluationRun {
        evaluation_id: format!("{}-{}", evaluation_prefix, uuid::Uuid::new_v4()),
        scenario_id: scenario_id.to_string(),
        session_id: session_id.to_string(),
        goal: goal.to_string(),
        started_at: Utc::now().to_rfc3339(),
    };
    let run_dir = project_dir.join("agent_evaluations").join("runs");
    fs::create_dir_all(&run_dir).map_err(|error| error.to_string())?;
    let run_path = run_dir.join(format!("{}.json", run.evaluation_id));
    let serialized = serde_json::to_string_pretty(&run).map_err(|error| error.to_string())?;
    fs::write(run_path, serialized).map_err(|error| error.to_string())?;
    Ok(run)
}

pub fn build_and_persist_report(
    db: &DbState,
    session_id: &str,
    evaluation_id: Option<&str>,
    routing: Option<EvaluationRoutingMetadata>,
) -> Result<AgentEvaluationReport, String> {
    let project_dir = std::env::current_dir().map_err(|error| error.to_string())?;
    let cutoff = evaluation_id
        .map(|id| load_evaluation_started_at(&project_dir, id, session_id))
        .transpose()?;
    let log_path = project_dir
        .join("agent_logs")
        .join(format!("{}.json", session_id));
    let logs = entries_since(read_log_entries(&log_path)?, cutoff, "timestamp");
    let traces = entries_since(
        run_trace::list_session_traces(db, session_id)?,
        cutoff,
        "started_at",
    );
    let trace_status = traces
        .last()
        .and_then(|trace| trace.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("missing")
        .to_string();

    let (metrics, action_counts, repeated_action_signatures, mut warnings) =
        analyze(&logs, &traces);
    if logs.is_empty() {
        warnings.push(
            "未找到模型交换日志。请以调试模式运行评测，或确认会话未在首轮前失败。".to_string(),
        );
    }
    if trace_status == "missing" {
        warnings.push("未找到运行轨迹，因此工具成功/失败指标不可判定。".to_string());
    }
    let verdict = verdict(&metrics, &warnings);

    let report_dir = project_dir.join("agent_evaluations");
    fs::create_dir_all(&report_dir).map_err(|error| error.to_string())?;
    let report_stem = evaluation_id.unwrap_or(session_id);
    let baseline = load_latest_baseline(&report_dir, report_stem);
    let baseline_comparison = baseline
        .as_ref()
        .map(|previous| compare_metrics(previous, &metrics));
    let json_path = report_dir.join(format!("{}.json", report_stem));
    let markdown_path = report_dir.join(format!("{}.md", report_stem));
    let report = AgentEvaluationReport {
        schema_version: 1,
        generated_at: Utc::now().to_rfc3339(),
        session_id: session_id.to_string(),
        evaluation_id: evaluation_id.map(str::to_string),
        routing,
        log_path: log_path.display().to_string(),
        trace_status,
        metrics,
        baseline: baseline_comparison,
        action_counts,
        repeated_action_signatures,
        warnings,
        verdict,
        report_json_path: json_path.display().to_string(),
        report_markdown_path: markdown_path.display().to_string(),
    };
    let json = serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?;
    fs::write(&json_path, json).map_err(|error| error.to_string())?;
    fs::write(&markdown_path, render_markdown(&report)).map_err(|error| error.to_string())?;
    Ok(report)
}

fn evaluation_family(stem: &str) -> &str {
    if stem.starts_with("agent-complex-cancellation-") {
        "agent-complex-cancellation-"
    } else {
        "agent-readonly-"
    }
}

fn load_latest_baseline(
    report_dir: &std::path::Path,
    current_stem: &str,
) -> Option<AgentEvaluationReport> {
    let family = evaluation_family(current_stem);
    fs::read_dir(report_dir)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let is_current = path.file_stem().and_then(|name| name.to_str()) == Some(current_stem);
            let is_same_family = path
                .file_stem()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(family));
            if is_current
                || !is_same_family
                || path.extension().and_then(|extension| extension.to_str()) != Some("json")
            {
                return None;
            }
            let source = fs::read_to_string(path).ok()?;
            let report = serde_json::from_str::<AgentEvaluationReport>(&source).ok()?;
            (report.schema_version == 1 && report.evaluation_id.is_some()).then_some(report)
        })
        .max_by(|left, right| left.generated_at.cmp(&right.generated_at))
}

fn compare_metrics(
    baseline: &AgentEvaluationReport,
    current: &EvaluationMetrics,
) -> EvaluationBaselineComparison {
    let previous = &baseline.metrics;
    EvaluationBaselineComparison {
        baseline_evaluation_id: baseline.evaluation_id.clone(),
        baseline_generated_at: baseline.generated_at.clone(),
        llm_call_delta: current.llm_call_count as i64 - previous.llm_call_count as i64,
        parse_error_delta: current.parse_error_count as i64 - previous.parse_error_count as i64,
        tool_failure_delta: current.tool_failure_count as i64 - previous.tool_failure_count as i64,
        repeated_read_intent_delta: current.repeated_read_intent_count as i64
            - previous.repeated_read_intent_count as i64,
        repeated_failed_action_delta: current.repeated_failed_action_count as i64
            - previous.repeated_failed_action_count as i64,
        plan_title_transition_delta: current.plan_title_transition_count as i64
            - previous.plan_title_transition_count as i64,
        parallel_without_rationale_delta: current.parallel_without_rationale_count as i64
            - previous.parallel_without_rationale_count as i64,
        max_parallel_branches_delta: current.max_parallel_branches as i64
            - previous.max_parallel_branches as i64,
        max_prompt_tokens_delta: current.max_prompt_tokens - previous.max_prompt_tokens,
        max_context_usage_percent_delta: current.max_context_usage_percent
            - previous.max_context_usage_percent,
    }
}

fn load_evaluation_started_at(
    project_dir: &std::path::Path,
    evaluation_id: &str,
    expected_session_id: &str,
) -> Result<DateTime<Utc>, String> {
    let marker_path = project_dir
        .join("agent_evaluations")
        .join("runs")
        .join(format!("{}.json", evaluation_id));
    let source = fs::read_to_string(&marker_path)
        .map_err(|error| format!("无法读取评测启动标记 {}: {}", marker_path.display(), error))?;
    let run: AgentEvaluationRun =
        serde_json::from_str(&source).map_err(|error| error.to_string())?;
    if run.session_id != expected_session_id {
        return Err("评测启动标记与请求会话不一致".to_string());
    }
    parse_time(&run.started_at).ok_or_else(|| "评测启动标记时间无效".to_string())
}

fn entries_since(
    entries: Vec<Value>,
    cutoff: Option<DateTime<Utc>>,
    timestamp_field: &str,
) -> Vec<Value> {
    let Some(cutoff) = cutoff else { return entries };
    entries
        .into_iter()
        .filter(|entry| {
            entry
                .get(timestamp_field)
                .and_then(Value::as_str)
                .and_then(parse_time)
                .is_some_and(|timestamp| timestamp >= cutoff)
        })
        .collect()
}

fn read_log_entries(path: &std::path::Path) -> Result<Vec<Value>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let source = fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_json::from_str(&source)
        .map_err(|error| format!("无法解析评测日志 {}: {}", path.display(), error))
}

fn render_markdown(report: &AgentEvaluationReport) -> String {
    let metrics = &report.metrics;
    let mut document = format!("# Agent 只读回归评测报告\n\n评测会话：`{}`。生成时间：{}。运行轨迹状态：`{}`。综合判定：**{}**。\n\n## 核心指标\n\n| 指标 | 值 |\n| --- | ---: |\n| 模型调用数 | {} |\n| JSON 解析成功率 | {:.1}% |\n| JSON 解析错误 | {} |\n| 工具步骤 / 失败 | {} / {} |\n| 并行批次 / 最大分支 | {} / {} |\n| 读取意图 / 相同签名重复调用 | {} / {} |\n| Prompt Token 总量 / 峰值 | {} / {} |\n| 完成 Token 总量 | {} |\n| 缓存命中率 | {:.1}% |\n| 最大上下文使用率 | {:.1}% |\n| 最大发送上下文字符数 | {} |\n| 观测时长（秒） | {} |\n", report.session_id, report.generated_at, report.trace_status, report.verdict, metrics.llm_call_count, metrics.parse_success_rate_percent, metrics.parse_error_count, metrics.tool_step_count, metrics.tool_failure_count, metrics.parallel_batch_count, metrics.max_parallel_branches, metrics.read_intent_count, metrics.repeated_read_intent_count, metrics.total_prompt_tokens, metrics.max_prompt_tokens, metrics.total_completion_tokens, metrics.cache_hit_rate_percent, metrics.max_context_usage_percent, metrics.max_messages_char_count, metrics.observed_duration_seconds);
    document.push_str(&format!("\n> 口径：相同签名重复调用 = 完全相同的工具与参数被再次执行；同文件不同行窗口/字段级分段读取不计入（属合理取证）。运行时另有无进展提示（相同调用+相同输出第 3 次连续触发）。\n\n| 独特读取意图 / 相同签名重复调用 | {} / {} |\n| 重复失败调用 | {} |\n| 计划标题切换 | {} |\n| 未说明独立性的并行批次 | {} |\n| 资源作用域 outside / unknown / 重绑 | {} / {} / {} |\n", metrics.unique_read_intent_count, metrics.repeated_read_intent_count, metrics.repeated_failed_action_count, metrics.plan_title_transition_count, metrics.parallel_without_rationale_count, metrics.resource_scope_outside_count, metrics.resource_scope_unknown_count, metrics.scope_rebind_count));
    if let Some(routing) = &report.routing {
        document.push_str(&format!("\n评测路由：`{}`；推理模型：`{}`；执行模型：`{}`。\n", routing.preset, routing.reasoning_model_id, routing.execution_model_id));
    }
    document.push_str("\n## 与上一份评测的差异\n\n");
    if let Some(baseline) = &report.baseline {
        document.push_str(&format!("基线评测：`{}`，生成于 {}。正值表示本轮较基线增加；对解析错误、工具失败、重复读取和最大并行分支而言，负值通常更好。\n\n| 指标 | 本轮相对基线变化 |\n| --- | ---: |\n| 模型调用数 | {:+} |\n| JSON 解析错误 | {:+} |\n| 工具失败 | {:+} |\n| 相同签名重复调用 | {:+} |\n| 最大并行分支 | {:+} |\n| Prompt Token 峰值 | {:+} |\n| 上下文使用率峰值 | {:+.1}% |\n", baseline.baseline_evaluation_id.as_deref().unwrap_or("unknown"), baseline.baseline_generated_at, baseline.llm_call_delta, baseline.parse_error_delta, baseline.tool_failure_delta, baseline.repeated_read_intent_delta, baseline.max_parallel_branches_delta, baseline.max_prompt_tokens_delta, baseline.max_context_usage_percent_delta));
    } else {
        document.push_str(
            "这是首份有效评测报告，已建立比较基线。后续通过同一入口运行评测时会自动显示变化。\n",
        );
    }
    if let Some(baseline) = &report.baseline {
        document.push_str(&format!("\n### 收敛指标差异\n\n| 指标 | 本轮相对基线变化 |\n| --- | ---: |\n| 重复失败调用 | {:+} |\n| 计划标题切换 | {:+} |\n| 未说明独立性的并行批次 | {:+} |\n", baseline.repeated_failed_action_delta, baseline.plan_title_transition_delta, baseline.parallel_without_rationale_delta));
    }
    document.push_str("\n## 动作分布\n\n| 动作 | 次数 |\n| --- | ---: |\n");
    for (action, count) in &report.action_counts {
        document.push_str(&format!(
            "| `{}` | {} |\n",
            action.replace("|", "\\|"),
            count
        ));
    }
    document.push_str("\n## 重复读取意图\n\n");
    if report.repeated_action_signatures.is_empty() {
        document.push_str("未检测到相同读取意图的重复调用。必要的相邻分页仍需结合轨迹人工判断。\n");
    } else {
        for entry in &report.repeated_action_signatures {
            document.push_str(&format!(
                "- `{}`：{} 次\n",
                entry.signature.replace("`", "\\`"),
                entry.count
            ));
        }
    }
    document.push_str("\n## 需要复核的信号\n\n");
    if report.warnings.is_empty() {
        document.push_str("本次未观察到超过阈值的回归信号。该结论仅覆盖已落盘日志与运行轨迹，不代表全部运行时路径已被证明正确。\n");
    } else {
        for warning in &report.warnings {
            document.push_str(&format!("- {}\n", warning));
        }
    }
    document.push_str(&format!(
        "\n原始模型日志：`{}`。\n报告 JSON：`{}`。\n",
        report.log_path, report.report_json_path
    ));
    document
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn counts_parse_errors_parallelism_and_repeated_read_intents() {
        let logs = vec![
            json!({"parse_status": "success", "usage": {"prompt_tokens": 100, "cached_tokens": 50, "completion_tokens": 10}, "raw_response": "{\"tool\":\"filesystem\",\"action\":\"read_text_file\",\"command\":{\"path\":\"a.rs\",\"start_line\":1}}", "messages_sent_to_model": []}),
            json!({"parse_status": "parse_error", "usage": {"prompt_tokens": 120, "cached_tokens": 60, "completion_tokens": 12}, "raw_response": "{\"tool\":\"filesystem\",\"action\":\"read_text_file\",\"command\":{\"path\":\"a.rs\",\"start_line\":1}}", "messages_sent_to_model": []}),
            json!({"parse_status": "success", "usage": {"prompt_tokens": 130, "cached_tokens": 65, "completion_tokens": 15}, "raw_response": "{\"parallel\":[{\"tool\":\"filesystem\"},{\"tool\":\"filesystem\"}]}", "messages_sent_to_model": []}),
        ];
        let (metrics, _, repeated, _) = analyze(&logs, &[]);
        assert_eq!(metrics.llm_call_count, 3);
        assert_eq!(metrics.parse_error_count, 1);
        assert_eq!(metrics.parallel_batch_count, 1);
        assert_eq!(metrics.max_parallel_branches, 2);
        assert_eq!(metrics.repeated_read_intent_count, 1);
        assert_eq!(repeated.len(), 1);
        assert_eq!(metrics.unique_read_intent_count, 1);
        assert_eq!(metrics.parallel_without_rationale_count, 1);
    }

    #[test]
    fn tracks_repeated_failures_and_plan_title_transitions() {
        let logs = vec![
            json!({"parse_status": "success", "raw_response": "{\"plan_title\":\"地图\",\"tool\":\"filesystem\",\"action\":\"read_text_file\",\"command\":{\"path\":\"a.rs\"}}"}),
            json!({"parse_status": "success", "raw_response": "{\"plan_title\":\"证据链\",\"parallel\":[{\"tool\":\"filesystem\"},{\"tool\":\"filesystem\"}],\"parallel_reason\":\"两条已定位且互不依赖的验证问题\"}"}),
        ];
        let traces = vec![json!({"steps":[
            {"tool":"filesystem/read_text_file","params":{"path":"a.rs"},"status":"failed"},
            {"tool":"filesystem/read_text_file","params":{"path":"a.rs"},"status":"failed"}
        ]})];
        let (metrics, _, _, warnings) = analyze(&logs, &traces);
        assert_eq!(metrics.plan_title_transition_count, 1);
        assert_eq!(metrics.parallel_without_rationale_count, 0);
        assert_eq!(metrics.repeated_failed_action_count, 1);
        assert!(warnings.iter().any(|warning| warning.contains("相同失败调用重犯")));
    }

    #[test]
    fn deserializes_metrics_from_reports_created_before_convergence_fields() {
        let legacy_metrics = json!({
            "llm_call_count": 8,
            "parse_success_count": 7,
            "parse_error_count": 1,
            "parse_success_rate_percent": 87.5,
            "tool_step_count": 6,
            "tool_failure_count": 1,
            "tool_failure_rate_percent": 16.7,
            "parallel_batch_count": 1,
            "max_parallel_branches": 2,
            "read_intent_count": 4,
            "repeated_read_intent_count": 1,
            "max_prompt_tokens": 1200,
            "total_prompt_tokens": 5000,
            "total_completion_tokens": 800,
            "total_cached_tokens": 3000,
            "cache_hit_rate_percent": 60.0,
            "max_context_usage_percent": 12.5,
            "max_messages_char_count": 8000,
            "max_raw_response_char_count": 3000,
            "observed_duration_seconds": 90
        });
        let metrics: EvaluationMetrics = serde_json::from_value(legacy_metrics).unwrap();
        assert_eq!(metrics.unique_read_intent_count, 0);
        assert_eq!(metrics.repeated_failed_action_count, 0);
        assert_eq!(metrics.plan_title_transition_count, 0);
        assert_eq!(metrics.parallel_without_rationale_count, 0);
    }

    #[test]
    fn tracks_scope_observations_and_defaults_them_for_legacy_reports() {
        let traces = vec![json!({"steps":[
            {"tool":"filesystem/read_text_file","status":"succeeded","output":"[Resource scope]\nrelation: outside","resource_scope_revision": 2},
            {"tool":"filesystem/read_text_file","status":"succeeded","output":"[Resource scope]\nrelation: unknown","resource_scope_revision": 3}
        ]})];
        let (metrics, _, _, _) = analyze(&[], &traces);

        assert_eq!(metrics.resource_scope_outside_count, 1);
        assert_eq!(metrics.resource_scope_unknown_count, 1);
        assert_eq!(metrics.scope_rebind_count, 2);

        let legacy: EvaluationMetrics = serde_json::from_value(json!({
            "llm_call_count": 0, "parse_success_count": 0, "parse_error_count": 0,
            "parse_success_rate_percent": 0.0, "tool_step_count": 0, "tool_failure_count": 0,
            "tool_failure_rate_percent": 0.0, "parallel_batch_count": 0, "max_parallel_branches": 0,
            "read_intent_count": 0, "repeated_read_intent_count": 0, "max_prompt_tokens": 0,
            "total_prompt_tokens": 0, "total_completion_tokens": 0, "total_cached_tokens": 0,
            "cache_hit_rate_percent": 0.0, "max_context_usage_percent": 0.0,
            "max_messages_char_count": 0, "max_raw_response_char_count": 0, "observed_duration_seconds": 0
        })).unwrap();
        assert_eq!(legacy.resource_scope_outside_count, 0);
        assert_eq!(legacy.resource_scope_unknown_count, 0);
        assert_eq!(legacy.scope_rebind_count, 0);
    }
}
