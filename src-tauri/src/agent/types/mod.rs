use crate::agent::change_verification::ChangeVerification;
use crate::agent::review_contract::ReviewContract;
use serde::{Deserialize, Serialize};

pub const RUN_INTERRUPTED_ROUTE: &str = "run_interrupted";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStep {
    pub id: usize,
    pub description: String,
    pub thought: String,
    pub tool: String,
    pub command: String,
    pub status: String, // "pending" | "running" | "done" | "error"
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MainContent {
    pub total_objective: String,
    pub abstrack_task: String,
    pub tool_choose: String,
    pub current_message: String,
    pub now_content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TodoItem {
    pub id: usize,
    pub status: String, // "pending" | "in_progress" | "done" | "failed"
    pub description: String,
    pub evidence_output_id: Option<String>,
}

impl<'de> Deserialize<'de> for TodoItem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawTodoItem {
            #[serde(default)]
            id: Option<serde_json::Value>,
            #[serde(default)]
            status: Option<String>,
            #[serde(default)]
            description: Option<String>,
            #[serde(default)]
            task: Option<String>,
            #[serde(default)]
            title: Option<String>,
            #[serde(default)]
            text: Option<String>,
            #[serde(default)]
            content: Option<String>,
            #[serde(default)]
            name: Option<String>,
            #[serde(default)]
            evidence_output_id: Option<String>,
        }

        let raw = RawTodoItem::deserialize(deserializer)?;
        let id = match raw.id {
            Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0) as usize,
            Some(serde_json::Value::String(s)) => s.parse::<usize>().unwrap_or(0),
            _ => 0,
        };
        let description = raw
            .description
            .or(raw.task)
            .or(raw.title)
            .or(raw.text)
            .or(raw.content)
            .or(raw.name)
            .unwrap_or_default();
        let status = raw.status.unwrap_or_else(|| "pending".to_string());

        Ok(Self {
            id,
            status,
            description,
            evidence_output_id: raw.evidence_output_id,
        })
    }
}

/// 动态申请扩展当前活跃块的修改范围边界 (Scope) 请求
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ScopeExtensionRequest {
    /// 申请纳入 scope 的文件或目录相对路径列表
    #[serde(default)]
    pub paths: Vec<String>,
    /// 正当的技术联动理由（必填，解释为什么需要改动外部文件）
    #[serde(default)]
    pub reason: String,
}

/// 结构化方案提交（submit_plan）：模型声明「方案已成熟」并请求进入审查。
/// 宿主校验后把「登记方案 + 预扣审查预算 + 注册审查批次 + 派发 Tech Lead」
/// 收敛为一次一致的状态转换——方案落账与审查注册同生同灭，杜绝半截状态。
/// 派发产生的外部副作用（子任务 spawn）先行完成，内存态合并在其后一次提交。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct PlanSubmission {
    /// 方案核心摘要与实施步骤（必填）
    #[serde(default)]
    pub plan_summary: String,
    /// 拟修改或创建的文件路径列表（0~50 条；纯分析/只读审查/报告类交付可传空数组，
    /// 表示交付物为结论文本而非文件改动——由 Tech Lead 审查裁定声明是否合理）
    #[serde(default)]
    pub target_files: Vec<String>,
    /// 根因假设/问题定性（强烈建议填写）
    #[serde(default)]
    pub root_cause: String,
    /// 支撑方案的证据引用（必填，1~6 条：已读取过的文件路径或证据标识）
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    /// 关键假设（可选）
    #[serde(default)]
    pub assumptions: Vec<String>,
    /// 仍未闭合的问题（可选）
    #[serde(default)]
    pub open_questions: Vec<String>,
    /// 打算如何验证改动（可选但强烈建议）
    #[serde(default)]
    pub intended_verification: String,
    /// 自评风险等级：low | medium | high（可选）
    #[serde(default)]
    pub risk_level: String,
    /// 方案修订理由（仅当已有方案获放行后再次提交时必填）：陈述执行中新发现的
    /// 证据为何与已批准方案冲突。批准后重提视为修订轮——重开 Tech Lead 评审、
    /// 照常校验证据并消耗审查预算；留空则按误重复提交拒绝。
    #[serde(default)]
    pub revision_reason: String,
}

