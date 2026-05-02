mod agent;
mod commands;
mod db;
mod llm;

use db::{get_db_path, init_db, DbState};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let db_path = get_db_path();
    let conn = init_db(&db_path).expect("Failed to initialize database");

    let registry = std::sync::Arc::new(tokio::sync::Mutex::new(agent::mcp::PluginRegistry::new()));

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .manage(DbState(std::sync::Mutex::new(conn)))
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
            commands::delete_model,
            commands::test_model,
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
            commands::save_mcp_plugin,
            commands::delete_mcp_plugin,
            commands::toggle_mcp_plugin,
            // Agent 执行
            agent::execute_command,
            agent::run_agent_main_loop,
            agent::dispatch_agent_step,
            agent::set_browser_launch_mode,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
