use crate::agent::change_verification::ChangeVerification;
use crate::agent::context::types::{EvidenceReceiptEntry, FailedAttempt, PlanGateProjection};
use std::collections::BTreeMap;
use crate::agent::review_contract::ReviewContract;
use crate::agent::task_store::TaskResourceRef;
use crate::agent::types::TodoItem;

/// 写入后验证的完整规则文案（静态）：首次有待闭合写入时作为一次性
/// 历史事件注入（见 SandwichContext::ensure_rule_events）；此后尾部
/// 只投影一行待闭合状态，不再每轮重发这段规则。
pub(crate) const VERIFICATION_RULES_TEXT: &str = "- 写后验证建议：按任务风险选择测试或实际观测；主动验证授权已具备，无需向用户申请许可。已有有效回执可直接供终审使用，无需重复填报或重跑。\n- change_verification_update 为可选申报；如使用 verified，引用真实已完成的 step_N / output_id，宿主核对本会话、本运行及晚于最近副作用的时序。系统识别到成功验证时会自动标记为 verified；状态仍不代替证据充分性判断。\n- 缺少申报不单独阻断交付，也不自动猜命令重跑；真实验证失败仍需处理，未测范围如实披露并交终审判断。";

/// 审查契约的闭合规则文案（静态）：契约存在时作为一次性历史事件注入；
/// 尾部契约卡此后只渲染范围+义务清单动态值。
pub(crate) const CONTRACT_RULES_TEXT: &str = "review_contract_update 按义务 id 合并（局部更新），未提交项保留；不用重复抄写原问题、范围或验证方法。无执行命令的项可引用现有 output_id/源码位置申报 evidenced，或以 waived + 理由说明不适用，交终审核验。绑定 verify_command 的项由宿主直接执行，不必先手工申报通过或复制同一验证结果。验证命令/脚本允许按实际产物和环境显式修订；请在本轮说明原因，修订不是通过证明，旧执行结果不能冒充新方法结果。申报不等于验收通过；open 不单独阻断交付，交终审结合实际回执判断。普通建议不自动成为额外义务，原用户验收要求及真实阻塞继续核验。";

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
        // 紧凑行式：pretty JSON 里 ~76% 是排版空白，状态卡每轮重发不划算
        let lines: Vec<String> = todos
            .iter()
            .map(|t| {
                let ev = t
                    .evidence_output_id
                    .as_deref()
                    .map(|id| format!(" (证据 {})", id))
                    .unwrap_or_default();
                if t.is_review_reference() {
                    format!("- [历史审查参考，非验收待办；原状态 {}] #{} {}{}", t.status, t.id, t.description, ev)
                } else {
                    format!("- [{}] #{} {}{}", t.status, t.id, t.description, ev)
                }
            })
            .collect();
        format!("【当前任务面板 (Todo List)】\n{}\n\n", lines.join("\n"))
    }

    pub fn render_review_contract(
        contract: Option<&ReviewContract>,
        rules_emitted: bool,
    ) -> String {
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
                        "已申报证据，待验收: {}",
                        item.evidence_refs
                            .iter()
                            .take(2)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                    "waived" => format!(
                        "已申报豁免: {}",
                        item.waived_reason.as_deref().unwrap_or("未说明")
                    ),
                    _ => "尚未申报完成依据".to_string(),
                };
                format!(
                    "- [{}] {}：{}（{}）",
                    status, item.id, item.question, detail
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        if rules_emitted {
            // 闭合规则已事件化进历史（可缓存前缀），尾部只投影动态义务清单
            format!(
                "【审查契约】\n范围：{}\n{}\n（申报规则见历史规则卡；申报不等于验收通过，可按 id 补正）\n\n",
                contract.scope_statement, obligations
            )
        } else {
            format!(
                "【审查契约】\n范围：{}\n{}\n{}\n\n",
                contract.scope_statement, obligations, CONTRACT_RULES_TEXT
            )
        }
    }

    pub fn render_change_verification(
        side_effect_count: usize,
        verification: Option<&ChangeVerification>,
        suggested_cmd: Option<&str>,
        rules_emitted: bool,
    ) -> String {
        if side_effect_count == 0 {
            return String::new();
        }
        let state = verification.map(ChangeVerification::summary)
            .unwrap_or_else(|| "未收到验证申报；终审核对已有执行回执，不代表未验证".into());
        let cmd_hint = suggested_cmd.map(|cmd| format!(
            "\n建议检查（按需执行，未自动运行）：`{cmd}`"
        )).unwrap_or_default();
        format!(
            "【写入后验证参考】\n{}{}\n{}\n\n",
            state, cmd_hint,
            if rules_emitted { "无需为同步状态重复填报；交付仍由适用审查决定。" } else { VERIFICATION_RULES_TEXT }
        )
    }

    /// 方案门禁状态卡：让模型在「决策前」看到当前模式与放行条件，
    /// 避免靠写入被拒（一整轮 LLM 往返的代价）来反推门禁规则。
    /// 指引文案必须与 loop_runner 的 check_pre_write_gatekeeper 拦截语义一一对应。
    /// 方案门禁状态卡：让模型在「决策前」看到当前模式与放行条件，
    /// 避免靠写入被拒（一整轮 LLM 往返的代价）来反推门禁规则。
    /// 【按需条件化渲染原则】：每轮仅命中唯一的激活状态分支，严格控体积（单状态 1~3 行），稳吃前缀缓存。
    pub fn render_plan_gate(gate: Option<&PlanGateProjection>) -> String {
        let pending = gate.and_then(|g| g.pending_human_reply.as_ref()).map(|r| r.render()).unwrap_or_default();
        format!("{}{}", pending, Self::render_plan_gate_status(gate))
    }

    fn render_plan_gate_status(gate: Option<&PlanGateProjection>) -> String {
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
                "【方案门禁状态】🟢 门禁已放行{}\n指引: 按已批准方案执行写入并验证。若执行中新证据、验证失败或已批准假设被推翻，须停止沿用旧方案并重新 submit_plan，填写 revision_reason 且提交更新后的 evidence_refs；仅措辞优化、主观偏好或无新增证据不得重开。修订会撤销旧批准、重新锁定写入并消耗审查轮次。修订方案不是从零开始：须回顾上文历史里已完成的写入，在 plan_summary 中写明“已完成并保留的部分”与“需回退/重做的部分”。\n\n",
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
        // 同文件的 read|path|LX-Y 明细合并成单行覆盖图：
        // 「已读 L1-60, L500-850」比 39 条行窗流水更省也更可读——
        // 长任务中模型据此知道哪些区间已有覆盖，不必回读重新发现。
        let mut read_files: BTreeMap<String, Vec<(String, &EvidenceReceiptEntry)>> =
            BTreeMap::new();
        let mut others: Vec<(&String, &EvidenceReceiptEntry)> = Vec::new();
        for (key, e) in ledger.iter() {
            let mut parts = key.splitn(3, '|');
            let kind = parts.next().unwrap_or("");
            let target = parts.next().unwrap_or("");
            let detail = parts.next().unwrap_or("");
            if kind == "read" && !target.is_empty() {
                read_files
                    .entry(target.to_string())
                    .or_default()
                    .push((detail.to_string(), e));
            } else {
                others.push((key, e));
            }
        }

        let mut lines = Vec::new();
        // 覆盖行按最近活跃排序（与旧行为一致：近的在上面）
        let mut file_rows: Vec<(usize, String)> = read_files
            .into_iter()
            .map(|(path, windows)| {
                let last_step = windows.iter().map(|(_, e)| e.last_step).max().unwrap_or(0);
                let first_step = windows.iter().map(|(_, e)| e.first_step).min().unwrap_or(0);
                let total_count: u32 = windows.iter().map(|(_, e)| e.count).sum();
                let latest_oid = windows
                    .iter()
                    .max_by_key(|(_, e)| e.last_step)
                    .and_then(|(_, e)| e.output_id.as_deref());
                let coverage = Self::merge_window_labels(&windows);
                let oid = latest_oid.map(|id| format!(" → {}", id)).unwrap_or_default();
                let line = format!(
                    "- read {} 已读 {} · ×{} (step {}~{}){}",
                    path, coverage, total_count, first_step, last_step, oid
                );
                (last_step, line)
            })
            .collect();
        file_rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

        others.sort_by(|a, b| b.1.last_step.cmp(&a.1.last_step).then(a.0.cmp(b.0)));
        let mut other_rows: Vec<(usize, String)> = others
            .into_iter()
            .map(|(key, e)| {
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
                (
                    e.last_step,
                    format!("- {} {}{} · {}{}", kind, target, detail_str, steps, oid),
                )
            })
            .collect();

        // 覆盖行与其余条目统一按最近活跃混排
        let mut all_rows = Vec::new();
        all_rows.append(&mut file_rows);
        all_rows.append(&mut other_rows);
        all_rows.sort_by(|a, b| b.0.cmp(&a.0));

        const MAX_LINES: usize = 20;
        let total = all_rows.len();
        for (_, line) in all_rows.into_iter().take(MAX_LINES) {
            lines.push(line);
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

    /// 行窗明细标签（L1-150 / head=N / 整读…）合并为覆盖区间串：
    /// 可解析的 L 窗按起点排序并合并重叠/相邻区间，非行窗形态原样保留。
    fn merge_window_labels(windows: &[(String, &EvidenceReceiptEntry)]) -> String {
        let mut ranges: Vec<(i64, i64)> = Vec::new();
        let mut misc: Vec<String> = Vec::new();
        for (label, _) in windows {
            let parsed = if let Some(rest) = label.strip_prefix('L') {
                let mut it = rest.splitn(2, '-');
                let s = it.next().and_then(|v| v.parse::<i64>().ok());
                let e = it.next().and_then(|v| v.parse::<i64>().ok());
                match (s, e) {
                    (Some(s), Some(e)) => Some((s, e)),
                    (Some(s), None) => Some((s, i64::MAX / 2)),
                    _ => None,
                }
            } else {
                None
            };
            match parsed {
                Some(r) => ranges.push(r),
                None => misc.push(label.clone()),
            }
        }
        ranges.sort();
        let mut merged: Vec<(i64, i64)> = Vec::new();
        for (s, e) in ranges {
            if let Some(last) = merged.last_mut() {
                if s <= last.1 + 1 {
                    last.1 = last.1.max(e);
                    continue;
                }
            }
            merged.push((s, e));
        }
        let mut parts: Vec<String> = merged
            .iter()
            .map(|(s, e)| {
                if *e >= i64::MAX / 2 {
                    format!("L{}-", s)
                } else {
                    format!("L{}-{}", s, e)
                }
            })
            .collect();
        parts.extend(misc);
        if parts.is_empty() {
            return "（无行窗记录）".to_string();
        }
        const MAX_PARTS: usize = 8;
        if parts.len() > MAX_PARTS {
            let shown = parts[..MAX_PARTS].join(", ");
            format!("{} …+{}段", shown, parts.len() - MAX_PARTS)
        } else {
            parts.join(", ")
        }
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
        Self::render_decision_instruction_at(compact_tools, chrono::Local::now().fixed_offset())
    }

    // 日期只投影到请求尾部，不写入静态系统前缀；跨日更新，不注入逐秒变化。
    fn render_decision_instruction_at(
        compact_tools: Option<&str>,
        now: chrono::DateTime<chrono::FixedOffset>,
    ) -> String {
        let instruction = if let Some(tools) = compact_tools.filter(|s| !s.trim().is_empty()) {
            format!(
                "【当前可用工具快速索引】(插件级条目先经 capability_search / next_tool_hint 取得完整工具名和 schema；勿猜名称)\n{}\n\n请基于上述上下文执行下一步。只能输出纯文本 JSON（禁止裹 ```），tool 名必须来自当前工具清单或本会话已加载的工具 schema。thought 字段默认留空——仅在关键转折/矛盾发现/方案切换/受阻时简要写出思考推断（支持直截了当的 Markdown 表述说清楚），严禁问候/客套/向用户汇报进度、不复述 description，常规连续执行严格静默；description 字段保持一句话进度标签；仅当上一步失败或被拦截时在 reflection 补一句原因与改法；本轮若取得阶段突破、关键结论、里程碑进展或发现风险阻碍，额外输出 finding(1~2 句结论) 与 finding_kind(start|breakthrough|milestone|summary|warning|blocker)——finding 与 thought 互斥，输出 finding 时 thought 必须为 ''，严禁同一轮两处各说一遍。",
                tools
            )
        } else {
            "请基于上述上下文执行下一步。只能输出纯文本 JSON（禁止裹 ```），tool 名必须来自上方工具菜单或本会话已加载的工具 schema；目录仅列插件时先经 capability_search / next_tool_hint 取得完整名称与参数。thought 字段默认留空——仅在关键转折/矛盾发现/方案切换/受阻时简要写出思考推断（支持直截了当的 Markdown 表述说清楚），严禁问候/客套/向用户汇报进度、不复述 description，常规连续执行严格静默；description 字段保持一句话进度标签；仅当上一步失败或被拦截时在 reflection 补一句原因与改法；本轮若取得阶段突破、关键结论、里程碑进展或发现风险阻碍，额外输出 finding(1~2 句结论) 与 finding_kind(start|breakthrough|milestone|summary|warning|blocker)——finding 与 thought 互斥，输出 finding 时 thought 必须为 ''，严禁同一轮两处各说一遍。".to_string()
        };
        format!(
            "【宿主当前日期】{}（宿主本地时区 UTC{}；用于时间定位，不证明版本或发布信息）\n\n{}",
            now.format("%Y-%m-%d"), now.format("%:z"), instruction
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_tail_date_tracks_the_host_day_and_offset_without_second_churn() {
        let render = |timestamp: &str| ContextRenderer::render_decision_instruction_at(
            Some("example: 46 个工具"),
            chrono::DateTime::parse_from_rfc3339(timestamp).unwrap(),
        );
        let before = render("2026-10-19T23:59:58+08:00");
        assert_eq!(before, render("2026-10-19T23:59:59+08:00"));
        let after = render("2026-10-20T00:00:00+08:00");
        assert!(before.contains("2026-10-19") && after.contains("2026-10-20"));
        assert!(after.contains("UTC+08:00") && after.contains("不证明版本"));
        // 同一瞬间，宿主处于 UTC 时日期应不同，不能硬编码用户或供应商时区。
        let utc = render("2026-10-19T16:00:00+00:00");
        assert!(utc.contains("2026-10-19") && utc.contains("UTC+00:00"));
        assert!(after.contains("capability_search / next_tool_hint") && after.contains("已加载的工具 schema"));
        let core = include_str!("../../../prompts/core_logic.md");
        assert!(!core.contains("【宿主当前日期】"));
    }

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
        assert!(ContextRenderer::render_change_verification(0, None, None, false).is_empty());
    }

    #[test]
    fn render_change_verification_prompts_proactive_authorization_and_inferred_cmd() {
        let out = ContextRenderer::render_change_verification(
            1,
            None,
            Some("npx vue-tsc --noEmit"),
            false,
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
        let out = ContextRenderer::render_change_verification(1, Some(&verified), None, false);
        assert!(out.contains("已验证：cargo test all pass"));
        assert!(out.contains("终审"));
        assert!(!out.contains("直接放行"));
    }
}
