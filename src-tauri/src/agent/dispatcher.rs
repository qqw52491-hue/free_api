use crate::agent::mcp::PluginRegistry;
use crate::agent::mcp_catalog::invalidate_plugin;
use crate::agent::plan_tree_dispatch::{
    run_plan_tree_sync, validate_controlled_parallel_batch, MAX_CONTROLLED_PARALLEL_BRANCHES,
};
use crate::agent::retrieval;
use crate::agent::task_store;
use crate::agent::types::{AgentInstruction, DispatchResult};
use crate::agent::utils::{run_osascript, run_shell};
use crate::agent::web_search;
use crate::db::DbState;
use chrono::Utc;
use reqwest::Url;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

/// `finish` 是 Agent 协议的终态动作，不是本地工具或 MCP 工具。
/// 统一判定可防止某个调用路径把已完成任务派发为未知 action。
pub fn is_finish_instruction(instruction: &AgentInstruction) -> bool {
    instruction.get_tool().eq_ignore_ascii_case("finish")
        || instruction.get_action().eq_ignore_ascii_case("finish")
}

/// MCP 服务总名本身不是可调用工具。调用必须指定 `服务/具体工具`，或以服务名配合不同的具体 action。
/// 该检查只拒绝无效形态，不从参数中猜测或迁移任何工具、cwd、path 或 project。
fn mcp_service_requires_concrete_tool(tool: &str, action: &str) -> bool {
    !tool.contains('/')
        && (action.trim().is_empty() || tool.trim().eq_ignore_ascii_case(action.trim()))
}

fn has_read_only_flag(inst: &AgentInstruction) -> bool {
    let check_val = |v: &serde_json::Value| -> bool {
        if let Some(obj) = v.as_object() {
            if obj.get("read_only").and_then(|b| b.as_bool()) == Some(true)
                || obj.get("readonly").and_then(|b| b.as_bool()) == Some(true)
            {
                return true;
            }
            if let Some(meta) = obj.get("metadata").or_else(|| obj.get("meta")) {
                if meta.get("read_only").and_then(|b| b.as_bool()) == Some(true)
                    || meta.get("readonly").and_then(|b| b.as_bool()) == Some(true)
                {
                    return true;
                }
            }
        }
        false
    };

    if let Some(cmd) = &inst.command {
        if check_val(cmd) {
            return true;
        }
    }
    if let Some(params) = &inst.params {
        if check_val(params) {
            return true;
        }
    }
    if let Some(val) = inst.extra_fields.get("read_only") {
        if val.as_bool() == Some(true) {
            return true;
        }
    }
    if let Some(val) = inst.extra_fields.get("readonly") {
        if val.as_bool() == Some(true) {
            return true;
        }
    }
    if let Some(meta) = inst.extra_fields.get("metadata").or_else(|| inst.extra_fields.get("meta")) {
        if check_val(meta) {
            return true;
        }
    }
    false
}

pub fn extract_shell_command_string(inst: &AgentInstruction) -> Option<String> {
    let parse_from_value = |val: &serde_json::Value| -> Option<String> {
        // 1. 纯字符串形态: "curl -s ..."
        if let Some(s) = val.as_str() {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }

        // 2. JSON Object 形态: 常见字段与 args 列表
        if let Some(obj) = val.as_object() {
            // 2.1 完整命令行字段优先
            for key in &["script", "command_line", "CommandLine"] {
                if let Some(s) = obj.get(*key).and_then(|v| v.as_str()) {
                    let trimmed = s.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    }
                }
            }

            // 2.2 提取 command / cmd / input 字段
            let cmd_val = obj.get("command").or_else(|| obj.get("cmd")).or_else(|| obj.get("input"));
            let cmd_str = cmd_val.and_then(|v| v.as_str());

            // 提取 args 字段（字符串数组）
            let args_vec: Option<Vec<&str>> = obj.get("args").and_then(|v| {
                v.as_array().map(|arr| {
                    arr.iter().filter_map(|item| item.as_str()).collect()
                })
            });

            if let Some(cmd) = cmd_str {
                let cmd_trimmed = cmd.trim();
                let base = cmd_trimmed.rsplit('/').next().unwrap_or(cmd_trimmed);

                // 特殊处理解释器包装: bash / sh / zsh（形如 {"command": "bash", "args": ["-c", "date; curl ..."]}）
                if base == "bash" || base == "sh" || base == "zsh" {
                    if let Some(ref args) = args_vec {
                        // 寻找 -c 及其后面的实际脚本参数
                        for (i, arg) in args.iter().enumerate() {
                            if *arg == "-c" {
                                if i + 1 < args.len() {
                                    return Some(args[i + 1].trim().to_string());
                                }
                            } else if arg.starts_with("-c ") {
                                return Some(arg[3..].trim().to_string());
                            } else if arg.starts_with("-c=") {
                                return Some(arg[3..].trim().to_string());
                            }
                        }
                    }
                }

                // 如果包含 args，将其与 cmd 拼接为完整命令行以供全链条白名单核验
                if let Some(ref args) = args_vec {
                    if !args.is_empty() {
                        let mut full = cmd_trimmed.to_string();
                        for arg in args {
                            full.push(' ');
                            full.push_str(arg);
                        }
                        return Some(full);
                    }
                }

                return Some(cmd_trimmed.to_string());
            }

            // 如果没有 command 字段，但 args 提供了完整命令列表
            if let Some(ref args) = args_vec {
                if !args.is_empty() {
                    return Some(args.join(" "));
                }
            }
        }
        None
    };

    if let Some(ref cmd) = inst.command {
        if let Some(res) = parse_from_value(cmd) {
            return Some(res);
        }
    }
    if let Some(ref params) = inst.params {
        if let Some(res) = parse_from_value(params) {
            return Some(res);
        }
    }
    None
}

/// 拆分复合 shell 命令为各个独立子段（尊重单双引号内的分号与管道）
pub fn split_shell_segments(cmd: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut current_start = 0;
    let bytes = cmd.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while i < len {
        let b = bytes[i];
        if b == b'\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
        } else if b == b'"' && !in_single_quote {
            in_double_quote = !in_double_quote;
        } else if !in_single_quote && !in_double_quote {
            if b == b';' || b == b'|' {
                if i > current_start {
                    segments.push(&cmd[current_start..i]);
                }
                current_start = i + 1;
            } else if b == b'&' && i + 1 < len && bytes[i + 1] == b'&' {
                if i > current_start {
                    segments.push(&cmd[current_start..i]);
                }
                i += 1;
                current_start = i + 1;
            }
        }
        i += 1;
    }
    if current_start < len {
        segments.push(&cmd[current_start..]);
    }
    segments
}

