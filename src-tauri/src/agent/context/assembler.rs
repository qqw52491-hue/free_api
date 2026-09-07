use crate::agent::context::audit::AuditLedger;
use crate::agent::context::memory::MemoryManager;
use crate::agent::context::render::ContextRenderer;
use crate::agent::context::types::{ChatMessage, SandwichContext};
use serde_json::json;

impl SandwichContext {
    /// 组装发给大模型的消息体（真·三明治架构：顶层面包真静态 + 中间夹心纯追加 + 底层面包状态卡）
    pub fn assemble_messages(&self) -> Vec<ChatMessage> {
        let mut messages = Vec::new();

        // ════════════════════════════════════════════════════════════════
        // 🍞 1. 顶层面包（100% 静态前缀，任务期内绝对不变，稳吃 Prefix Cache）
        //    M0: System Prompt — 跨整个 Epoch 绝不改动
        //    M1: 当前 Epoch 的任务目标 — 压缩断代时才可能更新，其余步骤只读
        // ════════════════════════════════════════════════════════════════
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: json!(self.system_prompt.clone()),
        });

        // M1: 任务目标静态锚点（epoch 内不变，压缩时由 compaction 提升最新目标写入 ctx.user_goal）
        if !self.user_goal.is_empty() {
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: json!(format!("【任务目标】\n{}", self.user_goal)),
            });
        }

        // ════════════════════════════════════════════════════════════════
        // 🥩 2. 中间夹心（纯 Append-only 线性历史流水：工具手册事件、执行流水、目标变更事件）
        // 随着任务一步步向后追加，无动态插入，完美覆盖主流大模型的前缀缓存
        // ════════════════════════════════════════════════════════════════
        // 若 turns_history 首条消息与 M1 静态目标完全相同，则跳过首条，避免 M1 与历史首条合并产生重复目标
        let history_slice = if let Some(first) = self.turns_history.first() {
            if first.role == "user" && first.content.as_str() == Some(&self.user_goal) {
                &self.turns_history[1..]
            } else {
                &self.turns_history[..]
            }
        } else {
            &self.turns_history[..]
        };

        // 🥩 中间夹心：纯 Append-only 线性历史，组装时绝不改写历史字节。
        // 旧轮次的重型输出由 compaction 在触发高水位时一次性脱水定型（见 compaction.rs），
        // 平时相邻两轮请求的前缀差异只来自追加的新消息，最大化增量前缀缓存；
        // 此前"每轮按保护窗口改写旧消息"的方案会让前缀缓存每轮在窗口边界断裂。
        messages.extend(history_slice.iter().cloned());

        // ════════════════════════════════════════════════════════════════
        // 🍞 3. 底层面包（动态状态卡 + 避坑台账 + 不可逆操作 + 最新观测）
        // 变化点集中在末尾，绝对不砸前面的长缓存；且离输出端最近，强化注意力防遗忘
        // ════════════════════════════════════════════════════════════════
        let mut bottom_state_card = String::new();

        // (0) 活跃工具指针（任务目标已静态锚定在 M1，底部卡片仅保留动态变化的指针）
        if let Some(tool) = &self.active_tool {
            bottom_state_card.push_str(&format!("【当前活跃工具】: {}\n\n", tool));
        }

        // (1) 阶段计划
        if let (Some(id), Some(title)) = (&self.plan_state.active_id, &self.plan_state.active_title)
        {
            bottom_state_card.push_str(&format!(
                "【当前活动计划】id={} | title={}\n如下一步仍属于此阶段，复用相同 plan_id/plan_title；只有真正切换工作阶段才创建新计划。\n\n",
                id, title
            ));
        }

        // (1.1) 目标块（TargetBlock）状态机投影
        if let Some(block) = self.active_target_block() {
            let checklist_desc = if block.checklist.is_empty() {
                "无明确 Checklist".to_string()
            } else {
                block
                    .checklist
                    .iter()
                    .enumerate()
                    .map(|(i, c)| format!("{}. {}", i + 1, c))
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            bottom_state_card.push_str(&format!(
                "【当前活跃目标块】id={} | title={}\n• 预设档位: {}\n• 边界范围(Scope): {}\n• 验收清单(Checklist): {}\n• 步数已用: {}/{} (硬顶上限)\n\n",
                block.id,
                block.title,
                block.tier,
                if block.scope.is_empty() { "不限".to_string() } else { block.scope.join(", ") },
                checklist_desc,
                block.steps_count,
                block.hard_step_limit
            ));
        }

        // (2) 全局进度摘要：完整版已事件化入历史（set_progress_summary），
        // 尾部只携带首行+版本指针；事件缺失（旧会话/异常路径）回退全文。
        if !self.progress_summary.is_empty() {
            if self.last_persisted_summary.as_deref() == Some(self.progress_summary.as_str()) {
                let first_line: String = self
                    .progress_summary
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(160)
                    .collect();
                bottom_state_card.push_str(&format!(
                    "【全局进度摘要】当前 v{}（首行）：{}… 完整版见历史事件 v{}\n\n",
                    self.persisted_summary_revision, first_line, self.persisted_summary_revision
                ));
            } else {
                bottom_state_card.push_str(&format!(
                    "【全局进度与策略摘要】\n{}\n\n",
                    self.progress_summary
                ));
            }
        }

        // (3) 避坑指南（失败教训）
        bottom_state_card.push_str(&ContextRenderer::render_failed_attempts(
            &self.failed_attempts,
        ));

        // (4) 不可逆操作安全台账
        bottom_state_card.push_str(&AuditLedger::render_ledger(&self.side_effects));

        // (5) 核心事实记忆库（按 BTreeMap 严格确定性字母序）
        bottom_state_card.push_str(&MemoryManager::render_memory_card(&self.memories));

        // (6) 活跃资源作用域契约；独立于历史与压缩，每轮均从事实状态投影。
        let resource_scope_projection = self.resource_scope.render_projection();
        if !resource_scope_projection.is_empty() {
            bottom_state_card.push_str(&format!("{}\n\n", resource_scope_projection));
        }

        // (6.1) 项目受管文件清单与句柄 (Project Resource Manifest)
        // 动态放在尾部状态卡，既能让模型实时感知最新文件，又绝对不破坏 M0 顶层长前缀缓存！
        if let Some(manifest) = &self.project_resource_manifest {
            if !manifest.is_empty() {
                bottom_state_card.push_str(&format!("{}\n\n", manifest));
            }
        }

        // (7) 会话任务数据区目录 (TaskStore 资源摘要)
        bottom_state_card.push_str(&ContextRenderer::render_task_resources(
            &self.task_resources,
        ));

        // (8) 任务清单 (Todo List)
        bottom_state_card.push_str(&ContextRenderer::render_todo_list(&self.todo_list));

        // (9) 审查契约仅在复杂审查/诊断任务显式建立时出现，普通任务不增加状态卡。
        bottom_state_card.push_str(&ContextRenderer::render_review_contract(
            self.review_contract.as_ref(),
        ));
        // (10) 仅在已有成功写入时显示；将验证闭合的下一步保持在模型当前上下文内。
        bottom_state_card.push_str(&ContextRenderer::render_change_verification(
            self.side_effects.len(),
            self.change_verification.as_ref(),
            self.inferred_verification_command().as_deref(),
        ));
        // (11) 方案门禁状态投影：决策前可见当前模式与放行条件，避免靠写入被拒学习规则。
        bottom_state_card.push_str(&ContextRenderer::render_plan_gate(
            self.plan_gate.as_ref(),
        ));
        let trimmed_bottom = bottom_state_card.trim_end();
        if !trimmed_bottom.is_empty() {
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: json!(trimmed_bottom),
            });
        }

        // (8) 当前最新环境观测 + (9) 决策指令
        // 合并为单条 user 消息，避免连续多条 user 消息触发 Anthropic 等 provider 的 422 错误
        let mut final_user_parts = Vec::new();

        // 仅在历史流为空（如外部直接注入初始观测）且 current_observation 非空时才补充，
        // 避免与 turns_history 中的【执行输出】重复注入导致同一份结果出现两遍
        if self.turns_history.is_empty() && !self.current_observation.is_empty() {
            final_user_parts.push(format!(
                "【当前最新环境观测 (Observation)】\n(⚠️系统提示：这是你刚才操作后获取的最新的完整数据！请直接从这里读取你需要的信息，不要重复请求！)\n{}",
                self.current_observation
            ));
        }

        final_user_parts.push(ContextRenderer::render_decision_instruction(
            self.compact_tools_index.as_deref(),
        ));

        let final_user_str = final_user_parts.join("\n\n");
        if !final_user_str.trim().is_empty() {
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: json!(final_user_str),
            });
        }

        // ════════════════════════════════════════════════════════════════
        // 出口保持原始消息序列：组装层不做角色归并。
        // 归并是 provider 适配层的职责（见 llm_client 的
        // normalize_messages_for_provider）：OpenAI 兼容路由接受连续同角色
        // 消息，保持原序列可让历史尾部字节跨轮稳定，前缀缓存不会被每轮
        // 重新渲染的状态卡/决策指令打断；只有要求严格交替角色的 provider
        // （如 Anthropic 系）才在发送前归并。
        // ════════════════════════════════════════════════════════════════
        messages
    }
}

