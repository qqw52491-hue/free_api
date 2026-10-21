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

        // Return the complete execution result. The caller persists it before the
        // shared SandwichContext ingress sampler creates a bounded model view.
        md.push_str("详细返回数据 (JSON):\n```json\n");
        let json_str = serde_json::to_string_pretty(&self.results)
            .unwrap_or_else(|_| self.results.to_string());
        md.push_str(&json_str);
        md.push_str("\n```\n");
        md
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
