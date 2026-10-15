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

/// 流程审计用的最近记录：包含历史审批/交付状态，供分流复核等场景追溯。
pub(crate) fn collect_step_trace_digest(db: &DbState, session_id: &str) -> Option<String> {
    collect_trace_digest(db, session_id, false)
}

/// 终审只把工具调用回执作为证据索引，不把历史终审判决再当成本轮验收证据。
pub(crate) fn collect_execution_evidence_digest(db: &DbState, session_id: &str) -> Option<String> {
    collect_trace_digest(db, session_id, true)
}

fn collect_trace_digest(db: &DbState, session_id: &str, execution_only: bool) -> Option<String> {
    // 分类依据是宿主保存的 tool/outcome，不根据输出正文里的词语猜测。
    // 先排除流程回执再取窗口，连续 finish 不会挤走取证/验证回执。
    let mut items = {
        let conn = db.reader().ok()?;
        let mut stmt = conn.prepare(
            "SELECT l.step_id,l.output_id,l.created_at,l.raw_output,f.tool,f.outcome \
             FROM agent_step_logs l LEFT JOIN agent_step_logs_fts f \
             ON f.output_id=l.output_id AND f.session_id=l.session_id \
             WHERE l.session_id=?1 AND (?2=0 OR ( \
               lower(trim(COALESCE(f.tool,''))) NOT IN \
                 ('','core','finish','prepare_finish','submit_plan','fail_task','ask_human','wait_jobs','resume_child') \
               AND lower(COALESCE(f.outcome,''))!='finish_challenged')) \
             ORDER BY julianday(l.created_at) DESC,l.rowid DESC LIMIT 16"
        ).ok()?;
        let rows = stmt
            .query_map(rusqlite::params![session_id, execution_only], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            })
            .ok()?;
        rows.collect::<Result<Vec<_>, _>>().ok()?
    }; // 读取正文/溢出文件前释放数据库连接。
    let has_more = items.len() > 15;
    items.truncate(15);
    items.reverse();
    let mut lines = Vec::new();
    for (step_id, output_id, at, raw, tool, outcome) in items {
        let snippet = crate::agent::run_trace::resolve_raw_output(&raw)
            .map(|text| step_trace_snippet(&text))
            .unwrap_or_else(|| {
                "正文暂不可读取，请按 output_id 核查存储；不能据此判断该操作未执行".into()
            });
        lines.push(format!(
            "- step {step_id} | {at} | tool={} | status={} | output_id={output_id}: {snippet}",
            tool.as_deref().unwrap_or("unknown"),
            outcome.as_deref().unwrap_or("unknown")
        ));
    }
    if execution_only {
        Some(format!(
            "\n\n【工具回执证据索引（{} 条，has_more={}）】\n仅列可归类的工具调用回执；流程回执不在此索引。索引缺项不代表此前未取证，失败回执也不代表命令已执行。可按 output_id 用 read_step_output 分页取原文。\n{}",
            lines.len(), has_more, if lines.is_empty() { "尚无可索引的工具回执".into() } else { lines.join("\n") }
        ))
    } else if lines.is_empty() {
        None
    } else {
        Some(format!("\n\n【流程审计记录（{} 条，has_more={}；包括历史审批/交付回执，不是本次验收结论）】\n{}", lines.len(), has_more, lines.join("\n")))
    }
}

