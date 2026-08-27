use crate::agent::mcp::client::McpClient;
use crate::agent::mcp::types::{clean_tool_schema, McpToolDef, PluginConfig};
use crate::task_registry::{TaskKind, TaskRegistry};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// 插件注册来源。DB 对账只作用于 ManagedDb 项，绝不移除 Yaml 项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSource {
    Yaml,
    ManagedDb,
}

/// classify 的判定结果：名字当前被哪种来源占用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisteredKind {
    ManagedDb,
    Yaml,
    /// clients 有项但 sources 缺记录（违反同步约束的防御态）
    Orphan,
}

#[derive(Default, Clone)]
pub struct PluginRegistry {
    pub clients: HashMap<String, Arc<McpClient>>,
    /// 每个已注册插件的来源标记；与 clients 键同步维护。
    pub sources: HashMap<String, PluginSource>,
}

/// 纯对账函数：让运行中的 ManagedDb 注册项与 DB 期望集合一致。
/// desired 的键为 DB 管理插件名，值为 enabled 状态。
/// 返回被移除的插件名列表（供日志/事件使用）。Yaml 来源项永远保留。
pub fn reconcile_managed_plugins(
    clients: &mut HashMap<String, Arc<McpClient>>,
    sources: &mut HashMap<String, PluginSource>,
    desired: &HashMap<String, bool>,
) -> Vec<String> {
    let managed_names: Vec<String> = sources
        .iter()
        .filter(|(_, source)| **source == PluginSource::ManagedDb)
        .map(|(name, _)| name.clone())
        .collect();

    let mut removed = Vec::new();
    for name in managed_names {
        let keep = desired.get(&name).copied().unwrap_or(false);
        if !keep {
            clients.remove(&name);
            sources.remove(&name);
            removed.push(name);
        }
    }
    removed
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            sources: HashMap::new(),
        }
    }

    /// 以指定来源注册插件。与现有 Yaml 项同名时拒绝，避免静默覆盖。
    pub fn insert_with_source(
        &mut self,
        name: String,
        client: Arc<McpClient>,
        source: PluginSource,
    ) -> Result<(), String> {
        if let Some(existing) = self.sources.get(&name) {
            if *existing != source {
                return Err(format!(
                    "插件名 [{}] 已被 {:?} 来源占用；请换一个名称或先移除原插件",
                    name, existing
                ));
            }
        }
        self.clients.insert(name.clone(), client);
        self.sources.insert(name, source);
        Ok(())
    }

    /// 判断某名字在注册表中的当前状态；None 表示全新可注册。
    /// 孤儿指 clients 有项但 sources 缺记录（违反同步约束的防御态）。
    pub fn classify(&self, name: &str) -> Option<RegisteredKind> {
        if !self.clients.contains_key(name) {
            return None;
        }
        match self.sources.get(name) {
            Some(PluginSource::ManagedDb) => Some(RegisteredKind::ManagedDb),
            Some(PluginSource::Yaml) => Some(RegisteredKind::Yaml),
            None => Some(RegisteredKind::Orphan),
        }
    }

    /// 合并另一个 registry（如 YAML 目录扫描结果）。
    /// 同名且来源相同 → 后者覆盖（保持多目录同名旧行为）；
    /// 同名且来源不同 → 显式拒绝并记录，绝不静默覆盖。
    /// 返回成功合并（含覆盖）的插件名列表。
    pub fn merge_registry(&mut self, other: PluginRegistry) -> Vec<String> {
        let names: Vec<String> = other.clients.keys().cloned().collect();
        let mut merged = Vec::new();
        for name in names {
            // load_from_dir 保证 sources 有记录；缺记录时按 Yaml 语义处理
            let source = other
                .sources
                .get(&name)
                .copied()
                .unwrap_or(PluginSource::Yaml);
            let Some(client) = other.clients.get(&name).cloned() else {
                continue;
            };
            match self.insert_with_source(name.clone(), client, source) {
                Ok(()) => merged.push(name),
                Err(error) => println!("⚠️ 跳过合并插件 [{}]: {}", name, error),
            }
        }
        merged
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
                // YAML 内同名后文件覆盖：同一来源内的重复以最后加载为准（保持原行为）
                let _ = registry.insert_with_source(name, client_arc, PluginSource::Yaml);
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
                    if let Ok(card) =
                        crate::agent::mcp_catalog::generate_llm_mcp_card(&plugin_name, &tools, cfg)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造 url 型 McpClient：spawn 只建立 HTTP 客户端结构，不发起任何网络请求。
    fn make_client(name: &str) -> Arc<McpClient> {
        let config = PluginConfig {
            name: name.to_string(),
            command: String::new(),
            args: vec![],
            // 端口 9（discard 协议）确保即使误发起请求也不会命中真实服务
            url: Some("http://localhost:9/mcp".to_string()),
            headers: Default::default(),
            env: Default::default(),
            capability_card: None,
        };
        Arc::new(McpClient::spawn(config).expect("url 型 spawn 不联网，必然成功"))
    }

    fn registry_with(
        entries: &[(&str, PluginSource)],
    ) -> (
        HashMap<String, Arc<McpClient>>,
        HashMap<String, PluginSource>,
    ) {
        let mut clients = HashMap::new();
        let mut sources = HashMap::new();
        for (name, source) in entries {
            let client = make_client(name);
            clients.insert(name.to_string(), client);
            sources.insert(name.to_string(), *source);
        }
        (clients, sources)
    }

    #[test]
    fn reconcile_removes_deleted_managed_plugin() {
        let (mut clients, mut sources) = registry_with(&[
            ("managed-a", PluginSource::ManagedDb),
            ("managed-b", PluginSource::ManagedDb),
        ]);
        let desired = HashMap::from([("managed-a".to_string(), true)]);

        let removed = reconcile_managed_plugins(&mut clients, &mut sources, &desired);

        assert_eq!(removed, vec!["managed-b".to_string()]);
        assert!(clients.contains_key("managed-a"));
        assert!(!clients.contains_key("managed-b"));
        assert!(!sources.contains_key("managed-b"));
    }

    #[test]
    fn reconcile_removes_disabled_managed_plugin() {
        let (mut clients, mut sources) = registry_with(&[("managed-a", PluginSource::ManagedDb)]);
        // desired 里存在但 enabled=false → 必须移除
        let desired = HashMap::from([("managed-a".to_string(), false)]);

        let removed = reconcile_managed_plugins(&mut clients, &mut sources, &desired);

        assert_eq!(removed, vec!["managed-a".to_string()]);
        assert!(clients.is_empty());
        assert!(sources.is_empty());
    }

    #[test]
    fn reconcile_never_removes_yaml_plugin_due_to_db_diff() {
        let (mut clients, mut sources) = registry_with(&[
            ("yaml-x", PluginSource::Yaml),
            ("managed-a", PluginSource::ManagedDb),
        ]);
        // DB 期望集合为空（例如 DB 被清空）：只允许移除 managed 项
        let desired: HashMap<String, bool> = HashMap::new();

        let removed = reconcile_managed_plugins(&mut clients, &mut sources, &desired);

        assert_eq!(removed, vec!["managed-a".to_string()]);
        assert!(
            clients.contains_key("yaml-x"),
            "Yaml 来源项绝不能被 DB 对账移除"
        );
        assert_eq!(sources.get("yaml-x"), Some(&PluginSource::Yaml));
    }

    #[test]
    fn plugin_source_name_collision_is_explicit() {
        let mut registry = PluginRegistry::new();
        registry
            .insert_with_source("dup".to_string(), make_client("dup"), PluginSource::Yaml)
            .expect("首次注册必然成功");

        // 同名不同来源：必须显式拒绝，不能静默覆盖 YAML 插件
        let collision = registry.insert_with_source(
            "dup".to_string(),
            make_client("dup"),
            PluginSource::ManagedDb,
        );
        assert!(collision.is_err(), "跨来源同名注册必须被拒绝");
        assert_eq!(registry.sources.get("dup"), Some(&PluginSource::Yaml));

        // 同来源重复注册（YAML 目录多文件同名）保持"最后加载生效"的原行为
        registry
            .insert_with_source("dup".to_string(), make_client("dup"), PluginSource::Yaml)
            .expect("同来源覆盖是既有行为");
        assert_eq!(registry.sources.get("dup"), Some(&PluginSource::Yaml));
    }

    #[test]
    fn merge_registry_syncs_sources_for_yaml_plugins() {
        // 复现 session_bootstrap 首次扫描路径：YAML 目录 registry 合并进主 registry
        // 必须同时合并 sources，否则 YAML 项无来源标记（对账与冲突判定失效）
        let mut yaml_registry = PluginRegistry::new();
        yaml_registry
            .insert_with_source(
                "yaml-a".to_string(),
                make_client("yaml-a"),
                PluginSource::Yaml,
            )
            .unwrap();

        let mut main_registry = PluginRegistry::new();
        let merged = main_registry.merge_registry(yaml_registry);

        assert_eq!(merged, vec!["yaml-a".to_string()]);
        assert!(main_registry.clients.contains_key("yaml-a"));
        assert_eq!(
            main_registry.sources.get("yaml-a"),
            Some(&PluginSource::Yaml),
            "YAML 插件合并后必须有来源标记"
        );
    }

    #[test]
    fn merge_registry_rejects_cross_source_collision() {
        // 主 registry 已有 ManagedDb 项时，YAML 扫描遇到同名 → 拒绝合并，保留 DB 项
        let mut main_registry = PluginRegistry::new();
        main_registry
            .insert_with_source(
                "dup".to_string(),
                make_client("dup"),
                PluginSource::ManagedDb,
            )
            .unwrap();

        let mut yaml_registry = PluginRegistry::new();
        yaml_registry
            .insert_with_source("dup".to_string(), make_client("dup"), PluginSource::Yaml)
            .unwrap();

        let merged = main_registry.merge_registry(yaml_registry);

        assert!(merged.is_empty(), "跨来源同名必须被拒绝合并");
        assert_eq!(
            main_registry.sources.get("dup"),
            Some(&PluginSource::ManagedDb),
            "已注册的 DB 项不能被 YAML 覆盖"
        );
    }

    #[test]
    fn classify_reports_registration_state() {
        // session_bootstrap 的 DB 挂载决策依据：
        // None=全新可注册 / ManagedDb=保留 / Yaml=拒绝 / Orphan=防御跳过
        let mut registry = PluginRegistry::new();
        assert_eq!(registry.classify("fresh"), None);

        registry
            .insert_with_source(
                "yaml-x".to_string(),
                make_client("yaml-x"),
                PluginSource::Yaml,
            )
            .unwrap();
        registry
            .insert_with_source(
                "db-a".to_string(),
                make_client("db-a"),
                PluginSource::ManagedDb,
            )
            .unwrap();
        // 构造孤儿：直接塞 client 不带来源（模拟违反同步约束的防御态）
        registry
            .clients
            .insert("orphan".to_string(), make_client("orphan"));

        assert_eq!(registry.classify("yaml-x"), Some(RegisteredKind::Yaml));
        assert_eq!(registry.classify("db-a"), Some(RegisteredKind::ManagedDb));
        assert_eq!(registry.classify("orphan"), Some(RegisteredKind::Orphan));
    }
}
