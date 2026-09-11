//! 智能自适应门禁策略引擎 (Policy as Code).
//!
//! 核心原则：
//! 1. 默认单 Agent 极速体验：纯样式/文案/低危只读走 FastPass 白名单，0 额外 LLM 审查，毫秒级放行；
//! 2. 确定性准入控制：高危文件 (src-tauri/、DB、配置等) 或危险终端命令硬规则拦截，按需自动升级多 Agent 盲审；
//! 3. 策略外置：优先加载工作区根目录下的 `.agent_policy.json`，支持项目级自定义策略；
//! 4. Tauri IPC 敏感调用感知：前端涉及 invoke() 或 @tauri-apps/api 动作不予当作纯 UI 放行；
//! 5. 真实 Git Diff 范围防逃逸 (Scope Drift)：写后校验实际文件改动是否超出 Plan 自报审批范围。

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyConfig {
    #[serde(default = "default_fast_pass_patterns")]
    pub fast_pass_patterns: Vec<String>,
    #[serde(default = "default_high_risk_patterns")]
    pub high_risk_patterns: Vec<String>,
    #[serde(default = "default_dangerous_commands")]
    pub dangerous_commands: Vec<String>,
    #[serde(default = "default_tauri_ipc_sensitive")]
    pub tauri_ipc_sensitive: bool,
    #[serde(default = "default_max_review_rounds")]
    pub max_review_rounds: u32,
}

fn default_fast_pass_patterns() -> Vec<String> {
    vec![
        "*.css".to_string(),
        "*.scss".to_string(),
        "*.sass".to_string(),
        "*.less".to_string(),
        "*.svg".to_string(),
        "docs/**".to_string(),
        "*.md".to_string(),
        "*.txt".to_string(),
    ]
}

fn default_high_risk_patterns() -> Vec<String> {
    vec![
        // 1. 核心密钥与环境变量 (全语言通用)
        ".env*".to_string(),
        "**/.env*".to_string(),
        "**/*secret*".to_string(),
        "**/*credentials*".to_string(),
        // 2. 数据库与数据迁移
        "**/migrations/**".to_string(),
        "**/*.sql".to_string(),
        "schema.prisma".to_string(),
        // 3. 各主流语言的核心构建与依赖清单
        "Cargo.toml".to_string(),
        "Cargo.lock".to_string(),
        "package.json".to_string(),
        "pnpm-lock.yaml".to_string(),
        "requirements.txt".to_string(),
        "pyproject.toml".to_string(),
        "Pipfile".to_string(),
        "go.mod".to_string(),
        "go.sum".to_string(),
        "pom.xml".to_string(),
        "build.gradle*".to_string(),
        // 4. 容器编排与 CI/CD 基础设施
        "Dockerfile*".to_string(),
        "docker-compose*.yml".to_string(),
        "**/.github/workflows/**".to_string(),
        // 5. 权限与安全模块
        "**/auth/**".to_string(),
        "**/permissions/**".to_string(),
        "**/security/**".to_string(),
        // 6. 本地原生核心目录（若存在）
        "src-tauri/**".to_string(),
        "tauri.conf.json".to_string(),
    ]
}

fn default_dangerous_commands() -> Vec<String> {
    vec![
        "rm -rf".to_string(),
        "sudo".to_string(),
        "git reset --hard".to_string(),
        "drop database".to_string(),
        "chmod 777".to_string(),
        "mkfs".to_string(),
        ":(){ :|:& };:".to_string(),
    ]
}

fn default_tauri_ipc_sensitive() -> bool {
    true
}

fn default_max_review_rounds() -> u32 {
    3
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            fast_pass_patterns: default_fast_pass_patterns(),
            high_risk_patterns: default_high_risk_patterns(),
            dangerous_commands: default_dangerous_commands(),
            tauri_ipc_sensitive: default_tauri_ipc_sensitive(),
            max_review_rounds: default_max_review_rounds(),
        }
    }
}

