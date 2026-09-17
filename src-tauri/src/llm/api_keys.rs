//! 多 API Key 解析、脱敏与可轮换错误判定。

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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParsedApiKeyItem {
    pub key: String,
    #[serde(default = "default_key_enabled")]
    pub enabled: bool,
}

fn default_key_enabled() -> bool {
    true
}

/// 解析文本中的所有 API Key 项（包含启用与临时禁用状态）
pub fn parse_all_api_key_items(raw: &str) -> Vec<ParsedApiKeyItem> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return vec![];
    }
    // 如果是 JSON 数组格式
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(arr) = val.as_array() {
                let mut list = Vec::new();
                for item in arr {
                    if let Some(s) = item.as_str() {
                        let clean = s.trim();
                        if !clean.is_empty() {
                            list.push(ParsedApiKeyItem {
                                key: clean.to_string(),
                                enabled: true,
                            });
                        }
                    } else if let Some(obj) = item.as_object() {
                        let enabled = obj.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                        let disabled = obj.get("disabled").and_then(|v| v.as_bool()).unwrap_or(false);
                        let is_enabled = enabled && !disabled;
                        let key_str = obj
                            .get("key")
                            .or_else(|| obj.get("api_key"))
                            .or_else(|| obj.get("token"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .trim();
                        if !key_str.is_empty() {
                            list.push(ParsedApiKeyItem {
                                key: key_str.to_string(),
                                enabled: is_enabled,
                            });
                        }
                    }
                }
                if !list.is_empty() {
                    return list;
                }
            }
        }
    }
    // 按照换行、逗号（中英文）、分号（中英文）分割
    trimmed
        .split(|c: char| c == '\n' || c == '\r' || c == ',' || c == ';' || c == '，' || c == '；')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| ParsedApiKeyItem {
            key: s.to_string(),
            enabled: true,
        })
        .collect()
}

/// 解析平台中配置的所有处于启用状态的可用 API Key 列表（过滤掉 enabled: false 的项）
pub fn parse_api_keys(raw: &str) -> Vec<String> {
    parse_all_api_key_items(raw)
        .into_iter()
        .filter(|item| item.enabled && !item.key.trim().is_empty())
        .map(|item| item.key)
        .collect()
}

/// 对 API Key 进行日志与显示脱敏
pub fn mask_api_key(key: &str) -> String {
    let clean = key.trim();
    let chars: Vec<char> = clean.chars().collect();
    if chars.len() > 10 {
        let prefix: String = chars[..6].iter().collect();
        let suffix: String = chars[chars.len() - 4..].iter().collect();
        format!("{}...{}", prefix, suffix)
    } else if !chars.is_empty() {
        "***".to_string()
    } else {
        "(无Key)".to_string()
    }
}

/// 判断是否为可触发 Key 轮换/故障转移的错误（如 401 鉴权失效、402 余额不足、403 权限受限、429 频率限制、503 服务超载/高并发、502/504 等）
pub fn is_key_rotatable_error(status_code: Option<u16>, body_or_msg: &str) -> bool {
    // 🛑 400 Bad Request 是客户端参数/请求体格式错误，切换 Key 无意义，严禁轮换
    if let Some(code) = status_code {
        if code == 400 {
            return false;
        }
        if code == 401
            || code == 402
            || code == 403
            || code == 429
            || code == 500
            || code == 502
            || code == 503
            || code == 504
        {
            return true;
        }
    }
    let lower = body_or_msg.to_lowercase();
    if lower.contains("api 错误 400") || lower.contains("bad request") {
        return false;
    }
    lower.contains("503")
        || lower.contains("service unavailable")
        || lower.contains("unavailable")
        || lower.contains("high demand")
        || lower.contains("spikes in demand")
        || lower.contains("overloaded")
        || lower.contains("busy")
        || lower.contains("resource_exhausted")
        || lower.contains("resource exhausted")
        || lower.contains("insufficient_quota")
        || lower.contains("insufficient quota")
        || lower.contains("quota_exceeded")
        || lower.contains("quota exceeded")
        || lower.contains("rate_limit")
        || lower.contains("rate limit")
        || lower.contains("ratelimit")
        || lower.contains("balance")
        || lower.contains("余额不足")
        || lower.contains("额度不足")
        || lower.contains("欠费")
        || lower.contains("quota")
        || lower.contains("exceeded your current quota")
        || lower.contains("invalid_api_key")
        || lower.contains("invalid api key")
        || lower.contains("usage limit reached")
        || lower.contains("401 unauthorized")
        || lower.contains("402 payment required")
        || lower.contains("429 too many requests")
        || lower.contains("403 forbidden")
        || lower.contains("502 bad gateway")
        || lower.contains("504 gateway timeout")
        || lower.contains("api 错误 5")
        || lower.contains("api 错误 401")
        || lower.contains("api 错误 402")
        || lower.contains("api 错误 403")
        || lower.contains("api 错误 429")
        || lower.contains("capacity")
        || lower.contains("temporarily unavailable")
}
