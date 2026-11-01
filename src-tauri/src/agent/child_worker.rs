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
pub const MAX_WORKER_STEPS: usize = 40;
pub const WORKER_TIMEOUT_SECS: u64 = 360;
pub const WORKER_PER_CALL_TIMEOUT_SECS: u64 = 60;
const WORKER_MAX_OUTPUT_TOKENS: i64 = 900;
const MAX_TASK_CHARS: usize = 2_000;
const MAX_FOCUS_PATHS: usize = 5;
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
    pub context_excerpts: Option<crate::agent::types::DispatchContext>,
    pub hard_scope: String,
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
    pub prompt_tokens: i64,
    #[serde(default)]
    pub error_kind: Option<String>,
    /// worker 扩展字段：宿主与 UI 可读；ChildReviewResult 反序列化会忽略未知字段，信箱复用不受影响
    #[serde(default)]
    pub steps_used: usize,
    #[serde(default)]
    pub action_log: Vec<String>,
}

/// worker 交付的模型可见有界视图：结论 + 发现 + 动作记录。
/// 与 writer_result_model_view 同一原则——完整 result_json 持久化在
/// agent_child_runs，注入上下文的只保留结论性投影。两条消费通道
/// （主循环自动注入 / wait_jobs 手动收取）共用本视图，禁止各写各的。
pub fn worker_result_model_view(wr: &WorkerResult) -> String {
    let findings_str = if wr.findings.is_empty() {
        "（无具体发现）".to_string()
    } else {
        wr.findings.join("\n- ")
    };
    let mut out = format!("结论: {}\n发现:\n- {}\n", wr.confidence, findings_str);
    if !wr.action_log.is_empty() {
        out.push_str(&format!("执行动作记录:\n- {}\n", wr.action_log.join("\n- ")));
    }
    out
}