/// 终态预检（prepare_finish）：模型请求宿主按 finish 同一套门禁做交付就绪评估。
/// 与 finish 的区别：只沉淀 FinalizationReceipt、绝不提交终态；
/// 收据按 diff_hash + scope 版本 + 契约指纹保鲜，有效期内 finish 直接复用结论，
/// 不重复执行义务验证与终审 LLM 调用。必须单独成步，不与工具调用混发。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct PrepareFinishRequest {
    /// 预检自述摘要（可选；就绪时作为交付摘要预填）
    #[serde(default)]
    pub summary: Option<String>,
    /// 交付说明（可选）
    #[serde(default)]
    pub details: Option<String>,
}

/// 执行模型主动申请的重新分流（retriage）。
/// 模型在探索中自认方向存疑时请求独立 supervisor 用已收集证据做一次结构化复核；
/// 宿主只负责机械组装证据包与配额裁决，不判断「探索是否足够」。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct RetriageRequest {
    /// 为什么请求重新分流（必填）：发现了什么让原判决失效的证据
    #[serde(default)]
    pub reason: String,
    /// 目前仍未解决的关键问题清单（供 supervisor 判断缺的是证据还是方向）
    #[serde(default)]
    pub unresolved_questions: Vec<String>,
}

/// 一次 retriage（重新分流）的结构化判决记录。
/// 追加进 checkpoint.triage_revisions——原 triage_verdict 永不改写，
/// 终审可回看「判决 A 被什么证据推翻成判决 B」的完整链条。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TriageRevision {
    /// 判决发生时的步数
    #[serde(default)]
    pub at_step: usize,
    /// 触发来源：agent_request（模型主动申请）或 budget_milestone（资源里程点兜底）
    #[serde(default)]
    pub trigger: String,
    /// 判决：continue|narrow|split|replan|escalate
    #[serde(default)]
    pub decision: String,
    /// supervisor 给出的判决理由
    #[serde(default)]
    pub reason: String,
    /// narrow 判决点名的后续取证问题（供主 Agent 继续探索）
    #[serde(default)]
    pub next_questions: Vec<String>,
    /// split 判决给出的可并行窄子任务（供主 Agent 自行 dispatches 派发）
    #[serde(default)]
    pub split_tasks: Vec<String>,
    /// escalate 判决的建议档位：current|stronger
    #[serde(default)]
    pub recommended_tier: String,
    /// 实际给出判决的模型 id（宿主回填）
    #[serde(default)]
    pub model_id: String,
    /// 非空 = 调用失败降级产物而非真实判决（值为降级原因）
    #[serde(default)]
    pub fallback: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShortMemory {
    pub step_id: usize,
    pub tool: String,
    pub command: String,
    pub output_summary: String,
    pub success: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryItem {
    pub key: String,
    pub value: String,
}

/// 兼容新旧模型输出的记忆载荷。执行上下文最终只存 `key -> value`，但此前
/// 部分提示会诱导模型输出 `summary/detail`；这种可规范化差异不能使整轮规划失败。
impl<'de> Deserialize<'de> for MemoryItem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawMemoryItem {
            #[serde(default)]
            key: String,
            #[serde(default)]
            value: String,
            #[serde(default)]
            summary: String,
            #[serde(default)]
            detail: String,
        }

        let raw = RawMemoryItem::deserialize(deserializer)?;
        let value = if !raw.value.trim().is_empty() {
            raw.value
        } else {
            [raw.summary.trim(), raw.detail.trim()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(Self {
            key: raw.key,
            value,
        })
    }
}

// ==================== Token 用量统计 ====================
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cached_tokens: i64,
    pub cache_hit_rate: f64,    // cached_tokens / prompt_tokens * 100
    pub context_window: i64,    // 配置的上下文窗口大小 (num_ctx)
    pub usage_percent: f64,     // total_tokens / context_window * 100
    pub ttft_ms: u64,           // 首字延迟 (Time to First Token)
    pub total_duration_ms: u64, // 请求总耗时
    pub tokens_per_second: f64, // 吐字速率 (tok/s)
}

