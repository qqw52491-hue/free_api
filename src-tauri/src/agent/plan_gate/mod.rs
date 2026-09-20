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

/// 多专家联合会审面板开关（A/B 对照实验用）。
/// `FREE_API_EVAL_REVIEW_PANEL=off|0|disabled|false` → 钉死 1v1：Tech Lead 的
/// escalate 裁决被降级为 rebuttal，plan_escalation_required 不置位，
/// submit_plan 的 1v1 通道保持可用。缺省=on（生产行为不变）。
pub(crate) fn review_panel_enabled() -> bool {
    !matches!(
        std::env::var("FREE_API_EVAL_REVIEW_PANEL")
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref(),
        Some("off") | Some("0") | Some("disabled") | Some("false")
    )
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
/// `evidence_required` 由任务入口分流判决驱动（evidence_policy=code_refs/operational_probes
/// 或无判决时为 true；判决为 none/source_refs 时允许 0~6 条——产物类方案没有"已读代码"可引）。
/// 成功返回 (规范化 target_files, 去重 evidence_refs)。
pub(crate) fn validate_plan_submission(
    submission: &crate::agent::types::PlanSubmission,
    evidence_required: bool,
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
    // 空数组合法：纯分析/只读审查/报告类交付没有"拟改文件"可声明，交付物即结论文本。
    // 下游已消化空声明——policy 层判 FastPass，finish 零交集对账门对空声明自然
    // NotApplicable（plan_targets=None → 跳过），scope 预检只认非空 block.scope。
    if target_files.len() > 50 {
        return Err(
            "【⚠️ submit_plan 结构不合法】target_files 最多 50 条非空文件路径；纯分析/只读任务可传空数组 []。".to_string(),
        );
    }
    let mut evidence_refs: Vec<String> = submission
        .evidence_refs
        .iter()
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .collect();
    evidence_refs.dedup();
    if evidence_required && evidence_refs.is_empty() {
        return Err(
            "【⚠️ submit_plan 结构不合法】evidence_refs 必须包含 1~6 条你已实际读取过的证据引用（文件路径或证据标识）；空手提交方案没有审查意义。"
                .to_string(),
        );
    }
    if evidence_refs.len() > 6 {
        return Err(
            "【⚠️ submit_plan 结构不合法】evidence_refs 最多 6 条，请精选最关键的证据引用。"
                .to_string(),
        );
    }
    if let Some(reason) = plan_risk_preflight(submission, &target_files) {
        return Err(reason);
    }
    Ok((target_files, evidence_refs))
}

/// 首轮方案静态预检（0 Token 宿主规则）：target_files 命中敏感模式时，
/// 要求方案文本显式陈述对应防御设计——目的不是静态验证方案正确性
/// （那是 Tech Lead 审查的职责），而是倒逼主脑在首轮摘要里把常见打回
/// 模式（脱敏/有界/事务/重试上限）显式写明，砍掉「打回→重探→重写」的返工段。
///
/// 逃生口：任一文本字段出现「不涉及/无需/只读」等显式豁免陈述即放行——
/// 本门禁只强制"显式陈述"，不代行语义判断，避免制造无解打回链。
fn plan_risk_preflight(
    submission: &crate::agent::types::PlanSubmission,
    target_files: &[String],
) -> Option<String> {
    // (目标路径触发词, 防御陈述期望词, 风险类别标签)
    const RULES: &[(&[&str], &[&str], &str)] = &[
        (
            &[
                "auth", "token", "login", "credential", "password", "secret", "apikey",
                "api_key", "oauth", "jwt",
            ],
            &[
                "脱敏", "mask", "sanitize", "redact", "加密", "明文", "不落盘", "不落地", "隐藏",
            ],
            "敏感凭据",
        ),
        (
            &["cache", "hashmap", "hash_map", "lru", "memo", "缓存"],
            &[
                "capacity", "evict", "lru", "ttl", "bound", "有界", "上限", "容量", "淘汰", "过期",
            ],
            "缓存/内存结构",
        ),
        (
            &["retry", "reconnect", "poll", "watch", "failover", "重试", "轮询"],
            &[
                "max_retries", "backoff", "circuit", "熔断", "退避", "上限", "重试次数", "封顶",
            ],
            "重试/循环",
        ),
        (
            &["sql", "database", "sqlite", "transaction", "数据库"],
            &["transaction", "atomic", "事务", "回滚", "rollback", "原子"],
            "数据库/持久化",
        ),
    ];
    // 通用豁免词：模型显式声明该风险不适用即视为已陈述（软强制的逃生口）
    const EXEMPTION_KEYWORDS: &[&str] = &[
        "不涉及", "无此风险", "无需", "只读", "n/a", "not applicable", "已有", "已覆盖",
    ];

    let mut missing: Vec<(&str, &[&str], Vec<String>)> = Vec::new();
    for (triggers, expect, label) in RULES {
        let hit_files: Vec<String> = target_files
            .iter()
            .filter(|f| {
                let lf = f.to_lowercase();
                triggers.iter().any(|t| lf.contains(t))
            })
            .cloned()
            .collect();
        if hit_files.is_empty() {
            continue;
        }
        let text = format!(
            "{}\n{}\n{}\n{}\n{}",
            submission.plan_summary,
            submission.root_cause,
            submission.assumptions.join("\n"),
            submission.open_questions.join("\n"),
            submission.intended_verification
        )
        .to_lowercase();
        let addressed = expect.iter().any(|k| text.contains(k))
            || EXEMPTION_KEYWORDS.iter().any(|k| text.contains(k));
        if !addressed {
            missing.push((label, expect, hit_files));
        }
    }
    if missing.is_empty() {
        return None;
    }
    let mut msg = String::from(
        "【⚠️ submit_plan 预检拦截】拟修改文件命中敏感模块，但方案文本未显式陈述对应防御设计：\n",
    );
    for (label, expect, files) in &missing {
        let shown: Vec<&str> = files.iter().take(3).map(|s| s.as_str()).collect();
        msg.push_str(&format!(
            "  · {} 类（命中: {}{}）：请在 plan_summary/root_cause/assumptions 中说明如何处理（期望涉及: {}）\n",
            label,
            shown.join(", "),
            if files.len() > 3 { " 等" } else { "" },
            expect.join("/")
        ));
    }
    msg.push_str(
        "补充陈述后重新 submit_plan 即可；若经核实改动确不涉及该类风险（如只读访问、已有保护覆盖），请在 plan_summary 中显式写明豁免理由，预检即放行。",
    );
    Some(msg)
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
    Some(DispatchResult { view: None,
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

/// 阶段审查拦截判定（独立于方案门禁：Approved 后的写入也要过阶段审查这一关）。
/// blocking_regardless 未解决 → 拦截写入；解套唯一路径是 resume_child 对质撤回。
/// final_only 不在此拦截，由收尾审查兜底。
/// triage 分流豁免方案门时本拦截仍须执行——它是已判出中间态错误的物理拦杆。
pub(crate) fn stage_blocker_interception(checkpoint: &AgentCheckpoint, db: &DbState) -> Option<DispatchResult> {
    if checkpoint.stage_review_blockers_open == 0 {
        return None;
    }
    // resume 指路：优先 blocker 产出者，否则取当前阶段批次的最新成员（台账推导）
    let stage_run_id = checkpoint
        .stage_review_resume_hint(db)
        .unwrap_or_default();
    let final_only_summary = checkpoint.stage_review_final_only.join("\n");
    Some(DispatchResult {
        view: None,
        success: false,
        stdout: String::new(),
        stderr: crate::agent::gate_messages::stage_review_blocking_open(
            checkpoint.stage_review_blockers_open,
            &stage_run_id,
            &final_only_summary,
        ),
        route: "stage_review:blocking_open".to_string(),
    })
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
    // ---- 阶段级审查拦截先于方案门（triage 分流豁免方案门时它仍单独执行）----
    if let Some(blocked) = stage_blocker_interception(checkpoint, db) {
        return Some(blocked);
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
            Some(DispatchResult { view: None,
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
            // 齐套判定由台账推导：批次有成员且仍有回执未消费 = 未齐套
            let is_batch_incomplete = checkpoint.plan_review_batch_id.is_some()
                && !checkpoint.plan_review_batch_complete(db);

            if checkpoint.plan_reviewer_retry_count == 0 {
                Some(DispatchResult { view: None,
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
                        checkpoint.plan_review_delivered_count(db),
                        checkpoint.plan_review_member_count(db).max(
                            checkpoint.plan_review_expected_count as usize,
                        ),
                    )
                } else {
                    crate::agent::gate_messages::cross_review_need_wait_reviewer_jobs()
                };
                Some(DispatchResult { view: None,
                    success: false,
                    stdout: String::new(),
                    stderr,
                    route: "plan_gatekeeper:need_wait_reviewer_jobs".to_string(),
                })
            } else if !checkpoint.plan_reviewer_consumed {
                // 已消耗过审查轮次，但后台既没有在跑的任务、也没有待回收的结论 ——
                // 说明上一次派发并未真正启动（角色卡未授权/不存在、参数非法等）。
                // 此时必须引导模型重新派发，否则它会一直 wait_jobs 收空信箱而永久无法写入。
                Some(DispatchResult { view: None,
                    success: false,
                    stdout: String::new(),
                    stderr: crate::agent::gate_messages::cross_review_reviewer_result_missing(),
                    route: "plan_gatekeeper:reviewer_result_missing".to_string(),
                })
            } else if checkpoint.plan_review_deliveries.is_empty()
                && checkpoint.plan_review_failed_count(db) > 0
            {
                let budget_exhausted = checkpoint.plan_reviewer_retry_count
                    >= crate::agent::checkpoint::max_plan_reviewer_rounds();
                // 失败原因从台账 error_kind 直查（台账是唯一事实源）
                let reasons: Vec<String> = checkpoint
                    .plan_review_batch_id
                    .as_ref()
                    .and_then(|bid| {
                        db.0.lock().ok().and_then(|conn| {
                            let mut stmt = conn.prepare(
                                "SELECT id, COALESCE(error_kind, status) FROM agent_child_runs WHERE batch_id = ?1 AND consumed_at IS NOT NULL AND status != 'succeeded'"
                            ).ok()?;
                            let rows = stmt.query_map(rusqlite::params![bid], |r| {
                                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                            }).ok()?;
                            Some(rows.flatten().map(|(id, r)| format!("- 审查任务 `{}`: {}", id, r)).collect())
                        })
                    })
                    .unwrap_or_default();
                let summary = reasons.join("\n");
                Some(DispatchResult { view: None,
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
                Some(DispatchResult { view: None,
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
                let unwithdrawn_blockings = checkpoint.aggregate_unwithdrawn_blockings();
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
                    Some(DispatchResult { view: None,
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
                    Some(DispatchResult { view: None,
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
                Some(DispatchResult { view: None,
                    success: false,
                    stdout: String::new(),
                    stderr: crate::agent::gate_messages::cross_review_scorecard_missing(budget_exhausted),
                    route: "plan_gatekeeper:scorecard_missing".to_string(),
                })
            }
        }
        crate::agent::checkpoint::PlanExecutionMode::Auto => {
            if !is_resume_child && step_id == 0 && context.turns_history.is_empty() {
                Some(DispatchResult { view: None,
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

                let plan_text = format!("{} {}", context.effective_goal(), context.progress_summary);
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
                        Some(DispatchResult { view: None,
                            success: false,
                            stdout: String::new(),
                            stderr,
                            route: "plan_gatekeeper:escalated_to_cross_review".to_string(),
                        })
                    }
                    crate::agent::agent_policy::PolicyDecision::Block { reason } => {
                        Some(DispatchResult { view: None,
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
mod tests;
