use crate::db::DbState;
use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

#[path = "run_trace_source.rs"]
mod output_source;
pub(crate) use output_source::{saved_output_source, source_for_task};

pub fn upsert_snapshot(
    db: &DbState,
    run_id: &str,
    session_id: &str,
    goal: &str,
    status: &str,
    steps: &[Value],
    completion: Option<&Value>,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    let now = Utc::now().to_rfc3339();
    let steps_json = serde_json::to_string(steps).map_err(|error| error.to_string())?;
    let completion_json = completion
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| error.to_string())?;
    conn.execute(
        "INSERT INTO agent_run_traces (id, session_id, goal, status, steps_json, completion_json, started_at, updated_at, completed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, CASE WHEN ?4 IN ('succeeded', 'failed', 'cancelled') THEN ?7 ELSE NULL END)
         ON CONFLICT(id) DO UPDATE SET
            status=excluded.status,
            steps_json=CASE WHEN excluded.steps_json = '[]' AND agent_run_traces.steps_json != '[]' AND agent_run_traces.steps_json IS NOT NULL THEN agent_run_traces.steps_json ELSE excluded.steps_json END,
            completion_json=COALESCE(excluded.completion_json, agent_run_traces.completion_json),
            updated_at=excluded.updated_at,
            completed_at=CASE WHEN excluded.status IN ('succeeded', 'failed', 'cancelled') THEN excluded.updated_at ELSE agent_run_traces.completed_at END",
        params![run_id, session_id, goal, status, steps_json, completion_json, now],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

/// 逐步追加：单步 INSERT，替代每步整写 steps_json 的 O(n) 序列化+整行重写。
/// seq 取 steps 数组下标（追加顺序单调递增）；同时轻触父行 updated_at 供监控排序。
pub fn append_step(
    db: &DbState,
    run_id: &str,
    seq: i64,
    step: &Value,
) -> Result<(), String> {
    let mut conn = db.0.lock().map_err(|error| error.to_string())?;
    let step_json = serde_json::to_string(step).map_err(|error| error.to_string())?;
    let now = Utc::now().to_rfc3339();
    let tx = conn.transaction().map_err(|error| error.to_string())?;
    tx.execute(
        "INSERT INTO agent_run_steps (run_id, seq, step_json) VALUES (?1, ?2, ?3)
         ON CONFLICT(run_id, seq) DO UPDATE SET step_json=excluded.step_json",
        params![run_id, seq, step_json],
    )
    .map_err(|error| error.to_string())?;
    tx.execute(
        "UPDATE agent_run_traces SET updated_at=?2 WHERE id=?1",
        params![run_id, now],
    )
    .map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?;
    Ok(())
}

/// 行优先读取：agent_run_steps 有数据时按 seq 顺序重组（增量路径）；
/// 为空时回退 steps_json 列（旧快照/测试直接播种/终态整写路径）。
pub fn load_run_steps(db: &DbState, run_id: &str) -> Result<Vec<Value>, String> {
    let conn = db.reader()?;
    let mut statement = conn
        .prepare("SELECT step_json FROM agent_run_steps WHERE run_id=?1 ORDER BY seq ASC")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![run_id], |r| r.get::<_, String>(0))
        .map_err(|error| error.to_string())?;
    let mut steps: Vec<Value> = Vec::new();
    for row in rows {
        let raw = row.map_err(|error| error.to_string())?;
        steps.push(serde_json::from_str::<Value>(&raw).unwrap_or(Value::Null));
    }
    drop(statement);
    if !steps.is_empty() {
        return Ok(steps);
    }
    let row: Option<String> = conn
        .query_row(
            "SELECT steps_json FROM agent_run_traces WHERE id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    if let Some(json_str) = row {
        let val: Value = serde_json::from_str(&json_str).unwrap_or_else(|_| json!([]));
        if let Some(arr) = val.as_array() {
            return Ok(arr.clone());
        }
    }
    Ok(Vec::new())
}

