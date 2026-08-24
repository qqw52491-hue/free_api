use crate::agent::context::no_progress::NoProgressTracker;
use crate::agent::context::resource_scope::ResourceScopeContract;
use crate::agent::review_contract::ReviewContract;
use crate::agent::task_store::TaskResourceRef;
use crate::agent::types::{AgentInstruction, TodoItem};
use crate::llm::ModelConfig;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: String,
    pub content: serde_json::Value, // 支持 String 或 Array (多模态)
}

/// 失败教训记录（用于防止大模型在长任务中重复尝试已知失败的方法）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FailedAttempt {
    pub step_id: usize,
    pub tool: String,
    pub action: String,
    pub error_summary: String,
    #[serde(default)]
    pub output_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SideEffectRecord {
    pub step_id: usize,
    pub tool: String,
    pub description: String,
    pub timestamp: String,
}

/// 阶段计划状态机：自包含管理计划 ID、标题与自增编号
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanState {
    pub active_id: Option<String>,
    pub active_title: Option<String>,
    pub next_index: usize,
}

impl PlanState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 推进或激活指定计划（支持显式 new_plan 标志与 plan_id 回显，防止标题微小抖动导致 Episode 碎片化）
    pub fn resolve_plan_explicit(
        &mut self,
        requested_id: Option<String>,
        requested_title: Option<String>,
        is_new_plan: bool,
        default_title: &str,
    ) -> (String, String) {
        // 1. 如果模型明确指定了 ID
        if let Some(id) = requested_id {
            let title = requested_title
                .or_else(|| self.active_title.clone())
                .unwrap_or_else(|| default_title.to_string());
            self.active_id = Some(id.clone());
            self.active_title = Some(title.clone());
            return (id, title);
        }

        // 2. 如果模型显式申明开启新阶段 (new_plan: true)
        if is_new_plan {
            self.next_index = self.next_index.saturating_add(1);
            let id = format!("plan-{}", self.next_index);
            let title = requested_title
                .or_else(|| self.active_title.clone())
                .unwrap_or_else(|| default_title.to_string());
            self.active_id = Some(id.clone());
            self.active_title = Some(title.clone());
            return (id, title);
        }

        // 3. 如果已有活跃阶段，且未显式声明 new_plan，则保持当前 ID，仅平滑更新标题（防止措辞微调导致 Episode 碎片化）
        if let Some(existing_id) = self.active_id.clone() {
            let title = requested_title
                .or_else(|| self.active_title.clone())
                .unwrap_or_else(|| default_title.to_string());
            self.active_title = Some(title.clone());
            return (existing_id, title);
        }

        // 4. 初次启动自动分配 plan-1
        self.next_index = self.next_index.saturating_add(1);
        let id = format!("plan-{}", self.next_index);
        let title = requested_title.unwrap_or_else(|| default_title.to_string());
        self.active_id = Some(id.clone());
        self.active_title = Some(title.clone());
        (id, title)
    }

    /// 兼容旧版调用入口
    pub fn resolve_plan(
        &mut self,
        requested_id: Option<String>,
        requested_title: Option<String>,
        default_title: &str,
    ) -> (String, String) {
        self.resolve_plan_explicit(requested_id, requested_title, false, default_title)
    }
}

