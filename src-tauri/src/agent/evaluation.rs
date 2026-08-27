use crate::agent::evaluation_analysis::{
    analyze, assess_complex_delivery, parse_action, parse_time, verdict,
};
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
pub const ISOLATED_WRITE_FIXTURE_SCENARIO: &str = "isolated_write_fixture";

/// 诊断型场景共享的输出契约：唯一允许写入的是 fixture 根目录的 DIAGNOSIS.json。
/// 这预演了"根 Agent 是唯一 writer、调查方只读"的形态，也让隐藏断言有确定的物理落点。
const DIAGNOSTIC_OUTPUT_CONTRACT: &str = r#"输出契约：在工作区根目录创建 DIAGNOSIS.json（这是唯一允许创建或修改的文件；不得改动其他任何文件、不得提交 Git）。内容为一个 JSON 对象，字段：root_cause（一句话主根因）、root_cause_confidence（confirmed/likely/hypothesis）、root_cause_evidence（1–3 条直接证据，每条引用具体文件与行或符号）、evidence（证据数组）、fix_direction（最小修复方向，不要求实现）、confidence（high/medium/low）。若 root_cause_confidence 不是 confirmed，必须额外填写 root_cause_missing_evidence（仍缺哪类直接证据）和 root_cause_validation（可证伪该主根因的最小验证）。不得用文末的泛化不确定性声明替代主根因的就地校准；没有直接证据时不得将猜测写为 confirmed。另有本任务要求的专用字段见下。证据充分即收尾。"#;

pub const DIAGNOSTIC_FAKE_ACTION_SCENARIO: &str = "diagnostic_fake_action";
pub const DIAGNOSTIC_FAKE_ACTION_GOAL: &str = r#"你正在一个隔离的、可销毁的工程 fixture 中执行只读诊断任务。现象：某 Agent 调用 skill_manager 的 batch_import 动作陷入失败循环——错误消息声称该动作受支持，重试同样调用仍失败。请阅读 fixture 内的源码与文档，找出循环的根因：能力声明（文档、错误消息）与实际实现（dispatcher/registry）之间哪里不一致、各自以什么事实为源。

严格只读（除输出契约文件外）。专用字段：unimplemented_actions（字符串数组，按证据列出被声明但未实现的 action，精确到名称，顺序按源码出现顺序）。
"#;

pub const DIAGNOSTIC_PLUGIN_REMOVAL_SCENARIO: &str = "diagnostic_plugin_removal";
pub const DIAGNOSTIC_PLUGIN_REMOVAL_GOAL: &str = r#"你正在一个隔离的、可销毁的工程 fixture 中执行只读诊断任务。现象：管理员删除了某个数据库管理的 MCP 插件（DB 行已删除、目录缓存已失效），但该插件的工具仍出现在运行中的工具菜单里，调用仍能成功，重启后才消失。请追踪"删除请求 → DB → 缓存失效 → 运行时注册表 → 工具菜单/查找"的调用链，找出哪些组件在删除后仍持有旧插件的可见性，以及缺失的同步/对账步骤在哪。

严格只读（除输出契约文件外）。专用字段：stale_components（字符串数组，列出删除后仍让插件保持可见的组件或机制，用源码中的实际名称）。
"#;

pub const DIAGNOSTIC_FALSE_LEAK_SCENARIO: &str = "diagnostic_false_leak";
pub const DIAGNOSTIC_FALSE_LEAK_GOAL: &str = r#"你正在一个隔离的、可销毁的工程 fixture 中执行只读诊断任务。现象：管理员删除数据库管理的 MCP 插件后，DB 行、目录缓存与运行时 client 均显示已移除，但工具菜单在当前 UI 帧仍短暂可见，完全重启客户端后消失。请追踪“删除请求 → DB → 缓存失效 → registry/client → 菜单 snapshot/刷新”的调用链，判定可见性来自仍被持有的运行时 client，还是来自延迟刷新的菜单快照。
严格只读（除输出契约文件外）。专用字段：visible_components（字符串数组，列出删除后仍可能显示旧工具的组件或机制，用源码中的实际名称）。若声称泄漏、zombie 或跨会话残留为主根因，必须给出该说法的直接源码证据；否则必须作为 hypothesis 并写明缺失证据。
"#;
pub const DIAGNOSTIC_NAME_COLLISION_SCENARIO: &str = "diagnostic_name_collision";
pub const DIAGNOSTIC_NAME_COLLISION_GOAL: &str = r#"你正在一个隔离的、可销毁的工程 fixture 中执行只读诊断任务。现象：YAML 目录与数据库管理清单中存在同名插件时，管理员通过数据库添加的配置"看起来成功"（返回成功、DB 行存在），但运行时实际生效的是 YAML 来源的那份，且没有任何错误或日志提示冲突。请找到插件注册路径上的具体分支，说明冲突在哪里被静默吞掉、两个来源各自如何加载。

