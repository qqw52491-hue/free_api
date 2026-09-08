use crate::agent::protocol::FocusDomain;
use serde_json::Value;
use std::path::Path;
use tokio::process::Command;

/// diff 最大允许注入的行数上限（防止撑爆模型上下文）
pub const MAX_DIFF_LINES: usize = 80;
/// diff 最大允许注入的字符数上限
pub const MAX_DIFF_CHARS: usize = 2500;
/// 变更文件清单最大行数
pub const MAX_FILES_COUNT: usize = 50;
/// 验证日志最大尾部行数
pub const MAX_VERIFY_LOG_LINES: usize = 40;
/// 验证日志最大字符数
pub const MAX_VERIFY_LOG_CHARS: usize = 1500;

/// 异步采集工作区当前未暂存及已暂存的变更文件清单
pub async fn collect_changed_files(workspace_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace_root)
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return Some("当前工作区干净，暂无变更文件。".to_string());
    }

    let total = lines.len();
    let mut out = String::new();
    for line in lines.iter().take(MAX_FILES_COUNT) {
        out.push_str(line);
        out.push('\n');
    }
    if total > MAX_FILES_COUNT {
        out.push_str(&format!(
            "...（共 {} 个变更文件，已截取前 {} 个；完整列表请使用 list_dir/git_diff 工具查阅）\n",
            total, MAX_FILES_COUNT
        ));
    }
    Some(out.trim_end().to_string())
}

/// 异步采集工作区关键变更切片（带行数和字符预算硬截断与自取引导）
pub async fn collect_git_diff(workspace_root: &Path) -> Option<String> {
    // 优先尝试 git diff HEAD（兼顾已暂存与未暂存），失败时 fallback 到 git diff
    let output = match Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(workspace_root)
        .output()
        .await
    {
        Ok(out) if out.status.success() => out,
        _ => Command::new("git")
            .arg("diff")
            .current_dir(workspace_root)
            .output()
            .await
            .ok()?,
    };

    let mut diff = String::from_utf8_lossy(&output.stdout).to_string();
    if diff.trim().is_empty() {
        // 如果 HEAD 无差异，再试一次 git diff
        if let Ok(fallback) = Command::new("git")
            .arg("diff")
            .current_dir(workspace_root)
            .output()
            .await
        {
            diff = String::from_utf8_lossy(&fallback.stdout).to_string();
        }
    }

    if diff.trim().is_empty() {
        return Some("当前暂无代码改动差异 (git diff 为空)。".to_string());
    }

    let lines: Vec<&str> = diff.lines().collect();
    let total_lines = lines.len();
    let mut truncated = false;
    let mut slice_lines = Vec::new();

    for line in lines.iter().take(MAX_DIFF_LINES) {
        slice_lines.push(*line);
    }
    if total_lines > MAX_DIFF_LINES {
        truncated = true;
    }

    let mut joined = slice_lines.join("\n");
    if joined.chars().count() > MAX_DIFF_CHARS {
        joined = joined.chars().take(MAX_DIFF_CHARS).collect();
        truncated = true;
    }

    if truncated {
        joined.push_str(&format!(
            "\n\n[⚠️ 上下文预算控制：当前 diff 共 {} 行，已截取关键切片；深入细节请调用只读工具 `git_diff` 或 `read_file` 自行取证]",
            total_lines
        ));
    }

    Some(joined)
}

/// 采集最近一次自动化验证/构建/测试日志的尾部关键摘要
pub fn collect_last_verify_log(last_verify_log: Option<&str>) -> Option<String> {
    let raw = last_verify_log?.trim();
    if raw.is_empty() {
        return None;
    }

    let lines: Vec<&str> = raw.lines().collect();
    let total = lines.len();
    let tail: Vec<&str> = if total > MAX_VERIFY_LOG_LINES {
        lines[total - MAX_VERIFY_LOG_LINES..].to_vec()
    } else {
        lines
    };

    let mut out = tail.join("\n");
    if out.chars().count() > MAX_VERIFY_LOG_CHARS {
        out = out
            .chars()
            .rev()
            .take(MAX_VERIFY_LOG_CHARS)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
    }
    Some(out)
}

