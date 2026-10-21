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
    /// 尾部状态卡携带量独立覆盖：""=跟随预算模式档位 | "off"=仅未闭合门禁自愈段
    /// | "minimal"=门禁段全量 | "full"=全量（含失败记录/台账/记忆等弱模型定向辅助）
    #[serde(default)]
    pub state_card_level: String,
    /// 路由模式：rules（默认，现行规则分流）| scored（难度打分制）
    #[serde(default)]
    pub routing_mode: String,
    /// 角色级模型覆盖：角色名 → "pro"/"flash"/"vision"（档位别名）或具体模型 UUID。
    /// 空串或缺省 = 跟随自动路由。用户只需维护 pro/flash/vision 三个槽，
    /// 角色挂档位由系统默认表处理，钉死某角色才需要填这里。
    #[serde(default)]
    pub role_models: BTreeMap<String, String>,
    /// 会话级阶段审查门禁覆盖："disabled"（直通免审）| 角色名（如 "frontend_ui_reviewer"）| 空串/缺省（跟随全局默认）
    #[serde(default)]
    pub stage_reviewer_override: Option<String>,
    /// 审查人数：true = 方案/终审及适用的阶段审查最多 3 位；false = 单审（默认）。
    /// 保留 stage_review_committee 字段名兼容已有前端配置，不额外触发阶段审查。
    #[serde(default)]
    pub stage_review_committee: bool,
    /// 本会话排除/休眠的角色卡标识列表（无需修改全局工坊，即可在当前会话动态启闭某专家）
    #[serde(default)]
    pub disabled_role_cards: Vec<String>,
    /// 审查员跨家族自动优选开关。默认关闭：未显式配置 reviewer 角色时跟随常规回退链，
    /// 不自动改派异构家族模型（自动挑选可能落到未充值/未订阅的账号，实弹实测 7 派 7 死
    /// 且方案静默经仲裁放行）。true = 恢复旧的跨家族优选行为。
    #[serde(default)]
    pub auto_cross_family_reviewer: bool,
    /// 仅允许用户显式指定的审查备用模型；空串默认禁用，不扫描模型池。
    #[serde(default)]
    pub reviewer_backup_model: String,
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
            state_card_level: String::new(),
            routing_mode: String::new(),
            role_models: BTreeMap::new(),
            stage_reviewer_override: None,
            stage_review_committee: false,
            disabled_role_cards: Vec::new(),
            auto_cross_family_reviewer: false,
            reviewer_backup_model: String::new(),
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
    // 家族判定必须用模型【名称】——传入的 current_model 与候选均为 UUID，
    // 对 UUID 判家族恒为 "other"（模型 ID 不代表家族）。
    // 查名失败时回退用原值判（兼容直接传模型名的调用方）。
    let current_name: String = conn
        .query_row(
            "SELECT name FROM models WHERE id = ?1",
            [current_model],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| current_model.to_string());
    let current_family = model_family(&current_name);
    if current_family == "other" {
        return (current_model.to_string(), None);
    }
    let models: Vec<(String, String)> = {
        let Ok(mut stmt) = conn.prepare("SELECT id, name FROM models WHERE enabled = 1") else {
            return (current_model.to_string(), None);
        };
        stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    };
    let failed = recently_failed_models(conn);
    let alternative = models.iter().find(|(id, name)| {
        let family = model_family(name);
        family != current_family && family != "other" && !failed.contains(id)
    });
    match alternative {
        Some((m, m_name)) => (
            m.clone(),
            Some(format!(
                " reviewer 异构优选：{}（{} 系）→ {}（{} 系），与规划者模型家族错开以互补盲区。",
                current_model, current_family, m, model_family(m_name)
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

/// 近 30 分钟内派发失败的模型集合——enabled=1 不代表平台可达，
/// request_or_stream 失败的模型就是"平台已死"的实证，优选与故障转移必须跳过，
/// 否则会反复选中同一批死平台模型烧光审查预算（goal_anchor 实弹踩过）。
pub(crate) fn recently_failed_models(conn: &rusqlite::Connection) -> std::collections::BTreeSet<String> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT DISTINCT model_id FROM agent_child_runs
         WHERE status = 'failed' AND started_at > datetime('now', '-30 minutes')",
    ) else {
        return Default::default();
    };
    stmt.query_map([], |row| row.get::<_, String>(0))
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// 显式备用校验：不扫描其他模型、不中转第三个模型，旧配置缺省为空。
pub(crate) fn configured_reviewer_backup(
    conn: &rusqlite::Connection, primary: &str, configured: &str,
) -> Result<Option<(String, String)>, String> {
    let id = configured.trim();
    if id.is_empty() || id == primary { return Ok(None); }
    let result = conn.query_row("SELECT name, enabled FROM models WHERE id=?1",
        rusqlite::params![id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)))
        .map_err(|e| format!("配置的审查备用模型 {id} 不可用：{e}"))?;
    if !result.1 { return Err(format!("配置的审查备用模型 {} 已禁用", result.0)); }
    Ok(Some((id.to_string(), result.0)))
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
            other => {
                // 具体 UUID：按命中槽位归正确档位，避免错标 tier 进而错配推理强度
                let tier = if other == self.pro {
                    RoutingTier::Pro
                } else if other == self.vision {
                    RoutingTier::Vision
                } else {
                    RoutingTier::Flash
                };
                RoutingDecision {
                    model_id: other.to_string(),
                    tier,
                    reason: format!("角色 {} 手动指定具体模型", role),
                }
            }
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
        // 视觉最高优先级，不可覆盖：队列/历史里有图就必须走视觉档——
        // 角色钉死（role_models）若先于本检查返回，图片会发给纯文本模型等于没看。
        if Self::has_pending_vision(context) {
            // vision 槽空时回退 pro→flash（与 schedule_manager 同惯例）：
            // 空 model_id 会把调用直接打挂——宁可降级发图，不可丢图炸步。
            let model_id = [&config.vision, &config.pro, &config.flash]
                .into_iter()
                .find(|m| !m.is_empty())
                .cloned()
                .unwrap_or_default();
            return RoutingDecision {
                model_id,
                tier: RoutingTier::Vision,
                reason: "当前上下文包含截图/图片，必须使用视觉模型处理".to_string(),
            };
        }
        if config.force_pro_on_first_step && step_id == 0 {
            return RoutingDecision {
                model_id: config.pro.clone(),
                tier: RoutingTier::Pro,
                reason: "受控评测首轮规划，固定使用 Pro 建立推理基线".to_string(),
            };
        }
        // 角色覆盖优先：planner = 首步，或挂了重方案门且尚未批准（分解路由的
        // 「强模型取证定案」相位）；相位事实来自 checkpoint 投影而非历史猜测——
        // triage 注入取证单后历史已非空，旧判法会让生产会话的规划步错误落到
        // executor 档。未配置 role_models 时本分支不产生任何行为差异。
        let planning_active = context
            .plan_gate
            .as_ref()
            .map_or(false, |g| g.plan_required && g.phase != "approved");
        let role = if context.turns_history.is_empty() || planning_active {
            roles::PLANNER
        } else {
            roles::EXECUTOR
        };
        if let Some(decision) = config.role_override(role) {
            return decision;
        }
        // 方案规划相位默认走推理档（Pro）：摸底、调度探针取证、出方案属于全局推理，
        // 且相位内不中途切换模型。用户钉死 planner 角色（上方 role_override）优先；
        // 推理档未配置时回退自动路由。
        if planning_active && !config.pro.trim().is_empty() {
            return RoutingDecision {
                model_id: config.pro.clone(),
                tier: RoutingTier::Pro,
                reason: "方案规划相位默认使用推理档（Pro）：摸底、调度探针取证、出方案".to_string(),
            };
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
        // 探针（只读侦察）未显式配置时默认 Flash 档，不再跟随主 Agent 当前步的模型：
        // 规划相位主 Agent 是 Pro，探针若跟随就会让“Flash 取证”落空。
        // 与探针页“探针算力”默认高亮模型 2 的显示一致；角色卡自带 model_id 优先级更高（派发处处理）。
        if role == roles::SCOUT && !config.flash.trim().is_empty() {
            return RoutingDecision {
                model_id: config.flash.clone(),
                tier: RoutingTier::Flash,
                reason: "探针未配置算力，默认使用执行档（Flash）".to_string(),
            };
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
    /// （含 view_image 一次性注入队列——不落历史但本轮调用必须走视觉档）
    fn has_pending_vision(context: &SandwichContext) -> bool {
        if !context.pending_view_images.is_empty() {
            return true;
        }
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
    fn reviewer_backup_is_explicit_and_never_scans_pool() {
        use crate::db::init_db;
        let dir = std::env::temp_dir().join(format!("fover-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = init_db(&dir.join("t.db")).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO platforms (id, name, base_url, api_key, created_at) VALUES
             ('pa','pa','https://a','k','2026-01-01'), ('pb','pb','https://b','k','2026-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO models (id, platform_id, name, display_name, enabled, created_at) VALUES
             ('m-exec','pa','gpt-exec','e',1,'2026-01-01'),
             ('m-same-family','pa','gpt-2','e',1,'2026-01-01'),
             ('m-cross-same-platform','pa','claude-x','e',1,'2026-01-01'),
             ('m-cross-cross-platform','pb','glm-x','e',1,'2026-01-01')",
            [],
        )
        .unwrap();
        use super::configured_reviewer_backup as pick;
        assert_eq!(pick(&conn, "m-exec", "").unwrap(), None);
        assert_eq!(pick(&conn, "m-exec", "m-exec").unwrap(), None);
        assert_eq!(pick(&conn, "m-exec", " m-same-family ").unwrap().unwrap().0, "m-same-family");
        assert!(pick(&conn, "m-exec", "missing").is_err());
        conn.execute("UPDATE models SET enabled=0 WHERE id='m-same-family'", []).unwrap();
        assert!(pick(&conn, "m-exec", "m-same-family").is_err(), "不能自动改选其他启用模型");
        assert_eq!(pick(&conn, "m-exec", "").unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

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
        // （id 是不含家族线索的标识、name 携带家族关键词——与真实 models 表形态一致）
        conn.execute(
            "INSERT OR REPLACE INTO platforms (id, name, base_url, api_key, created_at) VALUES ('p','test','https://example.com','sk-test','2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO models (id, platform_id, name, display_name, enabled, created_at) VALUES
             ('uuid-gpt','p','gpt-x','d',1,'2026-01-01T00:00:00Z'),
             ('uuid-claude','p','claude-y','d',1,'2026-01-01T00:00:00Z'),
             ('uuid-glm','p','glm-z','d',1,'2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let (model, note) = super::prefer_cross_family_reviewer(&conn, "gpt-x");
        assert_ne!(super::model_family(&model), "openai", "应切到异构家族");
        assert!(note.unwrap().contains("异构优选"));

        // 单家族：无异构可选 → 保持原模型 + 显式告警
        conn.execute("DELETE FROM models WHERE id IN ('uuid-claude','uuid-glm')", []).unwrap();
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

    /// view_image 一次性注入：队列非空即必须切视觉档——该队列不落历史，
    /// has_pending_vision 是唯一感知它的通道；漏判 = 图片发出去了却没人看。
    #[test]
    fn pending_view_image_forces_vision_tier() {
        let config = mixed_config();
        let mut context = SandwichContext::new("system".to_string(), "目标".to_string());
        push_user_turn(&mut context, "上一步执行输出：render ok");
        assert_eq!(AgentRouter::route(&context, &config).tier, RoutingTier::Flash);

        context.pending_view_images.push((
            "【view_image】sheet.jpg".to_string(),
            "data:image/jpeg;base64,QUJD".to_string(),
        ));
        let d = AgentRouter::route(&context, &config);
        assert_eq!(d.tier, RoutingTier::Vision, "pending 图片必须切视觉档");
        assert_eq!(d.model_id, "model-vision");

        // 消费后（请求返回核销）回落普通路由
        context.pending_view_images.clear();
        assert_eq!(AgentRouter::route(&context, &config).tier, RoutingTier::Flash);
    }

    /// 实测事故回归：角色卡钉死 executor→flash 时，view_image 注入的图曾被
    /// 发给纯文本模型（role_override 短路在视觉检查之前）。视觉是不可覆盖
    /// 的硬规则——钉死也不能抢在它前面。
    #[test]
    fn pending_view_image_beats_role_pin() {
        let mut config = mixed_config();
        config
            .role_models
            .insert(crate::agent::router::roles::EXECUTOR.to_string(), "model-flash".to_string());
        let mut context = SandwichContext::new("system".to_string(), "目标".to_string());
        push_user_turn(&mut context, "上一步执行输出：render ok");
        context.pending_view_images.push((
            "【view_image】sheet.jpg".to_string(),
            "data:image/jpeg;base64,QUJD".to_string(),
        ));
        let d = AgentRouter::route_for_step(&context, &config, 7);
        assert_eq!(d.tier, RoutingTier::Vision, "executor 钉死不得压过视觉硬规则");
        assert_eq!(d.model_id, "model-vision");

        // vision 槽空时回退（钉死的 flash 也比空 model_id 强——至少能发出去）
        let mut config2 = ModelRoutingConfig { vision: String::new(), ..mixed_config() };
        config2.role_models.clone_from(&config.role_models);
        let d2 = AgentRouter::route_for_step(&context, &config2, 7);
        assert_eq!(d2.tier, RoutingTier::Vision);
        assert_eq!(d2.model_id, "model-pro", "vision 空槽应回退 pro");
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

        // 钉具体 UUID 且命中已知槽位时档位必须归位（曾错标 Flash 导致推理强度错配）
        config.role_models.insert(roles::REVIEWER.to_string(), "model-pro".into());
        let pinned = AgentRouter::route_for_role(&context, &config, 3, roles::REVIEWER);
        assert_eq!(pinned.model_id, "model-pro");
        assert_eq!(pinned.tier, RoutingTier::Pro);

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
    fn plan_phase_selects_planner_over_history_emptiness() {
        let mut config = ModelRoutingConfig {
            routing_mode: "rules".into(),
            ..mixed_config()
        };
        config.role_models.insert(roles::PLANNER.to_string(), "pro".into());
        config.role_models.insert(roles::EXECUTOR.to_string(), "flash".into());

        // 重方案门 + 探索中：即使历史非空（triage 已注入取证单）仍属 planner 相位
        let mut context = SandwichContext::new("system".to_string(), "目标".to_string());
        push_user_turn(&mut context, "上一步反馈：执行成功");
        context.record_step_outcome(true);
        context.plan_gate = Some(crate::agent::context::types::PlanGateProjection {
            plan_required: true,
            phase: "exploring".to_string(),
            ..Default::default()
        });
        assert_eq!(
            AgentRouter::route_for_step(&context, &config, 3).model_id,
            "model-pro",
            "批准前的规划相位必须归 planner"
        );

        // 批准后移交 executor
        context.plan_gate.as_mut().unwrap().phase = "approved".to_string();
        assert_eq!(
            AgentRouter::route_for_step(&context, &config, 4).model_id,
            "model-flash",
            "方案批准后执行归 executor"
        );

        // 轻任务（Auto+direct：未挂重方案门）探索期不占 planner 档
        let gate = context.plan_gate.as_mut().unwrap();
        gate.phase = "exploring".to_string();
        gate.plan_required = false;
        assert_eq!(
            AgentRouter::route_for_step(&context, &config, 5).model_id,
            "model-flash"
        );

        // 未配置角色钉死时：规划相位默认走推理档（Pro），方案批准/轻任务仍走既有规则路由
        let bare = ModelRoutingConfig {
            routing_mode: "rules".into(),
            ..mixed_config()
        };
        context.plan_gate = Some(crate::agent::context::types::PlanGateProjection {
            plan_required: true,
            phase: "exploring".to_string(),
            ..Default::default()
        });
        assert_eq!(
            AgentRouter::route_for_step(&context, &bare, 6).model_id,
            "model-pro",
            "无角色配置时规划相位默认走推理档"
        );
        context.plan_gate.as_mut().unwrap().phase = "approved".to_string();
        assert_eq!(
            AgentRouter::route_for_step(&context, &bare, 7).model_id,
            "model-flash",
            "方案批准后回到既有规则路由"
        );
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

    fn planning_context(phase: &str) -> SandwichContext {
        let mut context = SandwichContext::new("system".to_string(), "目标".to_string());
        push_user_turn(&mut context, "上一步反馈：执行成功");
        context.record_step_outcome(true);
        context.plan_gate = Some(crate::agent::context::types::PlanGateProjection {
            plan_required: true,
            phase: phase.to_string(),
            ..Default::default()
        });
        context
    }

    #[test]
    fn planning_phase_defaults_to_pro_without_role_pin() {
        for mode in ["rules", "scored"] {
            let mut config = ModelRoutingConfig { routing_mode: mode.into(), ..mixed_config() };
            // 规划相位（重方案门未批准）：未钉 planner 也走 Pro
            let exploring = planning_context("exploring");
            let d = AgentRouter::route_for_step(&exploring, &config, 3);
            assert_eq!((d.tier, d.model_id.as_str()), (RoutingTier::Pro, "model-pro"), "mode={mode}");
            // 方案批准后回到执行相位：非首步、一切顺利 → Flash
            let approved = planning_context("approved");
            assert_eq!(AgentRouter::route_for_step(&approved, &config, 3).tier, RoutingTier::Flash, "mode={mode}");
            // 用户钉死 planner 优先于默认
            config.role_models.insert(roles::PLANNER.to_string(), "flash".into());
            assert_eq!(AgentRouter::route_for_step(&exploring, &config, 3).tier, RoutingTier::Flash, "mode={mode}");
        }
        // 推理档未配置时不滥用空 model_id：回退自动路由
        let no_pro = ModelRoutingConfig { pro: String::new(), ..mixed_config() };
        assert!(!AgentRouter::route_for_step(&planning_context("exploring"), &no_pro, 3).model_id.is_empty());
    }

    #[test]
    fn scout_defaults_to_flash_even_in_pro_planning_phase() {
        let mut config = ModelRoutingConfig { routing_mode: "rules".into(), ..mixed_config() };
        let exploring = planning_context("exploring");
        // 规划相位主 Agent 是 Pro，但未配置的探针仍是 Flash
        assert_eq!(AgentRouter::route_for_step(&exploring, &config, 3).tier, RoutingTier::Pro);
        let scout = AgentRouter::route_for_role(&exploring, &config, 3, roles::SCOUT);
        assert_eq!((scout.tier, scout.model_id.as_str()), (RoutingTier::Flash, "model-flash"));
        // 用户在探针页点“模型 1”后仍然生效
        config.role_models.insert(roles::SCOUT.to_string(), "pro".into());
        assert_eq!(AgentRouter::route_for_role(&exploring, &config, 3, roles::SCOUT).tier, RoutingTier::Pro);
        // 其他角色（审查/写入）保持“跟随自动路由”不变
        let plain = ModelRoutingConfig { routing_mode: "rules".into(), ..mixed_config() };
        assert_eq!(AgentRouter::route_for_role(&exploring, &plain, 3, roles::REVIEWER).tier, RoutingTier::Pro);
    }
}

#[cfg(test)]
mod serde_probe {
    #[test]
    fn role_models_deserializes_from_js_shape() {
        let json = r#"{"pro":"34217be8","flash":"34217be8","vision":"34217be8","reasoning_effort":"high","role_models":{"reviewer":"4b78ff71-0be8-43e1-a02c-5a5b77f7d6b4"}}"#;
        let cfg: super::ModelRoutingConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.reviewer_backup_model.is_empty(), "旧配置默认不启用备用");
        assert_eq!(cfg.role_models.get("reviewer").map(|s| s.as_str()), Some("4b78ff71-0be8-43e1-a02c-5a5b77f7d6b4"));
    }
}