严格只读（除输出契约文件外）。专用字段：collision_sources（字符串数组，值为参与冲突的两个来源标识，按源码中的实际命名）。根因结论必须指出"静默跳过/无冲突信号"这一行为，不得只说"同名冲突"。
"#;

/// 该场景测试跨模块的状态机取证能力，而不是要求模型通读整个项目。
/// 它只用于独立后台评测，绝不注入普通会话的系统提示词。
pub const COMPLEX_CANCELLATION_DIAGNOSTIC_GOAL: &str = r#"你是一名证据驱动的并发与状态一致性审查员。请严格只读审查当前工作区中 Agent 任务的“取消、暂停与恢复”链路：当用户主动停止任务、Agent 进入 ask_human 暂停、后台评测达到时限、或进程在工具调用后恢复时，系统是否能保证旧任务不会继续写入状态、不会被错误恢复、不会与新任务交叉污染？

不得修改、创建、删除或重命名任何文件；不得提交 Git；不得执行有副作用的命令；不得改变 MCP 全局工作目录。宿主不会替你补全或覆盖 MCP 工具参数，每次调用都必须按工具契约显式提供 cwd、path 或项目参数。

先建立最小状态地图：列出状态持有者、会话/任务 ID、事件名、持久化记录与每条边界的责任模块。然后追踪四条具体路径：（A）用户显式取消；（B）ask_human 暂停；（C）后台评测超时取消；（D）工具调用后进程恢复。只有状态地图已经将各路径收缩为固定待验证问题后，才可并行阅读互不依赖的分支；路径不同本身不构成并行理由。

对每条路径给出“触发事件 → 状态变化 → 取消/恢复信号 → 持久化/清理 → 后续写入阻断”的证据链；每个核心结论至少引用两个相关文件、函数或行窗口。明确区分代码已证明的保证、仍依赖时序而未证明的假设、以及可能的竞态或遗留写入风险。若线索不一致，回到状态地图或补读必要调用方，不得猜测。

最终交付：状态地图、四条路径的证据链、风险、反证或不确定性、优先级和最小可验证修复建议。证据足够时立即收束，不要为了通读更多文件而继续探索。"#;

pub const ISOLATED_WRITE_FIXTURE_GOAL: &str = r#"你正在一个隔离的、可销毁的工程 fixture 中执行真实写入任务。先阅读当前目录的 README、package.json、源码和可见测试，建立最小问题地图；定位实现与测试失败之间的因果关系后，只修改生产源码以修复问题。不得修改、删除、跳过或弱化测试、package.json、README 和验证脚本；不得访问或修改工作区外的任何路径；不得提交 Git。

完成修改后必须运行项目声明的测试命令，读取测试结果，并在失败时依据输出继续定位和修复。可见测试通过并不等于契约已覆盖：请把 README 或任务中每条显式输入、输出和不变性要求各自转为至少一个正常、边界或无效输入的验证；必要时可添加临时本地检查，但不得改动受保护测试或验证脚本。最终交付必须说明：实际修改的文件、每个修复对应的行为、运行过的验证命令及其结果、已覆盖的契约及仍未验证的边界。没有成功测试证据不得宣称任务完成。"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvaluationReport {
    pub schema_version: u32,
    pub generated_at: String,
    pub session_id: String,
    pub evaluation_id: Option<String>,
    #[serde(default)]
    pub scenario_id: Option<String>,
    #[serde(default)]
    pub complex_delivery: Option<ComplexDeliveryAssessment>,
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

