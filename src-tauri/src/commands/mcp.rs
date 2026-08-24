use crate::agent::mcp::{McpClient, PluginConfig};
use crate::agent::mcp_catalog::{
    invalidate_plugin, remember_tools, status as catalog_status, McpCatalogStatus,
};
use crate::db::DbState;
use crate::llm::ModelConfig;
use chrono::Utc;
use reqwest::Url;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::State;
use uuid::Uuid;

// ==================== MCP PLUGIN STRUCTS & HELPERS ====================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPluginRow {
    pub id: String,
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: std::collections::HashMap<String, String>,
    pub capability_card: String,
    pub enabled: bool,
    pub created_at: String,
}

/// Agent Runtime 直接读取的插件配置。SQLite 中的 enabled 标志优先于同名 YAML。
#[derive(Debug, Clone)]
pub struct ManagedMcpPlugin {
    pub config: PluginConfig,
    pub enabled: bool,
}

fn scan_yaml_plugins() -> Vec<PluginConfig> {
    let mut list = Vec::new();
    let search_paths = vec![
        std::path::PathBuf::from("plugins"),
        std::path::PathBuf::from("../plugins"),
        std::path::PathBuf::from("../../plugins"),
    ];
    for dir in search_paths {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("yaml")
                    || path.extension().and_then(|e| e.to_str()) == Some("yml")
                {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(config) = serde_yaml::from_str::<PluginConfig>(&content) {
                            if !list.iter().any(|c: &PluginConfig| c.name == config.name) {
                                list.push(config);
                            }
                        }
                    }
                }
            }
        }
    }
    list
}

pub fn load_managed_mcp_plugins(db: &DbState) -> Result<Vec<ManagedMcpPlugin>, String> {
    let mut map: std::collections::HashMap<String, ManagedMcpPlugin> =
        std::collections::HashMap::new();

    // 1. 先扫描 YAML 本地文件插件（默认启用）
    for config in scan_yaml_plugins() {
        let name = config.name.clone();
        map.insert(
            name,
            ManagedMcpPlugin {
                config,
                enabled: true,
            },
        );
    }

    // 2. 再读取 SQLite 数据库中的插件（数据库优先覆盖 enabled 状态与 capability_card）
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT name, command, args, env, capability_card, enabled, url, headers FROM mcp_plugins ORDER BY created_at ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let name: String = row.get(0)?;
            let command: String = row.get(1)?;
            let args_json: String = row.get(2)?;
            let env_json: String = row.get(3)?;
            let capability_card: String = row.get::<_, Option<String>>(4)?.unwrap_or_default();
            let enabled = row.get::<_, i64>(5)? == 1;
            let url: String = row.get::<_, Option<String>>(6)?.unwrap_or_default();
            let headers_json: String = row.get::<_, Option<String>>(7)?.unwrap_or_default();
            Ok((name, command, args_json, env_json, capability_card, enabled, url, headers_json))
        })
        .map_err(|e| e.to_string())?;

    for row in rows.flatten() {
        let (name, command, args_json, env_json, capability_card, enabled, url, headers_json) = row;
        let card_opt = if capability_card.trim().is_empty() {
            None
        } else {
            Some(capability_card.trim().to_string())
        };
        let url_opt = if url.trim().is_empty() { None } else { Some(url.trim().to_string()) };
        let headers: std::collections::HashMap<String, String> =
            serde_json::from_str(&headers_json).unwrap_or_default();
        if let Some(existing) = map.get_mut(&name) {
            existing.enabled = enabled;
            if !command.is_empty() {
                existing.config.command = command;
            }
            if url_opt.is_some() {
                // DB 的原生 HTTP 配置优先于同名 YAML 的传输层定义
                existing.config.url = url_opt.clone();
                existing.config.headers = headers.clone();
            }
            if card_opt.is_some() {
                existing.config.capability_card = card_opt;
            }
        } else {
            map.insert(
                name.clone(),
                ManagedMcpPlugin {
                    config: PluginConfig {
                        name,
                        command,
                        args: serde_json::from_str(&args_json).unwrap_or_default(),
                        url: url_opt,
                        headers,
                        env: serde_json::from_str(&env_json).unwrap_or_default(),
                        capability_card: card_opt,
                    },
                    enabled,
                },
            );
        }
    }

    let mut plugins: Vec<ManagedMcpPlugin> = map.into_values().collect();
    plugins.sort_by(|a, b| a.config.name.cmp(&b.config.name));
    Ok(plugins)
}

