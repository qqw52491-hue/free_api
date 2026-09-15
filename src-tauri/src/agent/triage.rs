//! 任务入口 LLM 分流（Triage）：新 turn 起步时由 Pro 模型做一次结构化复杂度判决，
//! 判决落 checkpoint 后各门禁按 route 分流——轻任务不再被「检测到写工具」焊死进方案门。
//!
//! 纪律：
//! - 判决是建议层不是物理层：机械安全地板（path_guard 白名单、删改名动作、
//!   多文件扩散升级、显式人工审批模式）不被判决推翻；
//! - 失败一律保守降级（fallback = technical_plan，等价维持旧行为），绝不 panic；
//! - tmp/ 下的复杂产物（PPT/HTML/报告）质量复杂度照样可以是 high——路径不代表复杂度。

use crate::agent::router::ModelRoutingConfig;
use crate::db::DbState;
use crate::llm::{self, ModelConfig};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;

const TRIAGE_TIMEOUT_SECS: u64 = 45;

/// 任务分流的结构化判决。所有字段容错反序列化：缺失/非法值由 normalize() 收敛到安全默认。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriageVerdict {
    /// 产物质量复杂度：low|medium|high
    #[serde(default)]
    pub quality_complexity: String,
    /// 对已有资产的破坏风险：none|reversible|destructive
    #[serde(default)]
    pub asset_risk: String,
    /// 协同范围：isolated|multi_component|systemic
    #[serde(default)]
    pub coordination_scope: String,
    /// 需求歧义度：low|high
    #[serde(default)]
    pub requirement_ambiguity: String,
    /// 分流路由：direct|outline_review|technical_plan|expert_panel|user_clarification
    #[serde(default)]
    pub route: String,
    /// 前置审查建议挂载的角色名（来自可调用的 reviewer 清单）
    #[serde(default)]
    pub pre_review_roles: Vec<String>,
    /// 阶段审查模式：none|milestone（空串按 milestone）
    #[serde(default)]
    pub stage_review_mode: String,
    #[serde(default)]
    pub stage_review_roles: Vec<String>,
    /// 终审模式：mechanical_only|artifact_quality|code_review|operational_verification|panel
    #[serde(default)]
    pub final_review_mode: String,
    #[serde(default)]
    pub final_review_roles: Vec<String>,
    /// 方案证据要求：none|source_refs|code_refs|operational_probes
    #[serde(default)]
    pub evidence_policy: String,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub reason: String,
    /// 判决自报的升级触发条件（观测用，门禁侧另有机械地板兜底）
    #[serde(default)]
    pub escalation_triggers: Vec<String>,
    /// 实际给出判决的模型 id（宿主回填，非模型输出）
    #[serde(default)]
    pub model_id: String,
    /// 非空 = 本判决是失败降级产物而非真实模型判决（值为降级原因）
    #[serde(default)]
    pub fallback: Option<String>,
}

