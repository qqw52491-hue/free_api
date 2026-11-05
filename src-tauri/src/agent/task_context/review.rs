use super::section;
use serde_json::Value;
pub(crate) const REVIEW_PROTOCOL: &str = "\n【当前只读审查执行协议】本次仅取证并交回审查结果，不实施修改、不派发子任务、不启动嵌套审查。以上审查格式是结果字段契约，工具调用使用 {\"tool\":\"工具名\",\"params\":{...}}；最终使用 {\"tool\":\"finish\",\"params\":{\"summary\":\"审查完成说明\",...原审查结果字段}} 提交，不直接返回无动作的结果 JSON。权限由宿主执行。完整历史可用 list_execution_steps/list_child_runs/read_child_result 分页导航，get_step_output/read_step_output(output_id,offset,limit,可选run_id) 读取；目录、描述和自述不是验证事实。";
pub(crate) fn review_system(messages: &[Value]) -> String {
    let mut text = messages
        .iter()
        .filter(|m| m["role"] == "system")
        .filter_map(|m| m["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    text.push_str(REVIEW_PROTOCOL);
    text
}
pub(crate) fn review_goal(messages: &[Value]) -> String {
    let mut text = String::new();
    for m in messages.iter().filter(|m| m["role"] != "system") {
        let content = m["content"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| m["content"].to_string());
        text.push_str(&format!(
            "【审查材料/历史：{}】\n{content}\n",
            m["role"].as_str().unwrap_or("unknown")
        ));
    }
    text.push_str(REVIEW_PROTOCOL);
    text
}
/// Total stays below the existing 6000-character transport limit, including notices.
pub(crate) fn stage(
    goal: &str,
    todos: &str,
    extras: &[String],
    focus: Option<&str>,
    diff: &str,
) -> String {
    let placeholder = "尚未到达阶段的预生成存根不构成当前阻塞；按当前阶段目标审查。";
    let mut blocks = vec![
        ("任务目标".to_string(), goal, 1200, false),
        ("阶段计划（中间态，非最终形态）".into(), todos, 900, false),
        ("未来阶段占位存根".into(), placeholder, 100, false),
    ];
    let per_extra = 2000 / extras.len().max(1);
    blocks.extend(extras.iter().enumerate().map(|(i, s)| {
        (
            format!("执行证据目录/补充材料 {}", i + 1),
            s.as_str(),
            per_extra,
            false,
        )
    }));
    blocks.push((
        "审查专家专项关注材料".into(),
        focus.unwrap_or("（无）"),
        500,
        false,
    ));
    blocks.push((
        "当前累计变更 diff（完整内容用 git_diff 回查）".into(),
        diff,
        1100,
        true,
    ));
    let demands: Vec<_> = blocks
        .iter()
        .map(|(title, body, _, _)| title.chars().count() + body.chars().count() + 4)
        .collect();
    let reserved: Vec<_> = blocks.iter().map(|(_, _, floor, _)| *floor).collect();
    let allocated = super::budget::shares(&demands, &reserved, 6000);
    blocks
        .iter()
        .zip(allocated)
        .map(|((title, body, _, tail), n)| section(title, body, n, *tail))
        .collect()
}

/// Public project rules survive adapter selection. Presentation uses the same
/// effective review capability set as the compact index, never the raw ceiling.
pub(crate) fn review_entry(
    messages: &[Value],
    workspace_rules: &str,
    custom_rules: &str,
    config: &crate::agent::execution::runtime::ExecutionConfig,
    budget: u32,
    verify: bool,
) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        review_system(messages),
        workspace_rules,
        custom_rules,
        super::visibility::reviewer_menu(config, budget, verify)
    )
}
