//! 终态文件检查的任务边界；不持久化新状态，不改变日志或任务生命周期。
use super::*;
use chrono::Utc;
use std::collections::BTreeSet;
use std::path::PathBuf;

pub(crate) struct TaskReviewFiles {
    pub paths: Vec<PathBuf>,
    pub notices: Vec<String>,
}

/// 只读取现有交付字段，不从回答正文、文件名或扩展名猜测交付对象。
pub(crate) fn delivery_file_paths(
    db: &DbState,
    session: &str,
    payload: &serde_json::Value,
) -> Vec<String> {
    let mut paths = BTreeSet::new();
    for key in ["artifacts", "files", "created_files", "paths"] {
        let Some(value) = payload.get(key) else {
            continue;
        };
        let values: Vec<_> = value
            .as_array()
            .map(|v| v.iter().collect())
            .unwrap_or_else(|| vec![value]);
        for value in values {
            if let Some(path) = value.as_str().or_else(|| {
                ["canonical_path", "path", "file_path"]
                    .iter()
                    .find_map(|key| value.get(key).and_then(|v| v.as_str()))
            }) {
                if !path.trim().is_empty() {
                    paths.insert(path.to_string());
                }
            } else if let Some(id) = value
                .get("artifact_id")
                .or_else(|| value.get("id"))
                .and_then(|v| v.as_str())
            {
                if let Ok(record) = crate::agent::artifact_manager::get_artifact_record(db, id) {
                    if record.session_id == session {
                        paths.insert(record.canonical_path);
                    }
                }
            }
        }
    }
    paths.into_iter().collect()
}

fn physical(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

/// 已有 shell 任务身份确认的日志路径；不把整个 jobs 目录或所有 .log 文件排除。
fn host_job_logs(db: &DbState, session: &str, root: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare("SELECT id,result_json FROM agent_background_jobs WHERE parent_session_id=?1 AND job_type='shell'")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([session], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })
        .map_err(|e| e.to_string())?;
    let mut paths = BTreeSet::new();
    for row in rows {
        let (id, result) = row.map_err(|e| e.to_string())?;
        // 旧版本宿主固定生成的精确路径；不迁移历史文件。
        if Path::new(&id).components().count() == 1 {
            paths.insert(physical(root.join("jobs").join(format!("{id}.log"))));
        }
        if let Some(path) = result
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| {
                v.get("log_path")
                    .and_then(|p| p.as_str())
                    .map(str::to_owned)
            })
        {
            paths.insert(physical(PathBuf::from(path)));
        }
    }
    Ok(paths)
}

pub(crate) fn task_review_files(
    db: &DbState,
    session: &str,
    workspace: &Path,
    context: &SandwichContext,
    _task_start: chrono::DateTime<Utc>,
    delivered: &[String],
) -> TaskReviewFiles {
    let root = crate::agent::path_guard::get_session_tmp_dir(session);
    let mut notices = Vec::new();
    let host_logs = host_job_logs(db, session, &root).unwrap_or_else(|e| {
        notices.push(format!("后台日志来源暂无法核对：{e}；不据此新增交付条件。"));
        BTreeSet::new()
    });
    let mut paths = BTreeSet::new();
    // 临时目录只纳入已登记路径和明确交付对象，不遍历历史文件。
    // 任务写入线索可补上保留旧 mtime 的文件；不是修改者或完成度证明。
    for raw in context
        .side_effects
        .iter()
        .skip(context.inherited_side_effects_count)
        .flat_map(|effect| effect.affected_paths.iter())
    {
        if let Ok(resolved) =
            crate::agent::path_guard::ensure_readable_path(raw, workspace, Some(session), false)
        {
            if resolved.canonical_path.is_file() && !host_logs.contains(&resolved.canonical_path) {
                paths.insert(resolved.canonical_path);
            }
        }
    }
    // 受管产物位于工作区/tmp 之外；只允许本会话已登记的精确物理文件，
    // 不把整个 artifacts 目录添加到通用文件权限。
    let registered: BTreeSet<PathBuf> = if delivered.is_empty() {
        BTreeSet::new()
    } else {
        crate::agent::artifact_manager::list_session_artifacts(db, session)
            .unwrap_or_default()
            .into_iter()
            .map(|r| PathBuf::from(r.canonical_path))
            .collect()
    };
    // 明确交付优先于时间与日志默认用途，包括再次交付旧文件。
    for raw in delivered {
        match crate::agent::path_guard::ensure_readable_path(raw, workspace, Some(session), false) {
            Ok(resolved) => {
                paths.insert(resolved.canonical_path);
            }
            Err(_)
                if registered.contains(&PathBuf::from(raw))
                    && Path::new(raw).canonicalize().ok().as_ref() == Some(&PathBuf::from(raw)) =>
            {
                paths.insert(PathBuf::from(raw));
            }
            Err(error) => notices.push(format!(
                "明确交付路径 {raw} 暂不可读取：{error}；请结合任务核验，不代表已检查。"
            )),
        }
    }
    TaskReviewFiles {
        paths: paths.into_iter().collect(),
        notices,
    }
}

