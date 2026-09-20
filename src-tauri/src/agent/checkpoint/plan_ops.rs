//! AgentCheckpoint 方案门/阶段审查/工兵回执操作（impl 分块）。

use super::*;
use crate::agent::context::SandwichContext;
use crate::agent::control::HumanControl;
use crate::agent::types::{AgentInstruction, DispatchResult};
use crate::db::DbState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tauri::AppHandle;

/// agent_child_runs 的终态枚举（'running' 以外全部）
const TERMINAL_RUN_STATUSES: [&str; 6] = [
    "succeeded",
    "timed_out",
    "failed",
    "cancelled",
    "interrupted",
    "invalid_output",
];

/// 台账孤儿判定：该 run 永远不可能再产生交付动作——
/// 「已终态且回执已消费」= 交付/失败记账被绕过（如历史 failover 漏换 id、
/// 损坏回执被丢弃）；「台账行缺失」= 从未真正存在。
/// 已终态但回执未消费返回 false：回执仍在信箱等正常消费，不抢判。
fn child_run_is_delivery_orphan(conn: &rusqlite::Connection, run_id: &str) -> bool {
    match conn.query_row(
        "SELECT status, consumed_at FROM agent_child_runs WHERE id = ?1",
        rusqlite::params![run_id],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
    ) {
        Err(rusqlite::Error::QueryReturnedNoRows) => true,
        Ok((status, Some(_consumed_at))) => TERMINAL_RUN_STATUSES.contains(&status.as_str()),
        _ => false,
    }
}

impl AgentCheckpoint {

