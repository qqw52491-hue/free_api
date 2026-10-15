//! reviewer 只读取证工具白名单：解除「信息同源」瓶颈。
//!
//! reviewer 此前是"瞎子"——它看到的世界完全由主 Agent 喂给，主 Agent 漏掉的
//! 代码路径它永远不知道存在（相关盲区），跨家族模型收益被吃掉大半。本模块给
//! reviewer 一个最小只读白名单（read_file/grep_search/git_diff/git_log/list_dir/
//! run_command/python），让它拿着 findings 疑点自己去仓库核实。禁写、路径围栏
//! （read_file/list_dir/grep_search 走 contained_multi；run_command 的词元级路径审计与
//! python 的解释器内护栏同口径限定工作区+会话 tmp 两根）、输出截断。

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const MAX_TOOL_OUTPUT_CHARS: usize = 6000;
const MAX_GREP_MATCHES: usize = 30;
const MAX_READ_LINES: usize = 220;
/// read_file 单页字符预算：220 行代码约 12~16k 字符；6000 会把一页砍到百行内且不提示。
const MAX_READ_CHARS: usize = 16_000;
// 限制工作量而非目录层数；达到上限必须披露搜索未完成。
const MAX_GREP_ENTRIES: usize = 50_000;
const MAX_GREP_FILE_BYTES: u64 = 512 * 1024;
const SKIP_DIRS: [&str; 6] = [".git", "target", "node_modules", "dist", ".writer-tmp", "build"];
/// 取证预算默认值与硬上限：每一步都是一次带完整历史的 LLM 调用，
/// 不设帽时失控配置会把单次审查成本放大到不可接受。
const DEFAULT_FORENSIC_BUDGET: u32 = 8;
const MAX_FORENSIC_BUDGET: u32 = 30;

/// reviewer 取证步数预算：channel-config.reviewer_tool_steps。
/// None = 默认 8；0 = 关闭取证（调用方据此裁剪提示词工具段，回到"瞎子"模式）；
/// 任何取值被 MAX_FORENSIC_BUDGET 封顶。child reviewer 与终审共用。
pub fn forensic_budget() -> u32 {
    crate::agent::channel::channel_config_shared()
        .read()
        .ok()
        .and_then(|cfg| cfg.reviewer_tool_steps)
        .unwrap_or(DEFAULT_FORENSIC_BUDGET)
        .min(MAX_FORENSIC_BUDGET)
}

/// 允许根集合：工作区 + 可选会话 tmp（均已 canonicalize）。
fn allowed_roots(workspace: &Path, session_tmp: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(w) = workspace.canonicalize() {
        roots.push(w);
    }
    if let Some(t) = session_tmp {
        if let Ok(c) = t.canonicalize() {
            roots.push(c);
        }
    }
    roots
}

/// 取证子进程注入的环境变量：SESSION_TMP（会话产物物理根，材料中 tmp/ 前缀的
/// 真实落点）与 REVIEW_ALLOWED_ROOTS（python 护栏的读路径围栏依据）。
fn forensic_envs(workspace: &Path, session_tmp: Option<&Path>) -> Vec<(String, String)> {
    let mut envs = Vec::new();
    let mut roots = vec![workspace.to_path_buf()];
    if let Some(tmp) = session_tmp {
        envs.push((
            "SESSION_TMP".to_string(),
            tmp.to_string_lossy().to_string(),
        ));
        roots.push(tmp.to_path_buf());
    }
    if let Ok(joined) = std::env::join_paths(roots.iter()) {
        envs.push((
            "REVIEW_ALLOWED_ROOTS".to_string(),
            joined.to_string_lossy().to_string(),
        ));
    }
    envs
}

/// 引号感知切词：引号内空白保持为单一词元（`cat "/a b"` 不能拆开审计）。
/// 不处理转义——$()/` 等命令替换已被上游只读白名单拒绝，剩余形态里
/// 反斜杠不构成路径语义。
fn shell_tokens(cmd: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in cmd.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    started = true;
                }
                c if c.is_whitespace() => {
                    if started || !cur.is_empty() {
                        tokens.push(std::mem::take(&mut cur));
                        started = false;
                    }
                }
                _ => cur.push(c),
            },
        }
    }
    if started || !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

/// run_command 的词元级路径围栏：shell 白名单只管命令动词不管路径参数，
/// `cat ~/.ssh/id_rsa`、`cat /etc/passwd`、`ls ../../` 这类"只读命令+越界路径"
/// 会把宿主任意文件内容回灌进审查上下文。此处把命令按词元扫描，凡能
/// canonicalize 为真实路径的词元（含 $SESSION_TMP/$HOME/~ 展开与 --flag=value
/// 右值）都必须落在允许根内；解析不了的词元读不到内容，放行。
/// 已知边界：词元扫描是启发式，挡不住命令自身的间接读（如 `git config
/// --get` 读 ~/.gitconfig）——残余面由审查者提示词纪律承担。
fn command_paths_fenced(
    cmd: &str,
    cwd: &Path,
    roots: &[PathBuf],
    session_tmp: Option<&Path>,
) -> Result<(), String> {
    let home = std::env::var("HOME").unwrap_or_default();
    for raw_tok in shell_tokens(cmd) {
        // VAR=value / --flag=value 的右值同样可能是路径
        for piece in raw_tok.split('=') {
            let mut tok = piece.trim().to_string();
            if tok.is_empty() {
                continue;
            }
            if let Some(tmp) = session_tmp {
                let tmp_str = tmp.to_string_lossy().to_string();
                tok = tok
                    .replace("${SESSION_TMP}", &tmp_str)
                    .replace("$SESSION_TMP", &tmp_str);
            }
            if !home.is_empty() {
                tok = tok.replace("${HOME}", &home).replace("$HOME", &home);
            }
            if tok == "~" || tok.starts_with("~/") {
                if home.is_empty() {
                    return Err("无法展开 ~ 路径（HOME 未设置）".to_string());
                }
                tok = format!("{}{}", home, &tok[1..]);
            } else if tok.starts_with('~') {
                // ~user 形态无法精确展开，保守拒绝
                return Err(format!("~user 形态路径不支持: {}", piece));
            }
            let looks_pathy =
                tok.starts_with('/') || tok == ".." || tok.starts_with("../") || tok.contains('/');
            if !looks_pathy {
                continue;
            }
            let abs = if Path::new(&tok).is_absolute() {
                PathBuf::from(&tok)
            } else {
                cwd.join(&tok)
            };
            let Ok(canon) = abs.canonicalize() else {
                continue; // 解析不了 = 该路径不存在，读不到内容
            };
            if canon == Path::new("/dev/null") {
                continue;
            }
            if !roots.iter().any(|r| canon.starts_with(r)) {
                return Err(format!(
                    "路径越界（命令中的路径限工作区与会话 tmp 内）: {}",
                    piece
                ));
            }
        }
    }
    Ok(())
}

