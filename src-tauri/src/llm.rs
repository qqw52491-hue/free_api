//! LLM 请求基础设施
//!
//! 提供统一的模型配置、HTTP 请求构建、SSE 流式解析等公共能力，
//! 供 Agent (call_llm) 和 Chat (send_chat) 两条路径共用。

use crate::db::DbState;
use futures_util::StreamExt;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::State;
use tokio::sync::watch;

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
        // 火山方舟 Chat API 原生支持 response_format；以请求级协议确保最终正文为合法 JSON。
        // 这不改变任务提示、工具参数或模型路由，仅约束机器可解析的外层输出。
        self.apply_provider_capabilities(&mut body, reasoning_effort);
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
    /// profile 缺失、不是对象或字段无效时安全降级为通用请求，不按模型名猜测供应商能力。
    pub fn apply_provider_capabilities(&self, body: &mut Value, preference: &str) {
        let profile: Value = match serde_json::from_str::<Value>(&self.provider_capabilities) {
            Ok(value) if value.is_object() => value,
            Ok(_) => {
                eprintln!(
                    "⚠️ 模型 {} 的 provider_capabilities 不是对象，已忽略",
                    self.model_id
                );
                return;
            }
            Err(error) => {
                eprintln!(
                    "⚠️ 模型 {} 的 provider_capabilities 无法解析，已忽略: {}",
                    self.model_id, error
                );
                return;
            }
        };
        // 平台级 API 格式：如火山方舟需要 response_format: json_object 强制 JSON 输出模式。
        // 之前按 URL contains("volces.com") 硬编码，现在由 profile 声明。
        if let Some(response_format) = profile.get("response_format") {
            if let Some(object) = body.as_object_mut() {
                object.insert("response_format".to_string(), response_format.clone());
            }
        }

        let Some(thinking) = profile.get("thinking").and_then(Value::as_object) else {
            return;
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
            return;
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
            return;
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

    /// 构建认证 HeaderMap
    pub fn auth_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        let clean_key = self.api_key.trim();
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
        headers.insert("HTTP-Referer", "https://free-api-chat.app".parse().unwrap());
        headers.insert("X-OpenRouter-Title", "Free API Chat".parse().unwrap());
        headers
    }

    /// 打印平台检测日志
    pub fn log_platform_info(&self) {
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

// ========================================================================
// StreamEvent — SSE 流式事件，供回调使用
// ========================================================================

/// SSE 流中解析出的事件类型
pub enum StreamEvent<'a> {
    /// 思考/推理内容
    Thinking {
        chunk: &'a str,
        accumulated: &'a str,
    },
    /// 正常内容
    Content {
        chunk: &'a str,
        #[allow(dead_code)]
        accumulated_content: &'a str,
        full_thinking: &'a str,
    },
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RawUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
}

pub struct StreamResult {
    pub full_response: String,
    pub thinking_text: String,
    pub usage: Option<RawUsage>,
    pub ttft_ms: u64,
    pub total_duration_ms: u64,
    pub tokens_per_second: f64,
    /// 本次成功历经的实际请求尝试数（含 transient 重试；首次即成功 = 1）。
    /// 失败路径 Err(String) 不携带该值，telemetry 侧记 0 表示不可知。
    pub attempt_count: u32,
}

// ========================================================================
// 内部工具函数
// ========================================================================

/// 从 SSE JSON 中解析 content/thinking/usage
fn parse_sse_delta<'a>(
    json: &'a serde_json::Value,
) -> (Option<&'a str>, Option<&'a str>, Option<RawUsage>) {
    let delta = &json["choices"][0]["delta"];

    let thinking = delta
        .get("reasoning_content")
        .and_then(|v| v.as_str())
        .or_else(|| delta.get("reasoning").and_then(|v| v.as_str()))
        .or_else(|| {
            json.get("message")
                .and_then(|m| m.get("thinking"))
                .and_then(|v| v.as_str())
        });

    let content = delta.get("content").and_then(|v| v.as_str());

    let usage = parse_usage(json);

    (content, thinking, usage)
}

/// 从 JSON 中提取 usage 信息，支持 OpenAI / DeepSeek / Anthropic / Gemini / 硅基流动等前缀缓存字段
fn parse_usage(json: &serde_json::Value) -> Option<RawUsage> {
    let usage = json.get("usage")?;
    let prompt = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let completion = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    // 多平台前缀缓存命中 Token 提取：
    // 1. OpenAI / OpenRouter: prompt_tokens_details.cached_tokens
    // 2. DeepSeek 官方 / 硅基流动: prompt_cache_hit_tokens
    // 3. Anthropic Claude: cache_read_input_tokens
    // 4. Gemini: cached_content_token_count
    let cached = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_i64())
        .or_else(|| {
            usage
                .get("prompt_cache_hit_tokens")
                .and_then(|v| v.as_i64())
        })
        .or_else(|| {
            usage
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_i64())
        })
        .or_else(|| {
            usage
                .get("cached_content_token_count")
                .and_then(|v| v.as_i64())
        })
        .unwrap_or(0);

    if prompt > 0 || completion > 0 || cached > 0 {
        Some(RawUsage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            cached_tokens: cached,
        })
    } else {
        None
    }
}

