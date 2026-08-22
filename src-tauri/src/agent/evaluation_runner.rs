use crate::agent::control::cancel_agent_run;
use crate::agent::evaluation;
use crate::agent::main_loop::run_agent_main_loop;
use crate::agent::mcp::PluginRegistry;
use crate::agent::router::ModelRoutingConfig;
use crate::db::DbState;
use rusqlite::params;
use serde::Serialize;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tauri::{AppHandle, Emitter, Listener, Manager};
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout, Duration};

/// 默认运行 20 分钟；到期后通过现有取消通道在下一个安全边界停止。
pub const DEFAULT_EVALUATION_DURATION_MINUTES: u64 = 20;
const MAX_EVALUATION_DURATION_MINUTES: u64 = 20;
const STOP_GRACE_PERIOD: Duration = Duration::from_secs(120);
const HUMAN_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(250);

// 回归评测固定使用本机 SQLite 中的火山订阅模型，避免同名模型跨平台串路由。
const EVALUATION_PLATFORM_NAME: &str = "火山订阅";
const EVALUATION_REASONING_MODEL_NAME: &str = "deepseek-v4-pro";
const EVALUATION_EXECUTION_MODEL_NAME: &str = "minimax-m3";

/// 独立后台评测的启动回执。会话 ID 与评测 ID 均可用于定位实时事件、日志与最终报告。
#[derive(Debug, Clone, Serialize)]
pub struct BackgroundEvaluationStart {
    pub session_id: String,
    pub evaluation_id: String,
    pub scenario_id: String,
    pub max_duration_minutes: u64,
    /// SQLite 模型 UUID：固定为火山订阅的 DeepSeek V4 Pro。
    pub reasoning_model_id: String,
    /// SQLite 模型 UUID：固定为火山订阅的 MiniMax M3。
    pub execution_model_id: String,
}

/// 在不依赖 UI、模型或 MCP 的情况下监督一个后台工作单元。
/// 返回 true 表示工作单元达到时限且已执行安全停止回调。
async fn supervise_worker<F, C, CFut>(
    worker: &mut Pin<Box<F>>,
    deadline: Duration,
    stop_grace_period: Duration,
    on_deadline: C,
) -> bool
where
    F: Future,
    C: FnOnce() -> CFut,
    CFut: Future<Output = ()>,
{
    let timed_out = tokio::select! {
        _ = worker.as_mut() => false,
        _ = sleep(deadline) => true,
    };
    if timed_out {
        on_deadline().await;
        let _ = timeout(stop_grace_period, worker.as_mut()).await;
    }
    timed_out
}

fn is_agent_paused_payload(payload: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|event_type| event_type == "agent_paused")
}

fn resolve_enabled_model_id(
    db: &DbState,
    platform_name: &str,
    model_name: &str,
) -> Result<String, String> {
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    conn.query_row(
        "SELECT m.id
         FROM models m JOIN platforms p ON p.id = m.platform_id
         WHERE p.name = ?1 AND m.name = ?2 AND m.enabled = 1",
        params![platform_name, model_name],
        |row| row.get::<_, String>(0),
    )
    .map_err(|error| {
        format!(
            "回归评测模型未就绪：平台‘{}’中未找到已启用的模型‘{}’：{}",
            platform_name, model_name, error
        )
    })
}

fn fixed_evaluation_model_routing(
    db: &DbState,
    mut requested: ModelRoutingConfig,
) -> Result<ModelRoutingConfig, String> {
    let reasoning_model_id = resolve_enabled_model_id(
        db,
        EVALUATION_PLATFORM_NAME,
        EVALUATION_REASONING_MODEL_NAME,
    )?;
    let execution_model_id = resolve_enabled_model_id(
        db,
        EVALUATION_PLATFORM_NAME,
        EVALUATION_EXECUTION_MODEL_NAME,
    )?;

    requested.pro = reasoning_model_id.clone();
    requested.flash = execution_model_id;
    // 该只读评测不含图像；如意外出现视觉分流，仍落到受核验的 Pro，而非外部同名配置。
    requested.vision = reasoning_model_id;
    requested.force_pro_on_first_step = true;
    Ok(requested)
}

