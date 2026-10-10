//! 评审 Git 基线：变更 diff 捕获与复算。

use super::*;
use crate::agent::context::SandwichContext;
use crate::agent::router::{AgentRouter, ModelRoutingConfig, roles};
use crate::agent::tools::role_manager::resolve_enabled_card;
use crate::db::DbState;
use crate::llm::{self, ModelConfig};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;


impl GitBaseline {
    /// 任务启动时捕获当前工作区 Git 状态基准
    pub fn capture(workspace: &Path) -> Self {
        if !is_git_repo(workspace) {
            return Self {
                is_git: false,
                base_head: None,
                pre_dirty: Default::default(),
            };
        }
        let base_head = git_cmd(workspace, &["rev-parse", "HEAD"]);
        Self {
            is_git: true,
            base_head,
            pre_dirty: Default::default(),
        }
    }

    /// 任务起点捕获：在 capture() 基础上记录当前已脐文件的内容哈希。
    /// 阶段审查等「即时复算」场景仍用 capture()（不记录脐文件）。
    pub fn capture_at_run_start(workspace: &Path) -> Self {
        // 非 git 目录：先在应用数据目录建「影子仓库」快照（失败则仍走 mtime 扫描），之后 capture() 就能走 git 路径。
        let _ = ensure_shadow_snapshot(workspace);
        let mut b = Self::capture(workspace);
        if b.is_git {
            let mut names: Vec<String> = Vec::new();
            if let Some(t) = git_cmd(workspace, &["diff", "--name-only", "--relative", "HEAD"]) {
                names.extend(t.lines().map(|l| l.trim().to_string()));
            }
            if let Some(t) = git_cmd(workspace, &["ls-files", "--others", "--exclude-standard"]) {
                names.extend(t.lines().map(|l| l.trim().to_string()));
            }
            for n in names.into_iter().filter(|n| !n.is_empty()) {
                let h = Self::file_content_hash(workspace, &n);
                b.pre_dirty.insert(n, h);
            }
            // 用 git 自己存「启动时的工作区状态」：stash create 只生成一个 commit 对象并返回其 sha，
            // 不动工作区、不写 stash 列表。之后 `git diff <sha>` 就是「启动以来的改动」，
            // 同一文件里用户原有的几行也能分开（未跟踪文件仍走上面的指纹）。
            // 工作区干净或命令失败（如未配置身份）时返回空，继续用 HEAD 作起点。
            if let Some(snap) = git_cmd(workspace, &["stash", "create"]) {
                let snap = snap.trim().to_string();
                if !snap.is_empty() {
                    b.base_head = Some(snap);
                }
            }
        }
        b
    }

    fn file_content_hash(workspace: &Path, rel: &str) -> String {
        match std::fs::read(workspace.join(rel)) {
            Ok(bytes) => {
                let mut hasher = Sha256::new();
                hasher.update(&bytes);
                format!("{:x}", hasher.finalize())
            }
            Err(_) => "<missing>".to_string(),
        }
    }

    /// 该文件在任务启动时就是脐的，且内容至今没有变化（即本次任务没碰过）
    pub(crate) fn unchanged_since_baseline(&self, workspace: &Path, rel: &str) -> bool {
        self.pre_dirty
            .get(rel)
            .is_some_and(|h| *h == Self::file_content_hash(workspace, rel))
    }

    /// 自由会话降级构造：无真实工作区锚点（workspace_dir 实为进程 cwd 的"事故目录"）
    /// 时使用——等价于"工作区非 git 仓库"，diff 域证据全部缺席，审查改靠
    /// 副作用台账与产物注册链。与 capture() 命中非 repo 目录的结果一致。
    pub fn non_git() -> Self {
        Self {
            is_git: false,
            base_head: None,
            pre_dirty: Default::default(),
        }
    }

    /// 提取相对起点的审查材料：
    /// 1. 有 Git 仓库：提取 Git Diff（并结合未跟踪新文件）
    /// 2. 无 Git 仓库 / 临时目录：扫描任务启动后新增/修改的文本文件与二进制产物元数据（若无改动则 0ms 免检）
    pub fn collect_review_material(
        &self,
        workspace: &Path,
        context: &SandwichContext,
        run_started_at: chrono::DateTime<chrono::Utc>,
    ) -> Option<(String, String)> {
        // 若本轮为纯对话/只读任务（未产生任何新增写入副作用），直接 0ms 跳过代码评审
        if context.side_effects.len() == context.inherited_side_effects_count {
            return None;
        }

        if self.is_git {
            if let Some(res) = self.collect_git_diff(workspace) {
                return Some(res);
            }
            // Git 模式下若无任何 Diff，且无新增外部副作用台账，判定为无文件变更任务（0ms 跳过）
            if context.side_effects.len() == context.inherited_side_effects_count {
                return None;
            }
        }

        // --- Non-Git 目录或无 Git 跟踪变更的 Fallback 收集 ---
        Self::collect_fallback_workspace_changes(workspace, context, run_started_at)
    }

