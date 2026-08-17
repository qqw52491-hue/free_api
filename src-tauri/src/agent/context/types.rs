use crate::agent::task_store::TaskResourceRef;
use crate::agent::types::{AgentInstruction, TodoItem};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: String,
    pub content: serde_json::Value, // 支持 String 或 Array (多模态)
}

/// 失败教训记录（用于防止大模型在长任务中重复尝试已知失败的方法）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FailedAttempt {
    pub step_id: usize,
    pub tool: String,
    pub action: String,
    pub error_summary: String,
    #[serde(default)]
    pub output_id: Option<String>,
}

/// 不可逆动作台账（已执行的破坏性或外部影响操作，绝不淘汰）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SideEffectRecord {
    pub step_id: usize,
    pub tool: String,
    pub description: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandwichContext {
    pub system_prompt: String,
    pub user_goal: String,
    /// 核心记忆库：严格采用 BTreeMap 保证跨平台与跨进程序列化字节绝对确定！
    pub memories: BTreeMap<String, String>,
    pub failed_attempts: Vec<FailedAttempt>,
    pub side_effects: Vec<SideEffectRecord>,
    pub todo_list: Vec<TodoItem>,
    pub turns_history: Vec<ChatMessage>,
    pub current_observation: String,
    pub progress_summary: String,
    #[serde(default)]
    pub active_plan_id: Option<String>,
    #[serde(default)]
    pub active_plan_title: Option<String>,
    #[serde(default)]
    pub next_plan_index: usize,
    #[serde(default)]
    pub task_resources: Vec<TaskResourceRef>,
    pub active_tool: Option<String>,
    pub active_tool_detail: String,
    pub carry_memories: bool,
    /// 当前任务所用模型的上下文窗口（tokens）。
    #[serde(default)]
    pub context_window_tokens: usize,
}

impl SandwichContext {
    pub fn new(system_prompt: String, user_goal: String) -> Self {
        Self {
            system_prompt,
            user_goal,
            memories: BTreeMap::new(),
            failed_attempts: Vec::new(),
            side_effects: Vec::new(),
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
            context_window_tokens: 0,
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

    pub fn set_task_resources(&mut self, resources: Vec<TaskResourceRef>) {
        self.task_resources = resources;
    }

    pub fn update_memory(&mut self, key: String, value: String) {
        if !key.trim().is_empty() && !value.trim().is_empty() {
            self.memories.insert(key, value);
        }
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

    pub fn record_failed_attempt(
        &mut self,
        step_id: usize,
        tool: String,
        action: String,
        error_summary: String,
        output_id: Option<String>,
    ) {
        self.failed_attempts.push(FailedAttempt {
            step_id,
            tool,
            action,
            error_summary,
            output_id,
        });
        if self.failed_attempts.len() > 6 {
            self.failed_attempts.remove(0);
        }
    }

    pub fn record_side_effect(
        &mut self,
        step_id: usize,
        tool: String,
        description: String,
    ) {
        self.side_effects.push(SideEffectRecord {
            step_id,
            tool,
            description,
            timestamp: chrono::Utc::now().to_rfc3339(),
        });
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
}
