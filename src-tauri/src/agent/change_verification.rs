use serde::{Deserialize, Serialize};

const MAX_EVIDENCE_REFS: usize = 8;
const MAX_EVIDENCE_REF_CHARS: usize = 240;
const MAX_WAIVER_REASON_CHARS: usize = 600;

/// 对本会话最近一批真实写入的验证闭合。
///
/// `verified` 需要可审计的证据引用；`waived` 需要明确解释为何不验证。
/// 每次成功副作用都会由 `SandwichContext::record_side_effect` 清空该状态，
/// 因此旧验证不能覆盖后续写入。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeVerification {
    pub status: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub waived_reason: Option<String>,
}

impl ChangeVerification {
    pub fn is_resolved(&self) -> bool {
        matches!(
            normalized_status(self).as_deref(),
            Some("verified" | "waived")
        )
    }

    pub fn summary(&self) -> String {
        match normalized_status(self).as_deref() {
            Some("verified") => format!("已验证：{}", self.evidence_refs.join("；")),
            Some("waived") => format!(
                "已豁免验证：{}",
                self.waived_reason.as_deref().unwrap_or_default()
            ),
            _ => "验证状态无效".to_string(),
        }
    }
}

pub fn apply_change_verification_update(
    current: &mut Option<ChangeVerification>,
    update: ChangeVerification,
) -> Result<(), String> {
    validate_change_verification(&update)?;

    if let Some(existing) = current {
        if existing != &update {
            return Err(
                "当前写入批次已有已闭合验证，不能静默改写。若又发生真实写入，宿主会自动清空旧验证并要求重新验证或豁免。".to_string(),
            );
        }
        return Ok(());
    }

    *current = Some(update);
    Ok(())
}

pub fn change_verification_blocker(
    side_effect_count: usize,
    verification: Option<&ChangeVerification>,
) -> Option<String> {
    if side_effect_count == 0 || verification.is_some_and(ChangeVerification::is_resolved) {
        return None;
    }

    Some(format!(
        "本会话已记录 {} 次真实写入，但最近写入后尚无验证证据或明确豁免",
        side_effect_count
    ))
}

fn validate_change_verification(verification: &ChangeVerification) -> Result<(), String> {
    let status = normalized_status(verification)
        .ok_or_else(|| "change_verification.status 只能是 verified 或 waived。".to_string())?;

    let refs_are_valid = !verification.evidence_refs.is_empty()
        && verification.evidence_refs.len() <= MAX_EVIDENCE_REFS
        && verification.evidence_refs.iter().all(|reference| {
            !reference.trim().is_empty() && reference.chars().count() <= MAX_EVIDENCE_REF_CHARS
        });
    let waiver = verification.waived_reason.as_deref().unwrap_or("").trim();

    match status.as_str() {
        "verified" if refs_are_valid && waiver.is_empty() => Ok(()),
        "verified" => Err(format!(
            "verified 必须包含 1–{} 条非空 evidence_refs，且不能带 waived_reason。",
            MAX_EVIDENCE_REFS
        )),
        "waived"
            if verification.evidence_refs.is_empty()
                && !waiver.is_empty()
                && waiver.chars().count() <= MAX_WAIVER_REASON_CHARS =>
        {
            Ok(())
        }
        "waived" => Err(format!(
            "waived 不能带 evidence_refs，且必须包含 1–{} 个字符的 waived_reason。",
            MAX_WAIVER_REASON_CHARS
        )),
        _ => unreachable!("normalized_status already restricts variants"),
    }
}

fn normalized_status(verification: &ChangeVerification) -> Option<String> {
    let status = verification.status.trim().to_ascii_lowercase();
    matches!(status.as_str(), "verified" | "waived").then_some(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verified() -> ChangeVerification {
        ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec!["output_id: test-42".to_string()],
            waived_reason: None,
        }
    }

    #[test]
    fn only_verified_or_waived_can_close_real_writes() {
        assert!(apply_change_verification_update(&mut None, verified()).is_ok());

        let waived = ChangeVerification {
            status: "waived".to_string(),
            evidence_refs: Vec::new(),
            waived_reason: Some("用户明确要求只修改，不运行测试。".to_string()),
        };
        assert!(apply_change_verification_update(&mut None, waived).is_ok());

        let invalid = ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: Vec::new(),
            waived_reason: None,
        };
        assert!(apply_change_verification_update(&mut None, invalid).is_err());
    }

    #[test]
    fn unresolved_writes_block_finish_but_readonly_work_does_not() {
        assert!(change_verification_blocker(0, None).is_none());
        assert!(change_verification_blocker(1, None).is_some());
        assert!(change_verification_blocker(1, Some(&verified())).is_none());
    }

    #[test]
    fn closed_verification_is_idempotent_but_cannot_be_rewritten() {
        let mut current = Some(verified());
        assert!(apply_change_verification_update(&mut current, verified()).is_ok());

        let rewrite = ChangeVerification {
            status: "waived".to_string(),
            evidence_refs: Vec::new(),
            waived_reason: Some("改成豁免".to_string()),
        };
        assert!(apply_change_verification_update(&mut current, rewrite).is_err());
    }
}
