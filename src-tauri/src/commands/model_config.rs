//! Portable LLM configuration. Preview never writes; import is additive and atomic.
use crate::db::DbState;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub version: u32,
    pub platforms: Vec<ConfigPlatform>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ConfigPlatform {
    pub name: String,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default)]
    pub models: Vec<ConfigModel>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ConfigModel {
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: i64,
    #[serde(default = "default_temperature")]
    pub temperature: f64,
    #[serde(default)]
    pub context_window: i64,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_capabilities")]
    pub provider_capabilities: Value,
}

fn default_max_tokens() -> i64 {
    4096
}
fn default_temperature() -> f64 {
    0.7
}
fn default_enabled() -> bool {
    true
}
fn default_capabilities() -> Value {
    serde_json::json!({})
}

#[derive(Default, Serialize)]
pub struct ConfigImportResult {
    pub added_platforms: usize,
    pub reused_platforms: usize,
    pub added_models: usize,
    pub skipped_models: usize,
    pub platforms_without_key: usize,
}

fn validate_config(mut config: ModelConfig) -> Result<ModelConfig, String> {
    if config.version != 1 {
        return Err("不支持此配置版本，当前仅支持 version: 1".into());
    }
    if config.platforms.is_empty() {
        return Err("请至少选择一个平台".into());
    }
    let mut platforms = std::collections::HashSet::new();
    for (index, platform) in config.platforms.iter_mut().enumerate() {
        let label = format!("第 {} 个平台", index + 1);
        platform.name = platform.name.trim().to_string();
        platform.base_url = platform.base_url.trim().trim_end_matches('/').to_string();
        if platform.name.is_empty() {
            return Err(format!("{label}缺少平台名称 name"));
        }
        let url = reqwest::Url::parse(&platform.base_url)
            .map_err(|_| format!("{label}的 base_url 必须是完整 HTTP/HTTPS 地址"))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(format!("{label}的 base_url 必须是完整 HTTP/HTTPS 地址"));
        }
        if !platforms.insert((platform.name.clone(), platform.base_url.clone())) {
            return Err(format!(
                "{label}与前面的平台名称和接口地址重复，请合并后导入"
            ));
        }
        let mut names = std::collections::HashSet::new();
        for (model_index, model) in platform.models.iter_mut().enumerate() {
            let label = format!("{label}的第 {} 个模型", model_index + 1);
            model.name = model.name.trim().to_string();
            if model.name.is_empty() || !names.insert(model.name.clone()) {
                return Err(format!("{label}的 name 为空或重复"));
            }
            if model.display_name.trim().is_empty() {
                model.display_name = model.name.clone();
            }
            if model.max_tokens <= 0 || model.context_window < 0 || !model.temperature.is_finite() {
                return Err(format!("{label}参数错误：max_tokens 须大于 0，context_window 须不小于 0，temperature 须为有限数值"));
            }
            if !model.provider_capabilities.is_object() {
                return Err(format!("{label}的 provider_capabilities 必须是 JSON 对象"));
            }
        }
    }
    Ok(config)
}

#[tauri::command]
pub async fn preview_model_config(json: String) -> Result<ModelConfig, String> {
    // Do not echo arbitrary input (which can contain credentials) in errors.
    let mut deserializer = serde_json::Deserializer::from_str(json.trim_start_matches('\u{feff}'));
    let config = serde_path_to_error::deserialize(&mut deserializer).map_err(|error| {
        format!(
            "配置 JSON 格式错误（字段 {}）；请使用 version、platforms 和 models 结构",
            error.path()
        )
    })?;
    deserializer
        .end()
        .map_err(|_| "JSON 末尾包含多余内容".to_string())?;
    validate_config(config)
}

