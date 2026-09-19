use crate::agent::retrieval::types::SearchHit;
use rusqlite::{params, Connection};

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
        None => false,
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
                Err(_) => continue,
            },
            None => raw,
        };
        let _ = index_step_fts(
            conn,
            &output_id,
            &session_id,
            step_id as usize,
            "",
            "",
            "",
            &content,
        );
        indexed += 1;
    }

    println!(
        "✅ [FTS5 Migration] trigram 索引回填完成，共 {} 行",
        indexed
    );
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
    if clean_query.chars().count() < 3 {
        return search_history_short_like(
            conn,
            session_id,
            clean_query,
            outcome_filter,
            since_step,
            limit,
        );
    }

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
    } else {
        // 全局域：排除评测会话产生的历史，防止考题/演练数据污染真实排查
        sql.push_str(" AND session_id NOT LIKE 'agent-evaluation-%'");
    }

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
                parent_id: None,
                level: Some(0),
                step_id: Some(step_id as usize),
                tool: Some(tool),
                action: Some(action),
                outcome: Some(outcome),
                snippet,
                evidence: Vec::new(),
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

/// 针对 < 3 字符的短查询降级走 SQL LIKE
fn search_history_short_like(
    conn: &Connection,
    session_id: &str,
    clean_query: &str,
    outcome_filter: Option<&str>,
    since_step: Option<usize>,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    let like_pattern = format!("%{}%", clean_query);
    let mut sql = String::from(
        "SELECT output_id, step_id, tool, action, outcome, content
         FROM agent_step_logs_fts
         WHERE (content LIKE ?1 OR action LIKE ?1 OR tool LIKE ?1)",
    );

    let mut params_vec: Vec<rusqlite::types::Value> = vec![like_pattern.into()];

    if !session_id.is_empty() {
        let next_idx = params_vec.len() + 1;
        sql.push_str(&format!(" AND session_id = ?{}", next_idx));
        params_vec.push(session_id.to_string().into());
    } else {
        // 全局域：排除评测会话产生的历史
        sql.push_str(" AND session_id NOT LIKE 'agent-evaluation-%'");
    }

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
    sql.push_str(&format!(" ORDER BY step_id DESC LIMIT ?{}", next_idx));
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
            let content: String = row.get(5)?;
            let chars: Vec<char> = content.chars().collect();
            let query_chars: Vec<char> = clean_query.chars().collect();
            let snippet = if let Some(char_idx) = chars
                .windows(query_chars.len())
                .position(|w| w == query_chars.as_slice())
            {
                let start = char_idx.saturating_sub(15);
                let end = (char_idx + query_chars.len() + 25).min(chars.len());
                format!("...{}...", chars[start..end].iter().collect::<String>())
            } else {
                chars.iter().take(60).collect::<String>()
            };
            Ok(SearchHit {
                kind: "history".to_string(),
                id_or_path: output_id,
                parent_id: None,
                level: Some(0),
                step_id: Some(step_id as usize),
                tool: Some(tool),
                action: Some(action),
                outcome: Some(outcome),
                snippet,
                evidence: Vec::new(),
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
