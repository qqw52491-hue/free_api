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
    if let Some(reason) = crate::agent::plan_gate::submit_plan_rejection(checkpoint) {
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
        return reject(
            "【⚠️ submit_plan 已拒绝】仍有子任务在后台运行。请先调用 wait_jobs 收取结论后再决定是否需要新一轮评审。"
                .to_string(),
            "plan_liveness:submit_plan_in_flight",
        );
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
    let question: String = format!("【方案评审·第 {} 轮】{}", discussion_round, summary_flat)
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
    if reviewer_model == routing.model_id {
        let (preferred, _note) = crate::agent::router::prefer_cross_family_reviewer(
            &*db.0.lock().unwrap(),
            &reviewer_model,
        );
        reviewer_model = preferred;
    }
    let mut snapshot = crate::agent::child_reviewer::PlanReviewerDispatchSnapshot::new(
        &reviewer_model,
        routing.tier.to_string(),
        model_routing.reasoning_effort_for(&routing.tier),
        crate::agent::tools::role_manager::persona_block(&card),
        effective_params.clone(),
    );
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
        )
        .await
    {
        Ok(child_run_id) => {
            checkpoint.commit_plan_submission(
                submission.clone(),
                &child_run_id,
                &snapshot,
                step_id,
            );
            DispatchResult { view: None,
                success: true,
                stdout: format!(
                    "【✅ 方案已提交并进入审查】结构化方案已落账，Tech Lead 方案评审已异步派发（child_run_id: {}，第 {} 轮）。\n\
                     请调用 wait_jobs(job_ids:[\"{}\"]) 挂起等待裁决；结论注入后按 decision 行动：approved 放行写入、rebuttal 修订后重审、escalate 按指定专家派联合会审。",
                    child_run_id, discussion_round, child_run_id
                ),
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