impl TriageVerdict {
    /// 非法枚举值收敛到安全默认：未知 route→technical_plan、未知 final_mode→code_review、
    /// 未知 evidence→code_refs、stage_review_mode 空/未知→milestone。
    pub fn normalize(&mut self) {
        let norm = |s: &str| s.trim().to_ascii_lowercase();
        self.quality_complexity = match norm(&self.quality_complexity).as_str() {
            "low" | "medium" | "high" => norm(&self.quality_complexity),
            _ => "medium".to_string(),
        };
        self.asset_risk = match norm(&self.asset_risk).as_str() {
            "none" | "reversible" | "destructive" => norm(&self.asset_risk),
            _ => "reversible".to_string(),
        };
        self.coordination_scope = match norm(&self.coordination_scope).as_str() {
            "isolated" | "multi_component" | "systemic" => norm(&self.coordination_scope),
            _ => "multi_component".to_string(),
        };
        self.requirement_ambiguity = match norm(&self.requirement_ambiguity).as_str() {
            "low" | "high" => norm(&self.requirement_ambiguity),
            _ => "low".to_string(),
        };
        self.route = match norm(&self.route).as_str() {
            "direct" | "outline_review" | "technical_plan" | "expert_panel"
            | "user_clarification" => norm(&self.route),
            _ => "technical_plan".to_string(),
        };
        self.stage_review_mode = match norm(&self.stage_review_mode).as_str() {
            "none" => "none".to_string(),
            // 空串与未知值一律按 milestone（保守：保留中段审查）
            _ => "milestone".to_string(),
        };
        self.final_review_mode = match norm(&self.final_review_mode).as_str() {
            "mechanical_only"
            | "artifact_quality"
            | "code_review"
            | "operational_verification"
            | "panel" => norm(&self.final_review_mode),
            _ => "code_review".to_string(),
        };
        self.evidence_policy = match norm(&self.evidence_policy).as_str() {
            "none" | "source_refs" | "code_refs" | "operational_probes" => {
                norm(&self.evidence_policy)
            }
            _ => "code_refs".to_string(),
        };
        self.confidence = self.confidence.clamp(0.0, 1.0);
        self.pre_review_roles.retain(|r| !r.trim().is_empty());
        self.stage_review_roles.retain(|r| !r.trim().is_empty());
        self.final_review_roles.retain(|r| !r.trim().is_empty());
    }

    /// 是否需要 submit_plan 技术方案门（technical_plan / expert_panel 才需要）
    pub fn requires_plan_gate(&self) -> bool {
        matches!(
            self.route.trim().to_ascii_lowercase().as_str(),
            "technical_plan" | "expert_panel"
        )
    }

    /// 阶段审查开关：stage_review_mode != "none"（空串按 milestone → true）
    pub fn needs_stage_review(&self) -> bool {
        !self.stage_review_mode.trim().eq_ignore_ascii_case("none")
    }

    /// 方案提交是否强制 evidence_refs（code_refs / operational_probes 需要）
    pub fn evidence_required(&self) -> bool {
        matches!(
            self.evidence_policy.trim().to_ascii_lowercase().as_str(),
            "code_refs" | "operational_probes"
        )
    }

    /// 需求歧义必须先问用户
    pub fn needs_user_clarification(&self) -> bool {
        self.route.trim().eq_ignore_ascii_case("user_clarification")
    }

    /// 失败降级判决：等价于「维持现状行为」——方案门照开、阶段审查保留、
    /// 证据要求不放宽。fallback 记录降级原因供审计。
    pub fn fallback(reason: &str) -> Self {
        Self {
            quality_complexity: "medium".to_string(),
            asset_risk: "reversible".to_string(),
            coordination_scope: "multi_component".to_string(),
            requirement_ambiguity: "low".to_string(),
            route: "technical_plan".to_string(),
            pre_review_roles: Vec::new(),
            stage_review_mode: "milestone".to_string(),
            stage_review_roles: Vec::new(),
            final_review_mode: "code_review".to_string(),
            final_review_roles: Vec::new(),
            evidence_policy: "code_refs".to_string(),
            confidence: 0.0,
            reason: "分流调用失败，已按保守默认降级".to_string(),
            escalation_triggers: Vec::new(),
            model_id: String::new(),
            fallback: Some(reason.to_string()),
        }
    }
}

