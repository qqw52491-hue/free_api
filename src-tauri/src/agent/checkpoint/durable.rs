//! DurableAgentContext 与提示词差分/合并辅助。

use super::*;
use crate::agent::context::SandwichContext;
use crate::agent::control::HumanControl;
use crate::agent::types::{AgentInstruction, DispatchResult};
use crate::db::DbState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tauri::AppHandle;

/// 只保存继续任务所需的事实；system prompt、连接、工具详情和大对象都在恢复时重建。
#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct DurableAgentContext {
    pub user_goal: String,
    /// 当前轮次任务目标（与 M1 锚点 user_goal 分离；旧 checkpoint 缺字段时
    /// 恢复回退 user_goal，二者在单轮语义下本就一致）。
    #[serde(default)]
    pub current_goal: String,
    pub memories: std::collections::BTreeMap<String, String>,
    /// 任务数据区目录，数据行仍由 agent_task_records 独立实时持久化。
    #[serde(default)]
    pub task_resources: Vec<TaskResourceRef>,
    pub todo_list: Vec<TodoItem>,
    #[serde(default)]
    pub review_contract: Option<ReviewContract>,
    /// 最近一批真实写入的验证状态；旧 checkpoint 缺字段时默认 None。
    #[serde(default)]
    pub change_verification: Option<ChangeVerification>,
    pub turns_history: Vec<ChatMessage>,
    /// 避坑教训；嵌入字段以兼容旧 checkpoint JSON。
    #[serde(default)]
    pub failed_attempts: Vec<FailedAttempt>,
    #[serde(default)]
    pub side_effects: Vec<SideEffectRecord>,
    #[serde(default)]
    pub inherited_side_effects_count: usize,
    pub progress_summary: String,
    pub observation_summary: String,
    pub active_tool: Option<String>,
    #[serde(default)]
    pub epoch_system_prompt: Option<String>,
    #[serde(default)]
    pub last_reported_prompt_tokens: Option<usize>,
    #[serde(default)]
    pub last_reported_history_estimate_tokens: Option<usize>,
    #[serde(default)]
    pub last_persisted_summary: Option<String>,
    #[serde(default)]
    pub persisted_summary_revision: u64,
    #[serde(default)]
    pub needs_observation_refresh: bool,
}

impl DurableAgentContext {
    pub fn capture(context: &SandwichContext) -> Self {
        Self {
            user_goal: context.user_goal.clone(),
            current_goal: context.effective_goal().to_string(),
            memories: context.memories.clone(),
            task_resources: context.task_resources.clone(),
            todo_list: context.todo_list.clone(),
            review_contract: context.review_contract.clone(),
            change_verification: context.change_verification.clone(),
            turns_history: context.turns_history.clone(),
            failed_attempts: context.failed_attempts.clone(),
            side_effects: context.side_effects.clone(),
            inherited_side_effects_count: context.inherited_side_effects_count,
            progress_summary: context.progress_summary.clone(),
            observation_summary: truncate_chars(
                &context.current_observation,
                MAX_OBSERVATION_CHARS,
            ),
            active_tool: context.active_tool.clone(),
            epoch_system_prompt: context
                .epoch_system_prompt
                .clone()
                .or_else(|| Some(context.system_prompt.clone())),
            last_reported_prompt_tokens: context.last_reported_prompt_tokens,
            last_reported_history_estimate_tokens: context.last_reported_history_estimate_tokens,
            last_persisted_summary: context.last_persisted_summary.clone(),
            persisted_summary_revision: context.persisted_summary_revision,
            // 浏览器/DOM 可能已变化，恢复后不能相信旧观测。
            needs_observation_refresh: !context.current_observation.is_empty(),
        }
    }

    pub fn restore_into(&self, context: &mut SandwichContext) {
        context.user_goal = self.user_goal.clone();
        // 旧 checkpoint 无 current_goal 字段（反序列化为空串）→ 回退锚点，语义等价
        context.current_goal = if self.current_goal.is_empty() {
            self.user_goal.clone()
        } else {
            self.current_goal.clone()
        };
        context.memories = self.memories.clone();
        context.task_resources = self.task_resources.clone();
        context.todo_list = self.todo_list.clone();
        context.review_contract = self.review_contract.clone();
        context.change_verification = self.change_verification.clone();
        context.turns_history = self.turns_history.clone();
        context.failed_attempts = self.failed_attempts.clone();
        context.side_effects = self.side_effects.clone();
        context.inherited_side_effects_count = self.inherited_side_effects_count;
        context.progress_summary = self.progress_summary.clone();
        context.current_observation = self.observation_summary.clone();
        context.active_tool = self.active_tool.clone();
        context.last_reported_prompt_tokens = self.last_reported_prompt_tokens;
        context.last_reported_history_estimate_tokens = self.last_reported_history_estimate_tokens;
        context.last_persisted_summary = self.last_persisted_summary.clone();
        context.persisted_summary_revision = self.persisted_summary_revision;

        // 🌟 统一不变性保护：保持已有会话的 M0 顶层系统提示词绝对恒定，稳吃 Prefix Cache！
        if let Some(baseline_prompt) = &self.epoch_system_prompt {
            if !baseline_prompt.trim().is_empty() {
                let incoming_prompt = context.system_prompt.clone();
                context.system_prompt = baseline_prompt.clone();
                context.epoch_system_prompt = Some(baseline_prompt.clone());

                // 若检测到外部环境/项目规约/MCP/Skill 发生了变更，作为事件追加至历史流水
                if baseline_prompt != &incoming_prompt {
                    let diff_summary =
                        generate_prompt_diff_summary(baseline_prompt, &incoming_prompt);
                    if !diff_summary.is_empty() {
                        context.turns_history.push(ChatMessage {
                            role: "user".to_string(),
                            content: serde_json::json!(format!(
                                "【系统能力/规约更新事件】\n检测到项目规约或能力配置已发生变更，最新变更如下：\n{}\n(已写入当前会话历史流，本会话自此步起遵循最新规约)",
                                diff_summary
                            )),
                        });
                    }
                }
            }
        } else {
            context.epoch_system_prompt = Some(context.system_prompt.clone());
        }

        // 工具说明书是派生缓存，必须依照当前 Runtime 重新加载。
        context.active_tool_detail.clear();
        context.carry_memories = false;
        context.sync_fetched_tools_from_history();

        // 🌟 核心防超窗保护：恢复后主动执行自适应 Token 预算压缩，确保历史不超模型上下文
        context.maybe_compact_history();

        if self.needs_observation_refresh {
            context.add_error_feedback(
                "【系统恢复提示】任务从 checkpoint 恢复。请确认当前工作区状态并继续执行。",
            );
        }
    }

