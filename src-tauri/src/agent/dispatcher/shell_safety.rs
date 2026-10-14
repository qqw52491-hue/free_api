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

    if inst.get_tool().eq_ignore_ascii_case("local_shell") {
        return parse_from_value(&inst.get_params());
    }
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

/// 纯只读 shell 命令（白名单，用于 parallel 只读扇出放宽）：
/// 每一段（按换行/`;`/`|`/`&` 切）的首个命令都在只读白名单内，且无写重定向、
/// heredoc、命令替换。与 is_safe_read_only_shell_command（黑名单）不同：
/// `node render.mjs`、`ffmpeg -y` 这类有副作用的命令一律不算。
pub fn is_pure_read_shell_command(raw_cmd: &str) -> bool {
    let raw = raw_cmd.trim();
    if raw.is_empty() || raw.contains("<<") || raw.contains("$(") || raw.contains('`') {
        return false;
    }
    if has_file_write_redirection(raw) {
        return false;
    }
    // 引号内容抹除后分析结构；`>&` 的 fd 复制（2>&1）先去掉 &，免得被当成分隔符
    let blanked = crate::agent::context::AuditLedger::blank_quoted_segments(raw).replace(">&", ">");
    const READ_CMDS: [&str; 33] = [
        "cat", "ls", "head", "tail", "grep", "egrep", "fgrep", "rg", "stat", "wc", "file",
        "ffprobe", "echo", "printf", "pwd", "which", "jq", "diff", "sed", "find", "du", "df",
        "tree", "basename", "dirname", "realpath", "readlink", "date", "whoami", "cd", "true",
        "test", "[",
    ];
    const GIT_READ: [&str; 8] = [
        "status", "diff", "log", "show", "ls-files", "rev-parse", "branch", "remote",
    ];
    let mut any = false;
    for seg in blanked.split(|c: char| matches!(c, '\n' | ';' | '|' | '&')) {
        let tokens: Vec<&str> = seg.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }
        // 跳过前置的 VAR=value 与 time
        let mut i = 0;
        while i < tokens.len() {
            let t = tokens[i];
            let is_assign = t
                .split_once('=')
                .map(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(false);
            if is_assign || t == "time" {
                i += 1;
            } else {
                break;
            }
        }
        let Some(first) = tokens.get(i) else { return false };
        let name = first.rsplit('/').next().unwrap_or(first);
        let rest = &tokens[i + 1..];
        let ok = if name == "git" {
            // 跳过 git 全局选项（-C/-c 带参数）找子命令
            let mut sub = "";
            let mut it = rest.iter();
            while let Some(t) = it.next() {
                if matches!(*t, "-C" | "-c" | "--git-dir" | "--work-tree") {
                    it.next();
                } else if !t.starts_with('-') {
                    sub = t;
                    break;
                }
            }
            GIT_READ.contains(&sub)
                && !rest.iter().any(|t| t.starts_with("--output"))
                && !(sub == "branch" && rest.iter().any(|t| matches!(*t, "-d" | "-D" | "-m" | "-M" | "-c" | "-C" | "--delete" | "--move" | "--copy")))
                && !(sub == "remote" && rest.iter().any(|t| matches!(*t, "add" | "remove" | "rm" | "rename" | "set-url" | "prune" | "update")))
        } else if READ_CMDS.contains(&name) {
            match name {
                "sed" => !rest.iter().any(|t| t.starts_with("-i") || *t == "--in-place" || t.starts_with("--in-place=")),
                "find" => !rest.iter().any(|t| {
                    matches!(*t, "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir")
                        || t.starts_with("-fprint")
                        || *t == "-fls"
                }),
                _ => true,
            }
        } else {
            false
        };
        if !ok {
            return false;
        }
        any = true;
    }
    any
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

    // 2.4 git 工作区破坏操作：checkout --/restore/clean/reset --hard 会销毁他人
    // 未提交工作且绕开文件写入台账（不走 path_guard）。实测事故（2026-09-25）：
    // 终审指令"撤销无关修改保持提交纯净"被执行为 git checkout -- 销毁了
    // eval_runner.mjs 85 行未提交工作——此类命令必须归类为破坏性写操作。
    // 窄口径：checkout -b / checkout <branch>（切分支）不拦，只拦还原路径形态。
    // 注意传原始大小写串：branch -D(强删) 与 branch -d(安全删) 仅靠大小写区分。
    if is_git_worktree_destructive(trimmed) {
        return true;
    }

    false
}

