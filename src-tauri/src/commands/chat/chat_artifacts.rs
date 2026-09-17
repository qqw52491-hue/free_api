//! 交付物预览/打开与附件解析命令。

use crate::db::{ChatMessage, ChatSession, DbState};
use crate::llm::{self, ModelConfig, StreamEvent};
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

use super::*;

/// 原生安全在系统中打开交付物文件 (使用系统关联默认程序如 PDF 查看器/Excel/代码编辑器)
#[tauri::command]
pub async fn open_artifact_in_system(app: tauri::AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;

    let canonical = resolve_safe_artifact_path(&path)?;
    let canonical_str = canonical.to_string_lossy().to_string();

    app.opener()
        .open_path(&canonical_str, None::<&str>)
        .map_err(|e| format!("无法在系统默认应用中打开文件: {}", e))?;

    Ok(())
}

/// 在系统文件管理器中高亮定位交付物文件
#[tauri::command]
pub async fn show_artifact_in_folder(app: tauri::AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;

    let canonical = resolve_safe_artifact_path(&path)?;
    let canonical_str = canonical.to_string_lossy().to_string();

    app.opener()
        .reveal_item_in_dir(&canonical_str)
        .map_err(|e| format!("无法在文件夹中定位文件: {}", e))?;

    Ok(())
}

/// 安全读取本地文本/Markdown/HTML/JSON 产物文件内容供前端直接在线预览
#[tauri::command]
pub async fn read_artifact_file(path: String) -> Result<String, String> {
    let canonical = resolve_safe_artifact_path(&path)?;

    // 限制读取文本类文件，防止误读超大二进制导致内存溢出 (上限 10MB)
    let metadata = std::fs::metadata(&canonical).map_err(|e| format!("无法获取文件信息: {}", e))?;
    if metadata.len() > 10 * 1024 * 1024 {
        return Err("文件过大（超过 10MB），建议直接在系统默认程序中打开".to_string());
    }

    std::fs::read_to_string(&canonical).map_err(|e| format!("无法以文本形式读取此文件: {}", e))
}

/// 二进制产物预览渲染结果：office/PDF 经"转 PDF → 逐页 PNG"管道产出
/// base64 内联的 HTML 文档，前端直接塞进既有画布预览通道。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedArtifactPreview {
    pub kind: String, // "html" | "text"
    pub html: Option<String>,
    pub text: Option<String>,
    pub page_count: usize,
    pub truncated: bool,
}

const PREVIEW_MAX_PAGES: u32 = 10;
const PREVIEW_DPI: &str = "110";
const PREVIEW_CMD_TIMEOUT_SECS: u64 = 180;

/// 产物预览渲染入口：文本类直读；pdf 走 pdftoppm；office 系先 soffice 转 pdf。
/// 渲染产物缓存于数据目录 previews/<内容指纹>/，同文件重复预览秒开。
#[tauri::command]
pub async fn render_artifact_preview(path: String) -> Result<RenderedArtifactPreview, String> {
    let canonical = resolve_safe_artifact_path(&path)?;
    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    const TEXT_EXTS: &[&str] = &[
        "md", "markdown", "txt", "html", "htm", "json", "yaml", "yml", "toml", "js", "ts",
        "jsx", "tsx", "vue", "rs", "py", "go", "java", "c", "cpp", "h", "sql", "css", "scss",
        "sh", "svg", "csv", "log", "xml",
    ];
    if TEXT_EXTS.contains(&ext.as_str()) {
        let text = std::fs::read_to_string(&canonical)
            .map_err(|e| format!("无法以文本形式读取此文件: {e}"))?;
        return Ok(RenderedArtifactPreview {
            kind: "text".to_string(),
            html: None,
            text: Some(text),
            page_count: 0,
            truncated: false,
        });
    }

    const OFFICE_EXTS: &[&str] = &[
        "pptx", "ppt", "docx", "doc", "xlsx", "xls", "odt", "ods", "odp", "rtf",
    ];
    let pdf_path = if ext == "pdf" {
        canonical.clone()
    } else if OFFICE_EXTS.contains(&ext.as_str()) {
        convert_office_to_pdf(&canonical).await?
    } else {
        return Err(format!("暂不支持预览 .{ext} 格式，请改用系统程序打开"));
    };

    render_pdf_to_html(&pdf_path).await
}

/// 预览产物缓存目录：源文件路径+mtime 指纹，文件变了自动换目录重渲。
fn preview_cache_dir(src: &std::path::Path) -> std::path::PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.to_string_lossy().hash(&mut h);
    if let Ok(m) = src.metadata().and_then(|m| m.modified()) {
        if let Ok(d) = m.duration_since(std::time::UNIX_EPOCH) {
            d.as_secs().hash(&mut h);
        }
    }
    dirs_next::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("free-api-chat")
        .join("previews")
        .join(format!("{:016x}", h.finish()))
}