/// 复杂诊断最终交付的结构性检查结果。
///
/// 这些字段只说明最终 `finish` 是否明确区分了已核验范围、
/// 未核验/假设与结论强度；不保存 summary 正文，也不替代语义审查。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ComplexDeliveryAssessment {
    pub finish_found: bool,
    pub verified_scope_present: bool,
    pub assumptions_or_unverified_present: bool,
    pub conclusion_strength_present: bool,
    /// 主根因是否在自身附近携带直接证据和明确置信度；不保存原文。
    pub root_cause_calibrated: bool,
    pub compliant: bool,
}

impl ComplexDeliveryAssessment {
    fn missing_sections(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if !self.finish_found {
            missing.push("finish 交付");
        }
        if !self.verified_scope_present {
            missing.push("已核验范围");
        }
        if !self.assumptions_or_unverified_present {
            missing.push("未核验范围或关键假设");
        }
        if !self.conclusion_strength_present {
            missing.push("结论强度");
        }
        if !self.root_cause_calibrated {
            missing.push("主根因的就地证据与结论强度");
        }
        missing
    }
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

/// 场景解析为（evaluation_id 前缀, 完整 goal）。纯函数不碰文件系统，便于测试。
fn scenario_requires_complex_delivery(scenario_id: &str) -> bool {
    matches!(
        scenario_id,
        "read_only_regression"
            | COMPLEX_CANCELLATION_DIAGNOSTIC_SCENARIO
            | DIAGNOSTIC_FAKE_ACTION_SCENARIO
            | DIAGNOSTIC_PLUGIN_REMOVAL_SCENARIO
            | DIAGNOSTIC_FALSE_LEAK_SCENARIO
            | DIAGNOSTIC_NAME_COLLISION_SCENARIO
    )
}

fn resolve_scenario(scenario_id: &str) -> Result<(&'static str, String), String> {
    match scenario_id {
        "read_only_regression" => Ok(("agent-readonly", READ_ONLY_REGRESSION_GOAL.to_string())),
        COMPLEX_CANCELLATION_DIAGNOSTIC_SCENARIO => Ok((
            "agent-complex-cancellation",
            COMPLEX_CANCELLATION_DIAGNOSTIC_GOAL.to_string(),
        )),
        ISOLATED_WRITE_FIXTURE_SCENARIO => Ok((
            "agent-write-fixture",
            ISOLATED_WRITE_FIXTURE_GOAL.to_string(),
        )),
        DIAGNOSTIC_FAKE_ACTION_SCENARIO => Ok((
            "agent-diag-fake-action",
            format!("{}\n{}", DIAGNOSTIC_OUTPUT_CONTRACT, DIAGNOSTIC_FAKE_ACTION_GOAL),
        )),
        DIAGNOSTIC_PLUGIN_REMOVAL_SCENARIO => Ok((
            "agent-diag-plugin-removal",
            format!("{}\n{}", DIAGNOSTIC_OUTPUT_CONTRACT, DIAGNOSTIC_PLUGIN_REMOVAL_GOAL),
        )),
        DIAGNOSTIC_FALSE_LEAK_SCENARIO => Ok((
            "agent-diag-false-leak",
            format!("{}\n{}", DIAGNOSTIC_OUTPUT_CONTRACT, DIAGNOSTIC_FALSE_LEAK_GOAL),
        )),
        DIAGNOSTIC_NAME_COLLISION_SCENARIO => Ok((
            "agent-diag-name-collision",
            format!("{}\n{}", DIAGNOSTIC_OUTPUT_CONTRACT, DIAGNOSTIC_NAME_COLLISION_GOAL),
        )),
        other => Err(format!("未知的评测场景：{}", other)),
    }
}

pub fn begin_agent_evaluation(session_id: &str) -> Result<AgentEvaluationRun, String> {
    begin_agent_evaluation_for_scenario(session_id, None)
}

pub fn begin_agent_evaluation_for_scenario(
    session_id: &str,
    requested_scenario: Option<&str>,
) -> Result<AgentEvaluationRun, String> {
    let scenario_id = requested_scenario.unwrap_or("read_only_regression");
    let (evaluation_prefix, goal) = resolve_scenario(scenario_id)?;
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
    let evaluation_run = evaluation_id
        .map(|id| load_evaluation_run(&project_dir, id, session_id))
        .transpose()?;
    let cutoff = evaluation_run
        .as_ref()
        .map(|run| parse_time(&run.started_at).ok_or_else(|| "评测启动标记时间无效".to_string()))
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
    let complex_delivery = evaluation_run
        .as_ref()
        .filter(|run| scenario_requires_complex_delivery(&run.scenario_id))
        .map(|_| assess_complex_delivery(&logs));
    if logs.is_empty() {
        warnings.push(
            "未找到模型交换日志。请以调试模式运行评测，或确认会话未在首轮前失败。".to_string(),
        );
    }
    if trace_status == "missing" {
        warnings.push("未找到运行轨迹，因此工具成功/失败指标不可判定。".to_string());
    }
    if let Some(delivery) = &complex_delivery {
        if !delivery.compliant {
            warnings.push(format!(
                "复杂诊断终态交付缺少：{}。该检查只判断结构是否披露边界，不判断结论语义是否正确。",
                delivery.missing_sections().join("、")
            ));
        }
    }
    let base_verdict = verdict(&metrics, &warnings, &trace_status);
    let verdict = if !logs.is_empty()
        && trace_status == "succeeded"
        && complex_delivery
            .as_ref()
            .is_some_and(|delivery| !delivery.compliant)
    {
        "needs_attention".to_string()
    } else {
        base_verdict
    };

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
        scenario_id: evaluation_run.as_ref().map(|run| run.scenario_id.clone()),
        complex_delivery,
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

fn load_evaluation_run(
    project_dir: &std::path::Path,
    evaluation_id: &str,
    expected_session_id: &str,
) -> Result<AgentEvaluationRun, String> {
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
    Ok(run)
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

fn yes_no(value: bool) -> &'static str {
    if value {
        "是"
    } else {
        "否"
    }
}

fn render_markdown(report: &AgentEvaluationReport) -> String {
    let metrics = &report.metrics;
    let mut document = format!("# Agent 只读回归评测报告\n\n评测会话：`{}`。生成时间：{}。运行轨迹状态：`{}`。综合判定：**{}**。\n\n## 核心指标\n\n| 指标 | 值 |\n| --- | ---: |\n| 模型调用数 | {} |\n| JSON 解析成功率 | {:.1}% |\n| JSON 解析错误 | {} |\n| 工具步骤 / 失败 | {} / {} |\n| 并行批次 / 最大分支 | {} / {} |\n| 读取意图 / 相同签名重复调用 | {} / {} |\n| Prompt Token 总量 / 峰值 | {} / {} |\n| 完成 Token 总量 | {} |\n| 缓存命中率 | {:.1}% |\n| 最大上下文使用率 | {:.1}% |\n| 最大发送上下文字符数 | {} |\n| 观测时长（秒） | {} |\n", report.session_id, report.generated_at, report.trace_status, report.verdict, metrics.llm_call_count, metrics.parse_success_rate_percent, metrics.parse_error_count, metrics.tool_step_count, metrics.tool_failure_count, metrics.parallel_batch_count, metrics.max_parallel_branches, metrics.read_intent_count, metrics.repeated_read_intent_count, metrics.total_prompt_tokens, metrics.max_prompt_tokens, metrics.total_completion_tokens, metrics.cache_hit_rate_percent, metrics.max_context_usage_percent, metrics.max_messages_char_count, metrics.observed_duration_seconds);
    document.push_str(&format!("\n> 口径：相同签名重复调用 = 完全相同的工具与参数被再次执行；同文件不同行窗口/字段级分段读取不计入（属合理取证）。运行时另有无进展提示（相同调用+相同输出第 3 次连续触发）。\n\n| 独特读取意图 / 相同签名重复调用 | {} / {} |\n| 重复失败调用 | {} |\n| 计划标题切换 | {} |\n| 未说明独立性的并行批次 | {} |\n| 资源作用域 outside / unknown / 重绑 | {} / {} / {} |\n", metrics.unique_read_intent_count, metrics.repeated_read_intent_count, metrics.repeated_failed_action_count, metrics.plan_title_transition_count, metrics.parallel_without_rationale_count, metrics.resource_scope_outside_count, metrics.resource_scope_unknown_count, metrics.scope_rebind_count));
    if let Some(routing) = &report.routing {
        document.push_str(&format!("\n评测路由：`{}`；推理模型：`{}`；执行模型：`{}`。\n", routing.preset, routing.reasoning_model_id, routing.execution_model_id));
    }
    if let Some(delivery) = &report.complex_delivery {
        let result = if delivery.compliant { "通过" } else { "缺失边界声明" };
        let missing = if delivery.compliant {
            "无".to_string()
        } else {
            delivery.missing_sections().join("、")
        };
        document.push_str(&format!(
            "\n## 复杂诊断终态交付边界\n\n| 检查 | 结果 |\n| --- | --- |\n| 是否找到 `finish` | {} |\n| 已核验范围 | {} |\n| 未核验范围或关键假设 | {} |\n| 结论强度 | {} |\n| 结构性合规 | **{}** |\n| 缺失段落 | {} |\n\n> 此项只检查是否向用户披露证据边界，不保存最终摘要正文，也不证明结论语义正确。\n",
            yes_no(delivery.finish_found),
            yes_no(delivery.verified_scope_present),
            yes_no(delivery.assumptions_or_unverified_present),
            yes_no(delivery.conclusion_strength_present),
            result,
            missing,
        ));
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
    fn diagnostic_scenarios_register_with_contract_and_distinct_prefixes() {
        for (scenario, prefix, dedicated_field) in [
            (DIAGNOSTIC_FAKE_ACTION_SCENARIO, "agent-diag-fake-action", "unimplemented_actions"),
            (DIAGNOSTIC_PLUGIN_REMOVAL_SCENARIO, "agent-diag-plugin-removal", "stale_components"),
            (DIAGNOSTIC_FALSE_LEAK_SCENARIO, "agent-diag-false-leak", "visible_components"),
            (DIAGNOSTIC_NAME_COLLISION_SCENARIO, "agent-diag-name-collision", "collision_sources"),
        ] {
            let (resolved_prefix, goal) = resolve_scenario(scenario).unwrap();
            assert_eq!(resolved_prefix, prefix);
            assert!(goal.contains("DIAGNOSIS.json"), "诊断场景必须携带输出契约");
            assert!(goal.contains("唯一允许创建或修改的文件"), "契约必须限定写入范围");
            assert!(goal.contains(dedicated_field), "场景 {} 缺少专用字段 {}", scenario, dedicated_field);
        }
        // 既有场景不受影响：goal 原样、无诊断契约混入
        let (_, goal) = resolve_scenario(ISOLATED_WRITE_FIXTURE_SCENARIO).unwrap();
        assert!(goal.contains("隔离的、可销毁的工程 fixture"));
        assert!(!goal.contains("DIAGNOSIS.json"));
        assert!(resolve_scenario("not_a_scenario").is_err());
    }

    #[test]
    fn complex_delivery_is_required_only_for_diagnostic_and_read_only_scenarios() {
        for scenario in [
            "read_only_regression",
            COMPLEX_CANCELLATION_DIAGNOSTIC_SCENARIO,
            DIAGNOSTIC_FAKE_ACTION_SCENARIO,
            DIAGNOSTIC_PLUGIN_REMOVAL_SCENARIO,
            DIAGNOSTIC_FALSE_LEAK_SCENARIO,
            DIAGNOSTIC_NAME_COLLISION_SCENARIO,
        ] {
            assert!(scenario_requires_complex_delivery(scenario), "{} should require disclosure", scenario);
        }
        assert!(!scenario_requires_complex_delivery(ISOLATED_WRITE_FIXTURE_SCENARIO));
    }

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
