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

/// 判定给定路径是否物理位于当前会话临时目录或全局会话临时根目录下，或是 tmp/ 相对快捷路径
pub fn is_path_in_session_tmp(raw_path: &str, session_id: Option<&str>) -> bool {
    let trimmed = raw_path.trim();
    if trimmed.starts_with("tmp/") || trimmed.starts_with("./tmp/") {
        return true;
    }
    let p = Path::new(trimmed);
    if let Some(sid) = session_id {
        let specific_tmp = get_session_tmp_dir(sid);
        if p.starts_with(&specific_tmp) {
            return true;
        }
    }
    let global_tmp = dirs_next::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("free-api-chat")
        .join("tmp");
    if p.starts_with(&global_tmp) {
        return true;
    }
    if let Ok(canon) = p.canonicalize() {
        if let Ok(canon_global) = global_tmp.canonicalize() {
            if canon.starts_with(&canon_global) {
                return true;
            }
        }
    }
    false
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

    // 会话 tmp 身份优先于工作区归属：自由会话里 workspace_root 即 session_tmp
    // （见 main_loop::resolve_workspace_dir），若先判工作区会把 tmp 产物全部误标
    // is_session_tmp=false，破坏草稿免审与盲写基线豁免。项目会话两区天然不相交
    // （tmp 在应用数据目录下），判定顺序对其无影响。
    if in_tmp {
        let display = canonical.to_string_lossy().to_string();
        Ok(AllowedPathResolution {
            canonical_path: canonical,
            display_path: display,
            is_session_tmp: true,
            newly_created_dir: newly_created,
        })
    } else if in_workspace {
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

/// 机械兜底豁免判定：仅当任务入口分流未产生判决（无 TriageVerdict 的旧 checkpoint
/// 恢复）时，由写入门禁调用本函数判定本次写入是否属于"零爆炸半径"的小活。
/// 有判决时一律按 TriageVerdict.route 分流，本函数不当主判官。
///
/// 方案门护的是"已有资产不被未对齐改动"——下列写入此前提不成立，豁免：
///   - tmp/ 会话产物（草稿或交付物，均不动已有资产）；
///   - 磁盘上不存在的新文件（从零创建，无"已读对象"可引）；
///   - 单文件局部小编辑（target/replacement 均 ≤12 非空行：改颜色/加日志级）。
/// 例外：本会话已触及 ≥2 个互异工作区文件且本次写第 3 个新目标 → 不豁免
///   （多文件扩散 = 复杂度信号，升级回慢路径走方案门）。
/// 非 local_file 的写入（child_writer/shell/MCP）目标不可静态界定，一律不豁免。
pub fn is_plan_gate_exempt_write(
    inst: &crate::agent::types::AgentInstruction,
    session_id: Option<&str>,
    workspace_dir: &Path,
    workspace_touched: &[String],
) -> bool {
    if !inst.get_tool().eq_ignore_ascii_case("local_file") {
        return false;
    }
    let action = inst.get_action().to_ascii_lowercase();
    let action = if action.is_empty() || action == "local_file" {
        inst.get_params()
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase()
    } else {
        action
    };
    let is_write = action.starts_with("write");
    let is_edit = action.starts_with("edit");
    if !is_write && !is_edit {
        return false;
    }
    let params = inst.get_params();
    let Some(path_str) = params
        .get("path")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    else {
        return false;
    };
    if is_path_in_session_tmp(path_str, session_id) {
        return true;
    }
    let p = Path::new(path_str);
    let resolved = if p.is_absolute() {
        p.to_path_buf()
    } else {
        workspace_dir.join(p)
    };
    // 工作区外路径：path_guard 自有拦截，不属于本豁免职责
    if !resolved.starts_with(workspace_dir) {
        return false;
    }
    // 多文件扩散升级：已触及 ≥2 个互异工作区文件，本次写第 3 个新目标 → 回慢路径
    let touched_set: std::collections::HashSet<String> = workspace_touched
        .iter()
        .map(|t| t.trim().trim_start_matches("./").to_string())
        .collect();
    let this_norm = path_str.trim_start_matches("./").to_string();
    if touched_set.len() >= 2 && !touched_set.contains(&this_norm) {
        return false;
    }
    // 从零新建：无已存在资产受损、无"已读对象"可引 → 快路径
    if !resolved.exists() {
        return true;
    }
    // 已存在文件：仅小编辑豁免（全量覆写/大改仍走方案门）
    if is_edit {
        let count_nonempty = |key: &str| -> usize {
            inst.get_params()
                .get(key)
                .and_then(|v| v.as_str())
                .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
                .unwrap_or(usize::MAX)
        };
        const TRIVIAL_EDIT_MAX_LINES: usize = 12;
        return count_nonempty("target_content") <= TRIVIAL_EDIT_MAX_LINES
            && count_nonempty("replacement_content") <= TRIVIAL_EDIT_MAX_LINES;
    }
    false
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

    /// 自由会话场景：workspace_root 即 session_tmp 时，tmp 路径仍须判定为
    /// is_session_tmp=true（tmp 身份优先于工作区归属），否则草稿免审与
    /// 盲写基线豁免失效，tmp 覆写会误撞"未读基线"拦截。
    #[test]
    fn test_session_tmp_identity_wins_when_workspace_is_tmp() {
        let session_id = format!("test_sess_{}", uuid::Uuid::new_v4());
        let tmp_root = get_session_tmp_dir(&session_id);
        std::fs::create_dir_all(&tmp_root).unwrap();
        std::fs::write(tmp_root.join("draft.md"), "v1").unwrap();

        // workspace_root == session_tmp：tmp 内路径须归 tmp 而非工作区
        let res = ensure_allowed_path("tmp/draft.md", &tmp_root, Some(&session_id), false)
            .unwrap();
        assert!(res.is_session_tmp, "tmp 路径在 workspace==tmp 下仍须保持 tmp 身份");

        // 工作区外仍拒绝
        let outside = temp_dir();
        assert!(ensure_allowed_path(
            outside.join("x.txt").to_str().unwrap(),
            &tmp_root,
            Some(&session_id),
            false
        )
        .is_err());

        let _ = std::fs::remove_dir_all(&tmp_root);
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
