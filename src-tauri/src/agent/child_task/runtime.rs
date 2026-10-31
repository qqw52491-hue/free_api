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
                None,
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
            if let Some(detail) = super::relay_progress(&child_id, payload) {
                let _ = event_app.emit(&format!("agent-progress-{parent}"), detail);
            }
        }
    });
    let relay = Relay {
        app: app.clone(),
        id,
    };
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
        let settled = super::settle(&db, &binding, &result, started.elapsed().as_millis() as u64);
        if let Err(error) = settled {
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
        let _=app.emit(&format!("agent-progress-{}",binding.parent_session),json!({"type":"child_task_progress","child_run_id":binding.child_run,"event":{"type":"returned"}}));
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
