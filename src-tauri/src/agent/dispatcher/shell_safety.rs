//! Shell 命令安全谓词：段切分、重定向、危险命令与只读判定。

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

use super::*;

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

/// 检测命令行中是否存在真正写盘的文件重定向（如 `> file.txt`、`>> out.log`）。
/// 判定委托给审计台账同款 has_write_redirect（引号抹除 + 转义 \> + fd 复制
/// `>&N`/`>&-` + 哑目标 /dev/null|stdout|stderr|tty|pts/*|fd/*|nul 豁免），
/// 外加 `[[ a > b ]]` 比较域抹除——两侧对"只读探针"口径一致，不再各自漂移。
pub fn has_file_write_redirection(cmd: &str) -> bool {
    let blanked = crate::agent::context::AuditLedger::blank_quoted_segments(cmd);
    let masked = mask_double_bracket_regions(&blanked);
    crate::agent::context::AuditLedger::has_write_redirect(&masked)
}

/// 抹除 `[[ ... ]]` 成对括号内的内容（保留 `]]` 之后的部分——
/// `[[ x ]] > f` 的写盘重定向仍须被检出）。引号内容已被上游抹除，
/// 残留 `[[` 必是结构性的。未闭合的 `[[` 不抹除（保守：内部 > 仍按重定向计）。
fn mask_double_bracket_regions(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = bytes.to_vec();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            // 找最近的 "]]"
            let mut j = i + 2;
            let mut close = None;
            while j + 1 < bytes.len() {
                if bytes[j] == b']' && bytes[j + 1] == b']' {
                    close = Some(j + 2);
                    break;
                }
                j += 1;
            }
            if let Some(end) = close {
                for k in i..end {
                    out[k] = b' ';
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_pipe_to_interpreter(lower: &str) -> bool {
    // tee 带文件参数即落盘写入；xargs 把管道输出变成任意命令的参数——
    // 与解释器同级的"管道藏副作用"通道
    let interpreters = [
        "bash", "sh", "zsh", "python", "python3", "node", "perl", "ruby",
        "tee", "xargs",
    ];
    for interp in &interpreters {
        for prefix in &[format!("| {}", interp), format!("|{}", interp)] {
            let mut search_from = 0;
            while let Some(pos) = lower[search_from..].find(prefix) {
                let abs_pos = search_from + pos;
                let after = &lower[abs_pos + prefix.len()..];
                if after.is_empty()
                    || after.starts_with(' ')
                    || after.starts_with('\t')
                    || after.starts_with('-')
                    || after.starts_with('|')
                    || after.starts_with(';')
                    || after.starts_with('&')
                {
                    return true;
                }
                search_from = abs_pos + prefix.len();
            }
        }
    }
    false
}

/// 检测 Shell 命令是否试图进行写盘重定向、文本就地替换、文件删除或破坏性修改。
/// 仅拦截这几类明确的破坏性写盘操作；其他所有常规命令与探针全部放行。
pub fn is_destructive_shell_command(raw_cmd: &str) -> bool {
    let trimmed = raw_cmd.trim();
    if trimmed.is_empty() {
        return false;
    }

    // 1. 检查是否存在真实文件写盘/追加重定向（自动排除 /dev/null、2>&1、引号内 >）
    if has_file_write_redirection(trimmed) {
        return true;
    }

    // 2. 检查文本就地替换、文件删除等破坏性操作
    let lower = trimmed.to_ascii_lowercase();

    // 2.1 文本就地替换与流编辑写盘 (sed -i, perl -i, awk -i inplace 等)
    if lower.contains("sed -i")
        || lower.contains("perl -pi")
        || lower.contains("perl -i")
        || lower.contains("awk -i inplace")
    {
        return true;
    }

    // 2.2 文件或目录删除、文本清空与强制截断 (rm, rmdir, unlink, truncate, shred)
    if lower.contains("rm ")
        || lower.contains("rm\t")
        || lower.contains("rmdir ")
        || lower.contains("rmdir\t")
        || lower.contains("unlink ")
        || lower.ends_with(" rm")
        || lower.contains("truncate ")
        || lower.contains("shred ")
        || lower.contains("dd of=")
    {
        return true;
    }

    // 2.3 危险管道注入与提权 (curl ... | bash, sudo 等)
    if is_pipe_to_interpreter(&lower)
        || lower.starts_with("sudo ")
        || lower.contains("; sudo ")
        || lower.contains("&& sudo ")
        || lower.contains("|| sudo ")
        || lower.contains("| sudo ")
    {
        return true;
    }

    false
}

/// 兼容性辅助包装：非破坏性即为安全只读
pub fn is_safe_read_only_shell_command(raw_cmd: &str) -> bool {
    !is_destructive_shell_command(raw_cmd)
}
