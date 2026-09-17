//! SSE 流式事件解析：StreamEvent/Usage 提取与逐行处理。

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


use super::*;


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
pub(crate) fn process_sse_line<F: FnMut(StreamEvent) -> bool>(
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
