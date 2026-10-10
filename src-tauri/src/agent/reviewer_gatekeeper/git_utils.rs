//! git 探测与只读命令执行。

use super::*;
use crate::agent::context::SandwichContext;
use crate::agent::router::{AgentRouter, ModelRoutingConfig, roles};
use crate::agent::tools::role_manager::resolve_enabled_card;
use crate::db::DbState;
use crate::llm::{self, ModelConfig};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;


pub(crate) fn real_git_repo(workspace: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(workspace)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 影子仓库根目录：只放在应用数据目录里，绝不往用户目录里写 `.git`。
fn shadow_root() -> std::path::PathBuf {
    #[cfg(test)]
    {
        std::env::temp_dir().join(format!("free-api-shadow-test-{}", std::process::id()))
    }
    #[cfg(not(test))]
    {
        dirs_next::data_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("free-api-chat")
            .join("shadow")
    }
}

pub(crate) fn shadow_git_dir(workspace: &Path) -> std::path::PathBuf {
    let canon = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canon.to_string_lossy().as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    shadow_root().join(&hex[..16])
}

fn shadow_ready(workspace: &Path) -> bool {
    shadow_git_dir(workspace).join("HEAD").exists()
}

pub(crate) fn is_git_repo(workspace: &Path) -> bool {
    real_git_repo(workspace) || shadow_ready(workspace)
}

/// 构造 git 命令；非 git 目录且已有影子仓库时，指向影子仓库（GIT_DIR + GIT_WORK_TREE）。
pub(crate) fn git_command(workspace: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(workspace);
    if shadow_ready(workspace) && !real_git_repo(workspace) {
        cmd.env("GIT_DIR", shadow_git_dir(workspace))
            .env("GIT_WORK_TREE", workspace);
    }
    cmd
}

pub(crate) fn git_cmd(workspace: &Path, args: &[&str]) -> Option<String> {
    let output = git_command(workspace).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

const SHADOW_SKIP_DIRS: &[&str] = &[
    "node_modules", "target", ".git", "dist", "build", ".venv", "venv", "__pycache__", ".next", ".idea",
];
const SHADOW_MAX_FILES: usize = 8000;
const SHADOW_MAX_BYTES: u64 = 300 * 1024 * 1024;

/// 目录是否小到值得做影子快照（跳过依赖/产物目录后统计文件数与总大小）。
fn within_shadow_budget(root: &Path) -> bool {
    let mut files = 0usize;
    let mut bytes = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !SHADOW_SKIP_DIRS.contains(&name.as_str()) {
                    stack.push(entry.path());
                }
            } else {
                files += 1;
                bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
                if files > SHADOW_MAX_FILES || bytes > SHADOW_MAX_BYTES {
                    return false;
                }
            }
        }
    }
    true
}

/// 非 git 工作区：在应用数据目录建「影子仓库」并提交一次当前状态，作为本次任务的起点。
/// 返回是否成功建立快照；失败（无 git、目录过大、危险目录等）时返回 false，调用方退回 mtime 扫描。
pub(crate) fn ensure_shadow_snapshot(workspace: &Path) -> bool {
    if real_git_repo(workspace) {
        return false;
    }
    let canon = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    if canon == Path::new("/") {
        return false;
    }
    if let Some(home) = dirs_next::home_dir() {
        if canon == home {
            return false;
        }
    }
    let gd = shadow_git_dir(workspace);
    if !gd.join("HEAD").exists() {
        if !within_shadow_budget(&canon) {
            return false;
        }
        if std::fs::create_dir_all(&gd).is_err() {
            return false;
        }
        let ok = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&canon)
            .env("GIT_DIR", &gd)
            .env("GIT_WORK_TREE", &canon)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            return false;
        }
        let _ = std::fs::create_dir_all(gd.join("info"));
        let mut ex = String::new();
        for d in SHADOW_SKIP_DIRS {
            ex.push_str(&format!("{}/\n", d));
        }
        ex.push_str("*.log\n.DS_Store\n");
        let _ = std::fs::write(gd.join("info").join("exclude"), ex);
    }
    let run = |args: &[&str]| -> bool {
        Command::new("git")
            .args(args)
            .current_dir(&canon)
            .env("GIT_DIR", &gd)
            .env("GIT_WORK_TREE", &canon)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !run(&["add", "-A"]) {
        return false;
    }
    run(&[
        "-c", "user.email=shadow@free-api.local",
        "-c", "user.name=free-api-shadow",
        "-c", "commit.gpgsign=false",
        "commit", "-q", "--allow-empty", "-m", "run-start snapshot",
    ])
}