/// 核心三明治上下文总控结构体 (State Root)
/// 严格遵循三明治分层拓扑，将静态不可变前缀、只追加对话流水与动态尾部状态卡解耦。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandwichContext {
    // ════════════════════════════════════════════════════════════════════
    // 🍞 1. 顶层面包 (Static Prefix Zone - 100% 静态，锁死前缀缓存)
    // ════════════════════════════════════════════════════════════════════
    /// 系统核心契约与安全规则 (core_logic.md + 内置工具手册)
    pub system_prompt: String,
    /// 用户的终极任务目标 (如 "部署 Rust + PostgreSQL 项目并在 8080 启动")
    pub user_goal: String,
    /// 当前按需展开的活跃工具/Skill 规格详情 (通过 next_tool_hint 懒加载，其余时间为空)
    pub active_tool_detail: String,

    // ════════════════════════════════════════════════════════════════════
    // 🥩 2. 中间夹心 (Append-Only Conversation Stream - 线性追加流水)
    // ════════════════════════════════════════════════════════════════════
    /// 历史对话回合流 (仅包含 action_brief 与脱水采样后的 execution feedback)
    pub turns_history: Vec<ChatMessage>,

    // ════════════════════════════════════════════════════════════════════
    // 🍞 3. 底层面包 (Dynamic Tail State Card - 动态聚合状态卡，压在最末端)
    // ════════════════════════════════════════════════════════════════════
    /// 活动计划状态机 (聚合 plan_id, title 与自增计数器)
    #[serde(default)]
    pub plan_state: PlanState,
    /// 核心事实便签库 (严格 BTreeMap 字典序，保证序列化字节绝对确定)
    pub memories: BTreeMap<String, String>,
    /// 避坑指南 (记录近期失败原因与规避策略，最多保留 6 条)
    pub failed_attempts: Vec<FailedAttempt>,
    /// 不可逆外部操作安全台账 (记录 shell_exec/文件修改等写操作)
    pub side_effects: Vec<SideEffectRecord>,
    /// 当前任务待办清单 (Todo items)
    pub todo_list: Vec<TodoItem>,
    /// 复杂审查、诊断或研究任务的可审计完成契约；普通任务保持 None。
    #[serde(default)]
    pub review_contract: Option<ReviewContract>,
    /// 全局阶段进度与策略宏观摘要 (~100 tokens，每轮更新)
    pub progress_summary: String,
    /// 活跃资源作用域契约；独立于历史压缩，持续投影到每轮上下文。
    #[serde(default)]
    pub resource_scope: ResourceScopeContract,
    /// 无进展重复检测：相同工具+相同参数+相同输出/失败的运行时事实（策略级提示用）。
    #[serde(default)]
    pub no_progress: NoProgressTracker,
    /// 服务端回传的最近一次完整 Agent prompt tokens（总口径：system+goal+历史+尾部）。
    /// 它必须与同一完整请求发送时的历史估算快照成对使用；隔离 JSON 修复请求
    /// 不覆盖这组基线，避免将短修复 prompt 误用于下一次完整上下文的水位校准。
    #[serde(default)]
    pub last_reported_prompt_tokens: Option<usize>,
    /// 与 `last_reported_prompt_tokens` 同一完整请求发送时的历史保守估算。
    /// 下次水位只追加 `当前历史 - 此快照` 的正向增量，不再反扣本地静态段或
    /// 依赖固定单步余量。
    #[serde(default)]
    pub last_reported_history_estimate_tokens: Option<usize>,
    /// 已事件化写入历史的进度摘要全文（write-once）：变化才追加完整版进
    /// 历史，尾部状态卡此后只携带首行摘要+版本指针，削减每轮全量重发。
    #[serde(default)]
    pub last_persisted_summary: Option<String>,
    /// 进度摘要的历史事件版本号。
    #[serde(default)]
    pub persisted_summary_revision: u64,
    /// 外部独立任务数据区目录引用 (轻量 schema 与行数，完整数据行在 SQLite)
    #[serde(default)]
    pub task_resources: Vec<TaskResourceRef>,
    /// 当前最新一次工具执行的环境观测结果 (Observation)
    pub current_observation: String,

    // ════════════════════════════════════════════════════════════════════
    // ⚙️ 4. 运行时调度元数据 (Runtime Metadata)
    // ════════════════════════════════════════════════════════════════════
    pub active_tool: Option<String>,
    /// 尾部轻量可用工具快速索引（~50 tokens，紧邻模型输出端强化注意力防幻觉）
    #[serde(default)]
    pub compact_tools_index: Option<String>,
    pub carry_memories: bool,
    /// 当前任务所用模型的上下文窗口（tokens）
    #[serde(default)]
    pub context_window_tokens: usize,
    /// 已加载并写入历史的工具手册集合（字典序 BTreeSet 保证确定性序列化）
    #[serde(default)]
    pub fetched_tools: BTreeSet<String>,
    /// 压缩引擎专用 LLM 配置（运行时注入，不参与 checkpoint 序列化）
    #[serde(skip)]
    pub compaction_model_config: Option<ModelConfig>,
}

