//! 运行控制 handler：状态查询、应答、改目标、取消。

use super::*;

pub(crate) fn model_id_of_session(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<String, (StatusCode, Json<Value>)> {
    conn.query_row(
        "SELECT model_id FROM chat_sessions WHERE id=?1",
        rusqlite::params![session_id],
        |row| row.get::<_, String>(0),
    )
    .map_err(|_| bad_request("会话缺少模型，请先在通道配置中填写默认模型"))
}

pub(crate) fn assistant_message_for_run<R: tauri::Runtime>(
    app: &AppHandle<R>,
    session_id: &str,
    run_id: &str,
) -> Option<String> {
    let conn = db_conn(app).ok()?;
    load_assistant_message_for_run(&conn, session_id, run_id).ok().flatten()
}

fn load_assistant_message_for_run(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT content FROM chat_messages WHERE session_id=?1 AND run_id=?2 AND role='assistant'
         ORDER BY rowid DESC LIMIT 1",
        rusqlite::params![session_id, run_id],
        |row| row.get::<_, String>(0),
    ).optional()
}

fn load_persisted_run_status(
    conn: &rusqlite::Connection,
    run_id: &str,
) -> rusqlite::Result<Option<Value>> {
    let row = conn.query_row(
        "SELECT session_id, status, completion_json FROM agent_run_traces WHERE id=?1",
        [run_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?)),
    ).optional()?;
    let Some((session_id, status, completion)) = row else { return Ok(None) };
    let status = super::run_spawn::terminal_status(&status).unwrap_or(&status);
    let completion = completion.and_then(|body| serde_json::from_str::<Value>(&body).ok());
    let error = completion.as_ref().and_then(|body| body.get("reason").or_else(|| body.get("summary")));
    let result = if status == "done" {
        load_assistant_message_for_run(conn, &session_id, run_id)?
    } else {
        None
    };
    Ok(Some(json!({
        "run_id": run_id, "session_id": session_id, "status": status, "result": result,
        "error": if status == "failed" { error } else { None },
    })))
}

fn run_status_value(record: &RunRecord) -> Result<Value, (StatusCode, Json<Value>)> {
    let mut value = serde_json::to_value(record).map_err(internal_error)?;
    if let Some(wait_id) = record.paused_wait_id.as_ref().filter(|_| record.status == "waiting_human") {
        if crate::agent::control::current_human_wait(&record.session_id, &record.run_id).as_ref() == Some(wait_id) {
            value["wait_id"] = json!(wait_id);
        } else {
            // The control slot has already advanced even if a progress event
            // has not updated the display record yet.
            value["status"] = json!("running");
            value["paused_reason"] = Value::Null;
            value["paused_form"] = Value::Null;
        }
    }
    Ok(value)
}

pub(crate) async fn handler_run_status(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(run_id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.request_config();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let record = runs_store().lock().expect("runs store poisoned").get(&run_id).cloned();
    if let Some(record) = record {
        return Ok(Json(run_status_value(&record)?));
    }
    {
        let conn = db_conn(&state.app)?;
        let persisted = state.acp_token.is_some() || conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM channel_mobile_submissions WHERE run_id=?1)",
                [&run_id],
                |row| row.get::<_, bool>(0),
            )
            .map_err(internal_error)?;
        if persisted {
            if let Some(record) = load_persisted_run_status(&conn, &run_id).map_err(internal_error)? {
                return Ok(Json(record));
            }
        }
    }
    Err(not_found("任务不存在或已被清理"))
}

pub(crate) async fn handler_run_list(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let mut runs: Vec<RunRecord> = runs_store()
        .lock()
        .expect("runs store poisoned")
        .values()
        .cloned()
        .collect();
    runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    let runs = runs.iter().map(run_status_value).collect::<Result<Vec<_>, _>>()?;
    Ok(Json(json!({ "runs": runs })))
}

