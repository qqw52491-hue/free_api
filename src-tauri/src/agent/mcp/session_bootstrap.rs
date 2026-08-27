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
                    // merge_registry 同时合并 clients 与 sources，
                    // 保证 YAML 项在主 registry 中有来源标记（对账与冲突判定依赖它）
                    for name in registry.merge_registry(reg) {
                        println!("🔌 挂载 YAML 插件: {}", name);
                    }
                }
            }
        }

        // DB 管理插件对账：期望集合来自当前 DB 行（含 enabled 状态）。
        // reconcile 只作用于 ManagedDb 来源项：DB 行已删除或禁用 → 从 registry 移除；
        // Yaml 来源项永不因 DB 对账被移除。
        let managed_plugins = crate::commands::load_managed_mcp_plugins(db)?;
        let desired: std::collections::HashMap<String, bool> = managed_plugins
            .iter()
            .map(|managed| (managed.config.name.clone(), managed.enabled))
            .collect();

        let removed = {
            // 解构一次性可变借用整个 registry，再拆分两个字段引用给纯函数
            let crate::agent::mcp::registry::PluginRegistry {
                clients, sources, ..
            } = &mut *registry;
            crate::agent::mcp::registry::reconcile_managed_plugins(clients, sources, &desired)
        };
        for name in &removed {
            println!(
                "🗑️ 卸载已删除/禁用的 DB 插件: {}（配置变更在本次会话同步点生效）",
                name
            );
        }

        for managed in managed_plugins {
            if !managed.enabled {
                continue;
            }
            let name = managed.config.name.clone();
            // 用来源分类决策，而不是 clients.contains_key：
            // YAML 同名必须显式拒绝（不能静默忽略 DB 配置），孤儿项防御性跳过
            match registry.classify(&name) {
                None => {
                    // 全新插件 → spawn 并以 ManagedDb 来源注册
                    match McpClient::spawn(managed.config) {
                        Ok(client) => {
                            let client_arc = Arc::new(client);
                            match registry.insert_with_source(
                                name.clone(),
                                client_arc.clone(),
                                crate::agent::mcp::registry::PluginSource::ManagedDb,
                            ) {
                                Ok(()) => {
                                    let c = client_arc;
                                    let n = name.clone();
                                    tokio::spawn(async move {
                                        let _ = c.ensure_session_initialized(None).await;
                                        if let Ok(tools) = c.list_tools().await {
                                            println!(
                                                "🔌 DB 插件就绪: {} ({} 个工具)",
                                                n,
                                                tools.len()
                                            );
                                        }
                                    });
                                    println!("🔌 挂载新 DB 插件: {}", name);
                                }
                                Err(error) => {
                                    eprintln!("❌ DB 插件 [{}] 注册被拒绝: {}", name, error);
                                }
                            }
                        }
                        Err(error) => eprintln!("❌ DB 插件启动失败 [{}]: {}", name, error),
                    }
                }
                // 已注册的 managed 项 → 保留，不重复 spawn
                Some(crate::agent::mcp::registry::RegisteredKind::ManagedDb) => {}
                // YAML 同名 → 显式拒绝：DB 配置不会被静默忽略或覆盖 YAML 插件
                Some(crate::agent::mcp::registry::RegisteredKind::Yaml) => {
                    eprintln!(
                        "❌ DB 插件 [{}] 与已挂载的 YAML 插件同名，拒绝挂载（如需启用 DB 配置请重命名或移除同名 YAML 文件）",
                        name
                    );
                }
                // 孤儿：clients 有项但 sources 缺记录 → 防御性跳过，避免来源不明时覆盖
                Some(crate::agent::mcp::registry::RegisteredKind::Orphan) => {
                    eprintln!(
                        "⚠️ 注册表存在无来源标记的插件 [{}]，跳过 DB 挂载以避免覆盖",
                        name
                    );
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
