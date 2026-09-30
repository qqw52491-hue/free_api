//! view_image 工具臂：模型主动把图片文件拉进自己的下一轮调用。
//!
//! 信道为一次性注入：图片编码后进 context.pending_view_images 暂存队列，
//! 只随下一次消息组装出现一轮、不落 turns_history（否则其后每次调用都
//! 占视觉档位并重复烧图 token）；下一轮工具派发开始即核销清空。
//! router 的 has_pending_vision 认此队列——当轮硬规则切视觉模型档。
//! 对模型而言这是纯能力开放：用不用、何时用由它自己判断，不挂任何门禁。
//! 典型场景：渲染/截图/图表类产物生成后自检（contact sheet、原型截图）。
//! 视频不可直接注入——模型自行 ffmpeg 抽帧后看帧（与 skill 手册约定一致）。

use super::*;

/// 单张图片文件大小上限（base64 后约 ×1.34，留足提供商侧余量）。
const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

/// 视觉 API 只认位图；svg/ico/其他扩展一律拒收。
fn image_mime_for(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

/// 纯逻辑核：路径白名单解析 → 位图格式/大小校验 → base64 编码。
/// 成功返回 (display_path, data_url, size_bytes)；失败返回 (stderr, route)。
/// 与 local_file 同一白名单：workspace ∪ session tmp，越界即拒。
pub(crate) fn resolve_and_encode_image(
    raw_path: &str,
    workspace_dir: &Path,
    session_id: &str,
) -> Result<(String, String, u64), (String, &'static str)> {
    let resolved = crate::agent::path_guard::ensure_allowed_path(
        raw_path,
        workspace_dir,
        Some(session_id),
        false,
    )
    .map_err(|e| (e, "view_image:path_denied"))?;

    if !resolved.canonical_path.is_file() {
        return Err((
            format!("❌ 文件不存在: {}", resolved.display_path),
            "view_image:not_found",
        ));
    }

    let mime = image_mime_for(&resolved.canonical_path).ok_or_else(|| {
        (
            format!(
                "❌ 不是可注入的位图格式: {}（支持 png/jpg/jpeg/webp/gif/bmp；视频请 ffmpeg 抽帧后再看）",
                resolved.display_path
            ),
            "view_image:not_image",
        )
    })?;

    let size = std::fs::metadata(&resolved.canonical_path)
        .map_err(|e| (format!("❌ 无法读取文件元数据: {}", e), "view_image:io"))?
        .len();
    if size > MAX_IMAGE_BYTES {
        return Err((
            format!(
                "❌ 图片过大（{} 字节 > 8MB 上限）。请压缩/缩放后再看（如 ffmpeg -i in.png -vf scale=960:-1 out.jpg）。",
                size
            ),
            "view_image:too_large",
        ));
    }

    let bytes = std::fs::read(&resolved.canonical_path)
        .map_err(|e| (format!("❌ 读取文件失败: {}", e), "view_image:io"))?;

    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let data_url = format!("data:{};base64,{}", mime, b64);
    Ok((resolved.display_path, data_url, size))
}

pub(super) async fn dispatch_view_image(
    ctx: &mut DispatchCtx<'_>,
    inst: &AgentInstruction,
) -> Result<ArmOutcome, String> {
    Ok(ArmOutcome::Done({
        let params = inst.get_params();
        let raw_path = params
            .get("path")
            .or_else(|| params.get("file"))
            .or_else(|| params.get("file_path"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if raw_path.is_empty() {
            return Ok(ArmOutcome::Done(DispatchResult { view: None,
                stdout: String::new(),
                stderr: "❌ 缺少必填参数 path（图片文件路径）".to_string(),
                success: false,
                route: "view_image:missing".to_string(),
            }));
        }

        let (display_path, data_url, size) = match resolve_and_encode_image(
            raw_path,
            ctx.workspace_dir,
            ctx.final_session_id,
        ) {
            Ok(ok) => ok,
            Err((stderr, route)) => {
                return Ok(ArmOutcome::Done(DispatchResult { view: None,
                    stdout: String::new(),
                    stderr,
                    success: false,
                    route: route.to_string(),
                }));
            }
        };

        // 一次性注入：不落 turns_history，只在下一次消息组装里出现一轮；
        // 看到的关键信息须写进步骤文本沉淀，需要再看就重新调用。
        ctx.context.pending_view_images.push((
            format!(
                "【view_image】你请求查看的图片: {}（{} 字节）",
                display_path, size
            ),
            data_url,
        ));

        DispatchResult { view: None,
            stdout: format!(
                "✅ 图片已注入下一次调用：{}（{} 字节，仅当轮可见不落历史；关键信息请写进步骤文本，需再看请重新调用）。",
                display_path, size
            ),
            stderr: String::new(),
            success: true,
            route: "view_image".to_string(),
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::resolve_and_encode_image;

    /// 真实图片文件全链路：工作区内 png → 解析 → data_url 编码正确。
    #[test]
    fn resolve_and_encode_real_png() {
        let dir = std::env::temp_dir().join(format!("fac-vi-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 1x1 红色 PNG（真实编码字节，非伪文件）
        let png: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92,
            0xEF, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        std::fs::write(dir.join("pixel.png"), png).unwrap();

        let (display, data_url, size) =
            resolve_and_encode_image("pixel.png", &dir, "no-such-session").unwrap();
        assert_eq!(display, "pixel.png");
        assert_eq!(size, png.len() as u64);
        assert!(data_url.starts_with("data:image/png;base64,"));

        // 非位图扩展拒收
        std::fs::write(dir.join("vector.svg"), b"<svg/>").unwrap();
        let err = resolve_and_encode_image("vector.svg", &dir, "no-such-session").unwrap_err();
        assert_eq!(err.1, "view_image:not_image");

        // 越界路径拒收（白名单外）
        let err = resolve_and_encode_image("/etc/hosts", &dir, "no-such-session").unwrap_err();
        assert_eq!(err.1, "view_image:path_denied");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
