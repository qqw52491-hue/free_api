use super::*;
use crate::agent::types::{ChildErrorKind, TokenUsage};
use crate::commands::projects::read_project_resource_excerpts_for_session;
use crate::db::DbState;
use crate::llm::{self, ModelConfig};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;
use uuid::Uuid;


pub const MAX_CHILDREN_PER_KIND_PER_RUN: u8 = 2;
pub const MAX_TOTAL_CHILDREN_PER_RUN: u8 = 6;
// 向后兼容原有常量别名
pub const MAX_CHILD_REVIEWS_PER_PARENT_RUN: u8 = MAX_CHILDREN_PER_KIND_PER_RUN;
    /// reviewer 首个正文/思考与后续静默时限（秒），可通过 FREE_API_REVIEWER_TIMEOUT_SECS 调整。
    /// 持续输出刷新静默计时，但不延长 FREE_API_LLM_CALL_BUDGET_SECONDS 的调用总预算。
    /// 实测教训：45s 对慢平台/大证据包太紧——审查超时→退款→重派→再超时，死循环到模型弃疗。
    /// 慢思考家族（Claude thinking 等）默认放宽到 120s：单审常超 45s，实测流错误+超时连发。
    /// pub(crate)：resume_child 复审路径（loop_runner）同样复用本模型感知超时。
    pub(crate) fn child_timeout_secs(model_name: &str) -> u64 {
        std::env::var("FREE_API_REVIEWER_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|v| v.clamp(30, 600))
            .unwrap_or_else(|| {
                let thinking_class = matches!(
                    crate::agent::model_filter::detect_model_family(model_name, ""),
                    crate::agent::model_filter::ModelFamily::ClaudeThinking
                )
                // "-high" 等推理档位后缀：实测 finish-review 落账 gemini-3.1-pro-high
                // 35s 超时率 29/70（41%），同会话普通 pro 仅 1/12——高档位必须按
                // 慢思考家族给超时，否则取证环每轮都在赌延迟。
                || model_name.to_lowercase().contains("-high");
                if thinking_class {
                    120
                } else {
                    // 60s：实测 pro 审查大证据包 45s 下 2/4 超时（quota 健康时也逼近上限）
                    60
                }
            })
    }

    /// reap 宽限覆盖最长的静默重试等待，并受既有调用总预算约束。
    pub(crate) fn max_child_timeout_secs() -> u64 {
        let base = std::env::var("FREE_API_REVIEWER_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|v| v.clamp(30, 600))
            .unwrap_or(120);
        base.saturating_mul(2).min(llm::llm_call_total_budget_ms().div_ceil(1000))
    }

pub(crate) const CHILD_MAX_OUTPUT_TOKENS: i64 = 900;
pub(crate) const MAX_QUESTION_CHARS: usize = 2_000;
pub(crate) const MAX_EVIDENCE_CHARS: usize = 6_000;
/// 引用的容量预算（Unicode 字符，含条目间分隔）；不是证据质量或数量要求。
pub(crate) const MAX_REVIEW_REFERENCE_CHARS: usize = 16_384;
pub(crate) const MAX_CHILD_RESOURCE_IDS: usize = 2;
pub(crate) const MAX_FINDING_CHARS: usize = 700;
pub(crate) const MAX_MISSING_EVIDENCE_CHARS: usize = 300;

pub(crate) fn discard_stream_event(_: llm::StreamEvent<'_>) -> bool {
    true
}

#[derive(Debug, Clone)]
pub struct ChildReviewRequest {
    pub(crate) question: String,
    pub(crate) evidence_excerpt: String,
    pub(crate) evidence_refs: Vec<String>,
    pub(crate) resource_ids: Vec<String>,
}

pub(crate) fn default_dispatch_tier() -> String {
    "flash".to_string()
}

pub(crate) fn default_dispatch_effort() -> String {
    "high".to_string()
}

/// 方案审查派发参数快照（强类型 DTO，用于审查员故障转移与重派）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanReviewerDispatchSnapshot {
    pub model: String,
    #[serde(default = "default_dispatch_tier")]
    pub tier: String,
    #[serde(default = "default_dispatch_effort")]
    pub effort: String,
    #[serde(default)]
    pub persona: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub custom_schema: crate::agent::protocol::ContractSchema,
    #[serde(default)]
    pub focus_domains: Vec<crate::agent::protocol::FocusDomain>,
    #[serde(default)]
    pub specialized_context: Option<String>,
    /// 用途标记（plan_review 等），落库到 agent_child_runs.purpose
    #[serde(default)]
    pub purpose: Option<String>,
    /// 同批联合会审归组 id，落库到 agent_child_runs.batch_id
    #[serde(default)]
    pub batch_id: Option<String>,
}

impl PlanReviewerDispatchSnapshot {
    pub(crate) fn from_envelope(env: &crate::agent::protocol::DispatchEnvelope) -> Self {
        Self { model: env.model_id.clone(), tier: env.model_tier.clone(), effort: env.reasoning_effort.clone(),
            persona: env.persona.clone(), params: env.input_payload.clone(), custom_schema: env.custom_schema.clone(),
            focus_domains: env.focus_domains.clone(), specialized_context: env.specialized_context.clone(),
            purpose: env.purpose.clone(), batch_id: env.batch_id.clone() }
    }

    pub fn new(
        model: impl Into<String>,
        tier: impl Into<String>,
        effort: impl Into<String>,
        persona: impl Into<String>,
        params: Value,
    ) -> Self {
        Self {
            model: model.into(),
            tier: tier.into(),
            effort: effort.into(),
            persona: persona.into(),
            params,
            custom_schema: Default::default(),
            focus_domains: Vec::new(),
            specialized_context: None,
            purpose: None,
            batch_id: None,
        }
    }

    pub fn with_purpose(mut self, purpose: impl Into<String>) -> Self {
        self.purpose = Some(purpose.into());
        self
    }

    pub fn with_batch_id(mut self, batch_id: impl Into<String>) -> Self {
        self.batch_id = Some(batch_id.into());
        self
    }

    pub fn with_custom_schema(mut self, schema: crate::agent::protocol::ContractSchema) -> Self {
        self.custom_schema = schema;
        self
    }

    pub fn with_focus_domains(mut self, domains: Vec<crate::agent::protocol::FocusDomain>) -> Self {
        self.focus_domains = domains;
        self
    }

    pub fn with_specialized_context(mut self, ctx: impl Into<String>) -> Self {
        self.specialized_context = Some(ctx.into());
        self
    }

    pub fn to_json_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    pub fn from_json_str(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }

    /// 依据当前快照派发子审查任务。
    /// `retry_of` 为重试/故障转移的前驱 run_id：新 run 继承其 task_id
    /// （attempt_id 由落库侧按同 task_id 自动递增），逻辑任务身份不漂移。
    pub async fn spawn(
        &self,
        app: Option<AppHandle>,
        db: &DbState,
        parent_run_id: &str,
        parent_session_id: &str,
        workspace_root: &Path,
        interrupt_rx: watch::Receiver<u64>,
        retry_of: Option<&str>,
    ) -> Result<String, String> {
        let mut envelope = crate::agent::protocol::DispatchEnvelope::reviewer(
            parent_run_id,
            parent_session_id,
            &self.model,
            self.params.clone(),
        )
        .with_tier(&self.tier)
        .with_effort(&self.effort)
        .with_persona(&self.persona);

        // 继承前驱 run 的逻辑任务身份（task_id 复用，attempt 自动 +1）。
        // 幂等收口：若同 task_id 已有更晚的在途/未落账 attempt（崩溃窗口里备胎
        // 已 INSERT 但父 checkpoint 回滚，重放会再次走到这里），直接认养该行
        // 而不再 spawn——否则一次重放就会多派一个备胎。
        if let Some(prev) = retry_of {
            if let Some(prev_identity) = crate::agent::child_reviewer::load_run_identity(db, prev) {
                if let Some(live_id) = crate::agent::child_reviewer::live_attempt_for_task(
                    db,
                    parent_run_id,
                    &prev_identity.task_id,
                    prev,
                    prev_identity.attempt_id,
                ) {
                    return Ok(live_id);
                }
                envelope = envelope.with_task_id(prev_identity.task_id);
            }
        }
        if let Some(p) = &self.purpose {
            envelope = envelope.with_purpose(p.clone());
        }
        if let Some(b) = &self.batch_id {
            envelope = envelope.with_batch_id(b.clone());
        }

        if !self.custom_schema.is_empty() {
            envelope = envelope.with_custom_schema(self.custom_schema.clone());
        }

        envelope.focus_domains = self.focus_domains.clone();
        envelope.specialized_context = self.specialized_context.clone();
        envelope.dispatch(app, db, workspace_root, interrupt_rx).await
    }
}



/// 可选审查评分，仅供历史兼容、遥测与展示，不参与放行。
/// 包含正确性、破坏性风险（安全性）、代码规范、测试覆盖四个维度，各 0~100 分
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../src/types/generated/")]
pub struct ReviewScorecard {
    pub correctness: u32,
    pub risk: u32,
    pub style: u32,
    #[serde(default)]
    pub test_coverage: Option<u32>,
    #[serde(default)]
    pub null_reason: Option<String>,
    /// 历史记录是否为系统合成评分；新审查不再合成评分。
    #[serde(default)]
    pub synthesized: bool,
}

impl ReviewScorecard {
    pub fn new(correctness: u32, risk: u32, style: u32, test_coverage: Option<u32>) -> Self {
        Self {
            correctness: correctness.min(100),
            risk: risk.min(100),
            style: style.min(100),
            test_coverage: test_coverage.map(|t| t.min(100)),
            null_reason: None,
            synthesized: false,
        }
    }

    pub fn with_reason(
        correctness: u32,
        risk: u32,
        style: u32,
        test_coverage: Option<u32>,
        null_reason: Option<String>,
    ) -> Self {
        Self {
            correctness: correctness.min(100),
            risk: risk.min(100),
            style: style.min(100),
            test_coverage: test_coverage.map(|t| t.min(100)),
            null_reason,
            synthesized: false,
        }
    }

    /// 计算有效维度的综合平均分（向下取整，处理 test_coverage 为 None 的动态分母）
    pub fn average_score(&self) -> u32 {
        let mut sum = self.correctness + self.risk + self.style;
        let mut count = 3u32;
        if let Some(tc) = self.test_coverage {
            sum += tc;
            count += 1;
        }
        if count == 0 {
            0
        } else {
            sum / count
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewFinding {
    pub id: String,                 // F1..F5 —— 供反驳时定位
    pub severity: String,           // blocking | suggestion | question
    /// blocking 细分：regardless（无论后续阶段如何都是错的，阶段审查据此拦截写入）
    /// | final_only（最终交付前必须解决，中间态可接受，只记账带入收尾审查）。
    /// 缺省按 regardless 处理（fail-closed）。
    #[serde(default)]
    pub finality: String,
    #[serde(default)]
    pub location: String,           // "src/foo.rs:142"，定位不到则空串
    #[serde(default)]
    pub current: String,            // 现在是什么
    #[serde(default)]
    pub why: String,                // 为什么是问题
    #[serde(default)]
    pub suggestion: String,         // 建议改法
    #[serde(default)]
    pub evidence_strength: String,  // confirmed | supported | unverified（逐条）
}

impl ReviewFinding {
    pub fn display_line(&self) -> String {
        let mut prefix = Vec::new();
        if !self.id.is_empty() {
            prefix.push(format!("[{}]", self.id));
        }
        if !self.severity.is_empty() && self.severity != "suggestion" {
            prefix.push(format!("[{}]", self.severity.to_uppercase()));
        }
        if !self.location.is_empty() {
            prefix.push(format!("at {}", self.location));
        }
        let desc = if !self.why.is_empty() {
            &self.why
        } else if !self.current.is_empty() {
            &self.current
        } else {
            &self.suggestion
        };
        let mut line = if prefix.is_empty() {
            desc.to_string()
        } else {
            format!("{} {}", prefix.join(" "), desc)
        };
        if !self.suggestion.is_empty() && !self.why.is_empty() {
            line.push_str(&format!(" (建议: {})", self.suggestion));
        }
        line
    }
}

pub(crate) fn deserialize_findings<'de, D>(deserializer: D) -> Result<Vec<ReviewFinding>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum FindingHelper {
        Structured(ReviewFinding),
        Text(String),
    }

    let helpers = Vec::<FindingHelper>::deserialize(deserializer)?;
    let mut results = Vec::new();
    for (idx, h) in helpers.into_iter().enumerate() {
        match h {
            FindingHelper::Structured(f) => results.push(f),
            FindingHelper::Text(s) => results.push(ReviewFinding {
                id: format!("F{}", idx + 1),
                severity: "suggestion".to_string(),
                finality: "regardless".to_string(),
                location: String::new(),
                current: String::new(),
                why: s,
                suggestion: String::new(),
                evidence_strength: "supported".to_string(),
            }),
        }
    }
    Ok(results)
}

/// 第二轮审查对既有 blocking 异议的裁决（异议闭环的核心载体）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObjectionVerdict {
    /// 对应 findings 中某条异议的 id（如 F1）
    pub finding_id: String,
    /// "maintain"（维持异议，仍需解决）| "withdraw"（撤回异议，视为已解决）
    pub verdict: String,
    /// 裁决理由：withdraw 必须说明为何不再成立
    pub reason: String,
}

/// 依据审查者第二轮裁决计算被撤回的 blocking 异议 id。
/// 裁决只对「确实存在且为 blocking」的异议生效；对未知 id 或非 blocking 异议的
/// withdraw 一律无效（fail-closed：无效裁决不放宽门禁）。
pub fn resolve_withdrawn_ids(
    findings: &[ReviewFinding],
    verdicts: &[ObjectionVerdict],
) -> Vec<String> {
    verdicts
        .iter()
        .filter(|v| v.verdict.eq_ignore_ascii_case("withdraw"))
        .filter(|v| {
            findings
                .iter()
                .any(|f| f.id == v.finding_id && f.severity.eq_ignore_ascii_case("blocking"))
        })
        .map(|v| v.finding_id.clone())
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChildReviewResult {
    pub child_run_id: String,
    pub status: String,
    #[serde(default, deserialize_with = "deserialize_findings")]
    pub findings: Vec<ReviewFinding>,
    pub confidence: String,
    pub evidence_refs: Vec<String>,
    pub missing_evidence: Vec<String>,
    pub duration_ms: u64,
    pub completion_tokens: i64,
    /// 输入 token 用量（成本可观测：多 Agent 的隐性大头在 prompt 侧）
    #[serde(default)]
    pub prompt_tokens: i64,
    #[serde(default)]
    pub error_kind: Option<String>,
    #[serde(default)]
    pub raw_result_json: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub routing: Option<String>,
    #[serde(default)]
    pub withdrawn_findings: Vec<String>,
    /// 第二轮审查对上轮 blocking 异议的逐条裁决
    #[serde(default)]
    pub objection_verdicts: Vec<ObjectionVerdict>,
    /// 结构化审查评分卡（4个维度，各 0~100 分）
    #[serde(default)]
    pub scorecard: Option<ReviewScorecard>,
    /// 逻辑任务身份（台账列透出；旧数据缺省回退为 child_run_id）
    #[serde(default)]
    pub task_id: Option<String>,
    /// 同逻辑任务的第几次尝试（台账列透出；旧数据缺省为 1）
    #[serde(default)]
    pub attempt_id: Option<u32>,
}

/// 方案/代码审查员的业务领域交付产物（与宿主执行耗时、网络超时等物理事实解耦）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewerDeliveryData {
    #[serde(default, deserialize_with = "deserialize_findings")]
    pub findings: Vec<ReviewFinding>,
    pub confidence: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub missing_evidence: Vec<String>,
    #[serde(default)]
    pub routing: Option<String>,
    #[serde(default)]
    pub withdrawn_findings: Vec<String>,
    #[serde(default)]
    pub objection_verdicts: Vec<ObjectionVerdict>,
    #[serde(default)]
    pub scorecard: Option<ReviewScorecard>,
    /// 动态扩展字段：承载 custom_schema 声明的角色特化产物（如 cve_refs, risk_level 等）
    #[serde(default, flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

impl ReviewerDeliveryData {
    /// 获取扩展字段原始 JSON 值
    pub fn get_extra(&self, key: &str) -> Option<&serde_json::Value> {
        self.extra.get(key)
    }

    /// 获取扩展字段字符串值
    pub fn get_extra_str(&self, key: &str) -> Option<&str> {
        self.extra.get(key).and_then(|v| v.as_str())
    }

    /// 未被审查者撤回的 blocking 异议数——异议闭环的收敛判据：
    /// 评分达标且该值为 0 才允许放行写入。
    pub fn unwithdrawn_blocking_count(&self) -> u32 {
        self.findings
            .iter()
            .filter(|f| {
                f.severity.eq_ignore_ascii_case("blocking")
                    && !self.withdrawn_findings.contains(&f.id)
            })
            .count() as u32
    }

    /// 阶段审查的拦截判据：未撤回且 finality != final_only 的 blocking 数。
    /// final_only（仅最终交付前必须解决）不拦截中间态写入，只记账带入收尾审查。
    pub fn stage_blocking_open(&self) -> u32 {
        self.findings
            .iter()
            .filter(|f| {
                f.severity.eq_ignore_ascii_case("blocking")
                    && f.finality != "final_only"
                    && !self.withdrawn_findings.contains(&f.id)
            })
            .count() as u32
    }

    /// final_only 级 blocking 的展示行（带入收尾审查的 carry-over 义务）
    pub fn stage_final_only_lines(&self) -> Vec<String> {
        self.findings
            .iter()
            .filter(|f| {
                f.severity.eq_ignore_ascii_case("blocking")
                    && f.finality == "final_only"
                    && !self.withdrawn_findings.contains(&f.id)
            })
            .map(|f| f.display_line())
            .collect()
    }

    /// 本轮异议动态三元组：(新增, 撤回, 维持)。
    /// - 撤回：被审查者裁决 withdraw 的 blocking 异议数；
    /// - 维持：裁决 maintain 的 blocking 异议数；
    /// - 新增：本轮 findings 中未被任何裁决覆盖的 blocking 异议数（首轮全部算新增）。
    pub fn objection_stats(&self) -> (u32, u32, u32) {
        let is_blocking = |f: &ReviewFinding| f.severity.eq_ignore_ascii_case("blocking");
        let withdrawn = self
            .withdrawn_findings
            .iter()
            .filter(|id| self.findings.iter().any(|f| &f.id == *id && is_blocking(f)))
            .count() as u32;
        let maintained = self
            .objection_verdicts
            .iter()
            .filter(|v| {
                v.verdict.eq_ignore_ascii_case("maintain")
                    && self
                        .findings
                        .iter()
                        .any(|f| f.id == v.finding_id && is_blocking(f))
            })
            .count() as u32;
        let covered: Vec<&String> = self
            .objection_verdicts
            .iter()
            .map(|v| &v.finding_id)
            .collect();
        let added = self
            .findings
            .iter()
            .filter(|f| is_blocking(f) && !covered.iter().any(|id| **id == f.id))
            .count() as u32;
        (added, withdrawn, maintained)
    }
}

impl ChildReviewResult {
    /// 判定本次结果是否属于基础设施或执行异常（如超时/取消/崩溃/格式损坏/请求层失败，非有效业务审查结论）
    pub fn is_infra_failure(&self) -> bool {
        self.error_kind
            .as_deref()
            .map(|raw| ChildErrorKind::parse(raw).is_infra_failure())
            .unwrap_or(false)
    }

    /// 提取审查员的纯业务领域交付数据
    pub fn to_reviewer_delivery(&self) -> ReviewerDeliveryData {
        ReviewerDeliveryData {
            findings: self.findings.clone(),
            confidence: self.confidence.clone(),
            evidence_refs: self.evidence_refs.clone(),
            missing_evidence: self.missing_evidence.clone(),
            routing: self.routing.clone(),
            withdrawn_findings: self.withdrawn_findings.clone(),
            objection_verdicts: self.objection_verdicts.clone(),
            scorecard: self.scorecard.clone(),
            extra: Default::default(),
        }
    }

    /// 未被审查者撤回的 blocking 异议数（委托至领域交付数据）
    pub fn unwithdrawn_blocking_count(&self) -> u32 {
        self.to_reviewer_delivery().unwithdrawn_blocking_count()
    }

    /// 阶段审查的拦截判据（委托至领域交付数据）
    pub fn stage_blocking_open(&self) -> u32 {
        self.to_reviewer_delivery().stage_blocking_open()
    }

    /// final_only 级 blocking 的展示行（委托至领域交付数据）
    pub fn stage_final_only_lines(&self) -> Vec<String> {
        self.to_reviewer_delivery().stage_final_only_lines()
    }

    /// 本轮异议动态三元组（委托至领域交付数据）
    pub fn objection_stats(&self) -> (u32, u32, u32) {
        self.to_reviewer_delivery().objection_stats()
    }

    /// 尝试将交付内容反序列化为写入工兵结果 (WriterResult)
    pub fn as_writer_result(&self) -> Option<crate::agent::child_writer::WriterResult> {
        self.raw_result_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
    }

    /// 尝试将交付内容反序列化为只读侦察任务结果 (WorkerResult)
    pub fn as_worker_result(&self) -> Option<crate::agent::child_worker::WorkerResult> {
        self.raw_result_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawChildReviewRequest {
    #[serde(default)]
    pub(crate) question: String,
    #[serde(default)]
    pub(crate) evidence_excerpt: String,
    #[serde(default)]
    pub(crate) evidence_refs: Vec<String>,
    #[serde(default)]
    pub(crate) resource_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum FlexibleRawFinding {
    Structured(RawReviewFinding),
    Text(String),
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawReviewFinding {
    #[serde(default)]
    pub(crate) finality: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default, alias = "file")]
    pub location: Option<String>,
    #[serde(default)]
    pub current: Option<String>,
    #[serde(default, alias = "description")]
    pub why: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default, alias = "recommendation")]
    pub suggestion: Option<String>,
    #[serde(default)]
    pub evidence_strength: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawChildReviewOutput {
    pub(crate) findings: Vec<FlexibleRawFinding>,
    pub(crate) confidence: String,
    #[serde(default)]
    pub(crate) evidence_refs: Vec<String>,
    #[serde(default)]
    pub(crate) missing_evidence: Vec<String>,
    #[serde(default)]
    pub(crate) routing: Option<String>,
    #[serde(default)]
    pub(crate) scorecard: Option<RawReviewScorecard>,
    #[serde(default)]
    pub(crate) objection_verdicts: Vec<RawObjectionVerdict>,
}

/// 宽容解析 objection_verdicts：字段缺失/非法时降级为 maintain（保守方向，不放宽门禁）
#[derive(Debug, Deserialize)]
pub(crate) struct RawObjectionVerdict {
    #[serde(default)]
    pub(crate) finding_id: Option<String>,
    #[serde(default)]
    pub(crate) verdict: Option<String>,
    #[serde(default)]
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawReviewScorecard {
    #[serde(default)]
    pub(crate) correctness: Option<u32>,
    #[serde(default)]
    pub(crate) risk: Option<u32>,
    #[serde(default)]
    pub(crate) style: Option<u32>,
    #[serde(default)]
    pub(crate) test_coverage: Option<u32>,
    #[serde(default)]
    pub(crate) null_reason: Option<String>,
}
