//! MCP 插件会话同步：从 main_loop 抽出的启动期同步逻辑。
//!
//! 首次任务全量扫描 YAML 插件目录并 spawn；后续任务仅对 DB 管理插件做
//! 增量同步（新增→spawn、禁用→移除、已有保留），消除每次 spawn 的秒级
//! 延迟。行为与原 main_loop 内联实现完全一致。

use crate::agent::mcp::{McpClient, PluginRegistry};
use crate::db::DbState;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;

pub async fn sync_mcp_plugins_for_session(
    app: &AppHandle,
    db: &DbState,
    registry_state: &Arc<Mutex<PluginRegistry>>,
    workspace_dir: &Path,
    session_id: &str,
) -> Result<(), String> {
    let _ = app.emit(
        &format!("agent-progress-{}", session_id),
        json!({
            "type": "action_phase",
            "phase": "mcp",
            "description": "🔍 正在同步 MCP 插件与存活状态..."
        }),
    );
    {
        let cwd = workspace_dir.to_path_buf();
        let mut registry = registry_state.lock().await;
        let is_first_init = registry.clients.is_empty();

        if is_first_init {
            // 首次：多路扫描 YAML 插件目录，全量 spawn
            println!("🔍 [插件探测] 首次初始化，扫描 YAML 插件目录...");
            let search_paths: Vec<std::path::PathBuf> = vec![
                cwd.join("plugins"),
                cwd.join("../plugins"),
                cwd.join("../../plugins"),
                app.path()
                    .app_config_dir()
                    .unwrap_or_default()
                    .join("plugins"),
                app.path()
                    .resource_dir()
                    .unwrap_or_default()
                    .join("plugins"),
            ];
            for path in search_paths {
                if path.exists() {
                    let reg = PluginRegistry::load_from_dir(&path);
                    for (name, client) in reg.clients {
                        println!("🔌 挂载 YAML 插件: {}", name);
                        registry.clients.insert(name, client);
                    }
                }
            }
        }

        // DB 管理插件：每次增量同步（新增 → spawn，禁用 → 移除，已有保留）
        let managed_plugins = crate::commands::load_managed_mcp_plugins(db)?;
        for managed in managed_plugins {
            let name = managed.config.name.clone();
            if !managed.enabled {
                if registry.clients.remove(&name).is_some() {
                    println!("⏸️ 卸载已禁用 DB 插件: {}", name);
                }
                continue;
            }
            if !registry.clients.contains_key(&name) {
                // 新增插件 → spawn
                match McpClient::spawn(managed.config) {
                    Ok(client) => {
                        let client_arc = Arc::new(client);
                        let c = client_arc.clone();
                        let n = name.clone();
                        tokio::spawn(async move {
                            let _ = c.ensure_session_initialized(None).await;
                            if let Ok(tools) = c.list_tools().await {
                                println!("🔌 DB 插件就绪: {} ({} 个工具)", n, tools.len());
                            }
                        });
                        println!("🔌 挂载新 DB 插件: {}", name);
                        registry.clients.insert(name, client_arc);
                    }
                    Err(error) => eprintln!("❌ DB 插件启动失败 [{}]: {}", name, error),
                }
            }
        }

        println!(
            "📊 MCP 插件就绪: {} 个（首次初始化: {}）",
            registry.clients.len(),
            is_first_init
        );
    }
    Ok(())
}
