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

/// 双根围栏：工作区 + 可选会话 tmp 目录（tmp/ 前缀路径直接映射到会话 tmp 根）。
/// 会话 tmp 产物在物理上位于工作区外，缺了它审查者对 tmp 交付物永远够不到。
fn contained_multi(workspace: &Path, session_tmp: Option<&Path>, target: &Path) -> Option<PathBuf> {
    let ws = workspace.canonicalize().ok()?;
    // tmp/<rel> 显示命名空间 → 会话 tmp 物理根；优先于工作区解析
    // （工作区内可能存在同名 tmp/ 目录——如 src-tauri/tmp/ 历史遗留——读到错文件比读不到更糟）
    if let Some(tmp) = session_tmp {
        if let Ok(tmp_root) = tmp.canonicalize() {
            let raw = target.to_string_lossy();
            let stripped = raw
                .strip_prefix("tmp/")
                .or_else(|| raw.strip_prefix("./tmp/"));
            let tmp_absolute = match (target.is_absolute(), stripped) {
                (true, _) => target.to_path_buf(),
                (false, Some(rel)) => tmp_root.join(rel),
                (false, None) => tmp_root.join(target),
            };
            if let Ok(tt) = tmp_absolute.canonicalize() {
                if tt.starts_with(&tmp_root) {
                    // 非 tmp/ 前缀的相对路径不优先映射到 tmp 根，避免吞掉工作区相对路径
                    if target.is_absolute() || stripped.is_some() {
                        return Some(tt);
                    }
                }
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
            let mut out = String::new();
            for (idx, line) in content.lines().take(MAX_READ_LINES).enumerate() {
                out.push_str(&format!("{}: {}\n", idx + 1, line));
            }
            Ok(cap(out))
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
            let cwd = session_tmp.unwrap_or(workspace);
            run_captured("sh", &["-c", &cmd], cwd)
        }
        "python" => {
            let code = get_str("code");
            if code.is_empty() {
                return Err("python 需要 code 参数".to_string());
            }
            // 运行时只读护栏：进解释器前 patch 掉 open 写模式与 os/shutil 的写类函数，
            // 让意外/诱导式写操作显式抛错而非静默落盘。
            // 说明：这是对"注入材料诱导写盘/手滑误写"的机械底线，不是完整沙箱——
            // 刻意绕过（importlib 重载/ctypes）无法机械防住，边界由审查者提示词纪律承担。
            let cwd = session_tmp.unwrap_or(workspace);
            run_captured("python3", &["-c", PYTHON_GUARD_SRC, &code], cwd)
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

/// 只读 Python 护栏源码：在 exec 用户代码前 patch 掉 open 的写类模式与
/// os/shutil/pathlib 的常见写函数，使误写/诱导写显式抛 PermissionError。
/// 通过 argv 传递（python3 -c GUARD CODE），规避 shell 引号地狱。
const PYTHON_GUARD_SRC: &str = r#"
import builtins, os, shutil, sys, pathlib
_open = builtins.open
def _ro_open(f, mode='r', *a, **k):
    if any(c in mode for c in 'wax+'):
        raise PermissionError('reviewer 只读取证：禁止文件写入')
    return _open(f, mode, *a, **k)
builtins.open = _ro_open
def _blocked(*a, **k):
    raise PermissionError('reviewer 只读取证：禁止文件系统写操作')
for _fn in ('remove','unlink','rmdir','rename','replace','makedirs','mkdir',
            'chmod','chown','symlink','link','truncate','utime','removedirs',
            'renames','mktemp','mkfifo','mknod'):
    if hasattr(os, _fn):
        setattr(os, _fn, _blocked)
for _fn in ('rmtree','move','copy','copy2','copytree','copyfile','copyfileobj'):
    if hasattr(shutil, _fn):
        setattr(shutil, _fn, _blocked)
for _fn in ('write_text','write_bytes','mkdir','rmdir','unlink','rename',
            'replace','chmod','symlink_to','hardlink_to','touch'):
    if hasattr(pathlib.Path, _fn):
        setattr(pathlib.Path, _fn, _blocked)
exec(compile(sys.argv[1], '<reviewer>', 'exec'))
"#;

/// 运行子命令并合并 stdout/stderr，限时 15s（超时强杀），输出按 MAX_TOOL_OUTPUT_CHARS 截断。
/// 取证命令固定 cwd 在允许根内（session_tmp 优先——tmp 产物检查的默认工作目录）。
fn run_captured(program: &str, args: &[&str], cwd: &Path) -> Result<String, String> {
    use std::io::Read;
    use std::time::Instant;
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        // 最小环境：PATH 留给解释器/动态库解析，清掉代理与凭据类变量防外泄面
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()))
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("LANG", "C.UTF-8")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
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

    #[test]
    fn read_file_is_fenced_and_capped() {
        let dir = std::env::temp_dir().join(format!("rtools-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "line1\nline2\n").unwrap();

        let ok = execute(&dir, None, "read_file", &json!({"path": "a.txt"})).unwrap();
        assert!(ok.contains("line2"));

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
