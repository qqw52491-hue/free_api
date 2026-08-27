use crate::agent::types::TokenUsage;
use crate::db::DbState;
use serde_json::Value;

/// 一次完整 planning 调用的纯观测记录（零行为参与）。
/// 所有字段均来自调用点已有事实；不含 API key、base URL、prompt、正文、工具输出。
#[derive(Debug, Clone)]
pub struct LlmCallRecord<'a> {
    pub session_id: &'a str,
    pub step_id: i64,
    pub model_id: &'a str,
    pub model_tier: &'a str,
    pub reasoning_effort: &'a str,
    /// 完整上下文请求（full_context）或未回传 usage 的调用（no_usage_reported）。
    /// 与主循环水位基线的既有约定一致：prompt_tokens > 0 视为完整请求。
    pub route: &'a str,
    /// 本次成功前经历的 JSON 修复重试次数（外层可见部分）。
    pub retry_index: i64,
    pub usage: &'a TokenUsage,
    pub history_message_count: i64,
    pub fetched_tools_count: i64,
    /// 稳定前缀版本（ContextFingerprint::capture 的 combined_prefix_hash 前 16 位）。
    pub prefix_fingerprint: Option<&'a str>,
}

/// 写入一条逐调用 telemetry。失败只打日志不阻断主循环：
/// 观测系统的故障不应影响被观测的循环（与 run_trace 同一容错纪律）。
pub fn record_llm_call(db: &DbState, record: &LlmCallRecord) {
    let conn = match db.0.lock() {
        Ok(conn) => conn,
        Err(_) => {
            eprintln!("⚠️ [Telemetry] DB 锁不可用，跳过本条 LLM 调用记录");
            return;
        }
    };
    let result = conn.execute(
        "INSERT INTO agent_llm_calls (
            session_id, step_id, created_at, model_id, model_tier, reasoning_effort,
            route, retry_index, prompt_tokens, completion_tokens, total_tokens,
            cached_tokens, cache_hit_rate, context_window, usage_percent,
            history_message_count, fetched_tools_count, prefix_fingerprint,
            ttft_ms, total_duration_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
        rusqlite::params![
            record.session_id,
            record.step_id,
            chrono::Utc::now().to_rfc3339(),
            record.model_id,
            record.model_tier,
            record.reasoning_effort,
            record.route,
            record.retry_index,
            record.usage.prompt_tokens,
            record.usage.completion_tokens,
            record.usage.total_tokens,
            record.usage.cached_tokens,
            record.usage.cache_hit_rate,
            record.usage.context_window,
            record.usage.usage_percent,
            record.history_message_count,
            record.fetched_tools_count,
            record.prefix_fingerprint,
            record.usage.ttft_ms as i64,
            record.usage.total_duration_ms as i64,
        ],
    );
    if let Err(e) = result {
        eprintln!("⚠️ [Telemetry] LLM 调用记录写入失败（不阻断主循环）: {}", e);
    }
}

/// 按会话读取逐调用记录（评测对照与基线分析用）。
pub fn list_llm_calls(db: &DbState, session_id: &str) -> Result<Vec<Value>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT step_id, model_id, model_tier, reasoning_effort, route, retry_index,
                    prompt_tokens, completion_tokens, cached_tokens, cache_hit_rate,
                    context_window, usage_percent, history_message_count,
                    fetched_tools_count, prefix_fingerprint, ttft_ms, total_duration_ms, created_at
             FROM agent_llm_calls WHERE session_id = ?1 ORDER BY step_id ASC, id ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![session_id], |row| {
            Ok(json_row(row))
        })
        .map_err(|e| e.to_string())?;
    let mut calls = Vec::new();
    for row in rows {
        calls.push(row.map_err(|e| e.to_string())?);
    }
    Ok(calls)
}