/// 核验单条子命令是否命中纯只读白名单
fn is_single_segment_safe(seg: &str) -> bool {
    let words: Vec<&str> = seg.split_whitespace().collect();
    if words.is_empty() {
        return true;
    }
    let first = words[0].to_ascii_lowercase();
    let base_cmd = first.rsplit('/').next().unwrap_or(&first);

    match base_cmd {
        "curl" | "wget" => {
            // 严禁写文件、上传数据或非 GET/HEAD 动作
            let mut idx = 1;
            while idx < words.len() {
                let word = words[idx];
                let lower = word.to_ascii_lowercase();

                // 1. 写文件类参数: -o, -O, --output, --output=..., --remote-name
                if lower == "-o" || lower == "-o" || lower.starts_with("-o=") || lower == "--output" || lower.starts_with("--output=") || lower == "--remote-name" {
                    // 特例: -o - 表示输出到 stdout (纯只读打印)
                    if lower == "-o" && idx + 1 < words.len() && words[idx + 1] == "-" {
                        idx += 2;
                        continue;
                    }
                    return false;
                }
                // 2. 非 GET 请求方法: -X POST, -X PUT, -X DELETE, -X PATCH, --request ...
                if lower == "-x" || lower == "--request" {
                    if idx + 1 < words.len() {
                        let method = words[idx + 1].to_ascii_uppercase();
                        if method != "GET" && method != "HEAD" {
                            return false;
                        }
                        idx += 2;
                        continue;
                    }
                }
                if lower.starts_with("-xpost") || lower.starts_with("-xput") || lower.starts_with("-xdelete") || lower.starts_with("-xpatch") {
                    return false;
                }
                // 3. 上传数据: -d, --data, --json, -F, --form, -T, --upload-file
                if lower == "-d" || lower.starts_with("-d=") || lower.starts_with("-d@") || lower == "--data" || lower.starts_with("--data") || lower == "--json" {
                    return false;
                }
                if word == "-F" || lower.starts_with("-f=") || lower == "--form" || lower.starts_with("--form=") {
                    return false;
                }
                if word == "-T" || lower == "--upload-file" {
                    return false;
                }

                idx += 1;
            }
            true
        }
        "git" => {
            if words.len() < 2 {
                return true; // 单纯 git 或 git --help
            }
            let sub = words[1].to_ascii_lowercase();
            matches!(
                sub.as_str(),
                "status" | "diff" | "log" | "show" | "branch" | "tag" | "rev-parse" | "ls-files" | "remote" | "describe" | "check-ignore" | "grep" | "--version" | "-v" | "help"
            )
        }
        "ls" | "cat" | "head" | "tail" | "grep" | "rg" | "wc" | "pwd" | "which" | "whereis" | "whoami" | "date" | "echo" | "diff" | "cmp" | "sort" | "uniq" | "file" | "stat" | "uname" | "uptime" | "id" | "tree" | "ping" | "nslookup" | "dig" => {
            true
        }
        "node" | "python" | "python3" | "cargo" | "rustc" | "npm" => {
            // 仅允许版本查询类只读探针
            if words.len() >= 2 {
                let second = words[1].to_ascii_lowercase();
                second == "-v" || second == "--version" || second == "version"
            } else {
                false
            }
        }
        _ => false,
    }
}

/// 严格分析 Shell 命令行是否为纯只读安全探针。
/// 必须满足：
/// 1. 不包含任何文件写入重定向（> 或 >>，排除 2>&1 正常错误输出合并）；
/// 2. 不包含任何危险管道注入（| bash）、提权、删除修改操作；
/// 3. 切分后的每一个子命令必须 100% 命中只读白名单。
pub fn is_safe_read_only_shell_command(raw_cmd: &str) -> bool {
    let trimmed = raw_cmd.trim();
    if trimmed.is_empty() {
        return false;
    }

    // 1. 危险模式一票否决：检查写盘重定向（允许 2>&1 和 1>&2）
    let chars: Vec<char> = trimmed.chars().collect();
    let len = chars.len();
    for i in 0..len {
        if chars[i] == '>' {
            let prev = if i > 0 { chars[i - 1] } else { ' ' };
            let next = if i + 1 < len { chars[i + 1] } else { ' ' };
            let is_fd_dup = (prev == '1' || prev == '2') && next == '&';
            if !is_fd_dup {
                return false; // 文件输出重定向，视为写盘
            }
        }
    }

    // 检查危险管道/注入/提权关键词
    let lower = trimmed.to_ascii_lowercase();
    let dangerous_tokens = [
        "| bash", "| sh", "| zsh", "| python", "| node", "| perl", "| ruby", "| tee",
        "sudo ", "chmod ", "chown ", "rm ", "rmdir ", "touch ", "mkdir ", "mv ",
        "sed -i", "eval ", "exec ", "dd ", "mkfs", "kill ", "pkill ", "reboot", "shutdown",
        "git commit", "git push", "git reset", "git checkout", "git clean", "git rebase", "git merge",
        "npm install", "npm i ", "npm run build", "cargo build", "cargo install", "yarn add", "pnpm add",
        "pip install", "pip uninstall",
    ];
    for token in &dangerous_tokens {
        if lower.contains(token) {
            return false;
        }
    }

    // 2. 切分各级命令链（支持 ; && || |）
    let segments = split_shell_segments(trimmed);
    if segments.is_empty() {
        return false;
    }

    for seg in segments {
        let seg_trimmed = seg.trim();
        if seg_trimmed.is_empty() {
            continue;
        }
        if !is_single_segment_safe(seg_trimmed) {
            return false;
        }
    }

    true
}

pub fn is_write_or_side_effect_tool(inst: &AgentInstruction) -> bool {
    // 显式声明只读时优先放行：即使工具名命中写动词（如 preview_saved_logs / run_query with read_only: true），
    // 只要参数或元数据中明确声明 read_only: true，一律按只读工具放行。
    if has_read_only_flag(inst) {
        return false;
    }

    // MCP 工具常以 "服务名/具体工具" 形式出现（如 remote-hub/shell_exec）。
    // 分类前统一剥离命名空间，否则前缀判定会被命名空间遮蔽（remote-hub/read_file 匹配不到 read_ 前缀）。
    let raw_tool = inst.get_tool().to_ascii_lowercase();
    let tool = raw_tool.rsplit('/').next().unwrap_or(&raw_tool);
    let action = inst.get_action().to_ascii_lowercase();

    // 🌟 全链条只读 Shell 嗅探：如果指令提取出了具体命令行且经白名单核验确认 100% 只读（如 curl 查网络、git status/diff、ls/cat 等无副作用探针），直接按只读放行，绝不误触修改代码的审查门禁！
    if let Some(cmd_str) = extract_shell_command_string(inst) {
        if is_safe_read_only_shell_command(&cmd_str) {
            return false;
        }
    }

    // 写动词优先于只读前缀白名单：
    // 形如 get_shell_output / list_running_jobs 的"取结果"类工具，虽然以只读前缀开头，
    // 但本质是脚本执行类动作，必须按写操作处理，否则会被只读前缀误判放行。
    let has_write_verb = tool.contains("excel")
        || tool.contains("write")
        || tool.contains("edit")
        || tool.contains("shell")
        || tool.contains("bash")
        || tool.contains("exec")
        || tool.contains("create")
        || tool.contains("delete")
        || tool.contains("save")
        || tool.contains("run")
        || action.starts_with("write")
        || action.starts_with("edit")
        || action.starts_with("create")
        || action.starts_with("delete")
        || action.starts_with("save")
        || action.starts_with("exec")
        || action.starts_with("run");

    if has_write_verb {
        return true;
    }

    // task_store 的变更型动作（不带 create/update 前缀时由 action.contains 兜底识别）
    if tool == "task_store"
        && (action.contains("create") || action.contains("upsert") || action.contains("update"))
    {
        return true;
    }

    // 其余一律视为只读：包括 web_fetch / web_search / ripgrep_search、
    // read_* / get_* / list_* / search_* 系列，以及未登记的未知工具（保守按非写处理）。
    false
}

