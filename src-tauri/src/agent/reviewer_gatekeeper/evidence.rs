//! 评审证据采集：tmp 产物清单、步骤轨迹摘要与 Office 预览提取。

use super::*;
use crate::agent::context::SandwichContext;
use crate::agent::router::{AgentRouter, ModelRoutingConfig, roles};
use crate::agent::tools::role_manager::resolve_enabled_card;
use crate::db::DbState;
use crate::llm::{self, ModelConfig};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;


/// 会话 tmp 实物审查材料扫描：tmp/ 写入不入副作用台账、不进工作区 git diff——
/// 纯产物任务（PPT/HTML/报告写进会话沙箱）若无本扫描，终审永远 skipped_empty 盲放行。
/// 文本文件给 ≤2000 字符预览+相对 tmp/ 路径；二进制列文件名+大小+扩展名；
/// 总量截断 MAX_DIFF_CHARS。无本轮新文件时返回 None。
pub(crate) fn collect_session_tmp_material(
    session_id: &str,
    run_started_at: chrono::DateTime<chrono::Utc>,
) -> Option<(String, String)> {
    let tmp_dir = crate::agent::path_guard::get_session_tmp_dir(session_id);
    if !tmp_dir.is_dir() {
        return None;
    }
    let since_sys = std::time::UNIX_EPOCH
        + std::time::Duration::from_millis(run_started_at.timestamp_millis().max(0) as u64);
    let mut files = Vec::new();
    GitBaseline::scan_modified_files(&tmp_dir, 0, since_sys, &mut files);
    if files.is_empty() {
        return None;
    }

    let mut sections = Vec::new();
    for path in &files {
        let rel = path
            .strip_prefix(&tmp_dir)
            .map(|r| r.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string_lossy().to_string());
        if is_noise_path(&rel) {
            continue;
        }
        let display = format!("tmp/{}", rel);
        if is_binary_file(&rel) {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("bin")
                .to_string();
            let size_str = path
                .metadata()
                .map(|m| format!("{} KB", m.len() / 1024))
                .unwrap_or_else(|_| "未知".to_string());
            // 办公/数据类二进制（xlsx/docx/pdf 等）抽内容预览——否则审查者只见文件名+大小，
            // "整列空/映射丢字段"这类语义缺陷对终审天然不可见。失败则退回元数据行。
            let preview = extract_office_preview(path);
            if let Some(preview) = preview {
                sections.push(format!(
                    "--- 会话产物: {} (类型: {}, 大小: {}) ---\n{}",
                    display, ext, size_str, preview
                ));
            } else {
                sections.push(format!(
                    "【会话产物·二进制】文件: {} (类型: {}, 大小: {})",
                    display, ext, size_str
                ));
            }
        } else if let Ok(content) = std::fs::read_to_string(path) {
            let sample = if content.chars().count() > 2000 {
                format!(
                    "{}...\n[文件过长截断]",
                    content.chars().take(2000).collect::<String>()
                )
            } else {
                content
            };
            sections.push(format!("--- 会话产物: {} ---\n{}", display, sample));
        }
    }
    let combined = sections.join("\n\n").trim().to_string();
    if combined.is_empty() {
        return None;
    }

    let mut hasher = Sha256::new();
    hasher.update(combined.as_bytes());
    let hash_hex = format!("{:x}", hasher.finalize());

    let final_text = if combined.chars().count() > MAX_DIFF_CHARS {
        format!(
            "{}\n\n... [会话产物材料过长，已安全截断前 {} 字符进行审查]",
            combined.chars().take(MAX_DIFF_CHARS).collect::<String>(),
            MAX_DIFF_CHARS
        )
    } else {
        combined
    };
    Some((final_text, hash_hex))
}

