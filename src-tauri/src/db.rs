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

/// 启动清理：上次进程遗留的 running trace 标记为已中断。
/// runs_store 不跨进程存活，应用重启后这些任务不可能再恢复执行；
/// 不清理会让会话历史里永远挂着"运行中"的僵尸记录（微信复用会话尤其明显）。
pub fn mark_orphan_running_traces_interrupted(conn: &Connection) -> Result<usize> {
    let n = conn.execute(
        "UPDATE agent_run_traces
         SET status = 'cancelled',
             completed_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
             completion_json = COALESCE(completion_json, json_object('summary', '应用重启，任务已中断'))
         WHERE status = 'running'",
        [],
    )?;
    Ok(n)
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

        -- 项目只登记用户明确选择的本地根目录；它不会扫描或上传目录内容。
        CREATE TABLE IF NOT EXISTS agent_projects (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            root_path TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        -- 会话归属用关系表保存，避免修改既有 chat_sessions 结构或破坏历史会话。
        CREATE TABLE IF NOT EXISTS agent_project_sessions (
            session_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            created_at TEXT NOT NULL,
            FOREIGN KEY (session_id) REFERENCES chat_sessions(id) ON DELETE CASCADE,
            FOREIGN KEY (project_id) REFERENCES agent_projects(id) ON DELETE CASCADE
        );
        -- 资源只保存项目内 canonical 路径及元数据；读取时再次 canonicalize 并校验边界。
        CREATE TABLE IF NOT EXISTS agent_project_resources (
            resource_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            label TEXT NOT NULL,
            relative_path TEXT NOT NULL,
            canonical_path TEXT NOT NULL,
            kind TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            UNIQUE(project_id, canonical_path),
            FOREIGN KEY (project_id) REFERENCES agent_projects(id) ON DELETE CASCADE
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
        CREATE INDEX IF NOT EXISTS idx_agent_project_sessions_project ON agent_project_sessions(project_id);
        CREATE INDEX IF NOT EXISTS idx_agent_project_resources_project ON agent_project_resources(project_id, created_at DESC);

        -- child reviewer 只保存可观测运行元数据；不存 prompt、原始回复或思维文本。
        CREATE TABLE IF NOT EXISTS agent_child_runs (
            id TEXT PRIMARY KEY,
            parent_run_id TEXT NOT NULL,
            parent_session_id TEXT NOT NULL,
            workspace_root TEXT NOT NULL,
            model_id TEXT NOT NULL,
            status TEXT NOT NULL,
            started_at TEXT NOT NULL,
            finished_at TEXT,
            duration_ms INTEGER,
            completion_tokens INTEGER NOT NULL DEFAULT 0,
            evidence_ref_count INTEGER NOT NULL DEFAULT 0,
            error_kind TEXT,
            result_json TEXT
        );
        CREATE TABLE IF NOT EXISTS agent_role_cards (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            label TEXT NOT NULL DEFAULT '',
            model_id TEXT NOT NULL DEFAULT '',
            persona_prompt TEXT NOT NULL DEFAULT '',
            scope_hints TEXT NOT NULL DEFAULT '',
            default_test_command TEXT NOT NULL DEFAULT '',
            allowed_kinds TEXT NOT NULL DEFAULT '',
            hard_scope TEXT NOT NULL DEFAULT '',
            max_concurrency INTEGER NOT NULL DEFAULT 0,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_agent_child_runs_parent ON agent_child_runs(parent_run_id, started_at DESC);
        CREATE INDEX IF NOT EXISTS idx_agent_child_runs_session ON agent_child_runs(parent_session_id, started_at DESC);

        -- 全局后台异步任务表与持久化信箱 (Durable Pending Inbox)
        CREATE TABLE IF NOT EXISTS agent_background_jobs (
            id TEXT PRIMARY KEY,
            parent_session_id TEXT NOT NULL,
            parent_run_id TEXT NOT NULL,
            job_type TEXT NOT NULL,
            title TEXT NOT NULL,
            params_json TEXT NOT NULL DEFAULT '{}',
            status TEXT NOT NULL,
            started_at TEXT NOT NULL,
            finished_at TEXT,
            duration_ms INTEGER,
            result_json TEXT,
            error_kind TEXT,
            consumed INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_agent_jobs_parent ON agent_background_jobs(parent_run_id, consumed, started_at DESC);
        CREATE INDEX IF NOT EXISTS idx_agent_jobs_session ON agent_background_jobs(parent_session_id, consumed, started_at DESC);

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

        -- 逐调用 LLM usage telemetry（纯观测，零行为参与）：
        -- 为调优对照（单/多 Agent、提示版本、采样策略）提供可 SQL 查询的事实账本。
        -- 只存计数、标识与派生指标；严禁存储 API key、base URL、完整 prompt、
        -- 模型正文或工具输出（原始证据继续走 agent_step_logs 的 output_id 回查）。
        -- compaction 痕迹不由本表直接判定：history_message_count 下降 +
        -- fetched_tools_count 归零（compaction 会重置已加载工具手册集合）
        -- 即可从本表自身事后推断，避免侵入压缩模块。
        CREATE TABLE IF NOT EXISTS agent_llm_calls (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            step_id INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            model_id TEXT,
            model_tier TEXT,
            reasoning_effort TEXT,
            route TEXT NOT NULL DEFAULT 'full_context',
            retry_index INTEGER NOT NULL DEFAULT 0,
            prompt_tokens INTEGER NOT NULL DEFAULT 0,
            completion_tokens INTEGER NOT NULL DEFAULT 0,
            total_tokens INTEGER NOT NULL DEFAULT 0,
            cached_tokens INTEGER NOT NULL DEFAULT 0,
            cache_hit_rate REAL NOT NULL DEFAULT 0,
            context_window INTEGER NOT NULL DEFAULT 0,
            usage_percent REAL NOT NULL DEFAULT 0,
            history_message_count INTEGER NOT NULL DEFAULT 0,
            fetched_tools_count INTEGER NOT NULL DEFAULT 0,
            prefix_fingerprint TEXT,
            ttft_ms INTEGER NOT NULL DEFAULT 0,
            total_duration_ms INTEGER NOT NULL DEFAULT 0,
            session_call_index INTEGER NOT NULL DEFAULT 0,
            request_kind TEXT NOT NULL DEFAULT 'full_context',
            usage_source TEXT NOT NULL DEFAULT 'unavailable',
            outcome TEXT NOT NULL DEFAULT 'instruction_ready',
            request_try INTEGER NOT NULL DEFAULT 1,
            transport_attempt_count INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_agent_llm_calls_session
            ON agent_llm_calls(session_id, step_id);
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
    let _ = conn.execute(
        "ALTER TABLE models ADD COLUMN context_window INTEGER NOT NULL DEFAULT 0;",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE models ADD COLUMN provider_capabilities TEXT NOT NULL DEFAULT '{}';",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE mcp_plugins ADD COLUMN capability_card TEXT NOT NULL DEFAULT '';",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE agent_memory_nodes ADD COLUMN confidence_score INTEGER NOT NULL DEFAULT 100;",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE agent_memory_nodes ADD COLUMN hit_count INTEGER NOT NULL DEFAULT 0;",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE mcp_plugins ADD COLUMN url TEXT NOT NULL DEFAULT '';",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE mcp_plugins ADD COLUMN headers TEXT NOT NULL DEFAULT '{}';",
        [],
    );
    // -1 = 未设置（按 base_url 推断），0 = 显式不要求交替，1 = 显式要求归并连续同角色消息
    let _ = conn.execute(
        "ALTER TABLE platforms ADD COLUMN requires_alternating_roles INTEGER NOT NULL DEFAULT -1;",
        [],
    );

    // agent_llm_calls 逐调用口径扩展（旧库自愈）：从"成功返回的快照"升级为
    // "每一次实际请求尝试的记录"——失败、JSON 修复重发、中断均各占一行。
    let _ = conn.execute(
        "ALTER TABLE agent_llm_calls ADD COLUMN session_call_index INTEGER NOT NULL DEFAULT 0;",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE agent_llm_calls ADD COLUMN request_kind TEXT NOT NULL DEFAULT 'full_context';",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE agent_llm_calls ADD COLUMN usage_source TEXT NOT NULL DEFAULT 'unavailable';",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE agent_llm_calls ADD COLUMN outcome TEXT NOT NULL DEFAULT 'instruction_ready';",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE agent_llm_calls ADD COLUMN request_try INTEGER NOT NULL DEFAULT 1;",
        [],
    );
    let _ = conn.execute("ALTER TABLE agent_llm_calls ADD COLUMN transport_attempt_count INTEGER NOT NULL DEFAULT 0;", []);
    // 子任务类型：review = 受限只读审查者（child_reviewer）；worker = L2 只读侦察 worker（child_worker）。
    // 两者共用 agent_child_runs 账本与信箱回收机制，按 kind 分别计数与呈现。
    let _ = conn.execute(
        "ALTER TABLE agent_child_runs ADD COLUMN kind TEXT NOT NULL DEFAULT 'review';",
        [],
    );
    // result_json 晚于首版建表加入：老库缺失时子任务终态结果将无法落盘/消费，必须补齐
    let _ = conn.execute(
        "ALTER TABLE agent_child_runs ADD COLUMN result_json TEXT;",
        [],
    );
    // 角色卡 v2 字段（P3）：allowed_kinds / hard_scope / max_concurrency
    let _ = conn.execute(
        "ALTER TABLE agent_role_cards ADD COLUMN allowed_kinds TEXT NOT NULL DEFAULT '';",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE agent_role_cards ADD COLUMN hard_scope TEXT NOT NULL DEFAULT '';",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE agent_role_cards ADD COLUMN max_concurrency INTEGER NOT NULL DEFAULT 0;",
        [],
    );
    if let Err(error) = migrate_uvx_bridge_plugins_to_native(&conn) {
        eprintln!(
            "⚠️ mcp_plugins uvx 桥转原生 HTTP 迁移失败（保留原配置继续可用）: {}",
            error
        );
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

#[cfg(test)]
mod migration_tests {
    use super::*;

    #[test]
    fn orphan_running_traces_marked_interrupted_on_cleanup() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE agent_run_traces (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL, goal TEXT,
                status TEXT NOT NULL, steps_json TEXT NOT NULL DEFAULT '[]',
                completion_json TEXT, started_at TEXT NOT NULL,
                updated_at TEXT NOT NULL, completed_at TEXT);",
        )
        .unwrap();
        for (id, status) in [("t-running", "running"), ("t-done", "succeeded"), ("t-fail", "failed")] {
            conn.execute(
                "INSERT INTO agent_run_traces (id, session_id, goal, status, steps_json, started_at, updated_at)
                 VALUES (?1, 's1', 'g', ?2, '[]', '2026-08-30T00:00:00+00:00', '2026-08-30T00:00:00+00:00')",
                rusqlite::params![id, status],
            )
            .unwrap();
        }
        let n = mark_orphan_running_traces_interrupted(&conn).unwrap();
        assert_eq!(n, 1, "只有 running 状态应被清理");
        let (status, has_summary, has_completed): (String, i64, Option<String>) = conn
            .query_row(
                "SELECT status,
                        completion_json LIKE '%应用重启，任务已中断%',
                        completed_at
                 FROM agent_run_traces WHERE id='t-running'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "cancelled");
        assert_eq!(has_summary, 1, "应补写中断摘要");
        assert!(has_completed.is_some(), "应写 completed_at");
        // 终态记录不动
        let untouched: i64 = conn
            .query_row("SELECT count(*) FROM agent_run_traces WHERE id IN ('t-done','t-fail') AND status NOT IN ('succeeded','failed')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(untouched, 0);
    }

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
            .query_row(
                "SELECT command FROM mcp_plugins WHERE name = 'excel'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(excel_command, "uvx");
    }
}
