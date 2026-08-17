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
            created_at TEXT NOT NULL
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
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL
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
    ",
    )?;

    // 滚动迁移：早期版本的 FTS 表用默认 unicode61 分词器（中文子串检索全部失效），
    // 检测到旧表时重建为 trigram，并从 agent_step_logs 主表回填索引（失败不阻断主流程）
    if let Err(e) = crate::agent::retrieval::ensure_trigram_fts(&conn) {
        eprintln!("⚠️ FTS5 trigram 迁移失败（不影响主流程，历史检索降级）: {}", e);
    }

    // 滚动迁移：为旧版数据库补齐新字段，CREATE TABLE 不会重建已有表
    let _ = conn.execute_batch(
        "ALTER TABLE models ADD COLUMN context_window INTEGER NOT NULL DEFAULT 0;",
    );

    Ok(conn)
}
