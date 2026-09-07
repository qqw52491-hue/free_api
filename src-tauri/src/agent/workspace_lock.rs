//! 工作区软锁：同一项目目录同时只允许一个 run，防止两个任务并发写同一堆文件。
//! 具备同会话自动协同打断、僵尸锁自动自愈、基于 run_id 的防踩踏释放机制。

use crate::db::DbState;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceLockEntry {
    pub session_id: String,
    pub run_id: String,
    pub goal: String,
    pub acquired_at: Instant,
}

fn workspace_locks() -> &'static Mutex<HashMap<String, WorkspaceLockEntry>> {
    static WORKSPACE_LOCKS: OnceLock<Mutex<HashMap<String, WorkspaceLockEntry>>> = OnceLock::new();
    WORKSPACE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) struct WorkspaceRunGuard {
    key: String,
    run_id: String,
}

impl Drop for WorkspaceRunGuard {
    fn drop(&mut self) {
        if let Ok(mut locks) = workspace_locks().lock() {
            // 核心安全保障：只有当前持锁者的 run_id 与本 guard 相同时才移除，
            // 防止延迟 Drop 误把后序新任务的锁释放掉。
            if let Some(entry) = locks.get(&self.key) {
                if entry.run_id == self.run_id {
                    locks.remove(&self.key);
                }
            }
        }
    }
}

/// 尝试获取工作区锁。
/// 1. 若当前被同会话的前序任务占用（如前序任务因网络硬失败处于端点韧性重试等待中，用户再次发起新任务）：
///    - 第 0 轮自旋立即向旧任务发送取消打断信号，让其从 sleep/重试中唤醒退出并触发 Drop 释放锁；
///    - 若自旋 1.5 秒后旧任务仍被外部重试阻塞未退出，安全抢占接管锁，防止无限死锁；
/// 2. 若当前被跨会话占用：
///    - 检查持锁者是否为僵尸锁（超过 10 秒且对应 session 的控制通道已不复存在）；若已成僵尸则安全回收；
/// 3. 自旋等待最多 3 秒（60 次 * 50ms）；若旧任务正常退出释放，新任务即可无缝接力。
pub(crate) async fn acquire_workspace_lock(
    workspace_dir: &std::path::Path,
    session_id: &str,
    run_id: &str,
    goal: &str,
) -> Result<WorkspaceRunGuard, String> {
    let key = workspace_dir
        .canonicalize()
        .unwrap_or_else(|_| workspace_dir.to_path_buf())
        .to_string_lossy()
        .to_string();

    let mut sent_cancel = false;

    for attempt in 0..60 {
        let (should_cancel_same_session, check_zombie_session, conflict_info) = {
            let mut locks = workspace_locks().lock().map_err(|e| e.to_string())?;
            if let Some(entry) = locks.get(&key) {
                if entry.session_id == session_id {
                    let should_cancel = !sent_cancel;
                    if should_cancel {
                        sent_cancel = true;
                    }
                    if attempt >= 30 {
                        locks.remove(&key);
                        locks.insert(
                            key.clone(),
                            WorkspaceLockEntry {
                                session_id: session_id.to_string(),
                                run_id: run_id.to_string(),
                                goal: goal.chars().take(60).collect(),
                                acquired_at: Instant::now(),
                            },
                        );
                        return Ok(WorkspaceRunGuard {
                            key,
                            run_id: run_id.to_string(),
                        });
                    }
                    (should_cancel, None, Some((entry.goal.clone(), true)))
                } else {
                    let zombie_candidate = if entry.acquired_at.elapsed() > Duration::from_secs(10) {
                        Some(entry.session_id.clone())
                    } else {
                        None
                    };
                    (false, zombie_candidate, Some((entry.goal.clone(), false)))
                }
            } else {
                locks.insert(
                    key.clone(),
                    WorkspaceLockEntry {
                        session_id: session_id.to_string(),
                        run_id: run_id.to_string(),
                        goal: goal.chars().take(60).collect(),
                        acquired_at: Instant::now(),
                    },
                );
                return Ok(WorkspaceRunGuard {
                    key,
                    run_id: run_id.to_string(),
                });
            }
        };

        // 异步操作移至 MutexGuard 外部，确保整个 Future 满足 Send 特征
        if should_cancel_same_session {
            let _ = crate::agent::control::cancel_agent_run(session_id.to_string()).await;
        }

        if let Some(target_session) = check_zombie_session {
            let has_channel = crate::agent::control::intervention_channels()
                .lock()
                .await
                .contains_key(&target_session);
            if !has_channel {
                let mut locks = workspace_locks().lock().map_err(|e| e.to_string())?;
                if let Some(entry) = locks.get(&key) {
                    if entry.session_id == target_session {
                        locks.remove(&key);
                        locks.insert(
                            key.clone(),
                            WorkspaceLockEntry {
                                session_id: session_id.to_string(),
                                run_id: run_id.to_string(),
                                goal: goal.chars().take(60).collect(),
                                acquired_at: Instant::now(),
                            },
                        );
                        return Ok(WorkspaceRunGuard {
                            key,
                            run_id: run_id.to_string(),
                        });
                    }
                }
            }
        }

        if attempt == 59 {
            if let Some((occupied_by, is_same_session)) = conflict_info {
                let hint = if is_same_session {
                    "前序任务仍在处理中断或重试中，请等待其退出后再试。"
                } else {
                    "同一项目目录同时只允许一个任务，请等待其完成或先停止该任务。"
                };
                return Err(format!(
                    "工作区已被占用：{key}（正在执行「{occupied_by}」）。{hint}"
                ));
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err("获取工作区锁超时".to_string())
}

/// 强制解锁指定工作区（或根据 session_id 释放），供前端或排错救援使用
#[tauri::command]
pub async fn force_release_workspace_lock(
    workspace_path: Option<String>,
    session_id: Option<String>,
) -> Result<bool, String> {
    let mut locks = workspace_locks().lock().map_err(|e| e.to_string())?;
    if let Some(path) = workspace_path {
        let canonical = std::path::Path::new(&path)
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from(&path))
            .to_string_lossy()
            .to_string();
        let removed = locks.remove(&canonical).is_some() || locks.remove(&path).is_some();
        return Ok(removed);
    }
    if let Some(sid) = session_id {
        let initial_len = locks.len();
        locks.retain(|_, entry| entry.session_id != sid);
        return Ok(locks.len() < initial_len);
    }
    let had_any = !locks.is_empty();
    locks.clear();
    Ok(had_any)
}

/// P5: 父 run 终态时自动统一回收该 run 下所有子 writer 副本
pub(crate) struct ChildWorkspaceGuard<'a> {
    pub(crate) db: &'a DbState,
    pub(crate) run_id: &'a str,
}

impl<'a> Drop for ChildWorkspaceGuard<'a> {
    fn drop(&mut self) {
        crate::agent::child_writer::cleanup_child_workspaces(self.db, self.run_id);
    }
}
