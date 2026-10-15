//! local_file: 原生文件读写与精准局部替换工具（受工作区与临时目录双白名单保护）。
//!
//! 设计目标：
//! 1. 终结大文件全量覆盖改动痛点，提供原子级 `edit`（search-and-replace）能力；
//! 2. 严格的物理路径牢笼（`path_guard::ensure_allowed_path`），严防越界；
//! 3. 严格的 action 白名单校验：只支持 `read`、`write`、`edit`、`list`；
//! 4. edit 唯一性保障：命中多处时精确返回所有匹配行号列表，引导模型一轮内补充上下文修正。

use crate::agent::path_guard::{ensure_allowed_path, ensure_readable_path};
use crate::agent::types::DispatchResult;
use crate::db::DbState;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

const MAX_READ_BYTES: usize = 2 * 1024 * 1024; // 2MB 单次读取上限

/// 有效动作归一（dispatcher 的分类器与本地 dispatch 共用同一口径）：
/// 外层 action 槽被误填写动词（edit/write）但 params.action 显式声明只读
/// （read/list）且不带任何内容字段时，以内层真实意图为准降级为只读。
/// 实测复现：模型发出 {"action":"edit","command":{"action":"read",path..}} 的
/// 纯读请求，被外层标签判成写并撞高危门禁——而按 edit 分发本就会因缺
/// target_content/replacement_content 失败，纠错为 read 无损安全。
/// 带内容字段时写意图与只读声明矛盾——不猜，保留外层交由门禁/缺参报错判定。
pub fn resolve_act<'a>(act: &'a str, params: &'a serde_json::Value) -> &'a str {
    if !act.eq_ignore_ascii_case("write") && !act.eq_ignore_ascii_case("edit") {
        return act;
    }
    let write_shaped = params.get("content").is_some()
        || params.get("target_content").is_some()
        || params.get("replacement_content").is_some();
    if write_shaped {
        return act;
    }
    match params.get("action").and_then(|v| v.as_str()) {
        Some(inner) if inner.eq_ignore_ascii_case("read") => "read",
        Some(inner) if inner.eq_ignore_ascii_case("list") => "list",
        _ => act,
    }
}

