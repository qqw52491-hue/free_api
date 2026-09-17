//! LLM 请求基础设施
//!
//! 提供统一的模型配置、HTTP 请求构建、SSE 流式解析等公共能力，
//! 供 Agent (call_llm) 和 Chat (send_chat) 两条路径共用。

use crate::db::DbState;
use futures_util::StreamExt;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use tauri::State;
use tokio::sync::watch;

/// 全局多 Key 负载均衡轮询计数器（每次新请求顺序轮换起始 Key，把高并发打散到各个 Key，避免单一 Key 触发 RPM/并发限流）
static GLOBAL_KEY_REQUEST_ROTATOR: AtomicUsize = AtomicUsize::new(0);


// ========================================================================
// ModelConfig — 从数据库解析的模型配置，含平台适配参数
// ========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// SQLite `models.id`：路由唯一标识，避免同名模型跨平台混淆。
    pub model_id: String,
    /// SQLite `platforms.name`：用于日志和评测报告中的实际来源审计。
    pub platform_name: String,
    pub base_url: String,
    pub api_key: String,
    pub model_name: String,
    pub max_tokens: i64,
    pub temperature: f64,
    pub is_ollama: bool,
    pub is_lmstudio: bool,
    // is_local 可用于未来扩展，目前通过 is_ollama/is_lmstudio 已足够
    // pub is_local: bool,
    pub context_window: i64,
    /// 模型级 provider capability profile（JSON）。空对象表示无额外厂商字段。
    pub provider_capabilities: String,
    pub timeout_secs: u64,
    /// 消息角色规约：-1 未设置（按 base_url 推断）、0 显式接受连续同角色、
    /// 1 显式要求 user/assistant 交替（发送前归并连续同角色消息）。
    pub requires_alternating_roles: i64,
}

impl ModelConfig {
    /// 从数据库查询模型+平台信息，自动检测本地模型类型
    pub fn from_db(state: &State<'_, DbState>, model_id: &str) -> Result<Self, String> {
        Self::from_db_state(state.inner(), model_id)
    }

    pub fn from_db_state(db: &DbState, model_id: &str) -> Result<Self, String> {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        Self::from_conn(&conn, model_id)
    }

    pub fn from_conn(conn: &rusqlite::Connection, model_id: &str) -> Result<Self, String> {
        // 自愈：滚动迁移可能在旧库或并发启动时尚未补齐。每种缺列只尝试一次，
        // 并保留原始查询错误，避免把数据库故障静默吞掉。
        for _ in 0..=2 {
            match Self::query_model_row(conn, model_id) {
                Ok(result) => return Ok(result),
                Err(error) if error.contains("no such column: m.provider_capabilities") => {
                    let _ = conn.execute(
                        "ALTER TABLE models ADD COLUMN provider_capabilities TEXT NOT NULL DEFAULT '{}';",
                        [],
                    );
                }
                Err(error) if error.contains("no such column: p.requires_alternating_roles") => {
                    let _ = conn.execute(
                        "ALTER TABLE platforms ADD COLUMN requires_alternating_roles INTEGER NOT NULL DEFAULT -1;",
                        [],
                    );
                }
                Err(error) => return Err(error),
            }
        }
        Self::query_model_row(conn, model_id)
    }

