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

        // ════════════════════════════════════════════════════════════════
        // 🥩 2. 中间夹心（纯 Append-only 线性历史流水）
        // 随着任务一步步向后追加，无动态插入，完美覆盖主流大模型的前缀缓存
        // ════════════════════════════════════════════════════════════════
        messages.extend(self.turns_history.clone());

        // ════════════════════════════════════════════════════════════════
        // 🍞 3. 底层面包（状态卡 + 任务目标 + 最新观测）
        // 变化点钉死在末尾，绝对不砸前面的长缓存；且离输出端最近，强化注意力防遗忘
        // ════════════════════════════════════════════════════════════════
        let mut bottom_state_card = String::new();

        // (1) 终极目标（置于末尾状态卡首位，近因效应强化）
        bottom_state_card.push_str(&format!("【用户终极任务目标】\n{}\n\n", self.user_goal));

        // (2) 阶段计划
        if let (Some(id), Some(title)) = (&self.active_plan_id, &self.active_plan_title) {
            bottom_state_card.push_str(&format!(
                "【当前活动计划】id={} | title={}\n如下一步仍属于此阶段，复用相同 plan_id/plan_title；只有真正切换工作阶段才创建新计划。\n\n",
                id, title
            ));
        }

        // (3) 全局进度摘要
        if !self.progress_summary.is_empty() {
            bottom_state_card.push_str(&format!(
                "【全局进度与策略摘要】\n{}\n\n",
                self.progress_summary
            ));
        }

        // (4) 会话任务数据区目录 (TaskStore 资源摘要)
        if !self.task_resources.is_empty() {
            let resources: Vec<String> = self
                .task_resources
                .iter()
                .take(20)
                .map(|resource| {
                    let summary: String = resource.summary.chars().take(240).collect();
                    format!(
                        "- id={} | kind={} | label={} | rows={} | status={} | summary={}",
                        resource.resource_id,
                        resource.kind,
                        resource.label,
                        resource.row_count,
                        resource.status,
                        summary
                    )
                })
                .collect();
            bottom_state_card.push_str(&format!(
                "【会话任务数据区目录】\n{}\n(记录本体已独立持久化，默认不会发给你；需要时调用内部 task_store 的 list_records 或 get_record 分页查询。)\n\n",
                resources.join("\n")
            ));
        }

        // (5) 冷存储记忆库 (L1 Key-Value)
        if !self.memories.is_empty() {
            if self.carry_memories {
                let facts: Vec<String> = self
                    .memories
                    .iter()
                    .map(|(k, v)| format!("  {}: {}", k, v))
                    .collect();
                bottom_state_card.push_str(&format!(
                    "【冷存储记忆库 (已全量展开)】\n{}\n\n",
                    facts.join("\n")
                ));
            } else {
                let keys: Vec<String> = self.memories.keys().cloned().collect();
                bottom_state_card.push_str(&format!(
                    "【冷存储记忆库索引 (当前已折叠)】\n当前包含键值: [{}]\n(如需提取具体数据进行处理，请在 JSON 中设置 \"require_memory\": true，下一轮将全量展开)\n\n",
                    keys.join(", ")
                ));
            }
        }

        // (6) 任务清单 (Todo List)
        let todo_json = serde_json::to_string_pretty(&self.todo_list).unwrap_or_default();
        bottom_state_card.push_str(&format!("【当前任务面板 (Todo List)】\n{}\n\n", todo_json));

        messages.push(ChatMessage {
            role: "user".to_string(),
            content: json!(bottom_state_card.trim_end()),
        });

        // (7) 当前最新环境观测 (Observation)
        if !self.current_observation.is_empty() {
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: json!(format!(
                    "【当前最新环境观测 (Observation)】\n(⚠️系统提示：这是你刚才操作后获取的最新的完整数据！请直接从这里读取你需要的信息，不要重复请求！)\n{}",
                    self.current_observation
                )),
            });
        }

        // (8) 最终行动约束与决策指令
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: json!("请基于上述所有上下文环境，思考并执行下一步操作。只能输出纯文本 JSON（禁止裹 ```），tool 名必须严格符合白名单。注意：你必须先在 reflection 字段审视上一步的结果与当前环境。")
        });

        messages
    }
}
