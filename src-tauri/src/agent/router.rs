use crate::agent::context::SandwichContext;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 前端传入的模型路由配置表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRoutingConfig {
    pub pro: String,
    pub flash: String,
    pub vision: String,
    /// 旧版统一思考策略；保留用于反序列化及对缺失独立字段的兼容回退。
    #[serde(default = "default_reasoning_effort")]
    pub reasoning_effort: String,
    /// 三个模型角色的独立策略；空值时兼容回退到 reasoning_effort。
    #[serde(default)]
    pub pro_reasoning_effort: String,
    #[serde(default)]
    pub flash_reasoning_effort: String,
    #[serde(default)]
    pub vision_reasoning_effort: String,
    /// 仅供受控后台评测启用：第 0 步（及其内部重试）固定使用 Pro 建立推理基线。
    /// 普通会话不传该字段时默认 `false`，保持原有自动路由。
    #[serde(default)]
    pub force_pro_on_first_step: bool,
    /// 上下文预算与压缩策略模式：deep_coding (长程默认) | cost_saving (省流) | ultra_fast (极速)
    #[serde(default = "default_context_budget_mode")]
    pub context_budget_mode: String,
    /// 路由模式：rules（默认，现行规则分流）| scored（难度打分制）
    #[serde(default)]
    pub routing_mode: String,
    /// 角色级模型覆盖：角色名 → "pro"/"flash"/"vision"（档位别名）或具体模型 UUID。
    /// 空串或缺省 = 跟随自动路由。用户只需维护 pro/flash/vision 三个槽，
    /// 角色挂档位由系统默认表处理，钉死某角色才需要填这里。
    #[serde(default)]
    pub role_models: BTreeMap<String, String>,
}

impl Default for ModelRoutingConfig {
    fn default() -> Self {
        Self {
            pro: String::new(),
            flash: String::new(),
            vision: String::new(),
            reasoning_effort: "auto".to_string(),
            pro_reasoning_effort: String::new(),
            flash_reasoning_effort: String::new(),
            vision_reasoning_effort: String::new(),
            force_pro_on_first_step: false,
            context_budget_mode: "deep_coding".to_string(),
            routing_mode: String::new(),
            role_models: BTreeMap::new(),
        }
    }
}

/// 系统固定角色名（角色集合稳定，任务千变万化都流过这些角色）。
pub mod roles {
    pub const PLANNER: &str = "planner";
    pub const EXECUTOR: &str = "executor";
    pub const REVIEWER: &str = "reviewer";
    pub const SCOUT: &str = "scout";
    pub const WRITER: &str = "writer";
}
fn default_reasoning_effort() -> String {
    "auto".to_string()
}
fn default_context_budget_mode() -> String {
    "deep_coding".to_string()
}
/// 从 model_id 启发式判断模型家族。跨家族异构审查的前提是能识别家族；
/// 私有部署名无法识别时返回 "other"（按同家族保守处理，不做切换）。
pub fn model_family(model_id: &str) -> &'static str {
    let id = model_id.to_ascii_lowercase();
    if id.contains("claude") || id.contains("anthropic") {
        "anthropic"
    } else if id.contains("gpt") || id.contains("o1") || id.contains("o3") || id.contains("o4")
        || id.contains("openai")
    {
        "openai"
    } else if id.contains("gemini") || id.contains("gemma") {
        "google"
    } else if id.contains("deepseek") {
        "deepseek"
    } else if id.contains("qwen") || id.contains("qwq") {
        "qwen"
    } else if id.contains("glm") || id.contains("zhipu") || id.contains("chatglm") {
        "glm"
    } else if id.contains("mistral") || id.contains("mixtral") {
        "mistral"
    } else if id.contains("llama") {
        "llama"
    } else if id.contains("kimi") || id.contains("moonshot") {
        "moonshot"
    } else if id.contains("doubao") {
        "doubao"
    } else {
        "other"
    }
}

