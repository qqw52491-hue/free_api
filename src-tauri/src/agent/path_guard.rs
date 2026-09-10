//! 共享路径守卫与多工作区/会话临时目录双白名单校验。
//!
//! 防逃逸原则：
//! 1. 强制 canonicalize 物理路径，消除 `..` 穿透与软链接越界；
//! 2. 双白名单机制：目标必须位于当前项目工作区，或当前会话专属临时目录内；
//! 3. 懒加载（Lazy-create）：会话临时目录仅在首次触碰写入时按需创建，不污染磁盘；
//! 4. 越界时给出结构化、带候选白名单的清晰错误提示。

use std::path::{Path, PathBuf};

/// 获取会话专属临时目录根路径
pub fn get_session_tmp_dir(session_id: &str) -> PathBuf {
    let base = dirs_next::data_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("free-api-chat").join("tmp").join(session_id)
}

#[derive(Debug, Clone)]
pub struct AllowedPathResolution {
    /// 物理绝对路径（已 canonicalize 或基于合法 parent 构建）
    pub canonical_path: PathBuf,
    /// 相对展示路径（相对于对应 root）
    pub display_path: String,
    /// 是否属于临时会话目录
    pub is_session_tmp: bool,
    /// 本次是否发生了新目录的懒加载创建
    pub newly_created_dir: Option<PathBuf>,
}

/// 核心工作区与会话临时目录双白名单校验
///
/// 1. 允许目标处于 workspace_root 范围内；
/// 2. 允许目标处于 session_tmp_dir 范围内；
/// 3. 若处于 session_tmp_dir 且磁盘目录尚未存在，当 auto_create 为 true 时自动执行懒加载创建；
/// 4. 严防 `..` 穿越与符号链接逃逸。
pub fn ensure_allowed_path(
    raw: &str,
    workspace_root: &Path,
    session_id: Option<&str>,
    auto_create_if_tmp: bool,
) -> Result<AllowedPathResolution, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("路径不能为空".to_string());
    }

    let ws_canonical = workspace_root
        .canonicalize()
        .map_err(|e| format!("工作区根目录不可访问 ({}): {}", workspace_root.display(), e))?;

    let session_tmp_buf = session_id.map(get_session_tmp_dir);
    let mut newly_created = None;

    let target = if let Some(ref tmp_root) = session_tmp_buf {
        let p = Path::new(trimmed);
        if p.starts_with(tmp_root) {
            p.to_path_buf()
        } else if trimmed.starts_with("tmp/") || trimmed.starts_with("./tmp/") {
            let stripped = trimmed.trim_start_matches('.').trim_start_matches('/');
            let sub = stripped.strip_prefix("tmp/").unwrap_or(stripped);
            tmp_root.join(sub)
        } else if p.is_relative() {
            ws_canonical.join(p)
        } else {
            p.to_path_buf()
        }
    } else {
        let p = Path::new(trimmed);
        if p.is_relative() {
            ws_canonical.join(p)
        } else {
            p.to_path_buf()
        }
    };

    // 针对 session_tmp 的懒创建：如果目标在 session_tmp 内且目录不存在，按需自动创建
    if let Some(ref tmp_root) = session_tmp_buf {
        if target.starts_with(tmp_root) && auto_create_if_tmp {
            if !tmp_root.exists() {
                std::fs::create_dir_all(tmp_root).map_err(|e| {
                    format!("无法懒加载创建会话临时目录 ({}): {}", tmp_root.display(), e)
                })?;
                newly_created = Some(tmp_root.clone());
            }
            if let Some(parent) = target.parent() {
                if !parent.exists() {
                    let _ = std::fs::create_dir_all(parent);
                }
            }
        }
    }

    // 目标路径真实性/规范化检查（支持新建文件：若文件尚不存在，canonicalize 其已存在的父目录）
    let canonical = match target.canonicalize() {
        Ok(path) => path,
        Err(_) => {
            let parent = target
                .parent()
                .ok_or_else(|| format!("非法路径: {}", trimmed))?;
            let parent_canonical = parent.canonicalize().map_err(|e| {
                format!(
                    "路径所在父目录不可访问或不存在 ({}): {}",
                    parent.display(),
                    e
                )
            })?;
            let file_name = target
                .file_name()
                .ok_or_else(|| format!("非法文件名: {}", trimmed))?;
            parent_canonical.join(file_name)
        }
    };

    // 白名单命中判定
    let in_workspace = canonical.starts_with(&ws_canonical);
    let in_tmp = if let Some(ref tmp_root) = session_tmp_buf {
        if let Ok(canonical_tmp) = tmp_root.canonicalize() {
            canonical.starts_with(&canonical_tmp)
        } else {
            canonical.starts_with(tmp_root)
        }
    } else {
        false
    };

    if in_workspace {
        let display = canonical
            .strip_prefix(&ws_canonical)
            .unwrap_or(&canonical)
            .to_string_lossy()
            .to_string();
        Ok(AllowedPathResolution {
            canonical_path: canonical,
            display_path: display,
            is_session_tmp: false,
            newly_created_dir: newly_created,
        })
    } else if in_tmp {
        let display = canonical.to_string_lossy().to_string();
        Ok(AllowedPathResolution {
            canonical_path: canonical,
            display_path: display,
            is_session_tmp: true,
            newly_created_dir: newly_created,
        })
    } else {
        let allowed_ws_display = ws_canonical.display().to_string();
        let allowed_tmp_display = session_tmp_buf
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "（未分配）".to_string());
        Err(format!(
            "❌ [工作区越界拦截] 目标路径 \"{}\" 超出允许的操作范围。\n当前会话仅允许访问以下物理目录白名单：\n1. 项目工作区: {}\n2. 会话临时目录: {}",
            trimmed, allowed_ws_display, allowed_tmp_display
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("test-path-guard-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn test_workspace_containment() {
        let ws = temp_dir();
        let file = ws.join("a.txt");
        std::fs::write(&file, "hello").unwrap();

        let res = ensure_allowed_path("a.txt", &ws, None, false).unwrap();
        assert_eq!(res.is_session_tmp, false);
        assert_eq!(res.canonical_path, file.canonicalize().unwrap());
        let _ = std::fs::remove_dir_all(ws);
    }

    #[test]
    fn test_escape_is_blocked() {
        let ws = temp_dir();
        let outside = temp_dir();
        let secret = outside.join("secret.txt");
        std::fs::write(&secret, "pw").unwrap();

        let res = ensure_allowed_path(
            secret.to_str().unwrap(),
            &ws,
            None,
            false,
        );
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("工作区越界拦截"));
        let _ = std::fs::remove_dir_all(ws);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn test_session_tmp_lazy_create() {
        let ws = temp_dir();
        let session_id = format!("test_sess_{}", uuid::Uuid::new_v4());
        let res = ensure_allowed_path(
            "tmp/draft.md",
            &ws,
            Some(&session_id),
            true,
        )
        .unwrap();

        assert_eq!(res.is_session_tmp, true);
        assert!(res.canonical_path.to_string_lossy().contains(&session_id));
        // 验证目录已被懒创建
        let tmp_root = get_session_tmp_dir(&session_id);
        assert!(tmp_root.exists());

        // 清理
        let _ = std::fs::remove_dir_all(tmp_root);
        let _ = std::fs::remove_dir_all(ws);
    }
}
