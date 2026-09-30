//! submit_plan 编排：结构化方案提交 → Tech Lead 审查派发的一次性状态转换。
//!
//! 从 loop_runner 拆出：该处理器自成一条完整编排链（适用性预检 → 结构校验 →
//! 角色卡解析 → 证据包组装 → spawn → checkpoint 一致提交），与工具派发主链解耦。

use crate::agent::checkpoint::AgentCheckpoint;
use crate::agent::types::DispatchResult;
use crate::db::DbState;
use serde_json::json;
use std::path::Path;
use tauri::AppHandle;
use tokio::sync::watch;

/// submit_plan 编排：校验 → 组装 Tech Lead 证据包 → spawn（唯一可失败的外部副作用）
/// → 成功后由 checkpoint.commit_plan_submission 一次合并全部状态。
/// 任何失败路径都不触碰 checkpoint，杜绝「方案已保存但审查未注册」的半截状态。
#[allow(clippy::too_many_arguments)]
pub async fn handle_submit_plan(
    app: &AppHandle,
    db: &DbState,
    model_routing: &crate::agent::router::ModelRoutingConfig,
    parent_run_id: &str,
    workspace_dir: &Path,
    final_session_id: &String,
    step_id: usize,
    submission: &crate::agent::types::PlanSubmission,
    context: &mut crate::agent::context::SandwichContext,
    checkpoint: &mut AgentCheckpoint,
    interrupt_rx: watch::Receiver<u64>,
) -> DispatchResult {
    let reject = |stderr: String, route: &str| DispatchResult { view: None,
        success: false,
        stdout: String::new(),
        stderr,
        route: route.to_string(),
    };

    // ---- 适用性门禁（纯 checkpoint 判定）----
    let is_revision = checkpoint.plan_phase.is_approved();
    if let Some(reason) =
        crate::agent::plan_gate::submit_plan_rejection(checkpoint, submission)
    {
        let route = if reason.contains("审查轮数已达上限") {
            "plan_gatekeeper:reviewer_retry_limit"
        } else if reason.contains("在途") {
            "plan_liveness:submit_plan_in_flight"
        } else {
            "plan_liveness:submit_plan_not_applicable"
        };
        return reject(reason, route);
    }
    // DB 在途守卫：崩溃孤儿行（spawn 成功但批次未注册）也要挡住重复提交
    if crate::agent::child_reviewer::has_running_child_runs(db, parent_run_id) {
        // 披露可并行路径而非只指 wait_jobs：子任务结论产出后主循环自动注入，
        // 有未完成待办时干等是纯烧 token（实测单场 21 次串行轮询）。
        let open_todos = context
            .todo_list
            .iter()
            .filter(|t| t.status == "pending" || t.status == "in_progress")
            .count();
        let parallel_hint = if open_todos > 0 {
            format!("当前还有 {} 项未完成待办可并行推进；", open_todos)
        } else {
            String::new()
        };
        return reject(
            format!(
                "【⚠️ submit_plan 已拒绝】仍有子任务在后台运行。{}子任务结论产出后会自动注入上下文——可先推进独立工作，或调用 wait_jobs 挂起等待收取结论。",
                parallel_hint
            ),
            "plan_liveness:submit_plan_in_flight",
        );
    }

    // ---- Auto 模式策略预检：submit_plan 兼任「报方案拿判定」通道 ----
    // 判定前置为正常工具结果：FastPass 直接落账放行（不派审查——白名单文件
    // 烧一轮 tech_lead 是纯浪费）；Escalate 切换为交叉审查模式并落入下方既有
    // 派发链（自动开启审查）；Block 直接拒绝。写入处门禁仍兜底——此处只是把
    // 「撞上才知道要审查」换成「报方案时拿到判定回执」。
    if checkpoint.plan_mode == crate::agent::checkpoint::PlanExecutionMode::Auto {
        if let Some(verdict) = auto_policy_verdict(checkpoint, submission, workspace_dir, step_id) {
            return verdict;
        }
        // Escalate：auto_policy_verdict 已把模式切换为 CrossReview，
        // 落入下方既有校验与 Tech Lead 派发链。
    }

    // ---- 结构校验：方案必须带来源证据，防止空想方案进入审查 ----
    // 证据强度由任务入口分流判决驱动（evidence_policy=none/source_refs 时允许空证据），
    // 无判决默认要求证据（旧行为）。
    let evidence_required = checkpoint
        .triage_verdict
        .as_ref()
        .map_or(true, |v| v.evidence_required());
    let (target_files, evidence_refs) =
        match crate::agent::plan_gate::validate_plan_submission(submission, evidence_required) {
            Ok(validated) => validated,
            Err(reason) => return reject(reason, "plan_liveness:submit_plan_invalid"),
        };

    // ---- 解析 tech_lead 角色卡 ----
    if crate::agent::tools::role_manager::is_card_disabled_for(
        &model_routing.disabled_role_cards,
        "tech_lead",
        "reviewer",
    ) {
        return reject(
            "【⚠️ submit_plan 已拒绝】tech_lead 角色卡已被用户在当前会话排除/休眠。\n请改用 child_reviewer 指定其他 reviewer 角色提交方案审查。"
                .to_string(),
            "plan_liveness:submit_plan_role_unavailable",
        );
    }
    let card = match crate::agent::tools::role_manager::resolve_enabled_card(db, "tech_lead") {
        Ok(Some(card)) if card.allows_kind("reviewer") => card,
        Ok(Some(_)) => {
            return reject(
                "【⚠️ submit_plan 已拒绝】tech_lead 角色卡未授权挂载为 reviewer。请改用 child_reviewer 指定其他角色。"
                    .to_string(),
                "plan_liveness:submit_plan_role_unavailable",
            )
        }
        Ok(None) => {
            return reject(
                "【⚠️ submit_plan 已拒绝】tech_lead 角色卡不存在或已禁用。请改用 child_reviewer 指定其他 reviewer 角色。"
                    .to_string(),
                "plan_liveness:submit_plan_role_unavailable",
            )
        }
        Err(error) => {
            return reject(
                format!("【⚠️ submit_plan 已拒绝】tech_lead 角色卡查询失败：{}。请改用 child_reviewer 重试。", error),
                "plan_liveness:submit_plan_role_unavailable",
            )
        }
    };

    // ---- 组装 Tech Lead 评审证据包 ----
    // 契约约束：child_reviewer.question 拒绝一切控制字符（含 \n），必须单行；
    // 多行证据包走 evidence_excerpt（仅长度校验，允许换行）。
    let flatten = |raw: &str| -> String {
        raw.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
    };
    let discussion_round = checkpoint.plan_reviewer_retry_count + 1;
    let summary_flat: String = flatten(submission.plan_summary.trim())
        .chars()
        .take(400)
        .collect();
    // 修订轮上下文共享：被取代的已批准方案 + 前轮裁决一并交给 Tech Lead，
    // 审查者在既有讨论基础上裁决"变更是否成立"，而不是零起点重审。
    let superseded_summary: Option<String> = if is_revision {
        checkpoint.submitted_plan.as_ref().map(|p| {
            let s: String = p.plan_summary.trim().chars().take(800).collect();
            s
        })
    } else {
        None
    };
    let question: String = if is_revision {
        format!("【方案修订评审·第 {} 轮】{}", discussion_round, summary_flat)
    } else {
        format!("【方案评审·第 {} 轮】{}", discussion_round, summary_flat)
    }
    .chars()
    .take(1900)
    .collect();

    let mut evidence_excerpt = format!(
        "方案摘要：{}\n根因假设：{}\n拟修改文件：{}",
        submission.plan_summary.trim(),
        if submission.root_cause.trim().is_empty() {
            "（未填写）"
        } else {
            submission.root_cause.trim()
        },
        if target_files.is_empty() {
            "（无：声明为纯分析/只读交付，不产生文件改动）".to_string()
        } else {
            target_files.join(", ")
        },
    );
    if !submission.assumptions.is_empty() {
        evidence_excerpt.push_str(&format!("\n关键假设：{}", submission.assumptions.join("；")));
    }
    if !submission.open_questions.is_empty() {
        evidence_excerpt.push_str(&format!(
            "\n未决问题：{}",
            submission.open_questions.join("；")
        ));
    }
    if !submission.intended_verification.trim().is_empty() {
        evidence_excerpt.push_str(&format!(
            "\n预期验证方式：{}",
            submission.intended_verification.trim()
        ));
    }
    if !submission.risk_level.trim().is_empty() {
        evidence_excerpt.push_str(&format!("\n自评风险等级：{}", submission.risk_level.trim()));
    }
    if is_revision {
        let prev_verdict = checkpoint
            .plan_review_deliveries
            .values()
            .find_map(|d| d.get_extra_str("decision"))
            .map(|s| s.to_string())
            .unwrap_or_else(|| "（无前轮裁决记录）".to_string());
        evidence_excerpt.push_str(&format!(
            "\n\n【方案修订轮】本提交取代此前已批准方案。修订理由：{}\n被取代方案摘要：{}\n前轮审查裁决：{}",
            submission.revision_reason.trim(),
            superseded_summary.as_deref().unwrap_or("（无）"),
            prev_verdict,
        ));
    }
    // ---- 证据范围对账（机械差集，非语义判断）----
    // 「引为证据 ≠ 列入修改目标」：被引用文件可能依赖被改对象的语义——
    // workspace_leak 实测教训：方案引用 path_guard.rs 却未列入 target_files，
    // 而其判定顺序恰被 workspace_root 的新取值颠覆。差集只摆事实不判对错，
    // 逐项 HOLDS/BROKEN/NA 裁决义务由审查输出侧强制覆盖。
    let evidence_scope_refs = crate::agent::impact_pack::evidence_scope_refs(
        &target_files,
        &evidence_refs,
        Some(workspace_dir),
    );
    if !evidence_scope_refs.is_empty() {
        evidence_excerpt.push_str(&format!(
            "\n\n【📎 证据范围对账】以下文件被方案引为证据但未列入拟修改目标：{}",
            evidence_scope_refs.join(", ")
        ));
    }
    let evidence_excerpt: String = evidence_excerpt.chars().take(5500).collect();
    let evidence_refs: Vec<String> = evidence_refs.iter().map(|r| flatten(r)).collect();

    let custom_inputs = json!({
        "plan_summary": submission.plan_summary.trim(),
        "target_files": target_files,
        "discussion_round": discussion_round,
        "user_goal": context.effective_goal(),
        "root_cause": submission.root_cause.trim(),
        "assumptions": submission.assumptions,
        "open_questions": submission.open_questions,
        "intended_verification": submission.intended_verification.trim(),
        "risk_level": submission.risk_level.trim(),
        "is_plan_revision": is_revision,
        "revision_reason": submission.revision_reason.trim(),
        "superseded_plan_summary": superseded_summary,
    });
    // ---- 角色卡 input_schema 必填校验（输入侧硬约束，与输出侧 custom_schema 对称）----
    if let Err(err) =
        crate::agent::tools::role_manager::validate_dispatch_inputs(&card, Some(&custom_inputs))
    {
        return reject(err, "plan_liveness:submit_plan_invalid");
    }
    // 任务入口分流判决简报并入 custom_inputs（verified_facts/hard_constraints 保留键升格一级小节）
    let custom_inputs = match checkpoint.triage_verdict.as_ref() {
        Some(v) => crate::agent::triage::merge_brief_into_inputs(Some(&custom_inputs), v),
        None => custom_inputs,
    };
    let specialized_context = crate::agent::context_collector::assemble_focus_context(
        &card.parse_focus_domains(),
        workspace_dir,
        None,
        Some(&custom_inputs),
    )
    .await;
    // ---- 影响面机械枚举：target_files 的契约面/use 依赖边 → 跨文件消费点 ----
    // 方案期无 diff，按目标文件的 pub 定义与 use 导入推导潜在耦合半径；
    // refs 透传进 params，审查输出侧由 child_reviewer 强制逐条裁决（防"引而不看"）。
    let impact = if crate::agent::impact_pack::impact_pack_enabled() {
        // 方案文本（摘要+根因+假设）一并挖矿：值流耦合（局部变量→谓词）不在
        // use 边/pub 项里，但模型写方案时几乎必然点名涉事符号。
        let plan_text = format!(
            "{}\n{}\n{}\n{}",
            submission.plan_summary,
            submission.root_cause,
            submission.assumptions.join("\n"),
            submission.open_questions.join("\n"),
        );
        crate::agent::impact_pack::build_for_targets(workspace_dir, &target_files, &plan_text)
    } else {
        crate::agent::impact_pack::ImpactPack::default()
    };
    let mut effective_params = json!({
        "role": card.name,
        "role_label": card.label,
        "question": question,
        "evidence_excerpt": evidence_excerpt,
        "evidence_refs": evidence_refs,
        "custom_inputs": custom_inputs,
        // 1v1 方案评审标记：授权审查输出 revised_* 定稿字段（见 child_reviewer 提示词条款）
        "plan_review": true,
    });
    if !impact.refs.is_empty() {
        effective_params["impact_pack_refs"] = json!(impact.refs);
    }
    if !evidence_scope_refs.is_empty() {
        effective_params["evidence_scope_refs"] = json!(evidence_scope_refs);
    }
    let specialized_context = match (specialized_context, impact.section.is_empty()) {
        (Some(spec), false) => Some(format!("{}\n\n{}", spec, impact.section)),
        (Some(spec), true) => Some(spec),
        (None, false) => Some(impact.section),
        (None, true) => None,
    };
    if let Some(spec) = specialized_context {
        effective_params["specialized_context"] = json!(spec);
    }

    // ---- 路由与异构优选（与 child_reviewer 常规派发同一条路径）----
    let routing = crate::agent::router::AgentRouter::route_for_role(
        context,
        model_routing,
        step_id,
        crate::agent::router::roles::REVIEWER,
    );
    let has_explicit_reviewer_slot = model_routing
        .role_models
        .get(crate::agent::router::roles::REVIEWER)
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let mut reviewer_model = if card.model_id.is_empty() || has_explicit_reviewer_slot {
        routing.model_id.clone()
    } else {
        card.model_id.clone()
    };
    if model_routing.auto_cross_family_reviewer
        && reviewer_model == routing.model_id
        && !has_explicit_reviewer_slot
    {
        let (preferred, _note) = crate::agent::router::prefer_cross_family_reviewer(
            &*db.0.lock().unwrap(),
            &reviewer_model,
        );
        reviewer_model = preferred;
    }
    let persona = crate::agent::tools::role_manager::persona_block(&card)
        + &crate::agent::tools::role_manager::skills_block(db, &card);
    let mut snapshot = crate::agent::child_reviewer::PlanReviewerDispatchSnapshot::new(
        &reviewer_model,
        routing.tier.to_string(),
        model_routing.reasoning_effort_for(&routing.tier),
        persona,
        effective_params.clone(),
    )
    .with_purpose("plan_review")
    .with_batch_id(format!("batch-{}", uuid::Uuid::new_v4()));
    if let Some(schema) = card.parse_custom_schema() {
        snapshot = snapshot.with_custom_schema(schema);
    }
    if let Some(spec) = effective_params
        .get("specialized_context")
        .and_then(|v| v.as_str())
    {
        snapshot = snapshot.with_specialized_context(spec);
    }

    // ---- 唯一可失败的外部副作用先行；成功后才做一致的状态合并 ----
    match snapshot
        .spawn(
            Some(app.clone()),
            db,
            parent_run_id,
            final_session_id,
            workspace_dir,
            interrupt_rx,
            None,
        )
        .await
    {
        Ok(child_run_id) => {
            // 批次锚点 = 快照盖章的 batch_id（子 run 行已写入），
            // 成员身份此后全部由台账 batch_id+consumed_at 推导
            let batch_id = snapshot.batch_id.clone().unwrap_or_default();
            checkpoint.commit_plan_submission(
                submission.clone(),
                &batch_id,
                &snapshot,
                step_id,
            );
            let _ = child_run_id;
            DispatchResult { view: None,
                success: true,
                stdout: if is_revision {
                    format!(
                        "【✅ 修订方案已提交并重开审查】原放行状态已撤销，写入门禁重新生效。Tech Lead 修订评审已异步派发（child_run_id: {}，第 {} 轮）。\n\
                         请调用 wait_jobs(job_ids:[\"{}\"]) 挂起等待裁决；结论注入后按 decision 行动：approved 恢复写入、rebuttal 继续修订重审、escalate 按指定专家派联合会审。\n\
                         【修订轮附带义务】获批后请用 todo_update 对照新方案核对任务台账：作废与新方案无关的待办、补充新工作项；已完成的有效工作保持 done 不受影响。",
                        child_run_id, discussion_round, child_run_id
                    )
                } else {
                    format!(
                        "【✅ 方案已提交并进入审查】结构化方案已落账，Tech Lead 方案评审已异步派发（child_run_id: {}，第 {} 轮）。\n\
                         请调用 wait_jobs(job_ids:[\"{}\"]) 挂起等待裁决；结论注入后按 decision 行动：approved 放行写入、rebuttal 修订后重审、escalate 按指定专家派联合会审。",
                        child_run_id, discussion_round, child_run_id
                    )
                },
                stderr: String::new(),
                route: "submit_plan:dispatched".to_string(),
            }
        }
        Err(error) => reject(
            format!(
                "【⚠️ 方案提交失败】Tech Lead 审查任务派发未成功（{}），本次 submit_plan 未落账，方案与预算状态均未变。\n\
                 请修正后重新 submit_plan，或改用 child_reviewer 手动派发。",
                error
            ),
            "submit_plan:dispatch_failed",
        ),
    }
}

