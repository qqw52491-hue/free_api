//! local_file: 原生文件读写与精准局部替换工具（受工作区与临时目录双白名单保护）。
//!
//! 设计目标：
//! 1. 终结大文件全量覆盖改动痛点，提供原子级 `edit`（search-and-replace）能力；
//! 2. 严格的物理路径牢笼（`path_guard::ensure_allowed_path`），严防越界；
//! 3. 严格的 action 白名单校验：只支持 `read`、`write`、`edit`、`list`；
//! 4. edit 唯一性保障：命中多处时精确返回所有匹配行号列表，引导模型一轮内补充上下文修正。

use crate::agent::path_guard::ensure_allowed_path;
use crate::agent::types::DispatchResult;
use crate::db::DbState;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

const MAX_READ_BYTES: usize = 2 * 1024 * 1024; // 2MB 单次读取上限

pub fn dispatch(
    _db: &DbState,
    session_id: &str,
    action: &str,
    params: &serde_json::Value,
    workspace_root: Option<&Path>,
) -> DispatchResult {
    let ws_root = match workspace_root {
        Some(p) => p,
        None => {
            return DispatchResult {
                stdout: String::new(),
                stderr: "❌ [系统错误] 未提供当前活跃的工作区路径，拒绝操作文件。".to_string(),
                success: false,
                route: "local_file:missing_workspace".to_string(),
            };
        }
    };

    let act = if !action.is_empty() {
        action
    } else {
        params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("read")
    };

    match act {
        "read" => execute_read(ws_root, session_id, params),
        "write" => execute_write(ws_root, session_id, params),
        "edit" => execute_edit(ws_root, session_id, params),
        "list" => execute_list(ws_root, session_id, params),
        unknown => DispatchResult {
            stdout: String::new(),
            stderr: format!(
                "❌ [非法动作] local_file 不支持 action='{}'。允许的 action 必须严格为：read、write、edit、list 之一。",
                unknown
            ),
            success: false,
            route: "local_file:invalid_action".to_string(),
        },
    }
}

// =====================================================================
// 乐观并发冲突检测基线 (Optimistic Concurrency / versionGuard)
//
// 机制说明（汲取 mcp-remote-setup 生产实践）：
// 1. read 读取文件时：自动计算 SHA256 哈希存入会话级内存基线表；
// 2. write / edit 写入或局部替换前：比对磁盘当前文件的实时 SHA256；
// 3. 若哈希不一致（说明在 Agent 读取与写入之间，文件已被用户在 IDE 或外部工具修改）：
//    立即硬拦截拒绝，杜绝"丢失更新"（Lost Update）事故；
// 4. 写入成功后立即刷新基线，保证后续连续修改顺畅通过；
// 5. 支持 force: true 逃生通道强制覆盖。
// =====================================================================

#[derive(Debug, Clone)]
pub struct ConflictInfo {
    pub known_hex: String,
    pub live_hex: String,
}

static FILE_BASELINES: LazyLock<Mutex<HashMap<(String, PathBuf), String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn calculate_sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let res = hasher.finalize();
    format!("{:x}", res)
}

pub fn record_file_baseline(session_id: &str, path: &Path, content_bytes: &[u8]) {
    let hash = calculate_sha256_hex(content_bytes);
    if let Ok(mut map) = FILE_BASELINES.lock() {
        map.insert((session_id.to_string(), path.to_path_buf()), hash);
    }
}

pub fn check_file_conflict(
    session_id: &str,
    path: &Path,
    live_bytes: &[u8],
) -> Option<ConflictInfo> {
    let live_hex = calculate_sha256_hex(live_bytes);
    if let Ok(map) = FILE_BASELINES.lock() {
        if let Some(known_hex) = map.get(&(session_id.to_string(), path.to_path_buf())) {
            if *known_hex != live_hex {
                return Some(ConflictInfo {
                    known_hex: known_hex[..12.min(known_hex.len())].to_string(),
                    live_hex: live_hex[..12.min(live_hex.len())].to_string(),
                });
            }
        }
    }
    None
}

/// 从 DispatchResult 中安全判定本次 local_file 操作是否作用于会话临时目录 (session_tmp)
pub fn is_dispatch_result_tmp(result: &DispatchResult) -> bool {
    if !result.success {
        return false;
    }
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&result.stdout) {
        if let Some(b) = val.get("is_tmp").and_then(|v| v.as_bool()) {
            return b;
        }
    }
    result.stdout.contains("\"is_tmp\": true")
}

