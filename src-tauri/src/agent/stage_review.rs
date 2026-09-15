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
    let config_enabled = crate::agent::channel::channel_config_shared()
        .read()
        .ok()
        .and_then(|c| c.stage_review_enabled);
    crate::agent::channel::profile_stage_review_enabled(
        crate::agent::channel::active_gate_profile(),
        config_enabled,
    )
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
    let config_max = crate::agent::channel::channel_config_shared()
        .read()
        .ok()
        .and_then(|c| c.stage_review_max_files);
    crate::agent::channel::profile_stage_review_max_files(
        crate::agent::channel::active_gate_profile(),
        config_max,
    )
}

/// 双触发判定。返回 Some(原因) 表示应派发阶段审查。
pub fn should_fire(
    checkpoint: &AgentCheckpoint,
    context: &SandwichContext,
    instruction: &AgentInstruction,
    model_routing: &ModelRoutingConfig,
) -> Option<String> {
    // 任务入口分流判决 stage_review_mode="none" → 阶段审查整体关闭；
    // 注意这只关闭"中段复审"，已判出的 stage blockers 拦截在写入门禁处仍生效。
    if checkpoint
        .triage_verdict
        .as_ref()
        .is_some_and(|v| !v.needs_stage_review())
    {
        return None;
    }
    // 0. 会话级阶段审查门禁覆盖检查（若为 disabled/none 则直通免审）
    if let Some(ref ovr) = model_routing.stage_reviewer_override {
        let clean = ovr.trim();
        if clean.eq_ignore_ascii_case("disabled") || clean.eq_ignore_ascii_case("none") {
            return None;
        }
    }
    // 任务正在调用 finish 交付结果时绝不触发异步阶段审查（防止与终态门禁冲突导致自己拦截自己）
    if instruction.get_tool().eq_ignore_ascii_case("finish") {
        return None;
    }
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
    if checkpoint.has_in_flight_stage_review() {
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

/// 自动自适应优选审查专家：
/// 1. 若为单审模式（is_committee = false）：
///    - 若启用的审查专家只有 1 位，自动挂载该专家；
///    - 若有多位，根据改动文件类型（前端 vs 后端）匹配对应 1 位专家；
///    - 若无审查专家，返回空 Vec 走系统默认异构对抗模型兜底。
/// 2. 若为联合会审模式（is_committee = true）：
///    - 根据改动文件类型挑选匹配专家并发派发，硬性上限 3 位（防 Token 爆炸）。
fn resolve_adaptive_reviewer_cards(
    db: &DbState,
    context: &SandwichContext,
    disabled_cards: &[String],
    is_committee: bool,
) -> Vec<crate::agent::tools::role_manager::RoleCard> {
    let Ok(cards) = crate::agent::tools::role_manager::list_cards(db, false) else {
        return Vec::new();
    };
    let active_reviewers: Vec<crate::agent::tools::role_manager::RoleCard> = cards
        .into_iter()
        .filter(|c| {
            c.enabled
                && c.allows_kind("reviewer")
                && !crate::agent::tools::role_manager::is_card_disabled_for(disabled_cards, &c.name, "reviewer")
        })
        .collect();

    if active_reviewers.is_empty() {
        return Vec::new();
    }

    let all_paths: Vec<String> = context
        .side_effects
        .iter()
        .flat_map(|s| {
            let mut paths = s.affected_paths.clone();
            paths.push(s.description.clone());
            paths
        })
        .collect();

    let has_frontend = all_paths.iter().any(|p| {
        let l = p.to_ascii_lowercase();
        l.ends_with(".vue")
            || l.ends_with(".tsx")
            || l.ends_with(".jsx")
            || l.ends_with(".css")
            || l.ends_with(".scss")
            || l.ends_with(".html")
    });
    let has_backend = all_paths.iter().any(|p| {
        let l = p.to_ascii_lowercase();
        l.ends_with(".rs")
            || l.ends_with(".go")
            || l.ends_with(".py")
            || l.ends_with(".sql")
            || l.ends_with(".toml")
            || l.ends_with(".java")
    });

    let is_fe_card = |c: &crate::agent::tools::role_manager::RoleCard| {
        let s = format!("{} {} {}", c.name, c.label, c.scope_hints).to_ascii_lowercase();
        s.contains("前端")
            || s.contains("frontend")
            || s.contains("ui")
            || s.contains("vue")
            || s.contains("react")
    };
    let is_be_card = |c: &crate::agent::tools::role_manager::RoleCard| {
        let s = format!("{} {} {}", c.name, c.label, c.scope_hints).to_ascii_lowercase();
        s.contains("后端")
            || s.contains("backend")
            || s.contains("安全")
            || s.contains("security")
            || s.contains("api")
    };

    if !is_committee {
        if active_reviewers.len() == 1 {
            return vec![active_reviewers.into_iter().next().unwrap()];
        }
        if has_frontend && !has_backend {
            if let Some(c) = active_reviewers.iter().find(|c| is_fe_card(c)) {
                return vec![c.clone()];
            }
        } else if has_backend && !has_frontend {
            if let Some(c) = active_reviewers.iter().find(|c| is_be_card(c)) {
                return vec![c.clone()];
            }
        }
        return vec![active_reviewers.into_iter().next().unwrap()];
    }

    // 联合会审（committee）多审模式：上限 3 位
    if active_reviewers.len() <= 3 {
        let mut sorted = active_reviewers;
        sorted.sort_by_key(|c| {
            let fe = is_fe_card(c);
            let be = is_be_card(c);
            if has_frontend && fe {
                0
            } else if has_backend && be {
                0
            } else if fe || be {
                1
            } else {
                2
            }
        });
        return sorted;
    }

    // 活跃专家 > 3 位时：优先挑选契合领域的专家（至多 3 位）
    let mut selected: Vec<crate::agent::tools::role_manager::RoleCard> = Vec::new();

    if has_frontend {
        for c in active_reviewers.iter().filter(|c| is_fe_card(c)) {
            if !selected.iter().any(|s| s.name == c.name) {
                selected.push(c.clone());
                if selected.len() >= 2 {
                    break;
                }
            }
        }
    }
    if has_backend {
        for c in active_reviewers.iter().filter(|c| is_be_card(c)) {
            if !selected.iter().any(|s| s.name == c.name) {
                selected.push(c.clone());
                if selected.len() >= 3 {
                    break;
                }
            }
        }
    }

    for c in active_reviewers.into_iter() {
        if selected.len() >= 3 {
            break;
        }
        if !selected.iter().any(|s| s.name == c.name) {
            selected.push(c);
        }
    }

    selected
}

/// 派发一次阶段审查。证据包由宿主组装（任务目标 + 阶段计划 + 当前累计 diff），
/// reviewer 走自适应专家挂载或跨家族异构模型优选。在联合会审模式下，并发唤起多位专家。
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
    // 1. 检查会话级是否有指定的审查官角色卡，未硬性指定时自适应从启用专家中匹配
    let (is_explicit_override, explicit_card) = if let Some(ref ovr) = model_routing.stage_reviewer_override {
        let clean = ovr.trim();
        if clean.eq_ignore_ascii_case("disabled") || clean.eq_ignore_ascii_case("none") {
            return Ok(());
        } else if !clean.is_empty()
            && !clean.eq_ignore_ascii_case("default")
            && !clean.eq_ignore_ascii_case("follow_global")
            && !clean.eq_ignore_ascii_case("auto")
        {
            if crate::agent::tools::role_manager::is_card_disabled_for(&model_routing.disabled_role_cards, clean, "reviewer") {
                (false, None)
            } else {
                match crate::agent::tools::role_manager::resolve_enabled_card(db, clean) {
                    Ok(Some(card)) if card.allows_kind("reviewer") => (true, Some(card)),
                    _ => (false, None),
                }
            }
        } else {
            (false, None)
        }
    } else {
        (false, None)
    };

    // 会话级硬性指定单张卡片时：指定单人优先，committee 自动失效
    let role_cards = if is_explicit_override {
        explicit_card.into_iter().collect()
    } else {
        resolve_adaptive_reviewer_cards(
            db,
            context,
            &model_routing.disabled_role_cards,
            model_routing.stage_review_committee,
        )
    };

    let routing = AgentRouter::route_for_role(
        context,
        model_routing,
        step_id,
        router::roles::REVIEWER,
    );

    // 新一轮阶段审查重置拦截状态（final_only 跨阶段累加 carry-over 到收尾审查，不可清空）
    checkpoint.stage_review_blockers_open = 0;

    if role_cards.is_empty() {
        // 无挂载专家卡：走系统默认的跨家族异构对抗模型兜底
        let (model_to_use, cross_family_note) = {
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            let has_explicit_reviewer_slot = model_routing
                .role_models
                .get(router::roles::REVIEWER)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);

            if has_explicit_reviewer_slot {
                (routing.model_id.clone(), None)
            } else {
                router::prefer_cross_family_reviewer(&conn, &routing.model_id)
            }
        };

        let evidence = build_stage_evidence(context, workspace, None);
        let question = "阶段级写入审查：审查当前阶段变更是否满足阶段目标且无 blocking 缺陷".to_string();
        let params = json!({
            "role": "stage_reviewer",
            "role_label": "🛡️ 阶段综合审查官",
            "question": question,
            "evidence_excerpt": evidence,
        });

        let envelope = crate::agent::protocol::DispatchEnvelope::reviewer(
            parent_run_id,
            session_id,
            &model_to_use,
            params,
        )
        .with_task_id(format!("stage_review_{}", checkpoint.step_id))
        .with_tier(&routing.tier.to_string())
        .with_effort(model_routing.reasoning_effort_for(&routing.tier));

        let child_run_id = envelope
            .dispatch(Some(app.clone()), db, workspace, interrupt_rx)
            .await?;

        checkpoint.stage_review_run_id = Some(child_run_id.clone());
        checkpoint.stage_review_run_ids = vec![child_run_id.clone()];
        checkpoint.stage_review_budget_used += 1;
        checkpoint.stage_review_shell_baseline = checkpoint.uncovered_shell_mutations;
        checkpoint.stage_review_baseline_side_effects = context.side_effects.len() as u32;

        let note_text = cross_family_note.map(|n| format!(":{}", n)).unwrap_or_default();
        let _ = app.emit(
            &format!("agent-log-{}", session_id),
            format!("📋 阶段审查已派发 ({}){}", child_run_id, note_text),
        );
        return Ok(());
    }

    // 挂载专家审查（单审或多专家联合会审并发派发）
    let mut dispatched_run_ids = Vec::new();
    for card in &role_cards {
        let model_to_use = {
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            let has_explicit_reviewer_slot = model_routing
                .role_models
                .get(router::roles::REVIEWER)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);

            if !card.model_id.is_empty() {
                card.model_id.clone()
            } else if has_explicit_reviewer_slot {
                routing.model_id.clone()
            } else {
                router::prefer_cross_family_reviewer(&conn, &routing.model_id).0
            }
        };

        let focus_context = {
            let domains = card.parse_focus_domains();
            if !domains.is_empty() {
                crate::agent::context_collector::assemble_focus_context(
                    &domains,
                    workspace,
                    None,
                    None,
                )
                .await
            } else {
                None
            }
        };

        let evidence = build_stage_evidence(context, workspace, focus_context.as_deref());
        let question = format!(
            "阶段级写入审查（专家角色：{}）：审查当前阶段变更是否满足阶段目标、无 blocking 缺陷并符合角色规范",
            card.label
        );
        let params = json!({
            "role": card.name,
            "role_label": card.label,
            "question": question,
            "evidence_excerpt": evidence,
        });

        let mut envelope = crate::agent::protocol::DispatchEnvelope::reviewer(
            parent_run_id,
            session_id,
            &model_to_use,
            params,
        )
        .with_task_id(format!("stage_review_{}_{}", checkpoint.step_id, card.name))
        .with_tier(&routing.tier.to_string())
        .with_effort(model_routing.reasoning_effort_for(&routing.tier));

        let persona = crate::agent::tools::role_manager::persona_block(card);
        if !persona.is_empty() {
            envelope = envelope.with_persona(persona);
        }
        if let Some(schema) = card.parse_custom_schema() {
            envelope = envelope.with_custom_schema(schema);
        }

        let child_run_id = envelope
            .dispatch(Some(app.clone()), db, workspace, interrupt_rx.clone())
            .await?;

        // ★ 立即注册：dispatch 成功后即刻写入 checkpoint，
        //   防止后续循环 dispatch 失败时已派出的任务成为孤儿。
        dispatched_run_ids.push(child_run_id.clone());
        checkpoint.stage_review_run_ids = dispatched_run_ids.clone();
        checkpoint.stage_review_run_id = dispatched_run_ids.first().cloned();

        let _ = app.emit(
            &format!("agent-log-{}", session_id),
            format!("📋 阶段审查已派发 ({}) (挂载专家: {})", child_run_id, card.label),
        );
    }
    // 预算语义：整轮联合会审无论派发 1 位还是多位，仅计 1 轮
    checkpoint.stage_review_budget_used += 1;
    checkpoint.stage_review_shell_baseline = checkpoint.uncovered_shell_mutations;
    checkpoint.stage_review_baseline_side_effects = context.side_effects.len() as u32;

    if dispatched_run_ids.len() > 1 {
        let _ = app.emit(
            &format!("agent-log-{}", session_id),
            format!("👥 联合会审已启动：共唤起 {} 位审查专家并发把关", dispatched_run_ids.len()),
        );
    }

    Ok(())
}

