//! 数据库初始化：读连接池与全量 schema/迁移。

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

pub type ReadPool = Pool<SqliteConnectionManager>;

static READ_POOL: OnceLock<ReadPool> = OnceLock::new();


use super::*;

pub fn init_read_pool(path: &PathBuf) -> Result<ReadPool, String> {
    let manager = SqliteConnectionManager::file(path).with_init(|c| {
        c.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA query_only=ON;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;",
        )
    });
    let pool = Pool::builder()
        .max_size(8)
        .min_idle(Some(2))
        .build(manager)
        .map_err(|e| format!("初始化只读连接池失败: {e}"))?;
    let _ = READ_POOL.set(pool.clone());
    Ok(pool)
}

pub fn init_db(path: &PathBuf) -> Result<Connection> {
    std::fs::create_dir_all(path.parent().unwrap()).ok();
    let conn = Connection::open(path)?;

    conn.execute_batch(
        "
        PRAGMA journal_mode=WAL;
        PRAGMA foreign_keys=ON;
        PRAGMA synchronous=NORMAL;
        PRAGMA busy_timeout=10000;

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
            prompt_tokens INTEGER NOT NULL DEFAULT 0,
            evidence_ref_count INTEGER NOT NULL DEFAULT 0,
            error_kind TEXT,
            result_json TEXT,
            kind TEXT NOT NULL DEFAULT 'review',
            workspace_copy TEXT,
            resumed_from TEXT,
            message_history_json TEXT,
            consumed_at TEXT,
            task_id TEXT,
            attempt_id INTEGER NOT NULL DEFAULT 1,
            purpose TEXT,
            batch_id TEXT,
            contract_id TEXT
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
            custom_schema TEXT NOT NULL DEFAULT '',
            focus_domains TEXT NOT NULL DEFAULT '[]',
            input_schema TEXT NOT NULL DEFAULT '',
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
        -- 分叉支持：每回合终态提交时留存一份 checkpoint 快照（append-only）。
        -- agent_checkpoints 本体仍是最新态单行（崩溃恢复语义）；本表让
        -- fork_session 能取回任意回合边界的历史状态。anchor_message_id 指向
        -- 本回合写入的 assistant 消息，作为分叉点定位锚。
        CREATE TABLE IF NOT EXISTS agent_checkpoint_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            anchor_message_id TEXT NOT NULL,
            phase TEXT NOT NULL,
            snapshot_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_agent_checkpoint_history_session
            ON agent_checkpoint_history(session_id, id);
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

        -- 逐步增量持久化的运行轨迹：每步一行，替代每步整写 steps_json 的 O(n²) 序列化。
        -- steps_json 列仍保留（启动快照/终态快照/旧数据兼容），读者按「行优先、列回退」。
        CREATE TABLE IF NOT EXISTS agent_run_steps (
            run_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            step_json TEXT NOT NULL,
            PRIMARY KEY (run_id, seq)
        );
        CREATE INDEX IF NOT EXISTS idx_agent_run_steps_run
            ON agent_run_steps(run_id, seq ASC);

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

    // 门禁触发遥测：每次拦截落一行，回答"哪些机制真实触发过、频率如何"，
    // 为门禁机制的保留/退役提供数据。纯观测，不参与任何门禁逻辑。
    // 注意顺序：先建表/补列，最后建索引——索引引用的新列在旧库上必须先经 ALTER 补齐。
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_gate_fires (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT NOT NULL,
            session_id TEXT NOT NULL DEFAULT '',
            step_id INTEGER NOT NULL,
            gate TEXT NOT NULL,
            route TEXT NOT NULL,
            created_at TEXT NOT NULL
        );",
    )?;
    // 滚动迁移：早期版本只有 run_id（UUID），评测报告按 session_id 关联会恒空
    // （列已存在时本语句报 duplicate column，属于正常情况，静默忽略）
    let _ = conn.execute(
        "ALTER TABLE agent_gate_fires ADD COLUMN session_id TEXT NOT NULL DEFAULT ''",
        [],
    );
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_agent_gate_fires_route
            ON agent_gate_fires(route);
        CREATE INDEX IF NOT EXISTS idx_agent_gate_fires_session
            ON agent_gate_fires(session_id);",
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
    let _ = conn.execute(
        "ALTER TABLE agent_role_cards ADD COLUMN custom_schema TEXT NOT NULL DEFAULT '';",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE agent_role_cards ADD COLUMN focus_domains TEXT NOT NULL DEFAULT '[]';",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE agent_role_cards ADD COLUMN input_schema TEXT NOT NULL DEFAULT '';",
        [],
    );
    // P5 副本生命周期与脱水追问字段：workspace_copy / resumed_from / message_history_json
    let _ = conn.execute(
        "ALTER TABLE agent_child_runs ADD COLUMN workspace_copy TEXT;",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE agent_child_runs ADD COLUMN resumed_from TEXT;",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE agent_child_runs ADD COLUMN message_history_json TEXT;",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE agent_child_runs ADD COLUMN consumed_at TEXT;",
        [],
    );
    // 子任务成本台账补齐 prompt 侧：多 Agent 的隐性成本大头在 prompt（继承历史 +
    // 证据包），只记 completion 会让成本观测系统性偏低（旧库自愈）。
    let _ = conn.execute(
        "ALTER TABLE agent_child_runs ADD COLUMN prompt_tokens INTEGER NOT NULL DEFAULT 0;",
        [],
    );
    // DispatchEnvelope 逻辑身份落库：task_id 在同一逻辑任务的多次物理尝试间稳定
    // （failover/重试/resume 共享，attempt_id 递增）；purpose 标记用途
    // （plan_review/stage_review/rebuttal/writer/worker），batch_id 归组同批联合会审，
    // contract_id 记录交付契约版本。落库后"谁在等结果/这道题烧了多少钱"变成查询
    // 而非 checkpoint 里手动维护的名单。
    let _ = conn.execute("ALTER TABLE agent_child_runs ADD COLUMN task_id TEXT;", []);
    let _ = conn.execute(
        "ALTER TABLE agent_child_runs ADD COLUMN attempt_id INTEGER NOT NULL DEFAULT 1;",
        [],
    );
    let _ = conn.execute("ALTER TABLE agent_child_runs ADD COLUMN purpose TEXT;", []);
    let _ = conn.execute("ALTER TABLE agent_child_runs ADD COLUMN batch_id TEXT;", []);
    let _ = conn.execute("ALTER TABLE agent_child_runs ADD COLUMN contract_id TEXT;", []);
    // 存量回填：旧行的物理 run 就是它自己的逻辑任务，purpose 取 kind
    let _ = conn.execute(
        "UPDATE agent_child_runs SET task_id = id WHERE task_id IS NULL;",
        [],
    );
    let _ = conn.execute(
        "UPDATE agent_child_runs SET purpose = kind WHERE purpose IS NULL;",
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

    if let Err(e) = crate::agent::tools::role_manager::ensure_default_role_cards(&conn) {
        // 预装失败不阻断启动，但必须留痕，避免内置卡缺失后 1v1 方案评审静默失效
        eprintln!("[init_db] 预装内置角色卡失败: {e}");
    }

    Ok(conn)
}
