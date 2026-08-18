//! LLM 请求基础设施
//!
//! 提供统一的模型配置、HTTP 请求构建、SSE 流式解析等公共能力，
//! 供 Agent (call_llm) 和 Chat (send_chat) 两条路径共用。

use crate::db::DbState;
use futures_util::StreamExt;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::State;
use tokio::sync::watch;

// ========================================================================
// ModelConfig — 从数据库解析的模型配置，含平台适配参数
// ========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
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
    pub timeout_secs: u64,
}

impl ModelConfig {
    /// 从数据库查询模型+平台信息，自动检测本地模型类型
    pub fn from_db(state: &State<'_, DbState>, model_id: &str) -> Result<Self, String> {
        let (base_url, api_key, model_name, max_tokens, temperature, context_window_db) = {
            let conn = state.0.lock().map_err(|e| e.to_string())?;
            let mut stmt = conn
                .prepare(
                    "SELECT p.base_url, p.api_key, m.name, m.max_tokens, m.temperature, m.context_window
                     FROM models m JOIN platforms p ON p.id = m.platform_id
                     WHERE m.id = ?1",
                )
                .map_err(|e| e.to_string())?;
            stmt.query_row(params![model_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, f64>(4)?,
                    row.get::<_, i64>(5)?,
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
            base_url,
            api_key,
            model_name,
            max_tokens,
            temperature,
            is_ollama,
            is_lmstudio,
            context_window,
            timeout_secs,
        })
    }

    /// 构建 reqwest HTTP 客户端
    pub fn build_client(&self) -> Result<reqwest::Client, String> {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .connect_timeout(std::time::Duration::from_secs(15))
            .tcp_keepalive(Some(std::time::Duration::from_secs(15)))
            .tcp_nodelay(true)
            .pool_idle_timeout(Some(std::time::Duration::from_secs(90)))
            .build()
            .map_err(|e| e.to_string())
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
        if self.model_name.contains("o1") || self.model_name.contains("o3") || self.model_name.contains("gpt-4o") {
            body["max_completion_tokens"] = json!(effective_max_tokens);
        }

        // stream_options 只有 OpenAI/OpenRouter 支持，本地模型不加
        if !self.is_ollama && !self.is_lmstudio {
            body["stream_options"] = json!({ "include_usage": true });
        }

        if self.is_deepseek() {
            if reasoning_effort == "off" {
                // DeepSeek 的非思考模式可继续使用常规采样参数。
                body["thinking"] = json!({ "type": "disabled" });
            } else {
                body["thinking"] = json!({ "type": "enabled" });
                body["reasoning_effort"] = json!(self.deepseek_reasoning_effort(reasoning_effort));
                // DeepSeek 思考模式下这些采样参数不会生效，避免发送误导性配置。
                if let Some(object) = body.as_object_mut() {
                    object.remove("temperature");
                    object.remove("top_p");
                }
            }
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

    fn is_deepseek(&self) -> bool {
        self.base_url.contains("api.deepseek.com")
            || self.model_name.to_ascii_lowercase().contains("deepseek")
    }

    /// 将界面高 / 中 / 低映射为 DeepSeek 官方接受的 max / high / low。
    /// 自动模式优先保证 Flash 的低延迟，其他 DeepSeek 模型维持官方默认 high。
    fn deepseek_reasoning_effort(&self, preference: &str) -> &'static str {
        match preference {
            "low" => "low",
            "medium" => "high",
            "high" | "max" => "max",
            "auto" if self.model_name.to_ascii_lowercase().contains("flash") => "low",
            _ => "high",
        }
    }

    /// API 端点 URL
    pub fn endpoint_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// 构建认证 HeaderMap
    pub fn auth_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "Authorization",
            format!("Bearer {}", self.api_key).parse().unwrap(),
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
        .or_else(|| usage.get("prompt_cache_hit_tokens").and_then(|v| v.as_i64()))
        .or_else(|| usage.get("cache_read_input_tokens").and_then(|v| v.as_i64()))
        .or_else(|| usage.get("cached_content_token_count").and_then(|v| v.as_i64()))
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
fn process_sse_line<F: FnMut(StreamEvent)>(
    line: &str,
    thinking_text: &mut String,
    full_response: &mut String,
    usage_data: &mut Option<RawUsage>,
    first_token_instant: &mut Option<std::time::Instant>,
    on_event: &mut F,
) {
    if line.is_empty() || line == "data: [DONE]" {
        return;
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
                    on_event(StreamEvent::Thinking {
                        chunk: t,
                        accumulated: thinking_text,
                    });
                }
            }

            if let Some(c) = content {
                if !c.is_empty() {
                    if first_token_instant.is_none() {
                        *first_token_instant = Some(std::time::Instant::now());
                    }
                    full_response.push_str(c);
                    on_event(StreamEvent::Content {
                        chunk: c,
                        accumulated_content: full_response,
                        full_thinking: thinking_text,
                    });
                }
            }

            if usage.is_some() {
                *usage_data = usage;
            }
        }
    }
}

// ========================================================================
// 核心流式请求
// ========================================================================

pub const STREAM_INTERRUPTED_ERROR: &str = "agent_stream_interrupted";

