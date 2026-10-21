//! 审查人数与专家选择共用现有规则，不推断任务质量或新增审查触发条件。
use crate::agent::{context::SandwichContext, tools::role_manager::RoleCard};
use crate::db::DbState;
pub(crate) const MAX_REVIEWERS: usize = 3;
#[cfg(test)]
pub(crate) mod tests;

/// 人数/角色配置变化后不能复用旧单人就绪收据；不影响冻结验证命令的执行事实。
pub(crate) fn policy_fingerprint(
    db: &DbState,
    routing: &crate::agent::router::ModelRoutingConfig,
) -> String {
    if !routing.stage_review_committee {
        return String::new();
    }
    use sha2::{Digest, Sha256};
    let cards = crate::agent::tools::role_manager::list_cards(db, false)
        .ok()
        .map(|cards| {
            cards
                .into_iter()
                .filter(|card| {
                    card.allows_kind("reviewer")
                        && !crate::agent::tools::role_manager::is_card_disabled_for(
                            &routing.disabled_role_cards,
                            &card.name,
                            "reviewer",
                        )
                })
                .collect::<Vec<_>>()
        });
    format!(
        "{:x}",
        Sha256::digest(
            serde_json::json!({"committee":true,"cards":cards,
                "reviewer":routing.role_models.get("reviewer"),
                "fallback_models":[routing.pro,routing.flash,routing.vision],
                "backup":routing.reviewer_backup_model,
                "cross_family":routing.auto_cross_family_reviewer})
            .to_string()
            .as_bytes()
        )
    )
}

pub(crate) fn select_review_cards(
    db: &DbState,
    context: &SandwichContext,
    extra_paths: &[String],
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
                && !crate::agent::tools::role_manager::is_card_disabled_for(
                    disabled_cards,
                    &c.name,
                    "reviewer",
                )
        })
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

/// 保留本环节既有主审角色，其余名额从同一专家池中选择。
pub(crate) fn review_team(
    db: &DbState,
    context: &SandwichContext,
    paths: &[String],
    disabled: &[String],
    committee: bool,
    primary: Option<RoleCard>,
) -> Vec<RoleCard> {
    if !committee {
        return primary.into_iter().collect();
    }
    let candidates = select_review_cards(db, context, paths, disabled, true);
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
