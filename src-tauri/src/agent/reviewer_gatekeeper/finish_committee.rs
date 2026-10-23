use super::finish_diff::evaluate_finish_diff_single;
use super::*;

pub(super) async fn evaluate_committee<R: tauri::Runtime>(
    app: &AppHandle<R>,
    db: &DbState,
    context: &SandwichContext,
    model_routing: &ModelRoutingConfig,
    step_id: usize,
    workspace: &Path,
    baseline: &GitBaseline,
    run_started_at: chrono::DateTime<chrono::Utc>,
    session_id: &str,
    interrupt_rx: &mut watch::Receiver<u64>,
    last_reviewed_hash: Option<&str>,
    last_review_result: Option<&ReviewResult>,
    stage_carry_over: &[String],
    host_verification: Option<&str>,
    human_waivers: Option<&str>,
    // 门禁适用域交接：机械门 NotApplicable 跳过的检查项说明（Skip 不静默，
    // 终审须以命令退出码/终端输出/API 响应等语义证据核验这些项）
    gate_notices: &[String],
    approved_target_files: Option<&[String]>,
    // 任务入口分流判决：mechanical_only 短路、终审角色与产物质量标准分流均由此驱动
    triage: Option<&crate::agent::triage::TriageVerdict>,
    // 保留参数：未来"控制面自改"机械升级规则将按模式分流（当前不变量由提示词契约承担）
    _plan_mode: Option<crate::agent::checkpoint::PlanExecutionMode>,
    // 调用方已组装 summary + details 的完整交付正文，参与审查与复用指纹。
    finish_summary: Option<&str>,
) -> ReviewResult {
    let primary = triage
        .and_then(|v| v.final_review_roles.iter().find(|r| !r.trim().is_empty()))
        .and_then(|r| resolve_enabled_card(db, r).ok().flatten())
        .filter(|c| c.allows_kind("reviewer"))
        .or_else(|| resolve_enabled_card(db, "reviewer").ok().flatten())
        .filter(|c| {
            !crate::agent::tools::role_manager::is_card_disabled_for(
                &model_routing.disabled_role_cards,
                &c.name,
                "reviewer",
            )
        });
    let cards = crate::agent::review_team::review_team(
        db,
        context,
        approved_target_files.unwrap_or(&[]),
        &model_routing.disabled_role_cards,
        true,
        primary,
    );
    if cards.is_empty() {
        let mut result = evaluate_finish_diff_single(
            app,
            db,
            context,
            model_routing,
            step_id,
            workspace,
            baseline,
            run_started_at,
            session_id,
            interrupt_rx,
            last_reviewed_hash,
            last_review_result,
            stage_carry_over,
            host_verification,
            human_waivers,
            gate_notices,
            approved_target_files,
            triage,
            _plan_mode,
            finish_summary,
            "finish-review",
            None,
        )
        .await;
        result.summary = format!(
            "已选择多人终审，但无可用专家卡，实际按基础单人审查处理；{}",
            result.summary
        );
        return result;
    }
    let interrupt_generation = *interrupt_rx.borrow();
    let verification_lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
    let _ = app.emit(&format!("agent-log-{session_id}"), format!(
        "多人终审本轮选择 {} 位候选：{}。各角色独立核验；模型可能相同，实际有效结论数量在最终回执披露。",
        cards.len(),cards.iter().map(|c|c.label.as_str()).collect::<Vec<_>>().join("、")));
    let mut futures = Vec::new();
    for (index, card) in cards.iter().enumerate() {
        let mut rx = interrupt_rx.clone();
        let mut member_triage = triage
            .cloned()
            .unwrap_or_else(|| crate::agent::triage::TriageVerdict::fallback("用户选择多人终审"));
        member_triage.final_review_roles = vec![card.name.clone()];
        let label = card.label.clone();
        let verification_lock = verification_lock.clone();
        futures.push(async move {
            let suffix = format!("finish-review-member-{}", index + 1);
            let result = evaluate_finish_diff_single(
                app,
                db,
                context,
                model_routing,
                step_id,
                workspace,
                baseline,
                run_started_at,
                session_id,
                &mut rx,
                None,
                None,
                stage_carry_over,
                host_verification,
                human_waivers,
                gate_notices,
                approved_target_files,
                Some(&member_triage),
                _plan_mode,
                finish_summary,
                &suffix,
                Some(verification_lock),
            )
            .await;
            (label, result)
        });
    }
    let results = futures::future::join_all(futures).await;
    settle_committee_results(results, *interrupt_rx.borrow() != interrupt_generation)
}

