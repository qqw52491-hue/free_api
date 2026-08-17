use crate::agent::types::DispatchResult;
use crate::db::DbState;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchParams {
    pub query: String,
    #[serde(default)]
    pub kinds: Option<Vec<String>>, // ["history", "code", "all"]
    #[serde(default)]
    pub outcome: Option<String>, // "failed", "succeeded", "any"
    #[serde(default)]
    pub since_step: Option<usize>,
    #[serde(default)]
    pub path_prefix: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub kind: String, // "history" | "code"
    pub id_or_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    pub snippet: String,
}

/// 写入或更新单步执行日志的 FTS5 索引
pub fn index_step_fts(
    conn: &Connection,
    output_id: &str,
    session_id: &str,
    step_id: usize,
    tool: &str,
    action: &str,
    outcome: &str,
    content: &str,
) -> Result<(), String> {
    // 限制单条存入 FTS5 索引的文本长度在合理范围（前 4,000 字符足够分词搜索），避免巨大文档导致索引膨胀
    let content_capped: String = content.chars().take(4_000).collect();

    conn.execute(
        "INSERT INTO agent_step_logs_fts (output_id, session_id, step_id, tool, action, outcome, content)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            output_id,
            session_id,
            step_id as i64,
            tool,
            action,
            outcome,
            content_capped
        ],
    )
    .map_err(|e| format!("FTS5 index insert error: {}", e))?;

    Ok(())
}

