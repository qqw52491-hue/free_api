use crate::agent::mcp::{McpClient, PluginConfig};
use crate::agent::mcp_catalog::{
    invalidate_plugin, remember_tools, status as catalog_status, McpCatalogStatus,
};
use crate::agent::task_store;
use crate::db::{ChatMessage, ChatSession, DbState, Model, Platform};
use crate::llm::{self, ModelConfig, StreamEvent};
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

// ==================== MCP PLUGIN COMMANDS ====================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPluginRow {
    pub id: String,
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: std::collections::HashMap<String, String>,
    pub enabled: bool,
    pub created_at: String,
}

/// Agent Runtime 直接读取的插件配置。SQLite 中的 enabled 标志优先于同名 YAML。
#[derive(Debug, Clone)]
pub struct ManagedMcpPlugin {
    pub config: PluginConfig,
    pub enabled: bool,
}

pub fn load_managed_mcp_plugins(db: &DbState) -> Result<Vec<ManagedMcpPlugin>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT name, command, args, env, enabled FROM mcp_plugins ORDER BY created_at ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let args_json: String = row.get(2)?;
            let env_json: String = row.get(3)?;
            Ok(ManagedMcpPlugin {
                config: PluginConfig {
                    name: row.get(0)?,
                    command: row.get(1)?,
                    args: serde_json::from_str(&args_json).unwrap_or_default(),
                    env: serde_json::from_str(&env_json).unwrap_or_default(),
                },
                enabled: row.get::<_, i64>(4)? == 1,
            })
        })
        .map_err(|e| e.to_string())?;
    let plugins = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(plugins)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCustomInstructionRow {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub description: String,
    pub content: String,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

const MAX_CUSTOM_INSTRUCTION_CHARS: usize = 12_000;
const MAX_CUSTOM_INSTRUCTIONS_IN_PROMPT: usize = 24_000;

fn normalized_custom_instruction(
    kind: &str,
    name: &str,
    description: Option<&str>,
    content: &str,
) -> Result<(String, String, String, String), String> {
    let kind = kind.trim().to_ascii_lowercase();
    if kind != "prompt" && kind != "skill" {
        return Err("类型只能是 prompt 或 skill".to_string());
    }
    let name = name.trim().to_string();
    if name.is_empty() || name.chars().count() > 80 {
        return Err("名称不能为空且最多 80 个字符".to_string());
    }
    let description = description.unwrap_or_default().trim().to_string();
    if description.chars().count() > 240 {
        return Err("说明最多 240 个字符".to_string());
    }
    let content = content.trim().to_string();
    if content.is_empty() || content.chars().count() > MAX_CUSTOM_INSTRUCTION_CHARS {
        return Err(format!(
            "内容不能为空且最多 {} 个字符",
            MAX_CUSTOM_INSTRUCTION_CHARS
        ));
    }
    Ok((kind, name, description, content))
}

/// 已启用的用户条目只作为受控补充上下文，不替代内置系统约束、工具契约或当前任务。
pub fn load_enabled_custom_instruction_context(db: &DbState) -> Result<String, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT kind, name, description, content FROM agent_custom_instructions WHERE enabled = 1 ORDER BY updated_at ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut remaining = MAX_CUSTOM_INSTRUCTIONS_IN_PROMPT;
    let mut sections = Vec::new();
    for row in rows {
        if remaining == 0 {
            break;
        }
        let (kind, name, description, content) = row.map_err(|e| e.to_string())?;
        let label = if kind == "skill" {
            "自定义 Skill"
        } else {
            "自定义提示词"
        };
        let mut section = format!("### {}：{}", label, name);
        if !description.trim().is_empty() {
            section.push_str(&format!("\n{}", description.trim()));
        }
        section.push_str(&format!("\n{}", content.trim()));
        let clipped = section.chars().take(remaining).collect::<String>();
        remaining = remaining.saturating_sub(clipped.chars().count());
        sections.push(clipped);
    }

    if sections.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!(
            "\n## 用户自定义提示词与 Skill（仅作补充；不得覆盖内置安全规则、工具契约或用户当前任务）\n{}\n",
            sections.join("\n\n")
        ))
    }
}

