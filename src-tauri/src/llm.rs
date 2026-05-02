//! LLM 请求基础设施
//!
//! 提供统一的模型配置、HTTP 请求构建、SSE 流式解析等公共能力，
//! 供 Agent (call_llm) 和 Chat (send_chat) 两条路径共用。

use crate::db::DbState;
use futures_util::StreamExt;
use rusqlite::params;
use serde_json::json;
use tauri::State;

// ========================================================================
// ModelConfig — 从数据库解析的模型配置，含平台适配参数
// ========================================================================

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
        let (base_url, api_key, model_name, max_tokens, temperature) = {
            let conn = state.0.lock().map_err(|e| e.to_string())?;
            let mut stmt = conn
                .prepare(
                    "SELECT p.base_url, p.api_key, m.name, m.max_tokens, m.temperature
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

        let context_window = if is_ollama || is_lmstudio {
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
            .build()
            .map_err(|e| e.to_string())
    }

    /// 构建 OpenAI 兼容的请求体
    pub fn build_body(&self, messages: &[serde_json::Value]) -> serde_json::Value {
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

        let mut body = json!({
            "model": self.model_name,
            "messages": messages,
            "max_tokens": self.max_tokens,
            "temperature": use_temp,
            "top_p": use_top_p,
            "stream": true,
        });

        // stream_options 只有 OpenAI/OpenRouter 支持，本地模型不加
        if !self.is_ollama && !self.is_lmstudio {
            body["stream_options"] = json!({ "include_usage": true });
        }

        // Ollama 需要通过 options 注入 num_ctx
        if self.is_ollama {
            body["options"] = json!({
                "num_ctx": self.context_window,
                "temperature": 0.0,
                "top_p": 0.9,
            });
        }

        body
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

// ========================================================================
// StreamResult — 单次流式请求的完整结果
// ========================================================================

pub struct StreamResult {
    pub full_response: String,
    pub thinking_text: String,
    pub usage: Option<(i64, i64)>,
}

// ========================================================================
// 内部工具函数
// ========================================================================

/// 从 SSE JSON 中解析 content/thinking/usage
fn parse_sse_delta<'a>(
    json: &'a serde_json::Value,
) -> (Option<&'a str>, Option<&'a str>, Option<(i64, i64)>) {
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

/// 从 JSON 中提取 usage 信息
fn parse_usage(json: &serde_json::Value) -> Option<(i64, i64)> {
    let usage = json.get("usage")?;
    let prompt = usage
        .get("prompt_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let completion = usage
        .get("completion_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if prompt > 0 || completion > 0 {
        Some((prompt, completion))
    } else {
        None
    }
}

/// 处理一行 SSE 数据
fn process_sse_line<F: FnMut(StreamEvent)>(
    line: &str,
    thinking_text: &mut String,
    full_response: &mut String,
    usage_data: &mut Option<(i64, i64)>,
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
                    thinking_text.push_str(t);
                    on_event(StreamEvent::Thinking {
                        chunk: t,
                        accumulated: thinking_text,
                    });
                }
            }

            if let Some(c) = content {
                if !c.is_empty() {
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

/// 发送一次流式请求，解析 SSE 事件并通过回调实时通知调用方。
///
/// `on_event` 在每个 thinking/content chunk 到达时被调用，
/// 调用方可以借此实现实时 UI 推送。
pub async fn stream_once<F: FnMut(StreamEvent)>(
    config: &ModelConfig,
    messages: &[serde_json::Value],
    on_event: &mut F,
) -> Result<StreamResult, String> {
    let client = config.build_client()?;
    let body = config.build_body(messages);
    let url = config.endpoint_url();
    let headers = config.auth_headers();

    let resp = client
        .post(&url)
        .headers(headers)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("请求失败: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("API 错误 {}: {}", status, body));
    }

    let mut stream = resp.bytes_stream();
    let mut full_response = String::new();
    let mut thinking_text = String::new();
    let mut line_buf = String::new();
    let mut usage_data: Option<(i64, i64)> = None;

    while let Some(item) = stream.next().await {
        let chunk = item.map_err(|e| e.to_string())?;
        line_buf.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(pos) = line_buf.find('\n') {
            let line = line_buf[..pos].trim().to_string();
            line_buf = line_buf[pos + 1..].to_string();
            process_sse_line(
                &line,
                &mut thinking_text,
                &mut full_response,
                &mut usage_data,
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
            on_event,
        );
    }

    Ok(StreamResult {
        full_response,
        thinking_text,
        usage: usage_data,
    })
}

/// 带自动重试的流式请求
pub async fn stream_with_retry<F: FnMut(StreamEvent)>(
    config: &ModelConfig,
    messages: &[serde_json::Value],
    max_retries: u32,
    on_event: &mut F,
) -> Result<StreamResult, String> {
    let mut last_error = String::new();
    for i in 0..max_retries {
        match stream_once(config, messages, on_event).await {
            Ok(result) => return Ok(result),
            Err(e) => {
                last_error = e;
                println!("⚠️ LLM 请求失败 (第 {} 次): {}", i + 1, last_error);
                if i < max_retries - 1 {
                    tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
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
