//! 计划树（PlanTree）与受控并行的分发执行：从 dispatcher.rs 抽出。
//!
//! run_plan_tree_sync 把模型的 parallel/plan_tree 指令转换为 PlanNode 树并
//! 同步执行；validate_controlled_parallel_batch 在执行前校验单批分支数
//! 不超过 MAX_CONTROLLED_PARALLEL_BRANCHES（响亮拒绝而非静默截断）。

use crate::agent::dispatcher::{emit_tool_event, execute_command_inner};
use crate::agent::mcp::PluginRegistry;
use crate::agent::plan_tree::PlanNode;
use crate::agent::types::DispatchResult;
use crate::db::DbState;
use serde_json::Value;
use std::sync::Arc;

// ═══════════════════════════════════════════════════════════════════════
// 常量
// ═══════════════════════════════════════════════════════════════════════

/// 受控并行的单批最大分支数（与系统提示词"单批最多两个分支"一致）。

pub fn run_plan_tree_sync(
    db: &DbState,
    session_id: &str,
    plan_node: &crate::agent::plan_tree::PlanNode,
    registry: &PluginRegistry,
) -> DispatchResult {
    // 硬拒绝第一道：任何深度出现超限/空并行组，整棵树拒绝执行，一个分支都不跑。
    if let Err(error) = validate_plan_tree_parallel_fanout(plan_node) {
        println!("🚫 [PlanTree: {}] 并行扇出越界，拒绝执行: {}", session_id, error);
        return DispatchResult {
            stdout: String::new(),
            stderr: error,
            success: false,
            route: "plan_tree_parallel_rejected".to_string(),
        };
    }

    let db_clone = db.clone();
    let sid = session_id.to_string();
    let reg_clone = registry.clone();

    let invoker: crate::agent::plan_tree::ToolInvoker = Arc::new(move |tool, args| {
        let db_c = db_clone.clone();
        let sid_c = sid.clone();
        let reg_c = reg_clone.clone();

        Box::pin(async move {
            let action = args
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // 修复: 并行分支在 tokio worker 线程上调用 invoker，而 execute_command_inner
            // 对 MCP 插件使用 handle.block_on，在 runtime 内嵌套 block_on 会直接 panic。
            // 用 spawn_blocking 把同步分发放到 blocking 线程执行，消除 runtime 嵌套。
            let res = tokio::task::spawn_blocking(move || {
                execute_command_inner(&db_c, &sid_c, &tool, &action, &args, &reg_c)
            })
            .await
            .map_err(|e| format!("工具执行任务崩溃: {}", e))?;
            if res.success {
                let parsed_val: serde_json::Value = serde_json::from_str(&res.stdout)
                    .unwrap_or_else(|_| serde_json::json!({ "output": res.stdout }));
                Ok(parsed_val)
            } else {
                Err(if !res.stderr.is_empty() {
                    res.stderr
                } else {
                    "工具执行返回失败".to_string()
                })
            }
        })
    });

    let report = futures::executor::block_on(crate::agent::plan_tree::execute_plan_tree(
        plan_node, invoker, None,
    ));

    let success = report.success;
    let feedback = report.to_llm_feedback();

    DispatchResult {
        stdout: feedback.clone(),
        stderr: if success { String::new() } else { feedback },
        success,
        route: if success {
            "plan_tree_success".to_string()
        } else {
            "plan_tree_failed".to_string()
        },
    }
}

pub(crate) const MAX_CONTROLLED_PARALLEL_BRANCHES: usize = 2;

/// 并行仅接受少量、已经由模型明确给出的独立分支。
/// 该校验不推断、补充或改写任何 MCP 子步骤参数。
pub(crate) fn validate_controlled_parallel_batch(
    parallel_steps: &[serde_json::Value],
) -> Result<(), String> {
    match parallel_steps.len() {
        0 => Err("parallel 必须至少包含一个已确定的独立分支；未知范围请使用单步取证。".to_string()),
        1..=MAX_CONTROLLED_PARALLEL_BRANCHES => Ok(()),
        actual => Err(format!(
            "parallel 单批最多允许 {} 个独立分支，当前为 {}。请先串行收缩范围与工具契约，再保留至多两个互不依赖、参数已确定的分支。宿主不会改写子步骤参数。",
            MAX_CONTROLLED_PARALLEL_BRANCHES, actual
        )),
    }
}