/// 双根围栏：工作区 + 可选会话 tmp 目录（tmp/ 前缀路径直接映射到会话 tmp 根）。
/// 会话 tmp 产物在物理上位于工作区外，缺了它审查者对 tmp 交付物永远够不到。
fn contained_multi(workspace: &Path, session_tmp: Option<&Path>, target: &Path) -> Option<PathBuf> {
    let ws = workspace.canonicalize().ok()?;
    // tmp/<rel> 显示命名空间 → 会话 tmp 物理根；优先于工作区解析
    // （工作区内可能存在同名 tmp/ 目录——如 src-tauri/tmp/ 历史遗留——读到错文件比读不到更糟）
    if let Some(tmp) = session_tmp {
        if let Ok(tmp_root) = tmp.canonicalize() {
            let raw = target.to_string_lossy();
            let normalized = raw.strip_prefix("./").unwrap_or(&raw);
            // 裸 "tmp"/"tmp/" 也映射到会话 tmp 根（实测审查者 list_dir "tmp" 列产物区被拒）
            let stripped = normalized
                .strip_prefix("tmp/")
                .or_else(|| (normalized == "tmp").then_some(""));
            let tmp_absolute = match (target.is_absolute(), stripped) {
                (true, _) => target.to_path_buf(),
                (false, Some(rel)) => tmp_root.join(rel),
                (false, None) => tmp_root.join(target),
            };
            match tmp_absolute.canonicalize() {
                Ok(tt) if tt.starts_with(&tmp_root) => {
                    // 非 tmp/ 前缀的相对路径不优先映射到 tmp 根，避免吞掉工作区相对路径
                    if target.is_absolute() || stripped.is_some() {
                        return Some(tt);
                    }
                }
                // tmp/ 显式命名空间越界（tmp/../x）或指向不存在产物——
                // 属于 tmp 命名空间语义，拒绝而非落到工作区撞同名文件
                _ if stripped.is_some() => return None,
                _ => {}
            }
        }
    }
    // 相对路径必须先拼接工作区再解析，否则 canonicalize 会落到进程 CWD
    let absolute = if target.is_absolute() {
        target.to_path_buf()
    } else {
        ws.join(target)
    };
    let t = absolute.canonicalize().ok()?;
    if t.starts_with(&ws) {
        Some(t)
    } else {
        None
    }
}

fn cap(s: String) -> String {
    if s.chars().count() > MAX_TOOL_OUTPUT_CHARS {
        let head: String = s.chars().take(MAX_TOOL_OUTPUT_CHARS).collect();
        format!("{}\n…（输出已截断）", head)
    } else {
        s
    }
}

/// git 脏文件路径集：porcelain 输出的仓库根相对路径（含未跟踪）。
/// 供破坏性 git 命令按目标对账——目标不脏则 checkout --/restore 对其是
/// no-op，无未提交工作可销毁（committed 内容可从 HEAD 恢复）。
/// 返回 None = 非 git 目录/命令失败（调用方按保守方向处理）。
pub fn git_dirty_file_paths(cwd: &Path) -> Option<Vec<String>> {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.len() >= 3)
            .map(|l| {
                // "XY <path>"；"R  old -> new" 取 new；剥掉引号包裹
                let rest = &l[3..];
                let tail = rest.rsplit(" -> ").next().unwrap_or(rest);
                tail.trim().trim_matches('"').to_string()
            })
            .filter(|p| !p.is_empty())
            .collect(),
    )
}

