    use super::*;
    use crate::agent::checkpoint::AgentCheckpoint;
    use crate::agent::context::SandwichContext;
    use rusqlite::Connection;
    use std::path::PathBuf;

    const SRC: &str = "11111111-1111-1111-1111-111111111111";
    const DST: &str = "22222222-2222-2222-2222-222222222222";
    const TRACE1: &str = "aaaaaaaa-0000-0000-0000-000000000001";
    const TRACE2: &str = "aaaaaaaa-0000-0000-0000-000000000002";
    const OUT1: &str = "bbbbbbbb-0000-0000-0000-000000000001";
    const ART1: &str = "cccccccc-0000-0000-0000-000000000001";
    const MSG_U1: &str = "dddddddd-0000-0000-0000-0000000000u1";
    const MSG_A1: &str = "dddddddd-0000-0000-0000-0000000000a1";
    const MSG_U2: &str = "dddddddd-0000-0000-0000-0000000000u2";
    const MSG_A2: &str = "dddddddd-0000-0000-0000-0000000000a2";

    fn t(n: u64) -> String {
        format!("2026-01-01T00:00:{:02}Z", n)
    }

    fn test_conn() -> Connection {
        crate::db::init_db(&PathBuf::from(":memory:")).unwrap()
    }

    /// 种一个两回合会话：4 条消息 + 最新 checkpoint + 2 条回合历史快照
    /// + 2 条 run trace（各带 1 步）+ step_log + artifact + 资源/记录
    /// + 记忆链 + lesson + gate_fire + llm_call + child_run + 2 个 job。
    fn seed(conn: &Connection, src_tmp: &Path) {
        std::fs::create_dir_all(src_tmp).unwrap();
        std::fs::write(src_tmp.join("report.txt"), "artifact-body").unwrap();

        conn.execute(
            "INSERT INTO chat_sessions (id, title, model_id, created_at, updated_at, message_count)
             VALUES (?1, '原始会话', 'm1', ?2, ?2, 4)",
            params![SRC, t(0)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_projects (id, name, root_path, created_at, updated_at)
             VALUES ('p1', 'proj', '/tmp/proj', ?1, ?1)",
            params![t(0)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_project_sessions (session_id, project_id, created_at) VALUES (?1, 'p1', ?2)",
            params![SRC, t(0)],
        )
        .unwrap();
        for (id, role, content, at) in [
            (MSG_U1, "user", "第一轮任务", t(1)),
            (MSG_A1, "assistant", "第一轮完成", t(2)),
            (MSG_U2, "user", "第二轮任务", t(3)),
            (MSG_A2, "assistant", "第二轮完成", t(4)),
        ] {
            conn.execute(
                "INSERT INTO chat_messages (id, session_id, role, content, attachments, created_at, model_id)
                 VALUES (?1, ?2, ?3, ?4, NULL, ?5, 'm1')",
                params![id, SRC, role, content, at],
            )
            .unwrap();
        }

        let ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let cp = AgentCheckpoint::new(SRC.to_string(), &ctx);
        let snap = serde_json::to_string(&cp).unwrap();
        conn.execute(
            "INSERT INTO agent_checkpoints (run_id, phase, snapshot_json, version, created_at, updated_at)
             VALUES (?1, 'Done', ?2, 5, ?3, ?3)",
            params![SRC, snap, t(4)],
        )
        .unwrap();
        for (anchor, at) in [(MSG_A1, t(2)), (MSG_A2, t(4))] {
            conn.execute(
                "INSERT INTO agent_checkpoint_history (session_id, anchor_message_id, phase, snapshot_json, created_at)
                 VALUES (?1, ?2, 'Done', ?3, ?4)",
                params![SRC, anchor, snap, at],
            )
            .unwrap();
        }

        for (tid, at) in [(TRACE1, t(1)), (TRACE2, t(3))] {
            conn.execute(
                "INSERT INTO agent_run_traces (id, session_id, goal, status, steps_json, completion_json, started_at, updated_at, completed_at)
                 VALUES (?1, ?2, 'g', 'succeeded', '[]', NULL, ?3, ?3, ?3)",
                params![tid, SRC, at],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO agent_run_steps (run_id, seq, step_json)
                 VALUES (?1, 1, ?2)",
                params![
                    tid,
                    format!("{{\"tool\":\"local_file.write\",\"output_id\":\"{}\"}}", OUT1)
                ],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO agent_step_logs (output_id, session_id, step_id, raw_output, created_at)
             VALUES (?1, ?2, 1, 'wrote tmp/report.txt', ?3)",
            params![OUT1, SRC, t(1)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_artifacts (id, session_id, run_id, display_name, format, mime_type, kind, canonical_path, size_bytes, sha256, origin, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'r.txt', 'txt', 'text/plain', 'doc', ?4, 13, NULL, 'managed_generated', 'ready', ?5, ?5)",
            params![
                ART1,
                SRC,
                TRACE1,
                src_tmp.join("report.txt").to_string_lossy().to_string(),
                t(2)
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_task_resources (resource_id, session_id, kind, label, schema_json, summary, status, row_count, cursor_json, created_at, updated_at)
             VALUES ('res-1', ?1, 'table', '订单', '{}', '', 'active', 1, NULL, ?2, ?2)",
            params![SRC, t(1)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_task_records (resource_id, record_key, data_json, created_at, updated_at)
             VALUES ('res-1', 'k1', '{\"v\":1}', ?1, ?1)",
            params![t(1)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_memory_nodes (id, parent_id, level, kind, session_id, plan_id, step_index, content, metadata_json, confidence_score, hit_count, created_at)
             VALUES ('node-root', NULL, 0, 'fact', ?1, NULL, NULL, 'root', '{}', 100, 0, ?2),
                    ('node-child', 'node-root', 1, 'fact', ?1, NULL, NULL, 'child', '{}', 100, 0, ?2)",
            params![SRC, t(1)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_lesson_recalls (id, session_id, step_id, recalled_lesson_ids, applied_lesson_ids, outcome, created_at)
             VALUES ('les-1', ?1, 1, '[]', '[]', 'pending', ?2)",
            params![SRC, t(1)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_gate_fires (run_id, session_id, step_id, gate, route, created_at)
             VALUES (?1, ?2, 1, 'g', 'r', ?3)",
            params![TRACE1, SRC, t(1)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_llm_calls (session_id, step_id, created_at, model_id) VALUES (?1, 1, ?2, 'm1')",
            params![SRC, t(1)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_child_runs (id, parent_run_id, parent_session_id, workspace_root, model_id, status, started_at)
             VALUES ('child-1', ?1, ?2, '/ws', 'm1', 'succeeded', ?3)",
            params![TRACE1, SRC, t(1)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_background_jobs (id, parent_session_id, parent_run_id, job_type, title, params_json, status, started_at, consumed)
             VALUES ('job-live', ?1, ?2, 't', 'live', '{}', 'running', ?3, 0),
                    ('job-done', ?1, ?2, 't', 'done', '{}', 'succeeded', ?3, 1)",
            params![SRC, TRACE1, t(1)],
        )
        .unwrap();
    }

    fn count(conn: &Connection, sql: &str, sid: &str) -> i64 {
        conn.query_row(sql, params![sid], |r| r.get(0)).unwrap()
    }

    #[test]
    fn clone_copies_all_state_and_isolates_tmp() {
        let mut conn = test_conn();
        let dir = std::env::temp_dir().join(format!("fork-test-{}", Uuid::new_v4()));
        let src_tmp = dir.join("src");
        let dst_tmp = dir.join("dst");
        seed(&conn, &src_tmp);

        let report =
            fork_session_at(&mut conn, SRC, ForkPoint::Full, DST, &src_tmp, &dst_tmp).unwrap();

        assert_eq!(report.copied_messages, 4);
        assert_eq!(report.copied_traces, 2);
        assert_eq!(report.copied_steps, 2);
        assert_eq!(report.copied_artifacts, 1);
        assert!(report.checkpoint_copied);
        assert_eq!(report.skipped_live_jobs, 1);
        assert!(report.tmp_bytes_copied > 0);

        // 新会话行
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM chat_sessions WHERE id=?1", DST),
            1
        );
        // 消息 id 全部换新
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM chat_messages WHERE session_id=?1", DST),
            4
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM chat_messages WHERE session_id=?1 AND id IN (SELECT id FROM chat_messages WHERE session_id=?2)",
                params![DST, SRC],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
            0
        );
        // checkpoint：run_id=新会话、内容里旧 session id 已被重写
        let snap: String = conn
            .query_row(
                "SELECT snapshot_json FROM agent_checkpoints WHERE run_id=?1",
                params![DST],
                |r| r.get(0),
            )
            .unwrap();
        assert!(snap.contains(DST));
        assert!(!snap.contains(SRC));
        // trace/steps：id 重映射且引用一致
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM agent_run_traces WHERE session_id=?1", DST),
            2
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM agent_run_steps WHERE run_id IN (SELECT id FROM agent_run_traces WHERE session_id=?1)",
                DST
            ),
            2
        );
        // step_json 里的 output_id 已翻译成新 output_id
        let step_json: String = conn
            .query_row(
                "SELECT s.step_json FROM agent_run_steps s JOIN agent_run_traces t ON s.run_id=t.id WHERE t.session_id=?1 LIMIT 1",
                params![DST],
                |r| r.get(0),
            )
            .unwrap();
        let new_out: String = conn
            .query_row(
                "SELECT output_id FROM agent_step_logs WHERE session_id=?1",
                params![DST],
                |r| r.get(0),
            )
            .unwrap();
        assert!(step_json.contains(&new_out));
        assert!(!step_json.contains(OUT1));
        // artifact 路径指向新 tmp
        let cpath: String = conn
            .query_row(
                "SELECT canonical_path FROM agent_artifacts WHERE session_id=?1",
                params![DST],
                |r| r.get(0),
            )
            .unwrap();
        assert!(cpath.starts_with(dst_tmp.to_string_lossy().as_ref()));
        // tmp 物理复制且独立
        assert_eq!(
            std::fs::read_to_string(dst_tmp.join("report.txt")).unwrap(),
            "artifact-body"
        );
        // 资源记录/记忆/lesson/child/job 终态行
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM agent_task_resources WHERE session_id=?1", DST), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM agent_task_records WHERE resource_id IN (SELECT resource_id FROM agent_task_resources WHERE session_id=?1)", DST), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM agent_memory_nodes WHERE session_id=?1", DST), 2);
        // parent_id 链已翻译（不指向源节点 id）
        let child_parent: String = conn
            .query_row(
                "SELECT parent_id FROM agent_memory_nodes WHERE session_id=?1 AND level=1",
                params![DST],
                |r| r.get(0),
            )
            .unwrap();
        assert_ne!(child_parent, "node-root");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM agent_lesson_recalls WHERE session_id=?1", DST), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM agent_child_runs WHERE parent_session_id=?1", DST), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM agent_background_jobs WHERE parent_session_id=?1", DST), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM agent_llm_calls WHERE session_id=?1", DST), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM agent_checkpoint_history WHERE session_id=?1", DST), 2);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM agent_project_sessions WHERE session_id=?1", DST), 1);

        // 源会话删除级联不伤分叉
        crate::db::delete_session_completely(&conn, SRC, Some(&dir)).unwrap();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM chat_sessions WHERE id=?1", DST), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM chat_messages WHERE session_id=?1", DST), 4);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM agent_checkpoints WHERE run_id=?1", DST), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM agent_run_traces WHERE session_id=?1", DST), 2);
        assert!(dst_tmp.join("report.txt").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fork_at_message_uses_turn_boundary_state() {
        let mut conn = test_conn();
        let dir = std::env::temp_dir().join(format!("fork-test-{}", Uuid::new_v4()));
        let src_tmp = dir.join("src");
        let dst_tmp = dir.join("dst");
        seed(&conn, &src_tmp);

        let report = fork_session_at(&mut conn, SRC, ForkPoint::BeforeMessage(MSG_U2.to_string()), DST, &src_tmp, &dst_tmp).unwrap();

        assert_eq!(report.prompt_seed.as_deref(), Some("第二轮任务"));
        assert_eq!(report.copied_messages, 2); // u1 + a1
        assert_eq!(report.copied_traces, 1); // 只有第一轮
        assert!(report.checkpoint_copied); // 锚点=a1 的历史快照
        // 第二轮 trace / step / llm_call 不复制
        let trace_ids: Vec<String> = {
            let mut s = conn
                .prepare("SELECT id FROM agent_run_traces WHERE session_id=?1")
                .unwrap();
            s.query_map(params![DST], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(trace_ids.len(), 1);
        assert_ne!(trace_ids[0], TRACE1);
        // 历史快照只带锚点 a1 那条
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM agent_checkpoint_history WHERE session_id=?1", DST),
            1
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fork_before_timestamp_truncates_by_run_start_and_uses_passed_seed() {
        let mut conn = test_conn();
        let dir = std::env::temp_dir().join(format!("fork-test-{}", Uuid::new_v4()));
        let src_tmp = dir.join("src");
        let dst_tmp = dir.join("dst");
        seed(&conn, &src_tmp);

        // 第二轮 run.started_at = t(3)：时间戳分叉只保留第一轮世界状态，
        // prompt_seed 是前端透传的用户原话而非库内查到的消息。
        let t3_ms = chrono::DateTime::parse_from_rfc3339(&t(3))
            .unwrap()
            .timestamp_millis();
        let report = fork_session_at(
            &mut conn,
            SRC,
            ForkPoint::BeforeTimestamp {
                timestamp_ms: t3_ms,
                prompt_seed: Some("第二轮任务".to_string()),
            },
            DST,
            &src_tmp,
            &dst_tmp,
        )
        .unwrap();

        assert_eq!(report.prompt_seed.as_deref(), Some("第二轮任务"));
        assert_eq!(report.copied_messages, 2); // u1 + a1，当轮 u2 不入新会话
        assert_eq!(report.copied_traces, 1); // 第二轮 run 不复制
        assert!(report.checkpoint_copied); // 锚点=a1 的历史快照而非最新
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM agent_checkpoint_history WHERE session_id=?1", DST),
            1
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fork_at_completion_includes_turn_and_no_prompt_seed() {
        let mut conn = test_conn();
        let dir = std::env::temp_dir().join(format!("fork-test-{}", Uuid::new_v4()));
        let src_tmp = dir.join("src");
        let dst_tmp = dir.join("dst");
        seed(&conn, &src_tmp);

        let report = fork_session_at(&mut conn, SRC, ForkPoint::UpToMessage(MSG_A1.to_string()), DST, &src_tmp, &dst_tmp).unwrap();

        assert_eq!(report.prompt_seed, None);
        assert_eq!(report.copied_messages, 2); // u1 + a1
        assert_eq!(report.copied_traces, 1);
        assert!(report.checkpoint_copied); // 锚点=a1 的历史快照

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fork_before_any_turn_gets_no_checkpoint() {
        let mut conn = test_conn();
        let dir = std::env::temp_dir().join(format!("fork-test-{}", Uuid::new_v4()));
        let src_tmp = dir.join("src");
        let dst_tmp = dir.join("dst");
        seed(&conn, &src_tmp);

        let report = fork_session_at(&mut conn, SRC, ForkPoint::BeforeMessage(MSG_U1.to_string()), DST, &src_tmp, &dst_tmp).unwrap();
        assert_eq!(report.copied_messages, 0);
        assert!(!report.checkpoint_copied);
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM agent_checkpoints WHERE run_id=?1", DST),
            0
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fork_of_fork_is_just_another_clone() {
        let mut conn = test_conn();
        let dir = std::env::temp_dir().join(format!("fork-test-{}", Uuid::new_v4()));
        let src_tmp = dir.join("src");
        let mid_tmp = dir.join("mid");
        let dst_tmp = dir.join("dst");
        seed(&conn, &src_tmp);

        const MID: &str = "33333333-3333-3333-3333-333333333333";
        fork_session_at(&mut conn, SRC, ForkPoint::Full, MID, &src_tmp, &mid_tmp).unwrap();
        let r2 = fork_session_at(&mut conn, MID, ForkPoint::Full, DST, &mid_tmp, &dst_tmp).unwrap();
        assert_eq!(r2.copied_messages, 4);
        assert!(r2.checkpoint_copied);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM chat_messages WHERE session_id=?1", DST), 4);

        let _ = std::fs::remove_dir_all(&dir);
    }
