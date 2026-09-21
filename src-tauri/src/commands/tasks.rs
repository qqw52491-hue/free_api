use crate::agent::task_store;
use crate::db::DbState;
use tauri::{AppHandle, Manager, State};

// ==================== TASK STORE & TRACE COMMANDS ====================

/// 列出当前 session 的任务资源目录。资源记录本体不会混入 Agent prompt。
#[tauri::command]
pub async fn get_agent_task_resources(
    state: State<'_, DbState>,
    session_id: String,
) -> Result<Vec<task_store::TaskResourceRef>, String> {
    task_store::list_resource_refs(state.inner(), &session_id)
}

/// 读取 Agent 自己生成的任务总结 Markdown 正文。路径必须位于 app_data/task_summaries 下。
#[tauri::command]
pub fn read_agent_summary_markdown(app: AppHandle, path: String) -> Result<String, String> {
    let summaries_root = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法定位应用数据目录: {}", error))?
        .join("task_summaries");
    let canonical_root = summaries_root
        .canonicalize()
        .map_err(|_| "任务总结目录不存在或尚未生成总结".to_string())?;
    let requested = std::path::PathBuf::from(path)
        .canonicalize()
        .map_err(|_| "任务总结文件不存在".to_string())?;
    if requested
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("md")
        || !requested.starts_with(&canonical_root)
    {
        return Err("只允许读取 Agent 自动生成的任务总结 Markdown".to_string());
    }
    std::fs::read_to_string(&requested).map_err(|error| format!("无法读取任务总结: {}", error))
}

/// 返回指定会话全部已落盘的运行过程树。该数据仅用于历史复盘，不影响运行态恢复。
#[tauri::command]
pub async fn get_session_run_traces(
    state: State<'_, DbState>,
    session_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    crate::agent::run_trace::list_session_traces(state.inner(), &session_id)
}

/// 分页读取当前 session 任务资源中的记录；底层会执行资源归属校验和 100 条上限。
#[tauri::command]
pub async fn get_agent_task_records(
    state: State<'_, DbState>,
    session_id: String,
    resource_id: String,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    task_store::list_records_page(state.inner(), &session_id, &resource_id, offset, limit)
}

/// 返回内置的只读 Agent 回归评测目标。该目标不修改工作区，只用于真实执行链的行为验证。
#[tauri::command]
pub fn get_agent_read_only_regression_goal() -> String {
    crate::agent::evaluation::READ_ONLY_REGRESSION_GOAL.to_string()
}

/// 创建一次只读回归评测的启动标记，并返回固定评测目标。标记用于隔离同一会话的历史日志。
#[tauri::command]
pub fn begin_agent_read_only_regression(
    session_id: String,
) -> Result<crate::agent::evaluation::AgentEvaluationRun, String> {
    crate::agent::evaluation::begin_agent_evaluation(&session_id)
}

/// 基于已落盘的 LLM 交换日志与运行轨迹，生成指定评测的本地回归报告。
#[tauri::command]
pub fn generate_agent_evaluation_report(
    state: State<'_, DbState>,
    session_id: String,
    evaluation_id: Option<String>,
) -> Result<crate::agent::evaluation::AgentEvaluationReport, String> {
    crate::agent::evaluation::build_and_persist_report(
        state.inner(),
        &session_id,
        evaluation_id.as_deref(),
        None,
    )
}

// ==================== MANAGED ARTIFACT COMMANDS ====================

/// 查询指定会话的全部有效受管产物记录
#[tauri::command]
pub async fn list_session_artifacts(
    state: State<'_, DbState>,
    session_id: String,
) -> Result<Vec<crate::agent::artifact_manager::ArtifactRecord>, String> {
    crate::agent::artifact_manager::list_session_artifacts(state.inner(), &session_id)
}

/// 根据 artifact_id 读取产物文本内容（用于前端富文本/代码/HTML渲染）
#[tauri::command]
pub async fn read_artifact_by_id(
    state: State<'_, DbState>,
    artifact_id: String,
) -> Result<String, String> {
    crate::agent::artifact_manager::read_artifact_content(state.inner(), &artifact_id)
}

/// 根据 artifact_id 用操作系统默认程序打开文件
#[tauri::command]
pub async fn open_artifact_by_id(
    app: AppHandle,
    state: State<'_, DbState>,
    artifact_id: String,
) -> Result<(), String> {
    crate::agent::artifact_manager::open_artifact(&app, state.inner(), &artifact_id)
}

/// 根据 artifact_id 在系统文件管理器中高亮定位
#[tauri::command]
pub async fn reveal_artifact_by_id(
    app: AppHandle,
    state: State<'_, DbState>,
    artifact_id: String,
) -> Result<(), String> {
    crate::agent::artifact_manager::reveal_artifact(&app, state.inner(), &artifact_id)
}

