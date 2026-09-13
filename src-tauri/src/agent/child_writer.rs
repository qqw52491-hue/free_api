use crate::db::DbState;
use crate::llm::{self, ModelConfig};
use uuid::Uuid;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;

/// L3 隔离写入 worker 预算：比侦察 worker 更宽，但仍受步数/时长/写入量硬顶约束。
pub const MAX_WRITER_STEPS: usize = 14;
pub const WRITER_TIMEOUT_SECS: u64 = 300;
const WRITER_MAX_OUTPUT_TOKENS: i64 = 1_200;
const MAX_TASK_CHARS: usize = 2_000;
const MAX_FOCUS_PATHS: usize = 5;
const MAX_OBSERVATION_CHARS: usize = 60_000;
const MAX_ACTION_OUTPUT_CHARS: usize = 8_000;
const MAX_WRITES: usize = 20;
const MAX_WRITE_FILE_BYTES: usize = 400_000;
const COMMAND_TIMEOUT_SECS: u64 = 60;
const MAX_COMMAND_OUTPUT_BYTES: usize = 200_000;
const MAX_COPY_FILES: usize = 4_000;
const MAX_COPY_BYTES: u64 = 200_000_000;
const MAX_DIFF_TOTAL_CHARS: usize = 48_000;
/// 每个父运行只允许 1 个写入 worker（副本成本高 + 收敛责任单一）
pub const MAX_WRITERS_PER_RUN: usize = 1;

/// 写入 worker 允许的命令：只读侦察集 + 在副本内运行的构建/测试执行器。
/// 所有命令的 cwd 都被钉在隔离副本内，写入副作用只落副本。
const WRITER_COMMAND_ALLOWLIST: &[&str] = &[
    "ls", "cat", "head", "tail", "grep", "rg", "find", "wc", "stat", "file", "du", "diff", "sort",
    "uniq", "cut", "awk", "sed", "node", "npm", "npx", "cargo", "python3", "make", "git",
];
const GIT_READONLY_SUBCOMMANDS: &[&str] = &["log", "diff", "status", "show", "branch", "blame"];

#[derive(Debug, Clone)]
pub struct WriterRequest {
    pub task: String,
    pub test_command: String,
    pub focus_paths: Vec<String>,
    pub context_excerpts: Option<crate::agent::types::DispatchContext>,
    pub hard_scope: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictStatus {
    CleanReplay,     // root_now == spawn_hash
    AlreadyApplied,  // root_now == result_hash
    Conflict,        // 三者互不相等
}

impl ConflictStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::CleanReplay => "✅ 可直接重放",
            Self::AlreadyApplied => "✅ 内容已一致，无需操作",
            Self::Conflict => "⚠️ 发生冲突，禁止盲目覆盖",
        }
    }
}

/// 三态冲突检测纯函数：比较 spawn 时刻快照、writer 修改结果、当前主工作区三者指纹
pub fn evaluate_file_conflict(
    spawn_hash: &str,
    result_hash: &str,
    root_now_hash: &str,
) -> ConflictStatus {
    if root_now_hash == spawn_hash {
        ConflictStatus::CleanReplay
    } else if root_now_hash == result_hash {
        ConflictStatus::AlreadyApplied
    } else {
        ConflictStatus::Conflict
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub bytes_before: usize,
    pub bytes_after: usize,
    #[serde(default)]
    pub diff: String,
    #[serde(default)]
    pub conflict_status: Option<ConflictStatus>,
    #[serde(default)]
    pub spawn_hash: Option<String>,
    #[serde(default)]
    pub result_hash: Option<String>,
    #[serde(default)]
    pub root_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriterResult {
    pub child_run_id: String,
    pub status: String,
    pub findings: Vec<String>,
    pub confidence: String,
    pub evidence_refs: Vec<String>,
    pub missing_evidence: Vec<String>,
    pub duration_ms: u64,
    pub completion_tokens: i64,
    #[serde(default)]
    pub prompt_tokens: i64,
    #[serde(default)]
    pub error_kind: Option<String>,
    #[serde(default)]
    pub steps_used: usize,
    /// L3 扩展字段：root 审阅 diff 后用常规写工具应用到真实工作区（root 仍是唯一写入者）
    #[serde(default)]
    pub changed_files: Vec<ChangedFile>,
    #[serde(default)]
    pub test_command: String,
    #[serde(default)]
    pub test_status: String,
    #[serde(default)]
    pub test_output_tail: String,
    #[serde(default)]
    pub workspace_copy: String,
    #[serde(default)]
    pub patch_ref: Option<String>,
}

pub fn parse_writer_request(params: &Value) -> Result<WriterRequest, String> {
    let task = params
        .get("task")
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "child_writer 缺少 task（自包含的修改任务：目标文件、预期行为、约束）。".to_string())?;
    if task.chars().count() > MAX_TASK_CHARS {
        return Err(format!("child_writer.task 最多 {} 个字符。", MAX_TASK_CHARS));
    }
    let test_command = params
        .get("test_command")
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "child_writer 必须携带 test_command（在副本内验证修改的测试命令，如 \"npm test\"）。".to_string())?;
    if test_command.chars().count() > 200
        || test_command.chars().any(|c| [';', '|', '&', '>', '<', '`', '$', '(', ')', '\n'].contains(&c))
    {
        return Err("child_writer.test_command 必须是单条命令且不含控制/拼接字符。".to_string());
    }
    let mut focus_paths = Vec::new();
    if let Some(paths) = params.get("focus_paths").and_then(|v| v.as_array()) {
        for path in paths {
            let path = path
                .as_str()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| "child_writer.focus_paths 必须是字符串数组。".to_string())?;
            if focus_paths.len() >= MAX_FOCUS_PATHS {
                return Err(format!(
                    "child_writer.focus_paths 最多 {} 个。\n💡 建议：写入工兵需聚焦核心目标，请只传入 1~{} 个关键文件路径（如 [\"src/a.js\", \"src/b.js\"]）。",
                    MAX_FOCUS_PATHS,
                    MAX_FOCUS_PATHS
                ));
            }
            focus_paths.push(path);
        }
    }
    let context_excerpts = crate::agent::types::parse_dispatch_context(params.get("context_excerpts"))?;
    let hard_scope = params
        .get("hard_scope")
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_string())
        .unwrap_or_default();
    Ok(WriterRequest {
        task,
        test_command,
        focus_paths,
        context_excerpts,
        hard_scope,
    })
}

/// 把父工作区准备为 writer 的私有沙箱（优先尝试 git worktree 秒级创建；非 git 项目平滑降级为物理拷贝）。
pub fn create_isolated_workspace(parent_root: &Path, copy_root: &Path) -> Result<(usize, u64), String> {
    // 1. 优先尝试 Git Worktree（毫秒级、0 拷贝、天然分支隔离）
    let is_git_repo = parent_root.join(".git").exists();
    if is_git_repo {
        let worktree_res = std::process::Command::new("git")
            .args(&["worktree", "add", "--detach"])
            .arg(copy_root)
            .arg("HEAD")
            .current_dir(parent_root)
            .output();
        if let Ok(out) = worktree_res {
            if out.status.success() && copy_root.exists() {
                return Ok((1, 0));
            }
        }
    }

    // 2. 降级走物理拷贝
    create_isolated_copy(parent_root, copy_root)
}

/// 把父工作区复制为 writer 的私有副本（跳过 .git 与既有 writer 副本；预算硬顶防失控）。
pub fn create_isolated_copy(parent_root: &Path, copy_root: &Path) -> Result<(usize, u64), String> {
    let mut files = 0usize;
    let mut bytes: u64 = 0;
    copy_recursive(parent_root, copy_root, &mut files, &mut bytes)?;
    Ok((files, bytes))
}

fn copy_recursive(src: &Path, dst: &Path, files: &mut usize, bytes: &mut u64) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|error| format!("创建副本目录失败: {}", error))?;
    for entry in std::fs::read_dir(src).map_err(|error| format!("读取源目录失败: {}", error))? {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy().to_string();
        if name_str == ".git" || name_str.starts_with(".writer-") {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_dir() {
            copy_recursive(&src_path, &dst_path, files, bytes)?;
        } else if file_type.is_file() {
            *files += 1;
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            *bytes += size;
            if *files > MAX_COPY_FILES || *bytes > MAX_COPY_BYTES {
                return Err(format!(
                    "工作区超出副本预算（最多 {} 文件 / {} MB）；请先清理构建产物或缩小工作区。",
                    MAX_COPY_FILES,
                    MAX_COPY_BYTES / 1_000_000
                ));
            }
            std::fs::copy(&src_path, &dst_path).map_err(|error| format!("复制 {} 失败: {}", name_str, error))?;
        }
    }
    Ok(())
}

