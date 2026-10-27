//! 计划树（PlanTree）与受控并行的分发执行：从 dispatcher.rs 抽出。
//!
//! run_plan_tree_sync 把模型的 parallel/plan_tree 指令转换为 PlanNode 树并
//! 同步执行；validate_controlled_parallel_batch 在执行前校验单批分支数
//! 不超过 MAX_CONTROLLED_PARALLEL_BRANCHES（响亮拒绝而非静默截断）。

use crate::agent::dispatcher::{
    append_mcp_raw_results, emit_tool_event, execute_command_with_images, mcp_raw_results_view,
};
use crate::agent::mcp::PluginRegistry;
use crate::agent::plan_tree::PlanNode;
use crate::agent::types::DispatchResult;
use crate::db::DbState;
use serde_json::Value;
use std::sync::{Arc, Mutex};

pub fn run_plan_tree_sync(
    db: &DbState,
    session_id: &str,
    plan_node: &crate::agent::plan_tree::PlanNode,
    registry: &PluginRegistry,
    workspace_root: Option<&std::path::Path>,
) -> DispatchResult {
    run_plan_tree_sync_with_images(db, session_id, plan_node, registry, workspace_root, None, None, None)
}

pub(crate) fn run_plan_tree_sync_with_images(
    db: &DbState,
    session_id: &str,
    plan_node: &crate::agent::plan_tree::PlanNode,
    registry: &PluginRegistry,
    workspace_root: Option<&std::path::Path>,
    pending: Option<&crate::agent::tools::view_image::PendingImageQueue>,
    run_id: Option<&str>,
    shell_guard: Option<&std::sync::Arc<crate::agent::dispatcher::ShellExecutionGuard>>,
) -> DispatchResult {
    // 硬拒绝第一道：任何深度出现超限/空并行组，整棵树拒绝执行，一个分支都不跑。
    if let Err(error) = validate_plan_tree_parallel_fanout(plan_node) {
        println!("🚫 [PlanTree: {}] 并行扇出越界，拒绝执行: {}", session_id, error);
        return DispatchResult { view: None,
            stdout: String::new(),
            stderr: error,
            success: false,
            route: "plan_tree_parallel_rejected".to_string(),
        };
    }

    let db_clone = db.clone();
    let sid = session_id.to_string();
    let reg_clone = registry.clone();
    // 并行分支必须继承当前活跃工作区，否则 local_file/local_shell 会因「未提供工作区」拒绝。
    let ws_owned: Option<std::path::PathBuf> = workspace_root.map(|p| p.to_path_buf());
    let mcp_raw_items = Arc::new(Mutex::new(Vec::<Value>::new()));
    let mcp_raw_collector = mcp_raw_items.clone();

    let pending = pending.cloned();
    let run_id = run_id.map(str::to_owned);
    let shell_guard = shell_guard.cloned();
    let invoker: crate::agent::plan_tree::ToolInvoker = Arc::new(move |tool, args| {
        let db_c = db_clone.clone();
        let sid_c = sid.clone();
        let reg_c = reg_clone.clone();
        let items_c = mcp_raw_items.clone();
        let ws_c = ws_owned.clone();
        let pending_c = pending.clone();
        let run_id_c = run_id.clone();
        let shell_guard_c = shell_guard.clone();

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
                execute_command_with_images(
                    &db_c,
                    &sid_c,
                    &tool,
                    &action,
                    &args,
                    &reg_c,
                    ws_c.as_deref(),
                    pending_c.as_ref(),
                    run_id_c.as_deref(),
                    shell_guard_c.as_ref(),
                )
            })
            .await
            .map_err(|e| format!("工具执行任务崩溃: {}", e))?;
            if let Ok(mut items) = items_c.lock() {
                append_mcp_raw_results(&mut items, res.view.as_ref());
            }
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
    let collected = mcp_raw_collector
        .lock()
        .map(|items| items.clone())
        .unwrap_or_default();

    DispatchResult { view: mcp_raw_results_view(collected),
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

/// 受控并行统一单批上限；读、写、Shell 与未知工具均按分支数量核对。
pub(crate) const MAX_CONTROLLED_PARALLEL_BRANCHES: usize = 5;

const ASYNC_CHILD_TOOLS: &[&str] = &["child_worker", "child_writer", "child_reviewer"];

pub(crate) fn check_no_async_child_tools<I>(tools: I) -> Result<(), String>
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    for tool in tools {
        let t = tool.as_ref().to_ascii_lowercase();
        if ASYNC_CHILD_TOOLS
            .iter()
            .any(|&c| t == c || t.ends_with(&format!(":{}", c)) || t.ends_with(&format!("/{}", c)))
        {
            return Err(format!(
                "❌ 「{}」是异步后台子 Agent，不能放在 parallel 同步执行树中（契约冲突：parallel 同步等待完整结果，而子任务为异步后台运行并返回 job_id）。如需批量派发子任务，请直接使用对应工具的 dispatches 数组，例如：{{\"tool\": \"{}\", \"dispatches\": [ {{...}}, {{...}} ]}}。",
                tool.as_ref(),
                tool.as_ref()
            ));
        }
    }
    Ok(())
}

/// 并行按统一单批容量校验；此处不推断分支是否独立。
/// 该校验不推断、补充或改写任何 MCP 子步骤参数。
pub(crate) fn validate_controlled_parallel_batch(
    parallel_steps: &[serde_json::Value],
) -> Result<(), String> {
    if parallel_steps.is_empty() {
        return Err("parallel 必须至少包含一个已确定的独立分支；未知范围请使用单步取证。".to_string());
    }
    check_no_async_child_tools(parallel_steps.iter().map(|step| {
        step.get("tool")
            .and_then(serde_json::Value::as_str)
            .or_else(|| step.get("action").and_then(serde_json::Value::as_str))
            .unwrap_or("")
    }))?;
    let cap = MAX_CONTROLLED_PARALLEL_BRANCHES;
    if parallel_steps.len() <= cap {
        return Ok(());
    }
    Err(format!(
        "parallel 单批最多允许 {} 个独立分支，本批包含 {} 个，尚未执行。请拆分为每批至多 {} 个分支，或改为串行执行；本错误仅针对分支数量，未判断范围或参数是否明确。",
        cap,
        parallel_steps.len(),
        cap
    ))
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
                check_no_async_child_tools(parallel_children.iter().map(|child| match child {
                    PlanNode::Obj(child_obj) => child_obj.get_tool(),
                    PlanNode::Seq(_) => "seq_nested".to_string(),
                }))?;
                let cap = MAX_CONTROLLED_PARALLEL_BRANCHES;
                match parallel_children.len() {
                    0 => {
                        return Err(format!(
                            "plan_tree 并行组 [{}] 为空：parallel 必须至少包含一个已确定的独立分支；未知范围请使用单步取证。",
                            obj.id.clone().unwrap_or_else(|| "?".to_string())
                        ))
                    }
                    actual if actual <= cap => {}
                    actual => {
                        return Err(format!(
                            "plan_tree 并行组 [{}] 超过数量上限：本批最多允许 {} 个独立分支，本组包含 {} 个，执行树尚未执行。请拆分为每组至多 {} 个分支，或改为串行执行；本错误未判断范围或参数是否明确。",
                            obj.id.clone().unwrap_or_else(|| "?".to_string()),
                            cap,
                            actual,
                            cap
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

    /// 回归：parallel 分支曾写死 workspace_root=None，导致 local_file 报「未提供当前活跃的工作区」。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn parallel_branches_inherit_workspace_root_for_local_file() {
        let dir = std::env::temp_dir().join(format!("pt_ws_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "hello-parallel").unwrap();
        let db = DbState(Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap())));
        let registry = PluginRegistry::new();
        let node: PlanNode = serde_json::from_value(json!({
            "parallel": [
                {"tool": "local_file", "args": {"action": "read", "path": "a.txt"}}
            ]
        }))
        .unwrap();

        let with_ws = run_plan_tree_sync(&db, "s1", &node, &registry, Some(dir.as_path()));
        assert!(with_ws.success, "带工作区应成功: {}", with_ws.stderr);
        assert!(with_ws.stdout.contains("hello-parallel"), "{}", with_ws.stdout);

        let without_ws = run_plan_tree_sync(&db, "s1", &node, &registry, None);
        assert!(!without_ws.success);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn controlled_parallel_uniform_limit_preserves_payloads_for_all_tool_types() {
        for branch in [
            json!({"tool":"remote-http-mcp/read_text_file","command":{"path":"/repo/a.rs"}}),
            json!({"tool":"local_file","params":{"action":"write","path":"a.rs","content":"test"}}),
            json!({"tool":"model_manager","action":"delete_spec"}),
            json!({"tool":"model_manager","args":{"action":"delete_spec"},"params":{"action":"list_specs"}}),
            json!({"tool":"local_shell","params":{"command":"node -v"}}),
            json!({"tool":"unknown_tool","params":{"future":true}}),
        ] {
            let five = vec![branch.clone(); 5];
            let before = five.clone();
            assert!(
                validate_controlled_parallel_batch(&five).is_ok(),
                "{branch:?}"
            );
            assert_eq!(five, before, "容量校验不得改写实际参数");
            let six = vec![branch; 6];
            let error = validate_controlled_parallel_batch(&six).unwrap_err();
            assert!(
                error.contains("最多允许 5")
                    && error.contains("本批包含 6 个")
                    && error.contains("尚未执行"),
                "{error}"
            );
            assert!(!error.contains("纯只读") && !error.contains("不放宽"));
        }
    }

    #[test]
    fn screenshot_and_environment_probe_batch_uses_the_same_limit() {
        // 用户回执 b7a43acb 的实际三分支形状；此处只验证批次数量，不运行外部命令。
        let mut batch = vec![
            json!({"tool":"view_image","params":{"path":".attachments/first.png"}}),
            json!({"tool":"view_image","params":{"path":".attachments/second.png"}}),
            json!({"tool":"local_shell","params":{"command":"rustup target list --installed; node -v; npm -v; npx tauri --version"}}),
        ];
        assert!(validate_controlled_parallel_batch(&batch).is_ok());
        batch.push(json!({"tool":"local_file","params":{"action":"read","path":"Cargo.toml"}}));
        batch.push(json!({"tool":"local_file","params":{"action":"write","path":"report.txt","content":"test"}}));
        assert!(validate_controlled_parallel_batch(&batch).is_ok());
        let tree: PlanNode = serde_json::from_value(json!({"parallel":batch})).unwrap();
        assert!(validate_plan_tree_parallel_fanout(&tree).is_ok());
    }

    #[test]
    fn plan_tree_fanout_walker_rejects_oversized_or_empty_parallel_group_at_any_depth() {
        let parse = |v: serde_json::Value| -> PlanNode { serde_json::from_value(v).unwrap() };

        // 合法：嵌套串行子树也按统一的单组 5 分支上限处理
        let ok_tree = parse(json!([
            { "tool": "a", "args": {} },
            { "id": "g", "parallel": [
                { "tool": "b", "args": {} },
                [ { "tool": "c", "args": {} }, { "tool": "d", "args": {} } ]
            ]}
        ]));
        assert!(validate_plan_tree_parallel_fanout(&ok_tree).is_ok());

        // 越界：嵌套在串行链深处的并行组 6 分支，必须整树拒绝
        let bad_tree = parse(json!([
            { "tool": "a", "args": {} },
            { "id": "deep", "parallel": [
                { "tool": "b", "args": {} },
                [ { "id": "inner", "parallel": [
                    { "tool": "x", "args": {} },
                    { "tool": "y", "args": {} },
                    { "tool": "z", "args": {} },
                    { "tool": "u", "args": {} },
                    { "tool": "v", "args": {} },
                    { "tool": "w", "args": {} }
                ]}]
            ]}
        ]));
        let err = validate_plan_tree_parallel_fanout(&bad_tree).unwrap_err();
        assert!(err.contains("最多允许 5"), "错误需指明上限: {}", err);
        assert!(err.contains("inner"), "错误需定位到越界组 id: {}", err);

        // 空并行组同样拒绝
        let empty_tree = parse(json!({ "id": "void", "parallel": [] }));
        assert!(validate_plan_tree_parallel_fanout(&empty_tree)
            .unwrap_err()
            .contains("至少包含一个"));
    }

    #[test]
    fn parallel_rejects_async_child_subagents_with_helpful_hint() {
        let parse = |v: serde_json::Value| -> PlanNode { serde_json::from_value(v).unwrap() };

        // 1. validate_controlled_parallel_batch 直接拒绝 child_worker
        let steps = vec![
            json!({ "tool": "child_worker", "task": "调研" }),
            json!({ "tool": "child_worker", "task": "分析" }),
        ];
        let err = validate_controlled_parallel_batch(&steps).unwrap_err();
        assert!(err.contains("异步后台子 Agent"));
        assert!(err.contains("dispatches 数组"));

        // 2. plan_tree 拒绝嵌套的 child_writer
        let tree = parse(json!({
            "parallel": [
                { "tool": "child_writer", "task": "改码" },
                { "tool": "file_viewer", "path": "Cargo.toml" }
            ]
        }));
        let err2 = validate_plan_tree_parallel_fanout(&tree).unwrap_err();
        assert!(err2.contains("异步后台子 Agent"));
        assert!(err2.contains("child_writer"));
    }
}

