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


mod durable;
mod inquiry_ops;
mod plan_batch;
mod plan_ops;
mod types;

pub(crate) use durable::*;
pub(crate) use plan_batch::*;
pub(crate) use types::*;

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
    /// 上一次 finish 质询时 blocker 集合的签名（用于识别"原样重发"的空转）
    #[serde(default)]
    pub last_finish_blocker_sig: Option<u64>,
    /// 同一组 blocker 连续未变的次数（>=3 时升级指引：要求改契约引用或转 ask_human）
    #[serde(default)]
    pub repeated_blocker_count: u32,
    /// 上次审查的 Diff Hash（防重复重试相同 Diff）
    #[serde(default)]
    pub last_reviewed_diff_hash: Option<String>,
    /// 上次审查结果（审查指纹=diff+交付摘要未变时复用上次拦截，避免浪费 Token）
    #[serde(default)]
    pub last_review_result: Option<crate::agent::reviewer_gatekeeper::ReviewResult>,
    /// prepare_finish 沉淀的终态收据：diff/scope/契约未变时 finish 直接复用其结论，
    /// 不重复执行义务验证与终审 LLM 调用。
    #[serde(default)]
    pub pending_finalization: Option<crate::agent::finalization::FinalizationReceipt>,
    /// 方案制定与前置门禁阶段
    #[serde(default)]
    pub plan_phase: PlanPhase,
    /// 方案审查重试计数（防子 Agent 内部讨论死循环，上限 2 轮）
    #[serde(default)]
    pub plan_reviewer_retry_count: u32,
    /// 最终批准的方案摘要（用于防方案漂移与核对）
    #[serde(default)]
    pub approved_plan_summary: Option<String>,
    /// 方案在没有任何成功审查结论的情况下被放行（人工批准/仲裁时审查全败或未派发）。
    /// 持久化披露标记：交付与报告须如实声明「未经独立审查」，不许静默放行。
    #[serde(default)]
    pub plan_unreviewed_release: bool,
    /// 人类主人明确批准的特赦/豁免指示（用于在代码审查门禁中豁免特定改动）
    #[serde(default)]
    pub human_waivers: Vec<String>,
    /// 被变更层门禁挂起、等待人工确认的 git 破坏命令指纹
    /// （shell_safety::git_cmd_fingerprint 归一化串）。
    /// 实测事故 2026-09-25：git checkout -- 销毁他人未提交工作——
    /// 目标不全在本 session 写入台账/计划核准集的 git 破坏命令须先人工确认。
    #[serde(default)]
    pub git_destructive_pending: Vec<String>,
    /// 已在 ask_human 问题中原文引用、正在等待回复核销的指纹。
    #[serde(default)]
    pub git_destructive_asked: Vec<String>,
    /// 经人工确认放行的 git 破坏命令指纹（同一命令重发不再确认）。
    #[serde(default)]
    pub git_destructive_confirmed: Vec<String>,
    /// 策略引擎核准的目标改动文件列表（用于写后真实 Git Diff 范围防逃逸比对）
    #[serde(default)]
    pub approved_target_files: Option<Vec<String>>,
    /// 策略引擎最新评估摘要（供审计与调试展示）
    #[serde(default)]
    pub policy_decision_summary: Option<String>,
    /// 运行执行模式：Auto（全自动）、CrossReview（交叉审查）、PlanApproval（方案审批）
    #[serde(default)]
    pub plan_mode: PlanExecutionMode,
    /// 方案审查结果是否已被主循环或 wait_jobs 消费并注入上下文（“问过”且“听过”）
    #[serde(default)]
    pub plan_reviewer_consumed: bool,
    /// 旧版方案审查批次名单：仅作旧 checkpoint 反序列化入口，
    /// 加载后经 adopt_legacy_membership 收养进台账后清空；新写入不再序列化。
    #[serde(default, skip_serializing)]
    pub plan_review_batch: PlanReviewBatchState,
    /// 1v1 方案评审是否裁决必须升级多专家联合会审（Some 时存储建议拉起的专家角色列表）
    #[serde(default)]
    pub plan_escalation_required: Option<Vec<String>>,
    /// 本轮已消耗的 retriage（重新分流）次数（每 run 限额，防监管循环）
    #[serde(default)]
    pub retriage_used: u32,
    /// retriage 判决历史：追加式，永不覆盖原 triage_verdict（审计链完整）
    #[serde(default)]
    pub triage_revisions: Vec<crate::agent::types::TriageRevision>,
    /// 最近一次 submit_plan 落账的结构化方案（审查注册与派发的母本；重提时覆盖；
    /// Tech Lead 1v1 定稿后本字段即修订后的权威执行版本）
    #[serde(default)]
    pub submitted_plan: Option<crate::agent::types::PlanSubmission>,
    /// submitted_plan 落账时的 step_id（审计与血缘追踪）
    #[serde(default)]
    pub submitted_plan_step: Option<usize>,
    /// 方案最终定稿人：Some("tech_lead") = Tech Lead 在 1v1 中修订定稿，
    /// submitted_plan 已是审查者版本而非提交方原版（血缘标记，供审计与回放）
    #[serde(default)]
    pub plan_finalized_by: Option<String>,
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
    /// ── 台账锚点：成员身份/在途/齐套全部由 agent_child_runs 的
    /// batch_id + consumed_at + status 推导，checkpoint 只记"当前批次是谁"。
    /// 手写名单退役后不再有"忘了换名字"的漂移面。
    #[serde(default)]
    pub plan_review_batch_id: Option<String>,
    /// 批次登记时的成员数（派发时刻的既定事实，标量不漂移）
    #[serde(default)]
    pub plan_review_expected_count: u32,
    /// 已交付成员的解析产物缓存（run_id → 交付数据）——内容缓存而非成员名单，
    /// 幂等去重天然由 HashMap key 承担
    #[serde(default)]
    pub plan_review_deliveries: std::collections::HashMap<String, crate::agent::child_reviewer::ReviewerDeliveryData>,
    #[serde(default)]
    pub stage_review_batch_id: Option<String>,
    /// ── 旧版手写名单：仅作旧 checkpoint 反序列化入口，加载后经
    /// adopt_legacy_membership 一次性收养进台账并清空；新写入不再序列化。
    #[serde(default, skip_serializing)]
    pub stage_review_run_id: Option<String>,
    #[serde(default, skip_serializing)]
    pub stage_review_run_ids: Vec<String>,
    #[serde(default, skip_serializing)]
    pub stage_review_rebuttal_run_id: Option<String>,
    #[serde(default, skip_serializing)]
    pub stage_review_completed_run_ids: Vec<String>,
    /// 当前正在对质的目标审查任务 ID（用于对质失败计数隔离，防多专家交叉借分）
    #[serde(default)]
    pub stage_review_confrontation_target: Option<String>,
    /// 覆盖基线：上次成功阶段审查时的副作用台账长度（之后的新增 = 未覆盖写入）
    #[serde(default)]
    pub stage_review_baseline_side_effects: u32,
    /// 阶段审查未解决的 blocking_regardless 数（>0 时拦截写入，直到对质撤回）
    #[serde(default)]
    pub stage_review_blockers_open: u32,
    /// 产出当前未解决 blocker 的审查 child_run_id——run_ids 是在途表，
    /// 审查完成即被 remove_stage_review_run 清出，门禁提示需要它给 resume_child 指路
    ///（实测 goal_anchor：blocker 门禁打出 child_run_id:"" 空串，模型无处可去重试烧光预算）
    #[serde(default)]
    pub stage_review_blocker_source_run_id: Option<String>,
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
    /// 连续"整步只产出门禁/求助拒绝"的派发计数（P3 降级逃生口依据）：
    /// 写门禁拦截 + 审查轮数耗尽 + ask_human 冷却/配额 会构成互相推诿的死循环
    /// （门禁让求助→冷却让回门禁→回门禁再被拦），任何一步真实产出即清零。
    #[serde(default)]
    pub consecutive_gate_blocked: u32,
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
    /// 任务入口 LLM 分流判决（TriageVerdict）：门禁按 route 分流的权威依据。
    /// None = 未分流（旧 checkpoint 恢复或分流未执行），门禁按保守旧行为处理；
    /// 判决不推翻机械安全地板（path_guard / 删改名动作 / 多文件扩散）。
    #[serde(default)]
    pub triage_verdict: Option<crate::agent::triage::TriageVerdict>,
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

