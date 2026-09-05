use crate::agent::change_verification::ChangeVerification;
use crate::agent::context::no_progress::NoProgressTracker;
use crate::agent::context::resource_scope::ResourceScopeContract;
use crate::agent::context::zero_hit::ZeroHitTracker;
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

/// 重复读取台账条目：同工具+同路径的内容指纹与读取次数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadLedgerEntry {
    pub content_hash: String,
    pub count: u32,
    pub sample: String,
    pub line_count: usize,
}

/// 方案门禁状态投影：由 checkpoint 每轮规划前生成，注入底部状态卡，
/// 让模型在「决策前可见」当前执行模式与放行条件，避免靠写入被拒学习门禁规则。
/// 纯瞬态投影：不入持久化捕获，每轮从 checkpoint 事实状态重算。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct PlanGateProjection {
    /// 执行模式："auto" | "cross_review" | "plan_approval"
    pub mode: String,
    /// 方案阶段："exploring" | "reviewing" | "awaiting_approval" | "approved"
    pub phase: String,
    /// 是否已派发过方案审查子 Agent
    pub reviewer_dispatched: bool,
    /// 审查结论是否已消费（注入过主 Agent 上下文）
    pub reviewer_consumed: bool,
    /// 批准前审查预算：已用 / 上限
    pub reviewer_budget_used: u32,
    pub reviewer_budget_max: u32,
}

/// 失败教训记录（用于防止大模型在长任务中重复尝试已知失败的方法）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FailedAttempt {
    pub step_id: usize,
    pub tool: String,
    pub action: String,
    /// 规范化参数的稳定指纹；旧 checkpoint 缺失时保持空串以兼容恢复。
    #[serde(default)]
    pub params_fingerprint: String,
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

pub fn clip_plan_title(raw: &str) -> String {
    let first_line = raw.lines().next().unwrap_or("").trim();
    let normalized = first_line.split_whitespace().collect::<Vec<_>>().join(" ");
    let candidate = if normalized.is_empty() {
        "执行任务"
    } else {
        normalized.as_str()
    };
    let clipped: String = candidate.chars().take(40).collect();
    if candidate.chars().count() > 40 {
        format!("{}…", clipped)
    } else {
        clipped
    }
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
        let default_clean = clip_plan_title(default_title);
        let requested_title = requested_title.map(|t| clip_plan_title(&t));

        // 1. 如果模型明确指定了 ID
        if let Some(id) = requested_id {
            let title = requested_title
                .or_else(|| self.active_title.clone())
                .unwrap_or(default_clean);
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
                .unwrap_or(default_clean);
            self.active_id = Some(id.clone());
            self.active_title = Some(title.clone());
            return (id, title);
        }

        // 3. 如果已有活跃阶段，且未显式声明 new_plan，则保持当前 ID，仅平滑更新标题（防止措辞微调导致 Episode 碎片化）
        if let Some(existing_id) = self.active_id.clone() {
            let title = requested_title
                .or_else(|| self.active_title.clone())
                .unwrap_or(default_clean);
            self.active_title = Some(title.clone());
            return (existing_id, title);
        }

        // 4. 初次启动自动分配 plan-1
        self.next_index = self.next_index.saturating_add(1);
        let id = format!("plan-{}", self.next_index);
        let title = requested_title.unwrap_or(default_clean);
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
    /// shell 批改探针：CallLlm+RunTool 阶段开始前的 git 脏文件数快照（None=非 git 或未测）。
    /// 主循环在步骤结束后取差值入账，使 shell_exec 批量改文件无法绕过阶段审查的文件数阈值。
    #[serde(default)]
    pub shell_probe_pre_dirty: Option<u32>,
    /// 本次运行从之前任务继承的副作用数量（用于区分本轮是否有新增副作用）
    #[serde(default)]
    pub inherited_side_effects_count: usize,
    /// 当前任务待办清单 (Todo items)
    pub todo_list: Vec<TodoItem>,
    /// 复杂审查、诊断或研究任务的可审计完成契约；普通任务保持 None。
    #[serde(default)]
    pub review_contract: Option<ReviewContract>,
    /// 目标块状态机与管理器（粗粒度任务单元）
    #[serde(default)]
    pub block_manager: crate::agent::target_block::BlockManager,
    /// 最近一批真实写入的验证状态；每次新的成功写入都会自动清空。
    #[serde(default)]
    pub change_verification: Option<ChangeVerification>,
    /// 全局阶段进度与策略宏观摘要 (~100 tokens，每轮更新)
    pub progress_summary: String,
    /// 活跃资源作用域契约；独立于历史压缩，持续投影到每轮上下文。
    #[serde(default)]
    pub resource_scope: ResourceScopeContract,
    /// 项目受管文件清单动态投影（放置于底层面包，保障 M0 顶层前缀绝对不变）
    #[serde(default)]
    pub project_resource_manifest: Option<String>,
    /// 当前纪元（Epoch）锁定的 M0 顶层系统提示词基线（保住跨轮次长前缀缓存）
    #[serde(default)]
    pub epoch_system_prompt: Option<String>,
    /// 无进展重复检测：相同工具+相同参数+相同输出/失败的运行时事实（策略级提示用）。
    #[serde(default)]
    pub no_progress: NoProgressTracker,
    /// 受限检索偏航提示：已声明检索工具族在同一作用域连续结构化零命中
    /// 的运行时事实（换措辞盲搜的空洞检测，每组只提示一次）。
    #[serde(default)]
    pub zero_hit: ZeroHitTracker,
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
    /// 结构化路由信号：最近一次工具步骤是否失败（宿主按 DispatchResult 写入，
    /// 替代对反馈文本里 ❌ 字符串的脆弱匹配）。
    #[serde(default)]
    pub last_step_failed: bool,
    /// 结构化路由信号：连续失败步数（成功步清零）。供路由的难度打分与升维决策使用。
    #[serde(default)]
    pub consecutive_failures: u32,
    /// 重复读取台账：key = "工具|路径"，记录内容哈希/次数/内容摘要。
    /// 上下文压缩发生时清零（压缩后模型遗忘旧内容，重读重新合法）。
    #[serde(default)]
    pub read_ledger: std::collections::BTreeMap<String, ReadLedgerEntry>,
    /// P4 连续批发派发解析失败计数（达 2 次触发 3 步熔断降级）
    #[serde(default)]
    pub consecutive_batch_dispatch_errors: usize,
    /// P4 批发派发熔断保护期剩余步数（>0 时阻止 dispatches 并注入降级单发指引）
    #[serde(default)]
    pub batch_dispatch_circuit_breaker_remaining: usize,
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
    /// 上下文预算与压缩策略配置
    #[serde(default)]
    pub budget_config: crate::agent::context::budget::ContextBudgetConfig,
    /// 压缩引擎专用 LLM 配置（运行时注入，不参与 checkpoint 序列化）
    #[serde(skip)]
    pub compaction_model_config: Option<ModelConfig>,
    /// 方案门禁状态投影（每轮由 checkpoint 重算注入，瞬态，不序列化）
    #[serde(skip)]
    pub plan_gate: Option<PlanGateProjection>,
}

