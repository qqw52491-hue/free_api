use crate::agent::types::DispatchResult;
use crate::db::DbState;

pub fn dispatch(
    db: &DbState,
    action: &str,
    params: &serde_json::Value,
) -> DispatchResult {
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
                Err(e) => return DispatchResult {
                    stdout: String::new(),
                    stderr: format!("获取数据库锁失败: {}", e),
                    success: false,
                    route: "skill_manager:list".to_string(),
                },
            };
            let mut stmt = match conn.prepare("SELECT id, kind, name, description, content, enabled, updated_at FROM agent_custom_instructions ORDER BY updated_at DESC") {
                Ok(s) => s,
                Err(e) => return DispatchResult {
                    stdout: String::new(),
                    stderr: format!("查询失败: {}", e),
                    success: false,
                    route: "skill_manager:list".to_string(),
                },
            };
            let rows: Vec<serde_json::Value> = stmt.query_map([], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "kind": row.get::<_, String>(1)?,
                    "name": row.get::<_, String>(2)?,
                    "description": row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    "content": row.get::<_, String>(4)?,
                    "enabled": row.get::<_, i64>(5)? != 0,
                    "updated_at": row.get::<_, String>(6)?,
                }))
            }).ok().map(|iter| iter.flatten().collect()).unwrap_or_default();

            DispatchResult {
                stdout: serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".to_string()),
                stderr: String::new(),
                success: true,
                route: "skill_manager:list".to_string(),
            }
        }
        "add" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("").trim();
            let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("").trim();
            let kind = params.get("kind").and_then(|v| v.as_str()).unwrap_or("skill");
            let custom_desc = params.get("description").and_then(|v| v.as_str()).map(|s| s.trim().to_string());
            let model_id = params.get("model_id").and_then(|v| v.as_str()).map(|s| s.to_string());

            if name.is_empty() || content.is_empty() {
                return DispatchResult {
                    stdout: String::new(),
                    stderr: "❌ 缺少必填参数 name 或 content".to_string(),
                    success: false,
                    route: "skill_manager:add".to_string(),
                };
            }

            let desc = if let Some(d) = custom_desc.filter(|s| !s.is_empty()) {
                d
            } else if kind == "skill" {
                let block_res: Result<String, String> = (|| {
                    let handle = tokio::runtime::Handle::try_current();
                    let f = async {
                        let model_cfg = if let Some(ref mid) = model_id.filter(|s| !s.trim().is_empty()) {
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

                        crate::agent::skill_context::generate_llm_skill_summary(name, content, &model_cfg).await
                    };
                    if let Ok(h) = handle {
                        h.block_on(f)
                    } else {
                        futures::executor::block_on(f)
                    }
                })();
                block_res.unwrap_or_default()
            } else {
                String::new()
            };

            let conn = match db.0.lock() {
                Ok(c) => c,
                Err(e) => return DispatchResult {
                    stdout: String::new(),
                    stderr: format!("数据库锁获取失败: {}", e),
                    success: false,
                    route: "skill_manager:add".to_string(),
                },
            };

            let id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().to_rfc3339();
            let desc_val = if desc.is_empty() { None } else { Some(&desc) };

            let _ = conn.execute(
                "INSERT INTO agent_custom_instructions (id, kind, name, description, content, enabled, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6)",
                rusqlite::params![id, kind, name, desc_val, content, now],
            );

            DispatchResult {
                stdout: format!("✅ 成功保存 {} [{}]！\n💡 技能触发卡: {}", if kind == "skill" { "Skill" } else { "自定义提示词" }, name, desc),
                stderr: String::new(),
                success: true,
                route: "skill_manager:add".to_string(),
            }
        }
        "delete" => {
            let target = params.get("id").or_else(|| params.get("name")).and_then(|v| v.as_str()).unwrap_or("");
            let confirm = params.get("confirm").and_then(|v| v.as_bool()).unwrap_or(false);
            if target.is_empty() {
                return DispatchResult {
                    stdout: String::new(),
                    stderr: "❌ 缺少必填参数 id 或 name".to_string(),
                    success: false,
                    route: "skill_manager:delete".to_string(),
                };
            }
            if !confirm {
                return DispatchResult {
                    stdout: String::new(),
                    stderr: format!("⚠️ 警告：删除 [{}] 将永久移除配置，请确认并在 command 中附带 confirm: true", target),
                    success: false,
                    route: "skill_manager:delete".to_string(),
                };
            }
            let conn = match db.0.lock() {
                Ok(c) => c,
                Err(e) => return DispatchResult {
                    stdout: String::new(),
                    stderr: format!("数据库锁获取失败: {}", e),
                    success: false,
                    route: "skill_manager:delete".to_string(),
                },
            };
            let _ = conn.execute("DELETE FROM agent_custom_instructions WHERE id=?1 OR name=?1", rusqlite::params![target]);

            DispatchResult {
                stdout: format!("✅ 已成功删除 [{}]", target),
                stderr: String::new(),
                success: true,
                route: "skill_manager:delete".to_string(),
            }
        }
        _ => DispatchResult {
            stdout: String::new(),
            stderr: format!("❌ 未知的 skill_manager 动作 action='{}'。支持的动作: list, add, batch_import, toggle, delete", act),
            success: false,
            route: "skill_manager:unknown".to_string(),
        },
    }
}
