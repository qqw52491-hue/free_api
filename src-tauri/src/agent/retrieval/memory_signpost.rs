use crate::agent::retrieval::types::{MemoryNode, SearchHit};
use rusqlite::{params, Connection};

/// 插入三级记忆节点并同步写入 FTS5 trigram 索引
pub fn insert_memory_node(
    conn: &Connection,
    node: &MemoryNode,
    trigger_keywords: &str,
) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO agent_memory_nodes (id, parent_id, level, kind, session_id, plan_id, step_index, content, metadata_json, confidence_score, hit_count, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            node.id,
            node.parent_id,
            node.level as i64,
            node.kind,
            node.session_id,
            node.plan_id,
            node.step_index.map(|s| s as i64),
            node.content,
            node.metadata_json,
            node.confidence_score,
            node.hit_count,
            node.created_at,
        ],
    ).map_err(|e| format!("Memory node insert error: {}", e))?;

    let content_capped: String = node.content.chars().take(4_000).collect();
    conn.execute(
        "INSERT INTO agent_memory_nodes_fts (node_id, session_id, level, kind, plan_id, content, trigger_keywords)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            node.id,
            node.session_id,
            node.level as i64,
            node.kind,
            node.plan_id,
            content_capped,
            trigger_keywords,
        ],
    ).map_err(|e| format!("Memory node FTS5 insert error: {}", e))?;

    Ok(())
}

/// 跨层折叠检索与上下文补齐 (Cross-Tier Memory Search with Context Enrichment)
pub fn search_memory_nodes_enriched(
    conn: &Connection,
    session_id: &str,
    query: &str,
    kind_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    let clean_query = query.trim();
    if clean_query.is_empty() {
        return Ok(Vec::new());
    }

    let escaped_query = clean_query.replace('"', "\"\"");
    let match_pattern = format!("\"{}\"", escaped_query);

    let mut sql = String::from(
        "SELECT n.id, n.parent_id, n.level, n.kind, n.step_index, n.content,
                snippet(agent_memory_nodes_fts, 5, '<b>', '</b>', '...', 25), rank, n.confidence_score
         FROM agent_memory_nodes_fts fts
         JOIN agent_memory_nodes n ON fts.node_id = n.id
         WHERE agent_memory_nodes_fts MATCH ?1
           AND n.confidence_score >= 30",
    );

    let mut params_vec: Vec<rusqlite::types::Value> = vec![match_pattern.into()];

    if !session_id.is_empty() {
        sql.push_str(" AND n.session_id = ?2");
        params_vec.push(session_id.to_string().into());
    }

    if let Some(kind) = kind_filter {
        if !kind.is_empty() && kind != "all" {
            let next_idx = params_vec.len() + 1;
            sql.push_str(&format!(" AND n.kind = ?{}", next_idx));
            params_vec.push(kind.to_string().into());
        }
    }

    let next_idx = params_vec.len() + 1;
    sql.push_str(&format!(
        " ORDER BY n.confidence_score DESC, rank ASC LIMIT ?{}",
        next_idx
    ));
    params_vec.push((limit as i64).into());

    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rusqlite_params = rusqlite::params_from_iter(params_vec);

    let rows = stmt
        .query_map(rusqlite_params, |row| {
            let id: String = row.get(0)?;
            let parent_id: Option<String> = row.get(1)?;
            let level: i64 = row.get(2)?;
            let kind: String = row.get(3)?;
            let step_index: Option<i64> = row.get(4)?;
            let content: String = row.get(5)?;
            let snippet: String = row.get(6)?;
            Ok((
                id,
                parent_id,
                level as u8,
                kind,
                step_index.map(|s| s as usize),
                content,
                snippet,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut hits = Vec::new();
    for r in rows {
        if let Ok((id, parent_id, level, kind, step_index, content, snippet)) = r {
            let mut evidence = Vec::new();

            // 上下文补齐 (Context Enrichment)
            if level == 0 {
                if let Some(ref p_id) = parent_id {
                    if let Ok(parent_content) = conn.query_row(
                        "SELECT content FROM agent_memory_nodes WHERE id = ?1",
                        params![p_id],
                        |row| row.get::<_, String>(0),
                    ) {
                        evidence.push(format!("所属阶段总结: {}", parent_content));
                    }
                }
            } else if level == 2 {
                if let Ok(mut child_stmt) = conn
                    .prepare("SELECT content FROM agent_memory_nodes WHERE parent_id = ?1 LIMIT 2")
                {
                    if let Ok(child_rows) =
                        child_stmt.query_map(params![id], |row| row.get::<_, String>(0))
                    {
                        for c in child_rows.flatten() {
                            let short_c: String = c.chars().take(80).collect();
                            evidence.push(format!("关联证据: {}", short_c));
                        }
                    }
                }
            }

            hits.push(SearchHit {
                kind,
                id_or_path: id,
                parent_id,
                level: Some(level),
                step_id: step_index,
                tool: None,
                action: None,
                outcome: None,
                snippet: if snippet.is_empty() {
                    content.chars().take(80).collect()
                } else {
                    snippet
                },
                evidence,
            });
        }
    }

    Ok(hits)
}