pub fn list_session_traces(db: &DbState, session_id: &str) -> Result<Vec<Value>, String> {
    let conn = db.reader()?;
    let has_goal_message_id = conn.prepare("SELECT goal_message_id FROM agent_run_traces LIMIT 0").is_ok();
    let query = if has_goal_message_id {
        "SELECT id, goal, status, steps_json, completion_json, started_at, updated_at, completed_at, goal_message_id FROM agent_run_traces WHERE session_id=?1 ORDER BY started_at ASC, id ASC"
    } else {
        "SELECT id, goal, status, steps_json, completion_json, started_at, updated_at, completed_at, NULL FROM agent_run_traces WHERE session_id=?1 ORDER BY started_at ASC, id ASC"
    };
    let mut statement = conn
        .prepare(query)
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![session_id], |row| {
            let steps_json: String = row.get(3)?;
            let completion_json: Option<String> = row.get(4)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                steps_json,
                completion_json,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    rows.map(|row| {
        let (id, goal, status, steps_json, completion_json, started_at, updated_at, completed_at, goal_message_id) =
            row.map_err(|error| error.to_string())?;
        // 行优先：增量持久化后 steps_json 只在启动/终态整写
        let row_steps: Vec<Value> = conn
            .prepare("SELECT step_json FROM agent_run_steps WHERE run_id=?1 ORDER BY seq ASC")
            .and_then(|mut stmt| {
                stmt.query_map(params![id], |r| r.get::<_, String>(0))
                    .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
            })
            .unwrap_or_default()
            .into_iter()
            .filter_map(|raw| serde_json::from_str::<Value>(&raw).ok())
            .collect();
        let steps = if !row_steps.is_empty() {
            Value::Array(row_steps)
        } else {
            serde_json::from_str::<Value>(&steps_json).unwrap_or_else(|_| json!([]))
        };
        let completion = completion_json
            .as_deref()
            .and_then(|value| serde_json::from_str::<Value>(value).ok())
            .unwrap_or(Value::Null);
        let reviewers = review_step_history(&conn, session_id, &id).unwrap_or_default();
        Ok(json!({
            "id": id,
            "goal": goal,
            "goal_message_id": goal_message_id,
            "status": status,
            "steps": steps,
            "reviewers": reviewers,
            "completion": completion,
            "started_at": started_at,
            "updated_at": updated_at,
            "completed_at": completed_at,
        }))
    })
    .collect()
}

fn first_line_clipped(raw: &str, max_chars: usize) -> Option<String> {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    if line.chars().count() > max_chars {
        Some(format!(
            "{}...",
            line.chars().take(max_chars).collect::<String>()
        ))
    } else {
        Some(line.to_string())
    }
}

fn review_step_history(conn: &rusqlite::Connection, session: &str, parent_run: &str) -> rusqlite::Result<Vec<Value>> {
    let has_models = conn.prepare("SELECT name, display_name FROM models LIMIT 0").is_ok();
    let has_details = conn.prepare("SELECT error_kind, result_json FROM agent_child_runs LIMIT 0").is_ok();
    let model_expr = if has_models {
        "COALESCE(NULLIF(TRIM(m.display_name), ''), NULLIF(TRIM(m.name), ''), r.model_id)"
    } else {
        "r.model_id"
    };
    let model_join = if has_models {
        " LEFT JOIN models m ON m.id = r.model_id"
    } else {
        ""
    };
    let detail_cols = if has_details {
        "r.error_kind,r.result_json"
    } else {
        "NULL,NULL"
    };
    let sql = format!(
        "SELECT r.id,r.purpose,{model_expr},r.status,r.duration_ms,r.execution_checkpoint_json,{detail_cols}
         FROM agent_child_runs r{model_join}
         WHERE r.parent_session_id=?1 AND r.parent_run_id=?2 AND r.kind='review'
         ORDER BY r.started_at,r.id"
    );
    let mut members = conn.prepare(&sql)?;
    let mut steps = conn.prepare(
        "SELECT seq,step_json FROM agent_run_steps WHERE run_id=?1 ORDER BY seq DESC LIMIT 6",
    )?;
    let rows = members.query_map(params![session, parent_run], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, Option<String>>(7)?,
        ))
    })?;
    rows.map(|row| {
        let (id, purpose, model, status, duration, checkpoint, error_kind, result_json) = row?;
        let checkpoint = checkpoint.and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
        let role_label = checkpoint.as_ref().and_then(|value| {
            value["state"]["dispatch_input"]["params"]["role_label"]
                .as_str()
                .or_else(|| value["state"]["main_review"]["role_label"].as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        });
        let parent_step_id = checkpoint
            .as_ref()
            .and_then(|value| value["state"]["main_review"]["parent_step_id"].as_u64());
        let parsed_result = result_json.and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
        let verdict = parsed_result
            .as_ref()
            .and_then(|value| value["verdict"].as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let summary = parsed_result
            .as_ref()
            .and_then(|value| value["summary"].as_str())
            .and_then(|raw| first_line_clipped(raw, 200));
        let blocking_issue = parsed_result
            .as_ref()
            .and_then(|value| value["blocking_issues"].as_array())
            .and_then(|items| {
                items.iter().find_map(|item| {
                    item.as_str()
                        .or_else(|| item["issue"].as_str())
                        .or_else(|| item["description"].as_str())
                        .or_else(|| item["summary"].as_str())
                        .or_else(|| item["title"].as_str())
                })
            })
            .and_then(|raw| first_line_clipped(raw, 200));
        let missing_ev = parsed_result
            .as_ref()
            .and_then(|value| value["missing_evidence"].as_array())
            .and_then(|items| items.iter().find_map(|item| item.as_str()))
            .and_then(|raw| first_line_clipped(raw, 200));
        let is_problematic = status != "succeeded"
            || matches!(verdict.as_deref(), Some("rejected" | "failed_open"));
        let error_detail = if is_problematic {
            error_kind
                .as_deref()
                .and_then(|raw| first_line_clipped(raw, 200))
                .or(blocking_issue)
                .or(missing_ev)
                .or_else(|| summary.clone())
        } else {
            None
        };
        let rows = steps.query_map([&id], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?;
        let mut previews = Vec::new();
        for row in rows {
            let (seq, raw) = row?;
            if let Ok(step) = serde_json::from_str::<Value>(&raw) {
                let mut preview = super::child_task::review_step_preview(&step);
                preview["id"] = json!(seq);
                previews.push(preview);
            }
        }
        previews.reverse();
        Ok(json!({
            "id": id,
            "purpose": purpose,
            "model": model,
            "status": status,
            "label": role_label,
            "duration_ms": duration,
            "parent_step_id": parent_step_id,
            "verdict": verdict,
            "summary": summary,
            "error_detail": error_detail,
            "steps": previews,
        }))
    })
    .collect()
}

/// 超过该字符阈值（约 2,000 字符 / ~500 Tokens）时自动溢出落盘到本地独立文件
pub const DISK_SPILL_THRESHOLD_CHARS: usize = 2_000;

/// 溢出内容按 SHA-256 内容寻址：同一份大输出（如重复 MCP 证据包）物理上只写一次，
/// 多个 output_id 行共享同一 blob。marker 形态 `[[cas_blob:<hash>]]`，
/// 文件落在 outputs/blobs/<hash>.log；删除归属由 agent_step_logs 行引用计数裁决。
pub(crate) const CAS_BLOB_PREFIX: &str = "[[cas_blob:";
pub(crate) const CAS_BLOB_SUFFIX: &str = "]]";

pub fn get_outputs_dir() -> std::path::PathBuf {
    // 测试/评测可注入隔离目录：blob 落盘与孤儿清扫若指向真实数据目录，
    // 测试会把真实会话的产物当孤儿物理删除（实测全量套件删了 137 个 blob）。
    if let Ok(dir) = std::env::var("FREE_API_OUTPUTS_DIR") {
        let dir = dir.trim();
        if !dir.is_empty() {
            return std::path::PathBuf::from(dir);
        }
    }
    dirs_next::data_dir()
        .map(|d| d.join("free-api-chat").join("outputs"))
        .unwrap_or_else(|| std::path::PathBuf::from("./data/outputs"))
}

pub(crate) fn cas_blob_path(outputs_dir: &std::path::Path, hash: &str) -> std::path::PathBuf {
    outputs_dir.join("blobs").join(format!("{}.log", hash))
}

fn is_valid_blob_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 该 blob 是否仍被任何会话的 step_logs 行引用（跨会话共享判定）。
/// 查询失败宁可视为仍被引用，绝不误删共享证据。
pub(crate) fn cas_blob_still_referenced(conn: &rusqlite::Connection, hash: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_step_logs WHERE raw_output = ?1)",
        params![format!("{}{}{}", CAS_BLOB_PREFIX, hash, CAS_BLOB_SUFFIX)],
        |row| row.get::<_, i64>(0),
    )
    .map(|v| v == 1)
    .unwrap_or(true)
}

