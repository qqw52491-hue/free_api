use crate::agent::execution::runtime::ExecutionConfig;
use crate::agent::mcp::{McpClient, PluginConfig, PluginRegistry};
use crate::agent::mcp_catalog::fingerprint;
use crate::db::DbState;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Serialize, Deserialize)]
struct Grant {
    connection: String,
    tools: Vec<String>,
}

pub(crate) fn configured_tools(db: &DbState, config: &PluginConfig) -> Result<Vec<String>, String> {
    let raw: Option<String> = db
        .reader()?
        .query_row(
            "SELECT review_read_tools FROM mcp_plugins WHERE name=?1 AND enabled=1",
            [&config.name],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(raw
        .and_then(|raw| serde_json::from_str::<Grant>(&raw).ok())
        .filter(|grant| grant.connection == fingerprint(config))
        .map(|grant| grant.tools)
        .unwrap_or_default())
}

pub(crate) fn set_configured_tools(
    db: &DbState,
    config: &PluginConfig,
    mut tools: Vec<String>,
) -> Result<(), String> {
    tools.sort();
    tools.dedup();
    let previous = configured_tools(db, config)?;
    let mut added = tools
        .iter()
        .filter(|name| !previous.contains(name))
        .peekable();
    if added.peek().is_some() {
        let catalog = crate::agent::mcp_catalog::cached_tools(config)
            .ok_or("请先刷新 MCP 连接以获取工具目录。")?;
        for name in added {
            if name.is_empty() || !catalog.iter().any(|tool| tool.name == *name) {
                return Err(format!("MCP 目录中没有可配置的工具 {name:?}。"));
            }
        }
    }
    let raw = serde_json::to_string(&Grant {
        connection: fingerprint(config),
        tools,
    })
    .map_err(|error| error.to_string())?;
    db.writer()?
        .execute(
            "INSERT INTO mcp_plugins (id, name, command, created_at, review_read_tools)
         VALUES (?1, ?2, '', ?3, ?4)
         ON CONFLICT(name) DO UPDATE SET review_read_tools=excluded.review_read_tools",
            params![
                uuid::Uuid::new_v4().to_string(),
                config.name,
                chrono::Utc::now().to_rfc3339(),
                raw
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn definitions(client: &McpClient) -> Vec<crate::agent::mcp::McpToolDef> {
    crate::agent::mcp_catalog::cached_tools(&client.config)
        .unwrap_or_else(|| client.last_known_tools())
}

pub(crate) fn menu(
    db: &DbState,
    registry: &PluginRegistry,
    execution: &ExecutionConfig,
) -> (String, String) {
    if crate::agent::review_tools::forensic_budget() == 0 {
        return Default::default();
    }
    let mut entries = Vec::new();
    for managed in crate::commands::load_managed_mcp_plugins(db).unwrap_or_default() {
        let config = &managed.config;
        let Some(client) = registry.get(&config.name) else {
            continue;
        };
        if !managed.enabled || fingerprint(config) != fingerprint(&client.config) {
            continue;
        }
        let allowed = configured_tools(db, config).unwrap_or_default();
        if allowed.is_empty() {
            continue;
        }
        for tool in definitions(&client) {
            let name = format!("{}/{}", config.name, tool.name);
            if allowed.contains(&tool.name) && execution.permits(&name.to_ascii_lowercase()) {
                entries.push((name, tool.schema));
            }
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    if entries.is_empty() {
        return Default::default();
    }
    let index = entries
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let definitions = entries
        .into_iter()
        .map(|(name, schema)| format!("- {name} 参数 {schema}"))
        .collect::<Vec<_>>()
        .join("\n");
    (
        index,
        format!(
            "\n【已授权的 MCP 取证工具】按工具参数直接调用，也可组合到 parallel。\n{definitions}"
        ),
    )
}

pub(super) async fn resolve(
    db: &DbState,
    registry: &tokio::sync::Mutex<PluginRegistry>,
    name: &str,
) -> Result<(Arc<McpClient>, String), String> {
    let (plugin, tool) = name
        .split_once('/')
        .ok_or("MCP 取证须指定插件名/工具名。")?;
    let client = registry
        .lock()
        .await
        .get(plugin)
        .ok_or_else(|| format!("MCP 插件 {plugin} 当前未连接；本次未执行。"))?;
    let config = crate::commands::mcp::enabled_mcp_config(db, plugin)?;
    if !configured_tools(db, &config)?
        .iter()
        .any(|allowed| allowed == tool)
    {
        return Err(format!("MCP 工具 {name} 未获审查取证授权；本次未执行。"));
    }
    if fingerprint(&config) != fingerprint(&client.config)
        || !definitions(&client).iter().any(|item| item.name == tool)
    {
        return Err(format!(
            "当前 MCP 连接或目录无法确认工具 {name}；本次未执行。"
        ));
    }
    Ok((client, tool.into()))
}

pub(super) async fn call(
    client: &McpClient,
    session: &str,
    tool: &str,
    params: Value,
) -> Result<String, String> {
    let value = client
        .call_tool_for_session(Some(session), tool, params)
        .await?;
    if value["isError"] == true {
        return Err(crate::agent::dispatcher::format_mcp_model_output(&value));
    }
    // Preserve structured and binary evidence in the ordinary review receipt.
    serde_json::to_string(&value).map_err(|error| error.to_string())
}
