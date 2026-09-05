//! 阶段级写入审查：补齐「方案批准后 → 收尾审查前」中段零独立审查的缺口。
//!
//! 双触发（取先到者）：
//! 1. 语义触发：主 Agent 通过 todo_update 标记某阶段完成且该阶段有真实写入；
//! 2. 行为兜底：距上次阶段审查累计写入 N 个文件（防 todo 记账博弈——无论模型
//!    怎么记账，写盘这个事实逃不掉）。
//!
//! 拦截语义：blocking_regardless（中间态即错）未解决时拦截写入，解套唯一路径是
//! resume_child 对质撤回；blocking_if_final 只记账带入收尾审查。
//! 基础设施失败（超时/取消）不烧预算且该阶段视为「未审」——计数不清零，会重新触发。

use crate::agent::checkpoint::{AgentCheckpoint, PlanExecutionMode, MAX_STAGE_REVIEWS};
use crate::agent::context::SandwichContext;
use crate::agent::router::{self, AgentRouter, ModelRoutingConfig};
use crate::agent::types::AgentInstruction;
use crate::db::DbState;
use serde_json::json;
use std::path::Path;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;

fn stage_review_enabled() -> bool {
    crate::agent::channel::channel_config_shared()
        .read()
        .ok()
        .and_then(|c| c.stage_review_enabled)
        .unwrap_or(true)
}

/// 阶段审查预算上限：默认常量 5；长任务可通过 FREE_API_STAGE_REVIEW_MAX_ROUNDS 放宽（上限 40）
fn stage_review_budget_max() -> u32 {
    std::env::var("FREE_API_STAGE_REVIEW_MAX_ROUNDS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .map(|v| v.clamp(1, 40))
        .unwrap_or(MAX_STAGE_REVIEWS)
}

fn stage_review_max_files() -> u32 {
    crate::agent::channel::channel_config_shared()
        .read()
        .ok()
        .and_then(|c| c.stage_review_max_files)
        .unwrap_or(6)
}

/// 双触发判定。返回 Some(原因) 表示应派发阶段审查。
pub fn should_fire(
    checkpoint: &AgentCheckpoint,
    context: &SandwichContext,
    instruction: &AgentInstruction,
) -> Option<String> {
    // 仅在方案已批准后介入（批准前由方案审查负责），全自动模式保持原有行为
    if !checkpoint.plan_phase.is_approved() {
        return None;
    }
    if checkpoint.plan_mode == PlanExecutionMode::Auto {
        return None;
    }
    if !stage_review_enabled() {
        return None;
    }
    // 已有在途审查或尚有未解决的拦截时，不重复派发（等消费或对质收敛）
    if checkpoint.stage_review_run_id.is_some() || checkpoint.stage_review_rebuttal_run_id.is_some()
    {
        return None;
    }
    if checkpoint.stage_review_blockers_open > 0 {
        return None;
    }
    if checkpoint.stage_review_budget_used >= stage_review_budget_max() {
        return None;
    }

    // 触发 3（先于 writes_since 早退判定）：shell 批量修改兜底——shell_exec 一条命令可改
    // N 个文件并绕过 write_file 门禁，以 git 探针实测的未覆盖变更数独立兜底
    //（非 git 环境按每次调用计 1，阈值语义不变）。
    let shell_uncovered = checkpoint
        .uncovered_shell_mutations
        .saturating_sub(checkpoint.stage_review_shell_baseline);
    if shell_uncovered >= stage_review_max_files() {
        return Some(format!(
            "自上次阶段审查以来 shell 类工具修改了约 {} 个文件（阈值 {}）",
            shell_uncovered,
            stage_review_max_files()
        ));
    }

    let writes_since = context
        .side_effects
        .len()
        .saturating_sub(checkpoint.stage_review_baseline_side_effects as usize) as u32;
    if writes_since == 0 {
        return None;
    }

    // 触发 1：todo 阶段推进且伴随真实写入
    let todo_advanced = instruction.todo_update.iter().any(|t| {
        matches!(
            t.status.trim().to_ascii_lowercase().as_str(),
            "done" | "completed"
        )
    });
    if todo_advanced {
        return Some(format!(
            "todo 阶段完成且自上次审查以来有 {} 个写入动作",
            writes_since
        ));
    }

    // 触发 2：行为兜底——累计写入达到阈值
    if writes_since >= stage_review_max_files() {
        return Some(format!(
            "距上次阶段审查已累计写入 {} 个文件（阈值 {}）",
            writes_since,
            stage_review_max_files()
        ));
    }
    None
}

/// 派发一次阶段审查。证据包由宿主组装（任务目标 + 阶段计划 + 当前累计 diff），
/// reviewer 走跨家族优选（角色卡钉死除外）。
pub async fn dispatch(
    app: &AppHandle,
    db: &DbState,
    parent_run_id: &str,
    session_id: &str,
    workspace: &Path,
    checkpoint: &mut AgentCheckpoint,
    context: &SandwichContext,
    model_routing: &ModelRoutingConfig,
    step_id: usize,
    interrupt_rx: watch::Receiver<u64>,
) -> Result<(), String> {
    let routing = AgentRouter::route_for_role(
        context,
        model_routing,
        step_id,
        router::roles::REVIEWER,
    );
    let (model_to_use, cross_family_note) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        router::prefer_cross_family_reviewer(&conn, &routing.model_id)
    };

    let evidence = build_stage_evidence(context, workspace);

    let params = json!({
        "question": "阶段级写入审查：审查当前阶段变更是否满足阶段目标且无 blocking 缺陷",
        "evidence_excerpt": evidence,
    });
    let child_run_id = crate::agent::child_reviewer::spawn_child_reviewer(
        Some(app.clone()),
        db,
        parent_run_id,
        session_id,
        workspace,
        &model_to_use,
        &routing.tier.to_string(),
        model_routing.reasoning_effort_for(&routing.tier),
        &params,
        "",
        interrupt_rx,
    )
    .await?;

    checkpoint.stage_review_run_id = Some(child_run_id.clone());
    checkpoint.stage_review_budget_used += 1;
    // shell 变更基线同样在派发时记录（覆盖语义与 baseline_side_effects 一致）
    checkpoint.stage_review_shell_baseline = checkpoint.uncovered_shell_mutations;
    // 覆盖基线在【派发时】记录：reviewer 证据包取的是此刻的 diff，此后发生的写入
    // 不在本轮审查范围内，必须留在基线之外让下一次触发兜住（防覆盖记账竞态）。
    checkpoint.stage_review_baseline_side_effects = context.side_effects.len() as u32;
    if let Some(note) = cross_family_note {
        let _ = app.emit(
            &format!("agent-log-{}", session_id),
            format!("📋 阶段审查已派发 ({}):{}", child_run_id, note),
        );
    }
    Ok(())
}