fn enabled_mcp_config(db: &DbState, name: &str) -> Result<PluginConfig, String> {
    load_managed_mcp_plugins(db)?
        .into_iter()
        .find(|plugin| plugin.enabled && plugin.config.name == name)
        .map(|plugin| plugin.config)
        .ok_or_else(|| format!("未找到已启用的 MCP 插件: {}", name))
}

// ==================== MCP TAURI COMMANDS ====================

#[tauri::command]
pub async fn get_mcp_plugins(state: State<'_, DbState>) -> Result<Vec<McpPluginRow>, String> {
    let managed = load_managed_mcp_plugins(state.inner())?;
    let rows = managed
        .into_iter()
        .map(|m| McpPluginRow {
            id: format!("mcp-{}", m.config.name),
            name: m.config.name.clone(),
            command: if let Some(ref url) = m.config.url {
                format!("HTTP直连: {}", url)
            } else {
                m.config.command.clone()
            },
            args: m.config.args.clone(),
            env: m.config.env.clone(),
            capability_card: m.config.capability_card.clone()
                .or_else(|| crate::agent::mcp_catalog::cached_capability_card(&m.config))
                .unwrap_or_default(),
            enabled: m.enabled,
            created_at: if m.config.url.is_some() {
                "远程 HTTP/SSE".to_string()
            } else {
                "本地 stdio 进程".to_string()
            },
        })
        .collect();
    Ok(rows)
}

#[tauri::command]
pub async fn get_mcp_tool_catalog(
    state: State<'_, DbState>,
    name: Option<String>,
) -> Result<Vec<McpCatalogStatus>, String> {
    Ok(load_managed_mcp_plugins(state.inner())?
        .into_iter()
        .filter(|plugin| plugin.enabled)
        .filter(|plugin| {
            name.as_ref()
                .map(|target| target == &plugin.config.name)
                .unwrap_or(true)
        })
        .map(|plugin| catalog_status(&plugin.config))
        .collect())
}

#[tauri::command]
pub async fn refresh_mcp_plugin_connection(
    state: State<'_, DbState>,
    name: String,
) -> Result<McpCatalogStatus, String> {
    let config = enabled_mcp_config(state.inner(), &name)?;
    invalidate_plugin(&name);
    let client = McpClient::spawn(config.clone())?;
    client.initialize().await?;
    let tools = client.list_tools().await?;
    remember_tools(&config, &tools);

    let card = crate::agent::mcp_catalog::cached_capability_card(&config);
    Ok(McpCatalogStatus {
        name,
        tool_count: tools.len(),
        tools,
        capability_card: card,
        cached: true,
        refreshed_at_ms: Some(Utc::now().timestamp_millis() as u128),
    })
}

#[tauri::command]
pub async fn generate_mcp_capability_card(
    state: State<'_, DbState>,
    model_id: Option<String>,
    name: String,
    command: Option<String>,
    args: Option<Vec<String>>,
    env: Option<std::collections::HashMap<String, String>>,
) -> Result<String, String> {
    let model_cfg = if let Some(ref mid) = model_id.filter(|s| !s.trim().is_empty()) {
        ModelConfig::from_db(&state, mid)?
    } else {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT m.id FROM models m JOIN platforms p ON p.id = m.platform_id WHERE m.enabled = 1 ORDER BY m.created_at DESC")
            .map_err(|e| e.to_string())?;
        let model_ids: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .ok()
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        drop(stmt);
        drop(conn);

        let mut found = None;
        for id in model_ids {
            if let Ok(cfg) = ModelConfig::from_db(&state, &id) {
                found = Some(cfg);
                break;
            }
        }
        found.ok_or_else(|| "未找到可用的大模型配置，请先在模型管理中配置 API Key".to_string())?
    };

    let config = if let (Some(cmd), Some(a)) = (command, args) {
        PluginConfig {
            name: name.clone(),
            command: cmd,
            args: a,
            url: None,
            headers: std::collections::HashMap::new(),
            env: env.unwrap_or_default(),
            capability_card: None,
        }
    } else {
        enabled_mcp_config(state.inner(), &name)?
    };

    let client = McpClient::spawn(config.clone())?;
    client.initialize().await?;
    let tools = client.list_tools().await?;

    let card = crate::agent::mcp_catalog::generate_llm_mcp_card(&name, &tools, &model_cfg).await?;
    Ok(card)
}

