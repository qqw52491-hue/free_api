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

    /// 提取相对起点的过滤后代码变更 Diff
    pub fn collect_diff(&self, workspace: &Path) -> Option<(String, String)> {
        if !self.is_git {
            return None;
        }

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
                    if let Ok(content) = std::fs::read_to_string(&full_path) {
                        let sample = if content.chars().count() > 1000 {
                            format!("{}...\n[文件过长截断]", content.chars().take(1000).collect::<String>())
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
}

/// 判定路径是否为噪音文件（lockfile、构建产物、依赖库、媒体资源等）
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
        || p.ends_with(".png")
        || p.ends_with(".jpg")
        || p.ends_with(".jpeg")
        || p.ends_with(".svg")
        || p.ends_with(".ico")
        || p.ends_with(".woff")
        || p.ends_with(".woff2")
        || p.ends_with(".ttf")
        || p.ends_with(".pdf")
        || p.ends_with(".zip")
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
    session_id: &str,
    interrupt_rx: &mut watch::Receiver<u64>,
) -> ReviewResult {
    let started_at = std::time::Instant::now();

    // 1. 提取 Diff
    let Some((diff_content, diff_hash)) = baseline.collect_diff(workspace) else {
        return ReviewResult::skipped_empty();
    };

    // 2. 解析评审模型与角色人设
    let (model_id, persona_extra) = if let Ok(Some(card)) = resolve_enabled_card(db, "reviewer") {
        let mid = if !card.model_id.trim().is_empty() {
            card.model_id.trim().to_string()
        } else {
            AgentRouter::route_for_role(context, model_routing, step_id, roles::REVIEWER).model_id
        };
        (mid, card.persona_prompt)
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
            "description": "🧐 专家评审员正在对代码变更进行终审...",
            "diff_hash": diff_hash
        }),
    );

    // 4. 组装 Reviewer 提示词契约
    let system_prompt = format!(
        r#"你是资深架构与代码质量审查专家（Reviewer）。
{}
请对本次任务提交的代码变更（Git Diff）及任务目标进行严谨客观的终态质量审查。

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

注：若代码已满足需求且无严重阻塞性缺陷，blocking_issues 必须为空数组 [] 并将 verdict 设为 "passed"。"#,
        if persona_extra.trim().is_empty() {
            ""
        } else {
            &persona_extra
        }
    );

    let user_prompt = format!(
        "【任务目标】\n{}\n\n【本次交付的代码变更 Diff】\n```diff\n{}\n```\n\n请依据审查判定原则进行审查并输出严格的 JSON 结果。",
        context.user_goal,
        diff_content
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_noise_path_filter() {
        assert!(is_noise_path("package-lock.json"));
        assert!(is_noise_path("src/dist/bundle.js"));
        assert!(is_noise_path("target/debug/app"));
        assert!(is_noise_path("assets/logo.png"));
        assert!(!is_noise_path("src/agent/main_loop.rs"));
        assert!(!is_noise_path("src/components/ModelSelect.vue"));
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
}
