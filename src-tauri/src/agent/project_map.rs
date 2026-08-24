use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewMode {
    DiffReview,
    ProjectReview,
    Incident,
}

impl ReviewMode {
    fn label(self) -> &'static str {
        match self {
            Self::DiffReview => "变更 / Diff 审查",
            Self::ProjectReview => "项目结构理解与审查",
            Self::Incident => "代码问题排查",
        }
    }
}

const EXCLUDED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    "coverage",
    ".codebase-memory",
];

const MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "README.md",
];

const ENTRY_CANDIDATES: &[&str] = &[
    "src/main.rs",
    "src/main.ts",
    "src/main.tsx",
    "src/index.ts",
    "src/index.tsx",
    "src-tauri/src/main.rs",
    "main.py",
    "app.py",
    "server.js",
    "server.ts",
];

pub fn detect_review_mode(goal: &str) -> Option<ReviewMode> {
    let normalized = goal.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|word| normalized.contains(word));

    if has(&[
        "diff",
        "pr",
        "pull request",
        "commit",
        "改动",
        "变更",
        "提交",
    ]) {
        Some(ReviewMode::DiffReview)
    } else if has(&[
        "报错",
        "错误",
        "异常",
        "bug",
        "error",
        "exception",
        "failure",
        "崩溃",
    ]) {
        Some(ReviewMode::Incident)
    } else if has(&[
        "审查",
        "评估",
        "代码库",
        "项目",
        "架构",
        "理解代码",
        "review",
        "audit",
        "codebase",
    ]) {
        Some(ReviewMode::ProjectReview)
    } else {
        None
    }
}

pub fn review_bootstrap(goal: &str, workspace: &Path) -> Option<String> {
    let mode = detect_review_mode(goal)?;
    Some(render_project_map(mode, workspace))
}

fn render_project_map(mode: ReviewMode, workspace: &Path) -> String {
    let manifests = existing_relative_paths(workspace, MANIFESTS);
    let entries = existing_relative_paths(workspace, ENTRY_CANDIDATES);
    let top_level = top_level_entries(workspace);
    let git_status = git_output(workspace, &["status", "--short"]);
    let diff_stat = git_output(workspace, &["diff", "--stat"]);
    let changed_files = git_output(workspace, &["diff", "--name-only"]);

    let mut lines = vec![
        "\n## [系统预取：项目审查地图]".to_string(),
        format!("模式：{}", mode.label()),
        format!("根目录：{}", workspace.display()),
        "此地图仅是导航元数据，不含源码正文；地图生成后若发生写入，请重新核验受影响文件和 Git 状态。".to_string(),
        format!("已排除：{}", EXCLUDED_DIRS.join("、")),
    ];

    if !top_level.is_empty() {
        lines.push(format!("顶层条目：{}", top_level.join("、")));
    }
    if !manifests.is_empty() {
        lines.push(format!("Manifest / 文档：{}", manifests.join("、")));
    }
    if !entries.is_empty() {
        lines.push(format!("入口候选：{}", entries.join("、")));
    }

    match mode {
        ReviewMode::DiffReview => {
            append_command_section(&mut lines, "Git 工作区状态", git_status.as_deref());
            append_command_section(&mut lines, "变更统计", diff_stat.as_deref());
            append_command_section(&mut lines, "变更文件", changed_files.as_deref());
        }
        ReviewMode::ProjectReview => {
            append_command_section(&mut lines, "Git 工作区状态", git_status.as_deref());
        }
        ReviewMode::Incident => {
            lines.push("排查策略：先用错误码、文件路径、函数名或路由进行代码搜索；再读取命中的函数及其相邻调用链。注意区分表象与根因：报错描述的是最终症状，触发报错的上游逻辑偏离才是修复目标。当代码意图与实际行为矛盾时，至少追问两层因果再给出结论。交付方案前反证推演：按此方案修复后，调用方是否仍能正常工作。".to_string());
        }
    }

    lines.extend([
        "阅读协议：先根据地图选择一个文件、符号、错误码或 diff hunk；代码正文优先请求最小必要行范围。".to_string(),
        "若大文件请求被限制，返回的符号/范围目录是正常约束而非工具故障；请选择范围继续。完整原文只在结论确实依赖其细节时定向核验。".to_string(),
        "禁止首轮并行读取多个大文件正文；并行仅适合目录、Git 状态、文件名单等轻量元数据。".to_string(),
    ]);

    let mut output = lines.join("\n");
    const MAP_CHAR_CAP: usize = 4_000;
    if output.chars().count() > MAP_CHAR_CAP {
        output = output.chars().take(MAP_CHAR_CAP).collect();
        output.push_str("\n[地图已按预算截短；请使用目录/搜索工具定向补充元数据。]");
    }
    output
}

fn append_command_section(lines: &mut Vec<String>, label: &str, output: Option<&str>) {
    let Some(output) = output.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    let preview: String = output.chars().take(1_200).collect();
    lines.push(format!("{}：\n{}", label, preview));
}

fn existing_relative_paths(workspace: &Path, candidates: &[&str]) -> Vec<String> {
    candidates
        .iter()
        .filter(|relative| workspace.join(relative).is_file())
        .map(|relative| (*relative).to_string())
        .collect()
}

fn top_level_entries(workspace: &Path) -> Vec<String> {
    let mut names = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(workspace) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().to_string();
        if EXCLUDED_DIRS.contains(&file_name.as_str()) || file_name.starts_with('.') {
            continue;
        }
        let suffix = entry
            .file_type()
            .map(|kind| if kind.is_dir() { "/" } else { "" })
            .unwrap_or("");
        names.insert(format!("{}{}", file_name, suffix));
    }
    names.into_iter().take(40).collect()
}

fn git_output(workspace: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_mode_prefers_diff_over_general_review() {
        assert_eq!(
            detect_review_mode("审查这次 commit 改动"),
            Some(ReviewMode::DiffReview)
        );
        assert_eq!(
            detect_review_mode("这个项目的架构如何"),
            Some(ReviewMode::ProjectReview)
        );
        assert_eq!(
            detect_review_mode("为什么出现 error 500"),
            Some(ReviewMode::Incident)
        );
        assert_eq!(detect_review_mode("写一段欢迎文案"), None);
    }

    #[test]
    fn map_marks_exclusions_and_navigation_rules() {
        let unique = format!("free-api-map-test-{}", std::process::id());
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname=\"demo\"").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();

        let map = review_bootstrap("审查这个项目", &root).unwrap();
        assert!(map.contains("Cargo.toml"));
        assert!(map.contains("src/main.rs"));
        assert!(map.contains("已排除"));
        assert!(map.contains("阅读协议"));
        assert!(!map.contains("node_modules/"));

        std::fs::remove_dir_all(root).unwrap();
    }
}
