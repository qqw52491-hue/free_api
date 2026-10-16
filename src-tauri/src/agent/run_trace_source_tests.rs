use super::*;
use crate::agent::{context::SandwichContext, task_store, types::AgentInstruction};
use rusqlite::Connection;
use std::sync::{Arc, Mutex};

const AT: &str = "2026-10-16T06:20:00.123456Z";

fn db() -> DbState {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE agent_step_logs(output_id TEXT PRIMARY KEY,session_id TEXT,step_id INTEGER,raw_output TEXT,created_at TEXT);
         CREATE TABLE agent_step_logs_fts(output_id TEXT,session_id TEXT,step_id INTEGER,tool TEXT,action TEXT,outcome TEXT,content TEXT);
         CREATE TABLE agent_run_traces(id TEXT PRIMARY KEY,session_id TEXT);
         CREATE TABLE agent_run_steps(run_id TEXT,seq INTEGER,step_json TEXT,PRIMARY KEY(run_id,seq));",
    ).unwrap();
    DbState(Arc::new(Mutex::new(conn)))
}

fn put(db: &DbState, id: &str, step: i64, run: &str, seq: i64) {
    let conn = db.0.lock().unwrap();
    conn.execute(
        "INSERT INTO agent_step_logs VALUES (?1,'s',?2,?3,?4)",
        params![
            id,
            step,
            "六个容器启动成功\nE11000: duplicate hash\n末尾原文",
            AT
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO agent_step_logs_fts VALUES (?1,'s',?2,'local_shell','exec','succeeded','')",
        params![id, step],
    )
    .unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO agent_run_traces VALUES (?1,'s')",
        [run],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO agent_run_steps VALUES (?1,?2,?3)",
        params![
            run,
            seq,
            json!({"id":step,"output_id":id,"tool":"local_shell","action":"exec"}).to_string()
        ],
    )
    .unwrap();
}

fn call(db: &DbState, action: &str, request: Value) -> Value {
    let result = task_store::dispatch(db, "s", action, &request);
    assert!(result.success, "{}", result.stderr);
    serde_json::from_str(&result.stdout).unwrap()
}

#[test]
fn coincident_times_use_exact_output_identity_and_run_scoped_sequence() {
    let db = db();
    put(&db, "z-earlier", 7, "run-a", 10);
    put(&db, "a-later", 8, "run-a", 11);
    put(&db, "same-step-other-run", 7, "run-b", 0);
    for (id, step, run, seq) in [
        ("z-earlier", 7, "run-a", 10),
        ("a-later", 8, "run-a", 11),
        ("same-step-other-run", 7, "run-b", 0),
    ] {
        let result = call(
            &db,
            "get_step_output",
            json!({"output_id":id,
            "source":{"run_id":"spoofed","created_at":"2099-01-01T00:00:00Z"}}),
        );
        let source = &result["source"];
        assert_eq!(source["output_id"], id);
        assert_eq!(source["created_at"], AT);
        assert_eq!(source["step_id"], step);
        assert_eq!(source["run_id"], run);
        assert_eq!(source["run_seq"], seq);
        assert_eq!(source["run_location"], "exact_output_id");
        assert_eq!(source["tool"], "local_shell");
        assert_eq!(source["action"], "exec");
        assert_eq!(
            source["relation"], "unknown",
            "普通入口没有可信任务起点，不猜归属"
        );
    }
}

#[test]
fn every_read_and_search_alias_returns_the_same_fixed_source_without_changing_text() {
    let db = db();
    put(&db, "proof", 7, "run-a", 10);
    let original = crate::agent::run_trace::get_step_output(&db, "s", "proof").unwrap();
    let first = call(&db, "get_step_output", json!({"output_id":"proof"}));
    assert_eq!(first["raw_output"], original);
    for action in [
        "get_step_output",
        "get_output",
        "search_step_output",
        "grep_step_output",
        "search_output",
    ] {
        let result = call(
            &db,
            action,
            json!({"output_id":"proof","pattern":"E11000","offset":2,"limit":5}),
        );
        assert_eq!(result["source"], first["source"]);
        if action.starts_with("get") {
            assert_eq!(
                result["raw_output"],
                original.chars().skip(2).take(5).collect::<String>()
            );
        } else {
            assert_eq!(result["total_matches"], 1);
            assert_eq!(result["matches"][0]["line_number"], 2);
        }
    }
    let empty = call(
        &db,
        "search_step_output",
        json!({"output_id":"proof","pattern":"local_shell"}),
    );
    assert_eq!(empty["total_matches"], 0, "来源标记不混入待检索的原文");
    assert_eq!(empty["source"], first["source"]);
    assert_eq!(
        crate::agent::run_trace::get_step_output(&db, "s", "proof").unwrap(),
        original
    );
    for action in [
        "get_step_output",
        "get_output",
        "search_step_output",
        "grep_step_output",
        "search_output",
    ] {
        assert!(
            !task_store::dispatch(
                &db,
                "other-session",
                action,
                &json!({"output_id":"proof","pattern":"E11000"})
            )
            .success
        );
    }
}

