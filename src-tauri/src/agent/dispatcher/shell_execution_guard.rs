//! 派发时冻结已有授权，实际叶子执行前对账；组合工具不能绕过同一项检查。
use super::{
    extract_shell_command_string, git_cmd_fingerprint, git_destructive_review,
    split_shell_segments, GitDestructiveVerdict,
};
use crate::agent::{
    checkpoint::AgentCheckpoint,
    context::SandwichContext,
    types::{AgentInstruction, DispatchResult},
};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

pub(crate) struct ShellExecutionGuard {
    allowed: Vec<String>,
    pre_dirty: Vec<String>,
    pre_dirty_hashes: std::collections::HashMap<String, String>,
    baseline_root: PathBuf,
    primary_git_root: Option<PathBuf>,
    baseline_head: Option<String>,
    additional_baselines: std::collections::BTreeMap<String, crate::agent::reviewer_gatekeeper::GitBaseline>,
    confirmed: std::collections::BTreeMap<String, String>,
    pending: Mutex<std::collections::BTreeMap<String, String>>,
}

struct RepoTaskBaseline {
    paths_root: PathBuf,
    pre_dirty: Vec<String>,
    pre_dirty_hashes: std::collections::HashMap<String, String>,
    base_head: Option<String>,
}

impl ShellExecutionGuard {
    pub(crate) fn capture(
        context: &SandwichContext,
        checkpoint: &AgentCheckpoint,
        workspace: &Path,
    ) -> Self {
        Self {
            allowed: context
                .side_effects
                .iter()
                .flat_map(|e| e.affected_paths.iter().cloned())
                .chain(checkpoint.approved_target_files.iter().flatten().cloned())
                .collect(),
            pre_dirty: checkpoint
                .task_git_baseline
                .as_ref()
                .map(|b| {
                    b.pre_dirty
                        .iter()
                        .flat_map(|(path, hash)| {
                            let decoded = (hash == "<missing>")
                                .then(|| decode_legacy_git_path(path))
                                .flatten();
                            std::iter::once(path.clone()).chain(decoded)
                        })
                        .collect()
                })
                .unwrap_or_default(),
            baseline_root: workspace
                .canonicalize()
                .unwrap_or_else(|_| workspace.to_path_buf()),
            primary_git_root: git_root(workspace),
            baseline_head: checkpoint.task_git_baseline.as_ref().and_then(|b| b.base_head.clone()),
            additional_baselines: checkpoint.task_additional_git_baselines.clone(),
            pre_dirty_hashes: checkpoint
                .task_git_baseline
                .as_ref()
                .map(|b| b.pre_dirty.clone())
                .unwrap_or_default(),
            confirmed: checkpoint.git_destructive_approved_scopes.clone(),
            pending: Mutex::new(Default::default()),
        }
    }

    fn baseline_for_repo(&self, repo_root: &Path, cwd: &Path) -> Result<RepoTaskBaseline, String> {
        let additional = self.additional_baselines.iter()
            .filter(|(scope, baseline)| baseline.is_git && cwd.starts_with(Path::new(scope))
                && git_root(Path::new(scope)).as_deref() == Some(repo_root))
            .max_by_key(|(scope, _)| Path::new(scope).components().count());
        if let Some((scope, baseline)) = additional {
            return Ok(RepoTaskBaseline {
                paths_root: PathBuf::from(scope),
                pre_dirty: baseline.pre_dirty.keys().cloned().collect(),
                pre_dirty_hashes: baseline.pre_dirty.clone(),
                base_head: baseline.base_head.clone(),
            });
        }
        if self.primary_git_root.as_deref() == Some(repo_root) && cwd.starts_with(&self.baseline_root) {
            return Ok(RepoTaskBaseline {
                paths_root: self.baseline_root.clone(),
                pre_dirty: self.pre_dirty.clone(),
                pre_dirty_hashes: self.pre_dirty_hashes.clone(),
                base_head: self.baseline_head.clone(),
            });
        }
        let key = repo_root.to_string_lossy().into_owned();
        let baseline = self.additional_baselines.get(&key)
            .filter(|baseline| baseline.is_git)
            .ok_or_else(|| format!(
                "附加 Git 仓库没有保存独立任务起点：{}；为避免误清理用户原有修改，本次破坏性 Git 操作需要人工确认。旧检查点不会用当前状态补拍起点。",
                repo_root.display()
            ))?;
        let pre_dirty = baseline.pre_dirty.iter().flat_map(|(path, hash)| {
            let decoded = (hash == "<missing>").then(|| decode_legacy_git_path(path)).flatten();
            std::iter::once(path.clone()).chain(decoded)
        }).collect();
        Ok(RepoTaskBaseline {
            paths_root: repo_root.to_path_buf(),
            pre_dirty,
            pre_dirty_hashes: baseline.pre_dirty.clone(),
            base_head: baseline.base_head.clone(),
        })
    }