/// Auto 模式下 submit_plan 的策略预检：把「写入时才跑的判定」前置为正常工具结果。
/// - FastPass → 方案落账（审计母本）+ 核准范围登记（scope drift 对账基准），
///   返回就绪文案；不设 plan_phase=Approved——后续写入仍逐次过策略判定，
///   申报范围内放行、未申报的高危文件照常触发升级（比"一次直通永久放行"诚实）。
/// - Escalate → 模式切换为 CrossReview 并返回 None，由调用方落入既有校验与
///   Tech Lead 派发链（自动开启审查，复用整套批次台账/裁决/放行仪式）。
/// - Block   → 返回拒绝结果（危险命令黑名单级）。
/// 返回 Some = 终态答复；None = 继续走正常审查派发。
fn auto_policy_verdict(
    checkpoint: &mut AgentCheckpoint,
    submission: &crate::agent::types::PlanSubmission,
    workspace_dir: &Path,
    step_id: usize,
) -> Option<DispatchResult> {
    let target_files: Vec<String> = submission
        .target_files
        .iter()
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .collect();
    let policy = crate::agent::agent_policy::PolicyConfig::load_from_workspace_or_default(
        Some(workspace_dir),
    );
    // 方案级判定只评文件——具体 shell 命令到写入那一步才出现，由写入门禁逐条评估。
    let decision = crate::agent::agent_policy::evaluate_plan_policy(
        &policy,
        &target_files,
        &[],
        &submission.plan_summary,
    );
    checkpoint.policy_decision_summary = Some(decision.summary());
    match decision {
        crate::agent::agent_policy::PolicyDecision::FastPass { reason } => {
            checkpoint.submitted_plan = Some(submission.clone());
            checkpoint.submitted_plan_step = Some(step_id);
            if !target_files.is_empty() && checkpoint.approved_target_files.is_none() {
                checkpoint.approved_target_files = Some(target_files);
            }
            Some(DispatchResult { view: None,
                success: true,
                stdout: format!(
                    "【✅ 方案预检通过：无需审查】\n判定: {}\n\
                     已申报文件可直接写入；写入超出申报范围的文件时门禁仍会重新判定。",
                    reason
                ),
                stderr: String::new(),
                route: "plan_gatekeeper:auto_preflight_fast_pass".to_string(),
            })
        }
        crate::agent::agent_policy::PolicyDecision::EscalateToReview {
            reason,
            matched_pattern,
            suggested_roles,
        } => {
            println!(
                "[plan_gate] auto submit_plan 升级审查: {} (规则 {}, 建议角色: {})",
                reason,
                matched_pattern,
                suggested_roles.join(", ")
            );
            checkpoint.set_execution_mode(
                crate::agent::checkpoint::PlanExecutionMode::CrossReview,
            );
            None
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::checkpoint::PlanExecutionMode;
    use crate::agent::types::PlanSubmission;

    fn auto_checkpoint() -> AgentCheckpoint {
        let context =
            crate::agent::context::SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("run-auto-preflight".to_string(), &context);
        cp.plan_mode = PlanExecutionMode::Auto;
        cp
    }

    /// FastPass：白名单文件方案 → 返回"无需审查"就绪回执，方案与核准范围落账，
    /// 但不预支 plan_phase=Approved——后续写入仍逐次过策略判定。
    #[test]
    fn auto_submit_plan_fast_pass_returns_ready_verdict_without_review() {
        let mut cp = auto_checkpoint();
        let submission = PlanSubmission {
            plan_summary: "补充 README 的构建说明段落".to_string(),
            target_files: vec!["docs/README.md".to_string()],
            ..Default::default()
        };
        let res = auto_policy_verdict(&mut cp, &submission, Path::new("/tmp"), 7)
            .expect("whitelisted files must fast-pass");
        assert!(res.success, "{}", res.stderr);
        assert!(res.stdout.contains("无需审查"));
        assert!(cp.submitted_plan.is_some());
        assert_eq!(cp.submitted_plan_step, Some(7));
        assert_eq!(
            cp.approved_target_files.as_deref(),
            Some(&["docs/README.md".to_string()][..])
        );
        assert!(!cp.plan_phase.is_approved(), "直通判定不提前放行整轮写入");
    }

    /// Escalate：高危文件 → 返回 None 落入既有 Tech Lead 派发链，模式切换为审查。
    #[test]
    fn auto_submit_plan_escalate_flips_mode_to_cross_review() {
        let mut cp = auto_checkpoint();
        let submission = PlanSubmission {
            plan_summary: "重构 submit_plan 的提交判定流程".to_string(),
            target_files: vec!["src-tauri/src/agent/plan_submit.rs".to_string()],
            ..Default::default()
        };
        let res = auto_policy_verdict(&mut cp, &submission, Path::new("/tmp"), 7);
        assert!(res.is_none(), "src-tauri/** 高危文件必须落入审查派发链");
        assert_eq!(cp.plan_mode, PlanExecutionMode::CrossReview);
    }

    /// 空 target_files（只读交付方案）→ FastPass，但不登记核准范围。
    #[test]
    fn auto_submit_plan_empty_targets_fast_pass_without_scope() {
        let mut cp = auto_checkpoint();
        let submission = PlanSubmission {
            plan_summary: "纯只读分析报告，不改动任何文件".to_string(),
            target_files: vec![],
            ..Default::default()
        };
        let res = auto_policy_verdict(&mut cp, &submission, Path::new("/tmp"), 7)
            .expect("empty targets must fast-pass");
        assert!(res.success);
        assert!(cp.approved_target_files.is_none());
    }
}
