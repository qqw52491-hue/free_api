use crate::agent::change_verification::{change_verification_blocker, ChangeVerification};
use crate::agent::context::types::{FailedAttempt, PlanGateProjection};
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
                        item.evidence_refs
                            .iter()
                            .take(2)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                    "waived" => format!(
                        "豁免: {}",
                        item.waived_reason.as_deref().unwrap_or("未说明")
                    ),
                    _ => "等待核验或显式豁免".to_string(),
                };
                format!(
                    "- [{}] {}：{}（{}）",
                    status, item.id, item.question, detail
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "【审查契约】\n范围：{}\n{}\n终态前：每项必须以 evidenced + evidence_refs 或 waived + waived_reason 闭合。\n\n",
            contract.scope_statement, obligations
        )
    }

    pub fn render_change_verification(
        side_effect_count: usize,
        verification: Option<&ChangeVerification>,
        suggested_cmd: Option<&str>,
    ) -> String {
        if side_effect_count == 0 {
            return String::new();
        }
        if let Some(blocker) = change_verification_blocker(side_effect_count, verification) {
            let mut prompt = format!(
                "【写入后验证】\n\
                - {}\n\
                - 智能感知与主动闭环规则：\n\
                  * 🚀 主动验证授权：代码写入完成后，无需向用户申请许可，请主动调用终端工具运行自测或编译命令；若测试报错，直接依据错误输出就地修复；\n",
                blocker
            );
            if let Some(cmd) = suggested_cmd {
                prompt.push_str(&format!(
                    "  * 💡 系统自适应探测到当前工程推荐验证命令: `{}`，执行成功后系统将自动标记为 verified，无须手动声明；\n",
                    cmd
                ));
            } else {
                prompt.push_str(
                    "  * 若运行了验证命令（如 cargo test / npm test / pytest 等）且成功通过，系统将自动标记为 verified，无须手动声明；\n",
                );
            }
            prompt.push_str(
                "  * 若工程中无可用测试套件，或改动全为纯静态资源（样式/图片/文档等），系统在提交 finish 时将自动豁免 (waived)；\n\
                - 若需在调用 finish 交付时手动声明闭合（在顶层 JSON 中二选一填空，严禁拼入 summary 正文中）：\n\
                  1. 已验证：\"change_verification_update\": {\"status\": \"verified\", \"evidence_refs\": [\"测试步骤或命令输出摘要\"]}\n\
                  2. 免测豁免：\"change_verification_update\": {\"status\": \"waived\", \"waived_reason\": \"说明免测理由，如UI微调/无自动化单测环境\"}\n\n",
            );
            return prompt;
        }
        format!(
            "【写入后验证】\n- 最近写入：{}\n终态前：后续若无新增代码写入，调用 finish 将直接放行；若发生新的真实写入，将重新激活闭合要求。\n\n",
            verification
                .map(ChangeVerification::summary)
                .unwrap_or_default()
        )
    }
    /// 方案门禁状态卡：让模型在「决策前」看到当前模式与放行条件，
    /// 避免靠写入被拒（一整轮 LLM 往返的代价）来反推门禁规则。
    /// 指引文案必须与 loop_runner 的 check_pre_write_gatekeeper 拦截语义一一对应。
    pub fn render_plan_gate(gate: Option<&PlanGateProjection>) -> String {
        let Some(gate) = gate else {
            return String::new();
        };
        let phase_label = match gate.phase.as_str() {
            "exploring" => "探索取证中",
            "reviewing" => "方案审查中",
            "awaiting_approval" => "等待人工审批",
            "approved" => "已批准放行",
            other => other,
        };
        let (mode_label, guidance) = match gate.mode.as_str() {
            "cross_review" => {
                let review_state = if !gate.reviewer_dispatched {
                    "尚未派发"
                } else if gate.reviewer_consumed {
                    "结论已消费"
                } else {
                    "已派发·结论未消费"
                };
                let next = if gate.phase == "approved" {
                    "门禁已放行，可执行写入；新一批写入前的状态变更会反映在本卡。"
                } else if !gate.reviewer_dispatched {
                    "首次写入前必须先派发 child_reviewer 对抗审查（直接写工具将被门禁拦截）。"
                } else if !gate.reviewer_consumed {
                    "审查结论未回收：先 wait_jobs 收取并研判后再写入（问过 ≠ 听过，直接写入将被拦截）。"
                } else {
                    "研判审查结论后即可写入，门禁将自动放行。"
                };
                (
                    format!(
                        "🔍 交叉审查 (cross_review) | 审查: {}（预算 {}/{}）",
                        review_state, gate.reviewer_budget_used, gate.reviewer_budget_max
                    ),
                    next,
                )
            }
            "plan_approval" => {
                let next = if gate.phase == "approved" {
                    "方案已获人类批准，门禁放行，可执行写入。"
                } else if gate.phase == "awaiting_approval" {
                    "方案已呈报，等待人类在界面批准；期间不要调用写工具。"
                } else {
                    "写入前必须 ask_human(kind: plan_approval) 呈报完整方案并获人类批准（直接写工具将被拦截）。"
                };
                ("🛡️ 方案审批 (plan_approval)".to_string(), next)
            }
            _ => {
                let next = if gate.phase == "approved" {
                    "门禁放行中。"
                } else {
                    "完成只读排查与方案阐述后写入将自动放行；未经排查直接写入会被首轮门禁拦截。"
                };
                ("🚀 全自动 (auto)".to_string(), next)
            }
        };
        format!(
            "【方案门禁状态】模式: {} | 方案阶段: {}\n指引: {}\n\n",
            mode_label, phase_label, guidance
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

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(mode: &str, phase: &str, dispatched: bool, consumed: bool) -> PlanGateProjection {
        PlanGateProjection {
            mode: mode.to_string(),
            phase: phase.to_string(),
            reviewer_dispatched: dispatched,
            reviewer_consumed: consumed,
            reviewer_budget_used: if dispatched { 1 } else { 0 },
            reviewer_budget_max: 2,
        }
    }

    #[test]
    fn plan_gate_renders_nothing_when_absent() {
        assert!(ContextRenderer::render_plan_gate(None).is_empty());
    }

    #[test]
    fn cross_review_undispatched_guides_dispatch_before_write() {
        let out = ContextRenderer::render_plan_gate(Some(&gate(
            "cross_review",
            "exploring",
            false,
            false,
        )));
        assert!(out.contains("交叉审查"));
        assert!(out.contains("尚未派发"));
        assert!(out.contains("child_reviewer"), "应指引先派发审查: {out}");
    }

    #[test]
    fn cross_review_dispatched_unconsumed_guides_wait_jobs() {
        let out = ContextRenderer::render_plan_gate(Some(&gate(
            "cross_review",
            "reviewing",
            true,
            false,
        )));
        assert!(out.contains("结论未消费"));
        assert!(out.contains("wait_jobs"), "应指引挂起等待结论: {out}");
    }

    #[test]
    fn cross_review_approved_shows_gate_open() {
        let out = ContextRenderer::render_plan_gate(Some(&gate(
            "cross_review",
            "approved",
            true,
            true,
        )));
        assert!(out.contains("已批准放行"));
        assert!(!out.contains("将被拦截"), "放行后不应再出现拦截警告: {out}");
    }

    #[test]
    fn plan_approval_mode_guides_ask_human() {
        let out = ContextRenderer::render_plan_gate(Some(&gate(
            "plan_approval",
            "exploring",
            false,
            false,
        )));
        assert!(out.contains("方案审批"));
        assert!(out.contains("plan_approval"));
        assert!(out.contains("ask_human"), "应指引呈报审批卡片: {out}");
    }

    #[test]
    fn auto_mode_renders_compact_single_guidance() {
        let out = ContextRenderer::render_plan_gate(Some(&gate("auto", "exploring", false, false)));
        assert!(out.contains("全自动"));
        assert!(out.contains("自动放行"));
    }

    #[test]
    fn render_change_verification_empty_when_no_writes() {
        assert!(ContextRenderer::render_change_verification(0, None, None).is_empty());
    }

    #[test]
    fn render_change_verification_prompts_proactive_authorization_and_inferred_cmd() {
        let out = ContextRenderer::render_change_verification(
            1,
            None,
            Some("npx vue-tsc --noEmit"),
        );
        assert!(out.contains("写入后验证"));
        assert!(out.contains("主动验证授权"));
        assert!(out.contains("无需向用户申请许可"));
        assert!(out.contains("npx vue-tsc --noEmit"));
        assert!(out.contains("自动标记为 verified"));
    }

    #[test]
    fn render_change_verification_shows_resolved_summary() {
        let verified = ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec!["cargo test all pass".to_string()],
            waived_reason: None,
        };
        let out = ContextRenderer::render_change_verification(1, Some(&verified), None);
        assert!(out.contains("已验证：cargo test all pass"));
        assert!(out.contains("直接放行"));
    }
}
