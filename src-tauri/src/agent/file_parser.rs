use calamine::{open_workbook_auto, Data, Reader};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;

/// 文件提取后的结构化内容
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum ExtractedContent {
    /// 纯文本 / 代码 / Markdown / JSON / CSV
    Text {
        text: String,
        total_chars: usize,
        truncated: bool,
    },
    /// Excel 表格结构化提取
    Spreadsheet {
        sheets: Vec<SheetData>,
        summary: String,
    },
    /// Word (.docx) 文档提取
    Document {
        text: String,
        paragraphs_count: usize,
        tables_count: usize,
        truncated: bool,
    },
    /// PDF 文档提取
    Pdf {
        text: String,
        total_chars: usize,
        truncated: bool,
    },
    /// 多模态图片（包含 Data URL，直接适配 Vision 视觉模型）
    Image {
        mime_type: String,
        data_url: String,
        size_bytes: usize,
    },
    /// 压缩包目录结构清单
    Archive {
        files: Vec<String>,
        total_entries: usize,
    },
    /// 二进制文件降级元数据
    Binary {
        file_name: String,
        size_bytes: usize,
        mime_guess: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SheetData {
    pub name: String,
    pub row_count: usize,
    pub col_count: usize,
    pub preview_markdown: String,
}

const DEFAULT_MAX_CHARS: usize = 30_000;

/// 依据文件路径自动嗅探扩展名并提取内容
pub fn parse_file(path: &Path, max_text_chars: Option<usize>) -> Result<ExtractedContent, String> {
    let limit = max_text_chars.unwrap_or(DEFAULT_MAX_CHARS);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    // 🛡️ 安全保护：文件过大直接降级为元数据，防止 OOM
    const MAX_BINARY_BYTES: u64 = 50 * 1024 * 1024;  // 50 MB
    const MAX_IMAGE_BYTES: u64  = 10 * 1024 * 1024;  // 10 MB
    if let Ok(meta) = std::fs::metadata(path) {
        let is_image = matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp");
        let size_limit = if is_image { MAX_IMAGE_BYTES } else { MAX_BINARY_BYTES };
        if meta.len() > size_limit {
            return Ok(ExtractedContent::Binary {
                file_name: file_name.clone(),
                size_bytes: meta.len() as usize,
                mime_guess: if is_image {
                    format!("image/{}", if ext == "jpg" { "jpeg" } else { &ext })
                } else {
                    "application/octet-stream".to_string()
                },
            });
        }
    }

    match ext.as_str() {
        // 1. 表格类文件
        "xlsx" | "xls" | "xlsm" | "xlsb" | "ods" => extract_excel(path, limit),

        // 2. PDF 文档
        "pdf" => extract_pdf(path, limit),

        // 3. Word 文档 (.docx)
        "docx" => {
            let bytes = std::fs::read(path).map_err(|e| format!("读取 docx 失败: {}", e))?;
            extract_docx(&bytes, limit)
        }

        // 4. 常见图片格式（多模态视觉直通）
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" => {
            let bytes = std::fs::read(path).map_err(|e| format!("读取图片失败: {}", e))?;
            extract_image(&file_name, &bytes)
        }

        // 5. 压缩包目录结构
        "zip" => {
            let bytes = std::fs::read(path).map_err(|e| format!("读取 zip 失败: {}", e))?;
            extract_zip_archive(&bytes)
        }

        // 6. 常见文本/代码类格式
        "txt" | "md" | "markdown" | "json" | "yaml" | "yml" | "toml" | "csv" | "tsv" | "xml"
        | "html" | "css" | "js" | "jsx" | "ts" | "tsx" | "rs" | "py" | "go" | "java" | "c"
        | "cpp" | "h" | "hpp" | "sql" | "sh" | "bash" | "zsh" | "dockerfile" | "env" | "log" => {
            let bytes = std::fs::read(path).map_err(|e| format!("读取文本文件失败: {}", e))?;
            extract_text_file(&bytes, limit)
        }

        // 7. 其余尝试 UTF-8 读取，若失败则降级为二进制元数据
        _ => {
            let bytes = std::fs::read(path).map_err(|e| format!("读取文件失败: {}", e))?;
            if let Ok(text) = String::from_utf8(bytes.clone()) {
                let total = text.chars().count();
                let truncated = total > limit;
                let final_text = if truncated {
                    text.chars().take(limit).collect::<String>()
                } else {
                    text
                };
                Ok(ExtractedContent::Text {
                    text: final_text,
                    total_chars: total,
                    truncated,
                })
            } else {
                Ok(ExtractedContent::Binary {
                    file_name,
                    size_bytes: bytes.len(),
                    mime_guess: "application/octet-stream".to_string(),
                })
            }
        }
    }
}

/// 依据内存字节流提取
pub fn parse_bytes(
    file_name: &str,
    bytes: &[u8],
    max_text_chars: Option<usize>,
) -> Result<ExtractedContent, String> {
    let limit = max_text_chars.unwrap_or(DEFAULT_MAX_CHARS);
    let path = Path::new(file_name);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "docx" => extract_docx(bytes, limit),
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" => extract_image(file_name, bytes),
        "zip" => extract_zip_archive(bytes),
        _ => {
            if let Ok(text) = String::from_utf8(bytes.to_vec()) {
                let total = text.chars().count();
                let truncated = total > limit;
                let final_text = if truncated {
                    text.chars().take(limit).collect::<String>()
                } else {
                    text
                };
                Ok(ExtractedContent::Text {
                    text: final_text,
                    total_chars: total,
                    truncated,
                })
            } else {
                Ok(ExtractedContent::Binary {
                    file_name: file_name.to_string(),
                    size_bytes: bytes.len(),
                    mime_guess: "application/octet-stream".to_string(),
                })
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 具体格式解析器
// ─────────────────────────────────────────────────────────────────────────────

/// 1. Excel 表格提取 (.xlsx, .xls, .ods)
fn extract_excel(path: &Path, _max_chars: usize) -> Result<ExtractedContent, String> {
    let mut workbook =
        open_workbook_auto(path).map_err(|e| format!("打开 Excel 失败: {}", e))?;
    let sheet_names = workbook.sheet_names().to_vec();
    let mut sheets = Vec::new();
    let mut total_rows_all_sheets = 0;

    for name in &sheet_names {
        if let Ok(range) = workbook.worksheet_range(name) {
            let row_count = range.height();
            let col_count = range.width();
            total_rows_all_sheets += row_count;

            // 转换前 50 行生成 Markdown 预览
            let preview_rows = 50.min(row_count);
            let mut md = String::new();

            for (r_idx, row) in range.rows().take(preview_rows).enumerate() {
                let cells: Vec<String> = row
                    .iter()
                    .map(|c| match c {
                        Data::Empty => "".to_string(),
                        Data::String(s) => s.replace('|', "\\|").replace('\n', " "),
                        Data::Float(f) => format!("{:.4}", f)
                            .trim_end_matches('0')
                            .trim_end_matches('.')
                            .to_string(),
                        Data::Int(i) => i.to_string(),
                        Data::Bool(b) => b.to_string(),
                        Data::DateTime(dt) => dt.to_string(),
                        Data::DateTimeIso(s) => s.clone(),
                        Data::DurationIso(s) => s.clone(),
                        Data::Error(e) => format!("Error({:?})", e),
                    })
                    .collect();

                md.push_str(&format!("| {} |\n", cells.join(" | ")));
                if r_idx == 0 {
                    let sep: Vec<&str> = (0..cells.len()).map(|_| "---").collect();
                    md.push_str(&format!("| {} |\n", sep.join(" | ")));
                }
            }

            if row_count > preview_rows {
                md.push_str(&format!(
                    "\n> *(第 51 ~ {} 行已省略，总行数: {})*\n",
                    row_count, row_count
                ));
            }

            sheets.push(SheetData {
                name: name.clone(),
                row_count,
                col_count,
                preview_markdown: md,
            });
        }
    }

    let summary = format!(
        "Excel 工作簿包含 {} 个工作表 (Sheets: {:?})，总计约 {} 行数据。",
        sheets.len(),
        sheet_names,
        total_rows_all_sheets
    );

    Ok(ExtractedContent::Spreadsheet { sheets, summary })
}

/// 2. PDF 文档提取
fn extract_pdf(path: &Path, max_chars: usize) -> Result<ExtractedContent, String> {
    let text = pdf_extract::extract_text(path).map_err(|e| format!("解析 PDF 失败: {}", e))?;
    let total = text.chars().count();
    let truncated = total > max_chars;
    let final_text = if truncated {
        text.chars().take(max_chars).collect::<String>()
    } else {
        text
    };

    Ok(ExtractedContent::Pdf {
        text: final_text,
        total_chars: total,
        truncated,
    })
}

/// 3. Word 文档 (.docx) 提取
fn extract_docx(bytes: &[u8], max_chars: usize) -> Result<ExtractedContent, String> {
    let reader = Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| format!("解压 docx 失败: {}", e))?;

    let mut xml_content = String::new();
    {
        let mut document_xml = zip
            .by_name("word/document.xml")
            .map_err(|e| format!("未找到 word/document.xml: {}", e))?;
        document_xml
            .read_to_string(&mut xml_content)
            .map_err(|e| format!("读取 document.xml 失败: {}", e))?;
    }

    // 使用 quick-xml 快速提取所有 `<w:t>` 文本标签
    let mut reader = quick_xml::Reader::from_str(&xml_content);
    reader.config_mut().trim_text(true);

    let mut extracted_text = String::new();
    let mut in_text_node = false;
    let mut paragraphs_count = 0;
    let mut tables_count = 0;

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(ref e)) => match e.name().as_ref() {
                b"w:p" => {
                    paragraphs_count += 1;
                }
                b"w:tbl" => {
                    tables_count += 1;
                }
                b"w:t" => {
                    in_text_node = true;
                }
                _ => {}
            },
            Ok(quick_xml::events::Event::End(ref e)) => match e.name().as_ref() {
                b"w:p" => {
                    extracted_text.push('\n');
                }
                b"w:t" => {
                    in_text_node = false;
                }
                _ => {}
            },
            Ok(quick_xml::events::Event::Text(ref e)) => {
                if in_text_node {
                    if let Ok(txt) = e.unescape() {
                        extracted_text.push_str(&txt);
                    }
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(e) => return Err(format!("XML 解析失败: {:?}", e)),
            _ => {}
        }
        buf.clear();
    }

    let total = extracted_text.chars().count();
    let truncated = total > max_chars;
    let final_text = if truncated {
        extracted_text.chars().take(max_chars).collect::<String>()
    } else {
        extracted_text
    };

    Ok(ExtractedContent::Document {
        text: final_text,
        paragraphs_count,
        tables_count,
        truncated,
    })
}

fn strip_html_to_clean_text(html: &str) -> String {
    let mut s = html.to_string();
    while let Some(start) = s.find("<style") {
        if let Some(end) = s[start..].find("</style>") {
            s.replace_range(start..start + end + 8, "");
        } else {
            break;
        }
    }
    while let Some(start) = s.find("<script") {
        if let Some(end) = s[start..].find("</script>") {
            s.replace_range(start..start + end + 9, "");
        } else {
            break;
        }
    }
    while let Some(start) = s.find("<xml") {
        if let Some(end) = s[start..].find("</xml>") {
            s.replace_range(start..start + end + 6, "");
        } else {
            break;
        }
    }
    while let Some(start) = s.find("<!--") {
        if let Some(end) = s[start..].find("-->") {
            s.replace_range(start..start + end + 3, "");
        } else {
            break;
        }
    }
    s = s.replace("<h1>", "\n# ")
        .replace("</h1>", "\n")
        .replace("<h2>", "\n## ")
        .replace("</h2>", "\n")
        .replace("<h3>", "\n### ")
        .replace("</h3>", "\n")
        .replace("<p>", "\n")
        .replace("</p>", "\n")
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("<li>", "\n- ")
        .replace("</li>", "");
    let mut in_tag = false;
    let mut clean = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            clean.push(c);
        }
    }
    clean.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 4. 纯文本与 HTML 文件提取
fn extract_text_file(bytes: &[u8], max_chars: usize) -> Result<ExtractedContent, String> {
    let raw = String::from_utf8_lossy(bytes).to_string();
    let text = if raw.contains("<html") || raw.contains("<!DOCTYPE") || raw.contains("<body") {
        strip_html_to_clean_text(&raw)
    } else {
        raw
    };
    let total = text.chars().count();
    let truncated = total > max_chars;
    let final_text = if truncated {
        text.chars().take(max_chars).collect::<String>()
    } else {
        text
    };

    Ok(ExtractedContent::Text {
        text: final_text,
        total_chars: total,
        truncated,
    })
}

/// 5. 图片文件提取（Data URL Base64）
fn extract_image(file_name: &str, bytes: &[u8]) -> Result<ExtractedContent, String> {
    use base64::Engine;
    let ext = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_lowercase();
    let mime_type = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        _ => "image/png",
    }
    .to_string();

    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    let data_url = format!("data:{};base64,{}", mime_type, b64);

    Ok(ExtractedContent::Image {
        mime_type,
        data_url,
        size_bytes: bytes.len(),
    })
}

