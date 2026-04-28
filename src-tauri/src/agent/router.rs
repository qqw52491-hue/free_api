use crate::agent::context::SandwichContext;
use serde::{Deserialize, Serialize};

/// 前端传入的模型路由配置表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRoutingConfig {
    pub pro: String,
    pub flash: String,
    pub vision: String,
    /// 是否开启 Kimi 场外救援功能（默认关）
    #[serde(default)]
    pub enable_rescue: bool,
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
        if context.action_history.is_empty() {
            return RoutingDecision {
                model_id: config.pro.clone(),
                tier: RoutingTier::Pro,
                reason: "首步全局规划，需要强推理能力".to_string(),
            };
        }

        // 规则 2：上一步执行失败 -> 升维救场
        // 这是最关键的"自动升级"触发器：Flash 搞砸了，Pro 来擦屁股
        if let Some(last_action) = context.action_history.last() {
            if !last_action.success {
                return RoutingDecision {
                    model_id: config.pro.clone(),
                    tier: RoutingTier::Pro,
                    reason: format!(
                        "上一步执行失败 (Step {}：{})，升维至主力模型救场",
                        last_action.step, last_action.action
                    ),
                };
            }
        }

        // 规则 3：连续相同动作 >= 2 次（疑似死循环），切到大模型破局
        if Self::detect_repetition(context) {
            return RoutingDecision {
                model_id: config.pro.clone(),
                tier: RoutingTier::Pro,
                reason: "检测到重复动作模式，升维至主力模型破解死循环".to_string(),
            };
        }

        // 规则 4：当前 DOM 观测数据极其庞大，小模型可能断片
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
        // 这就是"自动降级"机制——只要上面所有异常条件都没有触发，
        // 系统自然而然就会回落到最经济的 Flash 模型。
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

    /// 检测是否存在重复动作模式（连续 >= 2 次相同动作）
    fn detect_repetition(context: &SandwichContext) -> bool {
        if context.action_history.len() < 2 {
            return false;
        }
        let recent = &context.action_history;
        let last = &recent[recent.len() - 1];
        let prev = &recent[recent.len() - 2];
        last.action == prev.action
    }
}