impl TokenUsage {
    pub fn new(
        prompt_tokens: i64,
        completion_tokens: i64,
        cached_tokens: i64,
        context_window: i64,
        ttft_ms: u64,
        total_duration_ms: u64,
        tokens_per_second: f64,
    ) -> Self {
        let total_tokens = prompt_tokens + completion_tokens;
        let usage_percent = if context_window > 0 {
            (total_tokens as f64 / context_window as f64) * 100.0
        } else {
            0.0
        };
        let cache_hit_rate = if prompt_tokens > 0 {
            ((cached_tokens as f64) / (prompt_tokens as f64)) * 100.0
        } else {
            0.0
        };
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached_tokens,
            cache_hit_rate,
            context_window,
            usage_percent,
            ttft_ms,
            total_duration_ms,
            tokens_per_second,
        }
    }

    pub fn summary(&self) -> String {
        format!(
            "📊 Token: 输入={}(缓存={:.0}%), 输出={}, 速率={:.1}tok/s, 首字={}ms | 上下文: {}/{} ({:.1}%)",
            self.prompt_tokens,
            self.cache_hit_rate,
            self.completion_tokens,
            self.tokens_per_second,
            self.ttft_ms,
            self.total_tokens,
            self.context_window,
            self.usage_percent
        )
    }
}

fn deserialize_optional_string_or_vec<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct StringOrVecVisitor;

    impl<'de> serde::de::Visitor<'de> for StringOrVecVisitor {
        type Value = Option<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string, a sequence of strings, or null")
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }

        fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut items = Vec::new();
            while let Some(item) = seq.next_element::<serde_json::Value>()? {
                if let Some(s) = item.as_str() {
                    let trimmed = s.trim();
                    if !trimmed.is_empty() {
                        items.push(trimmed.to_string());
                    }
                }
            }
            if items.is_empty() {
                Ok(None)
            } else {
                Ok(Some(items.join(", ")))
            }
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }
    }

    deserializer.deserialize_any(StringOrVecVisitor)
}

