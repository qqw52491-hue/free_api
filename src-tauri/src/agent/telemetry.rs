use crate::agent::types::TokenUsage;
use crate::db::DbState;
use serde_json::Value;

/// 一次实际 LLM 请求尝试的纯观测记录（零行为参与）。
/// 记录点在 call_llm 内每个请求结果旁：成功、parse_error、request_error、
/// interrupted 各占一行——JSON 修复重发不再混入完整上下文的缓存基线。
/// 所有字段均来自调用点已有事实；不含 API key、base URL、prompt、正文、工具输出。
#[derive(Debug, Clone)]
pub struct LlmCallRecord<'a> {
    pub session_id: &'a str,
    pub step_id: i64,
    pub model_id: &'a str,
    pub model_tier: &'a str,
    pub reasoning_effort: &'a str,
    /// 请求形态：full_context（完整上下文）/ regeneration_retry（原位纠偏重发）。
    /// 不再用 prompt_tokens>0 猜测——修复请求同样有 prompt tokens。
    pub request_kind: &'a str,
    /// usage 来源：provider / estimated（供应商未回传时的估算）/ unavailable（失败）。
    pub usage_source: &'a str,
    /// 结果类别：instruction_ready / parse_error / request_error / interrupted。
    pub outcome: &'a str,
    /// call_llm 内部请求序号（第几次发请求，JSON 修复重发递增）。
    pub request_try: i64,
    /// 成功路径的实际 transport 尝试数（含 transient 重试）；失败路径 0 = 不可知。
    pub transport_attempt_count: i64,
    pub usage: &'a TokenUsage,
    pub history_message_count: i64,
    pub fetched_tools_count: i64,
    /// 稳定前缀版本（ContextFingerprint::capture 的 combined_prefix_hash 前 16 位）。
    /// 仅 full_context 首次请求计算；regeneration_retry / 失败行固定 NULL。
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
            ttft_ms, total_duration_ms,
            session_call_index, request_kind, usage_source, outcome,
            request_try, transport_attempt_count
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19,
            (SELECT COALESCE(COUNT(*), 0) + 1 FROM agent_llm_calls WHERE session_id = ?20),
            ?21, ?22, ?23, ?24, ?25
        )",
        rusqlite::params![
            record.session_id,
            record.step_id,
            chrono::Utc::now().to_rfc3339(),
            record.model_id,
            record.model_tier,
            record.reasoning_effort,
            record.request_kind,
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
            record.session_id,
            record.request_kind,
            record.usage_source,
            record.outcome,
            record.request_try,
            record.transport_attempt_count,
        ],
    );
    if let Err(e) = result {
        eprintln!("⚠️ [Telemetry] LLM 调用记录写入失败（不阻断主循环）: {}", e);
    }
}

/// 记录一次尾部状态卡的分段字节量（审计四问前置取证：先量后裁）。
/// 尽力写入、失败只打日志——观测不许影响被观测的循环。
pub fn record_card_sections(
    db: &DbState,
    session_id: &str,
    step_id: usize,
    card_level: &str,
    sections: &[(String, usize)],
) {
    if sections.is_empty() {
        return;
    }
    let sections_json = serde_json::Map::from_iter(
        sections
            .iter()
            .map(|(name, bytes)| (name.clone(), serde_json::Value::from(*bytes))),
    );
    let total: usize = sections.iter().map(|(_, b)| *b).sum();
    let conn = match db.0.lock() {
        Ok(conn) => conn,
        Err(_) => {
            eprintln!("⚠️ [Telemetry] DB 锁不可用，跳过本条卡片分段记录");
            return;
        }
    };
    let result = conn.execute(
        "INSERT INTO agent_card_sections (session_id, step_id, card_level, sections_json, total_bytes, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            session_id,
            step_id as i64,
            card_level,
            serde_json::Value::Object(sections_json).to_string(),
            total as i64,
            chrono::Utc::now().to_rfc3339(),
        ],
    );
    if let Err(e) = result {
        eprintln!("⚠️ [Telemetry] 卡片分段写入失败（不阻断主循环）: {}", e);
    }
}

