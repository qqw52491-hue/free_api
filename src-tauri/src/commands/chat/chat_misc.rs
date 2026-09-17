//! 窗口预览、网页搜索与 URL 抓取命令。

use crate::db::{ChatMessage, ChatSession, DbState};
use crate::llm::{self, ModelConfig, StreamEvent};
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

use super::*;


/// 微信式原生独立 WebView 窗口预览
#[tauri::command]
pub async fn open_in_webview_window(
    app: tauri::AppHandle,
    url: String,
    title: Option<String>,
) -> Result<(), String> {
    use tauri::{WebviewUrl, WebviewWindowBuilder};

    let parsed_url = tauri::Url::parse(&url).map_err(|e| format!("无效 URL: {}", e))?;
    let short_uuid = uuid::Uuid::new_v4().to_string().replace('-', "")[..12].to_string();
    let window_label = format!("preview-{}", short_uuid);
    let window_title = title.unwrap_or_else(|| "网页预览".to_string());

    let init_script = r#"
        (function() {
            // 修复 Webview 中 target="_blank" 和 window.open 点不动的问题（百度/Google/B站 搜索结果默认都是 _blank）
            document.addEventListener('click', function(e) {
                var a = e.target.closest('a');
                if (a && (a.target === '_blank' || a.getAttribute('target') === '_blank')) {
                    a.target = '_self';
                }
            }, true);

            var origOpen = window.open;
            window.open = function(url) {
                if (url && typeof url === 'string') {
                    window.location.href = url;
                    return window;
                }
                return origOpen ? origOpen.apply(window, arguments) : window;
            };
        })();
    "#;

    WebviewWindowBuilder::new(&app, &window_label, WebviewUrl::External(parsed_url))
        .title(&window_title)
        .inner_size(1120.0, 780.0)
        .center()
        .resizable(true)
        .focused(true)
        .initialization_script(init_script)
        .build()
        .map_err(|e| format!("创建原生 WebView 窗口失败: {}", e))?;

    Ok(())
}

/// 原生聚合互联网搜索
#[tauri::command]
pub async fn search_web(
    query: String,
    max_results: Option<usize>,
) -> Result<Vec<crate::agent::web_search::WebSearchResultItem>, String> {
    crate::agent::web_search::execute_web_search(&query, max_results.unwrap_or(5)).await
}

/// 原生抓取并提取网页 Markdown
#[tauri::command]
pub async fn fetch_url_markdown(url: String) -> Result<String, String> {
    crate::agent::web_search::fetch_url_markdown(&url).await
}

pub fn resolve_safe_artifact_path(raw: &str) -> Result<std::path::PathBuf, String> {
    let trimmed = raw.trim().trim_matches('"').trim_matches('\'');
    let clean = trimmed.strip_prefix("file://").unwrap_or(trimmed).trim();
    if clean.is_empty() || clean == "[object Object]" {
        return Err("未提供有效的文件路径".to_string());
    }

    let path_buf = std::path::PathBuf::from(clean);
    if path_buf.exists() {
        return path_buf
            .canonicalize()
            .map_err(|e| format!("无法解析路径: {}", e));
    }

    // 1. 优先在应用数据目录的全局会话临时受管目录中查找 (解决 tmp/ 相对快捷路径或纯文件名)
    let global_tmp = dirs_next::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("free-api-chat")
        .join("tmp");

    if global_tmp.exists() {
        let stripped = clean.strip_prefix("./").unwrap_or(clean);
        let sub_path = stripped.strip_prefix("tmp/").unwrap_or(stripped);

        // 1.1 直接检查全局临时根目录下的 sub_path
        let direct_candidate = global_tmp.join(sub_path);
        if direct_candidate.exists() {
            return direct_candidate
                .canonicalize()
                .map_err(|e| format!("无法解析路径: {}", e));
        }

        // 1.2 遍历所有会话子目录，查找最新的匹配文件
        if let Ok(entries) = std::fs::read_dir(&global_tmp) {
            let mut candidates: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
            for entry in entries.flatten() {
                let session_dir = entry.path();
                if session_dir.is_dir() {
                    let candidate = session_dir.join(sub_path);
                    if candidate.exists() {
                        let mtime = candidate
                            .metadata()
                            .and_then(|m| m.modified())
                            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                        candidates.push((mtime, candidate));
                    }
                }
            }
            if !candidates.is_empty() {
                candidates.sort_by(|a, b| b.0.cmp(&a.0));
                return candidates[0]
                    .1
                    .canonicalize()
                    .map_err(|e| format!("无法解析路径: {}", e));
            }
        }
    }

    // 2. 尝试基于当前工作目录以及父级工作区目录查找相对路径
    if let Ok(current_dir) = std::env::current_dir() {
        let candidate = current_dir.join(clean);
        if candidate.exists() {
            return candidate
                .canonicalize()
                .map_err(|e| format!("无法解析路径: {}", e));
        }
        if let Some(parent) = current_dir.parent() {
            let candidate_parent = parent.join(clean);
            if candidate_parent.exists() {
                return candidate_parent
                    .canonicalize()
                    .map_err(|e| format!("无法解析路径: {}", e));
            }
        }
    }

    // 3. 尝试在操作系统系统临时目录下查找
    let sys_tmp = std::env::temp_dir();
    let sys_candidate = sys_tmp.join(clean);
    if sys_candidate.exists() {
        return sys_candidate
            .canonicalize()
            .map_err(|e| format!("无法解析路径: {}", e));
    }
    let stripped = clean.strip_prefix("./").unwrap_or(clean);
    if let Some(sub) = stripped.strip_prefix("tmp/") {
        let sys_sub_candidate = sys_tmp.join(sub);
        if sys_sub_candidate.exists() {
            return sys_sub_candidate
                .canonicalize()
                .map_err(|e| format!("无法解析路径: {}", e));
        }
    }

    Err(format!("文件不存在: {}", clean))
}