// 这是ai 回答的具体格式
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentInstruction {
    /// 【强制反思】AI 对上一步执行结果的客观评估，用于检测循环和死路
    #[serde(default)]
    pub reflection: String,
    #[serde(default)]
    pub thought: String,
    #[serde(default)]
    pub description: String,
    /// 面向用户的阶段解说/关键发现（1~2 句）：突破、里程碑结论、风险预警等显式标记
    #[serde(default)]
    pub finding: Option<String>,
    /// finding 性质分类：start|breakthrough|milestone|summary|warning|blocker
    #[serde(default)]
    pub finding_kind: Option<String>,
    /// 当前动作所属的稳定计划节点。相同 plan_id 的连续动作会收敛到同一父节点。
    #[serde(default)]
    pub plan_id: Option<String>,
    /// 面向用户的计划标题；切换该标题代表开始新的工作阶段。
    #[serde(default)]
    pub plan_title: Option<String>,
    /// 全局任务会话精炼主旨（仅在人类发起新任务或干预指令的首次规划中提供，4~12字名词短语）
    #[serde(default)]
    pub session_title: Option<String>,
    /// 显式申明开始新工作阶段（结合 plan_id 切换 Episode）
    #[serde(default)]
    pub new_plan: Option<bool>,
    /// 模型决策中显式采纳的 Lesson ID 列表（用于血缘追踪与精准负反馈归因）
    #[serde(default)]
    pub applied_lessons: Option<Vec<String>>,

    // 兼容 tool / action
    pub tool: Option<String>,
    pub action: Option<String>,

    // 兼容 command / params
    pub command: Option<serde_json::Value>,
    pub params: Option<serde_json::Value>,

    // 兼容大模型直接平铺参数的幻觉（例如直接输出 "url": "..."）
    pub url: Option<String>,
    pub text: Option<String>,
    pub id: Option<u64>,

    #[serde(default)]
    pub todo_update: Vec<TodoItem>,
    /// 目标块显式规划或批量更新（粗粒度任务单元）
    #[serde(default)]
    pub target_blocks_update: Option<Vec<crate::agent::target_block::TargetBlock>>,
    /// 执行模型主动申请临时 Pro 强模型援助（逃生通道，每块限频）
    #[serde(default)]
    pub request_escalation: Option<bool>,
    /// 交付或阶段收官时提交的 3 行自述（改动说明、选型理由、存疑与隐患点）
    #[serde(default)]
    pub self_report: Option<crate::agent::target_block::BlockSelfReport>,
    /// 动态申请扩展当前活跃块的修改范围边界 (Scope)，避免跨文件联动修改被机械门禁误杀
    #[serde(default)]
    pub scope_extension: Option<ScopeExtensionRequest>,
    /// 执行模型主动申请重新分流（retriage）：宿主将已收集证据打包交给
    /// supervisor 档模型做一次结构化复核。必须单独成步（不与工具调用混发）。
    #[serde(default)]
    pub request_retriage: Option<RetriageRequest>,
    /// 结构化方案提交：提交后宿主原子完成「登记方案 + 预扣预算 + 注册批次 + 派发 Tech Lead」。
    /// 必须单独成步（不与工具调用混发）；仅在交叉审查模式且方案未放行前有效。
    #[serde(default)]
    pub submit_plan: Option<PlanSubmission>,
    /// 终态预检：运行与 finish 相同的门禁评估但只沉淀 FinalizationReceipt、不提交终态。
    /// 必须单独成步（不与工具调用混发）。
    #[serde(default)]
    pub prepare_finish: Option<PrepareFinishRequest>,
    /// 仅复杂审查、诊断或研究任务使用的完整审查契约更新。
    #[serde(default)]
    pub review_contract_update: Option<ReviewContract>,
    /// 最近真实写入后的验证证据或明确豁免；纯只读任务无需提供。
    #[serde(default)]
    pub change_verification_update: Option<ChangeVerification>,
    #[serde(default)]
    pub memories_update: Vec<MemoryItem>,
    /// AI 主动维护的全局进度摘要（写日记），覆盖更新。
    #[serde(default)]
    pub progress_summary: Option<String>,
    /// 【指令流水线】支持单回合执行多条连续动作
    #[serde(default)]
    pub commands: Vec<serde_json::Value>,
    /// 【并行扇出】支持单回合并发执行多个子任务 (allSettled 语义)
    #[serde(default)]
    pub parallel: Option<Vec<serde_json::Value>>,
    /// 【嵌套计划树】支持串行/并行任意嵌套的高阶计划树
    #[serde(default)]
    pub plan_tree: Option<serde_json::Value>,
    /// 【批量派发】针对子 Agent（如 child_worker / child_writer）的一步多任务派发
    #[serde(default)]
    pub dispatches: Option<Vec<serde_json::Value>>,
    /// 【预加载优化】AI 预告下一轮想用的工具/Skill（支持逗号分隔或数组多选，系统提前加载对应详细说明书）
    #[serde(default, deserialize_with = "deserialize_optional_string_or_vec")]
    pub next_tool_hint: Option<String>,
    #[serde(default)]
    pub require_memory: Option<bool>,

    /// 当历史过长时，AI 主动总结到 memories_update，并传 true 来清空历史
    #[serde(default)]
    pub clear_history: Option<bool>,

    // 终极绝招：捕获所有未定义但被平铺的外卡字段（如 element_id, val 等幻觉字段）
    #[serde(flatten)]
    pub extra_fields: std::collections::HashMap<String, serde_json::Value>,
}

impl AgentInstruction {
    /// 反序列化成功并不代表模型给出了可执行指令。
    /// 计划、反思、Todo、审查契约等状态字段可以随动作一起出现，
    /// 但不能单独作为一个会被主循环执行的回合。
    ///
    /// 不在此处复制动态工具目录或各工具的参数 schema：这些仍由
    /// 工具注册表/宿主安全层校验。这里只拒绝会退化为隐式 `core` 的
    /// 无动作对象，并保留 parallel、plan_tree 与 pipeline 三类高阶动作。
    /// 是否存在真正的可执行入口（tool / action / commands / parallel / plan_tree / dispatches）。
    /// 与 validate_executable_shape 共用同一份判定，避免「形状校验」与「纯状态请求识别」
    /// 两处各写一套导致漂移。
    pub fn has_executable_entry(&self) -> bool {
        if self.parallel.is_some() || self.plan_tree.is_some() || !self.commands.is_empty() || self.has_dispatches() {
            return true;
        }

        let has_tool = self
            .tool
            .as_deref()
            .map(str::trim)
            .is_some_and(|tool| !tool.is_empty());
        let has_legacy_action = self
            .action
            .as_deref()
            .map(str::trim)
            .is_some_and(|action| !action.is_empty())
            || self.command.as_ref().is_some_and(|command| {
                if let Some(object) = command.as_object() {
                    object
                        .get("action")
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .is_some_and(|action| !action.is_empty())
                } else {
                    command.as_str().is_some_and(|action| {
                        let action = action.trim();
                        !action.is_empty() && !action.contains(' ')
                    })
                }
            });

        has_tool || has_legacy_action
    }