#[tauri::command]
pub async fn quick_connect_and_save_mcp(
    state: State<'_, DbState>,
    name: Option<String>,
    remote_url: String,
    bearer_token: Option<String>,
    http_transport: Option<String>,
    model_id: Option<String>,
) -> Result<McpPluginRow, String> {
    let raw_url = remote_url.trim().to_string();
    if raw_url.is_empty() {
        return Err("MCP 远端 URL 不能为空".to_string());
    }

    let transport = http_transport.unwrap_or_else(|| "streamable-http".to_string());
    let plugin_name = if let Some(n) = name.filter(|s| !s.trim().is_empty()) {
        n.trim().to_string()
    } else {
        Url::parse(&raw_url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.replace(['.', ':'], "-")))
            .unwrap_or_else(|| "remote-mcp".to_string())
    };

    // 原生 HTTP 直连：不经 uvx/mcp-stdio 桥，消除子进程依赖与 stdio 握手竞态。
    let _ = transport; // 传输协议由 McpClient 按 streamable-http 处理，无需桥参数
    let command = String::new();
    let args: Vec<String> = Vec::new();

    let mut headers_map = std::collections::HashMap::new();
    if let Some(token) = bearer_token.filter(|t| !t.trim().is_empty()) {
        headers_map.insert("Authorization".to_string(), format!("Bearer {}", token.trim()));
    }
    let env_map = std::collections::HashMap::new();

    let config = PluginConfig {
        name: plugin_name.clone(),
        command: command.clone(),
        args: args.clone(),
        url: Some(raw_url.clone()),
        headers: headers_map.clone(),
        env: env_map.clone(),
        capability_card: None,
    };

    let client = McpClient::spawn(config.clone())
        .map_err(|e| format!("启动 MCP 客户端失败: {}", e))?;
    client.initialize().await
        .map_err(|e| format!("MCP 握手失败: {}", e))?;
    let tools = client.list_tools().await
        .map_err(|e| format!("获取工具列表失败: {}", e))?;

    let model_cfg = if let Some(ref mid) = model_id.filter(|s| !s.trim().is_empty()) {
        ModelConfig::from_db(&state, mid)?
    } else {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT m.id FROM models m JOIN platforms p ON p.id = m.platform_id WHERE m.enabled = 1 ORDER BY m.created_at DESC")
            .map_err(|e| e.to_string())?;
        let model_ids: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .ok()
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        drop(stmt);
        drop(conn);

        let mut found = None;
        for id in model_ids {
            if let Ok(cfg) = ModelConfig::from_db(&state, &id) {
                found = Some(cfg);
                break;
            }
        }
        found.ok_or_else(|| "未找到可用的大模型配置，请先在模型管理中配置 API Key".to_string())?
    };

    let capability_card = crate::agent::mcp_catalog::generate_llm_mcp_card(&plugin_name, &tools, &model_cfg)
        .await
        .unwrap_or_default();

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let args_json = serde_json::to_string(&args).unwrap_or_default();
    let env_json = serde_json::to_string(&env_map).unwrap_or_default();
    let headers_json = serde_json::to_string(&headers_map).unwrap_or_default();

    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO mcp_plugins (id, name, command, args, env, capability_card, enabled, created_at, url, headers)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8, ?9)
         ON CONFLICT(name) DO UPDATE SET
             command = excluded.command,
             args = excluded.args,
             env = excluded.env,
             capability_card = excluded.capability_card,
             enabled = 1,
             url = excluded.url,
             headers = excluded.headers",
        rusqlite::params![id, plugin_name, command, args_json, env_json, capability_card, now, raw_url, headers_json],
    ).map_err(|e| format!("保存插件到数据库失败: {}", e))?;

    invalidate_plugin(&plugin_name);

    Ok(McpPluginRow {
        id,
        name: plugin_name,
        command,
        args,
        env: env_map,
        capability_card,
        enabled: true,
        created_at: now,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchMcpImportResult {
    pub name: String,
    pub success: bool,
    pub tool_count: usize,
    pub capability_card: String,
    pub error: Option<String>,
}

#[tauri::command]
pub async fn batch_import_mcp_plugins(
    state: State<'_, DbState>,
    import_payload: String,
    model_id: Option<String>,
) -> Result<Vec<BatchMcpImportResult>, String> {
    let trimmed = import_payload.trim();
    if trimmed.is_empty() {
        return Err("导入内容不能为空".to_string());
    }

    let mut parsed_configs: Vec<PluginConfig> = Vec::new();

    if trimmed.starts_with('{') {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let servers_obj = if val.get("mcpServers").is_some() {
                val.get("mcpServers").and_then(|v| v.as_object())
            } else {
                val.as_object()
            };

            if let Some(obj) = servers_obj {
                for (name, item) in obj {
                    if let Some(cmd) = item.get("command").and_then(|v| v.as_str()) {
                        let args = item.get("args")
                            .and_then(|v| v.as_array())
                            .map(|arr| arr.iter().filter_map(|s| s.as_str().map(|x| x.to_string())).collect())
                            .unwrap_or_default();
                        let env = item.get("env")
                            .and_then(|v| v.as_object())
                            .map(|obj| obj.iter().filter_map(|(k, v)| v.as_str().map(|val| (k.clone(), val.to_string()))).collect())
                            .unwrap_or_default();
                        parsed_configs.push(PluginConfig {
                            name: name.clone(),
                            command: cmd.to_string(),
                            args,
                            url: None,
                            headers: std::collections::HashMap::new(),
                            env,
                            capability_card: None,
                        });
                    } else if let Some(url_str) = item.get("url").and_then(|v| v.as_str()) {
                        // 原生 HTTP 直连：token 进 Authorization 头，不再搭 uvx/mcp-stdio 桥
                        let mut headers = std::collections::HashMap::new();
                        if let Some(token) = item.get("token").or_else(|| item.get("bearer_token")).and_then(|v| v.as_str()).filter(|t| !t.trim().is_empty()) {
                            headers.insert("Authorization".to_string(), format!("Bearer {}", token.trim()));
                        }
                        parsed_configs.push(PluginConfig {
                            name: name.clone(),
                            command: String::new(),
                            args: Vec::new(),
                            url: Some(url_str.to_string()),
                            headers,
                            env: std::collections::HashMap::new(),
                            capability_card: None,
                        });
                    }
                }
            }
        }
    }

    if parsed_configs.is_empty() {
        for line in trimmed.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.is_empty() {
                continue;
            }
            let first = parts[0];
            if first.starts_with("http://") || first.starts_with("https://") {
                let token = parts.get(1).map(|s| s.to_string());
                let host_name = Url::parse(first)
                    .ok()
                    .and_then(|u| u.host_str().map(|h| h.replace(['.', ':'], "-")))
                    .unwrap_or_else(|| "remote-mcp".to_string());
                let name = parts.get(2).map(|s| s.to_string()).unwrap_or(host_name);

                let mut headers = std::collections::HashMap::new();
                if let Some(t) = token.filter(|t| !t.trim().is_empty()) {
                    headers.insert("Authorization".to_string(), format!("Bearer {}", t.trim()));
                }

                parsed_configs.push(PluginConfig {
                    name,
                    command: String::new(),
                    args: Vec::new(),
                    url: Some(first.to_string()),
                    headers,
                    env: std::collections::HashMap::new(),
                    capability_card: None,
                });
            }
        }
    }

    if parsed_configs.is_empty() {
        return Err("未能解析出有效的 MCP 配置，请确保是标准 mcpServers JSON 格式或 HTTP URL 清单".to_string());
    }

    let model_cfg = if let Some(ref mid) = model_id.filter(|s| !s.trim().is_empty()) {
        ModelConfig::from_db(&state, mid)?
    } else {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT m.id FROM models m JOIN platforms p ON p.id = m.platform_id WHERE m.enabled = 1 ORDER BY m.created_at DESC")
            .map_err(|e| e.to_string())?;
        let model_ids: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .ok()
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        drop(stmt);
        drop(conn);

        let mut found = None;
        for id in model_ids {
            if let Ok(cfg) = ModelConfig::from_db(&state, &id) {
                found = Some(cfg);
                break;
            }
        }
        found.ok_or_else(|| "未找到可用的大模型配置，请先在模型管理中配置 API Key".to_string())?
    };

    let mut results = Vec::new();
    for config in parsed_configs {
        let name = config.name.clone();
        match McpClient::spawn(config.clone()) {
            Ok(client) => {
                if let Err(e) = client.initialize().await {
                    results.push(BatchMcpImportResult {
                        name,
                        success: false,
                        tool_count: 0,
                        capability_card: String::new(),
                        error: Some(format!("握手失败: {}", e)),
                    });
                    continue;
                }
                match client.list_tools().await {
                    Ok(tools) => {
                        let tool_count = tools.len();
                        let capability_card = crate::agent::mcp_catalog::generate_llm_mcp_card(&name, &tools, &model_cfg)
                            .await
                            .unwrap_or_default();
                        
                        let id = uuid::Uuid::new_v4().to_string();
                        let now = chrono::Utc::now().to_rfc3339();
                        let args_json = serde_json::to_string(&config.args).unwrap_or_default();
                        let env_json = serde_json::to_string(&config.env).unwrap_or_default();
                        let url_saved = config.url.clone().unwrap_or_default();
                        let headers_json = serde_json::to_string(&config.headers).unwrap_or_default();

                        if let Ok(conn) = state.0.lock() {
                            let _ = conn.execute(
                                "INSERT INTO mcp_plugins (id, name, command, args, env, capability_card, enabled, created_at, url, headers)
                                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8, ?9)
                                 ON CONFLICT(name) DO UPDATE SET
                                     command = excluded.command,
                                     args = excluded.args,
                                     env = excluded.env,
                                     capability_card = excluded.capability_card,
                                     enabled = 1,
                                     url = excluded.url,
                                     headers = excluded.headers",
                                rusqlite::params![id, name, config.command, args_json, env_json, capability_card, now, url_saved, headers_json],
                            );
                            invalidate_plugin(&name);
                        }

                        results.push(BatchMcpImportResult {
                            name,
                            success: true,
                            tool_count,
                            capability_card,
                            error: None,
                        });
                    }
                    Err(e) => {
                        results.push(BatchMcpImportResult {
                            name,
                            success: false,
                            tool_count: 0,
                            capability_card: String::new(),
                            error: Some(format!("获取工具列表失败: {}", e)),
                        });
                    }
                }
            }
            Err(e) => {
                results.push(BatchMcpImportResult {
                    name,
                    success: false,
                    tool_count: 0,
                    capability_card: String::new(),
                    error: Some(format!("启动客户端失败: {}", e)),
                });
            }
        }
    }

    Ok(results)
}