/// git 破坏命令三态判定：台账对账用——区分「无路径形态（整树/历史/远端）」
/// 与「可按路径对账」两类，供 dispatch 层决定放行/人工确认。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitDestructiveVerdict {
    /// 非 git 破坏命令
    NotDestructive,
    /// 变更型但不销毁既有内容（git stash pop/apply）：归写操作入账，
    /// 但不需要人工确认——pop 冲突时 git 拒绝并保留 stash，无静默销毁面。
    SideEffect,
    /// 无显式路径目标（reset --hard / clean / stash drop·clear /
    /// push -f / branch -D / checkout . / checkout *）——影响面无法按
    /// 路径界定，一律需要人工确认。
    Pathless,
    /// 有显式路径目标（checkout -- <paths> / restore <paths> / rm/mv <paths>），
    /// 可与 session 写入台账逐条对账。
    /// dirty_gated=true（checkout/restore）：只对「脏=有未提交内容」的目标算外来——
    /// 干净已提交文件上还原是 no-op，无可销毁物；rm/mv 物理删移文件恒 gated。
    Targets {
        paths: Vec<String>,
        dirty_gated: bool,
    },
}

/// 命令指纹：压缩空白后原文——ask_human 的 message 原文引用该指纹即可机械核对
/// 「主人确认的是不是这条命令」（模型改写措辞不破坏匹配）。
pub fn git_cmd_fingerprint(cmd: &str) -> String {
    cmd.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 剥离 shell 词法后的 token 序列（处理引号包裹的路径参数）。
fn shell_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<u8> = None;
    for &b in s.as_bytes() {
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                } else {
                    cur.push(b as char);
                }
            }
            None => {
                if b == b'\'' || b == b'"' {
                    quote = Some(b);
                } else if b == b' ' || b == b'\t' {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                } else {
                    cur.push(b as char);
                }
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 参数是否像文件路径（区别于分支名）：最后一段带字母开头的扩展名，
/// 且含 `/` 或扩展名在常见源码/配置后缀内。`release/1.0`、`origin/main`、`feature/x` 不算。
fn looks_like_file_path(s: &str) -> bool {
    const KNOWN_EXT: &[&str] = &[
        "java", "rs", "vue", "js", "jsx", "ts", "tsx", "py", "txt", "json", "yaml", "yml", "xml",
        "md", "css", "scss", "html", "sql", "sh", "toml", "properties", "cfg", "conf", "go",
        "c", "cpp", "h", "mjs", "lock",
    ];
    let last = s.rsplit('/').next().unwrap_or(s);
    let Some((stem, ext)) = last.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() || ext.is_empty() || ext.len() > 10 {
        return false;
    }
    if !ext.chars().next().map_or(false, |c| c.is_ascii_alphabetic())
        || !ext.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return false;
    }
    s.contains('/') || KNOWN_EXT.contains(&ext.to_ascii_lowercase().as_str())
}