    /// Tech Lead 1v1 定稿修订：从审查交付 extra 提取 revised_* 扁平字段。
    /// 字段契约见 child_reviewer 方案评审条款（plan_review 派发时授权输出）；
    /// 全部为空返回 None（未修订），任一非空字段即构成有效修订。
    /// 长度/条数上限防恶意或失控输出污染执行方案。
    pub fn plan_revision_from(
        delivery: &crate::agent::child_reviewer::ReviewerDeliveryData,
    ) -> Option<PlanRevision> {
        let take_str = |key: &str, cap: usize| -> Option<String> {
            delivery
                .get_extra_str(key)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().take(cap).collect())
        };
        let target_files = delivery
            .get_extra("revised_target_files")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .take(50)
                    .map(|s| s.chars().take(300).collect())
                    .collect::<Vec<String>>()
            })
            .filter(|v| !v.is_empty());
        let rev = PlanRevision {
            plan_summary: take_str("revised_plan_summary", 2000),
            root_cause: take_str("revised_root_cause", 2000),
            target_files,
            intended_verification: take_str("revised_verification", 1000),
        };
        if rev.plan_summary.is_none()
            && rev.root_cause.is_none()
            && rev.target_files.is_none()
            && rev.intended_verification.is_none()
        {
            None
        } else {
            Some(rev)
        }
    }

    /// 读取方案审查最近派发的强类型参数快照（若存在且格式合法）
    pub fn plan_reviewer_dispatch_snapshot(&self) -> Option<crate::agent::child_reviewer::PlanReviewerDispatchSnapshot> {
        self.plan_reviewer_last_dispatch
            .as_deref()
            .and_then(crate::agent::child_reviewer::PlanReviewerDispatchSnapshot::from_json_str)
    }

    /// 写入方案审查最近派发的强类型参数快照（底层序列化为兼容的 JSON 字符串）
    pub fn set_plan_reviewer_dispatch_snapshot(
        &mut self,
        snapshot: &crate::agent::child_reviewer::PlanReviewerDispatchSnapshot,
    ) {
        self.plan_reviewer_last_dispatch = Some(snapshot.to_json_string());
    }

    /// 注册本轮并发审查任务批次（齐套屏障：记录预期全部交付的 run_ids）
    pub fn register_plan_reviewer_batch(&mut self, run_ids: &[String]) {
        self.plan_review_batch = PlanReviewBatchState {
            expected_run_ids: run_ids.iter().cloned().collect(),
            delivered_run_ids: std::collections::BTreeSet::new(),
            failed_run_ids: std::collections::BTreeMap::new(),
            deliveries: Vec::new(),
        };
        if run_ids.len() > 1 {
            // 已并发派发了多个审查员（联合会审），清除升级挂起状态
            self.plan_escalation_required = None;
        }
        self.plan_reviewer_consumed = false;
        self.last_review_blocking_count = 0;
        self.last_review_scorecard = None;
    }

    /// submit_plan 的一致状态提交：方案落账 + 阶段推进 + 预算预扣 + 批次注册 + 派发快照。
    /// 仅在审查子任务已成功 spawn 之后调用——spawn 是唯一可失败的外部副作用，
    /// 本方法只做内存态合并，由外层 save_checkpoint 的乐观锁 UPDATE 完成持久化原子提交。
    /// 预算上限须由调用方在 spawn 前预检；此处 reserve 兜底失败时仍完成批次注册，
    /// 避免「子任务在跑但父状态不认识它」的孤儿状态。
    pub fn commit_plan_submission(
        &mut self,
        submission: crate::agent::types::PlanSubmission,
        child_run_id: &str,
        snapshot: &crate::agent::child_reviewer::PlanReviewerDispatchSnapshot,
        step_id: usize,
    ) {
        self.submitted_plan = Some(submission);
        self.submitted_plan_step = Some(step_id);
        if !self.plan_phase.is_approved() {
            self.plan_phase = PlanPhase::Reviewing;
        }
        let _ = self.reserve_plan_review_round();
        self.register_plan_reviewer_batch(&[child_run_id.to_string()]);
        self.set_plan_reviewer_dispatch_snapshot(snapshot);
    }

    /// 登记审查任务失败/异常退出（超时、崩溃、格式错误等，收口防死锁）
    pub fn record_reviewer_failure_for_run(&mut self, run_id: &str, reason: &str) {
        self.plan_review_batch.record_failure(run_id, reason);
        if self.plan_review_batch.is_complete() {
            self.plan_reviewer_consumed = true;
        }
    }

    /// 审查者故障转移重派时，将旧任务 ID 替换为新重派的任务 ID
    pub fn replace_expected_reviewer_run_id(&mut self, old_id: &str, new_id: &str) {
        self.plan_review_batch.replace_expected_run_id(old_id, new_id);
    }

    /// 齐套屏障自愈对账（保险丝，非主流程）。
    /// 点名「人到齐没」之前顺手核对台账：凡是 agent_child_runs 里已终态
    /// 且回执已被消费、却仍挂在 expected 里的 run_id——说明它的交付/失败
    /// 记账被绕过（如历史 failover 漏换 id、损坏回执被丢弃）——该任务
    /// 永远不可能交付，按失败从预期中划掉；台账里根本不存在的 id 同理。
    /// 防的是「记账漂移让屏障等一个死人」的死锁：改名单的动作漏了，
    /// 点名动作自己纠错，最多卡一拍而非卡死。
    /// 已终态但回执未消费的不动——回执仍在信箱里等正常消费，不能抢判。
    /// 返回本次划掉的数量。
    pub fn reconcile_plan_reviewer_batch(&mut self, db: &DbState) -> usize {
        let pending: Vec<String> = self
            .plan_review_batch
            .expected_run_ids
            .iter()
            .filter(|id| {
                !self.plan_review_batch.delivered_run_ids.contains(*id)
                    && !self.plan_review_batch.failed_run_ids.contains_key(*id)
            })
            .cloned()
            .collect();
        if pending.is_empty() {
            return 0;
        }
        let Ok(conn) = db.0.lock() else { return 0 };
        let mut pruned = 0;
        for id in pending {
            if child_run_is_delivery_orphan(&conn, &id) {
                self.plan_review_batch
                    .record_failure(&id, "delivery_orphan");
                pruned += 1;
            }
        }
        if pruned > 0 && self.plan_review_batch.is_complete() {
            self.plan_reviewer_consumed = true;
        }
        pruned
    }

    /// 阶段审查在途名单自愈（与审查批次同款的保险丝）。
    /// stage_review_run_ids / rebuttal 指针里挂着「已终态且已消费」或
    /// 台账不存在的 run_id 时清掉——孤儿在途 id 会让
    /// has_in_flight_stage_review() 永远为真，后续所有阶段审查都派不出去。
    /// 只清在途名单，不动 blockers 计数：blocker 的结清走对质/仲裁/
    /// 僵尸治理原路径，不在这里抢判。返回清理数量。
    pub fn reconcile_stage_review_runs(&mut self, db: &DbState) -> usize {
        let mut candidates: Vec<String> = self.stage_review_run_ids.clone();
        for extra in [
            self.stage_review_run_id.as_ref(),
            self.stage_review_rebuttal_run_id.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if !candidates.contains(extra) {
                candidates.push(extra.clone());
            }
        }
        if candidates.is_empty() {
            return 0;
        }
        let Ok(conn) = db.0.lock() else { return 0 };
        let mut pruned = 0;
        for id in candidates {
            if child_run_is_delivery_orphan(&conn, &id) {
                self.remove_stage_review_run(&id);
                if self.stage_review_rebuttal_run_id.as_deref() == Some(id.as_str()) {
                    self.stage_review_rebuttal_run_id = None;
                }
                pruned += 1;
            }
        }
        pruned
    }

    /// 消费并记录一个审查员的交付结论（支持幂等去重与齐套现场真聚合）
    pub fn record_reviewer_delivery_for_run(
        &mut self,
        run_id: Option<&str>,
        delivery: &crate::agent::child_reviewer::ReviewerDeliveryData,
    ) {
        let mut delivery = delivery.clone();
        // 🌟 1v1 Tech Lead 方案评审扩展契约解析与定级联动
        if let Some(dec) = delivery.get_extra_str("decision") {
            if dec.eq_ignore_ascii_case("escalate") {
                if !crate::agent::plan_gate::review_panel_enabled() {
                    // 钉死 1v1 对照组（A/B 实验）：escalate 降级为 rebuttal，
                    // 保留原始裁决在 original_decision 供复盘，追加说明引导主脑回 1v1。
                    delivery.extra.insert(
                        "original_decision".to_string(),
                        serde_json::json!("escalate"),
                    );
                    delivery
                        .extra
                        .insert("decision".to_string(), serde_json::json!("rebuttal"));
                    delivery.extra.insert(
                        "panel_note".to_string(),
                        serde_json::json!(
                            "多专家联合会审面板在本配置下已禁用；本次 escalate 按 rebuttal 处理，请继续在 1v1 通道中消化其关注点并修订方案后重新提交评审。"
                        ),
                    );
                    eprintln!(
                        "[checkpoint] review panel disabled: Tech Lead escalate downgraded to rebuttal"
                    );
                } else {
                    let roles: Vec<String> = delivery
                        .get_extra("escalate_roles")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|i| i.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    self.plan_escalation_required = Some(roles);
                }
            } else if dec.eq_ignore_ascii_case("approved") {
                self.plan_escalation_required = None;
                // 若 Tech Lead 裁决 approved 但未附带底层四维评分卡，自动补齐高分通过卡，避免 scorecard_missing 门禁误杀
                if delivery.scorecard.is_none() {
                    eprintln!(
                        "[checkpoint] Tech Lead 裁决 approved 但未附带评分卡，已合成基线放行评分卡（synthesized，非真实审查评分）"
                    );
                    delivery.scorecard =
                        Some(crate::agent::child_reviewer::ReviewScorecard::synthesized_baseline());
                }
                // Tech Lead 定稿权：approved 携带 revised_* 修订字段时，以审查者版本
                // 覆盖 submitted_plan 成为最终执行方案（省掉"打回→重提"整轮往返）。
                if let Some(rev) = Self::plan_revision_from(&delivery) {
                    if let Some(sub) = self.submitted_plan.as_mut() {
                        if let Some(v) = rev.plan_summary {
                            sub.plan_summary = v;
                        }
                        if let Some(v) = rev.root_cause {
                            sub.root_cause = v;
                        }
                        if let Some(v) = rev.intended_verification {
                            sub.intended_verification = v;
                        }
                        if let Some(files) = &rev.target_files {
                            sub.target_files = files.clone();
                        }
                    }
                    if let Some(files) = rev.target_files {
                        self.approved_target_files = Some(files);
                    }
                    self.plan_finalized_by = Some("tech_lead".to_string());
                    eprintln!("[checkpoint] Tech Lead 已携带 revised_* 修订定稿，submitted_plan 已切换为审查者版本");
                }
            }
        }

        let is_new = self.plan_review_batch.record_delivery(run_id, delivery.clone());
        if !is_new {
            return; // 幂等：重复交付忽略，防重复计数
        }

        let (added, withdrawn, maintained) = delivery.objection_stats();
        self.review_objection_added += added;
        self.review_objection_withdrawn += withdrawn;
        self.review_objection_maintained += maintained;
        self.review_total_withdrawn += withdrawn;
        self.review_total_maintained += maintained;

        // 现场真聚合：blocking 缺陷取并集，四维评分取算术均值
        self.last_review_blocking_count = self
            .plan_review_batch
            .aggregate_unwithdrawn_blockings()
            .len() as u32;
        self.last_review_scorecard = self.plan_review_batch.aggregate_scorecard();

        // 齐套屏障：只有全部预期任务到齐才置 consumed 为 true
        if self.plan_review_batch.is_complete() {
            self.plan_reviewer_consumed = true;
        }
    }

    /// 消费并记录一轮合法的方案审查结论（兼容旧接口与单发交付）
    pub fn record_reviewer_delivery(&mut self, delivery: &crate::agent::child_reviewer::ReviewerDeliveryData) {
        self.record_reviewer_delivery_for_run(None, delivery);
    }

    /// 生成方案门禁状态的瞬态投影，供上下文装配层注入底部状态卡（事前可见）。
    pub fn plan_gate_projection(&self) -> crate::agent::context::types::PlanGateProjection {
        let unwithdrawn_blockings = self.plan_review_batch.aggregate_unwithdrawn_blockings();
        let blocking_count = if unwithdrawn_blockings.is_empty() {
            self.last_review_blocking_count
        } else {
            unwithdrawn_blockings.len() as u32
        };

        let blocking_summary = if blocking_count > 0 {
            if !unwithdrawn_blockings.is_empty() {
                let items: Vec<String> = unwithdrawn_blockings
                    .iter()
                    .take(3)
                    .map(|b| {
                        let loc = if b.location.is_empty() { "全局" } else { &b.location };
                        format!("[{}] {}: {}", b.id, loc, b.why.chars().take(40).collect::<String>())
                    })
                    .collect();
                Some(items.join("; "))
            } else {
                Some(format!("存在 {} 条 Blocking 阻塞缺陷", blocking_count))
            }
        } else {
            None
        };

        let batch_incomplete = !self.plan_review_batch.expected_run_ids.is_empty()
            && !self.plan_review_batch.is_complete();
        let delivered_reviewers = self.plan_review_batch.delivered_run_ids.len();
        let expected_reviewers = self.plan_review_batch.expected_run_ids.len();
        let budget_exhausted = self.plan_reviewer_retry_count >= MAX_PLAN_REVIEWER_RETRIES;
        let human_waived = self.human_waiver_text().is_some();
        // 探索预算三态投影：仅交叉审查 + Exploring + 尚未派审时有意义，
        // 其余阶段清零（渲染层对应分支不会被命中）。
        let exploration_active = self.plan_mode == PlanExecutionMode::CrossReview
            && self.plan_phase == PlanPhase::Exploring
            && self.plan_reviewer_retry_count == 0;
        let (exploration_soft, exploration_hard, exploration_extensions_left) = if exploration_active {
            (
                crate::agent::plan_gate::cross_review_exploration_soft_steps(),
                crate::agent::plan_gate::cross_review_exploration_effective_limit(self),
                crate::agent::plan_gate::cross_review_exploration_extensions_left(self),
            )
        } else {
            (0, 0, 0)
        };

        crate::agent::context::types::PlanGateProjection {
            mode: self.plan_mode.as_str().to_string(),
            phase: self.plan_phase.as_str().to_string(),
            reviewer_dispatched: self.plan_reviewer_retry_count > 0,
            reviewer_consumed: self.plan_reviewer_consumed,
            reviewer_budget_used: self.plan_reviewer_retry_count,
            reviewer_budget_max: MAX_PLAN_REVIEWER_RETRIES,
            blocking_count,
            blocking_summary,
            batch_incomplete,
            delivered_reviewers,
            expected_reviewers,
            budget_exhausted,
            human_waived,
            exploration_steps: if exploration_active { self.step_id } else { 0 },
            exploration_soft_limit: exploration_soft,
            exploration_hard_limit: exploration_hard,
            exploration_extensions_left,
        }
    }

    /// 统一的执行模式切换入口：规范化方案阶段（人工确认过的方案永久保留）
    /// 并同步 auto_pilot 兼容字段。所有模式切换都必须走这里，
    /// 避免 reconcile / plan_mode / auto_pilot 三处赋值散落导致漏改。
    pub fn set_execution_mode(&mut self, new_mode: PlanExecutionMode) {
        self.reconcile_plan_phase_for_mode_switch(new_mode);
        self.plan_mode = new_mode;
        self.auto_pilot = matches!(
            new_mode,
            PlanExecutionMode::Auto | PlanExecutionMode::CrossReview
        );
    }

    /// 模式切换守卫：
    /// 1. 从全自动或交叉审查切为方案审批模式时，自动授予（无人工留痕）的 Approved 不再可信，重置为 Reviewing；
    ///    人工批准过的方案（approved_plan_summary 有留痕）保持放行，避免误伤已确认状态。
    /// 2. 切换为交叉审查模式时，若当前的 Approved 是全自动盲放行（既无人工审批也未消费过审查结论），
    ///    同样重置为 Reviewing 要求完成审查推演；
    pub fn reconcile_plan_phase_for_mode_switch(&mut self, new_mode: PlanExecutionMode) {
        match new_mode {
            PlanExecutionMode::PlanApproval => {
                if self.plan_phase.is_approved() && self.approved_plan_summary.is_none() {
                    self.plan_phase = PlanPhase::Reviewing;
                }
            }
            PlanExecutionMode::CrossReview => {
                if self.plan_phase.is_approved()
                    && self.approved_plan_summary.is_none()
                    && !self.plan_reviewer_consumed
                {
                    self.plan_phase = PlanPhase::Reviewing;
                }
            }
            PlanExecutionMode::Auto => {}
        }
    }

    /// 预扣一轮方案审查预算。已到上限返回 Err（调用方应拒绝派发并引导人工放行）。
    ///
    /// 所有消耗审查轮次的路径（常规派发、resume_child 反驳对质）都必须走这里，
    /// 防止旁路无上限自增绕过熔断。
    pub fn reserve_plan_review_round(&mut self) -> Result<(), String> {
        let max_rounds = max_plan_reviewer_rounds();
        if self.plan_reviewer_retry_count >= max_rounds {
            return Err(format!("方案审查轮次预算已耗尽（上限 {} 轮）", max_rounds));
        }
        self.plan_reviewer_retry_count += 1;
        self.plan_reviewer_consumed = false;
        self.last_review_blocking_count = 0;
        self.last_review_scorecard = None;
        self.plan_review_batch = PlanReviewBatchState::default();
        Ok(())
    }

    /// 判定是否存在在途的阶段审查任务（支持单审与联合会审）
    pub fn has_in_flight_stage_review(&self) -> bool {
        self.stage_review_run_id.is_some()
            || !self.stage_review_run_ids.is_empty()
            || self.stage_review_rebuttal_run_id.is_some()
    }

    /// 判定指定任务 ID 是否属于当前在途的阶段审查任务
    pub fn matches_stage_review_run(&self, run_id: &str) -> bool {
        self.stage_review_run_id.as_deref() == Some(run_id)
            || self.stage_review_run_ids.iter().any(|r| r == run_id)
    }

    /// 判定指定任务 ID 是否属于在途或已完成的阶段审查任务（用于对质目标识别）
    pub fn is_stage_review_target(&self, run_id: &str) -> bool {
        self.matches_stage_review_run(run_id)
            || self.stage_review_rebuttal_run_id.as_deref() == Some(run_id)
            || self.stage_review_completed_run_ids.iter().any(|r| r == run_id)
    }

    /// 记录已完成的阶段审查或对质任务 ID（最多保留 20 条）
    pub fn record_stage_review_completed(&mut self, run_id: &str) {
        if !self.stage_review_completed_run_ids.iter().any(|r| r == run_id) {
            self.stage_review_completed_run_ids.push(run_id.to_string());
            if self.stage_review_completed_run_ids.len() > 20 {
                self.stage_review_completed_run_ids.remove(0);
            }
        }
    }

    /// 移除已完成的阶段审查任务 ID；若列表中仍有其他任务则继续维持，否则清空
    pub fn remove_stage_review_run(&mut self, run_id: &str) {
        self.stage_review_run_ids.retain(|r| r != run_id);
        if self.stage_review_run_id.as_deref() == Some(run_id) {
            self.stage_review_run_id = self.stage_review_run_ids.first().cloned();
        }
        self.record_stage_review_completed(run_id);
    }

    /// 清空全部阶段审查在途任务 ID
    pub fn clear_stage_review_runs(&mut self) {
        self.stage_review_run_id = None;
        self.stage_review_run_ids.clear();
        self.stage_review_rebuttal_run_id = None;
    }

    /// 审查者技术失败的审查轮次退款：超时/取消/中断/非法输出/请求层失败不是实质性审查，
    /// 不应烧掉请求方的预算——技术失败和业务驳回是两笔账（实测教训：error_kind 落库值是
    /// "timeout" 而非 "timed_out"，且 invalid_output 未纳入退款，glm 实弹中模型为
    /// 审查员的故障买了单；402 欠费等 request_or_stream 同属零产出 infra 失败）。
    /// 退款封顶 3 次，防止持续故障下预算永不耗尽、模型无限空转。
    /// kind 归一由 ChildErrorKind::parse 承担（"timed_out"/"timeout" 同义）。
    pub fn refund_infra_failed_review_round(
        &mut self,
        error_kind: Option<crate::agent::types::ChildErrorKind>,
    ) -> bool {
        let infra = error_kind
            .map(|k| k.is_refundable())
            .unwrap_or(false);
        if !infra || self.reviewer_budget_refunds >= 3 || self.plan_reviewer_retry_count == 0 {
            return false;
        }
        self.plan_reviewer_retry_count -= 1;
        self.reviewer_budget_refunds += 1;
        true
    }

    /// 阶段审查退款统一结算（批循环结束后调用一次）。
    /// 为什么不能在回执处理循环内逐条结算：completed 回执按序处理，本条 infra
    /// 失败专家的 run_id 移除时，后续成功专家的 run_id 尚未移除，
    /// has_in_flight_stage_review() 会误判为 true 而永久错过退款窗口。
    /// 仅当在途清空且无 blocker 留存（本轮无实质产出）时才退还预算；
    /// 若已有专家交付了真实 blocking，本轮审查有实质输出，不退款。
    /// 返回 true = 实际完成了一次退款。
    pub fn settle_stage_review_refund(&mut self) -> bool {
        if !self.has_in_flight_stage_review()
            && self.stage_review_blockers_open == 0
            && self.reviewer_budget_refunds < 3
            && self.stage_review_budget_used > 0
        {
            self.stage_review_budget_used -= 1;
            self.reviewer_budget_refunds += 1;
            true
        } else {
            false
        }
    }

    /// 对质失败自动降级：blocking 异议经对质仍被审查员维持（confirmed 证据拒不撤回）
    /// 时累计失败次数；达到 threshold 后把未解决 blockers 降级为 final_only 记账
    /// 带入收尾审查，安全扣减 blockers_open 放行写入。返回 true = 本次触发降级。
    /// 空行（异议已全部撤回/解决）时重置计数。
    pub fn note_stage_review_confrontation_maintained(
        &mut self,
        blocker_lines: Vec<String>,
        threshold: u32,
    ) -> bool {
        self.note_stage_review_confrontation_maintained_for(None, blocker_lines, threshold)
    }

    /// 附带目标专家隔离的对质失败自动降级：不同专家独立计数，防止联合会审交叉借分。
    pub fn note_stage_review_confrontation_maintained_for(
        &mut self,
        target_child_id: Option<&str>,
        blocker_lines: Vec<String>,
        threshold: u32,
    ) -> bool {
        if blocker_lines.is_empty() {
            self.stage_review_confrontation_failures = 0;
            self.stage_review_confrontation_target = None;
            return false;
        }
        // 目标切换时，独立重置计数器，防止联合会审中不同专家的失败互相借分
        if let Some(target) = target_child_id {
            if self.stage_review_confrontation_target.as_deref() != Some(target) {
                self.stage_review_confrontation_failures = 0;
                self.stage_review_confrontation_target = Some(target.to_string());
            }
        }
        self.stage_review_confrontation_failures += 1;
        if self.stage_review_confrontation_failures < threshold {
            return false;
        }
        let count = blocker_lines.len() as u32;
        for line in blocker_lines {
            let tagged = format!("【对质未撤回自动降级】{line}");
            if !self
                .stage_review_final_only
                .iter()
                .any(|existing| existing == &tagged)
            {
                self.stage_review_final_only.push(tagged);
            }
        }
        // 安全扣减：仅扣减当前对质降级的条数，不可无条件全局置 0，防止未对质专家的 blocker 被静默清空
        self.stage_review_blockers_open = self.stage_review_blockers_open.saturating_sub(count);
        if self.stage_review_blockers_open == 0 {
            self.stage_review_blocker_stage = None;
            self.stage_review_blocker_source_run_id = None;
        }
        self.stage_review_confrontation_failures = 0;
        self.stage_review_confrontation_target = None;
        true
    }

    /// 僵尸 blocker 清账：blocker 出生阶段已被宿主判分越过 → 该阶段实现已被客观
    /// 判定通过，遗留的阶段审查拦截自动清零并以 final_only 记账带入收尾。
    /// 返回 Some(说明文案) 表示发生了清理。oracle 进度缺失/未越过时不动
    /// （fail-closed：对质与人工仲裁仍是唯一出口）。旧快照无出生阶段字段时同样不动。
    pub fn clear_stale_stage_blockers(&mut self, oracle_index: u32) -> Option<String> {
        if self.stage_review_blockers_open == 0 {
            return None;
        }
        let born = self.stage_review_blocker_stage?;
        if oracle_index <= born {
            return None;
        }
        self.stage_review_final_only.push(format!(
            "🧟 僵尸 blocker 清账：出生于阶段索引 {born} 的 {} 条 blocking_regardless，在宿主判分推进到索引 {oracle_index} 后仍未清零，自动降级 final_only 带入收尾",
            self.stage_review_blockers_open
        ));
        self.stage_review_blockers_open = 0;
        self.stage_review_blocker_stage = None;
        self.stage_review_blocker_source_run_id = None;
        Some(format!(
            "⏰ 宿主判分已越过历史阶段（blocker 出生索引 {born} < 当前进度 {oracle_index}），遗留的阶段审查 blocker 属僵尸拦截，已自动清账放行；遗留项以 final_only 带入收尾审查。"
        ))
    }

    /// 回滚一次「未产生任何后台任务」的审查派发：归还审查轮次并恢复派发前的消费状态。
    ///
    /// 派发失败（角色卡未授权/不存在、参数非法等）不应消耗审查预算，否则额度耗尽后
    /// 既无法重新派发、又永远等不到结论，交叉审查模式会永久阻塞写入。
    pub fn rollback_plan_review_dispatch(&mut self, pre_dispatch_consumed: bool) {
        self.plan_reviewer_retry_count = self.plan_reviewer_retry_count.saturating_sub(1);
        self.plan_reviewer_consumed = pre_dispatch_consumed;
    }
}
