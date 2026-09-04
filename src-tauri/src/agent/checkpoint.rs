use crate::agent::change_verification::ChangeVerification;
use crate::agent::context::{ChatMessage, FailedAttempt, SandwichContext, SideEffectRecord};
use crate::agent::review_contract::ReviewContract;
use crate::agent::task_store::TaskResourceRef;
use crate::agent::types::{AgentInstruction, TodoItem};
use crate::db::DbState;
use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::State;
use uuid::Uuid;

const CHECKPOINT_SCHEMA_VERSION: u32 = 2;
const MAX_OBSERVATION_CHARS: usize = 4_000;
/// 跨 finish 继承时台账只保留最近 N 条，避免底部状态卡随轮次线性膨胀。
const MAX_INHERITED_SIDE_EFFECTS: usize = 12;

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

/// 方案制定与前置审查门禁的生命周期阶段
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanPhase {
    /// 1. 探索取证中：只读探索事实，尚未形成经过审查的最终方案
    #[default]
    Exploring,
    /// 2. 方案审查中：主子 Agent 正在进行反证与挑刺审查
    Reviewing,
    /// 3. 等待人类审批：方案已完备，等待人类在 UI 点击确认或提出修改意见
    AwaitingApproval,
    /// 4. 方案已批准：前置门禁放行，允许执行代码写操作
    Approved,
}