/// 审查者读取本会话的持久化工具原文，沿用 output_id 的会话归属校验。
pub(crate) fn read_review_step_output(
    db: &DbState,
    session_id: &str,
    request: &serde_json::Value,
) -> Result<String, String> {
    let output_id = request
        .get("output_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or("read_step_output.output_id 缺失或不是非空字符串")?;
    let number = |key: &str, default: usize| -> Result<usize, String> {
        match request.get(key) {
            None => Ok(default),
            Some(v) => v
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| format!("read_step_output.{key} 必须为非负整数")),
        }
    };
    let offset = number("offset", 0)?;
    let limit = number("limit", 4000)?;
    if !(1..=16000).contains(&limit) {
        return Err("read_step_output.limit 必须在 1..16000 之间（按字符）".into());
    }
    let raw = crate::agent::run_trace::get_step_output(db, session_id, output_id)?;
    let total_chars = raw.chars().count();
    let content: String = raw.chars().skip(offset).take(limit).collect();
    let end_offset = offset.saturating_add(content.chars().count());
    Ok(
        json!({"output_id":output_id,"offset":offset,"next_offset":end_offset,
        "total_chars":total_chars,"has_more":end_offset<total_chars,"content":content})
        .to_string(),
    )
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

/// 方案评审证据锚点：目标文件（全路径+文件名）、方案引用中的路径、方案文本里反引号包裹的标识符。
/// 用于从执行者排查输出里挑出与方案直接相关的片段。
pub(crate) fn plan_evidence_anchors(
    target_files: &[String],
    evidence_refs: &[String],
    plan_text: &str,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        let s = s.trim();
        if s.len() >= 4 && !out.iter().any(|x| x == s) {
            out.push(s.to_string());
        }
    };
    for f in target_files {
        push(f);
        if let Some(name) = std::path::Path::new(f).file_name().and_then(|n| n.to_str()) {
            push(name);
        }
    }
    for r in evidence_refs {
        if (r.contains('/') || r.contains('.'))
            && r.chars().count() < 200
            && !r.chars().any(char::is_whitespace)
        {
            push(r);
        }
    }
    for (i, seg) in plan_text.split('`').enumerate() {
        if i % 2 == 1 {
            let n = seg.chars().count();
            if (4..=48).contains(&n) && !seg.chars().any(char::is_whitespace) {
                push(seg);
            }
        }
    }
    out.truncate(24);
    out
}

/// 方案评审用：把执行者排查阶段已取得的工具输出（含溢出落盘的大输出）按锚点挑重点摘进证据包，
/// 避免审查者把执行者已经查过的东西重新取证一遍。仅取本次任务（run）启动之后的输出。
pub(crate) fn collect_investigation_digest(
    db: &DbState,
    session_id: &str,
    run_id: &str,
    anchors: &[String],
    budget_chars: usize,
) -> Option<String> {
    let raws: Vec<String> = {
        let conn = db.0.lock().ok()?;
        let since: String = conn
            .query_row(
                "SELECT started_at FROM agent_run_traces WHERE id = ?1",
                rusqlite::params![run_id],
                |r| r.get(0),
            )
            .unwrap_or_default();
        let mut stmt = conn
            .prepare(
                "SELECT raw_output FROM agent_step_logs WHERE session_id = ?1 AND created_at >= ?2 \
                 ORDER BY rowid DESC LIMIT 40",
            )
            .ok()?;
        let rows = stmt
            .query_map(rusqlite::params![session_id, since], |r| r.get::<_, String>(0))
            .ok()?;
        let v: Vec<String> = rows.flatten().collect();
        v
    };
    let texts: Vec<String> = raws
        .iter()
        .filter_map(|r| crate::agent::run_trace::resolve_raw_output(r))
        .collect();
    build_investigation_digest(&texts, anchors, budget_chars)
}

