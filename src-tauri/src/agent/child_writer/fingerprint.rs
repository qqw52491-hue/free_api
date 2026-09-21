//! 文件指纹与冲突三态判定：git object-hash 优先、sha256 兜底。

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


/// 初始化副本内的 Git 基线快照（解决副本无 .git 仓库无法 diff 的问题）
pub async fn init_git_baseline(copy_root: &Path) -> bool {
    let init = crate::agent::utils::subprocess::command_output_with_timeout(
        tokio::process::Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(copy_root),
        30,
    )
    .await;
    if init.is_err() || !init.as_ref().unwrap().status.success() {
        return false;
    }
    let add = crate::agent::utils::subprocess::command_output_with_timeout(
        tokio::process::Command::new("git")
            .arg("add")
            .arg("-A")
            .current_dir(copy_root),
        60,
    )
    .await;
    if add.is_err() || !add.as_ref().unwrap().status.success() {
        return false;
    }
    let commit = crate::agent::utils::subprocess::command_output_with_timeout(
        tokio::process::Command::new("git")
            .args(&[
                "-c", "user.email=agent@local",
                "-c", "user.name=writer",
                "commit", "-qm", "spawn baseline", "--allow-empty"
            ])
            .current_dir(copy_root),
        60,
    )
    .await;
    commit.is_ok() && commit.unwrap().status.success()
}

/// 检查指定路径下 git 命令是否可用且可执行
pub async fn is_git_available(cwd: &Path) -> bool {
    crate::agent::utils::subprocess::command_output_with_timeout(
        tokio::process::Command::new("git")
            .arg("--version")
            .current_dir(cwd),
        15,
    )
    .await
    .map(|o| o.status.success())
    .unwrap_or(false)
}

/// 计算字节数据的指纹（优先使用 git hash-object --stdin，git 不可用时回退到 sha256）
pub async fn compute_bytes_hash(bytes: &[u8], git_available: bool, cwd: &Path) -> String {
    if bytes.is_empty() {
        return "empty/deleted".to_string();
    }
    if git_available {
        use tokio::io::AsyncWriteExt;
        if let Ok(mut child) = tokio::process::Command::new("git")
            .args(&["hash-object", "--stdin"])
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(bytes).await;
                drop(stdin);
            }
            let wait_out = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                child.wait_with_output(),
            )
            .await;
            if let Ok(Ok(out)) = wait_out {
                if out.status.success() {
                    let h = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if !h.is_empty() {
                        return h;
                    }
                }
            }
        }
    }
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// 计算指定路径文件的指纹（以原始字节为基准，避免编码与换行符偏差）
pub async fn compute_file_hash(path: &Path, git_available: bool) -> String {
    if !path.exists() {
        return "empty/deleted".to_string();
    }
    match std::fs::read(path) {
        Ok(bytes) => {
            let cwd = path.parent().unwrap_or(path);
            compute_bytes_hash(&bytes, git_available, cwd).await
        }
        Err(_) => "read_error".to_string(),
    }
}

/// 计算文本内容的指纹（与原始字节统一委托给 compute_bytes_hash）
pub async fn compute_str_hash(content: &str, git_available: bool, cwd: &Path) -> String {
    compute_bytes_hash(content.as_bytes(), git_available, cwd).await
}

/// 在根任务消费回执或准备合入真实工作区前，重新以当前主工作区的真实状态刷新冲突指纹
pub async fn refresh_file_conflicts(
    changed_files: &mut [ChangedFile],
    parent_root: &Path,
    git_available: bool,
) {
    for f in changed_files.iter_mut() {
        let parent_file = parent_root.join(&f.path);
        let root_now_h = compute_file_hash(&parent_file, git_available).await;
        f.root_hash = Some(root_now_h.clone());
        if let (Some(spawn_h), Some(result_h)) = (&f.spawn_hash, &f.result_hash) {
            f.conflict_status = Some(evaluate_file_conflict(spawn_h, result_h, &root_now_h));
        }
    }
}
