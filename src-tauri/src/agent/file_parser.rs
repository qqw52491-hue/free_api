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

/// 4. 纯文本文件提取
fn extract_text_file(bytes: &[u8], max_chars: usize) -> Result<ExtractedContent, String> {
    let text = String::from_utf8_lossy(bytes).to_string();
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

/// 将提取的内容渲染为大模型提示词（LLM Prompt）友好的 Markdown 注入格式
pub fn format_for_prompt(content: &ExtractedContent, file_name: &str) -> String {
    match content {
        ExtractedContent::Text {
            text,
            total_chars,
            truncated,
        } => {
            let trunc_hint = if *truncated {
                format!(
                    " (⚠️ 内容已截断至前 {} 字符，原文共 {} 字符)",
                    text.chars().count(),
                    total_chars
                )
            } else {
                String::new()
            };
            format!(
                "【用户已上传文本附件: `{}`{}】\n```\n{}\n```\n",
                file_name, trunc_hint, text
            )
        }
        ExtractedContent::Spreadsheet { sheets, summary } => {
            let mut out = format!("【用户已上传电子表格附件: `{}`】\n- 摘要: {}\n\n", file_name, summary);
            for s in sheets {
                out.push_str(&format!(
                    "### 工作表: `{}` (共 {} 行, {} 列)\n{}\n\n",
                    s.name, s.row_count, s.col_count, s.preview_markdown
                ));
            }
            out
        }
        ExtractedContent::Document {
            text,
            paragraphs_count,
            tables_count,
            truncated,
        } => {
            let trunc_hint = if *truncated { " (内容过长已截断)" } else { "" };
            format!(
                "【用户已上传 Word 文档附件: `{}`{}】(共 {} 个段落, {} 张表格)\n```markdown\n{}\n```\n",
                file_name, trunc_hint, paragraphs_count, tables_count, text
            )
        }
        ExtractedContent::Pdf {
            text,
            total_chars,
            truncated,
        } => {
            let trunc_hint = if *truncated { " (内容过长已截断)" } else { "" };
            format!(
                "【用户已上传 PDF 文档附件: `{}`{}】(全文共 {} 字符)\n```markdown\n{}\n```\n",
                file_name, trunc_hint, total_chars, text
            )
        }
        ExtractedContent::Image {
            mime_type,
            size_bytes,
            ..
        } => {
            format!(
                "【用户已上传图片附件: `{}`】(格式: {}, 大小: {:.1} KB)\n*(图片数据已作为视觉多模态消息携带)*\n",
                file_name,
                mime_type,
                (*size_bytes as f64) / 1024.0
            )
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
            format!(
                "【用户已上传压缩包附件: `{}`】(包含 {} 个文件条目)\n目录结构:\n- {}{}\n",
                file_name, total_entries, preview, more
            )
        }
        ExtractedContent::Binary {
            file_name,
            size_bytes,
            mime_guess,
        } => {
            format!(
                "【用户已上传二进制文件: `{}`】(类型: {}, 大小: {:.1} KB)\n",
                file_name,
                mime_guess,
                (*size_bytes as f64) / 1024.0
            )
        }
    }
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
}
