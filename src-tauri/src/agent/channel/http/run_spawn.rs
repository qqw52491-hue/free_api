//! 运行启动核心：会话运行守卫 RAII 与 register_and_spawn_run 派发。

use super::*;

/// 提交失败的类型：会话在途（调用方映射 409）或其他原因
pub enum SubmitError {
    InFlight,
    Other(String),
}

/// 统一的任务提交入口（HTTP 处理器与定时调度器共用）：
/// 登记 RunRecord → 订阅进度事件 → 后台 spawn 主循环。
/// 同一会话同时只允许一个在途任务（含 waiting_human）。
pub struct SessionRunGuard {
    pub(crate) app: AppHandle,
    pub(crate) session_id: String,
    pub(crate) run_id: String,
    pub(crate) goal: String,
    pub(crate) event_id: tauri::EventId,
    pub(crate) state_event_id: tauri::EventId,
}

impl SessionRunGuard {
    pub(crate) fn new(
        app: &AppHandle,
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
            source,
        });
        let _ = app.emit(
            "channel-run-started",
            json!({ "run_id": run_id, "session_id": session_id, "goal": goal, "at": now_ts() }),
        );
        notify_session_event(session_id, "start");

        let listen_session = session_id.to_string();
        let listen_run = run_id.to_string();
        let event_id = app.listen(
            format!("agent-progress-{listen_session}"),
            move |event: tauri::Event| {
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
        }
    }

    pub(crate) fn finish(self, outcome: &Result<(), String>) {
        let _ = self.app.unlisten(self.event_id);
        let _ = self.app.unlisten(self.state_event_id);
        match outcome {
            Ok(()) => {
                let result = last_assistant_message(&self.app, &self.session_id);
                let was_cancelled = runs_store()
                    .lock()
                    .expect("runs store poisoned")
                    .get(&self.run_id)
                    .map(|r| r.status == "cancelled")
                    .unwrap_or(false);
                patch_run(&self.run_id, |record| {
                    if !was_cancelled {
                        record.status = "done".to_string();
                    }
                    record.paused_reason = None;
                    record.paused_form = None;
                    record.result = result.clone();
                });
                notify_session_event(&self.session_id, if was_cancelled { "cancelled" } else { "done" });
                let _ = self.app.emit(
                    "channel-run-done",
                    json!({ "run_id": self.run_id, "session_id": self.session_id, "status": "done", "at": now_ts() }),
                );

                // 若开启了微信通道，主动向主人微信推送任务完成结果
                if !was_cancelled {
                    if let Ok(config) = load_channel_config() {
                        if config.auto_start_bridge {
                            let push_text = if let Some(ref text) = result {
                                let preview = if text.chars().count() > 300 {
                                    format!("{}...", text.chars().take(300).collect::<String>())
                                } else {
                                    text.clone()
                                };
                                format!("🏁 任务已完成：\n\n{}", preview)
                            } else {
                                "🏁 任务已完成。".to_string()
                            };
                            tauri::async_runtime::spawn(async move {
                                notify_wechat(&config, &push_text).await;
                            });
                        }
                    }
                }
            }
            Err(error) => {
                let err_msg = truncate_chars(error, 2_000);
                patch_run(&self.run_id, |record| {
                    if record.status != "cancelled" {
                        record.status = "failed".to_string();
                        record.error = Some(err_msg.clone());
                    }
                    record.paused_reason = None;
                    record.paused_form = None;
                });

                // 若开启了微信通道，主动向主人微信推送任务失败告警
                if let Ok(config) = load_channel_config() {
                    if config.auto_start_bridge {
                        let push_text = format!("❌ 任务执行失败：\n{}", truncate_chars(&err_msg, 200));
                        tauri::async_runtime::spawn(async move {
                            notify_wechat(&config, &push_text).await;
                        });
                    }
                }
                let db = self.app.state::<DbState>();
                let existing_steps =
                    crate::agent::run_trace::load_run_steps(db.inner(), &self.run_id)
                        .unwrap_or_default();
                let _ = crate::agent::run_trace::upsert_snapshot(
                    db.inner(),
                    &self.run_id,
                    &self.session_id,
                    &self.goal,
                    "failed",
                    &existing_steps,
                    Some(&json!({ "summary": error })),
                );
                notify_session_event(&self.session_id, "failed");
                let _ = self.app.emit(
                    "channel-run-done",
                    json!({ "run_id": self.run_id, "session_id": self.session_id, "status": "failed", "at": now_ts() }),
                );
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
        )
        .await;
    });

    Ok(run_id)
}