impl SandwichContext {
    pub fn new(system_prompt: String, user_goal: String) -> Self {
        Self {
            system_prompt,
            user_goal,
            memories: BTreeMap::new(),
            failed_attempts: Vec::new(),
            side_effects: Vec::new(),
            shell_probe_pre_dirty: None,
            inherited_side_effects_count: 0,
            todo_list: Vec::new(),
            review_contract: None,
            block_manager: crate::agent::target_block::BlockManager::new(),
            change_verification: None,
            turns_history: Vec::new(),
            current_observation: String::new(),
            progress_summary: String::new(),
            plan_state: PlanState::new(),
            resource_scope: ResourceScopeContract::default(),
            project_resource_manifest: None,
            epoch_system_prompt: None,
            no_progress: NoProgressTracker::default(),
            zero_hit: ZeroHitTracker::default(),
            last_reported_prompt_tokens: None,
            last_persisted_summary: None,
            persisted_summary_revision: 0,
            last_reported_history_estimate_tokens: None,
            task_resources: Vec::new(),
            last_step_failed: false,
            consecutive_failures: 0,
            read_ledger: std::collections::BTreeMap::new(),
            consecutive_batch_dispatch_errors: 0,
            batch_dispatch_circuit_breaker_remaining: 0,
            active_tool: None,
            compact_tools_index: None,
            active_tool_detail: String::new(),
            carry_memories: false,
            context_window_tokens: 0,
            fetched_tools: BTreeSet::new(),
            budget_config: crate::agent::context::budget::ContextBudgetConfig::default(),
            compaction_model_config: None,
            plan_gate: None,
        }
    }

    /// 宿主在每个工具步骤收尾后调用：结构化记录成功/失败，供路由难度打分使用。
    pub fn record_step_outcome(&mut self, success: bool) {
        self.last_step_failed = !success;
        if success {
            self.consecutive_failures = 0;
        } else {
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        }
    }

