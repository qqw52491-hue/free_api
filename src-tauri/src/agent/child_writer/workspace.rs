//! 隔离副本创建工作区：git worktree 优先、物理复制兜底与 hard_scope 围栏。

use super::*;
use crate::db::DbState;
use crate::llm::{self, ModelConfig};
use uuid::Uuid;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;


/// 把父工作区准备为 writer 的私有沙箱（优先尝试 git worktree 秒级创建；非 git 项目平滑降级为物理拷贝）。
pub fn create_isolated_workspace(parent_root: &Path, copy_root: &Path) -> Result<(usize, u64), String> {
    // 1. 优先尝试 Git Worktree（毫秒级、0 拷贝、天然分支隔离）
    let is_git_repo = parent_root.join(".git").exists();
    if is_git_repo {
        let worktree_res = std::process::Command::new("git")
            .args(&["worktree", "add", "--detach"])
            .arg(copy_root)
            .arg("HEAD")
            .current_dir(parent_root)
            .output();
        if let Ok(out) = worktree_res {
            if out.status.success() && copy_root.exists() {
                return Ok((1, 0));
            }
        }
    }

    // 2. 降级走物理拷贝
    create_isolated_copy(parent_root, copy_root)
}

/// 把父工作区复制为 writer 的私有副本（跳过 .git 与既有 writer 副本；预算硬顶防失控）。
pub fn create_isolated_copy(parent_root: &Path, copy_root: &Path) -> Result<(usize, u64), String> {
    let mut files = 0usize;
    let mut bytes: u64 = 0;
    copy_recursive(parent_root, copy_root, &mut files, &mut bytes)?;
    Ok((files, bytes))
}

pub(crate) fn copy_recursive(src: &Path, dst: &Path, files: &mut usize, bytes: &mut u64) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|error| format!("创建副本目录失败: {}", error))?;
    for entry in std::fs::read_dir(src).map_err(|error| format!("读取源目录失败: {}", error))? {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy().to_string();
        if name_str == ".git" || name_str.starts_with(".writer-") {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_dir() {
            copy_recursive(&src_path, &dst_path, files, bytes)?;
        } else if file_type.is_file() {
            *files += 1;
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            *bytes += size;
            if *files > MAX_COPY_FILES || *bytes > MAX_COPY_BYTES {
                return Err(format!(
                    "工作区超出副本预算（最多 {} 文件 / {} MB）；请先清理构建产物或缩小工作区。",
                    MAX_COPY_FILES,
                    MAX_COPY_BYTES / 1_000_000
                ));
            }
            std::fs::copy(&src_path, &dst_path).map_err(|error| format!("复制 {} 失败: {}", name_str, error))?;
        }
    }
    Ok(())
}

pub(crate) fn ensure_contained(copy_root: &Path, raw: &str) -> Result<PathBuf, String> {
    let root = copy_root
        .canonicalize()
        .map_err(|error| format!("副本工作区不可访问: {}", error))?;
    let target = PathBuf::from(raw.trim());
    let target = if target.is_relative() {
        copy_root.join(target)
    } else {
        target
    };
    // 写入目标允许尚不存在（新建文件）：校验其父目录必须已在副本内
    let canonical = match target.canonicalize() {
        Ok(path) => path,
        Err(_) => {
            let parent = target
                .parent()
                .ok_or_else(|| format!("非法写入路径: {}", raw))?
                .canonicalize()
                .map_err(|error| format!("写入路径父目录不可访问: {}", error))?;
            parent.join(target.file_name().ok_or_else(|| format!("非法路径: {}", raw))?)
        }
    };
    if !canonical.starts_with(&root) {
        return Err(format!("writer 只能访问副本内的路径：{}", raw));
    }
    Ok(canonical)
}

/// 检查目标相对路径是否落在 hard_scope 物理围栏内
pub fn check_hard_scope(copy_root: &Path, canonical: &Path, hard_scope: &str) -> Result<(), String> {
    if hard_scope.trim().is_empty() {
        return Ok(());
    }
    let root = copy_root
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