fn ensure_contained(copy_root: &Path, raw: &str) -> Result<PathBuf, String> {
    let root = copy_root
        .canonicalize()
        .map_err(|error| format!("副本工作区不可访问: {}", error))?;
    let target = PathBuf::from(raw.trim());
    let target = if target.is_relative() {
        copy_root.join(target)
    } else {
        target
    };
    // 写入目标允许尚不存在（新建文件）：校验其父目录必须已在副本内
    let canonical = match target.canonicalize() {
        Ok(path) => path,
        Err(_) => {
            let parent = target
                .parent()
                .ok_or_else(|| format!("非法写入路径: {}", raw))?
                .canonicalize()
                .map_err(|error| format!("写入路径父目录不可访问: {}", error))?;
            parent.join(target.file_name().ok_or_else(|| format!("非法路径: {}", raw))?)
        }
    };
    if !canonical.starts_with(&root) {
        return Err(format!("writer 只能访问副本内的路径：{}", raw));
    }
    Ok(canonical)
}

/// 检查目标相对路径是否落在 hard_scope 物理围栏内
pub fn check_hard_scope(copy_root: &Path, canonical: &Path, hard_scope: &str) -> Result<(), String> {
    if hard_scope.trim().is_empty() {
        return Ok(());
    }
    let root = copy_root
        .canonicalize()
        .map_err(|error| format!("工作区不可访问: {}", error))?;
    let rel = canonical.strip_prefix(&root).unwrap_or(canonical);
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    let rel_clean = rel_str.trim_start_matches("./");

    let allowed = hard_scope
        .split(|c| c == ',' || c == '；' || c == '\n')
        .map(|p| p.trim().replace('\\', "/"))
        .filter(|p| !p.is_empty())
        .any(|prefix| {
            let prefix_clean = prefix.trim_start_matches("./").trim_end_matches('/');
            if prefix_clean.is_empty() {
                return true;
            }
            rel_clean == prefix_clean || rel_clean.starts_with(&format!("{}/", prefix_clean))
        });

    if !allowed {
        return Err(format!(
            "【权限围栏拦截】目标路径 \"{}\" 超出当前角色的 hard_scope 允许范围（{}）。",
            rel_clean, hard_scope
        ));
    }
    Ok(())
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        text.to_string()
    } else {
        let mut result: String = text.chars().take(max_chars).collect();
        result.push_str(&format!("…（已截断，原文共 {} 字符）", count));
        result
    }
}

/// 精简行级 diff（- / + 行，无上下文），足够 root 审阅并重放修改。
pub fn line_diff(before: &str, after: &str) -> String {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    // LCS DP：文件行数上限保护（超大文件退化为摘要说明）
    if before_lines.len() * after_lines.len() > 4_000_000 {
        return format!("（文件过大未生成逐行 diff：{} 行 → {} 行）", before_lines.len(), after_lines.len());
    }
    let n = before_lines.len();
    let m = after_lines.len();
    let mut dp = vec![0u32; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * (m + 1) + j] = if before_lines[i] == after_lines[j] {
                dp[(i + 1) * (m + 1) + (j + 1)] + 1
            } else {
                dp[(i + 1) * (m + 1) + j].max(dp[i * (m + 1) + (j + 1)])
            };
        }
    }
    let mut out = String::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if before_lines[i] == after_lines[j] {
            i += 1;
            j += 1;
        } else if dp[(i + 1) * (m + 1) + j] >= dp[i * (m + 1) + (j + 1)] {
            out.push_str(&format!("- {}\n", before_lines[i]));
            i += 1;
        } else {
            out.push_str(&format!("+ {}\n", after_lines[j]));
            j += 1;
        }
    }
    while i < n {
        out.push_str(&format!("- {}\n", before_lines[i]));
        i += 1;
    }
    while j < m {
        out.push_str(&format!("+ {}\n", after_lines[j]));
        j += 1;
    }
    out
}

fn reject_shell_metachars(raw: &str) -> Result<(), String> {
    const FORBIDDEN: &[char] = &[';', '|', '&', '>', '<', '`', '$', '(', ')', '\n', '\r'];
    if raw.chars().any(|c| FORBIDDEN.contains(&c)) {
        return Err("run_command 不允许 shell 控制字符（管道/重定向/命令拼接）；一次只执行一条命令。".to_string());
    }
    Ok(())
}

fn command_allowed(argv: &[&str]) -> Result<(), String> {
    let Some(program) = argv.first() else {
        return Err("run_command 缺少命令。".to_string());
    };
    let base = program.rsplit('/').next().unwrap_or(program);
    if !WRITER_COMMAND_ALLOWLIST.contains(&base.to_string().as_str()) {
        return Err(format!(
            "命令 {} 不在 writer 白名单内；可用：{}",
            base,
            WRITER_COMMAND_ALLOWLIST.join(", ")
        ));
    }
    if base == "git" {
        let Some(sub) = argv.get(1) else {
            return Err("git 必须带只读子命令。".to_string());
        };
        if !GIT_READONLY_SUBCOMMANDS.contains(&sub.to_string().as_str()) {
            return Err(format!("git {} 不在 writer 只读白名单内。", sub));
        }
    }
    Ok(())
}

fn is_test_command(argv: &[&str]) -> bool {
    argv.iter().any(|token| *token == "test" || *token == "--test" || token.contains("test"))
}

#[derive(Debug, Deserialize)]
struct WriterAction {
    action: String,
    #[serde(default)]
    params: Value,
    #[serde(default)]
    findings: Vec<String>,
    #[serde(default)]
    confidence: String,
    #[serde(default)]
    evidence_refs: Vec<String>,
    #[serde(default)]
    missing_evidence: Vec<String>,
}

fn extract_json(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let bytes = raw.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, &byte) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&raw[start..=index]);
                }
            }
            _ => {}
        }
    }
    None
}

fn writer_system_prompt() -> &'static str {
    "你是 L3 隔离写入 worker。宿主已把工作区复制成你的私有副本：你的全部读/写/命令都只落在副本内，真实工作区对你是不可见的。每轮只输出一个纯 JSON 对象（无围栏、无闲聊）：\n\
{\"action\":\"list_dir\",\"params\":{\"path\":\".\"}}\n\
{\"action\":\"read_file\",\"params\":{\"path\":\"路径\"}}\n\
{\"action\":\"edit_file\",\"params\":{\"path\":\"路径\",\"target_content\":\"待替换原文本\",\"replacement_content\":\"替换后的新内容\"}}\n\
{\"action\":\"write_file\",\"params\":{\"path\":\"路径\",\"content\":\"完整新文件内容\"}}\n\
{\"action\":\"run_command\",\"params\":{\"command\":\"单条命令\"}}\n\
{\"action\":\"finish\",\"findings\":[\"1-8 条结论\"],\"confidence\":\"confirmed|supported|unverified\",\"evidence_refs\":[\"文件/测试引用\"],\"missing_evidence\":[\"...\"]}\n\
纪律：\n\
1. 先读后写：修改任何文件前必须先 read_file 读取最新内容，严禁盲目覆盖；\n\
2. 最小修改原则：修改已有代码时优先使用 edit_file 进行精准局部替换（确保 target_content 在文件中全局唯一），只修改与任务明确相关的代码，严禁顺带重构；\n\
3. 尊重既有设计：不得假设既有代码是错的，看似特殊的写法默认是开发者故意为之，禁止擅自改动现有逻辑；\n\
4. 严禁虚假作弊：禁止为了让测试通过而用静态硬编码/假数据蒙混过关，禁止过度设计；\n\
5. 保护注释与代码风格完整性，禁止随意精简或删除原有注释；\n\
6. 修改完成后必须用宿主指定的 test_command 验证且通过后才能 finish；禁止碰副本外的任何路径。"
}

