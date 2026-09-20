//! 项目作用域资源派发与会话归属判定。

use crate::agent::mcp::PluginRegistry;
use crate::agent::mcp_catalog::invalidate_plugin;
use crate::agent::plan_tree_dispatch::{
    run_plan_tree_sync, validate_controlled_parallel_batch, MAX_CONTROLLED_PARALLEL_BRANCHES,
};
use crate::agent::retrieval;
use crate::agent::task_store;
use crate::agent::types::{AgentInstruction, DispatchResult};
use crate::agent::utils::{run_osascript, run_shell};
use crate::agent::web_search;
use crate::db::DbState;
use chrono::Utc;
use reqwest::Url;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

use super::*;

/// 🌟 智能规范化 MCP 参数（自动解包嵌套层级，自动映射 path/filePath 等常见别名）
pub(crate) const MAX_CODE_READ_LINES: u64 = 240;

pub(crate) fn guard_code_read_range(
    tool_name: &str,
    params: serde_json::Value,
) -> (serde_json::Value, Option<String>) {
    let lower_tool = tool_name.to_ascii_lowercase();
    let reads_text = matches!(
        lower_tool.as_str(),
        "read_text_file" | "read_file" | "get_file_content" | "get_code_snippet"
    );
    let serde_json::Value::Object(mut obj) = params else {
        return (params, None);
    };
    if !reads_text || !obj.contains_key("path") {
        return (serde_json::Value::Object(obj), None);
    }

    let requested_start = obj
        .get("startLine")
        .or_else(|| obj.get("start_line"))
        .or_else(|| obj.get("offset"))
        .and_then(|value| value.as_u64());
    let requested_end = obj
        .get("endLine")
        .or_else(|| obj.get("end_line"))
        .and_then(|value| value.as_u64())
        .or_else(|| {
            obj.get("limit").and_then(|lim| {
                let s = requested_start.unwrap_or(1).max(1);
                lim.as_u64().map(|l| s.saturating_add(l).saturating_sub(1))
            })
        });

    let start_line = requested_start.unwrap_or(1).max(1);
    let max_end = start_line.saturating_add(MAX_CODE_READ_LINES - 1);
    let end_line = requested_end.unwrap_or(max_end).min(max_end);
    let constrained = requested_start.is_none()
        || requested_end.is_none()
        || requested_end.map(|end| end > max_end).unwrap_or(true);

    obj.insert("startLine".to_string(), json!(start_line));
    obj.insert("endLine".to_string(), json!(end_line));
    if obj.contains_key("start_line") {
        obj.insert("start_line".to_string(), json!(start_line));
    }
    if obj.contains_key("end_line") {
        obj.insert("end_line".to_string(), json!(end_line));
    }

    if !constrained {
        return (serde_json::Value::Object(obj), None);
    }

    let path = obj
        .get("path")
        .and_then(|value| value.as_str())
        .unwrap_or("<unknown>");
    let notice = format!(
        "[代码阅读护栏] {} 的请求已限定为第 {}–{} 行（最多 {} 行）。这是正常预算边界而非工具故障；如需更多证据，请按函数/符号、错误码或 diff hunk 选择相邻范围继续读取，而非重复全文请求。",
        path, start_line, end_line, MAX_CODE_READ_LINES
    );
    (serde_json::Value::Object(obj), Some(notice))
}

pub(crate) fn is_project_session(db: &DbState, session_id: &str) -> bool {
    let Ok(conn) = db.0.lock() else {
        return false;
    };
    crate::commands::projects::project_for_session(&conn, session_id)
        .ok()
        .flatten()
        .is_some()
}

