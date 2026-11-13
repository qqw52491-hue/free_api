use serde_json::Value;
/// Caller material remains data, never a grant or host-certified fact.
pub(crate) fn ordinary(
    params: &Value,
    role: &str,
    parent_goal: &str,
    investigation_reference: &str,
    parent_tmp: &std::path::Path,
    execution_id: &str,
) -> String {
    let mut material = format!(
        "【子任务目标】{}\n",
        params["task"].as_str().unwrap_or_default()
    );
    if !role.is_empty() {
        material.push_str(&format!("【角色与交付契约】\n{role}\n"));
    }
    for key in ["context", "context_excerpts", "focus_paths"] {
        if let Some(value) = params.get(key).filter(|v| !v.is_null()) {
            let text = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            material.push_str(&format!("【派发方材料：{key}】\n{text}\n"));
        }
    }
    // Explicit inputs are required material, independent of optional focus data.
    if let Some(inputs) = params.get("custom_inputs")
        .and_then(crate::agent::context_collector::render_custom_inputs)
    {
        material.push_str(&inputs);
        material.push('\n');
    }
    if let Some(command) = params["test_command"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
    {
        material.push_str(&format!(
            "【明确要求的验证命令】{command}。未执行不得报告通过。\n"
        ));
    }
    material.push_str(&format!(
        "【宿主提供的父会话证据目录】{}（引用使用绝对路径；你的 tmp/ 独立）。\n",
        parent_tmp.display()
    ));
    let parent_goal = parent_goal.trim();
    if !parent_goal.is_empty() && params["task"].as_str().map(str::trim) != Some(parent_goal) {
        let preview: String = parent_goal.chars().take(1100).collect();
        material.push_str(&format!("【父任务目标（供理解本次委派）】\n{preview}\n"));
        if preview.len() < parent_goal.len() {
            material.push_str(&format!(
                "父任务目标已节选。原文见 child_task.read_agent_result(child_run_id={execution_id}, view=dispatch_input) 的 parent_context.goal。\n"
            ));
        }
    }
    if !investigation_reference.trim().is_empty() {
        material.push_str(&format!("【父任务入口调查参考】\n{investigation_reference}\n"));
    }
    // Explicit inputs and result contracts are never silently discarded to meet
    // a soft budget. Optional collected focus material uses the remaining space.
    if let Some(focus) = params["specialized_context"]
        .as_str()
        .filter(|s| !s.is_empty())
    {
        let source = format!("\n【扩展材料原文入口】child_task.read_agent_result(child_run_id={execution_id}, view=dispatch_input, offset, limit) 的 params.specialized_context；若本执行无读取权限，交父任务回查。\n");
        material.push_str(&source);
        let budget = 16_000usize.saturating_sub(material.chars().count());
        if budget > 100 {
            material.push_str(&super::section("宿主采集的扩展材料", focus, budget, false));
        } else {
            material.push_str("【软预算披露】必要材料已占满预算，已原样保留；扩展材料未内联，可按上述入口回查。\n");
        }
    }
    material
}
