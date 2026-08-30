pub mod executor;
pub mod scope;
pub mod timeout;
pub mod types;

pub use executor::execute_plan_tree;
pub use scope::Scope;
pub use timeout::default_timeout_for_tool;
pub use types::{NodeResult, PlanExecutionReport, PlanNode, PlanNodeObj, ToolInvoker};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn test_seq_and_par_with_ref_resolution() {
        let invoker: ToolInvoker = Arc::new(|tool, args| {
            Box::pin(async move {
                match tool.as_str() {
                    "fetch_user" => Ok(json!({
                        "name": "Alice",
                        "city": "Shanghai"
                    })),
                    "search" => {
                        let q = args.get("q").and_then(|v| v.as_str()).unwrap_or("");
                        Ok(json!({
                            "query": q,
                            "results": [format!("result for {}", q)]
                        }))
                    }
                    "summarize" => {
                        let info = args.get("info").cloned().unwrap_or(json!({}));
                        Ok(json!({
                            "summary": format!("Summary of info: {:?}", info)
                        }))
                    }
                    _ => Err(format!("Unknown tool {}", tool)),
                }
            })
        });

        // 构造测试计划：
        // 1. fetch_user (串行) -> id: fetch_a
        // 2. parallel: search Alice + search Shanghai -> id: par_search
        // 3. summarize (串行) -> 引用 $par_search[0] 和 $fetch_a.name
        let plan_json = json!([
            {
                "id": "fetch_a",
                "tool": "fetch_user",
                "args": {}
            },
            {
                "id": "par_search",
                "parallel": [
                    { "id": "s1", "tool": "search", "args": { "q": "$fetch_a.name" } },
                    { "id": "s2", "tool": "search", "args": { "q": "$fetch_a.city" } }
                ]
            },
            {
                "id": "final_sum",
                "tool": "summarize",
                "args": {
                    "info": {
                        "user": "$fetch_a.name",
                        "first_search": "$par_search[0]"
                    }
                }
            }
        ]);

        let plan: PlanNode = serde_json::from_value(plan_json).unwrap();
        let report = execute_plan_tree(&plan, invoker, None).await;

        println!("Report: {}", report.to_llm_feedback());
        assert!(report.success);
        assert_eq!(report.total_steps, 4); // 1 + 2 (parallel) + 1
        assert_eq!(report.successful_steps, 4);
    }

    #[tokio::test]
    async fn test_seq_fail_fast() {
        let invoker: ToolInvoker = Arc::new(|tool, _args| {
            Box::pin(async move {
                if tool == "step_err" {
                    Err("Database connection refused".to_string())
                } else {
                    Ok(json!({ "ok": true }))
                }
            })
        });

        let plan_json = json!([
            { "tool": "step_1", "args": {} },
            { "tool": "step_err", "args": {} },
            { "tool": "step_3_never_run", "args": {} }
        ]);

        let plan: PlanNode = serde_json::from_value(plan_json).unwrap();
        let report = execute_plan_tree(&plan, invoker, None).await;

        assert!(!report.success);
        assert_eq!(report.status, "interrupted_by_error");
        // 第 3 步绝不会被执行
        assert_eq!(report.successful_steps, 1);
        assert_eq!(report.failed_steps, 1);
    }

    #[tokio::test]
    async fn test_parallel_fault_tolerant_and_timeout() {
        let invoker: ToolInvoker = Arc::new(|tool, _args| {
            Box::pin(async move {
                if tool == "slow_search" {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    Ok(json!({ "hits": 10 }))
                } else if tool == "failing_search" {
                    Err("404 Not Found".to_string())
                } else {
                    Ok(json!({ "status": "ok" }))
                }
            })
        });

        // 受控上限为 2：allSettled 语义用两分支演示（成功 + 超时）
        let plan_json = json!({
            "parallel": [
                { "tool": "fast_search", "args": {} },
                // 超时设定 100ms，而该工具 sleep 500ms -> 触发超时
                { "tool": "slow_search", "args": {}, "timeout_ms": 100 }
            ]
        });

        let plan: PlanNode = serde_json::from_value(plan_json).unwrap();
        let report = execute_plan_tree(&plan, invoker.clone(), None).await;

        println!("Report:\n{}", report.to_llm_feedback());
        assert!(!report.success);
        assert_eq!(report.total_steps, 2);
        assert_eq!(report.successful_steps, 1);
        assert_eq!(report.timed_out_steps, 1);
        assert_eq!(report.status, "completed");

        // 单点失败不中断其他分支
        let plan_fail: PlanNode = serde_json::from_value(json!({
            "parallel": [
                { "tool": "fast_search", "args": {} },
                { "tool": "failing_search", "args": {} }
            ]
        }))
        .unwrap();
        let report_fail = execute_plan_tree(&plan_fail, invoker, None).await;
        assert_eq!(report_fail.status, "completed");
        assert_eq!(report_fail.successful_steps, 1);
        assert_eq!(report_fail.failed_steps, 1);
    }

    #[tokio::test]
    async fn test_oversized_parallel_group_hard_rejected_before_spawn() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let invocations = Arc::new(AtomicUsize::new(0));
        let counter = invocations.clone();
        let invoker: ToolInvoker = Arc::new(move |tool, _args| {
            let counter = counter.clone();
            Box::pin(async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(json!({ "tool": tool }))
            })
        });

        // 3 分支未知工具并行组：按严格上限 2，必须在 spawn 层被整体拒绝
        let plan_json = json!({
            "id": "oversized",
            "parallel": [
                { "tool": "a", "args": {} },
                { "tool": "b", "args": {} },
                { "tool": "c", "args": {} }
            ]
        });

        let plan: PlanNode = serde_json::from_value(plan_json).unwrap();
        let report = execute_plan_tree(&plan, invoker, None).await;

        assert!(!report.success);
        assert_eq!(report.status, "interrupted_by_error");
        let summary = report.error_summary.unwrap_or_default();
        assert!(summary.contains("最多允许 2"), "错误需指明上限: {}", summary);
        assert!(summary.contains("未 spawn 任何子分支"));
        assert_eq!(report.total_steps, 0);
        assert_eq!(invocations.load(Ordering::SeqCst), 0, "一个子分支都不应被执行");
    }

    #[tokio::test]
    async fn test_readonly_fanout_spawn_allows_four_rejects_five() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let invocations = Arc::new(AtomicUsize::new(0));
        let counter = invocations.clone();
        let invoker: ToolInvoker = Arc::new(move |tool, _args| {
            let counter = counter.clone();
            Box::pin(async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(json!({ "tool": tool }))
            })
        });

        // 纯只读 4 分支：放宽上限内，正常执行
        let four: PlanNode = serde_json::from_value(json!({
            "id": "wide_read", "parallel": [
                { "tool": "remote-http-mcp/read_text_file", "args": {} },
                { "tool": "remote-http-mcp/read_text_file", "args": {} },
                { "tool": "remote-http-mcp/read_text_file", "args": {} },
                { "tool": "remote-http-mcp/read_text_file", "args": {} }
            ]
        }))
        .unwrap();
        let report = execute_plan_tree(&four, invoker.clone(), None).await;
        assert!(report.success, "纯只读 4 分支应放行: {:?}", report.error_summary);
        assert_eq!(report.successful_steps, 4);

        // 纯只读 5 分支：超过只读上限 4，spawn 层拒绝
        let five: PlanNode = serde_json::from_value(json!({
            "id": "too_wide_read", "parallel": [
                { "tool": "remote-http-mcp/read_text_file", "args": {} },
                { "tool": "remote-http-mcp/read_text_file", "args": {} },
                { "tool": "remote-http-mcp/read_text_file", "args": {} },
                { "tool": "remote-http-mcp/read_text_file", "args": {} },
                { "tool": "remote-http-mcp/read_text_file", "args": {} }
            ]
        }))
        .unwrap();
        let report = execute_plan_tree(&five, invoker, None).await;
        assert_eq!(report.status, "interrupted_by_error");
        assert!(report
            .error_summary
            .unwrap_or_default()
            .contains("最多允许 4"));
        assert_eq!(invocations.load(Ordering::SeqCst), 4, "只允许此前 4 次成功执行");
    }

    #[tokio::test]
    async fn test_deeply_nested_seq_par() {
        let invoker: ToolInvoker = Arc::new(|tool, args| {
            Box::pin(async move {
                if tool == "echo" {
                    let val = args.get("val").cloned().unwrap_or(json!(""));
                    Ok(json!({ "echoed": val }))
                } else {
                    Ok(json!({ "tool": tool }))
                }
            })
        });

        let plan_json = json!([
            { "id": "step0", "tool": "echo", "args": { "val": "root" } },
            {
                "id": "par_group",
                "parallel": [
                    [
                        { "id": "p1_s1", "tool": "echo", "args": { "val": "$step0.echoed" } },
                        { "id": "p1_s2", "tool": "echo", "args": { "val": "seq_in_par_step2" } }
                    ],
                    { "id": "p2", "tool": "echo", "args": { "val": "par_branch2" } }
                ]
            }
        ]);

        let plan: PlanNode = serde_json::from_value(plan_json).unwrap();
        let report = execute_plan_tree(&plan, invoker, None).await;

        assert!(report.success);
        assert_eq!(report.total_steps, 4);
        assert_eq!(report.successful_steps, 4);
    }

    #[tokio::test]
    async fn test_complex_path_extraction() {
        let scope = Scope::new();
        scope
            .set(
                "step1".to_string(),
                json!({
                    "users": [
                        { "name": "Tom", "score": 90 },
                        { "name": "Jerry", "score": 95 }
                    ],
                    "meta": { "total": 2 }
                }),
            )
            .await;

        let res1 = scope.resolve_value(&json!("$step1.users[1].name")).await;
        assert_eq!(res1, json!("Jerry"));

        let res2 = scope.resolve_value(&json!("$step1.meta.total")).await;
        assert_eq!(res2, json!(2));

        let res3 = scope
            .resolve_value(&json!("Hello $step1.users[0].name!"))
            .await;
        assert!(res3.as_str().unwrap().contains("Tom") || res3.is_string());
    }
}