pub fn dispatch(
    _db: &DbState,
    session_id: &str,
    action: &str,
    params: &serde_json::Value,
    workspace_root: Option<&Path>,
) -> DispatchResult {
    if let Some(result) = crate::agent::tools::input_validation::native_rejection("local_file", action, params) {
        return result;
    }
    let ws_root = match workspace_root {
        Some(p) => p,
        None => {
            return DispatchResult { view: None,
                stdout: String::new(),
                stderr: "❌ [系统错误] 未提供当前活跃的工作区路径，拒绝操作文件。".to_string(),
                success: false,
                route: "local_file:missing_workspace".to_string(),
            };
        }
    };

    let act = resolve_act(
        if !action.is_empty() {
            action
        } else {
            params
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("read")
        },
        params,
    );

    match act {
        "read" => execute_read(ws_root, session_id, params),
        "write" => execute_write(ws_root, session_id, params),
        "edit" => execute_edit(ws_root, session_id, params),
        "list" => execute_list(ws_root, session_id, params),
        unknown => {
            // 模型常见格式错误（多模型实测复现）：把工具名误填进 action 槽
            // （action='local_file'）。该形状可机械判定——没有任何工具的 action
            // 等于自己的名字。参数呈纯读形状（有行窗或无内容字段）时按缺省 read
            // 处置：read 非破坏且本就是 action 缺省值，纠错比回错省一整轮往返；
            // 带内容字段（写/改意图不明）不猜，回错并附正确范例。
            let write_shaped = params.get("content").is_some()
                || params.get("target_content").is_some()
                || params.get("replacement_content").is_some();
            if unknown == "local_file" && !write_shaped {
                execute_read(ws_root, session_id, params)
            } else {
                let hint = if unknown == "local_file" {
                    "（收到的 action 与工具名相同，本次未执行。若需读取：action='read'；写入：action='write'+content；替换：action='edit'+target_content+replacement_content；列目录：action='list'）"
                } else {
                    ""
                };
                DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!(
                        "❌ [非法动作] local_file 不支持 action='{}'。允许的 action 必须严格为：read、write、edit、list 之一。{}",
                        unknown, hint
                    ),
                    success: false,
                    route: "local_file:invalid_action".to_string(),
                }
            }
        }
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
// 乐观并发冲突与盲写拦截基线 (Optimistic Concurrency / versionGuard)
//
// 机制说明（汲取 mcp-remote-setup 与工业级编辑器实践）：
// 1. read 读取文件时：自动计算 SHA256 哈希存入会话级内存基线表；
// 2. write 写入前（若文件已在磁盘存在）：
//    - 若本会话从未读取过该文件（缺少基线）：硬拦截，防止基于幻觉全量盲写覆盖用户现有文件；
//    - 若基线哈希与磁盘实时哈希不一致：硬拦截，杜绝丢失外部修改（Lost Update）；
//    - 若带 force: true：直接绕过强制写入；
//    - 若是新建文件（磁盘不存在）或私有临时草稿（session_tmp）：直接写入；
// 3. edit 局部替换前：比对磁盘当前文件哈希；若无基线则依赖 target_content 严格唯一匹配物理锚点；
// 4. write / edit 成功后：立即刷新基线，由 Agent 自建的文件在此后具备完备基线保护；
// 5. 纯内存维护、重启清零、在会话删除时通过 cleanup_session_baselines 彻底清理；
// 6. 内置 50,000 条上限安全阀，超限自动轻量削顶，彻底杜绝内存泄漏风险。
// =====================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictInfo {
    pub known_hex: String,
    pub live_hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaselineStatus {
    /// 基线哈希与磁盘实时哈希完全一致（或新自建文件）
    Clean,
    /// 磁盘文件已被外部改动（基线哈希与实时哈希不匹配）
    Conflict(ConflictInfo),
    /// 当前会话尚未建立该文件的基线（从未读取过，也非本次会话新建）
    NoBaseline { live_hex: String },
}

const MAX_GLOBAL_BASELINES: usize = 50_000;

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
        // 全局安全阀：超出 50,000 条时轻量淘汰最早的 5,000 条，杜绝极端长期运行场景下的内存膨胀
        if map.len() >= MAX_GLOBAL_BASELINES {
            let to_remove: Vec<_> = map.keys().take(5_000).cloned().collect();
            for k in to_remove {
                map.remove(&k);
            }
        }
        map.insert((session_id.to_string(), path.to_path_buf()), hash);
    }
}

pub fn check_file_baseline(
    session_id: &str,
    path: &Path,
    live_bytes: &[u8],
) -> BaselineStatus {
    let live_hex = calculate_sha256_hex(live_bytes);
    if let Ok(map) = FILE_BASELINES.lock() {
        if let Some(known_hex) = map.get(&(session_id.to_string(), path.to_path_buf())) {
            if *known_hex != live_hex {
                return BaselineStatus::Conflict(ConflictInfo {
                    known_hex: known_hex[..12.min(known_hex.len())].to_string(),
                    live_hex: live_hex[..12.min(live_hex.len())].to_string(),
                });
            } else {
                return BaselineStatus::Clean;
            }
        }
    }
    BaselineStatus::NoBaseline {
        live_hex: live_hex[..12.min(live_hex.len())].to_string(),
    }
}

pub fn check_file_conflict(
    session_id: &str,
    path: &Path,
    live_bytes: &[u8],
) -> Option<ConflictInfo> {
    match check_file_baseline(session_id, path, live_bytes) {
        BaselineStatus::Conflict(info) => Some(info),
        _ => None,
    }
}