/// 组装阶段审查证据包：任务目标 + 阶段计划 + diff 尾部。
/// 硬约束：总长不得超过 child_reviewer 的 evidence_excerpt 预算（6000 字符），
/// 超限会导致派发在解析层被静默拒绝（实测教训）。diff 取尾部——最近的变更与当前阶段最相关。
fn build_stage_evidence(context: &SandwichContext, workspace: &Path) -> String {
    const MAX_EVIDENCE_CHARS: usize = 6_000;

    let mut evidence = String::new();
    evidence.push_str(&format!("【任务目标】{}\n", context.user_goal));
    let todos = context
        .todo_list
        .iter()
        .map(|t| format!("- [{}] {} ({})", t.status, t.description, t.id))
        .collect::<Vec<_>>()
        .join("\n");
    evidence.push_str(&format!(
        "【阶段计划（注意：这是中间态，非最终形态，final_only 级问题请如实标注）】\n{}\n",
        todos
    ));

    let diff_block = match crate::agent::reviewer_gatekeeper::GitBaseline::capture(workspace)
        .collect_git_diff(workspace)
    {
        Some((diff, _)) => format!(
            "【当前累计变更 diff（含自上次阶段审查以来的全部写入，尾部为最新变更）】\n{}",
            diff
        ),
        None => "【当前累计变更 diff】（无法获取 git diff，请基于方案与目标做结构性审查）\n".to_string(),
    };

    // 先按预算截断 diff 尾部，再拼入总包；总包兜底再截一次
    let used = evidence.chars().count();
    let diff_allowance = MAX_EVIDENCE_CHARS.saturating_sub(used + 64).max(200);
    let diff_tail: String = {
        let chars: Vec<char> = diff_block.chars().collect();
        let allowance = diff_allowance.max(200);
        if chars.len() > allowance {
            let tail: String = chars[chars.len() - allowance..].iter().collect();
            format!("…（前段截断，保留尾部）\n{}", tail)
        } else {
            diff_block
        }
    };
    evidence.push_str(&diff_tail);
    if evidence.chars().count() > MAX_EVIDENCE_CHARS {
        evidence = evidence.chars().take(MAX_EVIDENCE_CHARS).collect();
    }
    evidence
}