/// 紧凑产物清单：diff 存在时 tmp 材料不走兜底扫描（collect_session_tmp_material
/// 仅在无台账路径调用），审查者只能靠自述提示猜产物存在性、再烧一步 list_dir
/// 确认。此处总是列出本轮 tmp 产物的路径+大小（不含内容预览），让取证请求
/// 直接点名文件。阶段审查复用（中途产物同样可见）。无本轮新产物返回 None。
pub(crate) fn collect_session_tmp_manifest(
    session_id: &str,
    run_started_at: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    let tmp_dir = crate::agent::path_guard::get_session_tmp_dir(session_id);
    if !tmp_dir.is_dir() {
        return None;
    }
    let since_sys = std::time::UNIX_EPOCH
        + std::time::Duration::from_millis(run_started_at.timestamp_millis().max(0) as u64);
    let mut files = Vec::new();
    GitBaseline::scan_modified_files(&tmp_dir, 0, since_sys, &mut files);
    if files.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    for path in &files {
        let rel = path
            .strip_prefix(&tmp_dir)
            .map(|r| r.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string_lossy().to_string());
        if is_noise_path(&rel) {
            continue;
        }
        let size_str = path
            .metadata()
            .map(|m| {
                if m.len() >= 1024 {
                    format!("{} KB", m.len() / 1024)
                } else {
                    format!("{} B", m.len())
                }
            })
            .unwrap_or_else(|_| "未知".to_string());
        lines.push(format!("- tmp/{} ({})", rel, size_str));
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "\n\n【会话 tmp 产物清单（本轮新增 {} 个；仅列名与大小，内容经取证工具核查——read_file tmp/<路径> 或 python）】\n{}",
        lines.len(),
        lines.join("\n")
    ))
}

/// mechanical_only 直通前的产物机械嗅探：零 LLM 的确定性物理检查。
/// 只管无争议的物理异常——0 字节文件、二进制扩展名魔数不符、文本扩展名
/// 含 NUL（UTF-16/32 BOM 豁免）；空表头/缺列/数值口径这类语义判断不归
/// 机械层管（硬编码必误伤——复杂 Excel 首行留白合法），留给审查者用
/// 取证工具裁量。发现异常也不直接判死：伪格式可能被任务授权（fakefmt
/// 实测即如此），升级为 LLM 审查并把异常注入材料供其裁决。
pub(crate) fn sniff_session_tmp_anomalies(session_id: &str) -> Vec<String> {
    use std::io::Read;
    let tmp_dir = crate::agent::path_guard::get_session_tmp_dir(session_id);
    if !tmp_dir.is_dir() {
        return Vec::new();
    }
    let mut files = Vec::new();
    GitBaseline::scan_modified_files(&tmp_dir, 0, std::time::UNIX_EPOCH, &mut files);
    let mut anomalies = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(&tmp_dir)
            .map(|r| r.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string_lossy().to_string());
        if is_noise_path(&rel) {
            continue;
        }
        let Ok(meta) = path.metadata() else { continue };
        if meta.len() == 0 {
            anomalies.push(format!("tmp/{}：0 字节空文件", rel));
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if ext.is_empty() {
            continue;
        }
        let mut buf = [0u8; 4096];
        let n = std::fs::File::open(&path)
            .and_then(|mut f| f.read(&mut buf))
            .unwrap_or(0);
        let head = &buf[..n];
        let violation: Option<&str> = match ext.as_str() {
            "xlsx" | "docx" | "pptx" | "zip" | "jar" | "ods" | "odt" => {
                let ok = head.starts_with(b"PK\x03\x04")
                    || head.starts_with(b"PK\x05\x06")
                    || head.starts_with(b"PK\x07\x08");
                (!ok).then_some("应为 ZIP 容器魔数 PK\\x03\\x04")
            }
            "pdf" => (!head.starts_with(b"%PDF")).then_some("应为 %PDF 魔数"),
            "png" => (!head.starts_with(b"\x89PNG\r\n\x1a\n")).then_some("应为 PNG 魔数"),
            "jpg" | "jpeg" => (!head.starts_with(b"\xff\xd8\xff")).then_some("应为 JPEG 魔数"),
            "gif" => (!head.starts_with(b"GIF8")).then_some("应为 GIF8 魔数"),
            "gz" => (!head.starts_with(b"\x1f\x8b")).then_some("应为 GZIP 魔数"),
            "webp" => (!(head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP"))
                .then_some("应为 RIFF/WEBP 魔数"),
            "mp4" | "m4v" => (!(head.len() >= 8 && &head[4..8] == b"ftyp"))
                .then_some("应为 ISO-BMFF ftyp 盒"),
            // 文本类扩展名的反向检查：首 4KB 含 NUL → 二进制冒充文本；
            // UTF-16/32 BOM 豁免（ASCII 字符编码后也含 0x00）
            "csv" | "md" | "txt" | "json" | "xml" | "html" | "htm" | "log" | "ini" | "yaml"
            | "yml" | "toml" | "rs" | "py" | "js" | "ts" | "tsx" | "css" | "svg" | "sql" | "sh" => {
                let has_bom = head.starts_with(b"\xff\xfe")
                    || head.starts_with(b"\xfe\xff")
                    || head.starts_with(b"\xef\xbb\xbf")
                    || head.starts_with(b"\xff\xfe\x00\x00")
                    || head.starts_with(b"\x00\x00\xfe\xff");
                (!has_bom && head.contains(&0u8)).then_some("文本扩展名但内容含 NUL（二进制冒充）")
            }
            _ => None,
        };
        if let Some(expect) = violation {
            anomalies.push(format!("tmp/{}：扩展名 .{} 与内容魔数不符（{}）", rel, ext, expect));
        }
    }
    anomalies
}

/// 执行轨迹摘要：agent_step_logs 本会话最近 15 步输出的首行（各 ~100 字符）。
/// 审查者据此核对"执行者声称做过的验证"是否真的跑过——自述与轨迹不一致
/// （如声称跑过测试但轨迹无对应步）是最典型的谎报信号；对取证环也是
/// 免费的"已知证据地图"：轨迹里跑过的命令审查者无需重复执行。
/// 阶段审查复用（中途核查"该阶段声称做过的事"同一诉求）。
pub(crate) fn collect_step_trace_digest(db: &DbState, session_id: &str) -> Option<String> {
    let conn = db.0.lock().ok()?;
    let mut stmt = conn
        .prepare(
            "SELECT step_id, raw_output FROM agent_step_logs \
             WHERE session_id = ?1 ORDER BY step_id DESC LIMIT 15",
        )
        .ok()?;
    let mut items: Vec<(i64, String)> = stmt
        .query_map(rusqlite::params![session_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })
        .ok()?
        .flatten()
        .collect();
    if items.is_empty() {
        return None;
    }
    items.reverse(); // 恢复时间升序
    let mut lines = Vec::new();
    for (step_id, raw) in items {
        // 摘要提取按输出形态分派：
        // - JSON 回执（write/list/search 等）→ 抓 message/command/path 字段值，
        //   否则首行永远是 "{" 毫无信息量；
        // - shell 输出 → 状态行 + "$ cmd" 回显行（审查者要看见的是跑了什么命令）；
        // - 其他 → 前两非空行。
        let snippet = step_trace_snippet(&raw);
        if snippet.is_empty() {
            continue;
        }
        lines.push(format!("- step {}: {}", step_id, snippet));
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "\n\n【执行轨迹摘要（执行者实际工具输出摘要，最近 {} 步——可用于交叉核对自述中声称做过的验证是否真实执行）】\n{}",
        lines.len(),
        lines.join("\n")
    ))
}

/// 单步输出的摘要提取：JSON 回执抓关键字段，shell 输出抓状态行+命令回显。
pub(crate) fn step_trace_snippet(raw: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
        for key in ["message", "command", "path", "summary", "error"] {
            if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
                let s = s.trim();
                if !s.is_empty() {
                    return s.chars().take(120).collect();
                }
            }
        }
    }
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .take(2)
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(140)
        .collect()
}