fn settle_committee_results(results: Vec<(String, ReviewResult)>, interrupted: bool) -> ReviewResult {
    let mut merged = aggregate_committee_results(results);
    // 显式停止不是“不可用成员”：不能靠此前一位通过把停止转换为交付。
    if interrupted {
        merged.verdict = "failed_open".into();
        merged.summary = format!("多人终审已停止，未完成本轮交付判定。\n{}", merged.summary);
    }
    merged
}

fn aggregate_committee_results(results: Vec<(String, ReviewResult)>) -> ReviewResult {
    let mut merged = ReviewResult::skipped_empty();
    let count = results.len();
    let failed = results
        .iter()
        .filter(|(_, r)| r.is_service_failure())
        .count();
    let cancelled = results.iter().filter(|(_, r)| r.verdict == "cancelled").count();
    let valid = results
        .iter()
        .filter(|(_, r)| !r.is_service_failure() && r.verdict != "skipped")
        .count();
    let rejected = results
        .iter()
        .any(|(_, r)| !r.is_passed() && !r.is_service_failure() && r.verdict != "skipped");
    // 已派发成员全部收束后，仅用有效裁决聚合；不可用不是反对，也不是批准。
    merged.verdict = if rejected {
        "rejected"
    } else if valid > 0 {
        "passed"
    } else if failed > 0 {
        "failed_open"
    } else {
        "skipped"
    }
    .into();
    merged.score = if valid > 0 {
        (results
            .iter()
            .filter(|(_, r)| !r.is_service_failure() && r.verdict != "skipped")
            .map(|(_, r)| r.score as u64)
            .sum::<u64>() / valid as u64) as u32
    } else {
        0
    };
    merged.duration_ms = results
        .iter()
        .map(|(_, r)| r.duration_ms)
        .max()
        .unwrap_or(0);
    let mut summaries = vec![format!(
        "多人终审：候选 {} 位，有效结论 {} 位，通道失败 {} 位，人工停止或本任务排除 {} 位。",
        count, valid, failed - cancelled, cancelled
    )];
    if failed > 0 {
        summaries.push("不可用成员已排除，不计为批准；有效成员的阻塞仍然生效。".into());
    }
    if valid == 1 {
        summaries.push("仅有一位有效专家，本轮实际为单人审查；未宣称完成多人会审。".into());
    }
    let mut models = Vec::new();
    let mut hash = sha2::Sha256::new();
    for (label, result) in results {
        summaries.push(format!("【{}】{}", label, result.summary));
        let effective = !result.is_service_failure() && result.verdict != "skipped";
        models.push(result.model_id);
        hash.update(label.as_bytes());
        hash.update(result.diff_hash.as_bytes());
        if !effective {
            continue;
        }
        for mut issue in result.blocking_issues {
            issue.issue = format!("【{}】{}", label, issue.issue);
            merged.blocking_issues.push(issue);
        }
        merged.nit_issues.extend(result.nit_issues);
        merged.missing_evidence.extend(
            result
                .missing_evidence
                .into_iter()
                .map(|m| format!("【{}】{}", label, m)),
        );
    }
    merged.summary = summaries.join("\n");
    merged.model_id = models.join(",");
    merged.diff_hash = format!("{:x}", hash.finalize());
    merged
}

#[cfg(test)]
#[path = "finish_committee_tests.rs"]
mod tests;
