//! reviewer 只读取证工具白名单：解除「信息同源」瓶颈。
//!
//! reviewer 此前是"瞎子"——它看到的世界完全由主 Agent 喂给，主 Agent 漏掉的
//! 代码路径它永远不知道存在（相关盲区），跨家族模型收益被吃掉大半。本模块给
//! reviewer 一个最小只读白名单（read_file/grep_search/git_diff/git_log/list_dir），
//! 让它拿着 findings 疑点自己去仓库核实。禁写、禁执行、路径围栏、输出截断。

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const MAX_TOOL_OUTPUT_CHARS: usize = 6000;
const MAX_GREP_MATCHES: usize = 30;
const MAX_READ_LINES: usize = 220;
const MAX_WALK_DEPTH: usize = 6;
const SKIP_DIRS: [&str; 6] = [".git", "target", "node_modules", "dist", ".writer-tmp", "build"];

fn contained(workspace: &Path, target: &Path) -> Option<PathBuf> {
    let ws = workspace.canonicalize().ok()?;
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
    let out = Command::new("git").args(args).current_dir(workspace).output().ok()?;
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

fn walk_match(
    root: &Path,
    pattern: &str,
    matches: &mut Vec<String>,
    depth: usize,
) {
    if depth > MAX_WALK_DEPTH || matches.len() >= MAX_GREP_MATCHES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        if matches.len() >= MAX_GREP_MATCHES {
            return;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if SKIP_DIRS.contains(&name.as_str()) || name.starts_with(".writer-") {
                continue;
            }
            walk_match(&path, pattern, matches, depth + 1);
        } else if is_source_text(&path)
            && path.metadata().map(|m| m.len() < 512 * 1024).unwrap_or(false)
        {
            if let Ok(content) = std::fs::read_to_string(&path) {
                let lower_pattern = pattern.to_lowercase();
                for (idx, line) in content.lines().enumerate() {
                    if line.to_lowercase().contains(&lower_pattern) {
                        matches.push(format!("{}:{}: {}", path.display(), idx + 1, line.trim()));
                        if matches.len() >= MAX_GREP_MATCHES {
                            return;
                        }
                    }
                }
            }
        }
    }
}

/// 执行一个只读取证工具。args 即 reviewer 输出的 JSON 对象（含 "tool" 键）。
pub fn execute(workspace: &Path, name: &str, args: &Value) -> Result<String, String> {
    let get_str = |key: &str| args.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
    match name {
        "read_file" => {
            let raw = get_str("path");
            if raw.is_empty() {
                return Err("read_file 需要 path 参数".to_string());
            }
            let target = contained(workspace, &Path::new(&raw))
                .ok_or_else(|| format!("路径越界（只能访问工作区内文件）: {}", raw))?;
            let content = std::fs::read_to_string(&target)
                .map_err(|e| format!("读取失败: {}", e))?;
            let mut out = String::new();
            for (idx, line) in content.lines().take(MAX_READ_LINES).enumerate() {
                out.push_str(&format!("{}: {}\n", idx + 1, line));
            }
            Ok(cap(out))
        }
        "grep_search" => {
            let pattern = get_str("pattern");
            if pattern.is_empty() {
                return Err("grep_search 需要 pattern 参数".to_string());
            }
            let mut matches = Vec::new();
            walk_match(workspace, &pattern, &mut matches, 0);
            if matches.is_empty() {
                Ok(format!("无匹配: {}", pattern))
            } else {
                Ok(cap(format!(
                    "匹配 {} 处:\n{}",
                    matches.len(),
                    matches.join("\n")
                )))
            }
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
                contained(workspace, &Path::new(&raw))
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
            "未知取证工具 {}；可用: read_file/grep_search/git_diff/git_log/list_dir",
            name
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_file_is_fenced_and_capped() {
        let dir = std::env::temp_dir().join(format!("rtools-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "line1\nline2\n").unwrap();

        let ok = execute(&dir, "read_file", &json!({"path": "a.txt"})).unwrap();
        assert!(ok.contains("line2"));

        // 路径围栏：越界拒绝
        let escaped = "/etc/hosts";
        assert!(execute(&dir, "read_file", &json!({"path": escaped})).is_err());

        // 未知工具拒绝
        assert!(execute(&dir, "write_file", &json!({})).is_err());

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
        let out = execute(&dir, "grep_search", &json!({"pattern": "dangling_pointer"})).unwrap();
        assert!(out.contains("lib.rs:1"), "grep 结果应带 文件:行号 定位: {}", out);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