/// 执行前整树校验：plan_tree 指令不经过 validate_controlled_parallel_batch，
/// 模型可以在嵌套树的任意深度塞进超限并行组，因此必须在 spawn 前递归扫描。
pub(crate) fn validate_plan_tree_parallel_fanout(node: &PlanNode) -> Result<(), String> {
    match node {
        PlanNode::Seq(children) => {
            for child in children {
                validate_plan_tree_parallel_fanout(child)?;
            }
            Ok(())
        }
        PlanNode::Obj(obj) => {
            if let Some(ref parallel_children) = obj.parallel {
                match parallel_children.len() {
                    0 => {
                        return Err(format!(
                            "plan_tree 并行组 [{}] 为空：parallel 必须至少包含一个已确定的独立分支；未知范围请使用单步取证。",
                            obj.id.clone().unwrap_or_else(|| "?".to_string())
                        ))
                    }
                    1..=MAX_CONTROLLED_PARALLEL_BRANCHES => {}
                    actual => {
                        return Err(format!(
                            "plan_tree 并行组 [{}] 超过受控上限：单批最多允许 {} 个独立分支，当前为 {}。请将每组并行收缩到至多两个互不依赖、参数已确定的分支，或改为串行执行。宿主不会改写子步骤参数。",
                            obj.id.clone().unwrap_or_else(|| "?".to_string()),
                            MAX_CONTROLLED_PARALLEL_BRANCHES,
                            actual
                        ))
                    }
                }
                for child in parallel_children {
                    validate_plan_tree_parallel_fanout(child)?;
                }
            }
            Ok(())
        }
    }
}

// 根据指令执行具体动作

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn controlled_parallel_allows_at_most_two_explicit_branches_without_parameter_rewrite() {
        let one = vec![
            serde_json::json!({"tool": "remote-http-mcp/read_text_file", "command": {"path": "/repo/a.rs"}}),
        ];
        let two = vec![
            serde_json::json!({"tool": "remote-http-mcp/read_text_file", "command": {"cwd": "/repo", "path": "a.rs"}}),
            serde_json::json!({"tool": "remote-http-mcp/read_text_file", "command": {"cwd": "/repo", "path": "b.rs"}}),
        ];
        let three = vec![
            serde_json::json!({}),
            serde_json::json!({}),
            serde_json::json!({}),
        ];

        assert!(validate_controlled_parallel_batch(&one).is_ok());
        assert!(validate_controlled_parallel_batch(&two).is_ok());
        assert!(validate_controlled_parallel_batch(&three)
            .unwrap_err()
            .contains("最多允许 2"));
        assert_eq!(two[0]["command"]["cwd"], serde_json::json!("/repo"));
        assert_eq!(two[1]["command"]["path"], serde_json::json!("b.rs"));
    }

    #[test]
    fn plan_tree_fanout_walker_rejects_oversized_or_empty_parallel_group_at_any_depth() {
        let parse = |v: serde_json::Value| -> PlanNode { serde_json::from_value(v).unwrap() };

        // 合法：每组并行 ≤2，嵌套任意深度都放行
        let ok_tree = parse(json!([
            { "tool": "a", "args": {} },
            { "id": "g", "parallel": [
                { "tool": "b", "args": {} },
                [ { "tool": "c", "args": {} }, { "tool": "d", "args": {} } ]
            ]}
        ]));
        assert!(validate_plan_tree_parallel_fanout(&ok_tree).is_ok());

        // 越界：嵌套在串行链深处的并行组 3 分支，必须整树拒绝
        let bad_tree = parse(json!([
            { "tool": "a", "args": {} },
            { "id": "deep", "parallel": [
                { "tool": "b", "args": {} },
                [ { "id": "inner", "parallel": [
                    { "tool": "x", "args": {} },
                    { "tool": "y", "args": {} },
                    { "tool": "z", "args": {} }
                ]}]
            ]}
        ]));
        let err = validate_plan_tree_parallel_fanout(&bad_tree).unwrap_err();
        assert!(err.contains("最多允许 2"), "错误需指明上限: {}", err);
        assert!(err.contains("inner"), "错误需定位到越界组 id: {}", err);

        // 空并行组同样拒绝
        let empty_tree = parse(json!({ "id": "void", "parallel": [] }));
        assert!(validate_plan_tree_parallel_fanout(&empty_tree)
            .unwrap_err()
            .contains("至少包含一个"));
    }
}