/// 纯函数：texts 为最新在前的步骤输出原文。命中锚点的优先，其次越新越优先；最多 6 步、总量不超 budget。
pub(crate) fn build_investigation_digest(
    texts_newest_first: &[String],
    anchors: &[String],
    budget_chars: usize,
) -> Option<String> {
    const MAX_STEPS: usize = 10;
    const PER_STEP_CHARS: usize = 2000;
    const STDOUT_MARK: &str = "--- STDOUT ---";
    let mut picked: Vec<(bool, usize, String)> = Vec::new();
    let mut seen_cmd = std::collections::HashSet::new();
    for (idx, text) in texts_newest_first.iter().enumerate() {
        let t = text.trim();
        if t.chars().count() < 60 || t.starts_with('❌') || t.contains("[失败") {
            continue;
        }
        let cmd: Option<String> = t
            .lines()
            .find(|l| l.trim_start().starts_with("$ "))
            .map(|l| l.trim().chars().take(160).collect());
        if let Some(c) = &cmd {
            if !seen_cmd.insert(c.clone()) {
                continue;
            }
        }
        let body = match t.find(STDOUT_MARK) {
            Some(p) => &t[p + STDOUT_MARK.len()..],
            None => t,
        };
        let lines: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
        if lines.is_empty() {
            continue;
        }
        let hit_line = lines
            .iter()
            .position(|l| anchors.iter().any(|a| l.contains(a.as_str())));
        let hit = hit_line.is_some()
            || cmd
                .as_deref()
                .map_or(false, |c| anchors.iter().any(|a| c.contains(a.as_str())));
        // 既无命令回显又未命中锚点：多半是回执/等待/方案提交等非排查输出
        if cmd.is_none() && !hit {
            continue;
        }
        let (start, end) = match hit_line {
            Some(i) => (i.saturating_sub(3), (i + 30).min(lines.len())),
            None => (0, 40.min(lines.len())),
        };
        let mut excerpt = String::new();
        for l in &lines[start..end] {
            let l: String = l.chars().take(200).collect();
            if excerpt.chars().count() + l.chars().count() + 1 > PER_STEP_CHARS {
                break;
            }
            excerpt.push_str(&l);
            excerpt.push('\n');
        }
        if excerpt.trim().is_empty() {
            continue;
        }
        picked.push((
            hit,
            idx,
            format!("- {}\n```\n{}```", cmd.as_deref().unwrap_or("（工具输出）"), excerpt),
        ));
    }
    if picked.is_empty() {
        return None;
    }
    picked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut chosen: Vec<(bool, usize, String)> = Vec::new();
    let mut used = 0usize;
    for p in picked {
        if chosen.len() >= MAX_STEPS {
            break;
        }
        let n = p.2.chars().count();
        if used + n > budget_chars {
            continue;
        }
        used += n;
        chosen.push(p);
    }
    if chosen.is_empty() {
        return None;
    }
    chosen.sort_by(|a, b| b.1.cmp(&a.1)); // idx 越大越旧 → 恢复时间升序
    Some(format!(
        "\n\n【🔎 执行者排查阶段已取得的工具输出（宿主原样摘录，非执行者转述；下列内容已有证据，无需重复取证）】\n{}",
        chosen.into_iter().map(|c| c.2).collect::<Vec<_>>().join("\n")
    ))
}

#[cfg(test)]
mod investigation_digest_tests {
    use super::*;

    #[test]
    fn anchors_cover_targets_refs_and_backticked_symbols() {
        let a = plan_evidence_anchors(
            &["lib/toolResult.js".to_string()],
            &["b6c28fde-0711-4ef6-a40c-7805a0998733".to_string(), "mcp-hub.js".to_string()],
            "修改 `attachNormalizedResult` 并保留 `x`",
        );
        assert!(a.contains(&"lib/toolResult.js".to_string()));
        assert!(a.contains(&"toolResult.js".to_string()));
        assert!(a.contains(&"mcp-hub.js".to_string()));
        assert!(a.contains(&"attachNormalizedResult".to_string()));
        assert!(!a.iter().any(|s| s.starts_with("b6c28fde")), "UUID 不应作为锚点");
        assert!(!a.contains(&"x".to_string()));
    }

    #[test]
    fn digest_prefers_anchor_hits_skips_failures_and_dedups() {
        let noise = "✅ [成功] (exit_code: 0)\n$ ls -la\n--- STDOUT ---\nfile_a\nfile_b\nfile_c\nfile_d\nfile_e\nfile_f\n";
        let hit = "✅ [成功] (exit_code: 0)\n$ cat lib/toolResult.js\n--- STDOUT ---\nline1\nline2\nexport function attachNormalizedResult(res) {\n  structuredContent.normalized_result = x;\n}\n";
        let failed = "❌ [失败] (exit_code: 1)\n$ cat nope.js\n--- STDOUT ---\nlib/toolResult.js missing and so on and on and on\n";
        let dup = hit;
        let texts = vec![
            noise.to_string(),   // 最新
            failed.to_string(),
            dup.to_string(),
            "submit ok".to_string(),
        ];
        let anchors = vec!["attachNormalizedResult".to_string()];
        let d = build_investigation_digest(&texts, &anchors, 3000).unwrap();
        assert!(d.contains("attachNormalizedResult"));
        assert!(!d.contains("nope.js"), "失败输出不应入包");
        assert_eq!(d.matches("$ cat lib/toolResult.js").count(), 1);
        // 命中锚点的排在前选中，但输出按时间升序（旧的在前）
        assert!(d.find("cat lib/toolResult.js").unwrap() < d.find("ls -la").unwrap());
        // 预算极小时不超限
        assert!(build_investigation_digest(&texts, &anchors, 10).is_none());
    }
}