    pub(crate) fn record_pending(&self, checkpoint: &mut AgentCheckpoint) -> bool {
        let pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        for (fp, scope) in pending.iter() {
            if checkpoint.git_destructive_approved_scopes.get(fp) != Some(scope) {
                checkpoint.git_destructive_approved_scopes.remove(fp);
            }
            checkpoint
                .git_destructive_scopes
                .insert(fp.clone(), scope.clone());
            checkpoint.block_git_destructive(fp);
        }
        !pending.is_empty()
    }

    /// 在参数预检之后调用。每个真实 shell 叶子使用自己的 cwd 与最终参数。
    pub(crate) fn check(
        &self,
        tool: &str,
        action: &str,
        params: &serde_json::Value,
        workspace: &Path,
    ) -> Option<DispatchResult> {
        let inst: AgentInstruction = serde_json::from_value(
            serde_json::json!({"tool":tool,"action":action,"params":params}),
        )
        .ok()?;
        let command = extract_shell_command_string(&inst)?;
        let fp = git_cmd_fingerprint(&command);
        let cwd = params
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace.to_path_buf());
        let cwd = if cwd.is_absolute() {
            cwd
        } else {
            workspace.join(cwd)
        };
        let cwd = cwd.canonicalize().unwrap_or(cwd);
        let segments: Vec<String> = split_shell_segments(&command).into_iter().map(str::to_owned).collect();
        let ambiguous_git_context = has_ambiguous_git_context(&segments);
        let destructive_git_present = segments.iter().any(|segment| segment_has_destructive_git_verb(segment));
        let env_override_with_git = has_git_environment_override(&segments) && destructive_git_present;
        let reason = if env_override_with_git {
            Some("命令通过 GIT_DIR/GIT_WORK_TREE 环境变量改变了仓库定位，无法可靠绑定任务起点；请改用授权目录作为 local_shell.cwd，并使用普通 git 命令，或由主人明确确认。".to_string())
        } else if ambiguous_git_context && destructive_git_present {
            Some("命令包含 cd/pushd/popd、git -C、--git-dir 或 --work-tree，实际仓库位置无法由工具 cwd 可靠确定；请改用 local_shell.cwd 指定授权仓库目录，或由主人明确确认。".to_string())
        } else {
            segments.into_iter().find_map(|segment| {
                match git_destructive_review(&segment) {
                    GitDestructiveVerdict::NotDestructive | GitDestructiveVerdict::SideEffect => None,
                    GitDestructiveVerdict::Pathless => Some(
                        "无显式路径目标，影响面无法按台账界定（整树/历史/远端）".to_string()
                    ),
                    GitDestructiveVerdict::Targets { paths, dirty_gated } => {
                        if ambiguous_git_context {
                            return Some("命令包含 cd/pushd/popd、git -C、--git-dir 或 --work-tree，实际仓库位置无法由工具 cwd 可靠确定；请改用 local_shell.cwd 指定授权仓库目录，或由主人明确确认。".into());
                        }
                        let repo_root = match git_root(&cwd) {
                            Some(root) => root,
                            None => return Some("无法取得当前 Git 仓库根目录，未执行破坏性操作。".into()),
                        };
                        let baseline = match self.baseline_for_repo(&repo_root, &cwd) {
                            Ok(baseline) => baseline,
                            Err(error) => return Some(error),
                        };
                        let restores_baseline = baseline.base_head.as_ref()
                            .is_some_and(|base| restores_task_baseline(&segment, base));
                        match self.foreign_targets(&paths, dirty_gated, &cwd, workspace, &baseline, restores_baseline) {
                            Ok(foreign) if foreign.is_empty() => None,
                            Ok(foreign) => Some(format!(
                                "目标包含任务开始前已有修改或不在本会话写入台账/计划核准集：{}",
                                foreign.join(", ")
                            )),
                            Err(error) => Some(error),
                        }
                    }
                }
            })
        }?;
        let scope = git_operation_scope(&command, &cwd)
            .unwrap_or_else(|_| format!("unresolved:{}", uuid::Uuid::new_v4()));
        if self.confirmed.get(&fp) == Some(&scope) {
            return None;
        }
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.insert(fp.clone(), scope);
        let baseline_reference = git_root(&cwd)
            .and_then(|root| self.baseline_for_repo(&root, &cwd).ok())
            .and_then(|baseline| baseline.base_head)
            .unwrap_or_else(|| "未保存可用的独立仓库起点".into());
        Some(DispatchResult {
            view: None, success: false, stdout: String::new(),
            stderr: format!("【git 破坏命令待人工确认】`{fp}` —— {reason}。本条命令尚未执行。\n当前仓库任务起点引用：{}。若只撤销本轮修改，请先对照起点和当前文件，保留任务前已有内容；git checkout -- 会恢复暂存区版本，不等于任务起点。确需执行上述命令：用 ask_human 询问主人，message 中原样包含完整命令 `{fp}`；回复由主模型正常下一轮判断。否则使用局部修改或备份后处理。", baseline_reference),
            route: "git_destructive_needs_confirm".into(),
        })
    }

    fn foreign_targets(
        &self,
        targets: &[String],
        dirty_gated: bool,
        cwd: &Path,
        workspace: &Path,
        baseline: &RepoTaskBaseline,
        restores_baseline: bool,
    ) -> Result<Vec<String>, String> {
        let root = git_root(cwd).ok_or("无法取得 Git 根目录，未确认还原目标是否包含已有修改")?;
        let allowed: HashSet<_> = self
            .allowed
            .iter()
            .map(|p| normalize(p, workspace))
            .collect();
        let pre_dirty: HashSet<_> = baseline
            .pre_dirty
            .iter()
            .map(|p| normalize(p, &baseline.paths_root))
            .collect();
        // 让 Git 自己解释目录、通配符、:(top)/:(literal) 等 pathspec；-z 保留空格/Unicode/换行。
        // 暂存删除的文件已不在 index，仍可被 checkout HEAD/restore --source=HEAD 还原。
        let mut args = vec!["ls-files", "-z", "--cached", "--with-tree=HEAD"];
        if !dirty_gated {
            args.extend(["--others", "--exclude-standard"]);
        }
        args.push("--");
        args.extend(targets.iter().map(String::as_str));
        let files = git_paths(cwd, &args)?;
        let dirty: HashSet<_> = if dirty_gated {
            // 分开比较 index/worktree；二者相抵时 diff HEAD 为空，仍有未提交状态。
            let mut paths = git_paths(&root, &["diff", "--name-only", "--no-renames", "-z", "--"])?;
            paths.extend(git_paths(
                &root,
                &[
                    "diff",
                    "--cached",
                    "--name-only",
                    "--no-renames",
                    "-z",
                    "HEAD",
                    "--",
                ],
            )?);
            paths.extend(git_paths(
                &root,
                &["ls-files", "--others", "--exclude-standard", "-z"],
            )?);
            paths.into_iter().map(|p| normalize(&p, &root)).collect()
        } else {
            HashSet::new()
        };
        Ok(files
            .into_iter()
            .filter(|p| {
                let absolute = normalize(p, cwd);
                (!dirty_gated || dirty.contains(&absolute))
                    && (!absolute.starts_with(&baseline.paths_root) || (pre_dirty.contains(&absolute)
                        && !(restores_baseline
                            && Self::baseline_preserves_original(&absolute, &root, baseline)))
                        || !allowed.contains(&absolute))
            })
            .collect())
    }

    fn baseline_preserves_original(target: &Path, repo_root: &Path, baseline: &RepoTaskBaseline) -> bool {
        use sha2::{Digest, Sha256};
        let Some((_, expected)) = baseline.pre_dirty_hashes.iter().find(|(path, hash)| {
            hash.as_str() != "<missing>" && normalize(path, &baseline.paths_root) == target
        }) else {
            return false;
        };
        let Some(base) = &baseline.base_head else {
            return false;
        };
        let Some(relative) = target.strip_prefix(repo_root).ok() else {
            return false;
        };
        // Git 对象内路径使用 / 分隔，文件名本身仍按原始组件保留。
        let Some(components) = relative
            .iter()
            .map(|part| part.to_str())
            .collect::<Option<Vec<_>>>()
        else {
            return false;
        };
        let path = components.join("/");
        let Ok(output) = Command::new("git")
            .args(["show", &format!("{base}:{path}")])
            .current_dir(repo_root)
            .output()
        else {
            return false;
        };
        output.status.success() && format!("{:x}", Sha256::digest(output.stdout)) == *expected
    }
}