    /// P4 连续批发解析失败降级熔断逻辑：
    /// 连续 2 次批发解析失败 → 触发 3 步熔断期（期间阻止 dispatches 并注入降级单发指引）
    pub fn record_batch_dispatch_outcome(&mut self, success: bool) -> Option<String> {
        if success {
            self.consecutive_batch_dispatch_errors = 0;
            None
        } else {
            self.consecutive_batch_dispatch_errors += 1;
            if self.consecutive_batch_dispatch_errors >= 2 {
                self.batch_dispatch_circuit_breaker_remaining = 3;
                self.consecutive_batch_dispatch_errors = 0;
                Some("⚠️ 批发格式近期已连续 2 次解析失败，触发熔断降级！后续 3 步内请使用单工具派发（params: {...}），之后再恢复批发。".to_string())
            } else {
                None
            }
        }
    }

    /// 获取当前活跃的目标块
    pub fn active_target_block(&self) -> Option<&crate::agent::target_block::TargetBlock> {
        self.block_manager.active_block()
    }

    /// 获取当前活跃的目标块可变引用
    pub fn active_target_block_mut(&mut self) -> Option<&mut crate::agent::target_block::TargetBlock> {
        self.block_manager.active_block_mut()
    }

    /// 检查当前步骤是否允许调用批发派发（dispatches）
    pub fn check_batch_dispatch_allowed(&mut self, has_dispatches: bool) -> Result<(), String> {
        if has_dispatches && self.batch_dispatch_circuit_breaker_remaining > 0 {
            let remaining = self.batch_dispatch_circuit_breaker_remaining;
            self.batch_dispatch_circuit_breaker_remaining = remaining.saturating_sub(1);
            Err(format!(
                "⚠️ 批发格式近期连续解析失败触发熔断保护期（还剩 {} 步恢复）。本轮请直接使用单工具派发（params: {{...}}），不可使用 dispatches 批发。",
                remaining
            ))
        } else {
            Ok(())
        }
    }

    /// 重复读取硬拦截（四条件签名）：同工具+同路径第 3 次读取且内容未变 → 拒绝。
    /// 返回 Some(拒绝消息) 时宿主不得执行该读取；None = 放行并记账。
    /// 合法重读自动放行：文件内容变了（改后验证）、首次/前两次读取。
    pub fn check_repeat_read(
        &mut self,
        tool: &str,
        path: &str,
        content: &str,
    ) -> Option<String> {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        let hash = format!("{:x}", hasher.finalize());
        let key = format!("{}|{}", tool.to_lowercase(), path);
        let line_count = content.lines().count();
        let entry = self.read_ledger.entry(key).or_insert(ReadLedgerEntry {
            content_hash: hash.clone(),
            count: 0,
            sample: content.chars().take(200).collect(),
            line_count,
        });
        if entry.content_hash != hash {
            // 内容已变化（改后重读合法）：重置指纹
            *entry = ReadLedgerEntry {
                content_hash: hash,
                count: 1,
                sample: content.chars().take(200).collect(),
                line_count,
            };
            return None;
        }
        entry.count = entry.count.saturating_add(1);
        if entry.count >= 3 {
            let sample = entry.sample.clone();
            let lines = entry.line_count;
            let times = entry.count;
            // 正常累加计数（不再使用哨兵值：报出"已读取 2147483647 次"既荒谬又无信息量）。
            // 内容变化时上方已重置指纹，因此 >=3 后持续拦截，直至文件真正被修改。
            return Some(format!(
                "【重复读取拦截】该文件你已连续读取 {} 次且内容未变化（哈希一致，共 {} 行）。上次读取的内容开头：\n{}\n内容仍在你此前的上下文里；要定位片段请用 grep/搜索工具按关键词提取，或先修改文件后再读取。严禁原样重读。",
                times, lines, sample
            ));
        }
        None
    }

    /// 上下文压缩后调用：模型已遗忘旧读取内容，台账清零使重读重新合法。
    pub fn reset_read_ledger(&mut self) {
        self.read_ledger.clear();
    }

