use super::*;
use crate::agent::checkpoint::{AgentCheckpoint, DurableAgentContext};
use crate::agent::context::types::PlanState;
use crate::agent::types::AgentInstruction;
use std::sync::{Arc, Mutex};

fn db() -> DbState {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE agent_step_logs(output_id TEXT PRIMARY KEY,session_id TEXT,step_id INTEGER,raw_output TEXT,created_at TEXT);
        CREATE VIRTUAL TABLE agent_step_logs_fts USING fts5(output_id UNINDEXED,session_id UNINDEXED,step_id UNINDEXED,tool,action,outcome,content,tokenize='trigram');
        CREATE TABLE agent_memory_nodes(id TEXT PRIMARY KEY,parent_id TEXT,level INTEGER,kind TEXT,session_id TEXT,plan_id TEXT,step_index INTEGER,content TEXT,metadata_json TEXT,confidence_score INTEGER,hit_count INTEGER,created_at TEXT);
        CREATE VIRTUAL TABLE agent_memory_nodes_fts USING fts5(node_id UNINDEXED,session_id UNINDEXED,level UNINDEXED,kind UNINDEXED,plan_id UNINDEXED,content,trigger_keywords,tokenize='trigram');
        CREATE TABLE agent_background_jobs(id TEXT PRIMARY KEY,parent_session_id TEXT,parent_run_id TEXT,job_type TEXT,title TEXT,params_json TEXT,status TEXT,started_at TEXT,finished_at TEXT,duration_ms INTEGER,result_json TEXT,error_kind TEXT,consumed INTEGER);").unwrap();
    DbState(Arc::new(Mutex::new(conn)))
}

fn context() -> SandwichContext {
    let mut ctx = SandwichContext::new("SYS".into(), "修复上传提示".into());
    ctx.resolve_plan(
        &serde_json::from_value(json!({"plan_id":"locate","plan_title":"定位错误丢失位置"}))
            .unwrap(),
    );
    ctx
}

fn save(db: &DbState, group: &WorkGroup, status: &str, raw: &str) -> String {
    crate::agent::run_trace::save_step_output_in_group(
        db,
        &group.session_id,
        group.step_id,
        "local_shell",
        "核验上传提示",
        status,
        raw,
        Some(group),
    )
    .unwrap()
}

#[test]
fn plan_identity_survives_renaming_switching_and_explicit_id_precedence() {
    let mut plan = PlanState::new();
    let a =
        plan.resolve_plan_explicit(Some("plan-7".into()), Some("定位问题".into()), true, "目标");
    assert_eq!(a.0, "plan-7");
    let renamed = plan.resolve_plan_explicit(None, Some("定位错误丢失位置".into()), false, "目标");
    assert_eq!(renamed.0, a.0);
    let b = plan.resolve_plan_explicit(None, Some("补齐提示".into()), true, "目标");
    assert_eq!(b.0, "plan-8", "自动编号不能撞到显式编号");
    let returned = plan.resolve_plan_explicit(Some(a.0), None, false, "目标");
    assert_eq!(returned.1, "定位错误丢失位置", "回到旧组不能借用当前组标题");
}

#[test]
fn same_task_checkpoint_keeps_identity_and_groups_new_task_gets_its_own() {
    let ctx = context();
    let cp = AgentCheckpoint::new("session".into(), &ctx);
    let encoded = serde_json::to_string(&cp).unwrap();
    let restored: AgentCheckpoint = serde_json::from_str(&encoded).unwrap();
    let mut next = SandwichContext::new("SYS".into(), "继续".into());
    restored.data.restore_into(&mut next);
    assert_eq!(ctx.history_task_id, next.history_task_id);
    assert_eq!(ctx.plan_state, next.plan_state);
    let mut new_task = SandwichContext::new("SYS".into(), "新的任务".into());
    let fresh_id = new_task.history_task_id.clone();
    restored.data.inherit_filling_into(&mut new_task);
    assert_eq!(new_task.history_task_id, fresh_id);
    assert_ne!(new_task.history_task_id, ctx.history_task_id);
    assert!(new_task.active_plan_id().is_none());
}

