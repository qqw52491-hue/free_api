use crate::db::DbState;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use uuid::Uuid;

/// 角色池上限：每张卡的清单都会注入主 Agent 上下文，超池会稀释注意力与预算。
pub const MAX_ROLE_CARDS: usize = 16;
pub const SYSTEM_WORKER: &str = "system_worker";
pub const SYSTEM_WRITER: &str = "system_writer";
pub const SYSTEM_REVIEWER: &str = "system_reviewer";

/// 保留标识由宿主判定，不能由调用者伪造 is_system 来解除保护。
pub fn system_role_kind(name: &str) -> Option<&'static str> {
    match name {
        SYSTEM_WORKER => Some("worker"),
        SYSTEM_WRITER => Some("writer"),
        SYSTEM_REVIEWER => Some("reviewer"),
        _ => None,
    }
}
const MAX_NAME_CHARS: usize = 64;
const MAX_LABEL_CHARS: usize = 80;
const MAX_PERSONA_CHARS: usize = 3_000;
const MAX_SCOPE_CHARS: usize = 500;
const MAX_TEST_COMMAND_CHARS: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RoleCard {
    pub id: String,
    pub name: String,
    pub label: String,
    pub model_id: String,
    pub persona_prompt: String,
    pub scope_hints: String,
    pub default_test_command: String,
    #[serde(default)]
    pub allowed_kinds: String,      // 逗号分隔: writer,worker,reviewer；空=不限
    #[serde(default)]
    pub hard_scope: String,         // 逗号分隔路径前缀；空=不限制
    #[serde(default)]
    pub max_concurrency: i64,       // 0=跟随全局默认
    pub enabled: bool,
    #[serde(default)]
    pub is_system: bool,
    #[serde(default)]
    pub custom_schema: String,      // 结构化交付契约 (JSON 序列化的 ContractSchema)
    #[serde(default)]
    pub focus_domains: String,      // 角色关注领域白名单列表 (JSON 序列化的 Vec<FocusDomain>)
    #[serde(default)]
    pub input_schema: String,       // 角色自定义入参契约 (JSON 序列化的 ContractSchema)
    #[serde(default)]
    pub allowed_skills: String,     // 逗号分隔 skill 名白名单；空=不注入技能手册
    #[serde(default)]
    pub allowed_mcp: String,        // 逗号分隔 MCP 插件名白名单；空=不授权 MCP（前向兼容字段）
}

impl RoleCard {
    /// 解析角色卡绑定的结构化交付契约模式（ContractSchema）
    pub fn parse_custom_schema(&self) -> Option<crate::agent::protocol::ContractSchema> {
        let trimmed = self.custom_schema.trim();
        if trimmed.is_empty() {
            return None;
        }
        serde_json::from_str(trimmed).ok()
    }

    /// 解析角色声明的关注领域清单
    pub fn parse_focus_domains(&self) -> Vec<crate::agent::protocol::FocusDomain> {
        let trimmed = self.focus_domains.trim();
        if trimmed.is_empty() || trimmed == "[]" {
            return Vec::new();
        }
        if let Ok(domains) = serde_json::from_str::<Vec<crate::agent::protocol::FocusDomain>>(trimmed) {
            return domains;
        }
        trimmed
            .split(|c| c == ',' || c == '；' || c == ' ' || c == '\n')
            .filter_map(|s| crate::agent::protocol::FocusDomain::from_domain_str(s))
            .collect()
    }

    /// 解析角色自定义入参契约模式（ContractSchema）
    pub fn parse_input_schema(&self) -> Option<crate::agent::protocol::ContractSchema> {
        let trimmed = self.input_schema.trim();
        if trimmed.is_empty() {
            return None;
        }
        serde_json::from_str(trimmed).ok()
    }

    /// 检查该角色卡是否允许挂载到指定 child kind (writer, worker, reviewer)
    pub fn allows_kind(&self, kind: &str) -> bool {
        let allowed = self.allowed_kinds.trim();
        if allowed.is_empty() {
            return true;
        }
        allowed
            .split(|c| c == ',' || c == '；' || c == ' ' || c == '\n')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .any(|k| k == kind.to_ascii_lowercase())
    }

    /// 检查相对路径是否命中 hard_scope 物理围栏（空=全放行；非空=必须命中至少一个前缀）
    pub fn allows_path(&self, relative_path: &Path) -> bool {
        let hard = self.hard_scope.trim();
        if hard.is_empty() {
            return true;
        }
        let rel_str = relative_path.to_string_lossy().replace('\\', "/");
        let rel_clean = rel_str.trim_start_matches("./");

        hard.split(|c| c == ',' || c == '；' || c == '\n')
            .map(|p| p.trim().replace('\\', "/"))
            .filter(|p| !p.is_empty())
            .any(|prefix| {
                let prefix_clean = prefix.trim_start_matches("./").trim_end_matches('/');
                if prefix_clean.is_empty() {
                    return true;
                }
                rel_clean == prefix_clean || rel_clean.starts_with(&format!("{}/", prefix_clean))
            })
    }
}

