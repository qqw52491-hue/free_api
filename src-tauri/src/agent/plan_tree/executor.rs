use crate::agent::plan_tree::scope::Scope;
use crate::agent::plan_tree::types::{NodeResult, PlanExecutionReport, PlanNode, ToolInvoker};
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

const MAX_PARALLEL_CODE_EVIDENCE_LINES: u64 = 120;

/// 只读检查并行组的源码证据总量；绝不补齐、覆盖或改写 MCP 参数。
/// 多个局部范围可以并行，但未知范围或合计超预算的源码读取必须由模型拆分。
fn validate_parallel_code_evidence_budget(children: &[PlanNode]) -> Result<(), String> {
    fn collect(node: &PlanNode, reads: &mut Vec<(String, Option<u64>)>) {
        match node {
            PlanNode::Seq(children) => {
                for child in children {
                    collect(child, reads);
                }
            }
            PlanNode::Obj(obj) => {
                if let Some(children) = &obj.parallel {
                    for child in children {
                        collect(child, reads);
                    }
                    return;
                }

                let tool = obj.get_tool();
                let normalized = tool.to_ascii_lowercase();
                let is_source_body = matches!(
                    normalized.rsplit('/').next().unwrap_or(normalized.as_str()),
                    "read_text_file" | "read_file" | "get_file_content" | "get_code_snippet"
                );
                if !is_source_body {
                    return;
                }

                let args = obj.get_args();
                let start = args
                    .get("startLine")
                    .or_else(|| args.get("start_line"))
                    .and_then(|value| value.as_u64());
                let end = args
                    .get("endLine")
                    .or_else(|| args.get("end_line"))
                    .and_then(|value| value.as_u64());
                let lines = match (start, end) {
                    (Some(start), Some(end)) if end >= start => Some(end - start + 1),
                    _ => None,
                };
                reads.push((tool, lines));
            }
        }
    }

    let mut reads = Vec::new();
    for child in children {
        collect(child, &mut reads);
    }
    if reads.len() <= 1 {
        return Ok(());
    }

    let unknown_tools: Vec<String> = reads
        .iter()
        .filter(|(_, lines)| lines.is_none())
        .map(|(tool, _)| tool.clone())
        .collect();
    if !unknown_tools.is_empty() {
        return Err(format!(
            "并行源码证据未执行：{} 缺少明确 startLine/endLine，无法确认本批总量。请改为单个读取，或为每项提供有限行范围；宿主未改写任何 MCP 参数。",
            unknown_tools.join(", ")
        ));
    }

    let total_lines: u64 = reads.iter().filter_map(|(_, lines)| *lines).sum();
    if total_lines > MAX_PARALLEL_CODE_EVIDENCE_LINES {
        return Err(format!(
            "并行源码证据未执行：{} 项合计 {} 行，超过本轮 {} 行预算。请拆分为更小批次；宿主未改写任何 MCP 参数。",
            reads.len(),
            total_lines,
            MAX_PARALLEL_CODE_EVIDENCE_LINES
        ));
    }

    Ok(())
}