/// evidence_refs 引用形态 `路径` / `路径:行` / `路径:起-止`；返回 (路径, 行范围)。
fn parse_ref_range(r: &str) -> (&str, Option<(usize, usize)>) {
    if let Some((p, tail)) = r.rsplit_once(':') {
        let t = tail.trim();
        let parsed = match t.split_once('-') {
            Some((a, b)) => a.trim().parse::<usize>().ok().zip(b.trim().parse::<usize>().ok()),
            None => t.parse::<usize>().ok().map(|a| (a, a)),
        };
        if let Some((a, b)) = parsed {
            if a >= 1 && b >= a {
                return (p.trim(), Some((a, b)));
            }
        }
    }
    (r.trim(), None)
}

/// 去掉引用末尾的行号后缀，只保留路径。
pub(crate) fn strip_ref_range(r: &str) -> &str {
    parse_ref_range(r).0
}

/// 从文件正文摘出引用的片段：有行号用行号（≤120 行），否则用锚点命中行的前后文，都没有就取开头 40 行。
pub(crate) fn render_ref_excerpt(
    content: &str,
    range: Option<(usize, usize)>,
    anchors: &[String],
) -> String {
    const MAX_LINES: usize = 120;
    const MAX_CHARS: usize = 2600;
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    if total == 0 {
        return String::new();
    }
    let (start, end) = match range {
        Some((a, b)) => {
            let s = a.saturating_sub(1);
            if s >= total {
                return format!("（所引行号超出文件范围，文件共 {} 行）\n", total);
            }
            (s, b.min(total).min(s + MAX_LINES))
        }
        None => match lines
            .iter()
            .position(|l| anchors.iter().any(|a| l.contains(a.as_str())))
        {
            Some(i) => (i.saturating_sub(5), (i + 40).min(total)),
            None => (0, 40.min(total)),
        },
    };
    let mut out = String::new();
    let mut last = start;
    for (i, l) in lines.iter().enumerate().take(end).skip(start) {
        let l: String = l.chars().take(240).collect();
        let piece = format!("{}: {}\n", i + 1, l);
        if out.chars().count() + piece.chars().count() > MAX_CHARS {
            break;
        }
        out.push_str(&piece);
        last = i + 1;
    }
    if last < end || (range.is_none() && last < total) {
        out.push_str(&format!(
            "…（显示第 {}–{} 行，文件共 {} 行；需要更多请用 read_file 的 start_line 翻页）\n",
            start + 1,
            last,
            total
        ));
    }
    out
}

/// 把方案的 evidence_refs（文件路径，可带 `:起-止` 行号）由宿主展开为原文片段。
/// 只读工作区与会话 tmp 内的文件；非文本/过大文件（图片、视频等）只给元信息。路径之外的引用（UUID、step_N 等）忽略。
pub(crate) fn resolve_evidence_refs(
    workspace: &Path,
    session_id: &str,
    refs: &[String],
    anchors: &[String],
    budget_chars: usize,
) -> Option<String> {
    let ws = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
    let tmp_root = crate::agent::path_guard::get_session_tmp_dir(session_id);
    let tmp_canon = tmp_root.canonicalize().ok();
    let mut blocks: Vec<String> = Vec::new();
    let mut used = 0usize;
    for r in refs {
        let (path_str, range) = parse_ref_range(r);
        if path_str.is_empty()
            || !(path_str.contains('/') || path_str.contains('.'))
            || path_str.chars().any(char::is_whitespace)
        {
            continue;
        }
        let joined = if let Some(rest) = path_str.strip_prefix("tmp/") {
            tmp_root.join(rest)
        } else {
            let p = Path::new(path_str);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                workspace.join(p)
            }
        };
        let Ok(canon) = joined.canonicalize() else {
            continue;
        };
        let inside = canon.starts_with(&ws)
            || tmp_canon.as_ref().map_or(false, |t| canon.starts_with(t));
        if !inside || !canon.is_file() {
            continue;
        }
        let size = canon.metadata().map(|m| m.len()).unwrap_or(0);
        let text = if is_binary_file(&canon.to_string_lossy()) || size > 2 * 1024 * 1024 {
            None
        } else {
            std::fs::read_to_string(&canon).ok()
        };
        let block = match text {
            Some(t) => {
                let ex = render_ref_excerpt(&t, range, anchors);
                if ex.trim().is_empty() {
                    continue;
                }
                format!("- {}\n```\n{}```", r.trim(), ex)
            }
            None => format!(
                "- {}（{} 字节，非文本或过大，未摘录；审查员可用工具按需查看）",
                path_str, size
            ),
        };
        let n = block.chars().count();
        if used + n > budget_chars {
            continue;
        }
        used += n;
        blocks.push(block);
    }
    if blocks.is_empty() {
        return None;
    }
    Some(format!(
        "\n\n【📎 方案引用的证据原文（宿主按引用的 文件:行号 取出，非执行者转述）】\n{}",
        blocks.join("\n")
    ))
}

