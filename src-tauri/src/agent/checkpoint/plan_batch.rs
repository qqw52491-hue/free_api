//! PlanReviewBatchState 计划评审批次状态。

use super::*;
use crate::agent::context::SandwichContext;
use crate::agent::control::HumanControl;
use crate::agent::types::{AgentInstruction, DispatchResult};
use crate::db::DbState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tauri::AppHandle;

/// 方案审查并发批次状态（支持多审查员并发审查：齐套屏障、幂等去重、原始交付归集与现场真聚合）
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]

pub struct PlanReviewBatchState {
    /// 齐套屏障：本批派发的所有审查任务 child_run_id
    #[serde(default)]
    pub expected_run_ids: std::collections::BTreeSet<String>,
    /// 幂等保护：已收割的审查任务 child_run_id（去重防重复累加）
    #[serde(default)]
    pub delivered_run_ids: std::collections::BTreeSet<String>,
    /// 异常收口：记录失败/异常终止的任务及原因 (run_id -> error_reason)
    #[serde(default)]
    pub failed_run_ids: std::collections::BTreeMap<String, String>,
    /// 存储各个审查员的原始交付数据（保留角色与详细 findings，供现场真聚合和精准归因）
    #[serde(default)]
    pub deliveries: Vec<crate::agent::child_reviewer::ReviewerDeliveryData>,
}

impl PlanReviewBatchState {
    pub fn is_empty(&self) -> bool {
        self.expected_run_ids.is_empty() && self.deliveries.is_empty() && self.failed_run_ids.is_empty()
    }

    /// 判定本批审查任务是否已全部收口（成功交付或已确认为异常失败）
    pub fn is_complete(&self) -> bool {
        if self.expected_run_ids.is_empty() {
            // 没有登记 expected（如单审兼容或历史状态）：以是否有交付为准
            !self.deliveries.is_empty()
        } else {
            self.expected_run_ids.iter().all(|id| {
                self.delivered_run_ids.contains(id) || self.failed_run_ids.contains_key(id)
            })
        }
    }

    /// 还有多少个在途审查任务尚未收口（既未成功交付，亦未确认为异常失败）
    pub fn pending_count(&self) -> usize {
        self.expected_run_ids
            .iter()
            .filter(|id| !self.delivered_run_ids.contains(*id) && !self.failed_run_ids.contains_key(*id))
            .count()
    }

    /// 登记任务失败/异常终止（超时、崩溃、解析失败等，收口防死锁）
    pub fn record_failure(&mut self, run_id: &str, reason: &str) -> bool {
        if self.delivered_run_ids.contains(run_id) {
            return false;
        }
        self.failed_run_ids.insert(run_id.to_string(), reason.to_string()).is_none()
    }

    /// 故障转移重派时，无缝将旧 run_id 替换为新备用 run_id，齐套屏障无感转移
    pub fn replace_expected_run_id(&mut self, old_id: &str, new_id: &str) -> bool {
        if self.expected_run_ids.remove(old_id) {
            self.expected_run_ids.insert(new_id.to_string());
            self.failed_run_ids.remove(old_id);
            true
        } else {
            false
        }
    }

    /// 登记新交付（幂等：同一个 run_id 只记录一次）
    pub fn record_delivery(
        &mut self,
        run_id: Option<&str>,
        delivery: crate::agent::child_reviewer::ReviewerDeliveryData,
    ) -> bool {
        if let Some(id) = run_id {
            if !self.delivered_run_ids.insert(id.to_string()) {
                return false; // 幂等保护：重复交付直接忽略
            }
            self.failed_run_ids.remove(id); // 若此前暂记过失败，以最新成功交付为准
        }
        self.deliveries.push(delivery);
        true
    }

    /// 现场真聚合：提取所有审查员提出的未撤回 blocking 级缺陷（并集）
    pub fn aggregate_unwithdrawn_blockings(&self) -> Vec<crate::agent::child_reviewer::ReviewFinding> {
        let mut blockings = Vec::new();
        for del in &self.deliveries {
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

    /// 现场真聚合：计算所有审查员 Scorecard 的真实算术平均值（真平均）
    pub fn aggregate_scorecard(&self) -> Option<crate::agent::child_reviewer::ReviewScorecard> {
        let scorecards: Vec<&crate::agent::child_reviewer::ReviewScorecard> = self
            .deliveries
            .iter()
            .filter_map(|d| d.scorecard.as_ref())
            .collect();

        if scorecards.is_empty() {
            return None;
        }

        let n = scorecards.len() as u32;
        let sum_correctness: u32 = scorecards.iter().map(|s| s.correctness).sum();
        let sum_risk: u32 = scorecards.iter().map(|s| s.risk).sum();
        let sum_style: u32 = scorecards.iter().map(|s| s.style).sum();

        let mut tc_sum = 0u32;
        let mut tc_count = 0u32;
        for s in &scorecards {
            if let Some(tc) = s.test_coverage {
                tc_sum += tc;
                tc_count += 1;
            }
        }

        let test_coverage = if tc_count > 0 {
            Some(tc_sum / tc_count)
        } else {
            None
        };

        Some(crate::agent::child_reviewer::ReviewScorecard {
            correctness: sum_correctness / n,
            risk: sum_risk / n,
            style: sum_style / n,
            test_coverage,
            null_reason: scorecards.iter().find_map(|s| s.null_reason.clone()),
            synthesized: scorecards.iter().any(|s| s.synthesized),
        })
    }
}