    /// 派发层入口：识别读取型指令，提取路径，宿主读文件后执行四条件判定。
    /// 非读取型工具 / 路径不可解析 / 文件不可读 → None（放行，让工具自己报错）。
    pub fn check_repeat_read_dispatch(
        &mut self,
        instruction: &crate::agent::types::AgentInstruction,
        workspace_dir: &std::path::Path,
    ) -> Option<String> {
        const READ_TOOLS: &[&str] = &[
            "read_text_file", "read_file", "get_file_content", "get_code_snippet",
        ];
        let tool = instruction.get_tool().to_lowercase();
        if !READ_TOOLS.iter().any(|t| tool.contains(t)) {
            return None;
        }
        let params = instruction.get_params();
        let raw_path = params
            .get("path")
            .or_else(|| params.get("filePath"))
            .or_else(|| params.get("file_path"))
            .and_then(|v| v.as_str())?;
        if raw_path.trim().is_empty() || raw_path.chars().count() > 1_000 {
            return None;
        }
        let candidate = std::path::PathBuf::from(raw_path.trim());
        let target = if candidate.is_relative() {
            workspace_dir.join(candidate)
        } else {
            candidate
        };
        let Ok(canonical) = target.canonicalize() else {
            return None;
        };
        if !canonical.is_file() {
            return None;
        }
        // 上限保护：超大文件只取头部指纹段参与哈希，避免整读大文件
        let Ok(metadata) = std::fs::metadata(&canonical) else { return None; };
        let content = if metadata.len() > 1_000_000 {
            use std::io::Read;
            let mut file = std::fs::File::open(&canonical).ok()?;
            let mut bytes = vec![0u8; 1_000_000];
            let read_len = file.read(&mut bytes).ok()?;
            bytes.truncate(read_len);
            String::from_utf8_lossy(&bytes).to_string()
        } else {
            std::fs::read_to_string(&canonical).ok()?
        };
        let ledger_path = canonical.to_string_lossy().to_string();
        self.check_repeat_read(&tool, &ledger_path, &content)
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

    fn failed_attempt_params_fingerprint(params: &serde_json::Value) -> String {
        crate::agent::context::determinism::sha256_hex(&params.to_string())
    }

    fn is_deterministic_tool_failure(error: &str) -> bool {
        let normalized = error.to_ascii_lowercase();
        let transient = [
            "429",
            "rate limit",
            "timeout",
            "timed out",
            "network",
            "connection",
            "temporarily",
        ];
        if transient.iter().any(|needle| normalized.contains(needle)) {
            return false;
        }
        [
            "hub 参数契约",
            "未声明参数",
            "unknown action",
            "未知的",
            "required",
            "缺少",
            "enoent",
            "no such file or directory",
            "not a directory",
            "host security rejection",
            "relation: outside",
        ]
        .iter()
        .any(|needle| normalized.contains(needle))
    }

    /// 若同一工具、动作和参数已经出现确定性失败，则拒绝再次派发。
    /// 网络/限流等暂态失败不在此处阻断，仍由既有恢复策略处理。
    pub fn repeated_failure_blocker(
        &self,
        tool: &str,
        action: &str,
        params: &serde_json::Value,
    ) -> Option<String> {
        let params_fingerprint = Self::failed_attempt_params_fingerprint(params);
        self.failed_attempts
            .iter()
            .rev()
            .find(|attempt| {
                attempt.tool.eq_ignore_ascii_case(tool)
                    && attempt.action.eq_ignore_ascii_case(action)
                    && attempt.params_fingerprint == params_fingerprint
                    && Self::is_deterministic_tool_failure(&attempt.error_summary)
            })
            .map(|attempt| {
                let output_ref = attempt
                    .output_id
                    .as_deref()
                    .map(|id| format!(" 详情见 output_id={}。", id))
                    .unwrap_or_default();
                format!(
                    "【重复确定性失败拦截】{} / {} 已以相同参数失败（Step {}）：{}。禁止再次派发同一命令；必须更换工具、动作或参数，必要时 ask_human。{}",
                    tool, action, attempt.step_id, attempt.error_summary, output_ref
                )
            })
    }

    pub fn record_failed_attempt(
        &mut self,
        step_id: usize,
        tool: String,
        action: String,
        params: &serde_json::Value,
        error_summary: String,
        output_id: Option<String>,
    ) {
        let params_fingerprint = Self::failed_attempt_params_fingerprint(params);
        let short_error: String = error_summary.chars().take(160).collect();
        if let Some(existing) = self.failed_attempts.iter_mut().rev().find(|attempt| {
            attempt.step_id == step_id
                && attempt.tool.eq_ignore_ascii_case(&tool)
                && attempt.action.eq_ignore_ascii_case(&action)
                && attempt.params_fingerprint == params_fingerprint
        }) {
            existing.error_summary = short_error;
            if output_id.is_some() {
                existing.output_id = output_id;
            }
            return;
        }
        self.failed_attempts.push(FailedAttempt {
            step_id,
            tool,
            action,
            params_fingerprint,
            error_summary: short_error,
            output_id,
        });
        if self.failed_attempts.len() > 6 {
            self.failed_attempts.remove(0);
        }
    }

    pub fn record_side_effect(&mut self, step_id: usize, tool: String, description: String) {
        // 旧验证只能覆盖其前已知的写入；新的真实写入必须重新验证或豁免。
        self.change_verification = None;
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
        let url = if base64_image.starts_with("data:") {
            base64_image.to_string()
        } else {
            format!("data:image/jpeg;base64,{}", base64_image)
        };
        let msg = ChatMessage {
            role: "user".to_string(),
            content: json!([
                { "type": "text", "text": text },
                {
                    "type": "image_url",
                    "image_url": {
                        "url": url
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
    #[test]
    fn repeated_deterministic_failure_blocks_same_parameters_but_not_transient_or_changed_calls() {
        let mut ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let params = json!({"path": "/missing/project"});
        ctx.record_failed_attempt(
            1,
            "remote-hub/list_directory".to_string(),
            "list_directory".to_string(),
            &params,
            "ENOENT: no such file or directory".to_string(),
            Some("out-1".to_string()),
        );
        assert!(ctx
            .repeated_failure_blocker("remote-hub/list_directory", "list_directory", &params)
            .is_some());
        assert!(ctx
            .repeated_failure_blocker(
                "remote-hub/list_directory",
                "list_directory",
                &json!({"path": "/different/project"}),
            )
            .is_none());

        let transient = json!({"query": "retry me"});
        ctx.record_failed_attempt(
            2,
            "web_search".to_string(),
            "search".to_string(),
            &transient,
            "HTTP 429 rate limit".to_string(),
            None,
        );
        assert!(ctx
            .repeated_failure_blocker("web_search", "search", &transient)
            .is_none());
    }

    #[test]
    fn record_side_effect_invalidates_prior_change_verification() {
        let mut ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        ctx.record_side_effect(1, "edit_file".to_string(), "首次写入".to_string());
        ctx.change_verification = Some(ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec!["output_id: check-first".to_string()],
            waived_reason: None,
        });
        ctx.record_side_effect(2, "edit_file".to_string(), "后续写入".to_string());
        assert!(ctx.change_verification.is_none());
        assert_eq!(ctx.side_effects.len(), 2);
    }
}

#[cfg(test)]
mod repeat_read_tests {
    use super::*;

    fn ctx() -> SandwichContext {
        SandwichContext::new("system".to_string(), "目标".to_string())
    }

    #[test]
    fn repeat_read_allows_first_two_blocks_third_and_resets_on_change() {
        let mut context = ctx();
        let content = "line1\nline2\nline3";
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", content).is_none(), "第 1 次放行");
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", content).is_none(), "第 2 次放行");
        let blocked = context.check_repeat_read("read_text_file", "/ws/a.rs", content);
        assert!(blocked.is_some(), "第 3 次同内容必须拦截");
        assert!(blocked.unwrap().contains("已连续读取"), "拦截消息要给抓手");

        // 内容变化（改后重读）→ 放行并重置
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", "changed").is_none());

        // 重置后计数从 1 起：再连续两次放行，第 3 次同内容又拦（count ≥ 3）
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", "changed").is_none(), "重置后第 2 次放行");
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", "changed").is_some(), "第 3 次同内容再拦");
    }

    #[test]
    fn ledger_keys_are_per_path_and_reset_clears() {
        let mut context = ctx();
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", "A").is_none());
        assert!(context.check_repeat_read("read_text_file", "/ws/b.rs", "B").is_none(), "不同路径互不干扰");
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", "A").is_none());
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", "A").is_some());
        context.reset_read_ledger();
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", "A").is_none(), "清零后重读合法");
    }

    #[test]
    fn test_batch_dispatch_circuit_breaker_trips_and_recovers() {
        let mut context = ctx();
        // 1. 首次失败：不触发熔断
        assert!(context.record_batch_dispatch_outcome(false).is_none());
        assert!(context.check_batch_dispatch_allowed(true).is_ok());

        // 2. 连续第 2 次失败：触发 3 步熔断
        let alert = context.record_batch_dispatch_outcome(false);
        assert!(alert.is_some());
        assert!(alert.unwrap().contains("触发熔断降级"));

        // 3. 熔断期间：阻止批发派发，连续 3 步倒数
        let err1 = context.check_batch_dispatch_allowed(true).unwrap_err();
        assert!(err1.contains("还剩 3 步"));
        let err2 = context.check_batch_dispatch_allowed(true).unwrap_err();
        assert!(err2.contains("还剩 2 步"));
        let err3 = context.check_batch_dispatch_allowed(true).unwrap_err();
        assert!(err3.contains("还剩 1 步"));

        // 4. 熔断保护期结束：恢复允许批发
        assert!(context.check_batch_dispatch_allowed(true).is_ok());
    }
}
