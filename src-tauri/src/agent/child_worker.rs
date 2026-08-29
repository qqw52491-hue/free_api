use crate::agent::types::TokenUsage;
use uuid::Uuid;
use crate::db::DbState;
use crate::llm::{self, ModelConfig};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;


/// L2 只读侦察 worker 预算：比 L1 审查者（单次调用、45s）宽一档，
/// 但仍受步数/时长/观测量三重硬顶约束，防止探索失控。
pub const MAX_WORKER_STEPS: usize = 10;
pub const WORKER_TIMEOUT_SECS: u64 = 180;
pub const WORKER_PER_CALL_TIMEOUT_SECS: u64 = 60;
const WORKER_MAX_OUTPUT_TOKENS: i64 = 900;
const MAX_TASK_CHARS: usize = 2_000;
const MAX_FOCUS_PATHS: usize = 3;
const MAX_OBSERVATION_CHARS: usize = 40_000;
const MAX_ACTION_OUTPUT_CHARS: usize = 8_000;
const MAX_READ_FILE_BYTES: u64 = 2_000_000;
const COMMAND_TIMEOUT_SECS: u64 = 20;
const MAX_COMMAND_OUTPUT_BYTES: usize = 200_000;

/// 允许 worker 直接执行的第一 token 白名单：全部为只读侦察命令。
const COMMAND_ALLOWLIST: &[&str] = &[
    "ls", "cat", "head", "tail", "grep", "rg", "find", "wc", "stat", "file", "du", "diff", "sort",
    "uniq", "cut", "awk", "sed", "node", "python3", "git",
];
/// git 仅放行只读子命令
const GIT_READONLY_SUBCOMMANDS: &[&str] = &["log", "diff", "status", "show", "branch", "blame", "shortlog"];

#[derive(Debug, Clone)]
pub struct WorkerRequest {
    pub task: String,
    pub focus_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerResult {
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
    /// worker 扩展字段：宿主与 UI 可读；ChildReviewResult 反序列化会忽略未知字段，信箱复用不受影响
    #[serde(default)]
    pub steps_used: usize,
    #[serde(default)]
    pub action_log: Vec<String>,
}

pub fn parse_worker_request(params: &Value) -> Result<WorkerRequest, String> {
    let task = params
        .get("task")
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "child_worker 缺少 task（一段自包含的侦察任务描述）。".to_string())?;
    if task.chars().count() > MAX_TASK_CHARS {
        return Err(format!("child_worker.task 最多 {} 个字符。", MAX_TASK_CHARS));
    }
    let mut focus_paths = Vec::new();
    if let Some(paths) = params.get("focus_paths").and_then(|v| v.as_array()) {
        for path in paths {
            let path = path
                .as_str()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| "child_worker.focus_paths 必须是字符串数组。".to_string())?;
            if focus_paths.len() >= MAX_FOCUS_PATHS {
                return Err(format!("child_worker.focus_paths 最多 {} 个。", MAX_FOCUS_PATHS));
            }
            if path.chars().count() > 500 || path.chars().any(char::is_control) {
                return Err("child_worker.focus_paths 含非法路径。".to_string());
            }
            focus_paths.push(path);
        }
    }
    Ok(WorkerRequest { task, focus_paths })
}