fn execute_read(
    workspace_root: &Path,
    session_id: &str,
    params: &serde_json::Value,
) -> DispatchResult {
    let raw_path = match params.get("path").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.trim(),
        _ => {
            return DispatchResult {
                stdout: String::new(),
                stderr: "❌ read 操作缺少必填参数 'path'。".to_string(),
                success: false,
                route: "local_file:read:missing_path".to_string(),
            };
        }
    };

    let resolved = match ensure_allowed_path(raw_path, workspace_root, Some(session_id), false) {
        Ok(r) => r,
        Err(e) => {
            return DispatchResult {
                stdout: String::new(),
                stderr: e,
                success: false,
                route: "local_file:read:path_denied".to_string(),
            };
        }
    };

    if !resolved.canonical_path.exists() {
        return DispatchResult {
            stdout: String::new(),
            stderr: format!("❌ 文件不存在: {}", resolved.display_path),
            success: false,
            route: "local_file:read:not_found".to_string(),
        };
    }

    let metadata = match std::fs::metadata(&resolved.canonical_path) {
        Ok(m) => m,
        Err(e) => {
            return DispatchResult {
                stdout: String::new(),
                stderr: format!("无法读取文件元数据 ({}): {}", resolved.display_path, e),
                success: false,
                route: "local_file:read:io_error".to_string(),
            };
        }
    };

    if metadata.len() > MAX_READ_BYTES as u64 {
        return DispatchResult {
            stdout: String::new(),
            stderr: format!(
                "❌ 文件过大 ({} 字节 > 2MB 上限)。请使用 start_line 与 end_line 分片读取。",
                metadata.len()
            ),
            success: false,
            route: "local_file:read:too_large".to_string(),
        };
    }

    let bytes = match std::fs::read(&resolved.canonical_path) {
        Ok(b) => b,
        Err(e) => {
            return DispatchResult {
                stdout: String::new(),
                stderr: format!("读取文件失败 ({}): {}", resolved.display_path, e),
                success: false,
                route: "local_file:read:io_error".to_string(),
            };
        }
    };

    // 🌟 记录乐观并发哈希基线 (versionGuard)
    record_file_baseline(session_id, &resolved.canonical_path, &bytes);

    let content = String::from_utf8_lossy(&bytes);
    let all_lines: Vec<&str> = content.lines().collect();
    let total_lines = all_lines.len();

    let start_line = params
        .get("start_line")
        .or_else(|| params.get("startLine"))
        .and_then(|v| v.as_u64())
        .map(|v| (v as usize).max(1))
        .unwrap_or(1);

    let end_line = params
        .get("end_line")
        .or_else(|| params.get("endLine"))
        .and_then(|v| v.as_u64())
        .map(|v| (v as usize).min(total_lines))
        .unwrap_or(total_lines);

    if start_line > total_lines && total_lines > 0 {
        return DispatchResult {
            stdout: format!("文件共 {} 行，指定的起始行号 {} 超出范围。", total_lines, start_line),
            stderr: String::new(),
            success: true,
            route: "local_file:read:out_of_range".to_string(),
        };
    }

    let slice_start = if total_lines == 0 { 0 } else { start_line - 1 };
    let slice_end = end_line.min(total_lines);

    let mut output = format!(
        "📄 文件: {} | 共 {} 行 | 显示第 {} 至 {} 行\n---\n",
        resolved.display_path, total_lines, start_line, slice_end
    );

    for (idx, line) in all_lines[slice_start..slice_end].iter().enumerate() {
        let line_num = slice_start + idx + 1;
        output.push_str(&format!("{:4}: {}\n", line_num, line));
    }

    DispatchResult {
        stdout: output,
        stderr: String::new(),
        success: true,
        route: "local_file:read:success".to_string(),
    }
}

