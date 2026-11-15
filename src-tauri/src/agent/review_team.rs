//! 审查人数与专家选择共用现有规则，不推断任务质量或新增审查触发条件。
use crate::agent::{context::SandwichContext, tools::role_manager::RoleCard};
use crate::db::DbState;
pub(crate) const MAX_REVIEWERS: usize = 3;
#[cfg(test)]
pub(crate) mod tests;

pub(crate) fn final_review_policy_fingerprint(
    db: &DbState,
    routing: &crate::agent::router::ModelRoutingConfig,
    recommended: &[String],
) -> String {
    use sha2::{Digest, Sha256};
    let card = final_review_primary(db, recommended, &routing.disabled_role_cards);
    let cards = routing.stage_review_committee.then(|| {
        crate::agent::tools::role_manager::list_cards(db, false).ok().map(|cards| {
            cards.into_iter().filter(|candidate| {
                available_reviewer(candidate, &routing.disabled_role_cards)
                    && review_inputs_available(candidate)
            }).collect::<Vec<_>>()
        })
    }).flatten();
    format!(
        "{:x}",
        Sha256::digest(
            serde_json::json!({"committee":routing.stage_review_committee,"primary":card,"cards":cards,
                "reviewer":routing.role_models.get("reviewer"),
                "fallback_models":[routing.pro,routing.flash,routing.vision],
                "backup":routing.reviewer_backup_model,
                "cross_family":routing.auto_cross_family_reviewer})
            .to_string()
            .as_bytes()
        )
    )
}

fn available_reviewer(card: &RoleCard, disabled: &[String]) -> bool {
    card.enabled && card.allows_kind("reviewer")
        && !crate::agent::tools::role_manager::is_card_disabled_for(disabled, &card.name, "reviewer")
}

pub(crate) fn review_inputs_available(card: &RoleCard) -> bool {
    crate::agent::tools::role_manager::validate_dispatch_inputs(card, None).is_ok()
}

pub(crate) fn final_review_primary(db: &DbState, names: &[String], disabled: &[String]) -> Option<RoleCard> {
    names.iter().map(String::as_str).chain(std::iter::once("reviewer"))
        .find_map(|name| find_reviewer(db, name, disabled).filter(review_inputs_available))
}

pub(crate) fn select_review_cards(
    db: &DbState,
    context: &SandwichContext,
    extra_paths: &[String],
    disabled_cards: &[String],
    is_committee: bool,
    eligible: impl Fn(&RoleCard) -> bool,
) -> Vec<crate::agent::tools::role_manager::RoleCard> {
    let Ok(cards) = crate::agent::tools::role_manager::list_cards(db, false) else {
        return Vec::new();
    };
    let active_reviewers: Vec<crate::agent::tools::role_manager::RoleCard> = cards
        .into_iter()
        .filter(|card| available_reviewer(card, disabled_cards) && eligible(card))
        .collect();

    if active_reviewers.is_empty() {
        return Vec::new();
    }

    let mut all_paths: Vec<String> = context
        .side_effects
        .iter()
        .flat_map(|s| {
            let mut paths = s.affected_paths.clone();
            paths.push(s.description.clone());
            paths
        })
        .collect();
    all_paths.extend_from_slice(extra_paths);

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

/// Triage names are ordered recommendations, never a required roster or headcount.
/// Session exclusions and reviewer permissions remain authoritative.
pub(crate) fn recommended_primary(
    db: &DbState,
    names: &[String],
    disabled: &[String],
) -> Option<RoleCard> {
    recommended_primary_where(db, names, disabled, |_| true)
}

pub(crate) fn recommended_primary_where(
    db: &DbState,
    names: &[String],
    disabled: &[String],
    eligible: impl Fn(&RoleCard) -> bool,
) -> Option<RoleCard> {
    names.iter().find_map(|name| find_reviewer(db, name, disabled).filter(&eligible))
}

fn find_reviewer(db: &DbState, name: &str, disabled: &[String]) -> Option<RoleCard> {
    let name = name.trim();
    if name.is_empty()
        || crate::agent::tools::role_manager::is_card_disabled_for(disabled, name, "reviewer")
    {
        return None;
    }
    crate::agent::tools::role_manager::resolve_enabled_card(db, name)
        .ok().flatten().filter(|card| available_reviewer(card, disabled))
}

/// Shared parent workflow facts, separate from the review executor's own stages.
/// Configuration values are not labelled as human decisions when their origin is unknown.
pub(crate) fn parent_workflow_context(cp: &crate::agent::checkpoint::AgentCheckpoint) -> String {
    format!("【被审查父任务的流程配置快照】{}\n这些值属于父任务；当前只读 Reviewer 的内部环节关闭用于避免递归审查，不改变父任务配置。固定配置的显式值优先，未设值沿用任务策略；候选角色不是必须全部派发的名单，人数另由会审配置决定。", serde_json::json!({
        "fixed_settings_source": "parent_checkpoint.stages",
        "fixed_settings": cp.stages,
        "plan_mode": cp.plan_mode,
        "triage": cp.triage_verdict.as_ref().map(|v| serde_json::json!({
            "source": "parent_checkpoint.triage_verdict",
            "final_review_mode": v.final_review_mode, "stage_review_mode": v.stage_review_mode,
            "pre_review_role_candidates": v.pre_review_roles,
            "stage_review_role_candidates": v.stage_review_roles,
            "final_review_role_candidates": v.final_review_roles,
        })),
        "submitted_review_request": cp.submitted_plan.as_ref().map(|p| serde_json::json!({
            "final_review_mode": p.final_review_mode, "stage_review_mode": p.stage_review_mode,
        })),
    }))
}

/// 保留本环节既有主审角色，其余名额从同一专家池中选择。
pub(crate) fn review_team_where(
    db: &DbState,
    context: &SandwichContext,
    paths: &[String],
    disabled: &[String],
    committee: bool,
    primary: Option<RoleCard>,
    eligible: impl Fn(&RoleCard) -> bool,
) -> Vec<RoleCard> {
    let primary = primary.filter(&eligible);
    if !committee {
        return primary.into_iter().collect();
    }
    let candidates = select_review_cards(db, context, paths, disabled, true, eligible);
    let mut cards: Vec<_> = primary.into_iter().collect();
    for card in candidates {
        if cards.len() >= MAX_REVIEWERS {
            break;
        }
        if !cards.iter().any(|c| c.name == card.name) {
            cards.push(card);
        }
    }
    cards
}
