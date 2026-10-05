//! 会话级联清理、孤儿数据回收与 VACUUM 维护。

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

pub type ReadPool = Pool<SqliteConnectionManager>;


use super::*;

/// 会话键形态：决定「按会话删除」与「孤儿清扫」两种谓词的写法。
#[derive(Clone, Copy)]
enum SessionKey {
    /// `session_id` 直键（绝大多数会话级表）
    SessionId,
    /// `parent_session_id` 直键（子运行/后台任务）
    ParentSessionId,
    /// `run_id` 直键且取值即 session_id（agent_checkpoints）
    RunIdIsSessionId,
    /// 无会话键，经 agent_task_resources.resource_id 桥接
    ViaTaskResources,
    /// 无会话键，经 agent_run_traces.id 桥接
    ViaRunTraces,
}

impl SessionKey {
    /// 按会话删除谓词（?1 = session_id）。
    /// 桥接型在桥表行上求值 → 桥接子表必须先于桥表出现在注册表里。
    fn by_session(self) -> &'static str {
        match self {
            Self::SessionId => "session_id=?1",
            Self::ParentSessionId => "parent_session_id=?1",
            Self::RunIdIsSessionId => "run_id=?1",
            Self::ViaTaskResources => {
                "resource_id IN (SELECT resource_id FROM agent_task_resources WHERE session_id=?1)"
            }
            Self::ViaRunTraces => {
                "run_id IN (SELECT id FROM agent_run_traces WHERE session_id=?1)"
            }
        }
    }

    /// 孤儿清扫谓词（所属会话已不存在的行）。桥接型写成顺序无关形式：
    /// 「桥行已死」与「桥行属死会话」两态并查，不依赖注册表迭代顺序。
    fn orphan(self) -> &'static str {
        match self {
            Self::SessionId => "session_id NOT IN (SELECT id FROM chat_sessions)",
            Self::ParentSessionId => "parent_session_id NOT IN (SELECT id FROM chat_sessions)",
            Self::RunIdIsSessionId => "run_id NOT IN (SELECT id FROM chat_sessions)",
            Self::ViaTaskResources => {
                "resource_id NOT IN (SELECT resource_id FROM agent_task_resources) \
                 OR resource_id IN (SELECT resource_id FROM agent_task_resources \
                 WHERE session_id NOT IN (SELECT id FROM chat_sessions))"
            }
            Self::ViaRunTraces => {
                "run_id NOT IN (SELECT id FROM agent_run_traces) \
                 OR run_id IN (SELECT id FROM agent_run_traces \
                 WHERE session_id NOT IN (SELECT id FROM chat_sessions))"
            }
        }
    }
}

/// 会话级数据表注册表：delete_session_completely 与孤儿清扫的唯一事实源。
/// 历史教训：手写 DELETE 清单注释自称「覆盖全部 16 张表」，实测漏登
/// agent_stage_marks / agent_card_sections / agent_gate_fires 三张会话表——
/// 本表由 `session_scoped_registry_covers_schema` 反射测试兜底，漏登记 = 测试红。
/// 顺序约束：桥接子表必须先于桥表（by_session 谓词在桥表行还在时求值）。
const SESSION_SCOPED_TABLES: &[(&str, SessionKey)] = &[
    ("agent_task_records", SessionKey::ViaTaskResources),
    ("agent_task_resources", SessionKey::SessionId),
    ("agent_checkpoints", SessionKey::RunIdIsSessionId),
    ("agent_checkpoint_history", SessionKey::SessionId),
    ("agent_run_steps", SessionKey::ViaRunTraces),
    ("agent_run_traces", SessionKey::SessionId),
    ("agent_child_runs", SessionKey::ParentSessionId),
    ("agent_background_jobs", SessionKey::ParentSessionId),
    ("agent_llm_calls", SessionKey::SessionId),
    ("agent_artifacts", SessionKey::SessionId),
    ("agent_step_logs_fts", SessionKey::SessionId),
    ("agent_step_logs", SessionKey::SessionId),
    ("agent_memory_nodes_fts", SessionKey::SessionId),
    ("agent_memory_nodes", SessionKey::SessionId),
    ("agent_lesson_recalls", SessionKey::SessionId),
    ("agent_card_sections", SessionKey::SessionId),
    ("agent_gate_fires", SessionKey::SessionId),
    ("agent_stage_marks", SessionKey::SessionId),
    ("chat_messages", SessionKey::SessionId),
    ("agent_project_sessions", SessionKey::SessionId),
];

