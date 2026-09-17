//! 机械合入：副本字节搬运优先、权威 patch 兜底、冲突整体拒绝。

use super::*;
use crate::db::DbState;
use crate::llm::{self, ModelConfig};
use uuid::Uuid;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;


/// 机械合入：主 Agent 审阅 writer 回执后，由宿主把候选修改原样搬回真实工作区。
///
/// 安全协议（宁缺毋滥）：
/// 1. 逐文件以 apply 瞬间的主工作区实况重算冲突指纹（闭合 TOCTOU 窗口）；
/// 2. 任一文件 Conflict → 整体拒绝，绝不盲目覆盖；
/// 3. 搬运优先走仍存活的隔离副本（字节级精确，兼容非 git/二进制/新建文件），
///    且副本内源文件指纹必须等于回执 result_hash（副本被污染则拒绝）；
/// 4. 副本已被回收时退化为 patch 工件 + `git apply --check` 干跑校验。
pub async fn apply_writer_result(
    db: &DbState,
    child_run_id: &str,
    parent_session_id: &str,
    parent_root: &Path,
) -> Result<String, String> {
    let parent = parent_root
        .canonicalize()
        .map_err(|error| format!("工作区不可用: {}", error))?;
    // 指纹算法口径必须与副本侧 build_changed_files 一致——取决于 git 二进制是否可用，
    // 而非父工作区是否为 git 仓库（副本侧 init_git_baseline 会自行 git init）。
    // 口径错位会让非 git 项目的指纹永不匹配、apply 永远误报冲突。
    let git_available = is_git_available(&parent).await;
    let parent_is_git_repo = parent.join(".git").exists();

    let (kind, status, run_session, row_copy, result_json) = {
        let conn = db.0.lock().map_err(|error| error.to_string())?;
        conn.query_row(
            "SELECT kind, status, parent_session_id, workspace_copy, result_json FROM agent_child_runs WHERE id = ?1",
            rusqlite::params![child_run_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .map_err(|error| format!("未找到子任务 {}: {}", child_run_id, error))?
    };
    if kind != "writer" {
        return Err(format!(
            "child_run_id {} 不是写入工兵（kind={}），只有 writer 结果可机械合入。",
            child_run_id, kind
        ));
    }
    if run_session != parent_session_id {
        return Err("该 writer 不属于当前会话，拒绝合入。".to_string());
    }
    if status == "running" {
        return Err("该 writer 仍在运行，请先 wait_jobs 回收其回执后再合入。".to_string());
    }
    let raw = result_json.ok_or_else(|| "该 writer 未留下可合入的结果数据。".to_string())?;
    let mut wr: WriterResult = serde_json::from_str(&raw)
        .map_err(|error| format!("writer 结果解析失败: {}", error))?;
    if wr.changed_files.is_empty() {
        return Ok(format!("writer {} 未产生文件变更，无需合入。", child_run_id));
    }

    refresh_file_conflicts(&mut wr.changed_files, &parent, git_available).await;

    let conflicts: Vec<String> = wr
        .changed_files
        .iter()
        .filter(|f| f.conflict_status == Some(ConflictStatus::Conflict))
        .map(|f| f.path.clone())
        .collect();
    if !conflicts.is_empty() {
        return Err(format!(
            "❌ 机械合入已整体拒绝：以下 {} 个文件在 writer 开工后被改动（指纹不一致），绝不盲目覆盖：\n{}\n请 read_file 查看最新内容后自行落实，或 resume_child 让 writer 基于新基线重做。",
            conflicts.len(),
            conflicts
                .iter()
                .map(|p| format!("- {}", p))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    let clean: Vec<ChangedFile> = wr
        .changed_files
        .iter()
        .filter(|f| f.conflict_status == Some(ConflictStatus::CleanReplay))
        .cloned()
        .collect();
    let already = wr.changed_files.len() - clean.len();
    if clean.is_empty() {
        return Ok(format!(
            "✅ writer {} 的全部 {} 个变更在主工作区已一致（无需重复合入）。",
            child_run_id, already
        ));
    }

    // 合入前全量校验路径合法性（防逃逸）：f.path 必须是相对路径且不越出工作区
    for f in &clean {
        let rel = Path::new(&f.path);
        if rel.is_absolute()
            || rel
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(format!("非法合入路径: {}", f.path));
        }
    }

    let copy_root = row_copy
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            let wc = wr.workspace_copy.trim().to_string();
            if wc.is_empty() { None } else { Some(wc) }
        })
        .map(PathBuf::from)
        .filter(|p| p.exists());

    if let Some(copy_root) = copy_root {
        let mut applied = Vec::new();
        for f in &clean {
            let src = copy_root.join(&f.path);
            if !src.exists() {
                return Err(format!(
                    "隔离副本中的结果文件缺失: {}；副本可能已被清理，请按回执 diff 手动落实。",
                    f.path
                ));
            }
            let src_hash = compute_file_hash(&src, git_available).await;
            if f.result_hash.as_deref() != Some(src_hash.as_str()) {
                return Err(format!(
                    "副本内 {} 的内容指纹与回执不一致（副本疑似被污染），拒绝合入。",
                    f.path
                ));
            }
            let dest = parent.join(&f.path);
            if let Some(dir) = dest.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|error| format!("创建目录失败 {}: {}", dir.display(), error))?;
            }
            std::fs::copy(&src, &dest)
                .map_err(|error| format!("合入文件失败 {}: {}", f.path, error))?;
            applied.push(f.path.clone());
        }
        let mut report = format!(
            "✅ 机械合入完成：已把 writer {} 的 {} 个变更原样搬回真实工作区{}。\n已合入：\n{}",
            child_run_id,
            applied.len(),
            if already > 0 {
                format!("（另 {} 个文件原本已一致，跳过）", already)
            } else {
                String::new()
            },
            applied
                .iter()
                .map(|p| format!("- {}", p))
                .collect::<Vec<_>>()
                .join("\n")
        );
        if wr.test_status != "passed" {
            report.push_str(&format!(
                "\n⚠️ writer 测试状态: {}——合入后请在真实工作区自行运行 test_command 完成最终验证。",
                wr.test_status
            ));
        } else {
            report.push_str("\n⚠️ writer 的测试证据来自隔离副本；合入后请仍在真实工作区重跑 test_command 做最终验证。");
        }
        return Ok(report);
    }

    // 副本已回收的退化路径：权威 patch 工件 + git apply 干跑校验
    let patch_id = wr.patch_ref.clone().ok_or_else(|| {
        "隔离副本已回收且该 writer 无完整补丁工件；请按回执 diff 手动落实。".to_string()
    })?;
    if !parent_is_git_repo {
        return Err("隔离副本已回收且工作区非 git 仓库，无法机械应用补丁；请按回执 diff 手动落实。".to_string());
    }
    let patch = crate::agent::artifact_manager::read_artifact_content(db, &patch_id)
        .map_err(|error| format!("读取补丁工件失败: {}", error))?;
    let tmp_patch = std::env::temp_dir().join(format!(
        "freeapi-writer-apply-{}.patch",
        &child_run_id[..8.min(child_run_id.len())]
    ));
    std::fs::write(&tmp_patch, &patch)
        .map_err(|error| format!("写入临时补丁失败: {}", error))?;

    // 只合入仍 CleanReplay 的文件；AlreadyApplied 的条目用 --include 过滤掉避免重复应用
    let mut apply_args: Vec<String> = clean
        .iter()
        .map(|f| format!("--include={}", f.path))
        .collect();
    apply_args.push("--whitespace=nowarn".to_string());
    apply_args.push(tmp_patch.to_string_lossy().to_string());

    let check = tokio::process::Command::new("git")
        .arg("apply")
        .arg("--check")
        .args(&apply_args)
        .current_dir(&parent)
        .output()
        .await;
    match check {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            let _ = std::fs::remove_file(&tmp_patch);
            return Err(format!(
                "补丁干跑校验失败（git apply --check 拒绝），未做任何修改：\n{}\n请按回执 diff 手动落实。",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Err(error) => {
            let _ = std::fs::remove_file(&tmp_patch);
            return Err(format!("无法执行 git apply 干跑: {}", error));
        }
    }
    let applied = tokio::process::Command::new("git")
        .arg("apply")
        .args(&apply_args)
        .current_dir(&parent)
        .output()
        .await;
    let _ = std::fs::remove_file(&tmp_patch);
    match applied {
        Ok(out) if out.status.success() => Ok(format!(
            "✅ 机械合入完成：已通过补丁工件把 writer {} 的 {} 个变更应用到真实工作区{}。\n已合入：\n{}\n⚠️ 请在真实工作区重跑 test_command 做最终验证。",
            child_run_id,
            clean.len(),
            if already > 0 {
                format!("（另 {} 个文件原本已一致，跳过）", already)
            } else {
                String::new()
            },
            clean.iter()
                .map(|f| format!("- {}", f.path))
                .collect::<Vec<_>>()
                .join("\n")
        )),
        Ok(out) => Err(format!(
            "git apply 执行失败：\n{}\n请按回执 diff 手动落实。",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(error) => Err(format!("无法执行 git apply: {}", error)),
    }
}

/// 从 Git 全量 diff 中截取指定相对路径文件的独立 diff 块
pub fn extract_file_diff(full_diff: &str, relative_path: &str) -> Option<String> {
    let clean_path = relative_path.replace('\\', "/");
    let marker = format!("diff --git a/{} b/{}", clean_path, clean_path);
    if let Some(pos) = full_diff.find(&marker) {
        let after_marker = &full_diff[pos..];
        let end = after_marker[marker.len()..]
            .find("diff --git ")
            .map(|offset| marker.len() + offset)
            .unwrap_or(after_marker.len());
        return Some(after_marker[..end].trim_end().to_string());
    }
    None
}