impl SandwichContext {
    pub fn new(system_prompt: String, user_goal: String) -> Self {
        Self {
            system_prompt,
            user_goal,
            memories: BTreeMap::new(),
            failed_attempts: Vec::new(),
            side_effects: Vec::new(),
            todo_list: Vec::new(),
            review_contract: None,
            turns_history: Vec::new(),
            current_observation: String::new(),
            progress_summary: String::new(),
            plan_state: PlanState::new(),
            resource_scope: ResourceScopeContract::default(),
            no_progress: NoProgressTracker::default(),
            last_reported_prompt_tokens: None,
            last_persisted_summary: None,
            persisted_summary_revision: 0,
            last_reported_history_estimate_tokens: None,
            task_resources: Vec::new(),
            active_tool: None,
            compact_tools_index: None,
            active_tool_detail: String::new(),
            carry_memories: false,
            context_window_tokens: 0,
            fetched_tools: BTreeSet::new(),
            compaction_model_config: None,
        }
    }

    pub fn active_plan_id(&self) -> Option<&str> {
        self.plan_state.active_id.as_deref()
    }

    pub fn active_plan_title(&self) -> Option<&str> {
        self.plan_state.active_title.as_deref()
    }

    pub fn resolve_plan(&mut self, instruction: &AgentInstruction) -> (String, String) {
        self.ensure_plan(instruction)
    }

    pub fn ensure_plan(&mut self, instruction: &AgentInstruction) -> (String, String) {
        let requested_id = instruction
            .plan_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let requested_title = instruction
            .plan_title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let is_new_plan = instruction.new_plan.unwrap_or(false);

        self.plan_state.resolve_plan_explicit(
            requested_id,
            requested_title,
            is_new_plan,
            &self.user_goal,
        )
    }

    pub fn set_task_resources(&mut self, resources: Vec<TaskResourceRef>) {
        self.task_resources = resources;
    }