fn execute_write(
    workspace_root: &Path,
    session_id: &str,
    params: &serde_json::Value,
) -> DispatchResult {
    let raw_path = match params.get("path").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.trim(),
        _ => {
            return DispatchResult {
                stdout: String::new(),
                stderr: "❌ write 操作缺少必填参数 'path'。".to_string(),
                success: false,
                route: "local_file:write:missing_path".to_string(),
            };
        }
    };

    let content = match params.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return DispatchResult {
                stdout: String::new(),
                stderr: "❌ write 操作缺少必填参数 'content'。".to_string(),
                success: false,
                route: "local_file:write:missing_content".to_string(),
            };
        }
    };

    let resolved = match ensure_allowed_path(raw_path, workspace_root, Some(session_id), true) {
        Ok(r) => r,
        Err(e) => {
            return DispatchResult {
                stdout: String::new(),
                stderr: e,
                success: false,
                route: "local_file:write:path_denied".to_string(),
            };
        }
    };

    if let Some(parent) = resolved.canonical_path.parent() {
        if !parent.exists() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return DispatchResult {
                    stdout: String::new(),
                    stderr: format!("创建文件上级目录失败 ({}): {}", parent.display(), e),
                    success: false,
                    route: "local_file:write:create_dir_failed".to_string(),
                };
            }
        }
    }

    // 🌟 乐观并发保护：如果文件已存在且未开启 force，比对哈希基线防丢失更新
    let force = params.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    if !force && resolved.canonical_path.exists() {
        if let Ok(live_bytes) = std::fs::read(&resolved.canonical_path) {
            if let Some(conflict) = check_file_conflict(session_id, &resolved.canonical_path, &live_bytes) {
                let payload = json!({
                    "kind": "optimistic_concurrency_conflict",
                    "path": resolved.display_path,
                    "known_hash": conflict.known_hex,
                    "live_hash": conflict.live_hex,
                    "retryable": true,
                    "safe_to_retry": false,
                    "recommended_action": "read_latest_file_before_write",
                });
                return DispatchResult {
                    stdout: String::new(),
                    stderr: format!(
                        "❌ [并发冲突拦截] 文件 \"{}\" 自上次读取后已被外部修改 (哈希不匹配: 读时={}, 当前={})。\n💡 建议：为防止意外覆盖外部修改（丢失更新），写入已中止。请先调用 local_file.read 读取最新内容，或在确实需要强行覆盖时传入 force: true。\n<!-- 结构化元数据: {} -->",
                        resolved.display_path, conflict.known_hex, conflict.live_hex, payload
                    ),
                    success: false,
                    route: "local_file:write:conflict".to_string(),
                };
            }
        }
    }

    if let Err(e) = std::fs::write(&resolved.canonical_path, content) {
        return DispatchResult {
            stdout: String::new(),
            stderr: format!("写入文件失败 ({}): {}", resolved.display_path, e),
            success: false,
            route: "local_file:write:io_error".to_string(),
        };
    }

    // 写入成功后立即刷新内存基线，保障会话内后续写操作正常进行
    record_file_baseline(session_id, &resolved.canonical_path, content.as_bytes());

    let payload = json!({
        "path": resolved.display_path,
        "is_tmp": resolved.is_session_tmp,
        "size_bytes": content.len(),
        "message": format!("✅ 已成功写入 \"{}\"（共 {} 字节）", resolved.display_path, content.len())
    });

    DispatchResult {
        stdout: serde_json::to_string_pretty(&payload).unwrap_or_default(),
        stderr: String::new(),
        success: true,
        route: "local_file:write:success".to_string(),
    }
}