/// 基于 SQLite FTS5 的历史执行记录全文检索
pub fn search_history(
    conn: &Connection,
    session_id: &str,
    query: &str,
    outcome_filter: Option<&str>,
    since_step: Option<usize>,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    let clean_query = query.trim();
    if clean_query.is_empty() {
        return Ok(Vec::new());
    }

    // FTS5 简单转义（处理特殊字符）
    let escaped_query = clean_query.replace('"', "\"\"");
    let match_pattern = format!("\"{}\"", escaped_query);

    let mut sql = String::from(
        "SELECT output_id, step_id, tool, action, outcome, snippet(agent_step_logs_fts, 6, '<b>', '</b>', '...', 25), rank
         FROM agent_step_logs_fts
         WHERE agent_step_logs_fts MATCH ?1",
    );

    let mut params_vec: Vec<rusqlite::types::Value> = vec![match_pattern.into()];

    if !session_id.is_empty() {
        sql.push_str(" AND session_id = ?2");
        params_vec.push(session_id.to_string().into());
    }

    if let Some(outcome) = outcome_filter {
        if outcome != "any" && !outcome.is_empty() {
            let next_idx = params_vec.len() + 1;
            sql.push_str(&format!(" AND outcome = ?{}", next_idx));
            params_vec.push(outcome.to_string().into());
        }
    }

    if let Some(since) = since_step {
        let next_idx = params_vec.len() + 1;
        sql.push_str(&format!(" AND step_id >= ?{}", next_idx));
        params_vec.push((since as i64).into());
    }

    let next_idx = params_vec.len() + 1;
    sql.push_str(&format!(" ORDER BY rank ASC LIMIT ?{}", next_idx));
    params_vec.push((limit as i64).into());

    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;

    let rusqlite_params = rusqlite::params_from_iter(params_vec);
    let rows = stmt
        .query_map(rusqlite_params, |row| {
            let output_id: String = row.get(0)?;
            let step_id: i64 = row.get(1)?;
            let tool: String = row.get(2)?;
            let action: String = row.get(3)?;
            let outcome: String = row.get(4)?;
            let snippet: String = row.get(5)?;
            Ok(SearchHit {
                kind: "history".to_string(),
                id_or_path: output_id,
                step_id: Some(step_id as usize),
                tool: Some(tool),
                action: Some(action),
                outcome: Some(outcome),
                snippet,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut hits = Vec::new();
    for r in rows {
        if let Ok(hit) = r {
            hits.push(hit);
        }
    }

    Ok(hits)
}

/// 实时代码与文件搜索（避免过期快照）
pub fn search_code(
    workspace_root: &Path,
    query: &str,
    path_prefix: Option<&str>,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    let clean_query = query.trim();
    if clean_query.is_empty() {
        return Ok(Vec::new());
    }

    let search_dir = if let Some(prefix) = path_prefix {
        let p = workspace_root.join(prefix.trim_start_matches('/'));
        if p.exists() {
            p
        } else {
            workspace_root.to_path_buf()
        }
    } else {
        workspace_root.to_path_buf()
    };

    let mut hits = Vec::new();
    let max_hits = limit.min(20);

    // 优先尝试使用 ripgrep (若系统已安装)
    if let Ok(output) = std::process::Command::new("rg")
        .arg("--json")
        .arg("-m")
        .arg("3")
        .arg("--max-count")
        .arg(max_hits.to_string())
        .arg("-i")
        .arg(clean_query)
        .arg(&search_dir)
        .output()
    {
        if output.status.success() {
            let stdout_str = String::from_utf8_lossy(&output.stdout);
            for line in stdout_str.lines() {
                if hits.len() >= max_hits {
                    break;
                }
                if let Ok(val) = serde_json::from_str::<Value>(line) {
                    if val.get("type").and_then(|v| v.as_str()) == Some("match") {
                        if let Some(data) = val.get("data") {
                            let path_text = data
                                .get("path")
                                .and_then(|p| p.get("text"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let line_num = data
                                .get("line_number")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            let match_lines = data
                                .get("lines")
                                .and_then(|l| l.get("text"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .trim();

                            let rel_path = Path::new(path_text)
                                .strip_prefix(workspace_root)
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_else(|_| path_text.to_string());

                            hits.push(SearchHit {
                                kind: "code".to_string(),
                                id_or_path: format!("{}:{}", rel_path, line_num),
                                step_id: None,
                                tool: None,
                                action: None,
                                outcome: None,
                                snippet: match_lines.to_string(),
                            });
                        }
                    }
                }
            }
            if !hits.is_empty() {
                return Ok(hits);
            }
        }
    }

    Ok(hits)
}

/// 统一联邦检索调度入口
pub fn dispatch_search(
    db: &DbState,
    session_id: &str,
    params_value: &Value,
    workspace_root: &Path,
) -> DispatchResult {
    let params: SearchParams = match serde_json::from_value(params_value.clone()) {
        Ok(p) => p,
        Err(e) => {
            // 兼容直接把 query 作为字符串传入的场景
            if let Some(q) = params_value.as_str() {
                SearchParams {
                    query: q.to_string(),
                    kinds: None,
                    outcome: None,
                    since_step: None,
                    path_prefix: None,
                    limit: Some(10),
                }
            } else {
                return DispatchResult {
                    stdout: String::new(),
                    stderr: format!("搜索参数解析失败: {}", e),
                    success: false,
                    route: "retrieval".to_string(),
                };
            }
        }
    };

    let limit = params.limit.unwrap_or(10).min(30);
    let kinds = params
        .kinds
        .clone()
        .unwrap_or_else(|| vec!["history".to_string(), "code".to_string()]);

    let mut all_hits = Vec::new();

    // 1. 检索历史 (SQLite FTS5)
    if kinds.iter().any(|k| k == "history" || k == "all") {
        if let Ok(conn) = db.0.lock() {
            if let Ok(history_hits) = search_history(
                &conn,
                session_id,
                &params.query,
                params.outcome.as_deref(),
                params.since_step,
                limit,
            ) {
                all_hits.extend(history_hits);
            }
        }
    }

    // 2. 检索实时代码 (ripgrep)
    if kinds.iter().any(|k| k == "code" || k == "all") {
        if let Ok(code_hits) = search_code(
            workspace_root,
            &params.query,
            params.path_prefix.as_deref(),
            limit,
        ) {
            all_hits.extend(code_hits);
        }
    }

    let result_payload = json!({
        "query": params.query,
        "total_hits": all_hits.len(),
        "hits": all_hits
    });

    DispatchResult {
        stdout: serde_json::to_string_pretty(&result_payload)
            .unwrap_or_else(|_| result_payload.to_string()),
        stderr: String::new(),
        success: true,
        route: "retrieval".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE VIRTUAL TABLE agent_step_logs_fts USING fts5(
                output_id UNINDEXED,
                session_id UNINDEXED,
                step_id UNINDEXED,
                tool,
                action,
                outcome,
                content
            );
            ",
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_fts5_indexing_and_history_search() {
        let conn = setup_test_db();

        index_step_fts(
            &conn,
            "out-001",
            "session-test",
            1,
            "shell_exec",
            "cargo build",
            "failed",
            "error[E0425]: cannot find value `db_conn` in this scope in src/db.rs:42",
        )
        .unwrap();

        index_step_fts(
            &conn,
            "out-002",
            "session-test",
            2,
            "write_file",
            "fix db.rs",
            "succeeded",
            "Updated src/db.rs with correct connection pool",
        )
        .unwrap();

        // 1. 搜索报错关键词
        let hits = search_history(&conn, "session-test", "db_conn", None, None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id_or_path, "out-001");
        assert_eq!(hits[0].outcome.as_deref(), Some("failed"));

        // 2. 按 outcome="failed" 过滤
        let failed_hits =
            search_history(&conn, "session-test", "src", Some("failed"), None, 10).unwrap();
        assert_eq!(failed_hits.len(), 1);
        assert_eq!(failed_hits[0].id_or_path, "out-001");

        // 3. 搜索成功记录
        let succ_hits =
            search_history(&conn, "session-test", "pool", Some("succeeded"), None, 10).unwrap();
        assert_eq!(succ_hits.len(), 1);
        assert_eq!(succ_hits[0].id_or_path, "out-002");
    }
}
