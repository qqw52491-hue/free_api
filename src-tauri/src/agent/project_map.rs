use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewMode {
    DiffReview,
    ProjectReview,
    Incident,
    Implementation,
}

impl ReviewMode {
    fn label(self) -> &'static str {
        match self {
            Self::DiffReview => "变更 / Diff 审查",
            Self::ProjectReview => "项目结构理解与审查",
            Self::Incident => "代码问题排查",
            Self::Implementation => "实现 / 修复类开发任务",
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
        "修复",
        "实现",
        "新增",
        "开发",
        "重构",
        "编写",
        "添加",
        "fix",
        "implement",
        "feature",
    ]) {
        // 实现/修复类开发任务：同样需要符号地图做精准定位（Aider repo-map 的轻量形态）
        Some(ReviewMode::Implementation)
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
        ReviewMode::Implementation => {
            lines.push("实现策略：先对照符号地图锁定目标文件与函数，再读取最小行范围；修改前确认调用方与向后兼容，修改后确认测试覆盖。".to_string());
        }
    }

    // 符号地图（Aider repo-map 的轻量形态）：文件 → 关键定义 + 行号。
    // 让模型首轮定位从"反复搜索兜底"升级为"按图索骥"，直接命中目标区间。
    if let Some(symbols) = symbol_map_section(workspace) {
        lines.push(symbols);
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

const SYMBOL_MAP_BUDGET: usize = 2_200;
const SYMBOL_SCAN_FILES: usize = 15;
const SYMBOLS_PER_FILE: usize = 6;
const SOURCE_EXTENSIONS: &[&str] = &["rs", "ts", "tsx", "js", "py"];

/// 符号地图（Aider repo-map 的轻量文本形态）：扫描源码文件的顶层定义，
/// 输出 "文件:行号 → 签名" 的紧凑列表。无 tree-sitter 依赖；预算 2.2k 字符封顶。
fn symbol_map_section(workspace: &Path) -> Option<String> {
    let mut files = Vec::new();
    collect_source_files(workspace, 0, &mut files);
    if files.is_empty() {
        return None;
    }
    files.sort();

    let mut lines = Vec::new();
    let mut budget = SYMBOL_MAP_BUDGET;
    for path in files.iter().take(SYMBOL_SCAN_FILES) {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let mut file_syms: Vec<String> = Vec::new();
        for (idx, raw) in content.lines().enumerate() {
            let trimmed = raw.trim_start();
            let hit = if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                trimmed.starts_with("fn ")
                    || trimmed.starts_with("pub fn ")
                    || trimmed.starts_with("struct ")
                    || trimmed.starts_with("enum ")
                    || trimmed.starts_with("trait ")
                    || trimmed.starts_with("impl ")
            } else {
                trimmed.starts_with("function ")
                    || trimmed.starts_with("class ")
                    || trimmed.starts_with("def ")
                    || trimmed.starts_with("export function ")
            };
            if hit {
                file_syms.push(format!(
                    "  L{} {}",
                    idx + 1,
                    trimmed.chars().take(64).collect::<String>()
                ));
            }
        }
        if file_syms.is_empty() {
            continue;
        }
        let display = path
            .strip_prefix(workspace)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        let take = file_syms.len().min(SYMBOLS_PER_FILE);
        budget = budget.saturating_sub(display.chars().count() + 40);
        if budget == 0 {
            break;
        }
        lines.push(format!("{}:", display));
        for sym in file_syms.into_iter().take(take) {
            let cost = sym.chars().count() + 1;
            if budget < cost {
                break;
            }
            budget -= cost;
            lines.push(sym);
        }
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "符号地图（文件:行号 → 定义，用于首轮精准定位）：\n{}",
        lines.join("\n")
    ))
}

fn collect_source_files(dir: &Path, depth: usize, out: &mut Vec<std::path::PathBuf>) {
    if depth > 3 || out.len() >= SYMBOL_SCAN_FILES * 3 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if EXCLUDED_DIRS.contains(&name.as_str()) || name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_source_files(&path, depth + 1, out);
        } else if SOURCE_EXTENSIONS
            .iter()
            .any(|ext| name.rsplit('.').next() == Some(*ext))
        {
            out.push(path);
        }
        if out.len() >= 60 {
            return;
        }
    }
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

    #[test]
    fn implementation_mode_gets_symbol_map() {
        let unique = format!("free-api-map-sym-{}", std::process::id());
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(root.join("src/agent")).unwrap();
        std::fs::write(
            root.join("src/agent/checkpoint.rs"),
            "pub struct Batch {\n    ok: bool,\n}\n\npub fn is_complete(&self) -> bool {\n    true\n}\n",
        )
        .unwrap();

        assert_eq!(
            detect_review_mode("修复子代理容灾死锁"),
            Some(ReviewMode::Implementation),
            "修复/实现类任务必须命中 Implementation 模式"
        );
        let map = review_bootstrap("修复子代理容灾死锁", &root).unwrap();
        assert!(map.contains("符号地图"), "实现任务必须携带符号地图: {map}");
        assert!(map.contains("checkpoint.rs"), "符号地图应列出源文件");
        assert!(map.contains("is_complete"), "符号地图应含函数定义");

        std::fs::remove_dir_all(root).unwrap();
    }
}