/// 6. Zip 压缩包清单提取
fn extract_zip_archive(bytes: &[u8]) -> Result<ExtractedContent, String> {
    let reader = Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| format!("解压 zip 失败: {}", e))?;
    let mut files = Vec::new();
    let total_entries = zip.len();

    for i in 0..total_entries {
        if let Ok(file) = zip.by_index(i) {
            files.push(file.name().to_string());
        }
    }

    Ok(ExtractedContent::Archive {
        files,
        total_entries,
    })
}

/// 单文件内联上限：2 KB（约 500~600 Tokens，是工具往返成本与直接内联的盈亏平衡点）
pub const INLINE_MAX_BYTES: usize = 2 * 1024;

/// 全局内联预算上限：8 KB，防止多个"小文件"叠加冲垮上下文
pub const INLINE_TOTAL_BUDGET: usize = 8 * 1024;

/// 大纲最多提取条目数
pub const OUTLINE_MAX_ITEMS: usize = 20;

/// 非 Markdown 文件的速览行数
pub const PREVIEW_HEAD_LINES: usize = 15;

/// 格式化字节数大小
pub fn human_size(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

/// 提取结构大纲：Markdown 优先提取 '#' 标题层级，其他文本/代码提取前 N 行
pub fn extract_outline(text: &str, file_name: &str) -> Vec<String> {
    if file_name.ends_with(".md") || file_name.ends_with(".markdown") {
        text.lines()
            .filter_map(|l| {
                let t = l.trim_start();
                if t.starts_with('#') {
                    Some(t.to_string())
                } else {
                    None
                }
            })
            .collect()
    } else {
        text.lines()
            .take(PREVIEW_HEAD_LINES)
            .map(|l| {
                let trimmed = l.trim();
                if trimmed.chars().count() > 80 {
                    format!("| {}…", trimmed.chars().take(80).collect::<String>())
                } else {
                    format!("| {}", trimmed)
                }
            })
            .collect()
    }
}

/// 为中大文件构建轻量 Manifest 档案卡（仅约 150 Tokens）
pub fn build_manifest_card(
    file_name: &str,
    size_bytes: usize,
    text: &str,
    sandbox_path: Option<&str>,
) -> String {
    let line_count = text.lines().count();
    let outline = extract_outline(text, file_name);
    let display_path = sandbox_path.unwrap_or(file_name);

    let mut card = format!(
        "📎【用户已挂载本地参考文件】\n- 文件名: `{}`\n- 大小: {}（共 {} 行）\n- 沙箱访问路径: `{}`\n",
        file_name,
        human_size(size_bytes),
        line_count,
        display_path,
    );

    if !outline.is_empty() {
        if file_name.ends_with(".md") || file_name.ends_with(".markdown") {
            card.push_str("- 结构大纲速览 (章节):\n");
        } else {
            card.push_str("- 首部内容速览:\n");
        }
        for item in outline.iter().take(OUTLINE_MAX_ITEMS) {
            card.push_str(&format!("    {}\n", item));
        }
        if outline.len() > OUTLINE_MAX_ITEMS {
            card.push_str(&format!("    ...（其余 {} 条略）\n", outline.len() - OUTLINE_MAX_ITEMS));
        }
    }

    card.push_str(&format!(
        "💡 提示: 完整正文已在本地沙箱就绪。若需研判具体细节，请使用 `local_file` 按需分段读取，不要凭大纲臆测正文：\n   `local_file(action=\"read\", path=\"{}\", start_line=1, end_line=100)`\n",
        display_path
    ));
    card
}

/// 支持沙箱路径与预算控制的分层 Prompt 格式化
pub fn format_for_prompt_with_sandbox(
    content: &ExtractedContent,
    file_name: &str,
    sandbox_path: Option<&str>,
    can_inline: bool,
) -> (String, usize) {
    match content {
        ExtractedContent::Text {
            text,
            total_chars,
            truncated: _,
        } => {
            let size = text.len();
            if size <= INLINE_MAX_BYTES && can_inline {
                let formatted = format!(
                    "📎【用户已上传文本附件: `{}`】(大小: {}, 已内联全文)\n```\n{}\n```\n",
                    file_name,
                    human_size(size),
                    text
                );
                (formatted, size)
            } else {
                let card = build_manifest_card(file_name, *total_chars.max(&size), text, sandbox_path);
                (card, 0)
            }
        }
        ExtractedContent::Document {
            text,
            paragraphs_count: _,
            tables_count: _,
            truncated: _,
        } => {
            let size = text.len();
            if size <= INLINE_MAX_BYTES && can_inline {
                let formatted = format!(
                    "📎【用户已上传 Word 文档附件: `{}`】(大小: {}, 已内联全文)\n```markdown\n{}\n```\n",
                    file_name,
                    human_size(size),
                    text
                );
                (formatted, size)
            } else {
                let card = build_manifest_card(file_name, size, text, sandbox_path);
                (card, 0)
            }
        }
        ExtractedContent::Pdf {
            text,
            total_chars,
            truncated: _,
        } => {
            let size = text.len();
            if size <= INLINE_MAX_BYTES && can_inline {
                let formatted = format!(
                    "📎【用户已上传 PDF 文档附件: `{}`】(大小: {}, 已内联全文)\n```markdown\n{}\n```\n",
                    file_name,
                    human_size(size),
                    text
                );
                (formatted, size)
            } else {
                let card = build_manifest_card(file_name, *total_chars.max(&size), text, sandbox_path);
                (card, 0)
            }
        }
        ExtractedContent::Spreadsheet { sheets, summary } => {
            let mut out = format!("【用户已上传电子表格附件: `{}`】\n- 摘要: {}\n\n", file_name, summary);
            for s in sheets {
                out.push_str(&format!(
                    "### 工作表: `{}` (共 {} 行, {} 列)\n{}\n\n",
                    s.name, s.row_count, s.col_count, s.preview_markdown
                ));
            }
            (out, 0)
        }
        ExtractedContent::Image {
            mime_type,
            size_bytes,
            ..
        } => {
            let out = format!(
                "【用户已上传图片附件: `{}`】(格式: {}, 大小: {:.1} KB)\n*(图片数据已作为视觉多模态消息携带)*\n",
                file_name,
                mime_type,
                (*size_bytes as f64) / 1024.0
            );
            (out, 0)
        }
        ExtractedContent::Archive {
            files,
            total_entries,
        } => {
            let preview = files.iter().take(40).cloned().collect::<Vec<_>>().join("\n- ");
            let more = if *total_entries > 40 {
                format!("\n... 及其余 {} 个文件", total_entries - 40)
            } else {
                String::new()
            };
            let out = format!(
                "【用户已上传压缩包附件: `{}`】(包含 {} 个文件条目)\n目录结构:\n- {}{}\n",
                file_name, total_entries, preview, more
            );
            (out, 0)
        }
        ExtractedContent::Binary {
            file_name,
            size_bytes,
            mime_guess,
        } => {
            let out = format!(
                "【用户已上传二进制文件: `{}`】(类型: {}, 大小: {:.1} KB)\n",
                file_name,
                mime_guess,
                (*size_bytes as f64) / 1024.0
            );
            (out, 0)
        }
    }
}

/// 保持向后兼容的原有入口
pub fn format_for_prompt(content: &ExtractedContent, file_name: &str) -> String {
    format_for_prompt_with_sandbox(content, file_name, None, true).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_text_and_format() {
        let code = "fn main() { println!(\"Hello\"); }";
        let content = parse_bytes("main.rs", code.as_bytes(), Some(100)).unwrap();
        let prompt = format_for_prompt(&content, "main.rs");
        assert!(prompt.contains("用户已上传文本附件: `main.rs`"));
        assert!(prompt.contains("println!(\"Hello\")"));
    }

    #[test]
    fn test_extract_image_base64() {
        let dummy_png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
        let content = parse_bytes("chart.png", dummy_png, None).unwrap();
        match content {
            ExtractedContent::Image {
                mime_type,
                data_url,
                ..
            } => {
                assert_eq!(mime_type, "image/png");
                assert!(data_url.starts_with("data:image/png;base64,"));
            }
            _ => panic!("Expected image content"),
        }
    }

    #[test]
    fn test_extract_zip_archive() {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("src/main.rs", options).unwrap();
            use std::io::Write;
            zip.write_all(b"fn main() {}").unwrap();
            zip.start_file("README.md", options).unwrap();
            zip.write_all(b"# Test").unwrap();
            zip.finish().unwrap();
        }

        let content = parse_bytes("project.zip", &buf, None).unwrap();
        match content {
            ExtractedContent::Archive {
                files,
                total_entries,
            } => {
                assert_eq!(total_entries, 2);
                assert!(files.contains(&"src/main.rs".to_string()));
                assert!(files.contains(&"README.md".to_string()));
            }
            _ => panic!("Expected archive content"),
        }
    }

    #[test]
    fn test_extract_docx_xml() {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("word/document.xml", options).unwrap();
            use std::io::Write;
            let sample_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
                <w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                    <w:body>
                        <w:p><w:r><w:t>项目技术方案</w:t></w:r></w:p>
                        <w:p><w:r><w:t>采用 Rust + Tauri 架构</w:t></w:r></w:p>
                    </w:body>
                </w:document>"#;
            zip.write_all(sample_xml.as_bytes()).unwrap();
            zip.finish().unwrap();
        }

        let content = parse_bytes("proposal.docx", &buf, None).unwrap();
        match content {
            ExtractedContent::Document {
                text,
                paragraphs_count,
                ..
            } => {
                assert_eq!(paragraphs_count, 2);
                assert!(text.contains("项目技术方案"));
                assert!(text.contains("采用 Rust + Tauri 架构"));
            }
            _ => panic!("Expected docx document content"),
        }
    }

    #[test]
    fn test_format_spreadsheet_prompt() {
        let content = ExtractedContent::Spreadsheet {
            sheets: vec![SheetData {
                name: "销售统计".to_string(),
                row_count: 100,
                col_count: 3,
                preview_markdown: "| 日期 | 品类 | 金额 |\n| --- | --- | --- |\n| 2026-08-01 | 电子 | 5000 |\n".to_string(),
            }],
            summary: "共 1 个工作表，100 行数据".to_string(),
        };

        let prompt = format_for_prompt(&content, "sales.xlsx");
        assert!(prompt.contains("用户已上传电子表格附件: `sales.xlsx`"));
        assert!(prompt.contains("工作表: `销售统计`"));
        assert!(prompt.contains("2026-08-01"));
    }

    #[test]
    fn test_tiered_inline_and_manifest_card() {
        // 1. 小文件 (< 2KB): 直接内联
        let small_text = "# Title\nHello world!".to_string();
        let small_content = ExtractedContent::Text {
            text: small_text.clone(),
            total_chars: small_text.len(),
            truncated: false,
        };
        let (prompt, inlined) = format_for_prompt_with_sandbox(&small_content, "small.md", Some("tmp/small.md"), true);
        assert!(prompt.contains("已内联全文"));
        assert!(prompt.contains("Hello world!"));
        assert_eq!(inlined, small_text.len());

        // 2. 大文件 (> 2KB): 生成 Manifest 卡，不塞全量正文
        let large_text = format!(
            "# 第一章 架构背景\n这是背景说明\n## 1.1 动机\n具体动机\n# 第二章 方案设计\n具体设计\n{}",
            "x".repeat(3000)
        );
        let large_content = ExtractedContent::Text {
            text: large_text.clone(),
            total_chars: large_text.len(),
            truncated: false,
        };
        let (card_prompt, inlined_zero) = format_for_prompt_with_sandbox(&large_content, "arch.md", Some("tmp/arch.md"), true);
        assert_eq!(inlined_zero, 0);
        assert!(card_prompt.contains("📎【用户已挂载本地参考文件】"));
        assert!(card_prompt.contains("沙箱访问路径: `tmp/arch.md`"));
        assert!(card_prompt.contains("# 第一章 架构背景"));
        assert!(card_prompt.contains("## 1.1 动机"));
        assert!(card_prompt.contains("# 第二章 方案设计"));
        assert!(card_prompt.contains("local_file(action=\"read\", path=\"tmp/arch.md\""));
        // 确保没有塞 3000 个 x
        assert!(!card_prompt.contains(&"x".repeat(500)));
    }
}
