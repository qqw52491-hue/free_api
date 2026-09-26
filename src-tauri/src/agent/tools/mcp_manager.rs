use crate::agent::mcp_catalog::invalidate_plugin;
use crate::agent::types::DispatchResult;
use crate::db::DbState;
use reqwest::Url;

/// 确定性 args 脱敏：旗标名（按完整单词）命中 token/secret/key/auth/password 等
/// 敏感词时，其值（`--flag=value` 或下一个独立参数）替换为 `[REDACTED]`；
/// `Bearer xxx` 前缀同理。只按命名约定做确定匹配，不做语义猜测。
/// 返回 (脱敏后的 args, 是否发生了脱敏)。
fn redact_sensitive_args(args: &[serde_json::Value]) -> (Vec<serde_json::Value>, bool) {
    const SENSITIVE_WORDS: [&str; 9] = [
        "token",
        "secret",
        "key",
        "apikey",
        "authorization",
        "auth",
        "password",
        "passwd",
        "credential",
    ];

    let flag_is_sensitive = |flag: &str| -> bool {
        let core = flag.trim_start_matches('-').to_ascii_lowercase();
        core.split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| SENSITIVE_WORDS.contains(&word))
    };

    let mut out = Vec::with_capacity(args.len());
    let mut redacted_any = false;
    let mut expect_value_next = false;

    for arg in args {
        let Some(text) = arg.as_str() else {
            // 非字符串元素原样保留
            expect_value_next = false;
            out.push(arg.clone());
            continue;
        };

        if expect_value_next && !text.starts_with('-') {
            // 上一个是敏感旗标，本参数是它的值
            out.push(serde_json::json!("[REDACTED]"));
            redacted_any = true;
            expect_value_next = false;
            continue;
        }
        expect_value_next = false;

        if text.to_ascii_lowercase().starts_with("bearer ") {
            out.push(serde_json::json!("Bearer [REDACTED]"));
            redacted_any = true;
            continue;
        }

        if let Some((flag, _value)) = text.split_once('=') {
            if flag_is_sensitive(flag) {
                out.push(serde_json::json!(format!("{}=[REDACTED]", flag)));
                redacted_any = true;
                continue;
            }
        } else if text.starts_with('-') && flag_is_sensitive(text) {
            // 独立敏感旗标：值可能在下一个参数
            out.push(arg.clone());
            expect_value_next = true;
            continue;
        }
        out.push(arg.clone());
    }
    (out, redacted_any)
}

