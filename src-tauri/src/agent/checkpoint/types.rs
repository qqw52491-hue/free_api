//! 检查点类型层：阶段枚举、恢复策略与挂起工具调用。

use super::*;
use crate::agent::context::SandwichContext;
use crate::agent::control::HumanControl;
use crate::agent::types::{AgentInstruction, DispatchResult};
use crate::db::DbState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tauri::AppHandle;

pub(crate) const CHECKPOINT_SCHEMA_VERSION: u32 = 2;
pub(crate) const MAX_OBSERVATION_CHARS: usize = 4_000;
/// 跨 finish 继承时台账只保留最近 N 条，避免底部状态卡随轮次线性膨胀。
pub(crate) const MAX_INHERITED_SIDE_EFFECTS: usize = 12;

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

    if tool=="child_task" || (tool=="core" && action=="child_task") {
        // 宿主派发键/消息键绑定当前逻辑任务和步骤；重放认养已有记录，子执行器保留自身恢复规则。
        return RecoveryPolicy::Idempotent;
    }
    if tool == "task_store" {
        return match action.as_str() {
            "list_resources" | "get_resource" | "list_records" | "get_record"
            | "evidence_find" | "evidence_inspect" | "evidence_compare" | "get_work_group" => {
                RecoveryPolicy::SafeRetry
            }
            // 显式键重试保留首次观察；无键会追加新记录，重试最多产生重复观察，
            // 不覆盖既有证据或执行验证，故可安全重试但不宣称幂等。
            "evidence_record" => {
                if instruction.get_params().get("idempotency_key")
                    .and_then(serde_json::Value::as_str).is_some_and(|key| !key.trim().is_empty()) {
                    RecoveryPolicy::Idempotent
                } else {
                    RecoveryPolicy::SafeRetry
                }
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
