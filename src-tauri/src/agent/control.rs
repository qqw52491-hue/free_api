use crate::agent::checkpoint::{save_checkpoint, AgentCheckpoint};
use crate::agent::run_state::{AgentRunEvent, AgentRunMachine};
use crate::agent::types::RUN_INTERRUPTED_ROUTE;
use crate::db::DbState;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::{mpsc, watch, Mutex};

// 每个 session 的控制通道区分 ask_human 回答、替换目标与停止请求。
// 替换只在模型/工具/等待人工三个安全边界消费，绝不硬杀未知副作用工具。
#[derive(Debug, Clone)]
pub(crate) enum HumanControl {
    Answer(String, Option<crate::agent::conversation::ConversationCursor>),
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

// Access only while holding both channel locks, in intervention -> interrupt order.
fn channel_owners() -> &'static std::sync::Mutex<HashMap<String, String>> {
    static OWNERS: OnceLock<std::sync::Mutex<HashMap<String, String>>> = OnceLock::new();
    OWNERS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Only the run that registered a channel may interrupt or remove it.
pub(crate) async fn release_run_channels(session_id: &str, run_id: &str, interrupt: bool) -> bool {
    let mut channels = intervention_channels().lock().await;
    let mut interrupts = stream_interrupt_channels().lock().await;
    let Ok(mut owners) = channel_owners().lock() else { return false };
    if owners.get(session_id).map(String::as_str) != Some(run_id) {
        return false;
    }
    if interrupt {
        if let Some(sender) = interrupts.get(session_id) {
            sender.send_modify(|generation| *generation = generation.wrapping_add(1));
        }
    }
    channels.remove(session_id);
    interrupts.remove(session_id);
    owners.remove(session_id);
    true
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
) -> Result<Option<crate::agent::conversation::ConversationCursor>, String> {
    let message = message.trim().to_string();
    if message.is_empty() {
        return Err("人工回答不能为空".to_string());
    }
    let channels = intervention_channels().lock().await;
    let sender = channels
        .get(&session_id)
        .ok_or_else(|| "任务已结束或通道不存在".to_string())?;
    // 先落消息取得身份，再投递；运行中任务消费回答后按 id 登记，入口才推进连续游标。
    // 落库失败仍允许回答送达，不冒认其他未消费的聊天记录。
    let cursor = match crate::agent::conversation::append_human_intervention(
        state.inner(), &session_id, &message,
    ) {
        Ok(cursor) => Some(cursor),
        Err(error) => {
            eprintln!("[control] 干预消息落盘失败: {error}");
            None
        }
    };
    sender.send(HumanControl::Answer(message, cursor.clone())).await.map_err(|e| e.to_string())?;
    // 统一分发中枢：SSE(手机/网页) + Tauri(桌面) 双路齐达
    crate::agent::channel::notify_session_event(&session_id, "message");
    Ok(cursor)
}

#[tauri::command]
pub async fn replace_agent_goal(session_id: String, goal: String) -> Result<(), String> {
    let goal = goal.trim().to_string();
    if goal.is_empty() {
        return Err("新目标不能为空".to_string());
    }
    let (sender, interrupt) = {
        let channels = intervention_channels().lock().await;
        let interrupts = stream_interrupt_channels().lock().await;
        let sender = channels.get(&session_id).cloned()
            .ok_or_else(|| "任务已结束或通道不存在".to_string())?;
        (sender, interrupts.get(&session_id).cloned())
    };
    sender
        .send(HumanControl::ReplaceGoal(goal))
        .await
        .map_err(|e| e.to_string())?;
    if let Some(interrupt) = interrupt {
        interrupt.send_modify(|generation| *generation = generation.wrapping_add(1));
    }
    Ok(())
}

#[derive(Default)]
struct StartingRunTracker {
    in_flight_counts: HashMap<String, usize>,
    pending_cancels: HashSet<String>,
    registered_sessions: HashSet<String>,
}

fn starting_tracker() -> &'static std::sync::Mutex<StartingRunTracker> {
    static TRACKER: OnceLock<std::sync::Mutex<StartingRunTracker>> = OnceLock::new();
    TRACKER.get_or_init(|| std::sync::Mutex::new(StartingRunTracker::default()))
}