    /// 同一 session 上一轮已 finish 后，新用户消息仍 append 脱水夹心（Claude/Codex 会话内继承）。
    ///
    /// 不恢复旧 `user_goal` / todo / progress / observation（那是新任务）。
    /// 恢复：`turns_history`（与最新 chat_messages 去重追加）、memories、
    /// failed_attempts、side_effects。工具正文仍以磁盘 / output_id 为准。
    pub fn inherit_filling_into(&self, context: &mut SandwichContext) {
        // 保持 M1 静态目标锚点不变（跨轮前缀 100% 缓存命中），新提问已作为 user 消息追加在 chat_tail 尾部。
        // 注意：只回写锚点 user_goal，绝不动 current_goal——新轮上下文已携带本轮新目标，
        // 若一并继承会把终审/阶段审查的"当前任务"错绑回旧任务（实测导致拿任务1目标审任务2产物）。
        if !self.user_goal.is_empty() {
            context.user_goal = self.user_goal.clone();
        }
        let chat_tail = std::mem::take(&mut context.turns_history);
        context.turns_history = merge_chat_onto_filling(self.turns_history.clone(), chat_tail);

        // 保持已有会话的 M0 系统提示词恒定，若发生变更作为事件追加
        if let Some(baseline_prompt) = &self.epoch_system_prompt {
            if !baseline_prompt.trim().is_empty() {
                let incoming_prompt = context.system_prompt.clone();
                context.system_prompt = baseline_prompt.clone();
                context.epoch_system_prompt = Some(baseline_prompt.clone());

                if baseline_prompt != &incoming_prompt {
                    let diff_summary =
                        generate_prompt_diff_summary(baseline_prompt, &incoming_prompt);
                    if !diff_summary.is_empty() {
                        context.turns_history.push(ChatMessage {
                            role: "user".to_string(),
                            content: serde_json::json!(format!(
                                "【系统能力/规约更新事件】\n检测到项目规约或能力配置已发生变更，最新变更如下：\n{}\n(已写入当前会话历史流，本会话自此步起遵循最新规约)",
                                diff_summary
                            )),
                        });
                    }
                }
            }
        } else {
            context.epoch_system_prompt = Some(context.system_prompt.clone());
        }

        for (key, value) in &self.memories {
            context
                .memories
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }

        if context.failed_attempts.is_empty() {
            context.failed_attempts = self.failed_attempts.clone();
        }
        if context.side_effects.is_empty() {
            let mut effects = self.side_effects.clone();
            if effects.len() > MAX_INHERITED_SIDE_EFFECTS {
                let skip = effects.len() - MAX_INHERITED_SIDE_EFFECTS;
                effects.drain(..skip);
            }
            context.inherited_side_effects_count = effects.len();
            context.side_effects = effects;
        }

        context.sync_fetched_tools_from_history();
        context.maybe_compact_history();
    }
}

pub(crate) fn generate_prompt_diff_summary(old_p: &str, new_p: &str) -> String {
    let old_lines: std::collections::HashSet<&str> = old_p
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    let new_lines: std::collections::HashSet<&str> = new_p
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();

    let mut added: Vec<&str> = new_p
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !old_lines.contains(*l))
        .collect();
    let mut removed: Vec<&str> = old_p
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !new_lines.contains(*l))
        .collect();

    added.dedup();
    removed.dedup();

    if added.is_empty() && removed.is_empty() {
        return String::new();
    }

    let cap = 15;
    let mut parts = Vec::new();

    if !added.is_empty() {
        let shown = added.len().min(cap);
        let mut block = added[..shown]
            .iter()
            .map(|l| format!("  + {}", l))
            .collect::<Vec<_>>()
            .join("\n");
        if added.len() > cap {
            block.push_str(&format!("\n  ... 及其余 {} 处新增", added.len() - cap));
        }
        parts.push(format!("【新增/变更规约】\n{}", block));
    }

    if !removed.is_empty() {
        let shown = removed.len().min(cap);
        let mut block = removed[..shown]
            .iter()
            .map(|l| format!("  - {}", l))
            .collect::<Vec<_>>()
            .join("\n");
        if removed.len() > cap {
            block.push_str(&format!("\n  ... 及其余 {} 处移除", removed.len() - cap));
        }
        parts.push(format!("【已移除规约】\n{}", block));
    }

    parts.join("\n\n")
}

pub(crate) fn merge_chat_onto_filling(
    mut filling: Vec<ChatMessage>,
    chat: Vec<ChatMessage>,
) -> Vec<ChatMessage> {
    for msg in chat {
        let already = filling
            .iter()
            .any(|existing| existing.role == msg.role && existing.content == msg.content);
        if !already {
            filling.push(msg);
        }
    }
    filling
}