fn json_row(row: &rusqlite::Row<'_>) -> Value {
    let nullable = |idx: usize| -> Value {
        row.get::<_, Option<String>>(idx)
            .ok()
            .flatten()
            .map(Value::String)
            .unwrap_or(Value::Null)
    };
    serde_json::json!({
        "step_id": row.get::<_, i64>(0).unwrap_or_default(),
        "model_id": nullable(1),
        "model_tier": nullable(2),
        "reasoning_effort": nullable(3),
        "route": nullable(4),
        "retry_index": row.get::<_, i64>(5).unwrap_or_default(),
        "prompt_tokens": row.get::<_, i64>(6).unwrap_or_default(),
        "completion_tokens": row.get::<_, i64>(7).unwrap_or_default(),
        "cached_tokens": row.get::<_, i64>(8).unwrap_or_default(),
        "cache_hit_rate": row.get::<_, f64>(9).unwrap_or_default(),
        "context_window": row.get::<_, i64>(10).unwrap_or_default(),
        "usage_percent": row.get::<_, f64>(11).unwrap_or_default(),
        "history_message_count": row.get::<_, i64>(12).unwrap_or_default(),
        "fetched_tools_count": row.get::<_, i64>(13).unwrap_or_default(),
        "prefix_fingerprint": nullable(14),
        "ttft_ms": row.get::<_, i64>(15).unwrap_or_default(),
        "total_duration_ms": row.get::<_, i64>(16).unwrap_or_default(),
        "created_at": nullable(17),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> DbState {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE agent_llm_calls (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                step_id INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                model_id TEXT,
                model_tier TEXT,
                reasoning_effort TEXT,
                route TEXT NOT NULL DEFAULT 'full_context',
                retry_index INTEGER NOT NULL DEFAULT 0,
                prompt_tokens INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                cached_tokens INTEGER NOT NULL DEFAULT 0,
                cache_hit_rate REAL NOT NULL DEFAULT 0,
                context_window INTEGER NOT NULL DEFAULT 0,
                usage_percent REAL NOT NULL DEFAULT 0,
                history_message_count INTEGER NOT NULL DEFAULT 0,
                fetched_tools_count INTEGER NOT NULL DEFAULT 0,
                prefix_fingerprint TEXT,
                ttft_ms INTEGER NOT NULL DEFAULT 0,
                total_duration_ms INTEGER NOT NULL DEFAULT 0
            );",
        )
        .unwrap();
        DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)))
    }

    fn sample_usage() -> TokenUsage {
        TokenUsage {
            prompt_tokens: 12_000,
            completion_tokens: 300,
            total_tokens: 12_300,
            cached_tokens: 9_000,
            cache_hit_rate: 75.0,
            context_window: 131_072,
            usage_percent: 9.37,
            ttft_ms: 420,
            total_duration_ms: 2_100,
            tokens_per_second: 142.8,
        }
    }

    #[test]
    fn record_and_list_roundtrip_preserves_all_fields() {
        let db = test_db();
        let usage = sample_usage();
        record_llm_call(
            &db,
            &LlmCallRecord {
                session_id: "sess-a",
                step_id: 3,
                model_id: "glm-test",
                model_tier: "primary",
                reasoning_effort: "high",
                route: "full_context",
                retry_index: 0,
                usage: &usage,
                history_message_count: 14,
                fetched_tools_count: 4,
                prefix_fingerprint: Some("abc123def456abc1"),
            },
        );

        let calls = list_llm_calls(&db, "sess-a").unwrap();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call["step_id"], serde_json::json!(3));
        assert_eq!(call["model_id"], serde_json::json!("glm-test"));
        assert_eq!(call["reasoning_effort"], serde_json::json!("high"));
        assert_eq!(call["prompt_tokens"], serde_json::json!(12_000));
        assert_eq!(call["cached_tokens"], serde_json::json!(9_000));
        assert_eq!(call["cache_hit_rate"], serde_json::json!(75.0));
        assert_eq!(call["history_message_count"], serde_json::json!(14));
        assert_eq!(call["fetched_tools_count"], serde_json::json!(4));
        assert_eq!(call["prefix_fingerprint"], serde_json::json!("abc123def456abc1"));
        assert_eq!(call["ttft_ms"], serde_json::json!(420));
        assert_eq!(call["total_duration_ms"], serde_json::json!(2_100));
    }

    #[test]
    fn records_are_ordered_by_step_and_isolated_per_session() {
        let db = test_db();
        let usage = sample_usage();
        for (step, retry) in [(2i64, 1i64), (1, 0), (2, 0)] {
            record_llm_call(
                &db,
                &LlmCallRecord {
                    session_id: "sess-a",
                    step_id: step,
                    model_id: "m",
                    model_tier: "primary",
                    reasoning_effort: "high",
                    route: if retry > 0 { "full_context" } else { "full_context" },
                    retry_index: retry,
                    usage: &usage,
                    history_message_count: 5,
                    fetched_tools_count: 2,
                    prefix_fingerprint: None,
                },
            );
        }
        record_llm_call(
            &db,
            &LlmCallRecord {
                session_id: "sess-b",
                step_id: 1,
                model_id: "m",
                model_tier: "primary",
                reasoning_effort: "high",
                route: "full_context",
                retry_index: 0,
                usage: &usage,
                history_message_count: 1,
                fetched_tools_count: 0,
                prefix_fingerprint: None,
            },
        );

        let a = list_llm_calls(&db, "sess-a").unwrap();
        assert_eq!(a.len(), 3, "会话隔离：sess-b 不串入");
        let steps: Vec<i64> = a
            .iter()
            .filter_map(|c| c["step_id"].as_i64())
            .collect();
        assert_eq!(steps, vec![1, 2, 2], "按 step 升序");
        let b = list_llm_calls(&db, "sess-b").unwrap();
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn nullable_identifier_fields_roundtrip_as_null() {
        let db = test_db();
        let usage = TokenUsage::default();
        record_llm_call(
            &db,
            &LlmCallRecord {
                session_id: "s",
                step_id: 1,
                model_id: "",
                model_tier: "",
                reasoning_effort: "",
                route: "no_usage_reported",
                retry_index: 2,
                usage: &usage,
                history_message_count: 0,
                fetched_tools_count: 0,
                prefix_fingerprint: None,
            },
        );
        let calls = list_llm_calls(&db, "s").unwrap();
        // 空 usage 的隔离修复调用也如实记录（retry_index 保留经历的重试事实）
        assert_eq!(calls[0]["route"], serde_json::json!("no_usage_reported"));
        assert_eq!(calls[0]["retry_index"], serde_json::json!(2));
        assert_eq!(calls[0]["prompt_tokens"], serde_json::json!(0));
    }
}
