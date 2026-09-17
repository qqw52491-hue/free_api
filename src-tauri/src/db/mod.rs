use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

pub type ReadPool = Pool<SqliteConnectionManager>;

static READ_POOL: OnceLock<ReadPool> = OnceLock::new();

/// 读连接持有包装器（自动解引用为 Connection，借用结束自动归还池中）
pub enum DbReaderConnection<'a> {
    Pooled(r2d2::PooledConnection<SqliteConnectionManager>),
    Locked(std::sync::MutexGuard<'a, Connection>),
}

impl<'a> std::ops::Deref for DbReaderConnection<'a> {
    type Target = Connection;
    fn deref(&self) -> &Self::Target {
        match self {
            DbReaderConnection::Pooled(conn) => conn.deref(),
            DbReaderConnection::Locked(guard) => guard.deref(),
        }
    }
}

/// 全局数据库状态 (Tauri managed state)
/// 架构设计：Single-Writer Multi-Reader (SWMR) 模式
/// - `.0`: 独占写连接互斥锁（所有写入、事务排队，杜绝 SQLITE_BUSY 并发锁冲突）
/// - `reader()`: 并发只读连接池（独立只读连接，在 WAL 模式下多读完全并行，与写操作互不阻塞）
#[derive(Clone)]
pub struct DbState(pub Arc<Mutex<Connection>>);

impl DbState {
    /// 取得专职写连接（独占锁，排队写入，杜绝 SQLITE_BUSY 冲突）
    #[inline]
    pub fn writer(&self) -> Result<std::sync::MutexGuard<'_, Connection>, String> {
        self.0.lock().map_err(|e| format!("获取数据库写锁失败: {e}"))
    }

    /// 取得并发只读连接（优先从 r2d2 读连接池借出，多读完全并发，0 锁争用；
    /// 若连接池未配置/测试环境下未初始化，自动平滑降级为互斥锁）
    #[inline]
    pub fn reader(&self) -> Result<DbReaderConnection<'_>, String> {
        if let Some(pool) = READ_POOL.get() {
            pool.get()
                .map(DbReaderConnection::Pooled)
                .map_err(|e| format!("从只读连接池获取连接失败: {e}"))
        } else {
            self.0
                .lock()
                .map(DbReaderConnection::Locked)
                .map_err(|e| format!("获取数据库连接失败: {e}"))
        }
    }
}

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
    // 评测/并行实例隔离：设置 FREE_API_DB_PATH 时使用独立数据库副本，
    // 避免与用户正在使用的日常实例争抢 SQLite 锁（曾导致评测实例启动挂死）。
    if let Ok(path) = std::env::var("FREE_API_DB_PATH") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    let home = dirs_next::data_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join("free-api-chat").join("data_v2.db")
}

mod maintenance;
mod init;
mod model_specs;

pub use maintenance::*;
pub use init::*;
pub use model_specs::*;

#[cfg(test)]
mod migration_tests;