    pub fn update_memory(&mut self, key: String, value: String) -> Result<(), &'static str> {
        crate::agent::context::memory::MemoryManager::is_safe_memory(&key, &value)?;
        crate::agent::context::memory::MemoryManager::check_capacity(
            self.memories.len(),
            &key,
            &self.memories,
        )?;
        self.memories.insert(key, value);
        Ok(())
    }

    pub fn update_memories(
        &mut self,
        updates: Vec<crate::agent::types::MemoryItem>,
    ) -> Vec<String> {
        let mut rejections = Vec::new();
        for item in updates {
            if let Err(reason) =
                crate::agent::context::memory::MemoryManager::is_safe_memory(&item.key, &item.value)
            {
                println!("⚠️ 记忆项写入被安全拦截: {} (key: '{}')", reason, item.key);
                rejections.push(format!("记忆项 '{}' 未保存: {}", item.key, reason));
                continue;
            }
            if let Err(reason) = crate::agent::context::memory::MemoryManager::check_capacity(
                self.memories.len(),
                &item.key,
                &self.memories,
            ) {
                println!("⚠️ 记忆项容量拦截: {} (key: '{}')", reason, item.key);
                rejections.push(format!("记忆项 '{}' 未保存: {}", item.key, reason));
                continue;
            }
            self.memories.insert(item.key, item.value);
        }
        rejections
    }

    pub fn record_failed_attempt(
        &mut self,
        step_id: usize,
        tool: String,
        action: String,
        error_summary: String,
        output_id: Option<String>,
    ) {
        self.failed_attempts.push(FailedAttempt {
            step_id,
            tool,
            action,
            error_summary,
            output_id,
        });
        if self.failed_attempts.len() > 6 {
            self.failed_attempts.remove(0);
        }
    }

    pub fn record_side_effect(&mut self, step_id: usize, tool: String, description: String) {
        self.side_effects.push(SideEffectRecord {
            step_id,
            tool,
            description,
            timestamp: chrono::Utc::now().to_rfc3339(),
        });
    }

    pub fn update_observation(&mut self, obs: String) {
        self.current_observation = obs;
    }

    /// 注入截图/图片反馈信息 (多模态)
    pub fn add_image_feedback(&mut self, text: &str, base64_image: &str) {
        let msg = ChatMessage {
            role: "user".to_string(),
            content: json!([
                { "type": "text", "text": text },
                {
                    "type": "image_url",
                    "image_url": {
                        "url": format!("data:image/jpeg;base64,{}", base64_image)
                    }
                }
            ]),
        };
        self.turns_history.push(msg);
    }

    /// 注入 AI 原始输出记录（用于保存失败时的错误输出，避免 AI 失忆死循环）
    pub fn add_assistant_raw_message(&mut self, text: &str) {
        let msg = ChatMessage {
            role: "assistant".to_string(),
            content: json!(text),
        };
        self.turns_history.push(msg);
    }

    /// 注入错误反馈信息，告知 AI 修正它的输出（带安全截断，防止海量报错或异常全量 JSON 打爆上下文）
    pub fn add_error_feedback(&mut self, error_msg: &str) {
        let trimmed_msg = if error_msg.chars().count() > 3000 {
            let head: String = error_msg.chars().take(1500).collect();
            let tail: String = error_msg
                .chars()
                .rev()
                .take(1000)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            format!("{}\n\n... [⚠️ 错误详情较长，已截断中间 {} 字符。请重点依据上述错误原因与最后输出进行修正] ...\n\n{}", head, error_msg.chars().count() - 2500, tail)
        } else {
            error_msg.to_string()
        };

        let msg = ChatMessage {
            role: "user".to_string(),
            content: json!(format!(
                "【❌ 格式或执行错误】\n{}\n\
                请根据上述错误反思原因并调整策略，严格按照 System Prompt 要求输出合法的纯 JSON 格式！", 
                trimmed_msg
            )),
        };
        self.turns_history.push(msg);
    }

    /// 检查历史流或当前会话中是否已注入过同一份工具/插件手册。
    ///
    /// MCP 服务目录只说明可用能力，不等同于任一子工具的 JSON Schema。
    /// 因此 `remote-http-mcp` 与 `remote-http-mcp/read_text_file` 必须按不同键去重：
    /// 前者可防止重复注入整个服务目录，后者则允许在首次调用失败时追加一次精确参数契约。
    /// 更新全局进度摘要；内容变化时把完整版作为 write-once 历史事件追加。
    /// 状态卡尾部此后只携带首行摘要+版本指针（见 assembler），实测该块
    /// 占状态卡 ~86% 体积且 18 轮仅变化 3 次，事件化收益显著。
    pub fn set_progress_summary(&mut self, summary: &str) {
        let trimmed = summary.trim();
        if trimmed.is_empty() {
            self.progress_summary = summary.to_string();
            return;
        }
        let unchanged = self.last_persisted_summary.as_deref() == Some(trimmed);
        if !unchanged {
            self.persisted_summary_revision = self.persisted_summary_revision.saturating_add(1);
            self.turns_history.push(ChatMessage {
                role: "user".to_string(),
                content: serde_json::json!(format!(
                    "【全局进度与策略摘要 v{}（write-once 历史事件，后续轮次以此为准）】\n{}",
                    self.persisted_summary_revision, trimmed
                )),
            });
            self.last_persisted_summary = Some(trimmed.to_string());
        }
        self.progress_summary = trimmed.to_string();
    }

    pub fn has_tool_manual(&self, tool_key: &str) -> bool {
        let clean_key = tool_key.trim();
        if clean_key.is_empty() {
            return false;
        }
        if self.fetched_tools.contains(clean_key) {
            return true;
        }
        let header = format!("【工具/Skill 操作手册: {}】", clean_key);
        self.turns_history.iter().any(|m| match &m.content {
            serde_json::Value::String(s) => s.contains(&header),
            _ => false,
        })
    }

    /// 从已有的历史消息流水中反向扫描并同步已加载的手册集合（防止会话续接或重跑时重复注入 20k 文档）
    pub fn sync_fetched_tools_from_history(&mut self) {
        for msg in &self.turns_history {
            if let serde_json::Value::String(s) = &msg.content {
                if s.contains("【工具/Skill 操作手册") {
                    if let Some(start) = s.find("【工具/Skill 操作手册") {
                        if let Some(colon_pos) = s[start..].find(": ") {
                            let after_colon = &s[start + colon_pos + 2..];
                            if let Some(end_bracket) = after_colon.find("】") {
                                let key = after_colon[..end_bracket].trim();
                                if !key.is_empty() {
                                    self.fetched_tools.insert(key.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_update_memory_blocks_sensitive_secrets() {
        let mut ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let _ = ctx.update_memory("api_key".to_string(), "sk-1234567890abcdef".to_string());
        assert!(
            !ctx.memories.contains_key("api_key"),
            "Sensitive API key must be blocked!"
        );

        let _ = ctx.update_memory("db_port".to_string(), "5432".to_string());
        assert_eq!(
            ctx.memories.get("db_port").map(|s| s.as_str()),
            Some("5432")
        );
    }

    #[test]
    fn test_tool_manual_deduplication_keeps_service_catalog_and_subtool_schema_distinct() {
        let mut ctx = SandwichContext::new("sys".to_string(), "goal".to_string());

        // 初始状态：未加载
        assert!(!ctx.has_tool_manual("remote-hub"));
        assert!(!ctx.has_tool_manual("remote-hub/list_repos"));
        assert!(!ctx.has_tool_manual("remote-hub/read_text_file"));

        // 服务级目录不应被误认为已提供某个具体子工具的参数 schema。
        ctx.fetched_tools.insert("remote-hub".to_string());
        assert!(ctx.has_tool_manual("remote-hub"));
        assert!(!ctx.has_tool_manual("remote-hub/list_repos"));
        assert!(!ctx.has_tool_manual("remote-hub/read_text_file"));

        // 精确子工具手册仅覆盖自身，仍允许其他子工具按需披露。
        ctx.fetched_tools
            .insert("remote-hub/read_text_file".to_string());
        assert!(ctx.has_tool_manual("remote-hub/read_text_file"));
        assert!(!ctx.has_tool_manual("remote-hub/shell_exec"));

        // 2. 模拟从数据库恢复（内存集合为空，但历史流中包含手册文本）
        let mut restored_ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        restored_ctx.turns_history.push(ChatMessage {
            role: "user".to_string(),
            content: json!("【工具/Skill 操作手册: remote-hub】\n60 个工具说明\n(已写入历史流)"),
        });

        // 服务级持久化手册只覆盖服务级目录，不覆盖任意子工具。
        assert!(restored_ctx.has_tool_manual("remote-hub"));
        assert!(!restored_ctx.has_tool_manual("remote-hub/read_text_file"));

        // 执行反向同步后，内存集合也保留精确键语义。
        restored_ctx.sync_fetched_tools_from_history();
        assert!(restored_ctx.fetched_tools.contains("remote-hub"));
        assert!(!restored_ctx.has_tool_manual("remote-hub/search_code"));
    }
}
