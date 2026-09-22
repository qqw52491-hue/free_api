//! 模型规格 CRUD 与 uvx 桥接插件迁移。

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

pub type ReadPool = Pool<SqliteConnectionManager>;


use super::*;


// ==================== MODEL SPECS CRUD HELPERS ====================

pub fn get_all_model_specs(conn: &Connection) -> Result<Vec<ModelSpec>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, context_window, max_tokens, supports_thinking, default_temperature, description, created_at 
         FROM model_specs ORDER BY name ASC"
    )?;
    let list = stmt
        .query_map([], |row| {
            Ok(ModelSpec {
                id: row.get(0)?,
                name: row.get(1)?,
                context_window: row.get(2)?,
                max_tokens: row.get(3)?,
                supports_thinking: row.get(4)?,
                default_temperature: row.get(5)?,
                description: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(list)
}

pub fn upsert_model_spec_record(conn: &Connection, spec: &ModelSpec) -> Result<()> {
    conn.execute(
        "INSERT INTO model_specs (id, name, context_window, max_tokens, supports_thinking, default_temperature, description, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(name) DO UPDATE SET
            context_window = excluded.context_window,
            max_tokens = excluded.max_tokens,
            supports_thinking = excluded.supports_thinking,
            default_temperature = excluded.default_temperature,
            description = excluded.description",
        rusqlite::params![
            spec.id,
            spec.name,
            spec.context_window,
            spec.max_tokens,
            spec.supports_thinking,
            spec.default_temperature,
            spec.description,
            spec.created_at
        ],
    )?;
    Ok(())
}

pub fn delete_model_spec_record(conn: &Connection, id_or_name: &str) -> Result<usize> {
    conn.execute(
        "DELETE FROM model_specs WHERE id = ?1 OR name = ?1",
        rusqlite::params![id_or_name],
    )
}

/// 把历史遗留的 `uvx mcp-stdio <url> --transport ...` 桥接插件一次性转为原生 HTTP 直连。
/// 只识别精确的桥接形态（command=uvx 且 args[0]=mcp-stdio 且带 URL），从 args 提取
/// URL、从 env 的 MCP_BEARER_TOKEN 提取鉴权头；转换失败或形态不符的行原样保留。
/// 原生直连消除 uvx 子进程依赖与 stdio 会话桥的握手竞态，是配置层的推荐形态。
pub fn migrate_uvx_bridge_plugins_to_native(conn: &Connection) -> Result<usize, String> {
    let rows: Vec<(String, String, String, String)> = {
        let mut stmt = conn
            .prepare("SELECT name, command, args, env FROM mcp_plugins WHERE url = ''")
            .map_err(|e| e.to_string())?;
        let collected = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        collected
    };

    let mut migrated = 0usize;
    for (name, command, args_json, env_json) in rows {
        if command.trim() != "uvx" {
            continue;
        }
        let args: Vec<String> = serde_json::from_str(&args_json).unwrap_or_default();
        if args.first().map(String::as_str) != Some("mcp-stdio") {
            continue;
        }
        let Some(url) = args.get(1).filter(|u| u.starts_with("http")) else {
            continue;
        };
        let env: std::collections::HashMap<String, String> =
            serde_json::from_str(&env_json).unwrap_or_default();
        let mut headers = std::collections::HashMap::new();
        if let Some(token) = env.get("MCP_BEARER_TOKEN").filter(|t| !t.trim().is_empty()) {
            headers.insert(
                "Authorization".to_string(),
                format!("Bearer {}", token.trim()),
            );
        }
        let headers_json = serde_json::to_string(&headers).map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE mcp_plugins SET url = ?2, headers = ?3, command = '', args = '[]', env = '{}'
             WHERE name = ?1",
            rusqlite::params![name, url, headers_json],
        )
        .map_err(|e| e.to_string())?;
        migrated += 1;
    }
    if migrated > 0 {
        println!(
            "🔌 [DB 迁移] {} 个 uvx mcp-stdio 桥接插件已转为原生 HTTP 直连",
            migrated
        );
    }
    Ok(migrated)
}
