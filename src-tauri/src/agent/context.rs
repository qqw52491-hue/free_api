use crate::agent::task_store::TaskResourceRef;
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
    /// 当前过程树的活动计划节点；随 checkpoint 序列化，以便恢复后继续归属。
    #[serde(default)]
    pub active_plan_id: Option<String>,
    #[serde(default)]
    pub active_plan_title: Option<String>,
    #[serde(default)]
    pub next_plan_index: usize,
    /// 仅保存 session 任务数据区的目录；资源记录本体不进入 LLM 上下文。
    #[serde(default)]
    pub task_resources: Vec<TaskResourceRef>,

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
            active_plan_id: None,
            active_plan_title: None,
            next_plan_index: 0,
            task_resources: Vec::new(),
            active_tool: None,
            active_tool_detail: String::new(),
            carry_memories: false,
        }
    }

    /// 将指令归属到稳定计划节点。模型未填字段时继续最近计划；只有显式切换标题或 id 才创建新父节点。
    pub fn resolve_plan(&mut self, instruction: &AgentInstruction) -> (String, String) {
        let requested_id = instruction
            .plan_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let requested_title = instruction
            .plan_title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if let Some(id) = requested_id {
            let title = requested_title
                .or_else(|| self.active_plan_title.clone())
                .unwrap_or_else(|| self.user_goal.clone());
            self.active_plan_id = Some(id.clone());
            self.active_plan_title = Some(title.clone());
            return (id, title);
        }
        if let Some(title) = requested_title {
            if self.active_plan_title.as_deref() == Some(title.as_str()) {
                if let Some(id) = self.active_plan_id.clone() {
                    return (id, title);
                }
            }
            self.next_plan_index = self.next_plan_index.saturating_add(1);
            let id = format!("plan-{}", self.next_plan_index);
            self.active_plan_id = Some(id.clone());
            self.active_plan_title = Some(title.clone());
            return (id, title);
        }
        if let (Some(id), Some(title)) =
            (self.active_plan_id.clone(), self.active_plan_title.clone())
        {
            return (id, title);
        }
        self.next_plan_index = self.next_plan_index.saturating_add(1);
        let id = format!("plan-{}", self.next_plan_index);
        let title = self.user_goal.clone();
        self.active_plan_id = Some(id.clone());
        self.active_plan_title = Some(title.clone());
        (id, title)
    }

    pub fn add_step(&mut self, instruction: &AgentInstruction, output_summary: &str) {
        // 脱水版行动纪要：只记录"做了什么"，砍掉 thought/reflection 内心戏，大幅节省 Token
        // AI 下一步只需要知道历史里发生了什么动作，完全不需要重读上一轮的心理分析
        let action_brief = format!(
            "计划: {} | 工具: {} | 动作: {}",
            self.active_plan_title.as_deref().unwrap_or("未命名计划"),
            instruction.get_tool(),
            instruction.description,
        );
        let msg = ChatMessage {
            role: "assistant".to_string(),
            content: json!(action_brief),
        };
        self.turns_history.push(msg);

        // 历史对话中的执行结果采用智能 Head + Tail 采样（头 300 + 尾 500 + 关键报错行保留）
        let mut history_feedback = output_summary.to_string();
        if let Some(idx) = history_feedback.find("【可用元素清单】") {
            history_feedback
                .replace_range(idx.., "【可用元素清单】: (已折叠，详见底部当前最新观测)");
        } else {
            history_feedback = crate::agent::utils::sample_execution_log(&history_feedback, 300, 500);
        }

        let feedback = ChatMessage {
            role: "user".to_string(),
            content: json!(format!("【历史执行结果摘要】\n{}", history_feedback)),
        };
        self.turns_history.push(feedback);

        // 水位线滞后压缩机制 (Hysteresis Auto-Compact)：
        // 当历史消息达到高水位线 40 条（20 个 Step）时，触发一次批量压缩，将最旧的 20 条消息压缩为紧凑单行定型摘要，
        // 历史长度回落至低水位线 20 条，随后进入连续 20 步的纯 Append-only 缓存安全期（期间前缀缓存 100% 命中！）。
        if self.turns_history.len() >= 40 {
            self.compact_old_history(20);
        }
    }

    /// 水位线批量压缩老历史记录为不可变的定型单行摘要
    pub fn compact_old_history(&mut self, compact_count: usize) {
        if self.turns_history.len() <= compact_count {
            return;
        }
        let to_compact: Vec<ChatMessage> = self.turns_history.drain(0..compact_count).collect();
        let mut compacted_lines = Vec::new();
        for msg in to_compact {
            let content_str = match &msg.content {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let first_line = content_str.lines().next().unwrap_or("").trim();
            if !first_line.is_empty() {
                let line_preview: String = first_line.chars().take(120).collect();
                compacted_lines.push(format!("- [{}] {}", msg.role, line_preview));
            }
        }

        if !compacted_lines.is_empty() {
            let compact_card = ChatMessage {
                role: "user".to_string(),
                content: json!(format!(
                    "【历史早期执行归档 (已定型压缩)】\n{}\n(早期执行摘要已归档，当前窗口专注于近期步骤与终极目标)",
                    compacted_lines.join("\n")
                )),
            };
            self.turns_history.insert(0, compact_card);
        }
    }

    pub fn set_task_resources(&mut self, resources: Vec<TaskResourceRef>) {
        self.task_resources = resources;
    }

    pub fn update_memories(&mut self, updates: Vec<crate::agent::types::MemoryItem>) {
        for item in updates {
            if item.key.trim().is_empty() || item.value.trim().is_empty() {
                println!("⚠️ 跳过缺少 key 或 value 的无效记忆项");
                continue;
            }
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
