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

        // 🌟 证据化脱水：保护最新 6 条消息保持 100% 原汁原味（当前活跃工作窗口），
        // 较早的历史轮次自动脱水为精简结构化证据句柄，消除历史消息中数十条 raw stdout 造成的上下文膨胀
        let total_hist = history_slice.len();
        let protected_recent_count = 6;
        let dehydrated_history: Vec<ChatMessage> = history_slice
            .iter()
            .enumerate()
            .map(|(idx, m)| {
                if idx + protected_recent_count >= total_hist {
                    m.clone()
                } else {
                    dehydrate_historical_turn(m)
                }
            })
            .collect();
        messages.extend(dehydrated_history);

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
        if let (Some(id), Some(title)) = (&self.plan_state.active_id, &self.plan_state.active_title) {
            bottom_state_card.push_str(&format!(
                "【当前活动计划】id={} | title={}\n如下一步仍属于此阶段，复用相同 plan_id/plan_title；只有真正切换工作阶段才创建新计划。\n\n",
                id, title
            ));
        }

        // (2) 全局进度摘要
        if !self.progress_summary.is_empty() {
            bottom_state_card.push_str(&format!(
                "【全局进度与策略摘要】\n{}\n\n",
                self.progress_summary
            ));
        }

        // (3) 避坑指南（失败教训）
        bottom_state_card.push_str(&ContextRenderer::render_failed_attempts(&self.failed_attempts));

        // (4) 不可逆操作安全台账
        bottom_state_card.push_str(&AuditLedger::render_ledger(&self.side_effects));

        // (5) 核心事实记忆库（按 BTreeMap 严格确定性字母序）
        bottom_state_card.push_str(&MemoryManager::render_memory_card(&self.memories));

        // (6) 会话任务数据区目录 (TaskStore 资源摘要)
        bottom_state_card.push_str(&ContextRenderer::render_task_resources(&self.task_resources));

        // (7) 任务清单 (Todo List)
        bottom_state_card.push_str(&ContextRenderer::render_todo_list(&self.todo_list));

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

        final_user_parts.push(ContextRenderer::render_decision_instruction().to_string());

        let final_user_str = final_user_parts.join("\n\n");
        if !final_user_str.trim().is_empty() {
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: json!(final_user_str),
            });
        }

        // ════════════════════════════════════════════════════════════════
        // 🔄 出口统一合并：对所有相邻的同 role 消息做单趟无损归并
        // 彻底杜绝连续 2 条 user 或连续 2 条 assistant 导致 Anthropic 422 报错
        // ════════════════════════════════════════════════════════════════
        consolidate_consecutive_messages(messages)
    }
}

/// 对历史较旧的轮次（除最新工作窗口外）进行证据化脱水，移除冗余的重型 raw stdout 和长报错堆栈
fn dehydrate_historical_turn(msg: &ChatMessage) -> ChatMessage {
    if msg.role != "user" {
        return msg.clone();
    }
    let text = match &msg.content {
        serde_json::Value::String(s) => s,
        _ => return msg.clone(),
    };

    // 1. 如果是早期执行输出且较长（> 400 字符），提取 output_id 和结果状态，将正文脱水为结构化证据句柄
    if text.starts_with("【执行输出") && text.len() > 400 {
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
                format!("(完整详细数据已安全持久化至 SQLite，可通过 output_id={} 精准检索)", output_id)
            };
            return ChatMessage {
                role: "user".to_string(),
                content: json!(format!("{}\noutput_id: {}\n{}", first_line, output_id, summary)),
            };
        }
    }

    // 2. 如果是历史早期的格式/执行错误且已过去（> 300 字符），压缩冗余报错堆栈
    if text.starts_with("【❌ 格式或执行错误】") && text.len() > 300 {
        let first_err_line = text.lines().nth(1).unwrap_or("执行异常");
        let compact_err: String = first_err_line.chars().take(120).collect();
        return ChatMessage {
            role: "user".to_string(),
            content: json!(format!("【历史执行错误 (后续步骤已处理)】: {}", compact_err)),
        };
    }

    msg.clone()
}

/// 纯函数：将消息队列中连续相同角色的文本消息用换行合并为一条
fn consolidate_consecutive_messages(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
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
        assert!(msgs1[1].content.as_str().unwrap().contains("Initial Goal 1"));

        // 模拟会话中目标发生变更（压缩时 compaction 会把 ctx.user_goal 提升为最新目标）
        ctx.user_goal = "New Goal 2 (Updated)".to_string();
        ctx.active_tool = Some("remote-hub".to_string());

        let msgs2 = ctx.assemble_messages();
        // M0 绝对不变
        assert_eq!(msgs2[0].role, "system");
        assert_eq!(msgs2[0].content, json!("SYSTEM_PROMPT_STATIC_CONTRACT"));
        // M1 随 ctx.user_goal 更新（压缩断代才发生）
        assert!(msgs2[1].content.as_str().unwrap().contains("New Goal 2 (Updated)"));

        // 底部动态卡只有活跃工具，user_goal 已由 M1 承载，不重复
        let bottom_card = msgs2.last().unwrap().content.as_str().unwrap();
        assert!(bottom_card.contains("【当前活跃工具】: remote-hub"));
        assert!(!bottom_card.contains("【当前任务目标】"), "user_goal 不应再出现在底部卡片");
    }

    #[test]
    fn test_fetched_tools_compaction_reset() {
        let mut ctx = SandwichContext::new(
            "SYS".to_string(),
            "Goal".to_string(),
        );
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
        let mut ctx = SandwichContext::new(
            "SYSTEM_PROMPT".to_string(),
            "My User Goal".to_string(),
        );

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
    fn test_historical_evidence_dehydration() {
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

        // 验证较旧的历史轮次（如 step 0）被成功脱水为句柄，不再包含 1200 字符的原始 X
        let full_text = serde_json::to_string(&msgs).unwrap();
        assert!(full_text.contains("out_step_0"));
        assert!(full_text.contains("完整详细数据已安全持久化至 SQLite"));

        // 验证最新轮次（如 step 9）保持完整未被脱水
        assert!(full_text.contains("out_step_9"));
    }
}