#[tauri::command]
pub fn get_agent_custom_instructions(
    state: State<'_, DbState>,
) -> Result<Vec<AgentCustomInstructionRow>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, kind, name, description, content, enabled, created_at, updated_at FROM agent_custom_instructions ORDER BY updated_at DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(AgentCustomInstructionRow {
                id: row.get(0)?,
                kind: row.get(1)?,
                name: row.get(2)?,
                description: row.get(3)?,
                content: row.get(4)?,
                enabled: row.get::<_, i64>(5)? == 1,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let instructions = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(instructions)
}

#[tauri::command]
pub fn save_agent_custom_instruction(
    state: State<'_, DbState>,
    id: Option<String>,
    kind: String,
    name: String,
    description: Option<String>,
    content: String,
    enabled: bool,
) -> Result<AgentCustomInstructionRow, String> {
    let (kind, name, description, content) =
        normalized_custom_instruction(&kind, &name, description.as_deref(), &content)?;
    let id = id
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let now = Utc::now().to_rfc3339();
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO agent_custom_instructions (id, kind, name, description, content, enabled, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
             kind = excluded.kind, name = excluded.name, description = excluded.description,
             content = excluded.content, enabled = excluded.enabled, updated_at = excluded.updated_at",
        params![id, kind, name, description, content, if enabled { 1 } else { 0 }, now, now],
    )
    .map_err(|e| e.to_string())?;
    conn.query_row(
        "SELECT id, kind, name, description, content, enabled, created_at, updated_at FROM agent_custom_instructions WHERE id = ?1",
        params![id],
        |row| {
            Ok(AgentCustomInstructionRow {
                id: row.get(0)?, kind: row.get(1)?, name: row.get(2)?, description: row.get(3)?,
                content: row.get(4)?, enabled: row.get::<_, i64>(5)? == 1, created_at: row.get(6)?, updated_at: row.get(7)?,
            })
        },
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn toggle_agent_custom_instruction(
    state: State<'_, DbState>,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let changed = conn
        .execute(
            "UPDATE agent_custom_instructions SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
            params![if enabled { 1 } else { 0 }, Utc::now().to_rfc3339(), id],
        )
        .map_err(|e| e.to_string())?;
    if changed == 0 {
        return Err("未找到自定义提示词或 Skill".to_string());
    }
    Ok(())
}

#[tauri::command]
pub fn delete_agent_custom_instruction(
    state: State<'_, DbState>,
    id: String,
) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let changed = conn
        .execute(
            "DELETE FROM agent_custom_instructions WHERE id = ?1",
            params![id],
        )
        .map_err(|e| e.to_string())?;
    if changed == 0 {
        return Err("未找到自定义提示词或 Skill".to_string());
    }
    Ok(())
}

/// 列出当前 session 的任务资源目录。资源记录本体不会混入 Agent prompt。
#[tauri::command]
pub async fn get_agent_task_resources(
    state: State<'_, DbState>,
    session_id: String,
) -> Result<Vec<task_store::TaskResourceRef>, String> {
    task_store::list_resource_refs(state.inner(), &session_id)
}

/// 读取 Agent 自己生成的任务总结 Markdown 正文。路径必须位于 app_data/task_summaries 下。
#[tauri::command]
pub fn read_agent_summary_markdown(app: AppHandle, path: String) -> Result<String, String> {
    let summaries_root = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法定位应用数据目录: {}", error))?
        .join("task_summaries");
    let canonical_root = summaries_root
        .canonicalize()
        .map_err(|_| "任务总结目录不存在或尚未生成总结".to_string())?;
    let requested = std::path::PathBuf::from(path)
        .canonicalize()
        .map_err(|_| "任务总结文件不存在".to_string())?;
    if requested
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("md")
        || !requested.starts_with(&canonical_root)
    {
        return Err("只允许读取 Agent 自动生成的任务总结 Markdown".to_string());
    }
    std::fs::read_to_string(&requested).map_err(|error| format!("无法读取任务总结: {}", error))
}
/// 返回指定会话全部已落盘的运行过程树。该数据仅用于历史复盘，不影响运行态恢复。
#[tauri::command]
pub async fn get_session_run_traces(
    state: State<'_, DbState>,
    session_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    crate::agent::run_trace::list_session_traces(state.inner(), &session_id)
}
/// 分页读取当前 session 任务资源中的记录；底层会执行资源归属校验和 100 条上限。
#[tauri::command]
pub async fn get_agent_task_records(
    state: State<'_, DbState>,
    session_id: String,
    resource_id: String,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    task_store::list_records_page(state.inner(), &session_id, &resource_id, offset, limit)
}

#[tauri::command]
pub async fn get_mcp_plugins(state: State<'_, DbState>) -> Result<Vec<McpPluginRow>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare(
        "SELECT id, name, command, args, env, enabled, created_at FROM mcp_plugins ORDER BY created_at DESC"
    ).map_err(|e| e.to_string())?;
    let list = stmt
        .query_map([], |row| {
            let args_str: String = row.get(3)?;
            let env_str: String = row.get(4)?;
            Ok(McpPluginRow {
                id: row.get(0)?,
                name: row.get(1)?,
                command: row.get(2)?,
                args: serde_json::from_str(&args_str).unwrap_or_default(),
                env: serde_json::from_str(&env_str).unwrap_or_default(),
                enabled: row.get::<_, i64>(5)? == 1,
                created_at: row.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(list)
}

fn enabled_mcp_config(db: &DbState, name: &str) -> Result<PluginConfig, String> {
    load_managed_mcp_plugins(db)?
        .into_iter()
        .find(|plugin| plugin.enabled && plugin.config.name == name)
        .map(|plugin| plugin.config)
        .ok_or_else(|| format!("未找到已启用的 MCP 插件: {}", name))
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

/// 显式验证 MCP stdio 连接并强制重新发现工具。成功后目录缓存立即更新，
/// 下一次 Agent 运行会使用摘要菜单而非重复读取全部 schema。
#[tauri::command]
pub async fn refresh_mcp_plugin_connection(
    state: State<'_, DbState>,
    name: String,
) -> Result<McpCatalogStatus, String> {
    let config = enabled_mcp_config(state.inner(), &name)?;
    invalidate_plugin(&name);
    let mut client = McpClient::spawn(config.clone())?;
    client.initialize()?;
    let tools = client.list_tools()?;
    remember_tools(&config, &tools);
    Ok(McpCatalogStatus {
        name,
        tool_count: tools.len(),
        tools,
        cached: true,
        refreshed_at_ms: Some(Utc::now().timestamp_millis() as u128),
    })
}

#[tauri::command]
pub async fn save_mcp_plugin(
    state: State<'_, DbState>,
    name: String,
    command: String,
    args: Vec<String>,
    env: Option<std::collections::HashMap<String, String>>,
) -> Result<McpPluginRow, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let args_json = serde_json::to_string(&args).unwrap_or("[]".to_string());
    let env_map = env.unwrap_or_default();
    let env_json = serde_json::to_string(&env_map).unwrap_or("{}".to_string());

    // UPSERT: 如果 name 已存在就更新
    conn.execute(
        "INSERT INTO mcp_plugins (id, name, command, args, env, enabled, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)
         ON CONFLICT(name) DO UPDATE SET command=?3, args=?4, env=?5",
        params![id, name, command, args_json, env_json, now],
    )
    .map_err(|e| e.to_string())?;

    // UI 管理的配置写入本地忽略文件，避免把 bridge token 写进可提交的 YAML。
    let plugins_dir = std::env::current_dir().unwrap_or_default().join("plugins");
    let _ = std::fs::create_dir_all(&plugins_dir);
    let config = PluginConfig {
        name: name.clone(),
        command: command.clone(),
        args: args.clone(),
        env: env_map.clone(),
    };
    let yaml = serde_yaml::to_string(&config).unwrap_or_default();
    let _ = std::fs::write(plugins_dir.join(format!("{}.local.yaml", name)), yaml);
    invalidate_plugin(&name);

    Ok(McpPluginRow {
        id,
        name,
        command,
        args,
        env: env_map,
        enabled: true,
        created_at: now,
    })
}

#[tauri::command]
pub async fn delete_mcp_plugin(state: State<'_, DbState>, name: String) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM mcp_plugins WHERE name=?1", params![&name])
        .map_err(|e| e.to_string())?;

    // 仅删 UI 写入的本地配置；项目自带 YAML 不由 UI 删除。
    let plugins_dir = std::env::current_dir().unwrap_or_default().join("plugins");
    let yaml_path = plugins_dir.join(format!("{}.local.yaml", name));
    if yaml_path.exists() {
        let _ = std::fs::remove_file(yaml_path);
    }

    invalidate_plugin(&name);
    Ok(())
}

#[tauri::command]
pub async fn toggle_mcp_plugin(
    state: State<'_, DbState>,
    name: String,
    enabled: bool,
) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE mcp_plugins SET enabled=?1 WHERE name=?2",
        params![enabled as i64, &name],
    )
    .map_err(|e| e.to_string())?;
    invalidate_plugin(&name);
    Ok(())
}

// ==================== PLATFORM COMMANDS ====================

#[tauri::command]
pub async fn get_platforms(state: State<'_, DbState>) -> Result<Vec<Platform>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare(
        "SELECT id, name, base_url, api_key, created_at FROM platforms ORDER BY created_at DESC"
    ).map_err(|e| e.to_string())?;
    let list = stmt
        .query_map([], |row| {
            Ok(Platform {
                id: row.get(0)?,
                name: row.get(1)?,
                base_url: row.get(2)?,
                api_key: row.get(3)?,
                created_at: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(list)
}

#[tauri::command]
pub async fn add_platform(
    state: State<'_, DbState>,
    name: String,
    base_url: String,
    api_key: String,
) -> Result<Platform, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO platforms (id, name, base_url, api_key, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, name, base_url, api_key, now],
    ).map_err(|e| e.to_string())?;
    Ok(Platform {
        id,
        name,
        base_url,
        api_key,
        created_at: now,
    })
}

#[tauri::command]
pub async fn update_platform(
    state: State<'_, DbState>,
    id: String,
    name: String,
    base_url: String,
    api_key: String,
) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE platforms SET name=?1, base_url=?2, api_key=?3 WHERE id=?4",
        params![name, base_url, api_key, id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn delete_platform(state: State<'_, DbState>, id: String) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM platforms WHERE id=?1", params![id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ==================== MODEL COMMANDS ====================

#[tauri::command]
pub async fn get_models(
    state: State<'_, DbState>,
    platform_id: Option<String>,
) -> Result<Vec<Model>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let (query, p): (String, Vec<String>) = if let Some(pid) = platform_id {
        ("SELECT id, platform_id, name, display_name, max_tokens, temperature, enabled, status, latency_ms, context_window FROM models WHERE platform_id = ?1 ORDER BY created_at DESC".into(), vec![pid])
    } else {
        ("SELECT id, platform_id, name, display_name, max_tokens, temperature, enabled, status, latency_ms, context_window FROM models ORDER BY created_at DESC".into(), vec![])
    };
    let mut stmt = conn.prepare(&query).map_err(|e| e.to_string())?;
    let list = stmt
        .query_map(rusqlite::params_from_iter(p), |row| {
            Ok(Model {
                id: row.get(0)?,
                platform_id: row.get(1)?,
                name: row.get(2)?,
                display_name: row.get(3)?,
                max_tokens: row.get(4)?,
                temperature: row.get(5)?,
                enabled: row.get::<_, i64>(6)? == 1,
                status: row.get(7)?,
                latency_ms: row.get(8)?,
                context_window: row.get(9)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(list)
}

/// 获取所有模型 + 携带平台信息（用于对话页面的选择器）
#[tauri::command]
pub async fn get_all_models_with_platform(
    state: State<'_, DbState>,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT m.id, m.name, m.display_name, m.platform_id, p.name as platform_name, m.enabled
         FROM models m JOIN platforms p ON p.id = m.platform_id
         WHERE m.enabled = 1
         ORDER BY p.name, m.name",
        )
        .map_err(|e| e.to_string())?;
    let list = stmt
        .query_map([], |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "display_name": row.get::<_, String>(2)?,
                "platform_id": row.get::<_, String>(3)?,
                "platform_name": row.get::<_, String>(4)?,
                "enabled": row.get::<_, i64>(5)? == 1,
            }))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(list)
}

#[tauri::command]
pub async fn add_model(
    state: State<'_, DbState>,
    platform_id: String,
    name: String,
    display_name: String,
) -> Result<Model, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO models (id, platform_id, name, display_name, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, platform_id, name, display_name, now],
    ).map_err(|e| e.to_string())?;
    Ok(Model {
        id,
        platform_id,
        name,
        display_name,
        max_tokens: 4096,
        temperature: 0.7,
        enabled: true,
        status: "unknown".to_string(),
        latency_ms: 0,
        context_window: 0,
    })
}

/// 更新模型参数（max_tokens 输出上限、context_window 上下文窗口）
#[tauri::command]
pub async fn update_model(
    state: State<'_, DbState>,
    id: String,
    max_tokens: Option<i64>,
    context_window: Option<i64>,
    display_name: Option<String>,
) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    if let Some(v) = max_tokens {
        conn.execute(
            "UPDATE models SET max_tokens = ?1 WHERE id = ?2",
            params![v, id],
        ).map_err(|e| e.to_string())?;
    }
    if let Some(v) = context_window {
        conn.execute(
            "UPDATE models SET context_window = ?1 WHERE id = ?2",
            params![v, id],
        ).map_err(|e| e.to_string())?;
    }
    if let Some(v) = display_name {
        conn.execute(
            "UPDATE models SET display_name = ?1 WHERE id = ?2",
            params![v, id],
        ).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn delete_model(state: State<'_, DbState>, id: String) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM models WHERE id=?1", params![id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn test_model(
    state: State<'_, DbState>,
    model_id: String,
) -> Result<serde_json::Value, String> {
    let (platform, model_name) = {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn.prepare(
            "SELECT p.base_url, p.api_key, m.name FROM models m JOIN platforms p ON p.id = m.platform_id WHERE m.id = ?1"
        ).map_err(|e| e.to_string())?;
        stmt.query_row(params![model_id], |row| {
            Ok((
                (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?
    };

    let start = std::time::Instant::now();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;

    let clean_base = platform.0.trim().trim_end_matches('/');
    let clean_key = platform.1.trim();
    let auth_header = if clean_key.starts_with("Bearer ") {
        clean_key.to_string()
    } else {
        format!("Bearer {}", clean_key)
    };

    let url = format!("{}/chat/completions", clean_base);
    let body = json!({
        "model": model_name,
        "messages": [{"role": "user", "content": "Hi"}],
        "max_tokens": 16
    });

    let resp = client
        .post(&url)
        .header("Authorization", &auth_header)
        .header("HTTP-Referer", "https://free-api-chat.app")
        .header("X-OpenRouter-Title", "Free API Chat")
        .json(&body)
        .send()
        .await;

    let latency = start.elapsed().as_millis() as i64;
    let (status, msg) = match resp {
        Ok(r) if r.status().is_success() => ("online", "连接正常".to_string()),
        Ok(r) => {
            let s = r.status();
            let body = r.text().await.unwrap_or_default();
            (
                "offline",
                format!(
                    "HTTP {} - {}",
                    s,
                    body.chars().take(200).collect::<String>()
                ),
            )
        }
        Err(e) => ("offline", e.to_string()),
    };

    // 更新数据库
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE models SET status=?1, latency_ms=?2 WHERE id=?3",
        params![status, latency, model_id],
    )
    .map_err(|e| e.to_string())?;

    Ok(json!({ "status": status, "latency_ms": latency, "message": msg }))
}

// ==================== SESSION COMMANDS ====================

#[tauri::command]
pub async fn get_sessions(state: State<'_, DbState>) -> Result<Vec<ChatSession>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare(
        "SELECT id, title, model_id, created_at, updated_at, message_count FROM chat_sessions ORDER BY updated_at DESC LIMIT 100"
    ).map_err(|e| e.to_string())?;
    let list = stmt
        .query_map([], |row| {
            Ok(ChatSession {
                id: row.get(0)?,
                title: row.get(1)?,
                model_id: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
                message_count: row.get(5)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(list)
}

#[tauri::command]
pub async fn create_session(
    state: State<'_, DbState>,
    title: String,
    model_id: String,
) -> Result<ChatSession, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO chat_sessions (id, title, model_id, created_at, updated_at, message_count) VALUES (?1, ?2, ?3, ?4, ?5, 0)",
        params![id, title, model_id, now, now],
    ).map_err(|e| e.to_string())?;
    Ok(ChatSession {
        id,
        title,
        model_id,
        created_at: now.clone(),
        updated_at: now,
        message_count: 0,
    })
}

#[tauri::command]
pub async fn delete_session(state: State<'_, DbState>, id: String) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    // 任务数据区允许临时 session，因此由会话删除流程显式清理。
    conn.execute(
        "DELETE FROM agent_task_resources WHERE session_id=?1",
        params![&id],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "DELETE FROM agent_checkpoints WHERE run_id=?1",
        params![&id],
    )
    .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM chat_sessions WHERE id=?1", params![id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn rename_session(
    state: State<'_, DbState>,
    id: String,
    title: String,
) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE chat_sessions SET title=?1 WHERE id=?2",
        params![title, id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

// ==================== MESSAGE COMMANDS ====================

#[tauri::command]
pub async fn get_messages(
    state: State<'_, DbState>,
    session_id: String,
) -> Result<Vec<ChatMessage>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare(
        "SELECT id, session_id, role, content, attachments, created_at, model_id FROM chat_messages WHERE session_id=?1 ORDER BY created_at ASC"
    ).map_err(|e| e.to_string())?;
    let list = stmt
        .query_map(params![session_id], |row| {
            Ok(ChatMessage {
                id: row.get(0)?,
                session_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                attachments: row.get(4)?,
                created_at: row.get(5)?,
                model_id: row.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(list)
}

#[tauri::command]
pub async fn save_message(
    state: State<'_, DbState>,
    session_id: String,
    role: String,
    content: String,
    attachments: Option<String>,
    model_id: Option<String>,
) -> Result<ChatMessage, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO chat_messages (id, session_id, role, content, attachments, created_at, model_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, session_id, role, content, attachments, now, model_id],
    ).map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE chat_sessions SET updated_at=?1, message_count=message_count+1 WHERE id=?2",
        params![now, session_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(ChatMessage {
        id,
        session_id,
        role,
        content,
        attachments,
        created_at: now,
        model_id,
    })
}

// ==================== STREAM CHAT ====================

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AttachmentInfo {
    pub name: String,
    pub mime_type: String,
    pub data_base64: String,
}

#[tauri::command]
pub async fn send_chat(
    app: AppHandle,
    state: State<'_, DbState>,
    session_id: String,
    model_id: String,
    messages: Vec<serde_json::Value>,
    attachments: Option<Vec<AttachmentInfo>>,
) -> Result<String, String> {
    let config = ModelConfig::from_db(&state, &model_id)?;

    // 处理附件/图片
    let mut api_messages = messages.clone();
    if let Some(ref files) = attachments {
        if !files.is_empty() {
            if let Some(last) = api_messages.last_mut() {
                if last.get("role").and_then(|r| r.as_str()) == Some("user") {
                    let text = last["content"].as_str().unwrap_or("").to_string();
                    let mut parts: Vec<serde_json::Value> =
                        vec![json!({"type": "text", "text": text})];
                    for file in files {
                        if file.mime_type.starts_with("image/") {
                            parts.push(json!({
                                "type": "image_url",
                                "image_url": {
                                    "url": format!("data:{};base64,{}", file.mime_type, file.data_base64)
                                }
                            }));
                        }
                    }
                    *last = json!({"role": "user", "content": parts});
                }
            }
        }
    }

    // 实时事件回调
    let mut is_thinking = false;
    let mut on_event = |event: StreamEvent| match event {
        StreamEvent::Thinking {
            chunk,
            accumulated: _,
        } => {
            if !is_thinking {
                is_thinking = true;
                app.emit(
                    "chat-thinking",
                    json!({"session_id": session_id, "status": "start"}),
                )
                .ok();
            }
            app.emit(
                "chat-thinking",
                json!({
                    "session_id": session_id,
                    "content": chunk,
                    "status": "streaming"
                }),
            )
            .ok();
        }
        StreamEvent::Content {
            chunk,
            full_thinking,
            ..
        } => {
            if is_thinking {
                is_thinking = false;
                app.emit(
                    "chat-thinking",
                    json!({
                        "session_id": session_id,
                        "status": "done",
                        "full_thinking": full_thinking
                    }),
                )
                .ok();
            }
            app.emit(
                "chat-stream",
                json!({
                    "session_id": session_id,
                    "content": chunk,
                    "done": false
                }),
            )
            .ok();
        }
    };

    let (_interrupt_tx, mut interrupt_rx) = tokio::sync::watch::channel(0u64);
    let result = llm::stream_once(
        &config,
        &api_messages,
        "high",
        &mut interrupt_rx,
        &mut on_event,
    )
    .await?;

    // 思考结束通知
    if is_thinking && !result.thinking_text.is_empty() {
        app.emit(
            "chat-thinking",
            json!({
                "session_id": session_id,
                "status": "done",
                "full_thinking": &result.thinking_text
            }),
        )
        .ok();
    }

    if !result.thinking_text.is_empty() {
        println!(
            "🧠 思考过程 ({} 字符): {}...",
            result.thinking_text.len(),
            result.thinking_text.chars().take(200).collect::<String>()
        );
    }

    // 流结束通知
    app.emit(
        "chat-stream",
        json!({
            "session_id": session_id,
            "content": "",
            "done": true,
            "thinking": if result.thinking_text.is_empty() { None } else { Some(&result.thinking_text) }
        }),
    )
    .ok();

    // Token 用量统计
    let raw_usage = result.usage.unwrap_or_else(|| {
        let (p, c) = llm::estimate_usage(
            &api_messages,
            result.full_response.len(),
            config.context_window,
        );
        llm::RawUsage {
            prompt_tokens: p,
            completion_tokens: c,
            cached_tokens: 0,
        }
    });
    let final_prompt = raw_usage.prompt_tokens;
    let final_completion = raw_usage.completion_tokens;
    let final_cached = raw_usage.cached_tokens;
    let final_total = final_prompt + final_completion;
    let final_percent = if config.context_window > 0 {
        (final_total as f64 / config.context_window as f64) * 100.0
    } else {
        0.0
    };
    let cache_hit_rate = if final_prompt > 0 {
        (final_cached as f64 / final_prompt as f64) * 100.0
    } else {
        0.0
    };

    app.emit(
        "chat-token-usage",
        json!({
            "session_id": session_id,
            "prompt_tokens": final_prompt,
            "completion_tokens": final_completion,
            "total_tokens": final_total,
            "cached_tokens": final_cached,
            "cache_hit_rate": cache_hit_rate,
            "context_window": config.context_window,
            "usage_percent": final_percent,
            "ttft_ms": result.ttft_ms,
            "total_duration_ms": result.total_duration_ms,
            "tokens_per_second": result.tokens_per_second
        }),
    )
    .ok();

    println!(
        "📊 Chat Token: 输入={}, 输出={}, 合计={} | 上下文: {}/{} ({:.1}%)",
        final_prompt,
        final_completion,
        final_total,
        final_total,
        config.context_window,
        final_percent
    );

    Ok(result.full_response)
}

/// 微信式原生独立 WebView 窗口预览
/// 在桌面端创建一个独立的 WebviewWindow，无视任何 X-Frame-Options/CSP 限制（Google、B站、GitHub 均可直接完整浏览交互）
#[tauri::command]
pub async fn open_in_webview_window(
    app: tauri::AppHandle,
    url: String,
    title: Option<String>,
) -> Result<(), String> {
    use tauri::{WebviewUrl, WebviewWindowBuilder};

    let parsed_url = tauri::Url::parse(&url).map_err(|e| format!("无效 URL: {}", e))?;
    let short_uuid = uuid::Uuid::new_v4().to_string().replace('-', "")[..12].to_string();
    let window_label = format!("preview-{}", short_uuid);
    let window_title = title.unwrap_or_else(|| "网页预览".to_string());

    let init_script = r#"
        (function() {
            // 修复 Webview 中 target="_blank" 和 window.open 点不动的问题（百度/Google/B站 搜索结果默认都是 _blank）
            document.addEventListener('click', function(e) {
                var a = e.target.closest('a');
                if (a && (a.target === '_blank' || a.getAttribute('target') === '_blank')) {
                    a.target = '_self';
                }
            }, true);

            var origOpen = window.open;
            window.open = function(url) {
                if (url && typeof url === 'string') {
                    window.location.href = url;
                    return window;
                }
                return origOpen ? origOpen.apply(window, arguments) : window;
            };
        })();
    "#;

    WebviewWindowBuilder::new(&app, &window_label, WebviewUrl::External(parsed_url))
        .title(&window_title)
        .inner_size(1120.0, 780.0)
        .center()
        .resizable(true)
        .focused(true)
        .initialization_script(init_script)
        .build()
        .map_err(|e| format!("创建原生 WebView 窗口失败: {}", e))?;

    Ok(())
}