/// 提取 git 破坏命令的判定结果。输入原始命令串（不 lowercase——
/// branch -D 与 -d 语义相反；git 子命令按惯例小写，变体大小写保守不判）。
pub fn git_destructive_review(cmd: &str) -> GitDestructiveVerdict {
    for seg in split_shell_segments(cmd) {
        let s = seg.trim();
        if !(s == "git" || s.starts_with("git ") || s.starts_with("git\t")) {
            continue;
        }
        let tokens = shell_tokens(s);
        // tokens[0] = "git"
        let sub = tokens.get(1).map(|t| t.as_str()).unwrap_or("");
        // -n/--dry-run 是试运行形态（clean -n / push -n / rm -n / mv -n 均不落盘），放行
        if tokens.iter().any(|t| t == "-n" || t == "--dry-run") {
            continue;
        }
        // 非 - 开头的尾部参数 = 候选路径目标
        let path_args = |skip: usize| -> Vec<String> {
            tokens
                .iter()
                .skip(skip)
                .filter(|t| !t.starts_with('-'))
                .cloned()
                .collect()
        };
        match sub {
            "restore" => {
                // restore 语义无歧义：非 flag 参数即要还原的路径；无路径/整树 → Pathless
                let mut targets = match tokens.iter().position(|t| t == "--") {
                    Some(p) => path_args(p + 1),
                    None => path_args(2),
                };
                targets.retain(|t| t != "." && t != "*");
                if !targets.is_empty() {
                    return GitDestructiveVerdict::Targets {
                        paths: targets,
                        dirty_gated: true,
                    };
                }
                return GitDestructiveVerdict::Pathless;
            }
            "checkout" => {
                // 仅 `--` 后的路径与 `.`/`*` 是明确的还原形态；不带 -- 的 <arg>
                // 无法与分支名机械区分，保守放行（残余面：checkout <path> 形态可
                // 还原文件，但需刻意规避才成立——记录在案）。
                if let Some(p) = tokens.iter().position(|t| t == "--") {
                    let mut targets = path_args(p + 1);
                    targets.retain(|t| t != "." && t != "*");
                    if !targets.is_empty() {
                        return GitDestructiveVerdict::Targets {
                            paths: targets,
                            dirty_gated: true,
                        };
                    }
                    return GitDestructiveVerdict::Pathless;
                }
                if tokens.iter().skip(2).any(|t| t == "." || t == "*") {
                    return GitDestructiveVerdict::Pathless;
                }
                // 不带 -- 的 `git checkout src/A.java B.txt` 同样会覆盖工作区文件（实测 2026-10-10：
                // 代理用它还原了用户本地未提交改动）。有切分支/新建分支旗标则放行；
                // 否则参数里凡“像文件路径”的（带字母扩展名）按还原路径处理。
                let switching = tokens.iter().skip(2).any(|t| {
                    matches!(
                        t.as_str(),
                        "-b" | "-B" | "--orphan" | "--detach" | "-d" | "-t" | "--track"
                    )
                });
                if !switching {
                    let paths: Vec<String> = tokens
                        .iter()
                        .skip(2)
                        .filter(|t| !t.starts_with('-') && looks_like_file_path(t))
                        .cloned()
                        .collect();
                    if !paths.is_empty() {
                        return GitDestructiveVerdict::Targets { paths, dirty_gated: true };
                    }
                }
            }
            "rm" | "mv" => {
                let targets = path_args(2);
                if targets.is_empty() {
                    return GitDestructiveVerdict::Pathless;
                }
                return GitDestructiveVerdict::Targets {
                    paths: targets,
                    dirty_gated: false,
                };
            }
            "clean" => {
                return GitDestructiveVerdict::Pathless;
            }
            "reset" => {
                if tokens.iter().any(|t| t == "--hard" || t == "--keep") {
                    return GitDestructiveVerdict::Pathless;
                }
            }
            "push" => {
                if tokens.iter().any(|t| {
                    t == "-f" || t.starts_with("--force")
                }) {
                    return GitDestructiveVerdict::Pathless;
                }
            }
            "branch" => {
                if tokens.iter().any(|t| t == "-D") {
                    return GitDestructiveVerdict::Pathless;
                }
            }
            "stash" => {
                if tokens.iter().skip(2).any(|t| matches!(t.as_str(), "drop" | "clear")) {
                    return GitDestructiveVerdict::Pathless;
                }
                if tokens.iter().skip(2).any(|t| matches!(t.as_str(), "pop" | "apply")) {
                    return GitDestructiveVerdict::SideEffect;
                }
            }
            _ => {}
        }
    }
    GitDestructiveVerdict::NotDestructive
}

