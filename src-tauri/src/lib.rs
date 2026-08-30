#![allow(dead_code, unused_imports)]

mod agent;
mod commands;
mod db;
mod llm;
mod llm_breaker;
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
        .setup(|app| {
            // 启动清理：上次进程遗留的 running trace 标记为已中断（无跨进程恢复，僵尸记录防堆积）
            {
                let state = tauri::Manager::state::<DbState>(app);
                let lock = state.0.lock();
                match lock {
                    Ok(conn) => match crate::db::mark_orphan_running_traces_interrupted(&conn) {
                        Ok(n) if n > 0 => println!("[cleanup] 已将 {n} 条遗留 running trace 标记为已中断"),
                        Ok(_) => {}
                        Err(e) => eprintln!("[cleanup] 清理僵尸 running trace 失败: {e}"),
                    },
                    Err(e) => eprintln!("[cleanup] 数据库锁获取失败: {e}"),
                }
            }
            // 微信通道：本机 axum API（供 Node 桥调用），失败只打日志不阻断启动
            crate::agent::channel::spawn_channel_server(app.handle().clone());
            // 定时触发器：按计划自动派发任务，结果推微信
            // 隔离实例（测试/多开副本）设 FREE_API_DISABLE_SCHEDULER=1 停用，
            // 防止副本库里的历史定时任务被当成真实排程执行。
            if std::env::var("FREE_API_DISABLE_SCHEDULER").map(|v| v == "1").unwrap_or(false) {
                println!("[scheduler] FREE_API_DISABLE_SCHEDULER=1，定时触发器未启动");
            } else {
                crate::agent::scheduler::spawn_scheduler(app.handle().clone());
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // 平台
            commands::list_role_cards,
            commands::upsert_role_card,
            commands::toggle_role_card,
            commands::delete_role_card,
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
            commands::fetch_remote_models,
            commands::import_remote_models,
            // 会话
            commands::get_sessions,
            commands::create_session,
            commands::delete_session,
            commands::rename_session,
            // 项目、项目会话与用户明确授权的项目内资源
            commands::list_agent_projects,
            commands::create_agent_project,
            commands::delete_agent_project,
            commands::create_agent_project_session,
            commands::list_agent_project_sessions,
            commands::list_agent_project_resources,
            commands::add_agent_project_resource,
            commands::remove_agent_project_resource,
            commands::read_agent_project_resource,
            commands::get_agent_session_project,
            commands::get_agent_session_project_resources,
            // 消息
            commands::get_messages,
            commands::save_message,
            // 对话
            commands::send_chat,
            commands::cancel_chat,
            // MCP 插件管理
            commands::get_mcp_plugins,
            commands::get_mcp_tool_catalog,
            commands::refresh_mcp_plugin_connection,
            commands::generate_mcp_capability_card,
            commands::quick_connect_and_save_mcp,
            commands::batch_import_mcp_plugins,
            commands::save_mcp_plugin,
            commands::delete_mcp_plugin,
            commands::toggle_mcp_plugin,
            // 自定义提示词与 Skill 库
            commands::get_agent_custom_instructions,
            commands::generate_skill_capability_card,
            commands::batch_import_skills,
            commands::save_agent_custom_instruction,
            commands::toggle_agent_custom_instruction,
            commands::delete_agent_custom_instruction,
            // Agent 任务数据区与历史复盘（前端只读）
            commands::get_agent_task_resources,
            commands::get_agent_task_records,
            commands::get_session_run_traces,
            commands::read_agent_summary_markdown,
            commands::list_session_artifacts,
            commands::read_artifact_by_id,
            commands::open_artifact_by_id,
            commands::reveal_artifact_by_id,
            commands::delete_artifact_by_id,
            commands::export_artifact_to_file,
            commands::copy_file_to_clipboard,
            commands::get_agent_read_only_regression_goal,
            commands::begin_agent_read_only_regression,
            commands::generate_agent_evaluation_report,
            commands::open_in_webview_window,
            commands::open_artifact_in_system,
            commands::show_artifact_in_folder,
            commands::read_artifact_file,
            commands::resolve_artifact_absolute_path,
            commands::parse_attachment_file,
            commands::save_and_parse_uploaded_attachment,
            commands::search_web,
            commands::fetch_url_markdown,
            // Agent 执行
            agent::execute_command,
            agent::run_agent_main_loop,
            agent::evaluation_runner::start_background_agent_evaluation,
            agent::dispatch_agent_step,
            agent::get_agent_checkpoint,
            agent::send_human_intervention,
            agent::replace_agent_goal,
            agent::cancel_agent_run,
            // 微信通道（页面 ↔ Node 桥代理）
            agent::channel::wechat_bridge_start,
            agent::channel::wechat_bridge_status,
            agent::channel::wechat_bridge_login_start,
            agent::channel::wechat_bridge_relogin,
            agent::channel::wechat_bridge_qr,
            agent::channel::wechat_bridge_stop,
            agent::channel::wechat_channel_config_get,
            agent::channel::wechat_channel_config_set,
            agent::channel::wechat_channel_runs,
            agent::channel::wechat_channel_console_qr,
            // 定时触发器
            agent::scheduler::wechat_schedule_list,
            agent::scheduler::wechat_schedule_save,
            agent::scheduler::wechat_schedule_delete,
            agent::scheduler::wechat_schedule_toggle,
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
