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
    let mut newly_spawned = Vec::new();
    // 需要 uv/uvx 但运行时未就绪的插件：先延迟，锁块外 ensure_uv 后补 spawn
    let mut uv_deferred: Vec<crate::commands::ManagedMcpPlugin> = Vec::new();
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
                    // uv 依赖检查：需 uvx 的插件且 uv 未就绪 → 延迟到锁外 ensure 后补 spawn
                    if crate::agent::uv_runtime::is_uv_invocation(&managed.config.command)
                        && crate::agent::uv_runtime::resolve_uv(Some(app)).is_none()
                    {
                        uv_deferred.push(managed);
                        continue;
                    }
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
                                    newly_spawned.push((name.clone(), client_arc));
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

    // uv 依赖补装重试：需 uvx 的 stdio 插件在锁外 ensure_uv（联网下载官方
    // release+sha256 校验）后重试 spawn——下载耗时不定，绝不能持锁执行。
    if !uv_deferred.is_empty() {
        let names: Vec<String> = uv_deferred.iter().map(|m| m.config.name.clone()).collect();
        let _ = app.emit(
            &format!("agent-progress-{}", session_id),
            json!({
                "type": "action_phase",
                "phase": "mcp",
                "description": format!("⬇️ 插件 [{}] 需要 uv 运行时，正在自动下载...", names.join(", "))
            }),
        );
        match crate::agent::uv_runtime::ensure_uv(app).await {
            Ok(uv_path) => {
                println!("🧰 uv 运行时就绪: {}", uv_path.display());
                let mut registry = registry_state.lock().await;
                for managed in uv_deferred {
                    let name = managed.config.name.clone();
                    if registry.classify(&name).is_some() {
                        continue; // 防御：期间已被挂载
                    }
                    match McpClient::spawn(managed.config) {
                        Ok(client) => {
                            let client_arc = Arc::new(client);
                            match registry.insert_with_source(
                                name.clone(),
                                client_arc.clone(),
                                crate::agent::mcp::registry::PluginSource::ManagedDb,
                            ) {
                                Ok(()) => {
                                    newly_spawned.push((name.clone(), client_arc));
                                    println!("🔌 挂载延迟的 DB 插件: {}", name);
                                }
                                Err(error) => {
                                    eprintln!("❌ DB 插件 [{}] 注册被拒绝: {}", name, error);
                                }
                            }
                        }
                        Err(error) => {
                            eprintln!("❌ uv 就绪后插件 [{}] 仍启动失败: {}", name, error)
                        }
                    }
                }
            }
            Err(error) => {
                eprintln!(
                    "❌ uv 运行时自动就绪失败，以下插件未挂载 [{}]: {}",
                    names.join(", "),
                    error
                );
            }
        }
    }

    if !newly_spawned.is_empty() {
        println!(
            "⏳ [MCP同步] 正在等待 {} 个新挂载 DB 插件完成握手与工具发现...",
            newly_spawned.len()
        );
        let mut handles = Vec::new();
        for (name, client) in newly_spawned {
            handles.push(tokio::spawn(async move {
                let init_res = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    async {
                        let _ = client.ensure_session_initialized(None).await;
                        client.list_tools().await
                    },
                )
                .await;
                match init_res {
                    Ok(Ok(tools)) => {
                        println!("🔌 DB 插件就绪: {} ({} 个工具)", name, tools.len());
                    }
                    Ok(Err(err)) => {
                        eprintln!("⚠️ DB 插件 [{}] 初始化或获取工具失败: {}", name, err);
                    }
                    Err(_) => {
                        eprintln!(
                            "⚠️ DB 插件 [{}] 初始握手超时 (5s)，已降级放行由后台继续尝试",
                            name
                        );
                    }
                }
            }));
        }
        for handle in handles {
            let _ = handle.await;
        }
    }

    Ok(())
}
