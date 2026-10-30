//! 复用任务检查点保存可写目录起点。目录授权发布前追加，任务内只追加不重拍。
use super::GitBaseline;
use crate::agent::{
    checkpoint::AgentCheckpoint,
    workspace_access::{self, DirectoryAccess, ProjectDirectory},
};
use rusqlite::{Connection, OptionalExtension};
use std::{collections::BTreeMap, path::Path};

fn saved_on(conn: &Connection, session: &str) -> Result<Option<AgentCheckpoint>, String> {
    let json: Option<String> = conn
        .query_row(
            "SELECT snapshot_json FROM agent_checkpoints WHERE run_id=?1",
            [session],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    json.map(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
        .transpose()
}

/// 只合并相同逻辑任务的追加起点；其他任务/阶段/回执字段仍由原乐观锁保护。
pub(crate) fn merge_task_baselines_on(
    conn: &Connection,
    cp: &mut AgentCheckpoint,
) -> Result<(), String> {
    if let Some(saved) = saved_on(conn, &cp.run_id)? {
        if saved.data.history_task_id == cp.data.history_task_id {
            for (root, baseline) in saved.task_additional_git_baselines {
                cp.task_additional_git_baselines.insert(root, baseline);
            }
        }
    }
    Ok(())
}

pub(crate) fn saved_task_baselines(
    db: &crate::db::DbState,
    session: &str,
    task: &str,
) -> Result<BTreeMap<String, GitBaseline>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    Ok(saved_on(&conn, session)?
        .filter(|cp| cp.data.history_task_id == task)
        .map(|cp| cp.task_additional_git_baselines)
        .unwrap_or_default())
}

/// 调用方持有 DB 锁，先保存起点，再发布运行时可写权限。
pub(crate) fn capture_new_writable_directories_on(
    conn: &Connection,
    session: &str,
    primary: &Path,
    next: &[ProjectDirectory],
) -> Result<(), String> {
    let Some(mut cp) = saved_on(conn, session)? else {
        return Ok(());
    };
    if cp.phase == crate::agent::checkpoint::AgentPhase::Done {
        return Ok(());
    }
    let old = workspace_access::directories(Some(session), primary);
    let mut changed = false;
    for dir in next
        .iter()
        .filter(|dir| dir.access == DirectoryAccess::ReadWrite)
    {
        if old.iter().any(|prev| prev == dir)
            || cp.task_additional_git_baselines.contains_key(&dir.path)
        {
            continue;
        }
        let root = Path::new(&dir.path);
        if root.canonicalize().ok().as_deref() != Some(root) {
            continue;
        }
        cp.task_additional_git_baselines
            .insert(dir.path.clone(), GitBaseline::capture_at_run_start(root));
        changed = true;
    }
    if changed {
        // 该字段只有任务内追加这一种写法；不推进整包版本，避免打断正在执行的状态提交。
        // write_checkpoint_on 在同一 DB 锁内合并此字段，旧内存快照不能覆盖已发布的起点。
        let value =
            serde_json::to_string(&cp.task_additional_git_baselines).map_err(|e| e.to_string())?;
        conn.execute("UPDATE agent_checkpoints SET snapshot_json=json_set(snapshot_json, '$.task_additional_git_baselines', json(?1)) WHERE run_id=?2",
            rusqlite::params![value, session]).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