/// 对历史较旧的轮次进行证据化脱水，移除冗余的重型 raw stdout 和长报错堆栈。
/// 唯一调用方是 compaction（高水位事件内一次性定型）；日常组装绝不调用，
/// 以保证相邻请求之间前缀字节稳定（增量前缀缓存的前提）。
pub(crate) fn dehydrate_historical_turn(msg: &ChatMessage) -> ChatMessage {
    if msg.role != "user" {
        return msg.clone();
    }
    let text = match &msg.content {
        serde_json::Value::String(s) => s,
        _ => return msg.clone(),
    };

    // 1. 如果是早期执行输出且较长（> 400 字符），提取 output_id 和结果状态，将正文脱水为结构化证据句柄
    if (text.starts_with("【执行输出")
        || text.starts_with("[代码阅读护栏]")
        || text.contains("output_id: "))
        && text.len() > 400
    {
        let mut output_id = "";
        let mut status_line = "";
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("output_id: ") {
                output_id = trimmed[11..].trim();
            } else if (trimmed.starts_with("【计划执行结果报告】")
                || trimmed.starts_with("状态:")
                || trimmed.starts_with("Finished")
                || trimmed.starts_with("总耗时:"))
                && status_line.is_empty()
            {
                status_line = trimmed;
            }
        }
        if !output_id.is_empty() {
            let first_line = text.lines().next().unwrap_or("【执行输出】");
            let summary = if !status_line.is_empty() {
                format!(
                    "{}\n(完整详细数据已安全持久化至 SQLite，可通过 output_id={} 精准检索)",
                    status_line, output_id
                )
            } else {
                format!(
                    "(完整详细数据已安全持久化至 SQLite，可通过 output_id={} 精准检索)",
                    output_id
                )
            };
            return ChatMessage {
                role: "user".to_string(),
                content: json!(format!(
                    "{}\noutput_id: {}\n{}",
                    first_line, output_id, summary
                )),
            };
        }
    }

    // 2. 如果是历史早期的格式/执行错误且已过去（> 300 字符），压缩冗余报错堆栈
    if text.starts_with("【❌ 格式或执行错误】") && text.len() > 300 {
        let first_err_line = text.lines().nth(1).unwrap_or("执行异常");
        let compact_err: String = first_err_line.chars().take(120).collect();
        return ChatMessage {
            role: "user".to_string(),
            content: json!(format!(
                "【历史执行错误 (后续步骤已处理)】: {}",
                compact_err
            )),
        };
    }

    msg.clone()
}

