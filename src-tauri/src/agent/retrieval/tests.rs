    use super::*;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    #[test]
    fn test_count_prior_same_path_searches_normalizes_and_filters() {
        let steps = vec![
            json!({"id": 1, "action": "search", "status": "succeeded", "params": {"path": "src/context", "query": "a"}}),
            json!({"id": 2, "action": "search", "status": "succeeded", "params": {"path": "src/context/", "query": "b"}}),
            json!({"id": 3, "action": "search", "status": "failed", "params": {"path": "src/context", "query": "c"}}),
            json!({"id": 4, "action": "search", "status": "succeeded", "params": {"path": "src/other", "query": "a"}}),
            json!({"id": 5, "action": "read", "status": "succeeded", "params": {"path": "src/context"}}),
            json!({"id": 6, "action": "search", "status": "succeeded", "params": {"kinds": ["history"], "query": "x"}}),
        ];
        // 末尾斜杠归一化、failed 与非 search 步骤不计、无 path 的历史检索不计
        assert_eq!(count_prior_same_path_searches(&steps, "src/context"), 2);
        assert_eq!(count_prior_same_path_searches(&steps, "src/context/"), 2);
        assert_eq!(count_prior_same_path_searches(&steps, "src/other"), 1);
        assert_eq!(count_prior_same_path_searches(&steps, ""), 0);
    }

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

        let result = dispatch_search(&db, "session-dispatch", &json!({"query": "数据库服务器"}), None);
        assert!(result.success);
        let payload: Value = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(payload["total_hits"], 1);

        let short = dispatch_search(&db, "session-dispatch", &json!({"query": "数据"}), None);
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
            None,
        );
        assert!(!code_missing.success);
        assert!(code_missing.stderr.contains("path"));

        let str_query = dispatch_search(&db, "session-dispatch", &json!("数据库服务器"), None);
        assert!(str_query.success);
        let str_payload: Value = serde_json::from_str(&str_query.stdout).unwrap();
        assert_eq!(str_payload["total_hits"], 1);

        // view 结构化视图：与模型向文本同事实、两路投影
        let v = str_query.view.as_ref().expect("检索应产出结构化 view");
        assert_eq!(v["kind"], "search");
        assert_eq!(v["total_hits"], 1);
        assert!(v["hits"].as_array().unwrap()[0]["path"].is_string());
    }

    /// 空 query 静默失败洞：query="" 过 serde 后 search_code 返回空命中，
    /// 模型会把「参数形状错」误读成「没搜到」。必须显式报错并指路正确工具。
    #[test]
    fn test_search_empty_query_reports_error_with_stray_param_hint() {
        let conn = setup_test_db();
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        // 文件读参数误塞进 search（实测轨迹形态）
        let res = dispatch_search(
            &db,
            "sess-emptyq",
            &json!({"path": "src/a.rs", "start_line": 1, "end_line": 50, "kinds": ["code"]}),
            None,
        );
        assert!(!res.success, "缺 query 必须显式失败");
        assert!(res.stderr.contains("query"), "错误应点名 query: {}", res.stderr);
        assert!(res.stderr.contains("start_line"), "应识别误塞的行窗参数");
        assert!(res.stderr.contains("local_file"), "应指路正确工具");

        // query 显式空串同样拦截
        let res2 = dispatch_search(
            &db,
            "sess-emptyq",
            &json!({"query": "  ", "kinds": ["history"]}),
            None,
        );
        assert!(!res2.success, "空白 query 必须显式失败");
    }

    /// 截断事实必须如实上报：命中超 limit 时 has_more=true + 附不全清单警告。
    /// （goal_anchor/run_binding 实测教训：limit=3 截断被 total_hits 冒充完整清单，
    /// 模型据此漏修残留站点。）
    #[test]
    fn test_search_truncation_reports_has_more() {
        let dir = std::env::temp_dir().join(format!("search-trunc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // 单文件 5 处命中 > 默认 limit=3
        std::fs::write(
            dir.join("many.rs"),
            "fn a(){ marker() }\nfn b(){ marker() }\nfn c(){ marker() }\nfn d(){ marker() }\nfn e(){ marker() }\n",
        )
        .unwrap();
        let conn = setup_test_db();
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let res = dispatch_search(
            &db,
            "sess-trunc",
            &json!({"query": "marker", "kinds": ["code"], "path": "."}),
            Some(&dir),
        );
        assert!(res.success, "code 检索应成功: {}", res.stderr);
        let payload: Value = serde_json::from_str(&res.stdout).unwrap();
        assert_eq!(payload["has_more"], true, "命中>limit 必须标 has_more");
        assert_eq!(payload["total_hits"], 3, "total_hits 为返回条数");
        let notes = payload["notes"].as_array().expect("截断须附提示");
        assert!(
            notes.iter().any(|n| n.as_str().unwrap_or("").contains("被截断")),
            "notes 应含截断警告: {}",
            res.stdout
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 行表索引路径的测试库：在 setup_test_db 基础上补 run_traces/run_steps/step_logs。
    fn setup_index_db() -> Connection {
        let conn = setup_test_db();
        conn.execute_batch(
            "CREATE TABLE agent_run_traces (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL, goal TEXT NOT NULL,
                status TEXT NOT NULL, steps_json TEXT NOT NULL DEFAULT '[]',
                completion_json TEXT, started_at TEXT NOT NULL,
                updated_at TEXT NOT NULL, completed_at TEXT);
             CREATE TABLE agent_run_steps (
                run_id TEXT NOT NULL, seq INTEGER NOT NULL, step_json TEXT NOT NULL,
                PRIMARY KEY (run_id, seq));
             CREATE TABLE agent_step_logs (
                output_id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                step_id INTEGER NOT NULL, raw_output TEXT NOT NULL, created_at TEXT NOT NULL);",
        )
        .unwrap();
        conn
    }

    fn insert_run_step(conn: &Connection, run_id: &str, seq: i64, step: &Value) {
        conn.execute(
            "INSERT INTO agent_run_steps (run_id, seq, step_json) VALUES (?1,?2,?3)",
            params![run_id, seq, serde_json::to_string(step).unwrap()],
        )
        .unwrap();
    }

    fn seed_trace(conn: &Connection, session_id: &str, run_id: &str) {
        conn.execute(
            "INSERT INTO agent_run_traces (id, session_id, goal, status, steps_json, started_at, updated_at)
             VALUES (?1,?2,'g','running','[]','2026-01-01','2026-01-01')",
            params![run_id, session_id],
        )
        .unwrap();
    }

    #[test]
    fn test_indexed_dedup_hit_and_incremental_sync() {
        let conn = setup_index_db();
        let session = "sess-idx-dedup";
        seed_trace(&conn, session, "run-i1");
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));

        // 第一次派发：行表无任何步 → 正常检索路径（索引同步成功但无命中）
        let params = json!({"query": "数据库连接池", "kinds": ["history"], "path": "src"});
        let first = dispatch_search(&db, session, &params, None);
        assert_eq!(first.route, "retrieval", "无历史步时应正常检索而非命中缓存");

        // 落一步"同参成功检索"行 → 第二次派发应经增量索引命中去重锚点
        {
            let conn = db.0.lock().unwrap();
            insert_run_step(
                &conn,
                "run-i1",
                0,
                &json!({
                    "id": 1, "action": "search", "status": "succeeded",
                    "params": params,
                    "output": "{\"query\":\"数据库连接池\",\"total_hits\":3,\"hits\":[]}",
                    "output_id": "out-dedup-1"
                }),
            );
        }
        let second = dispatch_search(&db, session, &params, None);
        assert_eq!(second.route, "retrieval-cache", "同参重搜应命中增量索引去重");
        assert!(second.stdout.contains("检索去重"));
        assert!(second.stdout.contains("out-dedup-1"), "锚点应携带 output_id 句柄");
    }

    #[test]
    fn test_indexed_dedup_invalidated_by_write_step() {
        let conn = setup_index_db();
        let session = "sess-idx-inval";
        seed_trace(&conn, session, "run-i2");
        let params = json!({"query": "配置项", "kinds": ["history"]});
        insert_run_step(
            &conn,
            "run-i2",
            0,
            &json!({
                "id": 1, "action": "search", "status": "succeeded",
                "params": params,
                "output": "{\"total_hits\":1}", "output_id": "o1"
            }),
        );
        // 写步使之前的缓存命中失效——与原全量扫描 INVALIDATING_ACTIONS 语义一致
        insert_run_step(
            &conn,
            "run-i2",
            1,
            &json!({"id": 2, "action": "edit", "status": "succeeded", "params": {}}),
        );
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let result = dispatch_search(&db, session, &params, None);
        assert_eq!(result.route, "retrieval", "写步之后的同参检索不得命中缓存");
    }

    /// 回归：最新 run 须按 started_at 判定，不能按 UUID 字典序。
    /// 实测事故：旧 run 的 UUID 字典序更大（'z' > 'a'），ORDER BY id DESC 把索引
    /// 绑死在已完成的旧 run 上，当前 run 的步永不进索引——同参去重与 signpost
    /// 整轮静默失效（单 run 内同参重搜 4 次无一命中缓存）。
    #[test]
    fn test_indexed_dedup_binds_to_latest_run_by_started_at() {
        let conn = setup_index_db();
        let session = "sess-idx-latest-run";
        // 旧 run：已完成，id 字典序更大；新 run：进行中，id 字典序更小
        conn.execute(
            "INSERT INTO agent_run_traces (id, session_id, goal, status, steps_json, started_at, updated_at)
             VALUES ('run-zzz-old', ?1, 'g', 'succeeded', '[]', '2026-01-01T00:00:00+00:00', '2026-01-01T00:00:00+00:00')",
            params![session],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_run_traces (id, session_id, goal, status, steps_json, started_at, updated_at)
             VALUES ('run-aaa-new', ?1, 'g', 'running', '[]', '2026-02-01T00:00:00+00:00', '2026-02-01T00:00:00+00:00')",
            params![session],
        )
        .unwrap();
        let params = json!({"query": "去重锚点", "kinds": ["history"]});
        // 同参检索步落在进行中的新 run 里：绑错到旧 run 时索引内无此步，去重必失
        insert_run_step(
            &conn,
            "run-aaa-new",
            0,
            &json!({
                "id": 1, "action": "search", "status": "succeeded",
                "params": params,
                "output": "{\"total_hits\":1}", "output_id": "o-latest"
            }),
        );
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let result = dispatch_search(&db, session, &params, None);
        assert_eq!(
            result.route, "retrieval-cache",
            "索引须绑定 started_at 最新的 run 而非字典序最大的 run，实际: {}",
            result.stdout
        );
        assert!(result.stdout.contains("o-latest"));
    }

    #[test]
    fn test_indexed_same_path_signpost_counts() {
        let conn = setup_index_db();
        let session = "sess-idx-signpost";
        seed_trace(&conn, session, "run-i3");
        for (seq, id, q) in [(0, 1, "甲"), (1, 2, "乙")] {
            insert_run_step(
                &conn,
                "run-i3",
                seq,
                &json!({
                    "id": id, "action": "search", "status": "succeeded",
                    "params": {"query": q, "kinds": ["history"], "path": "src/context"},
                    "output": "{\"total_hits\":1}"
                }),
            );
        }
        // 干扰项：失败检索与异路径检索不应计数
        insert_run_step(
            &conn,
            "run-i3",
            2,
            &json!({"id": 3, "action": "search", "status": "failed",
                    "params": {"query": "丙", "path": "src/context"}}),
        );
        insert_run_step(
            &conn,
            "run-i3",
            3,
            &json!({"id": 4, "action": "search", "status": "succeeded",
                    "params": {"query": "丁", "path": "src/other"},
                    "output": "{\"total_hits\":1}"}),
        );
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let result = dispatch_search(
            &db,
            session,
            &json!({"query": "戊", "kinds": ["history"], "path": "src/context"}),
            None,
        );
        assert!(
            !result.stdout.contains("检索 signpost"),
            "未到第 6 次时不应提前附计数建议，实际: {}",
            result.stdout
        );
    }

    #[test]
    fn test_indexed_empty_output_match_does_not_shadow_earlier_hit() {
        // 语义对齐 find_cached_search：空输出的同参匹配不覆盖更早的非空命中
        let conn = setup_index_db();
        let session = "sess-idx-emptyout";
        seed_trace(&conn, session, "run-i4");
        let params = json!({"query": "重试策略", "kinds": ["history"]});
        insert_run_step(
            &conn,
            "run-i4",
            0,
            &json!({"id": 7, "action": "search", "status": "succeeded",
                    "params": params, "output": "{\"total_hits\":2}", "output_id": "o7"}),
        );
        insert_run_step(
            &conn,
            "run-i4",
            1,
            &json!({"id": 8, "action": "search", "status": "succeeded",
                    "params": params, "output": ""}),
        );
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let result = dispatch_search(&db, session, &params, None);
        assert_eq!(result.route, "retrieval-cache");
        assert!(result.stdout.contains("第 7 步"), "应锚定更早的非空命中: {}", result.stdout);
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

    struct TempSearchDir(std::path::PathBuf);

    impl TempSearchDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "free-api-search-{}-{}",
                tag,
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempSearchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_code_search_accepts_file_path_and_reports_resolved_root() {
        let temp = TempSearchDir::new("file-path");
        let file = temp.0.join("sample_scope.rs");
        std::fs::write(&file, "pub fn observe_tool_result() {}\n").unwrap();

        let conn = setup_test_db();
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let result = dispatch_search(
            &db,
            "session-file-path",
            &json!({
                "query": "observe_tool_result",
                "kinds": ["code"],
                "path": file.to_str().unwrap(),
            }),
            None,
        );

        assert!(
            result.success,
            "文件路径应可作为 code 检索目标: {}",
            result.stderr
        );
        let payload: Value = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(payload["total_hits"], 1);
        let hit_path = payload["hits"][0]["id_or_path"].as_str().unwrap();
        assert!(
            hit_path.starts_with("sample_scope.rs:"),
            "单文件检索的命中应使用文件名，实际: {}",
            hit_path
        );
        let resolved = payload["resolved_root"].as_str().unwrap();
        assert!(
            Path::new(resolved).is_absolute(),
            "resolved_root 必须是绝对路径，实际: {}",
            resolved
        );
    }

    /// 单文件检索：命中自带 ±3 行上下文窗口（行号 + '>' 标记命中行），
    /// 避免模型拿换词探测当逐行翻页器重建同一段代码块。
    #[test]
    fn test_code_search_file_path_returns_context_window() {
        let temp = TempSearchDir::new("ctx-window");
        let file = temp.0.join("props.vue");
        std::fs::write(
            &file,
            "<template>\n  <Sidebar\n    ref=\"treeRef\"\n    :active=\"activeId\"\n    :models=\"allModels\"\n    @open=\"openSession\"\n  />\n</template>\n",
        )
        .unwrap();

        let conn = setup_test_db();
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let result = dispatch_search(
            &db,
            "session-ctx-window",
            &json!({
                "query": "treeRef",
                "kinds": ["code"],
                "path": file.to_str().unwrap(),
            }),
            None,
        );

        assert!(result.success, "{}", result.stderr);
        let payload: Value = serde_json::from_str(&result.stdout).unwrap();
        let snippet = payload["hits"][0]["snippet"].as_str().unwrap();
        // 命中行（第3行）带 '>' 标记，且窗口包含上下相邻行——一次检索即可看到属性块
        assert!(snippet.contains(">    3|     ref=\"treeRef\""), "命中行应有行号与 '>' 标记: {}", snippet);
        assert!(snippet.contains(":active=\"activeId\""), "窗口应含命中行之后的上下文: {}", snippet);
        assert!(snippet.contains("<Sidebar"), "窗口应含命中行之前的上下文: {}", snippet);
    }

    /// 目录级检索保持单行命中（职责是定位分布，不带上下文防膨胀）
    #[test]
    fn test_code_search_directory_hits_stay_single_line() {
        let temp = TempSearchDir::new("dir-no-ctx");
        std::fs::write(
            temp.0.join("a.rs"),
            "fn alpha() {}\nfn beta() { alpha(); }\nfn gamma() {}\n",
        )
        .unwrap();

        let conn = setup_test_db();
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let result = dispatch_search(
            &db,
            "session-dir-no-ctx",
            &json!({
                "query": "fn beta",
                "kinds": ["code"],
                "path": temp.0.to_str().unwrap(),
            }),
            None,
        );

        assert!(result.success, "{}", result.stderr);
        let payload: Value = serde_json::from_str(&result.stdout).unwrap();
        let snippet = payload["hits"][0]["snippet"].as_str().unwrap();
        assert_eq!(snippet.trim(), "fn beta() { alpha(); }", "目录级命中应保持单行: {}", snippet);
    }

    #[test]
    fn test_code_search_regex_parse_error_falls_back_to_literal_match() {
        let temp = TempSearchDir::new("literal");
        std::fs::write(
            temp.0.join("parser.rs"),
            "fn parse_repetition() {\n    let q = 1;\n}\n",
        )
        .unwrap();

        let conn = setup_test_db();
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        // 含未转义元字符的查询在正则模式下必然 exit=2；应降级为字面量命中而不是失败。
        let result = dispatch_search(
            &db,
            "session-literal",
            &json!({
                "query": "fn parse_repetition() {",
                "kinds": ["code"],
                "path": temp.0.to_str().unwrap(),
            }),
            None,
        );

        assert!(result.success, "正则解析失败应降级重试: {}", result.stderr);
        let payload: Value = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(payload["total_hits"], 1);
        let notes = payload["notes"].as_array().unwrap();
        assert!(
            notes
                .iter()
                .any(|note| note.as_str().unwrap().contains("字面量")),
            "降级必须以 note 告知，实际: {:?}",
            notes
        );
    }

    #[test]
    fn test_code_search_reports_resolved_root_for_relative_directory() {
        let temp = TempSearchDir::new("rel-root");
        std::fs::write(temp.0.join("leaf.rs"), "pub struct Marker;\n").unwrap();

        let conn = setup_test_db();
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        // 相对目录：resolved_root 应解析为进程 cwd 下的绝对路径并出现在结果里，
        // 供资源作用域契约把此类访问判为 inside 而不是 unknown。
        let original_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&temp.0).unwrap();
        let result = dispatch_search(
            &db,
            "session-rel-root",
            &json!({
                "query": "Marker",
                "kinds": ["code"],
                "path": ".",
            }),
            None,
        );
        std::env::set_current_dir(original_cwd).unwrap();

        assert!(result.success, "相对目录检索应成功: {}", result.stderr);
        let payload: Value = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(payload["total_hits"], 1);
        let resolved = payload["resolved_root"].as_str().unwrap();
        assert!(
            Path::new(resolved).is_absolute(),
            "相对路径必须投影出绝对 resolved_root，实际: {}",
            resolved
        );
    }

    fn seed_gate_file(temp_tag: &str, name: &str) -> (TempSearchDir, String) {
        let temp = TempSearchDir::new(temp_tag);
        let file = temp.0.join(name);
        std::fs::write(
            &file,
            "fn alpha_one() {}\nfn alpha_two() {}\nfn alpha_three() {}\nfn alpha_four() {}\nfn alpha_five() {}\n",
        )
        .unwrap();
        // 必须返回 canonical 路径：生产侧 resolved_root 一律 canonicalize 后
        // 持久化，硬闸判定键同径归一；macOS 上 temp_dir 经 /var→/private/var
        // 软链，若种子写未 canonical 的原始路径，闸键与步键对不上、硬闸静默
        // 失效（本文件三个 gate 用例曾因此在本机全挂）。
        let path = file
            .canonicalize()
            .unwrap_or_else(|_| file.clone())
            .to_str()
            .unwrap()
            .to_string();
        (temp, path)
    }

    fn insert_succeeded_file_search(
        conn: &Connection,
        run_id: &str,
        seq: i64,
        path: &str,
        query: &str,
    ) {
        insert_run_step(
            conn,
            run_id,
            seq,
            &json!({
                "id": seq + 1, "action": "search", "status": "succeeded",
                "params": {"query": query, "kinds": ["code"], "path": path},
                "output": "{\"total_hits\":1}",
                "view": {"kind": "search", "resolved_root": path}
            }),
        );
    }

    #[test]
    fn test_sixth_unread_file_search_is_advisory_and_keeps_results() {
        let (_temp, file_str) = seed_gate_file("advice-six", "gate_target.rs");
        let conn = setup_index_db();
        let session = "sess-advice-six";
        seed_trace(&conn, session, "run-advice-six");
        for seq in 0..5 {
            insert_succeeded_file_search(&conn, "run-advice-six", seq, &file_str, &format!("prior_{seq}"));
        }
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let result = dispatch_search(&db, session,
            &json!({"query":"alpha_one", "kinds":["code"], "path":file_str}), None);
        assert!(result.success, "建议不得拒绝搜索: {}", result.stderr);
        assert_eq!(result.route, "retrieval");
        let data: Value = serde_json::from_str(&result.stdout).unwrap();
        assert!(data["total_hits"].as_u64().unwrap() > 0, "仍须返回真实命中");
        assert!(result.stdout.contains("已执行 6 次非缓存检索"));
        assert!(result.stdout.contains("本次检索已执行"));
        assert!(!result.stdout.contains("检索通道已暂停"));
    }

    #[test]
    fn test_meaningful_file_read_releases_search_gate() {
        let (_temp, file_str) = seed_gate_file("gate-read", "gate_target.rs");
        let conn = setup_index_db();
        let session = "sess-gate-read";
        seed_trace(&conn, session, "run-g2");
        for (seq, query) in ["alpha_one", "alpha_two", "alpha_three"]
            .iter()
            .enumerate()
        {
            insert_succeeded_file_search(&conn, "run-g2", seq as i64, &file_str, query);
        }
        insert_run_step(
            &conn,
            "run-g2",
            3,
            &json!({
                "id": 4, "action": "read", "status": "succeeded",
                "params": {"path": file_str},
                "view": {"kind": "read", "resolved_path": file_str}
            }),
        );
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let result = dispatch_search(
            &db,
            session,
            &json!({"query": "alpha_four", "kinds": ["code"], "path": file_str}),
            None,
        );
        assert!(result.success, "有效读取后可继续搜索: {}", result.stderr);
        assert_eq!(result.route, "retrieval");
    }

    #[test]
    fn test_cached_search_hits_do_not_count_toward_gate() {
        let (_temp, file_str) = seed_gate_file("gate-cache", "gate_target.rs");
        let conn = setup_index_db();
        let session = "sess-gate-cache";
        seed_trace(&conn, session, "run-g3");
        insert_succeeded_file_search(&conn, "run-g3", 0, &file_str, "alpha_one");
        for (seq, query) in ["alpha_two", "alpha_three"].iter().enumerate() {
            insert_run_step(
                &conn,
                "run-g3",
                seq as i64 + 1,
                &json!({
                    "id": seq + 2, "action": "search", "status": "succeeded",
                    "params": {"query": query, "kinds": ["code"], "path": file_str},
                    "output": "🎯【检索去重】本会话第 1 步已执行完全同参的检索且期间无文件修改（未重新检索）。",
                    "view": {"kind": "search-cache"}
                }),
            );
        }
        insert_run_step(
            &conn,
            "run-g3",
            3,
            &json!({
                "id": 4, "action": "search", "status": "succeeded",
                "params": {"query": "alpha_four", "kinds": ["code"], "path": file_str},
                "output": "🎯【检索去重】本会话第 1 步已执行完全同参的检索且期间无文件修改（未重新检索）。"
            }),
        );
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let result = dispatch_search(
            &db,
            session,
            &json!({"query": "alpha_five", "kinds": ["code"], "path": file_str}),
            None,
        );
        assert!(result.success, "去重回执不应计入建议计数: {}", result.stderr);
        assert_eq!(result.route, "retrieval");
    }

    #[test]
    fn test_write_step_clears_unread_file_search_counts() {
        let (_temp, file_str) = seed_gate_file("gate-write", "gate_target.rs");
        let conn = setup_index_db();
        let session = "sess-gate-write";
        seed_trace(&conn, session, "run-g4");
        for (seq, query) in ["alpha_one", "alpha_two", "alpha_three"]
            .iter()
            .enumerate()
        {
            insert_succeeded_file_search(&conn, "run-g4", seq as i64, &file_str, query);
        }
        insert_run_step(
            &conn,
            "run-g4",
            3,
            &json!({
                "id": 4, "action": "write", "status": "succeeded",
                "params": {"path": file_str}
            }),
        );
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let result = dispatch_search(
            &db,
            session,
            &json!({"query": "alpha_four", "kinds": ["code"], "path": file_str}),
            None,
        );
        assert!(result.success, "写步后应可正常搜索: {}", result.stderr);
        assert_eq!(result.route, "retrieval");
    }

    #[test]
    fn test_search_gate_is_scoped_per_file_path() {
        let temp = TempSearchDir::new("gate-scope");
        let file_a = temp.0.join("file_a.rs");
        let file_b = temp.0.join("file_b.rs");
        std::fs::write(
            &file_a,
            "fn alpha_one() {}\nfn alpha_two() {}\nfn alpha_three() {}\n",
        )
        .unwrap();
        std::fs::write(&file_b, "fn alpha_four() {}\nfn alpha_five() {}\n").unwrap();
        let path_a = file_a.to_str().unwrap().to_string();
        let path_b = file_b.to_str().unwrap().to_string();

        let conn = setup_index_db();
        let session = "sess-gate-scope";
        seed_trace(&conn, session, "run-g5");
        for (seq, query) in ["alpha_one", "alpha_two", "alpha_three"]
            .iter()
            .enumerate()
        {
            insert_succeeded_file_search(&conn, "run-g5", seq as i64, &path_a, query);
        }
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let result = dispatch_search(
            &db,
            session,
            &json!({"query": "alpha_four", "kinds": ["code"], "path": path_b}),
            None,
        );
        assert!(result.success, "文件 B 的检索不应被文件 A 的计数拦截: {}", result.stderr);
        assert_eq!(result.route, "retrieval");
    }

    #[test]
    fn test_search_advisory_fallback_scans_steps_json_without_blocking() {
        let (_temp, file_str) = seed_gate_file("advice-fallback", "gate_target.rs");
        let conn = setup_index_db();
        let session = "sess-advice-fallback";
        let steps: Vec<Value> = (0..6).map(|i| json!({
            "id":i+1, "action":"search", "status":"succeeded",
            "params":{"query":format!("prior_{i}"),"kinds":["code"],"path":file_str},
            "output":"results", "view":{"kind":"search", "resolved_root":file_str}
        })).collect();
        conn.execute("INSERT INTO agent_run_traces (id, session_id, goal, status, steps_json, started_at, updated_at) VALUES ('run-advice-fallback',?1,'g','running',?2,'2026-01-01','2026-01-01')",
            params![session,serde_json::to_string(&steps).unwrap()]).unwrap();
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let result = dispatch_search(&db, session,
            &json!({"query":"alpha_one","kinds":["code"],"path":file_str}),None);
        assert!(result.success);
        assert_eq!(result.route,"retrieval");
        assert!(result.stdout.contains("已执行 7 次非缓存检索"));
    }

    #[test]
    fn test_search_cache_remains_available_above_advisory_threshold() {
        let (_temp, file_str) = seed_gate_file("advice-cache", "gate_target.rs");
        let conn = setup_index_db();
        let session = "sess-advice-cache";
        seed_trace(&conn, session, "run-advice-cache");
        for seq in 0..6 {
            insert_succeeded_file_search(&conn,"run-advice-cache",seq,&file_str,&format!("prior_{seq}"));
        }
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let result = dispatch_search(&db,session,
            &json!({"query":"prior_0","kinds":["code"],"path":file_str}),None);
        assert!(result.success, "建议不能挡住缓存: {}",result.stderr);
        assert!(result.stdout.contains("【检索去重】"));
    }

    #[test]
    fn test_meaningful_read_resets_gate_across_path_aliases() {
        // 回归：计数与复位都按 canonical 路径——检索用绝对路径、read 的
        // params 写成 "/abs/./file" 别名，view.resolved_path 仍应复位计数。
        let (_temp, file_str) = seed_gate_file("gate-alias", "gate_target.rs");
        let conn = setup_index_db();
        let session = "sess-gate-alias";
        seed_trace(&conn, session, "run-g8");
        for (seq, query) in ["alpha_one", "alpha_two", "alpha_three"]
            .iter()
            .enumerate()
        {
            insert_succeeded_file_search(&conn, "run-g8", seq as i64, &file_str, query);
        }
        // read 的 params.path 用别名拼写，view.resolved_path 是 canonical。
        let alias = file_str.replace("gate_target.rs", "./gate_target.rs");
        insert_run_step(
            &conn,
            "run-g8",
            3,
            &json!({
                "id": 4, "action": "read", "status": "succeeded",
                "params": {"path": alias},
                "view": {"kind": "read", "resolved_path": file_str}
            }),
        );
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let result = dispatch_search(
            &db,
            session,
            &json!({"query": "alpha_four", "kinds": ["code"], "path": file_str}),
            None,
        );
        assert!(
            result.success,
            "read 的 canonical resolved_path 应复位别名路径的计数: {}",
            result.stderr
        );
        assert_eq!(result.route, "retrieval");
    }
    #[test]
    fn test_default_local_file_read_resets_index_and_fallback_count() {
        let path = "/repo/AgentChatStream.vue";
        let mut steps: Vec<Value> = (0..6).map(|i| json!({
            "id":i,"action":"search","status":"succeeded",
            "params":{"path":path,"query":format!("q{i}")},
            "view":{"kind":"search","resolved_root":path}
        })).collect();
        steps.push(json!({"id":7,"tool":"local_file","action":"local_file","status":"succeeded",
            "params":{"path":path,"start_line":440,"end_line":560},
            "view":{"kind":"read","resolved_path":path}}));
        assert!(is_meaningful_file_read_step(steps.last().unwrap()));
        assert_eq!(count_unread_file_searches(&steps,path),0);
        let mut idx = SessionSearchIndex {
            run_id:"alias".into(),next_seq:0,searches:Vec::new(),last_write_seq:None,
            path_counts:Default::default(),unread_file_search_counts:Default::default(),
            last_accessed:std::time::Instant::now()
        };
        for (seq,step) in steps.iter().enumerate() { index_step(&mut idx,seq as i64,step); }
        assert!(!idx.unread_file_search_counts.contains_key(path));
        let mut failed = steps.last().unwrap().clone();
        failed["status"] = json!("failed");
        assert!(!is_meaningful_file_read_step(&failed));
        let mut listed = steps.last().unwrap().clone();
        listed["view"]["kind"] = json!("list");
        assert!(!is_meaningful_file_read_step(&listed));
    }

    #[test]
    fn test_fifth_search_has_no_count_advisory_and_later_searches_continue() {
        let (_temp,file_str) = seed_gate_file("advice-boundary","gate_target.rs");
        let conn = setup_index_db();let session = "sess-advice-boundary";
        seed_trace(&conn,session,"run-advice-boundary");
        for seq in 0..4 {insert_succeeded_file_search(&conn,"run-advice-boundary",seq,&file_str,&format!("prior_{seq}"));}
        let db = crate::db::DbState(Arc::new(Mutex::new(conn)));
        let params = json!({"query":"alpha_one","kinds":["code"],"path":file_str});
        let fifth = dispatch_search(&db,session,&params,None);
        assert!(fifth.success);assert!(!fifth.stdout.contains("读取建议："));
        {
            let conn = db.0.lock().unwrap();
            for seq in 4..9 {insert_succeeded_file_search(&conn,"run-advice-boundary",seq,&file_str,&format!("prior_{seq}"));}
        }
        let tenth = dispatch_search(&db,session,&params,None);
        assert!(tenth.success);assert!(tenth.stdout.contains("已执行 10 次非缓存检索"));
        {
            let conn = db.0.lock().unwrap();
            insert_run_step(&conn,"run-advice-boundary",9,&json!({"tool":"local_file","action":"local_file",
                "status":"succeeded","params":{"path":file_str},"view":{"kind":"read","resolved_path":file_str}}));
        }
        let after_read = dispatch_search(&db,session,&params,None);
        assert!(after_read.success);assert!(!after_read.stdout.contains("读取建议："));
    }
