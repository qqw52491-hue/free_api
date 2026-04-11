use crate::agent::types::{AgentInstruction, HistoryStep, TodoItem};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: serde_json::Value, // 改为 Value，支持 String 或 Array (多模态)
}

/// 精简的动作摘要，用于在底部面包片注入可读的动作轨迹
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionSummary {
    pub step: usize,
    pub action: String,
    pub description: String,
    pub success: bool,
    pub output_preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandwichContext {
    pub system_prompt: String,
    pub user_goal: String,
    pub memories: HashMap<String, String>,
    pub todo_list: Vec<TodoItem>,
    pub turns_history: Vec<ChatMessage>,
    pub current_observation: String,

    // --- 动态载入的工具详情 ---
    pub active_tool: Option<String>,
    pub active_tool_detail: String,

    // --- Token 优化：是否要在本轮携带全部记忆内容 ---
    pub carry_memories: bool,

    // --- 结构化动作轨迹（用于循环检测和强注入） ---
    #[serde(default)]
    pub action_history: Vec<ActionSummary>,
    #[serde(default)]
    pub step_counter: usize,
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
            active_tool: None,
            active_tool_detail: String::new(),
            carry_memories: false,
            action_history: Vec::new(),
            step_counter: 0,
        }
    }

    pub fn add_step(&mut self, instruction: &AgentInstruction, output_summary: &str) {
        self.step_counter += 1;

        let _step = HistoryStep {
            thought: instruction.thought.clone(),
            description: instruction.description.clone(),
            tool: instruction.get_tool(),
            command: instruction.get_action(),
            output_summary: output_summary.chars().take(1000).collect(),
        };

        // 记录结构化动作摘要（用于底部面包片和循环检测）
        let is_success = !output_summary.starts_with("❌");
        let action_str = format!("{}:{}", instruction.get_tool(), instruction.get_action());
        let preview: String = output_summary
            .lines()
            .take(2)
            .collect::<Vec<&str>>()
            .join(" ")
            .chars()
            .take(120)
            .collect();

        self.action_history.push(ActionSummary {
            step: self.step_counter,
            action: action_str,
            description: instruction.description.clone(),
            success: is_success,
            output_preview: preview,
        });

        // 只保留最近 10 条动作摘要
        if self.action_history.len() > 10 {
            self.action_history = self.action_history[self.action_history.len() - 10..].to_vec();
        }

        // 我们存入 ChatHistory，AI 就能看到历史
        let msg = ChatMessage {
            role: "assistant".to_string(),
            content: json!(serde_json::to_string(instruction).unwrap_or_default()),
        };
        self.turns_history.push(msg);

        // 为防止历史上下文爆炸导致 Ollama 超出 4096 token 而阶段提示词，历史对话中的执行结果应该被极致压缩。
        // 凡是带有“【可用元素清单】”的长 DOM 树全部削减，因为当前最新 DOM 已经在 `current_observation` 底部了。
        let mut history_feedback = output_summary.to_string();
        if let Some(idx) = history_feedback.find("【可用元素清单】") {
            history_feedback.replace_range(
                idx..,
                "【可用元素清单】: (已折叠，详见底部当前最新观测)"
            );
        } else {
            // 普通日志截断到 300 字符
            history_feedback = history_feedback.chars().take(300).collect();
        }

        let feedback = ChatMessage {
            role: "user".to_string(),
            content: json!(format!("【历史执行结果摘要】\n{}", history_feedback)),
        };
        self.turns_history.push(feedback);

        // 滑动窗口：只保留最近 15 步 (30 条对话)，进一步严防死守
        if self.turns_history.len() > 30 {
            self.turns_history = self.turns_history[self.turns_history.len() - 30..].to_vec();
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

    /// 注入错误反馈信息，告知 AI 修正它的输出
    pub fn add_error_feedback(&mut self, error_msg: &str) {
        let msg = ChatMessage {
            role: "user".to_string(),
            content: json!(format!("【❌ 格式或执行错误】\n{}\n请检查格式规则（勿用 XML/```），修正后立即重新输出正确 JSON。", error_msg))
        };
        self.turns_history.push(msg);
    }

    /// 生成人类可读的动作轨迹文本（注入到底部面包片）
    fn format_action_trajectory(&self) -> String {
        if self.action_history.is_empty() {
            return String::from("（暂无历史动作，这是你的第一步）");
        }

        let mut lines = Vec::new();
        for entry in &self.action_history {
            let status = if entry.success { "✅" } else { "❌" };
            lines.push(format!(
                "  Step {}: {} {} → {} | {}",
                entry.step, status, entry.action, entry.description, entry.output_preview
            ));
        }

        lines.join("\n")
    }

    /// 检测是否存在循环模式（连续 N 次相同动作）
    fn detect_loop_warning(&self) -> Option<String> {
        if self.action_history.len() < 2 {
            return None;
        }

        let recent = &self.action_history;
        let last = &recent[recent.len() - 1];

        // 检测连续相同动作
        let mut consecutive_same = 0;
        for entry in recent.iter().rev() {
            if entry.action == last.action {
                consecutive_same += 1;
            } else {
                break;
            }
        }

        if consecutive_same >= 3 {
            return Some(format!(
                "🚨🚨🚨 【系统强制警告：检测到死循环！】\n你已连续 {} 次执行相同动作 \"{}\"！\n你必须在 reflection 中承认循环，并立刻执行以下任一操作：\n1. 直接构造搜索 URL（如 https://xxx.com/search?q=关键词）跳转\n2. 换一个完全不同的网站\n3. 调用 finish 终止任务\n绝对禁止再次执行 \"{}\"！",
                consecutive_same, last.action, last.action
            ));
        }

        if consecutive_same >= 2 {
            return Some(format!(
                "⚠️ 【系统提醒：疑似循环】你已连续 {} 次执行 \"{}\"。如果下一步还是相同动作，系统将强制判定为死循环。请立即更换策略！",
                consecutive_same, last.action
            ));
        }

        None
    }

    pub fn assemble_messages(&self) -> Vec<ChatMessage> {
        let mut messages = Vec::new();

        // 1. 系统角色提示词 (核心逻辑 + 动态模块)
        let mut full_system = self.system_prompt.clone();
        if !self.active_tool_detail.is_empty() {
            full_system.push_str("\n\n【当前活跃工具操作手册】\n");
            full_system.push_str(&self.active_tool_detail);
        }

        messages.push(ChatMessage {
            role: "system".to_string(),
            content: json!(full_system),
        });

        // 2. 将历史对话加入其中
        messages.extend(self.turns_history.clone());

        // 3. 将当前任务目标和面板信息放在末尾（首尾增强）
        let todo_json = serde_json::to_string_pretty(&self.todo_list).unwrap_or_default();
        let facts_section = if self.memories.is_empty() {
            String::new()
        } else if self.carry_memories {
            // 这里是 AI 申请了，由我们大方给出的全部细节
            let facts: Vec<String> = self
                .memories
                .iter()
                .map(|(k, v)| format!("  {}: {}", k, v))
                .collect();
            format!(
                "\n\n【核心事实与数据 (载入全部细节)】\n{}",
                facts.join("\n")
            )
        } else {
            // 这里是默认状态：只给一个 Key 列表，不给数据内容
            let keys: Vec<String> = self.memories.keys().cloned().collect();
            format!(
                "\n\n【冷存储档案库 (载入内容请设 require_memory: true)】\n索引清单: [{}]",
                keys.join(", ")
            )
        };

        // 4. 生成动作轨迹和循环预警
        let trajectory = self.format_action_trajectory();
        let loop_warning = self.detect_loop_warning().unwrap_or_default();

        let middle = format!(
            "【用户终极目标】\n{}\n\n【任务面板】\n{}\n\n【你的动作历史轨迹 (不可篡改，你必须审查！)】\n{}\n{}{}\n\n【近期记忆】\n(以上是历史对话){}",
            self.user_goal,
            todo_json,
            trajectory,
            if loop_warning.is_empty() { "" } else { "\n" },
            loop_warning,
            facts_section
        );
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: json!(middle),
        });

        if !self.current_observation.is_empty() {
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: json!(format!("【当前实时观测】\n{}", self.current_observation)),
            });
        }

        messages.push(ChatMessage {
            role: "user".to_string(),
            content: json!("请基于提示执行下一步操作。只能输出纯文本 JSON（禁止裹 ```），tool 名必须严格符合白名单。注意：你必须先填 reflection 字段审视上一步结果。")
        });

        messages
    }
}
