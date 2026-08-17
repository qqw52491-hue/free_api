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
    pub kinds: Option<Vec<String>>, // ["history", "code", "all"]，默认仅 history
    #[serde(default)]
    pub outcome: Option<String>, // "failed", "succeeded", "any"（大小写不敏感）
    #[serde(default)]
    pub since_step: Option<usize>,
    #[serde(default)]
    pub path: Option<String>, // code 检索必填：要搜索的目录绝对路径
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

/// 确保 FTS 表使用 trigram 分词器。
/// 早期版本（adecae5）用默认 unicode61 分词器建表——连续中文被当成单个 token，
/// 中文子串检索召回率≈0。检测到旧表时 DROP 重建，并从 agent_step_logs 主表回填索引。
/// 返回回填的行数（无需迁移时返回 0）。
pub fn ensure_trigram_fts(conn: &Connection) -> Result<usize, String> {
    let existing_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='agent_step_logs_fts'",
            [],
            |row| row.get(0),
        )
        .ok();

    let needs_rebuild = match existing_sql {
        Some(sql_text) => !sql_text.to_lowercase().contains("trigram"),
        None => false, // 表不存在时由 init_db 的 CREATE 语句负责新建
    };
    if !needs_rebuild {
        return Ok(0);
    }

    println!("📦 [FTS5 Migration] 检测到旧 unicode61 分词表，重建为 trigram 并回填历史索引...");
    conn.execute_batch(
        "DROP TABLE IF EXISTS agent_step_logs_fts;
         CREATE VIRTUAL TABLE agent_step_logs_fts USING fts5(
            output_id UNINDEXED,
            session_id UNINDEXED,
            step_id UNINDEXED,
            tool,
            action,
            outcome,
            content,
            tokenize='trigram'
         );",
    )
    .map_err(|e| format!("FTS5 trigram 重建失败: {}", e))?;

    rebuild_fts_from_step_logs(conn)
}

/// 从 agent_step_logs 主表回填 FTS 索引。
/// 主表没有 tool/action/outcome 列（元数据只存在于 FTS 侧），回填行以空元数据入库；
/// 溢出落盘的行读回磁盘文件前 4,000 字符建索引，读不到则跳过。
fn rebuild_fts_from_step_logs(conn: &Connection) -> Result<usize, String> {
    let mut stmt = conn
        .prepare("SELECT output_id, session_id, step_id, raw_output FROM agent_step_logs")
        .map_err(|e| format!("读取 agent_step_logs 失败: {}", e))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut indexed = 0usize;
    for row in rows {
        let (output_id, session_id, step_id, raw) = row.map_err(|e| e.to_string())?;
        let content = match raw
            .strip_prefix("[[spilled_to_disk:")
            .and_then(|s| s.strip_suffix("]]"))
        {
            Some(path) => match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(_) => continue, // 溢出文件丢失则跳过该行（原文已不可检索）
            },
            None => raw,
        };
        let _ = index_step_fts(conn, &output_id, &session_id, step_id as usize, "", "", "", &content);
        indexed += 1;
    }

    println!("✅ [FTS5 Migration] trigram 索引回填完成，共 {} 行", indexed);
    Ok(indexed)
}

