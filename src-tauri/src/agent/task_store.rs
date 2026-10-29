use crate::agent::types::DispatchResult;
use crate::db::DbState;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

const DEFAULT_PAGE_SIZE: usize = 25;
const MAX_PAGE_SIZE: usize = 100;
const DEFAULT_STEP_OUTPUT_PAGE_CHARS: usize = 3_000;
const MAX_STEP_OUTPUT_PAGE_CHARS: usize = 6_000;
const MAX_STEP_OUTPUT_CONTEXT_LINES: usize = 8;
const MAX_STEP_OUTPUT_MATCHES: usize = 8;

/// Checkpoint 与提示词中保存的只是资源目录，不保存资源记录本体。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskResourceRef {
    pub resource_id: String,
    pub kind: String,
    pub label: String,
    pub summary: String,
    pub status: String,
    pub row_count: i64,
    #[serde(default)]
    pub cursor: Option<Value>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
struct TaskRecord {
    key: String,
    data: Value,
    updated_at: String,
}

pub fn list_resource_refs(db: &DbState, session_id: &str) -> Result<Vec<TaskResourceRef>, String> {
    let conn = db.reader()?;
    list_resource_refs_conn(&conn, session_id)
}

/// 前端只读分页入口，复用与 Agent 内部工具相同的 session 所有权校验与页大小上限。
pub fn list_records_page(
    db: &DbState,
    session_id: &str,
    resource_id: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<Value, String> {
    let conn = db.reader()?;
    let mut params_value = json!({
        "resource_id": resource_id,
        "offset": offset.unwrap_or(0),
    });
    if let Some(limit) = limit {
        params_value["limit"] = json!(limit);
    }
    list_records(&conn, session_id, &params_value)
}

/// 内部工具入口。它仅操作当前 session 的任务数据，不能跨 session 读取资源。
pub fn dispatch(
    db: &DbState,
    session_id: &str,
    action: &str,
    params_value: &Value,
) -> DispatchResult {
    dispatch_in_workspace(db, session_id, action, params_value, None)
}

pub fn dispatch_in_workspace(
    db: &DbState,
    session_id: &str,
    action: &str,
    params_value: &Value,
    workspace_root: Option<&std::path::Path>,
) -> DispatchResult {
    if matches!(action, "evidence_record" | "evidence_find" | "evidence_inspect" | "evidence_compare") {
        return match crate::agent::evidence_store::dispatch(
            db, session_id, action, params_value, workspace_root
        ) {
            Ok(payload) => DispatchResult {
                view: None, stdout: serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
                stderr: String::new(), success: true, route: format!("task_store:{action}"),
            },
            Err(error) => DispatchResult {
                view: None, stdout: String::new(), stderr: error,
                success: false, route: format!("task_store:{action}"),
            },
        };
    }
    let raw_requested = params_value
        .get("raw")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || params_value
            .get("projection")
            .and_then(|v| v.as_str())
            == Some("raw");
    let result = (|| -> Result<Value, String> {
        if action == "get_work_group" {
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            return crate::agent::work_history::get_group(&conn, session_id, params_value);
        }
        if action == "get_step_output" || action == "get_output" {
            let output_id = optional_string(params_value, "output_id")
                .or_else(|| optional_string(params_value, "id"))
                .ok_or_else(|| "task_store 参数缺少 'output_id'。\n💡 正确示例: {\"action\": \"get_step_output\", \"params\": {\"output_id\": \"out-xxx\"}}".to_string())?;
            let persisted = crate::agent::run_trace::get_step_output(db, session_id, &output_id)?;
            let source = crate::agent::run_trace::saved_output_source(db, session_id, &output_id)?;
            let branch = crate::agent::plan_tree::output::select_branch(&persisted, params_value)?;
            let selection = branch.as_ref().map(|branch| branch.selection.clone());
            let persisted = branch.map(|branch| branch.text).unwrap_or(persisted);
            let projected = !raw_requested
                && crate::agent::dispatcher::project_mcp_durable_output(&persisted).is_some();
            let text = if projected {
                crate::agent::dispatcher::project_mcp_durable_output(&persisted)
                    .unwrap_or(persisted)
            } else {
                persisted
            };
            let offset = params_value
                .get("offset")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let requested_limit = params_value
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(DEFAULT_STEP_OUTPUT_PAGE_CHARS as u64)
                as usize;
            let effective_limit = requested_limit.clamp(1, MAX_STEP_OUTPUT_PAGE_CHARS);
            let total = text.chars().count();
            let sliced = text
                .chars()
                .skip(offset)
                .take(effective_limit)
                .collect::<String>();
            let returned_chars = sliced.chars().count();
            let mut payload = json!({
                "output_id": output_id,
                "source": source,
                "offset": offset,
                "next_offset": offset.saturating_add(returned_chars),
                "requested_limit": requested_limit,
                "effective_limit": effective_limit,
                "total_chars": total,
                "raw_output": sliced,
                "has_more": offset.saturating_add(returned_chars) < total,
                "projected": projected,
                "notice": if projected {
                    "该 output_id 为 MCP 证据包，默认返回模型可读投影；二进制/_meta/未投影字段在原始记录中，传 raw: true 可取回完整 JSON。若仍需证据，请先用关键词、文件路径、函数/符号或 diff hunk 缩小范围，再请求下一个最小片段。"
                } else {
                    "工件回查按页返回。若仍需证据，请先用关键词、文件路径、函数/符号或 diff hunk 缩小范围，再请求下一个最小片段。"
                }
            });
            if let Some(selection) = selection { payload["selection"] = selection; }
            return Ok(payload);
        }

        if action == "search_step_output"
            || action == "grep_step_output"
            || action == "search_output"
        {
            let output_id = optional_string(params_value, "output_id")
                .or_else(|| optional_string(params_value, "id"))
                .ok_or_else(|| "task_store 参数缺少 'output_id'。\n💡 正确示例: {\"action\": \"search_step_output\", \"params\": {\"output_id\": \"out-xxx\", \"pattern\": \"关键词\"}}".to_string())?;
            let pattern = required_string(params_value, "pattern")?;
            let requested_context_lines = params_value
                .get("context_lines")
                .and_then(|v| v.as_u64())
                .unwrap_or(3) as usize;
            let requested_max_matches = params_value
                .get("max_matches")
                .and_then(|v| v.as_u64())
                .unwrap_or(4) as usize;
            let context_lines = requested_context_lines.min(MAX_STEP_OUTPUT_CONTEXT_LINES);
            let max_matches = requested_max_matches.clamp(1, MAX_STEP_OUTPUT_MATCHES);
            let persisted = crate::agent::run_trace::get_step_output(db, session_id, &output_id)?;
            let source = crate::agent::run_trace::saved_output_source(db, session_id, &output_id)?;
            let branch = crate::agent::plan_tree::output::select_branch(&persisted, params_value)?;
            let selection = branch.as_ref().map(|branch| branch.selection.clone());
            let persisted = branch.map(|branch| branch.text).unwrap_or(persisted);
            let projected = !raw_requested
                && crate::agent::dispatcher::project_mcp_durable_output(&persisted).is_some();
            let text = if projected {
                crate::agent::dispatcher::project_mcp_durable_output(&persisted)
                    .unwrap_or(persisted)
            } else {
                persisted
            };

            let lines: Vec<&str> = text.lines().collect();
            let lower_pattern = pattern.to_lowercase();
            let mut matches = Vec::new();
            let mut total_match_count = 0usize;

            for (idx, line) in lines.iter().enumerate() {
                if line.to_lowercase().contains(&lower_pattern) {
                    total_match_count += 1;
                    if matches.len() < max_matches {
                        let start = idx.saturating_sub(context_lines);
                        let end = std::cmp::min(idx + context_lines + 1, lines.len());
                        let snippet = lines[start..end].join("\n");
                        matches.push(json!({
                            "line_number": idx + 1,
                            "snippet": snippet
                        }));
                    }
                }
            }

            let mut payload = json!({
                "output_id": output_id,
                "source": source,
                "pattern": pattern,
                "total_matches": total_match_count,
                "returned_matches": matches.len(),
                "requested_context_lines": requested_context_lines,
                "effective_context_lines": context_lines,
                "requested_max_matches": requested_max_matches,
                "effective_max_matches": max_matches,
                "matches": matches,
                "projected": projected,
                "notice": if projected {
                    "该 output_id 为 MCP 证据包，检索针对模型可读投影；二进制/_meta 不参与匹配，传 raw: true 可检索原始 JSON。"
                } else {
                    "搜索结果已受预算限制。若命中过多，请使用更具体的函数名、路径、错误码或 diff hunk 再次定向检索。"
                }
            });
            if let Some(selection) = selection { payload["selection"] = selection; }
            return Ok(payload);
        }

        let mut conn = db.0.lock().map_err(|e| e.to_string())?;
        match action {
            "create_resource" => create_resource(&mut conn, session_id, params_value),
            "upsert_records" => upsert_records(&mut conn, session_id, params_value),
            "list_resources" => Ok(json!({
                "resources": list_resource_refs_conn(&conn, session_id)?,
            })),
            "get_resource" => get_resource(&conn, session_id, params_value),
            "list_records" => list_records(&conn, session_id, params_value),
            "get_record" => get_record(&conn, session_id, params_value),
            "update_resource" => update_resource(&mut conn, session_id, params_value),
            "mcp_dedup_stats" => mcp_dedup_stats(&conn, session_id),
            _ => Err(format!(
                "task_store 不支持 action='{}'。可用动作：create_resource、upsert_records、list_resources、get_resource、list_records、get_record、update_resource, get_step_output, search_step_output, get_work_group, mcp_dedup_stats, evidence_record, evidence_find, evidence_inspect, evidence_compare",
                action
            )),
        }
    })();

    match result {
        Ok(payload) => DispatchResult { view: None,
            stdout: serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
            stderr: String::new(),
            success: true,
            route: format!("task_store:{}", action),
        },
        Err(err) => DispatchResult { view: None,
            stdout: String::new(),
            stderr: err,
            success: false,
            route: format!("task_store:{}", action),
        },
    }
}

/// 会话级 MCP 去重收益统计：只读聚合 agent_run_steps 里的指纹字段，
/// 不触碰步骤输出原文；用于观测折叠命中率与高频重复调用。
fn mcp_dedup_stats(conn: &Connection, session_id: &str) -> Result<Value, String> {
    let mut stmt = conn
        .prepare(
            "SELECT rs.step_json FROM agent_run_steps rs
             JOIN agent_run_traces t ON t.id = rs.run_id
             WHERE t.session_id = ?1",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![session_id], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;

    let mut evidence_steps = 0usize;
    let mut folded_steps = 0usize;
    let mut anchors: HashSet<String> = HashSet::new();
    let mut per_call: HashMap<String, usize> = HashMap::new();
    let mut folded_chars = 0usize;

    for row in rows.flatten() {
        let Ok(step) = serde_json::from_str::<Value>(&row) else {
            continue;
        };
        let Some(call_hash) = step
            .get("mcp_call_hash")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        evidence_steps += 1;
        *per_call.entry(call_hash.to_string()).or_default() += 1;
        if step
            .get("duplicate_of_step")
            .and_then(|v| v.as_i64())
            .is_some()
        {
            folded_steps += 1;
            folded_chars += step
                .get("output")
                .and_then(|v| v.as_str())
                .map(|s| s.chars().count())
                .unwrap_or(0);
            if let Some(canonical) = step
                .get("canonical_output_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                anchors.insert(canonical.to_string());
            }
        }
    }

    let mut repeated: Vec<(String, usize)> = per_call
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .collect();
    repeated.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    repeated.truncate(10);

    Ok(json!({
        "session_id": session_id,
        "mcp_evidence_steps": evidence_steps,
        "folded_duplicate_steps": folded_steps,
        "distinct_canonical_anchors": anchors.len(),
        "folded_context_chars": folded_chars,
        "repeat_call_groups": repeated.len(),
        "top_repeated_calls": repeated
            .iter()
            .map(|(hash, count)| json!({
                "call_hash_prefix": &hash[..hash.len().min(12)],
                "count": count,
            }))
            .collect::<Vec<_>>(),
        "note": "mcp_evidence_steps=产生完整证据 sidecar 的步数；folded_duplicate_steps=同参同结果且旧表示仍驻留上下文、被折叠为引用的步数；无法证明重复的调用一律全文放行。",
    }))
}

fn create_resource(
    conn: &mut Connection,
    session_id: &str,
    params_value: &Value,
) -> Result<Value, String> {
    let kind = required_string(params_value, "kind")?;
    let label = optional_string(params_value, "label").unwrap_or_else(|| kind.clone());
    let resource_id = optional_string(params_value, "resource_id")
        .unwrap_or_else(|| stable_resource_id(session_id, &kind, &label));
    let summary = optional_string(params_value, "summary").unwrap_or_default();
    let status = optional_string(params_value, "status").unwrap_or_else(|| "active".to_string());
    let schema = params_value
        .get("schema")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let cursor = params_value.get("cursor").cloned();
    let now = now();

    let changed = conn
        .execute(
            "INSERT INTO agent_task_resources (
                resource_id, session_id, kind, label, schema_json, summary, status, row_count,
                cursor_json, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9, ?9)",
            params![
                resource_id,
                session_id,
                kind,
                label,
                serialize_json(&schema)?,
                summary,
                status,
                cursor.map(|value| serialize_json(&value)).transpose()?,
                now,
            ],
        )
        .map_err(|e| {
            if e.to_string().contains("UNIQUE") {
                format!(
                    "任务资源 '{}' 已存在；请复用它或使用不同 resource_id",
                    resource_id
                )
            } else {
                e.to_string()
            }
        })?;

    if changed != 1 {
        return Err("创建任务资源失败".to_string());
    }

    let resource = get_resource_ref_conn(conn, session_id, &resource_id)?;
    Ok(json!({ "resource": resource }))
}

fn upsert_records(
    conn: &mut Connection,
    session_id: &str,
    params_value: &Value,
) -> Result<Value, String> {
    let resource_id = required_string(params_value, "resource_id")?;
    ensure_resource_owner(conn, session_id, &resource_id)?;
    let records = params_value
        .get("records")
        .and_then(Value::as_array)
        .ok_or_else(|| "upsert_records 需要 records 数组".to_string())?;
    if records.is_empty() {
        return Err("upsert_records 的 records 不能为空".to_string());
    }
    if records.len() > 1_000 {
        return Err("单次最多写入 1000 条记录；请分批写入以保持恢复边界清晰".to_string());
    }

    let summary = optional_string(params_value, "summary");
    let status = optional_string(params_value, "status");
    let cursor = params_value.get("cursor").cloned();
    let now = now();
    let tx = conn.transaction().map_err(|e| e.to_string())?;

    for (index, record) in records.iter().enumerate() {
        let key = record_key(&resource_id, record, index);
        tx.execute(
            "INSERT INTO agent_task_records (
                resource_id, record_key, data_json, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(resource_id, record_key) DO UPDATE SET
                data_json = excluded.data_json,
                updated_at = excluded.updated_at",
            params![resource_id, key, serialize_json(record)?, now],
        )
        .map_err(|e| e.to_string())?;
    }

    tx.execute(
        "UPDATE agent_task_resources
         SET row_count = (SELECT COUNT(*) FROM agent_task_records WHERE resource_id = ?1),
             summary = COALESCE(?2, summary),
             status = COALESCE(?3, status),
             cursor_json = COALESCE(?4, cursor_json),
             updated_at = ?5
         WHERE resource_id = ?1 AND session_id = ?6",
        params![
            resource_id,
            summary,
            status,
            cursor.map(|value| serialize_json(&value)).transpose()?,
            now,
            session_id,
        ],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;

    let resource = get_resource_ref_conn(conn, session_id, &resource_id)?;
    Ok(json!({
        "resource": resource,
        "written": records.len(),
        "note": "记录已实时持久化到 session 任务数据区；checkpoint 仅保存资源引用与摘要。",
    }))
}

fn get_resource(
    conn: &Connection,
    session_id: &str,
    params_value: &Value,
) -> Result<Value, String> {
    let resource_id = required_string(params_value, "resource_id")?;
    let resource = get_resource_ref_conn(conn, session_id, &resource_id)?;
    let schema_json: String = conn
        .query_row(
            "SELECT schema_json FROM agent_task_resources WHERE resource_id = ?1 AND session_id = ?2",
            params![resource_id, session_id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    let schema = parse_json(&schema_json)?;
    Ok(json!({ "resource": resource, "schema": schema }))
}

fn update_resource(
    conn: &mut Connection,
    session_id: &str,
    params_value: &Value,
) -> Result<Value, String> {
    let resource_id = required_string(params_value, "resource_id")?;
    ensure_resource_owner(conn, session_id, &resource_id)?;
    let label = optional_string(params_value, "label");
    let summary = optional_string(params_value, "summary");
    let status = optional_string(params_value, "status");
    let cursor = params_value.get("cursor").cloned();
    let schema = params_value.get("schema").cloned();

    if label.is_none()
        && summary.is_none()
        && status.is_none()
        && cursor.is_none()
        && schema.is_none()
    {
        return Err(
            "update_resource 至少需要 label、summary、status、cursor 或 schema 之一".to_string(),
        );
    }

    conn.execute(
        "UPDATE agent_task_resources
         SET label = COALESCE(?1, label),
             summary = COALESCE(?2, summary),
             status = COALESCE(?3, status),
             cursor_json = COALESCE(?4, cursor_json),
             schema_json = COALESCE(?5, schema_json),
             updated_at = ?6
         WHERE resource_id = ?7 AND session_id = ?8",
        params![
            label,
            summary,
            status,
            cursor.map(|value| serialize_json(&value)).transpose()?,
            schema.map(|value| serialize_json(&value)).transpose()?,
            now(),
            resource_id,
            session_id,
        ],
    )
    .map_err(|e| e.to_string())?;

    Ok(json!({
        "resource": get_resource_ref_conn(conn, session_id, &resource_id)?,
    }))
}

fn list_records(
    conn: &Connection,
    session_id: &str,
    params_value: &Value,
) -> Result<Value, String> {
    let resource_id = required_string(params_value, "resource_id")?;
    ensure_resource_owner(conn, session_id, &resource_id)?;
    let limit = page_limit(params_value);
    let offset = params_value
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;

    let total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_task_records WHERE resource_id = ?1",
            params![resource_id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    let mut statement = conn
        .prepare(
            "SELECT record_key, data_json, updated_at
             FROM agent_task_records
             WHERE resource_id = ?1
             ORDER BY record_key
             LIMIT ?2 OFFSET ?3",
        )
        .map_err(|e| e.to_string())?;
    let records = statement
        .query_map(params![resource_id, limit as i64, offset as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .map(|row| {
            let (key, data_json, updated_at) = row.map_err(|e| e.to_string())?;
            Ok(TaskRecord {
                key,
                data: parse_json(&data_json)?,
                updated_at,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let next_offset = offset + records.len();

    Ok(json!({
        "resource_id": resource_id,
        "records": records,
        "page": {
            "limit": limit,
            "offset": offset,
            "total": total,
            "has_more": next_offset < total as usize,
            "next_offset": if next_offset < total as usize { Some(next_offset) } else { None::<usize> },
        },
    }))
}

fn get_record(conn: &Connection, session_id: &str, params_value: &Value) -> Result<Value, String> {
    let resource_id = required_string(params_value, "resource_id")?;
    let record_key = required_string(params_value, "record_key")?;
    ensure_resource_owner(conn, session_id, &resource_id)?;

    let record = conn
        .query_row(
            "SELECT data_json, updated_at
             FROM agent_task_records
             WHERE resource_id = ?1 AND record_key = ?2",
            params![resource_id, record_key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "资源 '{}' 中不存在 record_key='{}'",
                resource_id, record_key
            )
        })?;

    Ok(json!({
        "resource_id": resource_id,
        "record": TaskRecord {
            key: record_key,
            data: parse_json(&record.0)?,
            updated_at: record.1,
        },
    }))
}

fn list_resource_refs_conn(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<TaskResourceRef>, String> {
    let mut statement = conn
        .prepare(
            "SELECT resource_id, kind, label, summary, status, row_count, cursor_json, updated_at
             FROM agent_task_resources
             WHERE session_id = ?1
             ORDER BY updated_at DESC, resource_id ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map(params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let resources = rows
        .map(|row| {
            let (resource_id, kind, label, summary, status, row_count, cursor_json, updated_at) =
                row.map_err(|e| e.to_string())?;
            Ok(TaskResourceRef {
                resource_id,
                kind,
                label,
                summary,
                status,
                row_count,
                cursor: cursor_json.as_deref().map(parse_json).transpose()?,
                updated_at,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(resources)
}

fn get_resource_ref_conn(
    conn: &Connection,
    session_id: &str,
    resource_id: &str,
) -> Result<TaskResourceRef, String> {
    let resource = conn
        .query_row(
            "SELECT resource_id, kind, label, summary, status, row_count, cursor_json, updated_at
             FROM agent_task_resources
             WHERE resource_id = ?1 AND session_id = ?2",
            params![resource_id, session_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("当前 session 不存在任务资源 '{}'", resource_id))?;

    Ok(TaskResourceRef {
        resource_id: resource.0,
        kind: resource.1,
        label: resource.2,
        summary: resource.3,
        status: resource.4,
        row_count: resource.5,
        cursor: resource.6.as_deref().map(parse_json).transpose()?,
        updated_at: resource.7,
    })
}

fn ensure_resource_owner(
    conn: &Connection,
    session_id: &str,
    resource_id: &str,
) -> Result<(), String> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM agent_task_resources WHERE resource_id = ?1 AND session_id = ?2",
            params![resource_id, session_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if exists.is_some() {
        Ok(())
    } else {
        Err(format!("当前 session 不拥有任务资源 '{}'", resource_id))
    }
}

fn required_string(params_value: &Value, key: &str) -> Result<String, String> {
    optional_string(params_value, key)
        .ok_or_else(|| format!("task_store 参数缺少非空字符串 '{}'", key))
}

fn optional_string(params_value: &Value, key: &str) -> Option<String> {
    params_value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn record_key(resource_id: &str, record: &Value, index: usize) -> String {
    for key in ["record_key", "id", "key"] {
        if let Some(value) = record.get(key) {
            if let Some(value) = value.as_str().map(ToOwned::to_owned) {
                if !value.trim().is_empty() {
                    return value;
                }
            }
            if let Some(value) = value.as_i64() {
                return value.to_string();
            }
            if let Some(value) = value.as_u64() {
                return value.to_string();
            }
        }
    }
    let canonical = serde_json::to_string(record).unwrap_or_else(|_| format!("record-{}", index));
    Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!("{}:{}", resource_id, canonical).as_bytes(),
    )
    .to_string()
}

fn stable_resource_id(session_id: &str, kind: &str, label: &str) -> String {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("free-api-task-store:{}:{}:{}", session_id, kind, label).as_bytes(),
    )
    .to_string()
}

fn page_limit(params_value: &Value) -> usize {
    params_value
        .get("limit")
        .and_then(Value::as_u64)
        .map(|limit| limit as usize)
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE)
}

fn serialize_json(value: &Value) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| e.to_string())
}

fn parse_json(text: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|e| format!("任务数据损坏，无法解析 JSON：{}", e))
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn memory_db() -> DbState {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE agent_task_resources (
                resource_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                label TEXT NOT NULL,
                schema_json TEXT NOT NULL DEFAULT '{}',
                summary TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'active',
                row_count INTEGER NOT NULL DEFAULT 0,
                cursor_json TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE agent_task_records (
                resource_id TEXT NOT NULL,
                record_key TEXT NOT NULL,
                data_json TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (resource_id, record_key)
            );
            ",
        )
        .unwrap();
        DbState(Arc::new(Mutex::new(conn)))
    }

    #[test]
    fn resources_are_session_scoped_and_records_are_recoverable() {
        let db = memory_db();
        let created = dispatch(
            &db,
            "session-a",
            "create_resource",
            &json!({
                "resource_id": "people",
                "kind": "people",
                "label": "候选人资料",
                "summary": "待采集"
            }),
        );
        assert!(created.success);

        let written = dispatch(
            &db,
            "session-a",
            "upsert_records",
            &json!({
                "resource_id": "people",
                "records": [
                    {"id": "p-1", "name": "甲", "city": "杭州"},
                    {"id": "p-2", "name": "乙", "city": "上海"}
                ],
                "cursor": {"processed": 2},
                "summary": "已采集 2 人"
            }),
        );
        assert!(written.success);

        let refs = list_resource_refs(&db, "session-a").unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].row_count, 2);
        assert_eq!(refs[0].cursor, Some(json!({"processed": 2})));
        assert_eq!(refs[0].summary, "已采集 2 人");

        let page = dispatch(
            &db,
            "session-a",
            "list_records",
            &json!({"resource_id": "people", "limit": 1}),
        );
        assert!(page.success);
        assert!(page.stdout.contains("p-1"));
        assert!(page.stdout.contains("has_more"));

        let denied = dispatch(
            &db,
            "session-b",
            "list_records",
            &json!({"resource_id": "people"}),
        );
        assert!(!denied.success);
        assert!(denied.stderr.contains("不拥有"));
    }

    #[test]
    fn upsert_replaces_a_record_without_growing_the_resource_count() {
        let db = memory_db();
        assert!(
            dispatch(
                &db,
                "session-a",
                "create_resource",
                &json!({"resource_id": "people", "kind": "people"}),
            )
            .success
        );
        assert!(
            dispatch(
                &db,
                "session-a",
                "upsert_records",
                &json!({"resource_id": "people", "records": [{"id": "p-1", "name": "旧值"}]}),
            )
            .success
        );
        assert!(
            dispatch(
                &db,
                "session-a",
                "upsert_records",
                &json!({"resource_id": "people", "records": [{"id": "p-1", "name": "新值"}]}),
            )
            .success
        );

        let refs = list_resource_refs(&db, "session-a").unwrap();
        assert_eq!(refs[0].row_count, 1);
        let result = dispatch(
            &db,
            "session-a",
            "get_record",
            &json!({"resource_id": "people", "record_key": "p-1"}),
        );
        assert!(result.success);
        assert!(result.stdout.contains("新值"));
    }

    #[test]
    fn frontend_page_api_is_session_scoped_and_paginates_records() {
        let db = memory_db();
        assert!(
            dispatch(
                &db,
                "session-a",
                "create_resource",
                &json!({
                    "resource_id": "people",
                    "kind": "people",
                    "label": "候选人资料"
                }),
            )
            .success
        );
        assert!(
            dispatch(
                &db,
                "session-a",
                "upsert_records",
                &json!({
                    "resource_id": "people",
                    "records": [
                        {"id": "p-1", "name": "甲"},
                        {"id": "p-2", "name": "乙"},
                        {"id": "p-3", "name": "丙"}
                    ]
                }),
            )
            .success
        );

        let page = list_records_page(&db, "session-a", "people", Some(1), Some(1)).unwrap();
        assert_eq!(page["page"]["offset"], 1);
        assert_eq!(page["page"]["total"], 3);
        assert_eq!(page["records"].as_array().unwrap().len(), 1);
        assert_eq!(page["records"][0]["key"], "p-2");

        assert!(list_records_page(&db, "session-b", "people", Some(0), Some(1)).is_err());
    }

    fn memory_db_with_step_logs() -> DbState {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE agent_step_logs (
                output_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                step_id INTEGER NOT NULL,
                raw_output TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE agent_run_traces (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL
            );
            CREATE TABLE agent_run_steps (
                run_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                step_json TEXT NOT NULL,
                PRIMARY KEY (run_id, seq)
            );",
        )
        .unwrap();
        DbState(Arc::new(Mutex::new(conn)))
    }

    #[test]
    fn get_step_output_projects_mcp_sidecar_and_raw_opt_in() {
        let db = memory_db_with_step_logs();
        let sidecar = serde_json::to_string_pretty(
            &crate::agent::dispatcher::mcp_raw_result_view(
                "weather",
                "get",
                &json!({"q": "x"}),
                &json!({
                    "content": [
                        {"type": "text", "text": "可见正文"},
                        {"type": "image", "data": "QkFTRTY0U0VOVElORUw=", "mimeType": "image/png"}
                    ],
                    "_meta": {"token": "SECRET-META"},
                }),
            ),
        )
        .unwrap();
        let oid = crate::agent::run_trace::save_step_output_with_meta(
            &db, "s-proj", 0, "weather/get", "call", "succeeded", &sidecar,
        )
        .unwrap();

        let res = dispatch(&db, "s-proj", "get_step_output", &json!({"output_id": oid}));
        assert!(res.success);
        let out: Value = serde_json::from_str(&res.stdout).unwrap();
        assert_eq!(out["projected"], true);
        let body = out["raw_output"].as_str().unwrap();
        assert!(body.contains("可见正文"));
        assert!(body.contains("weather/get"));
        assert!(!body.contains("SECRET-META"));
        assert!(!body.contains("QkFTRTY0U0VOVElORUw="));

        let raw_res = dispatch(
            &db,
            "s-proj",
            "get_step_output",
            &json!({"output_id": oid, "raw": true}),
        );
        let raw_out: Value = serde_json::from_str(&raw_res.stdout).unwrap();
        assert_eq!(raw_out["projected"], false);
        assert!(raw_out["raw_output"].as_str().unwrap().contains("SECRET-META"));

        let search = dispatch(
            &db,
            "s-proj",
            "search_step_output",
            &json!({"output_id": oid, "pattern": "SECRET-META"}),
        );
        let search_out: Value = serde_json::from_str(&search.stdout).unwrap();
        assert_eq!(search_out["projected"], true);
        assert_eq!(search_out["total_matches"], 0);

        let plain_id = crate::agent::run_trace::save_step_output_with_meta(
            &db, "s-proj", 1, "local_file", "read", "succeeded", "普通正文",
        )
        .unwrap();
        let plain = dispatch(
            &db,
            "s-proj",
            "get_step_output",
            &json!({"output_id": plain_id}),
        );
        let plain_out: Value = serde_json::from_str(&plain.stdout).unwrap();
        assert_eq!(plain_out["projected"], false);
        assert_eq!(plain_out["raw_output"], "普通正文");
    }

    #[test]
    fn mcp_dedup_stats_aggregates_trace_fields() {
        let db = memory_db_with_step_logs();
        {
            let conn = db.0.lock().unwrap();
            conn.execute(
                "INSERT INTO agent_run_traces (id, session_id) VALUES ('r1', 's-stat')",
                [],
            )
            .unwrap();
            let steps = [
                json!({"id": 0, "output": "正文", "mcp_call_hash": "hash-a", "mcp_raw_output_hash": "r1", "mcp_model_output_hash": "m1", "output_id": "o1", "canonical_output_id": "o1"}),
                json!({"id": 1, "output": "【MCP 重复结果已折叠】", "mcp_call_hash": "hash-a", "mcp_raw_output_hash": "r1", "mcp_model_output_hash": "m1", "output_id": "o2", "canonical_output_id": "o1", "duplicate_of_step": 0}),
                json!({"id": 2, "output": "正文B", "mcp_call_hash": "hash-b", "mcp_raw_output_hash": "r2", "mcp_model_output_hash": "m2", "output_id": "o3"}),
                json!({"id": 3, "output": "非 MCP 步"}),
            ];
            for (seq, step) in steps.iter().enumerate() {
                conn.execute(
                    "INSERT INTO agent_run_steps (run_id, seq, step_json) VALUES ('r1', ?1, ?2)",
                    params![seq as i64, step.to_string()],
                )
                .unwrap();
            }
        }
        let res = dispatch(&db, "s-stat", "mcp_dedup_stats", &json!({}));
        assert!(res.success);
        let out: Value = serde_json::from_str(&res.stdout).unwrap();
        assert_eq!(out["mcp_evidence_steps"], 3);
        assert_eq!(out["folded_duplicate_steps"], 1);
        assert_eq!(out["distinct_canonical_anchors"], 1);
        assert_eq!(out["repeat_call_groups"], 1);
        assert_eq!(out["top_repeated_calls"][0]["count"], 2);

        let other = dispatch(&db, "s-other", "mcp_dedup_stats", &json!({}));
        let other_out: Value = serde_json::from_str(&other.stdout).unwrap();
        assert_eq!(other_out["mcp_evidence_steps"], 0);
    }
}