/// 孤儿 blob 清扫：outputs/blobs/ 下没有任何 step_logs 行引用的文件物理删除。
/// 供会话级联删除与孤儿数据清理调用；目录缺失/读取失败一律静默跳过。
pub(crate) fn sweep_orphan_output_blobs(
    conn: &rusqlite::Connection,
    outputs_dir: &std::path::Path,
) -> usize {
    let blobs_dir = outputs_dir.join("blobs");
    let Ok(entries) = std::fs::read_dir(&blobs_dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        // 崩溃残留的未完成临时文件（.<hash>.tmp-<uuid>）：永远不会被引用。
        // 留 10 分钟宽限期，避免删掉正在写入的对端临时文件。
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.') && name.contains(".tmp-") {
                let stale = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > std::time::Duration::from_secs(600));
                if stale && std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
                continue;
            }
        }
        if path.extension().and_then(|e| e.to_str()) != Some("log") {
            continue;
        }
        let Some(hash) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !is_valid_blob_hash(hash) || cas_blob_still_referenced(conn, hash) {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// 落库到步骤快照的 thinking 上限：reasoning 全文仅作取证留痕，超长截断保护快照体积。
pub(crate) const MAX_TRACE_THINKING_CHARS: usize = 64_000;

pub(crate) fn clip_thinking_for_trace(text: &str) -> String {
    text.chars().take(MAX_TRACE_THINKING_CHARS).collect()
}

pub fn save_step_output_with_meta(
    db: &DbState,
    session_id: &str,
    step_id: usize,
    tool: &str,
    action: &str,
    outcome: &str,
    raw_output: &str,
) -> Result<String, String> {
    save_step_output_in_group(db, session_id, step_id, tool, action, outcome, raw_output, None)
}

pub(crate) fn save_step_output_in_group(
    db: &DbState,
    session_id: &str,
    step_id: usize,
    tool: &str,
    action: &str,
    outcome: &str,
    raw_output: &str,
    group: Option<&crate::agent::work_history::WorkGroup>,
) -> Result<String, String> {
    save_group_observation(db, session_id, step_id, tool, action, outcome, raw_output, group, None)
}

pub(crate) fn save_group_observation(
    db: &DbState, session_id: &str, step_id: usize, tool: &str, action: &str,
    outcome: &str, raw_output: &str, group: Option<&crate::agent::work_history::WorkGroup>,
    async_id: Option<&str>,
) -> Result<String, String> {
    let output_id = match async_id {
        Some(id) => uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, json!([session_id,id]).to_string().as_bytes()).to_string(),
        None => uuid::Uuid::new_v4().to_string(),
    };
    let now = Utc::now().to_rfc3339();

    // blob 落盘必须先于获取 SQLite 锁完成：大文件写盘耗时不可控，
    // 在锁内做文件 IO 会阻塞同一进程的其他数据库读写者。
    let sqlite_content = if raw_output.chars().count() > DISK_SPILL_THRESHOLD_CHARS {
        let outputs_dir = get_outputs_dir();
        let blobs_dir = outputs_dir.join("blobs");
        let hash = crate::agent::context::determinism::sha256_hex(raw_output);
        let blob_file = cas_blob_path(&outputs_dir, &hash);
        // 写一次共享多次：先写临时文件再原子 rename，崩溃不会留下半截 blob
        // 被后续 exists() 短路当成完好证据。
        let persisted = blob_file.exists() || {
            let tmp_file = blobs_dir.join(format!(".{}.tmp-{}", hash, uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&blobs_dir).is_ok()
                && std::fs::write(&tmp_file, raw_output).is_ok()
                && std::fs::rename(&tmp_file, &blob_file).is_ok()
        };
        if persisted {
            format!("{}{}{}", CAS_BLOB_PREFIX, hash, CAS_BLOB_SUFFIX)
        } else {
            raw_output.to_string()
        }
    } else {
        raw_output.to_string()
    };

    let conn = db.0.lock().map_err(|error| error.to_string())?;

    let inserted = conn.execute(
        "INSERT INTO agent_step_logs (output_id, session_id, step_id, raw_output, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(output_id) DO NOTHING",
        params![output_id, session_id, step_id, sqlite_content, now],
    )
    .map_err(|error| error.to_string())?;
    if inserted == 0 { return Ok(output_id); }

    let _ = crate::agent::retrieval::index_step_fts(
        &conn, &output_id, session_id, step_id, tool, action, outcome, raw_output,
    );

    let group = group.filter(|g| g.session_id == session_id);
    let parent_id = group.and_then(|g| match crate::agent::work_history::ensure_group(&conn, g) {
        Ok(id) => Some(id),
        Err(e) => { eprintln!("工作目录未保存，原始输出仍可回查: {e}"); None }
    });
    // 写入三级记忆模型的 Level 0 原始事件节点 (带语义关键词)
    let node = crate::agent::retrieval::MemoryNode {
        // 步骤号会在不同会话/运行中复用；一次观察的身份跟随原始输出。
        id: format!("event-{}", output_id),
        parent_id,
        level: 0,
        kind: "event".to_string(),
        session_id: session_id.to_string(),
        plan_id: group.map(|g| g.plan_id.clone()),
        step_index: if async_id.is_some() { None } else { Some(step_id) },
        content: format!(
            "工具: {} | 动作: {} | 状态: {}\n输出: {}",
            tool,
            action,
            outcome,
            raw_output.chars().take(500).collect::<String>()
        ),
        metadata_json: serde_json::json!({
            "output_id": output_id,
            "tool": tool,
            "action": action,
            "outcome": outcome,
            "work_group": group,
            "async_result_id": async_id,
        })
        .to_string(),
        confidence_score: 100,
        hit_count: 0,
        created_at: now,
    };
    let trigger_keywords = format!("{}, {}, {}", tool, action, outcome);
    let _ = crate::agent::retrieval::insert_memory_node(&conn, &node, &trigger_keywords);

    Ok(output_id)
}

pub(crate) fn step_output_matches_hash(
    db: &DbState,
    session_id: &str,
    output_id: &str,
    expected_hash: &str,
) -> bool {
    get_step_output(db, session_id, output_id)
        .map(|raw| crate::agent::context::determinism::sha256_hex(&raw) == expected_hash)
        .unwrap_or(false)
}

/// 把 step_logs 的 raw_output 还原为原文（支持 cas_blob / spilled_to_disk 溢出标记）；读取失败返回 None。
pub(crate) fn resolve_raw_output(raw: &str) -> Option<String> {
    if let Some(path_str) = raw
        .strip_prefix("[[spilled_to_disk:")
        .and_then(|value| value.strip_suffix("]]"))
    {
        return std::fs::read_to_string(path_str).ok();
    }
    if let Some(hash) = raw
        .strip_prefix(CAS_BLOB_PREFIX)
        .and_then(|value| value.strip_suffix(CAS_BLOB_SUFFIX))
    {
        if !is_valid_blob_hash(hash) {
            return None;
        }
        return std::fs::read_to_string(cas_blob_path(&get_outputs_dir(), hash)).ok();
    }
    Some(raw.to_string())
}

/// 回查范围只覆盖保存的工具回执，不承诺还原上游未返回、截断或采集失败的内容。
pub(crate) const SAVED_OUTPUT_SCOPE_NOTICE: &str = "仅覆盖该 output_id 已保存的工具回执；上游命令过滤、工具截断或采集失败而未保存的内容无法由此恢复。has_more/total_chars 只描述本次选中回执或投影的分页范围，不证明完整覆盖原始日志、对话或任务事实。";

pub fn get_step_output(db: &DbState, session_id: &str, output_id: &str) -> Result<String, String> {
    // 先通过 SQLite 校验 output_id 归属，再读取可能的溢出文件，避免跨 session 工件泄露。
    let raw: Option<String> = {
        let conn = db.0.lock().map_err(|error| error.to_string())?;
        conn.query_row(
            "SELECT raw_output FROM agent_step_logs WHERE output_id=?1 AND session_id=?2",
            params![output_id, session_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?
    };

    let raw = raw.ok_or_else(|| "当前 session 中未找到该 output_id".to_string())?;
    if let Some(path_str) = raw
        .strip_prefix("[[spilled_to_disk:")
        .and_then(|value| value.strip_suffix("]]"))
    {
        return std::fs::read_to_string(path_str).map_err(|error| {
            format!(
                "工件原文不可用 (output_id: {}, path: {}): {}",
                output_id, path_str, error
            )
        });
    }
    if let Some(hash) = raw
        .strip_prefix(CAS_BLOB_PREFIX)
        .and_then(|value| value.strip_suffix(CAS_BLOB_SUFFIX))
    {
        if !is_valid_blob_hash(hash) {
            return Err(format!("工件 blob 引用非法 (output_id: {})", output_id));
        }
        let path = cas_blob_path(&get_outputs_dir(), hash);
        return std::fs::read_to_string(&path).map_err(|error| {
            format!(
                "工件 blob 不可用 (output_id: {}, path: {}): {}",
                output_id,
                path.display(),
                error
            )
        });
    }

    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn test_db() -> DbState {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE agent_run_traces (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                goal TEXT NOT NULL,
                status TEXT NOT NULL,
                steps_json TEXT NOT NULL,
                completion_json TEXT,
                started_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                completed_at TEXT
            );",
            )
            .unwrap();
        DbState(Arc::new(Mutex::new(connection)))
    }

    #[test]
    fn clip_thinking_for_trace_truncates_at_cap() {
        let short = "先分析根因再动手";
        assert_eq!(clip_thinking_for_trace(short), short);
        let long = "思".repeat(MAX_TRACE_THINKING_CHARS + 500);
        let clipped = clip_thinking_for_trace(&long);
        assert_eq!(clipped.chars().count(), MAX_TRACE_THINKING_CHARS);
        // thinking 字段随步骤 JSON 原样往返（快照→行表→回放）
        let db = test_db();
        db.0.lock()
            .unwrap()
            .execute_batch(
                "CREATE TABLE agent_run_steps (
                    run_id TEXT NOT NULL, seq INTEGER NOT NULL, step_json TEXT NOT NULL,
                    PRIMARY KEY (run_id, seq));",
            )
            .unwrap();
        append_step(
            &db,
            "run-thinking",
            0,
            &json!({"id": 0, "thought": "简述", "thinking": clipped}),
        )
        .unwrap();
        let steps = load_run_steps(&db, "run-a-missing").unwrap_or_default();
        assert!(steps.is_empty());
        let loaded = load_run_steps(&db, "run-thinking").unwrap();
        assert_eq!(
            loaded[0]["thinking"].as_str().unwrap().chars().count(),
            MAX_TRACE_THINKING_CHARS
        );
    }

    fn step_output_test_db() -> DbState {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE agent_step_logs (
                output_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                step_id INTEGER NOT NULL,
                raw_output TEXT NOT NULL,
                created_at TEXT NOT NULL
            );",
            )
            .unwrap();
        DbState(Arc::new(Mutex::new(connection)))
    }

    /// 序列化所有触碰 outputs 目录的用例：env 覆盖是进程全局的，
    /// 并行测试须排队并各自持有独立临时目录，互不扫到对方 blob。
    static OUTPUTS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    struct IsolatedOutputs(std::path::PathBuf);
    impl IsolatedOutputs {
        fn new() -> (std::sync::MutexGuard<'static, ()>, Self) {
            let guard = OUTPUTS_TEST_LOCK.lock().unwrap();
            let dir = std::env::temp_dir().join(format!(
                "frc_outputs_{}_{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            std::env::set_var("FREE_API_OUTPUTS_DIR", &dir);
            (guard, Self(dir))
        }
    }
    impl Drop for IsolatedOutputs {
        fn drop(&mut self) {
            std::env::remove_var("FREE_API_OUTPUTS_DIR");
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn step_output_matches_hash_covers_inline_spill_and_missing() {
        let (_outputs_guard, _iso_outputs) = IsolatedOutputs::new();
        let db = step_output_test_db();
        let session = "sess-hash";

        let inline_body = "短结果正文".to_string();
        let inline_id = save_step_output_with_meta(
            &db,
            session,
            0,
            "remote-http-mcp",
            "call",
            "succeeded",
            &inline_body,
        )
        .unwrap();
        let inline_hash = crate::agent::context::determinism::sha256_hex(&inline_body);
        assert!(step_output_matches_hash(
            &db,
            session,
            &inline_id,
            &inline_hash
        ));
        assert!(!step_output_matches_hash(
            &db, session, &inline_id, "deadbeef"
        ));
        assert!(!step_output_matches_hash(
            &db,
            session,
            "no-such-id",
            &inline_hash
        ));
        assert!(!step_output_matches_hash(
            &db,
            "other-session",
            &inline_id,
            &inline_hash
        ));

        let spilled_body = "溢出行\n".repeat(DISK_SPILL_THRESHOLD_CHARS);
        let spilled_id = save_step_output_with_meta(
            &db,
            session,
            1,
            "remote-http-mcp",
            "call",
            "succeeded",
            &spilled_body,
        )
        .unwrap();
        let spilled_hash = crate::agent::context::determinism::sha256_hex(&spilled_body);
        let blob_path = {
            let conn = db.0.lock().unwrap();
            let raw: String = conn
                .query_row(
                    "SELECT raw_output FROM agent_step_logs WHERE output_id=?1",
                    params![spilled_id],
                    |r| r.get(0),
                )
                .unwrap();
            let hash = raw
                .strip_prefix(CAS_BLOB_PREFIX)
                .and_then(|v| v.strip_suffix(CAS_BLOB_SUFFIX))
                .expect("超长正文必须落盘为 cas blob");
            assert_eq!(hash, spilled_hash);
            cas_blob_path(&get_outputs_dir(), hash)
        };
        assert!(step_output_matches_hash(
            &db,
            session,
            &spilled_id,
            &spilled_hash
        ));
        std::fs::remove_file(&blob_path).unwrap();
        assert!(!step_output_matches_hash(
            &db,
            session,
            &spilled_id,
            &spilled_hash
        ));
    }

    #[test]
    fn cas_blob_shares_one_file_across_duplicate_outputs() {
        let (_outputs_guard, _iso_outputs) = IsolatedOutputs::new();
        let db = step_output_test_db();
        let session = "sess-cas";
        let body = "重复的大正文\n".repeat(DISK_SPILL_THRESHOLD_CHARS);
        let first_id = save_step_output_with_meta(
            &db, session, 0, "mcp", "call", "succeeded", &body,
        )
        .unwrap();
        let second_id = save_step_output_with_meta(
            &db, session, 1, "mcp", "call", "succeeded", &body,
        )
        .unwrap();
        let hash = crate::agent::context::determinism::sha256_hex(&body);
        let blob = cas_blob_path(&get_outputs_dir(), &hash);
        assert!(blob.exists());
        assert_eq!(get_step_output(&db, session, &first_id).unwrap(), body);
        assert_eq!(get_step_output(&db, session, &second_id).unwrap(), body);

        // 删除一个引用后 blob 仍被另一行引用；全部行删除后才可回收
        {
            let conn = db.0.lock().unwrap();
            conn.execute(
                "DELETE FROM agent_step_logs WHERE output_id=?1",
                params![first_id],
            )
            .unwrap();
            assert!(cas_blob_still_referenced(&conn, &hash));
            conn.execute(
                "DELETE FROM agent_step_logs WHERE output_id=?1",
                params![second_id],
            )
            .unwrap();
            assert!(!cas_blob_still_referenced(&conn, &hash));
            assert_eq!(sweep_orphan_output_blobs(&conn, &get_outputs_dir()), 1);
        }
        assert!(!blob.exists());
    }

    #[test]
    fn saves_and_loads_steps_in_session_order() {
        let db = test_db();
        upsert_snapshot(
            &db,
            "run-a",
            "session-1",
            "创建成绩单",
            "running",
            &[json!({"id": 0, "description": "创建工作簿"})],
            None,
        )
        .unwrap();
        upsert_snapshot(
            &db,
            "run-a",
            "session-1",
            "创建成绩单",
            "succeeded",
            &[json!({"id": 0, "status": "succeeded"})],
            Some(&json!({"summary": "完成"})),
        )
        .unwrap();
        let traces = list_session_traces(&db, "session-1").unwrap();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0]["status"], "succeeded");
        assert_eq!(traces[0]["completion"]["summary"], "完成");
    }

    #[test]
    fn history_projects_review_steps_only_for_the_owning_run_and_session() {
        let db = test_db();
        upsert_snapshot(&db, "parent", "session-1", "目标", "running", &[], None).unwrap();
        let child_step = json!({"id":0,"tool":"parallel","params":{"parallel":[
            {"tool":"read_file","params":{"path":"service.log","start_line":15,"secret":"omit"}}
        ]},"thought":"omit","output":"omit","status":"succeeded"});
        {
            let conn = db.0.lock().unwrap();
            conn.execute_batch(
                "CREATE TABLE agent_child_runs (
                    id TEXT, purpose TEXT, model_id TEXT, status TEXT, duration_ms INTEGER,
                    execution_checkpoint_json TEXT, parent_session_id TEXT, parent_run_id TEXT,
                    kind TEXT, started_at TEXT
                );
                CREATE TABLE agent_run_steps (run_id TEXT, seq INTEGER, step_json TEXT);
                INSERT INTO agent_child_runs VALUES
                    ('review', 'plan_review', 'model', 'running', NULL, NULL, 'session-1', 'parent', 'review', '1'),
                    ('other-session', 'plan_review', 'model', 'running', NULL, NULL, 'session-2', 'parent', 'review', '1'),
                    ('other-run', 'stage_review', 'model', 'running', NULL, NULL, 'session-1', 'other', 'review', '1'),
                    ('task', 'task', 'model', 'running', NULL, NULL, 'session-1', 'parent', 'task', '1');",
            ).unwrap();
            conn.execute("INSERT INTO agent_run_steps VALUES ('review', 0, ?1)", [child_step.to_string()]).unwrap();
        }
        let trace = &list_session_traces(&db, "session-1").unwrap()[0];
        let members = trace["reviewers"].as_array().unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0]["id"], "review");
        let step = &members[0]["steps"][0];
        assert_eq!(step["status"], "succeeded");
        assert_eq!(step["params"]["parallel"][0]["params"]["path"], "service.log");
        assert!(!step.to_string().contains("omit"));
        let live = crate::agent::child_task::relay_review_step("review", &json!({"type":"step_new","step":child_step})).unwrap();
        assert_eq!(live["child_run_id"], "review");
        assert_eq!(&live["event"]["step"], step);
    }

    #[test]
    fn review_step_history_resolves_model_names_main_review_metadata_and_failure_details() {
        let db = test_db();
        upsert_snapshot(&db, "parent", "session-1", "制作视频", "succeeded", &[], None).unwrap();
        {
            let conn = db.0.lock().unwrap();
            conn.execute_batch(
                "CREATE TABLE models (id TEXT PRIMARY KEY, name TEXT, display_name TEXT);
                CREATE TABLE agent_child_runs (
                    id TEXT PRIMARY KEY, purpose TEXT, model_id TEXT, status TEXT, duration_ms INTEGER,
                    error_kind TEXT, result_json TEXT, execution_checkpoint_json TEXT,
                    parent_session_id TEXT, parent_run_id TEXT, kind TEXT, started_at TEXT
                );
                CREATE TABLE agent_run_steps (run_id TEXT, seq INTEGER, step_json TEXT);
                INSERT INTO models VALUES
                    ('m-uuid-1', 'nex-n2.6-raw', 'nex-n2.6'),
                    ('m-uuid-2', 'fallback-name', '');",
            )
            .unwrap();
            let failed_cp = json!({
                "state": {
                    "main_review": {
                        "role": "reviewer",
                        "role_label": "基础审查员",
                        "parent_step_id": 30
                    }
                }
            })
            .to_string();
            let rejected_cp = json!({
                "state": {
                    "main_review": {
                        "role": "reviewer",
                        "role_label": "安全审查员",
                        "parent_step_id": 31
                    }
                }
            })
            .to_string();
            let passed_cp = json!({
                "state": {
                    "main_review": {
                        "role": "reviewer",
                        "role_label": "基础审查员",
                        "parent_step_id": 32
                    }
                }
            })
            .to_string();
            conn.execute(
                "INSERT INTO agent_child_runs VALUES
                    ('rev-1', 'final_review', 'm-uuid-1', 'failed', 8653,
                     '模型指令恢复已用完 2 次尝试；未取得可执行指令（空响应或仅思考过程）\n更多堆栈',
                     NULL, ?1, 'session-1', 'parent', 'review', '2026-11-13T09:06:09Z'),
                    ('rev-2', 'final_review', 'm-uuid-2', 'succeeded', 12000,
                     NULL, ?2, ?3, 'session-1', 'parent', 'review', '2026-11-13T09:06:20Z'),
                    ('rev-3', 'final_review', 'm-missing', 'succeeded', 34950,
                     NULL, ?4, ?5, 'session-1', 'parent', 'review', '2026-11-13T09:06:26Z');",
                params![
                    failed_cp,
                    json!({
                        "verdict": "rejected",
                        "summary": "发现阻塞项",
                        "blocking_issues": [{ "file": "reel.mp4", "issue": "抽帧画面全黑", "requirement": "完整短片" }]
                    })
                    .to_string(),
                    rejected_cp,
                    json!({ "verdict": "passed", "summary": "全部核验通过" }).to_string(),
                    passed_cp,
                ],
            )
            .unwrap();
        }
        let trace = &list_session_traces(&db, "session-1").unwrap()[0];
        let members = trace["reviewers"].as_array().unwrap();
        assert_eq!(members.len(), 3);

        assert_eq!(members[0]["id"], "rev-1");
        assert_eq!(members[0]["model"], "nex-n2.6");
        assert_eq!(members[0]["label"], "基础审查员");
        assert_eq!(members[0]["parent_step_id"], 30);
        assert_eq!(
            members[0]["error_detail"],
            "模型指令恢复已用完 2 次尝试；未取得可执行指令（空响应或仅思考过程）"
        );

        assert_eq!(members[1]["id"], "rev-2");
        assert_eq!(members[1]["model"], "fallback-name");
        assert_eq!(members[1]["label"], "安全审查员");
        assert_eq!(members[1]["parent_step_id"], 31);
        assert_eq!(members[1]["verdict"], "rejected");
        assert_eq!(members[1]["error_detail"], "抽帧画面全黑");

        assert_eq!(members[2]["id"], "rev-3");
        assert_eq!(members[2]["model"], "m-missing");
        assert_eq!(members[2]["label"], "基础审查员");
        assert_eq!(members[2]["parent_step_id"], 32);
        assert_eq!(members[2]["verdict"], "passed");
        assert!(members[2]["error_detail"].is_null());
    }
}
