use crate::agent::types::DispatchResult;
use crate::db::DbState;

/// 与真实 handler 对齐的查询动作；不能仅因调用方声称 read/list 就放宽。
pub(crate) fn is_readonly_action(action: &str) -> bool {
    matches!(action, "list_specs" | "list_platforms")
}

pub fn dispatch(db: &DbState, action: &str, params: &serde_json::Value) -> DispatchResult {
    let conn = match db.0.lock() {
        Ok(c) => c,
        Err(e) => {
            return DispatchResult { view: None,
                stdout: String::new(),
                stderr: format!("数据库锁获取失败: {}", e),
                success: false,
                route: "model_manager".to_string(),
            }
        }
    };

    let act = if !action.is_empty() {
        action
    } else {
        params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list_specs")
    };

    match act {
        "list_specs" => {
            match crate::db::get_all_model_specs(&conn) {
                Ok(specs) => DispatchResult { view: None,
                    stdout: serde_json::to_string_pretty(&specs).unwrap_or_else(|_| "[]".to_string()),
                    stderr: String::new(),
                    success: true,
                    route: "model_manager:list_specs".to_string(),
                },
                Err(e) => DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("获取模型规格列表失败: {}", e),
                    success: false,
                    route: "model_manager:list_specs".to_string(),
                },
            }
        }
        "upsert_spec" => {
            let name = match params.get("name").and_then(|v| v.as_str()) {
                Some(n) if !n.trim().is_empty() => n.trim().to_string(),
                _ => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: "❌ 缺少必填参数 name (模型规格名称)".to_string(),
                        success: false,
                        route: "model_manager:upsert_spec".to_string(),
                    }
                }
            };
            let context_window = params
                .get("context_window")
                .and_then(|v| v.as_i64())
                .unwrap_or(128000);
            let max_tokens = params
                .get("max_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(8192);
            let supports_thinking = params
                .get("supports_thinking")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let default_temperature = params
                .get("default_temperature")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.7);
            let description = params
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let spec = crate::db::ModelSpec {
                id: uuid::Uuid::new_v4().to_string(),
                name: name.clone(),
                context_window,
                max_tokens,
                supports_thinking,
                default_temperature,
                description,
                created_at: chrono::Utc::now().to_rfc3339(),
            };

            match crate::db::upsert_model_spec_record(&conn, &spec) {
                Ok(_) => DispatchResult { view: None,
                    stdout: format!(
                        "✅ 成功保存模型规格 [{}] (context_window: {}, max_tokens: {}, supports_thinking: {})",
                        spec.name, spec.context_window, spec.max_tokens, spec.supports_thinking
                    ),
                    stderr: String::new(),
                    success: true,
                    route: "model_manager:upsert_spec".to_string(),
                },
                Err(e) => DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("保存模型规格失败: {}", e),
                    success: false,
                    route: "model_manager:upsert_spec".to_string(),
                },
            }
        }
        "delete_spec" => {
            let target = match params
                .get("name")
                .or_else(|| params.get("id"))
                .and_then(|v| v.as_str())
            {
                Some(t) if !t.trim().is_empty() => t.trim(),
                _ => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: "❌ 缺少必填参数 name 或 id (要删除的模型规格)".to_string(),
                        success: false,
                        route: "model_manager:delete_spec".to_string(),
                    }
                }
            };

            let confirm = params
                .get("confirm")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !confirm {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!(
                        "⚠️ 警告：删除规格 [{}] 将影响所有基于该规格引用的模型配置。如确认删除，请再次调用并传入 confirm: true。",
                        target
                    ),
                    success: false,
                    route: "model_manager:delete_spec".to_string(),
                };
            }

            match crate::db::delete_model_spec_record(&conn, target) {
                Ok(count) => {
                    if count > 0 {
                        DispatchResult { view: None,
                            stdout: format!("✅ 已成功删除模型规格 [{}]", target),
                            stderr: String::new(),
                            success: true,
                            route: "model_manager:delete_spec".to_string(),
                        }
                    } else {
                        DispatchResult { view: None,
                            stdout: String::new(),
                            stderr: format!("未找到名称或ID为 [{}] 的模型规格", target),
                            success: false,
                            route: "model_manager:delete_spec".to_string(),
                        }
                    }
                }
                Err(e) => DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("删除模型规格失败: {}", e),
                    success: false,
                    route: "model_manager:delete_spec".to_string(),
                },
            }
        }
        "list_platforms" => {
            let mut stmt = match conn.prepare(
                "SELECT id, name, base_url, created_at FROM platforms ORDER BY created_at DESC",
            ) {
                Ok(s) => s,
                Err(e) => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: format!("查询平台失败: {}", e),
                        success: false,
                        route: "model_manager:list_platforms".to_string(),
                    }
                }
            };

            let platforms: Vec<serde_json::Value> = stmt
                .query_map([], |row| {
                    Ok(serde_json::json!({
                        "id": row.get::<_, String>(0)?,
                        "name": row.get::<_, String>(1)?,
                        "base_url": row.get::<_, String>(2)?,
                        "created_at": row.get::<_, String>(3)?,
                    }))
                })
                .ok()
                .map(|rows| rows.flatten().collect())
                .unwrap_or_default();

            DispatchResult { view: None,
                stdout: serde_json::to_string_pretty(&platforms).unwrap_or_else(|_| "[]".to_string()),
                stderr: String::new(),
                success: true,
                route: "model_manager:list_platforms".to_string(),
            }
        }
        "add_platform" => {
            let name = match params.get("name").and_then(|v| v.as_str()) {
                Some(n) if !n.trim().is_empty() => n.trim().to_string(),
                _ => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: "❌ 缺少必填参数 name (平台名称，如 OpenRouter)".to_string(),
                        success: false,
                        route: "model_manager:add_platform".to_string(),
                    }
                }
            };
            let base_url = match params.get("base_url").and_then(|v| v.as_str()) {
                Some(u) if !u.trim().is_empty() => u.trim().to_string(),
                _ => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: "❌ 缺少必填参数 base_url (API 基础地址，如 https://openrouter.ai/api/v1)".to_string(),
                        success: false,
                        route: "model_manager:add_platform".to_string(),
                    }
                }
            };
            let api_key = if let Some(keys_arr) = params.get("api_keys").and_then(|v| v.as_array()) {
                let list: Vec<String> = keys_arr
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                list.join("\n")
            } else {
                params.get("api_key").and_then(|v| v.as_str()).unwrap_or("").trim().to_string()
            };

            let parsed_keys = crate::llm::parse_api_keys(&api_key);
            let key_info = if parsed_keys.len() > 1 {
                format!("，已配置 {} 个 API Key (支持限流/欠费自动切换轮换)", parsed_keys.len())
            } else {
                String::new()
            };

            let platform_id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().to_rfc3339();

            match conn.execute(
                "INSERT INTO platforms (id, name, base_url, api_key, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![platform_id, name, base_url, api_key, now],
            ) {
                Ok(_) => DispatchResult { view: None,
                    stdout: format!(
                        "✅ 成功添加平台 [{}] (base_url: {}, ID: {}){}",
                        name, base_url, platform_id, key_info
                    ),
                    stderr: String::new(),
                    success: true,
                    route: "model_manager:add_platform".to_string(),
                },
                Err(e) => DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("添加平台失败: {}", e),
                    success: false,
                    route: "model_manager:add_platform".to_string(),
                },
            }
        }
        "update_platform" => {
            let platform_target = match params
                .get("platform_id")
                .or_else(|| params.get("platform_name"))
                .or_else(|| params.get("id"))
                .and_then(|v| v.as_str())
            {
                Some(p) if !p.trim().is_empty() => p.trim(),
                _ => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: "❌ 缺少必填参数 platform_id 或 platform_name (要更新的平台标识)".to_string(),
                        success: false,
                        route: "model_manager:update_platform".to_string(),
                    }
                }
            };

            let platform_row: Result<(String, String, String, String), rusqlite::Error> = conn.query_row(
                "SELECT id, name, base_url, api_key FROM platforms WHERE id = ?1 OR name = ?1 LIMIT 1",
                rusqlite::params![platform_target],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            );

            let (id, cur_name, cur_base_url, cur_api_key) = match platform_row {
                Ok(vals) => vals,
                Err(_) => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: format!("未找到平台 [{}]。请先调用 `list_platforms` 确认已有平台。", platform_target),
                        success: false,
                        route: "model_manager:update_platform".to_string(),
                    }
                }
            };

            let new_name = params.get("name").and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or(cur_name);
            let new_base_url = params.get("base_url").and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or(cur_base_url);
            let new_api_key = if let Some(keys_arr) = params.get("api_keys").and_then(|v| v.as_array()) {
                let list: Vec<String> = keys_arr
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                list.join("\n")
            } else if let Some(k) = params.get("api_key").and_then(|v| v.as_str()) {
                k.trim().to_string()
            } else {
                cur_api_key
            };

            let parsed_keys = crate::llm::parse_api_keys(&new_api_key);
            let key_info = if parsed_keys.len() > 1 {
                format!(" (共配置 {} 个 API Key，支持欠费/限流自动切换)", parsed_keys.len())
            } else {
                String::new()
            };

            match conn.execute(
                "UPDATE platforms SET name = ?1, base_url = ?2, api_key = ?3 WHERE id = ?4",
                rusqlite::params![new_name, new_base_url, new_api_key, id],
            ) {
                Ok(_) => DispatchResult { view: None,
                    stdout: format!(
                        "✅ 成功更新平台 [{}] (base_url: {}, ID: {}){}",
                        new_name, new_base_url, id, key_info
                    ),
                    stderr: String::new(),
                    success: true,
                    route: "model_manager:update_platform".to_string(),
                },
                Err(e) => DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("更新平台失败: {}", e),
                    success: false,
                    route: "model_manager:update_platform".to_string(),
                },
            }
        }
        "add_model" => {
            let platform_target = match params
                .get("platform_id")
                .or_else(|| params.get("platform_name"))
                .and_then(|v| v.as_str())
            {
                Some(p) if !p.trim().is_empty() => p.trim(),
                _ => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: "❌ 缺少必填参数 platform_id 或 platform_name".to_string(),
                        success: false,
                        route: "model_manager:add_model".to_string(),
                    }
                }
            };

            let model_name = match params.get("model_name").and_then(|v| v.as_str()) {
                Some(m) if !m.trim().is_empty() => m.trim().to_string(),
                _ => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: "❌ 缺少必填参数 model_name (实际模型标识)".to_string(),
                        success: false,
                        route: "model_manager:add_model".to_string(),
                    }
                }
            };

            let platform_id_res: Result<String, String> = conn
                .query_row(
                    "SELECT id FROM platforms WHERE id = ?1 OR name = ?1 LIMIT 1",
                    rusqlite::params![platform_target],
                    |row| row.get(0),
                )
                .map_err(|_| format!("未找到平台 [{}]。`add_model` 只能向已有平台添加模型；请先调用 `list_platforms` 确认已有平台，或先调用 `add_platform` 创建平台。", platform_target));

            let platform_id = match platform_id_res {
                Ok(id) => id,
                Err(e) => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: e,
                        success: false,
                        route: "model_manager:add_model".to_string(),
                    }
                }
            };

            let spec_name = params.get("spec_name").and_then(|v| v.as_str());
            let (mut cw, mut mt, mut temp) = (128000, 8192, 0.7);

            if let Some(sn) = spec_name {
                if let Ok((spec_cw, spec_mt, spec_temp)) = conn.query_row(
                    "SELECT context_window, max_tokens, default_temperature FROM model_specs WHERE name = ?1 OR id = ?1",
                    rusqlite::params![sn],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, f64>(2)?)),
                ) {
                    cw = spec_cw;
                    mt = spec_mt;
                    temp = spec_temp;
                }
            }

            if let Some(val) = params.get("context_window").and_then(|v| v.as_i64()) {
                cw = val;
            }
            if let Some(val) = params.get("max_tokens").and_then(|v| v.as_i64()) {
                mt = val;
            }
            if let Some(val) = params.get("temperature").and_then(|v| v.as_f64()) {
                temp = val;
            }

            let display_name = params
                .get("display_name")
                .and_then(|v| v.as_str())
                .unwrap_or(&model_name)
                .to_string();

            let model_id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().to_rfc3339();

            match conn.execute(
                "INSERT INTO models (id, platform_id, name, display_name, context_window, max_tokens, temperature, enabled, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8)",
                rusqlite::params![model_id, platform_id, model_name, display_name, cw, mt, temp, now],
            ) {
                Ok(_) => DispatchResult { view: None,
                    stdout: format!(
                        "✅ 成功将模型 [{}] 添加至平台 [{}] (ID: {}, 上下文窗口: {}, 最大Tokens: {})",
                        model_name, platform_target, model_id, cw, mt
                    ),
                    stderr: String::new(),
                    success: true,
                    route: "model_manager:add_model".to_string(),
                },
                Err(e) => DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("添加模型失败: {}", e),
                    success: false,
                    route: "model_manager:add_model".to_string(),
                },
            }
        }
        "delete_model" => {
            let target = match params
                .get("model_id")
                .or_else(|| params.get("model_name"))
                .and_then(|v| v.as_str())
            {
                Some(m) if !m.trim().is_empty() => m.trim(),
                _ => {
                    return DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: "❌ 缺少必填参数 model_id 或 model_name".to_string(),
                        success: false,
                        route: "model_manager:delete_model".to_string(),
                    }
                }
            };

            let confirm = params
                .get("confirm")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !confirm {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!(
                        "⚠️ 警告：从渠道删除模型 [{}] 将无法在对话中选择该模型。如确认删除，请再次调用并传入 confirm: true。",
                        target
                    ),
                    success: false,
                    route: "model_manager:delete_model".to_string(),
                };
            }

            match conn.execute(
                "DELETE FROM models WHERE id = ?1 OR name = ?1",
                rusqlite::params![target],
            ) {
                Ok(count) => {
                    if count > 0 {
                        DispatchResult { view: None,
                            stdout: format!("✅ 已成功删除模型 [{}] (受影响行数: {})", target, count),
                            stderr: String::new(),
                            success: true,
                            route: "model_manager:delete_model".to_string(),
                        }
                    } else {
                        DispatchResult { view: None,
                            stdout: String::new(),
                            stderr: format!("未找到标识为 [{}] 的模型", target),
                            success: false,
                            route: "model_manager:delete_model".to_string(),
                        }
                    }
                }
                Err(e) => DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("删除模型失败: {}", e),
                    success: false,
                    route: "model_manager:delete_model".to_string(),
                },
            }
        }
        _ => DispatchResult { view: None,
            stdout: String::new(),
            stderr: format!(
                "❌ 未知的 model_manager 动作 action='{}'。\n💡 支持的常用动作及范例:\n  - 查平台与模型: {{\"tool\": \"model_manager\", \"action\": \"list_platforms\"}}\n  - 添加平台: {{\"tool\": \"model_manager\", \"action\": \"add_platform\", \"params\": {{\"name\": \"OpenAI\", \"base_url\": \"https://api.openai.com/v1\", \"api_key\": \"sk-...\"}}}}\n  - 添加模型: {{\"tool\": \"model_manager\", \"action\": \"add_model\", \"params\": {{\"platform\": \"OpenAI\", \"model_id\": \"gpt-4o\"}}}}\n  (其余支持动作: list_specs, upsert_spec, delete_spec, update_platform, delete_model)",
                act
            ),
            success: false,
            route: "model_manager:unknown".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_state() -> (std::path::PathBuf, DbState) {
        let base =
            std::env::temp_dir().join(format!("free-api-model-manager-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let conn = crate::db::init_db(&base.join("test.db")).unwrap();
        (
            base,
            DbState(std::sync::Arc::new(std::sync::Mutex::new(conn))),
        )
    }

    #[test]
    fn adding_model_to_missing_platform_returns_recoverable_next_action() {
        let (base, state) = test_state();
        let result = dispatch(
            &state,
            "add_model",
            &json!({
                "platform_name": "not-configured",
                "model_name": "test-model",
            }),
        );

        assert!(!result.success);
        assert!(result.stderr.contains("未找到平台 [not-configured]"));
        assert!(result.stderr.contains("list_platforms"));
        assert!(result.stderr.contains("add_platform"));
        assert!(!result.stderr.contains("Query returned no rows"));
        drop(state);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn adding_platform_then_model_satisfies_the_platform_dependency() {
        let (base, state) = test_state();
        let platform = dispatch(
            &state,
            "add_platform",
            &json!({
                "name": "test-provider",
                "base_url": "https://example.invalid/v1",
                "api_keys": ["test-key-1", "test-key-2"],
            }),
        );
        assert!(platform.success, "{}", platform.stderr);
        assert!(platform.stdout.contains("2 个 API Key"));

        let update_res = dispatch(
            &state,
            "update_platform",
            &json!({
                "platform_name": "test-provider",
                "api_keys": ["test-key-new1", "test-key-new2", "test-key-new3"],
            }),
        );
        assert!(update_res.success, "{}", update_res.stderr);
        assert!(update_res.stdout.contains("3 个 API Key"));

        let model = dispatch(
            &state,
            "add_model",
            &json!({
                "platform_name": "test-provider",
                "model_name": "test-model",
            }),
        );
        assert!(model.success, "{}", model.stderr);
        assert_eq!(model.route, "model_manager:add_model");
        drop(state);
        let _ = std::fs::remove_dir_all(base);
    }
}
