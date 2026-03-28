use serde::{Deserialize, Serialize};
use std::collections::{VecDeque, HashMap};
use crate::agent::types::{ChatMessage, TodoItem, ShortMemory, MemoryItem};
use crate::agent::utils::chars_preview;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandwichContext {
    pub system_prompt: String,
    pub ultimate_goal: String,
    pub todo_list: Vec<TodoItem>,
    pub short_memory: VecDeque<ShortMemory>,
    pub memory_window_size: usize,
    pub current_observation: String,
    pub memories: HashMap<String, String>,
    /// AI 上一轮选择的工具名（用于按需加载说明书）
    #[serde(default)]
    pub active_tool: Option<String>,
    /// 当前活跃工具的详细 schema/说明（动态注入）
    #[serde(default)]
    pub active_tool_detail: String,
}

impl SandwichContext {
    pub fn new(system_prompt: String, ultimate_goal: String) -> Self {
        Self {
            system_prompt,
            ultimate_goal,
            todo_list: Vec::new(),
            short_memory: VecDeque::new(),
            memory_window_size: 15,
            current_observation: String::new(),
            memories: HashMap::new(),
            active_tool: None,
            active_tool_detail: String::new(),
        }
    }

    pub fn update_memories(&mut self, items: Vec<MemoryItem>) {
        for item in items {
            self.memories.insert(item.key, item.value);
        }
    }

    pub fn push_memory(&mut self, mem: ShortMemory) {
        if self.short_memory.len() >= self.memory_window_size {
            self.short_memory.pop_front();
        }
        self.short_memory.push_back(mem);
    }

    pub fn update_observation(&mut self, obs: String) {
        self.current_observation = obs;
    }

    pub fn assemble_messages(&self) -> Vec<ChatMessage> {
        let mut messages = Vec::new();
        
        // --- 1. 核心逻辑提示词 ---
        let mut full_system = self.system_prompt.clone();
        
        // --- 2. 动态加载当前工具的详细说明书 ---
        // active_tool_detail 由 mod.rs 主循环在每一步之前注入
        if !self.active_tool_detail.is_empty() {
            full_system.push_str("\n\n<active_tool_instructions>\n");
            full_system.push_str(&self.active_tool_detail);
            full_system.push_str("\n</active_tool_instructions>");
        }
        
        messages.push(ChatMessage { role: "system".to_string(), content: full_system });
        messages.push(ChatMessage { role: "user".to_string(), content: format!("【终极目标】\n{}", self.ultimate_goal) });

        let todo_json = serde_json::to_string_pretty(&self.todo_list).unwrap_or_else(|_| "[]".to_string());
        let memory_lines: Vec<String> = self.short_memory.iter().map(|m| {
            format!("  Step#{} [{}] {} -> {} | 成功: {}", m.step_id, m.tool, m.command, chars_preview(&m.output_summary, 1000), m.success)
        }).collect();

        // 核心事实 (Absolute Facts)
        let facts_section = if self.memories.is_empty() {
            String::new()
        } else {
            let facts: Vec<String> = self.memories.iter().map(|(k, v)| format!("  {}: {}", k, v)).collect();
            format!("\n\n【核心事实与数据 (Absolute Facts)】\n{}", facts.join("\n"))
        };

        let middle = format!("【任务面板】\n{}\n\n【近期记忆】\n{}{}", todo_json, if memory_lines.is_empty() { "无".to_string() } else { memory_lines.join("\n") }, facts_section);
        messages.push(ChatMessage { role: "user".to_string(), content: middle });

        if !self.current_observation.is_empty() {
            messages.push(ChatMessage { role: "user".to_string(), content: format!("【当前观测】\n{}", self.current_observation) });
        }

        messages.push(ChatMessage {
            role: "user".to_string(),
            content: "请基于以上上下文执行下一步。只返回 JSON 格式：{\"thought\":\"...\",\"action\":\"...\",\"params\":{}, \"todo_update\":[]}".to_string()
        });

        messages
    }
}