/// 基于 SQLite FTS5 (trigram 分词) 的历史执行记录全文检索
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
    // trigram 分词器下限为 3 字符，短查询直接返回空（由调用方负责给出提示）
    if clean_query.chars().count() < 3 {
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

    // outcome 过滤归一化：大小写不敏感，"any"/空串视为不过滤
    let normalized_outcome = outcome_filter
        .map(str::trim)
        .filter(|o| !o.is_empty() && !o.eq_ignore_ascii_case("any"))
        .map(str::to_lowercase);
    if let Some(outcome) = normalized_outcome {
        let next_idx = params_vec.len() + 1;
        sql.push_str(&format!(" AND outcome = ?{}", next_idx));
        params_vec.push(outcome.into());
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

/// 实时代码与文件搜索（避免过期快照；必须显式传入检索目录，由调用方校验）
pub fn search_code(search_root: &Path, query: &str, limit: usize) -> Result<Vec<SearchHit>, String> {
    let clean_query = query.trim();
    if clean_query.is_empty() {
        return Ok(Vec::new());
    }

    let max_hits = limit.min(20);

    let output = std::process::Command::new("rg")
        .arg("--json")
        .arg("--max-count")
        .arg(max_hits.to_string())
        .arg("--max-filesize")
        .arg("2M")
        .arg("-i")
        .arg("-g")
        .arg("!target")
        .arg("-g")
        .arg("!node_modules")
        .arg("-g")
        .arg("!dist")
        .arg("-g")
        .arg("!.git")
        .arg(clean_query)
        .arg(search_root)
        .output()
        .map_err(|e| {
            format!(
                "无法启动 ripgrep (rg)：{}。代码检索依赖系统安装的 rg，历史检索不受影响",
                e
            )
        })?;

    // rg 退出码：0=有匹配，1=无匹配（均属正常），>=2 才是真实错误
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(format!(
            "ripgrep 执行失败 (exit={:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let mut hits = Vec::new();

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
                        .strip_prefix(search_root)
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

    Ok(hits)
}

/// 统一联邦检索调度入口
/// 错误绝不静默：任何子检索失败都会写入 errors 字段回传给模型，
/// 避免"搜索失败"被伪装成"0 命中"的假阴性误导决策。
pub fn dispatch_search(db: &DbState, session_id: &str, params_value: &Value) -> DispatchResult {
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
                    path: None,
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
    // 默认只查历史：代码检索必须显式请求并提供 path（避免盲目扫盘）
    let kinds = params
        .kinds
        .clone()
        .unwrap_or_else(|| vec!["history".to_string()]);

    let mut all_hits = Vec::new();
    let mut errors = Vec::new();
    let mut notes = Vec::new();

    let trimmed_query = params.query.trim();
    if !trimmed_query.is_empty() && trimmed_query.chars().count() < 3 {
        notes.push(
            "查询词少于 3 个字符：trigram 分词下限为 3 字符，请输入更长的子串（如「数据库连接」而非「库」）"
                .to_string(),
        );
    }

    let wants_history = kinds.iter().any(|k| k == "history" || k == "all");
    let wants_code = kinds.iter().any(|k| k == "code" || k == "all");

    let mut history_ok = !wants_history;
    if wants_history {
        if let Ok(conn) = db.0.lock() {
            match search_history(
                &conn,
                session_id,
                &params.query,
                params.outcome.as_deref(),
                params.since_step,
                limit,
            ) {
                Ok(hits) => {
                    all_hits.extend(hits);
                    history_ok = true;
                }
                Err(e) => errors.push(format!("history 检索失败: {}", e)),
            }
        } else {
            errors.push("history 检索失败: 数据库锁获取失败".to_string());
        }
    }

    let mut code_ok = !wants_code;
    if wants_code {
        match params.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            None => {
                errors.push("code 检索需要显式 path 参数（要检索的目录绝对路径）".to_string());
            }
            Some(raw_path) => {
                let root = Path::new(raw_path);
                if !root.is_dir() {
                    errors.push(format!("path 不是有效目录: {}", raw_path));
                } else {
                    match search_code(root, &params.query, limit) {
                        Ok(hits) => {
                            all_hits.extend(hits);
                            code_ok = true;
                        }
                        Err(e) => errors.push(format!("code 检索失败: {}", e)),
                    }
                }
            }
        }
    }

    // 只有当"所有被请求的检索分支全部失败"时才算整体失败
    let success = history_ok && code_ok;

    let mut result_payload = json!({
        "query": params.query,
        "total_hits": all_hits.len(),
        "hits": all_hits
    });
    if !errors.is_empty() {
        result_payload["errors"] = json!(errors);
    }
    if !notes.is_empty() {
        result_payload["notes"] = json!(notes);
    }

    DispatchResult {
        stdout: serde_json::to_string_pretty(&result_payload)
            .unwrap_or_else(|_| result_payload.to_string()),
        stderr: if errors.is_empty() {
            String::new()
        } else {
            errors.join("\n")
        },
        success,
        route: "retrieval".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

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
                content,
                tokenize='trigram'
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

        // 2. 按 outcome="failed" 过滤（大小写不敏感归一）
        let failed_hits =
            search_history(&conn, "session-test", "src", Some("FAILED"), None, 10).unwrap();
        assert_eq!(failed_hits.len(), 1);
        assert_eq!(failed_hits[0].id_or_path, "out-001");

        // 3. 搜索成功记录
        let succ_hits =
            search_history(&conn, "session-test", "pool", Some("succeeded"), None, 10).unwrap();
        assert_eq!(succ_hits.len(), 1);
        assert_eq!(succ_hits[0].id_or_path, "out-002");

        // 4. since_step 步骤下界过滤
        let later_hits = search_history(&conn, "session-test", "db", None, Some(2), 10).unwrap();
        assert!(later_hits.iter().all(|h| h.step_id.unwrap_or(0) >= 2));
    }

    /// 回归测试：trigram 分词器下中文子串可命中（unicode61 时代此用例召回率为 0）
    #[test]
    fn test_chinese_substring_search_with_trigram() {
        let conn = setup_test_db();

        index_step_fts(
            &conn,
            "out-cn-001",
            "session-cn",
            1,
            "browser_click",
            "点击提交按钮",
            "failed",
            "错误: 无法连接到数据库服务器，请检查网络配置",
        )
        .unwrap();

        // 中文子串「服务器」「网络配置」在 unicode61 下均搜不到，trigram 下必须命中
        let hits = search_history(&conn, "session-cn", "服务器", None, None, 10).unwrap();
        assert_eq!(hits.len(), 1, "中文子串「服务器」必须可命中");
        assert_eq!(hits[0].id_or_path, "out-cn-001");
        assert!(hits[0].snippet.contains("服务器"));

        let hits2 = search_history(&conn, "session-cn", "网络配置", None, None, 10).unwrap();
        assert_eq!(hits2.len(), 1, "中文子串「网络配置」必须可命中");

        // 不足 3 字符的查询在 trigram 下无法命中，返回空（不报错）
        let short = search_history(&conn, "session-cn", "网络", None, None, 10).unwrap();
        assert!(short.is_empty());
    }

    /// 会话隔离：检索不得跨 session 泄漏
    #[test]
    fn test_session_isolation() {
        let conn = setup_test_db();
        for (out_id, session) in [("out-a", "session-a"), ("out-b", "session-b")] {
            index_step_fts(&conn, out_id, session, 1, "shell_exec", "cargo test", "failed",
                "linking error openssl not found").unwrap();
        }

        let hits = search_history(&conn, "session-a", "openssl", None, None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id_or_path, "out-a");

        // 空 session_id 显式表示跨会话检索
        let all = search_history(&conn, "", "openssl", None, None, 10).unwrap();
        assert_eq!(all.len(), 2);
    }

    /// 分发层：默认只查 history；短查询携带 notes 提示；code 检索缺 path 时显式报错
    #[test]
    fn test_dispatch_search_defaults_and_error_reporting() {
        let conn = setup_test_db();
        index_step_fts(
            &conn,
            "out-d-1",
            "session-dispatch",
            1,
            "shell_exec",
            "cargo build",
            "failed",
            "错误: 无法连接到数据库服务器",
        )
        .unwrap();
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));

        // 1. 默认 kinds=history，中文子串命中
        let result = dispatch_search(
            &db,
            "session-dispatch",
            &json!({"query": "数据库服务器"}),
        );
        assert!(result.success);
        let payload: Value = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(payload["total_hits"], 1);

        // 2. 短查询：命中 0 但带 notes 提示，不算失败
        let short = dispatch_search(&db, "session-dispatch", &json!({"query": "数据"}));
        assert!(short.success);
        let short_payload: Value = serde_json::from_str(&short.stdout).unwrap();
        assert_eq!(short_payload["total_hits"], 0);
        assert!(short_payload["notes"].is_array());

        // 3. code 检索缺 path：显式失败并说明原因（而非伪装成 0 命中）
        let code_missing = dispatch_search(
            &db,
            "session-dispatch",
            &json!({"query": "fn main", "kinds": ["code"]}),
        );
        assert!(!code_missing.success);
        assert!(code_missing.stderr.contains("path"));

        // 4. 字符串直传 query 的兼容分支
        let str_query = dispatch_search(&db, "session-dispatch", &json!("数据库服务器"));
        assert!(str_query.success);
        let str_payload: Value = serde_json::from_str(&str_query.stdout).unwrap();
        assert_eq!(str_payload["total_hits"], 1);
    }

    /// 迁移路径回归：旧 unicode61 表检测、重建与回填（存量数据库升级的必经路径）
    #[test]
    fn test_trigram_migration_rebuilds_old_unicode61_table() {
        let conn = Connection::open_in_memory().unwrap();
        // 模拟旧版 schema：主表 + 无 trigram 的 FTS 表
        conn.execute_batch(
            "CREATE TABLE agent_step_logs (
                output_id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                step_id INTEGER NOT NULL, raw_output TEXT NOT NULL, created_at TEXT NOT NULL);
             CREATE VIRTUAL TABLE agent_step_logs_fts USING fts5(
                output_id UNINDEXED, session_id UNINDEXED, step_id UNINDEXED,
                tool, action, outcome, content);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_step_logs VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["out-mig", "s-mig", 1i64, "错误: 无法连接到数据库服务器", "2026-08-17"],
        )
        .unwrap();

        // 迁移前：unicode61 下中文子串搜不到
        assert!(search_history(&conn, "s-mig", "服务器", None, None, 10)
            .unwrap()
            .is_empty());

        // 幂等性：第二次调用不再重建
        let rebuilt = ensure_trigram_fts(&conn).unwrap();
        assert_eq!(rebuilt, 1, "应从主表回填 1 行");
        assert_eq!(ensure_trigram_fts(&conn).unwrap(), 0, "重复迁移应为 0 行");

        // 迁移后：中文子串可命中
        let hits = search_history(&conn, "s-mig", "服务器", None, None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id_or_path, "out-mig");
    }
}