fn map_card(row: &rusqlite::Row<'_>) -> rusqlite::Result<RoleCard> {
    Ok(RoleCard {
        id: row.get(0)?,
        name: row.get(1)?,
        is_system: system_role_kind(&row.get::<_, String>(1)?).is_some(),
        label: row.get(2)?,
        model_id: row.get(3)?,
        persona_prompt: row.get(4)?,
        scope_hints: row.get(5)?,
        default_test_command: row.get(6)?,
        allowed_kinds: row.get(7)?,
        hard_scope: row.get(8)?,
        max_concurrency: row.get(9)?,
        enabled: row.get::<_, i64>(10)? != 0,
        custom_schema: row.get::<_, Option<String>>(11)?.unwrap_or_default(),
        focus_domains: row.get::<_, Option<String>>(12)?.unwrap_or_else(|| "[]".to_string()),
        input_schema: row.get::<_, Option<String>>(13)?.unwrap_or_default(),
        allowed_skills: row.get::<_, Option<String>>(14).ok().flatten().unwrap_or_default(),
        allowed_mcp: row.get::<_, Option<String>>(15).ok().flatten().unwrap_or_default(),
    })
}

const CARD_COLUMNS: &str =
    "id, name, label, model_id, persona_prompt, scope_hints, default_test_command, allowed_kinds, hard_scope, max_concurrency, enabled, custom_schema, focus_domains, input_schema, allowed_skills, allowed_mcp";


pub fn list_cards(db: &DbState, include_disabled: bool) -> Result<Vec<RoleCard>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let sql = if include_disabled {
        format!("SELECT {CARD_COLUMNS} FROM agent_role_cards ORDER BY name")
    } else {
        format!("SELECT {CARD_COLUMNS} FROM agent_role_cards WHERE enabled = 1 ORDER BY name")
    };
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], map_card)
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// 按标识取启用卡；未命中/被禁用返回 None，是否兜底由调用入口决定。
pub fn resolve_enabled_card(db: &DbState, name: &str) -> Result<Option<RoleCard>, String> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let card = conn
        .query_row(
            &format!("SELECT {CARD_COLUMNS} FROM agent_role_cards WHERE name = ?1 AND enabled = 1"),
            params![name],
            map_card,
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(card)
}

use rusqlite::OptionalExtension;