/// 处理一行 SSE 数据
fn process_sse_line<F: FnMut(StreamEvent) -> bool>(
    line: &str,
    thinking_text: &mut String,
    full_response: &mut String,
    usage_data: &mut Option<RawUsage>,
    first_token_instant: &mut Option<std::time::Instant>,
    on_event: &mut F,
) -> bool {
    if line.is_empty() || line == "data: [DONE]" {
        return true;
    }

    if let Some(data_str) = line.strip_prefix("data: ") {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(data_str) {
            let (content, thinking, usage) = parse_sse_delta(&json);

            if let Some(t) = thinking {
                if !t.is_empty() {
                    if first_token_instant.is_none() {
                        *first_token_instant = Some(std::time::Instant::now());
                    }
                    thinking_text.push_str(t);
                    if !on_event(StreamEvent::Thinking {
                        chunk: t,
                        accumulated: thinking_text,
                    }) {
                        return false;
                    }
                }
            }

            if let Some(c) = content {
                if !c.is_empty() {
                    if first_token_instant.is_none() {
                        *first_token_instant = Some(std::time::Instant::now());
                    }
                    full_response.push_str(c);
                    if !on_event(StreamEvent::Content {
                        chunk: c,
                        accumulated_content: full_response,
                        full_thinking: thinking_text,
                    }) {
                        return false;
                    }
                }
            }

            if usage.is_some() {
                *usage_data = usage;
            }
        }
    }
    true
}

// ========================================================================
// 核心流式请求
// ========================================================================

pub const STREAM_INTERRUPTED_ERROR: &str = "agent_stream_interrupted";

/// 发送一次流式请求，解析 SSE 事件并通过回调实时通知调用方。
///
/// `on_event` 在每个 thinking/content chunk 到达时被调用，
/// 调用方可以借此实现实时 UI 推送。
pub async fn stream_once<F: FnMut(StreamEvent) -> bool>(
    config: &ModelConfig,
    messages: &[serde_json::Value],
    reasoning_effort: &str,
    interrupt_rx: &mut watch::Receiver<u64>,
    on_event: &mut F,
) -> Result<StreamResult, String> {
    let client = config.build_client()?;
    if interrupt_rx.has_changed().unwrap_or(false) {
        return Err(STREAM_INTERRUPTED_ERROR.to_string());
    }
    let body = config.build_body(messages, reasoning_effort);
    let url = config.endpoint_url();
    let headers = config.auth_headers();

    let start_instant = std::time::Instant::now();
    let mut first_token_instant: Option<std::time::Instant> = None;

    let request = client
        .post(&url)
        .headers(headers)
        .json(&body)
        .timeout(std::time::Duration::from_secs(config.timeout_secs))
        .send();
    tokio::pin!(request);
    let resp = tokio::select! {
        result = &mut request => result.map_err(|e| format!("请求失败: {}", e))?,
        changed = interrupt_rx.changed() => {
            let _ = changed;
            return Err(STREAM_INTERRUPTED_ERROR.to_string());
        }
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("API 错误 {}: {}", status, body));
    }

    let mut stream = resp.bytes_stream();
    let mut full_response = String::new();
    let mut thinking_text = String::new();
    let mut line_buf = String::new();
    let mut usage_data: Option<RawUsage> = None;

    'stream_loop: loop {
        let item = tokio::select! {
            next = stream.next() => next,
            changed = interrupt_rx.changed() => {
                let _ = changed;
                return Err(STREAM_INTERRUPTED_ERROR.to_string());
            }
        };
        let Some(item) = item else {
            break;
        };
        let chunk = match item {
            Ok(c) => c,
            Err(e) => {
                let err_str = e.to_string();
                // 如果上游在输出完成前意外掐断了连接 (unexpected EOF)，但本地已接收到了完整 JSON 或实质内容，尝试优雅自愈
                if (err_str.contains("unexpected EOF") || err_str.contains("UnexpectedEof"))
                    && (!full_response.trim().is_empty() || !thinking_text.trim().is_empty())
                {
                    println!(
                        "⚠️ [LLM 流式告警] 上游 API 服务商提前断开连接 (unexpected EOF)，已接收 {} 字符内容，尝试自愈解析...",
                        full_response.len()
                    );
                    break;
                }
                return Err(format!(
                    "上游服务商流式连接意外中断 (unexpected EOF): {}",
                    err_str
                ));
            }
        };
        line_buf.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(pos) = line_buf.find('\n') {
            let line = line_buf[..pos].trim().to_string();
            line_buf = line_buf[pos + 1..].to_string();
            if !process_sse_line(
                &line,
                &mut thinking_text,
                &mut full_response,
                &mut usage_data,
                &mut first_token_instant,
                on_event,
            ) {
                break 'stream_loop;
            }
        }
    }

    // 处理缓冲区中残留的数据
    let remaining = line_buf.trim().to_string();
    if !remaining.is_empty() && remaining != "data: [DONE]" {
        let _ = process_sse_line(
            &remaining,
            &mut thinking_text,
            &mut full_response,
            &mut usage_data,
            &mut first_token_instant,
            on_event,
        );
    }

    let total_duration_ms = start_instant.elapsed().as_millis() as u64;
    let ttft_ms = first_token_instant
        .map(|t| t.duration_since(start_instant).as_millis() as u64)
        .unwrap_or(total_duration_ms);

    let gen_duration_secs = first_token_instant
        .map(|t| t.elapsed().as_secs_f64())
        .unwrap_or(0.0);

    let completion_count = usage_data
        .map(|u| u.completion_tokens)
        .unwrap_or_else(|| (full_response.len() as f64 / 3.5).ceil() as i64);

    let tokens_per_second = if gen_duration_secs > 0.05 && completion_count > 0 {
        (completion_count as f64) / gen_duration_secs
    } else if total_duration_ms > 0 && completion_count > 0 {
        (completion_count as f64) / (total_duration_ms as f64 / 1000.0)
    } else {
        0.0
    };

    Ok(StreamResult {
        full_response,
        thinking_text,
        usage: usage_data,
        ttft_ms,
        total_duration_ms,
        tokens_per_second,
        attempt_count: 1,
    })
}

