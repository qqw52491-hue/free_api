use crate::agent::plan_tree::timeout::default_timeout_for_tool;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// 树节点定义：支持 串行数组、并行组、原子叶子节点 任意递归嵌套
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PlanNode {
    /// 串行节点组：按顺序执行，Fail-Fast (一步失败立刻中断后续)
    Seq(Vec<PlanNode>),

    /// 并行或叶子节点对象
    Obj(PlanNodeObj),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanNodeObj {
    /// 步骤唯一标识 (如 "search_a", "step_1"；若无则自动分配 $0, $1)
    #[serde(default)]
    pub id: Option<String>,

    /// 并行分支：存在该字段时代表并行组 (allSettled 语义)
    #[serde(default)]
    pub parallel: Option<Vec<PlanNode>>,

    /// 工具名称 (如 "web_search", "fetch_url", "bash")
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub action: Option<String>,

    /// 工具参数 (支持 $id.field 或 $0[0].data 引用)
    #[serde(default)]
    pub args: Option<Value>,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub command: Option<Value>,

    /// 自定义超时秒数 (可选，覆盖默认值)
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,

    /// 描述说明
    #[serde(default)]
    pub description: Option<String>,

    /// 外卡字段捕获
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl PlanNodeObj {
    pub fn get_tool(&self) -> String {
        if let Some(ref t) = self.tool {
            return t.clone();
        }
        if let Some(ref a) = self.action {
            return a.clone();
        }
        "core".to_string()
    }

    pub fn get_args(&self) -> Result<Value, String> {
        if self.get_tool().eq_ignore_ascii_case("local_shell") {
            // args 优先；其余保持树节点 params 优先的既有语义。
            let (command, params) = if let Some(args) = self.args.as_ref() {
                (None, Some(args))
            } else if self.params.is_some() && !self.command.as_ref().is_some_and(Value::is_string) {
                (None, self.params.as_ref())
            } else { (self.command.as_ref(), self.params.as_ref()) };
            return crate::agent::tool_params::resolve_shell_call(command, params,
                self.extra.iter().map(|(key, value)| (key.as_str(), value)));
        }
        if let Some(ref args) = self.args {
            return Ok(args.clone());
        }
        if let Some(ref p) = self.params {
            return Ok(p.clone());
        }
        if let Some(ref c) = self.command {
            return Ok(c.clone());
        }
        Ok(Value::Object(serde_json::Map::new()))
    }

    pub fn get_timeout(&self) -> Duration {
        if let Some(ms) = self.timeout_ms {
            return Duration::from_millis(ms);
        }
        if let Some(secs) = self.timeout_secs {
            return Duration::from_secs(secs);
        }
        let default_secs = default_timeout_for_tool(&self.get_tool());
        Duration::from_secs(default_secs)
    }
}

/// 单个节点的执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub timed_out: bool,
    pub elapsed_ms: u64,
}

impl NodeResult {
    pub fn success(id: Option<String>, tool: Option<String>, data: Value, elapsed_ms: u64) -> Self {
        Self {
            ok: true,
            id,
            tool,
            data: Some(data),
            error: None,
            timed_out: false,
            elapsed_ms,
        }
    }

    pub fn failure(
        id: Option<String>,
        tool: Option<String>,
        error: String,
        timed_out: bool,
        elapsed_ms: u64,
    ) -> Self {
        Self {
            ok: false,
            id,
            tool,
            data: None,
            error: Some(error),
            timed_out,
            elapsed_ms,
        }
    }
}

/// 执行器函数签名：接收 (tool, resolved_args) -> Result<Value, String>
pub type ToolInvoker = Arc<
    dyn Fn(String, Value) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'static>>
        + Send
        + Sync,
>;

/// 完整树执行摘要报告
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanExecutionReport {
    pub success: bool,
    pub total_steps: usize,
    pub successful_steps: usize,
    pub failed_steps: usize,
    pub timed_out_steps: usize,
    pub elapsed_total_ms: u64,
    pub status: String, // "completed" | "interrupted_by_error"
    pub error_summary: Option<String>,
    pub results: Value,
}

impl PlanExecutionReport {
    pub fn to_llm_feedback(&self) -> String {
        let mut md = format!(
            "【计划执行结果报告】状态: {}\n总耗时: {}ms | 总步骤数: {} (成功: {}, 失败: {}, 超时: {})\n\n",
            if self.success { "全部成功" } else { "⚠️ 中断或部分失败" },
            self.elapsed_total_ms,
            self.total_steps,
            self.successful_steps,
            self.failed_steps,
            self.timed_out_steps
        );

        if let Some(ref err) = self.error_summary {
            md.push_str(&format!("⚠️ 错误原因: {}\n\n", err));
        }

        // 🌟 防爆窗保护：如果执行失败，对返回数据做脱水与瘦身处理，防止成功分支的超大 Payload（如 3.7w 的 git_diff）全量回灌上下文
        let sanitized_results = if !self.success {
            self.sanitize_failure_results()
        } else {
            self.results.clone()
        };

        md.push_str("详细返回数据 (JSON):\n```json\n");
        let json_str =
            serde_json::to_string_pretty(&sanitized_results).unwrap_or_else(|_| "{}".to_string());
        // 二次防御：单次报告结果若仍超出 3000 字符，执行安全截断
        if json_str.chars().count() > 3000 {
            let truncated: String = json_str.chars().take(2800).collect();
            md.push_str(&truncated);
            md.push_str("\n...（详细输出较长已自动截断，完整数据已安全持久化至 SQLite）");
        } else {
            md.push_str(&json_str);
        }
        md.push_str("\n```\n");
        md
    }

    /// 失败报告专用脱水器：成功分支仅保留工具名与状态，彻底剔除巨大数据体；失败分支保留核心错误
    fn sanitize_failure_results(&self) -> Value {
        if let Value::Array(ref arr) = self.results {
            let sanitized_arr: Vec<Value> = arr
                .iter()
                .map(|item| {
                    if let Value::Object(ref map) = item {
                        let mut new_map = serde_json::Map::new();
                        let is_ok = map.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);

                        for (k, v) in map {
                            if is_ok
                                && (k == "data" || k == "stdout" || k == "patch" || k == "output")
                            {
                                // 成功分支的重型数据直接替换为轻量标记
                                new_map.insert(
                                    k.clone(),
                                    serde_json::json!("(已执行成功，数据已落盘 SQLite)"),
                                );
                            } else if !is_ok && (k == "error" || k == "stderr") {
                                // 失败分支的重型错误栈替换为轻量标记，因为已经在 error_summary 中详细展示过
                                new_map.insert(
                                    k.clone(),
                                    serde_json::json!("(详细错误已在上方 error_summary 中展示，为节省上下文已截断)"),
                                );
                            } else if let Value::String(ref s) = v {
                                // 字符串过长时进行单字段截断
                                if s.chars().count() > 500 {
                                    let short_s: String = s.chars().take(480).collect();
                                    new_map.insert(
                                        k.clone(),
                                        serde_json::json!(format!("{}...（已截断）", short_s)),
                                    );
                                } else {
                                    new_map.insert(k.clone(), v.clone());
                                }
                            } else {
                                new_map.insert(k.clone(), v.clone());
                            }
                        }
                        Value::Object(new_map)
                    } else {
                        item.clone()
                    }
                })
                .collect();
            Value::Array(sanitized_arr)
        } else {
            self.results.clone()
        }
    }
}

#[cfg(test)]
mod shell_params_tests {
    use super::*; use serde_json::json;
    #[test]
    fn shell_params_tree_keeps_args_precedence_and_exposes_conflicts() {
        let node:PlanNodeObj=serde_json::from_value(json!({"tool":"local_shell","args":{"command":"pwd"},"params":{"command":"touch x"}})).unwrap();
        assert_eq!(node.get_args().unwrap(),json!({"command":"pwd"}));
        let node:PlanNodeObj=serde_json::from_value(json!({"tool":"local_shell","command":"pwd","params":{"command":"touch x"}})).unwrap();
        assert!(node.get_args().unwrap_err().contains("参数冲突"));
    }
}
