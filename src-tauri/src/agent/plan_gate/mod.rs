//! 方案审批结论解析与前置写入门禁。
//! 从 loop_runner.rs 原样抽出，行为不变；loop_runner 通过 re-export 维持原引用路径。

use crate::agent::checkpoint::{AgentCheckpoint, PlanExecutionMode};
use crate::agent::child_reviewer::{has_pending_review_results, has_running_child_runs};
use crate::agent::gate_messages;
use crate::agent::types::DispatchResult;
use crate::db::DbState;

/// 前端下发方案审批结论时使用的结构化标记，形如 `<!--plan-decision:{"approved":true}-->`。
/// 采用 HTML 注释形态，既不会干扰 Markdown 方案正文的渲染，也不会被模型当作指令文本。
const PLAN_DECISION_MARKER_PREFIX: &str = "<!--plan-decision:";
const PLAN_DECISION_MARKER_SUFFIX: &str = "-->";

/// 分流失败不是复杂度判决。只在 auto 模式下，按物理边界豁免本会话 tmp 写入。
/// 有效判决、工作区写入、显式审批及阶段 blocker 仍走既有规则。
pub(crate) fn pre_write_plan_required(
    checkpoint: &AgentCheckpoint,
    context: &crate::agent::context::SandwichContext,
    inst: &crate::agent::types::AgentInstruction,
    session_id: &str,
    workspace: &std::path::Path,
) -> bool {
    if submitted_review_active(checkpoint)
        || checkpoint.plan_mode == PlanExecutionMode::PlanApproval
        || (checkpoint.triage_verdict.is_some()
            && crate::agent::triage::write_floor_violation(inst, context, session_id).is_some())
    {
        return true;
    }
    match checkpoint.triage_verdict.as_ref() {
        Some(v) if v.fallback.is_some() => {
            checkpoint.plan_mode != PlanExecutionMode::Auto
                || !crate::agent::path_guard::is_confined_session_tmp_write(
                    inst, session_id, workspace,
                )
        }
        Some(v) => v.requires_plan_gate(),
        None => !crate::agent::path_guard::is_plan_gate_exempt_write(
            inst,
            Some(session_id),
            workspace,
            &context.new_affected_paths(),
        ),
    }
}

pub(crate) fn submitted_review_active(checkpoint: &AgentCheckpoint) -> bool {
    checkpoint.submitted_plan.is_some() && checkpoint.plan_review_batch_id.is_some()
        && checkpoint.plan_review_expected_count > 0
}

