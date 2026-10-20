//! 通道层测试（原 channel.rs tests 原样迁入）。

use super::*;
use super::http::*;
use super::bridge::*;
use super::runtime::*;


#[cfg(test)]
mod tests {
    use super::*;
use super::http::*;
use super::bridge::*;
use super::runtime::*;
    use axum::http::HeaderValue;

    fn test_config() -> ChannelConfig {
        ChannelConfig {
            token: "test-token".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn authorized_accepts_only_matching_token() {
        let config = test_config();
        let mut headers = HeaderMap::new();
        assert!(!authorized(&headers, &config));
        headers.insert("x-channel-token", HeaderValue::from_static("wrong"));
        assert!(!authorized(&headers, &config));
        headers.insert("x-channel-token", HeaderValue::from_static("test-token"));
        assert!(authorized(&headers, &config));
    }

    #[test]
    fn run_store_upsert_patch_and_eviction() {
        let key = format!("test-run-{}", uuid::Uuid::new_v4());
        let record = RunRecord {
            run_id: key.clone(),
            session_id: "session-x".to_string(),
            goal_preview: "goal".to_string(),
            status: "running".to_string(),
            paused_reason: None,
            paused_form: None,
            last_progress: None,
            event_count: 0,
            started_at: "2026-01-01T00:00:00+00:00".to_string(),
            updated_at: "2026-01-01T00:00:00+00:00".to_string(),
            result: None,
            error: None,
            llm_retries: 3,
            report_every_actions: 15,
            phase_push: true,
            phase: None,
            state_detail: None,
            source: "channel".to_string(),
            caller_notifies: false,
            supplements_closed: false,
        };
        assert!(record.application_notifies_terminal());
        let mut external = record.clone();
        external.caller_notifies = true;
        for status in ["running", "waiting_human", "done", "failed", "cancelled"] {
            external.status = status.to_string();
            assert!(!external.application_notifies_terminal());
        }
        external.caller_notifies = false;
        assert!(!external.application_notifies_terminal()); // cancelled
        external.status = "failed".to_string();
        assert!(external.application_notifies_terminal());
        upsert_run(record);
        patch_run(&key, |r| {
            r.event_count = 5;
            r.last_progress = Some("进行中".to_string());
        });
        {
            let runs = runs_store().lock().unwrap();
            let stored = runs.get(&key).expect("record should exist");
            assert_eq!(stored.event_count, 5);
            assert_eq!(stored.last_progress.as_deref(), Some("进行中"));
        }
        // 淘汰路径：填满后最旧已完成记录应被挤出
        for i in 0..(MAX_RUN_RECORDS + 10) {
            upsert_run(RunRecord {
                run_id: format!("evict-{i}-{}", std::process::id()),
                session_id: "session-evict".to_string(),
                goal_preview: "old".to_string(),
                status: "done".to_string(),
                paused_reason: None,
                paused_form: None,
                last_progress: None,
                event_count: 0,
                started_at: "2025-01-01T00:00:00+00:00".to_string(),
                updated_at: "2025-01-01T00:00:00+00:00".to_string(),
                result: None,
                error: None,
                llm_retries: 3,
                report_every_actions: 15,
                phase_push: true,
                phase: None,
                state_detail: None,
                source: "channel".to_string(),
                caller_notifies: false,
                supplements_closed: false,
            });
        }
        let runs = runs_store().lock().unwrap();
        assert!(runs.len() <= MAX_RUN_RECORDS);
        drop(runs);
        runs_store().lock().unwrap().remove(&key);
    }

    #[test]
    fn scratch_dir_defaults_under_data_dir() {
        let default_dir = effective_scratch_dir_pub(&ChannelConfig::default());
        assert!(default_dir.ends_with("wechat-scratch"));
    }

    #[test]
    fn config_serde_roundtrip_keeps_optional_fields() {
        let raw = serde_json::to_string(&ChannelConfig::default()).unwrap();
        let parsed: ChannelConfig = serde_json::from_str(&raw).unwrap();
        assert!(parsed.llm_retries.is_none());
        assert!(parsed.allowlist.is_empty());
        assert!(parsed.auto_start_bridge);
        assert_eq!(parsed.action_milestone_steps, 15);
        assert!(parsed.phase_pushes);
        // 缺失字段/未知字段都应容忍
        let parsed2: ChannelConfig =
            serde_json::from_str(r#"{"token":"t","unknown_field":1}"#).unwrap();
        assert_eq!(parsed2.token, "t");
        assert_eq!(parsed2.port, DEFAULT_API_PORT);
    }

    // ------------------------------------------------ 网页控制台：会话列表与 trace

    fn web_test_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE chat_sessions (
                id TEXT PRIMARY KEY, title TEXT NOT NULL, model_id TEXT NOT NULL,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                message_count INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE agent_projects (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, root_path TEXT NOT NULL);
            CREATE TABLE agent_project_sessions (
                project_id TEXT NOT NULL, session_id TEXT NOT NULL, created_at TEXT NOT NULL);
            CREATE TABLE agent_run_traces (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL, goal TEXT, status TEXT NOT NULL,
                steps_json TEXT NOT NULL, completion_json TEXT,
                started_at TEXT NOT NULL, updated_at TEXT NOT NULL, completed_at TEXT);",
        )
        .unwrap();
        conn
    }

    fn insert_session(conn: &rusqlite::Connection, id: &str, updated_at: &str) {
        conn.execute(
            "INSERT INTO chat_sessions (id, title, model_id, created_at, updated_at, message_count)
             VALUES (?1, ?1, 'model-a', '2026-08-30T00:00:00+00:00', ?2, 0)",
            rusqlite::params![id, updated_at],
        )
        .unwrap();
    }

    fn insert_trace(conn: &rusqlite::Connection, id: &str, session_id: &str, status: &str, updated_at: &str) {
        conn.execute(
            "INSERT INTO agent_run_traces (id, session_id, goal, status, steps_json, started_at, updated_at)
             VALUES (?1, ?2, '目标', ?3, '[]', ?4, ?4)",
            rusqlite::params![id, session_id, status, updated_at],
        )
        .unwrap();
    }

    #[test]
    fn session_listing_filters_and_orders() {
        let conn = web_test_conn();
        conn.execute(
            "INSERT INTO agent_projects (id, name, root_path) VALUES ('p1', 'free_api', '/tmp/free_api')",
            [],
        )
        .unwrap();
        insert_session(&conn, "s-project", "2026-08-30T01:00:00+00:00");
        conn.execute(
            "INSERT INTO agent_project_sessions (project_id, session_id, created_at)
             VALUES ('p1', 's-project', '2026-08-30T01:00:00+00:00')",
            [],
        )
        .unwrap();
        insert_trace(&conn, "t-project", "s-project", "succeeded", "2026-08-30T01:05:00+00:00");
        insert_session(&conn, "s-plain-new", "2026-08-30T02:00:00+00:00");
        insert_trace(&conn, "t-plain", "s-plain-new", "failed", "2026-08-30T02:10:00+00:00");
        insert_session(&conn, "s-bare", "2026-08-30T00:30:00+00:00");

        // 全量：按最近活动排序（s-plain-new > s-project > s-bare）
        let all = list_channel_sessions(&conn, None, false).unwrap();
        let ids: Vec<&str> = all
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["s-plain-new", "s-project", "s-bare"]);
        let plain = all.iter().find(|s| s["id"] == "s-plain-new").unwrap();
        assert_eq!(plain["status"], "failed");
        assert_eq!(plain["trace_id"], "t-plain");
        assert!(plain["project_id"].is_null());
        // 无 trace 的会话也要返回（status 为 null）
        let bare = all.iter().find(|s| s["id"] == "s-bare").unwrap();
        assert!(bare["status"].is_null());

        // 项目过滤
        let project = list_channel_sessions(&conn, Some("p1"), false).unwrap();
        assert_eq!(project.len(), 1);
        assert_eq!(project[0]["id"], "s-project");
        assert_eq!(project[0]["project_name"], "free_api");

        // 临时任务过滤：只含未绑定项目的会话
        let scratch = list_channel_sessions(&conn, None, true).unwrap();
        let scratch_ids: Vec<&str> = scratch.iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert_eq!(scratch_ids, vec!["s-plain-new", "s-bare"]);
    }

    #[test]
    fn run_overlay_marks_active_sessions() {
        let conn = web_test_conn();
        insert_session(&conn, "ov-session", "2026-08-30T01:00:00+00:00");
        insert_trace(&conn, "ov-trace", "ov-session", "succeeded", "2026-08-30T01:05:00+00:00");
        let sessions = list_channel_sessions(&conn, None, false).unwrap();
        assert_eq!(sessions[0]["status"], "succeeded");

        let run_id = format!("ov-run-{}", uuid::Uuid::new_v4());
        upsert_run(RunRecord {
            run_id: run_id.clone(),
            session_id: "ov-session".to_string(),
            goal_preview: "goal".to_string(),
            status: "waiting_human".to_string(),
            paused_reason: Some("需要确认删除哪些文件？".to_string()),
            paused_form: None,
            last_progress: None,
            event_count: 0,
            started_at: "2026-08-30T02:00:00+00:00".to_string(),
            updated_at: "2026-08-30T02:00:00+00:00".to_string(),
            result: None,
            error: None,
            llm_retries: 3,
            report_every_actions: 15,
            phase_push: true,
            phase: None,
            state_detail: None,
                source: "channel".to_string(),
                caller_notifies: false,
                supplements_closed: false,
            });
        let overlaid = apply_run_overlay(sessions);
        assert_eq!(overlaid[0]["status"], "waiting_human");
        assert_eq!(overlaid[0]["paused_reason"], "需要确认删除哪些文件？");
        assert_eq!(overlaid[0]["run_id"], run_id);
        let _ = runs_store().lock().unwrap().remove(&run_id);
    }

    #[test]
    fn eval_id_sanitizer_blocks_traversal_and_odd_chars() {
        assert!(is_safe_eval_id(
            "agent-write-fixture-05a1fa98-1903-4a62-a661-f6d88782bc01"
        ));
        assert!(is_safe_eval_id("agent-real-task_1.2"));
        assert!(!is_safe_eval_id("")); // 空
        assert!(!is_safe_eval_id("..")); // 纯点点
        assert!(!is_safe_eval_id("../secret")); // 相对路径穿越
        assert!(!is_safe_eval_id("a/b")); // 路径分隔符
        assert!(!is_safe_eval_id("报告")); // 非 ASCII
        assert!(!is_safe_eval_id("a b")); // 空格
    }

    #[test]
    fn percent_decode_handles_ascii_utf8_and_invalid() {
        assert_eq!(percent_decode("plain.txt"), "plain.txt");
        assert_eq!(percent_decode("%E6%8A%A5%E5%91%8A.txt"), "报告.txt");
        assert_eq!(percent_decode("a%2Fb%20c"), "a/b c");
        // 非法序列原样保留
        assert_eq!(percent_decode("abc%zz"), "abc%zz");
        assert_eq!(percent_decode("trail%"), "trail%");
    }

    #[test]
    fn trace_steps_sanitizer_truncates_long_fields() {
        let long_output = "x".repeat(TRACE_FIELD_TRUNCATE + 500);
        let mut steps = json!([
            { "tool": "read_file", "output": long_output, "thought": "短思考" },
            { "tool": "run_command", "output": "ok" }
        ]);
        sanitize_trace_steps(&mut steps);
        let entries = steps.as_array().unwrap();
        let sanitized = entries[0]["output"].as_str().unwrap();
        assert!(sanitized.chars().count() <= TRACE_FIELD_TRUNCATE + 20);
        assert!(sanitized.ends_with("……（已截断，完整内容见桌面端）"));
        assert_eq!(entries[0]["thought"], "短思考");
        assert_eq!(entries[1]["output"], "ok");
    }
}

// handler_web_home 是 async 但无 IO，直接验证页面已内嵌且非空
#[cfg(test)]
mod web_page_tests {
    use super::*;
use super::http::*;
use super::bridge::*;
use super::runtime::*;
    #[test]
    fn embedded_page_present() {
        assert!(CHANNEL_WEB_PAGE.contains("远程控制"));
        assert!(CHANNEL_WEB_PAGE.len() > 5_000);
    }
}


#[test]
fn gate_profile_parsing_and_tier_mapping() {
    use super::{parse_gate_profile, profile_stage_review_enabled, profile_stage_review_max_files, GateProfile};
    assert_eq!(parse_gate_profile(None), GateProfile::L2);
    assert_eq!(parse_gate_profile(Some("l1")), GateProfile::L1);
    assert_eq!(parse_gate_profile(Some("L3")), GateProfile::L3);
    assert_eq!(parse_gate_profile(Some("junk")), GateProfile::L2);

    // L1：阶段审查强制关，阈值无意义
    assert!(!profile_stage_review_enabled(GateProfile::L1, None));
    assert!(!profile_stage_review_enabled(GateProfile::L1, Some(true)));
    // L2：配置缺省 = 开
    assert!(profile_stage_review_enabled(GateProfile::L2, None));
    // 配置显式关可覆盖（L2/L3 下仍尊重配置）
    assert!(!profile_stage_review_enabled(GateProfile::L2, Some(false)));

    // L3：每次写入必审
    assert_eq!(profile_stage_review_max_files(GateProfile::L3, None), 1);
    assert_eq!(profile_stage_review_max_files(GateProfile::L3, Some(6)), 1);
    // L1/L2：按配置或缺省 6
    assert_eq!(profile_stage_review_max_files(GateProfile::L2, None), 6);
    assert_eq!(profile_stage_review_max_files(GateProfile::L1, Some(3)), 3);
}
