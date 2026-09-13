//! 方案审批结论解析与前置写入门禁。
//! 从 loop_runner.rs 原样抽出，行为不变；loop_runner 通过 re-export 维持原引用路径。

use crate::agent::checkpoint::{AgentCheckpoint, PlanExecutionMode, PlanPhase};
use crate::agent::child_reviewer::{has_pending_review_results, has_running_child_runs};
use crate::agent::gate_messages;
use crate::agent::types::DispatchResult;
use crate::db::DbState;

/// 前端下发方案审批结论时使用的结构化标记，形如 `<!--plan-decision:{"approved":true}-->`。
/// 采用 HTML 注释形态，既不会干扰 Markdown 方案正文的渲染，也不会被模型当作指令文本。
const PLAN_DECISION_MARKER_PREFIX: &str = "<!--plan-decision:";
const PLAN_DECISION_MARKER_SUFFIX: &str = "-->";

const DEFAULT_CROSS_REVIEW_EXPLORATION_MAX_STEPS: usize = 24;
const DEFAULT_CROSS_REVIEW_EXPLORATION_SOFT_STEPS: usize = 14;
const DEFAULT_CROSS_REVIEW_EXPLORATION_EXTENSION_STEPS: u32 = 8;
const DEFAULT_CROSS_REVIEW_EXPLORATION_MAX_EXTENSIONS: u32 = 2;
const DEFAULT_CROSS_REVIEW_EXPLORATION_ABSOLUTE_STEPS: usize = 48;

/// 探索预算配置：全部环境变量可调，默认值仅为起始点，后续按历史轨迹调优。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExplorationBudgetConfig {
    /// 软提醒阈值：到达后状态卡转为收敛指引，不拦截
    pub soft_steps: usize,
    /// 初始硬上限：到达后普通操作被门禁拒绝
    pub base_steps: usize,
    /// 单次延期最多可追加的步数
    pub per_extension_steps: u32,
    /// 延期申请次数上限
    pub max_extensions: u32,
    /// 无论如何不可突破的绝对上限
    pub absolute_steps: usize,
}

impl ExplorationBudgetConfig {
    pub(crate) fn from_env() -> Self {
        Self {
            soft_steps: cross_review_exploration_soft_steps(),
            base_steps: cross_review_exploration_max_steps(),
            per_extension_steps: cross_review_exploration_extension_steps(),
            max_extensions: cross_review_exploration_max_extensions(),
            absolute_steps: cross_review_exploration_absolute_steps(),
        }
    }

    /// 有效硬上限 = min(基础上限 + 已批延期步数, 绝对上限)
    pub(crate) fn effective_limit(&self, granted_steps: u32) -> usize {
        self.base_steps
            .saturating_add(granted_steps as usize)
            .min(self.absolute_steps)
    }
}

