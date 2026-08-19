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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: usize,
    pub status: String, // "pending" | "in_progress" | "done" | "failed"
    #[serde(default)]
    pub description: String,
    /// 完成该 Todo 所依据的执行产物 output_id 证据（防虚假勾选）
    #[serde(default)]
    pub evidence_output_id: Option<String>,
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
    pub cache_hit_rate: f64, // cached_tokens / prompt_tokens * 100
    pub context_window: i64, // 配置的上下文窗口大小 (num_ctx)
    pub usage_percent: f64,  // total_tokens / context_window * 100
    pub ttft_ms: u64,        // 首字延迟 (Time to First Token)
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

#[derive(Debug)]
pub struct DispatchResult {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    pub route: String,
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
}
