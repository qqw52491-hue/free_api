use crate::agent::retrieval::types::SearchHit;
use serde_json::Value;
use std::path::Path;

/// 实时代码与文件搜索（避免过期快照；必须显式传入检索目录，由调用方校验）
/// 返回 (命中列表, 是否因正则解析失败降级为字面量匹配)。
pub fn search_code(
    search_root: &Path,
    query: &str,
    limit: usize,
) -> Result<(Vec<SearchHit>, bool), String> {
    let clean_query = query.trim();
    if clean_query.is_empty() {
        return Ok((Vec::new(), false));
    }

    let max_hits = limit.min(20);

    let mut output = run_ripgrep(clean_query, search_root, max_hits, false)
        .map_err(|e| {
            format!(
                "无法启动 ripgrep (rg)：{}。代码检索依赖系统安装的 rg，历史检索不受影响",
                e
            )
        })?;

    let mut literal_fallback = false;
    // exit=2 是 rg 的用法/正则解析错误：模型常把 fn foo() 这类含元字符的串当查询词。
    // 降级为 -F 字面量匹配重试一次，而不是把原始正则错误原样抛回引发重复失败。
    if !output.status.success() && output.status.code() == Some(2) {
        output = run_ripgrep(clean_query, search_root, max_hits, true)
            .map_err(|e| {
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
    let mut hits = Vec::new();

    for line in stdout_str.lines() {
        if hits.len() >= max_hits {
            break;
        }
        if let Ok(val) = serde_json::from_str::<Value>(line) {
            if val.get("type").and_then(|v| v.as_str()) == Some("match") {
                if let Some(data) = val.get("data") {
                    let path_text = data
                        .get("path")
                        .and_then(|p| p.get("text"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let line_num = data
                        .get("line_number")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let match_lines = data
                        .get("lines")
                        .and_then(|l| l.get("text"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim();

                    let rel_path = if search_root.is_file() {
                        Path::new(path_text)
                            .file_name()
                            .map(|name| name.to_string_lossy().to_string())
                            .unwrap_or_else(|| path_text.to_string())
                    } else {
                        Path::new(path_text)
                            .strip_prefix(search_root)
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|_| path_text.to_string())
                    };

                    hits.push(SearchHit {
                        kind: "code".to_string(),
                        id_or_path: format!("{}:{}", rel_path, line_num),
                        parent_id: None,
                        level: None,
                        step_id: None,
                        tool: None,
                        action: None,
                        outcome: None,
                        snippet: match_lines.to_string(),
                        evidence: Vec::new(),
                    });
                }
            }
        }
    }

    Ok((hits, literal_fallback))
}

fn run_ripgrep(
    query: &str,
    search_root: &Path,
    max_hits: usize,
    fixed_strings: bool,
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
    if fixed_strings {
        command.arg("--fixed-strings");
    }
    command
        .arg(query)
        .arg(search_root)
        .output()
}
