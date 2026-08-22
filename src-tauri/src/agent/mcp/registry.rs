use crate::agent::mcp::client::McpClient;
use crate::agent::mcp::types::{clean_tool_schema, McpToolDef, PluginConfig};
use crate::task_registry::{TaskKind, TaskRegistry};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

#[derive(Default, Clone)]
pub struct PluginRegistry {
    pub clients: HashMap<String, Arc<McpClient>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
        }
    }

    pub fn load_from_dir(plugins_dir: &std::path::Path) -> Self {
        let mut registry = Self::new();
        let Ok(entries) = std::fs::read_dir(plugins_dir) else {
            return registry;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml")
                && path.extension().and_then(|e| e.to_str()) != Some("yml")
            {
                continue;
            }
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let config: PluginConfig = match serde_yaml::from_str(&content) {
                Ok(c) => c,
                Err(e) => {
                    println!("加载插件配置失败 [{}]: {}", path.display(), e);
                    continue;
                }
            };
            let name = config.name.clone();
            if let Ok(client) = McpClient::spawn(config) {
                let client_arc = Arc::new(client);
                let c = client_arc.clone();
                let n = name.clone();
                TaskRegistry::global().spawn(
                    TaskKind::McpInit,
                    format!("mcp-init:{}", n),
                    async move {
                        let _ = c.ensure_session_initialized(None).await;
                        if let Ok(tools) = c.list_tools().await {
                            println!("🚀 插件就绪: {} (共 {} 个工具)", n, tools.len());
                        }
                    },
                );
                registry.clients.insert(name, client_arc);
            } else {
                println!("❌ 插件启动失败: {}", name);
            }
        }
        registry
    }

    pub fn get(&self, name: &str) -> Option<Arc<McpClient>> {
        self.clients.get(name).cloned()
    }

    pub async fn prime_tool_catalog_for_session(
        &self,
        chat_session_id: &str,
        model_config: Option<&crate::llm::ModelConfig>,
    ) {
        for client in self.clients.values() {
            let client = client.clone();
            let session_id = chat_session_id.to_string();
            let needs_catalog = crate::agent::mcp_catalog::cached_tools(&client.config)
                .map_or(true, |tools| tools.is_empty());
            if !needs_catalog {
                if let Some(cfg) = model_config {
                    if let Some(tools) = crate::agent::mcp_catalog::cached_tools(&client.config) {
                        let plugin_cfg = client.config.clone();
                        let plugin_name = client.config.name.clone();
                        let cfg_clone = cfg.clone();
                        tokio::spawn(async move {
                            if let Ok(card) = crate::agent::mcp_catalog::generate_llm_mcp_card(
                                &plugin_name,
                                &tools,
                                &cfg_clone,
                            )
                            .await
                            {
                                crate::agent::mcp_catalog::set_cached_capability_card(
                                    &plugin_cfg,
                                    card,
                                );
                            }
                        });
                    }
                }
                continue;
            }

            let plugin_name = client.config.name.clone();
            let plugin_cfg = client.config.clone();
            let m_cfg = model_config.cloned();
            let plugin_name_log = client.config.name.clone();
            let outcome = tokio::time::timeout(Duration::from_secs(8), async move {
                client.ensure_session_initialized(Some(&session_id)).await?;
                let tools = match client.list_tools().await {
                    Ok(tools) => tools,
                    Err(first_error) => {
                        // 吸收握手竞态与瞬时抖动：短退避后重试一次，仍失败才向上报错。
                        tokio::time::sleep(Duration::from_millis(400)).await;
                        client.list_tools().await.map_err(|retry_error| {
                            format!("tools/list 首次失败: {first_error}; 重试失败: {retry_error}")
                        })?
                    }
                };
                if let Some(cfg) = &m_cfg {
                    if let Ok(card) = crate::agent::mcp_catalog::generate_llm_mcp_card(
                        &plugin_name,
                        &tools,
                        cfg,
                    )
                    .await
                    {
                        crate::agent::mcp_catalog::set_cached_capability_card(&plugin_cfg, card);
                    }
                }
                Ok::<(), String>(())
            })
            .await;
            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(error)) => eprintln!(
                    "❌ MCP 工具目录预热失败 [{}]：{}（该插件在模型目录中将显示为空）",
                    plugin_name_log, error
                ),
                Err(_) => eprintln!(
                    "⏰ MCP 工具目录预热超时 [{}]（该插件在模型目录中将显示为空）",
                    plugin_name_log
                ),
            }
        }
    }

    pub fn prewarm_all_for_session(&self, chat_session_id: &str) {
        for client in self.clients.values() {
            if client.config.url.is_some() {
                let c = client.clone();
                let sid = chat_session_id.to_string();
                TaskRegistry::global().spawn(
                    TaskKind::McpSessionInit,
                    format!("mcp-session-init:{}", sid),
                    async move {
                        let _ = c.ensure_session_initialized(Some(&sid)).await;
                    },
                );
            }
        }
    }

    pub async fn ping_plugin(
        &self,
        name: &str,
        chat_session_id: Option<&str>,
    ) -> Result<(), String> {
        if let Some(client) = self.get(name) {
            client.ping_for_session(chat_session_id).await
        } else {
            Err(format!("未找到插件: {}", name))
        }
    }

    pub fn plugin_names(&self) -> Vec<&str> {
        self.clients.keys().map(String::as_str).collect()
    }

    pub fn list_all_tools(&self) -> Vec<(String, McpToolDef)> {
        let mut result = Vec::new();
        for (plugin_name, client) in &self.clients {
            // TTL 过期不等于能力消失：回退到本进程内最近一次成功的 tools/list，
            // 避免长会话中模型重新请求说明书时拿到空目录。
            let tools = crate::agent::mcp_catalog::cached_tools(&client.config)
                .unwrap_or_else(|| client.last_known_tools());
            for tool in tools {
                result.push((plugin_name.clone(), tool));
            }
        }
        result
    }

    pub fn format_tools_menu(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        lines
            .push("\n## 🛠️ MCP 插件箱 (如需参数细节请返回 next_tool_hint: \"插件名\")".to_string());

        let mut catalogs = Vec::new();
        for (name, client) in &self.clients {
            let tools = crate::agent::mcp_catalog::cached_tools(&client.config)
                .unwrap_or_else(|| client.last_known_tools());
            let card = crate::agent::mcp_catalog::cached_capability_card(&client.config);
            catalogs.push((name.clone(), tools, card));
        }
        lines.extend(
            crate::agent::mcp_catalog::concise_menu(catalogs)
                .trim_start_matches(
                    "\n## MCP 工具目录（动态能力卡缓存；如需完整参数规范请使用 next_tool_hint）",
                )
                .lines()
                .map(str::to_string),
        );

        lines.join("\n")
    }

    pub fn format_tool_detail(&self, requested_name: &str) -> String {
        let all = self.list_all_tools();
        let (target_plugin, target_tool) = if requested_name.contains('/') {
            let parts: Vec<&str> = requested_name.split('/').collect();
            (parts[0], Some(parts[1]))
        } else {
            (requested_name, None)
        };

        let matched: Vec<_> = all
            .iter()
            .filter(|(p, t)| {
                if p != target_plugin {
                    return false;
                }
                if let Some(tool_name) = target_tool {
                    t.name == tool_name
                } else {
                    true
                }
            })
            .collect();

        if matched.is_empty() {
            return format!("\n❌ 未找到工具/插件 [{}] 的说明书。", requested_name);
        }

        let mut detail = format!(
            "\n## 📦 [{}] 工具规格 (严格遵守 JSON Schema)\n",
            requested_name
        );
        if target_tool.is_none() && matched.len() > 8 {
            detail.push_str(&format!(
                "该插件共包含 {} 个工具。在调用具体动作（如 {}/{}）时系统将自动精确提供该工具的参数 Schema：\n",
                matched.len(),
                target_plugin,
                matched.first().map(|(_, t)| t.name.as_str()).unwrap_or("tool_name")
            ));
            for (plugin, tool) in &matched {
                let desc = tool.description.lines().next().unwrap_or("").trim();
                detail.push_str(&format!("- {}/{}: {}\n", plugin, tool.name, desc));
            }
        } else {
            for (plugin, tool) in &matched {
                let compact_schema = clean_tool_schema(&tool.schema);
                detail.push_str(&format!(
                    "### {}/{}\n- 描述: {}\n- 参数: {}\n",
                    plugin,
                    tool.name,
                    tool.description.trim(),
                    compact_schema
                ));
            }
        }
        detail
    }
}