/// 当前 HEAD commit sha（消失分类基准：HEAD 前移 = 本步发生 commit）。
pub fn git_head_sha(cwd: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// HEAD:<rel> 的 blob sha——文件在 HEAD 不存在时返回 None。
/// 与 git hash-object 的工作区指纹同口径，可证明「内容是否被 commit 收走」。
pub fn git_head_blob(cwd: &Path, rel: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", &format!("HEAD:{rel}")])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// stash 条数（消失分类基准：stash 增长 = 本步发生 stash 收纳）。
pub fn git_stash_count(cwd: &Path) -> Option<usize> {
    let out = Command::new("git")
        .args(["stash", "list"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count(),
    )
}

/// 文件当前内容对应的 git blob sha（与 HEAD:<rel> 产物同口径可比对）。
/// 文件不存在或命令失败返回 None。
pub fn git_worktree_blob(cwd: &Path, rel: &str) -> Option<String> {
    let abs = cwd.join(rel);
    if !abs.exists() {
        return None;
    }
    let out = Command::new("git")
        .args(["hash-object", rel])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// git 脏文件探针：统计工作区当前变更/未跟踪文件数（shell 前后各测一次，差值即本次
/// shell 真实改动的文件数）。非 git 目录返回 None（调用方退化为按调用次数计 1）。
pub fn git_dirty_file_count(workspace: &Path) -> Option<u32> {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count() as u32,
    )
}

fn git_args(workspace: &Path, args: &[&str]) -> Option<String> {
    let out = crate::agent::reviewer_gatekeeper::git_command(workspace).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

fn is_source_text(path: &Path) -> bool {
    let skip_ext = [
        "png", "jpg", "jpeg", "gif", "webp", "ico", "pdf", "zip", "gz", "tar", "bin", "woff",
        "woff2", "ttf", "mp4", "mp3", "lock",
    ];
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => !skip_ext.contains(&ext.to_ascii_lowercase().as_str()),
        None => true,
    }
}

#[derive(Default)]
struct GrepScan {
    matches: Vec<String>,
    files_searched: usize,
    entries_visited: usize,
    excluded_directories: usize,
    excluded_files: usize,
    skipped_symlinks: usize,
    oversized_files: usize,
    non_utf8_files: usize,
    read_errors: usize,
    unread_samples: Vec<String>,
    more_matches: bool,
    stop_reason: Option<&'static str>,
}

impl GrepScan {
    fn record_unread(&mut self, path: &Path, reason: &str) {
        if self.unread_samples.len() < 3 {
            self.unread_samples
                .push(format!("{}: {reason}", path.display()));
        }
    }
}

/// 不限制源码包层级；不跟随遍历中的链接，避免跨根读取和链接环。
fn walk_match(roots: &[PathBuf], pattern: &str, entry_limit: usize) -> GrepScan {
    let mut scan = GrepScan::default();
    let mut pending: Vec<PathBuf> = roots.iter().rev().cloned().collect();
    let mut visited_dirs = std::collections::HashSet::new();
    let needle = pattern.to_lowercase();
    'walk: while let Some(path) = pending.pop() {
        if scan.entries_visited >= entry_limit {
            scan.stop_reason.get_or_insert("entry_limit");
            break;
        }
        scan.entries_visited += 1;
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                scan.read_errors += 1;
                scan.record_unread(&path, "metadata_error");
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            scan.skipped_symlinks += 1;
            continue;
        }
        if metadata.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !roots.contains(&path) && (SKIP_DIRS.contains(&name) || name.starts_with(".writer-"))
            {
                scan.excluded_directories += 1;
                continue;
            }
            if !visited_dirs.insert(path.clone()) {
                continue;
            }
            match std::fs::read_dir(&path) {
                Ok(entries) => {
                    for entry in entries {
                        if scan.entries_visited + pending.len() >= entry_limit {
                            scan.stop_reason.get_or_insert("entry_limit");
                            break;
                        }
                        match entry {
                            Ok(entry) => pending.push(entry.path()),
                            Err(_) => {
                                scan.read_errors += 1;
                                scan.record_unread(&path, "directory_entry_error");
                            }
                        }
                    }
                }
                Err(_) => {
                    scan.read_errors += 1;
                    scan.record_unread(&path, "directory_read_error");
                }
            }
        } else if !metadata.is_file() || !is_source_text(&path) {
            scan.excluded_files += 1;
        } else if metadata.len() >= MAX_GREP_FILE_BYTES {
            scan.oversized_files += 1;
            scan.record_unread(&path, "file_size_limit");
        } else {
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    scan.files_searched += 1;
                    for (idx, line) in content.lines().enumerate() {
                        if line.to_lowercase().contains(&needle) {
                            // 第 31 条只作哨兵：30 条恰好是全集时不能谎报还有更多。
                            if scan.matches.len() == MAX_GREP_MATCHES {
                                scan.more_matches = true;
                                scan.stop_reason = Some("match_limit");
                                break 'walk;
                            }
                            scan.matches.push(format!(
                                "{}:{}: {}",
                                path.display(),
                                idx + 1,
                                line.trim()
                            ));
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                    scan.non_utf8_files += 1;
                    scan.record_unread(&path, "non_utf8");
                }
                Err(_) => {
                    scan.read_errors += 1;
                    scan.record_unread(&path, "file_read_error");
                }
            }
        }
    }
    scan
}

fn render_grep_scan(mut scan: GrepScan, roots: &[PathBuf]) -> Result<String, String> {
    let complete = scan.stop_reason.is_none()
        && scan.read_errors == 0
        && scan.oversized_files == 0
        && scan.non_utf8_files == 0;
    let matched_at_least = scan.matches.len() + usize::from(scan.more_matches);
    let mut output_truncated = false;
    loop {
        // null = 存在未扫描内容，不能判定是否还有匹配；不冒充 false。
        let has_more = if scan.more_matches || output_truncated {
            Some(true)
        } else if complete {
            Some(false)
        } else {
            None
        };
        let result = serde_json::json!({
            "scope": {"roots": roots, "mode":"case_insensitive_literal", "excluded_directory_names":SKIP_DIRS,
                "excluded_directory_prefixes":[".writer-"],
                "excluded_file_types":"known_binary_extensions_and_lock", "follow_symlinks":false,
                "max_file_bytes_exclusive":MAX_GREP_FILE_BYTES},
            "returned_matches":scan.matches.len(), "matched_at_least":matched_at_least,
            "has_more":has_more, "search_complete":complete, "output_truncated":output_truncated,
            "stop_reason":scan.stop_reason, "files_searched":scan.files_searched,
            "unread_samples":scan.unread_samples,
            "skipped":{"directories":scan.excluded_directories,"file_types_or_special":scan.excluded_files,
                "symlinks":scan.skipped_symlinks,"oversized_files":scan.oversized_files,
                "non_utf8_files":scan.non_utf8_files,"read_errors":scan.read_errors},
            "matches":scan.matches,
        }).to_string();
        if result.chars().count() <= MAX_TOOL_OUTPUT_CHARS {
            return Ok(result);
        }
        output_truncated = true;
        if scan.matches.len() > 1 {
            scan.matches.pop();
        } else if let Some(line) = scan.matches.last_mut() {
            let len = line.chars().count();
            if len > 80 {
                *line = line.chars().take(len / 2).collect();
            } else {
                scan.matches.pop();
            }
        } else {
            return Err("搜索范围元数据超过输出上限，请用 path 指定较小的目录或文件".into());
        }
    }
}