/// 发送一次流式请求，解析 SSE 事件并通过回调实时通知调用方。
///
/// `on_event` 在每个 thinking/content chunk 到达时被调用，
/// 调用方可以借此实现实时 UI 推送。
pub async fn stream_once<F: FnMut(StreamEvent)>(
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

    let request = client.post(&url).headers(headers).json(&body).send();
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

    loop {
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
                return Err(format!("上游服务商流式连接意外中断 (unexpected EOF): {}", err_str));
            }
        };
        line_buf.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(pos) = line_buf.find('\n') {
            let line = line_buf[..pos].trim().to_string();
            line_buf = line_buf[pos + 1..].to_string();
            process_sse_line(
                &line,
                &mut thinking_text,
                &mut full_response,
                &mut usage_data,
                &mut first_token_instant,
                on_event,
            );
        }
    }

    // 处理缓冲区中残留的数据
    let remaining = line_buf.trim().to_string();
    if !remaining.is_empty() && remaining != "data: [DONE]" {
        process_sse_line(
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
    })
}

/// 判断是否为不可恢复的客户端/认证类致命错误（避免在 401/403/400/404 等错误上浪费重试配额与等待时间）
pub fn is_non_retryable_error(err: &str) -> bool {
    // 包含常见 HTTP 4xx 状态码（排除 429 速率限制，429 必须退避重试）
    let non_retryable_status = [
        "API 错误 400",
        "API 错误 401",
        "API 错误 402",
        "API 错误 403",
        "API 错误 404",
        "API 错误 422",
    ];
    for status in &non_retryable_status {
        if err.contains(status) {
            return true;
        }
    }
    // 常见明确的认证与配额不足错误关键词
    if err.contains("invalid_api_key")
        || err.contains("Incorrect API key")
        || err.contains("AuthenticationError")
        || err.contains("insufficient_quota")
        || err.contains("model_not_found")
    {
        return true;
    }
    false
}

/// 带自动重试与状态码感知的流式请求
pub async fn stream_with_retry<F: FnMut(StreamEvent)>(
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
            Ok(result) => return Ok(result),
            Err(e) => {
                if e == STREAM_INTERRUPTED_ERROR {
                    return Err(e);
                }
                // 🛑 智能状态码感知：客户端不可恢复错误（401 Key失效、403 无权、400 格式非法等）立即终止重试，杜绝浪费
                if is_non_retryable_error(&e) {
                    println!("❌ LLM 遇到不可恢复的客户端/认证错误 ({}): 立即终止重试！", e);
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
                    // 指数退避 (1s, 2s, 4s...) 给服务端恢复窗口
                    let backoff_ms = 1000u64 * (1 << i);
                    tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
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

    fn config(model_name: &str, base_url: &str) -> ModelConfig {
        ModelConfig {
            base_url: base_url.to_string(),
            api_key: "test".to_string(),
            model_name: model_name.to_string(),
            max_tokens: 512,
            temperature: 0.7,
            is_ollama: false,
            is_lmstudio: false,
            context_window: 128_000,
            timeout_secs: 120,
        }
    }

    #[test]
    fn auto_uses_low_effort_for_deepseek_flash() {
        let body = config("deepseek-v4-flash", "https://api.deepseek.com").build_body(&[], "auto");
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["reasoning_effort"], "low");
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn auto_uses_high_effort_for_deepseek_pro() {
        let body = config("deepseek-v4-pro", "https://api.deepseek.com").build_body(&[], "auto");
        assert_eq!(body["reasoning_effort"], "high");
    }

    #[test]
    fn medium_maps_to_balanced_deepseek_effort() {
        let body = config("deepseek-v4-pro", "https://api.deepseek.com").build_body(&[], "medium");
        assert_eq!(body["reasoning_effort"], "high");
    }
    #[test]
    fn high_maps_to_max_deepseek_effort() {
        let body = config("deepseek-v4-pro", "https://api.deepseek.com").build_body(&[], "high");
        assert_eq!(body["reasoning_effort"], "max");
    }
    #[test]
    fn off_disables_deepseek_thinking_without_an_effort() {
        let body = config("deepseek-v4-flash", "https://api.deepseek.com").build_body(&[], "off");
        assert_eq!(body["thinking"]["type"], "disabled");
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("temperature").is_some());
    }

    #[test]
    fn non_deepseek_models_do_not_receive_vendor_specific_fields() {
        let body = config("gpt-4o-mini", "https://api.openai.com/v1").build_body(&[], "max");
        assert!(body.get("thinking").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn test_retry_policy_respects_status_codes() {
        use super::is_non_retryable_error;

        // 不可重试的客户端致命错误
        assert!(is_non_retryable_error("API 错误 401 Unauthorized: invalid_api_key"));
        assert!(is_non_retryable_error("API 错误 403 Forbidden: user suspended"));
        assert!(is_non_retryable_error("API 错误 400 Bad Request: unsupported parameter"));
        assert!(is_non_retryable_error("API 错误 404 Not Found: model_not_found"));
        assert!(is_non_retryable_error("AuthenticationError: Incorrect API key provided"));
        assert!(is_non_retryable_error("insufficient_quota: out of credits"));

        // 必须退避重试的临时/服务端错误
        assert!(!is_non_retryable_error("API 错误 429 Too Many Requests: rate limit exceeded"));
        assert!(!is_non_retryable_error("API 错误 500 Internal Server Error"));
        assert!(!is_non_retryable_error("API 错误 502 Bad Gateway"));
        assert!(!is_non_retryable_error("API 错误 503 Service Unavailable"));
        assert!(!is_non_retryable_error("请求失败: connection reset by peer"));
        assert!(!is_non_retryable_error("上游服务商流式连接意外中断 (unexpected EOF)"));
    }
}