/// 执行计划树的主入口
pub async fn execute_plan_tree(
    plan: &PlanNode,
    invoker: ToolInvoker,
    scope: Option<Scope>,
) -> PlanExecutionReport {
    let start = Instant::now();
    let scope = scope.unwrap_or_else(Scope::new);
    let total_steps = Arc::new(AtomicUsize::new(0));
    let succ_steps = Arc::new(AtomicUsize::new(0));
    let fail_steps = Arc::new(AtomicUsize::new(0));
    let timeout_steps = Arc::new(AtomicUsize::new(0));

    let res = run_node_recursive(
        plan,
        invoker,
        scope.clone(),
        total_steps.clone(),
        succ_steps.clone(),
        fail_steps.clone(),
        timeout_steps.clone(),
    )
    .await;

    let elapsed = start.elapsed().as_millis() as u64;
    let total = total_steps.load(Ordering::SeqCst);
    let succ = succ_steps.load(Ordering::SeqCst);
    let fail = fail_steps.load(Ordering::SeqCst);
    let timed = timeout_steps.load(Ordering::SeqCst);

    match res {
        Ok(results_val) => PlanExecutionReport {
            success: fail == 0 && timed == 0,
            total_steps: total,
            successful_steps: succ,
            failed_steps: fail,
            timed_out_steps: timed,
            elapsed_total_ms: elapsed,
            status: "completed".to_string(),
            error_summary: if fail > 0 || timed > 0 {
                let mut err_details = Vec::new();
                fn extract_errs(val: &serde_json::Value, errs: &mut Vec<String>) {
                    if let serde_json::Value::Array(arr) = val {
                        for item in arr {
                            extract_errs(item, errs);
                        }
                    } else if let serde_json::Value::Object(obj) = val {
                        if obj.get("ok").and_then(|v| v.as_bool()) == Some(false) {
                            let id = obj.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                            let tool = obj.get("tool").and_then(|v| v.as_str()).unwrap_or("?");
                            let err_msg = obj
                                .get("error")
                                .and_then(|v| v.as_str())
                                .unwrap_or("未知错误");
                            errs.push(format!("- 分支 [{}] ({}) 报错: {}", id, tool, err_msg));
                        }
                    }
                }
                extract_errs(&results_val, &mut err_details);

                if err_details.is_empty() {
                    Some(format!("并行分支中有 {} 个失败或超时", fail + timed))
                } else {
                    Some(format!(
                        "并行分支中有 {} 个失败或超时:\n{}",
                        fail + timed,
                        err_details.join("\n")
                    ))
                }
            } else {
                None
            },
            results: results_val,
        },
        Err(err) => PlanExecutionReport {
            success: false,
            total_steps: total,
            successful_steps: succ,
            failed_steps: fail,
            timed_out_steps: timed,
            elapsed_total_ms: elapsed,
            status: "interrupted_by_error".to_string(),
            error_summary: Some(err.clone()),
            results: json!({ "interrupted_error": err }),
        },
    }
}