impl PolicyConfig {
    /// 尝试从工作区根目录读取 `.agent_policy.json`，若不存在或损坏则返回安全默认配置
    pub fn load_from_workspace_or_default(workspace_dir: Option<&Path>) -> Self {
        let Some(ws) = workspace_dir else {
            return Self::default();
        };
        let policy_file = ws.join(".agent_policy.json");
        if policy_file.is_file() {
            if let Ok(content) = std::fs::read_to_string(&policy_file) {
                if let Ok(cfg) = serde_json::from_str::<Self>(&content) {
                    return cfg;
                }
            }
        }
        Self::default()
    }
}

/// 策略评估决策结果
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyDecision {
    /// 🚀 低危极速通过：无高危特征，0 额外 LLM 调用，直接放行
    FastPass { reason: String },
    /// 🔍 自动升级审查：命中高危路径、敏感命令或 Tauri IPC 调用，必须派发专家盲审
    EscalateToReview {
        reason: String,
        matched_pattern: String,
        suggested_roles: Vec<String>,
    },
    /// 🛑 命中绝对黑名单破坏性操作，直接阻断
    Block { reason: String },
}

impl PolicyDecision {
    pub fn is_fast_pass(&self) -> bool {
        matches!(self, Self::FastPass { .. })
    }

    pub fn summary(&self) -> String {
        match self {
            Self::FastPass { reason } => format!("【极速放行】{}", reason),
            Self::EscalateToReview {
                reason,
                matched_pattern,
                suggested_roles,
            } => format!(
                "【按需升级审查】{} (命中: {}, 建议专家: {})",
                reason,
                matched_pattern,
                suggested_roles.join(", ")
            ),
            Self::Block { reason } => format!("【阻断执行】{}", reason),
        }
    }
}

/// 简单的通配符路径匹配器（无需引入外部重型正则/glob 依赖）
pub fn path_matches_pattern(path: &str, pattern: &str) -> bool {
    let norm_path = path.trim().replace('\\', "/");
    let norm_pattern = pattern.trim().replace('\\', "/");

    if norm_pattern.is_empty() || norm_path.is_empty() {
        return false;
    }

    // 0. 精准一致
    if norm_path == norm_pattern {
        return true;
    }

    // 1. 任意位置目录匹配 (**/dir/**)
    if let Some(mid) = norm_pattern.strip_prefix("**/").and_then(|s| s.strip_suffix("/**")) {
        return norm_path.contains(&format!("/{}/", mid))
            || norm_path.starts_with(&format!("{}/", mid));
    }

    // 2. 任意子目录后缀或文件名匹配 (**/filename or **/*.ext)
    if let Some(sub) = norm_pattern.strip_prefix("**/") {
        if let Some(ext) = sub.strip_prefix("*.") {
            return norm_path.ends_with(&format!(".{}", ext));
        }
        return norm_path.ends_with(&format!("/{}", sub)) || norm_path == sub;
    }

    // 3. 全局后缀匹配 (*.ext)
    if let Some(ext) = norm_pattern.strip_prefix("*.") {
        return norm_path.ends_with(&format!(".{}", ext));
    }

    // 4. 前缀递归匹配 (dir/** 或 dir/*)
    if let Some(prefix) = norm_pattern.strip_suffix("/**") {
        return norm_path.starts_with(&format!("{}/", prefix)) || norm_path == prefix;
    }
    if let Some(prefix) = norm_pattern.strip_suffix("/*") {
        if norm_path.starts_with(&format!("{}/", prefix)) {
            let rest = &norm_path[prefix.len() + 1..];
            return !rest.contains('/');
        }
        return false;
    }

    false
}