impl PlanPhase {
    pub fn is_approved(self) -> bool {
        matches!(self, Self::Approved)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exploring => "exploring",
            Self::Reviewing => "reviewing",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Approved => "approved",
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
                    let diff_summary = generate_prompt_diff_summary(baseline_prompt, &incoming_prompt);
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
                    let diff_summary = generate_prompt_diff_summary(baseline_prompt, &incoming_prompt);
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

fn generate_prompt_diff_summary(old_p: &str, new_p: &str) -> String {
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

fn merge_chat_onto_filling(
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
    /// 对显式未完成 Todo 的终态质询已经发出一次，避免一次质询变成无限循环。
    #[serde(default)]
    pub finish_reviewed: bool,
    /// 终态质询累计次数（达到上限后强制熔断放行，绝不允许死循环）
    #[serde(default)]
    pub finish_challenge_count: u32,
    /// 终态代码审查重试次数（达到上限后熔断升级，防止死循环）
    #[serde(default)]
    pub reviewer_retry_count: u32,
    /// 上次审查的 Diff Hash（防重复重试相同 Diff）
    #[serde(default)]
    pub last_reviewed_diff_hash: Option<String>,
    /// 上次审查结果（当 diff_hash 未变时复用上次拦截，避免浪费 Token）
    #[serde(default)]
    pub last_review_result: Option<crate::agent::reviewer_gatekeeper::ReviewResult>,
    /// 方案制定与前置门禁阶段
    #[serde(default)]
    pub plan_phase: PlanPhase,
    /// 方案审查重试计数（防子 Agent 内部讨论死循环，上限 2 轮）
    #[serde(default)]
    pub plan_reviewer_retry_count: u32,
    /// 最终批准的方案摘要（用于防方案漂移与核对）
    #[serde(default)]
    pub approved_plan_summary: Option<String>,
    /// 运行模式：true 为全自动模式 (Auto-pilot)，false 为方案审批模式 (Plan Approval First)
    #[serde(default = "default_auto_pilot")]
    pub auto_pilot: bool,
    pub last_error: Option<String>,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
}

fn default_auto_pilot() -> bool {
    true
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
            finish_reviewed: false,
            finish_challenge_count: 0,
            reviewer_retry_count: 0,
            last_reviewed_diff_hash: None,
            last_review_result: None,
            plan_phase: PlanPhase::Exploring,
            plan_reviewer_retry_count: 0,
            approved_plan_summary: None,
            auto_pilot: true,
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
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT snapshot_json, version FROM agent_checkpoints WHERE run_id = ?1",
            params![run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| error.to_string())?;

    if let Some((json, db_version)) = row {
        let mut checkpoint: AgentCheckpoint =
            serde_json::from_str(&json).map_err(|error| error.to_string())?;
        checkpoint.version = db_version;
        Ok(Some(checkpoint))
    } else {
        Ok(None)
    }
}

/// 通过 revision 进行乐观锁更新；一次 UPDATE 同时写 phase、pending 和 data，避免恢复到不自洽状态。
///
/// 当检测到乐观锁冲突（rows != 1）时，自动重新加载最新 checkpoint，
/// 将调用方当前持有的关键字段（data/phase/pending/prepared_instruction/step_id/last_error）
/// 合并到最新的快照上，然后重试。最多重试 3 次；仍失败则回退到原始错误。
pub fn save_checkpoint(db: &DbState, checkpoint: &mut AgentCheckpoint) -> Result<(), String> {
    let mut attempt = 0;
    let max_attempts = 3;

    loop {
        attempt += 1;
        checkpoint.updated_at = Utc::now().to_rfc3339();
        let conn = db.0.lock().map_err(|_| "数据库锁已中毒".to_string())?;

        if checkpoint.version == 0 {
            // snapshot JSON 与表中 revision 必须一致；否则第一次恢复会错误地再次 INSERT。
            checkpoint.version = 1;
            let snapshot_json =
                serde_json::to_string(checkpoint).map_err(|error| error.to_string())?;
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
            return Ok(());
        } else {
            let previous_version = checkpoint.version;
            checkpoint.version += 1;
            let snapshot_json =
                serde_json::to_string(checkpoint).map_err(|error| error.to_string())?;
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

            if rows == 1 {
                return Ok(());
            }

            // 乐观锁冲突：数据库中的 version 已被其他保存者推进。
            checkpoint.version = previous_version;

            if attempt >= max_attempts {
                return Err("checkpoint 已被另一条任务更新，请重新加载后再保存".to_string());
            }

            // 自动重试：重新加载最新 checkpoint 并合并当前关键状态。
            let reloaded = load_checkpoint(db, &checkpoint.run_id)?;
            match reloaded {
                Some(mut latest) => {
                    latest.data = checkpoint.data.clone();
                    latest.phase = checkpoint.phase;
                    latest.plan_phase = checkpoint.plan_phase;
                    latest.plan_reviewer_retry_count = checkpoint.plan_reviewer_retry_count;
                    latest.approved_plan_summary = checkpoint.approved_plan_summary.clone();
                    latest.auto_pilot = checkpoint.auto_pilot;
                    latest.finish_reviewed = checkpoint.finish_reviewed;
                    latest.finish_challenge_count = checkpoint.finish_challenge_count;
                    latest.pending = checkpoint.pending.clone();
                    latest.prepared_instruction = checkpoint.prepared_instruction.clone();
                    latest.step_id = checkpoint.step_id;
                    latest.last_error = checkpoint.last_error.clone();
                    checkpoint.version = latest.version;
                    checkpoint.data = latest.data;
                    checkpoint.phase = latest.phase;
                    checkpoint.plan_phase = latest.plan_phase;
                    checkpoint.plan_reviewer_retry_count = latest.plan_reviewer_retry_count;
                    checkpoint.approved_plan_summary = latest.approved_plan_summary;
                    checkpoint.auto_pilot = latest.auto_pilot;
                    checkpoint.finish_reviewed = latest.finish_reviewed;
                    checkpoint.finish_challenge_count = latest.finish_challenge_count;
                    checkpoint.pending = latest.pending;
                    checkpoint.prepared_instruction = latest.prepared_instruction;
                    checkpoint.step_id = latest.step_id;
                    checkpoint.last_error = latest.last_error;
                }
                None => {
                    // 记录已被删除，重置为首次写入。
                    checkpoint.version = 0;
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCheckpointSummary {
    pub run_id: String,
    pub phase: String,
    #[serde(default)]
    pub plan_phase: Option<String>,
    #[serde(default = "default_auto_pilot")]
    pub auto_pilot: bool,
    pub step_id: usize,
    pub user_goal: String,
    pub last_error: Option<String>,
    pub is_waiting_human: bool,
    pub form: Option<serde_json::Value>,
}

#[tauri::command]
pub async fn get_agent_checkpoint(
    state: State<'_, DbState>,
    session_id: String,
) -> Result<Option<AgentCheckpointSummary>, String> {
    let checkpoint = load_checkpoint(state.inner(), &session_id)?;
    Ok(checkpoint.map(|cp| {
        let form = cp
            .prepared_instruction
            .as_ref()
            .and_then(|inst| inst.get_params().get("form").cloned())
            .or_else(|| {
                cp.pending.as_ref().and_then(|p| {
                    p.instruction.get_params().get("form").cloned()
                })
            })
            .or_else(|| {
                crate::agent::channel::runs_store()
                    .lock()
                    .ok()
                    .and_then(|store| {
                        store
                            .values()
                            .find(|r| r.session_id == session_id && r.status == "waiting_human")
                            .and_then(|r| r.paused_form.clone())
                    })
            });
        AgentCheckpointSummary {
            run_id: cp.run_id,
            phase: cp.phase.as_str().to_string(),
            plan_phase: Some(cp.plan_phase.as_str().to_string()),
            auto_pilot: cp.auto_pilot,
            step_id: cp.step_id,
            user_goal: cp.data.user_goal,
            last_error: cp.last_error,
            is_waiting_human: cp.phase == AgentPhase::WaitUser,
            form,
        }
    }))
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

    #[test]
    fn inherit_filling_keeps_dehydrated_tool_trace_and_new_user_goal() {
        use crate::agent::context::{FailedAttempt, SideEffectRecord};
        use serde_json::json;

        let mut previous =
            SandwichContext::new("system".to_string(), "配置 OpenRouter".to_string());
        previous.turns_history = vec![
            ChatMessage {
                role: "user".to_string(),
                content: json!("配置 OpenRouter"),
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: json!("计划: 配置 | 工具: model_manager | 动作: list_models"),
            },
            ChatMessage {
                role: "user".to_string(),
                content: json!("【执行输出 (v4)】\noutput_id: out_err\n❌ 未知动作 list_models"),
            },
        ];
        previous.failed_attempts.push(FailedAttempt {
            step_id: 1,
            tool: "model_manager".to_string(),
            action: "list_models".to_string(),
            params_fingerprint: String::new(),
            error_summary: "未知动作".to_string(),
            output_id: Some("out_err".to_string()),
        });
        previous.side_effects.push(SideEffectRecord {
            step_id: 4,
            tool: "edit_file".to_string(),
            description: "model_manager.rs 新增 add_platform".to_string(),
            timestamp: "2026-08-24T00:00:00Z".to_string(),
        });
        previous
            .memories
            .insert("platform".to_string(), "OpenRouter".to_string());
        previous.todo_list = vec![crate::agent::types::TodoItem {
            id: 1,
            description: "旧任务待办".to_string(),
            status: "completed".to_string(),
            evidence_output_id: None,
        }];
        let mut done = AgentCheckpoint::new("session-a".to_string(), &previous);
        done.mark_done(&previous);

        let mut next =
            SandwichContext::new("system".to_string(), "为什么 list_models 报错".to_string());
        next.turns_history = vec![
            ChatMessage {
                role: "user".to_string(),
                content: json!("配置 OpenRouter"),
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: json!("OpenRouter 已就绪"),
            },
            ChatMessage {
                role: "user".to_string(),
                content: json!("为什么 list_models 报错"),
            },
        ];
        done.data.inherit_filling_into(&mut next);

        assert_eq!(
            next.user_goal, "配置 OpenRouter",
            "M1 静态锚点跨轮不变，确保 100% 前缀缓存命中"
        );
        assert!(next.todo_list.is_empty(), "新任务不继承旧 todo");
        assert_eq!(
            next.memories.get("platform").map(String::as_str),
            Some("OpenRouter")
        );
        assert_eq!(next.failed_attempts.len(), 1);
        assert_eq!(next.failed_attempts[0].action, "list_models");
        assert_eq!(
            next.failed_attempts[0].output_id.as_deref(),
            Some("out_err")
        );
        assert_eq!(next.side_effects.len(), 1);
        assert!(next
            .turns_history
            .iter()
            .any(|m| m.content.as_str()
                == Some("计划: 配置 | 工具: model_manager | 动作: list_models")));
        assert!(next
            .turns_history
            .iter()
            .any(|m| m.content.as_str() == Some("为什么 list_models 报错")));
        assert_eq!(
            next.turns_history
                .iter()
                .filter(|m| m.content.as_str() == Some("配置 OpenRouter"))
                .count(),
            1,
            "已在夹心里的 user 消息不重复追加"
        );
    }

    #[test]
    fn old_checkpoint_without_finish_reviewed_defaults_to_false() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let checkpoint = AgentCheckpoint::new("run-1".to_string(), &context);
        let mut legacy = serde_json::to_value(checkpoint).unwrap();
        legacy.as_object_mut().unwrap().remove("finish_reviewed");

        let restored: AgentCheckpoint = serde_json::from_value(legacy).unwrap();
        assert!(!restored.finish_reviewed);
    }

    #[test]
    fn review_contract_persists_on_restore_but_does_not_cross_new_task_boundary() {
        let mut source = SandwichContext::new("system".to_string(), "审查上下文".to_string());
        source.review_contract = Some(ReviewContract {
            scope_statement: "核验终态恢复".to_string(),
            obligations: vec![crate::agent::review_contract::ReviewObligation {
                id: "restore".to_string(),
                question: "checkpoint 是否恢复审查契约？".to_string(),
                status: "evidenced".to_string(),
                evidence_refs: vec!["checkpoint.rs:168-204".to_string()],
                waived_reason: None,
            }],
        });
        let checkpoint = AgentCheckpoint::new("run-contract".to_string(), &source);

        let mut resumed = SandwichContext::new("system".to_string(), "resume".to_string());
        checkpoint.data.restore_into(&mut resumed);
        assert_eq!(resumed.review_contract, source.review_contract);

        let mut legacy = serde_json::to_value(&checkpoint).unwrap();
        legacy
            .get_mut("data")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap()
            .remove("review_contract");
        let restored_legacy: AgentCheckpoint = serde_json::from_value(legacy).unwrap();
        assert!(restored_legacy.data.review_contract.is_none());

        let mut next_task = SandwichContext::new("system".to_string(), "新任务".to_string());
        checkpoint.data.inherit_filling_into(&mut next_task);
        assert!(next_task.review_contract.is_none());
    }
    #[test]
    fn change_verification_persists_on_restore_but_does_not_cross_new_task_boundary() {
        let mut source = SandwichContext::new("system".to_string(), "修改上下文".to_string());
        source.record_side_effect(1, "edit_file".to_string(), "更新配置".to_string());
        source.change_verification = Some(ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec!["output_id: cargo-test-42".to_string()],
            waived_reason: None,
        });
        let checkpoint = AgentCheckpoint::new("run-verification".to_string(), &source);
        let mut resumed = SandwichContext::new("system".to_string(), "resume".to_string());
        checkpoint.data.restore_into(&mut resumed);
        assert_eq!(resumed.change_verification, source.change_verification);
        let mut legacy = serde_json::to_value(&checkpoint).unwrap();
        legacy
            .get_mut("data")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap()
            .remove("change_verification");
        let restored_legacy: AgentCheckpoint = serde_json::from_value(legacy).unwrap();
        assert!(restored_legacy.data.change_verification.is_none());
        let mut next_task = SandwichContext::new("system".to_string(), "新任务".to_string());
        checkpoint.data.inherit_filling_into(&mut next_task);
        assert!(next_task.change_verification.is_none());
    }
}
