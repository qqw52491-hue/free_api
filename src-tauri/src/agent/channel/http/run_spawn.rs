//! 运行启动核心：会话运行守卫 RAII 与 register_and_spawn_run 派发。

use super::*;

fn terminal_status(state: &str) -> Option<&'static str> {
    match state {
        "succeeded" | "done" => Some("done"),
        "failed" => Some("failed"),
        "cancelled" => Some("cancelled"),
        _ => None,
    }
}

/// 提交失败的类型：会话在途（调用方映射 409）或其他原因
pub enum SubmitError {
    InFlight,
    Other(String),
}

/// 统一的任务提交入口（HTTP 处理器与定时调度器共用）：
/// 登记 RunRecord → 订阅进度事件 → 后台 spawn 主循环。
/// 同一会话同时只允许一个在途任务（含 waiting_human）。
pub struct SessionRunGuard<R: tauri::Runtime = tauri::Wry> {
    pub(crate) app: AppHandle<R>,
    pub(crate) session_id: String,
    pub(crate) run_id: String,
    pub(crate) goal: String,
    pub(crate) event_id: tauri::EventId,
    pub(crate) state_event_id: tauri::EventId,
    terminal: std::sync::Arc<std::sync::Mutex<Option<(String, Option<String>)>>>,
}

impl<R: tauri::Runtime> SessionRunGuard<R> {
    pub(crate) fn new(
        app: &AppHandle<R>,
        session_id: &str,
        run_id: &str,
        goal: &str,
        llm_retries: u32,
    ) -> Self {
        let started_at = now_ts();
        let existing = {
            let runs = runs_store().lock().expect("runs store poisoned");
            runs.get(run_id).cloned()
        };
        let source = existing
            .as_ref()
            .map(|r| r.source.clone())
            .unwrap_or_else(|| "desktop".to_string());
        let report_every_actions = existing
            .as_ref()
            .map(|r| r.report_every_actions)
            .unwrap_or(0);
        let phase_push = existing
            .as_ref()
            .map(|r| r.phase_push)
            .unwrap_or(false);

        upsert_run(RunRecord {
            run_id: run_id.to_string(),
            session_id: session_id.to_string(),
            goal_preview: truncate_chars(goal, MAX_GOAL_PREVIEW_CHARS),
            status: "running".to_string(),
            paused_reason: None,
            paused_form: None,
            last_progress: None,
            event_count: 0,
            started_at: started_at.clone(),
            updated_at: started_at,
            result: None,
            error: None,
            llm_retries: llm_retries.clamp(1, 100),
            report_every_actions,
            phase_push,
            phase: None,
            state_detail: None,
            caller_notifies: existing.as_ref().map(|r| r.caller_notifies).unwrap_or(false),
            supplements_closed: true,
            source,
        });
        let _ = app.emit(
            "channel-run-started",
            json!({ "run_id": run_id, "session_id": session_id, "goal": goal, "at": now_ts() }),
        );
        notify_session_event(session_id, "start");

        let terminal = std::sync::Arc::new(std::sync::Mutex::new(None));
        let progress_terminal = terminal.clone();
        let listen_session = session_id.to_string();
        let listen_run = run_id.to_string();
        let event_id = app.listen(
            format!("agent-progress-{listen_session}"),
            move |event: tauri::Event| {
                // 终态之后的迟到进度不能把等待/运行状态重新带回来。
                if progress_terminal.lock().is_ok_and(|value| value.is_some()) { return; }
                let Ok(payload) = serde_json::from_str::<Value>(event.payload()) else {
                    return;
                };
                let description = payload
                    .get("description")
                    .and_then(Value::as_str)
                    .or_else(|| payload.get("message").and_then(Value::as_str))
                    .map(|s| truncate_chars(s, 400));
                let paused = payload.get("type").and_then(Value::as_str) == Some("agent_paused");
                let is_real_step = matches!(
                    payload.get("type").and_then(Value::as_str),
                    Some("step_done") | Some("step_error") | Some("complete")
                );
                let paused_reason = if paused {
                    Some(
                        payload
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("Agent 请求人工协助。")
                            .to_string(),
                    )
                } else {
                    None
                };
                let paused_form = if paused {
                    payload.get("form").filter(|v| !v.is_null()).cloned()
                } else {
                    None
                };
                patch_run(&listen_run, |record| {
                    if is_real_step {
                        record.event_count += 1;
                    }
                    if let Some(text) = description {
                        record.last_progress = Some(text);
                    }
                    if let Some(reason) = paused_reason {
                        record.status = "waiting_human".to_string();
                        record.paused_reason = Some(reason);
                        record.paused_form = paused_form;
                    } else if record.status == "waiting_human" {
                        record.status = "running".to_string();
                        record.paused_reason = None;
                        record.paused_form = None;
                    }
                });
                notify_session_event(&listen_session, if paused { "waiting_human" } else { "progress" });
            },
        );

        let state_session = session_id.to_string();
        let state_run = run_id.to_string();
        let state_terminal = terminal.clone();
        let state_event_id = app.listen(
            format!("agent-run-state-{state_session}"),
            move |event: tauri::Event| {
                let Ok(payload) = serde_json::from_str::<Value>(event.payload()) else {
                    return;
                };
                let message = payload
                    .get("message")
                    .and_then(Value::as_str)
                    .map(|text| truncate_chars(text, 200));
                let state = payload
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_lowercase();
                if let Some(status) = terminal_status(&state) {
                    if let Ok(mut terminal) = state_terminal.lock() {
                        *terminal = Some((status.to_string(), message.clone()));
                    }
                }
                let milestone = ["runtime", "recovering", "rescuing", "finalizing"]
                    .iter()
                    .any(|marker| state.contains(marker));
                patch_run(&state_run, |record| {
                    if let Some(text) = &message {
                        record.state_detail = Some(text.clone());
                    }
                    if milestone {
                        record.phase = Some(
                            message.unwrap_or_else(|| format!("进入阶段 {state}")),
                        );
                    }
                });
                notify_session_event(&state_session, "state");
            },
        );

        Self {
            app: app.clone(),
            session_id: session_id.to_string(),
            run_id: run_id.to_string(),
            goal: goal.to_string(),
            event_id,
            state_event_id,
            terminal,
        }
    }