/// 非会话级表归属白名单：新增表必须在 SESSION_SCOPED_TABLES 与本表之间
/// 二选一登记——反射测试按「每张表都有归属」强制兑现，杜绝「忘了想」型泄漏。
const SESSION_CLEANUP_EXEMPT: &[(&str, &str)] = &[
    ("chat_sessions", "会话根表：级联的终点而非对象"),
    ("platforms", "全局渠道配置，跨会话共享"),
    ("models", "全局模型配置，随平台级联"),
    ("agent_projects", "项目级实体，生命周期归项目删除路径"),
    ("agent_project_resources", "项目级资源，FK 随 agent_projects 级联"),
    ("agent_role_cards", "全局角色卡配置"),
    ("agent_custom_instructions", "全局自定义提示词/Skill"),
    ("mcp_plugins", "全局 MCP 插件配置"),
    ("model_specs", "全局模型规格词典"),
];

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

    // 3. 严格级联清理：SESSION_SCOPED_TABLES 注册表驱动（唯一事实源；
    //    桥接子表先于桥表——by_session 谓词在桥表行还在时求值）。
    for (table, key) in SESSION_SCOPED_TABLES {
        let sql = format!("DELETE FROM {table} WHERE {}", key.by_session());
        let _ = conn.execute(&sql, rusqlite::params![id]);
    }
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

    // 同注册表驱动；orphan 谓词为顺序无关形式（桥接型并查「桥已死」与
    // 「桥属死会话」两态），迭代顺序不再承重。
    for (table, key) in SESSION_SCOPED_TABLES {
        let sql = format!("DELETE FROM {table} WHERE {}", key.orphan());
        if let Ok(affected) = conn.execute(&sql, []) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// 反射兜底：schema 中每张业务表必须有归属——会话级（SESSION_SCOPED_TABLES）
    /// 或显式豁免（SESSION_CLEANUP_EXEMPT，注明保留理由）。漏登记 = 删会话留
    /// 孤儿行；注册表/豁免表里的死项（表改名/删除后残留）同样拦。
    /// FTS5 影子表（{vtable}_data/_idx/...）与 sqlite_* 内部表由引擎自管，跳过。
    #[test]
    fn session_scoped_registry_covers_schema() {
        let conn = init_db(&PathBuf::from(":memory:")).expect("init_db :memory:");

        let all_tables: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table'")
                .unwrap();
            stmt.query_map([], |r| r.get(0)).unwrap().flatten().collect()
        };
        let vtables: HashSet<String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT name FROM sqlite_master WHERE type='table' \
                     AND sql LIKE 'CREATE VIRTUAL TABLE%'",
                )
                .unwrap();
            stmt.query_map([], |r| r.get(0)).unwrap().flatten().collect()
        };

        let registered: HashSet<&str> =
            SESSION_SCOPED_TABLES.iter().map(|(n, _)| *n).collect();
        let exempt: HashSet<&str> = SESSION_CLEANUP_EXEMPT.iter().map(|(n, _)| *n).collect();

        let unowned: Vec<&String> = all_tables
            .iter()
            .filter(|t| !t.starts_with("sqlite_"))
            .filter(|t| {
                !vtables
                    .iter()
                    .any(|v| t.len() > v.len() && t.starts_with(&format!("{v}_")))
            })
            .filter(|t| !registered.contains(t.as_str()) && !exempt.contains(t.as_str()))
            .collect();
        assert!(
            unowned.is_empty(),
            "以下表既未登记 SESSION_SCOPED_TABLES 也未豁免 SESSION_CLEANUP_EXEMPT——\
             新增表必须显式归属，否则删会话留孤儿行: {unowned:?}"
        );

        for (name, _) in SESSION_SCOPED_TABLES {
            assert!(
                all_tables.iter().any(|t| t == name),
                "SESSION_SCOPED_TABLES 登记了 schema 中不存在的表: {name}"
            );
        }
        for (name, _) in SESSION_CLEANUP_EXEMPT {
            assert!(
                all_tables.iter().any(|t| t == name),
                "SESSION_CLEANUP_EXEMPT 登记了 schema 中不存在的表: {name}"
            );
        }
    }

    /// 谓词列存在性兜底：SessionId/ParentSessionId/桥接键的谓词列若在 schema
    /// 里不存在（列改名事故），DELETE 会静默删空或报错被 let _ 吞掉——测试先行拦下。
    #[test]
    fn session_scoped_preds_reference_real_columns() {
        let conn = init_db(&PathBuf::from(":memory:")).expect("init_db :memory:");
        for (name, key) in SESSION_SCOPED_TABLES {
            let col = match key {
                SessionKey::SessionId => "session_id",
                SessionKey::ParentSessionId => "parent_session_id",
                SessionKey::RunIdIsSessionId | SessionKey::ViaRunTraces => "run_id",
                SessionKey::ViaTaskResources => "resource_id",
            };
            let cols: Vec<String> = {
                let mut stmt = conn.prepare(&format!("PRAGMA table_info({name})")).unwrap();
                stmt.query_map([], |r| r.get::<_, String>(1))
                    .unwrap()
                    .flatten()
                    .collect()
            };
            assert!(
                cols.iter().any(|c| c == col),
                "{name} 缺谓词列 {col}（现有列: {cols:?}）"
            );
        }
    }

    /// 端到端：删会话后注册表内所有表行归零，豁免表与其它会话行不受影响。
    #[test]
    fn delete_session_clears_every_registered_table() {
        let conn = init_db(&PathBuf::from(":memory:")).expect("init_db :memory:");
        conn.execute_batch(
            "INSERT INTO chat_sessions (id, title, model_id, created_at, updated_at)
             VALUES ('s1', 'a', 'm', '2026-01-01', '2026-01-01'),
                    ('s2', 'b', 'm', '2026-01-01', '2026-01-01');
             INSERT INTO chat_messages (id, session_id, role, content, created_at)
             VALUES ('m1', 's1', 'user', 'x', '2026-01-01'),
                    ('m2', 's2', 'user', 'y', '2026-01-01');
             INSERT INTO agent_stage_marks (session_id, stage, updated_at)
             VALUES ('s1', 'done', '2026-01-01'), ('s2', 'done', '2026-01-01');
             INSERT INTO agent_card_sections (session_id, step_id, card_level, sections_json, total_bytes, created_at)
             VALUES ('s1', 1, 'full', '{}', 10, '2026-01-01');
             INSERT INTO agent_gate_fires (run_id, session_id, step_id, gate, route, created_at)
             VALUES ('r1', 's1', 1, 'g', 'r', '2026-01-01');
             INSERT INTO agent_run_traces (id, session_id, goal, status, steps_json, started_at, updated_at)
             VALUES ('rt1', 's1', 'g', 'done', '[]', '2026-01-01', '2026-01-01');
             INSERT INTO agent_run_steps (run_id, seq, step_json) VALUES ('rt1', 1, '{}');
             INSERT INTO agent_task_resources (resource_id, session_id, kind, label, created_at, updated_at)
             VALUES ('res1', 's1', 'k', 'l', '2026-01-01', '2026-01-01');
             INSERT INTO agent_task_records (resource_id, record_key, data_json, created_at, updated_at)
             VALUES ('res1', 'k1', '{}', '2026-01-01', '2026-01-01');",
        )
        .unwrap();

        delete_session_completely(&conn, "s1", None).unwrap();

        for (name, key) in SESSION_SCOPED_TABLES {
            // 只数带本 session 谓词的行：其它会话（s2）的行应保留，不计入
            let n: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {name} WHERE {}", key.by_session()),
                    rusqlite::params!["s1"],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 0, "{name} 仍残留 s1 的行");
        }
        assert_eq!(
            conn.query_row::<i64, _, _>(
                "SELECT COUNT(*) FROM chat_messages WHERE session_id='s2'",
                [],
                |r| r.get(0)
            )
            .unwrap(),
            1,
            "其它会话的行不应被误删"
        );
    }
}
