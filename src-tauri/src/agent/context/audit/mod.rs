use crate::agent::context::types::{SandwichContext, SideEffectRecord};

/// 不可逆操作永久台账（安全审计区，无论历史如何压缩，台账永不丢弃）
pub struct AuditLedger;

mod shell_readonly;

impl AuditLedger {

    /// 格式化不可逆操作台账（如超过 8 条则进行同类动作聚合折叠，保持体积紧凑）
    pub fn render_ledger(records: &[SideEffectRecord]) -> String {
        if records.is_empty() {
            return String::new();
        }

        let mut lines = Vec::new();
        let total = records.len();

        if total <= 8 {
            for r in records {
                lines.push(format!(
                    "- [Step {}] {} · {}",
                    r.step_id, r.tool, r.description
                ));
            }
        } else {
            // 早期记录聚合
            let omitted = total - 6;
            lines.push(format!(
                "- [... 早期已安全执行 {} 项不可逆动作 (完整记录在 SQLite) ...]",
                omitted
            ));
            for r in &records[omitted..] {
                lines.push(format!(
                    "- [Step {}] {} · {}",
                    r.step_id, r.tool, r.description
                ));
            }
        }

        format!(
            "【已生效的不可逆外部操作 (Side-Effects 台账)】\n(⚠️系统提示：以下动作已被物理执行，请勿重复调用！)\n{}\n\n",
            lines.join("\n")
        )
    }

    /// 兼容性入口：根据 tool 与 params 判断是否产生外部物理副作用
    pub fn is_mutating_side_effect(tool: &str, params: &serde_json::Value) -> bool {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        Self::is_mutating_side_effect_with_action(tool, action, params)
    }

    /// 核心判断：结合工具名、动作名与参数细分判定是否属于可能产生外部物理副作用的操作
    pub fn is_mutating_side_effect_with_action(
        tool: &str,
        action: &str,
        params: &serde_json::Value,
    ) -> bool {
        let lower_tool = tool.to_lowercase();
        let action_name = lower_tool.split('/').last().unwrap_or(&lower_tool);
        if lower_tool == "child_task" { return false; } // 实际写入由子检查点的带来源台账导入。
        if lower_tool == "model_manager" {
            let effective_action = if action.is_empty() {
                params.get("action").and_then(|v| v.as_str()).unwrap_or("list_specs")
            } else {
                action
            };
            return !crate::agent::tools::model_manager::is_readonly_action(effective_action);
        }
        let act = if !action.is_empty() {
            action.to_lowercase()
        } else {
            params
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase()
        };

        // 0. 原生 local_file 工具专用精准判定：
        //    - read / list 为纯只读，绝不记录进 Side-Effects 台账；
        //    - 写入到 session_tmp（如 path 为 tmp/ 或 ./tmp/，或显式 is_tmp）为草稿免审，不进台账；
        //    - write / edit 写入项目工作区代码时，必须严格入账！
        if action_name == "local_file" {
            if act.starts_with("read") || act.starts_with("list") {
                return false;
            }
            if act.starts_with("write") || act.starts_with("edit") {
                if Self::is_temporary_draft_path(params) {
                    return false;
                }
                return true;
            }
            return false;
        }

        // 1. 显式只读工具白名单（绝不记录进 Side-Effects 台账）
        let is_readonly_tool = matches!(
            action_name,
            "read_file"
                | "read_text_file"
                | "read_media_file"
                | "read_multiple_files"
                | "list_directory"
                | "list_directory_with_sizes"
                | "directory_tree"
                | "get_file_info"
                | "list_allowed_directories"
                | "vscode_getdiagnostics"
                | "vscode_getsymbollspinfo"
                | "vscode_getreferences"
                | "vscode_listworkspaces"
                | "vscode_health"
                | "ripgrep_search"
                | "ripgrep_list_files"
                | "ripgrep_search_files"
                | "search_graph"
                | "query_graph"
                | "trace_path"
                | "get_code_snippet"
                | "get_graph_schema"
                | "get_architecture"
                | "search_code"
                | "list_projects"
                | "index_status"
                | "check_index_coverage"
                | "detect_changes"
                | "list_workspaces"
                | "git_diff"
                | "health_check"
                | "web_search"
                | "web_fetch"
                | "get_step_output"
                | "search_step_output"
                | "get_output"
                | "list_resources"
                | "get_resource"
                | "list_records"
                | "get_record"
                | "ask_human"
                | "finish"
        );
        if is_readonly_tool {
            return false;
        }

        // 2. shell 命令：细分检测是否为只读命令
        if action_name == "local_shell" || action_name == "shell_exec" || action_name == "shell_exec_async" {
            return !Self::is_readonly_shell_command(params);
        }

        // 3. MCP 命名空间工具（plugin/tool 形态）：写动词词表不可枚举——
        //    format_range/set_formula/rename_sheet/merge_cells/insert_rows 这类
        //    不含经典写动词的变更操作不能因词表漏收而对台账隐形。
        //    只读动词前缀放行，其余一律记副作用（fail-closed）。
        //    例外：叶子名带强壳标记（shell/bash/terminal/powershell/cmd/command）
        //    时参数语言就是 shell——走命令级分析，与 dispatcher 判定保持一致
        //    （x/run_command 跑 `ls` 不该一侧放行一侧入账）。
        if lower_tool.contains('/') {
            let orig_name = tool.split('/').last().unwrap_or(tool);
            let leaf_lower = orig_name.to_lowercase();
            let strong_shell_marker = leaf_lower.contains("shell")
                || leaf_lower.contains("bash")
                || leaf_lower.contains("terminal")
                || leaf_lower.contains("powershell")
                || leaf_lower.contains("cmd")
                || leaf_lower.contains("command");
            if strong_shell_marker {
                return !Self::is_readonly_shell_command(params);
            }
            return !Self::mcp_tool_is_readonly_verb(orig_name);
        }

        // 4. 常见写操作与变更类动作
        action_name.contains("write")
            || action_name.contains("edit")
            || action_name.contains("move")
            || action_name.contains("rename")
            || action_name.contains("delete")
            || action_name.contains("create")
            || action_name.contains("upsert")
            || action_name.contains("patch")
            || action_name.contains("apply")
            || action_name.contains("remove")
            || action_name.contains("insert")
            || action_name.contains("update")
            || action_name.contains("format")
            || action_name.contains("merge")
            || action_name.contains("append")
            || action_name.contains("install")
            || action_name.contains("set_")
            || action_name.contains("add_")
            || action_name.contains("drop")
            || action_name.contains("modify")
            || action_name.contains("fill")
            || action_name.contains("clear")
            || action_name.contains("resize")
            || action_name.contains("upload")
            || action_name.contains("download")
            || action_name.contains("import")
            || action_name.contains("export")
            || action_name.contains("exec")
            || action_name.contains("deploy")
            || act.starts_with("write")
            || act.starts_with("edit")
            || act.starts_with("delete")
            || act.starts_with("save")
            || act.starts_with("rename")
            || act.starts_with("update")
            || act.starts_with("insert")
            || act.starts_with("move")
    }

