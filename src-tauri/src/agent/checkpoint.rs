use crate::agent::context::{ChatMessage, SandwichContext};
use crate::agent::task_store::TaskResourceRef;
use crate::agent::types::{AgentInstruction, TodoItem};
use crate::db::DbState;
use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

const CHECKPOINT_SCHEMA_VERSION: u32 = 2;
const MAX_PERSISTED_TURNS: usize = 40;
const MAX_OBSERVATION_CHARS: usize = 4_000;

/// 可恢复调度只保留真正决定下一步处理器的阶段。
/// Planning / Executing / Recovering 仍可作为 UI 运行态，但不需要全部落盘。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPhase {
    CallLlm,
    RunTool,
    WaitUser,
    Done,
    Failed,
}

impl AgentPhase {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CallLlm => "call_llm",
            Self::RunTool => "run_tool",
            Self::WaitUser => "wait_user",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

/// 崩溃后处理 pending 调用的策略。默认保守，未知工具不自动重跑。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryPolicy {
    SafeRetry,
    Idempotent,
    Probeable,
    HumanReview,
}

impl RecoveryPolicy {
    pub fn requires_pending(self) -> bool {
        !matches!(self, Self::SafeRetry)
    }
}

/// 对当前项目已知的只读或可安全重复动作给出默认放行。
/// 未识别动作一律走 HumanReview，避免把提交、删除等操作误判为可重跑。
pub fn recovery_policy_for(instruction: &AgentInstruction) -> RecoveryPolicy {
    let action = instruction.get_action().to_ascii_lowercase();
    let tool = instruction.get_tool().to_ascii_lowercase();

    if tool == "task_store" {
        return match action.as_str() {
            "list_resources" | "get_resource" | "list_records" | "get_record" => {
                RecoveryPolicy::SafeRetry
            }
            // create_resource 使用 session/kind/label 生成确定性 ID；upsert 以 record_key UPSERT。
            "create_resource" | "upsert_records" | "update_resource" => RecoveryPolicy::Idempotent,
            _ => RecoveryPolicy::HumanReview,
        };
    }

    let safe_actions = [
        "extract",
        "look",
        "read",
        "screenshot",
        "tab_url",
        "url",
        "list_tabs",
        "wait",
        "wait_idle",
        "goto",
        "back",
        "forward",
        "refresh",
        "scroll",
        "search",
    ];

    if safe_actions.contains(&action.as_str())
        || action.starts_with("get_")
        || action.starts_with("list_")
        || action.starts_with("read_")
        || action.starts_with("search_")
        || tool.starts_with("get_")
        || tool.starts_with("list_")
        || tool.starts_with("read_")
        || tool.starts_with("search_")
    {
        RecoveryPolicy::SafeRetry
    } else {
        RecoveryPolicy::HumanReview
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingToolCall {
    pub tool: String,
    pub action: String,
    pub call_id: String,
    pub instruction: AgentInstruction,
    pub policy: RecoveryPolicy,
    pub attempts: u32,
    pub started_at: String,
}

impl PendingToolCall {
    pub fn new(instruction: AgentInstruction, policy: RecoveryPolicy) -> Self {
        Self {
            tool: instruction.get_tool(),
            action: instruction.get_action(),
            call_id: Uuid::new_v4().to_string(),
            instruction,
            policy,
            attempts: 1,
            started_at: Utc::now().to_rfc3339(),
        }
    }
}

/// 只保存继续任务所需的事实；system prompt、连接、工具详情和大对象都在恢复时重建。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableAgentContext {
    pub user_goal: String,
    pub memories: HashMap<String, String>,
    /// 任务数据区目录，数据行仍由 agent_task_records 独立实时持久化。
    #[serde(default)]
    pub task_resources: Vec<TaskResourceRef>,
    pub todo_list: Vec<TodoItem>,
    pub turns_history: Vec<ChatMessage>,
    pub progress_summary: String,
    pub observation_summary: String,
    pub active_tool: Option<String>,
    pub needs_observation_refresh: bool,
}

impl DurableAgentContext {
    pub fn capture(context: &SandwichContext) -> Self {
        let turns_history = if context.turns_history.len() > MAX_PERSISTED_TURNS {
            context.turns_history[context.turns_history.len() - MAX_PERSISTED_TURNS..].to_vec()
        } else {
            context.turns_history.clone()
        };

        Self {
            user_goal: context.user_goal.clone(),
            memories: context.memories.clone(),
            task_resources: context.task_resources.clone(),
            todo_list: context.todo_list.clone(),
            turns_history,
            progress_summary: context.progress_summary.clone(),
            observation_summary: truncate_chars(
                &context.current_observation,
                MAX_OBSERVATION_CHARS,
            ),
            active_tool: context.active_tool.clone(),
            // 浏览器/DOM 可能已变化，恢复后不能相信旧观测。
            needs_observation_refresh: !context.current_observation.is_empty(),
        }
    }

    pub fn restore_into(&self, context: &mut SandwichContext) {
        context.user_goal = self.user_goal.clone();
        context.memories = self.memories.clone();
        context.task_resources = self.task_resources.clone();
        context.todo_list = self.todo_list.clone();
        context.turns_history = self.turns_history.clone();
        context.progress_summary = self.progress_summary.clone();
        context.current_observation = self.observation_summary.clone();
        context.active_tool = self.active_tool.clone();
        // 工具说明书是派生缓存，必须依照当前 Runtime 重新加载。
        context.active_tool_detail.clear();
        context.carry_memories = false;

        if self.needs_observation_refresh {
            context.add_error_feedback(
                "【系统恢复提示】任务从 checkpoint 恢复。浏览器连接和 DOM 可能已变化，之前保存的元素 ID 不可信；执行交互前必须先 extract 重新获取页面状态。",
            );
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCheckpoint {
    pub schema_version: u32,
    pub run_id: String,
    pub phase: AgentPhase,
    pub data: DurableAgentContext,
    pub pending: Option<PendingToolCall>,
    /// LLM 已返回且已确认的结构化决策；工具开始前也必须可恢复。
    #[serde(default)]
    pub prepared_instruction: Option<AgentInstruction>,
    pub step_id: usize,
    pub retry_count: u32,
    pub last_error: Option<String>,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl AgentCheckpoint {
    pub fn new(run_id: String, context: &SandwichContext) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            schema_version: CHECKPOINT_SCHEMA_VERSION,
            run_id,
            phase: AgentPhase::CallLlm,
            data: DurableAgentContext::capture(context),
            pending: None,
            prepared_instruction: None,
            step_id: 0,
            retry_count: 0,
            last_error: None,
            version: 0,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    pub fn refresh_context(&mut self, context: &SandwichContext) {
        self.data = DurableAgentContext::capture(context);
    }

    pub fn restore_context(&self, context: &mut SandwichContext) {
        self.data.restore_into(context);
    }

    pub fn begin_planning(&mut self, step_id: usize, context: &SandwichContext) {
        self.phase = AgentPhase::CallLlm;
        self.step_id = step_id;
        self.pending = None;
        self.prepared_instruction = None;
        self.last_error = None;
        self.refresh_context(context);
    }

    /// LLM 的结构化回复已完整到达。先提交决策，再发任何工具调用。
    /// 即使工具尚未开始，崩溃恢复也能从 RunTool 直接续接该 instruction。
    pub fn record_instruction(&mut self, instruction: AgentInstruction, context: &SandwichContext) {
        self.phase = AgentPhase::RunTool;
        self.pending = None;
        self.prepared_instruction = Some(instruction);
        self.last_error = None;
        self.refresh_context(context);
    }

    /// 仅不安全调用在执行前落 pending。安全读操作崩溃后允许重新规划或重跑，不增加落盘成本。
    pub fn prepare_tool(
        &mut self,
        instruction: AgentInstruction,
        context: &SandwichContext,
    ) -> RecoveryPolicy {
        let policy = recovery_policy_for(&instruction);
        self.phase = AgentPhase::RunTool;
        self.prepared_instruction = Some(instruction.clone());
        self.refresh_context(context);
        self.last_error = None;

        if policy.requires_pending() {
            self.pending = Some(PendingToolCall::new(instruction, policy));
        } else {
            // 安全读不需要 pending；但保留已确认 instruction，崩溃后可直接续接。
            self.pending = None;
        }

        policy
    }

    /// 提交一个已完成的普通步骤，并把 durable cursor 移到下一步。
    /// 这样进程在通知前端后崩溃时，恢复不会重复已经写入上下文的工具结果。
    pub fn complete_step(&mut self, completed_step_id: usize, context: &SandwichContext) {
        self.phase = AgentPhase::CallLlm;
        self.step_id = completed_step_id.saturating_add(1);
        self.pending = None;
        self.prepared_instruction = None;
        self.last_error = None;
        self.refresh_context(context);
    }

    pub fn record_recoverable_error(&mut self, error: String, context: &SandwichContext) {
        self.phase = AgentPhase::CallLlm;
        self.pending = None;
        self.prepared_instruction = None;
        self.retry_count += 1;
        self.last_error = Some(error);
        self.refresh_context(context);
    }

    pub fn wait_for_human(&mut self, reason: String, context: &SandwichContext) {
        self.phase = AgentPhase::WaitUser;
        // pending 保留给人工审计；prepared instruction 已由 pending 覆盖或不再需要。
        self.prepared_instruction = None;
        self.last_error = Some(reason);
        self.refresh_context(context);
    }

    pub fn mark_done(&mut self, context: &SandwichContext) {
        self.phase = AgentPhase::Done;
        self.pending = None;
        self.prepared_instruction = None;
        self.last_error = None;
        self.refresh_context(context);
    }

    pub fn mark_failed(&mut self, reason: String, context: &SandwichContext) {
        self.phase = AgentPhase::Failed;
        self.pending = None;
        self.prepared_instruction = None;
        self.last_error = Some(reason);
        self.refresh_context(context);
    }

    /// 恢复时先将 attempts 持久化，避免每次崩溃都无痕重试。
    pub fn begin_recovery_attempt(&mut self) {
        if let Some(pending) = self.pending.as_mut() {
            pending.attempts += 1;
        }
    }

    pub fn recovery_policy(&self) -> Option<RecoveryPolicy> {
        self.pending.as_ref().map(|pending| pending.policy)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryDecision {
    ResumePlanning,
    /// LLM instruction 已落盘，但工具尚未形成 pending；可从工具处理器直接续接。
    ResumeTool,
    ResumeWaiting,
    RetryPending,
    ProbePending,
    RequireHumanReview,
    Terminal,
}

pub fn recovery_decision(checkpoint: &AgentCheckpoint) -> RecoveryDecision {
    if checkpoint.phase.is_terminal() {
        return RecoveryDecision::Terminal;
    }

    match checkpoint.pending.as_ref().map(|pending| pending.policy) {
        Some(RecoveryPolicy::SafeRetry) | Some(RecoveryPolicy::Idempotent) => {
            RecoveryDecision::RetryPending
        }
        Some(RecoveryPolicy::Probeable) => RecoveryDecision::ProbePending,
        Some(RecoveryPolicy::HumanReview) => RecoveryDecision::RequireHumanReview,
        None if checkpoint.phase == AgentPhase::RunTool
            && checkpoint.prepared_instruction.is_some() =>
        {
            RecoveryDecision::ResumeTool
        }
        None if checkpoint.phase == AgentPhase::WaitUser => RecoveryDecision::ResumeWaiting,
        None => RecoveryDecision::ResumePlanning,
    }
}

pub fn load_checkpoint(db: &DbState, run_id: &str) -> Result<Option<AgentCheckpoint>, String> {
    let conn = db.0.lock().map_err(|_| "数据库锁已中毒".to_string())?;
    let snapshot_json: Option<String> = conn
        .query_row(
            "SELECT snapshot_json FROM agent_checkpoints WHERE run_id = ?1",
            params![run_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;

    snapshot_json
        .map(|json| serde_json::from_str(&json).map_err(|error| error.to_string()))
        .transpose()
}

/// 通过 revision 进行乐观锁更新；一次 UPDATE 同时写 phase、pending 和 data，避免恢复到不自洽状态。
pub fn save_checkpoint(db: &DbState, checkpoint: &mut AgentCheckpoint) -> Result<(), String> {
    checkpoint.updated_at = Utc::now().to_rfc3339();
    let conn = db.0.lock().map_err(|_| "数据库锁已中毒".to_string())?;

    if checkpoint.version == 0 {
        // snapshot JSON 与表中 revision 必须一致；否则第一次恢复会错误地再次 INSERT。
        checkpoint.version = 1;
        let snapshot_json = serde_json::to_string(checkpoint).map_err(|error| error.to_string())?;
        conn.execute(
            "INSERT INTO agent_checkpoints (run_id, phase, snapshot_json, version, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                checkpoint.run_id,
                checkpoint.phase.as_str(),
                snapshot_json,
                checkpoint.version,
                checkpoint.created_at,
                checkpoint.updated_at
            ],
        )
        .map_err(|error| error.to_string())?;
    } else {
        let previous_version = checkpoint.version;
        checkpoint.version += 1;
        let snapshot_json = serde_json::to_string(checkpoint).map_err(|error| error.to_string())?;
        let rows = conn
            .execute(
                "UPDATE agent_checkpoints SET phase = ?1, snapshot_json = ?2, version = ?3, updated_at = ?4 WHERE run_id = ?5 AND version = ?6",
                params![
                    checkpoint.phase.as_str(),
                    snapshot_json,
                    checkpoint.version,
                    checkpoint.updated_at,
                    checkpoint.run_id,
                    previous_version
                ],
            )
            .map_err(|error| error.to_string())?;

        if rows != 1 {
            checkpoint.version = previous_version;
            return Err("checkpoint 已被另一条任务更新，请重新加载后再保存".to_string());
        }
    }

    Ok(())
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        let mut truncated: String = value.chars().take(max_chars).collect();
        truncated.push_str("\n[观测内容过长，恢复后必须重新读取原始资料]");
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instruction(action: &str) -> AgentInstruction {
        AgentInstruction {
            action: Some(action.to_string()),
            tool: Some("browser".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn unknown_actions_default_to_human_review() {
        assert_eq!(
            recovery_policy_for(&instruction("submit_order")),
            RecoveryPolicy::HumanReview
        );
    }

    #[test]
    fn readonly_actions_are_safe_to_retry() {
        assert_eq!(
            recovery_policy_for(&instruction("extract")),
            RecoveryPolicy::SafeRetry
        );
        assert_eq!(
            recovery_policy_for(&instruction("get_status")),
            RecoveryPolicy::SafeRetry
        );
    }

    #[test]
    fn pending_call_reuses_a_stable_id_across_recovery_attempts() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-1".to_string(), &context);
        checkpoint.prepare_tool(instruction("submit_order"), &context);
        let before = checkpoint.pending.as_ref().unwrap().call_id.clone();
        checkpoint.begin_recovery_attempt();
        let pending = checkpoint.pending.as_ref().unwrap();

        assert_eq!(pending.call_id, before);
        assert_eq!(pending.attempts, 2);
        assert_eq!(
            recovery_decision(&checkpoint),
            RecoveryDecision::RequireHumanReview
        );
    }

    #[test]
    fn safe_actions_keep_a_prepared_instruction_without_pending() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-1".to_string(), &context);
        checkpoint.prepare_tool(instruction("extract"), &context);

        assert!(checkpoint.pending.is_none());
        assert_eq!(checkpoint.phase, AgentPhase::RunTool);
        assert_eq!(
            checkpoint
                .prepared_instruction
                .as_ref()
                .map(AgentInstruction::get_action),
            Some("extract".to_string())
        );
        assert_eq!(recovery_decision(&checkpoint), RecoveryDecision::ResumeTool);
    }

    #[test]
    fn completed_step_advances_durable_cursor_before_next_planning_cycle() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-1".to_string(), &context);
        checkpoint.prepare_tool(instruction("submit_order"), &context);
        checkpoint.complete_step(7, &context);

        assert_eq!(checkpoint.phase, AgentPhase::CallLlm);
        assert_eq!(checkpoint.step_id, 8);
        assert!(checkpoint.pending.is_none());
        assert_eq!(
            recovery_decision(&checkpoint),
            RecoveryDecision::ResumePlanning
        );
    }

    #[test]
    fn wait_user_without_pending_resumes_waiting() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-1".to_string(), &context);
        checkpoint.wait_for_human("need confirmation".to_string(), &context);

        assert_eq!(checkpoint.phase, AgentPhase::WaitUser);
        assert!(checkpoint.pending.is_none());
        assert_eq!(
            recovery_decision(&checkpoint),
            RecoveryDecision::ResumeWaiting
        );
    }

    #[test]
    fn terminal_phases_never_resume_after_checkpoint_recovery() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut done = AgentCheckpoint::new("done".to_string(), &context);
        done.mark_done(&context);
        assert_eq!(recovery_decision(&done), RecoveryDecision::Terminal);

        let mut failed = AgentCheckpoint::new("failed".to_string(), &context);
        failed.mark_failed("terminal failure".to_string(), &context);
        assert_eq!(recovery_decision(&failed), RecoveryDecision::Terminal);
    }

    #[test]
    fn safe_read_recovery_resumes_the_saved_instruction_without_a_new_llm_call() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-1".to_string(), &context);
        checkpoint.prepare_tool(instruction("extract"), &context);

        assert!(checkpoint.pending.is_none());
        assert_eq!(checkpoint.phase, AgentPhase::RunTool);
        assert!(checkpoint.prepared_instruction.is_some());
        assert_eq!(recovery_decision(&checkpoint), RecoveryDecision::ResumeTool);
    }

    #[test]
    fn llm_instruction_is_persisted_before_tool_execution_and_cleared_on_completion() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-1".to_string(), &context);
        checkpoint.record_instruction(instruction("submit_order"), &context);

        assert_eq!(checkpoint.phase, AgentPhase::RunTool);
        assert!(checkpoint.pending.is_none());
        assert_eq!(recovery_decision(&checkpoint), RecoveryDecision::ResumeTool);

        checkpoint.complete_step(3, &context);
        assert_eq!(checkpoint.phase, AgentPhase::CallLlm);
        assert_eq!(checkpoint.step_id, 4);
        assert!(checkpoint.prepared_instruction.is_none());
        assert_eq!(
            recovery_decision(&checkpoint),
            RecoveryDecision::ResumePlanning
        );
    }

    #[test]
    fn task_store_resource_references_survive_checkpoint_context_recovery() {
        let mut context = SandwichContext::new("system".to_string(), "goal".to_string());
        context.set_task_resources(vec![crate::agent::task_store::TaskResourceRef {
            resource_id: "people-dataset".to_string(),
            kind: "people".to_string(),
            label: "候选人资料".to_string(),
            summary: "已持久化 100 人，已补全 42 人".to_string(),
            status: "active".to_string(),
            row_count: 100,
            cursor: Some(serde_json::json!({"enriched": 42})),
            updated_at: "2026-08-14T00:00:00Z".to_string(),
        }]);
        let checkpoint = AgentCheckpoint::new("run-1".to_string(), &context);

        assert_eq!(checkpoint.data.task_resources.len(), 1);
        assert_eq!(checkpoint.data.task_resources[0].row_count, 100);

        let mut restored =
            SandwichContext::new("other-system".to_string(), "other-goal".to_string());
        checkpoint.restore_context(&mut restored);
        assert_eq!(restored.task_resources, checkpoint.data.task_resources);
        assert_eq!(restored.task_resources[0].resource_id, "people-dataset");
        assert_eq!(
            restored.task_resources[0].cursor,
            Some(serde_json::json!({"enriched": 42}))
        );
    }

    #[test]
    fn task_store_upsert_uses_idempotent_pending_recovery() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut instruction = instruction("upsert_records");
        instruction.tool = Some("task_store".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-1".to_string(), &context);

        assert_eq!(
            recovery_policy_for(&instruction),
            RecoveryPolicy::Idempotent
        );
        checkpoint.prepare_tool(instruction, &context);
        assert_eq!(
            checkpoint.pending.as_ref().map(|pending| pending.policy),
            Some(RecoveryPolicy::Idempotent)
        );
        assert_eq!(
            recovery_decision(&checkpoint),
            RecoveryDecision::RetryPending
        );
    }
}
