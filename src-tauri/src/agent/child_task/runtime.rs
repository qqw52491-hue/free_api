//! 独立运行任务：仅持有宿主身份与资源，不复制主循环。
use super::{TaskBinding, TaskGuard};
use crate::{
    agent::{router::ModelRoutingConfig, workspace_lock::WorkspaceRunGuard},
    db::DbState,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex},
};
use tauri::{AppHandle, Emitter, Listener, Manager};
use tokio::sync::watch;

#[derive(Clone)]
struct Running {
    binding: TaskBinding,
    done: watch::Receiver<bool>,
    cancel: watch::Sender<bool>,
    paused: Arc<Mutex<Option<Value>>>,
}
static RUNS: LazyLock<Mutex<HashMap<String, Running>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub(crate) fn attached(id: &str) -> bool {
    RUNS.lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(id)
}
pub(crate) fn active(parent_run: &str) -> Vec<String> {
    RUNS.lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(_, r)| r.binding.parent_run == parent_run)
        .map(|(id, _)| id.clone())
        .collect()
}
pub(crate) fn paused(id: &str) -> Option<Value> {
    RUNS.lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(id)
        .and_then(|r| r.paused.lock().ok().and_then(|v| v.clone()))
}
pub(crate) fn request_cancel(id: &str) -> Result<(), String> {
    let runs = RUNS.lock().map_err(|e| e.to_string())?;
    let run = runs
        .get(id)
        .ok_or("子任务当前没有活跃执行器；未发送取消。")?;
    run.cancel.send_replace(true);
    Ok(())
}
pub(crate) async fn stop_children(parent: &str) {
    let runs: Vec<Running> = RUNS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .filter(|r| r.binding.parent_run == parent)
        .cloned()
        .collect();
    for run in &runs {
        run.cancel.send_replace(true);
    }
    for mut run in runs {
        while !*run.done.borrow_and_update() {
            if run.done.changed().await.is_err() {
                break;
            }
        }
    }
}