/// reviewer 跨家族优选（配置级异构的默认行为，显式 role_models/角色卡钉死不受影响）：
/// 当前 reviewer 模型与规划者同家族时，从启用模型中挑一个异构家族的替代；
/// 无异构可选（单家族部署）时保持原模型并返回告警，由调用方显式记录。
/// 返回 (最终模型, 说明)。
pub fn prefer_cross_family_reviewer(
    conn: &rusqlite::Connection,
    current_model: &str,
) -> (String, Option<String>) {
    let current_family = model_family(current_model);
    if current_family == "other" {
        return (current_model.to_string(), None);
    }
    let models: Vec<String> = {
        let Ok(mut stmt) = conn.prepare("SELECT id FROM models WHERE enabled = 1") else {
            return (current_model.to_string(), None);
        };
        stmt.query_map([], |row| row.get::<_, String>(0))
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    };
    let alternative = models.iter().find(|m| {
        let family = model_family(m);
        family != current_family && family != "other"
    });
    match alternative {
        Some(m) => (
            m.clone(),
            Some(format!(
                " reviewer 异构优选：{}（{} 系）→ {}（{} 系），与规划者模型家族错开以互补盲区。",
                current_model, current_family, m, model_family(m)
            )),
        ),
        None => (
            current_model.to_string(),
            Some(format!(
                " ⚠️ reviewer 与规划者同为 {} 系模型且无异构家族可选（单家族部署），跨家族审查收益将打折；建议接入其他家族模型或在角色卡钉死异构模型。",
                current_family
            )),
        ),
    }
}

impl ModelRoutingConfig {
    pub fn reasoning_effort_for(&self, tier: &RoutingTier) -> &str {
        let configured = match tier {
            RoutingTier::Pro => &self.pro_reasoning_effort,
            RoutingTier::Flash => &self.flash_reasoning_effort,
            RoutingTier::Vision => &self.vision_reasoning_effort,
        };
        if configured.is_empty() {
            &self.reasoning_effort
        } else {
            configured.as_str()
        }
    }
}

/// 路由判定结果（对外暴露，方便主循环获取判定理由并打日志）
#[derive(Debug, Clone)]
pub struct RoutingDecision {
    pub model_id: String,
    pub tier: RoutingTier,
    pub reason: String,
}

/// 模型层级标签
#[derive(Debug, Clone, PartialEq)]
pub enum RoutingTier {
    Pro,
    Flash,
    Vision,
}

impl std::fmt::Display for RoutingTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoutingTier::Pro => write!(f, "🧠 Pro"),
            RoutingTier::Flash => write!(f, "⚡ Flash"),
            RoutingTier::Vision => write!(f, "👁️ Vision"),
        }
    }
}

impl ModelRoutingConfig {
    /// 角色级覆盖解析：命中返回具体模型决策；未配置（空/缺省）返回 None 走自动路由。
    pub fn role_override(&self, role: &str) -> Option<RoutingDecision> {
        let raw = self.role_models.get(role)?.trim().to_string();
        if raw.is_empty() {
            return None;
        }
        let decision = match raw.as_str() {
            "pro" => RoutingDecision {
                model_id: self.pro.clone(),
                tier: RoutingTier::Pro,
                reason: format!("角色 {} 手动钉在 Pro 档", role),
            },
            "flash" => RoutingDecision {
                model_id: self.flash.clone(),
                tier: RoutingTier::Flash,
                reason: format!("角色 {} 手动钉在 Flash 档", role),
            },
            "vision" => RoutingDecision {
                model_id: self.vision.clone(),
                tier: RoutingTier::Vision,
                reason: format!("角色 {} 手动钉在 Vision 档", role),
            },
            other => RoutingDecision {
                model_id: other.to_string(),
                tier: RoutingTier::Flash,
                reason: format!("角色 {} 手动指定具体模型", role),
            },
        };
        (!decision.model_id.is_empty()).then_some(decision)
    }
}

pub struct AgentRouter;

impl AgentRouter {
    /// 在需要可复现的评测中，第 0 步由 Pro 建立推理基线；其他步骤按 routing_mode 分流。
    pub fn route_for_step(
        context: &SandwichContext,
        config: &ModelRoutingConfig,
        step_id: usize,
    ) -> RoutingDecision {
        if config.force_pro_on_first_step && step_id == 0 {
            return RoutingDecision {
                model_id: config.pro.clone(),
                tier: RoutingTier::Pro,
                reason: "受控评测首轮规划，固定使用 Pro 建立推理基线".to_string(),
            };
        }
        // 角色覆盖优先：首步视为 planner，其余视为 executor
        let role = if context.turns_history.is_empty() {
            roles::PLANNER
        } else {
            roles::EXECUTOR
        };
        if let Some(decision) = config.role_override(role) {
            return decision;
        }
        if config.routing_mode.eq_ignore_ascii_case("scored") {
            Self::route_scored(context, config)
        } else {
            Self::route(context, config)
        }
    }

