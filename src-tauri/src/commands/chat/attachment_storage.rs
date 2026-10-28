use super::ParsedAttachmentResult;
use base64::Engine;
use rusqlite::{params, Connection};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

pub(crate) fn adopt_draft_attachments_at(
    root: &Path,
    directory: &Path,
    mut goal: String,
    mut attachments: Option<Vec<ParsedAttachmentResult>>,
) -> Result<(String, Option<Vec<ParsedAttachmentResult>>), String> {
    for attachment in attachments.iter_mut().flatten() {
        let original = PathBuf::from(&attachment.file_path);
        let Ok(relative) = original.strip_prefix(root) else {
            continue;
        };
        let parts: Vec<_> = relative.components().collect();
        if parts.len() != 3
            || !matches!(parts[0], Component::Normal(name) if name.to_string_lossy().starts_with("draft-"))
            || parts[1] != Component::Normal(std::ffi::OsStr::new("attachments"))
            || !matches!(parts[2], Component::Normal(_))
        {
            continue;
        }
        std::fs::create_dir_all(directory).map_err(|e| format!("保存正式会话附件失败：{e}"))?;
        let name = original.file_name().ok_or("附件文件名缺失")?;
        let target = directory.join(format!(
            "{}-{}",
            uuid::Uuid::new_v4(),
            name.to_string_lossy()
        ));
        let bytes = if let Some(url) = attachment
            .image_data_url
            .as_deref()
            .filter(|url| url.starts_with("data:image/"))
        {
            let (_, payload) = url.split_once(";base64,").ok_or("图片附件编码无效")?;
            base64::engine::general_purpose::STANDARD
                .decode(payload)
                .map_err(|_| "图片附件编码无效")?
        } else {
            let canonical = original
                .canonicalize()
                .map_err(|e| format!("读取草稿附件失败：{e}"))?;
            let source_root = root.canonicalize().map_err(|e| e.to_string())?;
            if !canonical.starts_with(source_root) {
                return Err("草稿附件链接指向受管目录之外，未复制".into());
            }
            std::fs::read(&canonical).map_err(|e| format!("读取草稿附件失败：{e}"))?
        };
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&target)
            .map_err(|e| format!("保存正式会话附件失败：{e}"))?;
        file.write_all(&bytes)
            .map_err(|e| format!("保存正式会话附件失败：{e}"))?;
        let new_path = target.to_string_lossy().into_owned();
        // Change only the generated attachment marker, preserving ordinary user text.
        goal = goal.replace(
            &format!("| 路径: {}]", attachment.file_path),
            &format!("| 路径: {new_path}]"),
        );
        attachment.file_path = new_path;
    }
    Ok((goal, attachments))
}

pub(crate) fn adopt_draft_attachments(
    session_id: &str,
    goal: String,
    attachments: Option<Vec<ParsedAttachmentResult>>,
) -> Result<(String, Option<Vec<ParsedAttachmentResult>>), String> {
    let root = crate::agent::path_guard::get_session_tmp_dir(session_id)
        .parent()
        .ok_or("会话临时目录无效")?
        .to_path_buf();
    // Validate/create the destination only when a draft attachment actually needs adoption.
    if !attachments.as_ref().is_some_and(|items| items.iter().any(|a| {
        Path::new(&a.file_path).strip_prefix(&root).ok().and_then(|p| p.components().next())
            .is_some_and(|p| matches!(p, Component::Normal(name) if name.to_string_lossy().starts_with("draft-")))
    })) { return Ok((goal, attachments)); }
    let directory = crate::agent::path_guard::session_attachment_dir(session_id)?;
    adopt_draft_attachments_at(&root, &directory, goal, attachments)
}