    /// 是否为「纯重新分流申请」：携带 request_retriage 但没有任何工具入口。
    /// 分流是独立的状态判断动作，不与工具调用混发。
    pub fn is_bare_retriage_request(&self) -> bool {
        self.request_retriage.is_some() && !self.has_executable_entry()
    }

    /// 是否为「纯方案提交」：携带 submit_plan 但没有任何工具入口。
    /// 方案提交是独立的状态转换动作，不与工具调用混发。
    pub fn is_bare_plan_submission(&self) -> bool {
        self.submit_plan.is_some() && !self.has_executable_entry()
    }

    /// 顶层动作误写为工具调用的兼容解包：{"tool":"submit_plan","command":{...}}
    /// → submit_plan 字段（prepare_finish 同理）。实测跨场复犯的格式摩擦——
    /// 语义与独立动作完全等价，就地归一比打回重学省一整轮。
    /// 仅在恰好是「单工具调用且工具名为顶层动作」时解包；payload 不合法
    /// 返回 None 维持原样（走未知工具报错）。混发（另带 tool/commands）
    /// 不解包——那条歧义仍归协议误用分支处理。
    pub fn unwrap_standalone_tool_alias(&self) -> Option<AgentInstruction> {
        if self.submit_plan.is_some()
            || self.prepare_finish.is_some()
            || self.parallel.is_some()
            || self.plan_tree.is_some()
            || !self.commands.is_empty()
            || self.has_dispatches()
        {
            return None;
        }
        let tool = self.tool.as_deref().map(str::trim)?;
        if !matches!(tool, "submit_plan" | "prepare_finish") {
            return None;
        }
        let mut next = self.clone();
        let payload = next.command.take().or_else(|| next.params.take())?;
        match tool {
            "submit_plan" => {
                next.submit_plan = serde_json::from_value(payload).ok()?;
            }
            _ => {
                next.prepare_finish = serde_json::from_value(payload).ok()?;
            }
        }
        next.tool = None;
        next.action = None;
        Some(next)
    }

    /// 是否为「纯终态预检」：携带 prepare_finish 但没有任何工具入口。
    /// 预检是独立的评估动作，由宿主在主循环门禁段处理，不与工具调用混发。
    pub fn is_bare_prepare_finish(&self) -> bool {
        self.prepare_finish.is_some() && !self.has_executable_entry()
    }

    pub fn validate_executable_shape(&self) -> Result<(), String> {
        if self.has_executable_entry()
            || self.request_retriage.is_some()
            || self.submit_plan.is_some()
            || self.prepare_finish.is_some()
        {
            return Ok(());
        }

        Err(
            "AgentInstruction 缺少可执行入口（tool/action）。计划、反思、Todo、审查契约或进度摘要可以保留，但必须同时输出非空 tool、兼容 action，或 parallel、plan_tree、commands；宿主不会把无动作对象默认为 core。"
                .to_string(),
        )
    }

    pub fn has_dispatches(&self) -> bool {
        self.dispatches.as_ref().is_some_and(|list| !list.is_empty())
    }

    /// 获取批量派发列表；如果没有提供非空 dispatches 则回退到单个 `vec![self.get_params()]`
    pub fn get_dispatch_items(&self) -> Vec<serde_json::Value> {
        if let Some(ref items) = self.dispatches {
            if !items.is_empty() {
                return items.clone();
            }
        }
        vec![self.get_params()]
    }

    /// 检查是否为混合格式（既有 dispatches 又有 params/command 单发字段）
    pub fn is_mixed_dispatch_format(&self) -> bool {
        if let Some(ref items) = self.dispatches {
            if !items.is_empty() {
                return self.params.is_some() || self.command.is_some();
            }
        }
        false
    }

