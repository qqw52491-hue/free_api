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
        // ════════════════════════════════════════════════════════════════
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: json!(self.system_prompt.clone()),
        });

        // 活跃工具或 Skill 手册（次静态，同一工具连续调用时完全不变）
        if !self.active_tool_detail.is_empty() {
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: json!(format!(
                    "【当前活跃工具/Skill 操作手册】\n{}\n",
                    self.active_tool_detail
                )),
            });
        }

        // 终极目标（在单次回合内绝对静态，置于静态区末尾稳吃长缓存）
        if !self.user_goal.is_empty() {
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: json!(format!("【用户终极任务目标】\n{}\n", self.user_goal)),
            });
        }

        // ════════════════════════════════════════════════════════════════
        // 🥩 2. 中间夹心（纯 Append-only 线性历史流水）
        // 随着任务一步步向后追加，无动态插入，完美覆盖主流大模型的前缀缓存
        // ════════════════════════════════════════════════════════════════
        messages.extend(self.turns_history.clone());

        // ════════════════════════════════════════════════════════════════
        // 🍞 3. 底层面包（动态状态卡 + 避坑台账 + 不可逆操作 + 最新观测）
        // 变化点集中在末尾，绝对不砸前面的长缓存；且离输出端最近，强化注意力防遗忘
        // ════════════════════════════════════════════════════════════════
        let mut bottom_state_card = String::new();

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

        if !self.current_observation.is_empty() {
            final_user_parts.push(format!(
                "【当前最新环境观测 (Observation)】\n(⚠️系统提示：这是你刚才操作后获取的最新的完整数据！请直接从这里读取你需要的信息，不要重复请求！)\n{}",
                self.current_observation
            ));
        }

        final_user_parts.push(ContextRenderer::render_decision_instruction().to_string());

        // ════════════════════════════════════════════════════════════════
        // 🔄 出口统一合并：对所有相邻的同 role 消息做单趟无损归并
        // 彻底杜绝连续 2 条 user 或连续 2 条 assistant 导致 Anthropic 422 报错
        // ════════════════════════════════════════════════════════════════
        consolidate_consecutive_messages(messages)
    }
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