/// Tech Lead 1v1 定稿修订产物：revised_* 扁平字段经截断/过滤后的非空部分。
/// 详见 AgentCheckpoint::plan_revision_from。
#[derive(Debug, Clone, Default)]
pub struct PlanRevision {
    pub plan_summary: Option<String>,
    pub root_cause: Option<String>,
    pub target_files: Option<Vec<String>>,
    pub intended_verification: Option<String>,
}

impl PlanRevision {
    /// 被修订字段的逗号清单（观测/通知文案用）
    pub fn describe_fields(&self) -> String {
        let mut names = Vec::new();
        if self.plan_summary.is_some() {
            names.push("plan_summary");
        }
        if self.root_cause.is_some() {
            names.push("root_cause");
        }
        if self.target_files.is_some() {
            names.push("target_files");
        }
        if self.intended_verification.is_some() {
            names.push("intended_verification");
        }
        names.join(", ")
    }
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
            last_finish_blocker_sig: None,
            repeated_blocker_count: 0,
            last_reviewed_diff_hash: None,
            last_review_result: None,
            pending_finalization: None,
            plan_phase: PlanPhase::Exploring,
            plan_reviewer_retry_count: 0,
            approved_plan_summary: None,
            plan_unreviewed_release: false,
            human_waivers: Vec::new(),
            git_destructive_pending: Vec::new(),
            git_destructive_asked: Vec::new(),
            git_destructive_confirmed: Vec::new(),
            approved_target_files: None,
            policy_decision_summary: None,
            plan_mode: PlanExecutionMode::Auto,
            plan_reviewer_consumed: false,
            plan_review_batch: PlanReviewBatchState::default(),
            plan_review_batch_id: None,
            plan_review_expected_count: 0,
            plan_review_deliveries: std::collections::HashMap::new(),
            stage_review_batch_id: None,
            plan_escalation_required: None,
            retriage_used: 0,
            triage_revisions: Vec::new(),
            submitted_plan: None,
            submitted_plan_step: None,
            plan_finalized_by: None,
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
            stage_review_completed_run_ids: Vec::new(),
            stage_review_confrontation_target: None,
            stage_review_baseline_side_effects: 0,
            stage_review_blockers_open: 0,
            stage_review_blocker_source_run_id: None,
            stage_review_confrontation_failures: 0,
            stage_review_blocker_stage: None,
            plan_reviewer_failover_count: 0,
            plan_reviewer_last_dispatch: None,
            recent_gate_block_step: None,
            gate_human_escape_directed: false,
            consecutive_gate_blocked: 0,
            uncovered_shell_mutations: 0,
            stage_review_shell_baseline: 0,
            stage_review_final_only: Vec::new(),
            asked_human_questions: Vec::new(),
            triage_verdict: None,
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

enum WriteOutcome {
    Saved,
    Conflict,
}

/// 在已持有的连接上做一次乐观锁写入（INSERT 首存 / UPDATE 乐观锁）。
/// 冲突时恢复 checkpoint.version 并返回 Conflict，由调用方决定重试策略。
/// 供 save_checkpoint（独立提交）与 save_checkpoint_and_consume（同事务附带
/// 回执 applied_at 落账）共用——两个入口的乐观锁语义必须完全一致。
fn write_checkpoint_on(
    conn: &rusqlite::Connection,
    checkpoint: &mut AgentCheckpoint,
) -> Result<WriteOutcome, String> {
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
        Ok(WriteOutcome::Saved)
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
            Ok(WriteOutcome::Saved)
        } else {
            // 乐观锁冲突：数据库中的 version 已被其他保存者推进。
            checkpoint.version = previous_version;
            Ok(WriteOutcome::Conflict)
        }
    }
}

