//! 自动审查材料的已知运行日志边界；不影响显式读取日志的取证工具。

/// 宿主归档的运行日志目录。只匹配完整目录名，不按 JSON 扩展名或前缀猜测。
pub(crate) fn is_runtime_log_dir(name: &str) -> bool {
    name == "agent_logs"
}

/// 文件位于根或嵌套运行日志目录内。末段是文件名，同名普通文件不被排除。
/// 字符串路径也用于 Git diff 头，兼容两种路径分隔符。
pub(crate) fn is_runtime_log_path(path: &str) -> bool {
    let mut parts = path.split(['/', '\\']).peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_some() && is_runtime_log_dir(part) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_components_are_exact_and_do_not_exclude_real_json_names() {
        for path in [
            "agent_logs/review.json",
            "src-tauri/agent_logs/review.json",
            r"src-tauri\agent_logs\review.json",
            "diff --git a/agent_logs/old.json b/agent_logs/old.json",
        ] {
            assert!(is_runtime_log_path(path), "{path}");
        }
        for path in [
            "config/agent_logs.json",
            "my_agent_logs/config.json",
            "config/agent_logs",
            "src/agent_logs.rs",
            "reports/review.md",
        ] {
            assert!(!is_runtime_log_path(path), "{path}");
        }
    }
}