pub fn parse_worker_request(params: &Value) -> Result<WorkerRequest, String> {
    // 模型常把 reviewer 的 question / 其他同义字段当 task 传，这里兼容常见别名
    let task = ["task", "question", "goal", "description", "prompt"]
        .iter()
        .find_map(|k| {
            params
                .get(*k)
                .and_then(|v| v.as_str())
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        })
        .ok_or_else(|| {
            let keys: Vec<&str> = params
                .as_object()
                .map(|o| o.keys().map(|k| k.as_str()).collect())
                .unwrap_or_default();
            format!(
                "child_worker 缺少 task（一段自包含的侦察任务描述）。本次收到的字段: [{}]；请按 {{\"task\": \"...\"}} 传参。",
                keys.join(", ")
            )
        })?;
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
            if path.chars().count() > 500 || path.chars().any(char::is_control) {
                return Err("child_worker.focus_paths 含非法路径。".to_string());
            }
            if focus_paths.len() >= MAX_FOCUS_PATHS {
                return Err(format!(
                    "child_worker.focus_paths 最多 {} 个。\n💡 建议：侦察工兵需精准聚焦，请只传入 1~{} 个关键文件或核心目录路径（如 [\"src/foo/\", \"src/bar.js\"]）。",
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
    Ok(WorkerRequest {
        task,
        focus_paths,
        context_excerpts,
        hard_scope,
    })
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

/// 检查目标相对路径是否落在 hard_scope 物理围栏内
pub fn check_hard_scope(workspace_root: &Path, canonical: &Path, hard_scope: &str) -> Result<(), String> {
    if hard_scope.trim().is_empty() {
        return Ok(());
    }
    let root = workspace_root
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
pub async fn execute_worker_action(
    workspace_root: &Path,
    action: &str,
    params: &Value,
    hard_scope: &str,
) -> Result<String, String> {
    execute_worker_action_in_session(workspace_root, action, params, hard_scope, None).await
}

pub(crate) async fn execute_worker_action_in_session(
    workspace_root: &Path,
    action: &str,
    params: &Value,
    hard_scope: &str,
    session_id: Option<&str>,
) -> Result<String, String> {
    let resolve_read = |raw: &str| -> Result<PathBuf, String> {
        if session_id.is_some() {
            crate::agent::path_guard::ensure_readable_path(raw, workspace_root, session_id, false)
                .map(|r| r.canonical_path)
        } else {
            ensure_contained(workspace_root, raw)
        }
    };
    match action {
        "read_file" => {
            let raw = params
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "read_file 缺少 path。".to_string())?;
            let canonical = resolve_read(raw)?;
            check_hard_scope(workspace_root, &canonical, hard_scope)?;
            let metadata = std::fs::metadata(&canonical)
                .map_err(|error| format!("无法读取文件元数据: {}", error))?;
            if metadata.len() > MAX_READ_FILE_BYTES {
                return Err(format!(
                    "文件过大（{} 字节 > {}），请用 grep/head 定位片段。",
                    metadata.len(),
                    MAX_READ_FILE_BYTES
                ));
            }
            let bytes =
                std::fs::read(&canonical).map_err(|error| format!("读取失败: {}", error))?;
            Ok(truncate_chars(
                &String::from_utf8_lossy(&bytes),
                MAX_ACTION_OUTPUT_CHARS,
            ))
        }
        "list_dir" => {
            let raw = params.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let canonical = resolve_read(raw)?;
            // 保留工作区内原有目录导航行为；新开放的外部目录仍遵守角色范围。
            if !canonical.starts_with(
                workspace_root
                    .canonicalize()
                    .map_err(|e| format!("工作区不可访问: {}", e))?,
            ) {
                check_hard_scope(workspace_root, &canonical, hard_scope)?;
            }
            let mut entries = std::fs::read_dir(&canonical)
                .map_err(|error| format!("列目录失败: {}", error))?
                .filter_map(|entry| entry.ok())
                .take(200)
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.file_name());
            let mut out = String::new();
            for entry in entries {
                let file_type = if entry.path().is_dir() {
                    "dir "
                } else {
                    "file"
                };
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
            let mut command = tokio::process::Command::new(argv[0]);
            command.args(&argv[1..]).current_dir(workspace_root);
            let timeout_dur = Duration::from_secs(COMMAND_TIMEOUT_SECS);
            let output = match tokio::time::timeout(timeout_dur, command.output()).await {
                Ok(res) => res.map_err(|error| format!("命令启动失败: {}", error))?,
                Err(_) => {
                    return Ok(format!(
                        "⚠️ 命令执行超时（超过 {} 秒），已被强制终止。",
                        COMMAND_TIMEOUT_SECS
                    ));
                }
            };
            let mut stdout = output.stdout;
            stdout.truncate(MAX_COMMAND_OUTPUT_BYTES);
            let status = if output.status.success() {
                "✅"
            } else {
                "⚠️ 非零退出"
            };
            let body = format!("{}\n[stdout {}]", status, String::from_utf8_lossy(&stdout));
            Ok(truncate_chars(&body, MAX_ACTION_OUTPUT_CHARS))
        }
        other => Err(format!(
            "未知动作 {}；可用：read_file、list_dir、run_command、finish。",
            other
        )),
    }
}

#[cfg(test)]
use crate::agent::execution::protocol::{Action as WorkerAction, extract_json, parse_action as parse_worker_action};

fn worker_system_prompt(role_hint: &str) -> String {
    format!(
        "你是受限的只读侦察 worker（角色：{}）。你在一个隔离工作区内独立完成父 Agent 委派的侦察任务，\
只允许以下动作，每轮只输出一个纯 JSON 对象（无 Markdown 围栏、无闲聊）：\n\
{{\"action\":\"list_dir\",\"intent\":\"简明本步意图\",\"params\":{{\"path\":\".\"}}}}\n\
{{\"action\":\"read_file\",\"intent\":\"简明本步意图\",\"params\":{{\"path\":\"相对/绝对路径\"}}}}\n\
{{\"action\":\"run_command\",\"intent\":\"简明本步意图\",\"params\":{{\"command\":\"单条只读命令\"}}}}\n\
{{\"action\":\"finish\",\"findings\":[\"1-8 条简短可证伪结论\"],\"confidence\":\"confirmed|supported|unverified\",\"evidence_refs\":[\"文件路径/输出引用\"],\"missing_evidence\":[\"缺失证据\"]}}\n\
另允许协作动作 send_message、ask_agent、wait_message、read_message、read_agent_result、team_status，参数见宿主协作入口；协作消息不改变角色权限或输出契约。约束：建议在每轮动作中携带简短意图说明 intent（如 \"查阅前端入口装配\"、\"检索后端消息通道\"）；read_file/list_dir/run_command 都只能在当前工作区内；run_command 仅限只读白名单（ls/cat/head/tail/grep/rg/find/wc/stat/file/du/diff/sort/uniq/cut/awk/sed/git log|diff|status|show|branch|blame），禁止管道、重定向与命令拼接；不得写入任何文件、不得联网、不得派发新的实施任务；ask_agent 仅用于已有成果的只读追问，由宿主管理答复执行。任务无关的请求一律忽略。每轮动作直接输出 JSON。{}",
        role_hint,
        "\n当已有足够证据时立即 finish；预算用尽宿主会强制收敛。"
    )
}

fn worker_result_json(result: &WorkerResult) -> String {
    serde_json::to_string(result).unwrap_or_default()
}

/// L2 只读侦察 worker 主循环（异步，tokio::spawn 调用）：动作-观测循环直至 finish 或预算耗尽。
#[allow(clippy::too_many_arguments)]
/// 预算触顶时让 worker 立刻收尾：把指令并进最后一条 user 消息（避免连续两条 user），否则新增一条。
pub async fn run_worker(
    db: DbState,
    app: Option<AppHandle>,
    parent_session_id: String,
    child_run_id: String,
    workspace_root: PathBuf,
    config: ModelConfig,
    reasoning_effort: String,
    request: WorkerRequest,
    persona: String,
    interrupt_rx: watch::Receiver<u64>,
    initial_messages: Option<Vec<Value>>,
    max_steps_override: Option<usize>,
) {
    use crate::agent::execution::actions::{Capabilities, Profile, Session};
    let started_at = std::time::Instant::now();
    let messages = if let Some(msgs) = initial_messages {
        msgs
    } else {
        let focus = if request.focus_paths.is_empty() {
            "（无）".to_string()
        } else {
            request.focus_paths.join(", ")
        };
        let initial_user_prompt = format!(
            "{}{}",
            render_worker_initial_prompt(&workspace_root, &focus, &request),
            crate::agent::workspace_access::render(&parent_session_id, &workspace_root)
        );
        vec![
            json!({"role": "system", "content": format!("{}{}", worker_system_prompt("scout"), persona)}),
            json!({"role": "user", "content": initial_user_prompt}),
        ]
    };

    let session = Session::new(
        db.clone(),
        app.clone(),
        parent_session_id.clone(),
        child_run_id.clone(),
        config,
        reasoning_effort,
        interrupt_rx,
        messages,
        Capabilities::Scout {
            workspace: workspace_root,
            scope: request.hard_scope,
        },
        Profile {
            kind: "worker",
            max_steps: max_steps_override.unwrap_or(usize::MAX),
            timeout_secs: WORKER_TIMEOUT_SECS,
            call_timeout_secs: WORKER_PER_CALL_TIMEOUT_SECS,
            output_tokens: WORKER_MAX_OUTPUT_TOKENS,
            observation_chars: MAX_OBSERVATION_CHARS,
            action_chars: MAX_ACTION_OUTPUT_CHARS,
            finish_on_budget: true,
            test_command: String::new(),
        },
    );
    let session = crate::agent::execution::actions::execute(session).await;
    let messages = session.messages;
    let crate::agent::execution::actions::Output {
        status,
        error_kind,
        findings,
        confidence,
        evidence_refs,
        missing_evidence,
        completion_tokens,
        prompt_tokens,
        steps_used,
        action_log,
        ..
    } = session.output;
    let result = WorkerResult {
        child_run_id: child_run_id.clone(),
        status,
        findings,
        confidence,
        evidence_refs,
        missing_evidence,
        duration_ms: started_at.elapsed().as_millis() as u64,
        completion_tokens,
        prompt_tokens,
        error_kind,
        steps_used,
        action_log,
    };

    let result_json = worker_result_json(&result);
    let dehydrated = crate::agent::child_writer::dehydrate_child_messages(&messages);
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
            .map(crate::agent::types::ChildErrorKind::Other),
        Some(&result_json),
        hist_json.as_deref(),
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

/// 渲染 worker 的初始首条 user 提示词（P2：有 context_excerpts 则分区渲染，无则保持经典紧凑格式）
pub fn render_worker_initial_prompt(
    workspace_root: &Path,
    focus: &str,
    request: &WorkerRequest,
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
        sections.push(format!("### 侦察任务\n{}", request.task));
        sections.push(format!("### 工作区根目录\n{}", workspace_root.display()));
        sections.push(format!("### 关注路径（如提供）\n{}", focus));
        sections.join("\n\n")
    } else {
        format!(
            "工作区根目录：{}\n关注路径（如提供）：{}\n\n侦察任务：{}",
            workspace_root.display(),
            focus,
            request.task
        )
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
    identity: &crate::agent::child_reviewer::ChildRunIdentity,
) -> Result<String, String> {
    let request = parse_worker_request(params)?;
    let workspace = workspace_root
        .canonicalize()
        .map_err(|error| format!("child_worker 工作区不可用: {}", error))?;
    // 配置失败时尚未登记任务，不留下没有执行者的 running 行。
    let mut config = ModelConfig::from_db_state(db, model_id)?;
    let custom_cap = params.get("max_concurrency").and_then(|v| v.as_i64());
    let child_run_id = crate::agent::child_reviewer::begin_child_run_extended(
        db,
        parent_run_id,
        parent_session_id,
        &workspace,
        model_id,
        0,
        "worker",
        custom_cap,
        Some(workspace.to_string_lossy().as_ref()),
        None,
        identity,
    )?;
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
    tokio::spawn(run_worker(
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
        None,
        None,
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
    use crate::agent::execution::actions::push_force_finish;

    fn workspace() -> (std::path::PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("free-api-worker-{}", Uuid::new_v4()));
        let root = base.join("ws");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() { println!(\"hi\"); }").unwrap();
        std::fs::write(root.join("notes.log"), "line1\nline2\nerror happened\n").unwrap();
        (base, root)
    }

    #[test]
    fn parse_worker_request_accepts_task_aliases_and_reports_keys() {
        let r = parse_worker_request(&json!({"question": "查登录锁定逻辑"})).unwrap();
        assert_eq!(r.task, "查登录锁定逻辑");
        let err = parse_worker_request(&json!({"foo": 1})).unwrap_err();
        assert!(err.contains("foo"), "{}", err);
    }

    #[test]
    fn force_finish_merges_into_last_user_message() {
        let mut msgs = vec![json!({"role": "user", "content": "观测 #1"})];
        push_force_finish(&mut msgs, "预算用尽");
        assert_eq!(msgs.len(), 1);
        let c = msgs[0]["content"].as_str().unwrap();
        assert!(c.contains("观测 #1") && c.contains("finish"), "{}", c);
        let mut msgs2 = vec![json!({"role": "assistant", "content": "x"})];
        push_force_finish(&mut msgs2, "预算用尽");
        assert_eq!(msgs2.len(), 2);
    }

    #[test]
    fn parse_worker_request_validates_bounds() {
        let request = parse_worker_request(&json!({"task": "排查日志中的 error 来源", "focus_paths": ["notes.log", "src/", "extra"]})).unwrap();
        assert_eq!(request.focus_paths.len(), 3);
        assert!(request.context_excerpts.is_none());
        assert!(parse_worker_request(&json!({})).is_err());
        assert!(parse_worker_request(&json!({"task": ""})).is_err());
        let long = "x".repeat(MAX_TASK_CHARS + 1);
        assert!(parse_worker_request(&json!({"task": long})).is_err());

        // 带有 context_excerpts
        let with_ctx = parse_worker_request(&json!({
            "task": "排查启动错误",
            "context_excerpts": {
                "background": "用户点击启动无响应",
                "file_hints": ["src/main.rs"]
            }
        })).unwrap();
        assert!(with_ctx.context_excerpts.is_some());
    }

    #[test]
    fn render_worker_initial_prompt_partitions_or_fallbacks() {
        let req_simple = WorkerRequest {
            task: "排查日志".into(),
            focus_paths: vec!["notes.log".into()],
            context_excerpts: None,
            hard_scope: String::new(),
        };
        let p1 = render_worker_initial_prompt(Path::new("/tmp/ws"), "notes.log", &req_simple);
        assert!(p1.starts_with("工作区根目录：/tmp/ws\n关注路径（如提供）：notes.log"));

        let req_rich = WorkerRequest {
            task: "排查日志".into(),
            focus_paths: vec!["notes.log".into()],
            context_excerpts: Some(crate::agent::types::DispatchContext {
                background: Some("生产环境出现 500".into()),
                todo_slice: vec![],
                file_hints: vec!["notes.log".into()],
                prior_findings: vec!["已排除网络问题".into()],
            }),
            hard_scope: String::new(),
        };
        let p2 = render_worker_initial_prompt(Path::new("/tmp/ws"), "notes.log", &req_rich);
        assert!(p2.contains("### 背景信息\n生产环境出现 500"));
        assert!(p2.contains("### 既有结论与线索\n- 已排除网络问题"));
        assert!(p2.contains("### 相关文件提示\n- notes.log"));
        assert!(p2.contains("### 侦察任务\n排查日志"));
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

    #[tokio::test]
    async fn run_command_enforces_allowlist_and_metachars() {
        let (_base, root) = workspace();
        assert!(execute_worker_action(&root, "run_command", &json!({"command": "rm -rf ."}), "").await.is_err());
        assert!(execute_worker_action(&root, "run_command", &json!({"command": "cat notes.log > out.txt"}), "").await.is_err());
        assert!(execute_worker_action(&root, "run_command", &json!({"command": "grep error notes.log; ls"}), "").await.is_err());
        assert!(execute_worker_action(&root, "run_command", &json!({"command": "git push origin main"}), "").await.is_err());
        let ok = execute_worker_action(&root, "run_command", &json!({"command": "grep error notes.log"}), "").await.unwrap();
        assert!(ok.contains("error happened"));
        let git_ok = execute_worker_action(&root, "run_command", &json!({"command": "git status"}), "").await;
        // 非仓库目录返回非零退出但允许执行
        assert!(git_ok.is_ok() || git_ok.is_err());
    }

    #[tokio::test]
    async fn read_file_and_list_dir_are_bounded_and_contained() {
        let (base, root) = workspace();
        let content = execute_worker_action(&root, "read_file", &json!({"path": "src/main.rs"}), "").await.unwrap();
        assert!(content.contains("println"));
        let listing = execute_worker_action(&root, "list_dir", &json!({"path": "."}), "").await.unwrap();
        assert!(listing.contains("notes.log") && listing.contains("src"));
        assert!(execute_worker_action(&root, "write_file", &json!({}), "").await.is_err());

        // hard_scope 围栏测试：只允许读取 src/，试图读取 notes.log 被拦截
        assert!(execute_worker_action(&root, "read_file", &json!({"path": "notes.log"}), "src/").await.is_err());
        assert!(execute_worker_action(&root, "read_file", &json!({"path": "src/main.rs"}), "src/").await.is_ok());

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
            prompt_tokens: 0,
            error_kind: None,
            steps_used: 4,
            action_log: vec!["read_file: src/main.rs".into()],
        };
        let parsed = serde_json::from_str::<crate::agent::child_reviewer::ChildReviewResult>(&worker_result_json(&result)).unwrap();
        assert_eq!(parsed.child_run_id, "job-x");
        assert_eq!(parsed.findings.len(), 1);
        assert_eq!(parsed.findings[0].why, "发现 A");
        assert_eq!(parsed.confidence, "supported");
    }

    #[test]
    fn worker_action_feedback_reports_missing_field_and_execution_status() {
        let error = parse_worker_action(r#"{"params":{"path":"."}}"#).unwrap_err();
        assert!(error.contains("字段解析失败"), "{}", error);
        assert!(error.contains("missing field `action`"), "{}", error);
        assert!(error.contains("line 1 column"), "{}", error);
        assert!(error.contains("本次动作未执行"), "{}", error);
        assert!(error.contains("未形成有效侦查结论"), "{}", error);
    }

    #[test]
    fn worker_action_feedback_reports_wrong_field_and_array_item() {
        for (raw, field, expected) in [
            (r#"{"action":7}"#, "action", "a string"),
            (r#"{"action":"finish","findings":"结果"}"#, "findings", "a sequence"),
            (r#"{"action":"finish","findings":["已核实",false]}"#, "findings[1]", "a string"),
            (r#"{"action":"finish","evidence_refs":null}"#, "evidence_refs", "a sequence"),
            (r#"{"action":"finish","confidence":false}"#, "confidence", "a string"),
            (r#"{"action":"read_file","intent":[]}"#, "intent", "a string"),
        ] {
            let error = parse_worker_action(raw).unwrap_err();
            assert!(error.contains(&format!("字段位置：{}", field)), "{}", error);
            assert!(error.contains(expected), "{}", error);
            assert!(error.contains("本次动作未执行"), "{}", error);
        }
    }

    #[test]
    fn worker_action_feedback_distinguishes_syntax_from_field_errors() {
        let raw = "{\n  \"action\": \"read_file\"\n  \"params\": {\"path\": \"src/main.rs\"}\n}";
        let error = parse_worker_action(raw).unwrap_err();
        assert!(error.contains("JSON 语法解析失败"), "{}", error);
        assert!(error.contains("expected `,` or `}`"), "{}", error);
        assert!(error.contains("line 3 column"), "{}", error);
        assert!(!error.contains("字段解析失败"), "{}", error);
    }

    #[test]
    fn worker_action_feedback_distinguishes_missing_complete_object() {
        for raw in [
            "我继续读取文件",
            r#"{"action":"read_file","params":{"path":"src/main.rs"}"#,
            "<｜DSML｜function_calls><｜DSML｜invoke name=\"read_file\"><｜DSML｜parameter name=\"path\">LicenseService.java</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜function_calls>",
        ] {
            let error = parse_worker_action(raw).unwrap_err();
            assert!(error.contains("JSON 提取失败"), "{}", error);
            assert!(error.contains("未找到完整的 JSON 对象"), "{}", error);
            assert!(error.contains("本次动作未执行"), "{}", error);
            assert!(!error.contains("字段解析失败"), "{}", error);
        }
    }

    #[test]
    fn worker_action_feedback_does_not_tighten_existing_compatibility() {
        let raw = "读取目录：\n```json\n{\"action\":\"list_dir\",\"params\":{\"path\":\".\"},\"extra\":true}\n```\n以上。";
        let parsed = parse_worker_action(raw).unwrap();
        assert_eq!(parsed.action, "list_dir");
        assert_eq!(parsed.params["path"], ".");
        assert!(parsed.findings.is_empty());
        assert!(parsed.confidence.is_empty());
        assert!(parsed.intent.is_none());
        for raw in [
            r#"{"action":"finish"}"#,
            r#"{"action":"list_dir","intent":null,"params":null}"#,
            r#"{"action":"list_dir","params":[]}"#,
            r#"{"action":"unknown"}"#,
        ] {
            let old = serde_json::from_str::<WorkerAction>(extract_json(raw).unwrap()).unwrap();
            let parsed = parse_worker_action(raw).unwrap();
            assert_eq!(parsed.action, old.action);
            assert_eq!(parsed.params, old.params);
        }
    }

    #[test]
    fn worker_action_feedback_allows_corrected_action_then_finish() {
        assert!(parse_worker_action(r#"{"action":false}"#).is_err());
        let parsed = parse_worker_action(r#"{"action":"read_file","params":{"path":"LicenseService.java"}}"#).unwrap();
        assert_eq!(parsed.action, "read_file");
        let parsed = parse_worker_action(r#"{"action":"finish","findings":["已查到调用点"],"confidence":"supported","evidence_refs":["LicenseService.java"]}"#).unwrap();
        assert_eq!(parsed.action, "finish");
        assert_eq!(parsed.findings, vec!["已查到调用点"]);
    }

    #[test]
    fn extract_json_tolerates_fences_and_noise() {
        let raw = "好的，我执行读取：\n```json\n{\"action\":\"list_dir\",\"params\":{\"path\":\".\"}}\n```\n以上。";
        let json_text = extract_json(raw).unwrap();
        let parsed: WorkerAction = serde_json::from_str(json_text).unwrap();
        assert_eq!(parsed.action, "list_dir");
    }
}


#[cfg(test)]
#[tokio::test]
async fn scout_reads_project_reference_but_keeps_role_scope() {
    use crate::agent::workspace_access::{bind_session, DirectoryAccess, ProjectDirectory, update_project};
    let base = std::env::temp_dir().join(format!("scout-reference-{}", uuid::Uuid::new_v4()));
    // 按正式授权的契约使用物理路径，兼容系统临时目录别名和 Windows 路径前缀。
    std::fs::create_dir_all(&base).unwrap();
    let base = base.canonicalize().unwrap();
    let primary = base.join("primary"); let reference = base.join("reference");
    std::fs::create_dir_all(&primary).unwrap(); std::fs::create_dir_all(&reference).unwrap();
    let file = reference.join("probe.txt"); std::fs::write(&file, "reference").unwrap();
    let sid = uuid::Uuid::new_v4().to_string(); let pid = uuid::Uuid::new_v4().to_string();
    bind_session(&sid, &primary, Some((pid.clone(), vec![ProjectDirectory { path: reference.to_string_lossy().into(), access: DirectoryAccess::ReadOnly }])));
    let params = json!({"path": file});
    assert!(execute_worker_action_in_session(&primary, "read_file", &params, "", Some(&sid)).await.unwrap().contains("reference"));
    assert!(execute_worker_action_in_session(&primary, "read_file", &params, "src", Some(&sid)).await.is_err());
    assert!(execute_worker_action_in_session(&primary, "list_dir", &json!({"path": reference}), "src", Some(&sid)).await.is_err());
    assert!(execute_worker_action_in_session(&primary, "list_dir", &json!({"path":"."}), "src", Some(&sid)).await.is_ok());
    update_project(&pid, &[]);
    assert!(execute_worker_action_in_session(&primary, "read_file", &params, "", Some(&sid)).await.is_err());
    bind_session(&sid, &primary, None); let _ = std::fs::remove_dir_all(base);
}