/// 从办公/数据类二进制产物中抽取内容预览（xlsx→sheet markdown、docx/pdf→文本摘录），
/// 让终审能看到数据列内容而非仅文件名+大小。仅对有内容可取的类型返回 Some；
/// 图片/压缩包等无可读文本的类型返回 None，由调用方退回元数据行。
pub(crate) fn extract_office_preview(path: &std::path::Path) -> Option<String> {
    use crate::agent::file_parser::{parse_file, ExtractedContent};
    const MAX_PREVIEW_CHARS: usize = 1800;
    let content = parse_file(path, Some(MAX_PREVIEW_CHARS)).ok()?;
    let text = match content {
        ExtractedContent::Spreadsheet { sheets, summary } => {
            let mut out = summary;
            for s in &sheets {
                out.push_str(&format!(
                    "\n[Sheet: {} | {}行x{}列]\n{}",
                    s.name, s.row_count, s.col_count, s.preview_markdown
                ));
            }
            out
        }
        ExtractedContent::Document { text, .. } | ExtractedContent::Pdf { text, .. } => text,
        ExtractedContent::Text { text, .. } => text,
        _ => return None,
    };
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(if text.chars().count() > MAX_PREVIEW_CHARS {
        format!(
            "{}...\n[预览截断]",
            text.chars().take(MAX_PREVIEW_CHARS).collect::<String>()
        )
    } else {
        text.to_string()
    })
}