/// 启动独立的真实只读 Agent 回归评测。
///
/// 此命令不走图形化页面，也不复制或改写普通会话主循环。后台任务从 AppHandle
/// 取得已托管的数据库与 MCP 注册表，并直接复用 run_agent_main_loop；运行结束后只读
/// 分析本次隔离会话的真实日志与轨迹并写入报告。
///
/// 无人值守时若 Agent 调用 ask_human，主循环会发布 agent_paused 事件。本运行器会
/// 自动使用现有取消通道终止该评测并产出报告，而不会等待人类回复。
#[tauri::command]
pub async fn start_background_agent_evaluation(
    app: AppHandle,
    model_routing: ModelRoutingConfig,
    max_duration_minutes: Option<u64>,
    scenario: Option<String>,
) -> Result<BackgroundEvaluationStart, String> {
    let evaluation_model_routing = {
        let state = app.state::<DbState>();
        fixed_evaluation_model_routing(state.inner(), model_routing)?
    };
    let reasoning_model_id = evaluation_model_routing.pro.clone();
    let execution_model_id = evaluation_model_routing.flash.clone();
    let duration_minutes = max_duration_minutes
        .unwrap_or(DEFAULT_EVALUATION_DURATION_MINUTES)
        .clamp(1, MAX_EVALUATION_DURATION_MINUTES);
    let session_id = format!("agent-evaluation-{}", uuid::Uuid::new_v4());
    // 普通入口不传场景时保持标准回归；本地自动化可显式用环境变量选择
    // 已注册的只读场景，避免为了测试额外改动 UI 或普通会话。
    let requested_scenario = scenario.or_else(|| {
        std::env::var("FREE_API_EVALUATION_SCENARIO")
            .ok()
            .filter(|value| !value.trim().is_empty())
    });
    let evaluation_run = evaluation::begin_agent_evaluation_for_scenario(
        &session_id,
        requested_scenario.as_deref(),
    )?;
    let evaluation_id = evaluation_run.evaluation_id.clone();
    let scenario_id = evaluation_run.scenario_id.clone();
    let goal = evaluation_run.goal.clone();

    let event_app = app.clone();
    let worker_app = app.clone();
    let worker_session_id = session_id.clone();
    let worker_evaluation_id = evaluation_id.clone();
    let human_wait_detected = Arc::new(AtomicBool::new(false));
    let human_wait_for_listener = human_wait_detected.clone();
    let progress_event = format!("agent-progress-{}", session_id);
    let listener_id = app.listen(progress_event, move |event| {
        if is_agent_paused_payload(event.payload()) {
            human_wait_for_listener.store(true, Ordering::SeqCst);
        }
    });

    tauri::async_runtime::spawn(async move {
        let state_owner = worker_app.clone();
        let state = state_owner.state::<DbState>();
        let registry = state_owner.state::<Arc<Mutex<PluginRegistry>>>();
        let mut worker = Box::pin(run_agent_main_loop(
            worker_app.clone(),
            state,
            registry,
            evaluation_model_routing,
            goal,
            true,
            Some(worker_session_id.clone()),
            Some(true),
        ));

        let human_wait_app = event_app.clone();
        let human_wait_session_id = worker_session_id.clone();
        let human_wait_observer = tauri::async_runtime::spawn(async move {
            loop {
                if human_wait_detected.load(Ordering::SeqCst) {
                    let _ = human_wait_app.emit(
                        &format!("agent-log-{}", human_wait_session_id),
                        "[回归评测] 检测到 ask_human；无人值守评测不会等待人工回复，正在安全停止。",
                    );
                    let _ = cancel_agent_run(human_wait_session_id.clone()).await;
                    break;
                }
                sleep(HUMAN_WAIT_POLL_INTERVAL).await;
            }
        });

        let _ = supervise_worker(
            &mut worker,
            Duration::from_secs(duration_minutes * 60),
            STOP_GRACE_PERIOD,
            || async {
                let _ = event_app.emit(
                    &format!("agent-log-{}", worker_session_id),
                    "[回归评测] 已到达时限，已请求在下一个安全边界停止。",
                );
                let _ = cancel_agent_run(worker_session_id.clone()).await;
            },
        )
        .await;

        human_wait_observer.abort();
        worker_app.unlisten(listener_id);

        // 结束（或超过安全等待期）后释放主循环借用的受托管状态，再读取同一会话的报告。
        drop(worker);
        let report_state = state_owner.state::<DbState>();
        match evaluation::build_and_persist_report(
            report_state.inner(),
            &worker_session_id,
            Some(&worker_evaluation_id),
        ) {
            Ok(report) => {
                let _ = event_app.emit(
                    &format!("agent-log-{}", worker_session_id),
                    format!(
                        "[回归报告] 判定：{}；已写入 {}",
                        report.verdict, report.report_markdown_path
                    ),
                );
            }
            Err(error) => {
                let _ = event_app.emit(
                    &format!("agent-log-{}", worker_session_id),
                    format!("[回归报告生成失败] {}", error),
                );
            }
        }
    });

    Ok(BackgroundEvaluationStart {
        session_id,
        evaluation_id,
        scenario_id,
        max_duration_minutes: duration_minutes,
        reasoning_model_id,
        execution_model_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulated_agent_paused_event_is_detected_without_a_tauri_window() {
        assert!(is_agent_paused_payload(
            r#"{"type":"agent_paused","message":"需要人工确认"}"#
        ));
        assert!(!is_agent_paused_payload(r#"{"type":"agent_progress"}"#));
        assert!(!is_agent_paused_payload("not-json"));
    }

    #[tokio::test]
    async fn simulated_worker_completion_does_not_request_stop() {
        let stop_requested = Arc::new(AtomicBool::new(false));
        let flag = stop_requested.clone();
        let mut worker = Box::pin(async {});

        let timed_out = supervise_worker(
            &mut worker,
            Duration::from_millis(10),
            Duration::from_millis(1),
            move || async move {
                flag.store(true, Ordering::SeqCst);
            },
        )
        .await;

        assert!(!timed_out);
        assert!(!stop_requested.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn simulated_deadline_requests_stop_without_model_or_ui() {
        let stop_requested = Arc::new(AtomicBool::new(false));
        let flag = stop_requested.clone();
        let mut worker = Box::pin(std::future::pending::<()>());

        let timed_out = supervise_worker(
            &mut worker,
            Duration::from_millis(1),
            Duration::from_millis(1),
            move || async move {
                flag.store(true, Ordering::SeqCst);
            },
        )
        .await;

        assert!(timed_out);
        assert!(stop_requested.load(Ordering::SeqCst));
    }
}
