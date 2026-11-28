//! 工作区软锁：跨任务协调重叠的可写目录，同一父任务的子任务可共享持锁。

use super::workspace_access::{self, DirectoryAccess};
use crate::db::DbState;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
struct WorkspaceLockEntry {
    session_id: String,
    run_id: String,
    goal: String,
    acquired_at: Instant,
    writable_roots: HashSet<PathBuf>,
    generation: u64,
    holders: usize,
    delegation_holders: usize,
}

fn workspace_locks() -> &'static Mutex<HashMap<PathBuf, WorkspaceLockEntry>> {
    static WORKSPACE_LOCKS: OnceLock<Mutex<HashMap<PathBuf, WorkspaceLockEntry>>> = OnceLock::new();
    WORKSPACE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) struct WorkspaceRunGuard {
    key: PathBuf,
    run_id: String,
    generation: u64,
    delegated: bool,
}

fn insert_lock(
    locks: &mut HashMap<PathBuf, WorkspaceLockEntry>,
    key: PathBuf,
    writable_roots: HashSet<PathBuf>,
    session_id: &str,
    run_id: &str,
    goal: &str,
    delegated: bool,
) -> WorkspaceRunGuard {
    static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    locks.insert(
        key.clone(),
        WorkspaceLockEntry {
            session_id: session_id.to_string(),
            run_id: run_id.to_string(),
            goal: goal.chars().take(60).collect(),
            acquired_at: Instant::now(),
            writable_roots,
            generation,
            holders: 1,
            delegation_holders: usize::from(delegated),
        },
    );
    WorkspaceRunGuard {
        key,
        run_id: run_id.to_string(),
        generation,
        delegated,
    }
}

fn overlaps(first: &Path, second: &Path) -> bool {
    first.starts_with(second) || second.starts_with(first)
}

fn writable_roots(primary: &Path, session_id: &str) -> HashSet<PathBuf> {
    let mut roots = HashSet::from([primary.to_path_buf()]);
    for dir in workspace_access::directories_for_canonical_primary(session_id, primary) {
        if dir.access == DirectoryAccess::ReadWrite {
            // 权限表保存物理路径；目录离线再回来也仍属于该运行的保留范围。
            roots.insert(PathBuf::from(dir.path));
        }
    }
    roots
}

/// 将新授权与正在执行的任务锁同步发布。撤销权限立即生效，已持有的目录锁留到任务退出，
/// 保护已经开始的工具操作。待发布的配置只在所有运行中的归属通过检查后提交。
pub(crate) fn publish_writable_directory<'a, T>(
    sessions: impl IntoIterator<Item = &'a str>,
    directory: &Path,
    publish: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let sessions: HashSet<_> = sessions.into_iter().collect();
    let mut locks = workspace_locks().lock().map_err(|e| e.to_string())?;
    for recipient in locks
        .values()
        .filter(|entry| sessions.contains(entry.session_id.as_str()))
    {
        if let Some(conflict) = locks.values().find(|entry| {
            (entry.session_id != recipient.session_id || entry.run_id != recipient.run_id)
                && (sessions.contains(entry.session_id.as_str())
                    || entry
                        .writable_roots
                        .iter()
                        .any(|root| overlaps(root, directory)))
        }) {
            return Err(format!(
                "附加目录 {} 与运行中任务「{}」的可写范围重叠。请等待任务退出后再授权。",
                directory.display(),
                conflict.goal,
            ));
        }
    }
    // 运行开始时也在同一互斥区读取目录投影，避免在授权写入和锁更新之间漏锁。
    let result = publish()?;
    for entry in locks
        .values_mut()
        .filter(|entry| sessions.contains(entry.session_id.as_str()))
    {
        entry.writable_roots.insert(directory.to_path_buf());
    }
    Ok(result)
}

impl Drop for WorkspaceRunGuard {
    fn drop(&mut self) {
        if let Ok(mut locks) = workspace_locks().lock() {
            // 同一父任务的每个持有者独立释放；旧代 guard 不得释放新锁。
            if let Some(entry) = locks.get_mut(&self.key) {
                if entry.run_id == self.run_id && entry.generation == self.generation {
                    entry.holders -= 1;
                    if self.delegated {
                        entry.delegation_holders -= 1;
                    }
                    if entry.holders == 0 {
                        locks.remove(&self.key);
                    }
                }
            }
        }
    }
}

/// 尝试获取工作区锁，最多等待 60 次 * 50ms。同会话锁冲突可中断旧运行；
/// 仍有执行者或在途委派的锁继续保留。
pub(crate) async fn acquire_workspace_lock(
    workspace_dir: &Path,
    session_id: &str,
    run_id: &str,
    goal: &str,
) -> Result<WorkspaceRunGuard, String> {
    let key = workspace_dir
        .canonicalize()
        .map_err(|e| format!("工作区不可用：{} ({e})", workspace_dir.display()))?;
    acquire_workspace_lock_inner(key, session_id, run_id, goal, false).await
}