    /// 子 agent 派发的角色路由：先查角色覆盖，再回退自动分流。
    pub fn route_for_role(
        context: &SandwichContext,
        config: &ModelRoutingConfig,
        step_id: usize,
        role: &str,
    ) -> RoutingDecision {
        if let Some(decision) = config.role_override(role) {
            return decision;
        }
        Self::route_for_step(context, config, step_id)
    }

    /// 核心网关路由：根据当前上下文，智能决定该用哪个模型
    /// 返回一个包含模型 ID、层级和判定理由的结构体
    pub fn route(context: &SandwichContext, config: &ModelRoutingConfig) -> RoutingDecision {
        // ================================================================
        // 第一优先级：多模态检测（最高优先级，不可覆盖）
        // 如果当前轮次的历史记录中包含图片（比如截图反馈），
        // 必须路由到支持视觉的模型，否则纯文本模型根本看不到图片。
        // ================================================================
        if Self::has_pending_vision(context) {
            return RoutingDecision {
                model_id: config.vision.clone(),
                tier: RoutingTier::Vision,
                reason: "当前上下文包含截图/图片，必须使用视觉模型处理".to_string(),
            };
        }

        // ================================================================
        // 第二优先级：目标块逃生通道（主动呼叫 Pro 支援）
        // ================================================================
        if let Some(block) = context.active_target_block() {
            if block.active_escalation {
                return RoutingDecision {
                    model_id: config.pro.clone(),
                    tier: RoutingTier::Pro,
                    reason: format!("目标块 [{}] 申请临时 Pro 支援（逃生通道生效）", block.id),
                };
            }
        }

        // ================================================================
        // 第三优先级：基于客观规则的难度与救场分流
        // ================================================================

        // 规则 1：第一步（空历史），若目标块未明确指定 flash，默认全局规划需强模型
        if context.turns_history.is_empty() {
            if let Some(block) = context.active_target_block() {
                if block.tier.eq_ignore_ascii_case("flash") && !config.force_pro_on_first_step {
                    return RoutingDecision {
                        model_id: config.flash.clone(),
                        tier: RoutingTier::Flash,
                        reason: format!("目标块 [{}] 显式指定 Flash 摸底，快速启动", block.id),
                    };
                }
            }
            return RoutingDecision {
                model_id: config.pro.clone(),
                tier: RoutingTier::Pro,
                reason: "首步全局规划，需要强推理能力".to_string(),
            };
        }

        // 规则 2：上一步执行失败 -> 升维救场
        // 结构化信号（宿主按 DispatchResult 写入）优先；历史文本 ❌ 仅作旧 checkpoint 兜底
        if context.last_step_failed
            || context
                .turns_history
                .iter()
                .rev()
                .find(|m| m.role == "user")
                .map(|m| m.content.as_str().unwrap_or_default().contains("❌"))
                .unwrap_or(false)
        {
            return RoutingDecision {
                model_id: config.pro.clone(),
                tier: RoutingTier::Pro,
                reason: format!(
                    "上一步执行失败（连续失败 {} 次），升维至主力模型救场",
                    context.consecutive_failures
                ),
            };
        }

        // 规则 3：当前 DOM 观测数据极其庞大，小模型可能断片
        if context.current_observation.len() > 15000 {
            return RoutingDecision {
                model_id: config.pro.clone(),
                tier: RoutingTier::Pro,
                reason: format!(
                    "当前观测数据过长 ({}字符)，小模型容易截断丢失关键信息",
                    context.current_observation.len()
                ),
            };
        }

        // 规则 4：目标块预设档位继承（动作继承所属块的难度）
        if let Some(block) = context.active_target_block() {
            if block.tier.eq_ignore_ascii_case("pro") {
                return RoutingDecision {
                    model_id: config.pro.clone(),
                    tier: RoutingTier::Pro,
                    reason: format!("继承目标块 [{}] 预设 Pro 强推理档位", block.id),
                };
            }
        }

        // ================================================================
        // 兜底：一切平安，交给便宜又快的 Flash 模型干活
        // ================================================================
        RoutingDecision {
            model_id: config.flash.clone(),
            tier: RoutingTier::Flash,
            reason: "常规操作，使用极速模型节省成本".to_string(),
        }
    }

