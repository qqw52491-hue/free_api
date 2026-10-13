//! 同一任务的显式续跑：业务契约保留，失效的运行等待状态清理。
use super::*;

impl AgentCheckpoint {
    /// 被取消且尚未消费的阶段批次没有有效结论，不应继续占预算或把派发当覆盖。
    /// 只读台账；真实成功结论/问题不退款，不改变历史任务归属。
    pub(crate) fn retire_stopped_stage_wait(&mut self, db: &DbState) -> Result<(), String> {
        let Some(batch) = self.stage_review_batch_id.as_ref() else {
            return Ok(());
        };
        let (pending, delivered): (i64, i64) = db.0.lock().map_err(|e| e.to_string())?
            .query_row(
                "SELECT COALESCE(SUM(CASE WHEN consumed_at IS NULL THEN 1 ELSE 0 END),0),
                        COALESCE(SUM(CASE WHEN status='succeeded' AND consumed_at IS NOT NULL THEN 1 ELSE 0 END),0)
                 FROM agent_child_runs WHERE batch_id=?1 AND COALESCE(purpose,'') != 'rebuttal'",
                rusqlite::params![batch], |row| Ok((row.get(0)?, row.get(1)?)),
            ).map_err(|e| e.to_string())?;
        if pending > 0 && delivered == 0 && self.stage_review_blockers_open == 0 {
            self.stage_review_budget_used = self.stage_review_budget_used.saturating_sub(1);
            self.stage_review_baseline_side_effects = self.data.inherited_side_effects_count as u32;
            self.stage_review_shell_baseline = 0;
        }
        Ok(())
    }

    /// 所有同任务入口共用恢复路径；用户补充不是新的任务题面。
    pub(crate) fn restore_task_for_entry(&mut self, context: &mut SandwichContext, reply: &str) {
        self.restore_context(context);
        if self.phase == AgentPhase::Failed {
            self.resume_failed_task(context, reply);
        } else {
            if !reply.trim().is_empty() && reply != context.current_goal {
                context.add_error_feedback(&format!("【人类回复】{}", reply));
            }
            if self.phase == AgentPhase::WaitUser && self.pending.is_none() {
                self.begin_planning(self.step_id, context);
            }
        }
    }

    pub(crate) fn apply_entry_execution_mode(
        &mut self,
        resuming: bool,
        mode: Option<&str>,
        auto_pilot: bool,
    ) {
        if !resuming || mode.is_some() {
            self.set_execution_mode(PlanExecutionMode::from_str_or_auto_pilot(mode, auto_pilot));
        }
    }

    pub(crate) fn resume_failed_task(&mut self, context: &mut SandwichContext, reply: &str) {
        let stopped_reason = self.last_error.clone().unwrap_or_default();
        // 失败/取消不是审查通过；已落账的真实结论、blocking 和人工裁决一律保留。
        if !self.plan_phase.is_approved() {
            self.plan_review_batch_id = None;
            self.plan_review_expected_count = 0;
            self.plan_reviewer_retry_count = 0;
            self.plan_reviewer_failover_count = 0;
            self.plan_reviewer_last_dispatch = None;
            self.plan_review_batch = PlanReviewBatchState::default();
        }
        // 阶段批次的旧后台任务不会被新 run 轮询，不能继续假装在途。
        // 已审出的问题及其来源仍有效，可继续对质；仅清等待句柄。
        self.stage_review_batch_id = None;
        self.stage_review_run_id = None;
        self.stage_review_run_ids.clear();
        self.stage_review_rebuttal_run_id = None;
        self.stage_review_confrontation_target = None;
        self.retry_count = 0;
        self.recent_gate_block_step = None;
        self.consecutive_gate_blocked = 0;
        self.gate_human_escape_directed = false;
        // 终态预检需重新确认运行期条件；不是免审收据。
        self.pending_finalization = None;
        self.prepared_instruction = None;
        self.last_error = None;
        // 尚无确定结果的副作用只恢复人工核实入口，绝不自动重放。
        self.phase = if self.pending.is_some() {
            AgentPhase::WaitUser
        } else {
            AgentPhase::CallLlm
        };

        context.add_error_feedback(&format!(
            "【同一任务续跑】已恢复原目标、方案、契约和审查要求。上次停止原因：{}\n本次用户说明：{}\n旧运行的后台等待句柄不再沿用；失败/取消不算审查通过。",
            stopped_reason, reply,
        ));
        if !self.plan_phase.is_approved() {
            if let Some(plan) = &self.submitted_plan {
                context.add_error_feedback(&format!(
                    "【已保存的待审方案】此前尚未获得放行，不能等待已取消的旧审查。可沿用现有证据与以下方案重新提交审查；真实驳回意见仍在任务状态中。\n{}",
                    serde_json::to_string(plan).unwrap_or_default(),
                ));
            }
        }
        self.refresh_context(context);
    }
}
