//! 附件上传与工件原始内容/预览 handler。

use super::*;

/// 网页端附件上限（与微信桥回推上限一致）
pub(crate) const MAX_UPLOAD_BYTES: usize = 25 * 1024 * 1024;

/// 极简 percent-decode（文件名经 X-File-Name 头传输，JS 侧 encodeURIComponent）
pub(crate) fn percent_decode(input: &str) -> String {
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
        if let Some(byte) = hex {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// POST /channel/session/{session_id}/attachment-upload
/// 浏览器给不了本地路径，网页端直接收字节流（raw body + X-File-Name），
/// 落临时文件后走既有登记逻辑（复制进项目 .attachments/ 隔离目录）。
/// 附件仅支持项目会话——临时会话没有 .attachments 落点（与微信桥约束一致）。
pub(crate) async fn handler_session_attachment_upload(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    if !authorized(&headers, &config) {
        return Err(unauthorized());
    }
    if body.is_empty() {
        return Err(bad_request("附件内容为空"));
    }
    if body.len() > MAX_UPLOAD_BYTES {
        return Err(bad_request("附件超过 25MB 上限"));
    }
    let original_name = headers
        .get("x-file-name")
        .and_then(|v| v.to_str().ok())
        .map(percent_decode)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| bad_request("缺少 X-File-Name 文件名头"))?;
    let session_id = session_id.trim().to_string();
    let bound: bool = {
        let conn = db_conn(&state.app)?;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_project_sessions WHERE session_id=?1)",
            rusqlite::params![session_id],
            |row| row.get(0),
        )
        .map_err(internal_error)?
    };
    if !bound {
        return Err(bad_request("附件仅支持项目对话：请选择项目后再上传"));
    }
    let tmp = std::env::temp_dir().join(format!("free-api-upload-{}", uuid::Uuid::new_v4()));
    tokio::fs::write(&tmp, &body).await.map_err(internal_error)?;
    let registered = {
        let conn = db_conn(&state.app)?;
        crate::commands::projects::register_attachment_for_session(
            &conn,
            &session_id,
            &tmp,
            &original_name,
        )
    };
    let _ = std::fs::remove_file(&tmp);
    let resource_id = registered
        .map_err(|e| bad_request(&e))?
        .ok_or_else(|| bad_request("该会话未绑定项目，无法登记附件"))?;
    Ok(Json(json!({
        "resource_id": resource_id,
        "name": original_name,
        "size_bytes": body.len(),
    })))
}

/// 极简 percent-encode（Content-Disposition 的 UTF-8 文件名用；保留 RFC 3986 非保留字符）
pub(crate) fn percent_encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// GET /channel/artifact/{id}/raw?token=&dl=1 ：产物原始字节（inline 预览 / attachment 下载）。
/// <a> 链接带不了 header，token 允许走 query（与 SSE 一致）。
pub(crate) async fn handler_artifact_raw(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(artifact_id): AxumPath<String>,
    AxumQuery(query): AxumQuery<ArtifactRawQuery>,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    let token_ok = authorized(&headers, &config)
        || query
            .token
            .as_deref()
            .map(|v| v == config.token)
            .unwrap_or(false);
    if !token_ok {
        return Err(unauthorized());
    }
    let db = state.app.state::<DbState>();
    let record = crate::agent::artifact_manager::get_artifact_record(db.inner(), artifact_id.trim())
        .map_err(|e| not_found(&e))?;
    if record.status == "deleted" {
        return Err(not_found("该产物已被删除"));
    }
    let path = std::path::PathBuf::from(&record.canonical_path);
    if !path.exists() {
        return Err(not_found("产物文件在磁盘上丢失"));
    }
    let bytes = tokio::fs::read(&path).await.map_err(internal_error)?;
    if bytes.len() > 50 * 1024 * 1024 {
        return Err(bad_request("文件超过 50MB，请从桌面端工作区直接获取"));
    }
    let mime = record.mime_type.clone().unwrap_or_else(|| "application/octet-stream".into());
    let encoded = percent_encode_component(&record.display_name);
    let disposition = if query.dl.as_deref() == Some("1") {
        format!("attachment; filename*=UTF-8''{encoded}")
    } else {
        format!("inline; filename*=UTF-8''{encoded}")
    };
    let mut response = (
        StatusCode::OK,
        bytes,
    )
        .into_response();
    if let Ok(v) = axum::http::HeaderValue::from_str(&mime) {
        response.headers_mut().insert(axum::http::header::CONTENT_TYPE, v);
    }
    if let Ok(v) = axum::http::HeaderValue::from_str(&disposition) {
        response.headers_mut().insert(axum::http::header::CONTENT_DISPOSITION, v);
    }
    Ok(response)
}

/// GET /channel/artifact/{id}/preview?token= ：解析引擎产出的预览文本
/// （xlsx → 表结构+数据预览，pdf/docx → 全文截断，文本 → 原文；图片由 raw 端点直出）
pub(crate) async fn handler_artifact_preview(
    AxumState(state): AxumState<Arc<ChannelState>>,
    headers: HeaderMap,
    AxumPath(artifact_id): AxumPath<String>,
    AxumQuery(query): AxumQuery<PreviewQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config.read().expect("config poisoned").clone();
    let token_ok = authorized(&headers, &config)
        || query
            .token
            .as_deref()
            .map(|v| v == config.token)
            .unwrap_or(false);
    if !token_ok {
        return Err(unauthorized());
    }
    let db = state.app.state::<DbState>();
    let record = crate::agent::artifact_manager::get_artifact_record(db.inner(), artifact_id.trim())
        .map_err(|e| not_found(&e))?;
    let is_image = record
        .mime_type
        .as_deref()
        .map(|m| m.starts_with("image/"))
        .unwrap_or(false);
    if is_image {
        return Ok(Json(json!({ "kind": "image", "name": record.display_name })));
    }
    let path = std::path::PathBuf::from(&record.canonical_path);
    if !path.exists() {
        return Err(not_found("产物文件在磁盘上丢失"));
    }
    match crate::agent::file_parser::parse_file(&path, Some(12_000)) {
        Ok(content) => {
            let text = crate::agent::file_parser::format_for_prompt(&content, &record.display_name);
            Ok(Json(json!({ "kind": "text", "name": record.display_name, "text": text })))
        }
        Err(e) => Ok(Json(json!({
            "kind": "unsupported",
            "name": record.display_name,
            "text": format!("该格式（{}）暂不支持网页内预览，请点击下载后本地查看：{}", record.display_name, e),
        }))),
    }
}

#[derive(Deserialize)]
pub(crate) struct ArtifactRawQuery {
    pub(crate) token: Option<String>,
    /// dl=1 时以附件形式下发（浏览器触发下载），否则 inline 预览
    pub(crate) dl: Option<String>,
}
