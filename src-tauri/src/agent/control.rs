use crate::agent::checkpoint::{save_checkpoint, AgentCheckpoint};
use crate::agent::run_state::{AgentRunEvent, AgentRunMachine};
use crate::agent::types::RUN_INTERRUPTED_ROUTE;
use crate::db::DbState;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::{mpsc, watch, Mutex};

// 每个 session 的控制通道区分 ask_human 回答、替换目标与停止请求。
// 替换只在模型/工具/等待人工三个安全边界消费，绝不硬杀未知副作用工具。
#[derive(Debug, Clone)]
pub(crate) enum HumanControl {
    Answer(String),
    ReplaceGoal(String),
    Cancel,
}

pub fn intervention_channels() -> &'static Arc<Mutex<HashMap<String, mpsc::Sender<HumanControl>>>> {
    static CHANNELS: OnceLock<Arc<Mutex<HashMap<String, mpsc::Sender<HumanControl>>>>> =
        OnceLock::new();
    CHANNELS.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

pub fn stream_interrupt_channels() -> &'static Arc<Mutex<HashMap<String, watch::Sender<u64>>>> {
    static CHANNELS: OnceLock<Arc<Mutex<HashMap<String, watch::Sender<u64>>>>> = OnceLock::new();
    CHANNELS.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

pub async fn notify_stream_interrupt(session_id: &str) {
    if let Some(sender) = stream_interrupt_channels()
        .lock()
        .await
        .get(session_id)
        .cloned()
    {
        sender.send_modify(|generation| *generation = generation.wrapping_add(1));
    }
}

#[tauri::command]
pub async fn send_human_intervention(
    _state: State<'_, DbState>,
    session_id: String,
    message: String,
) -> Result<(), String> {
    let message = message.trim().to_string();
    if message.is_empty() {
        return Err("人工回答不能为空".to_string());
    }
    let channels = intervention_channels().lock().await;
    let sender = channels
        .get(&session_id)
        .ok_or_else(|| "任务已结束或通道不存在".to_string())?;
    sender
        .send(HumanControl::Answer(message))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn replace_agent_goal(session_id: String, goal: String) -> Result<(), String> {
    let goal = goal.trim().to_string();
    if goal.is_empty() {
        return Err("新目标不能为空".to_string());
    }
    let sender = intervention_channels()
        .lock()
        .await
        .get(&session_id)
        .cloned()
        .ok_or_else(|| "任务已结束或通道不存在".to_string())?;
    sender
        .send(HumanControl::ReplaceGoal(goal))
        .await
        .map_err(|e| e.to_string())?;
    notify_stream_interrupt(&session_id).await;
    Ok(())
}

#[tauri::command]
pub async fn cancel_agent_run(session_id: String) -> Result<(), String> {
    // 快速自旋轮询最多 300ms（防止前端刚发起任务立即点击停止时通道尚在注册中的微小窗口）
    for _ in 0..6 {
        if let Some(sender) = intervention_channels()
            .lock()
            .await
            .get(&session_id)
            .cloned()
        {
            let _ = sender.send(HumanControl::Cancel).await;
            notify_stream_interrupt(&session_id).await;
            return Ok(());
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }
    notify_stream_interrupt(&session_id).await;
    Ok(())
}

pub(crate) fn take_pending_interrupt(rx: &mut mpsc::Receiver<HumanControl>) -> Option<String> {
    loop {
        match rx.try_recv() {
            Ok(HumanControl::ReplaceGoal(goal)) => {
                return Some(format!("用户提交了新的任务目标：{}", goal))
            }
            Ok(HumanControl::Cancel) => return Some("用户请求停止当前任务。".to_string()),
            Ok(HumanControl::Answer(_)) => continue,
            Err(_) => return None,
        }
    }
}

pub(crate) async fn finish_interrupted_run(
    app: &AppHandle,
    db: &DbState,
    session_id: &str,
    run_id: &str,
    goal: &str,
    trace_steps: &[serde_json::Value],
    checkpoint: &mut AgentCheckpoint,
    context: &crate::agent::context::SandwichContext,
    machine: &mut AgentRunMachine,
    reason: String,
) -> Result<(), String> {
    let message = format!("{} 已在安全边界停止旧任务。", reason);
    checkpoint.mark_failed(message.clone(), context);
    save_checkpoint(db, checkpoint)?;
    crate::agent::advance_run_state(
        app,
        session_id,
        machine,
        crate::agent::run_state::AgentRunEvent::Cancel {
            reason: message.clone(),
        },
    )?;
    let failure_completion = serde_json::json!({ "summary": message });
    if let Err(error) = crate::agent::run_trace::upsert_snapshot(
        db,
        run_id,
        session_id,
        goal,
        "cancelled",
        trace_steps,
        Some(&failure_completion),
    ) {
        eprintln!("[run-trace] 无法保存取消轨迹: {error}");
    }
    app.emit(
        &format!("agent-log-{}", session_id),
        format!("⏹️ {} 不再派发旧计划。", message),
    )
    .ok();
    let _ = crate::agent::child_reviewer::cancel_running_child_runs(db, run_id);
    let _ = crate::agent::job_engine::cancel_all_running_jobs(db, run_id);
    intervention_channels().lock().await.remove(session_id);
    stream_interrupt_channels().lock().await.remove(session_id);
    Ok(())
}
