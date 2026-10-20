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

pub(crate) fn last_assistant_message(app: &AppHandle, session_id: &str) -> Option<String> {
    let conn = db_conn(app).ok()?;
    load_last_assistant_message(&conn, session_id)
}

fn load_last_assistant_message(conn: &rusqlite::Connection, session_id: &str) -> Option<String> {
    conn.query_row(
        "SELECT content FROM chat_messages WHERE session_id=?1 AND role='assistant' ORDER BY created_at DESC, id DESC LIMIT 1",
        rusqlite::params![session_id],
        |row| row.get::<_, String>(0),
    )
    .ok()
}

pub(crate) async fn handler_run_status(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(run_id): AxumPath<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let runs = runs_store().lock().expect("runs store poisoned");
    match runs.get(&run_id) {
        Some(record) => Ok(Json(serde_json::to_value(record).unwrap_or_else(|_| json!({})))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "任务不存在或已被清理"})),
        )),
    }
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
    Ok(Json(json!({ "runs": runs })))
}

pub(crate) async fn handler_run_answer(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(run_id): AxumPath<String>,
    Json(req): Json<AnswerReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    let text = req.text.trim().to_string();
    if text.is_empty() {
        return Err(bad_request("回答内容不能为空"));
    }
    let session_id = {
        let runs = runs_store().lock().expect("runs store poisoned");
        let run = runs.get(&run_id).ok_or_else(|| not_found("任务不存在"))?;
        if run.status != "waiting_human" {
            return Err((StatusCode::CONFLICT, Json(json!({"error": "任务当前未等待回答，请查状态；执行中补充要求请使用「补充：内容」。"}))));
        }
        run.session_id.clone()
    };
    let answer_state = state.app.state::<DbState>();
    crate::agent::control::send_human_intervention(answer_state, session_id.clone(), text)
        .await
        .map_err(|e| bad_request(&e))?;
    patch_run(&run_id, |record| {
        record.status = "running".to_string();
        record.paused_reason = None;
        record.paused_form = None;
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
    crate::agent::control::replace_agent_goal(session_id.clone(), goal)
        .await
        .map_err(|e| bad_request(&e))?;
    patch_run(&run_id, |record| {
        record.status = "running".to_string();
        record.paused_reason = None;
        record.paused_form = None;
    });
    Ok(Json(json!({"ok": true, "session_id": session_id})))
}

pub(crate) async fn handler_run_cancel(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(run_id): AxumPath<String>,
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
    crate::agent::control::cancel_agent_run(session_id.clone())
        .await
        .map_err(|e| bad_request(&e))?;
    patch_run(&run_id, |record| {
        // 已到终态（done/failed/cancelled）的任务不再覆盖，避免取消请求把完成态改写
        if matches!(record.status.as_str(), "running" | "waiting_human") {
            record.status = "cancelled".to_string();
        }
        record.paused_reason = None;
        record.paused_form = None;
    });
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
}

#[derive(Deserialize)]
pub(crate) struct GoalReq {
    pub(crate) goal: String,
}

#[cfg(test)]
mod result_delivery_tests {
    use super::load_last_assistant_message;

    #[test]
    fn last_assistant_result_preserves_text_beyond_24000_and_session_boundary() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE chat_messages (id TEXT, session_id TEXT, role TEXT, content TEXT, created_at INTEGER)").unwrap();
        let text = "结果🙂\n".repeat(9000);
        conn.execute("INSERT INTO chat_messages VALUES ('1','target','assistant',?1,1)", [&text]).unwrap();
        conn.execute("INSERT INTO chat_messages VALUES ('2','target','user','later user message',2)", []).unwrap();
        conn.execute("INSERT INTO chat_messages VALUES ('3','other','assistant','other session',3)", []).unwrap();
        assert_eq!(load_last_assistant_message(&conn, "target"), Some(text));
        assert_eq!(load_last_assistant_message(&conn, "missing"), None);
    }
}

#[cfg(test)]
mod supplement_tests {
    use super::*;

    #[test]
    fn supplements_only_enter_an_open_running_task() {
        let mut run = RunRecord {
            run_id: "supplement-test".into(), session_id: "s".into(), goal_preview: "goal".into(),
            status: "running".into(), paused_reason: None, paused_form: None, last_progress: None,
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