/// 把任务入口分流判决合并进子任务派发的 custom_inputs（纯函数，可单测）：
/// verified_facts / hard_constraints 是渲染层保留键，会被升格为独立一级小节；
/// 模型自填的同名键不覆盖、改为数组合并（自填在前、判决生成在后）。
pub fn merge_brief_into_inputs(
    inputs: Option<&serde_json::Value>,
    verdict: &TriageVerdict,
) -> serde_json::Value {
    let mut obj = inputs
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let append_items = |obj: &mut serde_json::Map<String, serde_json::Value>,
                        key: &str,
                        extra: Vec<String>| {
        if extra.is_empty() {
            return;
        }
        let mut items: Vec<serde_json::Value> = match obj.remove(key) {
            Some(serde_json::Value::Array(arr)) => arr,
            Some(serde_json::Value::Null) | None => Vec::new(),
            Some(other) => vec![other],
        };
        items.extend(extra.into_iter().map(serde_json::Value::String));
        obj.insert(key.to_string(), serde_json::Value::Array(items));
    };

    let mut facts = Vec::new();
    if !verdict.route.trim().is_empty() {
        let reason = verdict.reason.trim();
        facts.push(if reason.is_empty() {
            format!("任务入口分流判决 route={}", verdict.route)
        } else {
            format!("任务入口分流判决 route={}（{}）", verdict.route, reason)
        });
    }
    if !verdict.evidence_policy.trim().is_empty() {
        facts.push(format!("证据要求 evidence_policy={}", verdict.evidence_policy));
    }
    append_items(&mut obj, "verified_facts", facts);

    let triggers: Vec<String> = verdict
        .escalation_triggers
        .iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    append_items(&mut obj, "hard_constraints", triggers);

    serde_json::Value::Object(obj)
}

/// 机械安全地板：判决不可推翻的硬升级信号。返回 Some(触发原因) 表示命中地板，
/// 调用方据此强制回到方案门。与判决的 escalation_triggers 互为冗余（模型自报不可全信）。
pub fn write_floor_violation(
    inst: &crate::agent::types::AgentInstruction,
    context: &crate::agent::context::SandwichContext,
    session_id: &str,
) -> Option<&'static str> {
    // 1. 删/改名/移动/移除类动作：物理破坏性动作永远走方案门。
    // local_file 当前词表仅 read/write/edit/list，此防御面向全工具表与未来扩展。
    let tool = inst.get_tool().to_ascii_lowercase();
    let mut action = inst.get_action().to_ascii_lowercase();
    if action.is_empty() || action == tool {
        action = inst
            .get_params()
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
    }
    const DESTRUCTIVE_VERBS: &[&str] = &[
        "delete", "remove", "rename", "move", "unlink", "rmdir", "truncate", "drop",
    ];
    if DESTRUCTIVE_VERBS
        .iter()
        .any(|v| action.starts_with(v) || tool.contains(v))
    {
        return Some("destructive_action");
    }

    // 2. 多文件扩散升级：本轮新增受影响路径中去重的非 tmp 工作区文件 ≥ 4 个。
    // tmp/ 产物不计入——写一摞会话产物不等于改动工程资产。
    let workspace_spread: std::collections::HashSet<String> = context
        .new_affected_paths()
        .into_iter()
        .filter(|p| !crate::agent::path_guard::is_path_in_session_tmp(p, Some(session_id)))
        .map(|p| p.trim_start_matches("./").to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if workspace_spread.len() >= 4 {
        return Some("multi_file_spread");
    }
    None
}