/// 覆盖“进入主循环入口 → 注册控制通道”前的极早期启动窗口；
/// 离开入口作用域时自动递减计数并清理未消费的早期取消标记，避免串染下一轮运行。
pub(crate) struct StartingSessionGuard {
    session_id: String,
}

impl StartingSessionGuard {
    pub(crate) fn enter(session_id: &str) -> Self {
        if let Ok(mut tracker) = starting_tracker().lock() {
            *tracker.in_flight_counts.entry(session_id.to_string()).or_insert(0) += 1;
        }
        Self {
            session_id: session_id.to_string(),
        }
    }
}

impl Drop for StartingSessionGuard {
    fn drop(&mut self) {
        if let Ok(mut tracker) = starting_tracker().lock() {
            let remove_entry = match tracker.in_flight_counts.get_mut(&self.session_id) {
                Some(count) if *count > 1 => {
                    *count -= 1;
                    false
                }
                Some(_) => true,
                None => false,
            };
            if remove_entry {
                tracker.in_flight_counts.remove(&self.session_id);
                tracker.pending_cancels.remove(&self.session_id);
                tracker.registered_sessions.remove(&self.session_id);
            }
        }
    }
}

pub(crate) fn is_session_starting(session_id: &str) -> bool {
    starting_tracker()
        .lock()
        .map(|tracker| tracker.in_flight_counts.get(session_id).copied().unwrap_or(0) > 0)
        .unwrap_or(false)
}

pub(crate) fn mark_pending_cancel(session_id: &str) -> bool {
    starting_tracker()
        .lock()
        .map(|mut tracker| {
            if tracker.in_flight_counts.get(session_id).copied().unwrap_or(0) > 0
                && !tracker.registered_sessions.contains(session_id) {
                tracker.pending_cancels.insert(session_id.to_string());
                true
            } else {
                false
            }
        })
        .unwrap_or(false)
}

pub(crate) fn take_pending_cancel(session_id: &str) -> bool {
    starting_tracker()
        .lock()
        .map(|mut tracker| tracker.pending_cancels.remove(session_id))
        .unwrap_or(false)
}

/// 在同一 `intervention_channels -> stream_interrupt_channels` 临界区内原子完成
/// “消费早期取消标记 + 注册控制通道与流中断通道”，锁序恒为
/// `intervention_channels -> stream_interrupt_channels -> starting_tracker`，与 `cancel_agent_run` 互斥协调。
pub(crate) async fn register_run_channels(
    session_id: &str,
    run_id: &str,
    tx: mpsc::Sender<HumanControl>,
    interrupt_tx: watch::Sender<u64>,
) -> bool {
    let mut channels = intervention_channels().lock().await;
    let mut interrupts = stream_interrupt_channels().lock().await;
    let pending_cancel = take_pending_cancel(session_id);
    if pending_cancel {
        let _ = tx.try_send(HumanControl::Cancel);
        interrupt_tx.send_modify(|generation| *generation = generation.wrapping_add(1));
    }
    channels.insert(session_id.to_string(), tx);
    interrupts.insert(session_id.to_string(), interrupt_tx);
    if let Ok(mut owners) = channel_owners().lock() {
        owners.insert(session_id.to_string(), run_id.to_string());
    }
    if let Ok(mut tracker) = starting_tracker().lock() {
        if tracker.in_flight_counts.contains_key(session_id) {
            tracker.registered_sessions.insert(session_id.to_string());
        }
    }
    pending_cancel
}

/// 取消命令回执：明确告知调用方停止信号是否投递、后台是否确有活动运行，以及活动运行 ID。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CancelAgentRunResult {
    pub delivered: bool,
    pub has_active_run: bool,
    pub active_run_id: Option<String>,
}