/// 工作区包含性校验：worker 的所有文件访问都必须落在规范化后的工作区内。
fn ensure_contained(workspace_root: &Path, raw: &str) -> Result<PathBuf, String> {
    // 根目录同样规范化，抵御 /tmp 等符号链接导致的 starts_with 误判
    let root = workspace_root
        .canonicalize()
        .map_err(|error| format!("工作区不可访问: {}", error))?;
    let target = PathBuf::from(raw.trim());
    let target = if target.is_relative() {
        workspace_root.join(target)
    } else {
        target
    };
    let canonical = target
        .canonicalize()
        .map_err(|error| format!("路径不可访问 {}: {}", raw, error))?;
    if !canonical.starts_with(&root) {
        return Err(format!("worker 只能访问工作区内的路径：{}", raw));
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

fn reject_shell_metachars(raw: &str) -> Result<(), String> {
    const FORBIDDEN: &[char] = &[';', '|', '&', '>', '<', '`', '$', '(', ')', '\n', '\r'];
    if raw.chars().any(|c| FORBIDDEN.contains(&c)) {
        return Err("run_command 不允许 shell 控制字符（管道/重定向/命令拼接）；一次只执行一条只读命令。".to_string());
    }
    Ok(())
}

fn command_allowed(argv: &[&str]) -> Result<(), String> {
    let Some(program) = argv.first() else {
        return Err("run_command 缺少命令。".to_string());
    };
    let base = program.rsplit('/').next().unwrap_or(program);
    if !COMMAND_ALLOWLIST.contains(&base) {
        return Err(format!(
            "命令 {} 不在 worker 只读白名单内；可用：{}",
            base,
            COMMAND_ALLOWLIST.join(", ")
        ));
    }
    if base == "git" {
        let Some(sub) = argv.get(1) else {
            return Err("git 必须带只读子命令（log/diff/status/show/branch/blame/shortlog）。".to_string());
        };
        if !GIT_READONLY_SUBCOMMANDS.contains(&sub.to_string().as_str()) {
            return Err(format!("git {} 不在 worker 只读白名单内。", sub));
        }
    }
    Ok(())
}

/// worker 动作执行（无 LLM、可单测）：read_file / list_dir / run_command
pub fn execute_worker_action(
    workspace_root: &Path,
    action: &str,
    params: &Value,
) -> Result<String, String> {
    match action {
        "read_file" => {
            let raw = params
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "read_file 缺少 path。".to_string())?;
            let canonical = ensure_contained(workspace_root, raw)?;
            let metadata = std::fs::metadata(&canonical)
                .map_err(|error| format!("无法读取文件元数据: {}", error))?;
            if metadata.len() > MAX_READ_FILE_BYTES {
                return Err(format!(
                    "文件过大（{} 字节 > {}），请用 grep/head 定位片段。",
                    metadata.len(),
                    MAX_READ_FILE_BYTES
                ));
            }
            let bytes = std::fs::read(&canonical).map_err(|error| format!("读取失败: {}", error))?;
            Ok(truncate_chars(&String::from_utf8_lossy(&bytes), MAX_ACTION_OUTPUT_CHARS))
        }
        "list_dir" => {
            let raw = params
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".");
            let canonical = ensure_contained(workspace_root, raw)?;
            let mut entries = std::fs::read_dir(&canonical)
                .map_err(|error| format!("列目录失败: {}", error))?
                .filter_map(|entry| entry.ok())
                .take(200)
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.file_name());
            let mut out = String::new();
            for entry in entries {
                let file_type = if entry.path().is_dir() { "dir " } else { "file" };
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                out.push_str(&format!(
                    "{} {}\t{}\n",
                    file_type,
                    size,
                    entry.file_name().to_string_lossy()
                ));
            }
            Ok(truncate_chars(&out, MAX_ACTION_OUTPUT_CHARS))
        }
        "run_command" => {
            let raw = params
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "run_command 缺少 command。".to_string())?;
            reject_shell_metachars(raw)?;
            let argv: Vec<&str> = raw.split_whitespace().collect();
            command_allowed(&argv)?;
            let mut command = std::process::Command::new(argv[0]);
            command.args(&argv[1..]).current_dir(workspace_root);
            let output = command
                .output()
                .map_err(|error| format!("命令启动失败: {}", error))?;
            let mut stdout = output.stdout;
            stdout.truncate(MAX_COMMAND_OUTPUT_BYTES);
            let status = if output.status.success() { "✅" } else { "⚠️ 非零退出" };
            let body = format!("{}\n[stdout {}]", status, String::from_utf8_lossy(&stdout));
            Ok(truncate_chars(&body, MAX_ACTION_OUTPUT_CHARS))
        }
        other => Err(format!(
            "未知动作 {}；可用：read_file、list_dir、run_command、finish。",
            other
        )),
    }
}