/// Old messages keep their original text. Only recover an exact, session-owned marker;
/// multiple different images with the same label are ambiguous and never guessed.
pub(crate) fn recover_session_image(
    conn: &Connection,
    session_id: &str,
    path: &str,
) -> Result<ParsedAttachmentResult, String> {
    let marker = format!("| 路径: {path}]");
    let mut stmt = conn.prepare("SELECT content FROM chat_messages WHERE session_id=?1 AND role='user' AND instr(content, ?2)>0")
        .map_err(|e| e.to_string())?;
    let messages = stmt
        .query_map(params![session_id, marker], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut labels = std::collections::BTreeSet::new();
    for message in messages {
        let message = message.map_err(|e| e.to_string())?;
        for line in message.lines().filter(|line| line.ends_with(&marker)) {
            if let Some(rest) = line.strip_prefix("📎 [附件: ") {
                let label = rest.split(" (").next().unwrap_or("").trim();
                if !label.is_empty() {
                    labels.insert(label.to_string());
                }
            }
        }
    }
    if labels.len() != 1 {
        return Err("未找到当前会话唯一对应的附件来源".into());
    }
    let name = labels.into_iter().next().unwrap();
    // A later same-name upload cannot stand in for a compacted earlier image.
    let name_prefix = format!("📎 [附件: {name} (");
    let mut same_name = conn.prepare("SELECT content FROM chat_messages WHERE session_id=?1 AND role='user' AND instr(content, ?2)>0")
        .map_err(|e| e.to_string())?;
    let rows = same_name
        .query_map(params![session_id, name_prefix], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    for row in rows {
        let content = row.map_err(|e| e.to_string())?;
        for line in content
            .lines()
            .filter(|line| line.starts_with(&name_prefix))
        {
            if !line.ends_with(&marker) {
                return Err("当前会话有同名但不同来源的附件，未猜测历史图片".into());
            }
        }
    }
    let label = format!("【用户已附加图片: `{name}`】\n");
    let mut snapshots = conn.prepare("SELECT snapshot_json FROM agent_checkpoints WHERE run_id=?1 UNION ALL SELECT snapshot_json FROM agent_checkpoint_history WHERE session_id=?1")
        .map_err(|e| e.to_string())?;
    let rows = snapshots
        .query_map([session_id], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut candidates = BTreeMap::new();
    for row in rows {
        let raw = row.map_err(|e| e.to_string())?;
        let Ok(snapshot) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let Some(history) = snapshot
            .pointer("/data/turns_history")
            .and_then(Value::as_array)
        else {
            continue;
        };
        for message in history {
            if message["role"] != "user" {
                continue;
            }
            let Some(parts) = message["content"].as_array() else {
                continue;
            };
            if !parts.iter().any(|p| {
                p["type"] == "text"
                    && p["text"]
                        .as_str()
                        .is_some_and(|text| text.starts_with(&label))
            }) {
                continue;
            }
            for part in parts {
                let Some(url) = part
                    .pointer("/image_url/url")
                    .and_then(Value::as_str)
                    .filter(|url| url.starts_with("data:image/"))
                else {
                    continue;
                };
                let Some((header, payload)) = url.split_once(";base64,") else {
                    continue;
                };
                let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(payload) else {
                    continue;
                };
                use sha2::{Digest, Sha256};
                candidates.insert(
                    format!("{:x}", Sha256::digest(&bytes)),
                    (
                        url.to_string(),
                        header.trim_start_matches("data:").to_string(),
                        bytes.len(),
                    ),
                );
            }
        }
    }
    if candidates.len() != 1 {
        return Err("附件历史副本缺失或来源有歧义，未按文件名猜测图片".into());
    }
    let (url, mime, size) = candidates.into_values().next().unwrap();
    Ok(ParsedAttachmentResult {
        file_name: name,
        file_path: path.into(),
        size_bytes: size as u64,
        content_type: mime,
        prompt_text: String::new(),
        image_data_url: Some(url),
        raw_preview: None,
    })
}

#[tauri::command]
pub async fn parse_session_attachment(
    state: tauri::State<'_, crate::db::DbState>,
    session_id: String,
    path: String,
) -> Result<ParsedAttachmentResult, String> {
    if Path::new(&path).exists() {
        return super::parse_attachment_file(path).await;
    }
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    recover_session_image(&conn, &session_id, &path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const IMAGE: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Zl1sAAAAASUVORK5CYII=";

    fn database() -> Connection {
        crate::db::init_db(&PathBuf::from(":memory:")).unwrap()
    }
    fn session(conn: &Connection, id: &str) {
        conn.execute("INSERT INTO chat_sessions (id,title,model_id,created_at,updated_at) VALUES (?1,'test','','now','now')", [id]).unwrap();
    }
    fn marker(path: &str) -> String {
        format!("📎 [附件: screenshot.png (1 KB) | 路径: {path}]\n\n正文")
    }
    fn message(conn: &Connection, id: &str, path: &str) {
        conn.execute("INSERT INTO chat_messages (id,session_id,role,content,created_at) VALUES (?1,?1,'user',?2,'now')", params![id, marker(path)]).unwrap();
    }
    fn snapshot(url: &str) -> Value {
        json!({"data":{"turns_history":[{"role":"user","content":[
            {"type":"text","text":"【用户已附加图片: `screenshot.png`】\n原图回看副本（view_image 的 path）：/not-needed-for-recovery.png"},
            {"type":"image_url","image_url":{"url":url}}
        ]}]}})
    }
    fn save_snapshot(conn: &Connection, id: &str, value: Value) {
        conn.execute("INSERT INTO agent_checkpoints (run_id,phase,snapshot_json,created_at,updated_at) VALUES (?1,'done',?2,'now','now')", params![id, value.to_string()]).unwrap();
    }

    #[test]
    fn attachment_adoption_persists_image_and_marker_under_formal_session() {
        let root =
            std::env::temp_dir().join(format!("attachment-adoption-{}", uuid::Uuid::new_v4()));
        let draft = root.join("draft-1/attachments");
        std::fs::create_dir_all(&draft).unwrap();
        let source = draft.join("original-screenshot.png");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(IMAGE.split_once(";base64,").unwrap().1)
            .unwrap();
        std::fs::write(&source, &bytes).unwrap();
        let original_path = source.to_string_lossy().into_owned();
        let attachment = ParsedAttachmentResult {
            file_name: "screenshot.png".into(),
            file_path: original_path.clone(),
            size_bytes: bytes.len() as u64,
            content_type: "image/png".into(),
            image_data_url: Some(IMAGE.into()),
            raw_preview: None,
            prompt_text: String::new(),
        };
        let goal = format!("{}\n原始文字引用 {original_path}", marker(&original_path));
        let destination = root.join("formal/attachments");
        let (goal, adopted) =
            adopt_draft_attachments_at(&root, &destination, goal, Some(vec![attachment])).unwrap();
        let adopted = &adopted.as_ref().unwrap()[0];
        assert!(Path::new(&adopted.file_path).starts_with(&destination));
        assert_eq!(std::fs::read(&adopted.file_path).unwrap(), bytes);
        assert!(goal.contains(&format!("| 路径: {}]", adopted.file_path)));
        assert!(goal.contains(&format!("原始文字引用 {original_path}")));
        assert!(source.exists(), "adoption never deletes the source");
        let conn = database();
        session(&conn, "formal");
        crate::db::cleanup_orphaned_session_data_at(&conn, Some(&root), None).unwrap();
        assert!(
            source.exists(),
            "unregistered draft uploads survive startup cleanup"
        );
        assert!(Path::new(&adopted.file_path).exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn attachment_adoption_preserves_documents_without_inline_data() {
        let root = std::env::temp_dir().join(format!("document-adoption-{}", uuid::Uuid::new_v4()));
        let draft = root.join("draft-2/attachments");
        std::fs::create_dir_all(&draft).unwrap();
        let source = draft.join("report.txt");
        std::fs::write(&source, "original document").unwrap();
        let path = source.to_string_lossy().into_owned();
        let attachment = ParsedAttachmentResult {
            file_name: "report.txt".into(),
            file_path: path.clone(),
            size_bytes: 17,
            content_type: "text".into(),
            image_data_url: None,
            raw_preview: None,
            prompt_text: "summary".into(),
        };
        let (_, adopted) = adopt_draft_attachments_at(
            &root,
            &root.join("formal/attachments"),
            marker(&path),
            Some(vec![attachment]),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&adopted.unwrap()[0].file_path).unwrap(),
            "original document"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn attachment_recovery_requires_exact_marker_and_current_session_source() {
        let conn = database();
        session(&conn, "first");
        session(&conn, "other");
        let path = "/old/tmp/draft-123/attachments/unique-screenshot.png";
        message(&conn, "first", path);
        save_snapshot(&conn, "first", snapshot(IMAGE));
        let recovered = recover_session_image(&conn, "first", path).unwrap();
        assert_eq!(recovered.image_data_url.as_deref(), Some(IMAGE));
        assert_eq!(recovered.file_name, "screenshot.png");
        assert!(recover_session_image(&conn, "other", path).is_err());
        assert!(recover_session_image(&conn, "first", "/another/screenshot.png").is_err());
        assert!(recover_session_image(&conn, "first", "unique-screenshot.png").is_err());
    }

    #[test]
    fn attachment_recovery_reads_history_but_rejects_ambiguous_same_name_images() {
        let conn = database();
        session(&conn, "first");
        let path = "/missing/draft-1/attachments/screenshot.png";
        message(&conn, "first", path);
        conn.execute("INSERT INTO agent_checkpoint_history (session_id,anchor_message_id,phase,snapshot_json,created_at) VALUES ('first','anchor','done',?1,'now')", [snapshot(IMAGE).to_string()]).unwrap();
        assert!(recover_session_image(&conn, "first", path).is_ok());
        save_snapshot(&conn, "first", snapshot(IMAGE));
        assert!(
            recover_session_image(&conn, "first", path).is_ok(),
            "identical checkpoint replays are not ambiguous"
        );
        conn.execute(
            "UPDATE agent_checkpoints SET snapshot_json=?1 WHERE run_id='first'",
            [snapshot("data:image/png;base64,YW5vdGhlcg==").to_string()],
        )
        .unwrap();
        assert!(recover_session_image(&conn, "first", path)
            .unwrap_err()
            .contains("歧义"));
    }

    #[test]
    fn attachment_recovery_does_not_substitute_a_later_same_name_image() {
        let conn = database();
        session(&conn, "first");
        let path = "/missing/draft-1/attachments/first.png";
        message(&conn, "first", path);
        conn.execute("INSERT INTO chat_messages (id,session_id,role,content,created_at) VALUES ('later','first','user',?1,'later')", [marker("/missing/draft-2/attachments/second.png")]).unwrap();
        save_snapshot(&conn, "first", snapshot(IMAGE));
        assert!(recover_session_image(&conn, "first", path)
            .unwrap_err()
            .contains("不同来源"));
    }

    #[test]
    fn attachment_cleanup_keeps_referenced_upload_directory_and_removes_only_unowned_directory() {
        let conn = database();
        session(&conn, "formal");
        let root =
            std::env::temp_dir().join(format!("attachment-cleanup-{}", uuid::Uuid::new_v4()));
        for name in ["draft-unsent", "legacy-owner", "orphan-owner"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        let path = root.join("legacy-owner/attachments/photo.png");
        message(&conn, "formal", &path.to_string_lossy());
        crate::db::cleanup_orphaned_session_data_at(&conn, Some(&root), None).unwrap();
        assert!(root.join("draft-unsent").exists());
        assert!(root.join("legacy-owner").exists());
        assert!(!root.join("orphan-owner").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