/// 组装阶段审查证据包：任务目标 + 阶段计划 + 专项材料(如有) + diff 尾部。
/// 硬约束：总长不得超过 child_reviewer 的 evidence_excerpt 预算（6000 字符），
/// 超限会导致派发在解析层被静默拒绝（实测教训）。diff 取尾部——最近的变更与当前阶段最相关。
fn build_stage_evidence(
    context: &SandwichContext,
    workspace: &Path,
    focus_context: Option<&str>,
) -> String {
    const MAX_EVIDENCE_CHARS: usize = 6_000;

    let mut evidence = String::new();
    evidence.push_str(&format!("【任务目标】{}\n", context.effective_goal()));
    let todos = context
        .todo_list
        .iter()
        .map(|t| format!("- [{}] {} ({})", t.status, t.description, t.id))
        .collect::<Vec<_>>()
        .join("\n");
    // 【实测死锁教训】oracle 会为未来阶段预生成占位存根（src/stage-XX.mjs 内容为
    // "return value;"）。它们出现在累计 diff 里是设计内的未来工作，审查员曾把
    // "存根未实现未来阶段义务"判成 blocking_regardless，拦截一切写入形成死锁
    // （模型被迫 ask_human，整轮评测终止）。必须在证据包里显式豁免。
    evidence.push_str(
        "【占位存根豁免（重要）】宿主 oracle 会为尚未到达的未来阶段预生成占位文件          src/stage-XX.mjs（内容为 return value; 存根）。这些文件出现在 diff 中属于设计内          的未来工作，不属于任何当前阶段的缺陷：不得因此判出 blocking（无论 regardless          还是 final_only），最多作为一条 suggestion 提及。你的审查对象是已实现代码的          结构性质量与当前阶段义务的落实情况。\n\n",
    );
    evidence.push_str(&format!(
        "【阶段计划（注意：这是中间态，非最终形态，final_only 级问题请如实标注）】\n{}\n\n",
        todos
    ));

    // 如果角色卡提取了 focus_domains 材料，按预算（最多 1500 字符）安全注入
    if let Some(focus) = focus_context {
        let focus_clean = focus.trim();
        if !focus_clean.is_empty() {
            let focus_snippet: String = focus_clean.chars().take(1500).collect();
            evidence.push_str(&format!("【审查专家专项关注材料】\n{}\n\n", focus_snippet));
        }
    }

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

/// 消费阶段审查交付回执（成功或基础设施失败均走这里）。
/// oracle_index：宿主判分「下一未解阶段」索引（None = 无 oracle 场景）。
/// blockers>0 时记录出生索引，供僵尸 blocker 清账判定。
/// 返回注入父 Agent 上下文的摘要文案。
pub fn apply_stage_receipt(
    checkpoint: &mut AgentCheckpoint,
    receipt: &crate::agent::protocol::DeliveryReceipt,
    _context: &SandwichContext,
    oracle_index: Option<u32>,
) -> String {
    let was_rebuttal = checkpoint.stage_review_rebuttal_run_id.as_deref()
        == Some(receipt.child_run_id.as_str());
    checkpoint.remove_stage_review_run(&receipt.child_run_id);
    if was_rebuttal {
        checkpoint.record_stage_review_completed(&receipt.child_run_id);
        checkpoint.stage_review_rebuttal_run_id = None;
    }

    // 基础设施失败：退还阶段审查次数、基线不清零（该阶段视为「未审」，会重新触发）
    if receipt.is_infra_failure() {
        if !checkpoint.has_in_flight_stage_review() {
            if checkpoint.reviewer_budget_refunds < 3 && checkpoint.stage_review_budget_used > 0 {
                checkpoint.stage_review_budget_used -= 1;
                checkpoint.reviewer_budget_refunds += 1;
            }
        }
        return "⚠️ 阶段审查任务未正常完成（阶段审查次数已退还）。该阶段视为未审，累计写入达到阈值后将自动重审。".to_string();
    }

    // 基线已在派发时记录（见 dispatch）；此处不得用当前台账长度覆盖，
    // 否则派发后、消费前的写入会被错误标记为已覆盖。派发后的新写入计入
    // writes_since，由下一次触发（todo 推进/文件数阈值）安排复检。
    let new_blockers = receipt.stage_blocking_open();
    if was_rebuttal {
        let (_, stats_withdrawn, _) = receipt.objection_stats();
        let payload_withdrawn = receipt.as_reviewer_delivery().map(|d| {
            let direct_withdrawn = d.withdrawn_findings.len() as u32;
            let verdict_withdrawn = d.objection_verdicts.iter().filter(|v| {
                v.verdict.eq_ignore_ascii_case("withdraw") || v.verdict.eq_ignore_ascii_case("withdrawn")
            }).count() as u32;
            direct_withdrawn.max(verdict_withdrawn)
        }).unwrap_or(0);
        let withdrawn = stats_withdrawn.max(payload_withdrawn);
        checkpoint.stage_review_blockers_open = checkpoint
            .stage_review_blockers_open
            .saturating_sub(withdrawn);

        // 【对质新/维持 blocker 兜底】对质分支中 new_blockers（审查员仍持有或新增的
        // blocking 异议）不直接加回 blockers_open（避免与其他在途专家的计数双加），
        // 但必须通过 confrontation 机制跟踪：2 轮对质仍维持/新增 → 自动降级
        // final_only 记账放行。与 loop_runner.rs 的 wait_jobs 路径保持一致。
        // 若不跟踪，对质期间审查员新提出的 blocking 问题会被静默丢弃，构成安全漏洞。
        let blocker_lines: Vec<String> = receipt.findings()
            .into_iter()
            .filter(|f| {
                f.severity.eq_ignore_ascii_case("blocking")
                    && f.finality != "final_only"
                    && !receipt.as_reviewer_delivery()
                        .map(|d| d.withdrawn_findings.contains(&f.id))
                        .unwrap_or(false)
            })
            .map(|f| format!("{}: {}", f.id, f.why))
            .collect();
        if checkpoint.note_stage_review_confrontation_maintained_for(Some(&receipt.child_run_id), blocker_lines, 2) {
            // 降级日志将在调用方 format 输出到 stage_notes
        }
    } else {
        checkpoint.stage_review_blockers_open += new_blockers;
    }

    // 僵尸 blocker 治理：记录出生阶段索引（无 blocker 时清空，避免陈旧索引干扰）
    if checkpoint.stage_review_blockers_open > 0 {
        checkpoint.stage_review_blocker_stage = oracle_index;
    } else if !checkpoint.has_in_flight_stage_review() {
        checkpoint.stage_review_blocker_stage = None;
    }

    let receipt_final_only = receipt.stage_final_only_lines();
    checkpoint.stage_review_final_only.extend(receipt_final_only);
    checkpoint.stage_review_final_only.sort();
    checkpoint.stage_review_final_only.dedup();

    let (added, withdrawn, maintained) = receipt.objection_stats();
    checkpoint.review_total_withdrawn += withdrawn;
    checkpoint.review_total_maintained += maintained;

    let in_flight_remaining = checkpoint.stage_review_run_ids.len();
    let committee_suffix = if in_flight_remaining > 0 {
        format!("（联合会审仍在途，剩余 {} 位专家）", in_flight_remaining)
    } else {
        String::new()
    };

    let mut text = format!(
        "📊 阶段审查完成: 本次 blocking {} 条 / 累计 blocking_regardless 未解决 {} 条 / final_only 记账 {} 条；异议动态: 新增 {} / 撤回 {} / 维持 {}；成本: 输入 {} / 输出 {} tokens, 耗时 {}ms。{}",
        new_blockers,
        checkpoint.stage_review_blockers_open,
        checkpoint.stage_review_final_only.len(),
        added,
        withdrawn,
        maintained,
        receipt.prompt_tokens,
        receipt.completion_tokens,
        receipt.duration_ms,
        committee_suffix
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

/// 兼容老接口：将 ChildReviewResult 转换为 DeliveryReceipt 交付回执后处理
pub fn apply_stage_result(
    checkpoint: &mut AgentCheckpoint,
    result: &crate::agent::child_reviewer::ChildReviewResult,
    context: &SandwichContext,
    oracle_index: Option<u32>,
) -> String {
    let receipt: crate::agent::protocol::DeliveryReceipt = result.clone().into();
    apply_stage_receipt(checkpoint, &receipt, context, oracle_index)
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

    use crate::agent::router::ModelRoutingConfig;

    fn default_routing() -> ModelRoutingConfig {
        ModelRoutingConfig::default()
    }

    #[test]
    fn should_fire_requires_approved_non_auto_mode_and_real_writes() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let instruction = instruction_with_todo_done();
        let routing = default_routing();

        // 未批准：不触发（方案审查负责批准前）
        let mut cp = approved_checkpoint();
        cp.plan_phase = crate::agent::checkpoint::PlanPhase::Reviewing;
        assert!(should_fire(&cp, &context, &instruction, &routing).is_none());

        // 全自动模式：不触发（保持原有行为）
        let mut cp = approved_checkpoint();
        cp.set_execution_mode(PlanExecutionMode::Auto);
        assert!(should_fire(&cp, &context, &instruction, &routing).is_none());

        // 批准 + 交叉审查，但无写入：不触发
        let cp = approved_checkpoint();
        assert!(should_fire(&cp, &context, &instruction, &routing).is_none());

        // 有写入 + todo 阶段完成：触发
        let mut context2 = SandwichContext::new("system".to_string(), "goal".to_string());
        context2.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        let cp = approved_checkpoint();
        assert!(should_fire(&cp, &context2, &instruction, &routing).is_some());
    }

    #[test]
    fn write_count_threshold_is_the_behavioral_backstop() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        let routing = default_routing();
        // 无 todo 更新的普通指令
        let instruction = AgentInstruction::default();
        // 阈值默认 6：写 5 个不触发
        for i in 0..5 {
            context.record_side_effect(i, "write_file".to_string(), format!("f{}.rs", i));
        }
        let cp = approved_checkpoint();
        assert!(should_fire(&cp, &context, &instruction, &routing).is_none(), "5 个文件不应触发");
        // 第 6 个触发（记账博弈兜底：模型不更新 todo 也逃不掉）
        context.record_side_effect(6, "write_file".to_string(), "f6.rs".to_string());
        assert!(should_fire(&cp, &context, &instruction, &routing).is_some(), "达到阈值必须触发");
    }

    #[test]
    fn stage_evidence_never_exceeds_child_reviewer_budget() {
        let dir = std::env::temp_dir().join(format!("stage-ev-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git").args(args).current_dir(&dir).status().unwrap()
        };
        std::fs::write(dir.join("big.rs"), "fn base() {}\n").unwrap();
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

        let evidence = build_stage_evidence(&context, &dir, None);
        assert!(
            evidence.chars().count() <= 6_000,
            "证据包超限会被 child_reviewer 解析层拒绝（实测教训）: {}",
            evidence.chars().count()
        );
        assert!(evidence.contains("【任务目标】"), "目标必须保留");
        assert!(evidence.contains("前段截断"), "大 diff 应截断保留尾部");

        // 小 diff 不截断
        std::fs::write(dir.join("big.rs"), "fn small() {}\n").unwrap();
        let evidence2 = build_stage_evidence(&context, &dir, None);
        assert!(!evidence2.contains("前段截断"));
        assert!(evidence2.chars().count() <= 6_000);

        // 验证 focus_context 注入且总长度仍受 6000 字符限制
        let focus_mat = "【关注领域重点】: 前端组件及可访问性规范".repeat(20);
        let evidence3 = build_stage_evidence(&context, &dir, Some(&focus_mat));
        assert!(evidence3.contains("【审查专家专项关注材料】"));
        assert!(evidence3.chars().count() <= 6_000);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shell_batch_mutations_trigger_stage_review_independently() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let instruction = AgentInstruction::default();
        let routing = default_routing();
        let cp = approved_checkpoint();

        // shell 未覆盖变更 5 个：不触发（同 todo/文件数阈值）
        let mut cp5 = approved_checkpoint();
        cp5.uncovered_shell_mutations = 5;
        assert!(should_fire(&cp5, &context, &instruction, &routing).is_none(), "5 个 shell 变更不应触发");

        // 达到阈值 6：无 todo 推进也必须触发（防旁路的核心断言）
        let mut cp6 = approved_checkpoint();
        cp6.uncovered_shell_mutations = 6;
        let reason = should_fire(&cp6, &context, &instruction, &routing).unwrap();
        assert!(reason.contains("shell"), "触发原因应标明 shell 旁路: {}", reason);

        // 基线语义：派发时快照后，新增变更才计入未覆盖
        let mut cp_baselined = approved_checkpoint();
        cp_baselined.uncovered_shell_mutations = 10;
        cp_baselined.stage_review_shell_baseline = 8;
        assert!(should_fire(&cp_baselined, &context, &instruction, &routing).is_none(), "基线之上的增量 2 < 6 不触发");
        cp_baselined.uncovered_shell_mutations = 14;
        assert!(should_fire(&cp_baselined, &context, &instruction, &routing).is_some(), "基线上增量 6 触发");

        let _ = cp; let _ = cp5;
    }

    #[test]
    fn should_fire_short_circuits_when_triage_disables_stage_review() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        let instruction = instruction_with_todo_done();
        let routing = default_routing();

        // 判决 stage_review_mode="none"：即便写入+todo 完成也不触发
        let mut cp = approved_checkpoint();
        let mut verdict = crate::agent::triage::TriageVerdict::fallback("test");
        verdict.stage_review_mode = "none".to_string();
        verdict.route = "direct".to_string();
        cp.triage_verdict = Some(verdict);
        assert!(should_fire(&cp, &context, &instruction, &routing).is_none());

        // 判决 milestone：正常触发
        let mut cp2 = approved_checkpoint();
        let mut v2 = crate::agent::triage::TriageVerdict::fallback("test");
        v2.stage_review_mode = "milestone".to_string();
        cp2.triage_verdict = Some(v2);
        assert!(should_fire(&cp2, &context, &instruction, &routing).is_some());
    }

    #[test]
    fn should_fire_respects_budget_inflight_and_blockers() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        let instruction = instruction_with_todo_done();
        let routing = default_routing();

        // 预算耗尽
        let mut cp = approved_checkpoint();
        cp.stage_review_budget_used = crate::agent::checkpoint::MAX_STAGE_REVIEWS;
        assert!(should_fire(&cp, &context, &instruction, &routing).is_none());

        // 在途审查
        let mut cp = approved_checkpoint();
        cp.stage_review_run_id = Some("child-1".to_string());
        assert!(should_fire(&cp, &context, &instruction, &routing).is_none());

        // 有未解决拦截时不重复派发（等对质收敛）
        let mut cp = approved_checkpoint();
        cp.stage_review_blockers_open = 1;
        assert!(should_fire(&cp, &context, &instruction, &routing).is_none());
    }

    #[test]
    fn should_fire_respects_stage_reviewer_override_disabled_and_roles() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        let instruction = instruction_with_todo_done();
        let cp = approved_checkpoint();

        // 默认配置下：达到条件触发
        let default_cfg = default_routing();
        assert!(should_fire(&cp, &context, &instruction, &default_cfg).is_some());

        // 显式免审（"disabled" 或 "none"）：直通免审，完全不触发
        let mut disabled_cfg = default_routing();
        disabled_cfg.stage_reviewer_override = Some("disabled".to_string());
        assert!(should_fire(&cp, &context, &instruction, &disabled_cfg).is_none(), "disabled 必须直通免审");

        let mut none_cfg = default_routing();
        none_cfg.stage_reviewer_override = Some("None".to_string());
        assert!(should_fire(&cp, &context, &instruction, &none_cfg).is_none(), "None 必须直通免审");

        // 指定角色卡：依然正常触发阶段审查
        let mut custom_role_cfg = default_routing();
        custom_role_cfg.stage_reviewer_override = Some("frontend_gatekeeper".to_string());
        assert!(should_fire(&cp, &context, &instruction, &custom_role_cfg).is_some(), "指定角色卡仍应正常触发");
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
        let note = apply_stage_result(&mut cp, &result, &context, None);
        assert_eq!(cp.stage_review_blockers_open, 1, "仅 regardless 计入拦截");
        assert_eq!(cp.stage_review_final_only.len(), 1, "final_only 仅记账");
        assert_eq!(cp.stage_review_baseline_side_effects, 2, "消费不得覆盖派发时基线（防覆盖记账竞态）");
        assert!(cp.stage_review_run_id.is_none());
        assert!(note.contains("拦截"), "有 regardless 拦截时必须给出解套提示");

        // 基线推进后：同样写入不再触发
        assert!(should_fire(&cp, &context, &instruction_with_todo_done(), &default_routing()).is_none());
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
        let _ = apply_stage_result(&mut cp, &result, &context, None);
        // 预算退还 + 基线不动（视为未审，会重新触发）+ 不产生拦截
        assert_eq!(cp.stage_review_budget_used, 0);
        assert_eq!(cp.stage_review_baseline_side_effects, 0);
        assert_eq!(cp.stage_review_blockers_open, 0);
        assert!(cp.stage_review_run_id.is_none());
    }

    #[test]
    fn apply_stage_receipt_directly_handles_blockers_and_infra_failure() {
        use crate::agent::protocol::DeliveryReceipt;
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "a.rs".to_string());
        let mut cp = approved_checkpoint();
        cp.stage_review_run_id = Some("receipt-1".to_string());
        cp.stage_review_budget_used = 1;

        let receipt = DeliveryReceipt {
            child_run_id: "receipt-1".to_string(),
            task_id: "receipt-1".to_string(),
            attempt_id: 1,
            kind: "review".to_string(),
            status: "timed_out".to_string(),
            duration_ms: 60000,
            completion_tokens: 0,
            prompt_tokens: 0,
            error_kind: Some("timed_out".to_string()),
            payload: None,
            raw_text: None,
        };
        let note = apply_stage_receipt(&mut cp, &receipt, &context, None);
        assert_eq!(cp.stage_review_budget_used, 0);
        assert!(note.contains("阶段审查任务未正常完成"));
    }

    #[test]
    fn apply_stage_receipt_committee_accumulates_blockers_and_merges_final_only() {
        use crate::agent::protocol::DeliveryReceipt;
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.record_side_effect(1, "write_file".to_string(), "app.vue".to_string());
        context.record_side_effect(2, "write_file".to_string(), "lib.rs".to_string());
        let mut cp = approved_checkpoint();
        cp.stage_review_run_ids = vec!["rev-fe".to_string(), "rev-be".to_string()];
        cp.stage_review_run_id = Some("rev-fe".to_string());
        cp.stage_review_budget_used = 1;

        // 专家 1（前端）：发现 1 个 blocking，1 个 final_only
        let receipt1 = DeliveryReceipt {
            child_run_id: "rev-fe".to_string(),
            task_id: "stage_review_1_fe".to_string(),
            attempt_id: 1,
            kind: "review".to_string(),
            status: "succeeded".to_string(),
            duration_ms: 1200,
            completion_tokens: 150,
            prompt_tokens: 450,
            error_kind: None,
            payload: Some(serde_json::json!({
                "confidence": "confirmed",
                "findings": [
                    { "id": "FE-1", "severity": "blocking", "finality": "regardless", "why": "前端状态泄露" },
                    { "id": "FE-2", "severity": "blocking", "finality": "final_only", "why": "缺少可访问性 aria-label" }
                ],
                "withdrawn_findings": []
            })),
            raw_text: None,
        };
        let note1 = apply_stage_receipt(&mut cp, &receipt1, &context, None);
        assert_eq!(cp.stage_review_blockers_open, 1, "前端专家产出 1 个 blocking");
        assert_eq!(cp.stage_review_final_only.len(), 1);
        assert_eq!(cp.stage_review_run_ids, vec!["rev-be".to_string()]);
        assert_eq!(cp.stage_review_run_id, Some("rev-be".to_string()));
        assert!(note1.contains("联合会审仍在途，剩余 1 位专家"));

        // 专家 2（后端）：发现 2 个 blocking，1 个 final_only
        let receipt2 = DeliveryReceipt {
            child_run_id: "rev-be".to_string(),
            task_id: "stage_review_1_be".to_string(),
            attempt_id: 1,
            kind: "review".to_string(),
            status: "succeeded".to_string(),
            duration_ms: 1500,
            completion_tokens: 200,
            prompt_tokens: 500,
            error_kind: None,
            payload: Some(serde_json::json!({
                "confidence": "confirmed",
                "findings": [
                    { "id": "BE-1", "severity": "blocking", "finality": "regardless", "why": "SQL 注入隐患" },
                    { "id": "BE-2", "severity": "blocking", "finality": "regardless", "why": "并发死锁隐患" },
                    { "id": "BE-3", "severity": "blocking", "finality": "final_only", "why": "缺少索引日志" }
                ],
                "withdrawn_findings": []
            })),
            raw_text: None,
        };
        let note2 = apply_stage_receipt(&mut cp, &receipt2, &context, None);
        assert_eq!(cp.stage_review_blockers_open, 3, "联合会审累计 1 + 2 = 3 个 blocking");
        assert_eq!(cp.stage_review_final_only.len(), 2, "final_only 汇总 2 条");
        assert!(cp.stage_review_run_ids.is_empty(), "所有专家均已交付");
        assert!(cp.stage_review_run_id.is_none());
        assert!(!note2.contains("仍在途"));

        // 对质撤回测试：针对 BE-1 进行对质并撤回，blocking 减 1
        cp.stage_review_rebuttal_run_id = Some("reb-be".to_string());
        let rebuttal_receipt = DeliveryReceipt {
            child_run_id: "reb-be".to_string(),
            task_id: "stage_rebuttal_1".to_string(),
            attempt_id: 1,
            kind: "review".to_string(),
            status: "succeeded".to_string(),
            duration_ms: 800,
            completion_tokens: 80,
            prompt_tokens: 300,
            error_kind: None,
            payload: Some(serde_json::json!({
                "confidence": "confirmed",
                "findings": [
                    { "id": "BE-2", "severity": "blocking", "finality": "regardless", "why": "并发死锁隐患" }
                ],
                "withdrawn_findings": ["BE-1"],
                "objection_verdicts": [
                    { "finding_id": "BE-1", "verdict": "withdraw", "reason": "父 Agent 提供的参数化查询证据充分" }
                ]
            })),
            raw_text: None,
        };
        let reb_note = apply_stage_receipt(&mut cp, &rebuttal_receipt, &context, None);
        assert_eq!(cp.stage_review_blockers_open, 2, "撤回 1 条后 blocking 降为 2");
        assert!(reb_note.contains("对质复检结论"));
    }

    #[test]
    fn test_dispatch_preserves_stage_review_final_only() {
        let mut cp = approved_checkpoint();
        cp.stage_review_final_only = vec!["【对质未撤回自动降级】F1: 历史欠账".to_string()];
        cp.stage_review_blockers_open = 0;

        // dispatch 时：只清 blockers_open，不清空 final_only
        cp.stage_review_blockers_open = 0;
        // 验证 final_only 跨阶段累加 carry-over 到收尾审查未被抹除
        assert_eq!(cp.stage_review_final_only.len(), 1);
        assert!(cp.stage_review_final_only[0].contains("历史欠账"));
    }
}