#[test]
fn legacy_checkpoint_does_not_invent_prior_group_ownership() {
    let ctx = context();
    let mut data = serde_json::to_value(DurableAgentContext::capture(&ctx)).unwrap();
    data.as_object_mut().unwrap().remove("history_task_id");
    data.as_object_mut().unwrap().remove("plan_state");
    let saved: DurableAgentContext = serde_json::from_value(data).unwrap();
    let mut next = context();
    saved.restore_into(&mut next);
    assert!(next.active_plan_id().is_none());
    assert!(!next.history_task_id.is_empty());
}

#[test]
fn group_directory_pages_exact_observations_and_checks_session_ownership() {
    let db = db();
    let ctx = context();
    let group = WorkGroup::capture("s", &ctx, 12).unwrap();
    let a = save(&db, &group, "failed", "命令未启动：参数被拒绝");
    let b = save(&db, &group, "succeeded", "进程退出码 0，真实上传尚未验证");
    let result = crate::agent::task_store::dispatch(
        &db,
        "s",
        "get_work_group",
        &json!({"group_id":group.node_id(),"limit":1}),
    );
    assert!(result.success, "{}", result.stderr);
    let page: Value = serde_json::from_str(&result.stdout).unwrap();
    assert_eq!(page["has_more"], true);
    assert_eq!(page["events"].as_array().unwrap().len(), 1);
    let conn = db.0.lock().unwrap();
    let second = get_group(
        &conn,
        "s",
        &json!({"group_id":group.node_id(),"offset":1,"limit":1}),
    )
    .unwrap();
    let ids = [
        page["events"][0]["output_id"].as_str().unwrap(),
        second["events"][0]["output_id"].as_str().unwrap(),
    ];
    assert!(ids.contains(&a.as_str()) && ids.contains(&b.as_str()));
    assert!(get_group(&conn, "other", &json!({"group_id":group.node_id()})).is_err());
    assert!(get_group(&conn, "s", &json!({"group_id":group.node_id(),"offset":-1})).is_err());
    drop(conn);
    assert_eq!(
        crate::agent::run_trace::get_step_output(&db, "s", &a).unwrap(),
        "命令未启动：参数被拒绝"
    );
}

#[test]
fn search_merges_same_output_and_preserves_directory_candidates() {
    let db = db();
    let ctx = context();
    let group = WorkGroup::capture("s", &ctx, 12).unwrap();
    let output = save(&db, &group, "failed", "定位错误丢失位置：missing symbol");
    let conn = db.0.lock().unwrap();
    let mut history =
        crate::agent::retrieval::search_history(&conn, "s", "定位错误丢失位置", None, None, 10)
            .unwrap();
    crate::agent::retrieval::memory_signpost::enrich_history_groups(&conn, &mut history);
    let memories = crate::agent::retrieval::search_memory_nodes_enriched(
        &conn,
        "s",
        "定位错误丢失位置",
        None,
        10,
    )
    .unwrap();
    let (hits, more) =
        crate::agent::retrieval::memory_signpost::merge_history_hits(history, memories, 10);
    assert!(!more);
    assert_eq!(hits.iter().filter(|h| h.id_or_path == output).count(), 1);
    assert!(hits.iter().any(|h| h.level == Some(1)));
    assert_eq!(
        hits.iter()
            .find(|h| h.id_or_path == output)
            .unwrap()
            .parent_id
            .as_deref(),
        Some(group.node_id().as_str())
    );
}