/// 面向 AgentPanel 的轻量工具生命周期事件。只发送参数和输出摘要；完整大对象
/// 仍由 task_store / 文件系统持久化，不能被事件流或模型上下文重复承载。
#[derive(Debug, Clone, Serialize)]
pub struct AgentToolEvent {
    pub event_type: String,
    pub step_id: usize,
    pub description: String,
    pub tool: String,
    pub action: String,
    pub route: Option<String>,
    pub success: Option<bool>,
    pub params_summary: String,
    pub result_summary: String,
    pub duration_ms: Option<u128>,
    pub timestamp: String,
}

pub fn event_summary(value: &str, limit: usize) -> String {
    let normalized = value.trim();
    if normalized.chars().count() <= limit {
        normalized.to_string()
    } else {
        format!("{}…", normalized.chars().take(limit).collect::<String>())
    }
}

pub fn emit_tool_event(
    app: &AppHandle,
    session_id: &str,
    step_id: usize,
    inst: &AgentInstruction,
    event_type: &str,
    route: Option<String>,
    success: Option<bool>,
    result: &str,
    duration_ms: Option<u128>,
) {
    let payload = AgentToolEvent {
        event_type: event_type.to_string(),
        step_id,
        description: inst.description.clone(),
        tool: inst.get_tool(),
        action: inst.get_action(),
        route,
        success,
        params_summary: event_summary(&inst.get_params().to_string(), 8000),
        result_summary: event_summary(result, 16000),
        duration_ms,
        timestamp: Utc::now().to_rfc3339(),
    };
    let _ = app.emit(&format!("agent-tool-event-{}", session_id), payload);
}

/// 🌟 智能规范化 MCP 参数（自动解包嵌套层级，自动映射 path/filePath 等常见别名）
const MAX_CODE_READ_LINES: u64 = 240;

fn guard_code_read_range(
    tool_name: &str,
    params: serde_json::Value,
) -> (serde_json::Value, Option<String>) {
    let lower_tool = tool_name.to_ascii_lowercase();
    let reads_text = matches!(
        lower_tool.as_str(),
        "read_text_file" | "read_file" | "get_file_content" | "get_code_snippet"
    );
    let serde_json::Value::Object(mut obj) = params else {
        return (params, None);
    };
    if !reads_text || !obj.contains_key("path") {
        return (serde_json::Value::Object(obj), None);
    }

    let requested_start = obj
        .get("startLine")
        .or_else(|| obj.get("start_line"))
        .or_else(|| obj.get("offset"))
        .and_then(|value| value.as_u64());
    let requested_end = obj
        .get("endLine")
        .or_else(|| obj.get("end_line"))
        .and_then(|value| value.as_u64())
        .or_else(|| {
            obj.get("limit").and_then(|lim| {
                let s = requested_start.unwrap_or(1).max(1);
                lim.as_u64().map(|l| s.saturating_add(l).saturating_sub(1))
            })
        });

    let start_line = requested_start.unwrap_or(1).max(1);
    let max_end = start_line.saturating_add(MAX_CODE_READ_LINES - 1);
    let end_line = requested_end.unwrap_or(max_end).min(max_end);
    let constrained = requested_start.is_none()
        || requested_end.is_none()
        || requested_end.map(|end| end > max_end).unwrap_or(true);

    obj.insert("startLine".to_string(), json!(start_line));
    obj.insert("endLine".to_string(), json!(end_line));
    if obj.contains_key("start_line") {
        obj.insert("start_line".to_string(), json!(start_line));
    }
    if obj.contains_key("end_line") {
        obj.insert("end_line".to_string(), json!(end_line));
    }

    if !constrained {
        return (serde_json::Value::Object(obj), None);
    }

    let path = obj
        .get("path")
        .and_then(|value| value.as_str())
        .unwrap_or("<unknown>");
    let notice = format!(
        "[代码阅读护栏] {} 的请求已限定为第 {}–{} 行（最多 {} 行）。这是正常预算边界而非工具故障；如需更多证据，请按函数/符号、错误码或 diff hunk 选择相邻范围继续读取，而非重复全文请求。",
        path, start_line, end_line, MAX_CODE_READ_LINES
    );
    (serde_json::Value::Object(obj), Some(notice))
}

fn is_project_session(db: &DbState, session_id: &str) -> bool {
    let Ok(conn) = db.0.lock() else {
        return false;
    };
    crate::commands::projects::project_for_session(&conn, session_id)
        .ok()
        .flatten()
        .is_some()
}

