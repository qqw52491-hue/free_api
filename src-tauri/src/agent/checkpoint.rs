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

/// 批准前方案审查的轮数默认上限（门禁与投影共用同一事实源）。
/// 可用 FREE_API_PLAN_REVIEW_MAX_ROUNDS 调整（钳位 1..=6）——reviewer 的
/// invalid_output 连发会消耗请求方预算（虽已可退款），默认 2 轮在高抖动
/// 通道下偏紧（实测：glm 62 分→改进→预算耗尽→ask_human 弃赛）。
pub const MAX_PLAN_REVIEWER_RETRIES: u32 = 2;

pub fn max_plan_reviewer_rounds() -> u32 {
    std::env::var("FREE_API_PLAN_REVIEW_MAX_ROUNDS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .map(|v| v.clamp(1, 6))
        .unwrap_or(MAX_PLAN_REVIEWER_RETRIES)
}

/// 阶段级写入审查的次数硬上限（独立于方案审查预算，语义不同不可混用）
pub const MAX_STAGE_REVIEWS: u32 = 5;

/// 方案制定与前置审查门禁的生命周期阶段
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../src/types/generated/")]
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

/// 方案与执行前置门禁模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../src/types/generated/")]
pub enum PlanExecutionMode {
    /// 🚀 全自动模式 (Auto)：只读排查与初步规划后直接放行修改
    #[default]
    Auto,
    /// 🔍 双 Agent 交叉审查模式 (Cross-review)：动笔前必须派发 child_reviewer 并消费其反驳推演结论
    CrossReview,
    /// 🛡️ 方案审批模式 (Plan Approval First)：动笔前必须由人类在 UI 明确点击【批准方案】
    PlanApproval,
}

impl PlanExecutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::CrossReview => "cross_review",
            Self::PlanApproval => "plan_approval",
        }
    }

    pub fn from_str_or_auto_pilot(mode: Option<&str>, auto_pilot: bool) -> Self {
        match mode.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("cross_review") | Some("cross-review") | Some("crossreview") => Self::CrossReview,
            Some("plan_approval") | Some("plan-approval") | Some("plan") | Some("approval") => {
                Self::PlanApproval
            }
            Some("auto") | Some("auto_pilot") | Some("autopilot") => Self::Auto,
            _ => {
                if auto_pilot {
                    Self::Auto
                } else {
                    Self::PlanApproval
                }
            }
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
    /// 运行执行模式：Auto（全自动）、CrossReview（交叉审查）、PlanApproval（方案审批）
    #[serde(default)]
    pub plan_mode: PlanExecutionMode,
    /// 方案审查结果是否已被主循环或 wait_jobs 消费并注入上下文（“问过”且“听过”）
    #[serde(default)]
    pub plan_reviewer_consumed: bool,
    /// 方案审查最新评分卡（正确性、破坏性风险、代码规范、测试覆盖）
    #[serde(default)]
    pub last_review_scorecard: Option<crate::agent::child_reviewer::ReviewScorecard>,
    /// 最新一轮审查中未解决的 blocking 级异议数量（评分达标但存在 blocking 时同样拦截）
    #[serde(default)]
    pub last_review_blocking_count: u32,
    /// 本轮异议动态：(新增, 撤回, 维持) —— 膨胀观测：撤回≈新增且总数不降 = 审查空转
    #[serde(default)]
    pub review_objection_added: u32,
    #[serde(default)]
    pub review_objection_withdrawn: u32,
    #[serde(default)]
    pub review_objection_maintained: u32,
    /// 基础设施失败（超时/取消/中断）已退还的审查轮次数；封顶防无限重试
    #[serde(default)]
    pub reviewer_budget_refunds: u32,
    /// 全任务累计：异议撤回数 / 维持数。撤回率 = 撤回/(撤回+维持)，是误报率的上界
    /// （含"主Agent补充证据后合理撤回"的成分）；>50% 提示该 reviewer 判定过松。
    #[serde(default)]
    pub review_total_withdrawn: u32,
    #[serde(default)]
    pub review_total_maintained: u32,
    /// ---- 阶段级写入审查（双触发：todo 阶段推进 / 累计写入文件数兜底）----
    /// 已派发的阶段审查次数
    #[serde(default)]
    pub stage_review_budget_used: u32,
    /// 在途阶段审查任务的 child_run_id（含其 rebuttal 对质任务）
    #[serde(default)]
    pub stage_review_run_id: Option<String>,
    /// 联合会审模式下在途阶段审查任务的 child_run_id 列表
    #[serde(default)]
    pub stage_review_run_ids: Vec<String>,
    #[serde(default)]
    pub stage_review_rebuttal_run_id: Option<String>,
    /// 覆盖基线：上次成功阶段审查时的副作用台账长度（之后的新增 = 未覆盖写入）
    #[serde(default)]
    pub stage_review_baseline_side_effects: u32,
    /// 阶段审查未解决的 blocking_regardless 数（>0 时拦截写入，直到对质撤回）
    #[serde(default)]
    pub stage_review_blockers_open: u32,
    /// 同一 blocking 异议连续被审查员维持（对质拒撤回）的次数——达到阈值自动
    /// 降级为 final_only 记账（吵不赢就降级放行，账要还），防止三方锁死
    /// （审查员不撤 + 写入全禁 + 模型无处可去 = 实测死锁形态）。
    #[serde(default)]
    pub stage_review_confrontation_failures: u32,
    /// 僵尸 blocker 治理：blockers_open 变为 >0 时的宿主判分阶段索引（oracle 的
    /// 「下一未解阶段」index，0 基）。宿主判分越过该索引 = 该阶段的实现已被客观
    /// 判定通过，遗留的阶段审查拦截属僵尸 blocker，写入门禁自动清账（final_only
    /// 记账）。实测（09-07 pro 终局）：阶段 07 已过判分，旧 blocker 仍全局拦写。
    #[serde(default)]
    pub stage_review_blocker_stage: Option<u32>,
    /// ---- 方案审查者故障转移链（failover）----
    /// 已执行的自动故障转移次数（invalid_output 重试 1 次 + 超时切换备用审查员，封顶 2）
    #[serde(default)]
    pub plan_reviewer_failover_count: u32,
    /// 最近一次方案审查派发的完整参数快照（模型/tier/effort/persona/params），
    /// 供消费端在 reviewer 故障时自动重派，无需模型重新组织派发
    #[serde(default)]
    pub plan_reviewer_last_dispatch: Option<String>,
    /// 最近一次门禁拦截发生的 step_id（ask_human 冷却判定用）；
    /// 成功提交一个步骤即清零（模型已按拦截意见行动）
    #[serde(default)]
    pub recent_gate_block_step: Option<u64>,
    /// 门禁文案显式指示模型去 ask_human（如预算耗尽转人工）时置位：此时求助是合法的，冷却豁免
    #[serde(default)]
    pub gate_human_escape_directed: bool,
    /// shell 类工具造成的文件变更累计数（git 探针实测；非 git 环境按每次调用计 1）。
    /// 防旁路：shell_exec 一条命令可改 N 个文件且全部绕过 write_file 门禁，
    /// 该计数让阶段审查的文件数阈值对 shell 副作用同样生效。
    #[serde(default)]
    pub uncovered_shell_mutations: u32,
    /// 阶段审查派发时的 shell 变更基线（派发前已存在的变更视为本轮已覆盖；
    /// 消费时不得覆盖此基线——与 baseline_side_effects 的竞态修复同语义）
    #[serde(default)]
    pub stage_review_shell_baseline: u32,
    /// final_only 级 blocking 的 carry-over（不拦截，带入收尾审查）
    #[serde(default)]
    pub stage_review_final_only: Vec<String>,
    /// 任务内已向人类发起过提问的问题列表（用于相同/高度相似问题去重）
    #[serde(default)]
    pub asked_human_questions: Vec<String>,
    /// 运行模式兼容字段：true 为全自动/自主模式，false 为方案审批模式
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

/// 计算两个规范化字符串的字符 Bigram Jaccard 相似度（0.0 ~ 1.0）
pub fn question_bigram_similarity(s1: &str, s2: &str) -> f64 {
    let chars1: Vec<char> = s1.chars().collect();
    let chars2: Vec<char> = s2.chars().collect();
    if chars1.len() < 2 || chars2.len() < 2 {
        return if s1 == s2 { 1.0 } else { 0.0 };
    }

    use std::collections::HashSet;
    let mut bg1 = HashSet::new();
    for window in chars1.windows(2) {
        bg1.insert((window[0], window[1]));
    }

    let mut bg2 = HashSet::new();
    for window in chars2.windows(2) {
        bg2.insert((window[0], window[1]));
    }

    let intersection = bg1.intersection(&bg2).count();
    let union = bg1.union(&bg2).count();
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

/// 单个任务允许向用户发起信息类提问的默认最大配额（防琐碎问题没完没了；审批与私密凭据类求助不受此限）
pub const DEFAULT_MAX_HUMAN_INQUIRY_BUDGET: usize = 5;

/// 识别是否属于私密凭据/鉴权类提问（密码、Token、API Key 等，无合理默认值，豁免配额熔断）
pub fn is_credential_inquiry_kind(kind: &str) -> bool {
    let k = kind.trim().to_ascii_lowercase();
    k == "missing_credential"
        || k == "credential"
        || k == "secret"
        || k == "password"
        || k == "token"
        || k == "api_key"
        || k == "auth"
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
            plan_mode: PlanExecutionMode::Auto,
            plan_reviewer_consumed: false,
            last_review_scorecard: None,
            last_review_blocking_count: 0,
            review_objection_added: 0,
            review_objection_withdrawn: 0,
            review_objection_maintained: 0,
            reviewer_budget_refunds: 0,
            review_total_withdrawn: 0,
            review_total_maintained: 0,
            stage_review_budget_used: 0,
            stage_review_run_id: None,
            stage_review_run_ids: Vec::new(),
            stage_review_rebuttal_run_id: None,
            stage_review_baseline_side_effects: 0,
            stage_review_blockers_open: 0,
            stage_review_confrontation_failures: 0,
            stage_review_blocker_stage: None,
            plan_reviewer_failover_count: 0,
            plan_reviewer_last_dispatch: None,
            recent_gate_block_step: None,
            gate_human_escape_directed: false,
            uncovered_shell_mutations: 0,
            stage_review_shell_baseline: 0,
            stage_review_final_only: Vec::new(),
            asked_human_questions: Vec::new(),
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

    /// 读取方案审查最近派发的强类型参数快照（若存在且格式合法）
    pub fn plan_reviewer_dispatch_snapshot(&self) -> Option<crate::agent::child_reviewer::PlanReviewerDispatchSnapshot> {
        self.plan_reviewer_last_dispatch
            .as_deref()
            .and_then(crate::agent::child_reviewer::PlanReviewerDispatchSnapshot::from_json_str)
    }

    /// 写入方案审查最近派发的强类型参数快照（底层序列化为兼容的 JSON 字符串）
    pub fn set_plan_reviewer_dispatch_snapshot(
        &mut self,
        snapshot: &crate::agent::child_reviewer::PlanReviewerDispatchSnapshot,
    ) {
        self.plan_reviewer_last_dispatch = Some(snapshot.to_json_string());
    }

    /// 消费并记录一轮合法的方案审查结论，原子更新异议与评分卡记账状态
    pub fn record_reviewer_delivery(&mut self, delivery: &crate::agent::child_reviewer::ReviewerDeliveryData) {
        let (added, withdrawn, maintained) = delivery.objection_stats();
        self.last_review_blocking_count = delivery.unwithdrawn_blocking_count();
        self.review_objection_added = added;
        self.review_objection_withdrawn = withdrawn;
        self.review_objection_maintained = maintained;
        self.review_total_withdrawn += withdrawn;
        self.review_total_maintained += maintained;
        if let Some(ref sc) = delivery.scorecard {
            self.last_review_scorecard = Some(sc.clone());
        }
        self.plan_reviewer_consumed = true;
    }

    /// 生成方案门禁状态的瞬态投影，供上下文装配层注入底部状态卡（事前可见）。
    pub fn plan_gate_projection(&self) -> crate::agent::context::types::PlanGateProjection {
        crate::agent::context::types::PlanGateProjection {
            mode: self.plan_mode.as_str().to_string(),
            phase: self.plan_phase.as_str().to_string(),
            reviewer_dispatched: self.plan_reviewer_retry_count > 0,
            reviewer_consumed: self.plan_reviewer_consumed,
            reviewer_budget_used: self.plan_reviewer_retry_count,
            reviewer_budget_max: MAX_PLAN_REVIEWER_RETRIES,
        }
    }

    /// 统一的执行模式切换入口：规范化方案阶段（人工确认过的方案永久保留）
    /// 并同步 auto_pilot 兼容字段。所有模式切换都必须走这里，
    /// 避免 reconcile / plan_mode / auto_pilot 三处赋值散落导致漏改。
    pub fn set_execution_mode(&mut self, new_mode: PlanExecutionMode) {
        self.reconcile_plan_phase_for_mode_switch(new_mode);
        self.plan_mode = new_mode;
        self.auto_pilot = matches!(
            new_mode,
            PlanExecutionMode::Auto | PlanExecutionMode::CrossReview
        );
    }

    /// 模式切换守卫：
    /// 1. 从全自动或交叉审查切为方案审批模式时，自动授予（无人工留痕）的 Approved 不再可信，重置为 Reviewing；
    ///    人工批准过的方案（approved_plan_summary 有留痕）保持放行，避免误伤已确认状态。
    /// 2. 切换为交叉审查模式时，若当前的 Approved 是全自动盲放行（既无人工审批也未消费过审查结论），
    ///    同样重置为 Reviewing 要求完成审查推演；
    pub fn reconcile_plan_phase_for_mode_switch(&mut self, new_mode: PlanExecutionMode) {
        match new_mode {
            PlanExecutionMode::PlanApproval => {
                if self.plan_phase.is_approved() && self.approved_plan_summary.is_none() {
                    self.plan_phase = PlanPhase::Reviewing;
                }
            }
            PlanExecutionMode::CrossReview => {
                if self.plan_phase.is_approved()
                    && self.approved_plan_summary.is_none()
                    && !self.plan_reviewer_consumed
                {
                    self.plan_phase = PlanPhase::Reviewing;
                }
            }
            PlanExecutionMode::Auto => {}
        }
    }

    /// 预扣一轮方案审查预算。已到上限返回 Err（调用方应拒绝派发并引导人工放行）。
    ///
    /// 所有消耗审查轮次的路径（常规派发、resume_child 反驳对质）都必须走这里，
    /// 防止旁路无上限自增绕过熔断。
    pub fn reserve_plan_review_round(&mut self) -> Result<(), String> {
        let max_rounds = max_plan_reviewer_rounds();
        if self.plan_reviewer_retry_count >= max_rounds {
            return Err(format!("方案审查轮次预算已耗尽（上限 {} 轮）", max_rounds));
        }
        self.plan_reviewer_retry_count += 1;
        self.plan_reviewer_consumed = false;
        Ok(())
    }

    /// 判定是否存在在途的阶段审查任务（支持单审与联合会审）
    pub fn has_in_flight_stage_review(&self) -> bool {
        self.stage_review_run_id.is_some()
            || !self.stage_review_run_ids.is_empty()
            || self.stage_review_rebuttal_run_id.is_some()
    }

    /// 判定指定任务 ID 是否属于当前在途的阶段审查任务
    pub fn matches_stage_review_run(&self, run_id: &str) -> bool {
        self.stage_review_run_id.as_deref() == Some(run_id)
            || self.stage_review_run_ids.iter().any(|r| r == run_id)
    }

    /// 移除已完成的阶段审查任务 ID；若列表中仍有其他任务则继续维持，否则清空
    pub fn remove_stage_review_run(&mut self, run_id: &str) {
        self.stage_review_run_ids.retain(|r| r != run_id);
        if self.stage_review_run_id.as_deref() == Some(run_id) {
            self.stage_review_run_id = self.stage_review_run_ids.first().cloned();
        }
    }

    /// 清空全部阶段审查在途任务 ID
    pub fn clear_stage_review_runs(&mut self) {
        self.stage_review_run_id = None;
        self.stage_review_run_ids.clear();
        self.stage_review_rebuttal_run_id = None;
    }

    /// 审查者技术失败的审查轮次退款：超时/取消/中断/非法输出不是实质性审查，不应烧掉
    /// 请求方的预算——技术失败和业务驳回是两笔账（实测教训：error_kind 落库值是
    /// "timeout" 而非 "timed_out"，且 invalid_output 未纳入退款，glm 实弹中模型为
    /// 审查员的故障买了单）。退款封顶 3 次，防止持续故障下预算永不耗尽、模型无限空转。
    pub fn refund_infra_failed_review_round(&mut self, error_kind: Option<&str>) -> bool {
        let infra = matches!(
            error_kind,
            Some("timed_out")
                | Some("timeout")
                | Some("interrupted")
                | Some("cancelled")
                | Some("invalid_output")
        );
        if !infra || self.reviewer_budget_refunds >= 3 || self.plan_reviewer_retry_count == 0 {
            return false;
        }
        self.plan_reviewer_retry_count -= 1;
        self.reviewer_budget_refunds += 1;
        true
    }

    /// 对质失败自动降级：blocking 异议经对质仍被审查员维持（confirmed 证据拒不撤回）
    /// 时累计失败次数；达到 threshold 后把未解决 blockers 降级为 final_only 记账
    /// 带入收尾审查，blockers_open 清零放行写入。返回 true = 本次触发降级。
    /// 空行（异议已全部撤回/解决）时重置计数。
    pub fn note_stage_review_confrontation_maintained(
        &mut self,
        blocker_lines: Vec<String>,
        threshold: u32,
    ) -> bool {
        if blocker_lines.is_empty() {
            self.stage_review_confrontation_failures = 0;
            return false;
        }
        self.stage_review_confrontation_failures += 1;
        if self.stage_review_confrontation_failures < threshold {
            return false;
        }
        for line in blocker_lines {
            let tagged = format!("【对质未撤回自动降级】{line}");
            if !self
                .stage_review_final_only
                .iter()
                .any(|existing| existing == &tagged)
            {
                self.stage_review_final_only.push(tagged);
            }
        }
        self.stage_review_blockers_open = 0;
        self.stage_review_blocker_stage = None;
        self.stage_review_confrontation_failures = 0;
        true
    }

    /// 僵尸 blocker 清账：blocker 出生阶段已被宿主判分越过 → 该阶段实现已被客观
    /// 判定通过，遗留的阶段审查拦截自动清零并以 final_only 记账带入收尾。
    /// 返回 Some(说明文案) 表示发生了清理。oracle 进度缺失/未越过时不动
    /// （fail-closed：对质与人工仲裁仍是唯一出口）。旧快照无出生阶段字段时同样不动。
    pub fn clear_stale_stage_blockers(&mut self, oracle_index: u32) -> Option<String> {
        if self.stage_review_blockers_open == 0 {
            return None;
        }
        let born = self.stage_review_blocker_stage?;
        if oracle_index <= born {
            return None;
        }
        self.stage_review_final_only.push(format!(
            "🧟 僵尸 blocker 清账：出生于阶段索引 {born} 的 {} 条 blocking_regardless，在宿主判分推进到索引 {oracle_index} 后仍未清零，自动降级 final_only 带入收尾",
            self.stage_review_blockers_open
        ));
        self.stage_review_blockers_open = 0;
        self.stage_review_blocker_stage = None;
        Some(format!(
            "⏰ 宿主判分已越过历史阶段（blocker 出生索引 {born} < 当前进度 {oracle_index}），遗留的阶段审查 blocker 属僵尸拦截，已自动清账放行；遗留项以 final_only 带入收尾审查。"
        ))
    }

    /// 回滚一次「未产生任何后台任务」的审查派发：归还审查轮次并恢复派发前的消费状态。
    ///
    /// 派发失败（角色卡未授权/不存在、参数非法等）不应消耗审查预算，否则额度耗尽后
    /// 既无法重新派发、又永远等不到结论，交叉审查模式会永久阻塞写入。
    pub fn rollback_plan_review_dispatch(&mut self, pre_dispatch_consumed: bool) {
        self.plan_reviewer_retry_count = self.plan_reviewer_retry_count.saturating_sub(1);
        self.plan_reviewer_consumed = pre_dispatch_consumed;
    }

    /// 规范化提问文本以便去重比对：去除空白、常见中英文标点符号并转为小写
    pub fn normalize_question(text: &str) -> String {
        text.chars()
            .filter(|c| {
                !c.is_whitespace()
                    && !c.is_ascii_punctuation()
                    && !matches!(
                        *c,
                        '？' | '?'
                            | '。'
                            | '.'
                            | '！'
                            | '!'
                            | '，'
                            | ','
                            | '：'
                            | ':'
                            | '；'
                            | ';'
                            | '“'
                            | '”'
                            | '"'
                            | '\''
                            | '`'
                            | '【'
                            | '】'
                            | '['
                            | ']'
                            | '（'
                            | '）'
                            | '('
                            | ')'
                            | '、'
                            | '《'
                            | '》'
                    )
            })
            .flat_map(|c| c.to_lowercase())
            .collect()
    }

    /// 检查信息类提问是否已达到配额上限（私密凭据类提问在调用处豁免）
    pub fn check_inquiry_quota_exceeded(&self, budget: usize) -> Option<String> {
        if self.asked_human_questions.len() >= budget {
            Some(format!(
                "【信息提问配额已耗尽】本任务向用户提问已达上限（{}/{} 次）。为避免过度打扰用户，系统已关闭主动信息提问通道。\n\
请直接结合用户此前的全部输入、上下文已有信息及行业通用规范，采用最合理通用的默认参数与假设坚决推进执行；在最终输出中明确注明所采用的默认设定即可，严禁继续停顿提问。\n\
（注：若确需索取用户私密凭据/API Key，请在表单中设置 kind: \"missing_credential\" 申请凭据豁免通道）。",
                self.asked_human_questions.len(),
                budget
            ))
        } else {
            None
        }
    }

    /// 检查是否为重复提问。如果重复，返回拦截拒绝提示；否则返回 None。
    pub fn check_duplicate_question(&self, question: &str) -> Option<String> {
        let norm_incoming = Self::normalize_question(question);
        if norm_incoming.is_empty() {
            return None;
        }

        for past in &self.asked_human_questions {
            let norm_past = Self::normalize_question(past);
            if norm_past.is_empty() {
                continue;
            }

            // 1. 规范化后完全相同（剔除标点与空格后一致）
            if norm_incoming == norm_past {
                return Some(format!(
                    "【重复提问拦截】检测到你在本任务中此前已向用户询问过相同问题：\"{}\"。\n\
用户此前的回复已存在于上下文（【人类回复】）中。\n\
- 若此前提问已涵盖该信息，请直接结合已有回复或行业默认值继续执行，切勿就同一事项反复打扰用户；\n\
- 若确需针对不同的新参数/新槽位（如不同字段、新实体或不同操作）提问，请换用更具体详细的独立表述重新提问。",
                    past
                ));
            }

            // 2. 包含关系且差异字符纯为语气助词/口癖（如仅多了“呢”、“呀”、“吧”、“啊”、“吗”等，字符差 <= 2）
            let len_diff = norm_incoming.chars().count().abs_diff(norm_past.chars().count());
            if len_diff <= 2
                && (norm_incoming.contains(&norm_past) || norm_past.contains(&norm_incoming))
            {
                let diff_chars: String = if norm_incoming.len() > norm_past.len() {
                    norm_incoming.replace(&norm_past, "")
                } else {
                    norm_past.replace(&norm_incoming, "")
                };
                let is_filler = diff_chars.chars().all(|c| matches!(c, '呢' | '呀' | '吧' | '啊' | '吗' | '哈' | '请' | '问'));
                if is_filler {
                    return Some(format!(
                        "【重复提问拦截】检测到你在本任务中此前已向用户询问过高度相似的问题：\"{}\"。\n\
用户此前的回复已存在于上下文（【人类回复】）中。\n\
- 若此前提问已涵盖该信息，请直接结合已有回复或行业默认值继续执行，切勿就同一事项反复打扰用户；\n\
- 若确需针对不同的新参数/新槽位（如不同字段、新实体或不同操作）提问，请换用更具体详细的独立表述重新提问。",
                        past
                    ));
                }
            }

            // 3. 高度相似比对（Bigram Jaccard 相似度 >= 0.88，且两串长度均至少 8 个字符）
            // 采用 0.88 阈值保护“同模板不同槽位”的合法提问（例如“查询哪个城市” vs “查询哪个日期”）不被误伤
            if norm_incoming.chars().count() >= 8 && norm_past.chars().count() >= 8 {
                if question_bigram_similarity(&norm_incoming, &norm_past) >= 0.88 {
                    return Some(format!(
                        "【重复提问拦截】检测到你在本任务中此前已向用户询问过高度相似的问题：\"{}\"。\n\
用户此前的回复已存在于上下文（【人类回复】）中。\n\
- 若此前提问已涵盖该信息，请直接结合已有回复或行业默认值继续执行，切勿就同一事项反复打扰用户；\n\
- 若确需针对不同的新参数/新槽位（如不同字段、新实体或不同操作）提问，请换用更具体详细的独立表述重新提问。",
                        past
                    ));
                }
            }
        }

        None
    }

    /// 记录已发给人类的提问
    pub fn record_asked_question(&mut self, question: &str) {
        let trimmed = question.trim();
        if !trimmed.is_empty() && !self.asked_human_questions.iter().any(|q| q == trimmed) {
            self.asked_human_questions.push(trimmed.to_string());
        }
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
        // 成功提交一步 = 模型已行动（按拦截意见做了事），ask_human 冷却解除
        self.recent_gate_block_step = None;
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

        // ⚠️ db.0 是不可重入的 std Mutex：持有本锁期间绝不能再调 load_checkpoint——
        // 同线程二次加锁 = 永久自死锁。实测事故（09-07 pro 实弹）：wait_user 时仲裁
        // 观察器与本保存方并发推进 version → 乐观锁冲突 → 锁内 load → 全进程 DB
        // 使用者（IPC 命令/监控/仲裁/HTTP）全部排队冻结、tokio 工作线程耗尽、
        // 连不碰 DB 的 HTTP 路由也无人接单。因此锁内只做一次写入尝试，
        // 冲突必须先 Drop 锁再走 load+merge 重试。
        enum WriteOutcome {
            Saved,
            Conflict,
        }
        let outcome = {
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
                WriteOutcome::Saved
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
                    WriteOutcome::Saved
                } else {
                    // 乐观锁冲突：数据库中的 version 已被其他保存者推进。
                    checkpoint.version = previous_version;
                    WriteOutcome::Conflict
                }
            }
        };
        // conn 守卫已在此 Drop——重试路径再碰 db.0 是安全的。

        match outcome {
            WriteOutcome::Saved => return Ok(()),
            WriteOutcome::Conflict => {
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
                        latest.plan_mode = checkpoint.plan_mode;
                        latest.plan_reviewer_consumed = checkpoint.plan_reviewer_consumed;
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
                        checkpoint.plan_mode = latest.plan_mode;
                        checkpoint.plan_reviewer_consumed = latest.plan_reviewer_consumed;
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCheckpointSummary {
    pub run_id: String,
    pub phase: String,
    #[serde(default)]
    pub plan_phase: Option<String>,
    #[serde(default)]
    pub plan_mode: Option<String>,
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
                cp.pending
                    .as_ref()
                    .and_then(|p| p.instruction.get_params().get("form").cloned())
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
            plan_mode: Some(cp.plan_mode.as_str().to_string()),
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

    #[test]
    fn stale_stage_blocker_clears_when_oracle_passes_birth_stage() {
        // 09-07 pro 终局实测：阶段 07 已过宿主判分，旧 blocker 仍全局拦写（僵尸拦截）。
        // 规则：宿主判分越过 blocker 出生索引 → 自动清账（final_only 记账）；未越过或
        // 无出生索引（旧快照）→ 不动，对质与仲裁仍是唯一出口。
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("run-zombie".to_string(), &context);
        cp.stage_review_blocker_stage = Some(2);
        cp.stage_review_blockers_open = 1;

        assert!(cp.clear_stale_stage_blockers(2).is_none(), "未越过不清");
        assert_eq!(cp.stage_review_blockers_open, 1);

        let note = cp.clear_stale_stage_blockers(3).expect("越过应清账");
        assert_eq!(cp.stage_review_blockers_open, 0);
        assert!(cp.stage_review_blocker_stage.is_none());
        assert!(cp.stage_review_final_only.iter().any(|l| l.contains("僵尸 blocker")));
        assert!(note.contains("僵尸拦截"));

        cp.stage_review_blockers_open = 1;
        cp.stage_review_blocker_stage = None;
        assert!(cp.clear_stale_stage_blockers(9).is_none(), "无出生索引不清");
        assert_eq!(cp.stage_review_blockers_open, 1);
    }

    #[test]
    fn save_checkpoint_conflict_retry_does_not_relock_self() {
        // 回归：09-07 pro 实弹全局冻结案。save_checkpoint 曾在持有 db.0 锁的 scope 内
        // 调 load_checkpoint（同线程二次加锁 std Mutex）→ 永久自死锁，全进程 DB 使用者
        // 排队冻结。本测试走乐观锁冲突重试路径：修复前悬挂，修复后应正常保存。
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE agent_checkpoints (
                run_id TEXT PRIMARY KEY, phase TEXT NOT NULL, snapshot_json TEXT NOT NULL,
                version INTEGER NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);",
        )
        .unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-conflict".to_string(), &context);
        save_checkpoint(&db, &mut checkpoint).unwrap();

        // 模拟另一保存者（如 wait_user 时的仲裁观察器）并发把库中 version 顶到 5。
        {
            let conn = db.0.lock().unwrap();
            conn.execute(
                "UPDATE agent_checkpoints SET version = 5 WHERE run_id = 'run-conflict'",
                [],
            )
            .unwrap();
        }

        // 本方仍持内存 version=1 → UPDATE WHERE version=1 不命中 → 冲突 → 锁外重试。
        save_checkpoint(&db, &mut checkpoint).unwrap();

        let reloaded = load_checkpoint(&db, "run-conflict")
            .unwrap()
            .expect("checkpoint 应存在");
        assert_eq!(reloaded.version, 6, "合并后应基于库中 v5 再写 v6");
    }

    #[test]
    fn plan_gate_projection_mirrors_checkpoint_facts() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-test".to_string(), &context);
        checkpoint.plan_mode = PlanExecutionMode::CrossReview;
        checkpoint.plan_phase = PlanPhase::Reviewing;
        checkpoint.plan_reviewer_retry_count = 1;
        checkpoint.plan_reviewer_consumed = false;

        let proj = checkpoint.plan_gate_projection();
        assert_eq!(proj.mode, "cross_review");
        assert_eq!(proj.phase, "reviewing");
        assert!(proj.reviewer_dispatched);
        assert!(!proj.reviewer_consumed);
        assert_eq!(proj.reviewer_budget_used, 1);
        assert_eq!(proj.reviewer_budget_max, MAX_PLAN_REVIEWER_RETRIES);
    }

    #[test]
    fn plan_gate_projection_defaults_to_auto_exploring_undispatched() {
        let context = SandwichContext::new("system".to_string(), "goal".to_string());
        let checkpoint = AgentCheckpoint::new("run-test".to_string(), &context);

        let proj = checkpoint.plan_gate_projection();
        assert_eq!(proj.mode, "auto");
        assert_eq!(proj.phase, "exploring");
        assert!(!proj.reviewer_dispatched);
        assert!(!proj.reviewer_consumed);
        assert_eq!(proj.reviewer_budget_used, 0);
    }

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
            affected_paths: Vec::new(),
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
                verify_command: None,
                verify_script: None,
                verify_sha256: None,
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

    #[test]
    fn test_question_deduplication() {
        let context = SandwichContext::new("system".to_string(), "查天气".to_string());
        let mut cp = AgentCheckpoint::new("run-dedup".to_string(), &context);

        // 初始无问题：不重复
        assert!(cp.check_duplicate_question("请问你想查询哪个城市的天气？").is_none());

        // 记录第一个提问
        cp.record_asked_question("请问你想查询哪个城市的天气？");
        assert_eq!(cp.asked_human_questions.len(), 1);

        // 1. 完全相同的问题：拦截
        let dup1 = cp.check_duplicate_question("请问你想查询哪个城市的天气？");
        assert!(dup1.is_some());
        assert!(dup1.unwrap().contains("重复提问拦截"));

        // 2. 标点符号与空格变种：拦截
        let dup2 = cp.check_duplicate_question("  请问你想查询哪个城市的天气!  ");
        assert!(dup2.is_some());

        // 3. 口癖轻微变种（包含关系且差值纯为语气助词）：拦截
        let dup3 = cp.check_duplicate_question("请问你想查询哪个城市的天气呢？");
        assert!(dup3.is_some());

        // 4. 同模板不同槽位提问（城市 vs 日期）：绝不误伤，必须放行！
        assert!(cp.check_duplicate_question("您想查询哪个日期的天气？").is_none());
        assert!(cp.check_duplicate_question("请问你想查询哪个日期的天气？").is_none());

        // 5. 相似指标/参数槽位提问（指标A vs 指标B）：放行
        assert!(cp.check_duplicate_question("请提供要分析的指标A").is_none());
        cp.record_asked_question("请提供要分析的指标A");
        assert!(cp.check_duplicate_question("请提供要分析的指标B").is_none());

        // 6. 提问配额约束（全局封顶，可配默认 5 次）
        assert!(cp.check_inquiry_quota_exceeded(DEFAULT_MAX_HUMAN_INQUIRY_BUDGET).is_none());
        cp.record_asked_question("问题3");
        cp.record_asked_question("问题4");
        cp.record_asked_question("问题5");
        assert_eq!(cp.asked_human_questions.len(), 5);
        let quota_err = cp.check_inquiry_quota_exceeded(DEFAULT_MAX_HUMAN_INQUIRY_BUDGET);
        assert!(quota_err.is_some());
        assert!(quota_err.unwrap().contains("信息提问配额已耗尽"));

        // 7. 凭据类提问识别（豁免配额熔断）
        assert!(is_credential_inquiry_kind("missing_credential"));
        assert!(is_credential_inquiry_kind("credential"));
        assert!(is_credential_inquiry_kind("SECRET"));
        assert!(is_credential_inquiry_kind("api_key"));
        assert!(is_credential_inquiry_kind("password"));
        assert!(!is_credential_inquiry_kind("missing_parameter"));
        assert!(!is_credential_inquiry_kind("text"));
        assert!(!is_credential_inquiry_kind("plan_approval"));
    }

    #[test]
    fn plan_reviewer_dispatch_snapshot_roundtrip_and_defaults() {
        let ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("test-run-snap".to_string(), &ctx);
        assert!(cp.plan_reviewer_dispatch_snapshot().is_none());

        // 1. 写入强类型快照
        let snap = crate::agent::child_reviewer::PlanReviewerDispatchSnapshot::new(
            "gemini-2.5-pro",
            "pro",
            "high",
            "code_reviewer",
            serde_json::json!({"question": "check bounds"}),
        );
        cp.set_plan_reviewer_dispatch_snapshot(&snap);

        // 2. 验证底层存储为合法 JSON 字符串
        let raw_str = cp.plan_reviewer_last_dispatch.as_ref().unwrap();
        assert!(raw_str.contains("gemini-2.5-pro"));

        // 3. 读取强类型快照无损恢复
        let recovered = cp.plan_reviewer_dispatch_snapshot().unwrap();
        assert_eq!(recovered.model, "gemini-2.5-pro");
        assert_eq!(recovered.tier, "pro");
        assert_eq!(recovered.effort, "high");
        assert_eq!(recovered.persona, "code_reviewer");
        assert_eq!(recovered.params["question"], "check bounds");

        // 4. 兼容老版本/缺省字段：tier 和 effort 缺省时自动补齐默认值
        cp.plan_reviewer_last_dispatch = Some(r#"{"model":"gemini-flash"}"#.to_string());
        let legacy = cp.plan_reviewer_dispatch_snapshot().unwrap();
        assert_eq!(legacy.model, "gemini-flash");
        assert_eq!(legacy.tier, "flash");
        assert_eq!(legacy.effort, "high");

        // 5. 损坏 JSON 安全降级返回 None，绝不 panic
        cp.plan_reviewer_last_dispatch = Some(r#"{"corrupted json"#.to_string());
        assert!(cp.plan_reviewer_dispatch_snapshot().is_none());
    }
}