    pub fn get_action(&self) -> String {
        if self.parallel.is_some() {
            return "parallel".to_string();
        }
        if self.plan_tree.is_some() {
            return "plan_tree".to_string();
        }
        if !self.commands.is_empty() {
            return "pipeline".to_string();
        }
        if let Some(ref a) = self.action {
            return a.to_string();
        }
        if let Some(ref cmd) = self.command {
            if let Some(cmd_obj) = cmd.as_object() {
                if let Some(action_val) = cmd_obj.get("action") {
                    if let Some(action_str) = action_val.as_str() {
                        return action_str.to_string();
                    }
                }
            } else if let Some(cmd_str) = cmd.as_str() {
                // 如果 command 本身就是个单纯的字符串动词
                if !cmd_str.contains(' ') {
                    return cmd_str.to_string();
                }
            }
        }
        self.tool.clone().unwrap_or_default()
    }

    /// 提取下一轮需要预加载的工具或 Skill 列表（支持逗号分隔、分号分隔或数组多选；未设置时回退至当前 action）
    pub fn get_next_tool_hints(&self) -> Vec<String> {
        let raw_hint = self.next_tool_hint.as_deref().unwrap_or("").trim();
        if !raw_hint.is_empty() {
            let list: Vec<String> = raw_hint
                .split(|c| c == ',' || c == ';' || c == '，' || c == '；' || c == '、')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty() && *s != "finish" && *s != "core")
                .map(|s| s.to_string())
                .collect();
            if !list.is_empty() {
                return list;
            }
        }
        let action = self.get_action();
        if action.is_empty() || action == "finish" || action == "core" {
            Vec::new()
        } else {
            vec![action]
        }
    }

    pub fn get_tool(&self) -> String {
        if self.parallel.is_some() {
            return "parallel".to_string();
        }
        if self.plan_tree.is_some() {
            return "plan_tree".to_string();
        }
        if !self.commands.is_empty() {
            return "pipeline".to_string();
        }
        self.tool.clone().unwrap_or_else(|| "core".to_string())
    }

    pub fn get_params(&self) -> serde_json::Value {
        if let Some(ref par) = self.parallel {
            return serde_json::json!({ "parallel": par });
        }
        if let Some(ref tree) = self.plan_tree {
            return serde_json::json!({ "plan_tree": tree });
        }
        if !self.commands.is_empty() {
            return serde_json::json!({ "commands": self.commands });
        }

        // 发现 AI 有时会将动词（如 "type"）放在 command 字段，把其余参数放在 params 字段
        let c_is_verb = self.command.as_ref().map_or(false, |c| {
            c.is_string() && !c.as_str().unwrap().contains(' ') && !c.as_str().unwrap().is_empty()
        });

        if c_is_verb && self.params.as_ref().map_or(false, |p| p.is_object()) {
            let mut map = self.params.as_ref().unwrap().as_object().unwrap().clone();
            map.insert(
                "command".to_string(),
                self.command.as_ref().unwrap().clone(),
            );
            return serde_json::Value::Object(map);
        }

        if let Some(ref c) = self.command {
            if c.is_object() || (c.is_string() && c.as_str().unwrap().contains(' ')) {
                return c.clone();
            }
            if self.params.is_none() {
                return c.clone();
            }
        }

        if let Some(ref p) = self.params {
            return p.clone();
        }

        // 如果都没传，并且提取到了平铺的幻觉参数，则自动组装成 JSON Object 返回给下层
        let mut map = serde_json::Map::new();
        if let Some(ref u) = self.url {
            map.insert("url".to_string(), serde_json::Value::String(u.clone()));
        }
        if let Some(ref t) = self.text {
            map.insert("text".to_string(), serde_json::Value::String(t.clone()));
        }
        if let Some(i) = self.id {
            map.insert("id".to_string(), serde_json::Value::Number(i.into()));
        }

        // 把所有的外卡字段都塞进去
        for (k, v) in &self.extra_fields {
            map.insert(k.clone(), v.clone());
        }

        if !map.is_empty() {
            return serde_json::Value::Object(map);
        }

        serde_json::json!({})
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryStep {
    pub thought: String,
    pub description: String,
    pub tool: String,
    pub command: String,
    pub output_summary: String,
}

#[derive(Debug, Clone)]
pub struct DispatchResult {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    pub route: String,
    /// 可选结构化视图：工具执行者直供的 UI 渲染数据（kind 判别联合），
    /// 前端按 view.kind 路由到类型化卡片；None 时走既有文本启发式渲染。
    /// 与 stdout/stderr（模型向文本）正交分离——同一事实，两路投影。
    pub view: Option<serde_json::Value>,
}

/// 子 Agent 派发时的结构化交接上下文（P2 交接契约升级）
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct DispatchContext {
    /// root 从自己的上下文里摘的相关材料，逐条注入 user prompt
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub todo_slice: Vec<String>,
    #[serde(default)]
    pub file_hints: Vec<String>,
    #[serde(default)]
    pub prior_findings: Vec<String>,
}

impl DispatchContext {
    pub fn is_empty(&self) -> bool {
        self.background.as_deref().unwrap_or("").trim().is_empty()
            && self.todo_slice.is_empty()
            && self.file_hints.is_empty()
            && self.prior_findings.is_empty()
    }
}

/// 解析可选的结构化交接上下文（容错解析 + 边界约束）
pub fn parse_dispatch_context(val: Option<&serde_json::Value>) -> Result<Option<DispatchContext>, String> {
    let Some(val) = val else {
        return Ok(None);
    };
    if val.is_null() {
        return Ok(None);
    }
    let obj = val
        .as_object()
        .ok_or_else(|| "context_excerpts 必须是一个 JSON 对象。".to_string())?;

    let background = obj
        .get("background")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(ref bg) = background {
        if bg.chars().count() > 4000 {
            return Err("context_excerpts.background 最多 4000 字符。".to_string());
        }
    }

    let parse_str_list = |field: &str, max_items: usize, max_chars: usize| -> Result<Vec<String>, String> {
        let mut list = Vec::new();
        if let Some(arr) = obj.get(field).and_then(|v| v.as_array()) {
            for (idx, item) in arr.iter().enumerate() {
                if idx >= max_items {
                    return Err(format!("context_excerpts.{} 最多包含 {} 项。", field, max_items));
                }
                let s = item
                    .as_str()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| format!("context_excerpts.{} 的元素必须是非空字符串。", field))?;
                if s.chars().count() > max_chars {
                    return Err(format!("context_excerpts.{} 单项最多 {} 字符。", field, max_chars));
                }
                list.push(s);
            }
        }
        Ok(list)
    };

    let todo_slice = parse_str_list("todo_slice", 20, 500)?;
    let file_hints = parse_str_list("file_hints", 30, 500)?;
    let prior_findings = parse_str_list("prior_findings", 20, 1000)?;

    let ctx = DispatchContext {
        background,
        todo_slice,
        file_hints,
        prior_findings,
    };

    if ctx.is_empty() {
        Ok(None)
    } else {
        Ok(Some(ctx))
    }
}