pub(crate) fn cross_review_exploration_max_steps() -> usize {
    std::env::var("FREE_API_CROSS_REVIEW_EXPLORATION_MAX_STEPS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .map(|value| value.clamp(8, 100))
        .unwrap_or(DEFAULT_CROSS_REVIEW_EXPLORATION_MAX_STEPS)
}

pub(crate) fn cross_review_exploration_soft_steps() -> usize {
    std::env::var("FREE_API_CROSS_REVIEW_EXPLORATION_SOFT_STEPS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .map(|value| value.clamp(4, 80))
        .unwrap_or(DEFAULT_CROSS_REVIEW_EXPLORATION_SOFT_STEPS)
}

pub(crate) fn cross_review_exploration_extension_steps() -> u32 {
    std::env::var("FREE_API_CROSS_REVIEW_EXPLORATION_EXTENSION_STEPS")
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .map(|value| value.clamp(1, 32))
        .unwrap_or(DEFAULT_CROSS_REVIEW_EXPLORATION_EXTENSION_STEPS)
}

pub(crate) fn cross_review_exploration_max_extensions() -> u32 {
    std::env::var("FREE_API_CROSS_REVIEW_EXPLORATION_MAX_EXTENSIONS")
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .map(|value| value.clamp(0, 8))
        .unwrap_or(DEFAULT_CROSS_REVIEW_EXPLORATION_MAX_EXTENSIONS)
}

pub(crate) fn cross_review_exploration_absolute_steps() -> usize {
    std::env::var("FREE_API_CROSS_REVIEW_EXPLORATION_ABSOLUTE_STEPS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .map(|value| value.clamp(16, 200))
        .unwrap_or(DEFAULT_CROSS_REVIEW_EXPLORATION_ABSOLUTE_STEPS)
}

/// 当前有效探索硬上限（基础上限 + 已批延期，受绝对上限封顶）
pub(crate) fn cross_review_exploration_effective_limit(checkpoint: &AgentCheckpoint) -> usize {
    ExplorationBudgetConfig::from_env()
        .effective_limit(checkpoint.plan_exploration_granted_steps)
}

/// 剩余可申请的延期次数
pub(crate) fn cross_review_exploration_extensions_left(checkpoint: &AgentCheckpoint) -> u32 {
    cross_review_exploration_max_extensions()
        .saturating_sub(checkpoint.plan_exploration_extensions_used)
}

/// 探索延期裁决结果： granted 表示已入账并抬升有效上限；
/// message 为面向模型的裁决说明（注入上下文或作为纯申请的本步结果）。
#[derive(Debug)]
pub(crate) struct ExtensionVerdict {
    pub granted: bool,
    pub granted_steps: u32,
    pub message: String,
    pub route: &'static str,
}

/// 裁决模型的探索延期申请（生产入口：预算配置取自环境变量）。
pub(crate) fn adjudicate_exploration_extension(
    checkpoint: &mut AgentCheckpoint,
    request: &crate::agent::types::ExplorationExtensionRequest,
) -> ExtensionVerdict {
    adjudicate_exploration_extension_with(
        checkpoint,
        request,
        ExplorationBudgetConfig::from_env(),
    )
}

/// 探索延期裁决（显式配置版，供测试注入确定性预算）。
///
/// 判定顺序：
/// 1. 适用性——仅交叉审查 + Exploring + 尚未派发过首次审查；
/// 2. 结构——unresolved_question 非空、next_queries 为 1~6 条非空取证项；
/// 3. 预算——延期次数与绝对上限余量；
/// 全部通过后一次性入账：extensions_used +1、granted_steps += grant。
/// 除入账两个字段外不触碰其他状态：模型仍须自己决定何时派发审查。
pub(crate) fn adjudicate_exploration_extension_with(
    checkpoint: &mut AgentCheckpoint,
    request: &crate::agent::types::ExplorationExtensionRequest,
    cfg: ExplorationBudgetConfig,
) -> ExtensionVerdict {
    let applicable = checkpoint.plan_mode == PlanExecutionMode::CrossReview
        && checkpoint.plan_phase == PlanPhase::Exploring
        && checkpoint.plan_reviewer_retry_count == 0;
    if !applicable {
        return ExtensionVerdict {
            granted: false,
            granted_steps: 0,
            message: gate_messages::exploration_extension_not_applicable(),
            route: "plan_liveness:extension_not_applicable",
        };
    }

    let extensions_left = cfg
        .max_extensions
        .saturating_sub(checkpoint.plan_exploration_extensions_used);
    let deny = |reason: &str| ExtensionVerdict {
        granted: false,
        granted_steps: 0,
        message: gate_messages::exploration_extension_denied(reason, extensions_left),
        route: "plan_liveness:extension_denied",
    };

    if request.unresolved_question.trim().is_empty() {
        return deny("unresolved_question 不能为空：必须明确还缺哪条关键证据、它将如何改变方案。");
    }
    let concrete_queries = request
        .next_queries
        .iter()
        .filter(|q| !q.trim().is_empty())
        .count();
    if concrete_queries == 0 || concrete_queries > 6 {
        return deny("next_queries 必须包含 1~6 条非空的具体取证项，防止无边界续探。");
    }

    let effective = cfg.effective_limit(checkpoint.plan_exploration_granted_steps);
    let room = cfg.absolute_steps.saturating_sub(effective) as u32;
    if extensions_left == 0 {
        return deny("延期申请次数已用尽。");
    }
    if room == 0 {
        return deny("已到达探索绝对上限，无法追加任何步骤。");
    }

    let grant = request
        .extra_steps
        .unwrap_or(cfg.per_extension_steps)
        .clamp(1, cfg.per_extension_steps)
        .min(room);
    checkpoint.plan_exploration_extensions_used += 1;
    checkpoint.plan_exploration_granted_steps += grant;
    let new_limit = cfg.effective_limit(checkpoint.plan_exploration_granted_steps);
    ExtensionVerdict {
        granted: true,
        granted_steps: grant,
        message: gate_messages::exploration_extension_granted(
            grant,
            new_limit,
            extensions_left - 1,
        ),
        route: "plan_liveness:extension_granted",
    }
}

/// submit_plan 适用性预检（纯 checkpoint 判定，不含 DB 在途行检查）。
/// 返回 Some(reason) 表示当前状态不可用提交；None 放行。
/// 覆盖：模式/阶段/升级挂起/未闭合审查轮次/预算上限。
pub(crate) fn submit_plan_rejection(checkpoint: &AgentCheckpoint) -> Option<String> {
    if checkpoint.plan_mode != PlanExecutionMode::CrossReview {
        return Some(format!(
            "【ℹ️ submit_plan 不适用】当前执行模式为 {:?}，方案提交通道仅在交叉审查模式（cross_review）下有效。\n\
             全自动模式可直接在排查后写入；方案审批模式请调用 ask_human(kind: plan_approval) 呈报方案。",
            checkpoint.plan_mode
        ));
    }
    if checkpoint.plan_phase.is_approved() {
        return Some(
            "【ℹ️ submit_plan 不适用】方案已获放行，无需再次提交。请按已批准方案执行写入与验证。"
                .to_string(),
        );
    }
    if let Some(ref roles) = checkpoint.plan_escalation_required {
        return Some(format!(
            "【⚠️ submit_plan 已拒绝】Tech Lead 已裁决须升级多专家联合会审（建议角色：{}）。\n\
             请改用 child_reviewer 通过 dispatches 数组并发派发上述专家；submit_plan 的 1v1 单审通道不再适用。",
            if roles.is_empty() {
                "architect / security_specialist 等".to_string()
            } else {
                roles.join(", ")
            }
        ));
    }
    // 已预扣预算但结论未消费 = 审查轮次仍开着，重复提交只会制造无人认领的审查任务
    if checkpoint.plan_reviewer_retry_count > 0 && !checkpoint.plan_reviewer_consumed {
        return Some(
            "【⚠️ submit_plan 已拒绝】已有方案审查任务在途或结论尚未回收（问过 ≠ 听过）。\n\
             请先调用 wait_jobs 收取并研判审查结论；确认需要新一轮评审时再重新提交 submit_plan。"
                .to_string(),
        );
    }
    if checkpoint.plan_reviewer_retry_count >= crate::agent::checkpoint::max_plan_reviewer_rounds()
    {
        return Some(gate_messages::reviewer_retry_limit());
    }
    None
}

/// submit_plan 结构校验：方案必须带来源证据，防止空想方案进入审查。
/// 成功返回 (规范化 target_files, 去重 evidence_refs)。
pub(crate) fn validate_plan_submission(
    submission: &crate::agent::types::PlanSubmission,
) -> Result<(Vec<String>, Vec<String>), String> {
    if submission.plan_summary.trim().chars().count() < 10 {
        return Err(
            "【⚠️ submit_plan 结构不合法】plan_summary 不能为空且至少 10 个字符：请概述根因、改动方式与实施步骤。"
                .to_string(),
        );
    }
    let target_files: Vec<String> = submission
        .target_files
        .iter()
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .collect();
    if target_files.is_empty() || target_files.len() > 50 {
        return Err(
            "【⚠️ submit_plan 结构不合法】target_files 必须包含 1~50 条非空文件路径。".to_string(),
        );
    }
    let mut evidence_refs: Vec<String> = submission
        .evidence_refs
        .iter()
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .collect();
    evidence_refs.dedup();
    if evidence_refs.is_empty() || evidence_refs.len() > 6 {
        return Err(
            "【⚠️ submit_plan 结构不合法】evidence_refs 必须包含 1~6 条你已实际读取过的证据引用（文件路径或证据标识）；空手提交方案没有审查意义。"
                .to_string(),
        );
    }
    Ok((target_files, evidence_refs))
}

/// 探索活性门禁：交叉审查 + Exploring + 尚未派审 时，超过有效硬上限的探索被拒绝。
/// effective_limit 由调用方按「基础 + 已批延期（封顶绝对上限）」计算后传入，
/// 使本函数保持纯判定、可用任意预算做确定性测试。
pub(crate) fn check_cross_review_exploration_gate(
    checkpoint: &AgentCheckpoint,
    step_id: usize,
    tool: &str,
    action: &str,
    effective_limit: usize,
) -> Option<DispatchResult> {
    let reviewer_dispatch = tool.eq_ignore_ascii_case("child_reviewer")
        || action.eq_ignore_ascii_case("child_reviewer");
    let human_escalation =
        tool.eq_ignore_ascii_case("ask_human") || action.eq_ignore_ascii_case("ask");
    if checkpoint.plan_mode != PlanExecutionMode::CrossReview
        || checkpoint.plan_phase != PlanPhase::Exploring
        || checkpoint.plan_reviewer_retry_count > 0
        || step_id < effective_limit
        || reviewer_dispatch
        || human_escalation
    {
        return None;
    }
    Some(DispatchResult {
        success: false,
        stdout: String::new(),
        stderr: gate_messages::cross_review_exploration_budget_exhausted(
            effective_limit,
            cross_review_exploration_extensions_left(checkpoint),
        ),
        route: "plan_gatekeeper:exploration_budget_exhausted".to_string(),
    })
}

/// 方案审批的结构化结论（标记载荷的类型化契约）。
///
/// 与前端 `AgentHumanInterventionCard` / 移动端 app.js 的下发格式保持一致；
/// 结构变化必须双向同步，后端解析用本结构而非裸 JSON Value，避免字段拼写漂移静默失效。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct PlanDecision {
    /// true = 批准放行；false = 打回重新推演
    pub approved: bool,
    /// 打回时用户附带的修改意见（可选）
    #[serde(default)]
    pub note: Option<String>,
}

/// 从结构化标记中提取类型化的审批结论。
pub(crate) fn extract_plan_decision(reply: &str) -> Option<PlanDecision> {
    let start = reply.find(PLAN_DECISION_MARKER_PREFIX)?;
    let rest = &reply[start + PLAN_DECISION_MARKER_PREFIX.len()..];
    let end = rest.find(PLAN_DECISION_MARKER_SUFFIX)?;
    serde_json::from_str::<PlanDecision>(rest[..end].trim()).ok()
}

/// 剥离回复末尾的结构化审批标记，用于日志展示与持久化摘要。
pub(crate) fn strip_plan_decision_marker(reply: &str) -> String {
    match reply.find(PLAN_DECISION_MARKER_PREFIX) {
        Some(start) => {
            let head = &reply[..start];
            let rest = &reply[start + PLAN_DECISION_MARKER_PREFIX.len()..];
            match rest.find(PLAN_DECISION_MARKER_SUFFIX) {
                // 标记之后若还有正文（理论上不应出现），保留尾部内容
                Some(end) => format!("{}{}", head.trim_end(), rest[end + PLAN_DECISION_MARKER_SUFFIX.len()..].trim_end()),
                None => reply.to_string(),
            }
        }
        None => reply.to_string(),
    }
}

/// 从人类回复中解析方案审批结论。
///
/// 返回 `Some(true)` = 批准放行；`Some(false)` = 打回重新推演；`None` = 无法判定。
///
/// 判定顺序：
/// 1. 优先解析前端下发的结构化标记，这是权威来源，**不受用户自然语言内容干扰**；
/// 2. 无标记时（CLI / 直接调用等非 UI 通道）回退到关键词嗅探；
/// 3. 仍无法判定时返回 `None`，由调用方保持门禁关闭，绝不默认放行。
pub(crate) fn parse_plan_approval_decision(reply: &str) -> Option<bool> {
    // 1. 结构化标记：权威来源。即便用户补充意见里写了"打回修改"等字样也不会误判。
    if let Some(decision) = extract_plan_decision(reply) {
        return Some(decision.approved);
    }

    // 2. 回退：否定信号优先于肯定信号，避免"我不太同意但先按这个来"被判为批准
    let negative = [
        "【方案打回修改】", "打回修改", "\"approved\":false", "\"confirm\":false",
        "打回", "不同意", "拒绝", "不行", "别写", "不要写", "重新推演", "重新审查", "暂不批准",
    ];
    if negative.iter().any(|m| reply.contains(m)) {
        return Some(false);
    }
    let positive = [
        "【方案已批准】", "批准方案", "同意该方案", "方案通过",
        "批准", "同意", "通过", "好的", "好", "可以", "行", "没问题", "继续", "开始", "开始执行",
        "ok", "yes", "approve", "go", "proceed",
    ];
    let reply_lower = reply.to_ascii_lowercase();
    if positive.iter().any(|m| reply_lower.contains(m)) {
        return Some(true);
    }

    // 3. 无法判定：交由调用方保持门禁关闭
    None
}

/// 统合前置写入门禁判定（支持全自动、双 Agent 交叉审查、人工方案审批三种模式）
/// - 若放行返回 None，并适时将 plan_phase 置为 Approved；
/// - 若拦截返回 Some(DispatchResult)。
pub(crate) fn check_pre_write_gatekeeper(
    checkpoint: &mut AgentCheckpoint,
    context: &crate::agent::context::SandwichContext,
    db: &DbState,
    parent_run_id: &str,
    step_id: usize,
    is_resume_child: bool,
) -> Option<DispatchResult> {
    // ---- 阶段级审查拦截（独立于方案门禁：Approved 后的写入也要过阶段审查这一关）----
    // blocking_regardless 未解决 → 拦截写入；解套唯一路径是 resume_child 对质撤回。
    // final_only 不在此拦截，由收尾审查兜底。
    if checkpoint.stage_review_blockers_open > 0 {
        let stage_run_id = checkpoint
            .stage_review_run_ids
            .first()
            .cloned()
            .or_else(|| checkpoint.stage_review_run_id.clone())
            .or_else(|| checkpoint.stage_review_rebuttal_run_id.clone())
            .unwrap_or_default();
        let final_only_summary = checkpoint.stage_review_final_only.join("\n");
        return Some(DispatchResult {
            success: false,
            stdout: String::new(),
            stderr: crate::agent::gate_messages::stage_review_blocking_open(
                checkpoint.stage_review_blockers_open,
                &stage_run_id,
                &final_only_summary,
            ),
            route: "stage_review:blocking_open".to_string(),
        });
    }

    if checkpoint.plan_phase.is_approved() && checkpoint.stage_review_blockers_open == 0 {
        checkpoint.recent_gate_block_step = None;
        return None;
    }

    let interception = gate_interception(checkpoint, context, db, parent_run_id, step_id, is_resume_child);
    match &interception {
        Some(result) => {
            // 记录拦截步（ask_human 冷却依据）；预算耗尽这类"指示去求助"的拦截豁免冷却
            checkpoint.recent_gate_block_step = Some(step_id as u64);
            if result.route == "plan_gatekeeper:reviewer_retry_limit"
                || result.route == "plan_gatekeeper:scorecard_failed"
                || result.route == "plan_gatekeeper:blocking_findings_open"
                || result.route == "plan_gatekeeper:scorecard_missing"
                || result.route == "plan_gatekeeper:need_human_approval"
                || result.route == "plan_gatekeeper:escalation_required"
                || result.route == "plan_gatekeeper:reviewer_all_failed"
            {
                // 只要门禁文案明确指示去 ask_human 申请人工审批，就豁免冷却并标记由门禁引导仲裁
                checkpoint.gate_human_escape_directed = true;
            }
        }
        None => {
            checkpoint.recent_gate_block_step = None;
            checkpoint.gate_human_escape_directed = false;
        }
    }
    interception
}

fn gate_interception(
    checkpoint: &mut AgentCheckpoint,
    context: &crate::agent::context::SandwichContext,
    db: &DbState,
    parent_run_id: &str,
    step_id: usize,
    is_resume_child: bool,
) -> Option<DispatchResult> {
    match checkpoint.plan_mode {
        crate::agent::checkpoint::PlanExecutionMode::PlanApproval => {
            let stderr = if is_resume_child {
                crate::agent::gate_messages::plan_approval_resume_child_blocked()
            } else {
                crate::agent::gate_messages::plan_approval_need_human_approval()
            };
            Some(DispatchResult {
                success: false,
                stdout: String::new(),
                stderr,
                route: "plan_gatekeeper:need_human_approval".to_string(),
            })
        }
        crate::agent::checkpoint::PlanExecutionMode::CrossReview => {
            // 门禁判定前先巡检僵尸 running 行，防止 panic/悬挂的子任务把门禁永久锁死
            crate::agent::child_reviewer::reap_stale_child_runs(db, parent_run_id);
            // 先探测后台是否仍有审查任务在跑：用于区分「结论还在路上」与「派发压根没产生任务」。
            // 若不区分，后者会让模型只会反复调用 wait_jobs 收空信箱，而 consumed 永远置不上位，写入被无限拦截。
            let reviewer_running =
                crate::agent::child_reviewer::has_running_child_runs(db, parent_run_id);
            let is_batch_incomplete = !checkpoint.plan_review_batch.is_complete()
                && !checkpoint.plan_review_batch.expected_run_ids.is_empty();

            if checkpoint.plan_reviewer_retry_count == 0 {
                Some(DispatchResult {
                    success: false,
                    stdout: String::new(),
                    stderr: crate::agent::gate_messages::cross_review_need_reviewer_dispatch(),
                    route: "plan_gatekeeper:need_reviewer_dispatch".to_string(),
                })
            } else if reviewer_running
                || is_batch_incomplete
                || (!checkpoint.plan_reviewer_consumed
                    && crate::agent::child_reviewer::has_pending_review_results(db, parent_run_id))
            {
                let stderr = if is_batch_incomplete {
                    crate::agent::gate_messages::cross_review_batch_incomplete(
                        checkpoint.plan_review_batch.delivered_run_ids.len(),
                        checkpoint.plan_review_batch.expected_run_ids.len(),
                    )
                } else {
                    crate::agent::gate_messages::cross_review_need_wait_reviewer_jobs()
                };
                Some(DispatchResult {
                    success: false,
                    stdout: String::new(),
                    stderr,
                    route: "plan_gatekeeper:need_wait_reviewer_jobs".to_string(),
                })
            } else if !checkpoint.plan_reviewer_consumed {
                // 已消耗过审查轮次，但后台既没有在跑的任务、也没有待回收的结论 ——
                // 说明上一次派发并未真正启动（角色卡未授权/不存在、参数非法等）。
                // 此时必须引导模型重新派发，否则它会一直 wait_jobs 收空信箱而永久无法写入。
                Some(DispatchResult {
                    success: false,
                    stdout: String::new(),
                    stderr: crate::agent::gate_messages::cross_review_reviewer_result_missing(),
                    route: "plan_gatekeeper:reviewer_result_missing".to_string(),
                })
            } else if checkpoint.plan_review_batch.deliveries.is_empty()
                && !checkpoint.plan_review_batch.failed_run_ids.is_empty()
            {
                let budget_exhausted = checkpoint.plan_reviewer_retry_count
                    >= crate::agent::checkpoint::max_plan_reviewer_rounds();
                let reasons: Vec<String> = checkpoint
                    .plan_review_batch
                    .failed_run_ids
                    .iter()
                    .map(|(id, r)| format!("- 审查任务 `{}`: {}", id, r))
                    .collect();
                let summary = reasons.join("\n");
                Some(DispatchResult {
                    success: false,
                    stdout: String::new(),
                    stderr: crate::agent::gate_messages::cross_review_batch_all_failed(&summary, budget_exhausted),
                    route: "plan_gatekeeper:reviewer_all_failed".to_string(),
                })
            } else if let Some(ref req_roles) = checkpoint.plan_escalation_required {
                let role_list = if req_roles.is_empty() {
                    "多位专家 (如 architect, security_specialist)".to_string()
                } else {
                    req_roles.join(", ")
                };
                let budget_exhausted = checkpoint.plan_reviewer_retry_count
                    >= crate::agent::checkpoint::max_plan_reviewer_rounds();
                Some(DispatchResult {
                    success: false,
                    stdout: String::new(),
                    stderr: format!(
                        "【⚠️ 方案门禁拦截：Tech Lead 方案评审判定必须升级多专家联合会审】\n\
                         【原因】1v1 方案评审认为当前改动跨模块关联度高或涉及高危架构设计，单人无法完全放行。\n\
                         【必须执行的操作】：\n\
                         请调用 child_reviewer 工具（通过 dispatches 数组），并发派发多位专家（建议角色：{}）开展联合会审！\n\
                         随后调用 wait_jobs 等待齐套结论，会审通过后方可动笔写入！{}",
                        role_list,
                        if budget_exhausted { "\n【提示】若审查预算耗尽，可调用 ask_human 申请人工审批放行。" } else { "" }
                    ),
                    route: "plan_gatekeeper:escalation_required".to_string(),
                })
            } else if let Some(ref scorecard) = checkpoint.last_review_scorecard {
                let budget_exhausted = checkpoint.plan_reviewer_retry_count
                    >= crate::agent::checkpoint::max_plan_reviewer_rounds();
                let unwithdrawn_blockings = checkpoint.plan_review_batch.aggregate_unwithdrawn_blockings();
                let blocking_count = if unwithdrawn_blockings.is_empty() {
                    checkpoint.last_review_blocking_count
                } else {
                    unwithdrawn_blockings.len() as u32
                };

                if blocking_count > 0 {
                    // 🌟 层次 3 核心：门禁以 blocking 阻塞缺陷为首要依据，存在未闭合 blocking 则必须解决（附带详细清单与归属）
                    let mut details = String::new();
                    for (i, b) in unwithdrawn_blockings.iter().enumerate() {
                        details.push_str(&format!(
                            "{}. [{}] {}: {}\n   💡 建议: {}\n",
                            i + 1,
                            b.id,
                            if b.location.is_empty() { "全局" } else { &b.location },
                            b.why,
                            if b.suggestion.is_empty() { "请修复该致命设计隐患" } else { &b.suggestion }
                        ));
                    }
                    Some(DispatchResult {
                        success: false,
                        stdout: String::new(),
                        stderr: crate::agent::gate_messages::cross_review_blocking_findings_with_details(
                            blocking_count,
                            &details,
                            budget_exhausted,
                        ),
                        route: "plan_gatekeeper:blocking_findings_open".to_string(),
                    })
                } else if scorecard.is_abnormally_low() {
                    // 弱作用兜底：无 blocking 缺陷但评分异常偏低（正确性或安全性得分 < 60 分），防审查员放水，转人工核验
                    let reason = scorecard.gate_rejection_reason().unwrap_or_else(|| "审查评分异常偏低（正确性或安全性得分 < 60 分），存在放水嫌疑，需人工核验".to_string());
                    Some(DispatchResult {
                        success: false,
                        stdout: String::new(),
                        stderr: crate::agent::gate_messages::cross_review_scorecard_failed(&reason, budget_exhausted),
                        route: "plan_gatekeeper:scorecard_failed".to_string(),
                    })
                } else {
                    if checkpoint.approved_target_files.is_none() {
                        let mut target_files = Vec::new();
                        if let Some(ref inst) = checkpoint.prepared_instruction {
                            let params = inst.get_params();
                            if let Some(p) = params.get("path")
                                .or_else(|| params.get("file_path"))
                                .or_else(|| params.get("target"))
                                .and_then(|v| v.as_str())
                            {
                                target_files.push(p.to_string());
                            }
                        }
                        if !target_files.is_empty() {
                            checkpoint.approved_target_files = Some(target_files);
                        }
                    }
                    checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
                    None
                }
            } else {
                // fail-closed：审查结论缺失评分卡时绝不默认放行（旧的 fail-open 暗道）
                let budget_exhausted = checkpoint.plan_reviewer_retry_count
                    >= crate::agent::checkpoint::max_plan_reviewer_rounds();
                Some(DispatchResult {
                    success: false,
                    stdout: String::new(),
                    stderr: crate::agent::gate_messages::cross_review_scorecard_missing(budget_exhausted),
                    route: "plan_gatekeeper:scorecard_missing".to_string(),
                })
            }
        }
        crate::agent::checkpoint::PlanExecutionMode::Auto => {
            if !is_resume_child && step_id == 0 && context.turns_history.is_empty() {
                Some(DispatchResult {
                    success: false,
                    stdout: String::new(),
                    stderr: crate::agent::gate_messages::auto_mode_need_plan(),
                    route: "plan_gatekeeper:need_plan".to_string(),
                })
            } else {
                // 🌟 智能自适应策略准入评估：
                let ws_path = context
                    .resource_scope
                    .primary_filesystem()
                    .map(|b| std::path::Path::new(&b.locator));
                let policy = crate::agent::agent_policy::PolicyConfig::load_from_workspace_or_default(ws_path);

                let mut target_files = Vec::new();
                let mut commands = Vec::new();

                if let Some(ref inst) = checkpoint.prepared_instruction {
                    if let Some(cmd) = crate::agent::dispatcher::extract_shell_command_string(inst) {
                        commands.push(cmd);
                    }
                    let params = inst.get_params();
                    if let Some(p) = params.get("path")
                        .or_else(|| params.get("file_path"))
                        .or_else(|| params.get("target"))
                        .and_then(|v| v.as_str())
                    {
                        target_files.push(p.to_string());
                    }
                }

                if let Some(ref approved) = checkpoint.approved_target_files {
                    for f in approved {
                        if !target_files.contains(f) {
                            target_files.push(f.clone());
                        }
                    }
                }

                let plan_text = format!("{} {}", context.user_goal, context.progress_summary);
                let decision = crate::agent::agent_policy::evaluate_plan_policy(
                    &policy,
                    &target_files,
                    &commands,
                    &plan_text,
                );

                checkpoint.policy_decision_summary = Some(decision.summary());

                match decision {
                    crate::agent::agent_policy::PolicyDecision::FastPass { .. } => {
                        if !target_files.is_empty() && checkpoint.approved_target_files.is_none() {
                            checkpoint.approved_target_files = Some(target_files);
                        }
                        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
                        None
                    }
                    crate::agent::agent_policy::PolicyDecision::EscalateToReview {
                        reason,
                        matched_pattern,
                        suggested_roles,
                    } => {
                        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::CrossReview);
                        let stderr = format!(
                            "【智能自适应门禁：触发按需升级审查】\n\
                             原因: {}\n\
                             命中规则: `{}`\n\
                             系统已将任务切换为方案审查模式。在动笔修改代码前，请严格遵循两阶段固定审查流程：\n\
                             1. 优先调用 `child_reviewer`（指定 role: \"tech_lead\"）进行 1v1 方案对齐讨论（传入 plan_summary、target_files 与 discussion_round）；\n\
                             2. 根据 Tech Lead 的结构化裁决行动：若 approved 则直接放行；若 rebuttal 则微调方案；若 escalate 则按推荐专家（建议：{}）并发派发联合会审！",
                            reason, matched_pattern, suggested_roles.join(", ")
                        );
                        Some(DispatchResult {
                            success: false,
                            stdout: String::new(),
                            stderr,
                            route: "plan_gatekeeper:escalated_to_cross_review".to_string(),
                        })
                    }
                    crate::agent::agent_policy::PolicyDecision::Block { reason } => {
                        Some(DispatchResult {
                            success: false,
                            stdout: String::new(),
                            stderr: format!("【安全策略强力阻断】\n{}", reason),
                            route: "plan_gatekeeper:policy_blocked".to_string(),
                        })
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::child_reviewer::{ReviewFinding, ReviewScorecard, ReviewerDeliveryData};
    use crate::agent::context::SandwichContext;
    use std::sync::{Arc, Mutex};

    fn setup_test_db() -> DbState {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE agent_child_runs (
                id TEXT PRIMARY KEY,
                parent_run_id TEXT NOT NULL,
                child_run_id TEXT NOT NULL,
                status TEXT NOT NULL,
                kind TEXT,
                result_json TEXT,
                consumed_at TEXT,
                created_at TEXT,
                updated_at TEXT,
                finished_at TEXT,
                error_kind TEXT
            );",
        )
        .unwrap();
        DbState(Arc::new(Mutex::new(conn)))
    }

    #[test]
    fn test_gate_blocks_on_incomplete_batch_barrier() {
        let db = setup_test_db();
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-run".to_string(), &context);
        cp.plan_mode = PlanExecutionMode::CrossReview;
        cp.plan_reviewer_retry_count = 1;

        let batch = vec!["rev-1".to_string(), "rev-2".to_string()];
        cp.register_plan_reviewer_batch(&batch);

        // 仅交付 1 个，未齐套
        let del = ReviewerDeliveryData {
            findings: Vec::new(),
            confidence: "high".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: None,
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: Some(ReviewScorecard {
                correctness: 90,
                risk: 90,
                style: 90,
                test_coverage: None,
                null_reason: None,
                synthesized: false,
            }),
            extra: std::collections::HashMap::new(),
        };
        cp.record_reviewer_delivery_for_run(Some("rev-1"), &del);

        let res = check_pre_write_gatekeeper(&mut cp, &context, &db, "test-run", 1, false);
        assert!(res.is_some());
        let dispatch = res.unwrap();
        assert_eq!(dispatch.route, "plan_gatekeeper:need_wait_reviewer_jobs");
        assert!(dispatch.stderr.contains("齐套屏障保护"));
        assert!(dispatch.stderr.contains("本批次派发了 2 个审查任务，当前仅回传 1 个"));
    }

    #[test]
    fn test_gate_blocks_on_union_blocking_and_passes_when_clean() {
        let db = setup_test_db();
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-run".to_string(), &context);
        cp.plan_mode = PlanExecutionMode::CrossReview;
        cp.plan_reviewer_retry_count = 1;

        let batch = vec!["rev-1".to_string(), "rev-2".to_string()];
        cp.register_plan_reviewer_batch(&batch);

        // 审查员 1 发现 blocking 缺陷
        let del1 = ReviewerDeliveryData {
            findings: vec![ReviewFinding {
                id: "SEC-BLOCK-1".to_string(),
                severity: "blocking".to_string(),
                finality: "regardless".to_string(),
                location: "src/auth.rs:42".to_string(),
                current: "unauthenticated".to_string(),
                why: "missing auth check".to_string(),
                suggestion: "add auth guard".to_string(),
                evidence_strength: "confirmed".to_string(),
            }],
            confidence: "high".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: None,
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: Some(ReviewScorecard {
                correctness: 50,
                risk: 30,
                style: 80,
                test_coverage: None,
                null_reason: None,
                synthesized: false,
            }),
            extra: std::collections::HashMap::new(),
        };
        cp.record_reviewer_delivery_for_run(Some("rev-1"), &del1);

        // 审查员 2 交付无 blocking
        let del2 = ReviewerDeliveryData {
            findings: Vec::new(),
            confidence: "high".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: None,
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: Some(ReviewScorecard {
                correctness: 90,
                risk: 90,
                style: 90,
                test_coverage: None,
                null_reason: None,
                synthesized: false,
            }),
            extra: std::collections::HashMap::new(),
        };
        cp.record_reviewer_delivery_for_run(Some("rev-2"), &del2);

        // 齐套且存在 blocking 缺陷：必须拦截并提供精准归因
        let res = check_pre_write_gatekeeper(&mut cp, &context, &db, "test-run", 1, false);
        assert!(res.is_some());
        let dispatch = res.unwrap();
        assert_eq!(dispatch.route, "plan_gatekeeper:blocking_findings_open");
        assert!(dispatch.stderr.contains("SEC-BLOCK-1"));
        assert!(dispatch.stderr.contains("src/auth.rs:42"));
        assert!(dispatch.stderr.contains("add auth guard"));

        // 模拟下一轮：缺陷被解决并撤回后，无 blocking 且评分达标
        cp.register_plan_reviewer_batch(&batch);
        let mut del1_fixed = del1.clone();
        del1_fixed.withdrawn_findings = vec!["SEC-BLOCK-1".to_string()];
        del1_fixed.scorecard = Some(ReviewScorecard {
            correctness: 95,
            risk: 90,
            style: 85,
            test_coverage: None,
            null_reason: None,
            synthesized: false,
        });
        cp.record_reviewer_delivery_for_run(Some("rev-1"), &del1_fixed);
        cp.record_reviewer_delivery_for_run(Some("rev-2"), &del2);

        let res_clean = check_pre_write_gatekeeper(&mut cp, &context, &db, "test-run", 1, false);
        assert!(res_clean.is_none(), "全部齐套且无 blocking 时应放行写入");
        assert_eq!(cp.plan_phase, PlanPhase::Approved);
    }

    #[test]
    fn test_gate_handles_all_reviewers_failed_without_deadlock() {
        let db = setup_test_db();
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-run-fail".to_string(), &context);
        cp.plan_mode = PlanExecutionMode::CrossReview;
        cp.plan_reviewer_retry_count = 1;

        let batch = vec!["rev-1".to_string(), "rev-2".to_string()];
        cp.register_plan_reviewer_batch(&batch);

        // 两个任务均异常失败
        cp.record_reviewer_failure_for_run("rev-1", "timed_out");
        cp.record_reviewer_failure_for_run("rev-2", "corrupted_json_output");

        // 批次已收口，绝不能卡在 need_wait_reviewer_jobs！
        assert!(cp.plan_review_batch.is_complete());
        assert_eq!(cp.plan_review_batch.pending_count(), 0);

        let res = check_pre_write_gatekeeper(&mut cp, &context, &db, "test-run-fail", 1, false);
        assert!(res.is_some());
        let dispatch = res.unwrap();
        assert_eq!(dispatch.route, "plan_gatekeeper:reviewer_all_failed");
        assert!(dispatch.stderr.contains("rev-1"));
        assert!(dispatch.stderr.contains("timed_out"));
        assert!(dispatch.stderr.contains("rev-2"));
        assert!(dispatch.stderr.contains("corrupted_json_output"));
    }

    #[test]
    fn test_failover_replaces_expected_reviewer_in_batch() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-failover".to_string(), &context);
        cp.register_plan_reviewer_batch(&["rev-old".to_string()]);

        assert!(cp.plan_review_batch.expected_run_ids.contains("rev-old"));

        // 触发故障转移替换
        cp.replace_expected_reviewer_run_id("rev-old", "rev-new");
        assert!(!cp.plan_review_batch.expected_run_ids.contains("rev-old"));
        assert!(cp.plan_review_batch.expected_run_ids.contains("rev-new"));
        assert!(!cp.plan_review_batch.is_complete());

        // 新任务交付后完成
        let del = ReviewerDeliveryData {
            findings: Vec::new(),
            confidence: "high".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: None,
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: None,
            extra: Default::default(),
        };
        cp.record_reviewer_delivery_for_run(Some("rev-new"), &del);
        assert!(cp.plan_review_batch.is_complete());
    }

    #[test]
    fn test_tech_lead_approved_synthesizes_scorecard_and_passes_gate() {
        let db = setup_test_db();
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-tech-lead-ok".to_string(), &context);
        cp.plan_mode = PlanExecutionMode::CrossReview;
        cp.plan_reviewer_retry_count = 1;

        cp.register_plan_reviewer_batch(&["rev-tl".to_string()]);

        let mut extra = std::collections::HashMap::new();
        extra.insert("decision".to_string(), serde_json::json!("approved"));
        extra.insert("file_coupling".to_string(), serde_json::json!("isolated"));
        extra.insert("analysis".to_string(), serde_json::json!("方案改动局部且清晰，通过"));

        let del = ReviewerDeliveryData {
            findings: Vec::new(),
            confidence: "confirmed".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: Some("certain".to_string()),
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: None, // 无显式 scorecard，由契约自动合成合格评分卡
            extra,
        };
        cp.record_reviewer_delivery_for_run(Some("rev-tl"), &del);

        assert!(cp.last_review_scorecard.is_some(), "approved 应自动合成基线合格评分卡");
        assert!(
            cp.last_review_scorecard.as_ref().unwrap().synthesized,
            "合成的基线评分卡必须携带 synthesized 标记，与非审查员真实评分区分"
        );
        let res = check_pre_write_gatekeeper(&mut cp, &context, &db, "test-tech-lead-ok", 1, false);
        assert!(res.is_none(), "Tech Lead 裁决 approved 且无 blocking 时应放行写入");
        assert_eq!(cp.plan_phase, PlanPhase::Approved);
    }

    #[test]
    fn test_tech_lead_escalate_blocks_gate_until_multi_expert_council_passes() {
        let db = setup_test_db();
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-tech-lead-esc".to_string(), &context);
        cp.plan_mode = PlanExecutionMode::CrossReview;
        cp.plan_reviewer_retry_count = 1;

        // 1. Tech Lead 1v1 评审判定 escalate
        cp.register_plan_reviewer_batch(&["rev-tl".to_string()]);

        let mut extra = std::collections::HashMap::new();
        extra.insert("decision".to_string(), serde_json::json!("escalate"));
        extra.insert("file_coupling".to_string(), serde_json::json!("coupled"));
        extra.insert("analysis".to_string(), serde_json::json!("涉及核心鉴权架构改造，需联合会审"));
        extra.insert("escalate_roles".to_string(), serde_json::json!(["architect", "security_specialist"]));

        let del = ReviewerDeliveryData {
            findings: Vec::new(),
            confidence: "confirmed".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: Some("certain".to_string()),
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: None,
            extra,
        };
        cp.record_reviewer_delivery_for_run(Some("rev-tl"), &del);

        assert!(cp.plan_escalation_required.is_some());
        let res = check_pre_write_gatekeeper(&mut cp, &context, &db, "test-tech-lead-esc", 1, false);
        assert!(res.is_some());
        let dispatch = res.unwrap();
        assert_eq!(dispatch.route, "plan_gatekeeper:escalation_required");
        assert!(dispatch.stderr.contains("Tech Lead 方案评审判定必须升级多专家联合会审"));
        assert!(dispatch.stderr.contains("architect, security_specialist"));

        // 2. 主脑响应升级要求，并发派发多专家联合会审
        let council_batch = vec!["council-arch".to_string(), "council-sec".to_string()];
        cp.register_plan_reviewer_batch(&council_batch);
        assert!(cp.plan_escalation_required.is_none(), "注册多任务联合会审后升级挂起状态已清除");

        let council_del = ReviewerDeliveryData {
            findings: Vec::new(),
            confidence: "confirmed".to_string(),
            evidence_refs: Vec::new(),
            missing_evidence: Vec::new(),
            routing: Some("certain".to_string()),
            withdrawn_findings: Vec::new(),
            objection_verdicts: Vec::new(),
            scorecard: Some(ReviewScorecard::new(90, 90, 85, None)),
            extra: Default::default(),
        };
        cp.record_reviewer_delivery_for_run(Some("council-arch"), &council_del);
        cp.record_reviewer_delivery_for_run(Some("council-sec"), &council_del);

        // 联合会审齐套且无 blocking：放行写入！
        let res_council = check_pre_write_gatekeeper(&mut cp, &context, &db, "test-tech-lead-esc", 2, false);
        assert!(res_council.is_none(), "联合会审齐套通过后放行写入");
        assert_eq!(cp.plan_phase, PlanPhase::Approved);
    }

    fn exploration_checkpoint(
        mode: PlanExecutionMode,
        phase: PlanPhase,
        retry_count: u32,
    ) -> AgentCheckpoint {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-liveness".to_string(), &context);
        cp.plan_mode = mode;
        cp.plan_phase = phase;
        cp.plan_reviewer_retry_count = retry_count;
        cp
    }

    #[test]
    fn test_exploration_gate_fires_only_after_budget() {
        let cp = exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 0);
        assert!(check_cross_review_exploration_gate(&cp, 19, "read_file", "", 20).is_none());
        let res = check_cross_review_exploration_gate(&cp, 20, "grep_search", "", 20);
        assert!(res.is_some());
        let dispatch = res.unwrap();
        assert_eq!(
            dispatch.route,
            "plan_gatekeeper:exploration_budget_exhausted"
        );
        assert!(!dispatch.success);
        assert!(dispatch.stderr.contains("20"));
        assert!(dispatch.stderr.contains("child_reviewer"));
    }

    #[test]
    fn test_exploration_gate_allows_reviewer_dispatch_and_human_escalation() {
        let cp = exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 0);
        assert!(check_cross_review_exploration_gate(&cp, 20, "child_reviewer", "", 20).is_none());
        assert!(check_cross_review_exploration_gate(&cp, 20, "", "child_reviewer", 20).is_none());
        assert!(check_cross_review_exploration_gate(&cp, 20, "ask_human", "", 20).is_none());
        assert!(check_cross_review_exploration_gate(&cp, 20, "", "ask", 20).is_none());
    }

    #[test]
    fn test_exploration_gate_ignores_non_exploring_states() {
        let auto = exploration_checkpoint(PlanExecutionMode::Auto, PlanPhase::Exploring, 0);
        assert!(check_cross_review_exploration_gate(&auto, 100, "read_file", "", 20).is_none());

        let retried =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 1);
        assert!(check_cross_review_exploration_gate(&retried, 100, "read_file", "", 20).is_none());

        let approved =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Approved, 0);
        assert!(check_cross_review_exploration_gate(&approved, 100, "read_file", "", 20).is_none());
    }

    // ---------- 探索延期裁决（软提醒 + 结构化延长 + 绝对上限） ----------

    fn extension_cfg() -> ExplorationBudgetConfig {
        ExplorationBudgetConfig {
            soft_steps: 10,
            base_steps: 20,
            per_extension_steps: 8,
            max_extensions: 2,
            absolute_steps: 40,
        }
    }

    fn valid_extension_request() -> crate::agent::types::ExplorationExtensionRequest {
        crate::agent::types::ExplorationExtensionRequest {
            unresolved_question: "尚未确认 checkpoint 与 wait_jobs 的失败交付边界".to_string(),
            next_queries: vec!["定位 record_reviewer_failure_for_run 的调用点".to_string()],
            extra_steps: None,
            reason: String::new(),
        }
    }

    #[test]
    fn test_extension_grant_raises_effective_limit() {
        let cfg = extension_cfg();
        let mut cp =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 0);
        let verdict =
            adjudicate_exploration_extension_with(&mut cp, &valid_extension_request(), cfg);
        assert!(verdict.granted);
        assert_eq!(verdict.granted_steps, 8);
        assert_eq!(verdict.route, "plan_liveness:extension_granted");
        assert_eq!(cp.plan_exploration_extensions_used, 1);
        assert_eq!(cp.plan_exploration_granted_steps, 8);
        // 有效上限从 20 抬升到 28：第 25 步的搜索在授予后被放行
        assert!(check_cross_review_exploration_gate(&cp, 25, "search", "", 28).is_none());
    }

    #[test]
    fn test_extension_denied_without_unresolved_question() {
        let cfg = extension_cfg();
        let mut cp =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 0);
        let mut req = valid_extension_request();
        req.unresolved_question = "   ".to_string();
        let verdict = adjudicate_exploration_extension_with(&mut cp, &req, cfg);
        assert!(!verdict.granted);
        assert_eq!(verdict.route, "plan_liveness:extension_denied");
        // 结构不合法的拒绝不消耗延期次数
        assert_eq!(cp.plan_exploration_extensions_used, 0);
        assert_eq!(cp.plan_exploration_granted_steps, 0);
    }

    #[test]
    fn test_extension_denied_without_concrete_queries() {
        let cfg = extension_cfg();
        let mut cp =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 0);
        let mut req = valid_extension_request();
        req.next_queries = vec!["  ".to_string()];
        let verdict = adjudicate_exploration_extension_with(&mut cp, &req, cfg);
        assert!(!verdict.granted);
        assert_eq!(cp.plan_exploration_extensions_used, 0);

        req.next_queries = (0..7).map(|i| format!("query {}", i)).collect();
        let verdict = adjudicate_exploration_extension_with(&mut cp, &req, cfg);
        assert!(!verdict.granted, "超过 6 条取证项同样拒绝");
        assert_eq!(cp.plan_exploration_extensions_used, 0);
    }

    #[test]
    fn test_extension_count_capped_by_max_extensions() {
        let cfg = extension_cfg();
        let mut cp =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 0);
        let req = valid_extension_request();
        assert!(adjudicate_exploration_extension_with(&mut cp, &req, cfg).granted);
        assert!(adjudicate_exploration_extension_with(&mut cp, &req, cfg).granted);
        let third = adjudicate_exploration_extension_with(&mut cp, &req, cfg);
        assert!(!third.granted, "第三次申请必须被次数上限拒绝");
        assert_eq!(cp.plan_exploration_extensions_used, 2);
        assert_eq!(cp.plan_exploration_granted_steps, 16);
    }

    #[test]
    fn test_extension_never_exceeds_absolute_cap() {
        let cfg = extension_cfg(); // base 20 + abs 40 → 最多再授 20 步
        let mut cp =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 0);
        let mut req = valid_extension_request();
        req.extra_steps = Some(8);
        adjudicate_exploration_extension_with(&mut cp, &req, cfg); // +8 → 28
        adjudicate_exploration_extension_with(&mut cp, &req, cfg); // +8 → 36
        // 次数已用完；把次数上限抬高后，余量只剩 40-36=4 步
        let loose_cfg = ExplorationBudgetConfig { max_extensions: 9, ..cfg };
        let verdict = adjudicate_exploration_extension_with(&mut cp, &req, loose_cfg);
        assert!(verdict.granted);
        assert_eq!(verdict.granted_steps, 4, "授予量必须被绝对上限截断");
        assert_eq!(cfg.effective_limit(cp.plan_exploration_granted_steps), 40);
    }

    #[test]
    fn test_extension_not_applicable_outside_exploring() {
        let cfg = extension_cfg();
        let req = valid_extension_request();
        let mut auto = exploration_checkpoint(PlanExecutionMode::Auto, PlanPhase::Exploring, 0);
        let v = adjudicate_exploration_extension_with(&mut auto, &req, cfg);
        assert!(!v.granted);
        assert_eq!(v.route, "plan_liveness:extension_not_applicable");

        let mut dispatched =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Reviewing, 1);
        let v = adjudicate_exploration_extension_with(&mut dispatched, &req, cfg);
        assert!(!v.granted);

        let mut approved =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Approved, 0);
        let v = adjudicate_exploration_extension_with(&mut approved, &req, cfg);
        assert!(!v.granted);
        // 不适用裁决不得污染延期账本
        assert_eq!(auto.plan_exploration_extensions_used, 0);
        assert_eq!(dispatched.plan_exploration_granted_steps, 0);
        assert_eq!(approved.plan_exploration_extensions_used, 0);
    }

    #[test]
    fn test_extension_request_makes_bare_instruction_executable() {
        let bare = crate::agent::types::AgentInstruction {
            request_exploration_extension: Some(valid_extension_request()),
            ..Default::default()
        };
        assert!(bare.validate_executable_shape().is_ok());
        assert!(bare.is_bare_exploration_extension_request());

        let mut attached = crate::agent::types::AgentInstruction::default();
        attached.tool = Some("search".to_string());
        attached.request_exploration_extension = Some(valid_extension_request());
        assert!(attached.validate_executable_shape().is_ok());
        assert!(!attached.is_bare_exploration_extension_request());
    }

    // ---------- submit_plan：适用性预检 / 结构校验 / 一致提交 ----------

    fn valid_submission() -> crate::agent::types::PlanSubmission {
        crate::agent::types::PlanSubmission {
            plan_summary: "修复 checkpoint 保存时的竞态：把批次注册并入同一次 UPDATE".to_string(),
            target_files: vec!["src/agent/checkpoint.rs".to_string()],
            root_cause: "spawn 与批次注册分离导致半截状态".to_string(),
            evidence_refs: vec!["src/agent/checkpoint.rs:922".to_string()],
            assumptions: vec![],
            open_questions: vec![],
            intended_verification: "cargo test --lib plan_gate".to_string(),
            risk_level: "medium".to_string(),
        }
    }

    #[test]
    fn test_submit_plan_rejection_phase_matrix() {
        // 交叉审查 + Exploring + 未派审 → 放行
        let mut cp =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 0);
        assert!(submit_plan_rejection(&cp).is_none());

        // Auto 模式 → 不适用
        let auto = exploration_checkpoint(PlanExecutionMode::Auto, PlanPhase::Exploring, 0);
        assert!(submit_plan_rejection(&auto).unwrap().contains("不适用"));

        // PlanApproval 模式 → 不适用（应走 ask_human）
        let approval =
            exploration_checkpoint(PlanExecutionMode::PlanApproval, PlanPhase::Exploring, 0);
        assert!(submit_plan_rejection(&approval).unwrap().contains("不适用"));

        // 已放行 → 不适用
        let approved =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Approved, 0);
        assert!(submit_plan_rejection(&approved).unwrap().contains("已获放行"));

        // 升级挂起 → 拒绝并指引联合会审
        cp.plan_escalation_required = Some(vec!["architect".to_string()]);
        let rej = submit_plan_rejection(&cp).unwrap();
        assert!(rej.contains("联合会审"));
        assert!(rej.contains("architect"));
        cp.plan_escalation_required = None;

        // 审查轮次未闭合（已预扣未消费）→ 拒绝重复提交
        cp.plan_reviewer_retry_count = 1;
        cp.plan_reviewer_consumed = false;
        assert!(submit_plan_rejection(&cp).unwrap().contains("在途"));

        // 上一轮已消费 → 允许新一轮提交
        cp.plan_reviewer_consumed = true;
        assert!(submit_plan_rejection(&cp).is_none());

        // 预算耗尽 → 拒绝
        cp.plan_reviewer_retry_count = crate::agent::checkpoint::max_plan_reviewer_rounds();
        assert!(submit_plan_rejection(&cp).unwrap().contains("上限"));
    }

    #[test]
    fn test_validate_plan_submission_structure() {
        // 合法提交
        let (files, refs) = validate_plan_submission(&valid_submission()).unwrap();
        assert_eq!(files, vec!["src/agent/checkpoint.rs"]);
        assert_eq!(refs, vec!["src/agent/checkpoint.rs:922"]);

        // 摘要过短
        let mut s = valid_submission();
        s.plan_summary = "修 bug".to_string();
        assert!(validate_plan_submission(&s).unwrap_err().contains("plan_summary"));

        // 无目标文件 / 过多目标文件
        let mut s = valid_submission();
        s.target_files = vec![];
        assert!(validate_plan_submission(&s).unwrap_err().contains("target_files"));
        s.target_files = (0..51).map(|i| format!("f{i}.rs")).collect();
        assert!(validate_plan_submission(&s).unwrap_err().contains("target_files"));

        // 无证据 / 证据超上限
        let mut s = valid_submission();
        s.evidence_refs = vec!["  ".to_string()];
        assert!(validate_plan_submission(&s).unwrap_err().contains("evidence_refs"));
        s.evidence_refs = (0..7).map(|i| format!("src/f{i}.rs")).collect();
        assert!(validate_plan_submission(&s).unwrap_err().contains("evidence_refs"));
    }

    #[test]
    fn test_commit_plan_submission_is_consistent() {
        let mut cp =
            exploration_checkpoint(PlanExecutionMode::CrossReview, PlanPhase::Exploring, 0);
        let snapshot = crate::agent::child_reviewer::PlanReviewerDispatchSnapshot::new(
            "model-x",
            "pro",
            "high",
            "persona",
            serde_json::json!({"role": "tech_lead"}),
        );
        cp.commit_plan_submission(valid_submission(), "rev-42", &snapshot, 9);

        // 不变式：方案落账、阶段推进、预算预扣、批次注册、派发快照——同生同灭
        assert!(cp.submitted_plan.is_some());
        assert_eq!(cp.submitted_plan_step, Some(9));
        assert_eq!(cp.plan_phase, PlanPhase::Reviewing);
        assert_eq!(cp.plan_reviewer_retry_count, 1);
        assert!(!cp.plan_reviewer_consumed);
        assert!(cp.plan_review_batch.expected_run_ids.contains("rev-42"));
        assert_eq!(cp.plan_review_batch.expected_run_ids.len(), 1);
        assert!(cp.plan_reviewer_last_dispatch.is_some());
        // 提交后门禁应把后续提交视为「轮次未闭合」
        assert!(submit_plan_rejection(&cp).unwrap().contains("在途"));
    }

    #[test]
    fn test_submit_plan_instruction_shapes() {
        // 纯提交可执行且识别为 bare
        let bare = crate::agent::types::AgentInstruction {
            submit_plan: Some(valid_submission()),
            ..Default::default()
        };
        assert!(bare.validate_executable_shape().is_ok());
        assert!(bare.is_bare_plan_submission());
        assert!(!bare.is_bare_exploration_extension_request());

        // 附带工具入口 → 非 bare（宿主会以协议误用拒绝混发）
        let mut mixed = crate::agent::types::AgentInstruction::default();
        mixed.tool = Some("search".to_string());
        mixed.submit_plan = Some(valid_submission());
        assert!(mixed.validate_executable_shape().is_ok());
        assert!(!mixed.is_bare_plan_submission());
    }
}


