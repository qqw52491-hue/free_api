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
            "【审查契约】\n范围：{}\n{}\n终态前：每项必须以 evidenced + evidence_refs 或 waived + waived_reason 闭合。evidence_refs 形态随证据域走：代码任务用 `文件:行号`（须落入真实 diff），非代码任务（运维/系统/外部产物）可用 `step_N`、命令+输出摘要等可核验观测（机械行号门将不适用跳过，终审按语义证据核验）；若义务本质是\"命令可通过\"，可给该项挂 `verify_command` 由宿主亲自执行判分（构造 exit 0=义务成立），无需行号证据。\n\n",
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
                "  * 非编码任务（运维/系统/文件外产物）的验证手段自选：which/test -e/pacman -Q/systemctl status/curl -f 等状态探针成功即可——但探针命令必须引用本次改动的具体对象（路径/名称），无关探针不自动认账；\n\
                  * 存在性探针返回非零（找不到/不存在）若正是目标状态（卸载/删除/禁用类），请用下方手动声明 verified + evidence_refs=[\"step_N\"] 举证；\n",
            );
            prompt.push_str(
                "  * 若工程中无可用测试套件，或改动全为纯静态资源（样式/图片/文档等），系统在提交 finish 时将自动豁免 (waived)；\n\
                - 若需在调用 finish 交付时手动声明闭合（在顶层 JSON 中二选一填空，严禁拼入 summary 正文中）：\n\
                  1. 已验证：\"change_verification_update\": {\"status\": \"verified\", \"evidence_refs\": [\"step_N 或 命令输出摘要\"]}\n\
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
    /// 方案门禁状态卡：让模型在「决策前」看到当前模式与放行条件，
    /// 避免靠写入被拒（一整轮 LLM 往返的代价）来反推门禁规则。
    /// 【按需条件化渲染原则】：每轮仅命中唯一的激活状态分支，严格控体积（单状态 1~3 行），稳吃前缀缓存。
    pub fn render_plan_gate(gate: Option<&PlanGateProjection>) -> String {
        let Some(gate) = gate else {
            return String::new();
        };

        // 1. 已批准放行分支 (Approved)
        if gate.phase == "approved" {
            let waiver_hint = if gate.human_waived {
                "（已获人工特批放行）"
            } else {
                ""
            };
            return format!(
                "【方案门禁状态】🟢 门禁已放行{}\n指引: 门禁放行中，写入工具已全面解锁，可按方案执行写入并验证；新一批写入前的状态变更会反映在本卡。\n\n",
                waiver_hint
            );
        }

        // 2. 方案审批模式分支 (plan_approval)
        if gate.mode == "plan_approval" {
            let guidance = if gate.phase == "awaiting_approval" {
                "方案已呈报，等待人类在界面批准；期间不要调用写工具。"
            } else {
                "写入前必须 ask_human(kind: plan_approval) 呈报完整方案并获人类批准（直接写工具将被拦截）。"
            };
            return format!(
                "【方案门禁状态】🛡️ 方案审批 (plan_approval)\n指引: {}\n\n",
                guidance
            );
        }

        // 3. 全自动模式分支 (auto)
        if gate.mode != "cross_review" {
            return "【方案门禁状态】🚀 全自动 (auto)\n指引: 完成只读排查与方案阐述后写入将自动放行；未经排查直接写入会被首轮门禁拦截。\n\n".to_string();
        }

        // 4. 交叉审查模式分支 (cross_review) —— 细分且互斥的单一活跃状态
        // 4.1 尚未派发审查员 —— 按探索预算分三态（正常 / 收敛区软提醒 / 硬上限预告）
        if !gate.reviewer_dispatched {
            // 硬上限已触达：与门禁拦截文案保持一致，预先说明剩余逃生通道
            if gate.exploration_hard_limit > 0 && gate.exploration_steps >= gate.exploration_hard_limit
            {
                let extension_hint = if gate.exploration_extensions_left > 0 {
                    format!(
                        "或在指令中附 request_exploration_extension 申请延期（剩余 {} 次，需 unresolved_question + next_queries）",
                        gate.exploration_extensions_left
                    )
                } else {
                    "探索延期额度已用完".to_string()
                };
                return format!(
                    "【方案门禁状态】⛔ 交叉审查 (cross_review) | 探索预算已耗尽（步数 {}/{}）\n指引: 普通工具调用将被门禁拒绝；请立即输出 submit_plan 提交结构化方案（宿主自动派发 Tech Lead）或调用 child_reviewer，{}，或调用 ask_human 说明缺失信息。\n\n",
                    gate.exploration_steps, gate.exploration_hard_limit, extension_hint
                );
            }
            // 软提醒区：不拦截，只引导收敛（模型仍保有继续取证或立即派审的决定权）
            if gate.exploration_soft_limit > 0 && gate.exploration_steps >= gate.exploration_soft_limit
            {
                return format!(
                    "【方案门禁状态】🔍 交叉审查 (cross_review) | 审查: 尚未派发（预算 {}/{}）| 探索收敛区（步数 {}/硬上限 {}）\n指引: 请停止扩散搜索，汇总根因、目标文件与风险后输出 submit_plan 提交方案（宿主将自动派发 Tech Lead）；证据确实不足可在指令中附 request_exploration_extension 申请延期（剩余 {} 次，需 unresolved_question + next_queries）。\n\n",
                    gate.reviewer_budget_used, gate.reviewer_budget_max,
                    gate.exploration_steps, gate.exploration_hard_limit,
                    gate.exploration_extensions_left
                );
            }
            return format!(
                "【方案门禁状态】🔍 交叉审查 (cross_review) | 审查: 尚未派发（预算 {}/{}）\n指引: 首次写入前必须先完成方案审查：证据成熟后输出 submit_plan 提交结构化方案（宿主自动派发 Tech Lead），或调用 child_reviewer 手动派发；直接写工具将被门禁拦截。\n\n",
                gate.reviewer_budget_used, gate.reviewer_budget_max
            );
        }

        // 4.2 并发审查推演中尚未齐套（齐套屏障生效中）
        if gate.batch_incomplete || !gate.reviewer_consumed {
            let delivered = gate.delivered_reviewers;
            let expected = if gate.expected_reviewers > 0 { gate.expected_reviewers } else { 1 };
            return format!(
                "【方案门禁状态】⏳ 交叉审查 (cross_review) | 审查: 结论未消费（推演中 {}/{}）\n指引: 审查结论未回收：先 wait_jobs 收取并研判后再写入（问过 ≠ 听过，直接写入将被拦截）。\n\n",
                delivered, expected
            );
        }

        // 4.3 存在未闭合的 Blocking 阻塞缺陷（红色物理封锁态）
        if gate.blocking_count > 0 {
            let summary_line = if let Some(summary) = &gate.blocking_summary {
                format!("\n待闭合缺陷: {}", summary)
            } else {
                String::new()
            };
            let escape = if gate.budget_exhausted {
                "审查预算已耗尽，请立即调用 ask_human(kind: 'plan_approval') 说明情况并申请人工裁决放行。"
            } else {
                "针对异议完善方案并重新派发 child_reviewer 确认闭合；若审查员意见过度防御，可调用 ask_human(kind: 'plan_approval') 呈报人工仲裁放行。"
            };
            return format!(
                "【方案门禁状态】🔴 门禁封锁中（存在 {} 条未闭合 Blocking 致命缺陷）{}\n指引: 写入工具已被物理封锁，任何写入调用将被门禁当场拦截并丢弃！当前必须：{}\n\n",
                gate.blocking_count, summary_line, escape
            );
        }

        // 4.4 审查预算耗尽（达最大重试上限）且未放行
        if gate.budget_exhausted {
            return format!(
                "【方案门禁状态】⚠️ 审查预算已耗尽 ({}/{})\n指引: 审查轮数已达上限，禁止继续派发审查子 Agent。请调用 ask_human(kind: 'plan_approval') 呈报方案申请人工审批放行。\n\n",
                gate.reviewer_budget_used, gate.reviewer_budget_max
            );
        }

        // 4.5 审查结论已回收且无 Blocking 缺陷（等待门禁最终校验放行）
        format!(
            "【方案门禁状态】🔍 交叉审查 (cross_review) | 审查: 结论已消费（预算 {}/{}）\n指引: 审查结论已研判闭合（无 Blocking 缺陷）；请输出完善后的技术方案，门禁核验通过后放行写入。\n\n",
            gate.reviewer_budget_used, gate.reviewer_budget_max
        )
    }

    pub fn render_decision_instruction(compact_tools: Option<&str>) -> String {
        if let Some(tools) = compact_tools.filter(|s| !s.trim().is_empty()) {
            format!(
                "【当前可用工具快速索引】(只能从以下调用，严禁臆造 core/bash 等未声明工具)\n{}\n\n请基于上述上下文执行下一步。只能输出纯文本 JSON（禁止裹 ```），tool 名必须严格来自可用工具清单。thought 字段默认留空——仅当进入新工作阶段(plan_title 切换)、拿到改变判断的新证据/新发现、关键突破或里程碑、遇阻需要向用户说明、任务收官小结时，才写 1~3 句面向用户的解说（第一人称、不复述 description）；常规连续步骤严禁填写；description 字段保持一句话进度标签；仅当上一步失败或被拦截时在 reflection 补一句原因与改法；本轮若取得阶段突破、关键结论、里程碑进展或发现风险阻碍，额外输出 finding(1~2 句结论) 与 finding_kind(start|breakthrough|milestone|summary|warning|blocker)。",
                tools
            )
        } else {
            "请基于上述上下文执行下一步。只能输出纯文本 JSON（禁止裹 ```），tool 名必须严格来自上方工具菜单。thought 字段默认留空——仅当进入新工作阶段(plan_title 切换)、拿到改变判断的新证据/新发现、关键突破或里程碑、遇阻需要向用户说明、任务收官小结时，才写 1~3 句面向用户的解说（第一人称、不复述 description）；常规连续步骤严禁填写；description 字段保持一句话进度标签；仅当上一步失败或被拦截时在 reflection 补一句原因与改法；本轮若取得阶段突破、关键结论、里程碑进展或发现风险阻碍，额外输出 finding(1~2 句结论) 与 finding_kind(start|breakthrough|milestone|summary|warning|blocker)。".to_string()
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
            blocking_count: 0,
            blocking_summary: None,
            batch_incomplete: false,
            delivered_reviewers: if consumed { 1 } else { 0 },
            expected_reviewers: if dispatched { 1 } else { 0 },
            budget_exhausted: false,
            human_waived: false,
            ..Default::default()
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
    fn cross_review_blocking_findings_locks_write_and_shows_summary() {
        let mut g = gate("cross_review", "reviewing", true, true);
        g.blocking_count = 1;
        g.blocking_summary = Some("[F1] case-transfer-widget.js:1062: DOM 跳闪".to_string());

        let out = ContextRenderer::render_plan_gate(Some(&g));
        assert!(out.contains("门禁封锁中"), "应明确宣告门禁封锁: {out}");
        assert!(out.contains("物理封锁"), "应告知写入工具已被物理封锁: {out}");
        assert!(out.contains("[F1] case-transfer-widget.js:1062"), "应包含缺陷摘要: {out}");
        assert!(!out.contains("即可写入，门禁将自动放行"), "绝不应误导说即可写入: {out}");
    }

    #[test]
    fn cross_review_batch_incomplete_guides_wait_jobs() {
        let mut g = gate("cross_review", "reviewing", true, true);
        g.batch_incomplete = true;
        g.delivered_reviewers = 1;
        g.expected_reviewers = 2;

        let out = ContextRenderer::render_plan_gate(Some(&g));
        assert!(out.contains("wait_jobs"), "齐套屏障应指引 wait_jobs: {out}");
        assert!(out.contains("1/2"), "应展示齐套进度: {out}");
    }

    #[test]
    fn cross_review_budget_exhausted_guides_ask_human() {
        let mut g = gate("cross_review", "reviewing", true, true);
        g.budget_exhausted = true;
        g.reviewer_budget_used = 2;
        g.reviewer_budget_max = 2;

        let out = ContextRenderer::render_plan_gate(Some(&g));
        assert!(out.contains("预算已耗尽"), "应展示预算耗尽: {out}");
        assert!(out.contains("ask_human"), "应指引转人工审批: {out}");
    }

    #[test]
    fn cross_review_approved_shows_gate_open() {
        let out = ContextRenderer::render_plan_gate(Some(&gate(
            "cross_review",
            "approved",
            true,
            true,
        )));
        assert!(out.contains("门禁已放行"));
        assert!(!out.contains("将被拦截"), "放行后不应再出现拦截警告: {out}");
    }

    #[test]
    fn cross_review_approved_with_human_waiver_shows_waiver_hint() {
        let mut g = gate("cross_review", "approved", true, true);
        g.human_waived = true;

        let out = ContextRenderer::render_plan_gate(Some(&g));
        assert!(out.contains("门禁已放行"));
        assert!(out.contains("已获人工特批放行"));
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