    /// 提取任务期间新增或修改的文件相对路径列表（用于写后范围防逃逸校验）
    pub fn collect_changed_file_paths(
        &self,
        workspace: &Path,
        run_started_at: chrono::DateTime<chrono::Utc>,
    ) -> Vec<String> {
        let mut paths = Vec::new();
        if self.is_git {
            let diff_args = if let Some(ref base) = self.base_head {
                vec!["diff", "--name-only", base.as_str()]
            } else {
                vec!["diff", "--name-only", "HEAD"]
            };
            if let Some(text) = git_cmd(workspace, &diff_args) {
                for line in text.lines() {
                    let l = line.trim();
                    if !l.is_empty() && !is_noise_path(l) {
                        paths.push(l.to_string());
                    }
                }
            }
            if let Some(untracked) = git_cmd(workspace, &["ls-files", "--others", "--exclude-standard"]) {
                for line in untracked.lines() {
                    let l = line.trim();
                    if !l.is_empty() && !is_noise_path(l) {
                        paths.push(l.to_string());
                    }
                }
            }
        } else {
            let since_sys = std::time::UNIX_EPOCH
                + std::time::Duration::from_millis(run_started_at.timestamp_millis().max(0) as u64);
            let mut modified_files = Vec::new();
            Self::scan_modified_files(workspace, 0, since_sys, &mut modified_files);
            for p in modified_files {
                if let Ok(rel) = p.strip_prefix(workspace) {
                    let s = rel.to_string_lossy().to_string();
                    if !is_noise_path(&s) {
                        paths.push(s);
                    }
                }
            }
        }
        paths.retain(|p| !self.unchanged_since_baseline(workspace, p));
        paths
    }

    /// Git 模式下的代码变更 Diff 提取
    pub fn collect_git_diff(&self, workspace: &Path) -> Option<(String, String)> {
        // 非 git 基线（含自由会话 non_git() 降级态）不跑任何 git 命令——
        // 否则在"恰好是 repo 的事故 cwd"上会把宿主自己的脏 diff 误当任务改动。
        if !self.is_git {
            return None;
        }
        // 1. 获取 Git Diff 文本
        let mut diff_args: Vec<String> = vec![
            "diff".to_string(),
            self.base_head.clone().unwrap_or_else(|| "HEAD".to_string()),
        ];
        // 排除任务启动时就脐、且本次没碰过的文件（用户原有的未提交改动）
        let skip: Vec<&String> = self
            .pre_dirty
            .keys()
            .filter(|p| self.unchanged_since_baseline(workspace, p))
            .collect();
        if !skip.is_empty() {
            diff_args.push("--".to_string());
            diff_args.push(".".to_string());
            for p in &skip {
                diff_args.push(format!(":(exclude,literal){}", p));
            }
        }
        let diff_refs: Vec<&str> = diff_args.iter().map(|s| s.as_str()).collect();
        let raw_diff = git_cmd(workspace, &diff_refs).unwrap_or_default();

        // 2. 补足未暂存的 untracked 新文件摘要
        let untracked_files = git_cmd(workspace, &["ls-files", "--others", "--exclude-standard"])
            .unwrap_or_default();

        let mut combined_diff = String::new();
        if !raw_diff.trim().is_empty() {
            let filtered = filter_diff_noise(&raw_diff);
            if !filtered.trim().is_empty() {
                combined_diff.push_str(&filtered);
            }
        }

        if !untracked_files.trim().is_empty() {
            let mut untracked_chunks = Vec::new();
            for file_rel in untracked_files.lines() {
                let file_rel = file_rel.trim();
                if file_rel.is_empty()
                    || is_noise_path(file_rel)
                    || self.unchanged_since_baseline(workspace, file_rel)
                {
                    continue;
                }
                let full_path = workspace.join(file_rel);
                if full_path.is_file() {
                    if is_binary_file(file_rel) {
                        let size_str = full_path.metadata().map(|m| format!("{} KB", m.len() / 1024)).unwrap_or_else(|_| "未知".to_string());
                        untracked_chunks.push(format!("【新增二进制产物】文件: {} (大小: {})", file_rel, size_str));
                    } else if let Ok(content) = std::fs::read_to_string(&full_path) {
                        let sample = if content.chars().count() > 2000 {
                            format!("{}...\n[文件过长截断]", content.chars().take(2000).collect::<String>())
                        } else {
                            content
                        };
                        untracked_chunks.push(format!("--- 新增文件: {} ---\n{}", file_rel, sample));
                    }
                }
            }
            if !untracked_chunks.is_empty() {
                if !combined_diff.is_empty() {
                    combined_diff.push_str("\n\n");
                }
                combined_diff.push_str("【新增未跟踪文件】:\n");
                combined_diff.push_str(&untracked_chunks.join("\n\n"));
            }
        }

        let trimmed = combined_diff.trim();
        if trimmed.is_empty() {
            return None;
        }

        // 3. 计算 Diff 的 SHA256 哈希
        let mut hasher = Sha256::new();
        hasher.update(trimmed.as_bytes());
        let hash_hex = format!("{:x}", hasher.finalize());

        // 4. 控制 Diff 尺寸上限
        let final_diff = if trimmed.chars().count() > MAX_DIFF_CHARS {
            format!(
                "{}\n\n... [Diff 过长，已安全截断前 {} 字符进行审查]",
                trimmed.chars().take(MAX_DIFF_CHARS).collect::<String>(),
                MAX_DIFF_CHARS
            )
        } else {
            trimmed.to_string()
        };

        Some((final_diff, hash_hex))
    }

