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

    /// 登记本轮审查批次锚点：成员身份由台账 batch_id 承担，
    /// checkpoint 只记"当前批次是谁"+"派发时的既定成员数"两个标量。
    /// failover/对质续跑自动继承 batch_id 加入本批，无需手动换名。
    pub fn set_plan_review_batch(&mut self, batch_id: impl Into<String>, member_count: usize) {
        self.plan_review_batch_id = Some(batch_id.into());
        self.plan_review_expected_count = member_count as u32;
        self.plan_review_deliveries.clear();
        if member_count > 1 {
            // 已并发派发了多个审查员（联合会审），清除升级挂起状态
            self.plan_escalation_required = None;
        }
        self.plan_reviewer_consumed = false;
        self.last_review_blocking_count = 0;
        self.last_review_scorecard = None;
    }

    /// ── 台账推导（成员/在途/齐套一律查 agent_child_runs，唯一事实源）──
    /// 待决 = 批次成员行存在且回执未消费（consumed_at IS NULL：
    /// running 在途 + 终态未取件都算待决；回执落库消费后即出列）
    pub(crate) fn plan_review_pending_count(&self, db: &DbState) -> usize {
        let Some(ref batch_id) = self.plan_review_batch_id else { return 0 };
        db.0.lock().ok().and_then(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM agent_child_runs WHERE batch_id = ?1 AND consumed_at IS NULL",
                rusqlite::params![batch_id],
                |r| r.get::<_, i64>(0),
            ).ok()
        }).unwrap_or(0) as usize
    }

    /// 批次成员总数（台账实有——含 failover 备胎与对质续跑自动入批的成员）
    pub(crate) fn plan_review_member_count(&self, db: &DbState) -> usize {
        let Some(ref batch_id) = self.plan_review_batch_id else { return 0 };
        db.0.lock().ok().and_then(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM agent_child_runs WHERE batch_id = ?1",
                rusqlite::params![batch_id],
                |r| r.get::<_, i64>(0),
            ).ok()
        }).unwrap_or(0) as usize
    }

    /// 齐套屏障：批次有成员且无人待决。
    /// 死锁类问题结构性消失：failover 备胎自带 batch_id 自动入批，
    /// 终态成员消费后自然出列，不存在"名单忘了换名"的窗口。
    pub(crate) fn plan_review_batch_complete(&self, db: &DbState) -> bool {
        self.plan_review_member_count(db) > 0 && self.plan_review_pending_count(db) == 0
    }

    /// 已交付成员数（消费过且成功态）
    pub(crate) fn plan_review_delivered_count(&self, db: &DbState) -> usize {
        let Some(ref batch_id) = self.plan_review_batch_id else { return 0 };
        db.0.lock().ok().and_then(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM agent_child_runs WHERE batch_id = ?1 AND consumed_at IS NOT NULL AND status = 'succeeded'",
                rusqlite::params![batch_id],
                |r| r.get::<_, i64>(0),
            ).ok()
        }).unwrap_or(0) as usize
    }

    /// 已失败成员数（消费过但非成功终态）
    pub(crate) fn plan_review_failed_count(&self, db: &DbState) -> usize {
        let Some(ref batch_id) = self.plan_review_batch_id else { return 0 };
        db.0.lock().ok().and_then(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM agent_child_runs WHERE batch_id = ?1 AND consumed_at IS NOT NULL AND status != 'succeeded'",
                rusqlite::params![batch_id],
                |r| r.get::<_, i64>(0),
            ).ok()
        }).unwrap_or(0) as usize
    }

    /// run_id 是否属于当前审查批次（防止旧批次的迟到回执污染当前批状态）
    pub(crate) fn run_belongs_to_plan_batch(&self, db: &DbState, run_id: &str) -> bool {
        let Some(ref batch_id) = self.plan_review_batch_id else { return false };
        db.0.lock().ok().and_then(|conn| {
            conn.query_row(
                "SELECT batch_id FROM agent_child_runs WHERE id = ?1",
                rusqlite::params![run_id],
                |r| r.get::<_, Option<String>>(0),
            ).ok().flatten()
        }).as_deref() == Some(batch_id.as_str())
    }

    /// 阶段审查在途判定：当前阶段批次存在未消费成员（含对质续跑）
    pub(crate) fn stage_review_in_flight(&self, db: &DbState) -> bool {
        self.stage_review_in_flight_remaining(db) > 0
    }

    pub(crate) fn stage_review_in_flight_remaining(&self, db: &DbState) -> usize {
        let Some(ref batch_id) = self.stage_review_batch_id else { return 0 };
        db.0.lock().ok().and_then(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM agent_child_runs WHERE batch_id = ?1 AND consumed_at IS NULL",
                rusqlite::params![batch_id],
                |r| r.get::<_, i64>(0),
            ).ok()
        }).unwrap_or(0) as usize
    }

    /// 判定 run_id 是否属阶段审查语义（含已完成的——对质目标是已交付的审查员）：
    /// 优先 purpose 盖章（stage_review/rebuttal），兜底批次归属匹配
    /// （旧库未盖 purpose 的行靠 batch_id 仍能命中）。
    pub(crate) fn is_stage_review_target(&self, db: &DbState, run_id: &str) -> bool {
        db.0.lock().ok().and_then(|conn| {
            conn.query_row(
                "SELECT purpose, batch_id FROM agent_child_runs WHERE id = ?1",
                rusqlite::params![run_id],
                |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)),
            ).ok()
        }).map(|(purpose, batch)| {
            matches!(purpose.as_deref(), Some("stage_review") | Some("rebuttal"))
                || batch == self.stage_review_batch_id && batch.is_some()
        }).unwrap_or(false)
    }

    /// run_id 是否是一条对质续跑任务（purpose='rebuttal' 盖章）
    pub(crate) fn is_rebuttal_run(&self, db: &DbState, run_id: &str) -> bool {
        db.0.lock().ok().and_then(|conn| {
            conn.query_row(
                "SELECT purpose FROM agent_child_runs WHERE id = ?1",
                rusqlite::params![run_id],
                |r| r.get::<_, Option<String>>(0),
            ).ok().flatten()
        }).as_deref() == Some("rebuttal")
    }

    /// 门禁提示的 resume 指路：优先 blocker 产出者，否则当前阶段批次的任一成员
    pub(crate) fn stage_review_resume_hint(&self, db: &DbState) -> Option<String> {
        if let Some(ref src) = self.stage_review_blocker_source_run_id {
            return Some(src.clone());
        }
        let batch_id = self.stage_review_batch_id.as_ref()?;
        db.0.lock().ok().and_then(|conn| {
            conn.query_row(
                "SELECT id FROM agent_child_runs WHERE batch_id = ?1 ORDER BY started_at DESC LIMIT 1",
                rusqlite::params![batch_id],
                |r| r.get::<_, String>(0),
            ).ok()
        })
    }

    /// 旧版手写名单 → 台账锚点收养（升级一次性迁移）：
    /// 旧 checkpoint 反序列化出的 expected/stage id 列表逐一盖章 batch_id，
    /// 之后成员身份全部由台账推导。新存档不再携带这些字段（skip_serializing）。
    pub(crate) fn adopt_legacy_membership(&mut self, db: &DbState) {
        let Ok(conn) = db.0.lock() else { return };
        // 先收养阶段审查名单（其成员 kind='review' 与方案批次同型，先盖章防止被方案批次兜底吞并）
        let stage_legacy: Vec<String> = self
            .stage_review_run_ids
            .iter()
            .cloned()
            .chain(self.stage_review_run_id.iter().cloned())
            .chain(self.stage_review_completed_run_ids.iter().cloned())
            .collect();
        if self.stage_review_batch_id.is_none() && (!stage_legacy.is_empty() || self.stage_review_rebuttal_run_id.is_some()) {
            let batch_id = format!("stage-batch-legacy-{}", uuid::Uuid::new_v4());
            for id in &stage_legacy {
                let _ = conn.execute(
                    "UPDATE agent_child_runs SET batch_id=?1, purpose=COALESCE(purpose,'stage_review') WHERE id=?2",
                    rusqlite::params![batch_id, id],
                );
            }
            if let Some(ref reb) = self.stage_review_rebuttal_run_id {
                let _ = conn.execute(
                    "UPDATE agent_child_runs SET batch_id=?1, purpose='rebuttal' WHERE id=?2",
                    rusqlite::params![batch_id, reb],
                );
            }
            self.stage_review_batch_id = Some(batch_id);
        }
        self.stage_review_run_id = None;
        self.stage_review_run_ids.clear();
        self.stage_review_rebuttal_run_id = None;
        self.stage_review_completed_run_ids.clear();

        // 再收养方案审查名单
        if self.plan_review_batch_id.is_none() && !self.plan_review_batch.expected_run_ids.is_empty() {
            let batch_id = format!("batch-legacy-{}", uuid::Uuid::new_v4());
            for id in &self.plan_review_batch.expected_run_ids {
                let _ = conn.execute(
                    "UPDATE agent_child_runs SET batch_id=?1, purpose=COALESCE(purpose,'plan_review') WHERE id=?2",
                    rusqlite::params![batch_id, id],
                );
            }
            // 兜底收养：同父 run 下未消费、无批次的审查行（failover 备胎等历史漏登记）。
            // kind 枚举是 child_reviewer/child_writer/child_worker；
            // purpose 口径上 review=方案审查历史值，stage_review 行已被前一步收养盖章排除在外。
            let _ = conn.execute(
                "UPDATE agent_child_runs SET batch_id=?1, purpose=COALESCE(purpose,'plan_review')
                 WHERE parent_run_id=?2 AND batch_id IS NULL AND consumed_at IS NULL
                   AND kind IN ('child_reviewer','review','reviewer')
                   AND (purpose IS NULL OR purpose IN ('review','plan_review'))",
                rusqlite::params![batch_id, self.run_id],
            );
            self.plan_review_batch_id = Some(batch_id);
            self.plan_review_expected_count = self
                .plan_review_batch
                .expected_run_ids
                .len()
                .max(self.plan_review_member_count(db)) as u32;
        }
        self.plan_review_batch = Default::default();
    }

    /// 步边界状态刷新：收养旧名单 + 按台账重算 consumed 标志。
    /// 双向重算——批次齐套置 true；对质续跑等新成员入批后自动回落 false
    /// （旧手写名单时代对质续跑不在名单里，标志恒 true 掩盖了在途工作）。
    pub(crate) fn refresh_consumed_flags(&mut self, db: &DbState) {
        self.adopt_legacy_membership(db);
        if self.plan_review_batch_id.is_some() {
            self.plan_reviewer_consumed = self.plan_review_batch_complete(db);
        }
    }

    /// submit_plan 的一致状态提交：方案落账 + 阶段推进 + 预算预扣 + 批次注册 + 派发快照。
    /// 仅在审查子任务已成功 spawn 之后调用——spawn 是唯一可失败的外部副作用，
    /// 本方法只做内存态合并，由外层 save_checkpoint 的乐观锁 UPDATE 完成持久化原子提交。
    /// 预算上限须由调用方在 spawn 前预检；此处 reserve 兜底失败时仍完成批次注册，
    /// 避免「子任务在跑但父状态不认识它」的孤儿状态。
    pub fn commit_plan_submission(
        &mut self,
        submission: crate::agent::types::PlanSubmission,
        batch_id: &str,
        snapshot: &crate::agent::child_reviewer::PlanReviewerDispatchSnapshot,
        step_id: usize,
    ) {
        self.submitted_plan = Some(submission);
        self.submitted_plan_step = Some(step_id);
        // 新提交一律回到审查中：批准后修订（revision_reason 通道）同样须重审，
        // 旧放行状态不得沿用——写入门禁随之重新生效，直至新方案过审。
        self.plan_phase = PlanPhase::Reviewing;
        // 被取代方案的批准痕迹作废：旧 target_files 不再约束新方案的 diff 对账，
        // 人工批准留痕属于旧方案，修订未经人工确认不得继承。
        self.approved_target_files = None;
        self.approved_plan_summary = None;
        self.plan_unreviewed_release = false;
        let _ = self.reserve_plan_review_round();
        // 批次锚点 = 快照派发时盖在行上的 batch_id（plan_submit 生成后透传）
        self.set_plan_review_batch(batch_id, 1);
        self.set_plan_reviewer_dispatch_snapshot(snapshot);
    }

    /// 登记审查任务失败/异常退出（超时、崩溃、格式错误等）。
    /// 批次归属校验：迟到回执属旧批次时不污染当前批的齐套判定。
    pub fn record_reviewer_failure_for_run(&mut self, db: &DbState, run_id: &str) {
        if self.run_belongs_to_plan_batch(db, run_id)
            && self.plan_review_batch_complete(db)
        {
            self.plan_reviewer_consumed = true;
        }
    }

    /// 消费并记录一个审查员的交付结论（支持幂等去重与齐套现场真聚合）
    pub fn record_reviewer_delivery_for_run(
        &mut self,
        db: &DbState,
        run_id: Option<&str>,
        delivery: &crate::agent::child_reviewer::ReviewerDeliveryData,
    ) {
        // 批次归属守卫必须最先执行：迟到回执属其他批次时整体忽略——
        // 否则其 decision/blocking 会污染当前批的升级判定与聚合计数。
        // fail-open：当前批锚点不存在时无批可污染，照常受理。
        let in_batch = match run_id {
            Some(id) => {
                self.plan_review_batch_id.is_none()
                    || self.run_belongs_to_plan_batch(db, id)
            }
            None => true, // 无 run_id 的旧接口兼容路径
        };
        if !in_batch {
            return;
        }

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
                // 审查员自洽性机械校验：root_cause_verdict=insufficient 是审查员
                // 自己宣布「该根因修好后题面症状仍会复现」——此时 approved 属于
                // 输出内部矛盾（定稿一个自认不成立的方向），按 escalate 同款
                // 模式降级为 rebuttal，原始裁决留档。这不是替审查员做语义判断，
                // 是强制其两条自述结论不得互斥。
                if delivery
                    .get_extra_str("root_cause_verdict")
                    .map(|v| v.eq_ignore_ascii_case("insufficient"))
                    .unwrap_or(false)
                {
                    delivery.extra.insert(
                        "original_decision".to_string(),
                        serde_json::json!("approved"),
                    );
                    delivery
                        .extra
                        .insert("decision".to_string(), serde_json::json!("rebuttal"));
                    delivery.extra.insert(
                        "panel_note".to_string(),
                        serde_json::json!(
                            "审查员自判病根核对=insufficient（根因修好后症状仍会复现），与 approved 裁决矛盾——宿主按自洽性规则降级为 rebuttal：请按病根核对结论重新定位根因后修订方案再提交。"
                        ),
                    );
                    eprintln!(
                        "[checkpoint] root_cause_verdict=insufficient 与 approved 矛盾，按自洽性规则降级为 rebuttal"
                    );
                }
                // 降级后的 decision 已非 approved——下面的通过卡合成与定稿覆盖
                // 必须按【生效裁决】判定，否则 rebuttal 会领到放行评分卡。
                let still_approved = delivery
                    .get_extra_str("decision")
                    .map(|d| d.eq_ignore_ascii_case("approved"))
                    .unwrap_or(false);
                if still_approved {
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
        }

        let is_new = run_id
            .map(|id| {
                self.plan_review_deliveries
                    .insert(id.to_string(), delivery.clone())
                    .is_none()
            })
            .unwrap_or_else(|| {
                // 无 run_id 的旧接口：附加以保持兼容
                true
            });
        if !is_new {
            return; // 幂等：重复交付忽略，防重复计数
        }
        if run_id.is_none() {
            self.plan_review_deliveries.insert(
                format!("anon-{}", self.plan_review_deliveries.len()),
                delivery.clone(),
            );
        }

        let (added, withdrawn, maintained) = delivery.objection_stats();
        self.review_objection_added += added;
        self.review_objection_withdrawn += withdrawn;
        self.review_objection_maintained += maintained;
        self.review_total_withdrawn += withdrawn;
        self.review_total_maintained += maintained;

        // 现场真聚合：blocking 缺陷取并集，四维评分取算术均值
        self.last_review_blocking_count = self
            .aggregate_unwithdrawn_blockings()
            .len() as u32;
        self.last_review_scorecard = self.aggregate_scorecard();

        // 齐套屏障：台账推导——批次全部成员回执已消费才置 consumed
        if self.plan_review_batch_complete(db) {
            self.plan_reviewer_consumed = true;
        }
        // 真结论落账则撤销「未经审查放行」标记：此前的放行已被迟到的审查补上，
        // 留着旧标记会让交付被迫披露一个已不成立的事实。
        if !self.plan_review_deliveries.is_empty() {
            self.plan_unreviewed_release = false;
        }
    }

    /// 现场真聚合：提取所有已交付审查员的未撤回 blocking 级缺陷（并集）
    pub fn aggregate_unwithdrawn_blockings(&self) -> Vec<crate::agent::child_reviewer::ReviewFinding> {
        let mut blockings = Vec::new();
        for del in self.plan_review_deliveries.values() {
            for f in &del.findings {
                if f.severity.eq_ignore_ascii_case("blocking")
                    && !del.withdrawn_findings.iter().any(|w| w == &f.id)
                {
                    blockings.push(f.clone());
                }
            }
        }
        blockings
    }

    /// 现场真聚合：计算所有已交付审查员 Scorecard 的算术平均值（真平均）
    pub fn aggregate_scorecard(&self) -> Option<crate::agent::child_reviewer::ReviewScorecard> {
        let scorecards: Vec<&crate::agent::child_reviewer::ReviewScorecard> = self
            .plan_review_deliveries
            .values()
            .filter_map(|d| d.scorecard.as_ref())
            .collect();
        if scorecards.is_empty() {
            return None;
        }
        let n = scorecards.len() as u32;
        let correctness = scorecards.iter().map(|s| s.correctness).sum::<u32>() / n;
        let risk = scorecards.iter().map(|s| s.risk).sum::<u32>() / n;
        let style = scorecards.iter().map(|s| s.style).sum::<u32>() / n;
        let covs: Vec<u32> = scorecards.iter().filter_map(|s| s.test_coverage).collect();
        let test_coverage = if covs.is_empty() {
            None
        } else {
            Some(covs.iter().sum::<u32>() / covs.len() as u32)
        };
        Some(crate::agent::child_reviewer::ReviewScorecard {
            correctness,
            risk,
            style,
            test_coverage,
            null_reason: scorecards.iter().find_map(|s| s.null_reason.clone()),
            synthesized: scorecards.iter().any(|s| s.synthesized),
        })
    }

    /// 消费并记录一轮合法的方案审查结论（兼容旧接口与单发交付）
    pub fn record_reviewer_delivery(&mut self, db: &DbState, delivery: &crate::agent::child_reviewer::ReviewerDeliveryData) {
        self.record_reviewer_delivery_for_run(db, None, delivery);
    }

    /// 生成方案门禁状态的瞬态投影，供上下文装配层注入底部状态卡（事前可见）。
    /// 无 db 访问：批次口径用锚点标量（expected_count）+ 交付内容缓存 + consumed 标志，
    /// 与台账推导口径一致（delivered=已登记交付数，expected=派发时既定成员数）。
    pub fn plan_gate_projection(&self) -> crate::agent::context::types::PlanGateProjection {
        let unwithdrawn_blockings = self.aggregate_unwithdrawn_blockings();
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

        // 齐套投影：批次锚点存在且 consumed 未置位 = 尚有成员在途
        let batch_incomplete = self.plan_review_batch_id.is_some()
            && !self.plan_reviewer_consumed;
        let delivered_reviewers = self.plan_review_deliveries.len();
        let expected_reviewers = self.plan_review_expected_count as usize;
        let budget_exhausted = self.plan_reviewer_retry_count >= MAX_PLAN_REVIEWER_RETRIES;
        let human_waived = self.human_waiver_text().is_some();

        crate::agent::context::types::PlanGateProjection {
            mode: self.plan_mode.as_str().to_string(),
            phase: self.plan_phase.as_str().to_string(),
            plan_required: self
                .triage_verdict
                .as_ref()
                .map_or(false, |v| v.requires_plan_gate())
                || matches!(
                    self.plan_mode,
                    PlanExecutionMode::CrossReview | PlanExecutionMode::PlanApproval
                ),
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
            unreviewed_release: self.plan_unreviewed_release,
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
        self.plan_review_deliveries.clear();
        Ok(())
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
    /// 失败专家刚被消费时后续成功专家的回执尚未被消费，
    /// 台账在途判定会（正确地）视为仍有在途而推迟到批后统一结算。
    /// 仅当在途清空且无 blocker 留存（本轮无实质产出）时才退还预算；
    /// 若已有专家交付了真实 blocking，本轮审查有实质输出，不退款。
    /// 返回 true = 实际完成了一次退款。
    pub fn settle_stage_review_refund(&mut self, db: &DbState) -> bool {
        if !self.stage_review_in_flight(db)
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