/// 按会话读取逐调用记录（评测对照与基线分析用）。
pub fn list_llm_calls(db: &DbState, session_id: &str) -> Result<Vec<Value>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT step_id, model_id, model_tier, reasoning_effort, request_kind, request_try,
                    prompt_tokens, completion_tokens, cached_tokens, cache_hit_rate,
                    context_window, usage_percent, history_message_count,
                    fetched_tools_count, prefix_fingerprint, ttft_ms, total_duration_ms, created_at,
                    session_call_index, usage_source, outcome, transport_attempt_count
             FROM agent_llm_calls WHERE session_id = ?1 ORDER BY step_id ASC, id ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![session_id], |row| Ok(json_row(row)))
        .map_err(|e| e.to_string())?;
    let mut calls = Vec::new();
    for row in rows {
        calls.push(row.map_err(|e| e.to_string())?);
    }
    Ok(calls)
}

/// 一次门禁拦截触发的纯观测记录（零行为参与）。
/// 记录点在各门禁消费点（pre-write 两处、reviewer_retry_limit、finish_review），
/// 用于回答：哪些拦截机制真实触发过、频率如何——为门禁机制的保留/退役提供数据。
#[derive(Debug, Clone)]
pub struct GateFireRecord<'a> {
    pub run_id: &'a str,
    /// 会话 id：评测报告/面板按会话聚合时使用（run_id 是内部运行标识，二者不同）
    pub session_id: &'a str,
    pub step_id: i64,
    /// 门禁家族：pre_write / finish_review / finish_obligation_evidence / ...
    pub gate: &'a str,
    /// 拦截路由原文（如 plan_gatekeeper:escalation_required）
    pub route: &'a str,
}

/// 写入一条门禁触发记录。失败只打日志不阻断主循环：
/// 观测系统的故障不应影响被观测的循环（与 record_llm_call 同一容错纪律）。
pub fn record_gate_fire(db: &DbState, record: &GateFireRecord) {
    let conn = match db.0.lock() {
        Ok(conn) => conn,
        Err(_) => {
            eprintln!("⚠️ [Telemetry] DB 锁不可用，跳过本条门禁触发记录");
            return;
        }
    };
    if let Err(e) = conn.execute(
        "INSERT INTO agent_gate_fires (run_id, session_id, step_id, gate, route, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            record.run_id,
            record.session_id,
            record.step_id,
            record.gate,
            record.route,
            chrono::Utc::now().to_rfc3339()
        ],
    ) {
        eprintln!("⚠️ [Telemetry] 门禁触发记录写入失败（不影响主流程）: {e}");
    }
}

