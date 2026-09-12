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

const MAX_DIFF_CHARS: usize = 25_000;
const REVIEW_TIMEOUT_SECS: u64 = 35;

/// Git 任务起点基准快照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitBaseline {
    pub is_git: bool,
    pub base_head: Option<String>,
}

impl GitBaseline {
    /// 任务启动时捕获当前工作区 Git 状态基准
    pub fn capture(workspace: &Path) -> Self {
        if !is_git_repo(workspace) {
            return Self {
                is_git: false,
                base_head: None,
            };
        }
        let base_head = git_cmd(workspace, &["rev-parse", "HEAD"]);
        Self {
            is_git: true,
            base_head,
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
        paths
    }

    /// Git 模式下的代码变更 Diff 提取
    pub fn collect_git_diff(&self, workspace: &Path) -> Option<(String, String)> {
        // 1. 获取 Git Diff 文本
        let diff_args = if let Some(ref base) = self.base_head {
            vec!["diff", base.as_str()]
        } else {
            vec!["diff", "HEAD"]
        };
        let raw_diff = git_cmd(workspace, &diff_args).unwrap_or_default();

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
                if file_rel.is_empty() || is_noise_path(file_rel) {
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
    fn collect_fallback_workspace_changes(
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

    fn scan_modified_files(
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

/// 审查阻塞性缺陷结构
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BlockingIssue {
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub issue: String,
    #[serde(default)]
    pub requirement: String,
}

/// 次要改进建议结构（不阻碍交付）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NitIssue {
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub suggestion: String,
}

/// 终态代码审查结果
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewResult {
    pub verdict: String, // "passed" | "rejected" | "skipped" | "failed_open"
    pub score: u32,      // 0-100 仅供遥测与 UI 显示
    pub blocking_issues: Vec<BlockingIssue>,
    pub nit_issues: Vec<NitIssue>,
    pub summary: String,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub model_id: String,
    #[serde(default)]
    pub diff_hash: String,
}

impl ReviewResult {
    pub fn is_passed(&self) -> bool {
        self.blocking_issues.is_empty()
    }

    pub fn skipped_empty() -> Self {
        Self {
            verdict: "passed".to_string(),
            score: 100,
            blocking_issues: Vec::new(),
            nit_issues: Vec::new(),
            summary: "任务未涉及代码文件修改，免除 Diff 审查".to_string(),
            duration_ms: 0,
            model_id: "none".to_string(),
            diff_hash: String::new(),
        }
    }

    pub fn failed_open(reason: &str, model_id: &str, diff_hash: &str) -> Self {
        Self {
            verdict: "failed_open".to_string(),
            score: 85,
            blocking_issues: Vec::new(),
            nit_issues: Vec::new(),
            summary: format!("审查服务异常，降级安全放行: {}", reason),
            duration_ms: 0,
            model_id: model_id.to_string(),
            diff_hash: diff_hash.to_string(),
        }
    }

    pub fn rejected_with_scope_drift(reason: &str, diff_hash: &str) -> Self {
        Self {
            verdict: "rejected".to_string(),
            score: 0,
            blocking_issues: vec![BlockingIssue {
                file: "global".to_string(),
                issue: reason.to_string(),
                requirement: "立即撤销未经审批的高危文件改动，或在 Plan 中向人类申请特赦/扩展审批".to_string(),
            }],
            nit_issues: Vec::new(),
            summary: reason.to_string(),
            duration_ms: 0,
            model_id: "scope_drift_guard".to_string(),
            diff_hash: diff_hash.to_string(),
        }
    }
}

/// 判定是否为二进制文件（Excel/Word/PDF/图片/压缩包等）
pub fn is_binary_file(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.ends_with(".xlsx")
        || p.ends_with(".xls")
        || p.ends_with(".docx")
        || p.ends_with(".doc")
        || p.ends_with(".pptx")
        || p.ends_with(".pdf")
        || p.ends_with(".zip")
        || p.ends_with(".tar")
        || p.ends_with(".gz")
        || p.ends_with(".png")
        || p.ends_with(".jpg")
        || p.ends_with(".jpeg")
        || p.ends_with(".ico")
        || p.ends_with(".webp")
        || p.ends_with(".mp4")
        || p.ends_with(".mp3")
}

/// 判定路径是否为噪音文件（lockfile、构建产物、依赖库等）
pub fn is_noise_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.contains("package-lock.json")
        || p.contains("pnpm-lock.yaml")
        || p.contains("yarn.lock")
        || p.contains("cargo.lock")
        || p.contains("node_modules/")
        || p.contains("target/")
        || p.contains("dist/")
        || p.contains(".git/")
        || p.contains(".turbo/")
}

/// 过滤 Git Diff 中的噪音文件段落
pub fn filter_diff_noise(raw_diff: &str) -> String {
    let mut result = Vec::new();
    let mut current_chunk = Vec::new();
    let mut is_noisy = false;

    for line in raw_diff.lines() {
        if line.starts_with("diff --git ") {
            if !current_chunk.is_empty() && !is_noisy {
                result.extend(current_chunk);
            }
            current_chunk = vec![line];
            is_noisy = is_noise_path(line);
        } else {
            current_chunk.push(line);
        }
    }
    if !current_chunk.is_empty() && !is_noisy {
        result.extend(current_chunk);
    }
    result.join("\n")
}

fn is_git_repo(workspace: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(workspace)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn git_cmd(workspace: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// 执行终态审查（Reviewer Gatekeeper）
pub async fn evaluate_finish_diff(
    app: &AppHandle,
    db: &DbState,
    context: &SandwichContext,
    model_routing: &ModelRoutingConfig,
    step_id: usize,
    workspace: &Path,
    baseline: &GitBaseline,
    run_started_at: chrono::DateTime<chrono::Utc>,
    session_id: &str,
    interrupt_rx: &mut watch::Receiver<u64>,
    last_reviewed_hash: Option<&str>,
    last_review_result: Option<&ReviewResult>,
    stage_carry_over: &[String],
    host_verification: Option<&str>,
    human_waivers: Option<&str>,
    approved_target_files: Option<&[String]>,
    // 保留参数：未来"控制面自改"机械升级规则将按模式分流（当前不变量由提示词契约承担）
    _plan_mode: Option<crate::agent::checkpoint::PlanExecutionMode>,
) -> ReviewResult {
    let started_at = std::time::Instant::now();

    // 1. 提取审查材料（Git Diff 或 非 Git 修改文件/产物清单）
    let Some((mut review_material, diff_hash)) = baseline.collect_review_material(workspace, context, run_started_at) else {
        return ReviewResult::skipped_empty();
    };

    // 🌟 1.5 范围漂移校验（全模式降级为审查重点提示，不再硬拦）：
    // 排查是发现驱动的，实现阶段扩围是常态——人类审批方案时同样无法预知全部涉改文件，
    // 文件清单硬拦在所有模式下都会误伤必要修复（评测实测）。
    // 真正的不变量由"控制面自改升级"规则承担：改动监管机制自身必须升级人类，与文件清单无关。
    let changed_files = baseline.collect_changed_file_paths(workspace, run_started_at);
    let policy = crate::agent::agent_policy::PolicyConfig::load_from_workspace_or_default(Some(workspace));
    if let Err(scope_err) = crate::agent::agent_policy::verify_scope_drift(
        approved_target_files,
        &changed_files,
        &policy,
    ) {
        review_material.push_str(&format!(
            "\n\n【⚠️ 审查重点：本次修改涉及计划外高危文件，请重点核对其必要性与安全性】\n{}\n（排查中发现必须修改未申报文件属正常扩围，已自动纳入本轮审查范围）\n",
            scope_err
        ));
    }

    // 阶段审查 final_only 记账（carry-over）：最终交付前必须逐条解决的义务
    if !stage_carry_over.is_empty() {
        review_material.push_str("\n\n【上一阶段 final_only 记账（最终交付前必须逐条解决，本阶段审查未覆盖）】\n");
        for line in stage_carry_over {
            review_material.push_str(&format!("- {}\n", line));
        }
    }
    // 宿主实测验证：真实退出码，替代模型自报的"测试通过"
    if let Some(hv) = host_verification {
        review_material.push_str(&format!("\n\n【宿主实测验证（宿主亲自执行，非模型自报）】\n{}\n", hv));
    }

    // 2. 防敷衍拦截：若代码未变 (diff_hash 相同) 且上次审查未通过，
    // 且期间未发生人类人工仲裁特赦（无 human_waivers 介入），直接复用上次拦截结果，避免浪费 Token 重跑
    if let (Some(last_hash), Some(last_res)) = (last_reviewed_hash, last_review_result) {
        let has_active_waiver = human_waivers.map_or(false, |w| !w.trim().is_empty());
        if !last_hash.is_empty() && last_hash == diff_hash && !last_res.is_passed() && !has_active_waiver {
            let mut reused = last_res.clone();
            reused.summary = format!("【复用上次拦截 (Diff 未变)】检测到代码未做任何修复: {}", last_res.summary);
            reused.duration_ms = 0;
            return reused;
        }
    }

    // 3. 解析评审模型与角色人设（要求角色卡明确授权 allows_kind("reviewer")）
    let (model_id, persona_extra) = if let Ok(Some(card)) = resolve_enabled_card(db, "reviewer") {
        if card.allows_kind("reviewer") {
            let mid = if !card.model_id.trim().is_empty() {
                card.model_id.trim().to_string()
            } else {
                AgentRouter::route_for_role(context, model_routing, step_id, roles::REVIEWER).model_id
            };
            (mid, card.persona_prompt)
        } else {
            let decision = AgentRouter::route_for_role(context, model_routing, step_id, roles::REVIEWER);
            (decision.model_id, String::new())
        }
    } else {
        let decision = AgentRouter::route_for_role(context, model_routing, step_id, roles::REVIEWER);
        (decision.model_id, String::new())
    };

    if model_id.trim().is_empty() {
        return ReviewResult::failed_open("未配置有效的 Reviewer 模型", "none", &diff_hash);
    }

    // 3. 广播审查进度通知前端
    let _ = app.emit(
        &format!("agent-progress-{}", session_id),
        json!({
            "type": "action_phase",
            "phase": "reviewing",
            "description": "🧐 专家评审员正在对任务变更与成果进行终审...",
            "diff_hash": diff_hash
        }),
    );

    // 4. 组装 Reviewer 提示词契约
    let system_prompt = format!(
        r#"你是资深架构与代码质量审查专家（Reviewer）。
{}
请对本次任务提交的代码变更、文件修改及交付成果进行严谨客观的终态质量审查。

【核心审查判定原则】
1. 需求与逻辑：任务的核心目标是否已正确实现？是否存在明显的逻辑断层、未处理的边界异常或空指针风险？
2. 架构与稳定性：是否破坏已有功能？是否引入破坏性修改或未清理的临时脏代码？
3. 严格区分问题等级：
   - blocking_issues（严重阻塞性缺陷）：会导致功能不可用、逻辑错误、严重性能衰退或安全漏洞的问题。此类问题会【阻断交付】并打回重修；
   - nit_issues（次要建议）：命名规范、代码格式、轻微可读性或冗余注释等。此类问题【绝不阻断交付】。

【输出格式约束】
你必须且仅输出一个合法的 JSON 对象，不要输出任何多余的 Markdown 标记、解释或无关文本：
{{
  "verdict": "passed" | "rejected",
  "score": 88,
  "blocking_issues": [
    {{
      "file": "发生缺陷的文件相对路径",
      "issue": "具体缺陷现象与原因说明",
      "requirement": "明确具体的修复要求"
    }}
  ],
  "nit_issues": [
    {{
      "file": "文件相对路径",
      "suggestion": "次要改进建议"
    }}
  ],
  "summary": "一句话审查评价与结论"
}}

注：若成果已满足需求且无严重阻塞性缺陷，blocking_issues 必须为空数组 [] 并将 verdict 设为 "passed"。"#,
        if persona_extra.trim().is_empty() {
            ""
        } else {
            &persona_extra
        }
    );

    // 动态拼接目标块的 Checklist、Scope 边界与执行者自述
    let mut block_sections = Vec::new();
    if let Some(block) = context.active_target_block() {
        if !block.checklist.is_empty() {
            let items = block
                .checklist
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{}. {}", i + 1, c))
                .collect::<Vec<_>>()
                .join("\n");
            block_sections.push(format!(
                "【必须逐条核对的验收标准清单 (Checklist)】\n{}\n👉 审查强制要求：你必须对上述清单中的每一项在 Diff 中寻找实际代码证据。若某项未实现或无代码/输出证据支持，必须将其直接判定为 blocking_issue 并打回！绝不允许宽容放行。",
                items
            ));
        }

        if !block.scope.is_empty() {
            block_sections.push(format!(
                "【预定允许改动范围 (Scope)】\n{}\n👉 审查强制要求：仔细核对交付物中是否修改了超出上述范围的文件。顺手修改无关文件（Scope Creep）属于严重纪律缺陷，应提出修复要求。",
                block.scope.join("\n")
            ));
        }

        if let Some(report) = &block.self_report {
            block_sections.push(format!(
                "【执行模型提交的自述 (Self-Report)】\n- 核心改动: {}\n- 选型理由: {}\n- 存疑与隐患点: {}\n👉 审查强制要求：特别针对执行者自述中指出的“存疑与隐患点”，进行高强度的交叉严审！",
                report.what_changed, report.rationale, report.uncertainties
            ));
        }
    }

    if let Some(waivers) = human_waivers {
        let trimmed = waivers.trim();
        if !trimmed.is_empty() {
            block_sections.push(format!(
                "【👑 最高裁决：人类主人已明确批准的特赦与豁免指示 (Human Approved Waivers)】\n{}\n👉 审查强制纪律：人类主人的裁决具有最高权威（高于常规代码审查规则）。若人类已明确知晓并批准保留某处改动（如调试代码、特殊放行或特定业务权衡），你必须严格尊重该裁决，绝对禁止将用户已特赦的改动再次判定为 blocking_issues！其余未被特赦的代码仍须按正常标准严格审查。",
                trimmed
            ));
        }
    }

    let extra_guidance = if block_sections.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", block_sections.join("\n\n"))
    };

    let user_prompt = format!(
        "【任务目标】\n{}\n\n【本次交付的变更材料 / 成果物】\n```\n{}\n```{}\n\n请严格对照判定原则与验收标准输出合法的 JSON 审查结果。",
        context.user_goal,
        review_material,
        extra_guidance
    );

    let messages = vec![
        json!({ "role": "system", "content": system_prompt }),
        json!({ "role": "user", "content": user_prompt }),
    ];

    let model_config = match ModelConfig::from_db_state(db, &model_id) {
        Ok(cfg) => cfg,
        Err(e) => {
            return ReviewResult::failed_open(&format!("加载 Reviewer 模型配置失败: {}", e), &model_id, &diff_hash);
        }
    };

    let mut ignore_stream = |_: llm::StreamEvent<'_>| true;
    let call = llm::stream_with_retry(
        &model_config,
        &messages,
        "auto",
        interrupt_rx,
        1,
        &mut ignore_stream,
    );

    let stream_result = match tokio::time::timeout(Duration::from_secs(REVIEW_TIMEOUT_SECS), call).await {
        Ok(res) => res,
        Err(_) => {
            return ReviewResult::failed_open("Reviewer 审查超时（>35s），已降级放行", &model_id, &diff_hash);
        }
    };

    let duration_ms = started_at.elapsed().as_millis() as u64;

    let response_text = match stream_result {
        Ok(output) => output.full_response,
        Err(e) => {
            return ReviewResult::failed_open(&format!("Reviewer 调用失败: {}", e), &model_id, &diff_hash);
        }
    };

    // 5. 解析 LLM 输出的 JSON
    parse_reviewer_json(&response_text, &model_id, &diff_hash, duration_ms)
}

/// 稳健解析 Reviewer 返回的 JSON
fn parse_reviewer_json(
    raw_text: &str,
    model_id: &str,
    diff_hash: &str,
    duration_ms: u64,
) -> ReviewResult {
    let clean = extract_json_str(raw_text);
    if let Ok(mut parsed) = serde_json::from_str::<ReviewResult>(&clean) {
        parsed.duration_ms = duration_ms;
        parsed.model_id = model_id.to_string();
        parsed.diff_hash = diff_hash.to_string();
        // 依据契约以 blocking_issues 为唯一定论
        if parsed.blocking_issues.is_empty() {
            parsed.verdict = "passed".to_string();
        } else {
            parsed.verdict = "rejected".to_string();
        }
        return parsed;
    }

    // 尝试轻量容错提取
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&clean) {
        let score = v.get("score").and_then(|s| s.as_u64()).unwrap_or(80) as u32;
        let summary = v.get("summary").and_then(|s| s.as_str()).unwrap_or("审查完成").to_string();
        let blocking_issues = v.get("blocking_issues")
            .and_then(|b| serde_json::from_value::<Vec<BlockingIssue>>(b.clone()).ok())
            .unwrap_or_default();
        let nit_issues = v.get("nit_issues")
            .and_then(|n| serde_json::from_value::<Vec<NitIssue>>(n.clone()).ok())
            .unwrap_or_default();
        
        let verdict = if blocking_issues.is_empty() { "passed" } else { "rejected" }.to_string();
        return ReviewResult {
            verdict,
            score,
            blocking_issues,
            nit_issues,
            summary,
            duration_ms,
            model_id: model_id.to_string(),
            diff_hash: diff_hash.to_string(),
        };
    }

    // JSON 解析彻底失败：Fail-Open 避免卡死
    ReviewResult::failed_open("Reviewer 输出非标准 JSON，已降级放行", model_id, diff_hash)
}

fn extract_json_str(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(start) = trimmed.find("```json") {
        if let Some(end) = trimmed[start + 7..].find("```") {
            return trimmed[start + 7..start + 7 + end].trim().to_string();
        }
    }
    if let Some(start) = trimmed.find("```") {
        if let Some(end) = trimmed[start + 3..].find("```") {
            return trimmed[start + 3..start + 3 + end].trim().to_string();
        }
    }
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if start <= end {
            return trimmed[start..=end].to_string();
        }
    }
    trimmed.to_string()
}

/// 机械门禁零 Token 预检（在调用大模型审查前执行）：
/// 检查实际修改的文件是否超出了当前活跃目标块的预定边界 (Scope)。
pub fn mechanical_precheck_violations(
    workspace: &Path,
    active_block: Option<&crate::agent::target_block::TargetBlock>,
) -> Vec<String> {
    let mut violations = Vec::new();
    if let Some(block) = active_block {
        if !block.scope.is_empty() {
            if let Some(files_str) = git_cmd(workspace, &["diff", "--name-only", "HEAD"]) {
                let modified: Vec<String> = files_str
                    .lines()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty() && !is_noise_path(s))
                    .collect();
                let bad = block.find_scope_violations(&modified);
                for file in bad {
                    violations.push(format!(
                        "文件 [{}] 发生改动，但超出了当前目标块 [{}] 的预定允许范围 (Scope: {:?})",
                        file, block.id, block.scope
                    ));
                }
            }
        }
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_noise_path_filter() {
        assert!(is_noise_path("package-lock.json"));
        assert!(is_noise_path("src/dist/bundle.js"));
        assert!(is_noise_path("target/debug/app"));
        assert!(is_noise_path("node_modules/vue/index.js"));
        assert!(!is_noise_path("assets/report.xlsx"));
        assert!(!is_noise_path("exports/data.pdf"));
        assert!(!is_noise_path("src/agent/main_loop.rs"));
        assert!(!is_noise_path("src/components/ModelSelect.vue"));
    }

    #[test]
    fn test_binary_artifact_detection() {
        assert!(is_binary_file("assets/report.xlsx"));
        assert!(is_binary_file("exports/data.pdf"));
        assert!(is_binary_file("assets/logo.png"));
        assert!(is_binary_file("backup.zip"));
        assert!(!is_binary_file("src/main.rs"));
        assert!(!is_binary_file("README.md"));
    }

    #[test]
    fn test_diff_filter() {
        let diff = r#"diff --git a/package-lock.json b/package-lock.json
index 123..456 100644
--- a/package-lock.json
+++ b/package-lock.json
@@ -1,3 +1,3 @@
-1
+2
diff --git a/src/main.rs b/src/main.rs
index 789..abc 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,2 @@
-let a = 1;
+let a = 2;
"#;
        let filtered = filter_diff_noise(diff);
        assert!(!filtered.contains("package-lock.json"));
        assert!(filtered.contains("src/main.rs"));
    }

    #[test]
    fn test_json_parse_and_verdict() {
        let json_input = r#"{
            "verdict": "rejected",
            "score": 75,
            "blocking_issues": [
                {
                    "file": "src/main.rs",
                    "issue": "缺少空指针防御",
                    "requirement": "添加 Option::unwrap_or 默认值"
                }
            ],
            "nit_issues": [
                {
                    "file": "src/main.rs",
                    "suggestion": "变量命名建议更具体"
                }
            ],
            "summary": "存在一处潜在 panic 隐患"
        }"#;

        let res = parse_reviewer_json(json_input, "test-model", "hash123", 500);
        assert_eq!(res.verdict, "rejected");
        assert_eq!(res.blocking_issues.len(), 1);
        assert_eq!(res.nit_issues.len(), 1);
        assert!(!res.is_passed());

        let clean_json = r#"{
            "verdict": "passed",
            "score": 95,
            "blocking_issues": [],
            "nit_issues": [],
            "summary": "代码修改完善"
        }"#;
        let res2 = parse_reviewer_json(clean_json, "test-model", "hash123", 300);
        assert_eq!(res2.verdict, "passed");
        assert!(res2.is_passed());
    }

    #[test]
    fn test_human_waivers_bypass_anti_perfunctory() {
        let last_hash = "same_diff_hash";
        let diff_hash = "same_diff_hash";
        let last_res = ReviewResult {
            verdict: "rejected".to_string(),
            score: 50,
            blocking_issues: vec![BlockingIssue {
                file: "LicenseInterceptor.java".to_string(),
                issue: "后门".to_string(),
                requirement: "还原".to_string(),
            }],
            nit_issues: vec![],
            summary: "存在未修复缺陷".to_string(),
            duration_ms: 100,
            model_id: "test-model".to_string(),
            diff_hash: diff_hash.to_string(),
        };

        // 无豁免时：命中防敷衍拦截 (Diff 未变)
        let human_waivers_none: Option<&str> = None;
        let has_active_waiver = human_waivers_none.map_or(false, |w| !w.trim().is_empty());
        assert!(!has_active_waiver);
        assert!(!last_res.is_passed());
        assert_eq!(last_hash, diff_hash);

        // 有人类特赦豁免时：判定存在特赦，解除防敷衍拦截，允许调用审查员
        let human_waivers_some = Some("【选择项】: 保留本地 if(true) 调试放行，直接交付");
        let has_active_waiver_with_waiver = human_waivers_some.map_or(false, |w| !w.trim().is_empty());
        assert!(has_active_waiver_with_waiver);
    }
}