/// 乐观锁冲突后的重载+合并：把当前持有的关键字段叠加到库中最新快照上。
/// ⚠️ 必须在释放 db.0 锁之后调用（内部会再取锁），否则同线程二次加锁自死锁。
fn reload_and_merge_checkpoint(db: &DbState, checkpoint: &mut AgentCheckpoint) -> Result<(), String> {
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
    Ok(())
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
        let outcome = {
            let conn = db.0.lock().map_err(|_| "数据库锁已中毒".to_string())?;
            write_checkpoint_on(&conn, checkpoint)?
        };
        // conn 守卫已在此 Drop——重试路径再碰 db.0 是安全的。

        match outcome {
            WriteOutcome::Saved => return Ok(()),
            WriteOutcome::Conflict => {
                if attempt >= max_attempts {
                    return Err("checkpoint 已被另一条任务更新，请重新加载后再保存".to_string());
                }
                // 自动重试：重新加载最新 checkpoint 并合并当前关键状态。
                reload_and_merge_checkpoint(db, checkpoint)?;
            }
        }
    }
}

/// 一张已投递回执的落账记录：applied_at 随 checkpoint 在同一事务写入。
/// verdict = 消费端裁决回写（如契约违规降级为 invalid_output）——
/// 让台账 status/error_kind 与模型所见一致，不再"内存判违规、库里记成功"。
pub struct AppliedReceipt {
    pub run_id: String,
    /// (status, error_kind)；None = 按原终态仅记 applied_at
    pub verdict: Option<(String, String)>,
}