/// 任务入口分流判决：由 Pro 档模型产出结构化 TriageVerdict。
/// 任何失败（模型缺失/调用失败/超时/JSON 解析失败）都返回保守 fallback，绝不中断主循环。
pub async fn run_triage(
    app: &AppHandle,
    db: &DbState,
    model_routing: &ModelRoutingConfig,
    goal: &str,
    scope_manifest: &str,
    reviewer_names: &[String],
    session_id: &str,
    mut interrupt_rx: watch::Receiver<u64>,
) -> TriageVerdict {
    // 分流是全局复杂性判断，固定用 Pro 档；空则回退 flash→vision（与会话主模型同序）
    let model_id = if !model_routing.pro.trim().is_empty() {
        model_routing.pro.trim().to_string()
    } else if !model_routing.flash.trim().is_empty() {
        model_routing.flash.trim().to_string()
    } else {
        model_routing.vision.trim().to_string()
    };
    if model_id.is_empty() {
        return TriageVerdict::fallback("未配置可用模型");
    }

    let _ = app.emit(
        &format!("agent-progress-{}", session_id),
        json!({
            "type": "action_phase",
            "phase": "triaging",
            "description": "🧭 正在对任务做入口复杂度分流..."
        }),
    );

    let system_prompt = r#"你是任务分流裁判（Triage）。任务开始前，你要对它做一次结构化复杂度分流判决。
你必须且仅输出一个合法的 JSON 对象，禁止输出任何 Markdown 标记、解释或多余文本：
{
  "quality_complexity": "low|medium|high",
  "asset_risk": "none|reversible|destructive",
  "coordination_scope": "isolated|multi_component|systemic",
  "requirement_ambiguity": "low|high",
  "route": "direct|outline_review|technical_plan|expert_panel|user_clarification",
  "pre_review_roles": ["建议前置审查挂载的角色名，只能从给定清单中选，可为空数组"],
  "stage_review_mode": "none|milestone",
  "stage_review_roles": ["建议阶段审查角色，可为空数组"],
  "final_review_mode": "mechanical_only|artifact_quality|code_review|operational_verification|panel",
  "final_review_roles": ["建议终审角色，可为空数组"],
  "evidence_policy": "none|source_refs|code_refs|operational_probes",
  "confidence": 0.85,
  "reason": "一句话判决理由",
  "escalation_triggers": ["会把本任务升级回重路径的触发条件"]
}

【枚举语义】
- quality_complexity：产物本身的质量复杂度。注意：写入 tmp/ 会话目录的复杂产物（PPT/HTML/长报告）质量复杂度照样可以是 high——路径位置不代表复杂度。
- asset_risk：对已有资产的破坏风险。none=纯新增/只读；reversible=可逆修改；destructive=删除/改名/移动/不可逆变更。
- coordination_scope：isolated=单点孤立改动；multi_component=跨多文件/组件；systemic=影响系统级架构。
- requirement_ambiguity：low=需求明确可直接执行；high=关键需求缺失或歧义，瞎猜会做错方向。
- route（分流路由，决定前置讨论强度）：
  - direct：无需任何前置讨论直接执行（简单明确的小活、单文件产物）；
  - outline_review：需要先产出大纲/结构并经审查再动手（长文、多章节复杂产物）；
  - technical_plan：需要 submit_plan 技术方案审查（修改已有代码、多文件改动、有风险操作）；
  - expert_panel：跨域高风险，需多专家联合会审；
  - user_clarification：需求歧义必须先问用户，澄清前不得写入。
- stage_review_mode：none=中段无需阶段审查；milestone=按里程碑/写入阈值做阶段审查。
- final_review_mode：mechanical_only=机械校验即可交付（简单产物）；artifact_quality=非代码产物按产物质量标准终审（需求覆盖/结构完整/可用性）；code_review=代码级终审；operational_verification=须运行验证证据的终审；panel=多专家终审。
- evidence_policy：none=方案不需证据引用；source_refs=资料/需求来源即可；code_refs=须引用已读代码证据；operational_probes=须命令输出/探针证据。

【判定纪律】
- 宁可低估不要拔高：简单明确的任务判 direct，让门禁松手；
- 但触及已有代码修改、多文件扩散、删除改名、生产配置一律不低于 technical_plan；
- 拿不准且做错代价高 → user_clarification；拿不准但方向可逆 → technical_plan。"#;

    let reviewer_list = if reviewer_names.is_empty() {
        "（当前无可用审查角色）".to_string()
    } else {
        reviewer_names.join(", ")
    };
    let user_prompt = format!(
        "【任务目标】\n{}\n\n【可操作文件边界 / 作用域清单】\n{}\n\n【可派发的审查角色清单】\n{}\n\n请输出分流判决 JSON。",
        goal,
        if scope_manifest.trim().is_empty() { "（无作用域信息）" } else { scope_manifest },
        reviewer_list
    );
    let messages = vec![
        json!({ "role": "system", "content": system_prompt }),
        json!({ "role": "user", "content": user_prompt }),
    ];

    let model_config = match ModelConfig::from_db_state(db, &model_id) {
        Ok(cfg) => cfg,
        Err(e) => {
            let mut v = TriageVerdict::fallback(&format!("加载分流模型配置失败: {}", e));
            v.model_id = model_id;
            return v;
        }
    };

    let mut ignore_stream = |_: llm::StreamEvent<'_>| true;
    let call = llm::stream_with_retry(
        &model_config,
        &messages,
        "auto",
        &mut interrupt_rx,
        1,
        &mut ignore_stream,
    );

    let stream_result =
        match tokio::time::timeout(Duration::from_secs(TRIAGE_TIMEOUT_SECS), call).await {
            Ok(res) => res,
            Err(_) => {
                crate::agent::utils::llm_client::log_auxiliary_llm_exchange(
                    session_id,
                    "triage",
                    1,
                    &model_config,
                    &messages,
                    None,
                    "request_error",
                    Some("timeout"),
                );
                let mut v = TriageVerdict::fallback("分流模型调用超时(>45s)");
                v.model_id = model_id;
                return v;
            }
        };

    match stream_result {
        Ok(output) => {
            crate::agent::utils::llm_client::log_auxiliary_llm_exchange(
                session_id,
                "triage",
                1,
                &model_config,
                &messages,
                Some(&output),
                "success",
                None,
            );
            match parse_triage_json(&output.full_response) {
                Some(mut v) => {
                    v.normalize();
                    v.model_id = model_id;
                    v
                }
                None => {
                    let mut v = TriageVerdict::fallback("分流判决 JSON 解析失败");
                    v.model_id = model_id;
                    v
                }
            }
        }
        Err(e) => {
            crate::agent::utils::llm_client::log_auxiliary_llm_exchange(
                session_id,
                "triage",
                1,
                &model_config,
                &messages,
                None,
                "request_error",
                Some(&e),
            );
            let mut v = TriageVerdict::fallback(&format!("分流模型调用失败: {}", e));
            v.model_id = model_id;
            v
        }
    }
}