/// 评估 Plan 及其自报特征
pub fn evaluate_plan_policy(
    policy: &PolicyConfig,
    target_files: &[String],
    commands: &[String],
    plan_text: &str,
) -> PolicyDecision {
    // 1. 检查危险命令黑名单（绝对优先级）
    for cmd in commands {
        let cmd_lower = cmd.to_ascii_lowercase();
        for dang in &policy.dangerous_commands {
            if cmd_lower.contains(&dang.to_ascii_lowercase()) {
                return PolicyDecision::Block {
                    reason: format!("检测到高危毁灭性命令: `{}`", dang),
                };
            }
        }
    }

    // 2. 检查是否有终端命令需要执行
    if !commands.is_empty() {
        // 只要 Plan 中明确包含了终端执行命令，保守起见自动升级审查
        return PolicyDecision::EscalateToReview {
            reason: format!("Plan 包含 {} 条待执行终端命令，需确认安全性与执行目的", commands.len()),
            matched_pattern: "action:run_command".to_string(),
            suggested_roles: vec!["security".to_string(), "architecture".to_string()],
        };
    }

    // 3. 检查自报的目标文件列表
    if target_files.is_empty() {
        // 未声明任何目标文件（可能是只读排查、纯文本回复、或宏观方案阶段）
        return PolicyDecision::FastPass {
            reason: "只读排查或未改动任何本地文件".to_string(),
        };
    }

    // 3.1 检查是否触碰高危路径规则
    for file in target_files {
        for pattern in &policy.high_risk_patterns {
            if path_matches_pattern(file, pattern) {
                return PolicyDecision::EscalateToReview {
                    reason: format!("目标修改文件 `{}` 属于核心高危范围", file),
                    matched_pattern: pattern.clone(),
                    suggested_roles: vec!["security".to_string(), "architecture".to_string()],
                };
            }
        }
    }

    // 3.2 检查前端文件中的 Tauri IPC 敏感调用（invoke / @tauri-apps/api）
    if policy.tauri_ipc_sensitive {
        let touches_frontend_script = target_files.iter().any(|f| {
            let p = f.to_ascii_lowercase();
            p.ends_with(".vue")
                || p.ends_with(".ts")
                || p.ends_with(".js")
                || p.ends_with(".jsx")
                || p.ends_with(".tsx")
        });

        if touches_frontend_script {
            let plan_lower = plan_text.to_ascii_lowercase();
            let has_ipc_intent = plan_lower.contains("invoke(")
                || plan_lower.contains("@tauri-apps/api")
                || plan_lower.contains("tauri::command")
                || plan_lower.contains("tauri_command")
                || plan_lower.contains("调用系统底层")
                || plan_lower.contains("执行系统命令");

            if has_ipc_intent {
                return PolicyDecision::EscalateToReview {
                    reason: "前端修改涉及 Tauri IPC (invoke/native bridge) 跨边界调用，存在越权逃逸风险".to_string(),
                    matched_pattern: "tauri_ipc_bridge".to_string(),
                    suggested_roles: vec!["security".to_string()],
                };
            }
        }
    }

    // 3.3 检查是否全部位于极窄白名单中
    let all_in_fast_pass = target_files.iter().all(|file| {
        policy
            .fast_pass_patterns
            .iter()
            .any(|pattern| path_matches_pattern(file, pattern))
    });

    if all_in_fast_pass {
        return PolicyDecision::FastPass {
            reason: format!(
                "涉及的全部 {} 个目标文件均属于低危静态资源/文档白名单",
                target_files.len()
            ),
        };
    }

    // 3.4 既不在高危黑名单，也不在极窄白名单的未知文件（如常规业务代码变更）：
    // 保守原则：默认升级单 Reviewer 审查，确保代码健壮性
    PolicyDecision::EscalateToReview {
        reason: format!(
            "改动包含 {} 个未知/业务逻辑文件，需专家审查",
            target_files.len()
        ),
        matched_pattern: "non_whitelisted_file".to_string(),
        suggested_roles: vec!["architecture".to_string()],
    }
}