fn validate_card_input(
    params: &Value,
) -> Result<(String, String, String, String, String, String, String, String, i64, String, String, String, String, String), String> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "role_manager.upsert 缺少 name（英文角色标识，如 frontend_designer）。".to_string())?;
    if name.chars().count() > MAX_NAME_CHARS
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err("role name 只允许 ASCII 字母、数字与下划线（≤64 字符）。".to_string());
    }
    let take = |key: &str, max: usize, fallback: &str| -> String {
        params
            .get(key)
            .and_then(|v| v.as_str())
            .map(|v| v.trim().chars().take(max).collect())
            .unwrap_or_else(|| fallback.to_string())
    };
    let label = take("label", MAX_LABEL_CHARS, "");
    let model_id = take("model_id", 160, "");
    let persona_prompt = take("persona_prompt", MAX_PERSONA_CHARS, "");
    let scope_hints = take("scope_hints", MAX_SCOPE_CHARS, "");
    let default_test_command = take("default_test_command", MAX_TEST_COMMAND_CHARS, "");
    let allowed_kinds = take("allowed_kinds", 100, "");
    let hard_scope = take("hard_scope", 500, "");
    let max_concurrency = params
        .get("max_concurrency")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0)
        .min(20);
    let custom_schema = take("custom_schema", 10_000, "");
    if !custom_schema.is_empty() {
        let schema: crate::agent::protocol::ContractSchema = serde_json::from_str(&custom_schema)
            .map_err(|e| format!("custom_schema 不是有效的 JSON Schema: {}", e))?;
        for f in &schema.fields {
            crate::agent::protocol::validate_custom_field_name(&f.name)
                .map_err(|e| format!("custom_schema 包含非法字段名: {}", e))?;
        }
    }

    // 关注域只有 FocusDomain 枚举里的三种合法值；LLM 生成的角色卡常夹带中文语义词
    // （如「安全」「性能」）。非法值一律跳过，不让单个脏值把整张角色卡保存打回；
    // 仅当用户/调用方提供的值「全部非法」时才报错，且文案动态列出全部合法枚举。
    let focus_domains = if let Some(arr) = params.get("focus_domains").and_then(|v| v.as_array()) {
        let mut list: Vec<crate::agent::protocol::FocusDomain> = Vec::new();
        let mut rejected: Vec<String> = Vec::new();
        for item in arr {
            if let Some(s) = item.as_str() {
                let s_clean = s.trim();
                if s_clean.is_empty() { continue; }
                if let Some(domain) = crate::agent::protocol::FocusDomain::from_domain_str(s_clean) {
                    if !list.contains(&domain) {
                        list.push(domain);
                    }
                } else {
                    rejected.push(s_clean.to_string());
                }
            }
        }
        if list.is_empty() && !rejected.is_empty() {
            return Err(format!(
                "不支持的关注域: '{}'。可选关注域: {}",
                rejected.join("', '"),
                crate::agent::protocol::FocusDomain::all().iter().map(|d| d.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
        serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_string())
    } else if let Some(s) = params.get("focus_domains").and_then(|v| v.as_str()) {
        let trimmed = s.trim();
        if trimmed.is_empty() || trimmed == "[]" {
            "[]".to_string()
        } else if let Ok(list) = serde_json::from_str::<Vec<crate::agent::protocol::FocusDomain>>(trimmed) {
            serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_string())
        } else {
            // 反序列化失败时按分隔符兜底切分：先剥掉残留的 JSON 包装符号
            // （'["安全","性能"]' 直接按逗号切会留下 '["安全"' 这类碎片），再逐个校验。
            let cleaned = trimmed.trim_matches(|c: char| c == '[' || c == ']' || c == '"' || c == '\'' || c.is_whitespace());
            let mut list: Vec<crate::agent::protocol::FocusDomain> = Vec::new();
            let mut rejected: Vec<String> = Vec::new();
            for part in cleaned.split(|c| c == ',' || c == '；' || c == ' ') {
                let part_clean = part.trim().trim_matches(|c: char| c == '[' || c == ']' || c == '"' || c == '\'');
                if part_clean.is_empty() { continue; }
                if let Some(domain) = crate::agent::protocol::FocusDomain::from_domain_str(part_clean) {
                    if !list.contains(&domain) {
                        list.push(domain);
                    }
                } else {
                    rejected.push(part_clean.to_string());
                }
            }
            if list.is_empty() && !rejected.is_empty() {
                return Err(format!(
                    "不支持的关注域: '{}'。可选关注域: {}",
                    rejected.join("', '"),
                    crate::agent::protocol::FocusDomain::all().iter().map(|d| d.as_str()).collect::<Vec<_>>().join(", ")
                ));
            }
            serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_string())
        }
    } else {
        "[]".to_string()
    };

    let input_schema = take("input_schema", 10_000, "");
    let allowed_skills = take("allowed_skills", 2_000, "");
    let allowed_mcp = take("allowed_mcp", 2_000, "");
    if !input_schema.is_empty() {
        let schema: crate::agent::protocol::ContractSchema = serde_json::from_str(&input_schema)
            .map_err(|e| format!("input_schema 不是有效的 JSON Schema: {}", e))?;
        for f in &schema.fields {
            crate::agent::protocol::validate_custom_field_name(&f.name)
                .map_err(|e| format!("input_schema 包含非法字段名: {}", e))?;
        }
    }

    if let Some(bad) = [model_id.as_str(), default_test_command.as_str()]
        .iter()
        .find(|f| f.chars().any(|c| [';', '|', '&', '>', '<', '`', '$', '(', ')', '\n'].contains(&c)))
    {
        let _ = bad;
        return Err("model_id / default_test_command 不允许 shell 控制字符。".to_string());
    }
    Ok((
        name,
        label,
        model_id,
        persona_prompt,
        scope_hints,
        default_test_command,
        allowed_kinds,
        hard_scope,
        max_concurrency,
        custom_schema,
        focus_domains,
        input_schema,
        allowed_skills,
        allowed_mcp,
    ))
}