/// 纯函数：将消息队列中连续相同角色的文本消息用换行合并为一条。
/// 仅供 provider 适配层（llm_client）在目标 provider 要求严格
/// user/assistant 交替时调用；通用组装路径不得调用——那会把每轮变化的
/// 状态卡拼进稳定的 历史 消息，使其在下一轮恢复原始字节，
/// 从而打断逐字节前缀缓存。
pub(crate) fn consolidate_consecutive_messages(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    let mut consolidated: Vec<ChatMessage> = Vec::with_capacity(messages.len());

    for msg in messages {
        if let Some(last) = consolidated.last_mut() {
            if last.role == msg.role {
                match (&mut last.content, &msg.content) {
                    (serde_json::Value::String(prev_str), serde_json::Value::String(curr_str)) => {
                        prev_str.push_str("\n\n");
                        prev_str.push_str(curr_str);
                        continue;
                    }
                    _ => {}
                }
            }
        }
        consolidated.push(msg);
    }

    consolidated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assemble_messages_zero_mutation_top_prefix() {
        let mut ctx = SandwichContext::new(
            "SYSTEM_PROMPT_STATIC_CONTRACT".to_string(),
            "Initial Goal 1".to_string(),
        );
        ctx.active_tool = Some("excel".to_string());

        let msgs1 = ctx.assemble_messages();
        // M0: System Prompt — 永不改动
        assert_eq!(msgs1[0].role, "system");
        assert_eq!(msgs1[0].content, json!("SYSTEM_PROMPT_STATIC_CONTRACT"));
        // M1: 初始任务目标静态锚点
        assert_eq!(msgs1[1].role, "user");
        assert!(msgs1[1]
            .content
            .as_str()
            .unwrap()
            .contains("Initial Goal 1"));

        // 模拟会话中目标发生变更（压缩时 compaction 会把 ctx.user_goal 提升为最新目标）
        ctx.user_goal = "New Goal 2 (Updated)".to_string();
        ctx.active_tool = Some("remote-hub".to_string());

        let msgs2 = ctx.assemble_messages();
        // M0 绝对不变
        assert_eq!(msgs2[0].role, "system");
        assert_eq!(msgs2[0].content, json!("SYSTEM_PROMPT_STATIC_CONTRACT"));
        // M1 随 ctx.user_goal 更新（压缩断代才发生）
        assert!(msgs2[1]
            .content
            .as_str()
            .unwrap()
            .contains("New Goal 2 (Updated)"));

        // 底部动态卡是独立消息（组装出口不合并），user_goal 已由 M1 承载
        let bottom_card = msgs2
            .iter()
            .find(|m| {
                m.content
                    .as_str()
                    .map(|c| c.contains("【当前活跃工具】"))
                    .unwrap_or(false)
            })
            .expect("应存在独立的状态卡消息");
        assert!(bottom_card
            .content
            .as_str()
            .unwrap()
            .contains("【当前活跃工具】: remote-hub"));
        assert!(
            !bottom_card
                .content
                .as_str()
                .unwrap()
                .contains("【当前任务目标】"),
            "user_goal 不应再出现在底部卡片"
        );
        assert!(
            msgs2.len() >= 3,
            "历史、状态卡、决策指令应各自独立成条（OpenAI 兼容路由不合并）"
        );
    }

    #[test]
    fn test_fetched_tools_compaction_reset() {
        let mut ctx = SandwichContext::new("SYS".to_string(), "Goal".to_string());
        ctx.context_window_tokens = 8000;
        ctx.fetched_tools.insert("excel".to_string());
        ctx.fetched_tools.insert("remote-hub".to_string());

        // Add enough history (> 5000 tokens) to trigger compaction
        let big_chunk = "A".repeat(500);
        for i in 0..60 {
            ctx.turns_history.push(ChatMessage {
                role: "user".to_string(),
                content: json!(format!("Step {} output data: {}", i, big_chunk)),
            });
        }

        ctx.maybe_compact_history();
        // Compaction must reset fetched_tools to prevent stale references
        assert!(ctx.fetched_tools.is_empty());
    }

    #[test]
    fn test_no_duplicate_goal_and_observation() {
        let mut ctx = SandwichContext::new("SYSTEM_PROMPT".to_string(), "My User Goal".to_string());

        // 模拟 turns_history 第一条就是用户的原始 prompt (与 user_goal 相同)
        ctx.turns_history.push(ChatMessage {
            role: "user".to_string(),
            content: json!("My User Goal"),
        });
        ctx.turns_history.push(ChatMessage {
            role: "assistant".to_string(),
            content: json!("计划: Test | 工具: remote-hub/shell_exec | 动作: Run"),
        });
        ctx.turns_history.push(ChatMessage {
            role: "user".to_string(),
            content: json!("【执行输出 (v3)】\noutput_id: 123\noutput content"),
        });

        ctx.current_observation = "output content".to_string();

        let msgs = ctx.assemble_messages();

        // 验证用户目标只出现了一次（由 M1 提供）
        let m1_user_msg = msgs[1].content.as_str().unwrap();
        assert!(m1_user_msg.contains("【任务目标】\nMy User Goal"));
        assert!(!m1_user_msg.contains("My User Goal\n\nMy User Goal"));

        // 验证底部的 user 消息中不包含重复的【当前最新环境观测 (Observation)】
        let last_msg = msgs.last().unwrap().content.as_str().unwrap();
        assert!(!last_msg.contains("【当前最新环境观测 (Observation)】"));
        // 决策指令依然正常输出
        assert!(last_msg.contains("请基于上述上下文执行下一步"));
    }

    #[test]
    fn progress_summary_versions_as_write_once_history_event_with_digest_tail() {
        let mut ctx = SandwichContext::new("SYS".to_string(), "Goal".to_string());

        // 第一版：完整全文作为历史事件追加，版本号 1
        ctx.set_progress_summary("v1 阶段总结：完成拓扑分析。\n细节 A。\n细节 B。");
        assert_eq!(ctx.persisted_summary_revision, 1);
        let history_text = serde_json::to_string(&ctx.turns_history).unwrap();
        assert!(history_text.contains("【全局进度与策略摘要 v1（write-once 历史事件"));
        assert!(history_text.contains("细节 B"), "完整版必须进历史事件");

        // 尾部只携带首行摘要 + 版本指针（不再全量重发）
        let msgs = ctx.assemble_messages();
        let tail_card = msgs
            .iter()
            .find(|m| {
                m.content
                    .as_str()
                    .map(|c| c.contains("【全局进度摘要】"))
                    .unwrap_or(false)
            })
            .expect("尾部应存在摘要指针");
        let tail_text = tail_card.content.as_str().unwrap();
        assert!(tail_text.contains("v1（首行）"));
        assert!(tail_text.contains("完整版见历史事件 v1"));
        assert!(!tail_text.contains("细节 B"), "尾部不得再全量重发");

        // 未变化的更新：不追加新事件（历史 append-only 稳定）
        let history_before = serde_json::to_string(&ctx.turns_history).unwrap();
        ctx.set_progress_summary("v1 阶段总结：完成拓扑分析。\n细节 A。\n细节 B。");
        assert_eq!(ctx.persisted_summary_revision, 1, "未变化不得新版本");
        assert_eq!(
            history_before,
            serde_json::to_string(&ctx.turns_history).unwrap()
        );

        // 变化的更新：追加 v2 事件
        ctx.set_progress_summary("v2 阶段总结：进入压缩链路审查。");
        assert_eq!(ctx.persisted_summary_revision, 2);
        assert!(serde_json::to_string(&ctx.turns_history)
            .unwrap()
            .contains("【全局进度与策略摘要 v2"));
    }

    #[test]
    fn history_tail_bytes_stable_across_turns_without_provider_merge() {
        let mut ctx = SandwichContext::new("SYS".to_string(), "Goal".to_string());
        ctx.active_tool = Some("t".to_string());

        // 第一轮：历史尾部 (assistant, user 执行输出 C)，其后是临时渲染的状态卡与指令
        ctx.turns_history.push(ChatMessage {
            role: "assistant".to_string(),
            content: json!("计划: 检索 | 工具: t | 动作: run"),
        });
        ctx.turns_history.push(ChatMessage {
            role: "user".to_string(),
            content: json!("【执行输出 C】\noutput_id: c-1\npayload line"),
        });
        let round1 = ctx.assemble_messages();

        // 执行输出 C 必须独立成条，不得与状态卡/指令拼接
        let c1 = round1
            .iter()
            .find(|m| {
                m.content
                    .as_str()
                    .map(|c| c.starts_with("【执行输出 C】"))
                    .unwrap_or(false)
            })
            .expect("round1 应含执行输出 C");
        assert_eq!(
            c1.content,
            json!("【执行输出 C】\noutput_id: c-1\npayload line"),
            "历史消息不得被尾部动态内容改写字节"
        );

        // 第二轮：追加新轮次，状态卡/指令在新尾部重新渲染
        ctx.turns_history.push(ChatMessage {
            role: "assistant".to_string(),
            content: json!("计划: 继续 | 工具: t | 动作: next"),
        });
        ctx.turns_history.push(ChatMessage {
            role: "user".to_string(),
            content: json!("【执行输出 D】\noutput_id: d-1\nother payload"),
        });
        let round2 = ctx.assemble_messages();

        // 核心断言：round1 去掉末尾两条动态尾部（状态卡+指令）后的序列，
        // 必须是 round2 的逐字节前缀——服务端前缀缓存可复用的充要条件。
        let stable_len = round1.len() - 2;
        let prefix1: Vec<String> = round1[..stable_len]
            .iter()
            .map(|m| serde_json::to_string(m).unwrap())
            .collect();
        let prefix2: Vec<String> = round2[..stable_len]
            .iter()
            .map(|m| serde_json::to_string(m).unwrap())
            .collect();
        assert_eq!(prefix1, prefix2, "上一轮稳定前缀必须在下一轮逐字节复现");
    }

    #[test]
    fn test_assemble_keeps_history_verbatim_without_runtime_dehydration() {
        let mut ctx = SandwichContext::new("SYS".to_string(), "Goal".to_string());

        // 模拟 10 轮历史，每轮包含一个超长执行输出（> 1000 字符）
        let big_raw = "X".repeat(1200);
        for i in 0..10 {
            ctx.turns_history.push(ChatMessage {
                role: "assistant".to_string(),
                content: json!(format!("计划: Step {}", i)),
            });
            ctx.turns_history.push(ChatMessage {
                role: "user".to_string(),
                content: json!(format!(
                    "【执行输出 (v3)】\noutput_id: out_step_{}\n【计划执行结果报告】状态: 全部成功\n{}",
                    i, big_raw
                )),
            });
        }

        let msgs = ctx.assemble_messages();
        let full_text = serde_json::to_string(&msgs).unwrap();

        // 新契约：组装时严格 append-only，旧轮次的原始字节必须原样保留；
        // 脱水只允许发生在 compaction 高水位事件内（见 compaction 测试）。
        // 否则每轮按保护窗口改写旧消息会让前缀缓存在窗口边界反复断裂。
        assert!(
            full_text.contains(&big_raw),
            "组装不得改写历史消息（旧输出必须原样保留）"
        );
        assert!(
            !full_text.contains("完整详细数据已安全持久化至 SQLite"),
            "组装路径不得产生脱水存根"
        );
    }

    #[test]
    fn review_contract_card_is_only_rendered_when_present() {
        let mut ctx = SandwichContext::new("SYS".to_string(), "审查上下文".to_string());
        let plain = serde_json::to_string(&ctx.assemble_messages()).unwrap();
        assert!(!plain.contains("【审查契约】"));

        ctx.review_contract = Some(crate::agent::review_contract::ReviewContract {
            scope_statement: "核验 finish 到 checkpoint 的调用链".to_string(),
            obligations: vec![crate::agent::review_contract::ReviewObligation {
                verify_command: None,
                verify_script: None,
                verify_sha256: None,
                id: "terminal".to_string(),
                question: "finish 是否由宿主语义门控？".to_string(),
                status: "open".to_string(),
                evidence_refs: Vec::new(),
                waived_reason: None,
            }],
        });
        let rendered = serde_json::to_string(&ctx.assemble_messages()).unwrap();
        assert!(rendered.contains("【审查契约】"));
        assert!(rendered.contains("核验 finish 到 checkpoint 的调用链"));
        assert!(rendered.contains("[open] terminal：finish 是否由宿主语义门控？"));
        assert!(rendered.contains("等待核验或显式豁免"));
    }
    #[test]
    fn change_verification_card_only_tracks_real_writes_and_their_closure() {
        let mut ctx = SandwichContext::new("SYS".to_string(), "修改上下文".to_string());
        let plain = serde_json::to_string(&ctx.assemble_messages()).unwrap();
        assert!(!plain.contains("【写入后验证】"));
        ctx.record_side_effect(1, "edit_file".to_string(), "更新配置".to_string());
        let pending = serde_json::to_string(&ctx.assemble_messages()).unwrap();
        assert!(pending.contains("【写入后验证】"));
        assert!(pending.contains("尚无验证证据或明确豁免"));
        ctx.change_verification = Some(crate::agent::change_verification::ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec!["output_id: check-42".to_string()],
            waived_reason: None,
        });
        let closed = serde_json::to_string(&ctx.assemble_messages()).unwrap();
        assert!(closed.contains("最近写入：已验证：output_id: check-42"));
        assert!(!closed.contains("尚无验证证据或明确豁免"));
    }
}
