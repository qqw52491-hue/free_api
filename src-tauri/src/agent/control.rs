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

/// 会话执行位置面包屑：主循环在关键 await 边界打点，停滞看门狗触发时读取。
/// 纯内存态、不进 checkpoint——把"卡在 RunTool"细化到具体挂死的子阶段，
/// 用于定位 await 死锁/漏唤醒/panic 逃逸的确切现场。
fn stall_stages() -> &'static std::sync::Mutex<HashMap<String, String>> {
    static STAGES: OnceLock<std::sync::Mutex<HashMap<String, String>>> = OnceLock::new();
    STAGES.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

pub(crate) fn mark_stage(session_id: &str, stage: &str) {
    if let Ok(mut stages) = stall_stages().lock() {
        stages.insert(session_id.to_string(), stage.to_string());
    }
    // 尽力落库：让"卡在哪个子阶段"可实时查询（UI/CLI/看门狗之外的观察者）。
    // 必须 try_lock 非阻塞——mark_stage 位于派发等热路径上，若阻塞等待
    // 反而会成为它要诊断的冻结源之一；写不进去时内存态仍然是真相。
    if let Some(db) = crate::db::global_db() {
        if let Ok(conn) = db.0.try_lock() {
            let _ = conn.execute(
                "INSERT OR REPLACE INTO agent_stage_marks (session_id, stage, updated_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    session_id,
                    stage,
                    chrono::Utc::now().to_rfc3339()
                ],
            );
        }
    }
}

pub(crate) fn read_stage(session_id: &str) -> Option<String> {
    stall_stages()
        .lock()
        .ok()
        .and_then(|stages| stages.get(session_id).cloned())
}

pub(crate) fn clear_stage(session_id: &str) {
    if let Ok(mut stages) = stall_stages().lock() {
        stages.remove(session_id);
    }
    if let Some(db) = crate::db::global_db() {
        if let Ok(conn) = db.0.try_lock() {
            let _ = conn.execute(
                "DELETE FROM agent_stage_marks WHERE session_id = ?1",
                rusqlite::params![session_id],
            );
        }
    }
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
    state: State<'_, DbState>,
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
        .send(HumanControl::Answer(message.clone()))
        .await
        .map_err(|e| e.to_string())?;

    // 落盘成会话消息气泡：run 中时段的问答原来只走干预通道不写库，
    // 导致桌面/网页会话页永远看不到人的回答。落库失败不阻断应答送达。
    if let Err(error) =
        crate::agent::conversation::append_human_intervention(state.inner(), &session_id, &message)
    {
        eprintln!("[control] 干预消息落盘失败（已送达运行中任务）: {error}");
    }
    // 统一分发中枢：SSE(手机/网页) + Tauri(桌面) 双路齐达
    crate::agent::channel::notify_session_event(&session_id, "message");
    Ok(())
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
    for warning in crate::agent::failure::stop_run_runtime(db, session_id, run_id).await {
        eprintln!("[control] {warning}");
    }
    Ok(())
}