/// 单次暂态请求重试的最大等待时间。上限允许长流程在 429/网络波动时持续恢复，
/// 同时避免 20 次尝试沿用无界指数退避而等待数小时。
const MAX_TRANSIENT_RETRY_BACKOFF_MS: u64 = 15_000;

fn transient_retry_backoff_ms(attempt_index: u32) -> u64 {
    let exponent = attempt_index.min(4);
    (1_000u64 * (1u64 << exponent)).min(MAX_TRANSIENT_RETRY_BACKOFF_MS)
}

/// 带自动重试的流式请求（按配置次数重试所有错误）
pub async fn stream_with_retry<F: FnMut(StreamEvent) -> bool>(
    config: &ModelConfig,
    messages: &[serde_json::Value],
    reasoning_effort: &str,
    interrupt_rx: &mut watch::Receiver<u64>,
    max_retries: u32,
    on_event: &mut F,
) -> Result<StreamResult, String> {
    let mut last_error = String::new();
    for i in 0..max_retries {
        match stream_once(config, messages, reasoning_effort, interrupt_rx, on_event).await {
            Ok(mut result) => {
                // 成功路径回填实际尝试次数（首次即成功 = 1，transient 重试后成功 = i+1）
                result.attempt_count = i + 1;
                return Ok(result);
            }
            Err(e) => {
                if e == STREAM_INTERRUPTED_ERROR {
                    return Err(e);
                }

                last_error = e;
                println!(
                    "⚠️ LLM 请求失败 (第 {} 次 / 共 {} 次): {}",
                    i + 1,
                    max_retries,
                    last_error
                );
                if i < max_retries - 1 {
                    // 有界指数退避（1s、2s、4s、8s、15s...）给服务端恢复窗口。
                    let backoff_ms = transient_retry_backoff_ms(i);
                    tokio::select! {
                        _ = tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)) => {},
                        changed = interrupt_rx.changed() => {
                            let _ = changed;
                            return Err(STREAM_INTERRUPTED_ERROR.to_string());
                        }
                    }
                }
            }
        }
    }
    Err(format!(
        "LLM 请求最终失败 ({}次尝试): {}",
        max_retries, last_error
    ))
}

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
mod tests {
    use super::ModelConfig;