/// git 破坏命令的写分类入口：委托 git_destructive_review 单一口径，
/// 非 NotDestructive 即记为破坏性写操作（入账+受变更层门禁）。
/// `git checkout <path>`（不带 --）无法与分支名机械区分，保守不拦；
/// `git stash`（不含 drop/clear/pop）保留现场可回收，不拦。
/// 输入为原始大小写命令串（-D 与 -d 语义相反，不能先 lowercase）。
fn is_git_worktree_destructive(cmd: &str) -> bool {
    git_destructive_review(cmd) != GitDestructiveVerdict::NotDestructive
}

/// 兼容性辅助包装：非破坏性即为安全只读
pub fn is_safe_read_only_shell_command(raw_cmd: &str) -> bool {
    !is_destructive_shell_command(raw_cmd)
}

// ─── 敏感访问审查：连数据库 / 读凭据密钥文件 ─────────────────────────────────────

/// 凭证/密钥类文件判定（保守：只认名字明确的几类）。
fn is_credential_file_arg(arg: &str) -> bool {
    if arg.starts_with('-') {
        return false;
    }
    let lower = arg.to_ascii_lowercase();
    let base = lower.rsplit('/').next().unwrap_or(&lower);
    // 公钥/示例模板不含机密，优先豁免
    if base.ends_with(".example")
        || base.ends_with(".sample")
        || base.ends_with(".template")
        || base.ends_with(".pub")
    {
        return false;
    }
    if lower.contains("/.ssh/")
        || lower.starts_with(".ssh/")
        || lower.starts_with("~/.ssh")
        || lower.contains(".aws/credentials")
        || lower.contains(".kube/config")
    {
        return true;
    }
    base == ".env"
        || base.starts_with(".env.")
        || base.ends_with(".pem")
        || base.ends_with(".key")
        || base.ends_with(".p12")
        || base.ends_with(".pfx")
        || base.starts_with("id_rsa")
        || base.starts_with("id_ed25519")
        || base.starts_with("id_ecdsa")
        || matches!(
            base,
            ".netrc"
                | ".pgpass"
                | ".my.cnf"
                | ".npmrc"
                | ".git-credentials"
                | "credentials"
                | "credentials.json"
                | "secrets"
        )
        || base.starts_with("secrets.")
}