/// 根据 artifact_id 安全删除产物记录与对应磁盘物理文件
#[tauri::command]
pub async fn delete_artifact_by_id(
    state: State<'_, DbState>,
    artifact_id: String,
) -> Result<(), String> {
    crate::agent::artifact_manager::delete_artifact(state.inner(), &artifact_id)
}

/// 将产物或文本另存为到用户指定的外部物理路径 (Native Save As)
#[tauri::command]
pub async fn export_artifact_to_file(
    state: State<'_, DbState>,
    artifact_id: Option<String>,
    source_path: Option<String>,
    content: Option<String>,
    target_path: String,
) -> Result<(), String> {
    let clean_target = if let Some(target) = target_path.strip_prefix("file://") {
        target.to_string()
    } else {
        target_path
    };

    if let Some(text) = content {
        if !text.is_empty() {
            std::fs::write(&clean_target, text.as_bytes())
                .map_err(|e| format!("写入文件失败: {}", e))?;
            return Ok(());
        }
    }

    if let Some(art_id) = artifact_id {
        if let Ok(rec) = crate::agent::artifact_manager::get_artifact_record(state.inner(), &art_id)
        {
            std::fs::copy(&rec.canonical_path, &clean_target)
                .map_err(|e| format!("保存文件失败: {}", e))?;
            return Ok(());
        }
    }

    if let Some(src) = source_path {
        if let Ok(safe_src) = crate::commands::chat::resolve_safe_artifact_path(&src) {
            std::fs::copy(&safe_src, &clean_target).map_err(|e| format!("保存文件失败: {}", e))?;
            return Ok(());
        }
    }

    Err("无法导出文件: 未找到有效的源文件或内容".to_string())
}

/// 将真实文件以原生文件对象形式注入系统剪贴板 (支持在操作系统文件管理器、桌面或聊天软件中 Ctrl+V 粘贴文件)
#[tauri::command]
pub fn copy_file_to_clipboard(path: String) -> Result<(), String> {
    let safe_path = crate::commands::chat::resolve_safe_artifact_path(&path)?;
    let canonical = safe_path
        .canonicalize()
        .map_err(|e| format!("路径无效: {}", e))?;
    let path_str = canonical.to_string_lossy().to_string();

    #[cfg(target_os = "linux")]
    {
        let file_uri = format!("file://{}", path_str);
        let gnome_data = format!("copy\n{}", file_uri);

        // 1. 尝试 wl-copy (Wayland 环境)
        if let Ok(mut child) = std::process::Command::new("wl-copy")
            .arg("--type")
            .arg("text/uri-list")
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(file_uri.as_bytes());
            }
            let _ = child.wait();
        }

        // 2. 尝试 xclip 写入 text/uri-list (X11 / KDE / 通用)
        if let Ok(mut child) = std::process::Command::new("xclip")
            .args(["-selection", "clipboard", "-t", "text/uri-list"])
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(format!("{}\r\n", file_uri).as_bytes());
            }
            let _ = child.wait();
        }

        // 3. 尝试 xclip 写入 x-special/gnome-copied-files (GNOME Nautilus 深度支持)
        if let Ok(mut child) = std::process::Command::new("xclip")
            .args([
                "-selection",
                "clipboard",
                "-t",
                "x-special/gnome-copied-files",
            ])
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(gnome_data.as_bytes());
            }
            let _ = child.wait();
        }
    }

    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!("Set-Clipboard -Path '{}'", path_str),
            ])
            .spawn();
    }

    #[cfg(target_os = "macos")]
    {
        let script = format!("set the clipboard to (POSIX file \"{}\")", path_str);
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .spawn();
    }

    Ok(())
}

/// 剪贴板图片载荷：两种来源分开携带
/// - data_urls: 剪贴板里直接就是图像二进制（截图工具/微信截图），已转 data URL
/// - file_paths: 文件管理器复制的文件（text/uri-list / gnome-copied-files），
///   交给前端复用 attachFilePaths → parse_attachment_file 既有管道
#[derive(serde::Serialize)]
pub struct ClipboardImagePayload {
    pub data_urls: Vec<String>,
    pub file_paths: Vec<String>,
}