pub(crate) fn task_review_snapshot(
    db: &DbState,
    session: &str,
    workspace: &Path,
    context: &SandwichContext,
    baseline: &GitBaseline,
    task_start: chrono::DateTime<Utc>,
    delivered: &[String],
) -> (ReviewMaterialSnapshot, TaskReviewFiles) {
    let mut files = task_review_files(db, session, workspace, context, task_start, delivered);
    let mut snapshot = ReviewMaterialSnapshot::empty();
    let tmp = crate::agent::path_guard::get_session_tmp_dir(session);
    let mut scopes = std::collections::BTreeMap::new();
    if workspace.canonicalize().ok() != tmp.canonicalize().ok() {
        scopes.insert(workspace.to_string_lossy().into_owned(), baseline.clone());
    }
    match saved_task_baselines(db, session, &context.history_task_id) {
        Ok(extra) => scopes.extend(extra),
        Err(e) => files.notices.push(format!("任务目录起点暂不可读取：{e}；未完成范围核对。")),
    }
    for dir in crate::agent::workspace_access::directories(Some(session), workspace) {
        if dir.access == crate::agent::workspace_access::DirectoryAccess::ReadWrite
            && !scopes.keys().any(|root| Path::new(&dir.path).starts_with(root))
        {
            files.notices.push(format!("可写目录 {} 未保存本任务起点；不以当前状态补拍或推断此前无变化，必要时按路径取证。", dir.path));
        }
    }
    for (root, baseline) in scopes {
        let root = PathBuf::from(root);
        if root.canonicalize().ok().as_ref() != Some(&root) {
            files.notices.push(format!("任务目录 {} 当前不可用或物理路径已改变，未完成起点比较。", root.display()));
            continue;
        }
        let authorized = root == workspace || crate::agent::path_guard::ensure_readable_path(
            &root.to_string_lossy(), workspace, Some(session), false).is_ok();
        if !authorized {
            files.notices.push(format!("任务目录 {} 当前未授权读取，未自动取材。", root.display()));
            continue;
        }
        if !baseline.is_git {
            files.notices.push(format!("目录 {} 未建立可用起点；仅核对已登记与明确交付路径，不以时间扫描推定任务变化。", root.display()));
            continue;
        }
        if let Some(base) = &baseline.base_head {
            if !git_command(&root).args(["cat-file", "-e", &format!("{base}^{{tree}}")])
                .output().is_ok_and(|o| o.status.success())
            {
                files.notices.push(format!("目录 {} 的任务起点对象 {} 当前不可读取，未完成差异比较。", root.display(), base));
                continue;
            }
        }
        let paths = baseline.collect_changed_file_paths(&root, task_start);
        // 先列完整范围，正文只作有界节选。绝对路径区分不同项目的同名文件。
        let listing = paths.iter().map(|p| root.join(p).to_string_lossy().into_owned()).collect::<Vec<_>>();
        let material = baseline.collect_review_snapshot(&root, context, task_start);
        if !listing.is_empty() {
            snapshot.material.push_str(&format!("\n【任务工程变化】{}\n{}\n{}\n", root.display(), listing.join("\n"), material.material));
            snapshot.hash.push_str(&material.hash);
        }
        for raw in paths {
            let path = root.join(raw);
            // 撤销/嵌套只读权限实时生效；删除路径也保留，由文件读取反馈不存在。
            if crate::agent::path_guard::ensure_readable_path(&path.to_string_lossy(), workspace, Some(session), false).is_ok() {
                files.paths.push(path);
            }
        }
    }
    files.paths.sort();
    files.paths.dedup();
    snapshot.append_task_files(&files);
    (snapshot, files)
}
