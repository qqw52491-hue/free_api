use crate::db::DbState;
use crate::llm::{self, ModelConfig};
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub bytes_before: usize,
    pub bytes_after: usize,
    #[serde(default)]
    pub diff: String,
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
                return Err(format!("child_writer.focus_paths 最多 {} 个。", MAX_FOCUS_PATHS));
            }
            focus_paths.push(path);
        }
    }
    Ok(WriterRequest {
        task,
        test_command,
        focus_paths,
    })
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
{\"action\":\"write_file\",\"params\":{\"path\":\"路径\",\"content\":\"完整新文件内容\"}}\n\
{\"action\":\"run_command\",\"params\":{\"command\":\"单条命令\"}}\n\
{\"action\":\"finish\",\"findings\":[\"1-8 条结论\"],\"confidence\":\"confirmed|supported|unverified\",\"evidence_refs\":[\"文件/测试引用\"],\"missing_evidence\":[\"...\"]}\n\
纪律：修改完成后必须用宿主指定的 test_command 验证且通过后才能 finish；write_file 必须写完整文件内容；禁止碰副本外的任何路径。"
}

/// 执行 writer 动作（同步、可单测）。返回 (观测输出, 是否测试命令)
pub fn execute_writer_action(
    copy_root: &Path,
    action: &str,
    params: &Value,
    changes: &mut BTreeMap<String, String>,
    writes_used: &mut usize,
    test_command: &str,
    test_evidence: &mut Option<(String, bool, String)>,
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
        "run_command" => {
            let raw = params
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "run_command 缺少 command。".to_string())?;
            reject_shell_metachars(raw)?;
            let argv: Vec<&str> = raw.split_whitespace().collect();
            command_allowed(&argv)?;
            let output = std::process::Command::new(argv[0])
                .args(&argv[1..])
                .current_dir(copy_root)
                .output()
                .map_err(|error| format!("命令启动失败: {}", error))?;
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
            "未知动作 {}；可用：list_dir、read_file、write_file、run_command、finish。",
            other
        )),
    }
}

/// 依据修改留底生成受限总 diff
fn build_changed_files(changes: &BTreeMap<String, String>, copy_root: &Path) -> Vec<ChangedFile> {
    let mut total = 0usize;
    changes
        .iter()
        .filter_map(|(relative, before)| {
            let after = std::fs::read_to_string(copy_root.join(relative)).unwrap_or_default();
            let diff = line_diff(before, &after);
            total += diff.chars().count();
            if total > MAX_DIFF_TOTAL_CHARS {
                return Some(ChangedFile {
                    path: relative.clone(),
                    bytes_before: before.len(),
                    bytes_after: after.len(),
                    diff: "（总 diff 预算耗尽，未内联）".to_string(),
                });
            }
            Some(ChangedFile {
                path: relative.clone(),
                bytes_before: before.len(),
                bytes_after: after.len(),
                diff,
            })
        })
        .collect()
}

