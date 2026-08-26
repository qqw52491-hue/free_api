use crate::agent::context::SandwichContext;
use serde::{Deserialize, Serialize};

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
}
fn default_reasoning_effort() -> String {
    "auto".to_string()
}
fn default_context_budget_mode() -> String {
    "deep_coding".to_string()
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

pub struct AgentRouter;

impl AgentRouter {
    /// 在需要可复现的评测中，第 0 步由 Pro 建立推理基线；其他步骤保留原有动态分流。
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
        Self::route(context, config)
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
        // 第二优先级：基于客观规则的难度分流
        // ================================================================

        // 规则 1：第一步（空历史），需要全局规划，必须用强模型
        if context.turns_history.is_empty() {
            return RoutingDecision {
                model_id: config.pro.clone(),
                tier: RoutingTier::Pro,
                reason: "首步全局规划，需要强推理能力".to_string(),
            };
        }

        // 规则 2：上一步执行失败 -> 升维救场
        // 检查最新的 user 消息（反馈），如果包含 ❌ 说明上一步报错了
        if let Some(last_msg) = context
            .turns_history
            .iter()
            .rev()
            .find(|m| m.role == "user")
        {
            if last_msg.content.as_str().unwrap_or_default().contains("❌") {
                return RoutingDecision {
                    model_id: config.pro.clone(),
                    tier: RoutingTier::Pro,
                    reason: "上一步执行失败，升维至主力模型救场".to_string(),
                };
            }
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

        // ================================================================
        // 兜底：一切平安，交给便宜又快的 Flash 模型干活
        // ================================================================
        RoutingDecision {
            model_id: config.flash.clone(),
            tier: RoutingTier::Flash,
            reason: "常规操作，使用极速模型节省成本".to_string(),
        }
    }

    /// 检查最近一轮的历史记录中是否包含需要视觉模型处理的图片
    /// 注意：只检查最后一条 user 消息，因为更早的图片已经被处理过了
    fn has_pending_vision(context: &SandwichContext) -> bool {
        // 从后往前找最后一条 user 消息
        for msg in context.turns_history.iter().rev() {
            if msg.role != "user" {
                continue;
            }
            if let Some(arr) = msg.content.as_array() {
                for item in arr {
                    if item.get("type").and_then(|t| t.as_str()) == Some("image_url") {
                        return true;
                    }
                }
            }
            // 找到最后一条 user 消息后就停止，不需要再往前翻
            break;
        }
        false
    }
}