/// 执行 writer 动作（异步、可单测）。返回 (观测输出, 是否测试命令)
pub async fn execute_writer_action(
    copy_root: &Path,
    action: &str,
    params: &Value,
    changes: &mut BTreeMap<String, String>,
    writes_used: &mut usize,
    test_command: &str,
    test_evidence: &mut Option<(String, bool, String)>,
    hard_scope: &str,
) -> Result<String, String> {
    match action {
        "list_dir" | "read_file" => {
            // 只读动作复用 scout 的实现语义，但根目录换成副本
            let raw = params
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("{} 缺少 path。", action))?;
            let canonical = ensure_contained(copy_root, raw)?;
            if action == "list_dir" {
                let mut entries = std::fs::read_dir(&canonical)
                    .map_err(|error| format!("列目录失败: {}", error))?
                    .filter_map(|entry| entry.ok())
                    .take(200)
                    .collect::<Vec<_>>();
                entries.sort_by_key(|entry| entry.file_name());
                let mut out = String::new();
                for entry in entries {
                    let kind = if entry.path().is_dir() { "dir " } else { "file" };
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    out.push_str(&format!("{} {}\t{}\n", kind, size, entry.file_name().to_string_lossy()));
                }
                Ok(truncate_chars(&out, MAX_ACTION_OUTPUT_CHARS))
            } else {
                let metadata = std::fs::metadata(&canonical)
                    .map_err(|error| format!("无法读取文件元数据: {}", error))?;
                if metadata.len() > 2_000_000 {
                    return Err("文件过大（>2MB），请用 grep/head 定位片段。".to_string());
                }
                let bytes = std::fs::read(&canonical).map_err(|error| format!("读取失败: {}", error))?;
                Ok(truncate_chars(&String::from_utf8_lossy(&bytes), MAX_ACTION_OUTPUT_CHARS))
            }
        }
        "write_file" => {
            if *writes_used >= MAX_WRITES {
                return Err(format!("写入次数已达上限 {}；请合并修改。", MAX_WRITES));
            }
            let raw = params
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "write_file 缺少 path。".to_string())?;
            let content = params
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "write_file 缺少 content（必须写完整文件内容）。".to_string())?;
            if content.len() > MAX_WRITE_FILE_BYTES {
                return Err(format!("write_file 内容过大（{} 字节 > {}）。", content.len(), MAX_WRITE_FILE_BYTES));
            }
            let canonical = ensure_contained(copy_root, raw)?;
            check_hard_scope(copy_root, &canonical, hard_scope)?;
            let relative = canonical
                .strip_prefix(copy_root.canonicalize().unwrap_or_else(|_| copy_root.to_path_buf()))
                .unwrap_or(&canonical)
                .to_string_lossy()
                .to_string();
            // 首次修改前留底，供 diff 生成
            if !changes.contains_key(&relative) {
                let before = std::fs::read(&canonical)
                    .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
                    .unwrap_or_default();
                changes.insert(relative.clone(), before);
            }
            if let Some(parent) = canonical.parent() {
                std::fs::create_dir_all(parent).map_err(|error| format!("创建目录失败: {}", error))?;
            }
            std::fs::write(&canonical, content).map_err(|error| format!("写入失败: {}", error))?;
            *writes_used += 1;
            Ok(format!("✅ 已写入 {}（{} 字节）", relative, content.len()))
        }
        "edit_file" => {
            if *writes_used >= MAX_WRITES {
                return Err(format!("写入次数已达上限 {}；请合并修改。", MAX_WRITES));
            }
            let raw = params
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "edit_file 缺少 path。".to_string())?;
            let target_content = params
                .get("target_content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "edit_file 缺少 target_content（待替换的目标原文本）。".to_string())?;
            let replacement_content = params
                .get("replacement_content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "edit_file 缺少 replacement_content（替换后的新内容）。".to_string())?;

            if target_content.is_empty() {
                return Err("edit_file target_content 不能为空。".to_string());
            }

            let canonical = ensure_contained(copy_root, raw)?;
            check_hard_scope(copy_root, &canonical, hard_scope)?;
            let relative = canonical
                .strip_prefix(copy_root.canonicalize().unwrap_or_else(|_| copy_root.to_path_buf()))
                .unwrap_or(&canonical)
                .to_string_lossy()
                .to_string();

            if !canonical.exists() {
                return Err(format!("待编辑的文件不存在: {}", relative));
            }

            // 首次修改前留底，供 diff 生成
            if !changes.contains_key(&relative) {
                let before = std::fs::read(&canonical)
                    .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
                    .unwrap_or_default();
                changes.insert(relative.clone(), before);
            }

            let current_content = std::fs::read_to_string(&canonical)
                .map_err(|error| format!("读取待编辑文件失败: {}", error))?;

            let matches: Vec<(usize, &str)> = current_content.match_indices(target_content).collect();
            if matches.is_empty() {
                return Err(format!(
                    "❌ [编辑失败：目标文本未找到] 在文件 \"{}\" 中未找到指定的 target_content。请先使用 read_file 读取最新内容核对精确文本。",
                    relative
                ));
            }
            if matches.len() > 1 {
                let line_numbers: Vec<usize> = matches
                    .iter()
                    .map(|(idx, _)| current_content[..*idx].lines().count() + 1)
                    .collect();
                return Err(format!(
                    "❌ [编辑失败：目标文本不唯一] 在文件 \"{}\" 中找到了 {} 处匹配（位于行号: {:?}）。请在 target_content 中多包含前后上下文确保唯一。",
                    relative, matches.len(), line_numbers
                ));
            }

            let match_offset = matches[0].0;
            let start_line = current_content[..match_offset].lines().count() + 1;
            let replaced_lines_count = target_content.lines().count().max(1);
            let end_line = start_line + replaced_lines_count - 1;

            let new_content = current_content.replacen(target_content, replacement_content, 1);
            std::fs::write(&canonical, new_content)
                .map_err(|error| format!("写入编辑结果失败: {}", error))?;
            *writes_used += 1;
            Ok(format!("✅ 已局部更新 {}（替换第 {} 至 {} 行的内容）", relative, start_line, end_line))
        }
        "run_command" => {
            let raw = params
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "run_command 缺少 command。".to_string())?;
            reject_shell_metachars(raw)?;
            let argv: Vec<&str> = raw.split_whitespace().collect();
            command_allowed(&argv)?;
            let mut cmd = tokio::process::Command::new(argv[0]);
            cmd.args(&argv[1..]).current_dir(copy_root);
            let timeout_dur = Duration::from_secs(COMMAND_TIMEOUT_SECS);
            let output = match tokio::time::timeout(timeout_dur, cmd.output()).await {
                Ok(res) => res.map_err(|error| format!("命令启动失败: {}", error))?,
                Err(_) => {
                    return Ok(format!("⚠️ 命令执行超时（超过 {} 秒），已被强制终止。", COMMAND_TIMEOUT_SECS));
                }
            };
            let mut stdout = output.stdout;
            stdout.truncate(MAX_COMMAND_OUTPUT_BYTES);
            let status = if output.status.success() { "✅ exit 0" } else { "⚠️ 非零退出" };
            let body = truncate_chars(&String::from_utf8_lossy(&stdout), MAX_ACTION_OUTPUT_CHARS);
            if is_test_command(&argv) {
                *test_evidence = Some((test_command.to_string(), output.status.success(), body.clone()));
            }
            Ok(format!("{}\n[stdout {}]", status, body))
        }
        other => Err(format!(
            "未知动作 {}；可用：list_dir、read_file、write_file、edit_file、run_command、finish。",
            other
        )),
    }
}

/// 初始化副本内的 Git 基线快照（解决副本无 .git 仓库无法 diff 的问题）
pub async fn init_git_baseline(copy_root: &Path) -> bool {
    let init = tokio::process::Command::new("git")
        .arg("init")
        .arg("-q")
        .current_dir(copy_root)
        .output()
        .await;
    if init.is_err() || !init.as_ref().unwrap().status.success() {
        return false;
    }
    let add = tokio::process::Command::new("git")
        .arg("add")
        .arg("-A")
        .current_dir(copy_root)
        .output()
        .await;
    if add.is_err() || !add.as_ref().unwrap().status.success() {
        return false;
    }
    let commit = tokio::process::Command::new("git")
        .args(&[
            "-c", "user.email=agent@local",
            "-c", "user.name=writer",
            "commit", "-qm", "spawn baseline", "--allow-empty"
        ])
        .current_dir(copy_root)
        .output()
        .await;
    commit.is_ok() && commit.unwrap().status.success()
}