fn run_node_recursive<'a>(
    node: &'a PlanNode,
    invoker: ToolInvoker,
    scope: Scope,
    total_steps: Arc<AtomicUsize>,
    succ_steps: Arc<AtomicUsize>,
    fail_steps: Arc<AtomicUsize>,
    timeout_steps: Arc<AtomicUsize>,
) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>> {
    Box::pin(async move {
        match node {
            // ── 规则 1：串行节点组 (Seq) -> Fail-Fast ──
            PlanNode::Seq(children) => {
                let mut results = Vec::with_capacity(children.len());
                for child in children {
                    let child_res = run_node_recursive(
                        child,
                        invoker.clone(),
                        scope.clone(),
                        total_steps.clone(),
                        succ_steps.clone(),
                        fail_steps.clone(),
                        timeout_steps.clone(),
                    )
                    .await?; // Fail-Fast: 一旦 Err，立刻中断整条串行链路并退出

                    results.push(child_res);
                }
                Ok(Value::Array(results))
            }

            // ── 规则 2：对象节点 (可能为并行组，或叶子工具) ──
            PlanNode::Obj(obj) => {
                // 分支 2.1：并行组 (allSettled 语义，等所有人完成，单点失败不中断其他)
                if let Some(ref parallel_children) = obj.parallel {
                    validate_parallel_code_evidence_budget(parallel_children)?;
                    let assigned_id = obj.id.clone().unwrap_or_else(|| scope.next_index_id());

                    let mut handles = Vec::with_capacity(parallel_children.len());
                    for child in parallel_children {
                        let invoker_c = invoker.clone();
                        let scope_c = scope.clone();
                        let total_c = total_steps.clone();
                        let succ_c = succ_steps.clone();
                        let fail_c = fail_steps.clone();
                        let time_c = timeout_steps.clone();
                        let child_owned = child.clone();

                        handles.push(
                            crate::task_registry::TaskRegistry::global()
                                .spawn_with_handle(
                                    crate::task_registry::TaskKind::PlanLeaf,
                                    format!("plan-leaf:{}", assigned_id),
                                    async move {
                                        run_node_recursive(
                                            &child_owned,
                                            invoker_c,
                                            scope_c,
                                            total_c,
                                            succ_c,
                                            fail_c,
                                            time_c,
                                        )
                                        .await
                                    },
                                )
                                .1,
                        );
                    }

                    // 全部等待完毕
                    let mut parallel_results = Vec::with_capacity(handles.len());
                    for h in handles {
                        match h.await {
                            Ok(Ok(val)) => parallel_results.push(val),
                            Ok(Err(err)) => {
                                parallel_results.push(json!({
                                    "ok": false,
                                    "error": err
                                }));
                            }
                            Err(join_err) => {
                                parallel_results.push(json!({
                                    "ok": false,
                                    "error": format!("Task join panic: {}", join_err)
                                }));
                            }
                        }
                    }

                    let group_val = Value::Array(parallel_results);
                    scope.set(assigned_id.clone(), group_val.clone()).await;
                    scope
                        .set(
                            assigned_id.trim_start_matches('$').to_string(),
                            group_val.clone(),
                        )
                        .await;

                    return Ok(group_val);
                }

                // 分支 2.2：原子叶子节点 (调用真实工具，带超时与变量替换)
                total_steps.fetch_add(1, Ordering::SeqCst);
                let tool_name = obj.get_tool();
                let assigned_id = obj.id.clone().unwrap_or_else(|| scope.next_index_id());
                let timeout_dur = obj.get_timeout();

                // 解析参数中的 $id 变量引用
                let raw_args = obj.get_args();
                let resolved_args = scope.resolve_value(&raw_args).await;

                let start_step = Instant::now();
                let invoker_future = (invoker)(tool_name.clone(), resolved_args);

                // 超时守卫
                let exec_res = tokio::time::timeout(timeout_dur, invoker_future).await;
                let elapsed_ms = start_step.elapsed().as_millis() as u64;

                match exec_res {
                    Ok(Ok(success_data)) => {
                        succ_steps.fetch_add(1, Ordering::SeqCst);
                        let node_res = NodeResult::success(
                            Some(assigned_id.clone()),
                            Some(tool_name),
                            success_data.clone(),
                            elapsed_ms,
                        );
                        let res_json =
                            serde_json::to_value(&node_res).unwrap_or(json!({ "ok": true }));

                        // 写入 Scope：同时支持带 $ 和不带 $ 的键
                        scope.set(assigned_id.clone(), success_data.clone()).await;
                        scope
                            .set(
                                assigned_id.trim_start_matches('$').to_string(),
                                success_data,
                            )
                            .await;

                        Ok(res_json)
                    }
                    Ok(Err(tool_err)) => {
                        fail_steps.fetch_add(1, Ordering::SeqCst);
                        let node_res = NodeResult::failure(
                            Some(assigned_id.clone()),
                            Some(tool_name.clone()),
                            tool_err.clone(),
                            false,
                            elapsed_ms,
                        );
                        let res_json =
                            serde_json::to_value(&node_res).unwrap_or(json!({ "ok": false }));

                        // 写入 Scope 错误状态
                        scope.set(assigned_id.clone(), res_json.clone()).await;
                        scope
                            .set(
                                assigned_id.trim_start_matches('$').to_string(),
                                res_json.clone(),
                            )
                            .await;

                        // 叶子调用失败返回带有失败详情的 Err 供上层判断 (如果是串行则中断，如果是并行由 tokio::spawn 捕获)
                        Err(format!("工具 [{}] 执行失败: {}", tool_name, tool_err))
                    }
                    Err(_timeout_err) => {
                        timeout_steps.fetch_add(1, Ordering::SeqCst);
                        let err_msg =
                            format!("工具 [{}] 执行超时 ({}s)", tool_name, timeout_dur.as_secs());
                        let node_res = NodeResult::failure(
                            Some(assigned_id.clone()),
                            Some(tool_name.clone()),
                            err_msg.clone(),
                            true,
                            elapsed_ms,
                        );
                        let res_json =
                            serde_json::to_value(&node_res).unwrap_or(json!({ "ok": false }));

                        scope.set(assigned_id.clone(), res_json.clone()).await;
                        scope
                            .set(
                                assigned_id.trim_start_matches('$').to_string(),
                                res_json.clone(),
                            )
                            .await;

                        Err(err_msg)
                    }
                }
            }
        }
    })
}
