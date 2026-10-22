//! 单次流式请求：HTTP 发起、中断语义与回调通知。

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
// 核心流式请求
// ========================================================================

pub const STREAM_INTERRUPTED_ERROR: &str = "agent_stream_interrupted";

/// 发送一次流式请求（使用指定的 API Key），解析 SSE 事件并通过回调实时通知调用方。
///
/// `on_event` 在每个 thinking/content chunk 到达时被调用，
/// 调用方可以借此实现实时 UI 推送。
pub async fn stream_once_with_key<F: FnMut(StreamEvent) -> bool>(
    config: &ModelConfig,
    key: &str,
    messages: &[serde_json::Value],
    reasoning_effort: &str,
    interrupt_rx: &mut watch::Receiver<u64>,
    on_event: &mut F,
) -> Result<StreamResult, String> {
    let client = config.build_client()?;
    if interrupt_rx.has_changed().unwrap_or(false) {
        return Err(STREAM_INTERRUPTED_ERROR.to_string());
    }
    let mut body = config.build_body(messages, reasoning_effort);
    let url = config.endpoint_url();
    let headers = config.auth_headers_for_key(key);

    let start_instant = std::time::Instant::now();
    let mut first_token_instant: Option<std::time::Instant> = None;

    let request = client
        .post(&url)
        .headers(headers.clone())
        .json(&body)
        .timeout(std::time::Duration::from_secs(config.timeout_secs))
        .send();
    tokio::pin!(request);
    let mut resp = tokio::select! {
        result = &mut request => result.map_err(|e| format!("请求失败: {}", e))?,
        changed = interrupt_rx.changed() => {
            if changed.is_ok() {
                return Err(STREAM_INTERRUPTED_ERROR.to_string());
            } else {
                // sender 已销毁（会话控制被释放）——通道永远等不到值；
                // pending() 会把任务永久挂死并废掉 reqwest 超时与主循环
                // 时限兜底；按"已中断"干净返回。
                return Err(STREAM_INTERRUPTED_ERROR.to_string());
            }
        }
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let err_body = resp.text().await.unwrap_or_default();

        // 🩹 自愈检测：若为 400 Bad Request 且检测到敏感参数被拒，就地修正 payload 重发 1 次
        if status == reqwest::StatusCode::BAD_REQUEST
            && crate::agent::model_filter::self_heal_bad_request_payload(&mut body, &err_body)
        {
            println!(
                "🔄 [参数自愈重试] 已自动剔除/修正非法字段，正在向 [{}] 原地重试...",
                config.model_name
            );
            let retry_req = client
                .post(&url)
                .headers(headers)
                .json(&body)
                .timeout(std::time::Duration::from_secs(config.timeout_secs))
                .send();
            tokio::pin!(retry_req);
            let retry_resp = tokio::select! {
                result = &mut retry_req => result.map_err(|e| format!("自愈重试请求失败: {}", e))?,
                changed = interrupt_rx.changed() => {
                    if changed.is_ok() {
                        return Err(STREAM_INTERRUPTED_ERROR.to_string());
                    } else {
                        // sender 已销毁（会话控制被释放）——通道永远等不到值，
                        // pending() 会把任务永久挂死并废掉 reqwest 超时与主循环
                        // 时限兜底；按"已中断"干净返回。
                        return Err(STREAM_INTERRUPTED_ERROR.to_string());
                    }
                }
            };
            if retry_resp.status().is_success() {
                println!("✅ [参数自愈成功] [{}] 自愈重试成功接入！", config.model_name);
                resp = retry_resp;
            } else {
                let s2 = retry_resp.status();
                let b2 = retry_resp.text().await.unwrap_or_default();
                return Err(format!("API 错误 {}: {}", s2, b2));
            }
        } else {
            return Err(format!("API 错误 {}: {}", status, err_body));
        }
    }

    let mut stream = resp.bytes_stream();
    let mut full_response = String::new();
    let mut thinking_text = String::new();
    let mut line_buf = String::new();
    let mut usage_data: Option<RawUsage> = None;
    let mut finish_reason = None;

    'stream_loop: loop {
        let item = tokio::select! {
            next = stream.next() => next,
            changed = interrupt_rx.changed() => {
                if changed.is_ok() {
                    return Err(STREAM_INTERRUPTED_ERROR.to_string());
                } else {
                    // sender 已销毁（会话控制被释放）——通道永远等不到值；
                    // pending() 会把任务永久挂死并废掉 reqwest 超时与主循环
                    // 时限兜底；按"已中断"干净返回。
                    return Err(STREAM_INTERRUPTED_ERROR.to_string());
                }
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
                &mut finish_reason,
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
            &mut finish_reason,
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
        finish_reason,
        usage: usage_data,
        ttft_ms,
        total_duration_ms,
        tokens_per_second,
        attempt_count: 1,
    })
}

/// 发送一次流式请求（使用首个可用 Key）
pub async fn stream_once<F: FnMut(StreamEvent) -> bool>(
    config: &ModelConfig,
    messages: &[serde_json::Value],
    reasoning_effort: &str,
    interrupt_rx: &mut watch::Receiver<u64>,
    on_event: &mut F,
) -> Result<StreamResult, String> {
    let first_key = config.api_keys().into_iter().next().unwrap_or_else(|| config.api_key.clone());
    stream_once_with_key(config, &first_key, messages, reasoning_effort, interrupt_rx, on_event).await
}
