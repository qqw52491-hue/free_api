use crate::agent::mcp::{McpToolDef, PluginConfig};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const CATALOG_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone)]
struct CachedCatalog {
    fingerprint: String,
    tools: Vec<McpToolDef>,
    refreshed_at: Instant,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpCatalogStatus {
    pub name: String,
    pub tool_count: usize,
    pub tools: Vec<McpToolDef>,
    pub cached: bool,
    pub refreshed_at_ms: Option<u128>,
}

static CATALOGS: OnceLock<Mutex<HashMap<String, CachedCatalog>>> = OnceLock::new();

fn catalogs() -> &'static Mutex<HashMap<String, CachedCatalog>> {
    CATALOGS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn fingerprint(config: &PluginConfig) -> String {
    let env = serde_json::to_string(&config.env).unwrap_or_default();
    let headers = serde_json::to_string(&config.headers).unwrap_or_default();
    format!(
        "{:?}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        config.url,
        config.command,
        config.args.join("\u{1f}"),
        headers,
        env
    )
}

pub fn remember_tools(config: &PluginConfig, tools: &[McpToolDef]) {
    let entry = CachedCatalog {
        fingerprint: fingerprint(config),
        tools: tools.to_vec(),
        refreshed_at: Instant::now(),
    };
    if let Ok(mut cache) = catalogs().lock() {
        cache.insert(config.name.clone(), entry);
    }
}

pub fn cached_tools(config: &PluginConfig) -> Option<Vec<McpToolDef>> {
    let cache = catalogs().lock().ok()?;
    let entry = cache.get(&config.name)?;
    if entry.fingerprint != fingerprint(config) || entry.refreshed_at.elapsed() > CATALOG_TTL {
        return None;
    }
    Some(entry.tools.clone())
}

pub fn status(config: &PluginConfig) -> McpCatalogStatus {
    match cached_tools(config) {
        Some(tools) => McpCatalogStatus {
            name: config.name.clone(),
            tool_count: tools.len(),
            tools,
            cached: true,
            refreshed_at_ms: None,
        },
        None => McpCatalogStatus {
            name: config.name.clone(),
            tool_count: 0,
            tools: Vec::new(),
            cached: false,
            refreshed_at_ms: None,
        },
    }
}

pub fn invalidate_plugin(name: &str) {
    if let Ok(mut cache) = catalogs().lock() {
        cache.remove(name);
    }
}

pub fn concise_menu(items: impl IntoIterator<Item = (String, Vec<McpToolDef>)>) -> String {
    let mut lines =
        vec!["\n## MCP 工具目录（摘要缓存；需要参数规格时使用 next_tool_hint）".to_string()];
    let mut sorted_items: Vec<(String, Vec<McpToolDef>)> = items.into_iter().collect();
    // 严格按插件名称字母序排序，消除 HashMap 遍历导致的随机性与前缀缓存击穿
    sorted_items.sort_by(|a, b| a.0.cmp(&b.0));

    for (name, mut tools) in sorted_items {
        // 工具列表也严格按名称字母序排序
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        let tool_names = tools.into_iter().map(|tool| tool.name).collect::<Vec<_>>();
        lines.push(format!("- {}: [{}]", name, tool_names.join(", ")));
    }
    lines.push("- 调用格式: {\"tool\": \"插件名/工具名\", \"command\": {参数对象}}".to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> PluginConfig {
        PluginConfig {
            name: "demo".to_string(),
            command: "demo-server".to_string(),
            args: vec!["--stdio".to_string()],
            url: None,
            headers: HashMap::new(),
            env: HashMap::new(),
        }
    }

    #[test]
    fn cache_reuses_matching_config_and_invalidates_on_change() {
        let mut cfg = config();
        remember_tools(
            &cfg,
            &[McpToolDef {
                name: "search".to_string(),
                description: "search".to_string(),
                schema: json!({}),
            }],
        );
        assert_eq!(cached_tools(&cfg).unwrap().len(), 1);
        cfg.args.push("--changed".to_string());
        assert!(cached_tools(&cfg).is_none());
        invalidate_plugin("demo");
        assert!(cached_tools(&config()).is_none());
    }
}