/// 回执消费提交点：checkpoint 快照 + 本批回执 applied_at 标记 + 裁决回写
/// 在同一 SQLite 事务内原子落账。崩溃只可能落在事务两侧：
/// - COMMIT 前：两者一起回滚，行仍 applied_at IS NULL → 下次轮询重新投递，
///   重放到"从未见过该回执"的 checkpoint 上，效果只生效一次；
/// - COMMIT 后：两者一起持久化，永不重投。
/// 唯一绕过事务的副作用（failover 备胎 spawn）由 task_id 查重兜底。
///
/// context 先 refresh 进 checkpoint 再序列化——todo/注入文本等 context 变更
/// 必须与 applied_at 同生共死，否则"标了已落账但效果丢了"。
pub fn save_checkpoint_and_consume(
    db: &DbState,
    checkpoint: &mut AgentCheckpoint,
    context: &crate::agent::context::SandwichContext,
    applied: &[AppliedReceipt],
) -> Result<(), String> {
    if applied.is_empty() {
        return save_checkpoint(db, checkpoint);
    }
    checkpoint.refresh_context(context);
    let mut attempt = 0;
    let max_attempts = 3;

    loop {
        attempt += 1;
        checkpoint.updated_at = Utc::now().to_rfc3339();
        let now = checkpoint.updated_at.clone();

        let outcome = {
            let mut conn = db.0.lock().map_err(|_| "数据库锁已中毒".to_string())?;
            let tx = conn
                .transaction()
                .map_err(|error| error.to_string())?;
            match write_checkpoint_on(&tx, checkpoint)? {
                WriteOutcome::Saved => {
                    for receipt in applied {
                        if let Some((status, error_kind)) = &receipt.verdict {
                            tx.execute(
                                "UPDATE agent_child_runs SET applied_at = ?1, status = ?2, error_kind = ?3 WHERE id = ?4",
                                params![now, status, error_kind, receipt.run_id],
                            )
                            .map_err(|error| error.to_string())?;
                        } else {
                            tx.execute(
                                "UPDATE agent_child_runs SET applied_at = ?1 WHERE id = ?2",
                                params![now, receipt.run_id],
                            )
                            .map_err(|error| error.to_string())?;
                        }
                    }
                    tx.commit().map_err(|error| error.to_string())?;
                    WriteOutcome::Saved
                }
                // 冲突：事务随 drop 回滚，applied_at 标记绝不先于 checkpoint 落盘
                WriteOutcome::Conflict => WriteOutcome::Conflict,
            }
        };

        match outcome {
            WriteOutcome::Saved => return Ok(()),
            WriteOutcome::Conflict => {
                if attempt >= max_attempts {
                    return Err("checkpoint 已被另一条任务更新，请重新加载后再保存".to_string());
                }
                reload_and_merge_checkpoint(db, checkpoint)?;
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
            user_goal: if cp.data.current_goal.is_empty() {
                cp.data.user_goal
            } else {
                cp.data.current_goal
            },
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
mod tests;
