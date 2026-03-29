use serde::{Deserialize, Serialize};
use serde_json::json;
use crate::agent::types::{AgentInstruction, TodoItem, HistoryStep};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
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
        }
    }

    pub fn add_step(&mut self, instruction: &AgentInstruction, output_summary: &str) {
        let step = HistoryStep {
            thought: instruction.thought.clone(),
            description: instruction.description.clone(),
            tool: instruction.get_tool(),
            command: instruction.get_action(),
            output_summary: output_summary.chars().take(1000).collect(), 
        };
        
        // 我们存入 ChatHistory，AI 就能看到历史
        let msg = ChatMessage {
            role: "assistant".to_string(),
            content: serde_json::to_string(instruction).unwrap_or_default()
        };
        self.turns_history.push(msg);
        
        let feedback = ChatMessage {
            role: "user".to_string(),
            content: format!("【执行结果】\n{}", output_summary)
        };
        self.turns_history.push(feedback);
        
        // 滑动窗口：只保留最近 15 步 (30 条对话)，防止上下文爆炸
        if self.turns_history.len() > 30 {
            self.turns_history = self.turns_history[self.turns_history.len()-30..].to_vec();
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

    /// 注入错误反馈信息，告知 AI 修正它的输出
    pub fn add_error_feedback(&mut self, error_msg: &str) {
        let msg = ChatMessage {
            role: "user".to_string(),
            content: format!("【❌ 格式或执行错误】\n{}\n请检查格式规则（勿用 XML/```），修正后立即重新输出正确 JSON。", error_msg)
        };
        self.turns_history.push(msg);
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
            content: full_system
        });

        // 2. 将历史对话加入其中
        messages.extend(self.turns_history.clone());

        // 3. 将当前任务目标和面板信息放在末尾（首尾增强）
        let todo_json = serde_json::to_string_pretty(&self.todo_list).unwrap_or_default();
        let facts_section = if self.memories.is_empty() {
            String::new()
        } else {
            let facts: Vec<String> = self.memories.iter().map(|(k, v)| format!("  {}: {}", k, v)).collect();
            format!("\n\n【核心事实与数据 (Absolute Facts)】\n{}", facts.join("\n"))
        };

        let middle = format!("【用户终极目标】\n{}\n\n【任务面板】\n{}\n\n【近期记忆】\n(以上是历史对话){}", self.user_goal, todo_json, facts_section);
        messages.push(ChatMessage { role: "user".to_string(), content: middle });

        if !self.current_observation.is_empty() {
            messages.push(ChatMessage { role: "user".to_string(), content: format!("【当前实时观测】\n{}", self.current_observation) });
        }

        messages.push(ChatMessage {
            role: "user".to_string(),
            content: "请基于提示执行下一步操作。只能输出纯文本 JSON（禁止裹 ```），tool 名必须严格符合白名单。".to_string()
        });

        messages
    }
}