    /// 难度打分制路由：只读结构化信号，产出透明的加权分；>= 60 升维 Pro。
    /// 视觉与首步规划仍是硬规则（前者不可覆盖，后者需建立推理基线）。
    pub fn route_scored(context: &SandwichContext, config: &ModelRoutingConfig) -> RoutingDecision {
        if Self::has_pending_vision(context) {
            return RoutingDecision {
                model_id: config.vision.clone(),
                tier: RoutingTier::Vision,
                reason: "当前上下文包含截图/图片，必须使用视觉模型处理".to_string(),
            };
        }

        // 目标块逃生通道优先（主动申请 Pro 援助）
        if let Some(block) = context.active_target_block() {
            if block.active_escalation {
                return RoutingDecision {
                    model_id: config.pro.clone(),
                    tier: RoutingTier::Pro,
                    reason: format!("目标块 [{}] 申请临时 Pro 支援（逃生通道生效）", block.id),
                };
            }
        }

        if context.turns_history.is_empty() {
            if let Some(block) = context.active_target_block() {
                if block.tier.eq_ignore_ascii_case("flash") && !config.force_pro_on_first_step {
                    return RoutingDecision {
                        model_id: config.flash.clone(),
                        tier: RoutingTier::Flash,
                        reason: format!("目标块 [{}] 显式指定 Flash 摸底，快速启动", block.id),
                    };
                }
            }
            return RoutingDecision {
                model_id: config.pro.clone(),
                tier: RoutingTier::Pro,
                reason: "首步全局规划，需要强推理能力".to_string(),
            };
        }

        // 目标块预设 Pro 档位直接升维
        if let Some(block) = context.active_target_block() {
            if block.tier.eq_ignore_ascii_case("pro") {
                return RoutingDecision {
                    model_id: config.pro.clone(),
                    tier: RoutingTier::Pro,
                    reason: format!("继承目标块 [{}] 预设 Pro 强推理档位", block.id),
                };
            }
        }

        let mut score = 0i32;
        let mut factors: Vec<String> = Vec::new();
        if context.review_contract.is_some() {
            score += 20;
            factors.push("复杂任务契约(+20)".to_string());
        }
        // 收官阶段硬升维：已收尾至少 2 项、仅剩 1–2 项未完时，收官步全部交给强模型。
        // 依据：尾段交付错的返工代价最高（24 阶段长程在第 47 步失败的实测教训）。
        if Self::is_endgame(&context.todo_list) {
            factors.push("收官阶段：任务收尾 precision 优先，直接升维".to_string());
            return RoutingDecision {
                model_id: config.pro.clone(),
                tier: RoutingTier::Pro,
                reason: format!("收官步全量升维强模型（{}）", factors.join("；")),
            };
        }
        if context.last_step_failed {
            let escalations = context.consecutive_failures.min(3) as i32;
            let weight = 15 * escalations;
            score += weight;
            factors.push(format!("连续失败{}次(+{})", context.consecutive_failures, weight));
        }
        if context.current_observation.len() > 15000 {
            score += 20;
            factors.push(format!("观测数据过长(+20, {}字符)", context.current_observation.len()));
        }
        if score >= 60 {
            RoutingDecision {
                model_id: config.pro.clone(),
                tier: RoutingTier::Pro,
                reason: format!("难度分 {} ≥ 60（{}），升维强模型", score, factors.join("、")),
            }
        } else {
            RoutingDecision {
                model_id: config.flash.clone(),
                tier: RoutingTier::Flash,
                reason: format!(
                    "难度分 {} < 60（{}），常规操作使用快模型",
                    score,
                    if factors.is_empty() { "无难度信号".to_string() } else { factors.join("、") }
                ),
            }
        }
    }

    /// 收官判定：总义务 ≥2、已完成 ≥2、剩余 0–2 项未完结（含全部完成待交付的时刻）。
    /// "扫地机器人干粗活，最后交房验收集合老师傅。"
    fn is_endgame(todos: &[crate::agent::types::TodoItem]) -> bool {
        let total = todos.len();
        let open = todos
            .iter()
            .filter(|t| t.status != "done" && t.status != "failed")
            .count();
        let done = total - open;
        total >= 2 && done >= 2 && open <= 2
    }