/// 只识别明确的“恢复本任务起点工作区内容”，不修改用户暂存区。
fn restores_task_baseline(command: &str, baseline: &str) -> bool {
    let tokens = super::shell_tokens(command.trim());
    if tokens.get(0).map(String::as_str) != Some("git")
        || tokens.get(1).map(String::as_str) != Some("restore")
    {
        return false;
    }
    let mut source = None;
    let mut worktree = false;
    let mut index = 2;
    while let Some(token) = tokens.get(index) {
        match token.as_str() {
            "--" => {
                return worktree && source.as_deref() == Some(baseline) && tokens.len() > index + 1
            }
            "--worktree" if !worktree => worktree = true,
            "--source" if source.is_none() => {
                index += 1;
                source = tokens.get(index).cloned();
            }
            _ if token.starts_with("--source=") && source.is_none() => {
                source = Some(token[9..].to_string())
            }
            _ => return false,
        }
        index += 1;
    }
    false
}

/// 绑定原始命令、实际 cwd、HEAD/index 状态和现有脏文件内容；不把许可复用于另一份内容。
pub(crate) fn git_operation_scope(command: &str, cwd: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let cwd = cwd.canonicalize().map_err(|e| e.to_string())?;
    let root = git_root(&cwd).ok_or("无法取得 Git 根目录")?;
    let mut hash = Sha256::new();
    hash.update(serde_json::to_vec(&(command, cwd.to_string_lossy())).map_err(|e| e.to_string())?);
    let mut selectors = Vec::new();
    let mut whole_tree = false;
    for segment in split_shell_segments(command) {
        match git_destructive_review(segment) {
            GitDestructiveVerdict::Targets { paths, .. } => selectors.extend(paths),
            GitDestructiveVerdict::Pathless => whole_tree = true,
            _ => {}
        }
    }
    let query_root = if whole_tree { &root } else { &cwd };
    if whole_tree {
        selectors.clear();
    }
    let with_paths = |args: Vec<&str>| {
        args.into_iter()
            .map(str::to_string)
            .chain(std::iter::once("--".to_string()))
            .chain(selectors.iter().cloned())
            .collect::<Vec<_>>()
    };
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&root)
        .output()
        .map_err(|e| e.to_string())?;
    hash.update(head.stdout);
    // 只绑定操作实际覆盖的路径；旁边运行日志变化不使这次许可失效。
    for args in [
        with_paths(vec![
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ]),
        with_paths(vec!["diff", "--cached", "--raw", "--relative", "-z"]),
    ] {
        let output = Command::new("git")
            .args(args)
            .current_dir(query_root)
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err("Git 操作版本查询失败".into());
        }
        hash.update(output.stdout.len().to_le_bytes());
        hash.update(output.stdout);
    }
    let mut paths = git_paths(
        query_root,
        &with_paths(vec![
            "diff",
            "--name-only",
            "--no-renames",
            "--relative",
            "-z",
        ]),
    )?;
    paths.extend(git_paths(
        query_root,
        &with_paths(vec![
            "diff",
            "--cached",
            "--name-only",
            "--no-renames",
            "--relative",
            "-z",
        ]),
    )?);
    paths.extend(git_paths(
        query_root,
        &with_paths(vec!["ls-files", "--others", "--exclude-standard", "-z"]),
    )?);
    paths.sort();
    paths.dedup();
    for path in paths {
        hash.update(path.as_bytes());
        hash.update([0]);
        let file = query_root.join(path);
        match std::fs::symlink_metadata(&file) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                hash.update(b"symlink");
                hash.update(
                    std::fs::read_link(&file)
                        .map_err(|e| e.to_string())?
                        .to_string_lossy()
                        .as_bytes(),
                );
            }
            Ok(metadata) if metadata.is_file() => {
                hash.update(b"file");
                let mut input = std::fs::File::open(file).map_err(|e| e.to_string())?;
                let mut bytes = [0u8; 65536];
                loop {
                    let n = input.read(&mut bytes).map_err(|e| e.to_string())?;
                    if n == 0 {
                        break;
                    }
                    hash.update(&bytes[..n]);
                }
            }
            Ok(_) => {
                hash.update(b"directory");
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                hash.update(b"missing");
            }
            Err(error) => return Err(error.to_string()),
        }
        hash.update([0]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn normalize(raw: &str, base: &Path) -> PathBuf {
    let path = Path::new(raw);
    let path = crate::agent::context::resource_scope::lexically_normalize(&if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    });
    // 对齐 /var 与 /private/var 等物理目录别名；不跟随被 Git 操作的最终符号链接。
    match (
        path.parent().and_then(|p| p.canonicalize().ok()),
        path.file_name(),
    ) {
        (Some(parent), Some(name)) => parent.join(name),
        _ => path,
    }
}

