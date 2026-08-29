use crate::db::DbState;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

/// 角色池上限：每张卡的清单都会注入主 Agent 上下文，超池会稀释注意力与预算。
pub const MAX_ROLE_CARDS: usize = 8;
const MAX_NAME_CHARS: usize = 64;
const MAX_LABEL_CHARS: usize = 80;
const MAX_PERSONA_CHARS: usize = 3_000;
const MAX_SCOPE_CHARS: usize = 500;
const MAX_TEST_COMMAND_CHARS: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleCard {
    pub id: String,
    pub name: String,
    pub label: String,
    pub model_id: String,
    pub persona_prompt: String,
    pub scope_hints: String,
    pub default_test_command: String,
    pub enabled: bool,
}

fn map_card(row: &rusqlite::Row<'_>) -> rusqlite::Result<RoleCard> {
    Ok(RoleCard {
        id: row.get(0)?,
        name: row.get(1)?,
        label: row.get(2)?,
        model_id: row.get(3)?,
        persona_prompt: row.get(4)?,
        scope_hints: row.get(5)?,
        default_test_command: row.get(6)?,
        enabled: row.get::<_, i64>(7)? != 0,
    })
}

const CARD_COLUMNS: &str =
    "id, name, label, model_id, persona_prompt, scope_hints, default_test_command, enabled";

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

/// 派发时按角色标识取启用的卡；未命中/被禁用返回 None（派发退化为默认行为）。
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

fn validate_card_input(params: &Value) -> Result<(String, String, String, String, String, String), String> {
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
    if let Some(bad) = [model_id.as_str(), default_test_command.as_str()]
        .iter()
        .find(|f| f.chars().any(|c| [';', '|', '&', '>', '<', '`', '$', '(', ')', '\n'].contains(&c)))
    {
        let _ = bad;
        return Err("model_id / default_test_command 不允许 shell 控制字符。".to_string());
    }
    Ok((name, label, model_id, persona_prompt, scope_hints, default_test_command))
}

pub fn upsert_card(db: &DbState, params: &Value) -> Result<RoleCard, String> {
    let (name, label, model_id, persona_prompt, scope_hints, default_test_command) =
        validate_card_input(params)?;
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
        "INSERT INTO agent_role_cards (id, name, label, model_id, persona_prompt, scope_hints, default_test_command, enabled, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?8)
         ON CONFLICT(name) DO UPDATE SET
            label=excluded.label, model_id=excluded.model_id, persona_prompt=excluded.persona_prompt,
            scope_hints=excluded.scope_hints, default_test_command=excluded.default_test_command,
            enabled=1, updated_at=excluded.updated_at",
        params![id, name, label, model_id, persona_prompt, scope_hints, default_test_command, now],
    )
    .map_err(|e| e.to_string())?;
    drop(conn);
    resolve_enabled_card(db, &name)?
        .ok_or_else(|| "角色卡写入后无法读取（意外状态）。".to_string())
}

pub fn toggle_card(db: &DbState, params: &Value) -> Result<String, String> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "role_manager.toggle 缺少 name。".to_string())?;
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
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "role_manager.delete 缺少 name。".to_string())?;
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let rows = conn
        .execute("DELETE FROM agent_role_cards WHERE name = ?1", params![name])
        .map_err(|e| e.to_string())?;
    Ok(format!("已删除角色 {}（{} 行）", name, rows))
}

