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
    db: &DbState,
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
    if checkpoint.stage_review_in_flight(db) {
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

    // 保留写方线索，不生成自动消费点清单或裁决 refs。
    let write_context = if crate::agent::impact_pack::write_context_enabled() {
        crate::agent::reviewer_gatekeeper::GitBaseline::capture(workspace)
            .collect_git_diff(workspace)
            .map(|(d, _)| crate::agent::impact_pack::build_write_context_from_diff(workspace, &d))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let mut extras = stage_extra_sections(db, session_id);
    if !write_context.is_empty() {
        extras.push(write_context.clone());
    }

    if role_cards.is_empty() {
        // 无挂载专家卡：走系统默认的跨家族异构对抗模型兜底
        let (model_to_use, cross_family_note) = {
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            let has_explicit_reviewer_slot = model_routing
                .role_models
                .get(router::roles::REVIEWER)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);

            if has_explicit_reviewer_slot || !model_routing.auto_cross_family_reviewer {
                (routing.model_id.clone(), None)
            } else {
                router::prefer_cross_family_reviewer(&conn, &routing.model_id)
            }
        };

        let evidence = build_stage_evidence(context, workspace, None, &extras);
        let question = "阶段级写入审查：审查当前阶段变更是否满足阶段目标且无 blocking 缺陷".to_string();
        let params = json!({
            "role": "stage_reviewer",
            "role_label": "🛡️ 阶段综合审查官",
            "question": question,
            "evidence_excerpt": evidence,
        });

        let stage_batch_id = format!("stage-batch-{}-{}", checkpoint.step_id, uuid::Uuid::new_v4());
        let envelope = crate::agent::protocol::DispatchEnvelope::reviewer(
            parent_run_id,
            session_id,
            &model_to_use,
            params,
        )
        .with_task_id(format!("stage_review_{}", checkpoint.step_id))
        .with_purpose("stage_review")
        .with_batch_id(stage_batch_id.clone())
        .with_tier(&routing.tier.to_string())
        .with_effort(model_routing.reasoning_effort_for(&routing.tier));

        let child_run_id = envelope
            .dispatch(Some(app.clone()), db, workspace, interrupt_rx)
            .await?;

        checkpoint.reviewer_dispatches.insert(child_run_id.clone(),
            crate::agent::child_reviewer::PlanReviewerDispatchSnapshot::from_envelope(&envelope));

        // 批次锚点落 checkpoint：在途/收口此后全部由台账推导
        checkpoint.stage_review_batch_id = Some(stage_batch_id);
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
    // 同批专家共享 batch_id：台账可按批联查这批审查的全部成本与交付
    let stage_batch_id = format!("stage-batch-{}-{}", checkpoint.step_id, uuid::Uuid::new_v4());
    let mut dispatched_run_ids = Vec::new();
    for card in &role_cards {
        let model_to_use = {
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            let has_explicit_reviewer_slot = model_routing
                .role_models
                .get(router::roles::REVIEWER)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);

            // 优先级：角色卡自带 model_id > 会话级 reviewer 算力槽 > 跨家族优选（与派发审查/探针一致）
            if !card.model_id.is_empty() {
                card.model_id.clone()
            } else if has_explicit_reviewer_slot {
                routing.model_id.clone()
            } else if model_routing.auto_cross_family_reviewer {
                router::prefer_cross_family_reviewer(&conn, &routing.model_id).0
            } else {
                routing.model_id.clone()
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

        let evidence = build_stage_evidence(context, workspace, focus_context.as_deref(), &extras);
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
        .with_purpose("stage_review")
        .with_batch_id(stage_batch_id.clone())
        .with_tier(&routing.tier.to_string())
        .with_effort(model_routing.reasoning_effort_for(&routing.tier));

        let persona = crate::agent::tools::role_manager::persona_block(card)
            + &crate::agent::tools::role_manager::skills_block(db, card);
        if !persona.is_empty() {
            envelope = envelope.with_persona(persona);
        }
        if let Some(schema) = card.parse_custom_schema() {
            envelope = envelope.with_custom_schema(schema);
        }

        let child_run_id = envelope
            .dispatch(Some(app.clone()), db, workspace, interrupt_rx.clone())
            .await?;

        checkpoint.reviewer_dispatches.insert(child_run_id.clone(),
            crate::agent::child_reviewer::PlanReviewerDispatchSnapshot::from_envelope(&envelope));

        // 成员身份由台账 batch_id 承担（dispatch 时已盖章）；锚点写一次即可
        dispatched_run_ids.push(child_run_id.clone());
        checkpoint.stage_review_batch_id = Some(stage_batch_id.clone());

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

/// 阶段审查补充材料：会话 tmp 产物清单 + 执行轨迹摘要（与终审同源复用）。
/// 阶段审查不止看 diff——中途产物存在性、"声称跑过的验证"是否真跑过，
/// 在阶段层就该进审查视野；拿不到则略过（非致命材料）。
/// 产物清单起点取本会话创建后的第一次 LLM 调用时间：fork/新会话的 tmp 会带着源会话遗留的产物
/// （实测混入 deep_space_beacon 等旧目录，导致审查员审错对象），这些文件的修改时间
/// 都早于首次调用，只列其后产生/修改的文件；查不到则退回全量。
pub(crate) fn stage_extra_sections(db: &DbState, session_id: &str) -> Vec<String> {
    let mut extras = Vec::new();
    // created_at 是 RFC3339 文本；只取"会话创建之后"的首次调用——fork 会把源会话的
    // 历史调用连同 tmp 一起复制过来，那些旧调用时间早于新会话创建时间，不能当起点。
    let first_call: Option<String> = db.0.lock().ok().and_then(|conn| {
        conn.query_row(
            "SELECT MIN(c.created_at) FROM agent_llm_calls c WHERE c.session_id = ?1 \
             AND c.created_at > COALESCE((SELECT created_at FROM chat_sessions WHERE id = ?1), '')",
            rusqlite::params![session_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
    });
    let epoch = first_call
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
        .map(|d| d.with_timezone(&chrono::Utc))
        .unwrap_or_else(|| chrono::DateTime::<chrono::Utc>::from(std::time::UNIX_EPOCH));
    if let Some(m) =
        crate::agent::reviewer_gatekeeper::collect_session_tmp_manifest(session_id, epoch)
    {
        extras.push(m);
    }
    if let Some(t) = crate::agent::reviewer_gatekeeper::collect_step_trace_digest(db, session_id) {
        extras.push(t);
    }
    extras
}

/// 组装阶段审查证据包：任务目标 + 阶段计划 + 专项材料(如有) + diff 尾部。
/// 硬约束：总长不得超过 child_reviewer 的 evidence_excerpt 预算（6000 字符），
/// 超限会导致派发在解析层被静默拒绝（实测教训）。diff 取尾部——最近的变更与当前阶段最相关。
/// extra_sections：tmp 产物清单/执行轨迹摘要等补充材料（阶段审查不止看 diff——
/// 中途产物与"声称跑过的验证"也该进证据视野），计入同一 6000 字符预算，
/// 挤占的是 diff 配额（全文 diff 审查者可经 git_diff 工具自取）。
fn build_stage_evidence(
    context: &SandwichContext,
    workspace: &Path,
    focus_context: Option<&str>,
    extra_sections: &[String],
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

    // 中途产物清单与执行轨迹摘要：阶段审查不止于 diff——产物存在性、
    // "声称测过但没跑"类谎报在阶段层就该被看见
    for extra in extra_sections {
        let trimmed = extra.trim();
        if !trimmed.is_empty() {
            evidence.push_str(trimmed);
            evidence.push_str("\n\n");
        }
    }

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
            "【当前累计变更 diff（按路径排序，含自上次阶段审查以来的全部写入；全文可经 git_diff 工具复核）】\n{}",
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

/// 阶段审查回执处理结果：模型可见摘要 + 待结算退款标记。
/// refund_pending 的语义见 settle_stage_review_refund：退款必须在批循环
/// 结束后统一结算，循环内逐条结算会因后续专家的 run_ids 尚未移除而误判在途。
pub struct StageReceiptOutcome {
    pub note: String,
    pub refund_pending: bool,
}

/// 消费阶段审查交付回执（成功或基础设施失败均走这里）。
/// oracle_index：宿主判分「下一未解阶段」索引（None = 无 oracle 场景）。
/// blockers>0 时记录出生索引，供僵尸 blocker 清账判定。
/// 单一事实源：主循环自动注入与 wait_jobs 手动收取都经 reducer 汇聚到本函数。
pub fn apply_stage_receipt(
    checkpoint: &mut AgentCheckpoint,
    db: &DbState,
    receipt: &crate::agent::protocol::DeliveryReceipt,
    oracle_index: Option<u32>,
) -> StageReceiptOutcome {
    // 对质判定由台账 purpose='rebuttal' 盖章承担（resume 落库时盖戳），
    // 不再需要 checkpoint 维护"当前对质指针"
    let was_rebuttal = checkpoint.is_rebuttal_run(db, &receipt.child_run_id);
    let parsed_kind = receipt
        .error_kind
        .as_deref()
        .map(crate::agent::types::ChildErrorKind::parse);

    // 对质子任务技术性失败特判（须先于通用 infra 分支与完成登记）：
    // 审查员输出非法 = 未产生有效裁决。原 blocking 异议必须保留——不能因
    // 空 findings 误清账（那会把"审查员失联"当成"异议撤回"）；不计对质拒
    // 撤回次数、不退还阶段次数；允许重新 resume_child 对质，或等宿主判分
    // 越过该阶段自动清账。
    if was_rebuttal
        && parsed_kind == Some(crate::agent::types::ChildErrorKind::InvalidOutput)
    {
        return StageReceiptOutcome {
            note: "⚖️ 对质子任务技术性失败（审查员输出非法，未产生有效裁决）：原 blocking 异议保留，可重新 resume_child 对质，或等宿主判分越过该阶段自动清账。".to_string(),
            refund_pending: false,
        };
    }

    // 基础设施失败：该阶段视为「未审」，会重新触发。
    // 退款不在此内联结算——标记 refund_pending 由调用方批后统一 settle。
    if receipt.is_infra_failure() {
        return StageReceiptOutcome {
            note: "⚠️ 阶段审查任务未正常完成。该阶段视为未审，累计写入达到阈值后将自动重审。".to_string(),
            refund_pending: true,
        };
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
        if checkpoint.stage_review_blockers_open == 0 {
            checkpoint.stage_review_blocker_source_run_id = None;
        }

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
        if new_blockers > 0 {
            checkpoint.stage_review_blocker_source_run_id = Some(receipt.child_run_id.clone());
        }
        // 新一轮非对质审查交付：上一轮对质遗留的拒撤回计数已失效，清零
        checkpoint.stage_review_confrontation_failures = 0;
        checkpoint.stage_review_confrontation_target = None;
    }

    // 僵尸 blocker 治理：记录出生阶段索引（无 blocker 时清空，避免陈旧索引干扰）
    if checkpoint.stage_review_blockers_open > 0 {
        checkpoint.stage_review_blocker_stage = oracle_index;
    } else if !checkpoint.stage_review_in_flight(db) {
        checkpoint.stage_review_blocker_stage = None;
    }

    let receipt_final_only = receipt.stage_final_only_lines();
    checkpoint.stage_review_final_only.extend(receipt_final_only);
    checkpoint.stage_review_final_only.sort();
    checkpoint.stage_review_final_only.dedup();

    let (added, withdrawn, maintained) = receipt.objection_stats();
    checkpoint.review_total_withdrawn += withdrawn;
    checkpoint.review_total_maintained += maintained;

    let in_flight_remaining = checkpoint.stage_review_in_flight_remaining(db);
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
    StageReceiptOutcome {
        note: text,
        refund_pending: false,
    }
}

/// 兼容老接口：将 ChildReviewResult 转换为 DeliveryReceipt 交付回执后处理
pub fn apply_stage_result(
    checkpoint: &mut AgentCheckpoint,
    db: &DbState,
    result: &crate::agent::child_reviewer::ChildReviewResult,
    oracle_index: Option<u32>,
) -> StageReceiptOutcome {
    let receipt: crate::agent::protocol::DeliveryReceipt = result.clone().into();
    apply_stage_receipt(checkpoint, db, &receipt, oracle_index)
}

#[cfg(test)]
mod tests;
