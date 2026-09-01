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
    /// 【预加载优化】AI 预告下一轮想用的工具，系统会提前加载该工具的详细说明书
    #[serde(default)]
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
    pub fn validate_executable_shape(&self) -> Result<(), String> {
        if self.parallel.is_some() || self.plan_tree.is_some() || !self.commands.is_empty() || self.has_dispatches() {
            return Ok(());
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

        if has_tool || has_legacy_action {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_summary_and_detail_are_normalized_without_rejecting_instruction() {
        let instruction: AgentInstruction = serde_json::from_value(json!({
            "tool": "finish",
            "command": { "summary": "done" },
            "memories_update": [{
                "key": "workbook",
                "summary": "工作簿已创建",
                "detail": "路径: /tmp/report.xlsx"
            }],
            "clear_history": true
        }))
        .expect("legacy memory fields should remain parseable");

        assert_eq!(instruction.get_action(), "finish");
        assert_eq!(instruction.memories_update.len(), 1);
        assert_eq!(instruction.memories_update[0].key, "workbook");
        assert_eq!(
            instruction.memories_update[0].value,
            "工作簿已创建\n路径: /tmp/report.xlsx"
        );
    }

    #[test]
    fn canonical_value_takes_precedence_over_legacy_fields() {
        let memory: MemoryItem = serde_json::from_value(json!({
            "key": "goal",
            "value": "canonical",
            "summary": "ignored",
            "detail": "ignored"
        }))
        .expect("canonical memory should parse");
        assert_eq!(memory.value, "canonical");
    }

    #[test]
    fn executable_shape_rejects_state_only_instruction() {
        let instruction: AgentInstruction = serde_json::from_value(json!({
            "plan_title": "核验完成语义",
            "description": "下一步应读取主循环",
            "progress_summary": "已建立审查义务"
        }))
        .expect("state-only JSON remains deserializable for recoverable validation");

        let error = instruction
            .validate_executable_shape()
            .expect_err("state-only object must not execute as implicit core");
        assert!(error.contains("缺少可执行入口"));
    }

    #[test]
    fn executable_shape_keeps_existing_action_forms_compatible() {
        let tool: AgentInstruction = serde_json::from_value(json!({
            "tool": "read_text_file",
            "command": {"path": "src/lib.rs"}
        }))
        .expect("tool instruction should deserialize");
        assert!(tool.validate_executable_shape().is_ok());

        let legacy_action: AgentInstruction = serde_json::from_value(json!({
            "action": "finish",
            "command": {"summary": "done"}
        }))
        .expect("legacy action instruction should deserialize");
        assert!(legacy_action.validate_executable_shape().is_ok());

        let pipeline: AgentInstruction = serde_json::from_value(json!({
            "commands": [{"tool": "read_text_file", "command": {"path": "src/lib.rs"}}]
        }))
        .expect("pipeline instruction should deserialize");
        assert!(pipeline.validate_executable_shape().is_ok());

        let plan_tree: AgentInstruction = serde_json::from_value(json!({
            "plan_tree": {"mode": "serial", "children": []}
        }))
        .expect("plan tree instruction should deserialize");
        assert!(plan_tree.validate_executable_shape().is_ok());
    }

    #[test]
    fn test_dispatch_context_parsing_and_validation() {
        // 空值或 null
        assert!(parse_dispatch_context(None).unwrap().is_none());
        assert!(parse_dispatch_context(Some(&serde_json::Value::Null)).unwrap().is_none());
        assert!(parse_dispatch_context(Some(&json!({}))).unwrap().is_none());

        // 非对象被拒绝
        assert!(parse_dispatch_context(Some(&json!("string"))).is_err());

        // 正常结构化解析
        let val = json!({
            "background": "项目使用 Tauri + Vue3",
            "todo_slice": ["修复单测", "格式化代码"],
            "file_hints": ["src-tauri/src/agent/child_writer.rs"],
            "prior_findings": ["命令超时已修复"]
        });
        let parsed = parse_dispatch_context(Some(&val)).unwrap().expect("should parse successfully");
        assert_eq!(parsed.background.as_deref(), Some("项目使用 Tauri + Vue3"));
        assert_eq!(parsed.todo_slice.len(), 2);
        assert_eq!(parsed.file_hints.len(), 1);
        assert_eq!(parsed.prior_findings.len(), 1);
    }

    #[test]
    fn test_is_task_thin_heuristics() {
        // 明显的过薄任务（无换行、无路径、无命令，短文本）
        assert!(is_task_thin("修复那个 bug"));
        assert!(is_task_thin("优化一下代码"));
        assert!(is_task_thin(""));

        // 包含文件特征，不属于薄任务
        assert!(!is_task_thin("修复 index.ts 的拼写错误"));
        assert!(!is_task_thin("检查 src/lib.rs"));
        assert!(!is_task_thin("修改 package.json 的版本号"));

        // 包含命令特征，不属于薄任务
        assert!(!is_task_thin("执行 npm test 解决报错"));
        assert!(!is_task_thin("运行 cargo test"));

        // 较长且自包含的指令，不属于薄任务
        assert!(!is_task_thin("请重构用户认证模块的持久化逻辑，增加登录重试机制与失败状态回滚"));

        // 包含换行多段落的指令，不属于薄任务
        assert!(!is_task_thin("任务说明：\n1. 检查配置\n2. 补充日志"));
    }

    #[test]
    fn test_agent_instruction_dispatches_helpers() {
        // 1. 单发（未提供 dispatches）：has_dispatches 为 false，get_dispatch_items 返回 params
        let single = json!({
            "tool": "child_worker",
            "params": { "task": "单人任务" }
        });
        let inst: AgentInstruction = serde_json::from_value(single).unwrap();
        assert!(!inst.has_dispatches());
        assert!(!inst.is_mixed_dispatch_format());
        let items = inst.get_dispatch_items();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["task"], "单人任务");

        // 2. 纯批发（dispatches 数组）：has_dispatches 为 true
        let batch = json!({
            "tool": "child_worker",
            "dispatches": [
                { "task": "任务A" },
                { "task": "任务B" }
            ]
        });
        let inst_b: AgentInstruction = serde_json::from_value(batch).unwrap();
        assert!(inst_b.has_dispatches());
        assert!(!inst_b.is_mixed_dispatch_format());
        assert_eq!(inst_b.get_dispatch_items().len(), 2);

        // 3. 混合格式：既有 dispatches 又有 params
        let mixed = json!({
            "tool": "child_worker",
            "params": { "task": "混淆任务" },
            "dispatches": [
                { "task": "任务1" }
            ]
        });
        let inst_m: AgentInstruction = serde_json::from_value(mixed).unwrap();
        assert!(inst_m.has_dispatches());
        assert!(inst_m.is_mixed_dispatch_format());
    }
}

