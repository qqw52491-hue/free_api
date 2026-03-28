use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStep {
    pub id: usize,
    pub description: String,
    pub thought: String,
    pub tool: String,
    pub command: String,
    pub status: String, // "pending" | "running" | "done" | "error"
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MainContent {
    pub total_objective: String,
    pub abstrack_task: String,
    pub tool_choose: String,
    pub current_message: String,
    pub now_content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: usize,
    pub status: String, // "pending" | "in_progress" | "done" | "failed"
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShortMemory {
    pub step_id: usize,
    pub tool: String,
    pub command: String,
    pub output_summary: String,
    pub success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryItem {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentInstruction {
    pub thought: String,
    #[serde(default)]
    pub description: String,
    
    // 兼容 tool / action
    pub tool: Option<String>,
    pub action: Option<String>,
    
    // 兼容 command / params
    pub command: Option<serde_json::Value>,
    pub params: Option<serde_json::Value>,

    #[serde(default)]
    pub todo_update: Vec<TodoItem>,
    #[serde(default)]
    pub memories_update: Vec<MemoryItem>,
    /// 【预加载优化】AI 预告下一轮想用的工具，系统会提前加载该工具的详细说明书
    #[serde(default)]
    pub next_tool_hint: Option<String>,
}

impl AgentInstruction {
    pub fn get_action(&self) -> String {
        self.tool.clone()
            .or_else(|| self.action.clone())
            .unwrap_or_default()
    }
    pub fn get_params(&self) -> serde_json::Value {
        self.command.clone()
            .or_else(|| self.params.clone())
            .unwrap_or(serde_json::Value::Null)
    }
}

#[derive(Debug)]
pub struct DispatchResult {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    pub route: String,
}