#[tauri::command]
pub async fn cancel_agent_run(session_id: String) -> Result<CancelAgentRunResult, String> {
    // 快速自旋轮询最多 300ms（防止前端刚发起任务立即点击停止时通道尚在注册中的微小窗口）
    for attempt in 0..6 {
        let (existing_sender, pending_marked, active_run_id) = {
            let channels = intervention_channels().lock().await;
            let interrupts = stream_interrupt_channels().lock().await;
            if let Some(sender) = channels.get(&session_id).cloned() {
                if let Some(interrupt_sender) = interrupts.get(&session_id) {
                    interrupt_sender.send_modify(|generation| *generation = generation.wrapping_add(1));
                }
                (Some(sender), false, None)
            } else {
                // 与 register_run_channels 共享 intervention_channels -> stream_interrupt_channels 锁序：
                // 要么主循环尚未注册（写入 pending_cancels 后必被注册期消费并触发流中断），
                // 要么主循环已同时插入两个通道（上面分支必已同时命中控制通道与流中断通道）。
                let marked = mark_pending_cancel(&session_id);
                if marked {
                    if let Some(interrupt_sender) = interrupts.get(&session_id) {
                        interrupt_sender.send_modify(|generation| *generation = generation.wrapping_add(1));
                    }
                }
                let run_id = crate::agent::channel::active_run_id_for_session(&session_id);
                (None, marked, run_id)
            }
        };

        if let Some(sender) = existing_sender {
            let delivered = sender.send(HumanControl::Cancel).await.is_ok();
            let active_run_id = crate::agent::channel::active_run_id_for_session(&session_id);
            let has_active_run = active_run_id.is_some() || is_session_starting(&session_id);
            return Ok(CancelAgentRunResult {
                delivered,
                has_active_run,
                active_run_id,
            });
        }
        if pending_marked {
            return Ok(CancelAgentRunResult {
                delivered: true,
                has_active_run: true,
                active_run_id,
            });
        }
        if let Some(run_id) = active_run_id {
            if attempt == 5 {
                notify_stream_interrupt(&session_id).await;
                return Ok(CancelAgentRunResult {
                    delivered: false,
                    has_active_run: true,
                    active_run_id: Some(run_id),
                });
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }
    notify_stream_interrupt(&session_id).await;
    let active_run_id = crate::agent::channel::active_run_id_for_session(&session_id);
    let has_active_run = active_run_id.is_some() || is_session_starting(&session_id);
    Ok(CancelAgentRunResult {
        delivered: false,
        has_active_run,
        active_run_id,
    })
}

pub(crate) fn take_pending_interrupt(rx: &mut mpsc::Receiver<HumanControl>) -> Option<String> {
    loop {
        match rx.try_recv() {
            Ok(HumanControl::ReplaceGoal(goal)) => {
                return Some(format!("用户提交了新的任务目标：{}", goal))
            }
            Ok(HumanControl::Cancel) => return Some("用户请求停止当前任务。".to_string()),
            Ok(HumanControl::Answer(..)) => continue,
            Err(_) => return None,
        }
    }
}

pub(crate) async fn finish_interrupted_run<R: tauri::Runtime>(
    app: &AppHandle<R>,
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
    crate::agent::checkpoint::save_checkpoint_with_chat_claims(db, checkpoint, run_id, context)?;
    crate::agent::advance_run_state(
        app,
        session_id,
        run_id,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::channel::{runs_store, RunRecord};

    fn test_run_record(run_id: &str, session_id: &str, status: &str) -> RunRecord {
        RunRecord {
            run_id: run_id.to_string(),
            session_id: session_id.to_string(),
            goal_preview: "test".to_string(),
            status: status.to_string(),
            paused_reason: None,
            paused_form: None,
            last_progress: None,
            event_count: 0,
            started_at: "2026-11-13T00:00:00Z".to_string(),
            updated_at: "2026-11-13T00:00:00Z".to_string(),
            result: None,
            error: None,
            llm_retries: 1,
            report_every_actions: 0,
            phase_push: false,
            phase: None,
            state_detail: None,
            source: "channel".to_string(),
            caller_notifies: false,
            supplements_closed: true,
        }
    }

    #[tokio::test]
    async fn cancel_reports_no_active_run_when_channel_and_record_missing() {
        let session_id = "test-cancel-missing-session";
        let res = cancel_agent_run(session_id.to_string()).await.unwrap();
        assert_eq!(
            res,
            CancelAgentRunResult {
                delivered: false,
                has_active_run: false,
                active_run_id: None,
            }
        );
    }

    #[tokio::test]
    async fn cancel_delivers_to_live_waiting_human_channel_and_returns_run_id() {
        let session_id = "test-cancel-live-waiting-session";
        let run_id = "test-cancel-live-waiting-run";
        let (tx, mut rx) = mpsc::channel(2);
        intervention_channels()
            .lock()
            .await
            .insert(session_id.to_string(), tx);
        runs_store()
            .lock()
            .unwrap()
            .insert(run_id.to_string(), test_run_record(run_id, session_id, "waiting_human"));

        let res = cancel_agent_run(session_id.to_string()).await.unwrap();
        assert_eq!(
            res,
            CancelAgentRunResult {
                delivered: true,
                has_active_run: true,
                active_run_id: Some(run_id.to_string()),
            }
        );
        assert!(matches!(rx.try_recv(), Ok(HumanControl::Cancel)));

        intervention_channels().lock().await.remove(session_id);
        runs_store().lock().unwrap().remove(run_id);
    }

    #[tokio::test]
    async fn cancel_during_early_startup_records_pending_cancel_and_guard_drop_cleans_stale_flag() {
        let session_id = "test-cancel-early-startup-session";
        {
            let _guard = StartingSessionGuard::enter(session_id);
            let res = cancel_agent_run(session_id.to_string()).await.unwrap();
            assert_eq!(
                res,
                CancelAgentRunResult {
                    delivered: true,
                    has_active_run: true,
                    active_run_id: None,
                }
            );
            assert!(take_pending_cancel(session_id));
            assert!(!take_pending_cancel(session_id));

            // 再次标记但未消费，退出 StartingSessionGuard 时须自动清除，避免串染下一轮运行。
            assert!(mark_pending_cancel(session_id));
        }
        assert!(!is_session_starting(session_id));
        assert!(!take_pending_cancel(session_id));
    }

    #[tokio::test]
    async fn cancel_reports_undelivered_when_active_record_has_no_channel() {
        let session_id = "test-cancel-undelivered-session";
        let run_id = "test-cancel-undelivered-run";
        runs_store()
            .lock()
            .unwrap()
            .insert(run_id.to_string(), test_run_record(run_id, session_id, "running"));

        let res = cancel_agent_run(session_id.to_string()).await.unwrap();
        assert_eq!(
            res,
            CancelAgentRunResult {
                delivered: false,
                has_active_run: true,
                active_run_id: Some(run_id.to_string()),
            }
        );

        runs_store().lock().unwrap().remove(run_id);
    }

    #[tokio::test]
    async fn cancel_during_cleanup_does_not_claim_pending_delivery() {
        let session_id = "test-cancel-during-cleanup";
        let run_id = "test-cancel-during-cleanup-run";
        let guard = StartingSessionGuard::enter(session_id);
        let (tx, _rx) = mpsc::channel(5);
        let (interrupt_tx, _interrupt_rx) = watch::channel(0u64);
        register_run_channels(session_id, "test-run", tx, interrupt_tx).await;
        runs_store().lock().unwrap().insert(
            run_id.to_string(), test_run_record(run_id, session_id, "running"),
        );
        intervention_channels().lock().await.remove(session_id);
        stream_interrupt_channels().lock().await.remove(session_id);

        let result = cancel_agent_run(session_id.to_string()).await.unwrap();
        assert_eq!(result, CancelAgentRunResult {
            delivered: false,
            has_active_run: true,
            active_run_id: Some(run_id.to_string()),
        });
        assert!(!take_pending_cancel(session_id));
        drop(guard);
        runs_store().lock().unwrap().remove(run_id);
        let next = StartingSessionGuard::enter(session_id);
        assert!(mark_pending_cancel(session_id));
        drop(next);
    }

    #[tokio::test]
    async fn register_run_channels_consumes_pending_cancel_and_notifies_stream() {
        let session_id = "test-register-consumes-pending-cancel";
        let _guard = StartingSessionGuard::enter(session_id);
        let cancel_res = cancel_agent_run(session_id.to_string()).await.unwrap();
        assert!(cancel_res.delivered);
        assert!(cancel_res.has_active_run);

        let (tx, mut rx) = mpsc::channel(5);
        let (interrupt_tx, interrupt_rx) = watch::channel(0u64);
        let consumed = register_run_channels(session_id, "test-run", tx, interrupt_tx).await;

        assert!(consumed);
        assert!(matches!(rx.try_recv(), Ok(HumanControl::Cancel)));
        assert_eq!(*interrupt_rx.borrow(), 1);
        assert!(!take_pending_cancel(session_id));

        intervention_channels().lock().await.remove(session_id);
        stream_interrupt_channels().lock().await.remove(session_id);
    }

    #[tokio::test]
    async fn concurrent_register_and_cancel_interleaving_never_loses_signal() {
        for round in 0..40 {
            let session_id = format!("test-concurrent-cancel-register-{round}");
            let _guard = StartingSessionGuard::enter(&session_id);
            let (tx, mut rx) = mpsc::channel(5);
            let (interrupt_tx, mut interrupt_rx) = watch::channel(0u64);
            let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));

            let b1 = barrier.clone();
            let s1 = session_id.clone();
            let reg_task = tokio::spawn(async move {
                b1.wait().await;
                if round % 2 == 0 {
                    tokio::task::yield_now().await;
                }
                register_run_channels(&s1, "test-run", tx, interrupt_tx).await
            });

            let b2 = barrier.clone();
            let s2 = session_id.clone();
            let cancel_task = tokio::spawn(async move {
                b2.wait().await;
                if round % 2 == 1 {
                    tokio::task::yield_now().await;
                }
                cancel_agent_run(s2).await.unwrap()
            });

            let (_consumed, cancel_res) = tokio::join!(reg_task, cancel_task);
            let (consumed, cancel_res) = (_consumed.unwrap(), cancel_res.unwrap());
            assert!(cancel_res.delivered, "round {round}: cancel must report delivered");
            assert!(cancel_res.has_active_run, "round {round}: active starting run must be visible");
            assert!(
                matches!(rx.try_recv(), Ok(HumanControl::Cancel)),
                "round {round}: signal was dropped (consumed={consumed})"
            );
            assert!(
                interrupt_rx.has_changed().unwrap_or(false),
                "round {round}: stream interrupt receiver was not notified (consumed={consumed})"
            );
            assert_eq!(
                *interrupt_rx.borrow_and_update(),
                1,
                "round {round}: stream interrupt generation must advance"
            );
            assert!(
                !take_pending_cancel(&session_id),
                "round {round}: unconsumed stale pending_cancel remained"
            );

            intervention_channels().lock().await.remove(&session_id);
            stream_interrupt_channels().lock().await.remove(&session_id);
        }
    }

    #[tokio::test]
    async fn registered_channels_never_receive_cancel_without_stream_interrupt() {
        let session_id = "test-cancel-both-channels-together";
        let (tx, mut rx) = mpsc::channel(5);
        let (interrupt_tx, mut interrupt_rx) = watch::channel(0u64);
        let consumed = register_run_channels(session_id, "test-run", tx, interrupt_tx).await;
        assert!(!consumed);

        let res = cancel_agent_run(session_id.to_string()).await.unwrap();
        assert!(res.delivered);
        assert!(matches!(rx.try_recv(), Ok(HumanControl::Cancel)));
        assert!(interrupt_rx.has_changed().unwrap_or(false));
        assert_eq!(*interrupt_rx.borrow_and_update(), 1);

        intervention_channels().lock().await.remove(session_id);
        stream_interrupt_channels().lock().await.remove(session_id);
    }

    #[tokio::test]
    async fn old_run_cleanup_does_not_interrupt_or_remove_new_run_channels() {
        let session = format!("cleanup-owner-{}", uuid::Uuid::new_v4());
        let (old_tx, _old_rx) = mpsc::channel(2);
        let (old_interrupt, _old_watch) = watch::channel(0);
        register_run_channels(&session, "old", old_tx, old_interrupt).await;
        let (new_tx, mut new_rx) = mpsc::channel(2);
        let (new_interrupt, mut new_watch) = watch::channel(0);
        register_run_channels(&session, "new", new_tx, new_interrupt).await;

        assert!(!release_run_channels(&session, "old", true).await);
        assert!(!new_watch.has_changed().unwrap());
        assert!(matches!(new_rx.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        assert!(intervention_channels().lock().await.contains_key(&session));
        assert!(release_run_channels(&session, "new", true).await);
        assert_eq!(*new_watch.borrow_and_update(), 1);
        assert!(!intervention_channels().lock().await.contains_key(&session));
    }
}