pub fn upsert_card(db: &DbState, params: &Value) -> Result<RoleCard, String> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("").trim();
    if system_role_kind(name).is_some() {
        // 基础卡只开放模型绑定；专精人设/入参/交付字段请放在自定义卡中。
        let object = params.as_object().ok_or("角色卡参数必须为对象")?;
        if object.keys().any(|key| key != "name" && key != "model_id") {
            return Err("系统基础卡仅支持修改 model_id；不能更改人设、工位或契约，请新建自定义角色卡。".into());
        }
        let model_id = params.get("model_id").and_then(Value::as_str)
            .ok_or("系统基础卡模型设置需要字符串 model_id（空串表示跟随路由）")?;
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let changed = conn.execute(
            "UPDATE agent_role_cards SET model_id = ?1, updated_at = ?2 WHERE name = ?3",
            params![model_id, chrono::Utc::now().to_rfc3339(), name],
        ).map_err(|e| e.to_string())?;
        drop(conn);
        if changed == 0 { return Err("系统基础卡尚未初始化，请重启应用。".into()); }
        return resolve_enabled_card(db, name)?.ok_or("系统基础卡不可用".into());
    }
    let (
        name,
        label,
        model_id,
        persona_prompt,
        scope_hints,
        default_test_command,
        allowed_kinds,
        hard_scope,
        max_concurrency,
        custom_schema,
        focus_domains,
        input_schema,
        allowed_skills,
        allowed_mcp,
    ) = validate_card_input(params)?;
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM agent_role_cards", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    let now = chrono::Utc::now().to_rfc3339();
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM agent_role_cards WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if existing.is_none() && count >= MAX_ROLE_CARDS as i64 {
        return Err(format!("角色池已满（最多 {} 张卡）；请先删除不再使用的角色。", MAX_ROLE_CARDS));
    }
    let id = existing.unwrap_or_else(|| Uuid::new_v4().to_string());
    conn.execute(
        "INSERT INTO agent_role_cards (id, name, label, model_id, persona_prompt, scope_hints, default_test_command, allowed_kinds, hard_scope, max_concurrency, enabled, custom_schema, focus_domains, input_schema, allowed_skills, allowed_mcp, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, ?11, ?12, ?13, ?14, ?15, ?16, ?16)
         ON CONFLICT(name) DO UPDATE SET
            label=excluded.label, model_id=excluded.model_id, persona_prompt=excluded.persona_prompt,
            scope_hints=excluded.scope_hints, default_test_command=excluded.default_test_command,
            allowed_kinds=excluded.allowed_kinds, hard_scope=excluded.hard_scope, max_concurrency=excluded.max_concurrency,
            custom_schema=excluded.custom_schema,
            focus_domains=excluded.focus_domains,
            input_schema=excluded.input_schema,
            allowed_skills=excluded.allowed_skills,
            allowed_mcp=excluded.allowed_mcp,
            enabled=1, updated_at=excluded.updated_at",
        params![id, name, label, model_id, persona_prompt, scope_hints, default_test_command, allowed_kinds, hard_scope, max_concurrency, custom_schema, focus_domains, input_schema, allowed_skills, allowed_mcp, now],
    )
    .map_err(|e| e.to_string())?;
    drop(conn);
    resolve_enabled_card(db, &name)?
        .ok_or_else(|| "角色卡写入后无法读取（意外状态）。".to_string())
}

pub fn toggle_card(db: &DbState, params: &Value) -> Result<String, String> {
    let name = if let Some(s) = params.as_str() {
        s
    } else {
        params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "role_manager.toggle 缺少 name。".to_string())?
    };
    if system_role_kind(name).is_some() {
        return Err("系统基础卡不能删除或停用；可调整模型绑定，专业需求请使用自定义卡。".into());
    }
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let rows = conn
        .execute(
            "UPDATE agent_role_cards SET enabled = 1 - enabled, updated_at = ?2 WHERE name = ?1",
            params![name, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|e| e.to_string())?;
    Ok(format!("已切换角色 {}（{} 行）", name, rows))
}

pub fn delete_card(db: &DbState, params: &Value) -> Result<String, String> {
    let name = if let Some(s) = params.as_str() {
        s
    } else {
        params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "role_manager.delete 缺少 name。".to_string())?
    };
    if system_role_kind(name).is_some() {
        return Err("系统基础卡不能删除或停用；可调整模型绑定，专业需求请使用自定义卡。".into());
    }
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let rows = conn
        .execute("DELETE FROM agent_role_cards WHERE name = ?1", params![name])
        .map_err(|e| e.to_string())?;
    Ok(format!("已删除角色 {}（{} 行）", name, rows))
}

