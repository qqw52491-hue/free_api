use crate::agent::context::types::FailedAttempt;
use crate::agent::review_contract::ReviewContract;
use crate::agent::task_store::TaskResourceRef;
use crate::agent::types::TodoItem;

pub struct ContextRenderer;

impl ContextRenderer {
    pub fn render_failed_attempts(attempts: &[FailedAttempt]) -> String {
        if attempts.is_empty() {
            return String::new();
        }

        let mut lines = Vec::new();
        for a in attempts {
            let out_ref = a
                .output_id
                .as_deref()
                .map(|id| format!(" (详情见 output_id={})", id))
                .unwrap_or_default();
            lines.push(format!(
                "- [Step {} 避坑] 工具: {} · 操作: {} · 失败原因: {}{}",
                a.step_id, a.tool, a.action, a.error_summary, out_ref
            ));
        }

        format!(
            "【历史已试过且失败的方案 (避坑指南)】\n(⚠️系统提示：以下方案经实测不可行，绝对禁止在相同参数下重复重试！)\n{}\n\n",
            lines.join("\n")
        )
    }

    pub fn render_task_resources(resources: &[TaskResourceRef]) -> String {
        if resources.is_empty() {
            return String::new();
        }

        let lines: Vec<String> = resources
            .iter()
            .take(20)
            .map(|r| {
                let summary: String = r.summary.chars().take(240).collect();
                format!(
                    "- id={} | kind={} | label={} | rows={} | status={} | summary={}",
                    r.resource_id, r.kind, r.label, r.row_count, r.status, summary
                )
            })
            .collect();

        format!(
            "【会话任务数据区目录 (TaskStore)】\n(💡提示：详细数据已在外部 SQLite 存储，使用 task_store 工具按 resource_id 分页查询/追加)\n{}\n\n",
            lines.join("\n")
        )
    }

    pub fn render_todo_list(todos: &[TodoItem]) -> String {
        if todos.is_empty() {
            return "【当前任务面板 (Todo List)】\n[]\n\n".to_string();
        }

        let json_str = serde_json::to_string_pretty(todos).unwrap_or_else(|_| "[]".to_string());
        format!("【当前任务面板 (Todo List)】\n{}\n\n", json_str)
    }

    pub fn render_review_contract(contract: Option<&ReviewContract>) -> String {
        let Some(contract) = contract else {
            return String::new();
        };
        let obligations = contract
            .obligations
            .iter()
            .map(|item| {
                let status = item.status.trim().to_ascii_lowercase();
                let detail = match status.as_str() {
                    "evidenced" => format!(
                        "证据: {}",
                        item.evidence_refs.iter().take(2).cloned().collect::<Vec<_>>().join("; ")
                    ),
                    "waived" => format!(
                        "豁免: {}",
                        item.waived_reason.as_deref().unwrap_or("未说明")
                    ),
                    _ => "等待核验或显式豁免".to_string(),
                };
                format!("- [{}] {}：{}（{}）", status, item.id, item.question, detail)
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "【审查契约】\n范围：{}\n{}\n终态前：每项必须以 evidenced + evidence_refs 或 waived + waived_reason 闭合。\n\n",
            contract.scope_statement, obligations
        )
    }

    pub fn render_decision_instruction(compact_tools: Option<&str>) -> String {
        if let Some(tools) = compact_tools.filter(|s| !s.trim().is_empty()) {
            format!(
                "【当前可用工具快速索引】(只能从以下调用，严禁臆造 core/bash 等未声明工具)\n{}\n\n请基于上述上下文执行下一步。只能输出纯文本 JSON（禁止裹 ```），tool 名必须严格来自可用工具清单。在 description 字段给出面向用户的一句话进度；仅当上一步失败或被拦截时，才在 reflection 中补一句原因与改法。",
                tools
            )
        } else {
            "请基于上述上下文执行下一步。只能输出纯文本 JSON（禁止裹 ```），tool 名必须严格来自上方工具菜单。在 description 字段给出面向用户的一句话进度；仅当上一步失败或被拦截时，才在 reflection 中补一句原因与改法。".to_string()
        }
    }
}
