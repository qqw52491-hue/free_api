use crate::agent::types::DispatchResult;
use crate::db::DbState;

pub fn dispatch(db: &DbState, action: &str, params: &serde_json::Value) -> DispatchResult {
    let act = if !action.is_empty() {
        action
    } else {
        params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list")
    };

    match act {
        "list" => {
            let conn = match db.0.lock() {
                Ok(c) => c,
                Err(e) => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: format!("获取数据库锁失败: {}", e),
                        success: false,
                        route: "skill_manager:list".to_string(),
                    }
                }
            };
            let mut stmt = match conn.prepare("SELECT id, kind, name, description, content, enabled, updated_at FROM agent_custom_instructions ORDER BY updated_at DESC") {
                Ok(s) => s,
                Err(e) => return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("查询失败: {}", e),
                    success: false,
                    route: "skill_manager:list".to_string(),
                },
            };
            let rows: Vec<serde_json::Value> = stmt
                .query_map([], |row| {
                    Ok(serde_json::json!({
                        "id": row.get::<_, String>(0)?,
                        "kind": row.get::<_, String>(1)?,
                        "name": row.get::<_, String>(2)?,
                        "description": row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        "content": row.get::<_, String>(4)?,
                        "enabled": row.get::<_, i64>(5)? != 0,
                        "updated_at": row.get::<_, String>(6)?,
                    }))
                })
                .ok()
                .map(|iter| iter.flatten().collect())
                .unwrap_or_default();

            DispatchResult { view: None,
                stdout: serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".to_string()),
                stderr: String::new(),
                success: true,
                route: "skill_manager:list".to_string(),
            }
        }
        "add" => {
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            let content = params
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            let kind = params
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("skill");
            let custom_desc = params
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string());
            let model_id = params
                .get("model_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            if name.is_empty() || content.is_empty() {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: "❌ 缺少必填参数 name 或 content".to_string(),
                    success: false,
                    route: "skill_manager:add".to_string(),
                };
            }

            // 触发卡是可选子结果：LLM 提炼失败不阻断保存，但必须显式可见
            let mut card_failure: Option<String> = None;
            let desc = if let Some(d) = custom_desc.filter(|s| !s.is_empty()) {
                d
            } else if kind == "skill" {
                let block_res: Result<String, String> = (|| {
                    let handle = tokio::runtime::Handle::try_current();
                    let f = async {
                        let model_cfg = if let Some(ref mid) =
                            model_id.filter(|s| !s.trim().is_empty())
                        {
                            crate::llm::ModelConfig::from_db_state(db, mid)?
                        } else {
                            let conn = db.0.lock().map_err(|e| e.to_string())?;
                            let mut stmt = conn
                                .prepare("SELECT m.id FROM models m JOIN platforms p ON p.id = m.platform_id WHERE m.enabled = 1 ORDER BY m.created_at DESC")
                                .map_err(|e| e.to_string())?;
                            let model_ids: Vec<String> = stmt
                                .query_map([], |row| row.get(0))
                                .ok()
                                .map(|rows| rows.flatten().collect())
                                .unwrap_or_default();
                            drop(stmt);
                            drop(conn);

                            let mut found = None;
                            for id in model_ids {
                                if let Ok(cfg) = crate::llm::ModelConfig::from_db_state(db, &id) {
                                    found = Some(cfg);
                                    break;
                                }
                            }
                            found.ok_or_else(|| "未找到可用的大模型配置".to_string())?
                        };

                        crate::agent::skill_context::generate_llm_skill_summary(
                            name, content, &model_cfg,
                        )
                        .await
                    };
                    if let Ok(h) = handle {
                        h.block_on(f)
                    } else {
                        futures::executor::block_on(f)
                    }
                })();
                match block_res {
                    Ok(summary) => summary,
                    Err(error) => {
                        // 只记录错误类别，不回显模型配置等敏感内容
                        eprintln!(
                            "⚠️ [skill_manager] Skill [{}] 触发卡提炼失败: {}",
                            name, error
                        );
                        card_failure = Some(error);
                        String::new()
                    }
                }
            } else {
                String::new()
            };

            let conn = match db.0.lock() {
                Ok(c) => c,
                Err(e) => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: format!("数据库锁获取失败: {}", e),
                        success: false,
                        route: "skill_manager:add".to_string(),
                    }
                }
            };

            let id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().to_rfc3339();
            // description 列为 NOT NULL DEFAULT ''：空卡片必须写空字符串，写 NULL 会违反约束
            let desc_val = desc.as_str();

            if let Err(e) = conn.execute(
                "INSERT INTO agent_custom_instructions (id, kind, name, description, content, enabled, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6)",
                rusqlite::params![id, kind, name, desc_val, content, now],
            ) {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("保存 Skill [{}] 写库失败: {}", name, e),
                    success: false,
                    route: "skill_manager:add:db_error".to_string(),
                };
            }

            let kind_label = if kind == "skill" {
                "Skill"
            } else {
                "自定义提示词"
            };
            let card_note = match desc.trim() {
                s if s.is_empty() => {
                    if card_failure.is_some() {
                        "⚠️ 技能触发卡未生成（LLM 提炼失败；内容已保存，可重新调用 add 补充 description）".to_string()
                    } else {
                        String::new()
                    }
                }
                s => format!("💡 技能触发卡: {}", s),
            };

            DispatchResult { view: None,
                stdout: format!("✅ 成功保存 {} [{}]！\n{}", kind_label, name, card_note),
                stderr: String::new(),
                success: true,
                route: "skill_manager:add".to_string(),
            }
        }
        "delete" => {
            let target = params
                .get("id")
                .or_else(|| params.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let confirm = params
                .get("confirm")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if target.is_empty() {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: "❌ 缺少必填参数 id 或 name".to_string(),
                    success: false,
                    route: "skill_manager:delete".to_string(),
                };
            }
            if !confirm {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("⚠️ 警告：删除 [{}] 将永久移除配置，请确认并在 command 中附带 confirm: true", target),
                    success: false,
                    route: "skill_manager:delete".to_string(),
                };
            }
            let conn = match db.0.lock() {
                Ok(c) => c,
                Err(e) => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: format!("数据库锁获取失败: {}", e),
                        success: false,
                        route: "skill_manager:delete".to_string(),
                    }
                }
            };
            let affected = conn.execute(
                "DELETE FROM agent_custom_instructions WHERE id=?1 OR name=?1",
                rusqlite::params![target],
            );
            match affected {
                Ok(0) => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: format!(
                            "❌ 未找到名为 [{}] 的 Skill 或提示词；请先用 list 查看现有条目",
                            target
                        ),
                        success: false,
                        route: "skill_manager:delete:not_found".to_string(),
                    };
                }
                Err(e) => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: format!("删除 Skill [{}] 写库失败: {}", target, e),
                        success: false,
                        route: "skill_manager:delete".to_string(),
                    };
                }
                Ok(_) => {}
            }

            DispatchResult { view: None,
                stdout: format!("✅ 已成功删除 [{}]", target),
                stderr: String::new(),
                success: true,
                route: "skill_manager:delete".to_string(),
            }
        }
        _ => DispatchResult { view: None,
            stdout: String::new(),
            stderr: format!(
                "❌ 未知的 skill_manager 动作 action='{}'。支持的动作: list, add, delete",
                act
            ),
            success: false,
            route: "skill_manager:unknown".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn test_db() -> DbState {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE agent_custom_instructions (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL CHECK(kind IN ('prompt', 'skill')),
                name TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                content TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );",
        )
        .unwrap();
        DbState(Arc::new(Mutex::new(conn)))
    }

    fn insert_skill(db: &DbState, name: &str) {
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO agent_custom_instructions (id, kind, name, description, content, enabled, created_at, updated_at)
             VALUES ('sid-1', 'skill', ?1, '', 'content', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            rusqlite::params![name],
        )
        .unwrap();
    }

    #[test]
    fn unknown_action_lists_only_implemented_actions() {
        let db = test_db();
        let result = dispatch(&db, "batch_import", &json!({}));
        assert!(!result.success);
        // stderr 开头会回显用户传入的 action 名（合理）；
        // 关键是"支持的动作"推荐清单不得包含未实现 action
        let supported_list = result
            .stderr
            .split("支持的动作:")
            .nth(1)
            .unwrap_or_default();
        assert!(!supported_list.contains("batch_import"));
        assert!(!supported_list.contains("toggle"));
        assert!(supported_list.contains("list"));
        assert!(supported_list.contains("add"));
        assert!(supported_list.contains("delete"));
    }

    #[test]
    fn tool_manual_does_not_advertise_unimplemented_actions() {
        let manual = include_str!("../../../prompts/tools/skill_manager.md");
        // 未实现 action 只允许出现在"不支持"的否定性说明行，不允许作为编号动作声明
        for action in ["batch_import", "toggle"] {
            for line in manual.lines() {
                if line.contains(action) {
                    assert!(
                        line.contains("不支持"),
                        "手册把未实现 action {} 声明为受支持动作: {}",
                        action,
                        line
                    );
                }
            }
        }
        assert!(manual.contains("`list`"));
        assert!(manual.contains("`add`"));
        assert!(manual.contains("`delete`"));
    }

    #[test]
    fn add_card_failure_is_visible_but_config_still_saved() {
        // models 表不存在 → 默认模型查找失败 → 触发卡提炼失败。
        // 配置必须仍保存成功，且输出显式说明卡片未生成，不伪造空卡。
        let db = test_db();
        let result = dispatch(
            &db,
            "add",
            &json!({
                "name": "SQL规范检查",
                "content": "检查所有 SQL 查询必须包含 LIMIT。"
            }),
        );
        assert!(
            result.success,
            "卡片失败不应阻断配置保存: {}",
            result.stderr
        );
        assert!(result.stdout.contains("成功保存"));
        assert!(
            result.stdout.contains("触发卡未生成"),
            "输出: {}",
            result.stdout
        );
        // 绝不出现"💡 技能触发卡: "后跟空内容这种伪造成功卡
        assert!(!result.stdout.contains("💡 技能触发卡"));
        // 内容确实入库
        let conn = db.0.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_custom_instructions WHERE name='SQL规范检查'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn add_write_failure_is_not_reported_as_success() {
        // kind 违反 CHECK 约束 → INSERT 失败 → 必须返回 success:false
        let db = test_db();
        let result = dispatch(
            &db,
            "add",
            &json!({
                "name": "bad-kind-skill",
                "content": "内容",
                "kind": "invalid_kind"
            }),
        );
        assert!(!result.success, "写库失败不能报告成功: {}", result.stdout);
        assert!(result.stderr.contains("写库失败"));
    }

    #[test]
    fn delete_missing_skill_fails_honestly() {
        let db = test_db();
        let result = dispatch(
            &db,
            "delete",
            &json!({ "name": "不存在的技能", "confirm": true }),
        );
        assert!(!result.success);
        assert!(result.stderr.contains("未找到"));
    }

    #[test]
    fn delete_removes_existing_skill_with_confirm() {
        let db = test_db();
        insert_skill(&db, "SQL规范检查");

        let no_confirm = dispatch(&db, "delete", &json!({ "name": "SQL规范检查" }));
        assert!(!no_confirm.success, "缺少 confirm 必须拒绝");

        let confirmed = dispatch(
            &db,
            "delete",
            &json!({ "name": "SQL规范检查", "confirm": true }),
        );
        assert!(confirmed.success);
        let conn = db.0.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_custom_instructions",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
}