    /// 检查历史记录中是否包含需要视觉模型处理的多模态图片
    fn has_pending_vision(context: &SandwichContext) -> bool {
        for msg in context.turns_history.iter().rev() {
            if let Some(arr) = msg.content.as_array() {
                for item in arr {
                    if item.get("type").and_then(|t| t.as_str()) == Some("image_url") {
                        return true;
                    }
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn model_family_heuristic_classification() {
        assert_eq!(super::model_family("claude-sonnet-4"), "anthropic");
        assert_eq!(super::model_family("gpt-4o-mini"), "openai");
        assert_eq!(super::model_family("o3-preview"), "openai");
        assert_eq!(super::model_family("gemini-2.5-pro"), "google");
        assert_eq!(super::model_family("deepseek-chat"), "deepseek");
        assert_eq!(super::model_family("qwen-max"), "qwen");
        assert_eq!(super::model_family("glm-4.6"), "glm");
        assert_eq!(super::model_family("kimi-k2"), "moonshot");
        // 未知命名按 other 保守处理，不做切换
        assert_eq!(super::model_family("my-private-llm-v2"), "other");
    }

    #[test]
    fn prefer_cross_family_reviewer_switches_and_falls_back() {
        use crate::db::init_db;
        let dir = std::env::temp_dir().join(format!("xfamily-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = init_db(&dir.join("t.db")).unwrap();

        // 多家族可用：openai 规划者 → 切到非 openai 家族
        conn.execute(
            "INSERT OR REPLACE INTO platforms (id, name, base_url, api_key, created_at) VALUES ('p','test','https://example.com','sk-test','2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO models (id, platform_id, name, display_name, enabled, created_at) VALUES
             ('gpt-x','p','n','d',1,'2026-01-01T00:00:00Z'),
             ('claude-y','p','n','d',1,'2026-01-01T00:00:00Z'),
             ('glm-z','p','n','d',1,'2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let (model, note) = super::prefer_cross_family_reviewer(&conn, "gpt-x");
        assert_ne!(super::model_family(&model), "openai", "应切到异构家族");
        assert!(note.unwrap().contains("异构优选"));

        // 单家族：无异构可选 → 保持原模型 + 显式告警
        conn.execute("DELETE FROM models WHERE id IN ('claude-y','glm-z')", []).unwrap();
        let (model, note) = super::prefer_cross_family_reviewer(&conn, "gpt-x");
        assert_eq!(model, "gpt-x");
        assert!(note.unwrap().contains("无异构家族可选"));

        // 未知家族（other）：保守不切换、不打扰
        let (model, note) = super::prefer_cross_family_reviewer(&conn, "my-private-llm");
        assert_eq!(model, "my-private-llm");
        assert!(note.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;
    use crate::agent::context::SandwichContext;
    use crate::agent::review_contract::ReviewContract;

    fn ctx_with(contract: Option<ReviewContract>, failed: bool, failures: u32, observation: usize) -> SandwichContext {
        let mut context = SandwichContext::new("system".to_string(), "目标".to_string());
        if contract.is_some() {
            context.review_contract = contract;
        }
        context.record_step_outcome(!failed); // failed=false 表示成功步
        if failures > 0 {
            // 直接设置连续失败次数（record_step_outcome 已置 1，再补足）
            context.consecutive_failures = failures;
        }
        context.current_observation = "x".repeat(observation);
        context
    }

    fn push_user_turn(context: &mut SandwichContext, text: &str) {
        context.turns_history.push(crate::agent::context::types::ChatMessage {
            role: "user".to_string(),
            content: serde_json::json!(text),
        });
    }

    fn mixed_config() -> ModelRoutingConfig {
        ModelRoutingConfig {
            pro: "model-pro".to_string(),
            flash: "model-flash".to_string(),
            vision: "model-vision".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn rules_mode_keeps_legacy_escalation_and_defaults() {
        let config = mixed_config();
        let mut context = SandwichContext::new("system".to_string(), "目标".to_string());
        assert_eq!(AgentRouter::route(&context, &config).tier, RoutingTier::Pro, "首步必须 Pro");

        push_user_turn(&mut context, "上一步反馈：执行成功");
        context.record_step_outcome(true);
        assert_eq!(AgentRouter::route(&context, &config).tier, RoutingTier::Flash, "成功步回落 Flash");

        context.record_step_outcome(false);
        assert_eq!(AgentRouter::route(&context, &config).tier, RoutingTier::Pro, "结构化失败必须升维");

        context.record_step_outcome(true);
        assert_eq!(AgentRouter::route(&context, &config).tier, RoutingTier::Flash, "恢复成功回落 Flash");

        context.current_observation = "x".repeat(15001);
        assert_eq!(AgentRouter::route(&context, &config).tier, RoutingTier::Pro, "超大观测升维");

        // 旧 checkpoint 兜底：历史文本 ❌ 仍可触发升维（未写结构化信号的旧会话）
        let mut legacy = SandwichContext::new("system".to_string(), "目标".to_string());
        push_user_turn(&mut legacy, "【❌ 执行失败】工具崩溃");
        assert_eq!(AgentRouter::route(&legacy, &config).tier, RoutingTier::Pro, "❌ 字符串兜底升维");
    }

    #[test]
    fn scored_mode_weights_signals_transparently() {
        let config = ModelRoutingConfig { routing_mode: "scored".into(), ..mixed_config() };

        // 简单成功步：无信号 → Flash
        let mut context = SandwichContext::new("system".to_string(), "目标".to_string());
        push_user_turn(&mut context, "上一步反馈：执行成功");
        context.record_step_outcome(true);
        assert_eq!(AgentRouter::route_scored(&context, &config).tier, RoutingTier::Flash);

        // 契约(+20) + 单次失败(+15) = 35 < 60 → 仍 Flash
        context.review_contract = Some(ReviewContract {
            scope_statement: "s".into(),
            obligations: vec![],
        });
        context.record_step_outcome(false);
        assert_eq!(AgentRouter::route_scored(&context, &config).tier, RoutingTier::Flash);

        // 契约(+20) + 连续失败2次(+30) = 50 < 60 → Flash
        context.record_step_outcome(false);
        assert_eq!(AgentRouter::route_scored(&context, &config).tier, RoutingTier::Flash);

        // 契约(+20) + 连续失败3次(+45) = 65 ≥ 60 → Pro
        context.record_step_outcome(false);
        assert_eq!(AgentRouter::route_scored(&context, &config).tier, RoutingTier::Pro);

        // 首步（空历史）无条件 Pro
        let fresh = SandwichContext::new("system".to_string(), "目标".to_string());
        assert_eq!(AgentRouter::route_scored(&fresh, &config).tier, RoutingTier::Pro);
    }

    #[test]
    fn endgame_stage_escalates_delivery_steps_to_pro() {
        let config = ModelRoutingConfig { routing_mode: "scored".into(), ..mixed_config() };
        let mut context = SandwichContext::new("system".to_string(), "目标".to_string());
        push_user_turn(&mut context, "上一步反馈：执行成功");

        // 任务前段：4 项义务只剩 1 项完成 —— 不是收官，走常规评分
        context.todo_list = vec![
            crate::agent::types::TodoItem { id: 1, status: "done".into(), description: "a".into(), evidence_output_id: None },
            crate::agent::types::TodoItem { id: 2, status: "pending".into(), description: "b".into(), evidence_output_id: None },
            crate::agent::types::TodoItem { id: 3, status: "pending".into(), description: "c".into(), evidence_output_id: None },
            crate::agent::types::TodoItem { id: 4, status: "pending".into(), description: "d".into(), evidence_output_id: None },
        ];
        assert_eq!(AgentRouter::route_scored(&context, &config).tier, RoutingTier::Flash);

        // 收官：4 项里只剩 1 项未完 —— 强制升维
        context.todo_list[2].status = "done".into();
        context.todo_list[3].status = "done".into();
        assert_eq!(AgentRouter::route_scored(&context, &config).tier, RoutingTier::Pro, "收官步必须全量升维");

        // 只剩最后一项时同样升维
        context.todo_list[1].status = "done".into();
        assert_eq!(AgentRouter::route_scored(&context, &config).tier, RoutingTier::Pro);

        // 单义务任务不算收官（无从谈起"最后几项"）
        let mut tiny = SandwichContext::new("system".to_string(), "目标".to_string());
        push_user_turn(&mut tiny, "ok");
        tiny.todo_list = vec![
            crate::agent::types::TodoItem { id: 1, status: "pending".into(), description: "x".into(), evidence_output_id: None },
        ];
        assert_eq!(AgentRouter::route_scored(&tiny, &config).tier, RoutingTier::Flash);
    }

    #[test]
    fn role_overrides_pin_models_and_empty_falls_through() {
        let mut config = ModelRoutingConfig {
            routing_mode: "rules".into(),
            ..mixed_config()
        };
        let mut context = SandwichContext::new("system".to_string(), "目标".to_string());
        push_user_turn(&mut context, "上一步反馈：执行成功");

        // 未配置角色：走自动路由（非首步 → Flash）
        assert_eq!(AgentRouter::route_for_role(&context, &config, 3, roles::SCOUT).tier, RoutingTier::Flash);

        // 钉档位别名
        config.role_models.insert(roles::REVIEWER.to_string(), "pro".into());
        let decision = AgentRouter::route_for_role(&context, &config, 3, roles::REVIEWER);
        assert_eq!(decision.model_id, "model-pro");
        assert_eq!(decision.tier, RoutingTier::Pro);

        // 钉具体模型 UUID（逃生舱）
        config.role_models.insert(roles::WRITER.to_string(), "some-uuid-xyz".into());
        assert_eq!(
            AgentRouter::route_for_role(&context, &config, 3, roles::WRITER).model_id,
            "some-uuid-xyz"
        );

        // 空串 = 跟随自动路由
        config.role_models.insert(roles::REVIEWER.to_string(), String::new());
        assert_eq!(AgentRouter::route_for_role(&context, &config, 3, roles::REVIEWER).tier, RoutingTier::Flash);

        // planner/executor 角色钩子在 route_for_step 内生效
        let mut context_nonempty = SandwichContext::new("system".to_string(), "目标".to_string());
        push_user_turn(&mut context_nonempty, "上一步反馈：执行成功");
        context_nonempty.record_step_outcome(true);
        config.role_models.insert(roles::EXECUTOR.to_string(), "executor-uuid".into());
        assert_eq!(
            AgentRouter::route_for_step(&context_nonempty, &config, 2).model_id,
            "executor-uuid"
        );
        config.role_models.remove(&roles::EXECUTOR.to_string());
        config.role_models.insert(roles::PLANNER.to_string(), "planner-uuid".into());
        let fresh = SandwichContext::new("system".to_string(), "目标".to_string());
        assert_eq!(AgentRouter::route_for_step(&fresh, &config, 0).model_id, "planner-uuid");
    }

    #[test]
    fn test_target_block_tier_inheritance_and_escalation() {
        let config = mixed_config();
        let mut context = SandwichContext::new("system".to_string(), "目标".to_string());
        push_user_turn(&mut context, "准备开始执行");
        context.record_step_outcome(true);

        // 默认无活跃块：成功步回落 Flash
        assert_eq!(AgentRouter::route(&context, &config).tier, RoutingTier::Flash);

        // 设置活跃块为 Pro 档位
        let mut pro_block = crate::agent::target_block::TargetBlock::new("block-heavy", "重构核心");
        pro_block.tier = "pro".to_string();
        context.block_manager.set_blocks(vec![pro_block]);

        // 继承目标块的 Pro 档位
        assert_eq!(AgentRouter::route(&context, &config).tier, RoutingTier::Pro);

        // 切换为 Flash 档位块
        let mut flash_block = crate::agent::target_block::TargetBlock::new("block-light", "修改配置");
        flash_block.tier = "flash".to_string();
        context.block_manager.set_blocks(vec![flash_block]);

        // 继承 Flash 档位
        assert_eq!(AgentRouter::route(&context, &config).tier, RoutingTier::Flash);

        // 触发逃生通道（主动申请 Pro 援助）
        context.active_target_block_mut().unwrap().request_pro_escalation().unwrap();
        assert_eq!(AgentRouter::route(&context, &config).tier, RoutingTier::Pro);

        // 单步结束，逃生通道复位，回落 Flash
        context.active_target_block_mut().unwrap().on_step_finish();
        assert_eq!(AgentRouter::route(&context, &config).tier, RoutingTier::Flash);
    }
}