    pub(crate) fn finish(self, outcome: &Result<(), String>) {
        let _ = self.app.unlisten(self.event_id);
        let _ = self.app.unlisten(self.state_event_id);
        let existing = runs_store().lock().expect("runs store poisoned").get(&self.run_id).cloned();
        let observed = self.terminal.lock().ok().and_then(|value| value.clone());
        let db = self.app.state::<DbState>();
        // 按本次 run + session 查权威终态，不能借用会话里上一轮的成功记录。
        let saved = db.0.lock().ok().and_then(|conn| conn.query_row(
            "SELECT status,completion_json FROM agent_run_traces WHERE id=?1 AND session_id=?2",
            rusqlite::params![self.run_id, self.session_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        ).optional().ok().flatten()).and_then(|(status, completion)| {
            let status = terminal_status(&status)?.to_string();
            let body = completion.and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
            let message = body.as_ref().and_then(|v| v.get("reason").or_else(|| v.get("summary")))
                .and_then(Value::as_str).map(str::to_owned);
            Some((status, message))
        });
        let previous = existing.as_ref().and_then(|r| terminal_status(&r.status)
            .map(|status| (status.to_string(), r.error.clone())));
        let known = saved.clone().or(observed).or(previous);
        let missing_terminal = known.is_none();
        let (status, message) = known.unwrap_or_else(|| ("failed".into(), Some(
            "运行已退出，但未取得本次任务的终态；不能确认成功完成。".into()
        )));
        let error = if status == "failed" {
            outcome.as_ref().err().cloned().or(message).map(|text| truncate_chars(&text, 2_000))
        } else { None };
        let result = last_assistant_message(&self.app, &self.session_id);
        patch_run(&self.run_id, |record| {
            record.status = status.clone();
            record.error = error.clone();
            record.result = result.clone();
            record.paused_reason = None;
            record.paused_form = None;
        });
        // 已有成功/失败/取消的事务结果不被收尾异常覆盖；缺终态才补运行失败记录。
        if saved.is_none() {
            let steps = crate::agent::run_trace::load_run_steps(db.inner(), &self.run_id).unwrap_or_default();
            let trace_status = if status == "done" { "succeeded" } else { &status };
            if let Err(error) = crate::agent::run_trace::upsert_snapshot(
                db.inner(), &self.run_id, &self.session_id, &self.goal, trace_status, &steps,
                Some(&json!({"summary":error.clone().or(result.clone()).unwrap_or_default()})),
            ) { eprintln!("[run-trace] 无法保存收尾终态：{error}"); }
        }
        if missing_terminal || outcome.is_err() {
            eprintln!("[run-finish] run={} status={} host_result={:?}", self.run_id, status, outcome);
        }
        notify_session_event(&self.session_id, &status);
        let _ = self.app.emit("channel-run-done", json!({
            "run_id":self.run_id, "session_id":self.session_id, "status":status, "at":now_ts()
        }));
        let notify_terminal = existing.as_ref().is_some_and(RunRecord::application_notifies_terminal);
        if status != "cancelled" && notify_terminal {
            if let Ok(config) = load_channel_config() {
                if config.auto_start_bridge {
                    let push_text = if status == "done" { completion_notification(result.as_deref()) }
                        else { format!("❌ 任务执行失败：\n{}", error.as_deref().unwrap_or("任务未完成")) };
                    tauri::async_runtime::spawn(async move { notify_wechat(&config, &push_text).await; });
                }
            }
        }
    }

}

/// 统一的任务提交入口（HTTP 处理器与定时调度器共用）：
/// 登记 RunRecord → 订阅进度事件 → 后台 spawn 主循环。
/// 同一会话同时只允许一个在途任务（含 waiting_human）。
pub(crate) fn register_and_spawn_run(
    app: AppHandle,
    session_id: String,
    goal: String,
    routing: ModelRoutingConfig,
    workspace_override: Option<std::path::PathBuf>,
    llm_retries: Option<u32>,
    _action_milestone_steps: u32,
    _phase_push: bool,
    plan_mode: Option<String>,
    caller_notifies: bool,
) -> Result<String, SubmitError> {
    {
        let runs = runs_store().lock().expect("runs store poisoned");
        if runs.values().any(|r| {
            r.session_id == session_id && matches!(r.status.as_str(), "running" | "waiting_human")
        }) {
            return Err(SubmitError::InFlight);
        }
    }

    let run_id = uuid::Uuid::new_v4().to_string();
    let started_at = now_ts();
    println!(
        "[channel] run {run_id} 启动 session={session_id} 重试上限={}",
        llm_retries
            .unwrap_or(crate::agent::main_loop::DEFAULT_TRANSIENT_LLM_REQUEST_RETRIES)
            .clamp(1, 100)
    );

    upsert_run(RunRecord {
        run_id: run_id.clone(),
        session_id: session_id.clone(),
        goal_preview: truncate_chars(&goal, MAX_GOAL_PREVIEW_CHARS),
        status: "running".to_string(),
        paused_reason: None,
        paused_form: None,
        last_progress: None,
        event_count: 0,
        started_at: started_at.clone(),
        updated_at: started_at.clone(),
        result: None,
        error: None,
        llm_retries: llm_retries
            .unwrap_or(crate::agent::main_loop::DEFAULT_TRANSIENT_LLM_REQUEST_RETRIES)
            .clamp(1, 100),
        report_every_actions: _action_milestone_steps,
        phase_push: _phase_push,
        phase: None,
        state_detail: None,
        source: "channel".to_string(),
        caller_notifies,
        supplements_closed: true,
    });

    // 立即持久化该轮任务初始状态（用户消息气泡由 SessionRunGuard::new 在 channel-run-started 前写入）
    let db_state = app.state::<DbState>();
    let _ = crate::agent::run_trace::upsert_snapshot(
        db_state.inner(),
        &run_id,
        &session_id,
        &goal,
        "running",
        &[],
        None,
    );

    let spawn_app = app.clone();
    let spawn_run = run_id.clone();
    let spawn_session = session_id.clone();
    let spawn_goal = goal;
    let spawn_plan_mode = plan_mode
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| "cross_review".to_string()); // 手机/通道端默认启用 cross_review 交叉审查模式
    tauri::async_runtime::spawn(async move {
        let db = spawn_app.state::<DbState>();
        let registry =
            spawn_app.state::<Arc<AsyncMutex<crate::agent::mcp::PluginRegistry>>>();
        let _ = run_agent_main_loop_in_workspace(
            spawn_app.clone(),
            db,
            registry,
            routing,
            spawn_goal.clone(),
            true, // 手机端任务默认 auto_pilot: true，放手让 Agent 自主闭环
            Some(spawn_session.clone()),
            None,
            llm_retries,
            None,
            workspace_override,
            Some(spawn_run.clone()),
            Some(spawn_plan_mode),
            None,
            None,
        )
        .await;
    });

