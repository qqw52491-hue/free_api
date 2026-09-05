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
            .stage_review_run_id
            .clone()
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

    if checkpoint.plan_phase.is_approved() {
        return None;
    }

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
            if checkpoint.plan_reviewer_retry_count == 0 {
                Some(DispatchResult {
                    success: false,
                    stdout: String::new(),
                    stderr: crate::agent::gate_messages::cross_review_need_reviewer_dispatch(),
                    route: "plan_gatekeeper:need_reviewer_dispatch".to_string(),
                })
            } else if reviewer_running
                || (!checkpoint.plan_reviewer_consumed
                    && crate::agent::child_reviewer::has_pending_review_results(db, parent_run_id))
            {
                Some(DispatchResult {
                    success: false,
                    stdout: String::new(),
                    stderr: crate::agent::gate_messages::cross_review_need_wait_reviewer_jobs(),
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
            } else if let Some(ref scorecard) = checkpoint.last_review_scorecard {
                let budget_exhausted = checkpoint.plan_reviewer_retry_count
                    >= crate::agent::checkpoint::MAX_PLAN_REVIEWER_RETRIES;
                if !scorecard.passes_gate() {
                    let reason = scorecard.gate_rejection_reason().unwrap_or_else(|| "审查评分未达放行门槛".to_string());
                    Some(DispatchResult {
                        success: false,
                        stdout: String::new(),
                        stderr: crate::agent::gate_messages::cross_review_scorecard_failed(&reason, budget_exhausted),
                        route: "plan_gatekeeper:scorecard_failed".to_string(),
                    })
                } else if checkpoint.last_review_blocking_count > 0 {
                    // 评分达标但存在未解决的 blocking 异议：分数不是唯一门槛，异议必须闭环
                    Some(DispatchResult {
                        success: false,
                        stdout: String::new(),
                        stderr: crate::agent::gate_messages::cross_review_blocking_findings_open(
                            checkpoint.last_review_blocking_count,
                            budget_exhausted,
                        ),
                        route: "plan_gatekeeper:blocking_findings_open".to_string(),
                    })
                } else {
                    checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
                    None
                }
            } else {
                // fail-closed：审查结论缺失评分卡时绝不默认放行（旧的 fail-open 暗道）
                let budget_exhausted = checkpoint.plan_reviewer_retry_count
                    >= crate::agent::checkpoint::MAX_PLAN_REVIEWER_RETRIES;
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
                checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
                None
            }
        }
    }
}