fn export_config(conn: &Connection, include_api_keys: bool) -> Result<ModelConfig, String> {
    let mut statement = conn
        .prepare("SELECT id, name, base_url, api_key FROM platforms ORDER BY name, base_url, id")
        .map_err(|e| e.to_string())?;
    let platforms = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let mut result = Vec::new();
    for (id, name, base_url, key) in platforms {
        let mut statement = conn.prepare("SELECT name, display_name, max_tokens, temperature, context_window, enabled, provider_capabilities FROM models WHERE platform_id = ?1 ORDER BY name, id")
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([id], |row| {
                Ok((
                    ConfigModel {
                        name: row.get(0)?,
                        display_name: row.get(1)?,
                        max_tokens: row.get(2)?,
                        temperature: row.get(3)?,
                        context_window: row.get(4)?,
                        enabled: row.get::<_, i64>(5)? == 1,
                        provider_capabilities: Value::Null,
                    },
                    row.get::<_, String>(6)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut models = Vec::new();
        for row in rows {
            let (mut model, raw) = row.map_err(|e| e.to_string())?;
            model.provider_capabilities = serde_json::from_str(&raw)
                .map_err(|_| "已有模型的能力配置不是合法 JSON，请先在模型管理中修正".to_string())?;
            models.push(model);
        }
        result.push(ConfigPlatform {
            name,
            base_url,
            api_key: include_api_keys.then_some(key),
            models,
        });
    }
    Ok(ModelConfig {
        version: 1,
        platforms: result,
    })
}

#[tauri::command]
pub async fn export_model_config(
    state: State<'_, DbState>,
    include_api_keys: bool,
) -> Result<ModelConfig, String> {
    // One connection/transaction keeps platform and model lists from different versions from mixing.
    let mut conn = state.0.lock().map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let result = export_config(&tx, include_api_keys)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(result)
}

fn import_config(conn: &mut Connection, config: ModelConfig) -> Result<ConfigImportResult, String> {
    let config = validate_config(config)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let now = Utc::now().to_rfc3339();
    let mut result = ConfigImportResult::default();
    for platform in config.platforms {
        let matches: Vec<(String, String)> = {
            let mut stmt = tx.prepare("SELECT id, api_key FROM platforms WHERE trim(name) = ?1 AND rtrim(trim(base_url), '/') = ?2 ORDER BY id")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![platform.name, platform.base_url], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
        };
        if matches.len() > 1 {
            return Err("本地存在多个同名且接口地址相同的平台，无法确定导入目标；本次未写入，请先整理重复平台".into());
        }
        let (platform_id, effective_key) = if let Some((id, key)) = matches.into_iter().next() {
            result.reused_platforms += 1;
            (id, key) // Existing settings and credentials are never overwritten.
        } else {
            let id = Uuid::new_v4().to_string();
            let key = platform.api_key.unwrap_or_default();
            tx.execute("INSERT INTO platforms (id, name, base_url, api_key, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, platform.name, platform.base_url, key, now]).map_err(|e| e.to_string())?;
            result.added_platforms += 1;
            (id, key)
        };
        if effective_key.trim().is_empty() {
            result.platforms_without_key += 1;
        }
        for model in platform.models {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT id FROM models WHERE platform_id = ?1 AND name = ?2 LIMIT 1",
                    params![platform_id, model.name],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if existing.is_some() {
                result.skipped_models += 1;
                continue;
            }
            tx.execute("INSERT INTO models (id, platform_id, name, display_name, max_tokens, temperature, context_window, enabled, provider_capabilities, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![Uuid::new_v4().to_string(), platform_id, model.name, model.display_name,
                    model.max_tokens, model.temperature, model.context_window, model.enabled,
                    model.provider_capabilities.to_string(), now]).map_err(|e| e.to_string())?;
            result.added_models += 1;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(result)
}

#[tauri::command]
pub async fn import_model_config(
    state: State<'_, DbState>,
    config: ModelConfig,
) -> Result<ConfigImportResult, String> {
    let mut conn = state.0.lock().map_err(|e| e.to_string())?;
    import_config(&mut conn, config)
}

#[tauri::command]
pub async fn save_model_config_file(
    config: ModelConfig,
    target_path: String,
) -> Result<(), String> {
    // Write JSON as text, without the artifact exporter's data-URL/base64 interpretation.
    let bytes = serde_json::to_vec_pretty(&config).map_err(|_| "配置序列化失败".to_string())?;
    tokio::fs::write(target_path, bytes)
        .await
        .map_err(|e| format!("保存配置失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> Connection {
        crate::db::init_db(&std::path::PathBuf::from(":memory:")).unwrap()
    }

    fn config() -> ModelConfig {
        serde_json::from_value(serde_json::json!({
            "version": 1,
            "platforms": [{"name":"测试平台", "base_url":"https://example.test/v1/",
                "api_key":"[{\"key\":\"secret-one\",\"enabled\":false},{\"key\":\"secret-two\",\"enabled\":true}]",
                "models":[{"name":"model-a", "display_name":"模型 A", "enabled":false,
                    "max_tokens":8192, "context_window":128000, "temperature":0.2,
                    "provider_capabilities":{"stream":false, "extra_body":{"thinking":{"type":"enabled"}}}},
                    {"name":"model-b"}]}]
        })).unwrap()
    }

    #[test]
    fn model_config_roundtrip_preserves_settings_and_omits_keys_by_default() {
        let mut source = database();
        let result = import_config(&mut source, config()).unwrap();
        assert_eq!((result.added_platforms, result.added_models), (1, 2));
        let public = serde_json::to_string(&export_config(&source, false).unwrap()).unwrap();
        assert!(!public.contains("secret-one"));
        assert!(!public.contains("api_key"));
        assert!(!public.contains("latency_ms"));
        assert!(!public.contains("platform_id"));
        let private = export_config(&source, true).unwrap();
        let original = serde_json::to_value(&private).unwrap();
        let mut target = database();
        import_config(&mut target, private).unwrap();
        assert_eq!(
            serde_json::to_value(export_config(&target, true).unwrap()).unwrap(),
            original
        );
        assert_eq!(
            target
                .query_row("SELECT status FROM models LIMIT 1", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "unknown"
        );
    }

    #[test]
    fn model_config_reimport_skips_models_and_preserves_existing_credentials() {
        let mut db = database();
        import_config(&mut db, config()).unwrap();
        let mut second = config();
        second.platforms[0].api_key = Some("replacement-must-not-be-used".into());
        second.platforms[0].models[0].max_tokens = 123;
        let extra = ConfigModel {
            name: "model-c".into(),
            ..second.platforms[0].models[1].clone()
        };
        second.platforms[0].models.push(extra);
        let result = import_config(&mut db, second).unwrap();
        assert_eq!(
            (
                result.added_platforms,
                result.reused_platforms,
                result.added_models,
                result.skipped_models
            ),
            (0, 1, 1, 2)
        );
        let stored = export_config(&db, true).unwrap();
        assert!(stored.platforms[0]
            .api_key
            .as_ref()
            .unwrap()
            .contains("secret-one"));
        assert_eq!(
            stored.platforms[0]
                .models
                .iter()
                .find(|m| m.name == "model-a")
                .unwrap()
                .max_tokens,
            8192
        );
    }

    #[test]
    fn model_config_missing_key_and_platform_only_import_are_supported() {
        let mut db = database();
        let mut input = config();
        input.platforms[0].api_key = None;
        input.platforms[0].models.clear();
        let result = import_config(&mut db, input).unwrap();
        assert_eq!(
            (
                result.added_platforms,
                result.added_models,
                result.platforms_without_key
            ),
            (1, 0, 1)
        );
        assert_eq!(
            export_config(&db, true).unwrap().platforms[0]
                .api_key
                .as_deref(),
            Some("")
        );
    }

    #[tokio::test]
    async fn model_config_preview_checks_format_defaults_bom_and_does_not_echo_secrets() {
        let input = "\u{feff}{\"version\":1,\"platforms\":[{\"name\":\"Local\",\"base_url\":\"http://localhost:11434/v1\",\"models\":[{\"name\":\"small\"}]}]}";
        let parsed = preview_model_config(input.into()).await.unwrap();
        let model = &parsed.platforms[0].models[0];
        assert_eq!(model.display_name, "small");
        assert_eq!(
            (model.max_tokens, model.context_window, model.enabled),
            (4096, 0, true)
        );
        assert!(preview_model_config(format!("{input} trailing"))
            .await
            .is_err());
        let wrong = "{\"version\":1,\"platforms\":[{\"name\":\"Local\",\"base_url\":\"http://localhost\",\"api_key\":true}]}";
        assert!(preview_model_config(wrong.into())
            .await
            .err()
            .unwrap()
            .contains("api_key"));
        let wrong = "{\"version\":\"TOP-SECRET\",\"platforms\":[]}";
        assert!(!preview_model_config(wrong.into())
            .await
            .err()
            .unwrap()
            .contains("TOP-SECRET"));
    }

    #[test]
    fn model_config_rejects_invalid_input_before_writing_any_platform() {
        let mut db = database();
        let mut invalid = config();
        invalid.platforms.push(ConfigPlatform {
            name: "bad".into(),
            base_url: "file:///tmp/local".into(),
            api_key: None,
            models: vec![],
        });
        assert!(import_config(&mut db, invalid).is_err());
        assert!(export_config(&db, true).unwrap().platforms.is_empty());
        let mut invalid = config();
        invalid.version = 2;
        assert!(import_config(&mut db, invalid).is_err());
        let mut invalid = config();
        invalid.platforms[0].models[0].provider_capabilities = Value::String("{}".into());
        assert!(import_config(&mut db, invalid).is_err());
        let mut invalid = config();
        let duplicate = invalid.platforms[0].models[0].clone();
        invalid.platforms[0].models.push(duplicate);
        assert!(import_config(&mut db, invalid).is_err());
    }

    #[test]
    fn model_config_database_error_rolls_back_earlier_insertions() {
        let mut db = database();
        db.execute_batch("CREATE TRIGGER reject_second_model BEFORE INSERT ON models WHEN NEW.name = 'model-b' BEGIN SELECT RAISE(ABORT, 'test failure'); END;").unwrap();
        assert!(import_config(&mut db, config()).is_err());
        assert!(export_config(&db, true).unwrap().platforms.is_empty());
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM models", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn model_config_ambiguous_local_platform_is_not_chosen_arbitrarily() {
        let mut db = database();
        import_config(&mut db, config()).unwrap();
        db.execute("INSERT INTO platforms (id, name, base_url, api_key, created_at) SELECT 'duplicate', name, base_url, 'different-key', created_at FROM platforms LIMIT 1", []).unwrap();
        assert!(import_config(&mut db, config()).is_err());
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM models", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn model_config_save_writes_literal_json_including_data_url_text() {
        let path = std::env::temp_dir().join(format!("模型配置-{}.json", Uuid::new_v4()));
        let mut input = config();
        input.platforms[0].models[0].provider_capabilities =
            serde_json::json!({"sample":"data:image/png;base64,AAAA"});
        let expected = serde_json::to_value(&input).unwrap();
        save_model_config_file(input, path.to_string_lossy().to_string())
            .await
            .unwrap();
        let stored = tokio::fs::read(&path).await.unwrap();
        tokio::fs::remove_file(&path).await.unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&stored).unwrap(), expected);
    }
}