    Ok(run_id)
}

// 发送端统一分段；这里保留全文，不生成只含前 300 字符的通知摘要。
fn completion_notification(result: Option<&str>) -> String {
    match result {
        Some(text) => format!("🏁 任务已完成：\n\n{text}"),
        None => "🏁 任务已完成。".to_string(),
    }
}

#[cfg(test)]
mod completion_notification_tests {
    use super::completion_notification;

    #[test]
    fn completion_notification_keeps_full_reply_beyond_old_limits() {
        let text = "完整结果🙂\n".repeat(6000);
        let notification = completion_notification(Some(&text));
        assert_eq!(notification, format!("🏁 任务已完成：\n\n{text}"));
        assert!(notification.ends_with(&text));
    }

    #[test]
    fn completion_notification_without_result_keeps_existing_message() {
        assert_eq!(completion_notification(None), "🏁 任务已完成。");
    }
}

#[cfg(test)]
mod state_regressions {
    use super::*;
    use crate::agent::{checkpoint::{AgentCheckpoint, AgentPhase}, context::SandwichContext, run_state::AgentRunMachine};

    #[tokio::test]
    async fn state_regression_handled_business_failure_must_not_be_reported_as_done() {
        let app = tauri::test::mock_app();
        let conn = crate::db::init_db(&std::path::PathBuf::from(":memory:")).unwrap();
        let session = format!("audit-fail-{}", uuid::Uuid::new_v4());
        let run = format!("audit-run-{}", uuid::Uuid::new_v4());
        conn.execute("INSERT INTO chat_sessions(id,title,model_id,created_at,updated_at) VALUES (?1,'audit','','now','now')", [&session]).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));
        app.manage(db.clone());
        let guard = SessionRunGuard::new(app.handle(), &session, &run, "cannot finish", 1);
        let context = SandwichContext::new("sys".into(), "cannot finish".into());
        let mut cp = AgentCheckpoint::new(session.clone(), &context);
        let mut machine = AgentRunMachine::new();
        let outcome = crate::agent::failure::finish_failed_run(app.handle(), &db, &session, &run,
            "cannot finish", &[], &mut cp, &context, &mut machine, "API 401: task incomplete".into()).await;
        assert!(outcome.is_ok(), "a handled business failure returns host Ok: {outcome:?}");
        assert_eq!(cp.phase, AgentPhase::Failed);
        guard.finish(&outcome);
        let status = runs_store().lock().unwrap().remove(&run).unwrap().status;
        let stored: String = db.0.lock().unwrap().query_row("SELECT status FROM agent_run_traces WHERE id=?1", [&run], |r| r.get(0)).unwrap();
        eprintln!("AUDIT handled-failure host_result=Ok checkpoint=failed trace={} channel={}", stored, status);
        assert_eq!(stored, "failed");
        assert_eq!(status, "failed", "host cleanup success is not business task success");
    }

    #[tokio::test]
    async fn state_regression_desktop_cancellation_must_not_be_reported_as_done() {
        let app = tauri::test::mock_app();
        let conn = crate::db::init_db(&std::path::PathBuf::from(":memory:")).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));
        app.manage(db.clone());
        let session = format!("audit-cancel-{}", uuid::Uuid::new_v4());
        let run = format!("audit-run-{}", uuid::Uuid::new_v4());
        let guard = SessionRunGuard::new(app.handle(), &session, &run, "cancel", 1);
        let context = SandwichContext::new("sys".into(), "cancel".into());
        let mut cp = AgentCheckpoint::new(session.clone(), &context);
        let mut machine = AgentRunMachine::new();
        let outcome = crate::agent::control::finish_interrupted_run(app.handle(), &db, &session, &run,
            "cancel", &[], &mut cp, &context, &mut machine, "用户请求停止当前任务。".into()).await;
        assert!(outcome.is_ok(), "{outcome:?}");
        guard.finish(&outcome);
        let status = runs_store().lock().unwrap().remove(&run).unwrap().status;
        let stored: String = db.0.lock().unwrap().query_row("SELECT status FROM agent_run_traces WHERE id=?1", [&run], |r| r.get(0)).unwrap();
        eprintln!("AUDIT handled-cancel machine={:?} trace={} channel={}", machine.state(), stored, status);
        assert_eq!(stored, "cancelled");
        assert_eq!(status, "cancelled", "desktop cancellation must survive host Ok teardown");
    }

    #[tokio::test]
    async fn state_regression_ok_without_terminal_is_not_success() {
        let app = tauri::test::mock_app();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(crate::db::init_db(&std::path::PathBuf::from(":memory:")).unwrap())));
        app.manage(db);
        let session = format!("unknown-terminal-{}", uuid::Uuid::new_v4());
        let run = format!("unknown-run-{}", uuid::Uuid::new_v4());
        let guard = SessionRunGuard::new(app.handle(), &session, &run, "unknown", 1);
        guard.finish(&Ok(()));
        let record = runs_store().lock().unwrap().remove(&run).unwrap();
        assert_eq!(record.status, "failed");
        assert!(record.error.unwrap().contains("未取得本次任务的终态"));
    }

    #[tokio::test]
    async fn state_regression_committed_success_survives_teardown_error_and_event_agrees() {
        let app = tauri::test::mock_app();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(crate::db::init_db(&std::path::PathBuf::from(":memory:")).unwrap())));
        app.manage(db.clone());
        let session = format!("committed-success-{}", uuid::Uuid::new_v4());
        let run = format!("committed-run-{}", uuid::Uuid::new_v4());
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let out = events.clone();
        let filter = run.clone();
        app.listen("channel-run-done", move |event| {
            let value: Value = serde_json::from_str(event.payload()).unwrap();
            if value["run_id"] == filter { out.lock().unwrap().push(value); }
        });
        let guard = SessionRunGuard::new(app.handle(), &session, &run, "success", 1);
        crate::agent::run_trace::upsert_snapshot(&db, &run, &session, "success", "succeeded", &[], Some(&json!({"summary":"committed"}))).unwrap();
        guard.finish(&Err("late emit failure".into()));
        assert_eq!(runs_store().lock().unwrap().remove(&run).unwrap().status, "done");
        assert_eq!(events.lock().unwrap().last().unwrap()["status"], "done");
        let status: String = db.0.lock().unwrap().query_row("SELECT status FROM agent_run_traces WHERE id=?1", [&run], |row| row.get(0)).unwrap();
        assert_eq!(status, "succeeded");
    }

    #[tokio::test]
    async fn state_regression_cancel_event_matches_record_even_on_host_error() {
        let app = tauri::test::mock_app();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(crate::db::init_db(&std::path::PathBuf::from(":memory:")).unwrap())));
        app.manage(db);
        let session = format!("pre-cancel-{}", uuid::Uuid::new_v4());
        let run = format!("cancel-run-{}", uuid::Uuid::new_v4());
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let out = events.clone();
        let filter = run.clone();
        app.listen("channel-run-done", move |event| {
            let value: Value = serde_json::from_str(event.payload()).unwrap();
            if value["run_id"] == filter { out.lock().unwrap().push(value); }
        });
        let guard = SessionRunGuard::new(app.handle(), &session, &run, "cancel", 1);
        patch_run(&run, |record| record.status = "cancelled".into());
        guard.finish(&Err("storage unavailable".into()));
        assert_eq!(runs_store().lock().unwrap().remove(&run).unwrap().status, "cancelled");
        assert_eq!(events.lock().unwrap().last().unwrap()["status"], "cancelled");
    }

    #[tokio::test]
    async fn state_regression_late_progress_cannot_reopen_terminal_waiting_state() {
        let app = tauri::test::mock_app();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(crate::db::init_db(&std::path::PathBuf::from(":memory:")).unwrap())));
        app.manage(db);
        let session = format!("late-progress-{}", uuid::Uuid::new_v4());
        let run = format!("late-run-{}", uuid::Uuid::new_v4());
        let guard = SessionRunGuard::new(app.handle(), &session, &run, "failed", 1);
        app.emit(&format!("agent-run-state-{session}"), json!({"state":"failed","message":"real failure"})).unwrap();
        app.emit(&format!("agent-progress-{session}"), json!({"type":"agent_paused","message":"late question"})).unwrap();
        assert_ne!(runs_store().lock().unwrap().get(&run).unwrap().status, "waiting_human");
        guard.finish(&Ok(()));
        assert_eq!(runs_store().lock().unwrap().remove(&run).unwrap().status, "failed");
    }

    #[tokio::test]
    async fn state_regression_normal_guard_completion_remains_done() {
        let app = tauri::test::mock_app();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(crate::db::init_db(&std::path::PathBuf::from(":memory:")).unwrap())));
        app.manage(db);
        let session = format!("audit-success-{}", uuid::Uuid::new_v4());
        let run = format!("audit-run-{}", uuid::Uuid::new_v4());
        let guard = SessionRunGuard::new(app.handle(), &session, &run, "done", 1);
        app.emit(&format!("agent-run-state-{session}"), json!({"state":"succeeded","message":"done"})).unwrap();
        guard.finish(&Ok(()));
        assert_eq!(runs_store().lock().unwrap().remove(&run).unwrap().status, "done");
    }
}