/// submit_plan 适用性预检（纯 checkpoint 判定，不含 DB 在途行检查）。
/// 返回 Some(reason) 表示当前状态不可用提交；None 放行。
/// 覆盖：模式/阶段/升级挂起/未闭合审查轮次/预算上限。
pub(crate) fn submit_plan_rejection(
    checkpoint: &AgentCheckpoint,
    submission: &crate::agent::types::PlanSubmission,
) -> Option<String> {
    if checkpoint.plan_mode != PlanExecutionMode::CrossReview
        && checkpoint.plan_mode != PlanExecutionMode::Auto
    {
        return Some(format!(
            "【ℹ️ submit_plan 不适用】当前执行模式为 {:?}，方案提交通道在交叉审查模式与全自动模式下有效。\n\
             方案审批模式请调用 ask_human(kind: plan_approval) 呈报方案。",
            checkpoint.plan_mode
        ));
    }
    if checkpoint.plan_phase.is_approved() && submission.revision_reason.trim().is_empty() {
        return Some(
            "【ℹ️ submit_plan 不适用】方案已获放行，无需再次提交。请按已批准方案执行写入与验证。\n\
             若执行中新发现的证据与已批准方案冲突（如取证引用过的文件在新语义下不再成立），\
             请附 revision_reason 说明冲突证据重新提交——修订轮将重开 Tech Lead 评审并消耗审查预算。"
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
    // 修订轮（已批准后带 revision_reason 重提）每次重新给一套审查次数，不受上一轮用尽的限制
    if !checkpoint.plan_phase.is_approved()
        && checkpoint.plan_reviewer_retry_count >= crate::agent::checkpoint::max_plan_reviewer_rounds()
    {
        return Some(gate_messages::reviewer_retry_limit());
    }
    None
}

/// submit_plan 结构校验：方案必须带来源证据，防止空想方案进入审查。
/// `evidence_required` 由任务入口分流判决驱动（evidence_policy=code_refs/operational_probes
/// 或无判决时为 true；none/source_refs 时允许空引用。引用共用子审查的总字符预算。
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
    let evidence_refs: Vec<String> = submission
        .evidence_refs
        .iter()
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .collect();
    let evidence_refs = crate::agent::child_reviewer::normalize_review_evidence_refs(&evidence_refs)
        .map_err(|reason| format!("【⚠️ submit_plan 参数未通过校验，尚未派发审查】{}", reason))?;
    if evidence_required && evidence_refs.is_empty() {
        return Err(
            "【⚠️ submit_plan 参数缺失】本次提交的 evidence_refs 为空，方案尚未提交审查。请补充已读取的证据引用（文件路径或证据标识）后重发；此错误不代表此前未取证。"
                .to_string(),
        );
    }
    Ok((target_files, evidence_refs))
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
    checkpoint.refresh_review_sources(db);
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
                || result.route == "plan_gatekeeper:blocking_findings_open"
                || result.route == "plan_gatekeeper:review_revisions_required"
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
    // 正式提交后已派出的审查是事实；Auto 不能在本批未收口时又走免审写入。
    // 只复用方案门判定，不改变真实 plan_mode，阶段审查触发方式保持不变。
    let effective_mode = if checkpoint.plan_mode == PlanExecutionMode::Auto && submitted_review_active(checkpoint) {
        PlanExecutionMode::CrossReview
    } else { checkpoint.plan_mode };
    match effective_mode {
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
            } else if checkpoint.plan_review_deliveries.is_empty() {
                Some(DispatchResult { view: None,
                    success: false,
                    stdout: String::new(),
                    stderr: gate_messages::cross_review_reviewer_result_missing(),
                    route: "plan_gatekeeper:reviewer_result_missing".to_string(),
                })
            } else {
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
                } else if checkpoint.plan_revisions_required(db) {
                    Some(DispatchResult { view: None,
                        success: false,
                        stdout: String::new(),
                        stderr: gate_messages::cross_review_revisions_required(budget_exhausted),
                        route: "plan_gatekeeper:review_revisions_required".to_string(),
                    })
                } else if checkpoint.submitted_plan.is_none() {
                    // 结构化方案未落账不构成放行依据：investigate 直连 Tech Lead 也能
                    // 派发方案评审并把 plan_phase 推进到 Reviewing，但批准必须锚定
                    // submit_plan 落账的契约字段（target_files/evidence_refs/root_cause
                    // /intended_verification），否则证据范围对账、修订通道、target_files
                    // diff 对账全部失去锚点（workspace_leak 实测：模型绕过 submit_plan
                    // 直派 investigate，方案未落账即被"批准"）。
                    Some(DispatchResult { view: None,
                        success: false,
                        stdout: String::new(),
                        stderr: crate::agent::gate_messages::plan_approval_requires_submission(),
                        route: "plan_gatekeeper:no_submitted_plan".to_string(),
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
                    crate::agent::agent_policy::PolicyDecision::FastPass { .. }
                        if checkpoint
                            .triage_verdict
                            .as_ref()
                            .map(|v| v.requires_plan_gate())
                            .unwrap_or(false)
                            && checkpoint.submitted_plan.is_none() =>
                    {
                        // 分流要求走方案闸（technical_plan/expert_panel）且尚未 submit_plan：
                        // 极速放行不得把规划阶段直接收尾为“已批准”——否则 Pro 跑一条良性试跑命令
                        // 就会被踢出规划、无方案无审查地交给执行模型。
                        if target_files.is_empty() {
                            // 纯探测/良性命令：放行这一步，但保持规划阶段不变
                            None
                        } else {
                            Some(DispatchResult { view: None,
                                success: false,
                                stdout: String::new(),
                                stderr: crate::agent::gate_messages::plan_approval_requires_submission(),
                                route: "plan_gatekeeper:no_submitted_plan".to_string(),
                            })
                        }
                    }
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
                             系统已将任务切换为方案审查模式。在动笔修改代码前，请按顺序完成两步：\n\
                             1. 单独输出 submit_plan，提交 plan_summary、target_files、evidence_refs、root_cause、assumptions、open_questions、intended_verification；宿主同时登记结构化方案并异步派发审查。tech_lead 缺失时使用基础 reviewer 加载相同方案审查协议，无需重建专业卡；\n\
                             2. 调用 wait_jobs 收取裁决：approved 放行，rebuttal 修订后重新 submit_plan，escalate 按裁决指定的可用专家（策略建议：{}）派发联合会审。缺少角色时明确报告，不伪造角色或跳过审查。",
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