fn execute_edit(
    workspace_root: &Path,
    session_id: &str,
    params: &serde_json::Value,
) -> DispatchResult {
    let raw_path = match params.get("path").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.trim(),
        _ => {
            return DispatchResult {
                stdout: String::new(),
                stderr: "❌ edit 操作缺少必填参数 'path'。".to_string(),
                success: false,
                route: "local_file:edit:missing_path".to_string(),
            };
        }
    };

    let target_content = match params.get("target_content").and_then(|v| v.as_str()) {
        Some(t) if !t.is_empty() => t,
        _ => {
            return DispatchResult {
                stdout: String::new(),
                stderr: "❌ edit 操作缺少必填参数 'target_content'（待替换的目标原文本）。".to_string(),
                success: false,
                route: "local_file:edit:missing_target".to_string(),
            };
        }
    };

    let replacement_content = match params.get("replacement_content").and_then(|v| v.as_str()) {
        Some(r) => r,
        None => {
            return DispatchResult {
                stdout: String::new(),
                stderr: "❌ edit 操作缺少必填参数 'replacement_content'（替换后的新内容）。".to_string(),
                success: false,
                route: "local_file:edit:missing_replacement".to_string(),
            };
        }
    };

    let resolved = match ensure_allowed_path(raw_path, workspace_root, Some(session_id), false) {
        Ok(r) => r,
        Err(e) => {
            return DispatchResult {
                stdout: String::new(),
                stderr: e,
                success: false,
                route: "local_file:edit:path_denied".to_string(),
            };
        }
    };

    if !resolved.canonical_path.exists() {
        return DispatchResult {
            stdout: String::new(),
            stderr: format!("❌ 待编辑的文件不存在: {}", resolved.display_path),
            success: false,
            route: "local_file:edit:not_found".to_string(),
        };
    }

    let current_content = match std::fs::read_to_string(&resolved.canonical_path) {
        Ok(c) => c,
        Err(e) => {
            return DispatchResult {
                stdout: String::new(),
                stderr: format!("读取待编辑文件失败 ({}): {}", resolved.display_path, e),
                success: false,
                route: "local_file:edit:io_error".to_string(),
            };
        }
    };

    // 🌟 乐观并发保护：比对哈希基线防基于过时上下文产生错误局部替换
    let force = params.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    if !force {
        if let Some(conflict) = check_file_conflict(session_id, &resolved.canonical_path, current_content.as_bytes()) {
            let payload = json!({
                "kind": "optimistic_concurrency_conflict",
                "path": resolved.display_path,
                "known_hash": conflict.known_hex,
                "live_hash": conflict.live_hex,
                "retryable": true,
                "safe_to_retry": false,
                "recommended_action": "read_latest_file_before_edit",
            });
            return DispatchResult {
                stdout: String::new(),
                stderr: format!(
                    "❌ [并发冲突拦截] 文件 \"{}\" 自上次读取后已被外部修改 (哈希不匹配: 读时={}, 当前={})。\n💡 建议：为防止基于过时代码上下文产生错误局部替换，编辑已中止。请先调用 local_file.read 读取最新代码核对行号与上下文，或在确实需要强行修改时传入 force: true。\n<!-- 结构化元数据: {} -->",
                    resolved.display_path, conflict.known_hex, conflict.live_hex, payload
                ),
                success: false,
                route: "local_file:edit:conflict".to_string(),
            };
        }
    }

    // 严格检查匹配次数与位置
    let matches: Vec<(usize, &str)> = current_content.match_indices(target_content).collect();

    if matches.is_empty() {
        return DispatchResult {
            stdout: String::new(),
            stderr: format!(
                "❌ [编辑失败：目标文本未找到] 在文件 \"{}\" 中未找到指定的 target_content。\n💡 建议：请先调用 local_file.read 读取该文件的最新内容，核对准确的文本、换行符和缩进。",
                resolved.display_path
            ),
            success: false,
            route: "local_file:edit:target_not_found".to_string(),
        };
    }

    if matches.len() > 1 {
        let line_numbers: Vec<usize> = matches
            .iter()
            .map(|(idx, _)| current_content[..*idx].lines().count() + 1)
            .collect();
        return DispatchResult {
            stdout: String::new(),
            stderr: format!(
                "❌ [编辑失败：目标文本不唯一] 在文件 \"{}\" 中找到了 {} 处匹配（分别位于第 {:?} 行）。\n💡 建议：为避免误伤，请在 target_content 中多包含前后几行独特的上下文代码，确保在文件中全局唯一后重试。",
                resolved.display_path,
                matches.len(),
                line_numbers
            ),
            success: false,
            route: "local_file:edit:target_ambiguous".to_string(),
        };
    }

    // 唯一匹配成功，计算替换行号并就地替换
    let match_char_offset = matches[0].0;
    let start_line = current_content[..match_char_offset].lines().count() + 1;
    let replaced_lines_count = target_content.lines().count().max(1);
    let end_line = start_line + replaced_lines_count - 1;

    let new_content = current_content.replacen(target_content, replacement_content, 1);

    if let Err(e) = std::fs::write(&resolved.canonical_path, &new_content) {
        return DispatchResult {
            stdout: String::new(),
            stderr: format!("写入编辑结果失败 ({}): {}", resolved.display_path, e),
            success: false,
            route: "local_file:edit:io_error".to_string(),
        };
    }

    // 编辑写入成功后立即刷新内存基线
    record_file_baseline(session_id, &resolved.canonical_path, new_content.as_bytes());

    let payload = json!({
        "path": resolved.display_path,
        "is_tmp": resolved.is_session_tmp,
        "start_line": start_line,
        "end_line": end_line,
        "replaced_bytes": replacement_content.len(),
        "message": format!(
            "✅ 已成功局部修改 \"{}\"（精准替换了第 {} 至 {} 行的内容）",
            resolved.display_path, start_line, end_line
        )
    });

    DispatchResult {
        stdout: serde_json::to_string_pretty(&payload).unwrap_or_default(),
        stderr: String::new(),
        success: true,
        route: "local_file:edit:success".to_string(),
    }
}

