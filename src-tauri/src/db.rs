use rusqlite::{Connection, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// 全局数据库状态 (Tauri managed state)
#[derive(Clone)]
pub struct DbState(pub Arc<Mutex<Connection>>);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Platform {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSpec {
    pub id: String,
    pub name: String,
    pub context_window: i64,
    pub max_tokens: i64,
    pub supports_thinking: bool,
    pub default_temperature: f64,
    pub description: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub platform_id: String,
    pub name: String,         // 模型标识，如 google/gemini-2.0-flash
    pub display_name: String, // 显示名称
    pub max_tokens: i64,
    pub temperature: f64,
    pub enabled: bool,
    pub status: String, // unknown, online, offline, testing
    pub latency_ms: i64,
    /// 模型上下文窗口大小（tokens）。用于 Agent 历史压缩水位计算。
    /// 0 表示未配置，后端将使用保守默认值（32768）。
    #[serde(default)]
    pub context_window: i64,
    /// 每模型的 provider 能力配置（JSON）。空对象表示仅使用通用 OpenAI 兼容字段。
    #[serde(default = "default_provider_capabilities")]
    pub provider_capabilities: String,
}

fn default_provider_capabilities() -> String {
    "{}".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    pub id: String,
    pub title: String,
    pub model_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub attachments: Option<String>,
    pub created_at: String,
    pub model_id: Option<String>,
}

pub fn get_db_path() -> PathBuf {
    let home = dirs_next::data_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join("free-api-chat").join("data_v2.db")
}

pub fn init_db(path: &PathBuf) -> Result<Connection> {
    std::fs::create_dir_all(path.parent().unwrap()).ok();
    let conn = Connection::open(path)?;

    conn.execute_batch(
        "
        PRAGMA journal_mode=WAL;
        PRAGMA foreign_keys=ON;

        CREATE TABLE IF NOT EXISTS platforms (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            base_url TEXT NOT NULL,
            api_key TEXT NOT NULL,
            created_at TEXT NOT NULL,
            requires_alternating_roles INTEGER NOT NULL DEFAULT -1
        );

        CREATE TABLE IF NOT EXISTS models (
            id TEXT PRIMARY KEY,
            platform_id TEXT NOT NULL,
            name TEXT NOT NULL,
            display_name TEXT NOT NULL,
            max_tokens INTEGER NOT NULL DEFAULT 4096,
            temperature REAL NOT NULL DEFAULT 0.7,
            enabled INTEGER NOT NULL DEFAULT 1,
            status TEXT NOT NULL DEFAULT 'unknown',
            latency_ms INTEGER NOT NULL DEFAULT 0,
            context_window INTEGER NOT NULL DEFAULT 0,
            provider_capabilities TEXT NOT NULL DEFAULT '{}',
            created_at TEXT NOT NULL,
            FOREIGN KEY (platform_id) REFERENCES platforms(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS chat_sessions (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            model_id TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            message_count INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS chat_messages (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            attachments TEXT,
            created_at TEXT NOT NULL,
            model_id TEXT,
            FOREIGN KEY (session_id) REFERENCES chat_sessions(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_models_platform ON models(platform_id);
        CREATE INDEX IF NOT EXISTS idx_messages_session ON chat_messages(session_id);
        CREATE INDEX IF NOT EXISTS idx_sessions_updated ON chat_sessions(updated_at DESC);

        CREATE TABLE IF NOT EXISTS mcp_plugins (
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
        );

        -- 用户维护的自定义提示词与 Skill。它们作为普通上下文追加，不能替换内置系统约束。
        CREATE TABLE IF NOT EXISTS agent_custom_instructions (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL CHECK(kind IN ('prompt', 'skill')),
            name TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            content TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_agent_custom_instructions_enabled
            ON agent_custom_instructions(enabled, updated_at DESC);

        -- Session 内部任务数据区：记录本体与 checkpoint 分离，允许工具实时写入。
        -- 不设 chat_sessions 外键，兼容 Agent 自动创建的临时 session_id。
        CREATE TABLE IF NOT EXISTS agent_task_resources (
            resource_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            label TEXT NOT NULL,
            schema_json TEXT NOT NULL DEFAULT '{}',
            summary TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'active',
            row_count INTEGER NOT NULL DEFAULT 0,
            cursor_json TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_agent_task_resources_session
            ON agent_task_resources(session_id, updated_at DESC);

        CREATE TABLE IF NOT EXISTS agent_task_records (
            resource_id TEXT NOT NULL,
            record_key TEXT NOT NULL,
            data_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (resource_id, record_key),
            FOREIGN KEY (resource_id) REFERENCES agent_task_resources(resource_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_agent_task_records_resource
            ON agent_task_records(resource_id, record_key);

        -- 一条记录保存完整、自洽的 Agent checkpoint：phase + data + pending。
        CREATE TABLE IF NOT EXISTS agent_checkpoints (
            run_id TEXT PRIMARY KEY,
            phase TEXT NOT NULL,
            snapshot_json TEXT NOT NULL,
            version INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_agent_checkpoints_phase
            ON agent_checkpoints(phase, updated_at DESC);
        -- 每个会话回合的可复盘过程树快照；不同于 checkpoint，即使任务完成也会保留。
        CREATE TABLE IF NOT EXISTS agent_run_traces (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            goal TEXT NOT NULL,
            status TEXT NOT NULL,
            steps_json TEXT NOT NULL,
            completion_json TEXT,
            started_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            completed_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_agent_run_traces_session
            ON agent_run_traces(session_id, started_at ASC);

        -- 受管产物持久化账本：记录由 Agent 生成或登记的文件元数据，统一按 artifact_id 管理
        CREATE TABLE IF NOT EXISTS agent_artifacts (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            run_id TEXT,
            display_name TEXT NOT NULL,
            format TEXT NOT NULL,
            mime_type TEXT,
            kind TEXT NOT NULL,
            canonical_path TEXT NOT NULL,
            size_bytes INTEGER NOT NULL DEFAULT 0,
            sha256 TEXT,
            origin TEXT NOT NULL DEFAULT 'managed_generated',
            status TEXT NOT NULL DEFAULT 'ready',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_agent_artifacts_session
            ON agent_artifacts(session_id, created_at ASC);

        CREATE TABLE IF NOT EXISTS agent_step_logs (
            output_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            step_id INTEGER NOT NULL,
            raw_output TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_agent_step_logs_session
            ON agent_step_logs(session_id, step_id);

        -- FTS5 全文检索引擎：支持按 tool, action, outcome, raw_output 快速 BM25 检索
        -- tokenize='trigram'：支持中文子串与代码标识符子串检索（unicode61 会把连续中文当成单 token，中文召回率≈0）
        CREATE VIRTUAL TABLE IF NOT EXISTS agent_step_logs_fts USING fts5(
            output_id UNINDEXED,
            session_id UNINDEXED,
            step_id UNINDEXED,
            tool,
            action,
            outcome,
            content,
            tokenize='trigram'
        );

        -- 浅层三级记忆节点表 (Level 0: 原始事件, Level 1: Episode片段, Level 2: 阶段摘要/Lesson)
        CREATE TABLE IF NOT EXISTS agent_memory_nodes (
            id TEXT PRIMARY KEY,
            parent_id TEXT,
            level INTEGER NOT NULL,
            kind TEXT NOT NULL,
            session_id TEXT NOT NULL,
            plan_id TEXT,
            step_index INTEGER,
            content TEXT NOT NULL,
            metadata_json TEXT NOT NULL DEFAULT '{}',
            confidence_score INTEGER NOT NULL DEFAULT 100,
            hit_count INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_agent_memory_nodes_session
            ON agent_memory_nodes(session_id, level, step_index);
        CREATE INDEX IF NOT EXISTS idx_agent_memory_nodes_parent
            ON agent_memory_nodes(parent_id);

        CREATE VIRTUAL TABLE IF NOT EXISTS agent_memory_nodes_fts USING fts5(
            node_id UNINDEXED,
            session_id UNINDEXED,
            level UNINDEXED,
            kind UNINDEXED,
            plan_id UNINDEXED,
            content,
            trigger_keywords,
            tokenize='trigram'
        );

        -- Lesson 召回与采纳血缘追踪表 (精确因果归因与负反馈打分)
        CREATE TABLE IF NOT EXISTS agent_lesson_recalls (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            step_id INTEGER NOT NULL,
            recalled_lesson_ids TEXT NOT NULL DEFAULT '[]',
            applied_lesson_ids TEXT NOT NULL DEFAULT '[]',
            outcome TEXT NOT NULL DEFAULT 'pending',
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_agent_lesson_recalls_session
            ON agent_lesson_recalls(session_id, step_id);

        -- 全局模型规格词典表 (Model Specs 预设库，与渠道解耦)
        CREATE TABLE IF NOT EXISTS model_specs (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            context_window INTEGER NOT NULL,
            max_tokens INTEGER NOT NULL,
            supports_thinking BOOLEAN NOT NULL DEFAULT 0,
            default_temperature REAL NOT NULL DEFAULT 0.7,
            description TEXT DEFAULT '',
            created_at TEXT NOT NULL
        );
    ",
    )?;

    // 滚动迁移：早期版本的 FTS 表用默认 unicode61 分词器（中文子串检索全部失效），
    // 检测到旧表时重建为 trigram，并从 agent_step_logs 主表回填索引（失败不阻断主流程）
    if let Err(e) = crate::agent::retrieval::ensure_trigram_fts(&conn) {
        eprintln!(
            "⚠️ FTS5 trigram 迁移失败（不影响主流程，历史检索降级）: {}",
            e
        );
    }

    // 滚动迁移：为旧版数据库补齐新字段，CREATE TABLE 不会重建已有表（每个 ALTER TABLE 独立执行，防止单个已存在字段中断整个批处理）
    let _ = conn.execute("ALTER TABLE models ADD COLUMN context_window INTEGER NOT NULL DEFAULT 0;", []);
    let _ = conn.execute("ALTER TABLE models ADD COLUMN provider_capabilities TEXT NOT NULL DEFAULT '{}';", []);
    let _ = conn.execute("ALTER TABLE mcp_plugins ADD COLUMN capability_card TEXT NOT NULL DEFAULT '';", []);
    let _ = conn.execute("ALTER TABLE agent_memory_nodes ADD COLUMN confidence_score INTEGER NOT NULL DEFAULT 100;", []);
    let _ = conn.execute("ALTER TABLE agent_memory_nodes ADD COLUMN hit_count INTEGER NOT NULL DEFAULT 0;", []);
    let _ = conn.execute("ALTER TABLE mcp_plugins ADD COLUMN url TEXT NOT NULL DEFAULT '';", []);
    let _ = conn.execute("ALTER TABLE mcp_plugins ADD COLUMN headers TEXT NOT NULL DEFAULT '{}';", []);
    // -1 = 未设置（按 base_url 推断），0 = 显式不要求交替，1 = 显式要求归并连续同角色消息
    let _ = conn.execute("ALTER TABLE platforms ADD COLUMN requires_alternating_roles INTEGER NOT NULL DEFAULT -1;", []);
    if let Err(error) = migrate_uvx_bridge_plugins_to_native(&conn) {
        eprintln!("⚠️ mcp_plugins uvx 桥转原生 HTTP 迁移失败（保留原配置继续可用）: {}", error);
    }

    // 初始化与更新预置标准物理规格库（纯净容量档位，与品牌型号解耦）
    let standard_specs = vec![
        (
            "32k 轻量/本地档",
            32768,
            4096,
            false,
            0.7,
            "适用于 7B/8B 小型模型、Ollama 本地模型与端侧轻量任务 (32k 上下文 / 4k 输出)",
        ),
        (
            "128k 主流标配档",
            128000,
            8192,
            false,
            0.7,
            "适用于 DeepSeek-V3、GPT-4o、商汤、通义等 90% 主流大模型 (128k 上下文 / 8k 输出)",
        ),
        (
            "128k 深度思考档",
            128000,
            8192,
            true,
            0.6,
            "适用于 DeepSeek-R1、o1/o3 等深度推理/思维链模型 (128k 上下文 / 8k 输出 / 思考模式)",
        ),
        (
            "200k 长上下文档",
            200000,
            8192,
            false,
            0.7,
            "适用于 Claude 3.5/3.7、Kimi 等长文档与大型代码库模型 (200k 上下文 / 8k 输出)",
        ),
        (
            "1M 超长窗口档",
            1048576,
            8192,
            false,
            0.7,
            "适用于 Google Gemini Flash、MiniMax 等百万 Token 级超大窗口模型 (1M 上下文 / 8k 输出)",
        ),
        (
            "2M 极致窗口档",
            2097152,
            16384,
            false,
            0.7,
            "适用于 Google Gemini Pro 等 200万 Token 极致超长上下文模型 (2M 上下文 / 16k 输出)",
        ),
    ];

    // 清理旧版本按具体品牌命名的历史规格（避免混淆）
    let _ = conn.execute(
        "DELETE FROM model_specs WHERE name IN (
            'DeepSeek-V3', 'DeepSeek-R1', 'DeepSeek-V4-Flash', 
            'Claude-3.5-Sonnet', 'Claude-3.7-Sonnet', 
            'GPT-4o', 'GPT-4o-mini', 
            'Gemini-2.0-Flash', 'Gemini-1.5-Pro', 
            'Qwen-2.5-Coder-32B', 'Llama-3.3-70B'
        );",
        [],
    );

    let now = chrono::Utc::now().to_rfc3339();
    for (name, ctx, max_tok, thinking, temp, desc) in standard_specs {
        let id = uuid::Uuid::new_v4().to_string();
        let _ = conn.execute(
            "INSERT INTO model_specs (id, name, context_window, max_tokens, supports_thinking, default_temperature, description, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(name) DO UPDATE SET 
                context_window = excluded.context_window,
                max_tokens = excluded.max_tokens,
                supports_thinking = excluded.supports_thinking,
                default_temperature = excluded.default_temperature,
                description = excluded.description",
            rusqlite::params![id, name, ctx, max_tok, thinking, temp, desc, now],
        );
    }

    Ok(conn)
}

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
            headers.insert("Authorization".to_string(), format!("Bearer {}", token.trim()));
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
        println!("🔌 [DB 迁移] {} 个 uvx mcp-stdio 桥接插件已转为原生 HTTP 直连", migrated);
    }
    Ok(migrated)
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    #[test]
    fn uvx_bridge_rows_convert_to_native_http_and_others_untouched() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mcp_plugins (
                id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE,
                command TEXT NOT NULL, args TEXT NOT NULL DEFAULT '[]',
                env TEXT NOT NULL DEFAULT '{}', capability_card TEXT NOT NULL DEFAULT '',
                enabled INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL,
                url TEXT NOT NULL DEFAULT '', headers TEXT NOT NULL DEFAULT '{}');",
        )
        .unwrap();

        // 1) 典型 uvx 桥：应转为原生 HTTP
        conn.execute(
            "INSERT INTO mcp_plugins (id, name, command, args, env, created_at) VALUES ('1', 'remote', 'uvx', ?1, ?2, '2026-01-01')",
            rusqlite::params![
                r#"["mcp-stdio","https://mcp.example.com/mcp","--transport","streamable-http"]"#,
                r#"{"MCP_BEARER_TOKEN":"tok-123"}"#
            ],
        ).unwrap();
        // 2) 本地 stdio 插件：必须原样保留
        conn.execute(
            "INSERT INTO mcp_plugins (id, name, command, args, env, created_at) VALUES ('2', 'excel', 'uvx', ?1, '{}', '2026-01-01')",
            rusqlite::params![r#"["excel-mcp-server","stdio"]"#],
        ).unwrap();

        let migrated = migrate_uvx_bridge_plugins_to_native(&conn).unwrap();
        assert_eq!(migrated, 1);

        let (url, headers, command, args, env): (String, String, String, String, String) = conn
            .query_row(
                "SELECT url, headers, command, args, env FROM mcp_plugins WHERE name = 'remote'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(url, "https://mcp.example.com/mcp");
        assert!(headers.contains("Bearer tok-123"));
        assert_eq!(command, "");
        assert_eq!(args, "[]");
        assert_eq!(env, "{}");

        // excel 未被改动
        let excel_command: String = conn
            .query_row("SELECT command FROM mcp_plugins WHERE name = 'excel'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(excel_command, "uvx");
    }
}
