use crate::agent::mcp_catalog::invalidate_plugin;
use crate::agent::types::DispatchResult;
use crate::db::DbState;
use reqwest::Url;

pub fn dispatch(
    db: &DbState,
    action: &str,
    params: &serde_json::Value,
) -> DispatchResult {
    let act = if !action.is_empty() {
        action
    } else {
        params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list")
    };

    match act {
        "list" => {
            let conn = match db.0.lock() {
                Ok(c) => c,
                Err(e) => return DispatchResult {
                    stdout: String::new(),
                    stderr: format!("获取数据库锁失败: {}", e),
                    success: false,
                    route: "mcp_manager:list".to_string(),
                },
            };
            let mut stmt = match conn.prepare("SELECT id, name, command, args, env, capability_card, enabled, created_at FROM mcp_plugins ORDER BY created_at DESC") {
                Ok(s) => s,
                Err(e) => return DispatchResult {
                    stdout: String::new(),
                    stderr: format!("查询失败: {}", e),
                    success: false,
                    route: "mcp_manager:list".to_string(),
                },
            };
            let rows: Vec<serde_json::Value> = stmt.query_map([], |row| {
                let args_str: String = row.get(3)?;
                let env_str: String = row.get(4)?;
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "command": row.get::<_, String>(2)?,
                    "args": serde_json::from_str::<serde_json::Value>(&args_str).unwrap_or(serde_json::json!([])),
                    "env": serde_json::from_str::<serde_json::Value>(&env_str).unwrap_or(serde_json::json!({})),
                    "capability_card": row.get::<_, String>(5)?,
                    "enabled": row.get::<_, i64>(6)? != 0,
                    "created_at": row.get::<_, String>(7)?,
                }))
            }).ok().map(|iter| iter.flatten().collect()).unwrap_or_default();

            DispatchResult {
                stdout: serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".to_string()),
                stderr: String::new(),
                success: true,
                route: "mcp_manager:list".to_string(),
            }
        }
        "add_http" | "quick_connect" => {
            let raw_url = params.get("url").or_else(|| params.get("remote_url")).and_then(|v| v.as_str()).unwrap_or("").trim();
            if raw_url.is_empty() {
                return DispatchResult {
                    stdout: String::new(),
                    stderr: "❌ 缺少必填参数 url。请提供远端 MCP URL (如 https://example.com/mcp)".to_string(),
                    success: false,
                    route: "mcp_manager:add_http".to_string(),
                };
            }
            let token = params.get("token").or_else(|| params.get("bearer_token")).and_then(|v| v.as_str());
            let name = params.get("name").and_then(|v| v.as_str());
            let transport = params.get("transport").or_else(|| params.get("http_transport")).and_then(|v| v.as_str()).unwrap_or("streamable-http");
            let model_id = params.get("model_id").and_then(|v| v.as_str()).map(|s| s.to_string());

            let plugin_name = if let Some(n) = name.filter(|s| !s.trim().is_empty()) {
                n.trim().to_string()
            } else {
                Url::parse(raw_url)
                    .ok()
                    .and_then(|u| u.host_str().map(|h| h.replace(['.', ':'], "-")))
                    .unwrap_or_else(|| "remote-mcp".to_string())
            };

            // 原生 HTTP 直连：不经 uvx/mcp-stdio 桥，消除子进程依赖与 stdio 握手竞态。
            let _ = transport; // McpClient 按 streamable-http 处理，无需桥参数
            let command = String::new();
            let args: Vec<String> = Vec::new();
            let mut headers_map = std::collections::HashMap::new();
            if let Some(t) = token.filter(|s| !s.trim().is_empty()) {
                headers_map.insert("Authorization".to_string(), format!("Bearer {}", t.trim()));
            }
            let env_map = std::collections::HashMap::new();

            let config = crate::agent::mcp::PluginConfig {
                name: plugin_name.clone(),
                command: command.clone(),
                args: args.clone(),
                url: Some(raw_url.to_string()),
                headers: headers_map.clone(),
                env: env_map.clone(),
                capability_card: None,
            };

            let client = match crate::agent::mcp::McpClient::spawn(config) {
                Ok(c) => c,
                Err(e) => return DispatchResult {
                    stdout: String::new(),
                    stderr: format!("启动客户端失败: {}", e),
                    success: false,
                    route: "mcp_manager:add_http".to_string(),
                },
            };

            let block_res: Result<(usize, String), String> = (|| {
                let handle = tokio::runtime::Handle::try_current();
                let f = async {
                    client.initialize().await.map_err(|e| format!("初始化握手失败: {}", e))?;
                    let tools = client.list_tools().await.map_err(|e| format!("获取工具失败: {}", e))?;
                    
                    let model_cfg = if let Some(ref mid) = model_id.filter(|s| !s.trim().is_empty()) {
                        crate::llm::ModelConfig::from_db_state(db, mid)?
                    } else {
                        let conn = db.0.lock().map_err(|e| e.to_string())?;
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
                            if let Ok(cfg) = crate::llm::ModelConfig::from_db_state(db, &id) {
                                found = Some(cfg);
                                break;
                            }
                        }
                        found.ok_or_else(|| "未找到可用的大模型配置".to_string())?
                    };

                    let card = crate::agent::mcp_catalog::generate_llm_mcp_card(&plugin_name, &tools, &model_cfg).await.unwrap_or_default();
                    Ok((tools.len(), card))
                };
                if let Ok(h) = handle {
                    h.block_on(f)
                } else {
                    futures::executor::block_on(f)
                }
            })();

            match block_res {
                Ok((tool_count, card)) => {
                    let conn = match db.0.lock() {
                        Ok(c) => c,
                        Err(e) => return DispatchResult {
                            stdout: String::new(),
                            stderr: format!("数据库锁获取失败: {}", e),
                            success: false,
                            route: "mcp_manager:add_http".to_string(),
                        },
                    };
                    let id = uuid::Uuid::new_v4().to_string();
                    let now = chrono::Utc::now().to_rfc3339();
                    let args_json = serde_json::to_string(&args).unwrap_or("[]".to_string());
                    let env_json = serde_json::to_string(&env_map).unwrap_or("{}".to_string());
                    let headers_json = serde_json::to_string(&headers_map).unwrap_or("{}".to_string());

                    let _ = conn.execute(
                        "INSERT INTO mcp_plugins (id, name, command, args, env, capability_card, enabled, created_at, url, headers)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8, ?9)
                         ON CONFLICT(name) DO UPDATE SET command=?3, args=?4, env=?5, capability_card=?6, url=?8, headers=?9",
                        rusqlite::params![id, plugin_name, command, args_json, env_json, card, now, raw_url, headers_json],
                    );
                    invalidate_plugin(&plugin_name);

                    DispatchResult {
                        stdout: format!("✅ 成功接入 MCP 插件 [{}] (包含 {} 个工具)！\n💡 智能能力卡: {}", plugin_name, tool_count, card),
                        stderr: String::new(),
                        success: true,
                        route: "mcp_manager:add_http".to_string(),
                    }
                }
                Err(e) => DispatchResult {
                    stdout: String::new(),
                    stderr: format!("连接并提炼 MCP 失败: {}", e),
                    success: false,
                    route: "mcp_manager:add_http".to_string(),
                },
            }
        }
        "delete" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let confirm = params.get("confirm").and_then(|v| v.as_bool()).unwrap_or(false);
            if name.is_empty() {
                return DispatchResult {
                    stdout: String::new(),
                    stderr: "❌ 缺少必填参数 name".to_string(),
                    success: false,
                    route: "mcp_manager:delete".to_string(),
                };
            }
            if !confirm {
                return DispatchResult {
                    stdout: String::new(),
                    stderr: format!("⚠️ 警告：删除 MCP 插件 [{}] 将永久移除配置，请再次确认并在 command 中附带 confirm: true", name),
                    success: false,
                    route: "mcp_manager:delete".to_string(),
                };
            }
            let conn = match db.0.lock() {
                Ok(c) => c,
                Err(e) => return DispatchResult {
                    stdout: String::new(),
                    stderr: format!("数据库锁获取失败: {}", e),
                    success: false,
                    route: "mcp_manager:delete".to_string(),
                },
            };
            let _ = conn.execute("DELETE FROM mcp_plugins WHERE name=?1", rusqlite::params![name]);
            invalidate_plugin(name);

            DispatchResult {
                stdout: format!("✅ 已成功删除 MCP 插件 [{}]", name),
                stderr: String::new(),
                success: true,
                route: "mcp_manager:delete".to_string(),
            }
        }
        "toggle" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let enabled = params.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
            let conn = match db.0.lock() {
                Ok(c) => c,
                Err(e) => return DispatchResult {
                    stdout: String::new(),
                    stderr: format!("数据库锁获取失败: {}", e),
                    success: false,
                    route: "mcp_manager:toggle".to_string(),
                },
            };
            let _ = conn.execute("UPDATE mcp_plugins SET enabled=?1 WHERE name=?2", rusqlite::params![enabled as i64, name]);
            invalidate_plugin(name);

            DispatchResult {
                stdout: format!("✅ 插件 [{}] 已切换为 {}", name, if enabled { "启用" } else { "停用" }),
                stderr: String::new(),
                success: true,
                route: "mcp_manager:toggle".to_string(),
            }
        }
        _ => DispatchResult {
            stdout: String::new(),
            stderr: format!("❌ 未知的 mcp_manager 动作 action='{}'。支持的动作: list, add_http, add_stdio, batch_import, toggle, delete", act),
            success: false,
            route: "mcp_manager:unknown".to_string(),
        },
    }
}