/// 检查指定路径下 git 命令是否可用且可执行
pub async fn is_git_available(cwd: &Path) -> bool {
    tokio::process::Command::new("git")
        .arg("--version")
        .current_dir(cwd)
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 计算字节数据的指纹（优先使用 git hash-object --stdin，git 不可用时回退到 sha256）
pub async fn compute_bytes_hash(bytes: &[u8], git_available: bool, cwd: &Path) -> String {
    if bytes.is_empty() {
        return "empty/deleted".to_string();
    }
    if git_available {
        use tokio::io::AsyncWriteExt;
        if let Ok(mut child) = tokio::process::Command::new("git")
            .args(&["hash-object", "--stdin"])
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(bytes).await;
                drop(stdin);
            }
            if let Ok(out) = child.wait_with_output().await {
                if out.status.success() {
                    let h = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if !h.is_empty() {
                        return h;
                    }
                }
            }
        }
    }
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// 计算指定路径文件的指纹（以原始字节为基准，避免编码与换行符偏差）
pub async fn compute_file_hash(path: &Path, git_available: bool) -> String {
    if !path.exists() {
        return "empty/deleted".to_string();
    }
    match std::fs::read(path) {
        Ok(bytes) => {
            let cwd = path.parent().unwrap_or(path);
            compute_bytes_hash(&bytes, git_available, cwd).await
        }
        Err(_) => "read_error".to_string(),
    }
}

/// 计算文本内容的指纹（与原始字节统一委托给 compute_bytes_hash）
pub async fn compute_str_hash(content: &str, git_available: bool, cwd: &Path) -> String {
    compute_bytes_hash(content.as_bytes(), git_available, cwd).await
}

/// 在根任务消费回执或准备合入真实工作区前，重新以当前主工作区的真实状态刷新冲突指纹
pub async fn refresh_file_conflicts(
    changed_files: &mut [ChangedFile],
    parent_root: &Path,
    git_available: bool,
) {
    for f in changed_files.iter_mut() {
        let parent_file = parent_root.join(&f.path);
        let root_now_h = compute_file_hash(&parent_file, git_available).await;
        f.root_hash = Some(root_now_h.clone());
        if let (Some(spawn_h), Some(result_h)) = (&f.spawn_hash, &f.result_hash) {
            f.conflict_status = Some(evaluate_file_conflict(spawn_h, result_h, &root_now_h));
        }
    }
}

/// 从 Git 全量 diff 中截取指定相对路径文件的独立 diff 块
pub fn extract_file_diff(full_diff: &str, relative_path: &str) -> Option<String> {
    let clean_path = relative_path.replace('\\', "/");
    let marker = format!("diff --git a/{} b/{}", clean_path, clean_path);
    if let Some(pos) = full_diff.find(&marker) {
        let after_marker = &full_diff[pos..];
        let end = after_marker[marker.len()..]
            .find("diff --git ")
            .map(|offset| marker.len() + offset)
            .unwrap_or(after_marker.len());
        return Some(after_marker[..end].trim_end().to_string());
    }
    None
}

/// 脱水子任务历史消息：单条 observation 上限 4000 字符，大输出截断保留头尾
pub fn dehydrate_child_messages(messages: &[Value]) -> Vec<Value> {
    const MAX_DEHYDRATE_OBSERVATION_CHARS: usize = 4_000;
    messages
        .iter()
        .map(|msg| {
            let mut m = msg.clone();
            if let Some(content) = m.get("content").and_then(|v| v.as_str()) {
                if content.chars().count() > MAX_DEHYDRATE_OBSERVATION_CHARS {
                    let char_count = content.chars().count();
                    let head: String = content.chars().take(2_500).collect();
                    let tail: String = content
                        .chars()
                        .skip(char_count.saturating_sub(1_500))
                        .collect();
                    let dehydrated = format!(
                        "{}\n\n... [数据较长已脱水，省略 {} 字符] ...\n\n{}",
                        head,
                        char_count.saturating_sub(4_000),
                        tail
                    );
                    m["content"] = json!(dehydrated);
                }
            }
            m
        })
        .collect()
}

/// 父 run 终态时统一回收所有该父 run 下的子 writer 隔离副本
pub fn cleanup_child_workspaces(db: &DbState, parent_run_id: &str) {
    let Ok(conn) = db.0.lock() else { return; };
    let mut stmt = match conn.prepare(
        "SELECT workspace_copy FROM agent_child_runs WHERE parent_run_id = ?1 AND workspace_copy IS NOT NULL"
    ) {
        Ok(s) => s,
        Err(_) => return,
    };
    let copies: Vec<String> = stmt
        .query_map([parent_run_id], |row| row.get(0))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default();

    for path_str in copies {
        let p = PathBuf::from(path_str);
        if p.exists() {
            let is_safe_writer_dir = p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(".writer-"));
            if is_safe_writer_dir {
                let _ = std::fs::remove_dir_all(&p);
            }
        }
    }
}

