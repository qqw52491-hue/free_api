use super::*;
use crate::agent::child_reviewer::PlanReviewerDispatchSnapshot;
use crate::agent::tools::role_manager::RoleCard;
#[cfg(test)]
#[path = "team_tests.rs"]
mod tests;

pub(super) async fn prepare_snapshots(
    db: &DbState,
    workspace: &Path,
    routing_config: &crate::agent::router::ModelRoutingConfig,
    routing: &crate::agent::router::RoutingDecision,
    primary: PlanReviewerDispatchSnapshot,
    cards: &[RoleCard],
) -> Vec<PlanReviewerDispatchSnapshot> {
    let mut snapshots = vec![primary.clone()];
    for card in cards.iter().skip(1) {
        let mut snapshot = primary.clone();
        snapshot.model = if card.model_id.trim().is_empty() {
            routing.model_id.clone()
        } else {
            card.model_id.clone()
        };
        if routing_config.auto_cross_family_reviewer
            && card.model_id.trim().is_empty()
            && routing_config
                .role_models
                .get("reviewer")
                .is_none_or(|s| s.trim().is_empty())
        {
            snapshot.model = crate::agent::router::prefer_cross_family_reviewer(
                &*db.0.lock().unwrap(),
                &snapshot.model,
            )
            .0;
        }
        snapshot.persona = crate::agent::tools::role_manager::persona_block(card)
            + &crate::agent::tools::role_manager::skills_block(db, card);
        snapshot.params["role"] = json!(card.name);
        snapshot.params["role_label"] = json!(card.label);
        snapshot.custom_schema = card.parse_custom_schema().unwrap_or_default();
        // 输入约束不凭主审的卡片推断；无法承接相同材料的专家在派发时披露。
        let inputs = snapshot.params.get("custom_inputs").cloned();
        if let Err(error) =
            crate::agent::tools::role_manager::validate_dispatch_inputs(card, inputs.as_ref())
        {
            snapshot.params["host_input_error"] = json!(error);
        }
        if let Some(extra) = crate::agent::context_collector::assemble_focus_context(
            &card.parse_focus_domains(),
            workspace,
            None,
            inputs.as_ref(),
        )
        .await
        {
            let shared = primary
                .params
                .get("specialized_context")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            snapshot.params["specialized_context"] = json!(format!("{shared}\n{extra}"));
        }
        snapshots.push(snapshot);
    }
    if snapshots.len() > 1 {
        for (snapshot, card) in snapshots.iter_mut().zip(cards) {
            // 并发成员审同一份方案，不让多个 revised_* 按到达先后互相覆盖。
            snapshot.params["plan_review_can_finalize"] = json!(false);
            snapshot.params["max_concurrency"] = json!(if card.max_concurrency > 0 {
                card.max_concurrency
                    .min(crate::agent::review_team::MAX_REVIEWERS as i64)
            } else {
                crate::agent::review_team::MAX_REVIEWERS as i64
            });
        }
    }
    snapshots
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn dispatch_plan_team(
    app: Option<AppHandle>,
    db: &DbState,
    parent_run: &str,
    session: &str,
    workspace: &Path,
    checkpoint: &mut AgentCheckpoint,
    submission: &crate::agent::types::PlanSubmission,
    step: usize,
    round: u32,
    revision: bool,
    committee: bool,
    snapshots: Vec<PlanReviewerDispatchSnapshot>,
    interrupt: watch::Receiver<u64>,
) -> DispatchResult {
    let mut ids = Vec::new();
    let mut labels = Vec::new();
    let mut notices = Vec::new();
    let batch = snapshots[0].batch_id.clone().unwrap_or_default();
    for snapshot in snapshots {
        let label = snapshot
            .params
            .get("role_label")
            .and_then(|v| v.as_str())
            .unwrap_or("审查员")
            .to_owned();
        let spawned = if let Some(error) = snapshot
            .params
            .get("host_input_error")
            .and_then(|v| v.as_str())
        {
            Err(error.to_owned())
        } else if let Err(error) = crate::llm::ModelConfig::from_db_state(db, &snapshot.model) {
            Err(format!("模型配置不可用：{error}"))
        } else {
            snapshot
                .spawn(
                    app.clone(),
                    db,
                    parent_run,
                    session,
                    workspace,
                    interrupt.clone(),
                    None,
                )
                .await
        };
        match spawned {
            Ok(id) => {
                if ids.is_empty() {
                    checkpoint.commit_plan_submission(submission.clone(), &batch, &snapshot, step);
                }
                checkpoint.reviewer_dispatches.insert(id.clone(), snapshot);
                ids.push(id);
                labels.push(label);
                checkpoint.plan_review_expected_count = ids.len() as u32;
                if ids.len() > 1 {
                    checkpoint.plan_escalation_required = None;
                }
            }
            Err(error) => notices.push(format!("{label}未启动：{error}")),
        }
    }
    if ids.is_empty() {
        return DispatchResult {
            view: None,
            success: false,
            stdout: String::new(),
            stderr: format!("方案审查未能启动，本次未批准方案：{}", notices.join("；")),
            route: "submit_plan:dispatch_failed".into(),
        };
    }
    if committee && ids.len() < 2 {
        notices.push(
            "已选择多人审查，但本轮实际仅启动 1 位；按该位实际结论处理，不宣称已完成多人会审"
                .into(),
        );
    }
    let summary = format!("【✅ {}方案已提交并进入审查】第 {} 轮，实际启动 {} 位：{}。\n请用 wait_jobs(job_ids:{}) 等待本批结果；所有成员结果收齐前不视为批准。审查完成不等于方案通过，具体 blocking 和修订裁决继续生效。{}",
        if revision {"修订"}else{""},round,ids.len(),labels.join("、"),
        serde_json::to_string(&ids).unwrap_or_default(),
        if notices.is_empty(){String::new()}else{format!("\n派发说明：{}",notices.join("；"))});
    DispatchResult {
        view: None,
        success: true,
        stdout: summary,
        stderr: String::new(),
        route: "submit_plan:dispatched".into(),
    }
}
