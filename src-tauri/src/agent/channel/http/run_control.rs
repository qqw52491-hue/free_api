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
    conn.query_row(
        "SELECT content FROM chat_messages WHERE session_id=?1 AND role='assistant' ORDER BY created_at DESC, id DESC LIMIT 1",
        rusqlite::params![session_id],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .map(|content| truncate_chars(&content, MAX_RESULT_CHARS))
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
    let session_id = {
        let runs = runs_store().lock().expect("runs store poisoned");
        runs.get(&run_id)
            .map(|r| r.session_id.clone())
            .ok_or_else(|| not_found("任务不存在"))?
    };
    let text = req.text.trim().to_string();
    if text.is_empty() {
        return Err(bad_request("回答内容不能为空"));
    }
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
