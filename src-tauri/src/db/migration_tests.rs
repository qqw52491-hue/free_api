    use super::*;

    #[test]
    fn orphan_running_traces_marked_interrupted_on_cleanup() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE agent_run_traces (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL, goal TEXT,
                status TEXT NOT NULL, steps_json TEXT NOT NULL DEFAULT '[]',
                completion_json TEXT, started_at TEXT NOT NULL,
                updated_at TEXT NOT NULL, completed_at TEXT);",
        )
        .unwrap();
        for (id, status) in [("t-running", "running"), ("t-done", "succeeded"), ("t-fail", "failed")] {
            conn.execute(
                "INSERT INTO agent_run_traces (id, session_id, goal, status, steps_json, started_at, updated_at)
                 VALUES (?1, 's1', 'g', ?2, '[]', '2026-08-30T00:00:00+00:00', '2026-08-30T00:00:00+00:00')",
                rusqlite::params![id, status],
            )
            .unwrap();
        }
        let n = mark_orphan_running_traces_interrupted(&conn).unwrap();
        assert_eq!(n, 1, "只有 running 状态应被清理");
        let (status, has_summary, has_completed): (String, i64, Option<String>) = conn
            .query_row(
                "SELECT status,
                        completion_json LIKE '%应用重启，任务已中断%',
                        completed_at
                 FROM agent_run_traces WHERE id='t-running'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "cancelled");
        assert_eq!(has_summary, 1, "应补写中断摘要");
        assert!(has_completed.is_some(), "应写 completed_at");
        // 终态记录不动
        let untouched: i64 = conn
            .query_row("SELECT count(*) FROM agent_run_traces WHERE id IN ('t-done','t-fail') AND status NOT IN ('succeeded','failed')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(untouched, 0);
    }

    #[test]
    fn uvx_bridge_rows_convert_to_native_http_and_others_untouched() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mcp_plugins (
                id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE,
                command TEXT NOT NULL, args TEXT NOT NULL DEFAULT '[]',
                env TEXT NOT NULL DEFAULT '{}', capability_card TEXT NOT NULL DEFAULT '',
                enabled INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL,
                url TEXT NOT NULL DEFAULT '', headers TEXT NOT NULL DEFAULT '{}');",
        )
        .unwrap();

        // 1) 典型 uvx 桥：应转为原生 HTTP
        conn.execute(
            "INSERT INTO mcp_plugins (id, name, command, args, env, created_at) VALUES ('1', 'remote', 'uvx', ?1, ?2, '2026-01-01')",
            rusqlite::params![
                r#"["mcp-stdio","https://mcp.example.com/mcp","--transport","streamable-http"]"#,
                r#"{"MCP_BEARER_TOKEN":"tok-123"}"#
            ],
        ).unwrap();
        // 2) 本地 stdio 插件：必须原样保留
        conn.execute(
            "INSERT INTO mcp_plugins (id, name, command, args, env, created_at) VALUES ('2', 'excel', 'uvx', ?1, '{}', '2026-01-01')",
            rusqlite::params![r#"["excel-mcp-server","stdio"]"#],
        ).unwrap();

        let migrated = migrate_uvx_bridge_plugins_to_native(&conn).unwrap();
        assert_eq!(migrated, 1);

        let (url, headers, command, args, env): (String, String, String, String, String) = conn
            .query_row(
                "SELECT url, headers, command, args, env FROM mcp_plugins WHERE name = 'remote'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(url, "https://mcp.example.com/mcp");
        assert!(headers.contains("Bearer tok-123"));
        assert_eq!(command, "");
        assert_eq!(args, "[]");
        assert_eq!(env, "{}");

        // excel 未被改动
        let excel_command: String = conn
            .query_row(
                "SELECT command FROM mcp_plugins WHERE name = 'excel'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(excel_command, "uvx");
    }

    #[test]
    fn test_delete_session_completely_and_orphan_cleanup() {
        let temp_dir = std::env::temp_dir().join(format!("test-cleanup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("test_cleanup.db");
        let conn = init_db(&db_path).unwrap();

        // 1. 插入一个完整会话及其关联数据
        let s_id = "session-100";
        conn.execute(
            "INSERT INTO chat_sessions (id, title, model_id, created_at, updated_at) VALUES (?1, 'Title', 'm1', '2026-01-01', '2026-01-01')",
            rusqlite::params![s_id],
        ).unwrap();
        conn.execute(
            "INSERT INTO chat_messages (id, session_id, role, content, created_at) VALUES ('msg-1', ?1, 'user', 'hi', '2026-01-01')",
            rusqlite::params![s_id],
        ).unwrap();
        conn.execute(
            "INSERT INTO agent_child_runs (id, parent_run_id, parent_session_id, workspace_root, model_id, status, started_at) VALUES ('child-1', 'r1', ?1, '/tmp', 'm1', 'done', '2026-01-01')",
            rusqlite::params![s_id],
        ).unwrap();
        conn.execute(
            "INSERT INTO agent_llm_calls (session_id, step_id, created_at) VALUES (?1, 1, '2026-01-01')",
            rusqlite::params![s_id],
        ).unwrap();
        conn.execute(
            "INSERT INTO agent_artifacts (id, session_id, display_name, format, kind, canonical_path, created_at, updated_at) VALUES ('art-1', ?1, 'f', 'txt', 'code', '/tmp/f.txt', '2026-01-01', '2026-01-01')",
            rusqlite::params![s_id],
        ).unwrap();
        conn.execute(
            "INSERT INTO agent_task_resources (resource_id, session_id, kind, label, created_at, updated_at) VALUES ('res-1', ?1, 'file', 'uri', '2026-01-01', '2026-01-01')",
            rusqlite::params![s_id],
        ).unwrap();
        conn.execute(
            "INSERT INTO agent_task_records (resource_id, record_key, data_json, created_at, updated_at) VALUES ('res-1', 'k1', '{}', '2026-01-01', '2026-01-01')",
            [],
        ).unwrap();

        // 2. 执行彻底删除（temp_dir 注入隔离：session tmp 删的是 temp_dir/tmp/<sid>）
        let session_tmp = temp_dir.join("tmp").join(s_id);
        std::fs::create_dir_all(&session_tmp).unwrap();
        std::fs::write(session_tmp.join("artifact.pptx"), b"x").unwrap();
        delete_session_completely(&conn, s_id, Some(&temp_dir)).unwrap();
        assert!(!session_tmp.exists());

        // 3. 验证所有关联表为 0
        let msg_count: i64 = conn.query_row("SELECT count(*) FROM chat_messages WHERE session_id=?1", [s_id], |r| r.get(0)).unwrap();
        assert_eq!(msg_count, 0);
        let child_count: i64 = conn.query_row("SELECT count(*) FROM agent_child_runs WHERE parent_session_id=?1", [s_id], |r| r.get(0)).unwrap();
        assert_eq!(child_count, 0);
        let llm_count: i64 = conn.query_row("SELECT count(*) FROM agent_llm_calls WHERE session_id=?1", [s_id], |r| r.get(0)).unwrap();
        assert_eq!(llm_count, 0);
        let art_count: i64 = conn.query_row("SELECT count(*) FROM agent_artifacts WHERE session_id=?1", [s_id], |r| r.get(0)).unwrap();
        assert_eq!(art_count, 0);
        let task_res_count: i64 = conn.query_row("SELECT count(*) FROM agent_task_resources WHERE session_id=?1", [s_id], |r| r.get(0)).unwrap();
        assert_eq!(task_res_count, 0);
        let task_rec_count: i64 = conn.query_row("SELECT count(*) FROM agent_task_records WHERE resource_id='res-1'", [], |r| r.get(0)).unwrap();
        assert_eq!(task_rec_count, 0);
        let session_count: i64 = conn.query_row("SELECT count(*) FROM chat_sessions WHERE id=?1", [s_id], |r| r.get(0)).unwrap();
        assert_eq!(session_count, 0);

        // 4. 验证孤儿清理：插入没有会话主记录的孤儿数据
        conn.execute(
            "INSERT INTO agent_child_runs (id, parent_run_id, parent_session_id, workspace_root, model_id, status, started_at) VALUES ('orphan-1', 'r1', 'ghost-session', '/tmp', 'm1', 'done', '2026-01-01')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO agent_llm_calls (session_id, step_id, created_at) VALUES ('ghost-session', 1, '2026-01-01')",
            [],
        ).unwrap();
        // tmp 清扫必须注入隔离根——测试 DB 为空时扫真实数据目录会把全部
        // 真实会话 tmp 当孤儿误删（曾真实发生：cargo test 清空用户产物）。
        let fake_tmp_root = temp_dir.join("tmp");
        std::fs::create_dir_all(fake_tmp_root.join("ghost-session")).unwrap();
        std::fs::create_dir_all(fake_tmp_root.join("alive-session")).unwrap();
        std::fs::write(fake_tmp_root.join("alive-session").join("a.pptx"), b"x").unwrap();
        conn.execute(
            "INSERT INTO chat_sessions (id, title, model_id, created_at, updated_at) VALUES ('alive-session', 'T', 'm1', '2026-01-01', '2026-01-01')",
            [],
        ).unwrap();
        let fake_outputs_root = temp_dir.join("outputs");
        std::fs::create_dir_all(fake_outputs_root.join("blobs")).unwrap();
        let cleaned = cleanup_orphaned_session_data_at(
            &conn,
            Some(fake_tmp_root.as_path()),
            Some(fake_outputs_root.as_path()),
        )
        .unwrap();
        assert!(cleaned >= 2);
        let orphan_child: i64 = conn.query_row("SELECT count(*) FROM agent_child_runs WHERE id='orphan-1'", [], |r| r.get(0)).unwrap();
        assert_eq!(orphan_child, 0);
        // 孤儿 tmp 目录被删、活会话目录保留——本次事故的两面各断一次
        assert!(!fake_tmp_root.join("ghost-session").exists());
        assert!(fake_tmp_root.join("alive-session").join("a.pptx").exists());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