/// 旧基线把 Git 展示用的 C 引号路径当作文件名，哈希因此为 <missing>。
/// 仅补充严格解码成功的候选；保留原键，不改写历史或猜测不存在的路径。
fn decode_legacy_git_path(raw: &str) -> Option<String> {
    let bytes = raw.strip_prefix('"')?.strip_suffix('"')?.as_bytes();
    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            result.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        let escaped = *bytes.get(i)?;
        if (b'0'..=b'3').contains(&escaped) {
            let rest = bytes.get(i..i + 3)?;
            if !rest.iter().all(|b| (b'0'..=b'7').contains(b)) {
                return None;
            }
            result.push((rest[0] - b'0') * 64 + (rest[1] - b'0') * 8 + (rest[2] - b'0'));
            i += 3;
        } else {
            result.push(match escaped {
                b'"' | b'\\' => escaped,
                b'a' => 7,
                b'b' => 8,
                b't' => 9,
                b'n' => 10,
                b'v' => 11,
                b'f' => 12,
                b'r' => 13,
                _ => return None,
            });
            i += 1;
        }
    }
    String::from_utf8(result).ok()
}

fn git_root(cwd: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() { return None; }
    PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
        .canonicalize()
        .ok()
}

fn segment_has_destructive_git_verb(segment: &str) -> bool {
    let tokens = super::shell_tokens(segment);
    let Some(git_index) = tokens.iter().position(|token| {
        Path::new(token).file_name().and_then(|name| name.to_str()) == Some("git")
    }) else { return false; };
    let args = &tokens[git_index + 1..];
    for (index, token) in args.iter().enumerate() {
        match token.as_str() {
            "checkout" | "restore" | "clean" | "reset" => return true,
            "stash" => return !matches!(args.get(index + 1).map(String::as_str), Some("list" | "show")),
            _ => {}
        }
    }
    false
}

