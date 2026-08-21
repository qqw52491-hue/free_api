pub mod fts_engine;
pub mod memory_signpost;
pub mod types;
pub mod workspace_search;

pub use fts_engine::*;
pub use memory_signpost::*;
pub use types::*;
pub use workspace_search::*;

use crate::agent::types::DispatchResult;
use crate::db::DbState;
use serde_json::{json, Value};
use std::path::Path;

/// 统一联邦检索调度入口
pub fn dispatch_search(db: &DbState, session_id: &str, params_value: &Value) -> DispatchResult {
    let params: SearchParams = match serde_json::from_value(params_value.clone()) {
        Ok(p) => p,
        Err(e) => {
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

    let limit = params.limit.unwrap_or(3).min(10);
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
            let mut history_branch_hits = Vec::new();
            if let Ok(hits) = search_history(
                &conn,
                session_id,
                &params.query,
                params.outcome.as_deref(),
                params.since_step,
                limit,
            ) {
                history_branch_hits.extend(hits);
            }

            // 联动查询三级记忆节点库 (Level 0~2 知识、Episode 与 Lesson)
            if let Ok(mem_hits) =
                search_memory_nodes_enriched(&conn, session_id, &params.query, None, limit)
            {
                history_branch_hits.extend(mem_hits);
            }

            // 去重并限制返回条数
            let mut seen_ids = std::collections::HashSet::new();
            for hit in history_branch_hits {
                if seen_ids.insert(hit.id_or_path.clone()) {
                    all_hits.push(hit);
                    if all_hits.len() >= limit {
                        break;
                    }
                }
            }
            history_ok = true;
        } else {
            errors.push("history 检索失败: 数据库锁获取失败".to_string());
        }
    }

    let mut code_ok = !wants_code;
    if wants_code {
        match params
            .path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
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

            CREATE TABLE agent_memory_nodes (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                level INTEGER NOT NULL,
                kind TEXT NOT NULL,
                session_id TEXT NOT NULL,
                plan_id TEXT,
                step_index INTEGER,
                content TEXT NOT NULL,
                metadata_json TEXT NOT NULL DEFAULT '{}',
                confidence_score INTEGER NOT NULL DEFAULT 100,
                hit_count INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            );

            CREATE VIRTUAL TABLE agent_memory_nodes_fts USING fts5(
                node_id UNINDEXED,
                session_id UNINDEXED,
                level UNINDEXED,
                kind UNINDEXED,
                plan_id UNINDEXED,
                content,
                trigger_keywords,
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

        let hits = search_history(&conn, "session-cn", "服务器", None, None, 10).unwrap();
        assert_eq!(hits.len(), 1, "中文子串「服务器」必须可命中");
        assert_eq!(hits[0].id_or_path, "out-cn-001");
        assert!(hits[0].snippet.contains("服务器"));

        let hits2 = search_history(&conn, "session-cn", "网络配置", None, None, 10).unwrap();
        assert_eq!(hits2.len(), 1, "中文子串「网络配置」必须可命中");

        let short = search_history(&conn, "session-cn", "网络", None, None, 10).unwrap();
        assert_eq!(short.len(), 1, "2 字短中文「网络」应通过 LIKE 降级命中");
        assert_eq!(short[0].id_or_path, "out-cn-001");
    }

    #[test]
    fn test_session_isolation() {
        let conn = setup_test_db();
        for (out_id, session) in [("out-a", "session-a"), ("out-b", "session-b")] {
            index_step_fts(
                &conn,
                out_id,
                session,
                1,
                "shell_exec",
                "cargo test",
                "failed",
                "linking error openssl not found",
            )
            .unwrap();
        }

        let hits = search_history(&conn, "session-a", "openssl", None, None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id_or_path, "out-a");

        let all = search_history(&conn, "", "openssl", None, None, 10).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_shallow_three_tier_memory_and_context_enrichment() {
        let conn = setup_test_db();

        let summary_node = MemoryNode {
            id: "phase-1".to_string(),
            parent_id: None,
            level: 2,
            kind: "summary".to_string(),
            session_id: "sess-tier".to_string(),
            plan_id: Some("plan-1".to_string()),
            step_index: Some(1),
            content: "阶段总结: 完成 PostgreSQL 驱动替换并修复跨域问题".to_string(),
            metadata_json: "{}".to_string(),
            confidence_score: 100,
            hit_count: 0,
            created_at: "2026-08-17".to_string(),
        };
        insert_memory_node(&conn, &summary_node, "PostgreSQL, 跨域, 阶段总结").unwrap();

        let event_node = MemoryNode {
            id: "step-10".to_string(),
            parent_id: Some("phase-1".to_string()),
            level: 0,
            kind: "event".to_string(),
            session_id: "sess-tier".to_string(),
            plan_id: Some("plan-1".to_string()),
            step_index: Some(10),
            content: "执行 cargo build 失败: error linking with cc failed: -lpq not found"
                .to_string(),
            metadata_json: "{}".to_string(),
            confidence_score: 100,
            hit_count: 0,
            created_at: "2026-08-17".to_string(),
        };
        insert_memory_node(&conn, &event_node, "linking error, lpq, PostgreSQL").unwrap();

        let hits = search_memory_nodes_enriched(&conn, "sess-tier", "lpq", None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id_or_path, "step-10");
        assert!(
            !hits[0].evidence.is_empty(),
            "命中原始事件应自动带出父阶段总结"
        );
        assert!(hits[0].evidence[0].contains("PostgreSQL 驱动替换"));

        let summary_hits =
            search_memory_nodes_enriched(&conn, "sess-tier", "跨域问题", None, 10).unwrap();
        assert_eq!(summary_hits.len(), 1);
        assert_eq!(summary_hits[0].id_or_path, "phase-1");
        assert!(
            !summary_hits[0].evidence.is_empty(),
            "命中阶段总结应自动带出子证据"
        );
        assert!(summary_hits[0].evidence[0].contains("linking with cc failed"));
    }

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

        let result = dispatch_search(&db, "session-dispatch", &json!({"query": "数据库服务器"}));
        assert!(result.success);
        let payload: Value = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(payload["total_hits"], 1);

        let short = dispatch_search(&db, "session-dispatch", &json!({"query": "数据"}));
        assert!(short.success);
        let short_payload: Value = serde_json::from_str(&short.stdout).unwrap();
        assert_eq!(
            short_payload["total_hits"], 1,
            "2 字短中文「数据」应通过 LIKE 降级命中"
        );

        let code_missing = dispatch_search(
            &db,
            "session-dispatch",
            &json!({"query": "fn main", "kinds": ["code"]}),
        );
        assert!(!code_missing.success);
        assert!(code_missing.stderr.contains("path"));

        let str_query = dispatch_search(&db, "session-dispatch", &json!("数据库服务器"));
        assert!(str_query.success);
        let str_payload: Value = serde_json::from_str(&str_query.stdout).unwrap();
        assert_eq!(str_payload["total_hits"], 1);
    }

    #[test]
    fn test_trigram_migration_rebuilds_old_unicode61_table() {
        let conn = Connection::open_in_memory().unwrap();
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
            rusqlite::params![
                "out-mig",
                "s-mig",
                1i64,
                "错误: 无法连接到数据库服务器",
                "2026-08-17"
            ],
        )
        .unwrap();

        assert!(search_history(&conn, "s-mig", "服务器", None, None, 10)
            .unwrap()
            .is_empty());

        let rebuilt = ensure_trigram_fts(&conn).unwrap();
        assert_eq!(rebuilt, 1, "应从主表回填 1 行");
        assert_eq!(ensure_trigram_fts(&conn).unwrap(), 0, "重复迁移应为 0 行");

        let hits = search_history(&conn, "s-mig", "服务器", None, None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id_or_path, "out-mig");
    }
}
