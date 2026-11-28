//! 子 Agent 副本回收与历史脱水。

use crate::db::DbState;
use serde_json::{json, Value};
use std::path::Path;

/// 脱水子任务历史消息：单条 observation 上限 4000 字符，大输出截断保留头尾
pub fn dehydrate_child_messages(messages: &[Value]) -> Vec<Value> {
    const MAX_DEHYDRATE_OBSERVATION_CHARS: usize = 4_000;
    messages
        .iter()
        .map(|msg| {
            let mut m = msg.clone();
            if let Some(content) = m.get("content").and_then(|v| v.as_str()) {
                if content.chars().count() > MAX_DEHYDRATE_OBSERVATION_CHARS {
                    let char_count = content.chars().count();
                    let head: String = content.chars().take(2_500).collect();
                    let tail: String = content
                        .chars()
                        .skip(char_count.saturating_sub(1_500))
                        .collect();
                    let dehydrated = format!(
                        "{}\n\n... [数据较长已脱水，省略 {} 字符] ...\n\n{}",
                        head,
                        char_count.saturating_sub(4_000),
                        tail
                    );
                    m["content"] = json!(dehydrated);
                }
            }
            m
        })
        .collect()
}

/// 父 run 终态时统一回收所有该父 run 下的子 writer 隔离副本
pub fn cleanup_child_workspaces(db: &DbState, parent_run_id: &str) {
    let Ok(conn) = db.0.lock() else {
        return;
    };
    let _ = remove_registered_copies(&conn, Some(parent_run_id));
}

/// 启动时恢复子任务状态，并回收已登记的旧 Writer 副本。
pub fn cleanup_orphaned_writer_workspaces(db: &DbState) -> usize {
    let Ok(conn) = db.0.lock() else {
        return 0;
    };
    let _ = conn.execute(
        "UPDATE agent_child_runs SET status = 'interrupted' WHERE status = 'running'",
        [],
    );
    remove_registered_copies(&conn, None)
}

fn remove_registered_copies(conn: &rusqlite::Connection, parent_run_id: Option<&str>) -> usize {
    let Ok(mut stmt) = conn.prepare(
        "SELECT DISTINCT workspace_root, workspace_copy FROM agent_child_runs
         WHERE kind = 'writer' AND workspace_copy IS NOT NULL
           AND (?1 IS NULL OR parent_run_id = ?1)",
    ) else {
        return 0;
    };
    let rows = match stmt.query_map([parent_run_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    rows.filter_map(Result::ok)
        .filter(|(root, copy)| {
            let copy = Path::new(copy);
            let root = Path::new(root);
            // 旧 Writer 将物理副本登记为其物理工作区的直接子目录。
            copy.parent() == Some(root)
                && copy
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".writer-"))
                && root.canonicalize().ok().as_deref() == Some(root)
                && copy.symlink_metadata().is_ok_and(|meta| meta.is_dir())
                && std::fs::remove_dir_all(copy).is_ok()
        })
        .count()
}