/// 补丁 1：写后真实 Git Diff 范围防逃逸校验
///
/// 当主 Agent 在 Plan 阶段自报了目标文件白名单后，写盘完成后比对实际的改动文件清单。
/// 若实际改动了未经批准的高危文件，立即判定越权逃逸！
pub fn verify_scope_drift(
    approved_files: Option<&[String]>,
    actual_changed_files: &[String],
    policy: &PolicyConfig,
) -> Result<(), String> {
    let Some(approved) = approved_files else {
        // 未显式约束白名单（如自由探索或未报备阶段），跳过范围漂移检查
        return Ok(());
    };

    if approved.is_empty() {
        return Ok(());
    }

    let mut unapproved_high_risk = Vec::new();

    for actual in actual_changed_files {
        let is_approved = approved.iter().any(|app| {
            actual == app
                || actual.ends_with(app)
                || app.ends_with(actual)
                || path_matches_pattern(actual, app)
        });

        if !is_approved {
            // 检查未审批文件是否属于高危路径
            let is_high_risk = policy
                .high_risk_patterns
                .iter()
                .any(|pattern| path_matches_pattern(actual, pattern));

            if is_high_risk {
                unapproved_high_risk.push(actual.clone());
            }
        }
    }

    if !unapproved_high_risk.is_empty() {
        return Err(format!(
            "【越权逃逸拦截】检测到实际代码改动了未经 Plan 审批的高危文件: {:?}！Plan 中仅申报了: {:?}。禁止暗度陈仓！",
            unapproved_high_risk, approved
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_matching_rules() {
        assert!(path_matches_pattern("src/style/main.css", "*.css"));
        assert!(path_matches_pattern("docs/readme.md", "*.md"));
        assert!(path_matches_pattern("docs/api/guide.md", "docs/**"));
        assert!(path_matches_pattern("src-tauri/src/main.rs", "src-tauri/**"));
        assert!(path_matches_pattern("src/db/migrations/001.sql", "**/migrations/**"));
        assert!(path_matches_pattern("src/db/migrations/001.sql", "**/*.sql"));
        assert!(path_matches_pattern("tauri.conf.json", "tauri.conf.json"));

        assert!(!path_matches_pattern("src/App.vue", "*.css"));
        assert!(!path_matches_pattern("src/main.rs", "src-tauri/**"));
    }

    #[test]
    fn test_fast_pass_evaluation() {
        let policy = PolicyConfig::default();
        let files = vec!["src/styles/btn.css".to_string(), "docs/intro.md".to_string()];
        let commands = vec![];
        let decision = evaluate_plan_policy(&policy, &files, &commands, "修改按钮样式");
        assert!(decision.is_fast_pass());
    }

    #[test]
    fn test_high_risk_file_escalates() {
        let policy = PolicyConfig::default();
        let files = vec!["src-tauri/src/main_loop.rs".to_string()];
        let commands = vec![];
        let decision = evaluate_plan_policy(&policy, &files, &commands, "修改主循环");
        assert!(matches!(decision, PolicyDecision::EscalateToReview { .. }));
    }

    #[test]
    fn test_dangerous_command_blocked() {
        let policy = PolicyConfig::default();
        let files = vec![];
        let commands = vec!["rm -rf /tmp/test".to_string()];
        let decision = evaluate_plan_policy(&policy, &files, &commands, "清理临时文件");
        assert!(matches!(decision, PolicyDecision::Block { .. }));
    }

    #[test]
    fn test_tauri_ipc_sensitive_escalates() {
        let policy = PolicyConfig::default();
        let files = vec!["src/components/MyButton.vue".to_string()];
        let commands = vec![];
        let decision = evaluate_plan_policy(
            &policy,
            &files,
            &commands,
            "在组件中通过 invoke('delete_all') 调用底层原生清理",
        );
        assert!(matches!(decision, PolicyDecision::EscalateToReview { .. }));
    }

    #[test]
    fn test_scope_drift_detection() {
        let policy = PolicyConfig::default();
        let approved = vec!["src/components/Button.vue".to_string()];

        // 正常改动申报的文件 -> 通过
        let actual_clean = vec!["src/components/Button.vue".to_string()];
        assert!(verify_scope_drift(Some(&approved), &actual_clean, &policy).is_ok());

        // 实际偷偷动了高危的 src-tauri/src/main.rs -> 必须精准拦截！
        let actual_sneaky = vec![
            "src/components/Button.vue".to_string(),
            "src-tauri/src/main.rs".to_string(),
        ];
        let err = verify_scope_drift(Some(&approved), &actual_sneaky, &policy).unwrap_err();
        assert!(err.contains("越权逃逸拦截"));
        assert!(err.contains("src-tauri/src/main.rs"));
    }
}