/// 执行一个只读取证工具。args 即 reviewer 输出的 JSON 对象（含 "tool" 键）。
/// session_tmp：会话产物物理根（tmp/<rel> 命名空间映射到它）；None 时退化为仅工作区。
pub fn execute(
    workspace: &Path,
    session_tmp: Option<&Path>,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    let get_str = |key: &str| args.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
    // 会话 tmp 目录可能尚未创建（无产物任务）——cwd/路径解析只对真实存在的根生效
    let session_tmp = session_tmp.filter(|p| p.is_dir());
    match name {
        "read_file" => {
            let raw = get_str("path");
            if raw.is_empty() {
                return Err("read_file 需要 path 参数".to_string());
            }
            // 可选 start_line：大文件翻页——实测审查者为看 220 行之后的内容
            // 重发同一请求白烧一步预算。
            let start_line = args
                .get("start_line")
                .and_then(|v| v.as_u64())
                .unwrap_or(1)
                .max(1) as usize;
            let target = contained_multi(workspace, session_tmp, &Path::new(&raw))
                .ok_or_else(|| format!("路径越界（只能访问工作区与会话 tmp 内文件）: {}", raw))?;
            // 办公/数据类二进制（xlsx/docx/pdf…）走解析器透出内容预览，
            // 直接 read_to_string 只会得到乱码或 UTF-8 解码失败
            if crate::agent::reviewer_gatekeeper::is_binary_file(&target.to_string_lossy()) {
                return match crate::agent::reviewer_gatekeeper::extract_office_preview(&target) {
                    Some(preview) => Ok(cap(format!(
                        "[二进制文件内容预览 {}]\n{}",
                        target.display(),
                        preview
                    ))),
                    None => Err(format!(
                        "二进制文件（{} 字节）无可读文本预览；深度检查请用 python 工具",
                        target.metadata().map(|m| m.len()).unwrap_or(0)
                    )),
                };
            }
            let content = std::fs::read_to_string(&target)
                .map_err(|e| format!("读取失败: {}", e))?;
            let total_lines = content.lines().count();
            let mut out = String::new();
            let mut last_shown = start_line - 1;
            for (idx, line) in content
                .lines()
                .enumerate()
                .filter(|(idx, _)| *idx >= start_line - 1)
                .take(MAX_READ_LINES)
            {
                let piece = format!("{}: {}\n", idx + 1, line);
                if !out.is_empty() && out.chars().count() + piece.chars().count() > MAX_READ_CHARS {
                    break;
                }
                out.push_str(&piece);
                last_shown = idx + 1;
            }
            if out.is_empty() && start_line > 1 {
                out = format!("（起始行 {} 超出文件总行数 {}）", start_line, total_lines);
            } else if last_shown < total_lines {
                // 明确告知"没读完"与续读位置，避免审查者把半个文件当全文下结论
                out.push_str(&format!(
                    "…（本页显示第 {}–{} 行，文件共 {} 行；未读完，用 start_line={} 继续）",
                    start_line, last_shown, total_lines, last_shown + 1
                ));
            }
            Ok(out)
        }
        "run_command" => {
            let cmd = get_str("command");
            if cmd.is_empty() {
                return Err("run_command 需要 command 参数".to_string());
            }
            // 只读白名单判定：ls/cat/grep/jq/stat 等经典只读命令放行；
            // 重定向/命令替换/写操作被 is_readonly_shell_command 天然拦截。
            let params = serde_json::json!({"command": cmd});
            if !crate::agent::context::audit::AuditLedger::is_readonly_shell_command(&params) {
                return Err(
                    "命令不在只读白名单内（可用: ls/cat/grep/rg/jq/stat/diff 等只读命令；\
                     数据文件深度检查请用 python 工具）"
                        .to_string(),
                );
            }
            // 词元级路径围栏：白名单只审命令动词，`cat /etc/passwd` 这类越界读在此拦截。
            let roots = allowed_roots(workspace, session_tmp);
            command_paths_fenced(&cmd, workspace, &roots, session_tmp)?;
            // cwd 固定为工作区（与 read_file 的相对路径语义一致）；
            // 会话产物经 $SESSION_TMP 环境变量可达——tmp/ 前缀是
            // read_file/list_dir 的虚拟命名空间，shell 不理解。
            let envs = forensic_envs(workspace, session_tmp);
            run_captured("sh", &["-c", &cmd], workspace, &envs)
        }
        "python" => {
            let code = get_str("code");
            if code.is_empty() {
                return Err("python 需要 code 参数".to_string());
            }
            // 运行时只读护栏：进解释器前 patch 掉 open 系（builtins/io/os/Path.open）
            // 的写模式与越界读、os/shutil 写类函数、进程派生与网络/FFI 模块，
            // 让意外/诱导式操作显式抛错而非静默生效。
            // 说明：这是对"注入材料诱导写盘/手滑误写"的机械底线，不是完整沙箱——
            // 刻意绕过（importlib 重载/sys.modules 摘除）无法机械防住，
            // 边界由审查者提示词纪律承担。
            // cwd 固定为工作区；会话产物经 $SESSION_TMP 环境变量可达。
            let envs = forensic_envs(workspace, session_tmp);
            run_captured("python3", &["-c", PYTHON_GUARD_SRC, &code], workspace, &envs)
        }
        "grep_search" => {
            let pattern = get_str("pattern");
            if pattern.trim().is_empty() {
                return Err("grep_search 需要非空 pattern 参数".to_string());
            }
            let roots = match args.get("path") {
                None => allowed_roots(workspace, session_tmp),
                Some(Value::String(path)) if !path.trim().is_empty() => vec![
                    contained_multi(workspace, session_tmp, Path::new(path))
                        .ok_or_else(|| format!("搜索路径不存在、不可访问或超出工作区/会话 tmp 范围: {path}"))?
                ],
                _ => return Err("grep_search.path 必须为非空字符串；省略时搜索工作区与会话 tmp".into()),
            };
            if roots.is_empty() { return Err("没有可访问的搜索根目录".into()); }
            render_grep_scan(walk_match(&roots, &pattern, MAX_GREP_ENTRIES), &roots)
        }
        "git_diff" => {
            let diff = git_args(workspace, &["diff", "HEAD"])
                .or_else(|| git_args(workspace, &["diff"]));
            match diff {
                Some(d) if !d.trim().is_empty() => Ok(cap(d)),
                _ => Ok("（当前无 git diff）".to_string()),
            }
        }
        "git_log" => {
            let n = args.get("n").and_then(|v| v.as_u64()).unwrap_or(10).min(30);
            Ok(cap(
                git_args(workspace, &["log", "--oneline", &format!("-{}", n)])
                    .unwrap_or_else(|| "（非 git 仓库）".to_string()),
            ))
        }
        "list_dir" => {
            let raw = get_str("path");
            let target = if raw.is_empty() {
                workspace.to_path_buf()
            } else {
                contained_multi(workspace, session_tmp, &Path::new(&raw))
                    .ok_or_else(|| format!("路径越界: {}", raw))?
            };
            let mut entries: Vec<String> = Vec::new();
            if let Ok(read) = std::fs::read_dir(&target) {
                for e in read.flatten().take(100) {
                    let kind = if e.path().is_dir() { "dir" } else { "file" };
                    entries.push(format!("[{}] {}", kind, e.file_name().to_string_lossy()));
                }
            }
            Ok(cap(format!(
                "{}:\n{}",
                target.display(),
                entries.join("\n")
            )))
        }
        _ => Err(format!(
            "未知取证工具 {}；可用: read_file/grep_search/git_diff/git_log/list_dir/run_command/python",
            name
        )),
    }
}

