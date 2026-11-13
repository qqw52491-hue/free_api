use super::{invoke_read_tool, ReviewProgram};
use crate::agent::mcp::PluginRegistry;
use crate::agent::{
    plan_tree::{NodeResult, PlanExecutionReport, PlanNodeObj},
    plan_tree_dispatch::validate_controlled_parallel_batch,
    types::DispatchResult,
};
use crate::db::DbState;
use serde_json::{json, Value};
use std::time::Instant;
use tokio::sync::watch;

pub(super) async fn run(
    db: &DbState,
    id: &str,
    program: &ReviewProgram,
    branches: &[Value],
    registry: &tokio::sync::Mutex<PluginRegistry>,
    interrupt: &watch::Receiver<u64>,
) -> DispatchResult {
    if let Err(stderr) = validate_controlled_parallel_batch(branches) {
        return DispatchResult {
            stdout: String::new(),
            stderr,
            success: false,
            route: "review:tool_rejected".into(),
            view: None,
        };
    }

    let start = Instant::now();
    let results = futures::future::join_all(branches.iter().enumerate().map(
        |(index, branch)| async move {
            let started = Instant::now();
            let mut branch_id = format!("${index}");
            let mut name = "parallel".to_owned();
            let result: Result<String, String> = async {
                let node: PlanNodeObj = serde_json::from_value(branch.clone())
                    .map_err(|error| format!("并行取证分支应为单个工具调用对象：{error}"))?;
                name = node.get_tool();
                if let Some(explicit_id) = &node.id {
                    branch_id.clone_from(explicit_id);
                }
                if node.parallel.is_some() || matches!(name.as_str(), "verify" | "finish") {
                    return Err("并行取证分支只接受单个取证调用；复验和提交结果需单独调用。".into());
                }
                let params = node.get_args()?;
                invoke_read_tool(
                    db,
                    id,
                    program,
                    registry,
                    &name,
                    node.action.as_deref().unwrap_or(""),
                    params,
                    interrupt,
                )
                .await
            }
            .await;
            let elapsed = started.elapsed().as_millis() as u64;
            match result {
                Ok(output) => NodeResult::success(
                    Some(branch_id),
                    Some(name),
                    json!({"output": output}),
                    elapsed,
                ),
                Err(error) => {
                    NodeResult::failure(Some(branch_id), Some(name), error, false, elapsed)
                }
            }
        },
    ))
    .await;
    let successful_steps = results.iter().filter(|node| node.ok).count();
    let failed_steps = results.len() - successful_steps;
    let stderr = if failed_steps == 0 {
        String::new()
    } else {
        let summaries = results.iter().map(|node| {
            let id = node.id.as_deref().unwrap_or("?");
            let tool = node.tool.as_deref().unwrap_or("parallel");
            let (status, detail) = if node.ok {
                ("成功", node.data.as_ref().and_then(|data| data["output"].as_str()).unwrap_or_default())
            } else {
                ("失败", node.error.as_deref().unwrap_or("未提供原因"))
            };
            let excerpt: String = detail.chars().take(220).collect();
            format!("- {id} {tool} {status}：{excerpt}")
        }).collect::<Vec<_>>().join("\n");
        format!("并行取证：{successful_steps} 个成功，{failed_steps} 个失败。已成功的分支可继续使用。\n{summaries}\n回查完整分支时使用本批 output_id 和 run_id={id:?}。")
    };
    let report = PlanExecutionReport {
        success: failed_steps == 0,
        total_steps: results.len(),
        successful_steps,
        failed_steps,
        timed_out_steps: 0,
        elapsed_total_ms: start.elapsed().as_millis() as u64,
        status: "completed".into(),
        error_summary: None,
        results: json!(results),
    };
    let mut stdout = report.to_llm_feedback();
    // Review reads default to the parent session. This batch belongs to the
    // review's own run in that tree, so branch lookups need its explicit ID.
    stdout.push_str(&format!("回查本批审查回执时，请同时传入 run_id={id:?}。\n"));
    DispatchResult {
        stdout,
        stderr,
        success: report.success,
        route: "review:parallel".into(),
        view: None,
    }
}