/// L3 隔离写入 worker 主循环（异步，tokio::spawn 调用）
#[allow(clippy::too_many_arguments)]
pub async fn run_writer_loop(
    db: DbState,
    app: Option<AppHandle>,
    parent_session_id: String,
    child_run_id: String,
    copy_root: PathBuf,
    config: ModelConfig,
    reasoning_effort: String,
    request: WriterRequest,
    mut interrupt_rx: watch::Receiver<u64>,
) {
    let started_at = std::time::Instant::now();
    let mut completion_tokens: i64 = 0;
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

    let focus = if request.focus_paths.is_empty() {
        "（无）".to_string()
    } else {
        request.focus_paths.join(", ")
    };
    let mut messages = vec![
        json!({"role": "system", "content": writer_system_prompt()}),
        json!({"role": "user", "content": format!(
            "副本工作区根目录：{}\n关注路径：{}\n必须执行的测试命令：{}\n\n修改任务：{}",
            copy_root.display(),
            focus,
            request.test_command,
            request.task
        )}),
    ];

    let terminal: &str = loop {
        if started_at.elapsed() >= Duration::from_secs(WRITER_TIMEOUT_SECS)
            || steps_used >= MAX_WRITER_STEPS
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
        let raw = match tokio::time::timeout(
            Duration::from_secs(60),
            call,
        )
        .await
        {
            Err(_) => {
                interrupted = Some("writer 单步调用超时".to_string());
                break "timed_out";
            }
            Ok(Err(error)) => {
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
                completion_tokens += result
                    .usage
                    .map(|usage| usage.completion_tokens)
                    .unwrap_or_default();
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
                steps_used -= 1;
                messages.push(json!({"role": "assistant", "content": raw}));
                messages.push(json!({"role": "user", "content": format!(
                    "【宿主拦截】你还没有运行测试命令 \"{}\"。修改未经测试验证不得交付：请先执行它，再 finish。",
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
        );
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
                MAX_WRITER_STEPS, WRITER_TIMEOUT_SECS
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
    let changed_files = build_changed_files(&changes, &copy_root);
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
        error_kind,
        steps_used,
        changed_files,
        test_command: request.test_command.clone(),
        test_status,
        test_output_tail: test_tail,
        workspace_copy: copy_root.display().to_string(),
    };

    crate::agent::child_reviewer::finish_child_run(
        &db,
        &child_run_id,
        &result.status,
        result.duration_ms,
        result.completion_tokens,
        result.error_kind.as_deref(),
        Some(&serde_json::to_string(&result).unwrap_or_default()),
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
    interrupt_rx: watch::Receiver<u64>,
) -> Result<String, String> {
    let request = parse_writer_request(params)?;
    let parent = workspace_root
        .canonicalize()
        .map_err(|error| format!("child_writer 工作区不可用: {}", error))?;
    {
        let conn = db.0.lock().map_err(|error| error.to_string())?;
        let existing: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_child_runs WHERE parent_run_id=?1 AND kind='writer'",
                rusqlite::params![parent_run_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if existing >= MAX_WRITERS_PER_RUN as i64 {
            return Err(format!(
                "每个父运行最多 {} 个写入 worker；请先回收现有 worker 的 diff 再由你自己落实修改。",
                MAX_WRITERS_PER_RUN
            ));
        }
    }

    let child_run_id = crate::agent::child_reviewer::begin_child_run(
        db,
        parent_run_id,
        parent_session_id,
        &parent,
        model_id,
        0,
        "writer",
    )?;
    let child8 = child_run_id.get(..8).unwrap_or(&child_run_id).to_string();
    let copy_root = parent.join(format!(".writer-{}", child8));
    let (files, bytes) = create_isolated_copy(&parent, &copy_root)?;

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
        copy_root.clone(),
        config,
        reasoning,
        request,
        interrupt_rx,
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
        assert!(parse_writer_request(&json!({"task": "修复 sum"})).is_err());
        assert!(parse_writer_request(&json!({"task": "x", "test_command": "a; b"})).is_err());
        assert!(parse_writer_request(&json!({"task": "x", "test_command": "npm test", "focus_paths": ["1","2","3","4","5","6"]})).is_err());
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

    #[test]
    fn write_file_records_before_snapshot_and_refuses_escape() {
        let (base, root) = workspace();
        let copy = base.join(".writer-copy2");
        create_isolated_copy(&root, &copy).unwrap();
        let mut changes = BTreeMap::new();
        let mut writes = 0usize;
        let mut test_evidence: Option<(String, bool, String)> = None;

        // 越界写入被拒绝
        assert!(execute_writer_action(&copy, "write_file", &json!({
            "path": "../evil.txt", "content": "boom"
        }), &mut changes, &mut writes, "npm test", &mut test_evidence).is_err());

        // 正常写入：留底 + 落盘
        let out = execute_writer_action(&copy, "write_file", &json!({
            "path": "src/sum.mjs", "content": "export function sum(a, b) { return a + b; }\n"
        }), &mut changes, &mut writes, "npm test", &mut test_evidence).unwrap();
        assert!(out.contains("已写入 src/sum.mjs"));
        assert_eq!(writes, 1);
        let before = changes.get("src/sum.mjs").unwrap();
        assert!(before.contains("return a - b;"), "首次写入前必须留底原内容");
        // 新建文件：留底为空
        execute_writer_action(&copy, "write_file", &json!({
            "path": "src/new-helper.mjs", "content": "export const ok = true;\n"
        }), &mut changes, &mut writes, "npm test", &mut test_evidence).unwrap();
        assert!(changes.contains_key("src/new-helper.mjs"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn run_command_tracks_test_evidence_and_blocks_non_allowlisted() {
        let (base, root) = workspace();
        let copy = base.join(".writer-copy3");
        create_isolated_copy(&root, &copy).unwrap();
        let mut changes = BTreeMap::new();
        let mut writes = 0usize;
        let mut test_evidence: Option<(String, bool, String)> = None;

        assert!(execute_writer_action(&copy, "run_command", &json!({"command": "rm -rf ."}), &mut changes, &mut writes, "npm test", &mut test_evidence).is_err());
        assert!(execute_writer_action(&copy, "run_command", &json!({"command": "npm test > out.log"}), &mut changes, &mut writes, "npm test", &mut test_evidence).is_err());

        execute_writer_action(&copy, "run_command", &json!({"command": "node --test"}), &mut changes, &mut writes, "node --test", &mut test_evidence).unwrap();
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
}
