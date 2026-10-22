//! 阶段审查只保存观察事实；阻塞数和连续对质次数从成员历史推导。
use super::*;
use crate::agent::child_reviewer::{ReviewFinding, ReviewerDeliveryData};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct StageReviewState {
    receipts: BTreeMap<String, StageReceipt>,
    // 无法从旧台账还原归属的计数不能被任意成员的撤回扣掉。
    legacy_blockers: u32,
    cleared_through: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StageReceipt {
    source: ReviewDeliverySource,
    delivery: ReviewerDeliveryData,
    rebuttal: bool,
}

#[derive(Default)]
struct Member {
    findings: BTreeMap<String, ReviewFinding>,
    deferred: BTreeSet<String>,
    maintained_rounds: u32,
    latest_run: String,
}

fn task_key(source: &ReviewDeliverySource) -> String {
    serde_json::to_string(&(&source.batch_id, &source.task_id)).expect("string identity serializes")
}

fn is_blocking(finding: &ReviewFinding) -> bool {
    finding.severity.eq_ignore_ascii_case("blocking")
        && !finding.finality.eq_ignore_ascii_case("final_only")
}

impl StageReviewState {
    fn view(&self) -> (u32, Option<String>, Vec<String>, u32, Option<String>) {
        let mut history: Vec<_> = self.receipts.iter().collect();
        history.sort_by(|a, b| a.1.source.cmp(&b.1.source).then(a.0.cmp(b.0)));
        let mut members: BTreeMap<String, Member> = BTreeMap::new();
        let mut final_only = BTreeSet::new();
        for (run, receipt) in history {
            let key = task_key(&receipt.source);
            if self
                .cleared_through
                .get(&key)
                .is_some_and(|attempt| *attempt >= receipt.source.attempt_id)
            {
                continue;
            }
            let member = members.entry(key).or_default();
            member.latest_run = run.clone();
            let mut withdrawn: BTreeSet<_> = receipt
                .delivery
                .withdrawn_findings
                .iter()
                .cloned()
                .collect();
            withdrawn.extend(
                receipt
                    .delivery
                    .objection_verdicts
                    .iter()
                    .filter(|v| {
                        v.verdict.eq_ignore_ascii_case("withdraw")
                            || v.verdict.eq_ignore_ascii_case("withdrawn")
                    })
                    .map(|v| v.finding_id.clone()),
            );
            for id in &withdrawn {
                member.findings.remove(id);
            }
            for (index, finding) in receipt.delivery.findings.iter().enumerate() {
                if withdrawn.contains(&finding.id) {
                    continue;
                }
                let id = if finding.id.is_empty() {
                    format!("{run}:anonymous:{index}")
                } else {
                    finding.id.clone()
                };
                member.findings.insert(id, finding.clone());
            }
            let active: Vec<_> = member
                .findings
                .iter()
                .filter(|(id, finding)| is_blocking(finding) && !member.deferred.contains(*id))
                .map(|(id, f)| (id.clone(), f.clone()))
                .collect();
            if active.is_empty() {
                member.maintained_rounds = 0;
            } else if receipt.rebuttal {
                member.maintained_rounds += 1;
                // 沿用原有两轮对质移交终审的策略；只移交该成员当前的具体发现。
                if member.maintained_rounds >= 2 {
                    for (id, finding) in active {
                        member.deferred.insert(id);
                        final_only.insert(format!(
                            "【对质未撤回自动降级】[审查任务 {}] {}: {}",
                            receipt.source.task_id, finding.id, finding.why
                        ));
                    }
                    member.maintained_rounds = 0;
                }
            }
        }
        let mut count = self.legacy_blockers;
        let mut source = None;
        let mut rounds = 0;
        let mut target = None;
        for (key, member) in members {
            let open = member
                .findings
                .iter()
                .filter(|(id, f)| is_blocking(f) && !member.deferred.contains(*id))
                .count() as u32;
            count += open;
            if open > 0 {
                source = Some(member.latest_run);
            }
            if member.maintained_rounds > 0 {
                rounds = member.maintained_rounds;
                target = Some(key);
            }
        }
        (
            count,
            source,
            final_only.into_iter().collect(),
            rounds,
            target,
        )
    }

    fn clear(&mut self) {
        for receipt in self.receipts.values() {
            self.cleared_through
                .entry(task_key(&receipt.source))
                .and_modify(|attempt| *attempt = (*attempt).max(receipt.source.attempt_id))
                .or_insert(receipt.source.attempt_id);
        }
        self.legacy_blockers = 0;
    }
}

impl AgentCheckpoint {
    /// 仅旧检查点首次遇到后续回执时尝试收养同批已消费的有效历史。
    fn hydrate_stage_review_state(
        &self,
        db: &DbState,
        incoming: &str,
        source: &ReviewDeliverySource,
    ) -> StageReviewState {
        let mut state = StageReviewState::default();
        if self.stage_review_blockers_open == 0 {
            return state;
        }
        let batch = self
            .stage_review_batch_id
            .as_ref()
            .or(source.batch_id.as_ref());
        let rows: Vec<(String, String, String)> =
            db.0.lock()
                .ok()
                .and_then(|conn| {
                    let mut stmt = conn
                        .prepare(
                            "SELECT id, COALESCE(purpose,''), result_json FROM agent_child_runs
                 WHERE batch_id=?1 AND id!=?2 AND consumed_at IS NOT NULL AND status='succeeded'
                   AND error_kind IS NULL AND result_json IS NOT NULL
                   AND purpose IN ('stage_review','rebuttal')",
                        )
                        .ok()?;
                    let values = stmt
                        .query_map(rusqlite::params![batch, incoming], |row| {
                            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                        })
                        .ok()?;
                    Some(values.flatten().collect())
                })
                .unwrap_or_default();
        for (run, purpose, raw) in rows {
            if let Ok(delivery) = serde_json::from_str::<ReviewerDeliveryData>(&raw) {
                state.receipts.insert(
                    run.clone(),
                    StageReceipt {
                        source: ReviewDeliverySource::from_run(db, &run),
                        delivery,
                        rebuttal: purpose == "rebuttal",
                    },
                );
            }
        }
        let known = state.view().0;
        if known > self.stage_review_blockers_open {
            // 旧版本可能已有人工裁决/清账；不能根据原始历史恢复被解除的阻塞。
            state.clear();
            state.legacy_blockers = self.stage_review_blockers_open;
        } else {
            state.legacy_blockers = self.stage_review_blockers_open - known;
        }
        state
    }

    pub(crate) fn record_stage_review_receipt(
        &mut self,
        db: &DbState,
        receipt: &crate::agent::protocol::DeliveryReceipt,
        rebuttal: bool,
    ) -> String {
        let source = ReviewDeliverySource::from_run(db, &receipt.child_run_id);
        if self.stage_review_state.is_none() {
            self.stage_review_state =
                Some(self.hydrate_stage_review_state(db, &receipt.child_run_id, &source));
        }
        let state = self.stage_review_state.as_mut().unwrap();
        if let Some(delivery) = receipt.as_reviewer_delivery() {
            state
                .receipts
                .entry(receipt.child_run_id.clone())
                .or_insert(StageReceipt {
                    source,
                    delivery,
                    rebuttal,
                });
        }
        let (count, source_run, final_only, rounds, target) = state.view();
        self.stage_review_blockers_open = count;
        self.stage_review_blocker_source_run_id = source_run.or_else(|| {
            (state.legacy_blockers > 0)
                .then(|| self.stage_review_blocker_source_run_id.clone())
                .flatten()
        });
        self.stage_review_confrontation_failures = rounds;
        self.stage_review_confrontation_target = target;
        self.stage_review_final_only.extend(final_only);
        if state.legacy_blockers > 0 {
            format!("\n旧阶段审查中有 {} 条阻塞的来源尚无法完整还原，本次未按撤回条数扣减这些旧项；可回查原审查或请求人工处理。", state.legacy_blockers)
        } else {
            String::new()
        }
    }

    /// 人工放行、旧阶段清账及新批次切换均明确退出旧投影，不让后续回执重放旧计数。
    pub(crate) fn clear_stage_review_blockers(&mut self) {
        self.stage_review_state
            .get_or_insert_with(Default::default)
            .clear();
        self.stage_review_blockers_open = 0;
        self.stage_review_blocker_source_run_id = None;
        self.stage_review_confrontation_failures = 0;
        self.stage_review_confrontation_target = None;
    }
}