#[cfg(test)]
mod evidence_ref_tests {
    use super::*;

    #[test]
    fn ref_range_parsing() {
        assert_eq!(parse_ref_range("lib/a.js:30-80"), ("lib/a.js", Some((30, 80))));
        assert_eq!(parse_ref_range("lib/a.js:7"), ("lib/a.js", Some((7, 7))));
        assert_eq!(parse_ref_range("lib/a.js"), ("lib/a.js", None));
        assert_eq!(parse_ref_range("lib/a.js:80-30"), ("lib/a.js:80-30", None));
        assert_eq!(strip_ref_range("x/y.rs:1-2"), "x/y.rs");
    }

    #[test]
    fn excerpt_uses_range_then_anchor_then_head() {
        let body: String = (1..=200).map(|i| format!("line{}\n", i)).collect();
        let r = render_ref_excerpt(&body, Some((10, 12)), &[]);
        assert!(r.contains("10: line10") && r.contains("12: line12") && !r.contains("13: line13"));
        assert!(!r.contains("显示第"), "范围完整展示不应有截断提示: {}", r);
        let a = render_ref_excerpt(&body, None, &["line100".to_string()]);
        assert!(a.contains("100: line100") && a.contains("95: line95"));
        let h = render_ref_excerpt(&body, None, &[]);
        assert!(h.contains("1: line1\n") && h.contains("文件共 200 行"));
        assert!(render_ref_excerpt(&body, Some((500, 600)), &[]).contains("超出文件范围"));
    }

