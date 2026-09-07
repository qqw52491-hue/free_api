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

/// 判断给定的 shell 命令是否包含明确的自动化测试、类型检查或构建验证特征
pub fn is_test_or_verification_command(cmd: &str) -> bool {
    let lower = cmd.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    let test_patterns = [
        "cargo test",
        "cargo check",
        "npm test",
        "npm run test",
        "npx vitest",
        "npx jest",
        "pnpm test",
        "bun test",
        "deno test",
        "npx tsc",
        "npx vue-tsc",
        "npm run check",
        "npm run build",
        "cargo build",
        "pytest",
        "python -m unittest",
        "go test",
        "mvn test",
        "gradle test",
        "ctest",
        "npm run lint",
        "eslint",
        "golangci-lint",
    ];
    test_patterns.iter().any(|pattern| lower.contains(pattern))
}

/// 判断文件路径是否为纯 UI 界面、样式、媒体或静态文档文件
pub fn is_pure_ui_or_doc_path(path_str: &str) -> bool {
    let lower = path_str.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    let non_ui_configs = [
        "package.json",
        "cargo.toml",
        "tsconfig.json",
        "go.mod",
        "pom.xml",
    ];
    if non_ui_configs.iter().any(|cfg| lower.ends_with(cfg)) {
        return false;
    }
    let pure_exts = [
        ".vue",
        ".css",
        ".scss",
        ".sass",
        ".less",
        ".html",
        ".htm",
        ".svg",
        ".png",
        ".jpg",
        ".jpeg",
        ".ico",
        ".gif",
        ".md",
        ".markdown",
        ".txt",
        ".json",
        ".yaml",
        ".yml",
    ];
    pure_exts.iter().any(|ext| lower.ends_with(ext))
}

/// 从工具参数 JSON 中提取可能被修改的文件路径
pub fn extract_file_paths_from_params(params: &serde_json::Value) -> Vec<String> {
    let mut paths = Vec::new();
    if let serde_json::Value::Object(map) = params {
        for (k, v) in map {
            let key_lower = k.to_lowercase();
            if (key_lower == "path"
                || key_lower == "target_file"
                || key_lower == "targetfile"
                || key_lower == "file_path"
                || key_lower == "filepath"
                || key_lower == "absolutepath")
                && v.is_string()
            {
                if let Some(s) = v.as_str() {
                    if !s.trim().is_empty() {
                        paths.push(s.trim().to_string());
                    }
                }
            }
        }
    }
    paths
}

/// 从工具参数 JSON 中提取 shell 命令字符串
pub fn extract_command_from_params(params: &serde_json::Value) -> Option<String> {
    if let serde_json::Value::Object(map) = params {
        for (k, v) in map {
            let key_lower = k.to_lowercase();
            if (key_lower == "command" || key_lower == "commandline" || key_lower == "cmd")
                && v.is_string()
            {
                if let Some(s) = v.as_str() {
                    if !s.trim().is_empty() {
                        return Some(s.trim().to_string());
                    }
                }
            }
        }
    }
    None
}

/// 检查给定路径切片是否全部为纯 UI 或静态文档文件
pub fn are_all_paths_ui_or_doc(paths: &[String]) -> bool {
    if paths.is_empty() {
        return false;
    }
    paths.iter().all(|p| is_pure_ui_or_doc_path(p))
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

    #[test]
    fn test_is_test_or_verification_command() {
        assert!(is_test_or_verification_command("cargo test --lib"));
        assert!(is_test_or_verification_command("npm test"));
        assert!(is_test_or_verification_command("npx vitest run"));
        assert!(is_test_or_verification_command("npx vue-tsc --noEmit"));
        assert!(is_test_or_verification_command("pytest tests/test_api.py"));
        assert!(!is_test_or_verification_command("git status"));
        assert!(!is_test_or_verification_command("cat src/main.rs"));
        assert!(!is_test_or_verification_command("ls -la"));
    }

    #[test]
    fn test_ui_and_doc_path_detection() {
        assert!(is_pure_ui_or_doc_path("src/components/AgentDeck.vue"));
        assert!(is_pure_ui_or_doc_path("src/style.css"));
        assert!(is_pure_ui_or_doc_path("README.md"));
        assert!(is_pure_ui_or_doc_path("docs/ARCHITECTURE.md"));
        assert!(!is_pure_ui_or_doc_path("src-tauri/src/main.rs"));
        assert!(!is_pure_ui_or_doc_path("src/auth.ts"));
        assert!(!is_pure_ui_or_doc_path("package.json"));
        assert!(!is_pure_ui_or_doc_path("Cargo.toml"));

        assert!(are_all_paths_ui_or_doc(&[
            "src/components/Card.vue".to_string(),
            "src/index.css".to_string(),
        ]));
        assert!(!are_all_paths_ui_or_doc(&[
            "src/components/Card.vue".to_string(),
            "src-tauri/src/main.rs".to_string(),
        ]));
    }
}