    /// MCP 工具名的只读动词判定：snake_case 前缀、连字符前缀或 camelCase
    /// 词首大写边界（readFile/getSheetData）都算；`getaway_x` 这类以读动词
    /// 起头但非独立词的形态不误判（小写续写不算词边界）。
    /// 入参传原始工具名（未小写化）——camelCase 边界依赖原始大小写。
    pub(crate) fn mcp_tool_is_readonly_verb(action_name: &str) -> bool {
        let lower = action_name.to_lowercase();
        const READ_VERBS: &[&str] = &[
            "read", "list", "ls", "get", "describe", "show", "inspect", "query",
            "search", "find", "check", "status", "info", "count", "exists",
            "validate", "ping", "health", "version", "help", "view", "peek",
            "fetch", "probe", "analyze", "summarize", "preview", "dump", "cat",
            "diff", "watch", "monitor", "trace", "resolve", "lookup", "detect",
            "estimate", "explain", "report",
        ];
        READ_VERBS.iter().any(|v| {
            lower == *v
                || lower.starts_with(&format!("{v}_"))
                || lower.starts_with(&format!("{v}-"))
                || action_name
                    .strip_prefix(v)
                    .map_or(false, |rest| {
                        rest.chars().next().map_or(false, |c| c.is_uppercase())
                    })
        })
    }