fn execute_list(
    workspace_root: &Path,
    session_id: &str,
    params: &serde_json::Value,
) -> DispatchResult {
    let raw_path = params
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or(".");

    let resolved = match ensure_allowed_path(raw_path, workspace_root, Some(session_id), false) {
        Ok(r) => r,
        Err(e) => {
            return DispatchResult {
                stdout: String::new(),
                stderr: e,
                success: false,
                route: "local_file:list:path_denied".to_string(),
            };
        }
    };

    if !resolved.canonical_path.is_dir() {
        return DispatchResult {
            stdout: String::new(),
            stderr: format!("指定的路径不是目录: {}", resolved.display_path),
            success: false,
            route: "local_file:list:not_a_dir".to_string(),
        };
    }

    let read_res = match std::fs::read_dir(&resolved.canonical_path) {
        Ok(r) => r,
        Err(e) => {
            return DispatchResult {
                stdout: String::new(),
                stderr: format!("列取目录失败 ({}): {}", resolved.display_path, e),
                success: false,
                route: "local_file:list:io_error".to_string(),
            };
        }
    };

    let mut entries = Vec::new();
    for entry in read_res.filter_map(|e| e.ok()) {
        let file_name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.path().is_dir();
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        entries.push(json!({
            "name": file_name,
            "is_dir": is_dir,
            "size_bytes": size,
        }));
    }

    entries.sort_by(|a, b| {
        let a_dir = a.get("is_dir").and_then(|v| v.as_bool()).unwrap_or(false);
        let b_dir = b.get("is_dir").and_then(|v| v.as_bool()).unwrap_or(false);
        if a_dir != b_dir {
            b_dir.cmp(&a_dir)
        } else {
            let a_name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let b_name = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
            a_name.cmp(b_name)
        }
    });

    let payload = json!({
        "dir": resolved.display_path,
        "is_tmp": resolved.is_session_tmp,
        "total_items": entries.len(),
        "entries": entries,
    });

    DispatchResult {
        stdout: serde_json::to_string_pretty(&payload).unwrap_or_default(),
        stderr: String::new(),
        success: true,
        route: "local_file:list:success".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("test-local-file-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn mock_db() -> DbState {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)))
    }

    #[test]
    fn test_local_file_write_and_read() {
        let ws = temp_dir();
        let db = mock_db();

        let write_p = json!({
            "path": "hello.txt",
            "content": "line 1\nline 2\nline 3"
        });
        let res_w = dispatch(&db, "test_session", "write", &write_p, Some(&ws));
        assert!(res_w.success, "write should succeed: {}", res_w.stderr);

        let read_p = json!({
            "path": "hello.txt",
            "start_line": 2,
            "end_line": 2
        });
        let res_r = dispatch(&db, "test_session", "read", &read_p, Some(&ws));
        assert!(res_r.success, "read should succeed: {}", res_r.stderr);
        assert!(res_r.stdout.contains("line 2"));
        assert!(!res_r.stdout.contains("line 1"));

        let _ = std::fs::remove_dir_all(ws);
    }

    #[test]
    fn test_local_file_edit_success_and_ambiguity_protection() {
        let ws = temp_dir();
        let db = mock_db();

        let write_p = json!({
            "path": "code.rs",
            "content": "fn foo() {}\nfn bar() {}\nfn foo() {}"
        });
        dispatch(&db, "test_session", "write", &write_p, Some(&ws));

        // 尝试替换重名项 "fn foo() {}" -> 应该报错且告知行号 [1, 3]
        let ambiguous_p = json!({
            "path": "code.rs",
            "target_content": "fn foo() {}",
            "replacement_content": "fn foo_unique() {}"
        });
        let res_amb = dispatch(&db, "test_session", "edit", &ambiguous_p, Some(&ws));
        assert!(!res_amb.success);
        assert!(res_amb.stderr.contains("目标文本不唯一"));
        assert!(res_amb.stderr.contains("行号: [1, 3]") || res_amb.stderr.contains("[1, 3]"));

        // 唯一替换
        let unique_p = json!({
            "path": "code.rs",
            "target_content": "fn bar() {}",
            "replacement_content": "fn bar_renamed() {}"
        });
        let res_uni = dispatch(&db, "test_session", "edit", &unique_p, Some(&ws));
        assert!(res_uni.success, "edit should succeed: {}", res_uni.stderr);
        assert!(res_uni.stdout.contains("精准替换了第 2 至 2 行"));

        // 验证文件已被修改
        let content = std::fs::read_to_string(ws.join("code.rs")).unwrap();
        assert!(content.contains("fn bar_renamed() {}"));

        let _ = std::fs::remove_dir_all(ws);
    }

    #[test]
    fn test_local_file_invalid_action_rejected() {
        let ws = temp_dir();
        let db = mock_db();

        let p = json!({"path": "a.txt"});
        let res = dispatch(&db, "test_session", "patch", &p, Some(&ws));
        assert!(!res.success);
        assert!(res.stderr.contains("非法动作"));

        let _ = std::fs::remove_dir_all(ws);
    }

    #[test]
    fn test_optimistic_lock_normal_read_write_succeeds() {
        let ws = temp_dir();
        let db = mock_db();
        let session = "test_opt_normal";

        // 写入初始文件
        let p_write = json!({"path": "file.txt", "content": "initial content"});
        let res = dispatch(&db, session, "write", &p_write, Some(&ws));
        assert!(res.success);

        // Agent 读取该文件，自动沉淀 baseline
        let p_read = json!({"path": "file.txt"});
        let res_read = dispatch(&db, session, "read", &p_read, Some(&ws));
        assert!(res_read.success);

        // 未发生外部篡改，Agent 再次正常修改 -> 顺利通过
        let p_edit = json!({
            "path": "file.txt",
            "target_content": "initial",
            "replacement_content": "updated"
        });
        let res_edit = dispatch(&db, session, "edit", &p_edit, Some(&ws));
        assert!(res_edit.success, "正常连续修改应通过: {}", res_edit.stderr);

        let _ = std::fs::remove_dir_all(ws);
    }

    #[test]
    fn test_optimistic_lock_external_modification_intercepted() {
        let ws = temp_dir();
        let db = mock_db();
        let session = "test_opt_conflict";

        // 写入初始文件并读取
        let p_write = json!({"path": "file.txt", "content": "initial content"});
        dispatch(&db, session, "write", &p_write, Some(&ws));

        let p_read = json!({"path": "file.txt"});
        dispatch(&db, session, "read", &p_read, Some(&ws));

        // 模拟外部（用户在IDE或外部格式化工具）修改了该文件
        std::fs::write(ws.join("file.txt"), "external user modified content").unwrap();

        // Agent 试图执行全量写入，未带 force -> 应该被并发冲突拦截！
        let p_write2 = json!({"path": "file.txt", "content": "agent overwrite"});
        let res_conflict = dispatch(&db, session, "write", &p_write2, Some(&ws));
        assert!(!res_conflict.success);
        assert!(res_conflict.stderr.contains("并发冲突拦截"));
        assert!(res_conflict.stderr.contains("哈希不匹配"));

        // Agent 试图执行局部编辑，未带 force -> 同样应被拦截！
        let p_edit = json!({
            "path": "file.txt",
            "target_content": "external",
            "replacement_content": "agent edit"
        });
        let res_edit_conflict = dispatch(&db, session, "edit", &p_edit, Some(&ws));
        assert!(!res_edit_conflict.success);
        assert!(res_edit_conflict.stderr.contains("并发冲突拦截"));

        let _ = std::fs::remove_dir_all(ws);
    }

    #[test]
    fn test_optimistic_lock_force_overwrite_bypasses() {
        let ws = temp_dir();
        let db = mock_db();
        let session = "test_opt_force";

        // 写入初始文件并读取
        let p_write = json!({"path": "file.txt", "content": "initial content"});
        dispatch(&db, session, "write", &p_write, Some(&ws));

        let p_read = json!({"path": "file.txt"});
        dispatch(&db, session, "read", &p_read, Some(&ws));

        // 模拟外部修改
        std::fs::write(ws.join("file.txt"), "external user modified content").unwrap();

        // 传入 force: true -> 成功绕过拦截强行覆盖
        let p_force = json!({"path": "file.txt", "content": "forced content", "force": true});
        let res_force = dispatch(&db, session, "write", &p_force, Some(&ws));
        assert!(res_force.success, "带 force 应绕过冲突拦截: {}", res_force.stderr);

        let content = std::fs::read_to_string(ws.join("file.txt")).unwrap();
        assert_eq!(content, "forced content");

        let _ = std::fs::remove_dir_all(ws);
    }
}