fn dispatch_project_resources(
    db: &DbState,
    session_id: &str,
    action: &str,
    params: &serde_json::Value,
) -> DispatchResult {
    if !is_project_session(db, session_id) {
        return DispatchResult {
            stdout: String::new(),
            stderr: "当前会话未关联项目，不能访问项目受管资源。".to_string(),
            success: false,
            route: "host_rejection:project_resources_without_project".to_string(),
        };
    }
    let action = if action.trim().is_empty() {
        params
            .get("action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("list_resources")
    } else {
        action
    };
    let conn = match db.0.lock() {
        Ok(conn) => conn,
        Err(error) => {
            return DispatchResult {
                stdout: String::new(),
                stderr: format!("项目资源账本不可用: {}", error),
                success: false,
                route: "internal:project_resources".to_string(),
            }
        }
    };
    match action {
        "list_resources" => {
            match crate::commands::projects::project_resources_for_session(&conn, session_id) {
                Ok(resources) => {
                    let projection = resources
                        .into_iter()
                        .map(|resource| {
                            json!({
                                "resource_id": resource.resource_id,
                                "label": resource.label,
                                "relative_path": resource.relative_path,
                                "kind": resource.kind,
                                "size_bytes": resource.size_bytes,
                            })
                        })
                        .collect::<Vec<_>>();
                    DispatchResult {
                        stdout: json!({"resources": projection}).to_string(),
                        stderr: String::new(),
                        success: true,
                        route: "internal:project_resources/list".to_string(),
                    }
                }
                Err(error) => DispatchResult {
                    stdout: String::new(),
                    stderr: error,
                    success: false,
                    route: "internal:project_resources/list".to_string(),
                },
            }
        }
        "read_resource" => {
            let Some(resource_id) = params
                .get("resource_id")
                .and_then(serde_json::Value::as_str)
            else {
                return DispatchResult {
                    stdout: String::new(),
                    stderr:
                        "project_resources.read_resource 缺少 resource_id；请先 list_resources。"
                            .to_string(),
                    success: false,
                    route: "host_rejection:project_resource_missing_id".to_string(),
                };
            };
            match crate::commands::projects::read_project_resource_for_session(
                &conn,
                session_id,
                resource_id,
            ) {
                Ok(preview) => DispatchResult {
                    stdout: json!({
                        "resource": {
                            "resource_id": preview.resource.resource_id,
                            "label": preview.resource.label,
                            "relative_path": preview.resource.relative_path,
                            "kind": preview.resource.kind,
                            "size_bytes": preview.resource.size_bytes,
                        },
                        "content": preview.content,
                        "truncated": preview.truncated,
                    })
                    .to_string(),
                    stderr: String::new(),
                    success: true,
                    route: "internal:project_resources/read".to_string(),
                },
                Err(error) => DispatchResult {
                    stdout: String::new(),
                    stderr: error,
                    success: false,
                    route: "host_rejection:project_resource_read".to_string(),
                },
            }
        }
        "register_resource" | "add_resource" | "register" => {
            let raw_path = params
                .get("path")
                .or_else(|| params.get("relative_path"))
                .or_else(|| params.get("file_path"))
                .or_else(|| params.get("canonical_path"))
                .and_then(serde_json::Value::as_str);
            let Some(raw_path) = raw_path else {
                return DispatchResult {
                    stdout: String::new(),
                    stderr: "project_resources.register_resource 缺少 path 参数（文件相对或绝对路径）。".to_string(),
                    success: false,
                    route: "host_rejection:project_resource_missing_path".to_string(),
                };
            };
            let label = params.get("label").and_then(serde_json::Value::as_str);
            match crate::commands::projects::add_project_resource_for_session(
                &conn,
                session_id,
                raw_path,
                label,
            ) {
                Ok(resource) => DispatchResult {
                    stdout: json!({
                        "message": "成功将文件登记为项目受管资源，同项目下的所有会话均可按 resource_id 读取。",
                        "resource": {
                            "resource_id": resource.resource_id,
                            "label": resource.label,
                            "relative_path": resource.relative_path,
                            "kind": resource.kind,
                            "size_bytes": resource.size_bytes,
                        }
                    }).to_string(),
                    stderr: String::new(),
                    success: true,
                    route: "internal:project_resources/register".to_string(),
                },
                Err(error) => DispatchResult {
                    stdout: String::new(),
                    stderr: error,
                    success: false,
                    route: "host_rejection:project_resource_register_failed".to_string(),
                },
            }
        }
        _ => DispatchResult {
            stdout: String::new(),
            stderr:
                "project_resources 支持 list_resources()、read_resource(resource_id: string) 与 register_resource(path: string, label?: string)。"
                    .to_string(),
            success: false,
            route: "host_rejection:project_resource_unknown_action".to_string(),
        },
    }
}

/// 执行单条指令的内部路由逻辑（4大核心原生工具 + MCP 插件路由，彻底杜绝历史兼容包袱）
pub(crate) fn execute_command_inner(
    db: &DbState,
    session_id: &str,
    tool: &str,
    action: &str,
    params: &serde_json::Value,
    registry: &PluginRegistry,
    workspace_root: Option<&std::path::Path>,
) -> DispatchResult {
    // 0. 宿主层安全防护：
    // 项目专属会话禁止切换工作区（锁定在项目根目录，防止越界串扰）
    let lower_tool = tool.to_ascii_lowercase();
    let lower_action = action.to_ascii_lowercase();
    if lower_tool.contains("switch_workspace")
        || lower_tool.contains("switchworkspace")
        || lower_tool.contains("set_workspace")
        || lower_tool.contains("change_workspace")
        || lower_action.contains("switch_workspace")
        || lower_action.contains("switchworkspace")
        || lower_action.contains("set_workspace")
    {
        if is_project_session(db, session_id) {
            return DispatchResult {
                stdout: String::new(),
                stderr: "❌ [宿主安全保护] 当前为专属项目会话，工作区已锁定在项目根目录，禁止越界切换。如需操作外部项目，请在通用临时会话中操作。".to_string(),
                success: false,
                route: "host_rejection:project_session_workspace_locked".to_string(),
            };
        }
        // 通用临时会话：放行给 MCP 插件进行工作区切换！
    }
    // 1. 核心原生工具 A: get_step_output (精准取回截断历史原文)
    if tool.eq_ignore_ascii_case("get_step_output") {
        return task_store::dispatch(db, session_id, "get_step_output", params);
    }

    // 2. 核心原生工具 B: task_store (会话任务数据区结构化操作)
    if tool.eq_ignore_ascii_case("task_store") {
        let final_action = if action.is_empty() {
            params
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("list_resources")
        } else {
            action
        };
        return task_store::dispatch(db, session_id, final_action, params);
    }

    // 2.5 项目受管资源：服务端按 session→project→resource 三重边界复核。
    if tool.eq_ignore_ascii_case("project_resources")
        || tool.eq_ignore_ascii_case("register_project_resource")
    {
        let final_action = if tool.eq_ignore_ascii_case("register_project_resource") {
            "register_resource"
        } else {
            action
        };
        return dispatch_project_resources(db, session_id, final_action, params);
    }

    // 3. 核心原生工具 C: search (跨执行日志 FTS5 与实时代码 ripgrep 联邦检索)
    if tool.eq_ignore_ascii_case("search") {
        return retrieval::dispatch_search(db, session_id, params, workspace_root);
    }

    // 3.5 核心原生工具 D: web_search / web_fetch (互联网全网检索与网页正文 Markdown 提取)
    if tool.eq_ignore_ascii_case("web_search") || tool.eq_ignore_ascii_case("web_fetch") {
        return web_search::dispatch(tool, params);
    }

    // 3.6 核心原生工具 E: model_manager (自主管理模型规格与渠道绑定，支持增删改查与安全确认)
    if tool.eq_ignore_ascii_case("model_manager") {
        return crate::agent::tools::model_manager::dispatch(db, action, params);
    }

    // 3.7 核心原生工具 F: mcp_manager (MCP 外部插件接入、批量导入与能力卡维护)
    if tool.eq_ignore_ascii_case("mcp_manager") {
        let act = action.to_ascii_lowercase();
        // 变更类危险动作（add_http, delete, toggle, quick_connect）必须经过用户明确确认
        if act.contains("add") || act.contains("delete") || act.contains("toggle") || act.contains("connect") {
            let user_approved = params
                .get("user_confirmed")
                .and_then(|v| v.as_bool())
                .or_else(|| params.get("confirm").and_then(|v| v.as_bool()))
                .unwrap_or(false);
            if !user_approved {
                return DispatchResult {
                    stdout: String::new(),
                    stderr: "❌ [权限安全拦截] 严禁在执行代码或排错任务中擅自调用 mcp_manager 添加、修改或重连 MCP 插件！\n💡 关键提醒：当前已连接的 MCP 工具（如 remote-hub 下的 read_text_file, edit_file, ripgrep_search 等）已经全部就绪且立即可用！请直接以 {\"tool\": \"remote-hub/具体工具名\", \"command\": {...}} 格式调用，绝不需要也不允许调用 mcp_manager 或派发空步骤去‘加载’或‘重连’！".to_string(),
                    success: false,
                    route: "mcp_manager:permission_denied".to_string(),
                };
            }
        }
        return crate::agent::tools::mcp_manager::dispatch(db, action, params);
    }

    // 3.8 核心原生工具 G: skill_manager (工作区自定义技能与提示词库维护)
    if tool.eq_ignore_ascii_case("skill_manager") {
        return crate::agent::tools::skill_manager::dispatch(db, action, params);
    }
    // 3.9 核心原生工具 H: role_manager (专精角色池维护——角色卡=人设+模型绑定+作用域)
    if tool.eq_ignore_ascii_case("role_manager") {
        return crate::agent::tools::role_manager::dispatch(db, action, params);
    }

    // 3.9 核心原生工具 H: schedule_manager (定时任务管理——对话内直接建计划)
    if tool.eq_ignore_ascii_case("schedule_manager") {
        return crate::agent::tools::schedule_manager::dispatch(db, action, params);
    }

    // 3.10 核心原生工具 I: project_manager (项目工作区管理)
    if tool.eq_ignore_ascii_case("project_manager") {
        return crate::agent::tools::project_manager::dispatch(db, action, params);
    }

    // 3.11 核心原生工具 J: system_info (本机运行状态)
    if tool.eq_ignore_ascii_case("system_info") {
        return crate::agent::tools::system_info::dispatch(db, action, params);
    }

    // 3.12 核心原生工具 K: wechat_manager (微信通道状态与主动知会)
    if tool.eq_ignore_ascii_case("wechat_manager") {
        return crate::agent::tools::wechat_manager::dispatch(db, action, params);
    }

    // 3.9 核心原生工具 H: artifact_manager (受管产物物理写入、元数据登记与产物账本管理)
    if tool.eq_ignore_ascii_case("artifact_manager") || tool.eq_ignore_ascii_case("artifact_store")
    {
        return crate::agent::artifact_manager::dispatch(db, session_id, action, params);
    }

    // 4. 外部 MCP 插件工具 (支持 "插件名/工具名" 标准格式 或 "插件名" + action)
    let (plugin_name, tool_name) = if tool.contains('/') {
        let parts: Vec<&str> = tool.split('/').collect();
        (parts[0].to_string(), parts[1].to_string())
    } else {
        (tool.to_string(), action.to_string())
    };

    if let Some(plugin) = registry.get(&plugin_name) {
        if mcp_service_requires_concrete_tool(tool, action) {
            return DispatchResult {
                stdout: String::new(),
                stderr: format!(
                    "❌ [工具契约] MCP 服务‘{}’是服务名称而非具体工具。\n💡 正确调用示范: {{\"tool\": \"{}/具体工具名\", \"command\": {{...}}}}（例如: \"{}/read_text_file\" 或 \"{}/ripgrep_search\"）。请勿遗漏斜杠与子工具名。",
                    plugin_name, plugin_name, plugin_name, plugin_name
                ),
                success: false,
                route: "host_rejection:mcp_service_requires_subtool".to_string(),
            };
        }
        // MCP 子工具参数必须逐字按模型提供的契约透传；宿主不得补全、别名映射或截断。
        let clean_params = params.clone();
        println!(
            "执行动作调用外部插件: {}/{} (会话: {}, 参数: {:?})",
            plugin_name, tool_name, session_id, clean_params
        );
        let call_res = if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.block_on(plugin.call_tool_for_session(
                Some(session_id),
                &tool_name,
                clean_params,
            ))
        } else {
            futures::executor::block_on(plugin.call_tool_for_session(
                Some(session_id),
                &tool_name,
                clean_params,
            ))
        };
        match call_res {
            Ok(out) => {
                let is_error = out
                    .get("isError")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let content = out
                    .get("content")
                    .and_then(|v| v.as_array())
                    .and_then(|arr| arr.get(0))
                    .and_then(|m| m.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if is_error {
                    return DispatchResult {
                        stdout: String::new(),
                        stderr: content,
                        success: false,
                        route: format!("plugin:{}/{}", plugin_name, tool_name),
                    };
                } else {
                    return DispatchResult {
                        stdout: content,
                        stderr: String::new(),
                        success: true,
                        route: format!("plugin:{}/{}", plugin_name, tool_name),
                    };
                }
            }
            Err(e) => {
                return DispatchResult {
                    stdout: String::new(),
                    stderr: format!("插件调用异常: {:?}", e),
                    success: false,
                    route: format!("plugin:{}/{}", plugin_name, tool_name),
                };
            }
        }
    }

    // 5. 针对将 parallel 误写为独立工具的精准纠错
    if tool == "parallel" || action == "parallel" {
        return DispatchResult {
            stdout: String::new(),
            stderr: "❌ 'parallel' 不是独立工具名，而是顶层并行执行模式字段。\n正确格式示例：{\"plan_title\": \"...\", \"parallel_reason\": \"...\", \"parallel\": [{\"tool\": \"工具1\", \"command\": {...}}, {\"tool\": \"工具2\", \"command\": {...}}]}。\n请重新按标准 JSON 格式输出！".to_string(),
            success: false,
            route: "unknown".to_string(),
        };
    }

    // 6. 未知工具或格式错误报错（动态生成当前所有已连接 MCP 插件及子工具清单，协助模型无硬编码纠错）
    let mut mcp_tool_hints = Vec::new();
    for (p_name, p_client) in &registry.clients {
        let tools = crate::agent::mcp_catalog::cached_tools(&p_client.config)
            .unwrap_or_else(|| p_client.last_known_tools());
        let tool_names = tools
            .iter()
            .map(|t| format!("{}/{}", p_name, t.name))
            .collect::<Vec<_>>();
        if !tool_names.is_empty() {
            mcp_tool_hints.push(format!("- {}: {}", p_name, tool_names.join(", ")));
        }
    }

    let mcp_guide = if mcp_tool_hints.is_empty() {
        "（当前未连接任何 MCP 插件）".to_string()
    } else {
        format!(
            "\n当前已连接的 MCP 插件及可用工具：\n{}",
            mcp_tool_hints.join("\n")
        )
    };

    let builtin_list = crate::agent::prompt_assembly::build_builtin_tools_compact_list();
    DispatchResult {
        stdout: String::new(),
        stderr: format!(
            "❌ 未知工具 tool='{}' (action='{}')。请检查拼写！\n可用原生内置工具：{}；{}\n外部 MCP 工具调用格式为：{{\"tool\": \"插件名/工具名\", \"command\": {{...}}}}。",
            tool,
            action,
            builtin_list,
            mcp_guide
        ),
        success: false,
        route: "unknown".to_string(),
    }
}

pub fn run_agent_step(
    db: &DbState,
    session_id: &str,
    instruction: &AgentInstruction,
    registry: &PluginRegistry,
    workspace_root: Option<&std::path::Path>,
) -> DispatchResult {
    let tool_name = instruction.get_tool().to_lowercase();
    if is_finish_instruction(instruction) {
        return DispatchResult {
            stdout: instruction
                .command
                .as_ref()
                .and_then(|value| value.get("summary").or_else(|| value.get("details")))
                .and_then(|value| value.as_str())
                .unwrap_or("任务已完成")
                .to_string(),
            stderr: String::new(),
            success: true,
            route: "finish".to_string(),
        };
    }

    // --- 场景 A0: 树形/并行计划树 PlanTree Execution ---
    if let Some(ref par_cmds) = instruction.parallel {
        if let Err(error) = validate_controlled_parallel_batch(par_cmds) {
            return DispatchResult {
                stdout: String::new(),
                stderr: error,
                success: false,
                route: "parallel_contract_rejected".to_string(),
            };
        }
        println!(
            "🌳 [PlanTree: {}] 检测到 parallel 并行指令，启动 {} 路并发执行...",
            session_id,
            par_cmds.len()
        );
        let plan_node =
            crate::agent::plan_tree::PlanNode::Obj(crate::agent::plan_tree::PlanNodeObj {
                parallel: Some(
                    par_cmds
                        .iter()
                        .map(|c| {
                            serde_json::from_value(c.clone()).unwrap_or_else(|_| {
                                crate::agent::plan_tree::PlanNode::Obj(
                                    serde_json::from_value(c.clone()).unwrap_or_default(),
                                )
                            })
                        })
                        .collect(),
                ),
                ..Default::default()
            });
        return run_plan_tree_sync(db, session_id, &plan_node, registry);
    }
    if let Some(ref tree_val) = instruction.plan_tree {
        if let Ok(plan_node) =
            serde_json::from_value::<crate::agent::plan_tree::PlanNode>(tree_val.clone())
        {
            println!(
                "🌳 [PlanTree: {}] 检测到 plan_tree 复合树形计划，启动树形解释器...",
                session_id
            );
            return run_plan_tree_sync(db, session_id, &plan_node, registry);
        }
    }

    // --- 核心修复：兼容 root.commands 和 root.command.commands ---
    let mut batch_commands = instruction.commands.clone();
    if batch_commands.is_empty() {
        if let Some(cmd_val) = &instruction.command {
            if let Some(cmds) = cmd_val.get("commands").and_then(|v| v.as_array()) {
                batch_commands = cmds.clone();
            }
        }
    }

    // --- 场景 A: 组合指令流水线 Batch Execution ---
    if !batch_commands.is_empty() {
        println!(
            "🚀 [Pipeline: {}] 正在按顺序执行 {} 个组合动作...",
            session_id,
            batch_commands.len()
        );
        let mut total_stdout = String::new();

        for (idx, cmd_val) in batch_commands.iter().enumerate() {
            let step_idx = idx + 1;
            // 提取单步的 tool 与 action (优先使用子命令自带的 tool，缺省退回根 tool_name)
            let step_tool = cmd_val
                .get("tool")
                .and_then(|v| v.as_str())
                .unwrap_or(&tool_name);
            let step_action = cmd_val
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();

            println!(
                "   ➡️  Step {}: 执行 [{}] 动作 '{}'...",
                step_idx, step_tool, step_action
            );

            // 执行单步。params 只透传子步骤的 command 载荷：cmd_val 顶层的 tool/action
            // 是本层路由字段，原样转发会被下游 MCP 的参数契约拒绝（「tool」不是声明字段）。
            let step_params = cmd_val.get("command").cloned().unwrap_or_else(|| cmd_val.clone());
            let res =
                execute_command_inner(db, session_id, step_tool, &step_action, &step_params, registry, workspace_root);

            // 拼接输出结果
            if !res.stdout.is_empty() {
                total_stdout.push_str(&format!("\n[Step {} Result]: {}", step_idx, res.stdout));
            }

            // --- 核心修复：Fail-Fast 快速失败机制 ---
            if !res.success {
                println!(
                    "   🚫 Step {} 执行失败，流水线立即熔断！原因: {}",
                    step_idx, res.stderr
                );
                return DispatchResult {
                    stdout: total_stdout,
                    stderr: format!("流水线在第 {} 步崩溃: {}", step_idx, res.stderr),
                    success: false,
                    route: format!("pipeline_fail_at_{}", step_idx),
                };
            }
        }

        return DispatchResult {
            stdout: total_stdout,
            stderr: String::new(),
            success: true,
            route: "pipeline_success".to_string(),
        };
    }

    // --- 场景 B: 传统单步指令 Single Execution ---
    let mut action = instruction.get_action().trim().to_lowercase();

    // --- 幻觉纠偏：如果 action 为空但 extra_fields 里有 tool_name，尝试打捞 ---
    if action.is_empty() {
        if let Some(name) = instruction
            .extra_fields
            .get("tool_name")
            .and_then(|v| v.as_str())
        {
            action = name.to_lowercase();
        }
    }

    let params = instruction.get_params();
    execute_command_inner(db, session_id, &tool_name, &action, &params, registry, workspace_root)
}

#[tauri::command]
pub async fn dispatch_agent_step(
    instruction_json: String,
    state: State<'_, DbState>,
    registry_state: State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    session_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let instruction: AgentInstruction =
        serde_json::from_str(&instruction_json).map_err(|e| e.to_string())?;

    // --- Strict JSON Schema Mutual Exclusivity Check ---
    let mut modes = 0;
    if instruction.tool.is_some() || instruction.action.is_some() {
        modes += 1;
    }
    if instruction.parallel.is_some() {
        modes += 1;
    }
    if instruction.plan_tree.is_some() {
        modes += 1;
    }
    if !instruction.commands.is_empty() {
        modes += 1;
    }

    if modes > 1 {
        return Ok(json!({
            "route": "schema_validation_failed",
            "success": false,
            "stdout": "",
            "stderr": "请选择单一步 (tool/action)、parallel 并行数组 或 plan_tree 计划树之一，严禁在同一 JSON 中混用！"
        }));
    }

    let registry_arc = registry_state.inner().clone();
    let db = state.inner().clone();
    let sid = session_id.unwrap_or_else(|| "default".to_string());

    let registry = {
        let guard = registry_arc.lock().await;
        (*guard).clone()
    };

    let result =
        tokio::task::spawn_blocking(move || run_agent_step(&db, &sid, &instruction, &registry, None))
            .await
            .map_err(|e| e.to_string())?;

    Ok(
        json!({ "route": result.route, "success": result.success, "stdout": result.stdout, "stderr": result.stderr }),
    )
}

#[tauri::command]
pub async fn execute_command(tool: String, command: String) -> Result<serde_json::Value, String> {
    let (stdout, stderr, success) = if tool == "osascript" {
        run_osascript(&command)
    } else {
        run_shell(&command)
    };
    Ok(json!({ "success": success, "stdout": stdout.trim(), "stderr": stderr.trim() }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{init_db, DbState};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    #[test]
    fn mcp_service_name_requires_an_explicit_subtool_without_parameter_inference() {
        assert!(mcp_service_requires_concrete_tool("remote-http-mcp", ""));
        assert!(mcp_service_requires_concrete_tool(
            "remote-http-mcp",
            "remote-http-mcp"
        ));
        assert!(!mcp_service_requires_concrete_tool(
            "remote-http-mcp/read_text_file",
            ""
        ));
        assert!(!mcp_service_requires_concrete_tool(
            "remote-http-mcp",
            "read_text_file"
        ));
    }

    #[test]
    fn code_read_guard_bounds_unscoped_and_large_requests() {
        let (unscoped, notice) = guard_code_read_range(
            "read_text_file",
            json!({"path": "src/agent/context/sampling.rs"}),
        );
        assert_eq!(unscoped["startLine"], 1);
        assert_eq!(unscoped["endLine"], MAX_CODE_READ_LINES);
        assert!(notice.unwrap().contains("正常预算边界"));

        let (large, notice) = guard_code_read_range(
            "read_text_file",
            json!({"path": "src/agent/context/sampling.rs", "startLine": 500, "endLine": 1000}),
        );
        assert_eq!(large["startLine"], 500);
        assert_eq!(large["endLine"], 739);
        assert!(notice.is_some());

        let (bounded, notice) = guard_code_read_range(
            "read_text_file",
            json!({"path": "src/agent/context/sampling.rs", "startLine": 40, "endLine": 100}),
        );
        assert_eq!(bounded["endLine"], 100);
        assert!(notice.is_none());
    }

    #[test]
    fn test_model_manager_crud_and_safety_confirm() {
        let conn = init_db(&PathBuf::from(":memory:")).expect("init memory db");
        let db = DbState(Arc::new(Mutex::new(conn)));

        // 1. 测试 list_specs
        let p_list = serde_json::json!({});
        let res = crate::agent::tools::model_manager::dispatch(&db, "list_specs", &p_list);
        assert!(res.success, "list_specs should succeed: {}", res.stderr);
        assert!(
            res.stdout.contains("DeepSeek-V3") || res.stdout.contains("DeepSeek"),
            "should contain default specs"
        );

        // 2. 测试 upsert_spec
        let p_upsert = serde_json::json!({
            "name": "Custom-Test-Model",
            "context_window": 65536,
            "max_tokens": 8192,
            "supports_thinking": true,
            "default_temperature": 0.6,
            "description": "A unit test model spec"
        });
        let res = crate::agent::tools::model_manager::dispatch(&db, "upsert_spec", &p_upsert);
        assert!(res.success, "upsert_spec should succeed: {}", res.stderr);

        // 3. 测试 delete_spec 无 confirm 拦截
        let p_del_no_confirm = serde_json::json!({ "name": "Custom-Test-Model" });
        let res =
            crate::agent::tools::model_manager::dispatch(&db, "delete_spec", &p_del_no_confirm);
        assert!(
            !res.success,
            "delete_spec without confirm must fail for safety"
        );
        assert!(res.stderr.contains("confirm: true"));

        // 4. 测试 delete_spec 带 confirm: true
        let p_del_confirm = serde_json::json!({ "name": "Custom-Test-Model", "confirm": true });
        let res = crate::agent::tools::model_manager::dispatch(&db, "delete_spec", &p_del_confirm);
        assert!(
            res.success,
            "delete_spec with confirm must succeed: {}",
            res.stderr
        );

        // 5. 测试 list_platforms
        let p_platforms = serde_json::json!({});
        let res = crate::agent::tools::model_manager::dispatch(&db, "list_platforms", &p_platforms);
        assert!(res.success, "list_platforms should succeed: {}", res.stderr);
    }

    #[test]
    fn test_prompts_tools_files_exist() {
        let candidate_dirs = [
            std::path::PathBuf::from("src-tauri/prompts/tools"),
            std::path::PathBuf::from("prompts/tools"),
            std::path::PathBuf::from("../src-tauri/prompts/tools"),
        ];

        let mut found_dir = None;
        for d in &candidate_dirs {
            if d.exists() && d.is_dir() {
                found_dir = Some(d.clone());
                break;
            }
        }

        let dir = found_dir.expect("prompts/tools directory must exist in candidate paths");
        let model_manager_path = dir.join("model_manager.md");
        assert!(
            model_manager_path.exists(),
            "model_manager.md must exist in {:?}",
            dir
        );
        let content = std::fs::read_to_string(&model_manager_path).expect("read model_manager.md");
        assert!(
            content.contains("<summary>"),
            "model_manager.md must contain <summary>"
        );
        assert!(
            content.contains("upsert_spec"),
            "model_manager.md must describe upsert_spec"
        );

        let mcp_manager_path = dir.join("mcp_manager.md");
        assert!(
            mcp_manager_path.exists(),
            "mcp_manager.md must exist in {:?}",
            dir
        );
        let mcp_content = std::fs::read_to_string(&mcp_manager_path).expect("read mcp_manager.md");
        assert!(
            mcp_content.contains("add_http"),
            "mcp_manager.md must describe add_http"
        );

        let skill_manager_path = dir.join("skill_manager.md");
        assert!(
            skill_manager_path.exists(),
            "skill_manager.md must exist in {:?}",
            dir
        );
        let skill_content =
            std::fs::read_to_string(&skill_manager_path).expect("read skill_manager.md");
        assert!(
            skill_content.contains("add"),
            "skill_manager.md must describe add"
        );
    }

    #[test]
    fn test_mcp_and_skill_manager_crud() {
        let conn = init_db(&PathBuf::from(":memory:")).expect("init memory db");
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        // 1. MCP Manager List & Toggle & Delete
        let res_list =
            crate::agent::tools::mcp_manager::dispatch(&db, "list", &serde_json::json!({}));
        assert!(res_list.success);

        // Delete without confirm
        let res_del_fail = crate::agent::tools::mcp_manager::dispatch(
            &db,
            "delete",
            &serde_json::json!({ "name": "fake_mcp" }),
        );
        assert!(!res_del_fail.success);
        assert!(res_del_fail.stderr.contains("confirm: true"));

        // Delete with confirm，但目标不存在：必须诚实失败而非伪报成功
        let res_del_ok = crate::agent::tools::mcp_manager::dispatch(
            &db,
            "delete",
            &serde_json::json!({ "name": "fake_mcp", "confirm": true }),
        );
        assert!(!res_del_ok.success);
        assert!(res_del_ok.stderr.contains("未找到"));

        // 2. Skill Manager Add & List & Delete
        let res_add_skill = crate::agent::tools::skill_manager::dispatch(
            &db,
            "add",
            &serde_json::json!({
                "name": "单元测试技能",
                "content": "编写高覆盖率的 Rust 与 Vue 单元测试",
                "kind": "skill",
                "description": "用于编写单元测试"
            }),
        );
        assert!(res_add_skill.success);
        assert!(res_add_skill.stdout.contains("单元测试技能"));

        let res_skill_list =
            crate::agent::tools::skill_manager::dispatch(&db, "list", &serde_json::json!({}));
        assert!(res_skill_list.success);
        assert!(res_skill_list.stdout.contains("单元测试技能"));

        let res_skill_del = crate::agent::tools::skill_manager::dispatch(
            &db,
            "delete",
            &serde_json::json!({ "name": "单元测试技能", "confirm": true }),
        );
        assert!(res_skill_del.success);
    }

    #[test]
    fn test_host_security_rejection_forbidden_workspace_switch() {
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        )));
        let registry = PluginRegistry::new();
        let p = serde_json::json!({ "path": "/some/other/workspace" });

        // 1. 通用会话未关联项目，不触发项目锁定硬拦截（放行给外部 MCP 路由）
        let res_general = execute_command_inner(&db, "general-session", "switch_workspace", "", &p, &registry, None);
        assert_ne!(res_general.route, "host_rejection:project_session_workspace_locked");

        // 2. 模拟项目会话绑定
        {
            let conn = db.0.lock().unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS agent_projects (id TEXT PRIMARY KEY, name TEXT, root_path TEXT, created_at TEXT, updated_at TEXT);
                 CREATE TABLE IF NOT EXISTS agent_project_sessions (project_id TEXT, session_id TEXT, created_at TEXT);
                 INSERT INTO agent_projects VALUES ('p1', 'proj', '/tmp', 'now', 'now');
                 INSERT INTO agent_project_sessions VALUES ('p1', 'proj-session', 'now');"
            ).unwrap();
        }

        let res = execute_command_inner(&db, "proj-session", "switch_workspace", "", &p, &registry, None);
        assert!(!res.success);
        assert!(res.stderr.contains("宿主安全保护"));
        assert_eq!(res.route, "host_rejection:project_session_workspace_locked");
    }

    #[test]
    fn test_code_read_guard_supports_offset_and_snippet() {
        // 1. 预算内的 offset + limit (50..149 共 100 行 <= 240)
        let p_within = serde_json::json!({
            "path": "src/main.rs",
            "offset": 50,
            "limit": 100
        });
        let (guarded_within, notice_within) = guard_code_read_range("get_code_snippet", p_within);
        assert_eq!(
            guarded_within.get("startLine").and_then(|v| v.as_u64()),
            Some(50)
        );
        assert_eq!(
            guarded_within.get("endLine").and_then(|v| v.as_u64()),
            Some(149)
        );
        assert!(
            notice_within.is_none(),
            "within-budget range should not need truncation notice"
        );

        // 2. 超出预算的 offset + limit (50..5000)
        let p_over = serde_json::json!({
            "path": "src/main.rs",
            "offset": 50,
            "limit": 5000
        });
        let (guarded_over, notice_over) = guard_code_read_range("get_code_snippet", p_over);
        assert_eq!(
            guarded_over.get("startLine").and_then(|v| v.as_u64()),
            Some(50)
        );
        assert_eq!(
            guarded_over.get("endLine").and_then(|v| v.as_u64()),
            Some(289)
        ); // 50 + 240 - 1
        assert!(
            notice_over.is_some(),
            "over-budget range must include truncation guard notice"
        );
    }

    #[test]
    fn test_is_write_or_side_effect_tool_read_only_override() {
        use crate::agent::types::AgentInstruction;
        use serde_json::json;

        // 1. preview_saved_logs 含有 "save" 动词，默认会被判为写操作
        let mut inst_default = AgentInstruction::default();
        inst_default.tool = Some("preview_saved_logs".to_string());
        assert!(is_write_or_side_effect_tool(&inst_default));

        // 2. 参数 command 中显式声明 read_only: true -> 优先判定为只读 (false)
        let mut inst_read_only = AgentInstruction::default();
        inst_read_only.tool = Some("preview_saved_logs".to_string());
        inst_read_only.command = Some(json!({ "read_only": true, "log_id": 123 }));
        assert!(!is_write_or_side_effect_tool(&inst_read_only));

        // 3. 元数据 metadata 中声明 read_only: true
        let mut inst_meta = AgentInstruction::default();
        inst_meta.tool = Some("preview_saved_logs".to_string());
        inst_meta.command = Some(json!({ "metadata": { "read_only": true } }));
        assert!(!is_write_or_side_effect_tool(&inst_meta));

        // 4. params 字段中声明 readonly: true
        let mut inst_params = AgentInstruction::default();
        inst_params.tool = Some("run_read_query".to_string());
        inst_params.params = Some(json!({ "readonly": true }));
        assert!(!is_write_or_side_effect_tool(&inst_params));

        // 5. 真正的写操作即使带 read_only: false 依然是写操作
        let mut inst_write = AgentInstruction::default();
        inst_write.tool = Some("write_file".to_string());
        inst_write.command = Some(json!({ "read_only": false }));
        assert!(is_write_or_side_effect_tool(&inst_write));
    }

    #[test]
    fn test_is_safe_read_only_shell_command() {
        use crate::agent::types::AgentInstruction;
        use serde_json::json;

        // 1. 纯只读 curl 查网络/天气 -> 识别为安全只读放行
        assert!(is_safe_read_only_shell_command("curl https://wttr.in/Shanghai"));
        assert!(is_safe_read_only_shell_command("curl -s https://api.weather.com/data"));
        assert!(is_safe_read_only_shell_command("wget -qO- https://example.com"));

        // 2. 纯只读 git status / diff / log / branch -> 放行
        assert!(is_safe_read_only_shell_command("git status"));
        assert!(is_safe_read_only_shell_command("git diff --stat"));
        assert!(is_safe_read_only_shell_command("git log -n 5"));
        assert!(is_safe_read_only_shell_command("git branch -a"));

        // 3. 常见探针与复合命令 (&&, ;, |) -> 放行
        assert!(is_safe_read_only_shell_command("ls -la && pwd"));
        assert!(is_safe_read_only_shell_command("cat src/main.rs | grep fn"));
        assert!(is_safe_read_only_shell_command("echo hello; date"));
        assert!(is_safe_read_only_shell_command("node -v"));
        assert!(is_safe_read_only_shell_command("cargo --version"));

        // 4. 危险操作一票否决：文件输出重定向 -> 拦截 (判为写操作)
        assert!(!is_safe_read_only_shell_command("curl https://foo.com > output.txt"));
        assert!(!is_safe_read_only_shell_command("echo hello >> README.md"));

        // 5. 危险管道与提权注入 -> 拦截
        assert!(!is_safe_read_only_shell_command("curl https://foo.com | bash"));
        assert!(!is_safe_read_only_shell_command("sudo ls"));
        assert!(!is_safe_read_only_shell_command("rm -rf src/"));
        assert!(!is_safe_read_only_shell_command("sed -i 's/a/b/g' file.txt"));

        // 6. 危险 git 动作 -> 拦截
        assert!(!is_safe_read_only_shell_command("git reset --hard"));
        assert!(!is_safe_read_only_shell_command("git checkout ."));
        assert!(!is_safe_read_only_shell_command("git commit -m 'test'"));

        // 7. 包管理构建/修改动作 -> 拦截
        assert!(!is_safe_read_only_shell_command("npm install"));
        assert!(!is_safe_read_only_shell_command("cargo build"));

        // 8. 复合命令中有一条未知/写操作 -> 全链条一票否决
        assert!(!is_safe_read_only_shell_command("git status && rm -rf tmp"));
        assert!(!is_safe_read_only_shell_command("ls -l; npm run build"));

        // 9. 集成到 is_write_or_side_effect_tool 验证
        let mut inst_curl = AgentInstruction::default();
        inst_curl.tool = Some("remote-hub/shell_exec".to_string());
        inst_curl.command = Some(json!({ "command": "curl https://wttr.in/Shanghai" }));
        assert!(!is_write_or_side_effect_tool(&inst_curl), "只读 curl 应该放行为只读操作");

        let mut inst_write_shell = AgentInstruction::default();
        inst_write_shell.tool = Some("remote-hub/shell_exec".to_string());
        inst_write_shell.command = Some(json!({ "command": "npm test" }));
        assert!(is_write_or_side_effect_tool(&inst_write_shell), "npm test 属于潜在测试/写入脚本必须受门禁保护");

        // 10. 用户实测场景: bash -c 复合天气/网络探针多命令
        let mut inst_bash_c = AgentInstruction::default();
        inst_bash_c.tool = Some("remote-hub/shell_exec".to_string());
        inst_bash_c.command = Some(json!({
            "command": "bash",
            "args": [
                "-c",
                "date; curl -s -m 5 'http://ip-api.com/json'; echo ''; curl -s -m 5 'wttr.in/?format=3'"
            ],
            "cwd": "/home/w/文档/free_api"
        }));
        assert!(!is_write_or_side_effect_tool(&inst_bash_c), "bash -c 复合只读命令必须放行为只读操作");

        // 11. bash -c 若内部含有写操作 -> 必须拦截
        let mut inst_bash_c_write = AgentInstruction::default();
        inst_bash_c_write.tool = Some("remote-hub/shell_exec".to_string());
        inst_bash_c_write.command = Some(json!({
            "command": "bash",
            "args": [
                "-c",
                "date; rm -rf target"
            ]
        }));
        assert!(is_write_or_side_effect_tool(&inst_bash_c_write), "bash -c 包含 rm 必须判定为写操作受门禁保护");

        // 12. 带有 command 与 args 拆分传递的形式 (如 curl -s http://...)
        let mut inst_split_args = AgentInstruction::default();
        inst_split_args.tool = Some("run_command".to_string());
        inst_split_args.command = Some(json!({
            "command": "curl",
            "args": ["-s", "-m", "5", "http://ip-api.com/json"]
        }));
        assert!(!is_write_or_side_effect_tool(&inst_split_args), "curl + args 拆分传递只读必须放行");
    }
}