/// 消费阶段审查结果（成功或基础设施失败均走这里）。
/// 返回注入父 Agent 上下文的摘要文案。
pub fn apply_stage_result(
    checkpoint: &mut AgentCheckpoint,
    result: &crate::agent::child_reviewer::ChildReviewResult,
    _context: &SandwichContext,
) -> String {
    let was_rebuttal = checkpoint.stage_review_rebuttal_run_id.as_deref()
        == Some(result.child_run_id.as_str());
    checkpoint.stage_review_run_id = None;
    checkpoint.stage_review_rebuttal_run_id = None;

    // 基础设施失败：退还阶段审查次数、基线不清零（该阶段视为「未审」，会重新触发）
    if matches!(
        result.error_kind.as_deref(),
        Some("timed_out") | Some("interrupted") | Some("cancelled")
    ) {
        if checkpoint.reviewer_budget_refunds < 3 && checkpoint.stage_review_budget_used > 0 {
            checkpoint.stage_review_budget_used -= 1;
            checkpoint.reviewer_budget_refunds += 1;
        }
        return "⚠️ 阶段审查任务未正常完成（阶段审查次数已退还）。该阶段视为未审，累计写入达到阈值后将自动重审。".to_string();
    }

    // 基线已在派发时记录（见 dispatch）；此处不得用当前台账长度覆盖，
    // 否则派发后、消费前的写入会被错误标记为已覆盖。派发后的新写入计入
    // writes_since，由下一次触发（todo 推进/文件数阈值）安排复检。
    checkpoint.stage_review_blockers_open = result.stage_blocking_open();
    checkpoint.stage_review_final_only = result.stage_final_only_lines();

    let (added, withdrawn, maintained) = result.objection_stats();
    checkpoint.review_total_withdrawn += withdrawn;
    checkpoint.review_total_maintained += maintained;
    let mut text = format!(
        "📊 阶段审查完成: blocking_regardless 未解决 {} 条 / final_only 记账 {} 条；异议动态: 新增 {} / 撤回 {} / 维持 {}；成本: 输入 {} / 输出 {} tokens, 耗时 {}ms。",
        checkpoint.stage_review_blockers_open,
        checkpoint.stage_review_final_only.len(),
        added,
        withdrawn,
        maintained,
        result.prompt_tokens,
        result.completion_tokens,
        result.duration_ms
    );
    if was_rebuttal {
        text.push_str("（对质复检结论）");
    }
    if checkpoint.stage_review_blockers_open > 0 {
        text.push_str(&format!(
            "\n🚨 后续写入已被拦截，请用 resume_child 对质解套（child_run_id 见门禁提示）。"
        ));
    }
    if !checkpoint.stage_review_final_only.is_empty() {
        text.push_str("\n【final_only 记账（最终交付前必须解决）】");
        for line in &checkpoint.stage_review_final_only {
            text.push_str(&format!("\n- {}", line));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::checkpoint::PlanExecutionMode;
    use crate::agent::child_reviewer::{ChildReviewResult, ReviewFinding};
    use crate::agent::types::TodoItem;

    fn approved_checkpoint() -> AgentCheckpoint {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test_run".to_string(), &context);
        cp.set_execution_mode(PlanExecutionMode::CrossReview);
        cp.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
        cp
    }

    fn instruction_with_todo_done() -> AgentInstruction {
        AgentInstruction {
            todo_update: vec![TodoItem {
                id: 1,
                status: "done".to_string(),
                description: "阶段一".to_string(),
                evidence_output_id: None,
            }],
            ..Default::default()
        }
    }

    fn finding(id: &str, severity: &str, finality: &str) -> ReviewFinding {
        ReviewFinding {
            id: id.to_string(),
            severity: severity.to_string(),
            finality: finality.to_string(),
            location: String::new(),
            current: String::new(),
            why: "问题".to_string(),
            suggestion: String::new(),
            evidence_strength: "confirmed".to_string(),
        }
    }

    #[test]
    fn should_fire_requires_approved_non_auto_mode_and_real_writes() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let instruction = instruction_with_todo_done();

        // 未批准：不触发（方案审查负责批准前）
        let mut cp = approved_checkpoint();
        cp.plan_phase = crate::agent::checkpoint::PlanPhase::Reviewing;
        assert!(should_fire(&cp, &context, &instruction).is_none());

        // 全自动模式：不触发（保持原有行为）
        let mut cp = approved_checkpoint();
        cp.set_execution_mode(PlanExecutionMode::Auto);
        assert!(should_fire(&cp, &context, &instruction).is_none());

        // 批准 + 交叉审查，但无写入：不触发
        let cp = approved_checkpoint();
        assert!(should_fire(&cp, &context, &instruction).is_none());

        // 有写入 + todo 阶段完成：触发
        let mut context2 = SandwichContext::new("system".to_string(), "goal".to_string());
        context2.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        let cp = approved_checkpoint();
        assert!(should_fire(&cp, &context2, &instruction).is_some());
    }

    #[test]
    fn write_count_threshold_is_the_behavioral_backstop() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        // 无 todo 更新的普通指令
        let instruction = AgentInstruction::default();
        // 阈值默认 6：写 5 个不触发
        for i in 0..5 {
            context.record_side_effect(i, "write_file".to_string(), format!("f{}.rs", i));
        }
        let cp = approved_checkpoint();
        assert!(should_fire(&cp, &context, &instruction).is_none(), "5 个文件不应触发");
        // 第 6 个触发（记账博弈兜底：模型不更新 todo 也逃不掉）
        context.record_side_effect(6, "write_file".to_string(), "f6.rs".to_string());
        assert!(should_fire(&cp, &context, &instruction).is_some(), "达到阈值必须触发");
    }

    #[test]
    fn stage_evidence_never_exceeds_child_reviewer_budget() {
        let dir = std::env::temp_dir().join(format!("stage-ev-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git").args(args).current_dir(&dir).status().unwrap()
        };
        std::fs::write(dir.join("big.rs"), "fn base() {}
").unwrap();
        assert!(git(&["init", "-q"]).success());
        assert!(git(&["-c", "user.email=t@t", "-c", "user.name=t", "add", "-A"]).success());
        assert!(git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init"]).success());
        // 已跟踪文件的修改才会进 git diff HEAD
        std::fs::write(dir.join("big.rs"), "x".repeat(40_000)).unwrap();

        let mut context = SandwichContext::new("system".to_string(), "目标".to_string());
        context.user_goal = "很长的目标".repeat(50);
        for i in 0..24 {
            context.todo_list.push(crate::agent::types::TodoItem {
                id: i,
                status: "done".to_string(),
                description: "很长的阶段任务描述".repeat(5),
                evidence_output_id: None,
            });
        }

        let evidence = build_stage_evidence(&context, &dir);
        assert!(
            evidence.chars().count() <= 6_000,
            "证据包超限会被 child_reviewer 解析层拒绝（实测教训）: {}",
            evidence.chars().count()
        );
        assert!(evidence.contains("【任务目标】"), "目标必须保留");
        assert!(evidence.contains("前段截断"), "大 diff 应截断保留尾部");

        // 小 diff 不截断
        std::fs::write(dir.join("big.rs"), "fn small() {}
").unwrap();
        let evidence2 = build_stage_evidence(&context, &dir);
        assert!(!evidence2.contains("前段截断"));
        assert!(evidence2.chars().count() <= 6_000);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shell_batch_mutations_trigger_stage_review_independently() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let instruction = AgentInstruction::default();
        let cp = approved_checkpoint();

        // shell 未覆盖变更 5 个：不触发（同 todo/文件数阈值）
        let mut cp5 = approved_checkpoint();
        cp5.uncovered_shell_mutations = 5;
        assert!(should_fire(&cp5, &context, &instruction).is_none(), "5 个 shell 变更不应触发");

        // 达到阈值 6：无 todo 推进也必须触发（防旁路的核心断言）
        let mut cp6 = approved_checkpoint();
        cp6.uncovered_shell_mutations = 6;
        let reason = should_fire(&cp6, &context, &instruction).unwrap();
        assert!(reason.contains("shell"), "触发原因应标明 shell 旁路: {}", reason);

        // 基线语义：派发时快照后，新增变更才计入未覆盖
        let mut cp_baselined = approved_checkpoint();
        cp_baselined.uncovered_shell_mutations = 10;
        cp_baselined.stage_review_shell_baseline = 8;
        assert!(should_fire(&cp_baselined, &context, &instruction).is_none(), "基线之上的增量 2 < 6 不触发");
        cp_baselined.uncovered_shell_mutations = 14;
        assert!(should_fire(&cp_baselined, &context, &instruction).is_some(), "基线上增量 6 触发");

        let _ = cp; let _ = cp5;
    }

    #[test]
    fn should_fire_respects_budget_inflight_and_blockers() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        let instruction = instruction_with_todo_done();

        // 预算耗尽
        let mut cp = approved_checkpoint();
        cp.stage_review_budget_used = crate::agent::checkpoint::MAX_STAGE_REVIEWS;
        assert!(should_fire(&cp, &context, &instruction).is_none());

        // 在途审查
        let mut cp = approved_checkpoint();
        cp.stage_review_run_id = Some("child-1".to_string());
        assert!(should_fire(&cp, &context, &instruction).is_none());

        // 有未解决拦截时不重复派发（等对质收敛）
        let mut cp = approved_checkpoint();
        cp.stage_review_blockers_open = 1;
        assert!(should_fire(&cp, &context, &instruction).is_none());
    }

    #[test]
    fn apply_stage_result_success_resets_baseline_and_sets_blockers() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        context.record_side_effect(2, "write_file".to_string(), "b.rs".to_string());
        let mut cp = approved_checkpoint();
        cp.stage_review_run_id = Some("child-9".to_string());
        // 派发时基线已记录为 2（dispatch 时刻的台账长度）
        cp.stage_review_baseline_side_effects = 2;

        let result = ChildReviewResult {
            child_run_id: "child-9".to_string(),
            status: "succeeded".to_string(),
            findings: vec![
                finding("F1", "blocking", "regardless"),
                finding("F2", "blocking", "final_only"),
                finding("F3", "suggestion", "regardless"),
            ],
            confidence: "confirmed".to_string(),
            evidence_refs: vec![],
            missing_evidence: vec![],
            duration_ms: 100,
            completion_tokens: 10,
            prompt_tokens: 20,
            error_kind: None,
            raw_result_json: None,
            kind: None,
            routing: None,
            withdrawn_findings: vec![],
            objection_verdicts: vec![],
            scorecard: None,
        };
        let note = apply_stage_result(&mut cp, &result, &context);
        assert_eq!(cp.stage_review_blockers_open, 1, "仅 regardless 计入拦截");
        assert_eq!(cp.stage_review_final_only.len(), 1, "final_only 仅记账");
        assert_eq!(cp.stage_review_baseline_side_effects, 2, "消费不得覆盖派发时基线（防覆盖记账竞态）");
        assert!(cp.stage_review_run_id.is_none());
        assert!(note.contains("拦截"), "有 regardless 拦截时必须给出解套提示");

        // 基线推进后：同样写入不再触发
        assert!(should_fire(&cp, &context, &instruction_with_todo_done()).is_none());
    }

    #[test]
    fn apply_stage_result_infra_failure_marks_stage_unreviewed() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        let mut cp = approved_checkpoint();
        cp.stage_review_run_id = Some("child-9".to_string());
        cp.stage_review_budget_used = 1;

        let result = ChildReviewResult {
            child_run_id: "child-9".to_string(),
            status: "timed_out".to_string(),
            findings: vec![],
            confidence: "unverified".to_string(),
            evidence_refs: vec![],
            missing_evidence: vec![],
            duration_ms: 100,
            completion_tokens: 0,
            prompt_tokens: 0,
            error_kind: Some("timed_out".to_string()),
            raw_result_json: None,
            kind: None,
            routing: None,
            withdrawn_findings: vec![],
            objection_verdicts: vec![],
            scorecard: None,
        };
        let _ = apply_stage_result(&mut cp, &result, &context);
        // 预算退还 + 基线不动（视为未审，会重新触发）+ 不产生拦截
        assert_eq!(cp.stage_review_budget_used, 0);
        assert_eq!(cp.stage_review_baseline_side_effects, 0);
        assert_eq!(cp.stage_review_blockers_open, 0);
        assert!(cp.stage_review_run_id.is_none());
    }
}