    /// 判定参数中的目标文件路径是否属于会话临时草稿目录 (session_tmp / tmp)
    pub fn is_temporary_draft_path(params: &serde_json::Value) -> bool {
        if params.get("is_tmp").and_then(|v| v.as_bool()).unwrap_or(false) {
            return true;
        }
        let raw_path = params
            .get("path")
            .or_else(|| params.get("target_file"))
            .or_else(|| params.get("filepath"))
            .or_else(|| params.get("file_path"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if raw_path.is_empty() {
            return false;
        }
        crate::agent::path_guard::is_path_in_session_tmp(raw_path, None)
    }

    /// 检测 shell 命令是否为只读查询（如 git log/show/diff, ls, cat, grep 等）
    pub fn is_readonly_shell_command(params: &serde_json::Value) -> bool {
        // local_shell 执行端接受非空命令字符串简写；台账须使用同一形态。
        if let Some(command) = params.as_str() {
            return Self::is_readonly_shell_command(&serde_json::json!({"command": command}));
        }
        let mut explicit_args: Vec<&str> = params
            .get("args")
            .and_then(|a| a.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        let owned_args_cmd;
        let raw_cmd = {
            // 与执行端相同：完整脚本字段优先于 command/cmd/input。
            let c = ["script", "command_line", "CommandLine", "command", "cmd", "input", "commandline"]
                .iter()
                .filter_map(|key| params.get(*key).and_then(|v| v.as_str()))
                .map(str::trim)
                .find(|value| !value.is_empty())
                .unwrap_or("");
            if c.is_empty() && !explicit_args.is_empty() {
                // args-only 形态（{"args":["ls","-la"]}）：args 本体即命令行——
                // dispatcher 同款提取，两侧分类保持一致
                owned_args_cmd = explicit_args.join(" ");
                explicit_args.clear();
                owned_args_cmd.as_str()
            } else {
                c
            }
        };

        // 命令替换/进程替换守卫：$(cmd)、`cmd`、<(cmd)、>(cmd) 内层不可静态审计，
        // 必须扫原文（双引号内的 $() 同样会执行，不能用抹除引号后的文本判断）。
        if raw_cmd.contains("$(")
            || raw_cmd.contains('`')
            || raw_cmd.contains("<(")
            || raw_cmd.contains(">(")
        {
            return false;
        }

        // 复合命令拆解：&&/||/;/|/&/换行 的每个子命令段都必须各自只读，
        // 否则 `which x && rm y`（首段只读）会把整串误判为只读、漏记副作用。
        // 直接在原文上扫描并跟踪引号状态——不能用 blank_quoted_segments 定位，
        // 多字节字符被抹成单空格会导致字节偏移错位。
        let bytes = raw_cmd.as_bytes();
        let mut quote: Option<u8> = None;
        let mut segments: Vec<&str> = Vec::new();
        let mut last = 0usize;
        let mut i = 0usize;
        while i < bytes.len() {
            let b = bytes[i];
            if let Some(q) = quote {
                if b == q {
                    quote = None;
                }
                i += 1;
                continue;
            }
            if b == b'\'' || b == b'"' {
                quote = Some(b);
                i += 1;
                continue;
            }
            let mut sep_len = 0usize;
            match b {
                b';' | b'\n' => sep_len = 1,
                b'|' => {
                    // `||` 与 `|&`（stderr 管道）各占两字符
                    if matches!(bytes.get(i + 1), Some(b'|') | Some(b'&')) {
                        sep_len = 2;
                    } else {
                        sep_len = 1;
                    }
                }
                b'&' => {
                    // `>&` 与 `|&` 中的 & 属重定向/管道符号；`&>`/`&>>` 的 &
                    // 与重定向同属一段（后续 > 检测会处理），均不作命令分隔
                    let prev = i > 0 && matches!(bytes[i - 1], b'>' | b'|');
                    let next_gt = bytes.get(i + 1) == Some(&b'>');
                    if !prev && !next_gt {
                        sep_len = if bytes.get(i + 1) == Some(&b'&') { 2 } else { 1 };
                    }
                }
                _ => {}
            }
            if sep_len > 0 {
                segments.push(raw_cmd[last..i].trim());
                i += sep_len;
                last = i;
            } else {
                i += 1;
            }
        }
        segments.push(raw_cmd[last..].trim());
        segments.retain(|s| !s.is_empty());

        // `[[ a || b ]]` 内的 ||/&& 是测试表达式而非命令分隔：把未闭合 [[ 的
        // 片段与后续片段重新合并，否则该常用只读探针会被误判为写入、清空验证态。
        let mut merged: Vec<String> = Vec::new();
        for seg in segments {
            if let Some(prev) = merged.last_mut() {
                let opens = prev.matches("[[").count();
                let closes = prev.matches("]]").count();
                if opens > closes {
                    prev.push_str(" || ");
                    prev.push_str(seg);
                    continue;
                }
            }
            merged.push(seg.to_string());
        }
        let segments: Vec<String> = merged;

        if segments.is_empty() {
            return false;
        }
        // 命令串含分隔符时 args 归属不明（属于哪一段不确定），保守按非只读
        if segments.len() > 1 && !explicit_args.is_empty() {
            return false;
        }

        segments.iter().all(|seg| {
            let mut tokens: Vec<&str> = seg.split_whitespace().collect();
            if explicit_args.is_empty() {
                Self::is_readonly_command_segment(&tokens)
            } else {
                // 单段命令 + 显式 args：args 接在命令后
                tokens.extend(explicit_args.iter().copied());
                Self::is_readonly_command_segment(&tokens)
            }
        })
    }
}

impl SandwichContext {
    pub fn render_side_effects_ledger(&self) -> String {
        AuditLedger::render_ledger(&self.side_effects)
    }
}

#[cfg(test)]
mod tests;
