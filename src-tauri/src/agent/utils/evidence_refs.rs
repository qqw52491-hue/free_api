//! 证据引用的公共容量规则；不判断证据是否充分或验证是否通过。
use std::collections::BTreeSet;

/// Unicode 字符，包含去重后条目之间的分隔符。
pub(crate) const MAX_EVIDENCE_REFERENCE_CHARS: usize = 16_384;

pub(crate) fn normalize_evidence_refs(references: &[String]) -> Result<Vec<String>, String> {
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::new();
    let mut total_chars = 0usize;
    for (index, reference) in references.iter().enumerate() {
        let reference = reference.trim();
        if reference.is_empty() {
            return Err(format!(
                "evidence_refs 第 {} 条为空；请提供非空引用或省略该条。",
                index + 1
            ));
        }
        if seen.insert(reference) {
            total_chars = total_chars
                .saturating_add(reference.chars().count())
                .saturating_add(usize::from(!normalized.is_empty()));
            if total_chars > MAX_EVIDENCE_REFERENCE_CHARS {
                return Err(format!(
                    "evidence_refs 去重后的引用总量超过 {} 个字符（含条目间分隔）；请缩小引用总量，长正文可保留在原始输出中并引用句柄。此为输入容量限制，不是审查判决。",
                    MAX_EVIDENCE_REFERENCE_CHARS
                ));
            }
            normalized.push(reference.to_owned());
        }
    }
    Ok(normalized)
}