#[test]
fn legacy_output_with_no_auxiliary_tables_still_reads_and_searches_with_unknown_fields() {
    let db = db();
    {
        let conn = db.0.lock().unwrap();
        conn.execute_batch("DROP TABLE agent_step_logs_fts; DROP TABLE agent_run_steps; DROP TABLE agent_run_traces;").unwrap();
        conn.execute(
            "INSERT INTO agent_step_logs VALUES ('old','s',7,'旧记录原文','unknown-time')",
            [],
        )
        .unwrap();
    }
    let read = call(&db, "get_step_output", json!({"output_id":"old"}));
    assert_eq!(read["raw_output"], "旧记录原文");
    assert_eq!(read["source"]["created_at"], "unknown-time");
    for field in ["run_id", "run_seq", "tool", "action", "outcome"] {
        assert!(read["source"][field].is_null());
    }
    let search = call(
        &db,
        "search_step_output",
        json!({"output_id":"old","pattern":"原文"}),
    );
    assert_eq!(search["total_matches"], 1);
    assert_eq!(search["source"], read["source"]);
    let source = source_for_task(read["source"].clone(), Utc::now());
    assert_eq!(source["relation"], "unknown");
}

#[test]
fn ambiguous_run_link_is_disclosed_and_foreign_session_trace_cannot_supply_source() {
    let db = db();
    put(&db, "proof", 7, "run-a", 10);
    {
        let conn = db.0.lock().unwrap();
        conn.execute("UPDATE agent_run_traces SET session_id='other'", [])
            .unwrap();
    }
    let unknown = call(&db, "get_step_output", json!({"output_id":"proof"}));
    assert!(unknown["source"]["run_id"].is_null());
    {
        let conn = db.0.lock().unwrap();
        conn.execute("UPDATE agent_run_traces SET session_id='s'", [])
            .unwrap();
        conn.execute("INSERT INTO agent_run_traces VALUES ('run-b','s')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO agent_run_steps VALUES ('run-b',1,?1)",
            [json!({"id":7,"output_id":"proof"}).to_string()],
        )
        .unwrap();
    }
    let ambiguous = call(&db, "get_step_output", json!({"output_id":"proof"}));
    assert_eq!(ambiguous["source"]["run_location"], "ambiguous");
    assert!(ambiguous["source"]["run_id"].is_null());
    assert!(ambiguous["source"]["run_seq"].is_null());
}

#[test]
fn recalled_result_is_appended_once_and_later_steps_keep_its_prefix_unchanged() {
    let db = db();
    put(&db, "proof", 7, "run-a", 10);
    let result = task_store::dispatch(&db, "s", "get_step_output", &json!({"output_id":"proof"}));
    let mut ctx = SandwichContext::new("SYS".into(), "当前测试GPU".into());
    ctx.context_window_tokens = 1_000_000;
    let instruction = AgentInstruction {
        tool: Some("get_step_output".into()),
        command: Some(json!({"output_id":"proof"})),
        ..Default::default()
    };
    ctx.add_step(&instruction, &result.stdout, "recall-call-output");
    let history = ctx.turns_history.clone();
    let request = ctx.assemble_messages();
    let stable_prefix = &request[..2 + history.len()];
    ctx.add_step(&instruction, "下一个操作的结果", "next-call-output");
    assert_eq!(&ctx.turns_history[..history.len()], &history);
    assert_eq!(
        &ctx.assemble_messages()[..stable_prefix.len()],
        stable_prefix
    );
    assert!(history[1]
        .content
        .as_str()
        .unwrap()
        .contains("saved_tool_output"));
    assert!(history[1].content.as_str().unwrap().contains(AT));
}

#[test]
fn reviewer_relation_retains_source_and_states_that_time_is_only_a_boundary() {
    let db = db();
    put(&db, "proof", 7, "run-a", 10);
    let source = saved_output_source(&db, "s", "proof").unwrap();
    let at = DateTime::parse_from_rfc3339(AT)
        .unwrap()
        .with_timezone(&Utc);
    let previous = source_for_task(source.clone(), at + chrono::Duration::seconds(1));
    assert_eq!(previous["relation"], "historical_reference");
    assert_eq!(previous["run_id"], "run-a");
    assert_eq!(previous["run_seq"], 10);
    let same = source_for_task(source, at);
    assert_eq!(same["relation"], "current_task");
    assert!(same["relation_basis"]
        .as_str()
        .unwrap()
        .contains("not proof"));
}