/// 汇总门禁触发次数（按 route 分组，触发次数降序）。
/// 供评测报告/管理面板读取，回答"哪个机制在真实工作"。
pub fn gate_fire_counts(db: &DbState) -> Vec<(String, i64)> {
    let conn = match db.0.lock() {
        Ok(conn) => conn,
        Err(_) => {
            eprintln!("⚠️ [Telemetry] DB 锁不可用，门禁触发统计返回空");
            return Vec::new();
        }
    };
    let mut stmt = match conn.prepare(
        "SELECT route, COUNT(*) AS fires FROM agent_gate_fires
         GROUP BY route ORDER BY fires DESC, route ASC",
    ) {
        Ok(stmt) => stmt,
        Err(e) => {
            eprintln!("⚠️ [Telemetry] 门禁触发统计查询失败: {e}");
            return Vec::new();
        }
    };
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    });
    match rows {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(e) => {
            eprintln!("⚠️ [Telemetry] 门禁触发统计读取失败: {e}");
            Vec::new()
        }
    }
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
        "request_kind": nullable(4),
        "request_try": row.get::<_, i64>(5).unwrap_or_default(),
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
        "session_call_index": row.get::<_, i64>(18).unwrap_or_default(),
        "usage_source": nullable(19),
        "outcome": nullable(20),
        "transport_attempt_count": row.get::<_, i64>(21).unwrap_or_default(),
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
                total_duration_ms INTEGER NOT NULL DEFAULT 0,
                session_call_index INTEGER NOT NULL DEFAULT 0,
                request_kind TEXT NOT NULL DEFAULT 'full_context',
                usage_source TEXT NOT NULL DEFAULT 'unavailable',
                outcome TEXT NOT NULL DEFAULT 'instruction_ready',
                request_try INTEGER NOT NULL DEFAULT 1,
                transport_attempt_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE agent_gate_fires (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                session_id TEXT NOT NULL DEFAULT '',
                step_id INTEGER NOT NULL,
                gate TEXT NOT NULL,
                route TEXT NOT NULL,
                created_at TEXT NOT NULL
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

    fn record(
        db: &DbState,
        session: &str,
        step: i64,
        kind: &str,
        source: &str,
        outcome: &str,
        try_no: i64,
        usage: &TokenUsage,
    ) {
        record_llm_call(
            db,
            &LlmCallRecord {
                session_id: session,
                step_id: step,
                model_id: "glm-test",
                model_tier: "primary",
                reasoning_effort: "high",
                request_kind: kind,
                usage_source: source,
                outcome,
                request_try: try_no,
                transport_attempt_count: if outcome == "instruction_ready" { 2 } else { 0 },
                usage,
                history_message_count: 14,
                fetched_tools_count: 4,
                prefix_fingerprint: if kind == "full_context" {
                    Some("abc123def456abc1")
                } else {
                    None
                },
            },
        );
    }

    #[test]
    fn gate_fires_roundtrip_and_aggregate_by_route() {
        let db = test_db();
        for (run_id, route) in [
            ("run-1", "plan_gatekeeper:escalation_required"),
            ("run-2", "plan_gatekeeper:escalation_required"),
            ("run-2", "finish_review:blocking"),
        ] {
            record_gate_fire(
                &db,
                &GateFireRecord {
                    run_id,
                    session_id: "sess-eval",
                    step_id: 7,
                    gate: if route.starts_with("finish_review") {
                        "finish_review"
                    } else {
                        "pre_write"
                    },
                    route,
                },
            );
        }

        let counts = gate_fire_counts(&db);
        assert_eq!(
            counts,
            vec![
                ("plan_gatekeeper:escalation_required".to_string(), 2),
                ("finish_review:blocking".to_string(), 1),
            ]
        );
    }

    #[test]
    fn record_and_list_roundtrip_preserves_all_fields() {
        let db = test_db();
        let usage = sample_usage();
        record(
            &db,
            "sess-a",
            3,
            "full_context",
            "provider",
            "instruction_ready",
            1,
            &usage,
        );

        let calls = list_llm_calls(&db, "sess-a").unwrap();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call["step_id"], serde_json::json!(3));
        assert_eq!(call["model_id"], serde_json::json!("glm-test"));
        assert_eq!(call["reasoning_effort"], serde_json::json!("high"));
        assert_eq!(call["request_kind"], serde_json::json!("full_context"));
        assert_eq!(call["usage_source"], serde_json::json!("provider"));
        assert_eq!(call["outcome"], serde_json::json!("instruction_ready"));
        assert_eq!(call["request_try"], serde_json::json!(1));
        assert_eq!(call["transport_attempt_count"], serde_json::json!(2));
        assert_eq!(call["prompt_tokens"], serde_json::json!(12_000));
        assert_eq!(call["cached_tokens"], serde_json::json!(9_000));
        assert_eq!(call["cache_hit_rate"], serde_json::json!(75.0));
        assert_eq!(call["history_message_count"], serde_json::json!(14));
        assert_eq!(call["fetched_tools_count"], serde_json::json!(4));
        assert_eq!(
            call["prefix_fingerprint"],
            serde_json::json!("abc123def456abc1")
        );
        assert_eq!(call["ttft_ms"], serde_json::json!(420));
        assert_eq!(call["total_duration_ms"], serde_json::json!(2_100));
    }

    #[test]
    fn json_repair_and_failure_rows_are_separate_records() {
        let db = test_db();
        let usage = sample_usage();
        // 完整上下文 parse 失败 → 修复重发成功：两行，不再只记最终成功快照
        record(
            &db,
            "sess-a",
            1,
            "full_context",
            "provider",
            "parse_error",
            1,
            &usage,
        );
        record(
            &db,
            "sess-a",
            1,
            "regeneration_retry",
            "provider",
            "instruction_ready",
            2,
            &usage,
        );
        // request_error 行：usage 全 0、来源 unavailable、指纹 NULL
        let zero = TokenUsage::default();
        record(
            &db,
            "sess-a",
            2,
            "full_context",
            "unavailable",
            "request_error",
            1,
            &zero,
        );

        let calls = list_llm_calls(&db, "sess-a").unwrap();
        assert_eq!(calls.len(), 3, "每次实际请求尝试各占一行");
        assert_eq!(calls[0]["outcome"], serde_json::json!("parse_error"));
        assert_eq!(
            calls[1]["request_kind"],
            serde_json::json!("regeneration_retry")
        );
        assert!(
            calls[1]["prefix_fingerprint"].is_null(),
            "修复请求不污染前缀基线"
        );
        assert_eq!(calls[2]["usage_source"], serde_json::json!("unavailable"));
        assert_eq!(calls[2]["prompt_tokens"], serde_json::json!(0));
        assert_eq!(calls[2]["transport_attempt_count"], serde_json::json!(0));
        // 会话内调用序号严格递增
        let idx: Vec<i64> = calls
            .iter()
            .filter_map(|c| c["session_call_index"].as_i64())
            .collect();
        assert_eq!(idx, vec![1, 2, 3]);
    }

    #[test]
    fn records_are_ordered_by_step_and_isolated_per_session() {
        let db = test_db();
        let usage = sample_usage();
        for step in [1i64, 2, 2] {
            record(
                &db,
                "sess-a",
                step,
                "full_context",
                "provider",
                "instruction_ready",
                1,
                &usage,
            );
        }
        record(
            &db,
            "sess-b",
            1,
            "full_context",
            "provider",
            "instruction_ready",
            1,
            &usage,
        );

        let a = list_llm_calls(&db, "sess-a").unwrap();
        assert_eq!(a.len(), 3, "会话隔离：sess-b 不串入");
        let steps: Vec<i64> = a.iter().filter_map(|c| c["step_id"].as_i64()).collect();
        assert_eq!(steps, vec![1, 2, 2], "按 step 升序");
        let b = list_llm_calls(&db, "sess-b").unwrap();
        assert_eq!(b.len(), 1);
        // session_call_index 各会话独立计数
        assert_eq!(b[0]["session_call_index"], serde_json::json!(1));
    }

    #[test]
    fn estimated_usage_source_is_marked_not_silent() {
        let db = test_db();
        let usage = TokenUsage::default();
        record(
            &db,
            "s",
            1,
            "full_context",
            "estimated",
            "instruction_ready",
            1,
            &usage,
        );
        let calls = list_llm_calls(&db, "s").unwrap();
        assert_eq!(calls[0]["usage_source"], serde_json::json!("estimated"));
    }

    #[test]
    fn old_schema_columns_still_writable_via_migration_defaults() {
        // 旧库（875cbf5 表结构）经滚动迁移补列后，旧行默认值可读、新行完整写入
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE agent_llm_calls (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                step_id INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                model_id TEXT, model_tier TEXT, reasoning_effort TEXT,
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
            );
            INSERT INTO agent_llm_calls (session_id, step_id, created_at, route, prompt_tokens)
            VALUES ('old', 1, '2026-08-26', 'full_context', 500);",
        )
        .unwrap();
        for alter in [
            "ALTER TABLE agent_llm_calls ADD COLUMN session_call_index INTEGER NOT NULL DEFAULT 0;",
            "ALTER TABLE agent_llm_calls ADD COLUMN request_kind TEXT NOT NULL DEFAULT 'full_context';",
            "ALTER TABLE agent_llm_calls ADD COLUMN usage_source TEXT NOT NULL DEFAULT 'unavailable';",
            "ALTER TABLE agent_llm_calls ADD COLUMN outcome TEXT NOT NULL DEFAULT 'instruction_ready';",
            "ALTER TABLE agent_llm_calls ADD COLUMN request_try INTEGER NOT NULL DEFAULT 1;",
            "ALTER TABLE agent_llm_calls ADD COLUMN transport_attempt_count INTEGER NOT NULL DEFAULT 0;",
        ] {
            conn.execute(alter, []).unwrap();
        }
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));
        let rows = list_llm_calls(&db, "old").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["outcome"], serde_json::json!("instruction_ready"));
        assert_eq!(rows[0]["usage_source"], serde_json::json!("unavailable"));
    }
}