/// 任务描述质量检查（软警示判断）：无换行、无路径/文件扩展名特征、无命令/代码特征、且长度较短时返回 true。
pub fn is_task_thin(task: &str) -> bool {
    let t = task.trim();
    if t.is_empty() {
        return true;
    }
    let has_newline = t.contains('\n');
    let has_path_or_file = t.contains('/')
        || t.contains('\\')
        || [
            ".rs", ".ts", ".js", ".jsx", ".tsx", ".json", ".vue", ".html", ".css", ".md", ".toml",
            ".py", ".sh", ".sql", ".yaml", ".yml",
        ]
        .iter()
        .any(|ext| t.contains(ext));
    let has_command_or_symbol = [
        "npm", "cargo", "pnpm", "yarn", "git", "test", "build", "run", "fn ", "function", "class",
        "def ", "struct", "let ", "const ",
    ]
    .iter()
    .any(|kw| t.to_lowercase().contains(kw));

    !has_newline && !has_path_or_file && !has_command_or_symbol && t.chars().count() < 30
}

/// 子任务（审查/对质/收割）`error_kind` 列的统一分类学。
///
/// 生产端只能写枚举值、消费端只认谓词——新增 kind 时编译器强制所有
/// match 点表态，杜绝"生产了但消费名单漏配"的静默失效（实测教训：
/// request_or_stream 生产端存在已久，failover/退款名单漏配导致 402
/// 欠费的备用审查员被当作"裁决"直接交付）。
///
/// worker/writer 的 error_kind 存自由文本（如 "writer 单步调用超时"），
/// 不在此列；stage_review 等别的表另有取值空间。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildErrorKind {
    /// 超时（落库统一 "timeout"；历史拼写 "timed_out" 由 parse 归一）
    Timeout,
    /// 随父任务/用户中断
    Interrupted,
    /// 流被主动打断取消
    Cancelled,
    /// 模型有响应但输出不合审查契约（含强制收敛后仍吐工具调用）
    InvalidOutput,
    /// 请求/流式层失败：402 欠费、断流、DNS 等零产出
    RequestOrStream,
    /// 收割器对僵死 running 行的占位标记，迟到真实结果可覆盖
    ReapedStale,
    /// 特化契约校验违例（载荷=违规详情），按实质结果交付不走故障转移
    ContractViolation(String),
    /// 未收编值：worker/writer 自由文本、未来新增 kind。
    /// 显式变体而非 `_` 通配——谓词用裸 match 全列举，新增具名变体时
    /// 所有谓词编译报错强制表态，这正是本枚举存在的意义。
    Other(String),
}

