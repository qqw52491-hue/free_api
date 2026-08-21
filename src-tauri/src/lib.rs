#![allow(dead_code, unused_imports)]

mod agent;
mod commands;
mod db;
mod llm;
pub(crate) mod task_registry;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use db::{get_db_path, init_db, DbState};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let db_path = get_db_path();
    let conn = init_db(&db_path).expect("Failed to initialize database");

    let registry = std::sync::Arc::new(tokio::sync::Mutex::new(agent::mcp::PluginRegistry::new()));

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init());

    // 仅显式 E2E 测试构建开启内嵌 WebDriver；正常开发与发布版不暴露自动化控制端口。
    #[cfg(feature = "wdio")]
    let builder = builder.plugin(tauri_plugin_wdio_webdriver::init());

    builder
        .manage(DbState(std::sync::Arc::new(std::sync::Mutex::new(conn))))
        .manage(registry)
        .invoke_handler(tauri::generate_handler![
            // 平台
            commands::get_platforms,
            commands::add_platform,
            commands::update_platform,
            commands::delete_platform,
            // 模型
            commands::get_models,
            commands::get_all_models_with_platform,
            commands::add_model,
            commands::update_model,
            commands::delete_model,
            commands::test_model,
            commands::get_model_specs,
            commands::upsert_model_spec,
            commands::delete_model_spec,
            commands::add_model_with_spec,
            // 会话
            commands::get_sessions,
            commands::create_session,
            commands::delete_session,
            commands::rename_session,
            // 消息
            commands::get_messages,
            commands::save_message,
            // 对话
            commands::send_chat,
            // MCP 插件管理
            commands::get_mcp_plugins,
            commands::get_mcp_tool_catalog,
            commands::refresh_mcp_plugin_connection,
            commands::save_mcp_plugin,
            commands::delete_mcp_plugin,
            commands::toggle_mcp_plugin,
            // 自定义提示词与 Skill 库
            commands::get_agent_custom_instructions,
            commands::save_agent_custom_instruction,
            commands::toggle_agent_custom_instruction,
            commands::delete_agent_custom_instruction,
            // Agent 任务数据区与历史复盘（前端只读）
            commands::get_agent_task_resources,
            commands::get_agent_task_records,
            commands::get_session_run_traces,
            commands::read_agent_summary_markdown,
            commands::open_in_webview_window,
            commands::search_web,
            commands::fetch_url_markdown,
            // Agent 执行
            agent::execute_command,
            agent::run_agent_main_loop,
            agent::dispatch_agent_step,
            agent::get_agent_checkpoint,
            agent::send_human_intervention,
            agent::replace_agent_goal,
            agent::cancel_agent_run,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // 仅首次 ExitRequested 触发优雅停机；后续（如 app_handle.exit 再次触发）直接放行
            static SHUTDOWN_STARTED: AtomicBool = AtomicBool::new(false);
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                if SHUTDOWN_STARTED.swap(true, Ordering::SeqCst) {
                    return; // 已在停机流程中，允许默认退出
                }
                api.prevent_exit();
                let registry = task_registry::TaskRegistry::global();
                registry.trigger_shutdown();
                let app_handle = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    registry.graceful_shutdown(Duration::from_secs(5)).await;
                    app_handle.exit(0);
                });
            }
        });
}
