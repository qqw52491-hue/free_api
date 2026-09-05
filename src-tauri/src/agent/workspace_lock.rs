//! 工作区软锁：同一项目目录同时只允许一个 run，防止两个任务并发写同一堆文件。
//! 从 main_loop.rs 原样抽出，行为不变。

use crate::db::DbState;

fn workspace_locks() -> &'static std::sync::Mutex<std::collections::HashMap<String, String>> {
    static WORKSPACE_LOCKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, String>>,
    > = std::sync::OnceLock::new();
    WORKSPACE_LOCKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

pub(crate) struct WorkspaceRunGuard {
    key: String,
}

impl Drop for WorkspaceRunGuard {
    fn drop(&mut self) {
        if let Ok(mut locks) = workspace_locks().lock() {
            locks.remove(&self.key);
        }
    }
}

pub(crate) async fn acquire_workspace_lock(workspace_dir: &std::path::Path, goal: &str) -> Result<WorkspaceRunGuard, String> {
    let key = workspace_dir
        .canonicalize()
        .unwrap_or_else(|_| workspace_dir.to_path_buf())
        .to_string_lossy()
        .to_string();

    // 尝试获取工作区锁；若恰逢前序任务被中断/退出的毫秒级释放窗口，自旋等待最多 2.5 秒（50ms * 50次），
    // 彻底消除新任务启动与旧任务释放锁之间的时序踩踏导致误报「工作区已被占用」。
    for attempt in 0..50 {
        {
            let mut locks = workspace_locks().lock().map_err(|e| e.to_string())?;
            if let Some(occupied_by) = locks.get(&key) {
                if attempt == 49 {
                    return Err(format!(
                        "工作区已被占用：{key}（正在执行「{occupied_by}」）。同一项目目录同时只允许一个任务，请等待其完成或先停止该任务。"
                    ));
                }
            } else {
                locks.insert(key.clone(), goal.chars().take(60).collect());
                return Ok(WorkspaceRunGuard { key });
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }
    Err("获取工作区锁超时".to_string())
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