impl ChildErrorKind {
    /// 落库字符串（与历史数据格式逐字节一致；Other 原样透传）
    pub fn db_value(&self) -> String {
        match self {
            Self::Timeout => "timeout".to_string(),
            Self::Interrupted => "interrupted".to_string(),
            Self::Cancelled => "cancelled".to_string(),
            Self::InvalidOutput => "invalid_output".to_string(),
            Self::RequestOrStream => "request_or_stream".to_string(),
            Self::ReapedStale => "reaped_stale".to_string(),
            Self::ContractViolation(detail) => format!("contract_violation: {detail}"),
            Self::Other(raw) => raw.clone(),
        }
    }

    /// 从落库字符串解析（恒成功）；容忍历史拼写 "timed_out" 与裸
    /// "contract_violation"；无法识别的值收编为 Other 原样保留。
    pub fn parse(raw: &str) -> Self {
        match raw {
            "timeout" | "timed_out" => Self::Timeout,
            "interrupted" => Self::Interrupted,
            "cancelled" => Self::Cancelled,
            "invalid_output" => Self::InvalidOutput,
            "request_or_stream" => Self::RequestOrStream,
            "reaped_stale" => Self::ReapedStale,
            _ => raw.strip_prefix("contract_violation").map_or_else(
                || Self::Other(raw.to_string()),
                |rest| Self::ContractViolation(rest.trim_start_matches(':').trim().to_string()),
            ),
        }
    }

    /// 基础设施/执行异常：零实质产出（区别于业务驳回/实质裁决）。
    /// ReapedStale 是占位标记、ContractViolation 按实质结果交付、
    /// Other 按非 infra 处理（与旧字符串未命中的行为一致）。
    /// 【纪律】裸 match 全列举，禁止 `_` 通配：新增变体必须当场表态。
    pub fn is_infra_failure(&self) -> bool {
        match self {
            Self::Timeout
            | Self::Interrupted
            | Self::Cancelled
            | Self::InvalidOutput
            | Self::RequestOrStream => true,
            Self::ReapedStale | Self::ContractViolation(_) | Self::Other(_) => false,
        }
    }

    /// 是否触发审查者故障转移换备用审查员。
    /// 用户主动取消/中断不重派；契约违例与收割占位按结果路径交付。
    pub fn is_failover_eligible(&self) -> bool {
        match self {
            Self::InvalidOutput | Self::Timeout | Self::RequestOrStream => true,
            Self::Interrupted
            | Self::Cancelled
            | Self::ReapedStale
            | Self::ContractViolation(_)
            | Self::Other(_) => false,
        }
    }

    /// 审查轮次退款资格：与 infra 同集，零产出不烧预算
    pub fn is_refundable(&self) -> bool {
        self.is_infra_failure()
    }
}

impl std::fmt::Display for ChildErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.db_value())
    }
}

#[cfg(test)]
mod tests;