    /// 非 Git 环境下的工作区变更与产物提取（泛化支持各类脚本、Excel、文档等）
    pub(crate) fn collect_fallback_workspace_changes(
        workspace: &Path,
        context: &SandwichContext,
        run_started_at: chrono::DateTime<chrono::Utc>,
    ) -> Option<(String, String)> {
        let since_sys = std::time::UNIX_EPOCH + std::time::Duration::from_millis(run_started_at.timestamp_millis().max(0) as u64);
        let mut sections = Vec::new();

        // 1. 扫描工作区自任务启动后修改或新增的文件
        let mut modified_files = Vec::new();
        Self::scan_modified_files(workspace, 0, since_sys, &mut modified_files);

        for path in &modified_files {
            let Ok(rel_path) = path.strip_prefix(workspace) else { continue };
            let rel_str = rel_path.to_string_lossy().to_string();
            if is_noise_path(&rel_str) {
                continue;
            }

            if is_binary_file(&rel_str) {
                let size_str = path.metadata().map(|m| format!("{} KB", m.len() / 1024)).unwrap_or_else(|_| "未知".to_string());
                sections.push(format!("【生成二进制产物】文件: {} (大小: {})", rel_str, size_str));
            } else if let Ok(content) = std::fs::read_to_string(path) {
                let sample = if content.chars().count() > 3000 {
                    format!("{}...\n[文件过长截断]", content.chars().take(3000).collect::<String>())
                } else {
                    content
                };
                sections.push(format!("--- 文件内容: {} ---\n{}", rel_str, sample));
            }
        }

        // 2. 结合 Side-Effects 台账（仅收集本轮新增的副作用）
        let new_side_effects = if context.side_effects.len() > context.inherited_side_effects_count {
            &context.side_effects[context.inherited_side_effects_count..]
        } else {
            &[]
        };
        if !new_side_effects.is_empty() {
            let side_effects_desc = new_side_effects.iter()
                .map(|r| format!("- [Step {}] {} · {}", r.step_id, r.tool, r.description))
                .collect::<Vec<_>>()
                .join("\n");
            sections.push(format!("【执行的外部不可逆操作清单 (Side-Effects)】:\n{}", side_effects_desc));
        }

        let combined = sections.join("\n\n").trim().to_string();
        if combined.is_empty() {
            return None;
        }

        // 3. 计算 SHA256 哈希
        let mut hasher = Sha256::new();
        hasher.update(combined.as_bytes());
        let hash_hex = format!("{:x}", hasher.finalize());

        // 4. 控制上限
        let final_text = if combined.chars().count() > MAX_DIFF_CHARS {
            format!(
                "{}\n\n... [审查材料过长，已安全截断前 {} 字符进行审查]",
                combined.chars().take(MAX_DIFF_CHARS).collect::<String>(),
                MAX_DIFF_CHARS
            )
        } else {
            combined
        };

        Some((final_text, hash_hex))
    }

    pub(crate) fn scan_modified_files(
        dir: &Path,
        depth: usize,
        since: std::time::SystemTime,
        out: &mut Vec<std::path::PathBuf>,
    ) {
        if depth > 4 || out.len() >= 20 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || is_noise_path(&name) {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    Self::scan_modified_files(&path, depth + 1, since, out);
                } else if meta.is_file() {
                    let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                    if mtime >= since {
                        out.push(path);
                    }
                }
            }
        }
    }
}