pub(crate) fn dispatch_project_resources(
    db: &DbState,
    session_id: &str,
    action: &str,
    params: &serde_json::Value,
) -> DispatchResult {
    if !is_project_session(db, session_id) {
        return DispatchResult { view: None,
            stdout: String::new(),
            stderr: "当前会话未关联项目，不能访问项目受管资源。".to_string(),
            success: false,
            route: "host_rejection:project_resources_without_project".to_string(),
        };
    }
    let action = if action.trim().is_empty() {
        params
            .get("action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("list_resources")
    } else {
        action
    };
    let conn = match db.0.lock() {
        Ok(conn) => conn,
        Err(error) => {
            return DispatchResult { view: None,
                stdout: String::new(),
                stderr: format!("项目资源账本不可用: {}", error),
                success: false,
                route: "internal:project_resources".to_string(),
            }
        }
    };
    match action {
        "list_resources" => {
            match crate::commands::projects::project_resources_for_session(&conn, session_id) {
                Ok(resources) => {
                    // 提示词承诺"按需分页取回"：offset/limit 必须真实生效且必须有界——
                    // 不传 limit 按默认页返回，超大 limit 收敛到硬上限，否则一次全量
                    // 倾倒就绕过了尾部清单的 20 条展示预算。
                    const LIST_RESOURCES_DEFAULT_LIMIT: usize = 50;
                    const LIST_RESOURCES_MAX_LIMIT: usize = 200;
                    let total = resources.len();
                    let offset = params
                        .get("offset")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0) as usize;
                    let limit = params
                        .get("limit")
                        .and_then(serde_json::Value::as_u64)
                        .map(|v| (v as usize).min(LIST_RESOURCES_MAX_LIMIT))
                        .unwrap_or(LIST_RESOURCES_DEFAULT_LIMIT);
                    let page = resources
                        .into_iter()
                        .skip(offset)
                        .take(limit)
                        .map(|resource| {
                            json!({
                                "resource_id": resource.resource_id,
                                "label": resource.label,
                                "relative_path": resource.relative_path,
                                "kind": resource.kind,
                                "size_bytes": resource.size_bytes,
                            })
                        })
                        .collect::<Vec<_>>();
                    let has_more = offset.saturating_add(page.len()) < total;
                    DispatchResult { view: None,
                        stdout: json!({
                            "resources": page,
                            "total": total,
                            "offset": offset,
                            "limit": limit,
                            "has_more": has_more,
                        })
                        .to_string(),
                        stderr: String::new(),
                        success: true,
                        route: "internal:project_resources/list".to_string(),
                    }
                }
                Err(error) => DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: error,
                    success: false,
                    route: "internal:project_resources/list".to_string(),
                },
            }
        }
        "read_resource" => {
            let Some(resource_id) = params
                .get("resource_id")
                .and_then(serde_json::Value::as_str)
            else {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr:
                        "project_resources.read_resource 缺少 resource_id；请先 list_resources。"
                            .to_string(),
                    success: false,
                    route: "host_rejection:project_resource_missing_id".to_string(),
                };
            };
            match crate::commands::projects::read_project_resource_for_session(
                &conn,
                session_id,
                resource_id,
            ) {
                Ok(preview) => DispatchResult { view: None,
                    stdout: json!({
                        "resource": {
                            "resource_id": preview.resource.resource_id,
                            "label": preview.resource.label,
                            "relative_path": preview.resource.relative_path,
                            "kind": preview.resource.kind,
                            "size_bytes": preview.resource.size_bytes,
                        },
                        "content": preview.content,
                        "truncated": preview.truncated,
                    })
                    .to_string(),
                    stderr: String::new(),
                    success: true,
                    route: "internal:project_resources/read".to_string(),
                },
                Err(error) => DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: error,
                    success: false,
                    route: "host_rejection:project_resource_read".to_string(),
                },
            }
        }
        "register_resource" | "add_resource" | "register" => {
            let raw_path = params
                .get("path")
                .or_else(|| params.get("relative_path"))
                .or_else(|| params.get("file_path"))
                .or_else(|| params.get("canonical_path"))
                .and_then(serde_json::Value::as_str);
            let Some(raw_path) = raw_path else {
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: "project_resources.register_resource 缺少 path 参数（文件相对或绝对路径）。".to_string(),
                    success: false,
                    route: "host_rejection:project_resource_missing_path".to_string(),
                };
            };
            let label = params.get("label").and_then(serde_json::Value::as_str);
            match crate::commands::projects::add_project_resource_for_session(
                &conn,
                session_id,
                raw_path,
                label,
            ) {
                Ok(resource) => DispatchResult { view: None,
                    stdout: json!({
                        "message": "成功将文件登记为项目受管资源，同项目下的所有会话均可按 resource_id 读取。",
                        "resource": {
                            "resource_id": resource.resource_id,
                            "label": resource.label,
                            "relative_path": resource.relative_path,
                            "kind": resource.kind,
                            "size_bytes": resource.size_bytes,
                        }
                    }).to_string(),
                    stderr: String::new(),
                    success: true,
                    route: "internal:project_resources/register".to_string(),
                },
                Err(error) => DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: error,
                    success: false,
                    route: "host_rejection:project_resource_register_failed".to_string(),
                },
            }
        }
        _ => DispatchResult { view: None,
            stdout: String::new(),
            stderr:
                "project_resources 支持 list_resources(offset?, limit?)、read_resource(resource_id: string) 与 register_resource(path: string, label?: string)。"
                    .to_string(),
            success: false,
            route: "host_rejection:project_resource_unknown_action".to_string(),
        },
    }
}