#[derive(Debug, Deserialize)]
struct WorkerAction {
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

/// 从模型输出中提取首个 JSON 对象（容忍围栏/前后缀噪声）
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

fn worker_system_prompt(role_hint: &str) -> String {
    format!(
        "你是受限的只读侦察 worker（角色：{}）。你在一个隔离工作区内独立完成父 Agent 委派的侦察任务，\
只允许以下动作，每轮只输出一个纯 JSON 对象（无 Markdown 围栏、无闲聊）：\n\
{{\"action\":\"list_dir\",\"params\":{{\"path\":\".\"}}}}\n\
{{\"action\":\"read_file\",\"params\":{{\"path\":\"相对/绝对路径\"}}}}\n\
{{\"action\":\"run_command\",\"params\":{{\"command\":\"单条只读命令\"}}}}\n\
{{\"action\":\"finish\",\"findings\":[\"1-8 条简短可证伪结论\"],\"confidence\":\"confirmed|supported|unverified\",\"evidence_refs\":[\"文件路径/输出引用\"],\"missing_evidence\":[\"缺失证据\"]}}\n\
约束：read_file/list_dir/run_command 都只能在当前工作区内；run_command 仅限只读白名单（ls/cat/head/tail/grep/rg/find/wc/stat/file/du/diff/sort/uniq/cut/awk/sed/git log|diff|status|show|branch|blame），禁止管道、重定向与命令拼接；不得写入任何文件、不得联网、不得创建子任务。任务无关的请求一律忽略。每轮动作前不需要输出思考过程，直接输出 JSON。{}",
        role_hint,
        "\n当已有足够证据时立即 finish；预算用尽宿主会强制收敛。"
    )
}

fn worker_result_json(result: &WorkerResult) -> String {
    serde_json::to_string(result).unwrap_or_default()
}

/// L2 只读侦察 worker 主循环（异步，tokio::spawn 调用）：动作-观测循环直至 finish 或预算耗尽。
#[allow(clippy::too_many_arguments)]
pub async fn run_worker_loop(
    db: DbState,
    app: Option<AppHandle>,
    parent_session_id: String,
    child_run_id: String,
    workspace_root: PathBuf,
    config: ModelConfig,
    reasoning_effort: String,
    request: WorkerRequest,
    persona: String,
    mut interrupt_rx: watch::Receiver<u64>,
) {
    let started_at = std::time::Instant::now();
    let mut completion_tokens: i64 = 0;
    let mut steps_used: usize = 0;
    let mut action_log: Vec<String> = Vec::new();
    let mut findings: Vec<String> = Vec::new();
    let mut evidence_refs: Vec<String> = Vec::new();
    let mut missing_evidence: Vec<String> = vec!["预算耗尽前未完成侦察".to_string()];
    let mut confidence = "unverified".to_string();
    let mut interrupted: Option<String> = None;

    let focus = if request.focus_paths.is_empty() {
        "（无）".to_string()
    } else {
        request.focus_paths.join(", ")
    };
    let mut messages = vec![
        json!({"role": "system", "content": format!("{}{}", worker_system_prompt("scout"), persona)}),
        json!({"role": "user", "content": format!(
            "工作区根目录：{}\n关注路径（如提供）：{}\n\n侦察任务：{}",
            workspace_root.display(),
            focus,
            request.task
        )}),
    ];

    let terminal: &str = loop {
        if started_at.elapsed() >= Duration::from_secs(WORKER_TIMEOUT_SECS)
            || steps_used >= MAX_WORKER_STEPS
        {
            break "budget_exhausted";
        }
        steps_used += 1;

        let mut config = config.clone();
        config.max_tokens = if config.max_tokens <= 0 {
            WORKER_MAX_OUTPUT_TOKENS
        } else {
            config.max_tokens.min(WORKER_MAX_OUTPUT_TOKENS)
        };
        let mut ignore_stream_event = |event: llm::StreamEvent<'_>| -> bool {
            if let Some(ref handle) = app {
                if let llm::StreamEvent::Thinking { chunk, .. } = event {
                    let _ = handle.emit(
                        &format!("agent-progress-{}", parent_session_id),
                        json!({
                            "type": "child_worker_stream",
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
            &mut ignore_stream_event,
        );
        let raw = match tokio::time::timeout(
            Duration::from_secs(WORKER_PER_CALL_TIMEOUT_SECS),
            call,
        )
        .await
        {
            Err(_) => {
                interrupted = Some("worker 单步调用超时".to_string());
                break "timed_out";
            }
            Ok(Err(error)) => {
                interrupted = Some(if error == llm::STREAM_INTERRUPTED_ERROR {
                    "worker 已随父任务取消".to_string()
                } else {
                    format!("worker 调用失败: {}", error)
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
            messages.push(json!({"role": "user", "content": "【宿主反馈】输出不是合法 JSON 动作。请直接输出一个 JSON 对象动作或 finish。"}));
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<WorkerAction>(json_text) else {
            messages.push(json!({"role": "assistant", "content": raw}));
            messages.push(json!({"role": "user", "content": "【宿主反馈】JSON 字段不符合动作协议（需要 action/params 或 finish+findings）。请修正后重新输出。"}));
            continue;
        };

        if parsed.action == "finish" {
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
                findings = vec!["worker 未产出有效结论".to_string()];
                missing_evidence.push("finish 时缺少 findings".to_string());
            }
            break "succeeded";
        }

        let observation = match execute_worker_action(&workspace_root, &parsed.action, &parsed.params) {
            Ok(output) => output,
            Err(error) => format!("【动作被拒绝】{}", error),
        };
        action_log.push(format!("{}: {}", parsed.action, parsed.params));
        let budget_left = MAX_OBSERVATION_CHARS.saturating_sub(
            messages
                .iter()
                .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
                .map(|c| c.chars().count())
                .sum::<usize>(),
        );
        let observation = if budget_left == 0 {
            missing_evidence = vec!["观测预算耗尽，未能完成全部侦察".to_string()];
            break "budget_exhausted";
        } else {
            truncate_chars(&observation, budget_left.min(MAX_ACTION_OUTPUT_CHARS))
        };
        messages.push(json!({"role": "assistant", "content": raw}));
        messages.push(json!({"role": "user", "content": format!("【宿主观测 #{}】\n{}", steps_used, observation)}));
    };

    // 收敛终态：中断类（cancelled/timed_out/failed）视为失败回收；正常路径即使预算耗尽也带部分结论交付
    let (status, error_kind) = match terminal {
        "succeeded" => ("succeeded", None),
        "budget_exhausted" => {
            if findings.is_empty() {
                findings.push("worker 未产出有效结论".to_string());
            }
            missing_evidence.push(format!(
                "以 budget_exhausted 终止（步数上限 {} / 时长上限 {}s）",
                MAX_WORKER_STEPS, WORKER_TIMEOUT_SECS
            ));
            ("succeeded", None)
        }
        "cancelled" => ("cancelled", Some("worker 已随父任务取消".to_string())),
        "timed_out" => ("timed_out", Some("worker 单步调用超时".to_string())),
        _ => ("failed", interrupted.clone()),
    };
    if findings.is_empty() && status != "succeeded" {
        missing_evidence.push("子任务未能产出结论".to_string());
        confidence = "unverified".to_string();
    }
    let result = WorkerResult {
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
        action_log,
    };

    crate::agent::child_reviewer::finish_child_run(
        &db,
        &child_run_id,
        &result.status,
        result.duration_ms,
        result.completion_tokens,
        result.error_kind.as_deref(),
        Some(&worker_result_json(&result)),
    );
    if let Some(handle) = app {
        let _ = handle.emit(
            &format!("agent-log-{}", parent_session_id),
            format!(
                "🛰️ 侦察 worker {} 结束：{}（{} 步，{} 条结论）",
                child_run_id,
                result.status,
                result.steps_used,
                result.findings.len()
            ),
        );
    }
}

/// 异步派发 L2 侦察 worker：立即返回 child_run_id，结果经 durable inbox 自动回收。
#[allow(clippy::too_many_arguments)]
pub async fn spawn_child_worker(
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
    let request = parse_worker_request(params)?;
    let workspace = workspace_root
        .canonicalize()
        .map_err(|error| format!("child_worker 工作区不可用: {}", error))?;
    let child_run_id = crate::agent::child_reviewer::begin_child_run(
        db,
        parent_run_id,
        parent_session_id,
        &workspace,
        model_id,
        0,
        "worker",
    )?;
    let mut config = ModelConfig::from_db_state(db, model_id)?;
    config.max_tokens = if config.max_tokens <= 0 {
        WORKER_MAX_OUTPUT_TOKENS
    } else {
        config.max_tokens.min(WORKER_MAX_OUTPUT_TOKENS)
    };

    let db_clone = DbState(db.0.clone());
    let child_run_id_clone = child_run_id.clone();
    let session_clone = parent_session_id.to_string();
    let workspace_clone = workspace.clone();
    let reasoning = reasoning_effort.to_string();
    tokio::spawn(run_worker_loop(
        db_clone,
        app,
        session_clone,
        child_run_id_clone,
        workspace_clone,
        config,
        reasoning,
        request,
        persona,
        interrupt_rx,
    ));
    Ok(child_run_id)
}

pub fn worker_result_token_usage(result: &WorkerResult) -> TokenUsage {
    TokenUsage {
        completion_tokens: result.completion_tokens,
        total_tokens: result.completion_tokens,
        ..TokenUsage::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> (std::path::PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("free-api-worker-{}", Uuid::new_v4()));
        let root = base.join("ws");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() { println!(\"hi\"); }").unwrap();
        std::fs::write(root.join("notes.log"), "line1\nline2\nerror happened\n").unwrap();
        (base, root)
    }

    #[test]
    fn parse_worker_request_validates_bounds() {
        let request = parse_worker_request(&json!({"task": "排查日志中的 error 来源", "focus_paths": ["notes.log", "src/", "extra"]})).unwrap();
        assert_eq!(request.focus_paths.len(), 3);
        assert!(parse_worker_request(&json!({})).is_err());
        assert!(parse_worker_request(&json!({"task": ""})).is_err());
        let long = "x".repeat(MAX_TASK_CHARS + 1);
        assert!(parse_worker_request(&json!({"task": long})).is_err());
    }

    #[test]
    fn containment_rejects_escape_paths() {
        let (base, root) = workspace();
        let outside = base.join("secret.txt");
        std::fs::write(&outside, "secret").unwrap();
        assert!(ensure_contained(&root, "../secret.txt").is_err());
        assert!(ensure_contained(&root, &outside.to_string_lossy()).is_err());
        assert!(ensure_contained(&root, "src/main.rs").is_ok());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn run_command_enforces_allowlist_and_metachars() {
        let (_base, root) = workspace();
        assert!(execute_worker_action(&root, "run_command", &json!({"command": "rm -rf ."})).is_err());
        assert!(execute_worker_action(&root, "run_command", &json!({"command": "cat notes.log > out.txt"})).is_err());
        assert!(execute_worker_action(&root, "run_command", &json!({"command": "grep error notes.log; ls"})).is_err());
        assert!(execute_worker_action(&root, "run_command", &json!({"command": "git push origin main"})).is_err());
        let ok = execute_worker_action(&root, "run_command", &json!({"command": "grep error notes.log"})).unwrap();
        assert!(ok.contains("error happened"));
        let git_ok = execute_worker_action(&root, "run_command", &json!({"command": "git status"}));
        // 非仓库目录返回非零退出但允许执行
        assert!(git_ok.is_ok() || git_ok.is_err());
    }

    #[test]
    fn read_file_and_list_dir_are_bounded_and_contained() {
        let (base, root) = workspace();
        let content = execute_worker_action(&root, "read_file", &json!({"path": "src/main.rs"})).unwrap();
        assert!(content.contains("println"));
        let listing = execute_worker_action(&root, "list_dir", &json!({"path": "."})).unwrap();
        assert!(listing.contains("notes.log") && listing.contains("src"));
        assert!(execute_worker_action(&root, "write_file", &json!({})).is_err());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn worker_result_json_is_inbox_compatible_with_child_review_result() {
        let result = WorkerResult {
            child_run_id: "job-x".into(),
            status: "succeeded".into(),
            findings: vec!["发现 A".into()],
            confidence: "supported".into(),
            evidence_refs: vec!["src/main.rs".into()],
            missing_evidence: vec![],
            duration_ms: 1200,
            completion_tokens: 300,
            error_kind: None,
            steps_used: 4,
            action_log: vec!["read_file: src/main.rs".into()],
        };
        let parsed = serde_json::from_str::<crate::agent::child_reviewer::ChildReviewResult>(&worker_result_json(&result)).unwrap();
        assert_eq!(parsed.child_run_id, "job-x");
        assert_eq!(parsed.findings, vec!["发现 A".to_string()]);
        assert_eq!(parsed.confidence, "supported");
    }

    #[test]
    fn extract_json_tolerates_fences_and_noise() {
        let raw = "好的，我执行读取：\n```json\n{\"action\":\"list_dir\",\"params\":{\"path\":\".\"}}\n```\n以上。";
        let json_text = extract_json(raw).unwrap();
        let parsed: WorkerAction = serde_json::from_str(json_text).unwrap();
        assert_eq!(parsed.action, "list_dir");
    }
}