/// 方案审查专用协议，仅作用于本次基础 reviewer 派发，不改变通用基础卡。
fn with_plan_review_protocol(mut card: RoleCard) -> RoleCard {
    let input_schema = json!({
        "fields": [
            {
                "name": "plan_summary",
                "field_type": "string",
                "required": true,
                "description": "主脑制定的实施方案核心摘要与执行步骤"
            },
            {
                "name": "target_files",
                "field_type": "string[]",
                "required": false,
                "description": "拟修改或创建的文件路径列表"
            },
            {
                "name": "discussion_round",
                "field_type": "number",
                "required": false,
                "description": "当前方案讨论的回合轮次（默认为 1）"
            },
            {
                "name": "user_goal",
                "field_type": "string",
                "required": false,
                "description": "用户的原始任务目标"
            }
        ],
        "strict_closed_set": false
    }).to_string();

    let custom_schema = json!({
        "fields": [
            {
                "name": "decision",
                "field_type": "string",
                "required": true,
                "description": "评审裁决：approved (通过开工), rebuttal (打回修改继续1v1), escalate (升级多专家联合会审)",
                "allowed_values": ["approved", "rebuttal", "escalate"]
            },
            {
                "name": "file_coupling",
                "field_type": "string",
                "required": true,
                "description": "修改关联度分析：isolated (单一孤立改动), coupled (跨层/深度关联)",
                "allowed_values": ["isolated", "coupled"]
            },
            {
                "name": "analysis",
                "field_type": "string",
                "required": true,
                "description": "综合技术含金量、文件关联度和战况的评审理由与分析"
            },
            {
                "name": "escalate_roles",
                "field_type": "string[]",
                "required": false,
                "description": "若 decision 为 escalate，指定需要参与联合会审的增援专家角色标识列表"
            }
        ],
        "strict_closed_set": false
    }).to_string();

    let persona_prompt = "你是一名资深技术主管 (Tech Lead)。你的核心职责是在主脑编写代码前，对其提交的方案 (Plan)、拟修改文件 (target_files) 和实施步骤进行深入的技术把关与 1v1 方案评审对齐。\n\n你必须客观评估：\n1. 方案可行性与严密性：方案设计是否逻辑自洽？是否存在遗漏的边界情况或破坏性变更？\n2. 改动关联度 (file_coupling)：\n   - isolated（单一/孤立）：各个修改文件职责明确、改动局部独立（如纯文案、单一组件样式、局部叶子函数修补、配置文件修改），即使改动了多个文件也是单一维度的修改。\n   - coupled（深度关联/跨层）：修改涉及多个架构层次、模块间契约变更、状态共享改造、全局鉴权/拦截器改动或数据库表结构与前端 IPC 联调。\n3. 讨论战况与定级决断 (decision)：\n   - approved：方案扎实清晰，改动范围可控（无论是单一改动，还是 1~2 轮内已对齐的关联改动）。直接放行，主脑可开工编码。无需评分；有具体阻塞缺陷时不得批准。普通疑问和建议不自动成为验收义务。\n   - rebuttal：方案存在可修正的逻辑缺陷、未考虑的边界或不严谨处，需要在 1v1 讨论中进一步推演修改。请在 findings 中给出具体修改意见与定位。\n   - escalate：方案改动范围巨大且跨模块强关联（如高危架构重构、底层数据流变更），或者主脑在 2~3 轮对质后方案仍存在不可调和的架构分歧。单人评审无法完全兜底，必须升级为多专家联合会审！请在 escalate_roles 中指定角色池中实际存在且可用的增援 reviewer；若缺少专业角色，明确报告缺口，不虚构角色标识。";

    // v2：在 v1 基础上追加审查清单——每次评审必须逐项核对并在意见中明确回答
    let persona_prompt_v2 = format!(
        "{persona_prompt}\n\n【审查清单（每次评审必须逐项核对，意见中明确回答每一项）】\n1. 状态机收敛：失败/超时/异常终态是否被纳入完成判定（is_complete、pending_count、all_failed）？\n2. 分支顺序：新产出的门禁路由在拦截链中的优先级是否正确（会不会被无关分支抢先）？\n3. 豁免链覆盖：新路由是否已加入 ask_human 冷却豁免白名单（漏加会造成二阶死锁）？\n4. 向后兼容：旧持久化数据（缺新字段的 checkpoint/会话）反序列化是否兼容？\n5. 测试覆盖：关键分支是否有回归测试，断言锁住的是行为还是实现细节？\n6. 监管披露：是否静默修改了监管行为而未在交付说明中披露？"
    );

    card.persona_prompt = persona_prompt_v2;
    card.input_schema = input_schema;
    card.custom_schema = custom_schema;
    card
}

/// tech_lead 真正缺失才兜底；显式禁用/排除、工位不符和查询错误不绕过。
pub fn resolve_plan_reviewer_card(db: &DbState, disabled_names: &[String]) -> Result<RoleCard, String> {
    if is_card_disabled_for(disabled_names, "tech_lead", "reviewer") {
        return Err("tech_lead 已被用户在当前会话排除/休眠；请指定其他 reviewer 审查。".into());
    }
    let tech_lead = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            &format!("SELECT {CARD_COLUMNS} FROM agent_role_cards WHERE name = 'tech_lead'"),
            [], map_card,
        ).optional().map_err(|e| e.to_string())?
    };
    if let Some(card) = tech_lead {
        if !card.enabled { return Err("tech_lead 已禁用；请指定其他 reviewer 审查。".into()); }
        if !card.allows_kind("reviewer") { return Err("tech_lead 未授权挂载为 reviewer；请指定其他 reviewer 审查。".into()); }
        return Ok(card);
    }
    let card = resolve_enabled_card(db, SYSTEM_REVIEWER)?
        .ok_or("系统基础 reviewer 尚未初始化，请重启应用。")?;
    if !card.allows_kind("reviewer") { return Err("系统基础 reviewer 工位配置异常".into()); }
    Ok(with_plan_review_protocol(card))
}

