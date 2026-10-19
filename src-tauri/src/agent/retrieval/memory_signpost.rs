use crate::agent::retrieval::types::{MemoryNode, SearchHit};
use rusqlite::{params, Connection};

/// 插入三级记忆节点并同步写入 FTS5 trigram 索引
pub fn insert_memory_node(
    conn: &Connection,
    node: &MemoryNode,
    trigger_keywords: &str,
) -> Result<(), String> {
    // 节点与独立 FTS 表必须同时提交；同 ID 重写不能留下旧关键词。
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute(
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

    tx.execute("DELETE FROM agent_memory_nodes_fts WHERE node_id = ?1", [&node.id])
        .map_err(|e| format!("Memory node FTS5 delete error: {}", e))?;
    let content_capped: String = node.content.chars().take(4_000).collect();
    tx.execute(
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

    tx.commit().map_err(|e| format!("Memory node commit error: {}", e))
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
                snippet(agent_memory_nodes_fts, 5, '<b>', '</b>', '...', 25), rank, n.confidence_score,
                n.session_id, n.metadata_json
         FROM agent_memory_nodes_fts fts
         JOIN agent_memory_nodes n ON fts.node_id = n.id
         WHERE agent_memory_nodes_fts MATCH ?1
           AND fts.session_id = n.session_id
           AND fts.content = substr(n.content, 1, 4000)
           AND n.confidence_score >= 30",
    );

    let mut params_vec: Vec<rusqlite::types::Value> = vec![match_pattern.into()];

    if !session_id.is_empty() {
        sql.push_str(" AND n.session_id = ?2");
        params_vec.push(session_id.to_string().into());
    } else {
        // 全局域：排除评测会话产生的记忆节点，防止考题数据污染真实排查
        sql.push_str(" AND n.session_id NOT LIKE 'agent-evaluation-%'");
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
            let node_session: String = row.get(9)?;
            let metadata: String = row.get(10)?;
            Ok((
                id,
                parent_id,
                level as u8,
                kind,
                step_index.map(|s| s as usize),
                content,
                snippet,
                node_session,
                metadata,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut hits = Vec::new();
    for r in rows {
        if let Ok((id, parent_id, level, kind, step_index, content, snippet, node_session, metadata)) = r {
            let mut evidence = Vec::new();

            // 上下文补齐 (Context Enrichment)
            if level == 0 {
                if let Some(ref p_id) = parent_id {
                    if let Ok(parent_content) = conn.query_row(
                        "SELECT content FROM agent_memory_nodes WHERE id = ?1 AND session_id = ?2",
                        params![p_id, node_session],
                        |row| row.get::<_, String>(0),
                    ) {
                        evidence.push(format!("所属工作目录/历史节点（不表示完成）: {}", parent_content));
                    }
                }
            } else if level == 1 {
                evidence.push(format!("工作目录；按需用 task_store.get_work_group(group_id={}) 分页查看观察", id));
            } else if level == 2 {
                if let Ok(mut child_stmt) = conn
                    .prepare("SELECT content FROM agent_memory_nodes WHERE parent_id = ?1 AND session_id = ?2 ORDER BY created_at, id LIMIT 2")
                {
                    if let Ok(child_rows) =
                        child_stmt.query_map(params![id, node_session], |row| row.get::<_, String>(0))
                    {
                        for c in child_rows.flatten() {
                            let short_c: String = c.chars().take(80).collect();
                            evidence.push(format!("关联证据: {}", short_c));
                        }
                    }
                }
            }

            // 预览不是原文；保留观察的精确回查引用。
            let mut observation_id = None;
            if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&metadata) {
                if let Some(output_id) = meta.get("output_id").and_then(|v| v.as_str()) {
                    evidence.push(format!("原始输出: output_id={}", output_id));
                    if level == 0 { observation_id = Some(output_id.to_string()); }
                }
            }

            hits.push(SearchHit {
                kind,
                id_or_path: observation_id.unwrap_or(id),
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

/// 同一观察可同时命中原文和 L0；先合并归属，再在两条检索分支间交错展示。
pub(crate) fn merge_history_hits(
    history: Vec<SearchHit>, memories: Vec<SearchHit>, limit: usize,
) -> (Vec<SearchHit>, bool) {
    let mut merged: Vec<SearchHit> = Vec::new();
    let mut positions = std::collections::HashMap::<String, usize>::new();
    let mut a = history.into_iter();
    let mut b = memories.into_iter();
    loop {
        let pair = [a.next(), b.next()];
        if pair.iter().all(Option::is_none) { break; }
        for hit in pair.into_iter().flatten() {
            if let Some(&pos) = positions.get(&hit.id_or_path) {
                let existing = &mut merged[pos];
                if existing.parent_id.is_none() { existing.parent_id = hit.parent_id; }
                for item in hit.evidence {
                    if !existing.evidence.contains(&item) { existing.evidence.push(item); }
                }
            } else {
                positions.insert(hit.id_or_path.clone(), merged.len());
                merged.push(hit);
            }
        }
    }
    let has_more = merged.len() > limit;
    merged.truncate(limit);
    (merged, has_more)
}

pub(crate) fn enrich_history_groups(conn: &Connection, hits: &mut [SearchHit]) {
    for hit in hits {
        // 新事件有精确输出身份；旧记录缺关联就保留未知，不按步骤号猜。
        if let Ok((parent, content)) = conn.query_row(
            "SELECT e.parent_id,p.content FROM agent_memory_nodes e JOIN agent_memory_nodes p
             ON e.parent_id=p.id AND e.session_id=p.session_id
             JOIN agent_step_logs l ON l.session_id=e.session_id AND l.output_id=json_extract(e.metadata_json,'$.output_id')
             WHERE l.output_id=?1 AND e.level=0 AND p.level=1",
            params![hit.id_or_path],
            |r| Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?))
        ) {
            hit.parent_id = Some(parent);
            hit.evidence.push(format!("所属工作目录（不表示完成）: {content}"));
        }
    }
}