/// office → pdf：LibreOffice headless 转换。UserInstallation 指向独立 profile，
/// 避免与用户正在运行的 LibreOffice 实例互斥（soffice 单实例锁）。
async fn convert_office_to_pdf(src: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let cache = preview_cache_dir(src);
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("doc")
        .to_string();
    let pdf = cache.join(format!("{stem}.pdf"));
    if pdf.exists() {
        return Ok(pdf);
    }
    std::fs::create_dir_all(&cache).map_err(|e| format!("无法创建预览缓存目录: {e}"))?;
    let profile = cache.join("lo_profile");
    let profile_url = format!("file://{}", profile.display());
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(PREVIEW_CMD_TIMEOUT_SECS),
        tokio::process::Command::new("soffice")
            .arg(format!("-env:UserInstallation={profile_url}"))
            .args(["--headless", "--convert-to", "pdf", "--outdir"])
            .arg(&cache)
            .arg(src)
            .output(),
    )
    .await
    .map_err(|_| "LibreOffice 转换超时（180s），文档过大或服务无响应".to_string())?
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "未找到 LibreOffice（soffice）。预览 office 文档需安装 libreoffice；或改用系统程序打开。".to_string()
        } else {
            format!("启动 LibreOffice 失败: {e}")
        }
    })?;
    if pdf.exists() {
        Ok(pdf)
    } else {
        let tail = String::from_utf8_lossy(&out.stderr);
        Err(format!(
            "LibreOffice 转换失败（exit {:?}）: {}",
            out.status.code(),
            tail.chars().take(300).collect::<String>()
        ))
    }
}

/// pdf → 逐页 PNG → HTML。pdftoppm 输出 `<prefix>-N.png`（N 按页数零填充），
/// 按数值排序后 base64 内联进 <img>。
async fn render_pdf_to_html(pdf: &std::path::Path) -> Result<RenderedArtifactPreview, String> {
    let cache = preview_cache_dir(pdf);
    std::fs::create_dir_all(&cache).map_err(|e| format!("无法创建预览缓存目录: {e}"))?;
    let prefix = cache.join("page");
    let mut pages = collect_preview_pages(&cache);
    if pages.is_empty() {
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(PREVIEW_CMD_TIMEOUT_SECS),
            tokio::process::Command::new("pdftoppm")
                .args(["-png", "-r", PREVIEW_DPI, "-f", "1", "-l"])
                .arg(PREVIEW_MAX_PAGES.to_string())
                .arg(pdf)
                .arg(&prefix)
                .output(),
        )
        .await
        .map_err(|_| "pdftoppm 渲染超时（180s）".to_string())?
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "未找到 pdftoppm（poppler-utils）。预览 PDF/office 文档需安装 poppler；或改用系统程序打开。"
                    .to_string()
            } else {
                format!("启动 pdftoppm 失败: {e}")
            }
        })?;
        if !out.status.success() {
            return Err(format!(
                "pdftoppm 渲染失败（exit {:?}）: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).chars().take(300).collect::<String>()
            ));
        }
        pages = collect_preview_pages(&cache);
    }
    if pages.is_empty() {
        return Err("渲染未产生任何页面图像（文档可能为空或已加密）".to_string());
    }

    use base64::Engine;
    let mut html = String::from(
        "<div class=\"artifact-doc-preview\" style=\"font-family:system-ui;background:#3a3a3d;padding:16px;display:flex;flex-direction:column;gap:12px;align-items:center;\">",
    );
    for (i, p) in pages.iter().enumerate() {
        let bytes = std::fs::read(p).map_err(|e| format!("读取预览页失败: {e}"))?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        html.push_str(&format!(
            "<img src=\"data:image/png;base64,{b64}\" alt=\"第 {} 页\" style=\"max-width:100%;box-shadow:0 2px 12px rgba(0,0,0,.4);border-radius:4px;\"/>",
            i + 1
        ));
    }
    let truncated = pages.len() as u32 >= PREVIEW_MAX_PAGES && pdf_has_more_pages(pdf, PREVIEW_MAX_PAGES);
    if truncated {
        html.push_str(&format!(
            "<p style=\"color:#bbb;font-size:13px;\">…仅预览前 {PREVIEW_MAX_PAGES} 页，完整文档请用系统程序打开</p>"
        ));
    }
    html.push_str("</div>");
    Ok(RenderedArtifactPreview {
        kind: "html".to_string(),
        html: Some(html),
        text: None,
        page_count: pages.len(),
        truncated,
    })
}

/// 收集缓存目录中已渲染的 page-*.png，按页码数值排序。
fn collect_preview_pages(cache: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut pages: Vec<(u64, std::path::PathBuf)> = std::fs::read_dir(cache)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_str()?.to_string();
                    let num = name
                        .strip_prefix("page-")?
                        .strip_suffix(".png")?
                        .parse::<u64>()
                        .ok()?;
                    Some((num, e.path()))
                })
                .collect()
        })
        .unwrap_or_default();
    pages.sort_by_key(|(n, _)| *n);
    pages.into_iter().map(|(_, p)| p).collect()
}

