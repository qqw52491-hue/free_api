//! 会话级联清理、孤儿数据回收与 VACUUM 维护。

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

pub type ReadPool = Pool<SqliteConnectionManager>;

static READ_POOL: OnceLock<ReadPool> = OnceLock::new();


use super::*;

/// 启动清理：上次进程遗留的 running trace 标记为已中断。
/// runs_store 不跨进程存活，应用重启后这些任务不可能再恢复执行；
/// 不清理会让会话历史里永远挂着"运行中"的僵尸记录（微信复用会话尤其明显）。
pub fn mark_orphan_running_traces_interrupted(conn: &Connection) -> Result<usize> {
    let n = conn.execute(
        "UPDATE agent_run_traces
         SET status = 'cancelled',
             completed_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
             completion_json = COALESCE(completion_json, json_object('summary', '应用重启，任务已中断'))
         WHERE status = 'running'",
        [],
    )?;
    Ok(n)
}

/// 彻底级联清理指定会话关联的所有 SQLite 业务表、运行日志、大文本落盘文件、上传附件与 FTS 全文索引
pub fn delete_session_completely(
    conn: &Connection,
    session_id: &str,
    app_data_dir: Option<&std::path::Path>,
) -> Result<(), String> {
    let id = session_id.trim();
    if id.is_empty() {
        return Ok(());
    }

    // 1. 清理该会话溢出落盘到本地 outputs/ 的 .log 大文本文件；
    //    cas_blob 是跨会话共享的内容寻址证据，先记下哈希，待本会话行删除后
    //    再按"无剩余引用"逐个裁决（别的会话仍引用就保留）。
    let mut cas_hashes = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT raw_output FROM agent_step_logs WHERE session_id=?1 AND raw_output LIKE '[[cas_blob:%'"
    ) {
        if let Ok(rows) = stmt.query_map(rusqlite::params![id], |row| row.get::<_, String>(0)) {
            for raw in rows.flatten() {
                if let Some(hash) = raw
                    .strip_prefix(crate::agent::run_trace::CAS_BLOB_PREFIX)
                    .and_then(|s| s.strip_suffix(crate::agent::run_trace::CAS_BLOB_SUFFIX))
                {
                    cas_hashes.push(hash.to_string());
                }
            }
        }
    }
    if let Ok(mut stmt) = conn.prepare(
        "SELECT raw_output FROM agent_step_logs WHERE session_id=?1 AND raw_output LIKE '[[spilled_to_disk:%'"
    ) {
        if let Ok(rows) = stmt.query_map(rusqlite::params![id], |row| row.get::<_, String>(0)) {
            for raw in rows.flatten() {
                if let Some(path_str) = raw.strip_prefix("[[spilled_to_disk:").and_then(|s| s.strip_suffix("]]")) {
                    let path = std::path::Path::new(path_str);
                    if path.exists() {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
        }
    }

    // 2. 清理上传到 attachments/{session_id} 的本地磁盘附件目录
    let mut attach_dirs = Vec::new();
    if let Some(dir) = app_data_dir {
        attach_dirs.push(dir.join("attachments").join(id));
    }
    if let Some(data_dir) = dirs_next::data_dir() {
        attach_dirs.push(data_dir.join("free-api-chat").join("attachments").join(id));
    }
    for dir in attach_dirs {
        if dir.exists() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    // 2.5 清理会话专属临时目录 tmp/{session_id}——产物文件（pptx/xlsx/草稿）的真身落盘处；
    // agent_artifacts 表行已级联删除，磁盘文件不删就成了永久孤儿。
    // 优先用注入的 app_data_dir（生产=app.path().app_data_dir()，测试=临时目录）；
    // 仅在未注入时才落回 dirs_next——测试绝不许碰真实数据目录。
    let session_tmp_root = app_data_dir
        .map(|d| d.join("tmp"))
        .or_else(|| dirs_next::data_dir().map(|d| d.join("free-api-chat").join("tmp")));
    if let Some(tmp_root) = session_tmp_root {
        let session_tmp = tmp_root.join(id);
        if session_tmp.exists() {
            let _ = std::fs::remove_dir_all(session_tmp);
        }
    }

    // 3. 严格级联清理所有关联表（覆盖全部 16 张表，包含子审查员历史、Telemetry 指标、产物账本等）
    let _ = conn.execute(
        "DELETE FROM agent_task_records WHERE resource_id IN (SELECT resource_id FROM agent_task_resources WHERE session_id=?1)",
        rusqlite::params![id],
    );
    let _ = conn.execute("DELETE FROM agent_task_resources WHERE session_id=?1", rusqlite::params![id]);
    let _ = conn.execute("DELETE FROM agent_checkpoints WHERE run_id=?1", rusqlite::params![id]);
    let _ = conn.execute("DELETE FROM agent_checkpoint_history WHERE session_id=?1", rusqlite::params![id]);
    // agent_run_steps 按 run_id 键（非 session_id）：必须先经 traces 表取回本 session
    // 全部 run_id 级联删除，顺序不可与下一行对调。
    let _ = conn.execute(
        "DELETE FROM agent_run_steps WHERE run_id IN (SELECT id FROM agent_run_traces WHERE session_id=?1)",
        rusqlite::params![id],
    );
    let _ = conn.execute("DELETE FROM agent_run_traces WHERE session_id=?1", rusqlite::params![id]);
    let _ = conn.execute("DELETE FROM agent_child_runs WHERE parent_session_id=?1", rusqlite::params![id]);
    let _ = conn.execute("DELETE FROM agent_background_jobs WHERE parent_session_id=?1", rusqlite::params![id]);
    let _ = conn.execute("DELETE FROM agent_llm_calls WHERE session_id=?1", rusqlite::params![id]);
    let _ = conn.execute("DELETE FROM agent_artifacts WHERE session_id=?1", rusqlite::params![id]);
    let _ = conn.execute("DELETE FROM agent_step_logs_fts WHERE session_id=?1", rusqlite::params![id]);
    let _ = conn.execute("DELETE FROM agent_step_logs WHERE session_id=?1", rusqlite::params![id]);
    let _ = conn.execute("DELETE FROM agent_memory_nodes_fts WHERE session_id=?1", rusqlite::params![id]);
    let _ = conn.execute("DELETE FROM agent_memory_nodes WHERE session_id=?1", rusqlite::params![id]);
    let _ = conn.execute("DELETE FROM agent_lesson_recalls WHERE session_id=?1", rusqlite::params![id]);
    let _ = conn.execute("DELETE FROM chat_messages WHERE session_id=?1", rusqlite::params![id]);
    let _ = conn.execute("DELETE FROM agent_project_sessions WHERE session_id=?1", rusqlite::params![id]);
    conn.execute("DELETE FROM chat_sessions WHERE id=?1", rusqlite::params![id])
        .map_err(|e| format!("删除会话主记录失败: {e}"))?;

    // 3.5 本会话的 step_logs 行已删除，此刻裁决 cas_blob 是否还有其它会话引用。
    // outputs 根目录沿用既有注入约定（生产=app_data_dir/free-api-chat，测试=临时目录）。
    if !cas_hashes.is_empty() {
        let outputs_dir = app_data_dir
            .map(|d| d.join("free-api-chat").join("outputs"))
            .unwrap_or_else(crate::agent::run_trace::get_outputs_dir);
        for hash in cas_hashes {
            if !crate::agent::run_trace::cas_blob_still_referenced(conn, &hash) {
                let _ = std::fs::remove_file(
                    crate::agent::run_trace::cas_blob_path(&outputs_dir, &hash),
                );
            }
        }
    }

    // 4. 清理内存中该会话的 local_file 乐观并发基线缓存（防止孤儿会话内存残留）
    crate::agent::tools::local_file::cleanup_session_baselines(id);

    Ok(())
}

/// 自动清扫由于历史版本缺失级联逻辑而残留在数据库中的孤儿会话历史数据
pub fn cleanup_orphaned_session_data(conn: &Connection) -> Result<usize> {
    // 生产路径：真实数据目录下的 tmp 根
    let tmp_root =
        dirs_next::data_dir().map(|d| d.join("free-api-chat").join("tmp"));
    let outputs_dir = crate::agent::run_trace::get_outputs_dir();
    cleanup_orphaned_session_data_at(conn, tmp_root.as_deref(), Some(outputs_dir.as_path()))
}

/// 可注入 tmp 根的实现——测试必须注入临时目录，
/// 否则「测试 DB 为空 → 真实 tmp 目录全员孤儿」会误删真实产物文件。
/// outputs_dir 同理：传 None 跳过孤儿 blob 清扫，绝不让测试碰真实数据目录。
pub(crate) fn cleanup_orphaned_session_data_at(
    conn: &Connection,
    tmp_root: Option<&std::path::Path>,
    outputs_dir: Option<&std::path::Path>,
) -> Result<usize> {
    let mut total_cleaned = 0;

    // 清理无主溢出落盘的 .log 文件
    if let Ok(mut stmt) = conn.prepare(
        "SELECT raw_output FROM agent_step_logs WHERE session_id NOT IN (SELECT id FROM chat_sessions) AND raw_output LIKE '[[spilled_to_disk:%'"
    ) {
        if let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) {
            for raw in rows.flatten() {
                if let Some(path_str) = raw.strip_prefix("[[spilled_to_disk:").and_then(|s| s.strip_suffix("]]")) {
                    let path = std::path::Path::new(path_str);
                    if path.exists() {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
        }
    }

    let cleanup_sqls = [
        "DELETE FROM agent_task_records WHERE resource_id NOT IN (SELECT resource_id FROM agent_task_resources)",
        "DELETE FROM agent_task_resources WHERE session_id NOT IN (SELECT id FROM chat_sessions)",
        "DELETE FROM agent_checkpoints WHERE run_id NOT IN (SELECT id FROM chat_sessions)",
        "DELETE FROM agent_checkpoint_history WHERE session_id NOT IN (SELECT id FROM chat_sessions)",
        "DELETE FROM agent_run_traces WHERE session_id NOT IN (SELECT id FROM chat_sessions)",
        // 步骤行表按 run_id 键：清掉父 trace 已消失（含刚被上一行删除）的孤儿步
        "DELETE FROM agent_run_steps WHERE run_id NOT IN (SELECT id FROM agent_run_traces)",
        "DELETE FROM agent_child_runs WHERE parent_session_id NOT IN (SELECT id FROM chat_sessions)",
        "DELETE FROM agent_background_jobs WHERE parent_session_id NOT IN (SELECT id FROM chat_sessions)",
        "DELETE FROM agent_llm_calls WHERE session_id NOT IN (SELECT id FROM chat_sessions)",
        "DELETE FROM agent_artifacts WHERE session_id NOT IN (SELECT id FROM chat_sessions)",
        "DELETE FROM agent_step_logs_fts WHERE session_id NOT IN (SELECT id FROM chat_sessions)",
        "DELETE FROM agent_step_logs WHERE session_id NOT IN (SELECT id FROM chat_sessions)",
        "DELETE FROM agent_memory_nodes_fts WHERE session_id NOT IN (SELECT id FROM chat_sessions)",
        "DELETE FROM agent_memory_nodes WHERE session_id NOT IN (SELECT id FROM chat_sessions)",
        "DELETE FROM agent_lesson_recalls WHERE session_id NOT IN (SELECT id FROM chat_sessions)",
        "DELETE FROM chat_messages WHERE session_id NOT IN (SELECT id FROM chat_sessions)",
        "DELETE FROM agent_project_sessions WHERE session_id NOT IN (SELECT id FROM chat_sessions)",
    ];

    for sql in &cleanup_sqls {
        if let Ok(affected) = conn.execute(sql, []) {
            total_cleaned += affected;
        }
    }

    // 孤儿 step_logs 行删除后，其持有的 cas_blob 引用随之消失：
    // 对 outputs/blobs 做一次引用计数清扫，回收不再被任何会话引用的证据文件。
    if let Some(outputs_root) = outputs_dir {
        total_cleaned +=
            crate::agent::run_trace::sweep_orphan_output_blobs(conn, outputs_root);
    }

    // 孤儿会话 tmp 目录清扫：tmp/<sid> 子目录若无对应 chat_sessions 行，
    // 说明其会话已被删除（或属历史版本残留），产物文件一并清除。
    if let Some(tmp_root) = tmp_root {
        if let Ok(entries) = std::fs::read_dir(tmp_root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let Some(sid) = entry.file_name().to_str().map(|s| s.to_string()) else {
                    continue;
                };
                let alive: bool = conn
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM chat_sessions WHERE id=?1)",
                        rusqlite::params![sid],
                        |r| r.get::<_, i64>(0),
                    )
                    .map(|v| v == 1)
                    .unwrap_or(true); // 查询失败宁可保留，不赌误删
                if !alive && std::fs::remove_dir_all(&path).is_ok() {
                    total_cleaned += 1;
                }
            }
        }
    }

    Ok(total_cleaned)
}

/// 执行 SQLite VACUUM 操作，清理数据库碎片并回收物理磁盘空间
pub fn vacuum_database(conn: &Connection) -> Result<(), String> {
    conn.execute_batch("VACUUM;")
        .map_err(|e| format!("执行数据库 VACUUM 瘦身失败: {e}"))
}
