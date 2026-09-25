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

mod actions;
mod apply;
mod cleanup;
mod fingerprint;
mod workspace;

pub(crate) use actions::*;
pub(crate) use apply::*;
pub(crate) use cleanup::*;
pub(crate) use fingerprint::*;
pub(crate) use workspace::*;
/// L3 隔离写入 worker 预算：比侦察 worker 更宽，但仍受步数/时长/写入量硬顶约束。
pub const MAX_WRITER_STEPS: usize = 14;
/// step_budget 覆盖的宿主硬上限：模型可声称"这活复杂"，但不许无限膨胀。
pub const WRITER_STEP_BUDGET_CAP: usize = 40;
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
    /// 步数预算覆盖：复杂任务派工时可声明更大预算；宿主机械钳位上限，
    /// 缺省走 MAX_WRITER_STEPS（防"预算无限"型滥用）。
    pub step_budget: Option<usize>,
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
    /// 基线时刻文件是否已存在：区分"已有空文件"与"全新创建"——
    /// 决定机械 apply 的语义是覆盖还是新建（新建文件 spawn_hash 记为 "empty/deleted"）
    #[serde(default)]
    pub existed_before: Option<bool>,
}

/// 首次修改前的文件基线：before 内容 + 当时是否已存在。
/// existed_before=false 时 spawn 指纹按缺失文件哨兵 "empty/deleted" 记录，
/// 使三态冲突检测对新建文件给出正确语义（父工作区仍无此文件 → CleanReplay）。
#[derive(Debug, Clone)]
pub struct WriteBaseline {
    pub content: String,
    pub existed_before: bool,
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
    // step_budget：非正整数或非法类型一律按缺省处理（fail-open 到默认预算），
    // 超限钳位到硬上限——复杂度判断是模型的，边界是宿主的。
    let step_budget = params
        .get("step_budget")
        .and_then(|v| v.as_u64())
        .map(|v| (v as usize).min(WRITER_STEP_BUDGET_CAP))
        .filter(|v| *v > 0);
    Ok(WriterRequest {
        task,
        test_command,
        focus_paths,
        context_excerpts,
        hard_scope,
        step_budget,
    })
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

/// 依据修改留底与 Git 快照基线生成 Unified Hunk，并执行 48KB 分级降级与三态冲突检测
async fn build_changed_files(
    changes: &BTreeMap<String, WriteBaseline>,
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
        let _ = crate::agent::utils::subprocess::command_output_with_timeout(
            tokio::process::Command::new("git")
                .arg("add")
                .arg("-A")
                .current_dir(copy_root),
            30,
        )
        .await;

        if let Ok(out) = crate::agent::utils::subprocess::command_output_with_timeout(
            tokio::process::Command::new("git")
                .args(&["diff", "--cached", "--unified=3"])
                .current_dir(copy_root),
            60,
        )
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
        // 权威全量补丁始终登记为受管工件：它是机械 apply 的合入凭据与审计留底；
        // 内联展示仍按 48KB 预算分级截断，但工件本身不截断
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

        if total_chars <= MAX_DIFF_TOTAL_CHARS {
            // Tier 1: 全量 hunk 在 48KB 预算内，逐文件提取
            for rel in changes.keys() {
                if let Some(fd) = extract_file_diff(&git_full_diff, rel) {
                    file_diffs.insert(rel.clone(), fd);
                }
            }
        } else {
            // Tier 2: 超出预算，尝试 --unified=0 降级重算（砍掉上下文行）
            let u0_res = crate::agent::utils::subprocess::command_output_with_timeout(
                tokio::process::Command::new("git")
                    .args(&["diff", "--cached", "--unified=0"])
                    .current_dir(copy_root),
                60,
            )
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
                // Tier 3 & 4: 内联仅保留文件首头，完整补丁经 patch_ref 工件取回
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
    for (relative, baseline) in changes {
        let canonical_copy = copy_root.join(relative);
        let canonical_parent = parent_root.join(relative);

        let after = std::fs::read_to_string(&canonical_copy).unwrap_or_default();
        let diff = if let Some(d) = file_diffs.get(relative) {
            d.clone()
        } else if git_available {
            line_diff(&baseline.content, &after)
        } else {
            format!("⚠️ git 不可用，回执为无行号简化 diff:\n{}", line_diff(&baseline.content, &after))
        };

        // 新建文件的 spawn 指纹记为缺失哨兵，与 compute_file_hash 对不存在文件的返回值一致：
        // 父工作区至今仍无此文件 → CleanReplay；被人抢先建出同名异内容文件 → Conflict
        let spawn_h = if baseline.existed_before {
            compute_str_hash(&baseline.content, git_available, copy_root).await
        } else {
            "empty/deleted".to_string()
        };
        let result_h = compute_file_hash(&canonical_copy, git_available).await;
        let root_now_h = compute_file_hash(&canonical_parent, git_available).await;
        let conflict_status = evaluate_file_conflict(&spawn_h, &result_h, &root_now_h);

        result_files.push(ChangedFile {
            path: relative.clone(),
            bytes_before: baseline.content.len(),
            bytes_after: after.len(),
            diff,
            conflict_status: Some(conflict_status),
            spawn_hash: Some(spawn_h),
            result_hash: Some(result_h),
            root_hash: Some(root_now_h),
            existed_before: Some(baseline.existed_before),
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
    let mut changes: BTreeMap<String, WriteBaseline> = BTreeMap::new();
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
        result.prompt_tokens,
        result
            .error_kind
            .as_deref()
            .map(|s| crate::agent::types::ChildErrorKind::Other(s.to_string())),
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

/// writer 回执的模型可见投影：主循环自动注入与 wait_jobs 手动收取共用同一视图，
/// 避免两条消费路径各写一份格式而漂移。diff 内联已在 build_changed_files 里按
/// 预算分级降级（超限只留首头），完整补丁始终是受管工件经 patch_ref 取回。
pub fn writer_result_model_view(wr: &WriterResult) -> String {
    let mut out = format!("状态: {}, 测试: {}\n", wr.status, wr.test_status);
    if !wr.test_output_tail.is_empty() {
        out.push_str(&format!("测试输出摘要:\n{}\n", wr.test_output_tail));
    }
    out.push_str("修改文件与冲突状态（主工作区实时现算）:\n");
    for cf in &wr.changed_files {
        let status_label = cf.conflict_status.map(|s| s.label()).unwrap_or("未知");
        out.push_str(&format!("- {} [{}]\n", cf.path, status_label));
        if !cf.diff.is_empty() {
            out.push_str(&format!("```diff\n{}\n```\n", cf.diff));
        }
    }
    if let Some(ref pr) = wr.patch_ref {
        out.push_str(&format!("完整补丁工件: 可用 artifact_manager(action: \"get\", id: \"{}\") 查看\n", pr));
    }
    if !wr.findings.is_empty() {
        out.push_str(&format!("修改说明:\n- {}\n", wr.findings.join("\n- ")));
    }
    out
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
    identity: &crate::agent::child_reviewer::ChildRunIdentity,
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
        // 配额语义=并发上限：只统计仍在运行中的工兵，已终态（succeeded/failed/
        // cancelled/interrupted/timed_out）的不得继续占用额度，否则第二个工兵永远派不出去。
        let existing: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_child_runs WHERE parent_run_id = ?1 AND kind = 'writer' AND status = 'running'",
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
        identity,
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
    let step_budget = request.step_budget;
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
        step_budget,
    ));
    Ok(child_run_id)
}

#[cfg(test)]
mod tests;
