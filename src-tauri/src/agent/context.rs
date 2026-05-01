use crate::agent::types::{AgentInstruction, TodoItem};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: serde_json::Value, // 改为 Value，支持 String 或 Array (多模态)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandwichContext {
    pub system_prompt: String,
    pub user_goal: String,
    pub memories: HashMap<String, String>,
    pub todo_list: Vec<TodoItem>,
    pub turns_history: Vec<ChatMessage>,
    pub current_observation: String,
    pub progress_summary: String,

    // --- 动态载入的工具详情 ---
    pub active_tool: Option<String>,
    pub active_tool_detail: String,

    pub carry_memories: bool,
}

impl SandwichContext {
    pub fn new(system_prompt: String, user_goal: String) -> Self {
        Self {
            system_prompt,
            user_goal,
            memories: HashMap::new(),
            todo_list: Vec::new(),
            turns_history: Vec::new(),
            current_observation: String::new(),
            progress_summary: String::new(),
            active_tool: None,
            active_tool_detail: String::new(),
            carry_memories: false,
        }
    }

    pub fn add_step(&mut self, instruction: &AgentInstruction, output_summary: &str) {
        // 脱水版行动纪要：只记录"做了什么"，砍掉 thought/reflection 内心戏，大幅节省 Token
        // AI 下一步只需要知道历史里发生了什么动作，完全不需要重读上一轮的心理分析
        let action_brief = format!(
            "工具: {} | 动作: {}",
            instruction.get_tool(),
            instruction.description,
        );
        let msg = ChatMessage {
            role: "assistant".to_string(),
            content: json!(action_brief),
        };
        self.turns_history.push(msg);

        // 为防止历史上下文爆炸导致 Ollama 超出 4096 token 而阶段提示词，历史对话中的执行结果应该被极致压缩。
        // 凡是带有“【可用元素清单】”的长 DOM 树全部削减，因为当前最新 DOM 已经在 `current_observation` 底部了。
        let mut history_feedback = output_summary.to_string();
        if let Some(idx) = history_feedback.find("【可用元素清单】") {
            history_feedback
                .replace_range(idx.., "【可用元素清单】: (已折叠，详见底部当前最新观测)");
        } else {
            // 普通日志截断到 300 字符
            history_feedback = history_feedback.chars().take(300).collect();
        }

        let feedback = ChatMessage {
            role: "user".to_string(),
            content: json!(format!("【历史执行结果摘要】\n{}", history_feedback)),
        };
        self.turns_history.push(feedback);

        // 滑动窗口：作为最后防线保留 40 条。主要依靠 AI 总结并设置 clear_history = true 来主动清理
        if self.turns_history.len() > 40 {
            self.turns_history = self.turns_history[self.turns_history.len() - 40..].to_vec();
        }
    }

    pub fn update_memories(&mut self, updates: Vec<crate::agent::types::MemoryItem>) {
        for item in updates {
            self.memories.insert(item.key, item.value);
        }
    }

    pub fn update_observation(&mut self, obs: String) {
        self.current_observation = obs;
    }


    /// 注入截图/图片反馈信息 (多模态)
    pub fn add_image_feedback(&mut self, text: &str, base64_image: &str) {
        let msg = ChatMessage {
            role: "user".to_string(),
            content: json!([
                { "type": "text", "text": text },
                {
                    "type": "image_url",
                    "image_url": {
                        "url": format!("data:image/jpeg;base64,{}", base64_image)
                    }
                }
            ]),
        };
        self.turns_history.push(msg);
    }

    /// 注入 AI 原始输出记录（用于保存失败时的错误输出，避免 AI 失忆死循环）
    pub fn add_assistant_raw_message(&mut self, text: &str) {
        let msg = ChatMessage {
            role: "assistant".to_string(),
            content: json!(text),
        };
        self.turns_history.push(msg);
    }

    /// 注入错误反馈信息，告知 AI 修正它的输出
    pub fn add_error_feedback(&mut self, error_msg: &str) {
        let msg = ChatMessage {
            role: "user".to_string(),
            content: json!(format!(
                "【❌ 格式或执行错误】\n{}\n\
                🚨 警告：如果是找不到元素或页面发生过跳转，说明你眼前的 DOM 清单已失效！你必须立刻使用 `extract` 命令重新获取最新页面元素，绝对禁止继续盲目尝试点击其他旧 ID！\n\
                请检查格式规则（勿用 XML/```），修正后立即重新输出正确 JSON。", 
                error_msg
            ))
        };
        self.turns_history.push(msg);
    }