async fn acquire_workspace_lock_inner(
    key: PathBuf,
    session_id: &str,
    run_id: &str,
    goal: &str,
    delegated: bool,
) -> Result<WorkspaceRunGuard, String> {
    let mut sent_cancel = false;

    for attempt in 0..60 {
        let (conflicting_root, conflict) = {
            let mut locks = workspace_locks().lock().map_err(|e| e.to_string())?;
            let roots = writable_roots(&key, session_id);
            let conflict = locks.iter().find(|(_, entry)| {
                (entry.session_id != session_id || entry.run_id != run_id)
                    && roots
                        .iter()
                        .any(|root| entry.writable_roots.iter().any(|held| overlaps(root, held)))
            });
            if let Some((root, entry)) = conflict {
                (root.clone(), entry.clone())
            } else {
                // 同一父任务的委派复用同一目录锁；独立持有者分别释放。
                if let Some(entry) = locks.get_mut(&key) {
                    entry.holders += 1;
                    entry.delegation_holders += usize::from(delegated);
                    entry.writable_roots.extend(roots);
                    return Ok(WorkspaceRunGuard {
                        key,
                        run_id: run_id.to_string(),
                        generation: entry.generation,
                        delegated,
                    });
                }
                return Ok(insert_lock(
                    &mut locks, key, roots, session_id, run_id, goal, delegated,
                ));
            }
        };

        let same_session = conflict.session_id == session_id;
        if same_session && !sent_cancel {
            sent_cancel = true;
            let _ = crate::agent::control::cancel_run(session_id, Some(&conflict.run_id)).await;
        }

        if conflict.delegation_holders == 0
            && ((same_session && attempt >= 30)
                || conflict.acquired_at.elapsed() > Duration::from_secs(10))
        {
            let has_channel = crate::agent::control::intervention_channels()
                .lock()
                .await
                .contains_key(&conflict.session_id);
            if !has_channel {
                let mut locks = workspace_locks().lock().map_err(|e| e.to_string())?;
                if let Some(entry) = locks.get(&conflicting_root) {
                    if entry.generation == conflict.generation
                        && entry.delegation_holders == 0
                        && crate::agent::control::active_execution_run_id(&entry.session_id)?
                            .as_ref()
                            != Some(&entry.run_id)
                        && crate::agent::child_task::runtime::active(&entry.run_id).is_empty()
                    {
                        // 重新检查其它重叠锁后才登记新范围。
                        locks.remove(&conflicting_root);
                        continue;
                    }
                }
            }
        }

        if attempt == 59 {
            let hint = if same_session {
                "前序任务仍在处理中断或重试中，请等待其退出后再试。"
            } else {
                "可写目录有重叠，请等待对方完成或先停止该任务。"
            };
            return Err(format!(
                "工作区已被占用：{}（与「{}」的可写范围重叠）。{hint}",
                key.display(),
                conflict.goal,
            ));
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
        let canonical = Path::new(&path)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(&path));
        let removed =
            locks.remove(&canonical).is_some() || locks.remove(Path::new(&path)).is_some();
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

/// 同一父 run 的并发子任务各持有一个引用，最后一个持有者退出才释放项目锁。
/// 不同父 run 的项目锁仍沿用原有跨任务协调规则。
pub(crate) async fn acquire_delegation_lock(
    workspace: &Path,
    parent_session: &str,
    parent_run: &str,
) -> Result<Option<WorkspaceRunGuard>, String> {
    let key = workspace.canonicalize().map_err(|e| e.to_string())?;
    acquire_workspace_lock_inner(key, parent_session, parent_run, "通用子任务委派", true)
        .await
        .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn workspace_lock_compares_real_parent_child_roots_and_preserves_delegate_ownership() {
        let base =
            std::env::temp_dir().join(format!("free-api-workspace-lock-{}", uuid::Uuid::new_v4()));
        let primary = base.join("project");
        let nested = primary.join("nested");
        let separate = base.join("project-other");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&separate).unwrap();

        let parent = acquire_workspace_lock(&primary, "tree-owner", "root-run", "parent")
            .await
            .unwrap();
        let unrelated =
            acquire_workspace_lock(&separate, "separate-owner", "separate-run", "unrelated")
                .await
                .unwrap();
        assert!(
            acquire_workspace_lock(&nested, "nested-owner", "nested-run", "independent child")
                .await
                .is_err()
        );

        let delegate = acquire_delegation_lock(&nested, "tree-owner", "root-run")
            .await
            .unwrap()
            .unwrap();
        drop(parent);
        assert!(acquire_workspace_lock(
            &primary,
            "nested-owner",
            "nested-run",
            "ancestor challenger"
        )
        .await
        .is_err());
        drop(delegate);
        let successor = acquire_workspace_lock(&primary, "nested-owner", "nested-run", "successor")
            .await
            .unwrap();
        drop((unrelated, successor));
        std::fs::remove_dir_all(base).unwrap();
    }
}