fn execute(
    app: AppHandle,
    binding: TaskBinding,
    routing: ModelRoutingConfig,
    goal: String,
    workspace: PathBuf,
    auto: bool,
    mode: String,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> {
    Box::pin(async move {
        let state = app.state::<DbState>();
        let registry = app.state::<Arc<tokio::sync::Mutex<crate::agent::mcp::PluginRegistry>>>();
        let review=crate::agent::child_reviewer::main_executor::load(state.inner(),&binding.child_run)?;
        let _review_workspace=if review.is_some() {
            let (session,run)=super::workspace_owner(&binding.parent_session,&binding.parent_run)?;
            crate::agent::workspace_lock::acquire_delegation_lock(&workspace,&session,&run).await?
        }else{None};
        let budget=review.map(|_|crate::agent::main_loop::AgentRunBudget{max_steps:Some(crate::agent::review_tools::forensic_budget() as usize+crate::agent::child_reviewer::MAX_REVIEW_OUTPUT_ATTEMPTS as usize),max_prompt_tokens:None});
        crate::agent::work_history::in_group(
            None,
            crate::agent::main_loop::run_agent_main_loop_in_workspace_inner(
                app.clone(),
                state,
                registry,
                routing,
                goal,
                auto,
                binding.child_session.clone(),
                None,
                crate::agent::reviewer_gatekeeper::LLM_REQUEST_ATTEMPTS
                    .load(std::sync::atomic::Ordering::Relaxed),
                None,
                Some(workspace),
                binding.child_run.clone(),
                Some(mode),
                budget,
                None,
                Some(binding),
            ),
        )
        .await
    })
}
struct Relay {
    app: AppHandle,
    id: tauri::EventId,
}
impl Drop for Relay {
    fn drop(&mut self) {
        self.app.unlisten(self.id)
    }
}

#[derive(Clone)]
pub(crate) struct ReviewRelay {
    pub member_id: Option<String>,
    pub label: Option<String>,
    pub model: String,
    pub progress: Option<Arc<crate::agent::reviewer_gatekeeper::progress::ReviewProgress>>,
}

pub(super) struct ReviewProgressBridge {
    progress: Arc<crate::agent::reviewer_gatekeeper::progress::ReviewProgress>,
    tool_budget: u32,
    round: usize,
    completed_tools: u32,
    prev_thinking_chars: usize,
    step_tools: HashMap<i64, String>,
    finish_steps: std::collections::HashSet<i64>,
}

impl ReviewProgressBridge {
    pub(super) fn new(
        progress: Arc<crate::agent::reviewer_gatekeeper::progress::ReviewProgress>,
        tool_budget: u32,
    ) -> Self {
        Self {
            progress,
            tool_budget,
            round: 0,
            completed_tools: 0,
            prev_thinking_chars: 0,
            step_tools: HashMap::new(),
            finish_steps: std::collections::HashSet::new(),
        }
    }

    pub(super) fn handle(&mut self, kind: &str, payload: &Value) {
        match kind {
            "action_phase" => match payload["phase"].as_str() {
                Some("calling_llm") => {
                    self.round += 1;
                    self.prev_thinking_chars = 0;
                    self.progress.request(self.round, self.completed_tools, self.tool_budget);
                }
                Some("skills") => {
                    self.progress.phase("waiting", "正在装配审查上下文与可用工具");
                }
                _ => {
                    if let Some(desc) = payload["description"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                        self.progress.phase("waiting", desc);
                    }
                }
            },
            "thinking" if !payload["done"].as_bool().unwrap_or(false) => {
                let total = payload["content"].as_str().map(|s| s.chars().count()).unwrap_or(0);
                let delta = total.saturating_sub(self.prev_thinking_chars);
                if delta > 0 {
                    self.prev_thinking_chars = total;
                    self.progress.observe_stream_activity("thinking", delta);
                }
            }
            "content_stream" => {
                let delta = payload["chunk"].as_str().map(|s| s.chars().count()).unwrap_or(0);
                if delta > 0 {
                    self.progress.observe_stream_activity("content", delta);
                }
            }
            "token_usage" => {
                self.progress.record_request_metrics(
                    Some(self.round.max(1)),
                    payload["completion_tokens"].as_i64(),
                    payload["ttft_ms"].as_u64().unwrap_or(0),
                    payload["total_duration_ms"].as_u64().unwrap_or(0),
                );
            }
            "llm_parse_error" => {
                self.progress.phase("format_retry", "模型输出未满足指令协议，正在按协议纠偏重试");
            }
            "step_new" => {
                if let Some(id) = payload["step"]["id"].as_i64() {
                    let tool = payload["step"]["tool"]
                        .as_str()
                        .or_else(|| payload["step"]["action"].as_str())
                        .unwrap_or("")
                        .trim();
                    if tool.eq_ignore_ascii_case("finish") {
                        self.finish_steps.insert(id);
                    } else if !tool.is_empty() {
                        let desc = payload["step"]["description"].as_str().map(str::trim).filter(|s| !s.is_empty());
                        let label = match desc {
                            Some(d) => format!("{tool}（{}）", d.chars().take(48).collect::<String>()),
                            None => tool.to_string(),
                        };
                        self.step_tools.insert(id, label);
                    }
                }
            }
            "step_start" => {
                let step_id = payload["step_id"].as_i64();
                if step_id.is_some_and(|id| self.finish_steps.contains(&id)) {
                    self.progress.phase("converging", "正在校验并收口终审结论");
                } else {
                    let label = payload["tool"]
                        .as_str()
                        .filter(|s| !s.trim().is_empty())
                        .map(str::to_owned)
                        .or_else(|| step_id.and_then(|id| self.step_tools.get(&id).cloned()))
                        .or_else(|| payload["description"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned))
                        .unwrap_or_else(|| "取证工具".to_string());
                    self.progress.phase("tool", &format!("正在取证：{label}"));
                }
            }
            "step_done" | "step_error" => {
                let step_id = payload["step_id"].as_i64();
                if !step_id.is_some_and(|id| self.finish_steps.contains(&id)) {
                    self.completed_tools += 1;
                    self.progress.tool_completed(self.completed_tools);
                }
            }
            _ => {}
        }
    }
}

impl ReviewRelay {
    fn annotate(&self, payload: &mut Value, parent_run: &str) {
        payload["review"] = json!({
            "member_id": self.member_id, "label": self.label, "model": self.model,
            "parent_run_id": parent_run
        });
    }
}

pub(crate) fn spawn(
    app: AppHandle,
    db: DbState,
    binding: TaskBinding,
    routing: ModelRoutingConfig,
    goal: String,
    workspace: PathBuf,
    auto: bool,
    mode: String,
    workspace_guard: Option<WorkspaceRunGuard>,
    review: Option<ReviewRelay>,
) -> Result<(), String> {
    let guard = TaskGuard::enter(binding.clone());
    let (done_tx, done) = watch::channel(false);
    let (cancel, cancel_rx) = watch::channel(false);
    let paused = Arc::new(Mutex::new(None));
    RUNS.lock().map_err(|e| e.to_string())?.insert(
        binding.child_run.clone(),
        Running {
            binding: binding.clone(),
            done,
            cancel,
            paused: paused.clone(),
        },
    );
    let event_app = app.clone();
    let parent = binding.parent_session.clone();
    let child_id = binding.child_run.clone();
    let child_session = binding.child_session.clone();
    let parent_run = binding.parent_run.clone();
    let review_events = review.clone();
    let tool_budget = crate::agent::review_tools::forensic_budget();
    let progress_bridge = review_events
        .as_ref()
        .and_then(|r| r.progress.clone())
        .map(|progress| Arc::new(Mutex::new(ReviewProgressBridge::new(progress, tool_budget))));
    let id = app.listen(format!("agent-progress-{child_session}"), move |event| {
        if let Ok(payload) = serde_json::from_str::<Value>(event.payload()) {
            let kind = payload["type"].as_str().unwrap_or_default();
            if kind == "agent_paused" {
                *paused.lock().unwrap_or_else(|e| e.into_inner()) = Some(payload.clone());
                crate::agent::job_engine::notify_job_state_changed();
            }
            if matches!(kind, "step_done" | "step_error" | "complete") {
                let was = paused
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take()
                    .is_some();
                if was {
                    crate::agent::job_engine::notify_job_state_changed();
                }
            }
            let nested = kind == "child_task_progress";
            if nested && payload.get("review").is_some() {
                return;
            }
            if !nested {
                if let Some(ref bridge) = progress_bridge {
                    bridge
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .handle(kind, &payload);
                }
            }
            let review_step = review_events.as_ref().and_then(|_| super::relay_review_step(&child_id, &payload));
            let duration = payload["metrics"]["tool_duration_ms"].as_u64();
            if let Some(mut detail) = review_step.or_else(|| super::relay_progress(&child_id, payload)) {
                detail["run_id"] = json!(&parent_run);
                if let Some(review) = review_events.as_ref().filter(|_| !nested) {
                    review.annotate(&mut detail, &parent_run);
                    if let Some(duration) = duration {
                        detail["event"]["duration_ms"] = json!(duration);
                    }
                }
                let _ = event_app.emit(&format!("agent-progress-{parent}"), detail);
            }
        }
    });
    let relay = Relay {
        app: app.clone(),
        id,
    };
    if let Some(review) = &review {
        if let Some(ref progress) = review.progress {
            progress.phase("waiting", "终审执行器已启动，正在准备首轮请求");
        }
        let mut detail = json!({"type":"child_task_progress","child_run_id":binding.child_run,"event":{"type":"started"}});
        detail["run_id"] = json!(&binding.parent_run);
        review.annotate(&mut detail, &binding.parent_run);
        let _ = app.emit(&format!("agent-progress-{}", binding.parent_session), detail);
    }
    tauri::async_runtime::spawn(async move {
        let _relay = relay;
        let started = std::time::Instant::now();
        let result = supervise(
            execute(
                app.clone(),
                binding.clone(),
                routing,
                goal,
                workspace,
                auto,
                mode,
            ),
            &binding.child_session,
            cancel_rx,
        )
        .await;
        let warnings = crate::agent::failure::stop_run_runtime(
            &db,
            &binding.child_session,
            &binding.child_run,
        )
        .await;
        let settled = match crate::agent::child_reviewer::main_executor::load(&db,&binding.child_run) {
            Ok(Some(_))=>crate::agent::child_reviewer::main_executor::settle(&db,&binding,&result,started.elapsed().as_millis() as u64),
            Ok(None)=>super::settle(&db, &binding, &result, started.elapsed().as_millis() as u64),
            Err(error)=>Err(error),
        };
        if let Err(error) = settled {
            // Execution has ended; a conversion/storage error must not leave a
            // running child forever. Existing terminal CAS preserves cancellation.
            crate::agent::execution::runtime::fail_checkpoint(&db,&binding.child_run,error.clone());
            let _ = app.emit(
                &format!("agent-log-{}", binding.parent_session),
                format!("子任务 {} 交接保存失败：{error}", binding.child_run),
            );
        }
        for warning in warnings {
            let _ = app.emit(&format!("agent-log-{}", binding.parent_session), warning);
        }
        drop(workspace_guard);
        drop(guard);
        done_tx.send_replace(true);
        RUNS.lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&binding.child_run);
        crate::agent::job_engine::notify_job_state_changed();
        let mut detail = json!({"type":"child_task_progress","child_run_id":binding.child_run,"event":{"type":"returned"}});
        detail["run_id"] = json!(&binding.parent_run);
        if let Some(review) = &review {
            review.annotate(&mut detail, &binding.parent_run);
            if let Ok(conn) = db.0.lock() {
                if let Ok((status, duration)) = conn.query_row(
                    "SELECT status,duration_ms FROM agent_child_runs WHERE id=?1 AND kind='review'",
                    [&binding.child_run],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)),
                ) {
                    detail["event"]["execution_status"] = json!(status);
                    detail["event"]["duration_ms"] = json!(duration);
                }
            }
        }
        let _ = app.emit(&format!("agent-progress-{}", binding.parent_session), detail);
    });
    Ok(())
}

async fn supervise(
    future: std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>,
    session: &str,
    mut cancel_rx: watch::Receiver<bool>,
) -> Result<(), String> {
    let mut core = tokio::spawn(future);
    let mut retry = tokio::time::interval(std::time::Duration::from_millis(50));
    let mut sent_cancel = false;
    loop {
        tokio::select! {
            result=&mut core=>return result.unwrap_or_else(|e|Err(format!("子执行器已退出：{e}"))),
            _=cancel_rx.changed()=>{},
            _=retry.tick(),if *cancel_rx.borrow()&&!sent_cancel=>{
                let sender=crate::agent::control::intervention_channels().lock().await.get(session).cloned();
                if let Some(sender)=sender {sent_cancel=sender.try_send(crate::agent::control::HumanControl::Cancel).is_ok();crate::agent::control::notify_stream_interrupt(session).await;}
            }
        }
    }
}

#[cfg(test)]
mod tests;