#[tokio::test]
async fn late_background_result_keeps_dispatch_group_and_is_not_relabelled_by_current_plan() {
    let db = db();
    let mut ctx = context();
    let origin = WorkGroup::capture("s", &ctx, 5).unwrap();
    let job = in_group(Some(origin.clone()), async {
        crate::agent::job_engine::begin_job(&db, "run-a", "s", "shell", "编译检查", "{}").unwrap()
    })
    .await;
    ctx.resolve_plan(
        &serde_json::from_value::<AgentInstruction>(
            json!({"plan_id":"other","plan_title":"处理另一个问题"}),
        )
        .unwrap(),
    );
    let current = WorkGroup::capture("s", &ctx, 6).unwrap();
    in_group(Some(current.clone()), async {
        crate::agent::job_engine::finish_job(&db, &job, "failed", 10, Some("exit_code: 1"), None)
            .unwrap();
        record_async_result(&db, "s", &job, "failed", "exit_code: 1");
    })
    .await;
    let conn = db.0.lock().unwrap();
    let page = get_group(&conn, "s", &json!({"group_id":origin.node_id()})).unwrap();
    assert_eq!(page["events"].as_array().unwrap().len(), 1);
    assert_eq!(page["events"][0]["outcome"], "failed");
    let step: i64 = conn
        .query_row("SELECT step_id FROM agent_step_logs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(step, 5);
    assert!(get_group(&conn, "s", &json!({"group_id":current.node_id()})).is_err());
}

#[tokio::test]
async fn blocking_dispatch_scope_is_isolated_and_unknown_results_stay_unassigned() {
    let db = db();
    let ctx = context();
    let group = WorkGroup::capture("s", &ctx, 3).unwrap();
    let db_clone = db.clone();
    let work = group.clone();
    let job = tokio::task::spawn_blocking(move || {
        in_blocking_group(Some(work), || {
            crate::agent::job_engine::begin_job(&db_clone, "run-a", "s", "shell", "编译", "{}")
                .unwrap()
        })
    })
    .await
    .unwrap();
    assert!(async_group(&db, "s", &job).is_some());
    assert!(async_group(&db, "foreign", &job).is_none());
    assert!(current_group().is_none());
    record_async_result(&db, "s", "legacy-job", "failed", "不可关联的旧回执");
    let conn = db.0.lock().unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM agent_step_logs", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn child_registration_and_resume_keep_original_group_even_after_parent_switches() {
    let db = db();
    db.0.lock().unwrap().execute_batch("CREATE TABLE agent_child_runs(id TEXT PRIMARY KEY,parent_run_id TEXT,parent_session_id TEXT,workspace_root TEXT,model_id TEXT,status TEXT,started_at TEXT,evidence_ref_count INTEGER,kind TEXT,workspace_copy TEXT,resumed_from TEXT,task_id TEXT,attempt_id INTEGER,purpose TEXT,batch_id TEXT,contract_id TEXT,contract_schema TEXT)").unwrap();
    let mut ctx = context();
    let original = WorkGroup::capture("s", &ctx, 7).unwrap();
    let child = in_group(Some(original.clone()), async {
        crate::agent::child_reviewer::begin_child_run(
            &db,
            "parent-a",
            "s",
            &std::env::temp_dir(),
            "fake",
            0,
            "worker",
            None,
        )
        .unwrap()
    })
    .await;
    ctx.resolve_plan(
        &serde_json::from_value::<AgentInstruction>(
            json!({"plan_id":"other","plan_title":"另一个工作块"}),
        )
        .unwrap(),
    );
    let current = WorkGroup::capture("s", &ctx, 8).unwrap();
    let identity = crate::agent::child_reviewer::load_run_identity(&db, &child).unwrap();
    let resumed = in_group(Some(current.clone()), async {
        crate::agent::child_reviewer::begin_child_run_extended(
            &db,
            "parent-b",
            "s",
            &std::env::temp_dir(),
            "fake",
            0,
            "worker",
            None,
            None,
            Some(&child),
            &identity,
        )
        .unwrap()
    })
    .await;
    assert_eq!(async_group(&db, "s", &resumed), Some(original.clone()));
    record_async_result(&db, "s", &resumed, "failed", "读取失败，尚未形成结论");
    let conn = db.0.lock().unwrap();
    let result = get_group(&conn, "s", &json!({"group_id":original.node_id()})).unwrap();
    assert_eq!(result["events"].as_array().unwrap().len(), 1);
    assert!(get_group(&conn, "s", &json!({"group_id":current.node_id()})).is_err());
}

#[tokio::test]
async fn late_result_does_not_revert_renamed_label_and_replay_is_atomic() {
    let db = db();
    let mut ctx = context();
    let original = WorkGroup::capture("s", &ctx, 9).unwrap();
    in_group(Some(original.clone()), async {
        bind_async(&db, "s", "async-one", None);
    })
    .await;
    ctx.resolve_plan(
        &serde_json::from_value::<AgentInstruction>(json!({"plan_title":"核验错误传递链"}))
            .unwrap(),
    );
    let renamed = WorkGroup::capture("s", &ctx, 10).unwrap();
    prepare_group(&db, Some(&renamed));
    let db_a = db.clone();
    let db_b = db.clone();
    let a = tokio::task::spawn_blocking(move || {
        record_async_result(&db_a, "s", "async-one", "failed", "original observation")
    });
    let b = tokio::task::spawn_blocking(move || {
        record_async_result(&db_b, "s", "async-one", "failed", "original observation")
    });
    a.await.unwrap();
    b.await.unwrap();
    let conn = db.0.lock().unwrap();
    let page = get_group(&conn, "s", &json!({"group_id":renamed.node_id()})).unwrap();
    assert!(page["label"].as_str().unwrap().contains("核验错误传递链"));
    assert_eq!(page["events"].as_array().unwrap().len(), 1);
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM agent_step_logs", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn directory_search_dispatch_and_missing_index_fail_open_preserve_raw_output() {
    let db = db();
    let ctx = context();
    let group = WorkGroup::capture("s", &ctx, 1).unwrap();
    for i in 0..4 {
        save(
            &db,
            &group,
            "failed",
            &format!("定位错误丢失位置 observation-{i}"),
        );
    }
    let result = crate::agent::retrieval::dispatch_search(
        &db,
        "s",
        &json!({"query":"定位错误丢失位置","kinds":["history"],"limit":2}),
        None,
    );
    assert!(result.success, "{}", result.stderr);
    let payload: Value = serde_json::from_str(&result.stdout).unwrap();
    assert_eq!(payload["hits"].as_array().unwrap().len(), 2);
    assert_eq!(payload["has_more"], true);
    assert!(payload["hits"]
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h["level"] == 1));
    db.0.lock()
        .unwrap()
        .execute_batch("DROP TABLE agent_memory_nodes_fts")
        .unwrap();
    let output = save(
        &db,
        &group,
        "failed",
        "raw output survives optional index failure",
    );
    assert_eq!(
        crate::agent::run_trace::get_step_output(&db, "s", &output).unwrap(),
        "raw output survives optional index failure"
    );
}