/// 核心上下文装配器：根据角色声明的关注域清单与自定义业务入参，异步组装结构化背景材料
pub async fn assemble_focus_context(
    domains: &[FocusDomain],
    workspace_root: &Path,
    last_verify_log: Option<&str>,
    custom_inputs: Option<&Value>,
) -> Option<String> {
    let mut sections = Vec::new();

    // 1. 装配主调度显式传入的自定义业务入参 (custom_inputs)
    if let Some(inputs) = custom_inputs {
        if let Some(obj) = inputs.as_object() {
            if !obj.is_empty() {
                let mut input_lines = Vec::new();
                input_lines.push("【📌 主调度传递的专项业务参数 (Custom Inputs)】".to_string());
                for (k, v) in obj {
                    let val_str = match v {
                        Value::String(s) => s.clone(),
                        Value::Array(arr) => {
                            let items: Vec<String> = arr.iter().map(|item| match item {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            }).collect();
                            format!("[{}]", items.join(", "))
                        }
                        other => other.to_string(),
                    };
                    input_lines.push(format!("- **{}**: {}", k, val_str));
                }
                sections.push(input_lines.join("\n"));
            }
        }
    }

    // 2. 根据 focus domains 并发/按需装配系统底层材料
    if !domains.is_empty() {
        let mut domain_blocks = Vec::new();
        domain_blocks.push("【🎯 系统按关注域预装配的专属材料 (Pre-assembled Focus Materials)】\n（提示：系统底层已为你自动准备好以下关键确定性材料，深入细节请使用只读工具自取）".to_string());

        for domain in domains {
            match domain {
                FocusDomain::ChangedFiles => {
                    if let Some(files) = collect_changed_files(workspace_root).await {
                        domain_blocks.push(format!("--- 📁 变更文件列表 (changed_files) ---\n```\n{}\n```", files));
                    }
                }
                FocusDomain::GitDiff => {
                    if let Some(diff) = collect_git_diff(workspace_root).await {
                        domain_blocks.push(format!("--- ⚡ 关键代码差异 (git_diff) ---\n```diff\n{}\n```", diff));
                    }
                }
                FocusDomain::LastVerifyLog => {
                    if let Some(log) = collect_last_verify_log(last_verify_log) {
                        domain_blocks.push(format!("--- 🧪 最近验证/构建输出 (last_verify_log) ---\n```\n{}\n```", log));
                    }
                }
            }
        }

        if domain_blocks.len() > 1 {
            sections.push(domain_blocks.join("\n\n"));
        }
    }

    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_collect_last_verify_log_truncation() {
        assert_eq!(collect_last_verify_log(None), None);
        assert_eq!(collect_last_verify_log(Some("   ")), None);

        let short_log = "cargo check passed\n0 warnings";
        assert_eq!(collect_last_verify_log(Some(short_log)), Some(short_log.to_string()));

        // 超长多行截断取尾部
        let mut long_log = String::new();
        for i in 0..100 {
            long_log.push_str(&format!("line {}\n", i));
        }
        let collected = collect_last_verify_log(Some(&long_log)).unwrap();
        assert!(collected.contains("line 99"));
        assert!(!collected.contains("line 10\n"));
    }

    #[tokio::test]
    async fn test_assemble_focus_context_with_inputs_only() {
        let inputs = json!({
            "business_goal": "重构权限模块",
            "impact_scope": ["auth.rs", "router.rs"]
        });

        let ctx = assemble_focus_context(&[], Path::new("/tmp"), None, Some(&inputs)).await;
        assert!(ctx.is_some());
        let text = ctx.unwrap();
        assert!(text.contains("【📌 主调度传递的专项业务参数 (Custom Inputs)】"));
        assert!(text.contains("business_goal"));
        assert!(text.contains("重构权限模块"));
        assert!(text.contains("impact_scope"));
    }

    #[tokio::test]
    async fn test_assemble_focus_context_with_verify_log() {
        let domains = vec![FocusDomain::LastVerifyLog];
        let verify_log = "test result: ok. 10 passed; 0 failed;";

        let ctx = assemble_focus_context(&domains, Path::new("/tmp"), Some(verify_log), None).await;
        assert!(ctx.is_some());
        let text = ctx.unwrap();
        assert!(text.contains("【🎯 系统按关注域预装配的专属材料"));
        assert!(text.contains("--- 🧪 最近验证/构建输出 (last_verify_log) ---"));
        assert!(text.contains("10 passed"));
    }
}