/// 主 Agent 的角色池菜单（注入工具目录旁）：一行一卡，说明派发方式。
pub fn build_role_cards_menu(db: &DbState) -> Result<String, String> {
    let cards = list_cards(db, false)?;
    if cards.is_empty() {
        return Ok(String::new());
    }
    let mut out = String::from("\n### 专精角色池（派发 child_reviewer/child_worker/child_writer 时传 \"role\": \"角色标识\" 即可挂载人设与专属模型）\n");
    for card in cards {
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
        out.push_str(&format!(
            "- [{}] {}：{}（{}{}）\n",
            card.name, card.label, card.persona_prompt.chars().take(80).collect::<String>(), model_note, scope_note
        ));
    }
    Ok(out)
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
        Ok(stdout) => crate::agent::types::DispatchResult {
            stdout,
            stderr: String::new(),
            success: true,
            route: format!("internal:role_manager/{act}"),
        },
        Err(error) => crate::agent::types::DispatchResult {
            stdout: String::new(),
            stderr: error,
            success: false,
            route: format!("host_rejection:role_manager/{act}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn setup_db() -> DbState {
        let base = std::env::temp_dir().join(format!("free-api-role-cards-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        DbState(Arc::new(Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )))
    }

    #[test]
    fn card_crud_round_trip_and_caps() {
        let db = setup_db();
        let created = upsert_card(&db, &json!({
            "name": "frontend_designer", "label": "前端审美大师",
            "model_id": "gemini-uuid-1", "persona_prompt": "专精 Tailwind 与暗黑玻璃拟态",
            "scope_hints": "src/components, src/assets", "default_test_command": "npm run build",
        })).unwrap();
        assert_eq!(created.name, "frontend_designer");
        assert!(created.enabled);

        // upsert 更新（同名覆盖）
        let updated = upsert_card(&db, &json!({"name": "frontend_designer", "label": "前端大师 v2"})).unwrap();
        assert_eq!(updated.label, "前端大师 v2");
        assert!(updated.model_id.is_empty(), "未传字段不应保留旧值（整卡覆盖语义）");

        // 名字非法字符被拒绝
        assert!(upsert_card(&db, &json!({"name": "bad;name"})).is_err());

        // 池子上限：已有 1 张（frontend_designer），再填到 8 张后拒绝第 9 张
        for i in 1..MAX_ROLE_CARDS {
            upsert_card(&db, &json!({"name": format!("role_{}", i)})).unwrap();
        }
        assert!(upsert_card(&db, &json!({"name": "overflow_role"})).is_err());
    }

    #[test]
    fn resolve_returns_none_for_missing_or_disabled() {
        let db = setup_db();
        upsert_card(&db, &json!({"name": "reviewer_pro", "model_id": "m-1"})).unwrap();
        assert!(resolve_enabled_card(&db, "reviewer_pro").unwrap().is_some());
        assert!(resolve_enabled_card(&db, "nonexistent").unwrap().is_none());
        assert!(resolve_enabled_card(&db, "").unwrap().is_none());
        toggle_card(&db, &json!({"name": "reviewer_pro"})).unwrap();
        assert!(resolve_enabled_card(&db, "reviewer_pro").unwrap().is_none(), "禁用后不得再被派发");
    }

    #[test]
    fn writer_defaults_merge_from_card() {
        let card = RoleCard {
            id: "x".into(), name: "frontend_designer".into(), label: "前端".into(),
            model_id: "gemini".into(), persona_prompt: "审美优先".into(),
            scope_hints: "src/components, src/assets".into(),
            default_test_command: "npm run build".into(), enabled: true,
        };
        // 模型未提供默认值时补齐
        let merged = merge_writer_defaults(&card, &json!({"task": "打磨 UI"}));
        assert_eq!(merged["test_command"], "npm run build");
        assert_eq!(merged["focus_paths"][0], "src/components");
        // 模型显式提供时不覆盖
        let merged = merge_writer_defaults(&card, &json!({"task": "x", "test_command": "cargo test", "focus_paths": ["src/a.rs"]}));
        assert_eq!(merged["test_command"], "cargo test");
        assert_eq!(merged["focus_paths"][0], "src/a.rs");
        // persona 块
        assert!(persona_block(&card).contains("【专精角色人设：前端】"));
        assert!(persona_block(&RoleCard { persona_prompt: String::new(), ..card }).is_empty());
    }

    #[test]
    fn role_cards_menu_lists_enabled_only() {
        let db = setup_db();
        assert!(build_role_cards_menu(&db).unwrap().is_empty());
        upsert_card(&db, &json!({"name": "a_role", "label": "角色A", "persona_prompt": "描述A"})).unwrap();
        upsert_card(&db, &json!({"name": "b_role", "label": "角色B"})).unwrap();
        toggle_card(&db, &json!({"name": "b_role"})).unwrap();
        let menu = build_role_cards_menu(&db).unwrap();
        assert!(menu.contains("a_role") && menu.contains("角色A"));
        assert!(!menu.contains("b_role"), "禁用的卡不得出现在菜单");
    }
}