pub(crate) async fn handler_run_answer(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(run_id): AxumPath<String>,
    Json(req): Json<AnswerReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.request_config();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let text = req.text.trim().to_string();
    if text.is_empty() {
        return Err(bad_request("回答内容不能为空"));
    }
    let (session_id, pending_wait) = {
        let runs = runs_store().lock().expect("runs store poisoned");
        let run = runs.get(&run_id).ok_or_else(|| not_found("任务不存在"))?;
        if run.status != "waiting_human" {
            return Err((StatusCode::CONFLICT, Json(json!({"error": "任务当前未等待回答，请查状态；执行中补充要求请使用「补充：内容」。"}))));
        }
        (run.session_id.clone(), run.paused_wait_id.clone())
    };
    let legacy_wait = if state.acp_token.is_none() { pending_wait.as_deref() } else { None };
    let wait_id = req.wait_id.as_deref()
        .or(legacy_wait)
        .ok_or_else(|| bad_request("缺少人工问题标识，请刷新任务状态"))?;
    let answer_state = state.app.state::<DbState>();
    crate::agent::control::send_human_reply(answer_state.inner(), &session_id, text, Some((&run_id, wait_id)))
        .await.map_err(|error| (StatusCode::CONFLICT, Json(json!({ "error": error }))))?;
    patch_run(&run_id, |record| {
        // A fast continuation may already have displayed a different question
        // or published a terminal result by the time the send returns.
        if record.status == "waiting_human" && record.paused_wait_id.as_deref() == Some(wait_id) {
            record.status = "running".to_string();
            record.paused_reason = None;
            record.paused_form = None;
            record.paused_wait_id = None;
        }
    });
    Ok(Json(json!({"ok": true, "session_id": session_id})))
}

/// 补充只落聊天记录，由下一步规划按消息身份导入，不发送停止/替换目标信号。
pub(crate) async fn handler_run_supplement(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(run_id): AxumPath<String>,
    Json(req): Json<AnswerReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let text = req.text.trim();
    if text.is_empty() {
        return Err(bad_request("补充内容不能为空"));
    }
    let session_id = {
        // 与交付关闭入口共用锁：关闭前成功落库的消息必须先导入，关闭后明确拒绝。
        let runs = runs_store().lock().expect("runs store poisoned");
        let run = runs.get(&run_id).ok_or_else(|| not_found("任务不存在"))?;
        validate_supplement_target(run).map_err(|message| (
            StatusCode::CONFLICT, Json(json!({"error": message})),
        ))?;
        let db = state.app.state::<DbState>();
        crate::agent::conversation::append_human_intervention(db.inner(), &run.session_id, text)
            .map_err(internal_error)?;
        run.session_id.clone()
    };
    notify_session_event(&session_id, "message");
    Ok(Json(json!({"ok": true, "session_id": session_id})))
}

fn validate_supplement_target(run: &RunRecord) -> Result<(), &'static str> {
    if run.status == "waiting_human" {
        return Err("当前任务正在等你回答，请发送「回答：内容」。");
    }
    if run.status != "running" {
        return Err("当前任务已结束，补充未发送，请作为新任务发送。");
    }
    if run.supplements_closed {
        return Err("当前任务尚未进入执行步骤或正在核验交付，补充未发送，请稍后查状态再发送。");
    }
    Ok(())
}

pub(crate) async fn handler_run_goal(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(run_id): AxumPath<String>,
    Json(req): Json<GoalReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let session_id = {
        let runs = runs_store().lock().expect("runs store poisoned");
        runs.get(&run_id)
            .map(|r| r.session_id.clone())
            .ok_or_else(|| not_found("任务不存在"))?
    };
    let goal = req.goal.trim().to_string();
    if goal.is_empty() {
        return Err(bad_request("新目标不能为空"));
    }
    crate::agent::control::replace_goal_for_run(&session_id, goal, Some(&run_id))
        .await
        .map_err(|error| (StatusCode::CONFLICT, Json(json!({ "error": error }))))?;
    Ok(Json(json!({"ok": true, "session_id": session_id})))
}

