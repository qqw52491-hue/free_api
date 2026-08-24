use serde::{Deserialize, Serialize};

const MAX_REVIEW_OBLIGATIONS: usize = 6;
const MAX_SCOPE_CHARS: usize = 500;
const MAX_OBLIGATION_ID_CHARS: usize = 64;
const MAX_OBLIGATION_TEXT_CHARS: usize = 360;
const MAX_EVIDENCE_REFS: usize = 6;
const MAX_EVIDENCE_REF_CHARS: usize = 240;
const MAX_WAIVER_REASON_CHARS: usize = 360;

/// 复杂审查、诊断或研究任务的可审计完成契约。
/// 普通任务保持 `None`，不会引入额外终态门槛。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewContract {
    pub scope_statement: String,
    pub obligations: Vec<ReviewObligation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewObligation {
    pub id: String,
    pub question: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub waived_reason: Option<String>,
}

impl ReviewContract {
    /// 将模型提供的完整契约更新应用到已持久化状态。
    /// 更新不得移除旧义务，也不得把已闭合义务退回 open。
    pub fn apply_update(&mut self, update: ReviewContract) -> Result<(), String> {
        validate_contract(&update)?;

        if self.obligations.is_empty() {
            *self = update;
            return Ok(());
        }

        for existing in &self.obligations {
            let Some(candidate) = update
                .obligations
                .iter()
                .find(|item| item.id == existing.id)
            else {
                return Err(format!(
                    "审查契约更新遗漏既有义务 {}；不得静默删除已承诺的核验项。若不再适用，请保留该项并标为 waived，附上理由。",
                    existing.id
                ));
            };
            if candidate.question.trim() != existing.question.trim() {
                return Err(format!(
                    "审查契约义务 {} 的问题文本不能被静默改写；请新增新义务，或保留原问题并以 waived 说明范围变化。",
                    existing.id
                ));
            }
            if is_closed(existing) && !same_closure(existing, candidate) {
                return Err(format!(
                    "审查契约义务 {} 已闭合，后续更新不得回退或改写其证据/豁免理由。",
                    existing.id
                ));
            }
        }

        self.scope_statement = update.scope_statement;
        self.obligations = update.obligations;
        Ok(())
    }

    pub fn open_descriptions(&self) -> Vec<String> {
        self.obligations
            .iter()
            .filter(|item| normalized_status(item).as_deref() == Some("open"))
            .map(|item| format!("审查义务 {}：{}", item.id, item.question.trim()))
            .collect()
    }

    pub fn is_complete(&self) -> bool {
        self.open_descriptions().is_empty()
    }
}

/// 原子应用模型发出的完整契约更新。校验失败时保留旧状态。
pub fn apply_review_contract_update(
    target: &mut Option<ReviewContract>,
    update: ReviewContract,
) -> Result<(), String> {
    let mut candidate = target.clone().unwrap_or(ReviewContract {
        scope_statement: String::new(),
        obligations: Vec::new(),
    });
    candidate.apply_update(update)?;
    *target = Some(candidate);
    Ok(())
}

fn validate_contract(contract: &ReviewContract) -> Result<(), String> {
    if contract.scope_statement.trim().is_empty() {
        return Err(
            "审查契约缺少 scope_statement；请明确本次审查/诊断覆盖的对象和边界。".to_string(),
        );
    }
    if contract.scope_statement.chars().count() > MAX_SCOPE_CHARS {
        return Err(format!(
            "审查契约 scope_statement 超过 {} 字符。",
            MAX_SCOPE_CHARS
        ));
    }
    if contract.obligations.is_empty() || contract.obligations.len() > MAX_REVIEW_OBLIGATIONS {
        return Err(format!(
            "审查契约 obligations 必须为 1–{} 项，避免空契约或过度拆分。",
            MAX_REVIEW_OBLIGATIONS
        ));
    }

    let mut ids = std::collections::BTreeSet::new();
    for item in &contract.obligations {
        if item.id.trim().is_empty() || item.id.chars().count() > MAX_OBLIGATION_ID_CHARS {
            return Err(format!(
                "审查义务 id 必须为 1–{} 个字符。",
                MAX_OBLIGATION_ID_CHARS
            ));
        }
        if !ids.insert(item.id.clone()) {
            return Err(format!("审查契约包含重复义务 id {}。", item.id));
        }
        if item.question.trim().is_empty()
            || item.question.chars().count() > MAX_OBLIGATION_TEXT_CHARS
        {
            return Err(format!(
                "审查义务 {} 的 question 必须为 1–{} 个字符。",
                item.id, MAX_OBLIGATION_TEXT_CHARS
            ));
        }

        match normalized_status(item).as_deref() {
            Some("open") => {
                if !item.evidence_refs.is_empty() || item.waived_reason.is_some() {
                    return Err(format!(
                        "审查义务 {} 标为 open 时不得同时声明证据或豁免理由。",
                        item.id
                    ));
                }
            }
            Some("evidenced") => {
                if item.evidence_refs.is_empty() {
                    return Err(format!(
                        "审查义务 {} 标为 evidenced 时至少需要一个 evidence_refs。",
                        item.id
                    ));
                }
                if item.waived_reason.is_some() {
                    return Err(format!(
                        "审查义务 {} 不能同时为 evidenced 和 waived。",
                        item.id
                    ));
                }
            }
            Some("waived") => {
                if item
                    .waived_reason
                    .as_deref()
                    .map(str::trim)
                    .filter(|reason| !reason.is_empty())
                    .is_none()
                {
                    return Err(format!(
                        "审查义务 {} 标为 waived 时必须说明 waived_reason。",
                        item.id
                    ));
                }
                if !item.evidence_refs.is_empty() {
                    return Err(format!(
                        "审查义务 {} 标为 waived 时不得同时声明 evidence_refs。",
                        item.id
                    ));
                }
            }
            _ => {
                return Err(format!(
                    "审查义务 {} 的 status 仅可为 open、evidenced 或 waived。",
                    item.id
                ));
            }
        }

        if item.evidence_refs.len() > MAX_EVIDENCE_REFS
            || item.evidence_refs.iter().any(|reference| {
                reference.trim().is_empty() || reference.chars().count() > MAX_EVIDENCE_REF_CHARS
            })
        {
            return Err(format!(
                "审查义务 {} 的 evidence_refs 最多 {} 项，且每项必须为 1–{} 个字符。",
                item.id, MAX_EVIDENCE_REFS, MAX_EVIDENCE_REF_CHARS
            ));
        }
        if item
            .waived_reason
            .as_deref()
            .is_some_and(|reason| reason.chars().count() > MAX_WAIVER_REASON_CHARS)
        {
            return Err(format!(
                "审查义务 {} 的 waived_reason 超过 {} 字符。",
                item.id, MAX_WAIVER_REASON_CHARS
            ));
        }
    }
    Ok(())
}

