//! 一次性图片注入的共享内核：单步与复合分支共用路径、格式和大小检查。
//! 图片只进入本次派发的内存队列，不进入工具输出、持久化历史或结构化视图。
use crate::agent::types::DispatchResult;
use serde_json::Value;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub(crate) type PendingImageQueue = Arc<Mutex<Vec<(String, String)>>>;

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
/// 与 local_file 读取同一权限：workspace ∪ session tmp ∪ 显式附加目录 ∪ skills。
pub(crate) fn resolve_and_encode_image(
    raw_path: &str,
    workspace_dir: &Path,
    session_id: &str,
) -> Result<(String, String, u64), (String, &'static str)> {
    let resolved = crate::agent::path_guard::ensure_readable_path(
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

pub(crate) fn dispatch(
    params: &Value,
    workspace: Option<&Path>,
    session_id: &str,
    pending: &PendingImageQueue,
) -> DispatchResult {
    let fail = |stderr: String, route: &str| DispatchResult {
        view: None,
        stdout: String::new(),
        stderr,
        success: false,
        route: route.to_string(),
    };
    let raw_path = params
        .get("path")
        .or_else(|| params.get("file"))
        .or_else(|| params.get("file_path"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if raw_path.is_empty() {
        return fail(
            "❌ 缺少必填参数 path（图片文件路径）".to_string(),
            "view_image:missing",
        );
    }
    let Some(workspace) = workspace else {
        return fail(
            "❌ view_image 未提供工作区，尚未读取图片。".to_string(),
            "view_image:no_workspace",
        );
    };
    let (display_path, data_url, size) =
        match resolve_and_encode_image(raw_path, workspace, session_id) {
            Ok(image) => image,
            Err((stderr, route)) => return fail(stderr, route),
        };
    let mut images = match pending.lock() {
        Ok(images) => images,
        Err(_) => {
            return fail(
                "❌ 图片暂存队列不可用，尚未注入图片。".to_string(),
                "view_image:queue_error",
            )
        }
    };
    images.push((
        format!(
            "【view_image】你请求查看的图片: {}（{} 字节）",
            display_path, size
        ),
        data_url,
    ));
    DispatchResult {
        view: None,
        stdout: format!("✅ 图片已注入下一次调用：{}（{} 字节，仅当轮可见不落历史；关键信息请写进步骤文本，需再看请重新调用）。", display_path, size),
        stderr: String::new(), success: true, route: "view_image".to_string(),
    }
}

/// 派发结束后交给当前 agent；即使其它分支失败，成功读取的图片仍可用于下一轮。
pub(crate) fn transfer_pending_images(
    pending: &PendingImageQueue,
    target: &mut Vec<(String, String)>,
) -> Result<(), String> {
    let mut images = pending
        .lock()
        .map_err(|_| "图片暂存队列不可用".to_string())?;
    target.extend(images.drain(..));
    Ok(())
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

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::agent::context::SandwichContext;
    use crate::agent::dispatcher::run_agent_step_with_images;
    use crate::agent::mcp::PluginRegistry;
    use crate::agent::types::AgentInstruction;
    use crate::db::{init_db, DbState};
    use base64::Engine;
    use serde_json::json;
    use std::path::PathBuf;

    fn fixture() -> (PathBuf, DbState, String) {
        let root =
            std::env::temp_dir().join(format!("fac-image-dispatch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aX1sAAAAASUVORK5CYII=";
        let png = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap();
        std::fs::write(root.join("a.png"), &png).unwrap();
        std::fs::write(root.join("b.png"), &png).unwrap();
        std::fs::write(root.join("probe.txt"), "image-adjacent-read-ok").unwrap();
        let db = DbState(Arc::new(Mutex::new(
            init_db(&PathBuf::from(":memory:")).unwrap(),
        )));
        (root, db, format!("data:image/png;base64,{b64}"))
    }

    fn run(root: &Path, db: &DbState, value: Value, queue: &PendingImageQueue) -> DispatchResult {
        let instruction: AgentInstruction = serde_json::from_value(value).unwrap();
        run_agent_step_with_images(
            db,
            "image-dispatch",
            &instruction,
            &PluginRegistry::new(),
            Some(root),
            queue,
            None,
        )
    }

    fn assert_handoff(queue: &PendingImageQueue, expected: usize, data_url: &str) {
        let mut ctx = SandwichContext::new("SYS".into(), "Goal".into());
        transfer_pending_images(queue, &mut ctx.pending_view_images).unwrap();
        assert!(queue.lock().unwrap().is_empty());
        assert_eq!(ctx.pending_view_images.len(), expected);
        let messages = ctx.assemble_messages();
        let image_count = messages
            .iter()
            .filter_map(|m| m.content.as_array())
            .flat_map(|a| a.iter())
            .filter(|item| item["type"] == "image_url" && item["image_url"]["url"] == data_url)
            .count();
        assert_eq!(image_count, expected);
        assert!(ctx.turns_history.is_empty());
        assert!(!serde_json::to_string(&ctx).unwrap().contains(data_url));
        // call_llm_phase performs this clear once the request returns.
        ctx.pending_view_images.clear();
        assert!(!serde_json::to_string(&ctx.assemble_messages())
            .unwrap()
            .contains(data_url));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn parallel_images_reach_next_request_without_binary_in_tool_output() {
        let (root, db, url) = fixture();
        let queue = PendingImageQueue::default();
        let result = run(
            &root,
            &db,
            json!({"parallel":[
                {"tool":"view_image","command":{"path":"a.png"}},
                {"tool":"view_image","args":{"file":"b.png"}},
                {"tool":"local_file","command":{"path":"probe.txt"}}
            ]}),
            &queue,
        );
        assert!(result.success, "{}", result.stderr);
        assert!(result.stdout.contains("image-adjacent-read-ok"));
        assert!(!result.stdout.contains("data:image") && !result.stderr.contains("data:image"));
        assert!(!serde_json::to_string(&result.view)
            .unwrap()
            .contains("data:image"));
        assert_handoff(&queue, 2, &url);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn parallel_failure_keeps_successful_image_for_next_request() {
        let (root, db, url) = fixture();
        let queue = PendingImageQueue::default();
        let result = run(
            &root,
            &db,
            json!({"parallel":[
                {"tool":"view_image","command":{"path":"a.png"}},
                {"tool":"view_image","command":{"path":"missing.png"}}
            ]}),
            &queue,
        );
        assert!(!result.success);
        assert!(result.stderr.contains("文件不存在"));
        assert_handoff(&queue, 1, &url);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn single_pipeline_and_nested_plan_tree_share_image_channel() {
        let (root, db, url) = fixture();
        for value in [
            json!({"tool":"view_image","command":{"file_path":"a.png"}}),
            json!({"commands":[{"tool":"view_image","command":{"path":"a.png"}}, {"tool":"local_file","command":{"path":"probe.txt"}}]}),
            json!({"plan_tree":[{"parallel":[
                {"tool":"view_image","params":{"path":"a.png"}},
                {"tool":"local_file","params":{"path":"probe.txt"}}
            ]}]}),
        ] {
            let queue = PendingImageQueue::default();
            let result = run(&root, &db, value, &queue);
            assert!(result.success, "{}", result.stderr);
            assert_handoff(&queue, 1, &url);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejected_parallel_fanout_does_not_enqueue_images() {
        let (root, db, _) = fixture();
        let queue = PendingImageQueue::default();
        let result = run(
            &root,
            &db,
            json!({"parallel":[
                {"tool":"view_image","command":{"path":"a.png"}},
                {"tool":"view_image","command":{"path":"a.png"}},
                {"tool":"view_image","command":{"path":"a.png"}},
                {"tool":"view_image","command":{"path":"a.png"}},
                {"tool":"view_image","command":{"path":"a.png"}}
            ]}),
            &queue,
        );
        assert!(!result.success);
        assert_eq!(result.route, "parallel_contract_rejected");
        assert!(queue.lock().unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn image_path_format_size_failures_and_queues_stay_isolated() {
        let (root, _, url) = fixture();
        let queue = PendingImageQueue::default();
        let other_queue = PendingImageQueue::default();
        std::fs::write(root.join("vector.svg"), "<svg/>").unwrap();
        std::fs::File::create(root.join("big.png"))
            .unwrap()
            .set_len(MAX_IMAGE_BYTES + 1)
            .unwrap();
        for (params, route) in [
            (json!({}), "view_image:missing"),
            (json!({"path":"missing.png"}), "view_image:not_found"),
            (json!({"path":"vector.svg"}), "view_image:not_image"),
            (json!({"path":"big.png"}), "view_image:too_large"),
            (json!({"path":"/etc/hosts"}), "view_image:path_denied"),
        ] {
            let result = dispatch(&params, Some(&root), "image-dispatch", &queue);
            assert_eq!(result.route, route);
            assert!(!result.success);
            assert!(queue.lock().unwrap().is_empty());
        }
        assert!(
            dispatch(
                &json!({"path":"a.png"}),
                Some(&root),
                "image-dispatch",
                &queue
            )
            .success
        );
        assert!(other_queue.lock().unwrap().is_empty());
        assert_handoff(&queue, 1, &url);
        std::fs::remove_dir_all(root).unwrap();
    }
}