pub fn dispatch(db: &DbState, action: &str, params: &serde_json::Value) -> DispatchResult {
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
                Err(e) => return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("获取数据库锁失败: {}", e),
                    success: false,
                    route: "mcp_manager:list".to_string(),
                },
            };
            let mut stmt = match conn.prepare("SELECT id, name, command, args, env, capability_card, enabled, created_at FROM mcp_plugins ORDER BY created_at DESC") {
                Ok(s) => s,
                Err(e) => return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("查询失败: {}", e),
                    success: false,
                    route: "mcp_manager:list".to_string(),
                },
            };
            let rows: Vec<serde_json::Value> = stmt.query_map([], |row| {
                let args_str: String = row.get(3)?;
                let env_str: String = row.get(4)?;
                // 安全边界：env 值可能包含密钥，只投影键名，绝不返回值
                let env_keys: Vec<String> = serde_json::from_str::<serde_json::Value>(&env_str)
                    .ok()
                    .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect()))
                    .unwrap_or_default();
                // args 可能携带 --token xxx 之类的秘密：确定性脱敏后再投影
                let raw_args: Vec<serde_json::Value> =
                    serde_json::from_str::<serde_json::Value>(&args_str)
                        .ok()
                        .and_then(|v| v.as_array().cloned())
                        .unwrap_or_default();
                let (safe_args, args_redacted) = redact_sensitive_args(&raw_args);
                let mut record = serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "command": row.get::<_, String>(2)?,
                    "args": safe_args,
                    "env_keys": env_keys,
                    "capability_card": row.get::<_, String>(5)?,
                    "enabled": row.get::<_, i64>(6)? != 0,
                    "created_at": row.get::<_, String>(7)?,
                });
                if args_redacted {
                    record["args_redacted"] = serde_json::json!(true);
                }
                Ok(record)
            }).ok().map(|iter| iter.flatten().collect()).unwrap_or_default();

            DispatchResult { view: None,
                stdout: serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".to_string()),
                stderr: String::new(),
                success: true,
                route: "mcp_manager:list".to_string(),
            }
        }
        "add_http" | "quick_connect" => {
            let raw_url = params.get("url").or_else(|| params.get("remote_url")).and_then(|v| v.as_str()).unwrap_or("").trim();
            if raw_url.is_empty() {
                return DispatchResult { view: None,
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
            // 当前仅支持 streamable-http 传输；显式拒绝其它值（如 sse），不做静默降级。
            let transport_normalized = transport.trim().to_ascii_lowercase();
            if !transport.is_empty() && transport_normalized != "streamable-http" {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!(
                        "❌ 不支持的传输协议 transport='{}'。当前仅支持 \"streamable-http\"；SSE 暂不支持。",
                        transport
                    ),
                    success: false,
                    route: "mcp_manager:add_http:unsupported_transport".to_string(),
                };
            }
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
                Err(e) => return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("启动客户端失败: {}", e),
                    success: false,
                    route: "mcp_manager:add_http".to_string(),
                },
            };

            let block_res: Result<(usize, Option<String>), String> = (|| {
                let handle = tokio::runtime::Handle::try_current();
                let f = async {
                    client.initialize().await.map_err(|e| format!("初始化握手失败: {}", e))?;
                    let tools = client.list_tools().await.map_err(|e| format!("获取工具失败: {}", e))?;

                    // 模型查找是能力卡的子依赖：找不到模型只影响卡片，不阻断接入
                    let model_cfg: Option<crate::llm::ModelConfig> =
                        if let Some(ref mid) = model_id.filter(|s| !s.trim().is_empty()) {
                            match crate::llm::ModelConfig::from_db_state(db, mid) {
                                Ok(cfg) => Some(cfg),
                                Err(error) => {
                                    eprintln!(
                                        "⚠️ [mcp_manager] 指定模型加载失败，插件 [{}] 能力卡将跳过: {}",
                                        plugin_name, error
                                    );
                                    None
                                }
                            }
                        } else {
                            let model_cfg: Result<Option<crate::llm::ModelConfig>, String> = (|| {
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

                                for id in model_ids {
                                    if let Ok(cfg) = crate::llm::ModelConfig::from_db_state(db, &id) {
                                        return Ok(Some(cfg));
                                    }
                                }
                                Ok(None)
                            })();
                            match model_cfg {
                                Ok(cfg) => cfg,
                                Err(error) => {
                                    eprintln!(
                                        "⚠️ [mcp_manager] 默认模型查找失败，插件 [{}] 能力卡将跳过: {}",
                                        plugin_name, error
                                    );
                                    None
                                }
                            }
                        };

                    // 能力卡是可选子结果：失败不阻断配置入库，但必须显式可见
                    let card = match model_cfg {
                        Some(cfg) => {
                            match crate::agent::mcp_catalog::generate_llm_mcp_card(&plugin_name, &tools, &cfg).await {
                                Ok(card) => Some(card),
                                Err(error) => {
                                    // 只记录错误类别与插件名，不回显 URL/token 等敏感内容
                                    eprintln!(
                                        "⚠️ [mcp_manager] 插件 [{}] 能力卡生成失败: {}",
                                        plugin_name, error
                                    );
                                    None
                                }
                            }
                        }
                        None => {
                            eprintln!(
                                "⚠️ [mcp_manager] 插件 [{}] 无可用模型配置，能力卡未生成",
                                plugin_name
                            );
                            None
                        }
                    };
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
                        Err(e) => return DispatchResult { view: None,
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

                    if let Err(e) = conn.execute(
                        "INSERT INTO mcp_plugins (id, name, command, args, env, capability_card, enabled, created_at, url, headers)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8, ?9)
                         ON CONFLICT(name) DO UPDATE SET command=?3, args=?4, env=?5, capability_card=?6, url=?8, headers=?9",
                        rusqlite::params![id, plugin_name, command, args_json, env_json, card, now, raw_url, headers_json],
                    ) {
                        return DispatchResult { view: None,
                            stdout: String::new(),
                            stderr: format!("插件 [{}] 连接验证成功，但配置写库失败: {}", plugin_name, e),
                            success: false,
                            route: "mcp_manager:add_http:db_error".to_string(),
                        };
                    }
                    invalidate_plugin(&plugin_name);

                    // 配置保存与能力卡是两个子结果：卡片失败时配置仍算成功，但绝不伪造空卡
                    let card_note = match card.as_deref().map(str::trim) {
                        Some(c) if !c.is_empty() => format!("💡 智能能力卡: {}", c),
                        _ => "⚠️ 智能能力卡未生成（连接与配置保存成功；可稍后重试接入或手动填写）".to_string(),
                    };

                    DispatchResult { view: None,
                        stdout: format!("✅ 成功接入 MCP 插件 [{}] (包含 {} 个工具)！\n{}", plugin_name, tool_count, card_note),
                        stderr: String::new(),
                        success: true,
                        route: "mcp_manager:add_http".to_string(),
                    }
                }
                Err(e) => DispatchResult { view: None,
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
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: "❌ 缺少必填参数 name".to_string(),
                    success: false,
                    route: "mcp_manager:delete".to_string(),
                };
            }
            if !confirm {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("⚠️ 警告：删除 MCP 插件 [{}] 将永久移除配置，请再次确认并在 command 中附带 confirm: true", name),
                    success: false,
                    route: "mcp_manager:delete".to_string(),
                };
            }
            let conn = match db.0.lock() {
                Ok(c) => c,
                Err(e) => return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("数据库锁获取失败: {}", e),
                    success: false,
                    route: "mcp_manager:delete".to_string(),
                },
            };
            let affected = conn.execute(
                "DELETE FROM mcp_plugins WHERE name=?1",
                rusqlite::params![name],
            );
            match affected {
                Ok(0) => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: format!("❌ 未找到名为 [{}] 的 MCP 插件；请先用 list 查看现有插件", name),
                        success: false,
                        route: "mcp_manager:delete:not_found".to_string(),
                    };
                }
                Err(e) => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: format!("删除插件 [{}] 写库失败: {}", name, e),
                        success: false,
                        route: "mcp_manager:delete".to_string(),
                    };
                }
                Ok(_) => {}
            }
            invalidate_plugin(name);

            DispatchResult { view: None,
                stdout: format!("✅ 已成功删除 MCP 插件 [{}]（运行中的插件将在下一次任务会话同步时卸载）", name),
                stderr: String::new(),
                success: true,
                route: "mcp_manager:delete".to_string(),
            }
        }
        // 引用式安装：包名/GitHub 地址/本地目录 → 解析+真实握手+落库。
        // 复用 UI「引用安装」同一核心；目录落点用应用数据目录的约定子路径。
        "install_ref" => {
            let reference = params
                .get("reference")
                .or_else(|| params.get("package"))
                .or_else(|| params.get("url"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if reference.is_empty() {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: "❌ 缺少必填参数 reference（包名 / github 仓库地址 / 本地目录路径 / .mcpb 打包文件）".to_string(),
                    success: false,
                    route: "mcp_manager:install_ref:missing_param".to_string(),
                };
            }
            let repos_dir = crate::commands::mcp_ref_install::default_repos_dir();
            let handle = tokio::runtime::Handle::try_current();
            let fut = crate::commands::mcp_ref_install::install_from_ref_core(
                db, reference, &repos_dir,
            );
            // 与 add_http 同模式：spawn_blocking 线程上 Handle::block_on 合法。
            let res = if let Ok(h) = handle {
                h.block_on(fut)
            } else {
                futures::executor::block_on(fut)
            };
            match res {
                Ok(r) => DispatchResult { view: None,
                    stdout: format!(
                        "✅ 已安装 MCP 插件 [{}]（形态: {}，握手实拉 {} 个工具）\n启动命令: {} {}\n将在下一次任务会话同步时生效",
                        r.plugin_name, r.resolved_kind, r.tool_count, r.command, r.args.join(" ")
                    ),
                    stderr: String::new(),
                    success: true,
                    route: "mcp_manager:install_ref".to_string(),
                },
                Err(e) => DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("❌ 引用安装失败: {e}"),
                    success: false,
                    route: "mcp_manager:install_ref:failed".to_string(),
                },
            }
        }
        "toggle" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let enabled = params.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
            let conn = match db.0.lock() {
                Ok(c) => c,
                Err(e) => return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("数据库锁获取失败: {}", e),
                    success: false,
                    route: "mcp_manager:toggle".to_string(),
                },
            };
            let affected = conn.execute(
                "UPDATE mcp_plugins SET enabled=?1 WHERE name=?2",
                rusqlite::params![enabled as i64, name],
            );
            match affected {
                Ok(0) => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: format!("❌ 未找到名为 [{}] 的 MCP 插件；请先用 list 查看现有插件", name),
                        success: false,
                        route: "mcp_manager:toggle:not_found".to_string(),
                    };
                }
                Err(e) => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: format!("切换插件 [{}] 状态写库失败: {}", name, e),
                        success: false,
                        route: "mcp_manager:toggle".to_string(),
                    };
                }
                Ok(_) => {}
            }
            invalidate_plugin(name);

            DispatchResult { view: None,
                stdout: format!("✅ 插件 [{}] 已切换为 {}（将在下一次任务会话同步时生效）", name, if enabled { "启用" } else { "停用" }),
                stderr: String::new(),
                success: true,
                route: "mcp_manager:toggle".to_string(),
            }
        }
        _ => DispatchResult { view: None,
            stdout: String::new(),
            stderr: format!("❌ 未知的 mcp_manager 动作 action='{}'。支持的动作: list, add_http, quick_connect (add_http 的别名), install_ref (包名/GitHub地址/本地目录粘贴即装), toggle, delete", act),
            success: false,
            route: "mcp_manager:unknown".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn test_db() -> DbState {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mcp_plugins (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                command TEXT NOT NULL,
                args TEXT NOT NULL DEFAULT '[]',
                env TEXT NOT NULL DEFAULT '{}',
                capability_card TEXT NOT NULL DEFAULT '',
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                url TEXT NOT NULL DEFAULT '',
                headers TEXT NOT NULL DEFAULT '{}'
            );",
        )
        .unwrap();
        DbState(Arc::new(Mutex::new(conn)))
    }

    fn insert_plugin(db: &DbState, name: &str, env_json: &str) {
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO mcp_plugins (id, name, command, args, env, capability_card, enabled, created_at, url, headers)
             VALUES ('pid-1', ?1, '', '[]', ?2, '', 1, '2026-01-01T00:00:00Z', 'https://example.com/mcp', '{}')",
            rusqlite::params![name, env_json],
        )
        .unwrap();
    }

    #[test]
    fn unknown_action_lists_only_implemented_actions() {
        let db = test_db();
        let result = dispatch(&db, "add_stdio", &json!({}));
        assert!(!result.success);
        // stderr 开头会回显用户传入的 action 名（合理）；
        // 关键是"支持的动作"推荐清单不得包含未实现 action
        let supported_list = result
            .stderr
            .split("支持的动作:")
            .nth(1)
            .unwrap_or_default();
        assert!(!supported_list.contains("add_stdio"));
        assert!(!supported_list.contains("batch_import"));
        assert!(supported_list.contains("list"));
        assert!(supported_list.contains("add_http"));
        assert!(supported_list.contains("quick_connect"));
        assert!(supported_list.contains("install_ref"));
        assert!(supported_list.contains("toggle"));
        assert!(supported_list.contains("delete"));
    }

    #[test]
    fn tool_manual_does_not_advertise_unimplemented_actions() {
        let manual = include_str!("../../../prompts/tools/mcp_manager.md");
        for action in ["add_stdio", "batch_import"] {
            for line in manual.lines() {
                if line.contains(action) {
                    assert!(
                        line.contains("不支持"),
                        "手册把未实现 action {} 声明为受支持动作: {}",
                        action,
                        line
                    );
                }
            }
        }
        assert!(manual.contains("`list`"));
        assert!(manual.contains("`add_http`"));
        assert!(manual.contains("`install_ref`"));
        assert!(manual.contains("`toggle`"));
        assert!(manual.contains("`delete`"));
    }

    #[test]
    fn rejects_unsupported_transport() {
        let db = test_db();
        let result = dispatch(
            &db,
            "add_http",
            &json!({
                "url": "https://example.com/mcp",
                "transport": "sse"
            }),
        );
        assert!(!result.success);
        assert!(result.stderr.contains("不支持"));
        assert!(result.stderr.contains("streamable-http"));
        assert_eq!(result.route, "mcp_manager:add_http:unsupported_transport");
    }

    #[test]
    fn delete_missing_plugin_fails_honestly() {
        let db = test_db();
        let result = dispatch(
            &db,
            "delete",
            &json!({ "name": "ghost-plugin", "confirm": true }),
        );
        assert!(!result.success);
        assert!(result.stderr.contains("未找到"));
        assert_eq!(result.route, "mcp_manager:delete:not_found");
    }

    #[test]
    fn toggle_missing_plugin_fails_honestly() {
        let db = test_db();
        let result = dispatch(
            &db,
            "toggle",
            &json!({ "name": "ghost-plugin", "enabled": false }),
        );
        assert!(!result.success);
        assert!(result.stderr.contains("未找到"));
        assert_eq!(result.route, "mcp_manager:toggle:not_found");
    }

    #[test]
    fn delete_removes_existing_plugin_with_confirm() {
        let db = test_db();
        insert_plugin(&db, "example", "{}");

        let no_confirm = dispatch(&db, "delete", &json!({ "name": "example" }));
        assert!(!no_confirm.success, "缺少 confirm 必须拒绝");

        let confirmed = dispatch(
            &db,
            "delete",
            &json!({ "name": "example", "confirm": true }),
        );
        assert!(confirmed.success);
        let conn = db.0.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM mcp_plugins", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn list_redacts_environment_values() {
        let db = test_db();
        let secret_value = "sk-super-secret-value-987654321";
        insert_plugin(
            &db,
            "example",
            &format!("{{\"API_TOKEN\": \"{}\"}}", secret_value),
        );

        let result = dispatch(&db, "list", &json!({}));
        assert!(result.success);
        assert!(
            !result.stdout.contains(secret_value),
            "env 值绝不能出现在 list 输出中"
        );
        assert!(result.stdout.contains("env_keys"));
        assert!(result.stdout.contains("API_TOKEN"));
        // 旧字段 env（含值）不再投影
        assert!(!result.stdout.contains("\"env\":"));
    }

    #[test]
    fn redact_sensitive_args_masks_flag_values() {
        // 覆盖三种形态：独立旗标 + 下一参数、=value 形式、Bearer 前缀
        let args = vec![
            json!("server-start"),
            json!("--api-key"),
            json!("sk-args-secret-abcdef"),
            json!("--token=inline-secret-123"),
            json!("--verbose"),
            json!("Bearer bearer-secret-xyz"),
            json!("--output"),
            json!("/tmp/result.json"),
        ];
        let (safe_args, redacted) = redact_sensitive_args(&args);

        assert!(redacted);
        let rendered = serde_json::to_string(&safe_args).unwrap();
        assert!(!rendered.contains("sk-args-secret-abcdef"));
        assert!(!rendered.contains("inline-secret-123"));
        assert!(!rendered.contains("bearer-secret-xyz"));
        assert!(rendered.contains("[REDACTED]"));
        // 非敏感旗标与其值必须原样保留
        assert!(rendered.contains("--verbose"));
        assert!(rendered.contains("/tmp/result.json"));
        assert!(rendered.contains("server-start"));
    }

    #[test]
    fn redact_sensitive_args_keeps_unrelated_flags() {
        // 相近词但非敏感完整单词（如 --monkey、--workspace-keyboard）不得误伤
        let args = vec![json!("--monkey"), json!("--workspace-keyboard")];
        let (safe_args, redacted) = redact_sensitive_args(&args);
        assert!(!redacted);
        assert_eq!(safe_args, args);
    }

    #[test]
    fn list_redacts_sensitive_args_values() {
        let db = test_db();
        // 构造含 token 的 args 与 env：两者都不能泄漏到模型可见输出
        let secret_in_args = "sk-args-leak-token-42";
        let env_json = "{\"API_TOKEN\": \"sk-env-leak-token-99\"}".to_string();
        let args_json = format!("[\"--token\", \"{}\"]", secret_in_args);
        {
            let conn = db.0.lock().unwrap();
            conn.execute(
                "INSERT INTO mcp_plugins (id, name, command, args, env, capability_card, enabled, created_at, url, headers)
                 VALUES ('pid-2', 'arg-leak', '', ?1, ?2, '', 1, '2026-01-01T00:00:00Z', 'https://example.com/mcp', '{}')",
                rusqlite::params![args_json, env_json],
            )
            .unwrap();
        }

        let result = dispatch(&db, "list", &json!({}));
        assert!(result.success);
        assert!(
            !result.stdout.contains(secret_in_args),
            "args 中的 token 绝不能出现在 list 输出"
        );
        assert!(!result.stdout.contains("sk-env-leak-token-99"));
        assert!(result.stdout.contains("[REDACTED]"));
        assert!(result.stdout.contains("args_redacted"));
    }
}