/// 敏感访问审查：命令会“连接数据库”或“把凭证/密钥文件内容读进上下文”时返回原因。
/// 验证类任务里模型常自己搞环境去连库（775b167b 实测：读数据库凭据文件 +
/// root 连本地 MySQL），凭据明文进入上下文与日志。这里只做机械识别（不判断合理性），
/// 放行交给 ask_human 的人工确认。忽略 heredoc 正文（内嵌脚本不当作 shell 命令判）。
pub fn sensitive_shell_access_review(cmd: &str) -> Option<String> {
    const DB_CLIENTS: [&str; 11] = [
        "mysql", "mariadb", "psql", "redis-cli", "mongo", "mongosh", "sqlcmd", "sqlplus",
        "mysqldump", "pg_dump", "mongodump",
    ];
    const READERS: [&str; 16] = [
        "cat", "sed", "head", "tail", "less", "more", "grep", "egrep", "fgrep", "rg", "awk",
        "strings", "bat", "nl", "cut", "base64",
    ];
    let mut heredoc_end: Option<String> = None;
    for line in cmd.lines() {
        if let Some(end) = &heredoc_end {
            if line.trim() == end {
                heredoc_end = None;
            }
            continue;
        }
        if let Some(pos) = line.find("<<") {
            let rest = line[pos + 2..].trim_start_matches('-').trim();
            if !rest.starts_with('<') {
                let delim = rest
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_matches(|c| c == '\'' || c == '"')
                    .to_string();
                if !delim.is_empty() {
                    heredoc_end = Some(delim);
                }
            }
        }
        for seg in split_shell_segments(line) {
            let toks = shell_tokens(seg);
            let mut i = 0;
            while i < toks.len() {
                let t = toks[i].as_str();
                let is_assign = !t.starts_with('-')
                    && t.split_once('=').is_some_and(|(k, _)| {
                        !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    });
                if is_assign || matches!(t, "sudo" | "env" | "nohup" | "time") {
                    i += 1;
                } else {
                    break;
                }
            }
            let Some(first) = toks.get(i) else { continue };
            let name = first.rsplit('/').next().unwrap_or(first).to_ascii_lowercase();
            let args = &toks[i + 1..];
            if DB_CLIENTS.contains(&name.as_str()) {
                let only_info = args
                    .iter()
                    .all(|a| matches!(a.as_str(), "--version" | "-V" | "-v" | "--help" | "-?"));
                if !only_info {
                    return Some(format!("命令会连接数据库（{}）", name));
                }
            }
            if READERS.contains(&name.as_str()) {
                if let Some(f) = args.iter().find(|a| is_credential_file_arg(a)) {
                    return Some(format!("命令会把凭证/密钥文件 `{}` 的内容读进上下文", f));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod pure_read_tests {
    use super::is_pure_read_shell_command as pure;

    #[test]
    fn whitelist_read_commands() {
        assert!(pure("ls -la out/"));
        assert!(pure("cat a.txt | head -20"));
        assert!(pure("cd /tmp && ls 2>&1"));
        assert!(pure("ffprobe -v error -show_format out/reel.mp4"));
        assert!(pure("git status --short"));
        assert!(pure("git -C /x diff --stat"));
        assert!(pure("grep -rn 'a > b' src"));
        assert!(pure("sed -n '1,40p' f.rs"));
        assert!(pure("FOO=1 /usr/bin/find . -name '*.rs'"));
    }

    #[test]
    fn rejects_side_effects() {
        assert!(!pure(""));
        assert!(!pure("node render.mjs"));
        assert!(!pure("ffmpeg -y -i a.mp4 b.mp4"));
        assert!(!pure("ls > out.txt"));
        assert!(!pure("cat a | tee b"));
        assert!(!pure("ls && rm -rf x"));
        assert!(!pure("sed -i 's/a/b/' f"));
        assert!(!pure("find . -name x -delete"));
        assert!(!pure("find . -exec rm {} ;"));
        assert!(!pure("cat <<EOF\nhi\nEOF"));
        assert!(!pure("echo $(rm x)"));
        assert!(!pure("git commit -m x"));
        assert!(!pure("git branch -D x"));
        assert!(!pure("ls &> out"));
    }
}

#[cfg(test)]
mod sensitive_access_tests {
    use super::sensitive_shell_access_review as review;

    #[test]
    fn flags_db_clients_and_credential_reads() {
        assert!(review("mysql -h127.0.0.1 -uroot -e 'select 1'").is_some());
        assert!(review("echo x; MYSQL_PWD=a mysql -uroot db").is_some());
        assert!(review("psql postgres://u:p@h/db -c 'select 1'").is_some());
        assert!(review("cat .env").is_some());
        assert!(review("sed -n '1,12p' ~/.ssh/id_rsa").is_some());
        assert!(review("grep -i password config/.env.production").is_some());
        assert!(review("cd app && cat secrets.yml | head").is_some());
    }

    #[test]
    fn leaves_ordinary_commands_alone() {
        assert!(review("which mysql mariadb").is_none());
        assert!(review("mysql --version").is_none());
        assert!(review("cat src/main.rs").is_none());
        assert!(review("cat .env.example").is_none());
        assert!(review("cat ~/.ssh/id_rsa.pub").is_none());
        assert!(review("grep -rn password src/").is_none());
        assert!(review("cargo test && node --check a.js").is_none());
        // heredoc 正文里的脚本不算 shell 命令
        assert!(review("python3 - <<'PY'\nimport os\nmysql = 1\ncat = 2\nPY").is_none());
    }
}