fn has_ambiguous_git_context(segments: &[String]) -> bool {
    segments.iter().any(|segment| {
        let tokens = super::shell_tokens(segment);
        if matches!(tokens.first().map(String::as_str), Some("cd" | "pushd" | "popd")) {
            return true;
        }
        let Some(git_index) = tokens.iter().position(|token| {
            Path::new(token).file_name().and_then(|name| name.to_str()) == Some("git")
        }) else { return false; };
        tokens[git_index + 1..].iter().any(|token| {
            token == "-C" || token.starts_with("-C")
                || token == "--git-dir" || token.starts_with("--git-dir=")
                || token == "--work-tree" || token.starts_with("--work-tree=")
        })
    })
}

fn has_git_environment_override(segments: &[String]) -> bool {
    segments.iter().any(|segment| {
        super::shell_tokens(segment).iter().any(|token| {
            token.starts_with("GIT_DIR=") || token.starts_with("GIT_WORK_TREE=")
        })
    })
}

fn git_paths<S: AsRef<str>>(cwd: &Path, args: &[S]) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .args(args.iter().map(|arg| arg.as_ref()))
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("Git 目标查询失败：{e}"))?;
    if !output.status.success() {
        return Err("Git 目标查询未成功，未确认还原目标是否包含已有修改".into());
    }
    output
        .stdout
        .split(|b| *b == 0)
        .filter(|b| !b.is_empty())
        .map(|b| {
            String::from_utf8(b.to_vec())
                .map_err(|_| "Git 路径不是有效 UTF-8，未确认目标归属".into())
        })
        .collect()
}

#[cfg(test)]
mod tests;
