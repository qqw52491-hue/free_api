use crate::agent::execution::runtime::ExecutionConfig;
/// Uses the same alias/action rules as runtime authorization, not a second allowlist.
pub(crate) fn tool(config: &ExecutionConfig, name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();
    let name = normalized.as_str();
    config.permits(name)
        // Action aliases such as send_message map to child_task.send at runtime.
        // Only check business actions here: wait is an always-available control
        // and would expose the index even when no business tools were granted.
        || (name == "child_task"
            && ["send", "ask"]
                .iter()
                .any(|action| config.permits(&format!("{name}.{action}"))))
        || ["read", "list", "write", "edit", "execute"]
            .iter()
            .any(|action| config.permits(&format!("{name}.{action}")))
        || config
            .allowed_tools
            .as_ref()
            .is_some_and(|tools| tools.iter().any(|t| t.starts_with(&format!("{name}."))))
}
pub(crate) fn restricted_menu(config: &ExecutionConfig) -> String {
    if config.allowed_tools.is_none() {
        return "\n【审查权限】使用本次审查材料列明的取证工具及 finish；不提供普通实施或委派工具。"
            .into();
    }
    format!("\n【启动时业务权限快照（每次执行仍由宿主核验）】{}\n仅使用上述精确工具/动作；结果提交和宿主允许的控制动作仍可用。工具发现不扩大权限。",config.allowed_tools.as_ref().map(|t|t.join("、")).unwrap_or_default())
}

/// Metadata for the review adapter's supported tools; runtime still authorizes
/// each call. Zero budget exposes only result submission.
fn reviewer_tools(config: &ExecutionConfig, budget: u32, verify: bool) -> Vec<&'static str> {
    let mut names = vec!["finish"];
    if budget > 0 {
        names.extend([
            "read_file",
            "list_dir",
            "grep_search",
            "git_diff",
            "git_log",
            "run_command",
            "python",
            "get_step_output",
            "read_step_output",
            "search_step_output",
            "list_execution_steps",
            "list_child_runs",
            "read_child_result",
            "evidence_find",
            "evidence_inspect",
            "evidence_compare",
        ]);
        if verify {
            names.push("verify");
        }
    }
    names
        .into_iter()
        .filter(|name| config.permits(name))
        .collect()
}

pub(crate) fn reviewer_index(config: &ExecutionConfig, budget: u32, verify: bool) -> String {
    reviewer_tools(config, budget, verify).join(", ")
}
pub(crate) fn reviewer_menu(config: &ExecutionConfig, budget: u32, verify: bool) -> String {
    let names = reviewer_tools(config, budget, verify);
    let mut menu = format!("\n【当前 Reviewer 可执行工具（配置上限与只读适配器的交集）】{}\n只使用上述工具；执行时仍由宿主检查权限和预算，不提供实施或委派能力。", names.join(", "));
    if names.iter().any(|name| !matches!(*name, "finish" | "verify")) {
        menu.push_str("\n可自行将互不依赖的取证调用放进同一个 {\"parallel\":[{\"tool\":\"工具名\",\"params\":{...}},...]}；每个分支单独校验和计入取证预算。verify 和 finish 单独调用。");
    }
    menu
}