/// 只读 Python 护栏源码：在 exec 用户代码前 patch 掉 open 系入口的写模式与
/// 越界读、os/shutil/pathlib 的写类函数、进程派生函数，并预阻塞
/// subprocess/socket/ctypes 等模块。通过 argv 传递（python3 -c GUARD CODE），
/// 规避 shell 引号地狱。读路径围栏依据 REVIEW_ALLOWED_ROOTS（冒号分隔）。
const PYTHON_GUARD_SRC: &str = r#"
import builtins, io, os, shutil, sys, pathlib
# 读路径围栏：白名单 = REVIEW_ALLOWED_ROOTS（工作区+会话 tmp）+ 解释器库目录
# + 系统区（/etc、/usr、/opt、/System、/Library 等——解释器 import 链会读
# mime.types/证书/源码；实测 openpyxl import 因 /etc/mime.types 被拒而整体失败）。
# 拒绝域 = 所有用户家目录（~/.ssh、~/.aws、~/Library 凭据面）中不在白名单的部分。
_ROOTS = [os.path.realpath(p) for p in os.environ.get('REVIEW_ALLOWED_ROOTS', '').split(os.pathsep) if p]
_ROOTS += [os.path.realpath(p) for p in sys.path if p]
_HOME = os.path.realpath(os.path.expanduser('~'))
_HOME_PARENT = os.path.dirname(_HOME)  # /Users 或 /home：所有用户家目录一并拒绝
_SYS_OK = []
for _p in ('/etc', '/usr', '/opt', '/System', '/Library', '/bin', '/sbin', '/dev', '/tmp'):
    if os.path.exists(_p):
        _SYS_OK.append(os.path.realpath(_p))
def _in_roots(p):
    try:
        rp = os.path.realpath(os.fspath(p))
    except Exception:
        return False
    for r in _ROOTS:
        if rp == r or rp.startswith(r + os.sep):
            return True
    if _HOME_PARENT and rp.startswith(_HOME_PARENT + os.sep):
        return False
    for r in _SYS_OK:
        if rp == r or rp.startswith(r + os.sep):
            return True
    return False
def _blocked(*a, **k):
    raise PermissionError('reviewer 只读取证：禁止写操作/进程派生')
_open = builtins.open
def _ro_open(f, mode='r', *a, **k):
    if isinstance(mode, str) and any(c in mode for c in 'wax+'):
        raise PermissionError('reviewer 只读取证：禁止文件写入')
    if not _in_roots(f):
        raise PermissionError('reviewer 取证：路径越界（限工作区与会话 tmp 内文件）')
    return _open(f, mode, *a, **k)
builtins.open = _ro_open
io.open = _ro_open  # pathlib.Path.open/zipfile 等内部走 io.open，必须同步 patch
def _ro_path_open(self, mode='r', *a, **k):
    return _ro_open(os.fspath(self), mode, *a, **k)
pathlib.Path.open = _ro_path_open
if hasattr(os, 'open'):  # 低级 fd 入口：os.open(fd, O_WRONLY) 绕过 builtins.open
    _os_open = os.open
    def _ro_os_open(path, flags, *a, **k):
        _WR = getattr(os, 'O_WRONLY', 1) | getattr(os, 'O_RDWR', 2) \
            | getattr(os, 'O_CREAT', 64) | getattr(os, 'O_TRUNC', 512) \
            | getattr(os, 'O_APPEND', 1024)
        if flags & _WR:
            raise PermissionError('reviewer 只读取证：禁止文件写入')
        if not _in_roots(path):
            raise PermissionError('reviewer 取证：路径越界（限工作区与会话 tmp 内文件）')
        return _os_open(path, flags, *a, **k)
    os.open = _ro_os_open
for _fn in ('remove','unlink','rmdir','rename','replace','makedirs','mkdir',
            'chmod','chown','symlink','link','truncate','utime','removedirs',
            'renames','mktemp','mkfifo','mknod','system','popen','kill','killpg',
            'fork','forkpty','posix_spawn','posix_spawnp','startfile'):
    if hasattr(os, _fn):
        setattr(os, _fn, _blocked)
for _name in dir(os):  # spawn*/exec* 全家族（execv/execve/spawnl/spawnv…）
    if _name.startswith(('spawn', 'exec')):
        setattr(os, _name, _blocked)
for _fn in ('rmtree','move','copy','copy2','copytree','copyfile','copyfileobj'):
    if hasattr(shutil, _fn):
        setattr(shutil, _fn, _blocked)
for _fn in ('write_text','write_bytes','mkdir','rmdir','unlink','rename',
            'replace','chmod','symlink_to','hardlink_to','touch'):
    if hasattr(pathlib.Path, _fn):
        setattr(pathlib.Path, _fn, _blocked)
# 进程派生/网络/FFI 模块整体预阻塞：sys.modules 置 None 使 import 直接 ImportError
for _mod in ('subprocess', 'pty', 'socket', 'ctypes', 'multiprocessing', '_posixsubprocess'):
    sys.modules[_mod] = None
exec(compile(sys.argv[1], '<reviewer>', 'exec'))
"#;