/// 容错解析分流判决 JSON（与 reviewer 输出同款的 ```json 围栏/首尾花括号提取）
fn parse_triage_json(raw_text: &str) -> Option<TriageVerdict> {
    let clean = extract_json_str(raw_text);
    serde_json::from_str::<TriageVerdict>(&clean).ok()
}

fn extract_json_str(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(start) = trimmed.find("```json") {
        if let Some(end) = trimmed[start + 7..].find("```") {
            return trimmed[start + 7..start + 7 + end].trim().to_string();
        }
    }
    if let Some(start) = trimmed.find("```") {
        if let Some(end) = trimmed[start + 3..].find("```") {
            return trimmed[start + 3..start + 3 + end].trim().to_string();
        }
    }
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if start <= end {
            return trimmed[start..=end].to_string();
        }
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::context::SandwichContext;
    use crate::agent::types::AgentInstruction;
    use serde_json::json;

    fn parse(raw: &str) -> TriageVerdict {
        let mut v = parse_triage_json(raw).expect("合法 JSON 必须解析成功");
        v.normalize();
        v
    }

    #[test]
    fn valid_json_parses_all_fields() {
        let v = parse(
            r#"{
            "quality_complexity": "high",
            "asset_risk": "reversible",
            "coordination_scope": "multi_component",
            "requirement_ambiguity": "low",
            "route": "technical_plan",
            "pre_review_roles": ["tech_lead"],
            "stage_review_mode": "milestone",
            "final_review_mode": "code_review",
            "final_review_roles": ["reviewer"],
            "evidence_policy": "code_refs",
            "confidence": 0.8,
            "reason": "改已有代码",
            "escalation_triggers": ["扩散到 4 个文件"]
        }"#,
        );
        assert_eq!(v.quality_complexity, "high");
        assert_eq!(v.route, "technical_plan");
        assert_eq!(v.pre_review_roles, vec!["tech_lead".to_string()]);
        assert_eq!(v.final_review_mode, "code_review");
        assert_eq!(v.evidence_policy, "code_refs");
        assert!((v.confidence - 0.8).abs() < 1e-9);
        assert!(v.fallback.is_none());
    }

    #[test]
    fn unknown_enum_values_normalize_to_safe_defaults() {
        let v = parse(
            r#"{
            "quality_complexity": "extreme",
            "asset_risk": "spicy",
            "coordination_scope": "galaxy",
            "requirement_ambiguity": "maybe",
            "route": "yolo",
            "stage_review_mode": "whatever",
            "final_review_mode": "vibes",
            "evidence_policy": "trust_me",
            "confidence": 42.0
        }"#,
        );
        assert_eq!(v.quality_complexity, "medium");
        assert_eq!(v.asset_risk, "reversible");
        assert_eq!(v.coordination_scope, "multi_component");
        assert_eq!(v.requirement_ambiguity, "low");
        assert_eq!(v.route, "technical_plan", "未知 route 必须收敛到最保守分流");
        assert_eq!(v.stage_review_mode, "milestone");
        assert_eq!(v.final_review_mode, "code_review");
        assert_eq!(v.evidence_policy, "code_refs");
        assert!((v.confidence - 1.0).abs() < 1e-9, "confidence 钳位到 [0,1]");
    }

    #[test]
    fn empty_stage_review_mode_counts_as_milestone() {
        let v = parse(r#"{"route": "direct", "stage_review_mode": ""}"#);
        assert_eq!(v.stage_review_mode, "milestone");
        assert!(v.needs_stage_review());
    }

    #[test]
    fn fallback_verdict_is_conservative() {
        let v = TriageVerdict::fallback("测试降级");
        assert!(v.requires_plan_gate(), "fallback 必须维持方案门（旧行为）");
        assert!(v.needs_stage_review());
        assert!(v.evidence_required());
        assert_eq!(v.route, "technical_plan");
        assert_eq!(v.confidence, 0.0);
        assert_eq!(v.fallback.as_deref(), Some("测试降级"));
    }

    #[test]
    fn route_table_drives_plan_gate() {
        for (route, expected) in [
            ("direct", false),
            ("outline_review", false),
            ("user_clarification", false),
            ("technical_plan", true),
            ("expert_panel", true),
        ] {
            let v = parse(&format!(r#"{{"route": "{}"}}"#, route));
            assert_eq!(v.requires_plan_gate(), expected, "route={}", route);
        }
    }

    #[test]
    fn evidence_policy_table() {
        for (policy, expected) in [
            ("none", false),
            ("source_refs", false),
            ("code_refs", true),
            ("operational_probes", true),
        ] {
            let v = parse(&format!(r#"{{"evidence_policy": "{}"}}"#, policy));
            assert_eq!(v.evidence_required(), expected, "policy={}", policy);
        }
    }

    #[test]
    fn malformed_output_fails_parse_instead_of_panicking() {
        assert!(parse_triage_json("这根本不是 JSON").is_none());
        assert!(parse_triage_json("").is_none());
        // 围栏包装的可提取
        let fenced = parse_triage_json("前缀噪音\n```json\n{\"route\":\"direct\"}\n```\n后缀");
        assert!(fenced.is_some());
    }

    #[test]
    fn write_floor_violation_catches_destructive_actions() {
        let context = SandwichContext::new("s".to_string(), "g".to_string());
        // local_file 当前词表无 delete；用通用工具动作验证防御面
        let inst: AgentInstruction = serde_json::from_value(json!({
            "tool": "local_file",
            "action": "delete",
            "command": { "path": "src/main.rs" }
        }))
        .unwrap();
        assert_eq!(
            write_floor_violation(&inst, &context, "sess"),
            Some("destructive_action")
        );

        let safe: AgentInstruction = serde_json::from_value(json!({
            "tool": "local_file",
            "action": "write",
            "command": { "path": "tmp/draft.md", "content": "x" }
        }))
        .unwrap();
        assert_eq!(write_floor_violation(&safe, &context, "sess"), None);
    }

    #[test]
    fn write_floor_violation_catches_multi_file_spread_but_ignores_tmp() {
        let mut context = SandwichContext::new("s".to_string(), "g".to_string());
        let inst: AgentInstruction = serde_json::from_value(json!({
            "tool": "local_file",
            "action": "write",
            "command": { "path": "e.rs", "content": "x" }
        }))
        .unwrap();
        for (i, p) in ["a.rs", "b.rs", "c.rs"].iter().enumerate() {
            context.record_side_effect_with_paths(
                i,
                "write_file".to_string(),
                "w".to_string(),
                vec![p.to_string()],
            );
        }
        assert_eq!(
            write_floor_violation(&inst, &context, "sess"),
            None,
            "3 个工作区文件未到阈值"
        );
        context.record_side_effect_with_paths(
            3,
            "write_file".to_string(),
            "w".to_string(),
            vec!["d.rs".to_string()],
        );
        assert_eq!(
            write_floor_violation(&inst, &context, "sess"),
            Some("multi_file_spread"),
            "第 4 个互异工作区文件触发扩散升级"
        );
        // tmp/ 路径不计入扩散
        let mut tmp_only = SandwichContext::new("s".to_string(), "g".to_string());
        for i in 0..6 {
            tmp_only.record_side_effect_with_paths(
                i,
                "write_file".to_string(),
                "w".to_string(),
                vec![format!("tmp/part-{}.md", i)],
            );
        }
        assert_eq!(write_floor_violation(&inst, &tmp_only, "sess"), None);
    }

    #[test]
    fn merge_brief_into_inputs_injects_verdict_brief() {
        let verdict = parse(
            r#"{
            "route": "technical_plan",
            "reason": "改已有代码",
            "evidence_policy": "code_refs",
            "escalation_triggers": ["扩散到 4 个文件", "涉及鉴权改动"]
        }"#,
        );

        // 无 inputs：判决生成 verified_facts 与 hard_constraints
        let merged = merge_brief_into_inputs(None, &verdict);
        let facts = merged["verified_facts"].as_array().unwrap();
        assert!(facts.iter().any(|f| f.as_str().unwrap().contains("route=technical_plan")));
        assert!(facts.iter().any(|f| f.as_str().unwrap().contains("evidence_policy=code_refs")));
        let constraints = merged["hard_constraints"].as_array().unwrap();
        assert_eq!(constraints.len(), 2);
        assert!(constraints.iter().any(|c| c.as_str().unwrap().contains("鉴权改动")));

        // 非 object inputs：按空对象处理，普通键与判决键共存
        let merged = merge_brief_into_inputs(Some(&json!("junk")), &verdict);
        assert!(merged["verified_facts"].is_array());

        // 模型自填同键不覆盖：自填在前、判决在后
        let inputs = json!({
            "plan_summary": "方案A",
            "verified_facts": ["模型自填事实"],
            "hard_constraints": "禁止删除文件"
        });
        let merged = merge_brief_into_inputs(Some(&inputs), &verdict);
        assert_eq!(merged["plan_summary"], "方案A");
        let facts = merged["verified_facts"].as_array().unwrap();
        assert_eq!(facts[0], "模型自填事实", "模型自填的必须在前");
        assert!(facts.len() > 1, "判决事实应追加在后");
        let constraints = merged["hard_constraints"].as_array().unwrap();
        assert_eq!(constraints[0], "禁止删除文件");
        assert_eq!(constraints.len(), 3);
    }

    #[test]
    fn merge_brief_into_inputs_skips_empty_verdict_fields() {
        // 判决无触发条件 → 不凭空造 hard_constraints；普通键原样保留
        let mut verdict = TriageVerdict::fallback("测试降级");
        verdict.escalation_triggers = Vec::new();
        let inputs = json!({"plan_summary": "方案A", "hard_constraints": ["自填边界"]});
        let merged = merge_brief_into_inputs(Some(&inputs), &verdict);
        assert_eq!(merged["plan_summary"], "方案A");
        let constraints = merged["hard_constraints"].as_array().unwrap();
        assert_eq!(constraints.len(), 1, "空触发条件不得追加内容");
        assert_eq!(constraints[0], "自填边界");

        // 模型未填 hard_constraints 且判决无触发 → 键不注入
        let merged = merge_brief_into_inputs(Some(&json!({"a": 1})), &verdict);
        assert!(merged.get("hard_constraints").is_none());
        assert_eq!(merged["a"], 1);
    }
}