/// pdfinfo 探测总页数是否超过 cap（拿不到时保守按"未截断"处理——反正页面已渲满）。
fn pdf_has_more_pages(pdf: &std::path::Path, cap: u32) -> bool {
    std::process::Command::new("pdfinfo")
        .arg(pdf)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Pages:").map(|v| v.trim().to_string()))
        })
        .and_then(|v| v.parse::<u32>().ok())
        .map(|total| total > cap)
        .unwrap_or(false)
}

/// 解析本地路径的真实物理绝对路径
#[tauri::command]
pub async fn resolve_artifact_absolute_path(path: String) -> Result<String, String> {
    let canonical = resolve_safe_artifact_path(&path)?;
    Ok(canonical.to_string_lossy().to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedAttachmentResult {
    pub file_name: String,
    pub file_path: String,
    pub size_bytes: u64,
    pub content_type: String,
    pub prompt_text: String,
    pub image_data_url: Option<String>,
    pub raw_preview: Option<String>,
}

/// 解析用户选择或拖拽的本地文件内容
#[tauri::command]
pub async fn parse_attachment_file(path: String) -> Result<ParsedAttachmentResult, String> {
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(format!("文件不存在: {}", path));
    }
    let metadata = std::fs::metadata(p).map_err(|e| format!("获取文件属性失败: {}", e))?;
    let file_name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    let extracted = crate::agent::file_parser::parse_file(p, None)?;
    let prompt_text = crate::agent::file_parser::format_for_prompt(&extracted, &file_name);

    let (content_type, image_data_url, raw_preview) = match &extracted {
        crate::agent::file_parser::ExtractedContent::Text { text, .. } => (
            "text".to_string(),
            None,
            Some(text.chars().take(500).collect()),
        ),
        crate::agent::file_parser::ExtractedContent::Spreadsheet { summary, .. } => {
            ("spreadsheet".to_string(), None, Some(summary.clone()))
        }
        crate::agent::file_parser::ExtractedContent::Document { text, .. } => (
            "document".to_string(),
            None,
            Some(text.chars().take(500).collect()),
        ),
        crate::agent::file_parser::ExtractedContent::Pdf { text, .. } => {
            ("pdf".to_string(), None, Some(text.chars().take(500).collect()))
        }
        crate::agent::file_parser::ExtractedContent::Image {
            mime_type,
            data_url,
            ..
        } => (mime_type.clone(), Some(data_url.clone()), None),
        crate::agent::file_parser::ExtractedContent::Archive { files, .. } => (
            "archive".to_string(),
            None,
            Some(
                files
                    .iter()
                    .take(10)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        ),
        crate::agent::file_parser::ExtractedContent::Binary { mime_guess, .. } => {
            (mime_guess.clone(), None, None)
        }
    };

    Ok(ParsedAttachmentResult {
        file_name,
        file_path: path,
        size_bytes: metadata.len(),
        content_type,
        prompt_text,
        image_data_url,
        raw_preview,
    })
}

/// 保存并解析从前端上传/拖拽的 Base64 文件
#[tauri::command]
pub async fn save_and_parse_uploaded_attachment(
    app: AppHandle,
    state: State<'_, DbState>,
    session_id: String,
    file_name: String,
    base64_data: String,
) -> Result<ParsedAttachmentResult, String> {
    use base64::Engine;
    use tauri::Manager;

    let clean_b64 = if let Some(pos) = base64_data.find("base64,") {
        &base64_data[pos + 7..]
    } else {
        &base64_data
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(clean_b64.trim())
        .map_err(|e| format!("Base64 解码失败: {}", e))?;

    let app_dir = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let attach_dir = app_dir.join("attachments").join(&session_id);
    std::fs::create_dir_all(&attach_dir).map_err(|e| format!("创建附件目录失败: {}", e))?;

    let target_path = attach_dir.join(&file_name);
    std::fs::write(&target_path, &bytes).map_err(|e| format!("保存附件文件失败: {}", e))?;

    let path_str = target_path.to_string_lossy().to_string();
    let mut parsed = parse_attachment_file(path_str.clone()).await?;

    // 📎 附件全文句柄：项目会话自动登记为受管资源，模型后续可按句柄回读全文/结构化摘要
    let handle_line = {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        match crate::commands::projects::register_attachment_for_session(
            &conn,
            &session_id,
            &target_path,
            &file_name,
        ) {
            Ok(Some(resource_id)) => Some(resource_id),
            _ => None,
        }
    };
    if let Some(resource_id) = handle_line {
        parsed.prompt_text = format!(
            "{}\n📎 附件全文句柄已登记：resource_id=`{}`。上方内容若为摘要或截断预览，如需完整内容请调用 project_resources.read_resource(resource_id=\"{}\")。",
            parsed.prompt_text, resource_id, resource_id
        );
    }
    Ok(parsed)
}