/// 原生读取系统剪贴板中的图片/文件（解决 Linux Wayland/WebKitGTK 等桌面环境下
/// DOM paste 事件 clipboardData 为 null 或不携带文件项的问题）。
/// 关键：先枚举剪贴板提供的 MIME 类型再按类型拉取——只查 image/png 会漏掉
/// 文件管理器复制（uri-list）与非 png 截图（image/jpeg/bmp）。
#[tauri::command]
pub fn read_clipboard_image() -> Result<Option<ClipboardImagePayload>, String> {
    use base64::Engine;
    let b64enc = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);

    /// 极简 percent-decode（file:// URI 中的 %20 等转义还原为真实路径）
    fn pct_decode(input: &str) -> String {
        let bytes = input.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            let hex = if bytes[i] == b'%' && i + 2 < bytes.len() {
                bytes
                    .get(i + 1..i + 3)
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|s| u8::from_str_radix(s, 16).ok())
            } else {
                None
            };
            if let Some(b) = hex {
                out.push(b);
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// 从 uri-list / gnome-copied-files 文本中提取本地文件路径
    fn parse_uri_list(text: &str) -> Vec<String> {
        let mut paths = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line == "copy" || line == "cut" {
                continue;
            }
            if let Some(rest) = line.strip_prefix("file://") {
                let p = std::path::PathBuf::from(pct_decode(rest));
                if p.is_file() {
                    paths.push(p.to_string_lossy().into_owned());
                }
            }
        }
        paths
    }

    #[cfg(target_os = "linux")]
    {
        // ── Wayland: wl-paste ──
        if let Ok(list) = std::process::Command::new("wl-paste")
            .args(["--list-types"])
            .output()
        {
            let types: Vec<String> = String::from_utf8_lossy(&list.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .collect();
            let mut payload = ClipboardImagePayload { data_urls: vec![], file_paths: vec![] };
            // 1. 直接图像类型（截图：image/png、image/jpeg 等）
            for mime in types.iter().filter(|t| t.starts_with("image/")) {
                if let Ok(out) = std::process::Command::new("wl-paste")
                    .args(["--type", mime])
                    .output()
                {
                    if out.status.success() && !out.stdout.is_empty() {
                        payload.data_urls.push(format!("data:{};base64,{}", mime, b64enc(&out.stdout)));
                    }
                }
            }
            // 2. 文件管理器复制：uri-list → 解析出真实文件路径
            if types.iter().any(|t| t == "text/uri-list") {
                if let Ok(out) = std::process::Command::new("wl-paste")
                    .args(["--type", "text/uri-list"])
                    .output()
                {
                    payload.file_paths.extend(parse_uri_list(&String::from_utf8_lossy(&out.stdout)));
                }
            }
            if types.iter().any(|t| t.contains("gnome-copied-files")) {
                if let Ok(out) = std::process::Command::new("wl-paste")
                    .args(["--type", "x-special/gnome-copied-files"])
                    .output()
                {
                    payload.file_paths.extend(parse_uri_list(&String::from_utf8_lossy(&out.stdout)));
                }
            }
            if !payload.data_urls.is_empty() || !payload.file_paths.is_empty() {
                payload.file_paths.sort();
                payload.file_paths.dedup();
                return Ok(Some(payload));
            }
        }

        // ── X11 / 通用: xclip ──
        if let Ok(list) = std::process::Command::new("xclip")
            .args(["-selection", "clipboard", "-t", "TARGETS", "-o"])
            .output()
        {
            let types: Vec<String> = String::from_utf8_lossy(&list.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .collect();
            let mut payload = ClipboardImagePayload { data_urls: vec![], file_paths: vec![] };
            for mime in types.iter().filter(|t| t.starts_with("image/")) {
                if let Ok(out) = std::process::Command::new("xclip")
                    .args(["-selection", "clipboard", "-t", mime, "-o"])
                    .output()
                {
                    if out.status.success() && !out.stdout.is_empty() {
                        payload.data_urls.push(format!("data:{};base64,{}", mime, b64enc(&out.stdout)));
                    }
                }
            }
            if types.iter().any(|t| t == "text/uri-list") {
                if let Ok(out) = std::process::Command::new("xclip")
                    .args(["-selection", "clipboard", "-t", "text/uri-list", "-o"])
                    .output()
                {
                    payload.file_paths.extend(parse_uri_list(&String::from_utf8_lossy(&out.stdout)));
                }
            }
            if !payload.data_urls.is_empty() || !payload.file_paths.is_empty() {
                payload.file_paths.sort();
                payload.file_paths.dedup();
                return Ok(Some(payload));
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        let script = "Add-Type -AssemblyName System.Windows.Forms; $img = [System.Windows.Forms.Clipboard]::GetImage(); if ($img) { $ms = New-Object System.IO.MemoryStream; $img.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png); [Convert]::ToBase64String($ms.ToArray()) }";
        if let Ok(output) = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", script])
            .output()
        {
            let b64 = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !b64.is_empty() {
                return Ok(Some(format!("data:image/png;base64,{}", b64)));
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        let script = "try\nset theImage to the clipboard as «class PNGf»\nreturn true\non error\nreturn false\nend try";
        // macOS 可通过 osascript 或 pbpaste 配合转换为 base64
    }

    Ok(None)
}