fn normalized_status(item: &ReviewObligation) -> Option<String> {
    let status = item.status.trim().to_ascii_lowercase();
    (!status.is_empty()).then_some(status)
}

fn is_closed(item: &ReviewObligation) -> bool {
    matches!(
        normalized_status(item).as_deref(),
        Some("evidenced" | "waived")
    )
}

fn same_closure(existing: &ReviewObligation, candidate: &ReviewObligation) -> bool {
    normalized_status(existing) == normalized_status(candidate)
        && existing.evidence_refs == candidate.evidence_refs
        && existing.waived_reason == candidate.waived_reason
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obligation(id: &str, question: &str, status: &str) -> ReviewObligation {
        ReviewObligation {
            id: id.to_string(),
            question: question.to_string(),
            status: status.to_string(),
            evidence_refs: Vec::new(),
            waived_reason: None,
        }
    }

    fn contract(obligations: Vec<ReviewObligation>) -> ReviewContract {
        ReviewContract {
            scope_statement: "审查上下文引擎的实现与验证边界".to_string(),
            obligations,
        }
    }

    #[test]
    fn evidenced_and_waived_obligations_need_auditable_closure() {
        let mut evidenced = obligation("implementation", "调用链是否真实接通？", "evidenced");
        assert!(contract(vec![evidenced.clone()])
            .apply_update(contract(vec![evidenced.clone()]))
            .is_err());
        evidenced.evidence_refs = vec!["main_loop.rs:695-905".to_string()];
        assert!(contract(vec![evidenced])
            .apply_update(contract(vec![obligation(
                "implementation",
                "调用链是否真实接通？",
                "open"
            )]))
            .is_err());

        let waived = obligation("tests", "是否存在可运行验证？", "waived");
        assert!(contract(vec![waived])
            .apply_update(contract(vec![obligation(
                "tests",
                "是否存在可运行验证？",
                "open"
            )]))
            .is_err());
    }

    #[test]
    fn contract_update_cannot_drop_or_reopen_existing_obligations() {
        let mut current = contract(vec![obligation("scope", "范围是否明确？", "open")]);
        assert!(current.apply_update(contract(vec![])).is_err());

        let mut evidenced = obligation("scope", "范围是否明确？", "evidenced");
        evidenced.evidence_refs = vec!["git diff --stat".to_string()];
        current
            .apply_update(contract(vec![evidenced.clone()]))
            .unwrap();
        assert!(current
            .apply_update(contract(vec![obligation(
                "scope",
                "范围是否明确？",
                "open"
            )]))
            .is_err());
        assert_eq!(current.obligations, vec![evidenced]);
    }

    #[test]
    fn open_descriptions_only_include_explicit_open_obligations() {
        let mut evidenced = obligation("evidence", "证据是否可回溯？", "evidenced");
        evidenced.evidence_refs = vec!["output:42".to_string()];
        let mut waived = obligation("optional", "可选前端范围是否读取？", "waived");
        waived.waived_reason = Some("用户限定后端审查".to_string());
        let review = contract(vec![
            obligation("scope", "审查范围是否明确？", "open"),
            evidenced,
            waived,
        ]);

        assert_eq!(
            review.open_descriptions(),
            vec!["审查义务 scope：审查范围是否明确？"]
        );
        assert!(!review.is_complete());
    }
}