#[test]
fn replay_observed_fifty_steps_with_one_id_does_not_invent_semantic_boundaries() {
    // 01608301 的只读轨迹采样：50 步均属 plan-1，但标题经历多次变化。
    // 这里只回放这一结构事实，不把改名机械解释为新工作组或完成。
    let db = db();
    let mut ctx = SandwichContext::new("SYS".into(), "审计任务".into());
    for i in 0..50 {
        let title = match i {
            0..=15 => "任务初始标签",
            16..=30 => "定位审计日志拦截与写入实现",
            _ => "审计日志报文隐藏字段设计与落地",
        };
        ctx.resolve_plan(
            &serde_json::from_value::<AgentInstruction>(
                json!({"plan_id":"plan-1","plan_title":title}),
            )
            .unwrap(),
        );
        let group = WorkGroup::capture("s", &ctx, i).unwrap();
        prepare_group(&db, Some(&group));
        save(
            &db,
            &group,
            "succeeded",
            "synthetic observation; business result unverified",
        );
    }
    let conn = db.0.lock().unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM agent_memory_nodes WHERE level=1",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM agent_memory_nodes WHERE level=0",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        50
    );
}

#[test]
fn unnamed_initial_group_uses_current_task_not_frozen_prompt_anchor() {
    let mut ctx = SandwichContext::new("SYS".into(), "历史缓存锚点".into());
    ctx.current_goal = "当前新的工作目标".into();
    let (_, title) = ctx.resolve_plan(&AgentInstruction::default());
    assert_eq!(title, "当前新的工作目标");
}
