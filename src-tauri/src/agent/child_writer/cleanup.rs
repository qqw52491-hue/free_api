//! 子 Agent 副本回收与历史脱水。

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
    let Ok(conn) = db.0.lock() else { return; };
    let mut stmt = match conn.prepare(
        "SELECT workspace_copy FROM agent_child_runs WHERE parent_run_id = ?1 AND workspace_copy IS NOT NULL"
    ) {
        Ok(s) => s,
        Err(_) => return,
    };
    let copies: Vec<String> = stmt
        .query_map([parent_run_id], |row| row.get(0))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default();

    for path_str in copies {
        let p = PathBuf::from(path_str);
        if p.exists() {
            let is_safe_writer_dir = p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(".writer-"));
            if is_safe_writer_dir {
                let _ = std::fs::remove_dir_all(&p);
            }
        }
    }
}

/// 应用启动或异常恢复时，统一回收所有已终态或崩溃残留的僵尸 writer 隔离副本（防止 200MB 磁盘泄漏）
pub fn cleanup_orphaned_writer_workspaces(db: &DbState) -> usize {
    let Ok(conn) = db.0.lock() else { return 0; };
    // 1. 将上次异常退出的 running 子任务标记为 interrupted
    let _ = conn.execute(
        "UPDATE agent_child_runs SET status = 'interrupted' WHERE status = 'running'",
        [],
    );

    // 2. 查出所有父任务已非活跃状态或已终态的历史 workspace_copy
    let mut stmt = match conn.prepare(
        "SELECT DISTINCT workspace_copy FROM agent_child_runs WHERE workspace_copy IS NOT NULL",
    ) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let copies: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default();

    let mut cleaned_count = 0;
    for path_str in copies {
        let p = PathBuf::from(&path_str);
        if p.exists() {
            let is_safe_writer_dir = p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(".writer-"));
            if is_safe_writer_dir && std::fs::remove_dir_all(&p).is_ok() {
                cleaned_count += 1;
            }
        }
    }

    // 3. 兜底：扫描各项目根目录下未被 DB 记录但以 .writer- 开头的物理孤儿目录
    if let Ok(mut proj_stmt) = conn.prepare("SELECT root_path FROM agent_projects") {
        if let Ok(proj_rows) = proj_stmt.query_map([], |row| row.get::<_, String>(0)) {
            for root_res in proj_rows.flatten() {
                let proj_path = PathBuf::from(root_res);
                if let Ok(entries) = std::fs::read_dir(&proj_path) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.is_dir() {
                            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                                if name.starts_with(".writer-") && std::fs::remove_dir_all(&path).is_ok() {
                                    cleaned_count += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    cleaned_count
}