/// 运行子命令并合并 stdout/stderr，限时 15s（超时强杀），输出按 MAX_TOOL_OUTPUT_CHARS 截断。
/// 取证命令 cwd 固定为工作区（与 read_file 相对路径语义一致）；
/// 会话产物经 extra_envs 注入的 SESSION_TMP 可达。
fn run_captured(
    program: &str,
    args: &[&str],
    cwd: &Path,
    extra_envs: &[(String, String)],
) -> Result<String, String> {
    use std::io::Read;
    use std::time::Instant;
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        // 最小环境：PATH 留给解释器/动态库解析，清掉代理与凭据类变量防外泄面
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()))
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("LANG", "C.UTF-8")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (k, v) in extra_envs {
        command.env(k, v);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("命令启动失败: {}", e))?;
    let deadline = Instant::now() + std::time::Duration::from_secs(15);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                return Err("命令超时（>15s），已强制终止".to_string());
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(e) => return Err(format!("等待命令失败: {}", e)),
        }
    }
    let mut out = String::new();
    if let Some(mut so) = child.stdout.take() {
        let mut buf = String::new();
        let _ = so.read_to_string(&mut buf);
        out.push_str(&buf);
    }
    if let Some(mut se) = child.stderr.take() {
        let mut buf = String::new();
        let _ = se.read_to_string(&mut buf);
        if !buf.trim().is_empty() {
            out.push_str(&format!("\n[stderr]\n{}", buf));
        }
    }
    if out.trim().is_empty() {
        out = "（命令无输出）".to_string();
    }
    Ok(cap(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct GrepFixture(PathBuf);

    impl GrepFixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("review-search-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for GrepFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn grep_deep_java_source_is_searchable_from_root_and_subdirectory() {
        let fixture = GrepFixture::new();
        let relative = "adp-iam/src/main/java/cn/com/example/iam/security/AuthInterceptor.java";
        let file = fixture.0.join(relative);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            "public class AuthInterceptor {\n boolean preHandle() { return true; }\n}\n",
        )
        .unwrap();
        for path in [None, Some("adp-iam/src/main/java"), Some(relative)] {
            let mut args = json!({"pattern":"preHandle"});
            if let Some(path) = path {
                args["path"] = json!(path);
            }
            let out = execute(&fixture.0, None, "grep_search", &args).unwrap();
            assert!(out.contains("AuthInterceptor.java:2"), "{path:?}: {out}");
        }
    }

    #[test]
    fn grep_path_restricts_matches_instead_of_searching_the_whole_workspace() {
        let fixture = GrepFixture::new();
        std::fs::create_dir_all(fixture.0.join("selected")).unwrap();
        std::fs::write(fixture.0.join("selected/in.java"), "SCOPE_NEEDLE\n").unwrap();
        std::fs::write(fixture.0.join("outside.java"), "SCOPE_NEEDLE\n").unwrap();
        let out = execute(
            &fixture.0,
            None,
            "grep_search",
            &json!({"pattern":"SCOPE_NEEDLE","path":"selected"}),
        )
        .unwrap();
        assert!(
            out.contains("in.java:1") && !out.contains("outside.java"),
            "{out}"
        );
    }

    #[test]
    fn grep_defaults_to_both_roots_but_explicit_paths_select_one_scope() {
        let workspace = GrepFixture::new();
        let tmp = GrepFixture::new();
        let outside = GrepFixture::new();
        std::fs::write(workspace.0.join("ws.txt"), "shared-needle\n").unwrap();
        std::fs::write(tmp.0.join("report.md"), "shared-needle\n").unwrap();
        std::fs::write(outside.0.join("secret.txt"), "shared-needle\n").unwrap();
        for (path, count) in [
            (None, 2),
            (Some("."), 1),
            (Some("tmp"), 1),
            (Some("tmp/report.md"), 1),
        ] {
            let mut args = json!({"pattern":"SHARED-NEEDLE"});
            if let Some(path) = path {
                args["path"] = json!(path);
            }
            let out: Value = serde_json::from_str(
                &execute(&workspace.0, Some(&tmp.0), "grep_search", &args).unwrap(),
            )
            .unwrap();
            assert_eq!(out["returned_matches"], count, "{out}");
            assert_eq!(out["search_complete"], true, "{out}");
        }
        for path in [
            outside.0.join("secret.txt").to_string_lossy().to_string(),
            "tmp/../secret.txt".into(),
            "missing".into(),
        ] {
            assert!(execute(
                &workspace.0,
                Some(&tmp.0),
                "grep_search",
                &json!({"pattern":"needle","path":path})
            )
            .is_err());
        }
        for args in [
            json!({"pattern":" "}),
            json!({"pattern":"x","path":null}),
            json!({"pattern":"x","path":1}),
        ] {
            assert!(execute(&workspace.0, Some(&tmp.0), "grep_search", &args).is_err());
        }
    }

    #[test]
    fn grep_uses_an_extra_match_to_distinguish_exact_limit_from_truncation() {
        let fixture = GrepFixture::new();
        for count in [0, MAX_GREP_MATCHES, MAX_GREP_MATCHES + 1] {
            std::fs::write(fixture.0.join("hits.txt"), "needle\n".repeat(count)).unwrap();
            let out = execute(
                &fixture.0,
                None,
                "grep_search",
                &json!({"pattern":"needle"}),
            )
            .unwrap();
            assert!(out.chars().count() <= MAX_TOOL_OUTPUT_CHARS);
            let out: Value = serde_json::from_str(&out).unwrap();
            assert_eq!(out["returned_matches"], count.min(MAX_GREP_MATCHES));
            assert_eq!(out["matched_at_least"], count);
            assert_eq!(out["has_more"], count > MAX_GREP_MATCHES);
            assert_eq!(out["search_complete"], count <= MAX_GREP_MATCHES);
        }
    }

    #[test]
    fn grep_output_cap_keeps_valid_metadata_and_a_locatable_match() {
        let fixture = GrepFixture::new();
        std::fs::write(
            fixture.0.join("long.java"),
            format!("needle {}\n", "汉字".repeat(8000)),
        )
        .unwrap();
        let raw = execute(
            &fixture.0,
            None,
            "grep_search",
            &json!({"pattern":"needle"}),
        )
        .unwrap();
        assert!(raw.chars().count() <= MAX_TOOL_OUTPUT_CHARS);
        let out: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(out["matched_at_least"], 1);
        assert_eq!(out["output_truncated"], true);
        assert_eq!(out["has_more"], true);
        assert!(out["matches"][0].as_str().unwrap().contains("long.java:1"));
    }

    #[test]
    fn grep_reports_unread_text_and_resource_limits_instead_of_complete_absence() {
        let fixture = GrepFixture::new();
        std::fs::write(fixture.0.join("encoded.java"), [0xff, 0xfe]).unwrap();
        std::fs::write(
            fixture.0.join("large.java"),
            vec![b'x'; MAX_GREP_FILE_BYTES as usize],
        )
        .unwrap();
        let raw = execute(
            &fixture.0,
            None,
            "grep_search",
            &json!({"pattern":"needle"}),
        )
        .unwrap();
        let out: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(out["returned_matches"], 0);
        assert_eq!(out["search_complete"], false);
        assert!(out["has_more"].is_null());
        assert_eq!(out["skipped"]["non_utf8_files"], 1);
        assert_eq!(out["skipped"]["oversized_files"], 1);
        assert!(out["unread_samples"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s.as_str().unwrap().contains("encoded.java")));
        assert!(out["unread_samples"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s.as_str().unwrap().contains("large.java")));
        let roots = vec![fixture.0.clone()];
        let out: Value =
            serde_json::from_str(&render_grep_scan(walk_match(&roots, "needle", 1), &roots).unwrap())
                .unwrap();
        assert_eq!(out["stop_reason"], "entry_limit");
        assert_eq!(out["search_complete"], false);
        assert!(out["has_more"].is_null());
    }

    #[cfg(unix)]
    #[test]
    fn grep_never_follows_discovered_symlinks_outside_roots_or_into_cycles() {
        let fixture = GrepFixture::new();
        let outside = GrepFixture::new();
        std::fs::write(fixture.0.join("inside.txt"), "needle-inside\n").unwrap();
        std::fs::write(outside.0.join("secret.txt"), "needle-outside\n").unwrap();
        std::os::unix::fs::symlink(&outside.0, fixture.0.join("outside-dir")).unwrap();
        std::os::unix::fs::symlink(outside.0.join("secret.txt"), fixture.0.join("outside-file"))
            .unwrap();
        std::os::unix::fs::symlink(".", fixture.0.join("cycle")).unwrap();
        let raw = execute(
            &fixture.0,
            None,
            "grep_search",
            &json!({"pattern":"needle"}),
        )
        .unwrap();
        let out: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(out["returned_matches"], 1);
        assert_eq!(out["skipped"]["symlinks"], 3);
        assert!(!raw.contains("needle-outside"));
        assert!(execute(
            &fixture.0,
            None,
            "grep_search",
            &json!({"pattern":"needle","path":"outside-dir"})
        )
        .is_err());
    }

    #[test]
    fn read_file_is_fenced_and_capped() {
        let dir = std::env::temp_dir().join(format!("rtools-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "line1\nline2\n").unwrap();

        let ok = execute(&dir, None, "read_file", &json!({"path": "a.txt"})).unwrap();
        assert!(ok.contains("line2"));
        assert!(!ok.contains("未读完"));

        // 超过单页行数：必须提示续读位置
        let big: String = (1..=300).map(|i| format!("row{}\n", i)).collect();
        std::fs::write(dir.join("big.txt"), big).unwrap();
        let page = execute(&dir, None, "read_file", &json!({"path": "big.txt"})).unwrap();
        assert!(page.contains("220: row220") && page.contains("start_line=221"), "{}", page);
        let page2 = execute(&dir, None, "read_file", &json!({"path": "big.txt", "start_line": 221})).unwrap();
        assert!(page2.contains("300: row300") && !page2.contains("未读完"));

        // 路径围栏：越界拒绝
        let escaped = "/etc/hosts";
        assert!(execute(&dir, None, "read_file", &json!({"path": escaped})).is_err());

        // 未知工具拒绝
        assert!(execute(&dir, None, "write_file", &json!({})).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tmp_namespace_resolves_to_session_tmp() {
        // 双根围栏：tmp/<rel> 映射到会话 tmp 物理根，普通相对路径走工作区
        let ws = std::env::temp_dir().join(format!("rtools-ws-{}", uuid::Uuid::new_v4()));
        let tmp = std::env::temp_dir().join(format!("rtools-tmp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("report.md"), "tmp-artifact-body").unwrap();
        std::fs::write(ws.join("src.rs"), "ws-body").unwrap();

        // tmp/ 显示命名空间 → 会话 tmp
        let out = execute(&ws, Some(&tmp), "read_file", &json!({"path": "tmp/report.md"})).unwrap();
        assert!(out.contains("tmp-artifact-body"));
        // 会话 tmp 绝对路径也放行
        let abs = tmp.join("report.md").to_string_lossy().to_string();
        let out2 = execute(&ws, Some(&tmp), "read_file", &json!({"path": abs})).unwrap();
        assert!(out2.contains("tmp-artifact-body"));
        // 工作区相对路径照常
        let out3 = execute(&ws, Some(&tmp), "read_file", &json!({"path": "src.rs"})).unwrap();
        assert!(out3.contains("ws-body"));
        // 工作区外且非会话 tmp → 拒绝
        assert!(execute(&ws, Some(&tmp), "read_file", &json!({"path": "/etc/hosts"})).is_err());
        // tmp/../ 逃逸 → 拒绝
        assert!(execute(&ws, Some(&tmp), "read_file", &json!({"path": "tmp/../a.txt"})).is_err());
        // 裸 tmp / tmp/ / ./tmp 均映射到会话 tmp 根（list_dir 列产物区）
        for p in ["tmp", "tmp/", "./tmp", "./tmp/"] {
            let out = execute(&ws, Some(&tmp), "list_dir", &json!({"path": p})).unwrap();
            assert!(out.contains("report.md"), "list_dir {p} 应列出会话 tmp 产物");
        }
        // tmp/ 命名空间内不存在的产物 → 拒绝（不落工作区撞同名路径）
        assert!(execute(&ws, Some(&tmp), "read_file", &json!({"path": "tmp/ghost.txt"})).is_err());
        // session_tmp 为 None 时 tmp/ 前缀不映射，工作区内无此文件则失败
        assert!(execute(&ws, None, "read_file", &json!({"path": "tmp/report.md"})).is_err());

        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn run_command_readonly_whitelist() {
        let dir = std::env::temp_dir().join(format!("rtools-cmd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.txt"), "hello-cmd").unwrap();

        // 只读命令放行
        let out = execute(&dir, None, "run_command", &json!({"command": "cat f.txt"})).unwrap();
        assert!(out.contains("hello-cmd"));

        // 写命令 / 重定向 / 命令替换 / 删除 → 全部拒绝
        assert!(execute(&dir, None, "run_command", &json!({"command": "echo x > f.txt"})).is_err());
        assert!(execute(&dir, None, "run_command", &json!({"command": "rm f.txt"})).is_err());
        assert!(execute(&dir, None, "run_command", &json!({"command": "cat f.txt | tee g.txt"})).is_err());
        assert!(execute(&dir, None, "run_command", &json!({"command": "cat $(cat f.txt)"})).is_err());
        assert!(execute(&dir, None, "run_command", &json!({"command": "mv f.txt g.txt"})).is_err());
        assert!(execute(&dir, None, "run_command", &json!({"command": "touch g.txt"})).is_err());
        // 确认文件未被破坏
        assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "hello-cmd");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_command_path_fence_and_session_tmp_env() {
        let ws = std::env::temp_dir().join(format!("rtools-fws-{}", uuid::Uuid::new_v4()));
        let tmp = std::env::temp_dir().join(format!("rtools-ftmp-{}", uuid::Uuid::new_v4()));
        let outside = std::env::temp_dir().join(format!("rtools-fout-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(ws.join("w.txt"), "ws-body").unwrap();
        std::fs::write(tmp.join("art.csv"), "a,b,c").unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();

        // $SESSION_TMP 展开后可读会话产物（cwd 为工作区，tmp/ 前缀对 shell 无意义）
        let out = execute(&ws, Some(&tmp), "run_command",
            &json!({"command": "cat $SESSION_TMP/art.csv"})).unwrap();
        assert!(out.contains("a,b,c"), "SESSION_TMP 展开应可读: {}", out);
        let out = execute(&ws, Some(&tmp), "run_command",
            &json!({"command": "cat \"$SESSION_TMP/art.csv\""})).unwrap();
        assert!(out.contains("a,b,c"));
        // 工作区相对路径照常
        let out = execute(&ws, Some(&tmp), "run_command",
            &json!({"command": "cat w.txt"})).unwrap();
        assert!(out.contains("ws-body"));

        // 越界路径：绝对路径 / ~ / .. 逃逸 / $HOME / --flag=value 右值 → 全部拒绝
        let secret_abs = outside.join("secret.txt").to_string_lossy().to_string();
        for cmd in [
            format!("cat {}", secret_abs),
            format!("cat {}", outside.to_string_lossy()),
            "cat /etc/hosts".to_string(),
            "ls ~".to_string(),
            "cat ~/.".to_string(),
            "ls ..".to_string(),
            "cat $HOME".to_string(),
            format!("head --file={}", secret_abs),
        ] {
            assert!(
                execute(&ws, Some(&tmp), "run_command", &json!({"command": cmd})).is_err(),
                "越界命令应被拒绝: {}",
                cmd
            );
        }

        // 无 SESSION_TMP 时 $SESSION_TMP 不展开——残留字面量路径解析不了，读不到内容，放行即可
        let out = execute(&ws, None, "run_command",
            &json!({"command": "cat w.txt"})).unwrap();
        assert!(out.contains("ws-body"));

        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn python_guard_blocks_writes_but_allows_reads() {
        // 环境无 python3 时跳过（CI/容器可能缺解释器）
        if Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("rtools-py-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("data.txt"), "py-read-ok").unwrap();

        // 读取放行
        let out = execute(
            &dir, None, "python",
            &json!({"code": "print(open('data.txt').read())"}),
        )
        .unwrap();
        assert!(out.contains("py-read-ok"), "python 读文件应放行: {}", out);

        // open 写模式 → 护栏拦截
        let w = execute(
            &dir, None, "python",
            &json!({"code": "open('evil.txt','w').write('x')"}),
        );
        assert!(w.is_err() || w.as_ref().unwrap().contains("PermissionError") || w.as_ref().unwrap().contains("只读"),
            "写 open 应被护栏拦截: {:?}", w);
        assert!(!dir.join("evil.txt").exists(), "evil.txt 不应被创建");

        // os.remove / os.makedirs / shutil.copy → 护栏拦截
        let rm = execute(&dir, None, "python", &json!({"code": "import os; os.remove('data.txt')"}));
        assert!(rm.is_err() || rm.as_ref().unwrap().contains("PermissionError") || rm.as_ref().unwrap().contains("只读"));
        assert!(dir.join("data.txt").exists(), "data.txt 不应被删除");

        // os.system / subprocess / Path.open 写 / 越界读 → 全部拦截
        let sys = execute(
            &dir, None, "python", &json!({"code": "import os; os.system('touch hacked.txt')"}));
        assert!(sys.is_err() || sys.as_ref().unwrap().contains("PermissionError") || sys.as_ref().unwrap().contains("只读"),
            "os.system 应被拦截: {:?}", sys);
        assert!(!dir.join("hacked.txt").exists());
        let sp = execute(
            &dir, None, "python", &json!({"code": "import subprocess"}));
        assert!(sp.is_err() || sp.as_ref().unwrap().contains("ImportError") || sp.as_ref().unwrap().contains("import"),
            "subprocess import 应被拦截: {:?}", sp);
        let pw = execute(
            &dir, None, "python",
            &json!({"code": "from pathlib import Path; Path('evil2.txt').open('w').write('x')"}));
        assert!(pw.is_err() || pw.as_ref().unwrap().contains("PermissionError") || pw.as_ref().unwrap().contains("只读"),
            "Path.open 写模式应被拦截: {:?}", pw);
        assert!(!dir.join("evil2.txt").exists());
        // 系统区可读（解释器 import 链需要 /etc/mime.types 等），家目录拒绝
        let sysread = execute(
            &dir, None, "python",
            &json!({"code": "from pathlib import Path; print(len(Path('/etc/hosts').read_text()))"}));
        assert!(sysread.is_ok(), "系统区文件应可读: {:?}", sysread);
        let homeread = execute(
            &dir, None, "python",
            &json!({"code": "import os; open(os.path.expanduser('~/.sentinel-secret')).read()"}));
        assert!(homeread.is_err() || homeread.as_ref().unwrap().contains("PermissionError") || homeread.as_ref().unwrap().contains("越界"),
            "家目录读应被拦截（无论文件是否存在）: {:?}", homeread);

        // $SESSION_TMP 内文件可读
        let tmp = std::env::temp_dir().join(format!("rtools-pyt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("art.txt"), "tmp-art").unwrap();
        let out = execute(
            &dir, Some(&tmp), "python",
            &json!({"code": "import os; print(open(os.environ['SESSION_TMP']+'/art.txt').read())"}),
        ).unwrap();
        assert!(out.contains("tmp-art"), "SESSION_TMP 内文件应可读: {}", out);
        let _ = std::fs::remove_dir_all(&tmp);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_dirty_probe_counts_new_files() {
        let dir = std::env::temp_dir().join(format!("rtools-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = Command::new("git").args(["init", "-q"]).current_dir(&dir).status();
        let _ = Command::new("git").args(["-c", "user.email=t@t", "-c", "user.name=t", "add", "-A"]).current_dir(&dir).status();
        let _ = Command::new("git").args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init"]).current_dir(&dir).status();
        assert_eq!(git_dirty_file_count(&dir), Some(0), "干净仓库应为 0");
        std::fs::write(dir.join("new.rs"), "fn x() {}
").unwrap();
        assert_eq!(git_dirty_file_count(&dir), Some(1), "新增文件应计 1");
        // 非 git 目录返回 None
        let plain = std::env::temp_dir().join(format!("rtools-plain-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(git_dirty_file_count(&plain), None);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&plain);
    }

    #[test]
    fn grep_finds_matches_with_locations() {
        let dir = std::env::temp_dir().join(format!("rtools-g-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "fn dangling_pointer() {}\n").unwrap();
        let out = execute(&dir, None, "grep_search", &json!({"pattern": "dangling_pointer"})).unwrap();
        assert!(out.contains("lib.rs:1"), "grep 结果应带 文件:行号 定位: {}", out);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