/// 只预装通用基础卡；用户删除的专业卡不会在重启后复活。
pub fn ensure_default_role_cards(conn: &rusqlite::Connection) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    for (name, kind, label, persona) in [
        (SYSTEM_WORKER, "worker", "基础只读侦察员", "根据任务与提供的证据进行只读调查，区分已核实事实与推测，按 worker 原生交付协议回报。"),
        (SYSTEM_WRITER, "writer", "基础写入执行员", "按明确的任务、范围和验收标准完成修改，保留无关内容，按 writer 原生交付协议回报改动与验证结果。"),
        (SYSTEM_REVIEWER, "reviewer", "基础综合审查员", "根据任务要求与证据独立审查，给出可定位的结论，区分缺陷、证据缺口与通道故障，按 reviewer 原生交付协议回报。"),
    ] {
        conn.execute(
            "INSERT INTO agent_role_cards (id, name, label, model_id, persona_prompt, scope_hints, default_test_command, allowed_kinds, hard_scope, max_concurrency, enabled, custom_schema, focus_domains, input_schema, created_at, updated_at)
             VALUES (?1, ?2, ?3, '', ?4, '', '', ?5, '', 0, 1, '', '[]', '', ?6, ?6)
             ON CONFLICT(name) DO UPDATE SET enabled = 1, allowed_kinds = excluded.allowed_kinds",
            params![format!("default-role-{name}"), name, label, persona, kind, now],
        ).map_err(|e| format!("初始化基础角色 {name} 失败: {e}"))?;
    }
    Ok(())
}

/// 检查角色卡在特定工位（如 "writer", "reviewer", "worker"）或全局是否处于禁用/休眠状态
pub fn is_card_disabled_for(disabled_names: &[String], card_name: &str, kind: &str) -> bool {
    if system_role_kind(card_name).is_some() { return false; }
    let name_lower = card_name.to_ascii_lowercase();
    let slot_prefix = format!("{}:", kind.to_ascii_lowercase());
    disabled_names.iter().any(|d| {
        let d_lower = d.to_ascii_lowercase();
        // 1. 全局禁用: "card_a"
        if d_lower == name_lower {
            return true;
        }
        // 2. 工位专属禁用: "writer:card_a"
        if d_lower.starts_with(&slot_prefix) {
            let target = &d_lower[slot_prefix.len()..];
            if target == name_lower {
                return true;
            }
        }
        // 3. 兼容 scout 与 worker
        if (kind.eq_ignore_ascii_case("scout") && d_lower.starts_with("worker:"))
            || (kind.eq_ignore_ascii_case("worker") && d_lower.starts_with("scout:"))
        {
            let target = d_lower.split_once(':').map(|(_, r)| r).unwrap_or("");
            if target == name_lower {
                return true;
            }
        }
        false
    })
}