/// 应用启动或异常恢复时，统一回收所有已终态或崩溃残留的僵尸 writer 隔离副本（防止 200MB 磁盘泄漏）
pub fn cleanup_orphaned_writer_workspaces(db: &DbState) -> usize {
    let Ok(conn) = db.0.lock() else { return 0; };
    // 1. 将上次异常退出的 running 子任务标记为 interrupted
    let _ = conn.execute(
        "UPDATE agent_child_runs SET status = 'interrupted' WHERE status = 'running'",
        [],
    );

    // 2. 查出所有父任务已非活跃状态或已终态的历史 workspace_copy
    let mut stmt = match conn.prepare(
        "SELECT DISTINCT workspace_copy FROM agent_child_runs WHERE workspace_copy IS NOT NULL",
    ) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let copies: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default();

    let mut cleaned_count = 0;
    for path_str in copies {
        let p = PathBuf::from(&path_str);
        if p.exists() {
            let is_safe_writer_dir = p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(".writer-"));
            if is_safe_writer_dir && std::fs::remove_dir_all(&p).is_ok() {
                cleaned_count += 1;
            }
        }
    }

    // 3. 兜底：扫描各项目根目录下未被 DB 记录但以 .writer- 开头的物理孤儿目录
    if let Ok(mut proj_stmt) = conn.prepare("SELECT root_path FROM agent_projects") {
        if let Ok(proj_rows) = proj_stmt.query_map([], |row| row.get::<_, String>(0)) {
            for root_res in proj_rows.flatten() {
                let proj_path = PathBuf::from(root_res);
                if let Ok(entries) = std::fs::read_dir(&proj_path) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.is_dir() {
                            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                                if name.starts_with(".writer-") && std::fs::remove_dir_all(&path).is_ok() {
                                    cleaned_count += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    cleaned_count
}

/// 依据修改留底与 Git 快照基线生成 Unified Hunk，并执行 48KB 分级降级与三态冲突检测
async fn build_changed_files(
    changes: &BTreeMap<String, String>,
    copy_root: &Path,
    parent_root: &Path,
    git_available: bool,
    db: &DbState,
    parent_session_id: &str,
    child_run_id: &str,
) -> (Vec<ChangedFile>, Option<String>) {
    let mut patch_ref = None;
    let mut git_full_diff = String::new();

    if git_available {
        let _ = tokio::process::Command::new("git")
            .arg("add")
            .arg("-A")
            .current_dir(copy_root)
            .output()
            .await;

        if let Ok(out) = tokio::process::Command::new("git")
            .args(&["diff", "--cached", "--unified=3"])
            .current_dir(copy_root)
            .output()
            .await
        {
            if out.status.success() {
                git_full_diff = String::from_utf8_lossy(&out.stdout).to_string();
            }
        }
    }

    let total_chars = git_full_diff.chars().count();
    let mut file_diffs: BTreeMap<String, String> = BTreeMap::new();

    if git_available && !git_full_diff.is_empty() {
        if total_chars <= MAX_DIFF_TOTAL_CHARS {
            // Tier 1: 全量 hunk 在 48KB 预算内，逐文件提取
            for rel in changes.keys() {
                if let Some(fd) = extract_file_diff(&git_full_diff, rel) {
                    file_diffs.insert(rel.clone(), fd);
                }
            }
        } else {
            // Tier 2: 超出预算，尝试 --unified=0 降级重算（砍掉上下文行）
            let u0_res = tokio::process::Command::new("git")
                .args(&["diff", "--cached", "--unified=0"])
                .current_dir(copy_root)
                .output()
                .await;
            let u0_diff = u0_res
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default();
            if !u0_diff.is_empty() && u0_diff.chars().count() <= MAX_DIFF_TOTAL_CHARS {
                for rel in changes.keys() {
                    if let Some(fd) = extract_file_diff(&u0_diff, rel) {
                        file_diffs.insert(rel.clone(), format!("⚠️ 上下文已精简 (--unified=0):\n{}", fd));
                    }
                }
            } else {
                // Tier 3 & 4: 完整补丁登记为 Artifact 工件，内联文件首头 + patch_ref
                let patch_title = format!("patch-{}.diff", &child_run_id[..8.min(child_run_id.len())]);
                if let Ok(art) = crate::agent::artifact_manager::save_managed_artifact(
                    db,
                    parent_session_id,
                    None,
                    &patch_title,
                    &git_full_diff,
                    Some("diff"),
                    Some("patch"),
                    Some("writer_patch"),
                ) {
                    patch_ref = Some(art.id.clone());
                }

                for rel in changes.keys() {
                    let fd = extract_file_diff(&git_full_diff, rel).unwrap_or_default();
                    let head_preview: String = fd.lines().take(10).collect::<Vec<_>>().join("\n");
                    let ref_hint = patch_ref
                        .as_deref()
                        .map(|id| format!("可用 artifact_manager(action: \"get\", id: \"{}\") 查看完整补丁", id))
                        .unwrap_or_else(|| "完整补丁已登记到受管工件".to_string());
                    file_diffs.insert(
                        rel.clone(),
                        format!(
                            "{}\n...（Diff 超出 48KB 预算，完整补丁已存入工件，{}）",
                            head_preview, ref_hint
                        ),
                    );
                }
            }
        }
    }

    let mut result_files = Vec::new();
    for (relative, before) in changes {
        let canonical_copy = copy_root.join(relative);
        let canonical_parent = parent_root.join(relative);

        let after = std::fs::read_to_string(&canonical_copy).unwrap_or_default();
        let diff = if let Some(d) = file_diffs.get(relative) {
            d.clone()
        } else if git_available {
            line_diff(before, &after)
        } else {
            format!("⚠️ git 不可用，回执为无行号简化 diff:\n{}", line_diff(before, &after))
        };

        let spawn_h = compute_str_hash(before, git_available, copy_root).await;
        let result_h = compute_file_hash(&canonical_copy, git_available).await;
        let root_now_h = compute_file_hash(&canonical_parent, git_available).await;
        let conflict_status = evaluate_file_conflict(&spawn_h, &result_h, &root_now_h);

        result_files.push(ChangedFile {
            path: relative.clone(),
            bytes_before: before.len(),
            bytes_after: after.len(),
            diff,
            conflict_status: Some(conflict_status),
            spawn_hash: Some(spawn_h),
            result_hash: Some(result_h),
            root_hash: Some(root_now_h),
        });
    }

    (result_files, patch_ref)
}

/// L3 隔离写入 worker 主循环（异步，tokio::spawn 调用）
#[allow(clippy::too_many_arguments)]
pub async fn run_writer_loop(
    db: DbState,
    app: Option<AppHandle>,
    parent_session_id: String,
    child_run_id: String,
    parent_root: PathBuf,
    copy_root: PathBuf,
    config: ModelConfig,
    reasoning_effort: String,
    request: WriterRequest,
    persona: String,
    mut interrupt_rx: watch::Receiver<u64>,
    initial_messages: Option<Vec<Value>>,
    max_steps_override: Option<usize>,
) {
    let started_at = std::time::Instant::now();
    let git_available = init_git_baseline(&copy_root).await;
    let mut completion_tokens: i64 = 0;
    let mut prompt_tokens: i64 = 0;
    let mut steps_used: usize = 0;
    let mut writes_used: usize = 0;
    let mut changes: BTreeMap<String, String> = BTreeMap::new();
    let mut test_evidence: Option<(String, bool, String)> = None;
    let mut test_nudged = false;
    let mut findings: Vec<String> = Vec::new();
    let mut confidence = "unverified".to_string();
    let mut evidence_refs: Vec<String> = Vec::new();
    let mut missing_evidence: Vec<String> = Vec::new();
    let mut interrupted: Option<String> = None;

    let max_steps = max_steps_override.unwrap_or(MAX_WRITER_STEPS);
    let mut messages = if let Some(msgs) = initial_messages {
        msgs
    } else {
        let focus = if request.focus_paths.is_empty() {
            "（无）".to_string()
        } else {
            request.focus_paths.join(", ")
        };
        let initial_user_prompt = render_writer_initial_prompt(&copy_root, &focus, &request);
        vec![
            json!({"role": "system", "content": format!("{}{}", writer_system_prompt(), persona)}),
            json!({"role": "user", "content": initial_user_prompt}),
        ]
    };

    let terminal: &str = loop {
        if started_at.elapsed() >= Duration::from_secs(WRITER_TIMEOUT_SECS)
            || steps_used >= max_steps
        {
            break "budget_exhausted";
        }
        steps_used += 1;

        let mut config = config.clone();
        config.max_tokens = if config.max_tokens <= 0 {
            WRITER_MAX_OUTPUT_TOKENS
        } else {
            config.max_tokens.min(WRITER_MAX_OUTPUT_TOKENS)
        };
        let mut on_stream_event = |event: llm::StreamEvent<'_>| -> bool {
            if let Some(ref handle) = app {
                if let llm::StreamEvent::Thinking { chunk, .. } = event {
                    let _ = handle.emit(
                        &format!("agent-progress-{}", parent_session_id),
                        json!({
                            "type": "child_writer_stream",
                            "stream_type": "thinking",
                            "chunk": chunk,
                            "child_run_id": &child_run_id,
                        }),
                    );
                }
            }
            true
        };
        let call = llm::stream_with_retry(
            &config,
            &messages,
            &reasoning_effort,
            &mut interrupt_rx,
            1,
            &mut on_stream_event,
        );
        let child_log_suffix = format!("child-{}", child_run_id);
        let raw = match tokio::time::timeout(
            Duration::from_secs(60),
            call,
        )
        .await
        {
            Err(_) => {
                crate::agent::utils::llm_client::log_auxiliary_llm_exchange(
                    &parent_session_id,
                    &child_log_suffix,
                    steps_used,
                    &config,
                    &messages,
                    None,
                    "request_error",
                    Some("timeout"),
                );
                interrupted = Some("writer 单步调用超时".to_string());
                break "timed_out";
            }
            Ok(Err(error)) => {
                crate::agent::utils::llm_client::log_auxiliary_llm_exchange(
                    &parent_session_id,
                    &child_log_suffix,
                    steps_used,
                    &config,
                    &messages,
                    None,
                    "request_error",
                    Some(&error),
                );
                interrupted = Some(if error == llm::STREAM_INTERRUPTED_ERROR {
                    "writer 已随父任务取消".to_string()
                } else {
                    format!("writer 调用失败: {}", error)
                });
                break if error == llm::STREAM_INTERRUPTED_ERROR {
                    "cancelled"
                } else {
                    "failed"
                };
            }
            Ok(Ok(result)) => {
                crate::agent::utils::llm_client::log_auxiliary_llm_exchange(
                    &parent_session_id,
                    &child_log_suffix,
                    steps_used,
                    &config,
                    &messages,
                    Some(&result),
                    "success",
                    None,
                );
                completion_tokens += result
                    .usage
                    .map(|usage| usage.completion_tokens)
                    .unwrap_or(0);
                prompt_tokens += result
                    .usage
                    .map(|usage| usage.prompt_tokens)
                    .unwrap_or(0);
                result.full_response
            }
        };

        let Some(json_text) = extract_json(&raw) else {
            messages.push(json!({"role": "assistant", "content": raw}));
            messages.push(json!({"role": "user", "content": "【宿主反馈】输出不是合法 JSON 动作，请重新输出。"}));
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<WriterAction>(json_text) else {
            messages.push(json!({"role": "assistant", "content": raw}));
            messages.push(json!({"role": "user", "content": "【宿主反馈】JSON 不符合动作协议。请修正。"}));
            continue;
        };

        if parsed.action == "finish" {
            if test_evidence.is_none() && !test_nudged {
                test_nudged = true;
                messages.push(json!({"role": "assistant", "content": raw}));
                messages.push(json!({"role": "user", "content": format!(
                    "【宿主强制门禁】尚未执行测试命令「{}」；交卷前必须运行测试并以通过为证据。请立即调用 run_command 执行测试。",
                    request.test_command
                )}));
                continue;
            }
            findings = parsed
                .findings
                .into_iter()
                .filter(|f| !f.trim().is_empty())
                .map(|f| f.chars().take(600).collect::<String>())
                .take(8)
                .collect();
            confidence = if matches!(
                parsed.confidence.as_str(),
                "confirmed" | "supported" | "unverified"
            ) {
                parsed.confidence
            } else {
                "unverified".to_string()
            };
            evidence_refs = parsed.evidence_refs.into_iter().take(10).collect();
            missing_evidence = parsed.missing_evidence.into_iter().take(5).collect();
            if findings.is_empty() {
                findings = vec!["writer 未产出有效结论".to_string()];
            }
            break "succeeded";
        }

        let observation = execute_writer_action(
            &copy_root,
            &parsed.action,
            &parsed.params,
            &mut changes,
            &mut writes_used,
            &request.test_command,
            &mut test_evidence,
            &request.hard_scope,
        )
        .await;
        let observation = match observation {
            Ok(output) => output,
            Err(error) => format!("【动作被拒绝】{}", error),
        };
        let budget_left = MAX_OBSERVATION_CHARS.saturating_sub(
            messages
                .iter()
                .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
                .map(|c| c.chars().count())
                .sum::<usize>(),
        );
        if budget_left == 0 {
            missing_evidence.push("观测预算耗尽".to_string());
            break "budget_exhausted";
        }
        messages.push(json!({"role": "assistant", "content": raw}));
        messages.push(json!({"role": "user", "content": format!("【宿主观测 #{}】\n{}", steps_used, truncate_chars(&observation, budget_left.min(MAX_ACTION_OUTPUT_CHARS)))}));
    };

    let (status, error_kind) = match terminal {
        "succeeded" => ("succeeded", None),
        "budget_exhausted" => {
            missing_evidence.push(format!(
                "以 budget_exhausted 终止（步数上限 {} / 时长上限 {}s）",
                max_steps, WRITER_TIMEOUT_SECS
            ));
            ("succeeded", None)
        }
        "cancelled" => ("cancelled", Some("writer 已随父任务取消".to_string())),
        "timed_out" => ("timed_out", Some("writer 单步调用超时".to_string())),
        _ => ("failed", interrupted.clone()),
    };
    if test_evidence.is_none() && status == "succeeded" {
        missing_evidence.push("交付缺少测试证据：test_command 未被执行".to_string());
        confidence = "unverified".to_string();
    }
    let (changed_files, patch_ref) = build_changed_files(
        &changes,
        &copy_root,
        &parent_root,
        git_available,
        &db,
        &parent_session_id,
        &child_run_id,
    )
    .await;
    let (test_status, test_tail) = match &test_evidence {
        Some((_, passed, output)) => (
            if *passed { "passed" } else { "failed" }.to_string(),
            truncate_chars(output, 2_000),
        ),
        None => ("not_run".to_string(), String::new()),
    };
    let result = WriterResult {
        child_run_id: child_run_id.clone(),
        status: status.to_string(),
        findings,
        confidence,
        evidence_refs,
        missing_evidence,
        duration_ms: started_at.elapsed().as_millis() as u64,
        completion_tokens,
        prompt_tokens,
        error_kind,
        steps_used,
        changed_files,
        test_command: request.test_command.clone(),
        test_status,
        test_output_tail: test_tail,
        workspace_copy: copy_root.display().to_string(),
        patch_ref,
    };

    let dehydrated = dehydrate_child_messages(&messages);
    let hist_json = serde_json::to_string(&dehydrated).ok();

    crate::agent::child_reviewer::finish_child_run_with_history(
        &db,
        &child_run_id,
        &result.status,
        result.duration_ms,
        result.completion_tokens,
        result.error_kind.as_deref(),
        Some(&serde_json::to_string(&result).unwrap_or_default()),
        hist_json.as_deref(),
    );
    if let Some(handle) = app {
        let _ = handle.emit(
            &format!("agent-log-{}", parent_session_id),
            format!(
                "🛠️ 写入 worker {} 结束：{}（{} 个文件修改，测试 {}）",
                child_run_id,
                result.status,
                result.changed_files.len(),
                result.test_status
            ),
        );
    }
    // P5: copy_root 保留保活，供后续 resume_child 追问复用；待父 run 终态由 cleanup_child_workspaces 统一回收
}

/// 渲染 writer 的初始首条 user 提示词（P2：有 context_excerpts 则分区渲染，无则保持经典紧凑格式）
pub fn render_writer_initial_prompt(
    copy_root: &Path,
    focus: &str,
    request: &WriterRequest,
) -> String {
    if let Some(ref ctx) = request.context_excerpts {
        let mut sections = Vec::new();
        if let Some(ref bg) = ctx.background {
            sections.push(format!("### 背景信息\n{}", bg));
        }
        if !ctx.todo_slice.is_empty() {
            let todos = ctx.todo_slice.iter().map(|t| format!("- {}", t)).collect::<Vec<_>>().join("\n");
            sections.push(format!("### 相关计划与待办\n{}", todos));
        }
        if !ctx.prior_findings.is_empty() {
            let findings = ctx.prior_findings.iter().map(|f| format!("- {}", f)).collect::<Vec<_>>().join("\n");
            sections.push(format!("### 既有结论与线索\n{}", findings));
        }
        if !ctx.file_hints.is_empty() {
            let hints = ctx.file_hints.iter().map(|h| format!("- {}", h)).collect::<Vec<_>>().join("\n");
            sections.push(format!("### 相关文件提示\n{}", hints));
        }
        sections.push(format!("### 修改任务\n{}", request.task));
        sections.push(format!("### 副本工作区根目录\n{}", copy_root.display()));
        sections.push(format!("### 关注路径\n{}", focus));
        sections.push(format!("### 必须执行的测试命令\n{}", request.test_command));
        sections.join("\n\n")
    } else {
        format!(
            "副本工作区根目录：{}\n关注路径：{}\n必须执行的测试命令：{}\n\n修改任务：{}",
            copy_root.display(),
            focus,
            request.test_command,
            request.task
        )
    }
}

/// 异步派发 L3 写入 worker：复制工作区为私有副本 → 动作-观测循环 → diff 回执。
#[allow(clippy::too_many_arguments)]
pub async fn spawn_child_writer(
    app: Option<AppHandle>,
    db: &DbState,
    parent_run_id: &str,
    parent_session_id: &str,
    workspace_root: &Path,
    model_id: &str,
    reasoning_effort: &str,
    params: &Value,
    persona: String,
    interrupt_rx: watch::Receiver<u64>,
) -> Result<String, String> {
    let request = parse_writer_request(params)?;
    let parent = workspace_root
        .canonicalize()
        .map_err(|error| format!("child_writer 工作区不可用: {}", error))?;
    let custom_cap = params.get("max_concurrency").and_then(|v| v.as_i64());
    let effective_writer_cap = match custom_cap {
        Some(cap) if cap > 0 => cap,
        _ => MAX_WRITERS_PER_RUN as i64,
    };
    {
        let conn = db.0.lock().map_err(|error| error.to_string())?;
        let existing: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_child_runs WHERE parent_run_id = ?1 AND kind = 'writer'",
                rusqlite::params![parent_run_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if existing >= effective_writer_cap {
            return Err(format!(
                "每个父运行最多 {} 个写入 worker；请先回收现有 worker 的 diff 再由你自己落实修改。",
                effective_writer_cap
            ));
        }
    }

    let child_nonce = Uuid::new_v4().to_string();
    let child8 = child_nonce.get(..8).unwrap_or(&child_nonce).to_string();
    let copy_root = parent.join(format!(".writer-{}", child8));
    let (files, bytes) = create_isolated_workspace(&parent, &copy_root)?;

    let child_run_id = crate::agent::child_reviewer::begin_child_run_extended(
        db,
        parent_run_id,
        parent_session_id,
        &parent,
        model_id,
        0,
        "writer",
        custom_cap,
        Some(copy_root.to_string_lossy().as_ref()),
        None,
    )?;

    let mut config = ModelConfig::from_db_state(db, model_id)?;
    config.max_tokens = if config.max_tokens <= 0 {
        WRITER_MAX_OUTPUT_TOKENS
    } else {
        config.max_tokens.min(WRITER_MAX_OUTPUT_TOKENS)
    };

    let db_clone = DbState(db.0.clone());
    let child_clone = child_run_id.clone();
    let session_clone = parent_session_id.to_string();
    let reasoning = reasoning_effort.to_string();
    let _ = (files, bytes); // 副本规模仅作日志观测，超预算在复制阶段已拒绝
    tokio::spawn(run_writer_loop(
        db_clone,
        app,
        session_clone,
        child_clone,
        parent.clone(),
        copy_root.clone(),
        config,
        reasoning,
        request,
        persona,
        interrupt_rx,
        None,
        None,
    ));
    Ok(child_run_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> (std::path::PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("free-api-writer-{}", uuid::Uuid::new_v4()));
        let root = base.join("ws");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("package.json"), "{\"scripts\":{\"test\":\"node --test\"}}").unwrap();
        std::fs::write(root.join("src/sum.mjs"), "export function sum(a, b) { return a - b; }\n").unwrap();
        std::fs::create_dir_all(root.join("test")).unwrap();
        std::fs::write(
            root.join("test/sum.test.mjs"),
            "import assert from 'node:assert/strict';\nimport test from 'node:test';\nimport { sum } from '../src/sum.mjs';\ntest('sum adds', () => { assert.equal(sum(1, 2), 3); });\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        (base, root)
    }

    #[test]
    fn parse_writer_request_requires_test_command() {
        let ok = parse_writer_request(&json!({"task": "修复 sum", "test_command": "npm test"})).unwrap();
        assert_eq!(ok.test_command, "npm test");
        assert!(ok.context_excerpts.is_none());
        assert!(parse_writer_request(&json!({"task": "修复 sum"})).is_err());
        assert!(parse_writer_request(&json!({"task": "x", "test_command": "a; b"})).is_err());
        assert!(parse_writer_request(&json!({"task": "x", "test_command": "npm test", "focus_paths": ["1","2","3","4","5","6"]})).is_err());

        // 带有 context_excerpts
        let with_ctx = parse_writer_request(&json!({
            "task": "修复 sum 函数边界",
            "test_command": "npm test",
            "context_excerpts": {
                "background": "sum.mjs 未对负数做容错",
                "todo_slice": ["修复负数逻辑", "通过单元测试"],
                "file_hints": ["src/sum.mjs"]
            }
        })).unwrap();
        assert!(with_ctx.context_excerpts.is_some());
        let ctx = with_ctx.context_excerpts.unwrap();
        assert_eq!(ctx.background.as_deref(), Some("sum.mjs 未对负数做容错"));
        assert_eq!(ctx.todo_slice.len(), 2);
    }

    #[test]
    fn render_writer_initial_prompt_partitions_or_fallbacks() {
        let req_simple = WriterRequest {
            task: "修复 bug".into(),
            test_command: "npm test".into(),
            focus_paths: vec!["src/main.rs".into()],
            context_excerpts: None,
            hard_scope: String::new(),
        };
        let p1 = render_writer_initial_prompt(Path::new("/tmp/copy"), "src/main.rs", &req_simple);
        assert!(p1.starts_with("副本工作区根目录：/tmp/copy\n关注路径：src/main.rs"));

        let req_rich = WriterRequest {
            task: "修复 bug".into(),
            test_command: "npm test".into(),
            focus_paths: vec!["src/main.rs".into()],
            context_excerpts: Some(crate::agent::types::DispatchContext {
                background: Some("这是详细背景".into()),
                todo_slice: vec!["步骤1".into()],
                file_hints: vec!["src/main.rs".into()],
                prior_findings: vec!["之前已发现 panic".into()],
            }),
            hard_scope: String::new(),
        };
        let p2 = render_writer_initial_prompt(Path::new("/tmp/copy"), "src/main.rs", &req_rich);
        assert!(p2.contains("### 背景信息\n这是详细背景"));
        assert!(p2.contains("### 相关计划与待办\n- 步骤1"));
        assert!(p2.contains("### 既有结论与线索\n- 之前已发现 panic"));
        assert!(p2.contains("### 相关文件提示\n- src/main.rs"));
        assert!(p2.contains("### 修改任务\n修复 bug"));
    }

    #[test]
    fn isolated_copy_skips_git_and_enforces_budget() {
        let (base, root) = workspace();
        let copy = base.join(".writer-copy1");
        let (files, bytes) = create_isolated_copy(&root, &copy).unwrap();
        assert!(files >= 2 && bytes > 0);
        assert!(!copy.join(".git").exists(), ".git 必须被排除");
        assert!(copy.join("src/sum.mjs").exists());
        assert!(copy.join("package.json").exists());
    }

    #[tokio::test]
    async fn write_file_records_before_snapshot_and_refuses_escape() {
        let (base, root) = workspace();
        let copy = base.join(".writer-copy2");
        create_isolated_copy(&root, &copy).unwrap();
        let mut changes = BTreeMap::new();
        let mut writes = 0usize;
        let mut test_evidence: Option<(String, bool, String)> = None;

        // 越界写入被拒绝
        assert!(execute_writer_action(&copy, "write_file", &json!({
            "path": "../evil.txt", "content": "boom"
        }), &mut changes, &mut writes, "npm test", &mut test_evidence, "").await.is_err());

        // hard_scope 物理围栏测试：只允许修改 src/，试图写 package.json 被拦截
        assert!(execute_writer_action(&copy, "write_file", &json!({
            "path": "package.json", "content": "{}"
        }), &mut changes, &mut writes, "npm test", &mut test_evidence, "src/").await.is_err());

        // 正常写入：留底 + 落盘
        let out = execute_writer_action(&copy, "write_file", &json!({
            "path": "src/sum.mjs", "content": "export function sum(a, b) { return a + b; }\n"
        }), &mut changes, &mut writes, "npm test", &mut test_evidence, "src/").await.unwrap();
        assert!(out.contains("已写入 src/sum.mjs"));
        assert_eq!(writes, 1);
        let before = changes.get("src/sum.mjs").unwrap();
        assert!(before.contains("return a - b;"), "首次写入前必须留底原内容");
        // 新建文件：留底为空
        execute_writer_action(&copy, "write_file", &json!({
            "path": "src/new-helper.mjs", "content": "export const ok = true;\n"
        }), &mut changes, &mut writes, "npm test", &mut test_evidence, "src/").await.unwrap();
        assert!(changes.contains_key("src/new-helper.mjs"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn run_command_tracks_test_evidence_and_blocks_non_allowlisted() {
        let (base, root) = workspace();
        let copy = base.join(".writer-copy3");
        create_isolated_copy(&root, &copy).unwrap();
        let mut changes = BTreeMap::new();
        let mut writes = 0usize;
        let mut test_evidence: Option<(String, bool, String)> = None;

        assert!(execute_writer_action(&copy, "run_command", &json!({"command": "rm -rf ."}), &mut changes, &mut writes, "npm test", &mut test_evidence, "").await.is_err());
        assert!(execute_writer_action(&copy, "run_command", &json!({"command": "npm test > out.log"}), &mut changes, &mut writes, "npm test", &mut test_evidence, "").await.is_err());

        execute_writer_action(&copy, "run_command", &json!({"command": "node --test"}), &mut changes, &mut writes, "node --test", &mut test_evidence, "").await.unwrap();
        let (command, passed, _) = test_evidence.expect("测试命令必须被记录为证据");
        assert_eq!(command, "node --test");
        // 测试会失败（sum 还没修），passed 应为 false
        assert!(!passed);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn line_diff_outputs_minus_and_plus_lines() {
        let diff = line_diff("a\nb\nc\n", "a\nB\nc\nd\n");
        assert!(diff.contains("- b"));
        assert!(diff.contains("+ B"));
        assert!(diff.contains("+ d"));
        assert!(!diff.contains("- a"));
    }

    #[test]
    fn test_evaluate_file_conflict_matrix() {
        // 1. 主工作区干净无第三方改动：可直接重放
        assert_eq!(
            evaluate_file_conflict("hash_v1", "hash_v2", "hash_v1"),
            ConflictStatus::CleanReplay
        );

        // 2. 主工作区已被修改且内容与 writer 产物完全一致：无需操作
        assert_eq!(
            evaluate_file_conflict("hash_v1", "hash_v2", "hash_v2"),
            ConflictStatus::AlreadyApplied
        );

        // 3. 主工作区被修改为第三方内容（三者互不相等）：真冲突，禁止盲目覆盖
        assert_eq!(
            evaluate_file_conflict("hash_v1", "hash_v2", "hash_v3"),
            ConflictStatus::Conflict
        );
    }

    #[test]
    fn test_dehydrate_child_messages_bounds() {
        let short_msg = json!({"role": "user", "content": "短文本"});
        let huge_text = "A".repeat(10_000);
        let long_msg = json!({"role": "user", "content": huge_text});

        let msgs = vec![short_msg, long_msg];
        let dehydrated = dehydrate_child_messages(&msgs);

        assert_eq!(dehydrated[0]["content"], "短文本");
        let long_content = dehydrated[1]["content"].as_str().unwrap();
        assert!(long_content.len() < 5000);
        assert!(long_content.contains("数据较长已脱水，省略 6000 字符"));
    }

    #[tokio::test]
    async fn test_init_git_baseline_and_diff_extraction() {
        let (base, root) = workspace();
        let copy = base.join(".writer-git-test");
        create_isolated_copy(&root, &copy).unwrap();

        let ok = init_git_baseline(&copy).await;
        if ok {
            // 修改文件
            std::fs::write(copy.join("src/sum.mjs"), "export function sum(a, b) { return a + b + 0; }\n").unwrap();
            let _ = tokio::process::Command::new("git").arg("add").arg("-A").current_dir(&copy).output().await;
            let out = tokio::process::Command::new("git")
                .args(&["diff", "--cached", "--unified=3"])
                .current_dir(&copy)
                .output()
                .await
                .unwrap();
            let diff_str = String::from_utf8_lossy(&out.stdout);
            let extracted = extract_file_diff(&diff_str, "src/sum.mjs");
            assert!(extracted.is_some());
            let fd = extracted.unwrap();
            assert!(fd.contains("diff --git a/src/sum.mjs b/src/sum.mjs"));
            assert!(fd.contains("+export function sum"));
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn test_cleanup_child_workspaces_removes_matching_dirs_only() {
        let (base, root) = workspace();
        let copy1 = base.join(".writer-test-cleanup-1");
        let copy2 = base.join(".writer-test-cleanup-2");
        let safe_keep = base.join("safe_keep_dir");
        std::fs::create_dir_all(&copy1).unwrap();
        std::fs::create_dir_all(&copy2).unwrap();
        std::fs::create_dir_all(&safe_keep).unwrap();

        let conn = crate::db::init_db(&base.join("test.db")).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));
        let conn = state.0.lock().unwrap();
        conn.execute(
            "INSERT INTO agent_child_runs (id, parent_run_id, parent_session_id, workspace_root, model_id, status, started_at, kind, workspace_copy) VALUES (?1, ?2, ?3, ?4, ?5, 'succeeded', ?6, 'writer', ?7)",
            rusqlite::params![
                "child-run-clean-1",
                "parent-run-clean",
                "session-clean",
                root.to_string_lossy(),
                "test-model",
                chrono::Utc::now().to_rfc3339(),
                copy1.to_string_lossy(),
            ],
        ).unwrap();
        conn.execute(
            "INSERT INTO agent_child_runs (id, parent_run_id, parent_session_id, workspace_root, model_id, status, started_at, kind, workspace_copy) VALUES (?1, ?2, ?3, ?4, ?5, 'succeeded', ?6, 'writer', ?7)",
            rusqlite::params![
                "child-run-clean-2",
                "parent-run-clean",
                "session-clean",
                root.to_string_lossy(),
                "test-model",
                chrono::Utc::now().to_rfc3339(),
                copy2.to_string_lossy(),
            ],
        ).unwrap();
        drop(conn);

        assert!(copy1.exists());
        assert!(copy2.exists());
        assert!(safe_keep.exists());

        cleanup_child_workspaces(&state, "parent-run-clean");

        assert!(!copy1.exists(), "已登记的 copy1 副本必须被回收");
        assert!(!copy2.exists(), "已登记的 copy2 副本必须被回收");
        assert!(safe_keep.exists(), "非 writer 目录绝不能被误删");

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn test_cleanup_orphaned_writer_workspaces_on_startup() {
        let (base, root) = workspace();
        let copy_db = base.join(".writer-test-orphan-db");
        let copy_disk_only = root.join(".writer-test-orphan-disk");
        let safe_keep = root.join("src");
        std::fs::create_dir_all(&copy_db).unwrap();
        std::fs::create_dir_all(&copy_disk_only).unwrap();
        std::fs::create_dir_all(&safe_keep).unwrap();

        let conn = crate::db::init_db(&base.join("test.db")).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));
        let conn = state.0.lock().unwrap();
        // 注册 agent_projects
        conn.execute(
            "INSERT INTO agent_projects (id, name, root_path, created_at, updated_at) VALUES ('p1', 'proj', ?1, ?2, ?2)",
            rusqlite::params![root.to_string_lossy(), chrono::Utc::now().to_rfc3339()],
        ).unwrap();
        // 模拟上次崩溃遗留的 running 任务
        conn.execute(
            "INSERT INTO agent_child_runs (id, parent_run_id, parent_session_id, workspace_root, model_id, status, started_at, kind, workspace_copy) VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6, 'writer', ?7)",
            rusqlite::params![
                "child-orphan-1",
                "parent-orphan",
                "session-orphan",
                root.to_string_lossy(),
                "test-model",
                chrono::Utc::now().to_rfc3339(),
                copy_db.to_string_lossy(),
            ],
        ).unwrap();
        drop(conn);

        assert!(copy_db.exists());
        assert!(copy_disk_only.exists());
        assert!(safe_keep.exists());

        let cleaned = cleanup_orphaned_writer_workspaces(&state);
        assert!(cleaned >= 2, "至少清理 2 处孤儿子目录，实际清理: {}", cleaned);
        assert!(!copy_db.exists(), "DB 中的僵尸目录必须被清理");
        assert!(!copy_disk_only.exists(), "磁盘扫描发现的孤儿目录必须被清理");
        assert!(safe_keep.exists(), "安全业务目录绝不可被误删");

        // 验证 status 被置为 interrupted
        let conn = state.0.lock().unwrap();
        let status: String = conn.query_row(
            "SELECT status FROM agent_child_runs WHERE id = 'child-orphan-1'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(status, "interrupted", "running 状态必须被标记为 interrupted");

        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn test_refresh_file_conflicts_catches_runtime_changes() {
        let (base, root) = workspace();
        let file_a = root.join("a.txt");
        std::fs::write(&file_a, "original content\n").unwrap();

        let spawn_hash = compute_file_hash(&file_a, false).await;
        let changed_file = ChangedFile {
            path: "a.txt".to_string(),
            bytes_before: 10,
            bytes_after: 20,
            diff: String::new(),
            spawn_hash: Some(spawn_hash.clone()),
            result_hash: Some("result_hash_abc".to_string()),
            root_hash: Some(spawn_hash.clone()),
            conflict_status: Some(ConflictStatus::CleanReplay),
        };

        let mut files = vec![changed_file];
        // 1. 父工程内容未变时，现算为 CleanReplay
        refresh_file_conflicts(&mut files, &root, false).await;
        assert_eq!(files[0].conflict_status, Some(ConflictStatus::CleanReplay));

        // 2. 期间父工程被其他步骤修改
        std::fs::write(&file_a, "modified by root or user\n").unwrap();
        refresh_file_conflicts(&mut files, &root, false).await;
        assert_eq!(files[0].conflict_status, Some(ConflictStatus::Conflict), "现算必须检测到中间发生的冲突");

        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn test_execute_writer_action_edit_file() {
        let (base, root) = workspace();
        let file = root.join("src/sum.mjs");

        let mut changes = BTreeMap::new();
        let mut writes_used = 0;
        let mut test_evidence = None;

        // 唯一匹配替换成功
        let p_ok = json!({
            "path": "src/sum.mjs",
            "target_content": "return a - b;",
            "replacement_content": "return a + b;"
        });
        let res = execute_writer_action(
            &root,
            "edit_file",
            &p_ok,
            &mut changes,
            &mut writes_used,
            "npm test",
            &mut test_evidence,
            "",
        )
        .await;

        assert!(res.is_ok(), "edit_file should succeed: {:?}", res);
        assert_eq!(writes_used, 1);
        assert!(changes.contains_key("src/sum.mjs"));

        let updated = std::fs::read_to_string(&file).unwrap();
        assert!(updated.contains("return a + b;"));

        // 未找到报错
        let p_not_found = json!({
            "path": "src/sum.mjs",
            "target_content": "non_existent_code();",
            "replacement_content": "foo();"
        });
        let res_err = execute_writer_action(
            &root,
            "edit_file",
            &p_not_found,
            &mut changes,
            &mut writes_used,
            "npm test",
            &mut test_evidence,
            "",
        )
        .await;
        assert!(res_err.is_err());
        assert!(res_err.unwrap_err().contains("目标文本未找到"));

        let _ = std::fs::remove_dir_all(base);
    }
}
