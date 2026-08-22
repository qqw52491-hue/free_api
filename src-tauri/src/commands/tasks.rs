use crate::agent::task_store;
use crate::db::DbState;
use tauri::{AppHandle, Manager, State};

// ==================== TASK STORE & TRACE COMMANDS ====================

/// 列出当前 session 的任务资源目录。资源记录本体不会混入 Agent prompt。
#[tauri::command]
pub async fn get_agent_task_resources(
    state: State<'_, DbState>,
    session_id: String,
) -> Result<Vec<task_store::TaskResourceRef>, String> {
    task_store::list_resource_refs(state.inner(), &session_id)
}

/// 读取 Agent 自己生成的任务总结 Markdown 正文。路径必须位于 app_data/task_summaries 下。
#[tauri::command]
pub fn read_agent_summary_markdown(app: AppHandle, path: String) -> Result<String, String> {
    let summaries_root = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法定位应用数据目录: {}", error))?
        .join("task_summaries");
    let canonical_root = summaries_root
        .canonicalize()
        .map_err(|_| "任务总结目录不存在或尚未生成总结".to_string())?;
    let requested = std::path::PathBuf::from(path)
        .canonicalize()
        .map_err(|_| "任务总结文件不存在".to_string())?;
    if requested
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("md")
        || !requested.starts_with(&canonical_root)
    {
        return Err("只允许读取 Agent 自动生成的任务总结 Markdown".to_string());
    }
    std::fs::read_to_string(&requested).map_err(|error| format!("无法读取任务总结: {}", error))
}

/// 返回指定会话全部已落盘的运行过程树。该数据仅用于历史复盘，不影响运行态恢复。
#[tauri::command]
pub async fn get_session_run_traces(
    state: State<'_, DbState>,
    session_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    crate::agent::run_trace::list_session_traces(state.inner(), &session_id)
}

/// 分页读取当前 session 任务资源中的记录；底层会执行资源归属校验和 100 条上限。
#[tauri::command]
pub async fn get_agent_task_records(
    state: State<'_, DbState>,
    session_id: String,
    resource_id: String,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    task_store::list_records_page(state.inner(), &session_id, &resource_id, offset, limit)
}

/// 返回内置的只读 Agent 回归评测目标。该目标不修改工作区，只用于真实执行链的行为验证。
#[tauri::command]
pub fn get_agent_read_only_regression_goal() -> String {
    crate::agent::evaluation::READ_ONLY_REGRESSION_GOAL.to_string()
}

/// 创建一次只读回归评测的启动标记，并返回固定评测目标。标记用于隔离同一会话的历史日志。
#[tauri::command]
pub fn begin_agent_read_only_regression(
    session_id: String,
) -> Result<crate::agent::evaluation::AgentEvaluationRun, String> {
    crate::agent::evaluation::begin_agent_evaluation(&session_id)
}

/// 基于已落盘的 LLM 交换日志与运行轨迹，生成指定评测的本地回归报告。
#[tauri::command]
pub fn generate_agent_evaluation_report(
    state: State<'_, DbState>,
    session_id: String,
    evaluation_id: Option<String>,
) -> Result<crate::agent::evaluation::AgentEvaluationReport, String> {
    crate::agent::evaluation::build_and_persist_report(
        state.inner(),
        &session_id,
        evaluation_id.as_deref(),
    )
}
