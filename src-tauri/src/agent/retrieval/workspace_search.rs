use crate::agent::retrieval::types::SearchHit;
use serde_json::Value;
use std::path::Path;

/// 实时代码与文件搜索（避免过期快照；必须显式传入检索目录，由调用方校验）
/// 返回 (命中列表, 是否因正则解析失败降级为字面量匹配, 是否命中数达上限被截断)。
/// 截断位必须如实上报——实测教训：找齐调用点/消费点类检索被默认 limit=3
/// 截断后 total_hits 报"3"冒充完整清单，模型据此漏修残留站点（goal_anchor、
/// run_binding 两场判红同因）。
/// 检索目标为单文件时命中自带 ±3 行上下文窗口：模型对"该文件某区块长什么样"
/// 类问题不再需要换关键词逐行探测（实测单文件曾被换词重搜 45 次）；
/// 目录级检索职责是定位分布，保持单行命中不膨胀。
pub fn search_code(
    search_root: &Path,
    query: &str,
    limit: usize,
) -> Result<(Vec<SearchHit>, bool, bool), String> {
    let clean_query = query.trim();
    if clean_query.is_empty() {
        return Ok((Vec::new(), false, false));
    }

    let max_hits = limit.min(20);
    let context_lines: usize = if search_root.is_file() { 3 } else { 0 };

    // rg --max-count 是【按文件】上限：单文件命中超限会被 rg 静默截掉，
    // Rust 层连第 max_hits+1 条都看不到、无法察觉截断——故按文件上限
    // 传 max_hits+1 做探测哨兵，全局截断位由下方 match_count 计数判定。
    let probe_hits = max_hits + 1;
    let mut output = run_ripgrep(clean_query, search_root, probe_hits, false, context_lines).map_err(|e| {
        format!(
            "无法启动 ripgrep (rg)：{}。代码检索依赖系统安装的 rg，历史检索不受影响",
            e
        )
    })?;

    let mut literal_fallback = false;
    // exit=2 是 rg 的用法/正则解析错误：模型常把 fn foo() 这类含元字符的串当查询词。
    // 降级为 -F 字面量匹配重试一次，而不是把原始正则错误原样抛回引发重复失败。
    if !output.status.success() && output.status.code() == Some(2) {
        output = run_ripgrep(clean_query, search_root, probe_hits, true, context_lines).map_err(|e| {
            format!(
                "无法启动 ripgrep (rg)：{}。代码检索依赖系统安装的 rg，历史检索不受影响",
                e
            )
        })?;
        literal_fallback = true;
    }

    if !output.status.success() && output.status.code() != Some(1) {
        return Err(format!(
            "ripgrep 执行失败 (exit={:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let stdout_str = String::from_utf8_lossy(&output.stdout);

    // 顺序收集 match + context 条目；第 max_hits+1 条 match 出现即停
    // （其前的 context 仍属于最后一条保留命中的尾部窗口）。
    struct RawEntry {
        path: String,
        line_no: u64,
        text: String,
        is_match: bool,
    }
    let mut entries: Vec<RawEntry> = Vec::new();
    let mut match_count = 0usize;
    for line in stdout_str.lines() {
        if let Ok(val) = serde_json::from_str::<Value>(line) {
            let entry_type = val.get("type").and_then(|v| v.as_str());
            if entry_type != Some("match") && entry_type != Some("context") {
                continue;
            }
            if let Some(data) = val.get("data") {
                let path_text = data
                    .get("path")
                    .and_then(|p| p.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let line_no = data
                    .get("line_number")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let text = data
                    .get("lines")
                    .and_then(|l| l.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim_end()
                    .to_string();
                if entry_type == Some("match") {
                    match_count += 1;
                    if match_count > max_hits {
                        break;
                    }
                }
                entries.push(RawEntry {
                    path: path_text,
                    line_no,
                    text,
                    is_match: entry_type == Some("match"),
                });
            }
        }
    }

    let mut hits = Vec::new();
    for (idx, entry) in entries.iter().enumerate() {
        if !entry.is_match {
            continue;
        }
        let rel_path = if search_root.is_file() {
            Path::new(&entry.path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| entry.path.clone())
        } else {
            Path::new(&entry.path)
                .strip_prefix(search_root)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| entry.path.clone())
        };

        // ±context_lines 行窗口：窗口内行号必须连续（跨 run 的断档行不属于本命中），
        // 窗口内可能包含相邻命中行——那是真实上下文，照收不误。
        let snippet = if context_lines > 0 {
            let lo = idx
                .saturating_sub(context_lines)
                .max(0);
            let hi = (idx + context_lines).min(entries.len().saturating_sub(1));
            let mut window = String::new();
            for (j, e) in entries.iter().enumerate().take(hi + 1).skip(lo) {
                // 同文件且行号与目标相邻才算上下文；跑到别的文件/断档段即截断
                if e.path != entry.path || e.line_no.abs_diff(entry.line_no) > context_lines as u64 {
                    continue;
                }
                let marker = if j == idx { '>' } else { ' ' };
                window.push_str(&format!("{}{:5}| {}\n", marker, e.line_no, e.text));
            }
            window.trim_end().to_string()
        } else {
            entry.text.trim().to_string()
        };

        hits.push(SearchHit {
            kind: "code".to_string(),
            id_or_path: format!("{}:{}", rel_path, entry.line_no),
            parent_id: None,
            level: None,
            step_id: None,
            tool: None,
            action: None,
            outcome: None,
            snippet,
            evidence: Vec::new(),
        });
    }

    Ok((hits, literal_fallback, match_count > max_hits))
}

fn run_ripgrep(
    query: &str,
    search_root: &Path,
    max_hits: usize,
    fixed_strings: bool,
    context_lines: usize,
) -> Result<std::process::Output, std::io::Error> {
    let mut command = std::process::Command::new("rg");
    command
        .arg("--json")
        .arg("--max-count")
        .arg(max_hits.to_string())
        .arg("--max-filesize")
        .arg("2M")
        .arg("-i")
        .arg("-g")
        .arg("!target")
        .arg("-g")
        .arg("!node_modules")
        .arg("-g")
        .arg("!dist")
        .arg("-g")
        .arg("!.git");
    if context_lines > 0 {
        command.arg("-C").arg(context_lines.to_string());
    }
    if fixed_strings {
        command.arg("--fixed-strings");
    }
    command.arg(query).arg(search_root).output()
}