    fn query_model_row(conn: &rusqlite::Connection, model_id: &str) -> Result<Self, String> {
        let (
            model_id,
            platform_name,
            base_url,
            api_key,
            model_name,
            max_tokens,
            temperature,
            context_window_db,
            provider_capabilities_db,
            requires_alternating_roles_db,
        ) = {
            let mut stmt = conn
                .prepare(
                    "SELECT m.id, p.name, p.base_url, p.api_key, m.name, m.max_tokens, m.temperature, m.context_window, m.provider_capabilities, p.requires_alternating_roles
                     FROM models m JOIN platforms p ON p.id = m.platform_id
                     WHERE m.id = ?1",
                )
                .map_err(|e| e.to_string())?;
            stmt.query_row(params![model_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, f64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, i64>(9)?,
                ))
            })
            .map_err(|e| format!("模型不存在: {}", e))?
        };

        let is_ollama =
            base_url.contains("localhost:11434") || base_url.contains("127.0.0.1:11434");
        let is_lmstudio = base_url.contains("localhost:1234")
            || base_url.contains("127.0.0.1:1234")
            || base_url.contains("lmstudio");
        let is_local = base_url.contains("localhost") || base_url.contains("127.0.0.1");

        // 用户配置的 context_window 优先；未配置（0）时按类型使用保守默认值
        let context_window = if context_window_db > 0 {
            context_window_db
        } else if is_ollama || is_lmstudio {
            32768
        } else {
            128000
        };
        let timeout_secs = if is_local { 300 } else { 120 };

        Ok(Self {
            model_id,
            platform_name,
            base_url,
            api_key,
            model_name,
            max_tokens,
            temperature,
            is_ollama,
            is_lmstudio,
            context_window,
            provider_capabilities: provider_capabilities_db,
            timeout_secs,
            requires_alternating_roles: requires_alternating_roles_db,
        })
    }

    /// 构建 reqwest HTTP 客户端
    pub fn build_client(&self) -> Result<reqwest::Client, String> {
        static HTTP_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

        // 我们在全局复用连接池，至于 per-request 的 timeout 可以交给调用者 req_builder.timeout() 设置
        // 但这里为了向后兼容且兼顾连接池，我们优先保留一个超时较长（或默认）的共享客户端。
        let client = HTTP_CLIENT.get_or_init(|| {
            reqwest::Client::builder()
                // 设置一个最长允许的超时，实际单次请求可以在 send 之前用 timeout(std::time::Duration::from_secs(self.timeout_secs)) 覆盖
                .timeout(std::time::Duration::from_secs(300))
                .connect_timeout(std::time::Duration::from_secs(15))
                .tcp_keepalive(Some(std::time::Duration::from_secs(15)))
                .tcp_nodelay(true)
                .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36")
                .pool_idle_timeout(Some(std::time::Duration::from_secs(90)))
                .build()
                .expect("Failed to build global HTTP client")
        });
        Ok(client.clone())
    }

    /// 构建 OpenAI 兼容的请求体
    pub fn build_body(
        &self,
        messages: &[serde_json::Value],
        reasoning_effort: &str,
    ) -> serde_json::Value {
        let local_temp = if self.temperature == 0.0 {
            0.6
        } else {
            self.temperature
        };
        let (use_temp, use_top_p) = if self.is_ollama {
            (0.0, 0.9)
        } else if self.is_lmstudio {
            (local_temp, 0.95)
        } else {
            (self.temperature, 1.0)
        };

        let effective_max_tokens = if self.max_tokens < 8192 {
            8192
        } else {
            self.max_tokens
        };

        let mut body = json!({
            "model": self.model_name,
            "messages": messages,
            "max_tokens": effective_max_tokens,
            "temperature": use_temp,
            "top_p": use_top_p,
            "stream": true,
        });

        // 兼容支持 max_completion_tokens 的现代模型 (如 o1, o3-mini, gpt-4o 等)
        if self.model_name.contains("o1")
            || self.model_name.contains("o3")
            || self.model_name.contains("gpt-4o")
        {
            body["max_completion_tokens"] = json!(effective_max_tokens);
        }

        // stream_options 只有 OpenAI/OpenRouter 支持，本地模型不加
        if !self.is_ollama && !self.is_lmstudio {
            body["stream_options"] = json!({ "include_usage": true });
        }

        // 1. 如果模型自身已配置了结构化的 provider_capabilities，优先应用自定义配置
        let has_custom_profile = self.apply_provider_capabilities(&mut body, reasoning_effort);

        // 2. 多厂商参数过滤与规范化层（Model Parameter Normalizer）：
        // 基于家族指纹识别（Kimi、OpenAI o系列、DeepSeek R1、Claude等）对请求体执行参数白名单清洗。
        // 若未配自定义画像，或属于严格敏感采样模型（如 Kimi-K3），确保敏感采样参数被剥离，防止 400 Bad Request
        let family = crate::agent::model_filter::detect_model_family(&self.model_name, &self.base_url);
        if !has_custom_profile || family == crate::agent::model_filter::ModelFamily::KimiReasoning {
            crate::agent::model_filter::sanitize_and_normalize_body(
                &mut body,
                family,
                reasoning_effort,
                effective_max_tokens,
            );
        }

        // Ollama 需要通过 options 注入 num_ctx 与 num_predict
        if self.is_ollama {
            body["options"] = json!({
                "num_ctx": self.context_window.max(32768),
                "num_predict": effective_max_tokens,
                "temperature": 0.0,
                "top_p": 0.9,
            });
        }

        body
    }

    /// 将模型级 capability profile 映射到 OpenAI 兼容请求字段。
    /// profile 缺失、不是对象或字段无效时安全降级为通用请求，返回 false；成功应用返回 true。
    pub fn apply_provider_capabilities(&self, body: &mut Value, preference: &str) -> bool {
        let profile: Value = match serde_json::from_str::<Value>(&self.provider_capabilities) {
            Ok(value) if value.is_object() => value,
            Ok(_) => return false,
            Err(_) => return false,
        };
        if profile.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            return false;
        }
        // 平台级 API 格式：如火山方舟需要 response_format: json_object 强制 JSON 输出模式。
        if let Some(response_format) = profile.get("response_format") {
            if let Some(object) = body.as_object_mut() {
                object.insert("response_format".to_string(), response_format.clone());
            }
        }

        let Some(thinking) = profile.get("thinking").and_then(Value::as_object) else {
            return true;
        };
        let field = thinking
            .get("field")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("thinking");
        let force_enabled = thinking
            .get("force_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let enabled = force_enabled || preference != "off";
        let value_key = if enabled {
            "enabled_value"
        } else {
            "disabled_value"
        };
        if let Some(value) = thinking.get(value_key) {
            if let Some(object) = body.as_object_mut() {
                object.insert(field.to_string(), value.clone());
            }
        }
        if !enabled {
            return true;
        }
        if thinking
            .get("remove_sampling_when_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            if let Some(object) = body.as_object_mut() {
                object.remove("temperature");
                object.remove("top_p");
            }
        }
        let Some(effort) = thinking.get("effort").and_then(Value::as_object) else {
            return true;
        };
        let effort_field = effort
            .get("field")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("reasoning_effort");
        let mapped = effort
            .get("map")
            .and_then(Value::as_object)
            .and_then(|mapping| mapping.get(preference).or_else(|| mapping.get("default")))
            .cloned()
            .or_else(|| effort.get("default").cloned());
        if let Some(value) = mapped {
            if let Some(object) = body.as_object_mut() {
                object.insert(effort_field.to_string(), value);
            }
        }
        true
    }

    /// 火山方舟 OpenAI 兼容 Chat 端点。

    /// 供评测与诊断日志持久化的脱敏基础地址。
    /// 移除查询串、片段与可能嵌入 URL 的用户信息，绝不记录 API Key。
    pub fn redacted_base_url(&self) -> String {
        let without_query = self.base_url.split('?').next().unwrap_or(&self.base_url);
        let without_fragment = without_query.split('#').next().unwrap_or(without_query);
        if let Some((scheme, rest)) = without_fragment.split_once("://") {
            let host_and_path = rest
                .rsplit_once('@')
                .map(|(_, host_and_path)| host_and_path)
                .unwrap_or(rest);
            format!("{}://{}", scheme, host_and_path)
        } else {
            without_fragment.to_string()
        }
    }
    /// API 端点 URL
    pub fn endpoint_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// 解析平台中配置的所有可用 API Key 列表（支持换行、逗号、分号、JSON 数组等分隔）
    pub fn api_keys(&self) -> Vec<String> {
        parse_api_keys(&self.api_key)
    }

    /// 构建认证 HeaderMap（使用首个可用 Key）
    pub fn auth_headers(&self) -> reqwest::header::HeaderMap {
        let first_key = self.api_keys().into_iter().next().unwrap_or_else(|| self.api_key.clone());
        self.auth_headers_for_key(&first_key)
    }

    /// 为指定的 API Key 构建认证 HeaderMap
    pub fn auth_headers_for_key(&self, key: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        let clean_key = key.trim();
        if !clean_key.is_empty() {
            let auth_val = if clean_key.starts_with("Bearer ") {
                clean_key.to_string()
            } else {
                format!("Bearer {}", clean_key)
            };
            headers.insert(
                "Authorization",
                auth_val
                    .parse()
                    .unwrap_or_else(|_| "Bearer invalid".parse().unwrap()),
            );
        }
        headers.insert("HTTP-Referer", "https://free-api-chat.app".parse().unwrap());
        headers.insert("X-OpenRouter-Title", "Free API Chat".parse().unwrap());
        headers
    }

    /// 打印平台检测日志
    pub fn log_platform_info(&self) {
        let keys = self.api_keys();
        if keys.len() > 1 {
            println!(
                "🔑 平台 [{}] 配置了 {} 个 API Key (已启用故障转移/自动轮换机制)",
                self.platform_name,
                keys.len()
            );
        }
        if self.is_ollama {
            println!(
                "🔓 检测到 Ollama 本地模型，已自动注入 num_ctx: {}",
                self.context_window
            );
        }
        if self.is_lmstudio {
            println!(
                "🔓 检测到 LM Studio 本地模型，context_window={}, timeout={}s",
                self.context_window, self.timeout_secs
            );
        }
    }
}

/// 解析文本中的所有 API Key 项（支持对象数组结构 [{"key": "...", "enabled": true}]）
mod api_keys;
mod sse;
mod stream;
mod retry;

pub(crate) use api_keys::*;
pub(crate) use sse::*;
pub(crate) use stream::*;
pub(crate) use retry::*;


/// 当 API 未返回 usage 时，粗略估算 Token 用量 (1 token ≈ 3 字符)
pub fn estimate_usage(
    messages: &[serde_json::Value],
    response_len: usize,
    _context_window: i64,
) -> (i64, i64) {
    let est_prompt = (messages
        .iter()
        .map(|m| serde_json::to_string(m).unwrap_or_default().len())
        .sum::<usize>()
        / 3) as i64;
    let est_completion = (response_len / 3) as i64;
    (est_prompt, est_completion)
}

#[cfg(test)]
mod tests;
