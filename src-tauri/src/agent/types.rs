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
pub struct AgentInstruction {
    pub thought: String,
    pub action: String,
    pub params: serde_json::Value,
    #[serde(default)]
    pub todo_update: Vec<TodoItem>,
}

#[derive(Debug)]
pub struct DispatchResult {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    pub route: String,
}