#[tauri::command]
pub async fn save_mcp_plugin(
    state: State<'_, DbState>,
    name: String,
    command: String,
    args: Vec<String>,
    env: std::collections::HashMap<String, String>,
    capability_card: Option<String>,
    enabled: bool,
) -> Result<McpPluginRow, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("插件名称不能为空".to_string());
    }
    if command.trim().is_empty() {
        return Err("插件可执行命令不能为空".to_string());
    }

    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let args_json = serde_json::to_string(&args).map_err(|e| e.to_string())?;
    let env_json = serde_json::to_string(&env).map_err(|e| e.to_string())?;
    let cap_card = capability_card.unwrap_or_default();

    conn.execute(
        "INSERT INTO mcp_plugins (id, name, command, args, env, capability_card, enabled, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(name) DO UPDATE SET
             command = excluded.command,
             args = excluded.args,
             env = excluded.env,
             capability_card = excluded.capability_card,
             enabled = excluded.enabled",
        params![id, name, command, args_json, env_json, cap_card, if enabled { 1 } else { 0 }, now],
    )
    .map_err(|e| e.to_string())?;

    invalidate_plugin(&name);

    Ok(McpPluginRow {
        id,
        name,
        command,
        args,
        env,
        capability_card: cap_card,
        enabled,
        created_at: now,
    })
}

#[tauri::command]
pub fn toggle_mcp_plugin(
    state: State<'_, DbState>,
    name: String,
    enabled: bool,
) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO mcp_plugins (id, name, command, args, env, capability_card, enabled, created_at)
         VALUES (?1, ?2, '', '[]', '{}', '', ?3, ?4)
         ON CONFLICT(name) DO UPDATE SET enabled = excluded.enabled",
        params![Uuid::new_v4().to_string(), name, if enabled { 1 } else { 0 }, now],
    )
    .map_err(|e| e.to_string())?;

    invalidate_plugin(&name);
    Ok(())
}

#[tauri::command]
pub fn delete_mcp_plugin(state: State<'_, DbState>, name: String) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM mcp_plugins WHERE name = ?1", params![name])
        .map_err(|e| e.to_string())?;

    invalidate_plugin(&name);
    Ok(())
}