    #[test]
    fn resolve_refs_reads_workspace_files_and_fences_outside() {
        let dir = std::env::temp_dir().join(format!("evref_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib/a.js"), "one\ntwo\nthree\nfour\n").unwrap();
        let refs = vec![
            "lib/a.js:2-3".to_string(),
            "/etc/hosts".to_string(),
            "b6c28fde-0711-4ef6-a40c-7805a0998733".to_string(),
            "../outside.txt".to_string(),
        ];
        let out = resolve_evidence_refs(&dir, "no-such-session", &refs, &[], 5000).unwrap();
        assert!(out.contains("2: two") && out.contains("3: three") && !out.contains("4: four"));
        assert!(!out.contains("localhost"), "工作区外文件不得读取");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod review_material_regressions {
    use super::*;
    use crate::agent::child_reviewer::retirement_tests::MockReviewer;

    fn put(
        db: &DbState,
        session: &str,
        step: i64,
        time: &str,
        tool: Option<&str>,
        outcome: &str,
        raw: &str,
    ) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let conn = db.0.lock().unwrap();
        conn.execute("INSERT INTO agent_step_logs(output_id,session_id,step_id,raw_output,created_at) VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![id,session,step,raw,time]).unwrap();
        if let Some(tool) = tool {
            crate::agent::retrieval::index_step_fts(
                &conn,
                &id,
                session,
                step as usize,
                tool,
                "test",
                outcome,
                raw,
            )
            .unwrap();
        }
        id
    }

    #[tokio::test]
    async fn finish_trace_is_not_a_list_of_its_own_prior_rejections() {
        let mock = MockReviewer::new(vec![json!({})]).await;
        let proof = put(
            &mock.db,
            "s",
            1,
            "2026-10-15T01:00:00Z",
            Some("local_shell"),
            "succeeded",
            "$ rg permission src\nsrc/auth.rs:10: permission_check()",
        );
        for step in 2..=22 {
            put(
                &mock.db,
                "s",
                step,
                "2026-10-15T02:00:00Z",
                Some("finish"),
                "finish_challenged",
                "HISTORIC_REJECTION: 必须先通过终审才能提交",
            );
        }
        put(
            &mock.db,
            "s",
            23,
            "2026-10-15T03:00:00Z",
            Some("wait_jobs"),
            "succeeded",
            "PLAN_SCORE_81",
        );
        put(
            &mock.db,
            "s",
            24,
            "2026-10-15T04:00:00Z",
            None,
            "failed",
            "UNCLASSIFIED_CONTROL",
        );
        let digest = collect_execution_evidence_digest(&mock.db, "s").unwrap();
        assert!(
            digest.contains("permission_check") && digest.contains(&proof),
            "{digest}"
        );
        assert!(!digest.contains("HISTORIC_REJECTION") && !digest.contains("PLAN_SCORE_81"));
        assert!(!digest.contains("UNCLASSIFIED_CONTROL"));
        assert!(digest.contains("不代表此前未取证"));
        assert!(
            collect_step_trace_digest(&mock.db, "s")
                .unwrap()
                .contains("HISTORIC_REJECTION"),
            "原始审计用途仍可查询历史流程"
        );
    }

    #[tokio::test]
    async fn evidence_restores_spilled_body_and_pages_only_owned_outputs() {
        let mock = MockReviewer::new(vec![json!({})]).await;
        let body = mock.root.join("synthetic-output.log");
        std::fs::write(&body, "CODE_PROOF: permission_check()\nTAIL_EVIDENCE").unwrap();
        let marker = format!("[[spilled_to_disk:{}]]", body.display());
        let id = put(
            &mock.db,
            "s",
            1,
            "2026-10-15T01:00:00Z",
            Some("local_shell"),
            "succeeded",
            &marker,
        );
        let digest = collect_execution_evidence_digest(&mock.db, "s").unwrap();
        assert!(digest.contains("CODE_PROOF") && !digest.contains("spilled_to_disk"));
        let page: serde_json::Value = serde_json::from_str(
            &read_review_step_output(
                &mock.db,
                "s",
                &json!({"output_id":id,"offset":0,"limit":10}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(page["content"], "CODE_PROOF");
        assert_eq!(page["has_more"], true);
        let page: serde_json::Value = serde_json::from_str(
            &read_review_step_output(
                &mock.db,
                "s",
                &json!({"output_id":id,"offset":10,"limit":100}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(page["content"].as_str().unwrap().contains("TAIL_EVIDENCE"));
        assert_eq!(page["has_more"], false);
        assert!(
            read_review_step_output(&mock.db, "other-session", &json!({"output_id":id})).is_err()
        );
        assert!(
            read_review_step_output(&mock.db, "s", &json!({"output_id":id,"offset":"wrong"}))
                .is_err()
        );
    }

    #[tokio::test]
    async fn evidence_window_reports_older_records_without_showing_broken_blob_markers() {
        let mock = MockReviewer::new(vec![json!({})]).await;
        for step in 0..17 {
            put(
                &mock.db,
                "s",
                step,
                "2026-10-15T01:00:00Z",
                Some("local_file"),
                "succeeded",
                "[[cas_blob:invalid-fixture-marker]]",
            );
        }
        let digest = collect_execution_evidence_digest(&mock.db, "s").unwrap();
        assert!(digest.contains("has_more=true") && digest.contains("正文暂不可读取"));
        assert!(!digest.contains("[[cas_blob:"));
    }

    #[tokio::test]
    async fn trace_uses_timestamps_and_keeps_failed_commands_and_source_text() {
        let mock = MockReviewer::new(vec![json!({})]).await;
        put(
            &mock.db,
            "s",
            99,
            "2026-10-15T01:00:00Z",
            Some("read_file"),
            "succeeded",
            "OLDER_PROOF",
        );
        put(
            &mock.db,
            "s",
            1,
            "2026-10-15T02:00:00Z",
            Some("local_shell"),
            "failed",
            "NEW_COMPILE_FAILURE: rustc error",
        );
        put(
            &mock.db,
            "s",
            2,
            "2026-10-15T03:00:00Z",
            Some("local_file"),
            "succeeded",
            "SOURCE_TEXT: 此函数返回‘终审未通过’，不是流程回执",
        );
        let digest = collect_execution_evidence_digest(&mock.db, "s").unwrap();
        assert!(digest.find("OLDER_PROOF").unwrap() < digest.find("NEW_COMPILE_FAILURE").unwrap());
        assert!(digest.contains("failed") && digest.contains("SOURCE_TEXT"));
    }
}