/// 主 Agent 的角色池菜单（注入工具目录旁）：一行一卡，说明派发方式。
pub fn build_role_cards_menu_filtered(db: &DbState, disabled_names: &[String]) -> Result<String, String> {
    let cards = list_cards(db, false)?;
    let active_cards: Vec<RoleCard> = cards
        .into_iter()
        .filter(|c| {
            if c.is_system { return true; }
            // 若全局禁用该卡片名称，排除
            if disabled_names.iter().any(|d| d.eq_ignore_ascii_case(&c.name)) {
                return false;
            }
            // 若卡片在它所支持的全部工位上都被单独排除了，才排除
            let kinds = ["writer", "worker", "reviewer"];
            let has_any_active = kinds
                .iter()
                .any(|k| c.allows_kind(k) && !is_card_disabled_for(disabled_names, &c.name, k));
            has_any_active
        })
        .collect();
    if active_cards.is_empty() {
        return Ok(String::new());
    }
    let mut out = String::from("\n### 角色池（派发 child_reviewer/child_worker/child_writer 时传 \"role\": \"角色标识\" 即可挂载人设与专属模型；每张卡标注了「可挂载」类型，只能用于所列类型，否则派发会被拒）\n【提示】reviewer 角色建议在角色选择器中钉一个与规划者不同家族的模型：异构模型盲区互补，交叉审查收益最大。\n");
    out.push_str("【基础卡】system_worker / system_writer / system_reviewer 常驻，不可删除或休眠，默认跟随路由；专业卡可自由管理。tech_lead 缺失时 submit_plan 使用基础 reviewer 加载方案审查协议，不免除审查。\n");
    for card in active_cards {
        let model_note = if card.model_id.is_empty() {
            "跟随档位路由".to_string()
        } else {
            "钉死专属模型".to_string()
        };
        let scope_note = if card.scope_hints.is_empty() {
            String::new()
        } else {
            format!("；作用域: {}", card.scope_hints)
        };
        let schema_note = if let Some(schema) = card.parse_custom_schema() {
            if !schema.is_empty() {
                let names: Vec<&str> = schema.fields.iter().map(|f| f.name.as_str()).collect();
                format!("；特化契约字段: [{}]", names.join(", "))
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        let focus_note = {
            let domains = card.parse_focus_domains();
            if !domains.is_empty() {
                let d_str: Vec<&str> = domains.iter().map(|d| d.as_str()).collect();
                format!("；【系统预装材料: {}，派发时无需重复贴大段代码】", d_str.join(", "))
            } else {
                String::new()
            }
        };
        let input_note = if let Some(in_schema) = card.parse_input_schema() {
            if !in_schema.is_empty() {
                let names: Vec<&str> = in_schema.fields.iter().map(|f| f.name.as_str()).collect();
                format!("；需在 custom_inputs 提供参数: [{}]", names.join(", "))
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        out.push_str(&format!(
            "- [{}] {}：{}（{}{}{}{}{}；可挂载: {}）\n",
            card.name, card.label, card.persona_prompt.chars().take(80).collect::<String>(), model_note, scope_note, schema_note, focus_note, input_note, card.allowed_kinds
        ));
    }
    out.push_str("\n> 💡 **【派发纪律】** 派发给子 Agent 的 task 必须自包含：\n");

    out.push_str("> 1. 背景：为什么做这件事；\n");
    out.push_str("> 2. 目标：要产出什么（清晰可验收的标准）；\n");
    out.push_str("> 3. 相关材料：涉及的文件路径/既有结论/约束。\n");
    out.push_str("> 建议在派发参数中携带可选的 context_excerpts（包含 background、todo_slice、file_hints、prior_findings），避免模糊派发。\n");
    out.push_str("> custom_inputs 支持三个渲染保留键：`verified_facts`（宿主已核实事实）、`hard_constraints`（判定边界）、`definition_of_done`（完成定义），\n");
    out.push_str("> 它们会被升格为独立一级小节优先呈现给子 Agent；派发审查/写作类子任务时建议主动填写，钉死事实与验收标准。\n");
    out.push_str("> 若角色卡声明了 input_schema 必填入参，custom_inputs 必须实际供给，否则派发会被系统拒绝。\n");
    Ok(out)
}

pub fn build_role_cards_menu(db: &DbState) -> Result<String, String> {
    build_role_cards_menu_filtered(db, &[])
}

/// 把角色卡默认值合并进 child_writer 派发参数（纯函数，可单测）：
/// 模型未提供 test_command 时用卡的默认命令；focus_paths 为空时用卡的作用域提示。
pub fn merge_writer_defaults(card: &RoleCard, params: &Value) -> Value {
    let mut merged = params.clone();
    let has_test = merged.get("test_command").and_then(|v| v.as_str()).map(|v| !v.trim().is_empty()).unwrap_or(false);
    if !has_test && !card.default_test_command.is_empty() {
        merged["test_command"] = json!(card.default_test_command);
    }
    let has_focus = merged
        .get("focus_paths")
        .and_then(|v| v.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    if !has_focus && !card.scope_hints.is_empty() {
        let paths: Vec<String> = card
            .scope_hints
            .split(|c| c == ',' || c == '\n' || c == '；')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect();
        if !paths.is_empty() {
            merged["focus_paths"] = json!(paths);
        }
    }
    merged
}

/// 人设注入格式：spawn 函数把它追加到各自的 system prompt。
pub fn persona_block(card: &RoleCard) -> String {
    if card.persona_prompt.is_empty() {
        return String::new();
    }
    format!("\n\n【专精角色人设：{}】{}", card.label, card.persona_prompt)
}

/// 逗号分隔白名单解析（allowed_skills / allowed_mcp / hard_scope 同构）
fn parse_csv_list(raw: &str) -> Vec<String> {
    raw.split(|c| c == ',' || c == '；' || c == ' ' || c == '\n')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

impl RoleCard {
    pub fn parse_allowed_skills(&self) -> Vec<String> {
        parse_csv_list(&self.allowed_skills)
    }
    pub fn parse_allowed_mcp(&self) -> Vec<String> {
        parse_csv_list(&self.allowed_mcp)
    }
}

/// 单本手册注入上限：子任务没有主循环的 next_tool_hint 按需取回通道，
/// 白名单项直接注入全文——上限防超长手册撑爆子任务上下文。
const SKILL_DOC_MAX_CHARS: usize = 6000;

/// 角色技能手册注入块：allowed_skills 白名单中的 skill 文档全文注入子任务
/// system prompt（scout/reviewer/writer 通用，知识注入而非工具授权）。
/// 空白名单返回空串保持现状；白名单中的无效 skill 名显式列出便于修正配置。
pub fn skills_block(db: &crate::db::DbState, card: &RoleCard) -> String {
    let names = card.parse_allowed_skills();
    if names.is_empty() {
        return String::new();
    }
    let dir = crate::agent::skill_context::resolve_skills_dir();
    let mut docs = Vec::new();
    let mut missing = Vec::new();
    for name in &names {
        match crate::agent::skill_context::load_skill_doc_with_db(
            db,
            &dir,
            name,
            SKILL_DOC_MAX_CHARS,
        ) {
            Some(doc) => docs.push(format!("### 📖 技能手册 `{}`\n{}", name, doc)),
            None => missing.push(name.clone()),
        }
    }
    if docs.is_empty() {
        return String::new();
    }
    let mut block = format!("\n\n【角色授权技能手册（白名单 {} 本，按手册执行对应领域工作）】\n", docs.len());
    block.push_str(&docs.join("\n\n"));
    if !missing.is_empty() {
        block.push_str(&format!("\n\n⚠️ 白名单中以下 skill 不存在，配置可能有误：{}", missing.join(", ")));
    }
    block
}

/// 派发入参校验（input_schema 输入侧硬约束，与输出侧 custom_schema 对称）：
/// 角色卡声明了必填入参字段时，custom_inputs 必须实际供给；
/// 缺失返回列出字段名与说明的可读错误，供派发点直接透出拒绝。
pub fn validate_dispatch_inputs(card: &RoleCard, custom_inputs: Option<&Value>) -> Result<(), String> {
    let schema = match card.parse_input_schema() {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(()),
    };
    let obj = custom_inputs.and_then(|v| v.as_object());
    let missing: Vec<String> = schema
        .fields
        .iter()
        .filter(|f| f.required)
        .filter(|f| match obj.and_then(|m| m.get(&f.name)) {
            // 缺键/null/空白串/空数组/空对象一律按缺失计；number/bool 存在即算有
            None | Some(Value::Null) => true,
            Some(Value::String(s)) => s.trim().is_empty(),
            Some(Value::Array(a)) => a.is_empty(),
            Some(Value::Object(o)) => o.is_empty(),
            Some(_) => false,
        })
        .map(|f| {
            if f.description.trim().is_empty() {
                f.name.clone()
            } else {
                format!("{}（{}）", f.name, f.description)
            }
        })
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "【⚠️ 子任务派发被拒绝】角色卡 `{}` 声明的必填入参缺失: {}。请在 custom_inputs 中补齐后重新派发。",
            card.name,
            missing.join("、")
        ));
    }
    // 只核验已声明且实际提供的字段类型；可选缺省/null 与额外字段保持兼容。
    for field in &schema.fields {
        let Some(value) = obj.and_then(|m| m.get(&field.name)).filter(|v| !v.is_null()) else { continue };
        use crate::agent::protocol::ContractFieldType;
        let valid = match field.field_type {
            ContractFieldType::String => value.is_string(),
            ContractFieldType::StringArray => value.as_array().is_some_and(|a| a.iter().all(Value::is_string)),
            ContractFieldType::Number => value.is_number(),
            ContractFieldType::Boolean => value.is_boolean(),
        };
        if !valid {
            return Err(format!("【⚠️ 子任务派发被拒绝】角色卡 `{}` 的 custom_inputs.{} 类型错误：期望 {}。请按角色卡类型补齐后重新派发。",
                card.name, field.name, field.field_type.as_str()));
        }
    }
    Ok(())
}

pub fn dispatch(db: &DbState, action: &str, params: &Value) -> crate::agent::types::DispatchResult {
    let act = if !action.is_empty() {
        action
    } else {
        params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list")
    };
    let result: Result<String, String> = match act {
        "list" => list_cards(db, true).map(|cards| {
            if cards.is_empty() {
                "角色池为空。用 upsert 创建第一张角色卡：name/label/model_id/persona_prompt/scope_hints/default_test_command。".to_string()
            } else {
                cards
                    .iter()
                    .map(|c| {
                        format!(
                            "- [{}] {} | 模型: {} | 启用: {} | 人设: {} 字 | 默认测试: {}",
                            c.name,
                            c.label,
                            if c.model_id.is_empty() { "跟随档位" } else { &c.model_id },
                            c.enabled,
                            c.persona_prompt.chars().count(),
                            if c.default_test_command.is_empty() { "（无）" } else { &c.default_test_command },
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }),
        "upsert" => upsert_card(db, params)
            .map(|c| format!("角色卡已保存：[{}] {}（模型: {}）", c.name, c.label, if c.model_id.is_empty() { "跟随档位" } else { &c.model_id })),
        "toggle" => toggle_card(db, params),
        "delete" => delete_card(db, params),
        other => Err(format!(
            "未知 action '{other}'；可用：list、upsert、toggle、delete。"
        )),
    };
    match result {
        Ok(stdout) => crate::agent::types::DispatchResult { view: None,
            stdout,
            stderr: String::new(),
            success: true,
            route: format!("internal:role_manager/{act}"),
        },
        Err(error) => crate::agent::types::DispatchResult { view: None,
            stdout: String::new(),
            stderr: error,
            success: false,
            route: format!("host_rejection:role_manager/{act}"),
        },
    }
}

#[cfg(test)]
mod tests;