    #[test]
    fn from_conn_self_heals_missing_platform_capability_column() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // 老库结构：platforms 没有 requires_alternating_roles 列
        conn.execute_batch(
            "CREATE TABLE platforms (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, base_url TEXT NOT NULL,
                api_key TEXT NOT NULL, created_at TEXT NOT NULL);
             CREATE TABLE models (
                id TEXT PRIMARY KEY, platform_id TEXT NOT NULL, name TEXT NOT NULL,
                max_tokens INTEGER, temperature REAL, context_window INTEGER, enabled INTEGER DEFAULT 1);
             INSERT INTO platforms VALUES ('p1', '火山订阅', 'https://ark.cn-beijing.volces.com/api/coding/v3', 'k', '2026');
             INSERT INTO models VALUES ('m1', 'p1', 'deepseek-v4-pro', 4096, 0.7, 200000, 1);",
        )
        .unwrap();

        // 老代码在这里会报 no such column；自愈后应成功且取默认值 -1
        let cfg = ModelConfig::from_conn(&conn, "m1").expect("缺列时应自愈补列并成功");
        assert_eq!(cfg.requires_alternating_roles, -1);

        // 列已真实补上：再次查询稳定成功
        let cfg2 = ModelConfig::from_conn(&conn, "m1").unwrap();
        assert_eq!(cfg2.model_name, "deepseek-v4-pro");
    }

    fn config(model_name: &str, base_url: &str) -> ModelConfig {
        ModelConfig {
            model_id: "test-model-id".to_string(),
            platform_name: "test-platform".to_string(),
            base_url: base_url.to_string(),
            api_key: "test".to_string(),
            model_name: model_name.to_string(),
            max_tokens: 512,
            temperature: 0.7,
            is_ollama: false,
            is_lmstudio: false,
            context_window: 128_000,
            provider_capabilities: "{}".to_string(),
            timeout_secs: 120,
            requires_alternating_roles: -1,
        }
    }

    #[test]
    fn configured_profile_injects_response_format_without_url_matching() {
        let body = with_capabilities(r#"{"response_format":{"type":"json_object"}}"#)
            .build_body(&[], "low");
        assert_eq!(body["response_format"]["type"], "json_object");
    }

    fn with_capabilities(profile: &str) -> ModelConfig {
        let mut config = config("configured-model", "https://example.invalid/v1");
        config.provider_capabilities = profile.to_string();
        config
    }

    #[test]
    fn configured_profile_maps_effort_and_removes_sampling_when_enabled() {
        let profile = r#"{
            "thinking": {
                "field": "thinking",
                "enabled_value": { "type": "enabled" },
                "disabled_value": { "type": "disabled" },
                "remove_sampling_when_enabled": true,
                "effort": {
                    "field": "reasoning_effort",
                    "map": { "auto": "low", "medium": "high", "high": "max" },
                    "default": "high"
                }
            }
        }"#;
        let auto = with_capabilities(profile).build_body(&[], "auto");
        assert_eq!(auto["thinking"]["type"], "enabled");
        assert_eq!(auto["reasoning_effort"], "low");
        assert!(auto.get("temperature").is_none());
        assert!(auto.get("top_p").is_none());

        let high = with_capabilities(profile).build_body(&[], "high");
        assert_eq!(high["reasoning_effort"], "max");

        let off = with_capabilities(profile).build_body(&[], "off");
        assert_eq!(off["thinking"]["type"], "disabled");
        assert!(off.get("reasoning_effort").is_none());
        assert!(off.get("temperature").is_some());
    }

    #[test]
    fn configured_profile_can_force_thinking_and_keep_ui_high_below_max() {
        let profile = r#"{
            "thinking": {
                "enabled_value": { "type": "enabled" },
                "force_enabled": true,
                "effort": {
                    "map": { "off": "low", "low": "low", "medium": "high", "high": "high", "max": "max", "auto": "high" }
                }
            }
        }"#;
        let high = with_capabilities(profile).build_body(&[], "high");
        assert_eq!(high["thinking"]["type"], "enabled");
        assert_eq!(high["reasoning_effort"], "high");

        let max = with_capabilities(profile).build_body(&[], "max");
        assert_eq!(max["reasoning_effort"], "max");

        let off = with_capabilities(profile).build_body(&[], "off");
        assert_eq!(off["thinking"]["type"], "enabled");
        assert_eq!(off["reasoning_effort"], "low");
    }

    #[test]
    fn unconfigured_models_do_not_receive_vendor_specific_fields() {
        let body = config("gpt-4o-mini", "https://api.openai.com/v1").build_body(&[], "max");
        assert!(body.get("thinking").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn transient_retry_backoff_is_exponential_but_bounded() {
        use super::transient_retry_backoff_ms;
        assert_eq!(transient_retry_backoff_ms(0), 1_000);
        assert_eq!(transient_retry_backoff_ms(3), 8_000);
        assert_eq!(transient_retry_backoff_ms(4), 15_000);
        assert_eq!(transient_retry_backoff_ms(19), 15_000);
    }
}