pub fn cleanup_session_baselines(session_id: &str) {
    if let Ok(mut map) = FILE_BASELINES.lock() {
        map.retain(|(sess, _), _| sess != session_id);
    }
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

/// 保留行号别名/默认值；只拒绝结束行早于起始行，不猜测或自动交换区间。
pub(crate) fn validate_read_range(params: &serde_json::Value) -> Result<(), String> {
    let start = params.get("start_line").or_else(|| params.get("startLine"))
        .and_then(serde_json::Value::as_u64).unwrap_or(1);
    if let Some(end) = params.get("end_line").or_else(|| params.get("endLine"))
        .and_then(serde_json::Value::as_u64) {
        if end < start {
            return Err(format!("❌ local_file.read 参数错误：end_line ({end}) 小于 start_line ({start})，尚未读取文件。请提供 end_line >= start_line 的行号区间。"));
        }
    }
    Ok(())
}

fn execute_read(
    workspace_root: &Path,
    session_id: &str,
    params: &serde_json::Value,
) -> DispatchResult {
    if let Err(stderr) = validate_read_range(params) {
        return DispatchResult {
            view: None, stdout: String::new(), stderr, success: false,
            route: super::input_validation::INVALID_INPUT_ROUTE.to_string(),
        };
    }
    let raw_path = match params.get("path").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.trim(),
        _ => {
            return DispatchResult { view: None,
                stdout: String::new(),
                stderr: "❌ read 操作缺少必填参数 'path'。".to_string(),
                success: false,
                route: "local_file:read:missing_path".to_string(),
            };
        }
    };

    let resolved = match ensure_readable_path(raw_path, workspace_root, Some(session_id), false) {
        Ok(r) => r,
        Err(e) => {
            return DispatchResult { view: None,
                stdout: String::new(),
                stderr: e,
                success: false,
                route: "local_file:read:path_denied".to_string(),
            };
        }
    };

    if !resolved.canonical_path.exists() {
        return DispatchResult { view: None,
            stdout: String::new(),
            stderr: format!("❌ 文件不存在: {}", resolved.display_path),
            success: false,
            route: "local_file:read:not_found".to_string(),
        };
    }

    let metadata = match std::fs::metadata(&resolved.canonical_path) {
        Ok(m) => m,
        Err(e) => {
            return DispatchResult { view: None,
                stdout: String::new(),
                stderr: format!("无法读取文件元数据 ({}): {}", resolved.display_path, e),
                success: false,
                route: "local_file:read:io_error".to_string(),
            };
        }
    };

    if metadata.len() > MAX_READ_BYTES as u64 {
        return DispatchResult { view: None,
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
            return DispatchResult { view: None,
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
        return DispatchResult { view: None,
            stdout: format!("文件共 {} 行，指定的起始行号 {} 超出范围。", total_lines, start_line),
            stderr: String::new(),
            success: true,
            route: "local_file:read:out_of_range".to_string(),
        };
    }

    let slice_start = if total_lines == 0 { 0 } else { start_line - 1 };
    let slice_end = end_line.min(total_lines);
    if slice_start > slice_end {
        return DispatchResult {
            view: None, stdout: String::new(),
            stderr: format!("❌ local_file.read 行号区间无法读取：start_line={start_line}，end_line={end_line}，文件共 {total_lines} 行。请提供有效区间。"),
            success: false, route: super::input_validation::INVALID_INPUT_ROUTE.to_string(),
        };
    }

    let mut output = format!(
        "📄 文件: {} | 共 {} 行 | 显示第 {} 至 {} 行\n---\n",
        resolved.display_path, total_lines, start_line, slice_end
    );

    // 导航提示：窗口式读取时提示"可整读"，引导模型一次性建立文件全貌而非碎片化重读。
    // 阈值与 SINGLE_OBSERVATION_INGRESS_CAP(4k tokens) 对齐：含行号前缀后
    // ~300 行是整读无损上限；301~1200 行整读会触发头尾采样（中部经句柄回查）。
    if (start_line > 1 || slice_end < total_lines) && total_lines <= 300 {
        output.push_str(&format!(
            "\n💡 该文件共 {} 行，未指定行号区间即可一次读取全文，避免碎片化重读。\n",
            total_lines
        ));
    } else if (start_line > 1 || slice_end < total_lines) && total_lines <= 1200 {
        output.push_str(&format!(
            "\n💡 该文件共 {} 行：整读会按入口预算截为头尾摘要+output_id 句柄（中部不在上下文内），如需中部内容请按行号窗口定向读取。\n",
            total_lines
        ));
    }

    for (idx, line) in all_lines[slice_start..slice_end].iter().enumerate() {
        let line_num = slice_start + idx + 1;
        output.push_str(&format!("{:4}: {}\n", line_num, line));
    }

    // UI 结构化视图：路径/行区间/总行数 + 前 40 行行号预览（完整输出仍在 stdout，
    // 预览只是卡片缩略，避免把整段文件内容在 step json 里重复持久化一份）。
    const PREVIEW_MAX_LINES: usize = 40;
    let preview_slice = &all_lines[slice_start..slice_end.min(slice_start + PREVIEW_MAX_LINES)];
    let mut preview = String::new();
    for (idx, line) in preview_slice.iter().enumerate() {
        preview.push_str(&format!("{:4}: {}\n", slice_start + idx + 1, line));
    }
    let view = serde_json::json!({
        "kind": "read",
        "path": resolved.display_path,
        "resolved_path": resolved.canonical_path.to_string_lossy(),
        "start_line": start_line,
        "end_line": slice_end,
        "total_lines": total_lines,
        "preview": preview,
        "preview_truncated": slice_end - slice_start > PREVIEW_MAX_LINES,
    });

    DispatchResult {
        view: Some(view),
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
            return DispatchResult { view: None,
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
            return DispatchResult { view: None,
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
            return DispatchResult { view: None,
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
                return DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("创建文件上级目录失败 ({}): {}", parent.display(), e),
                    success: false,
                    route: "local_file:write:create_dir_failed".to_string(),
                };
            }
        }
    }

    // 🌟 乐观并发与盲写拦截保护：如果文件已在磁盘存在且未开启 force
    let force = params.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    if !force && resolved.canonical_path.exists() {
        // 会话专属临时草稿目录 (session_tmp) 为当前会话单点独占的草稿工作区，不受盲写覆盖限制
        if !resolved.is_session_tmp {
            if let Ok(live_bytes) = std::fs::read(&resolved.canonical_path) {
                match check_file_baseline(session_id, &resolved.canonical_path, &live_bytes) {
                    BaselineStatus::Conflict(conflict) => {
                        let payload = json!({
                            "kind": "optimistic_concurrency_conflict",
                            "path": resolved.display_path,
                            "known_hash": conflict.known_hex,
                            "live_hash": conflict.live_hex,
                            "retryable": true,
                            "safe_to_retry": false,
                            "recommended_action": "read_latest_file_before_write",
                        });
                        return DispatchResult { view: None,
                            stdout: String::new(),
                            stderr: format!(
                                "❌ [并发冲突拦截] 文件 \"{}\" 自上次读取后已被外部修改 (哈希不匹配: 读时={}, 当前={})。\n💡 建议：为防止意外覆盖外部修改（丢失更新），写入已中止。请先调用 local_file.read 读取最新内容，或在确实需要强行覆盖时传入 force: true。\n<!-- 结构化元数据: {} -->",
                                resolved.display_path, conflict.known_hex, conflict.live_hex, payload
                            ),
                            success: false,
                            route: "local_file:write:conflict".to_string(),
                        };
                    }
                    BaselineStatus::NoBaseline { live_hex } => {
                        let payload = json!({
                            "kind": "unindexed_file_overwrite_prevention",
                            "path": resolved.display_path,
                            "live_hash": live_hex,
                            "retryable": true,
                            "safe_to_retry": false,
                            "recommended_action": "read_existing_file_before_overwrite",
                        });
                        return DispatchResult { view: None,
                            stdout: String::new(),
                            stderr: format!(
                                "❌ [全量覆盖保护拦截] 待写入的文件 \"{}\" 在磁盘上已存在，但本会话尚未通过 local_file.read 读取过该文件（缺少内容基线）。\n💡 建议：全量写入（write）会彻底覆写既有代码（高爆炸半径）。为防止误覆盖现有代码，请先调用 local_file.read 建立基线核对内容；若确信无需核对并要强行全量覆盖，请显式传入 force: true。\n<!-- 结构化元数据: {} -->",
                                resolved.display_path, payload
                            ),
                            success: false,
                            route: "local_file:write:missing_baseline".to_string(),
                        };
                    }
                    BaselineStatus::Clean => {}
                }
            }
        }
    }

    if let Err(e) = std::fs::write(&resolved.canonical_path, content) {
        return DispatchResult { view: None,
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

    let view = json!({
        "kind": "write",
        "path": resolved.display_path,
        "size_bytes": content.len(),
        "is_tmp": resolved.is_session_tmp,
    });

    DispatchResult {
        view: Some(view),
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
            return DispatchResult { view: None,
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
            return DispatchResult { view: None,
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
            return DispatchResult { view: None,
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
            return DispatchResult { view: None,
                stdout: String::new(),
                stderr: e,
                success: false,
                route: "local_file:edit:path_denied".to_string(),
            };
        }
    };

    if !resolved.canonical_path.exists() {
        return DispatchResult { view: None,
            stdout: String::new(),
            stderr: format!("❌ 待编辑的文件不存在: {}", resolved.display_path),
            success: false,
            route: "local_file:edit:not_found".to_string(),
        };
    }

    let current_content = match std::fs::read_to_string(&resolved.canonical_path) {
        Ok(c) => c,
        Err(e) => {
            return DispatchResult { view: None,
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
            return DispatchResult { view: None,
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
    // CRLF 文件：模型给的 target_content 几乎总是 LF，原样匹配会“目标文本未找到”。
    // 文件是 CRLF、目标不含 \r 且原样匹配不到时，把目标与替换内容都转成 CRLF 再匹配（保持文件原有换行风格）。
    let crlf_pair = if current_content.contains("\r\n")
        && !target_content.contains('\r')
        && !current_content.contains(target_content)
    {
        Some((
            crate::agent::tools::local_file::to_crlf(target_content),
            crate::agent::tools::local_file::to_crlf(replacement_content),
        ))
    } else {
        None
    };
    let (target_content, replacement_content): (&str, &str) = match &crlf_pair {
        Some((t, r)) => (t.as_str(), r.as_str()),
        None => (target_content, replacement_content),
    };
    let matches: Vec<(usize, &str)> = current_content.match_indices(target_content).collect();

    if matches.is_empty() {
        return DispatchResult { view: None,
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
        return DispatchResult { view: None,
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
        return DispatchResult { view: None,
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

    // UI 结构化视图：removed/added 行直供（各截 30 行），前端渲染真 diff 高亮卡，
    // 不再从 stdout 文本正则反推增删数。
    const DIFF_VIEW_MAX_LINES: usize = 30;
    let removed: Vec<&str> = target_content.lines().collect();
    let added: Vec<&str> = replacement_content.lines().collect();
    let view = json!({
        "kind": "diff",
        "path": resolved.display_path,
        "start_line": start_line,
        "end_line": end_line,
        "removed_lines": removed.iter().take(DIFF_VIEW_MAX_LINES).collect::<Vec<_>>(),
        "added_lines": added.iter().take(DIFF_VIEW_MAX_LINES).collect::<Vec<_>>(),
        "removed_total": removed.len(),
        "added_total": added.len(),
        "truncated": removed.len() > DIFF_VIEW_MAX_LINES || added.len() > DIFF_VIEW_MAX_LINES,
    });

    DispatchResult {
        view: Some(view),
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

    let resolved = match ensure_readable_path(raw_path, workspace_root, Some(session_id), false) {
        Ok(r) => r,
        Err(e) => {
            return DispatchResult { view: None,
                stdout: String::new(),
                stderr: e,
                success: false,
                route: "local_file:list:path_denied".to_string(),
            };
        }
    };

    if !resolved.canonical_path.is_dir() {
        return DispatchResult { view: None,
            stdout: String::new(),
            stderr: format!("指定的路径不是目录: {}", resolved.display_path),
            success: false,
            route: "local_file:list:not_a_dir".to_string(),
        };
    }

    let read_res = match std::fs::read_dir(&resolved.canonical_path) {
        Ok(r) => r,
        Err(e) => {
            return DispatchResult { view: None,
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

    DispatchResult { view: None,
        stdout: serde_json::to_string_pretty(&payload).unwrap_or_default(),
        stderr: String::new(),
        success: true,
        route: "local_file:list:success".to_string(),
    }
}

#[cfg(test)]
mod tests;

/// LF → CRLF（先归一再转，避免出现 \r\r\n）。
pub(crate) fn to_crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}
