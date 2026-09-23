use crate::agent::change_verification::{change_verification_blocker, ChangeVerification};
use crate::agent::context::types::{EvidenceReceiptEntry, FailedAttempt, PlanGateProjection};
use std::collections::BTreeMap;
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
                "【方案门禁状态】🟢 门禁已放行{}\n指引: 按已批准方案执行写入并验证。若执行中新证据、验证失败或已批准假设被推翻，须停止沿用旧方案并重新 submit_plan，填写 revision_reason 且提交更新后的 evidence_refs；仅措辞优化、主观偏好或无新增证据不得重开。修订会撤销旧批准、重新锁定写入并消耗审查轮次。\n\n",
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
        // 4.1 尚未派发审查员 —— 探索节奏由模型自定；宿主不按步数催促收敛
        if !gate.reviewer_dispatched {
            return format!(
                "【方案门禁状态】🔍 交叉审查 (cross_review) | 审查: 尚未派发（预算 {}/{}）\n指引: 首次写入前必须先完成方案审查：证据成熟后输出 submit_plan 提交结构化方案（宿主自动派发 Tech Lead）——submit_plan 是顶层独立动作：单独输出 {{\"submit_plan\": {{...}}}}，勿放进 tool/action/params；或调用 child_reviewer 手动派发；直接写工具将被门禁拦截。若探索中发现任务实际形状与初始分流不符（范围骤扩/方向被证伪），可单独输出 {{\"request_retriage\": {{\"reason\": \"...\", \"unresolved_questions\": [...]}}}} 申请一次独立复核分流。\n\n",
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

    /// 取证台账投影：宿主记录的读取/列目录/检索/回查足迹，跨压缩保留。
    /// 只列机械事实（什么、第几步、句柄在哪），不评判、不催促——
    /// 证据是否足够由模型自己判断。原文可按 output_id 经 get_step_output 取回。
    pub fn render_evidence_ledger(ledger: &BTreeMap<String, EvidenceReceiptEntry>) -> String {
        if ledger.is_empty() {
            return String::new();
        }
        // 最近活跃优先，同序按键字典序兜底，保证渲染确定性
        let mut entries: Vec<(&String, &EvidenceReceiptEntry)> = ledger.iter().collect();
        entries.sort_by(|a, b| b.1.last_step.cmp(&a.1.last_step).then(a.0.cmp(b.0)));
        const MAX_LINES: usize = 20;
        let total = entries.len();
        let mut lines = Vec::new();
        for (key, e) in entries.into_iter().take(MAX_LINES) {
            let mut parts = key.splitn(3, '|');
            let kind = parts.next().unwrap_or("");
            let target = parts.next().unwrap_or("");
            let detail = parts.next().unwrap_or("");
            let steps = if e.count <= 1 {
                format!("step={}", e.first_step)
            } else {
                format!("×{} (step {}~{})", e.count, e.first_step, e.last_step)
            };
            let oid = e
                .output_id
                .as_deref()
                .map(|id| format!(" → {}", id))
                .unwrap_or_default();
            let detail_str = if detail.is_empty() {
                String::new()
            } else {
                format!(" {}", detail)
            };
            lines.push(format!("- {} {}{} · {}{}", kind, target, detail_str, steps, oid));
        }
        let more = if total > MAX_LINES {
            format!("\n…另有 {} 项更早记录未列出", total - MAX_LINES)
        } else {
            String::new()
        };
        format!(
            "【取证台账】本任务已执行的读取/检索足迹（完整原文可按 output_id 经 get_step_output 取回）:\n{}{}\n\n",
            lines.join("\n"),
            more
        )
    }

    /// 运行遥测投影：当前步号与本 run 累计 token 消耗——宿主 100% 可核实
    /// 的机械事实。只报数，不带任何「该收敛/该停止」类暗示；节奏判断归模型。
    /// 步号口径与取证台账/避坑台账/run_trace 的 step id 一致。
    pub fn render_run_telemetry(
        step_id: usize,
        run_prompt_tokens: u64,
        run_completion_tokens: u64,
        last_context_tokens: Option<usize>,
        context_window: usize,
    ) -> String {
        // 首步之前（尚无一次完整 LLM 往返）无遥测可报，不渲染空卡
        if run_prompt_tokens == 0 && step_id == 0 {
            return String::new();
        }
        // 口径说明（不写给模型，只留注释）：累计数按主循环每次实际 LLM 调用
        // 落账；上下文规模取上一次完整请求回传的 prompt_tokens（是「上次」
        // 而非「本次」——本次请求尚未发出，无从先知）。
        let mut line = format!(
            "【运行遥测】step={} | 主循环累计 prompt={} / completion={} tokens",
            step_id, run_prompt_tokens, run_completion_tokens
        );
        if let Some(ctx_tokens) = last_context_tokens {
            line.push_str(&format!(" | 上次请求 prompt={}", ctx_tokens));
            if context_window > 0 {
                line.push_str(&format!(
                    " / 窗口 {} ({:.0}%)",
                    context_window,
                    ctx_tokens as f64 * 100.0 / context_window as f64
                ));
            }
        }
        line.push_str("\n\n");
        line
    }

    pub fn render_decision_instruction(compact_tools: Option<&str>) -> String {
        if let Some(tools) = compact_tools.filter(|s| !s.trim().is_empty()) {
            format!(
                "【当前可用工具快速索引】(只能从以下调用，严禁臆造 core/bash 等未声明工具)\n{}\n\n请基于上述上下文执行下一步。只能输出纯文本 JSON（禁止裹 ```），tool 名必须严格来自可用工具清单。thought 字段默认留空——仅在关键转折/矛盾发现/方案切换/受阻时写 1 句工程师推断，格式「[现象]——[推断]：[接下来动作]」，严禁问候/客套/向用户汇报进度、不复述 description，常规连续执行严格静默；description 字段保持一句话进度标签；仅当上一步失败或被拦截时在 reflection 补一句原因与改法；本轮若取得阶段突破、关键结论、里程碑进展或发现风险阻碍，额外输出 finding(1~2 句结论) 与 finding_kind(start|breakthrough|milestone|summary|warning|blocker)——finding 与 thought 互斥，输出 finding 时 thought 必须为 ''，严禁同一轮两处各说一遍。",
                tools
            )
        } else {
            "请基于上述上下文执行下一步。只能输出纯文本 JSON（禁止裹 ```），tool 名必须严格来自上方工具菜单。thought 字段默认留空——仅在关键转折/矛盾发现/方案切换/受阻时写 1 句工程师推断，格式「[现象]——[推断]：[接下来动作]」，严禁问候/客套/向用户汇报进度、不复述 description，常规连续执行严格静默；description 字段保持一句话进度标签；仅当上一步失败或被拦截时在 reflection 补一句原因与改法；本轮若取得阶段突破、关键结论、里程碑进展或发现风险阻碍，额外输出 finding(1~2 句结论) 与 finding_kind(start|breakthrough|milestone|summary|warning|blocker)——finding 与 thought 互斥，输出 finding 时 thought 必须为 ''，严禁同一轮两处各说一遍。".to_string()
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
        assert!(out.contains("revision_reason"), "批准后应显示有证据修订通道: {out}");
        assert!(out.contains("无新增证据不得重开"), "应抑制无依据反复修订: {out}");
        assert!(out.contains("重新锁定写入"), "应披露修订的门禁后果: {out}");
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