    pub fn assemble_messages(&self) -> Vec<ChatMessage> {
        let mut messages = Vec::new();

        // 1. 系统角色提示词 (核心逻辑，包含全部静态工具菜单)
        // 完全静态，位于最前排，完美命中核心缓存区
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: json!(self.system_prompt.clone()),
        });

        // 2. 静态前置上下文 (用户目标 + 长期记忆库)
        // 目标在单次任务中不会变，Memory 变动频率很低，也放入前缀缓存区
        let mut static_prefix = format!("【用户终极任务目标】\n{}\n\n", self.user_goal);

        if !self.progress_summary.is_empty() {
            static_prefix.push_str(&format!(
                "【全局进度与策略摘要 (Progress Summary)】\n{}\n\n",
                self.progress_summary
            ));
        }

        if !self.memories.is_empty() {
            if self.carry_memories {
                let facts: Vec<String> = self
                    .memories
                    .iter()
                    .map(|(k, v)| format!("  {}: {}", k, v))
                    .collect();
                static_prefix.push_str(&format!(
                    "【冷存储记忆库 (已全量载入)】\n{}\n\n",
                    facts.join("\n")
                ));
            } else {
                let keys: Vec<String> = self.memories.keys().cloned().collect();
                static_prefix.push_str(&format!(
                    "【冷存储记忆库索引 (当前已折叠)】\n当前包含键值: [{}]\n(如需提取具体数据进行处理，请在 JSON 中设置 \"require_memory\": true，下一轮将全量展开)\n\n",
                    keys.join(", ")
                ));
            }
        }

        messages.push(ChatMessage {
            role: "user".to_string(),
            content: json!(static_prefix.trim_end()),
        });

        // 2.5 低频工具操作手册 (关键缓存优化！)
        // 放在 static_prefix 之后、历史对话之前：
        // - 同一工具连续执行多步时，此消息一字不差 → 前缀缓存完美命中
        // - 换工具时此消息会变，但只影响从此往后的缓存，static_prefix 和 system_prompt 依然命中
        // - 放在历史对话之后（之前的做法）会导致每轮都因历史变长而缓存失效！
        if !self.active_tool_detail.is_empty() {
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: json!(format!(
                    "【当前活跃工具操作手册】\n{}\n",
                    self.active_tool_detail
                )),
            });
        }

        // 3. 线性增长区：历史对话轨迹
        // 随着执行一步步向后追加 (Append-only)，这正是当前各大模型 Prefix Caching 能够完美覆盖的部分
        messages.extend(self.turns_history.clone());

        // 4. 动态尾部区：极高频变动，放在最末尾避免污染前面的静态缓存
        let mut dynamic_suffix = String::new();

        if self.turns_history.len() >= 24 {
            dynamic_suffix.push_str("🚨 【系统严重警告：历史记录即将溢出】\n你的对话历史已过长，继续执行将导致上下文超载崩溃！\n请务必在本次思考中，将前面所有的关键进展、线索和数据提炼总结，放入 `memories_update` 中永久保存。同时必须在 JSON 顶层输出 `\"clear_history\": true` 来清空历史负担！如果不清空，系统可能会强制截断导致你失忆！\n\n");
        }

        let todo_json = serde_json::to_string_pretty(&self.todo_list).unwrap_or_default();
        dynamic_suffix.push_str(&format!("【当前任务面板 (Todo List)】\n{}\n\n", todo_json));

        messages.push(ChatMessage {
            role: "user".to_string(),
            content: json!(dynamic_suffix),
        });

        // 4.5 独立突出最新观测 (Observation)，防止被上面 10k 字的工具手册淹没
        if !self.current_observation.is_empty() {
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: json!(format!(
                    "【当前最新环境观测 (Observation)】\n(⚠️系统提示：这是你刚才操作后获取的最新的、未折叠的完整数据！请直接从这里读取你需要的信息，不要重复请求！)\n{}",
                    self.current_observation
                )),
            });
        }

        // 5. 最后的强制指令
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: json!("请基于上述所有上下文环境，思考并执行下一步操作。只能输出纯文本 JSON（禁止裹 ```），tool 名必须严格符合白名单。注意：你必须先在 reflection 字段审视上一步的结果与当前环境。")
        });

        messages
    }
}