pub(crate) async fn handler_run_cancel(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(run_id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.request_config();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let session_id = {
        let runs = runs_store().lock().expect("runs store poisoned");
        runs.get(&run_id)
            .map(|r| r.session_id.clone())
            .ok_or_else(|| not_found("任务不存在"))?
    };
    let result = crate::agent::control::cancel_run(&session_id, Some(&run_id))
        .await.map_err(|error| (StatusCode::CONFLICT, Json(json!({ "error": error }))))?;
    if !result.delivered {
        return Err((StatusCode::CONFLICT, Json(json!({"error": "当前运行未收到停止请求，请刷新任务状态"}))));
    }
    Ok(Json(json!({"ok": true, "session_id": session_id})))
}

// ===== 评测报告在线浏览 =====
// 报告目录解析：评测落盘约定 CWD/agent_evaluations（见 evaluation.rs），但 App 正常启动
// 时 CWD 是 / 或 .app 内部，与评测机 repo 不同 → 手机端列表空白。
// 修法：合并扫描 ①CWD（保留原约定）② 全部已注册 agent_projects.root_path 下的
// agent_evaluations——报告属 repo 资产，按项目根目录归集。

#[derive(Deserialize)]
pub(crate) struct AnswerReq {
    pub(crate) text: String,
    pub(crate) wait_id: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct GoalReq {
    pub(crate) goal: String,
}

#[cfg(test)]
mod result_delivery_tests {
    use super::{load_assistant_message_for_run, load_persisted_run_status};

    #[test]
    fn result_preserves_full_text_and_matches_run_even_when_clock_moves_back() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE chat_messages (id TEXT, session_id TEXT, role TEXT, content TEXT, created_at INTEGER, run_id TEXT);
             CREATE TABLE agent_run_traces (id TEXT PRIMARY KEY, session_id TEXT, status TEXT, completion_json TEXT);
             INSERT INTO agent_run_traces VALUES ('this-run','target','succeeded',NULL);
             INSERT INTO chat_messages VALUES
                ('0','target','assistant','previous answer with later timestamp',6,'previous-run'),
                ('2','target','user','later user message',3,'this-run'),
                ('3','other','assistant','other session',4,'this-run'),
                ('4','target','assistant','legacy unbound answer',5,NULL);",
        ).unwrap();
        let text = "结果🙂\n".repeat(9000);
        conn.execute("INSERT INTO chat_messages VALUES ('1','target','assistant',?1,1,'this-run')", [&text]).unwrap();
        assert_eq!(load_assistant_message_for_run(&conn, "target", "this-run").unwrap().as_deref(), Some(text.as_str()));
        assert_eq!(load_assistant_message_for_run(&conn, "target", "missing").unwrap(), None);
        let record = load_persisted_run_status(&conn, "this-run").unwrap().unwrap();
        assert_eq!(record["session_id"], "target");
        assert_eq!(record["status"], "done");
        assert_eq!(record["result"].as_str(), Some(text.as_str()));
    }
}

#[cfg(test)]
mod supplement_tests {
    use super::*;

    #[test]
    fn supplements_only_enter_an_open_running_task() {
        let mut run = RunRecord {
            run_id: "supplement-test".into(), session_id: "s".into(), goal_preview: "goal".into(),
            status: "running".into(), paused_reason: None, paused_form: None, paused_wait_id: None, last_progress: None,
            event_count: 0, started_at: now_ts(), updated_at: now_ts(), result: None, error: None,
            llm_retries: 3, report_every_actions: 15, phase_push: true, phase: None, state_detail: None,
            source: "channel".into(), caller_notifies: true, supplements_closed: false,
        };
        assert!(validate_supplement_target(&run).is_ok());
        run.supplements_closed = true;
        assert!(validate_supplement_target(&run).unwrap_err().contains("核验交付"));
        run.supplements_closed = false;
        for status in ["waiting_human", "done", "failed", "cancelled"] {
            run.status = status.into();
            assert!(validate_supplement_target(&run).is_err());
        }
    }
}
