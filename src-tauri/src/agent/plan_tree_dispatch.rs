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

// ═══════════════════════════════════════════════════════════════════════
// 常量
// ═══════════════════════════════════════════════════════════════════════

/// 受控并行的单批最大分支数（与系统提示词"单批最多两个分支"一致）。

pub fn run_plan_tree_sync(
    db: &DbState,
    session_id: &str,
    plan_node: &crate::agent::plan_tree::PlanNode,
    registry: &PluginRegistry,
    workspace_root: Option<&std::path::Path>,
) -> DispatchResult {
    run_plan_tree_sync_with_images(db, session_id, plan_node, registry, workspace_root, None, None)
}

pub(crate) fn run_plan_tree_sync_with_images(
    db: &DbState,
    session_id: &str,
    plan_node: &crate::agent::plan_tree::PlanNode,
    registry: &PluginRegistry,
    workspace_root: Option<&std::path::Path>,
    pending: Option<&crate::agent::tools::view_image::PendingImageQueue>,
    run_id: Option<&str>,
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
    let invoker: crate::agent::plan_tree::ToolInvoker = Arc::new(move |tool, args| {
        let db_c = db_clone.clone();
        let sid_c = sid.clone();
        let reg_c = reg_clone.clone();
        let items_c = mcp_raw_items.clone();
        let ws_c = ws_owned.clone();
        let pending_c = pending.clone();
        let run_id_c = run_id.clone();

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

pub(crate) const MAX_CONTROLLED_PARALLEL_BRANCHES: usize = 2;

/// 纯只读扇出上限：读/检索类工具无副作用，是宽扇出的安全区
/// （强模型常见的"同时读四处证据"不会推进度，但会互相踩的写入才会）。
pub(crate) const MAX_CONTROLLED_PARALLEL_READ_BRANCHES: usize = 4;

/// 只读类工具判定（保守）：取工具名最后一段（兼容 MCP "插件/工具名"），
/// 命中读取/检索语义且不含任何变更语义才算只读；未知工具一律按可变处理。
pub(crate) fn is_readonly_parallel_tool(tool: &str) -> bool {
    let name = tool
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(tool)
        .to_ascii_lowercase();
    const READ_HINTS: [&str; 7] = ["read", "search", "list", "glob", "grep", "stat", "view"];
    const MUTATING_HINTS: [&str; 8] = [
        "write", "exec", "edit", "delete", "remove", "move", "create", "run",
    ];
    READ_HINTS.iter().any(|h| name.contains(h))
        && !MUTATING_HINTS.iter().any(|h| name.contains(h))
}

/// 并行分支的"有效工具名"：`local_file` 本身没有读/写语义（靠 action 区分），
/// 只看工具名会把纯读取误判为可变而卡在上限 2。这里对 local_file 取 action 拼成
/// `local_file/<action>`（action 取自显式字段或 params/args/command 里的 action），
/// 交给 is_readonly_parallel_tool 判定；action 缺失按 `read` 计——与 local_file 执行端一致（缺省 action = read），实测模型常只写 path/行号。
/// local_shell 仅当命令命中纯只读白名单时按只读计；其它工具不放宽。
pub(crate) fn parallel_branch_label(
    tool: &str,
    action: Option<&str>,
    payloads: &[Option<&Value>],
) -> String {
    let base = tool.rsplit(['/', ':']).next().unwrap_or(tool);
    if base.eq_ignore_ascii_case("local_shell") {
        // 纯只读白名单命令按只读分支计；其余（含取不到命令串）保持原工具名
        let cmd = payloads.iter().flatten().find_map(|p| {
            if tool.eq_ignore_ascii_case("local_shell") {
                serde_json::from_value::<crate::agent::types::AgentInstruction>(
                    serde_json::json!({"tool":"local_shell","params":p}))
                    .ok().and_then(|inst| crate::agent::dispatcher::extract_shell_command_string(&inst))
            } else { shell_command_text(p) }
        });
        return match cmd {
            Some(c) if crate::agent::dispatcher::is_pure_read_shell_command(&c) => {
                format!("{}/read", tool)
            }
            _ => tool.to_string(),
        };
    }
    if !base.eq_ignore_ascii_case("local_file") {
        return tool.to_string();
    }
    let from_payload = || {
        payloads.iter().flatten().find_map(|p| {
            p.get("action")
                .and_then(Value::as_str)
                .or_else(|| {
                    p.get("command")
                        .and_then(|c| c.get("action"))
                        .and_then(Value::as_str)
                })
        })
    };
    match action.or_else(from_payload) {
        Some(a) if !a.trim().is_empty() => format!("{}/{}", tool, a.trim()),
        _ => format!("{}/read", tool),
    }
}

/// 从分支 payload 取 shell 命令串：payload 本身是字符串、或 `.command` 是字符串、
/// 或 `.command.command` / `.cmd` 是字符串。
fn shell_command_text(p: &Value) -> Option<String> {
    if let Some(s) = p.as_str() {
        return Some(s.to_string());
    }
    let c = p.get("command");
    c.and_then(Value::as_str)
        .or_else(|| c.and_then(|c| c.get("command")).and_then(Value::as_str))
        .or_else(|| p.get("cmd").and_then(Value::as_str))
        .map(str::to_string)
}

/// parallel 数组里的 JSON 分支 -> 有效工具名（见 parallel_branch_label）。
pub(crate) fn parallel_branch_label_from_json(step: &Value) -> String {
    let tool_field = step.get("tool").and_then(Value::as_str);
    let action_field = step.get("action").and_then(Value::as_str);
    let tool = tool_field.or(action_field).unwrap_or("");
    let explicit_action = if tool_field.is_some() { action_field } else { None };
    if tool.eq_ignore_ascii_case("local_shell") {
        return serde_json::from_value::<crate::agent::plan_tree::PlanNodeObj>(step.clone())
            .ok().and_then(|node| node.get_args().ok())
            .map(|params| parallel_branch_label(tool, explicit_action, &[Some(&params)]))
            .unwrap_or_else(|| tool.to_string());
    }
    parallel_branch_label(
        tool,
        explicit_action,
        &[step.get("params"), step.get("args"), step.get("command")],
    )
}

/// plan_tree 里的分支节点 -> 有效工具名（见 parallel_branch_label）。
pub(crate) fn parallel_branch_label_from_node(obj: &crate::agent::plan_tree::PlanNodeObj) -> String {
    let tool = obj.get_tool();
    let explicit_action = if obj.tool.is_some() {
        obj.action.as_deref()
    } else {
        None
    };
    if tool.eq_ignore_ascii_case("local_shell") {
        return obj.get_args().map(|params|
            parallel_branch_label(&tool, explicit_action, &[Some(&params)]))
            .unwrap_or(tool);
    }
    parallel_branch_label(
        &tool,
        explicit_action,
        &[obj.params.as_ref(), obj.args.as_ref(), obj.command.as_ref()],
    )
}

/// 单批并行上限：全部分支都是只读工具时放宽到只读上限，否则维持严格上限。
pub(crate) fn controlled_parallel_cap_for_tools<I>(tools: I) -> usize
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    let mut saw_any = false;
    for tool in tools {
        saw_any = true;
        if !is_readonly_parallel_tool(tool.as_ref()) {
            return MAX_CONTROLLED_PARALLEL_BRANCHES;
        }
    }
    if saw_any {
        MAX_CONTROLLED_PARALLEL_READ_BRANCHES
    } else {
        MAX_CONTROLLED_PARALLEL_BRANCHES
    }
}

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

/// 并行仅接受少量、已经由模型明确给出的独立分支。
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
    let cap = controlled_parallel_cap_for_tools(
        parallel_steps.iter().map(parallel_branch_label_from_json),
    );
    if parallel_steps.len() <= cap {
        return Ok(());
    }
    let cap_note = if cap == MAX_CONTROLLED_PARALLEL_READ_BRANCHES {
        "，纯只读扇出上限"
    } else {
        "；含写入/执行/未知工具的批次不放宽"
    };
    Err(format!(
        "parallel 单批最多允许 {} 个独立分支{}，本批包含 {} 个，尚未执行。请拆分为每批至多 {} 个分支，或改为串行执行；本错误仅针对分支数量，未判断范围或参数是否明确。",
        cap,
        cap_note,
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
                let cap = controlled_parallel_cap_for_tools(parallel_children.iter().map(
                    |child| match child {
                        PlanNode::Obj(child_obj) => parallel_branch_label_from_node(child_obj),
                        PlanNode::Seq(_) => "seq_nested".to_string(),
                    },
                ));
                match parallel_children.len() {
                    0 => {
                        return Err(format!(
                            "plan_tree 并行组 [{}] 为空：parallel 必须至少包含一个已确定的独立分支；未知范围请使用单步取证。",
                            obj.id.clone().unwrap_or_else(|| "?".to_string())
                        ))
                    }
                    actual if actual <= cap => {}
                    actual => {
                        let cap_note = if cap == MAX_CONTROLLED_PARALLEL_READ_BRANCHES {
                            "纯只读扇出上限"
                        } else {
                            "含写入/执行/未知工具，不放宽"
                        };
                        return Err(format!(
                            "plan_tree 并行组 [{}] 超过数量上限：本批最多允许 {} 个独立分支（{}），本组包含 {} 个，执行树尚未执行。请拆分为每组至多 {} 个分支，或改为串行执行；本错误未判断范围或参数是否明确。",
                            obj.id.clone().unwrap_or_else(|| "?".to_string()),
                            cap,
                            cap_note,
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
    fn readonly_fanout_widened_to_four_but_mixed_batches_stay_strict() {
        let read_tool = |p: &str| {
            serde_json::json!({"tool": "remote-http-mcp/read_text_file", "command": {"path": p}})
        };
        let four_reads: Vec<_> = ["a.rs", "b.rs", "c.rs", "d.rs"].iter().map(|p| read_tool(p)).collect();
        let five_reads: Vec<_> = ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"].iter().map(|p| read_tool(p)).collect();
        let three_with_write = vec![
            read_tool("a.rs"),
            read_tool("b.rs"),
            serde_json::json!({"tool": "remote-http-mcp/write_file", "command": {"path": "x.rs"}}),
        ];
        let three_with_shell = vec![
            read_tool("a.rs"),
            serde_json::json!({"tool": "remote-http-mcp/shell_exec", "command": {"command": "npm"}}),
            read_tool("b.rs"),
        ];

        // 纯只读扇出放宽到 4
        assert!(validate_controlled_parallel_batch(&four_reads).is_ok());
        assert!(validate_controlled_parallel_batch(&five_reads)
            .unwrap_err()
            .contains("最多允许 4"));
        // 任一写入/执行分支即回到严格上限 2
        assert!(validate_controlled_parallel_batch(&three_with_write)
            .unwrap_err()
            .contains("最多允许 2"));
        assert!(validate_controlled_parallel_batch(&three_with_shell)
            .unwrap_err()
            .contains("不放宽"));

        // 分类器：只读命中且无变更语义；未知工具保守按可变
        assert!(is_readonly_parallel_tool("remote-http-mcp/read_multiple_files"));
        assert!(is_readonly_parallel_tool("fs/list_directory"));
        assert!(!is_readonly_parallel_tool("remote-http-mcp/write_file"));
        assert!(!is_readonly_parallel_tool("remote-http-mcp/shell_exec"));
        assert!(!is_readonly_parallel_tool("core"));
        assert_eq!(
            controlled_parallel_cap_for_tools(["remote-http-mcp/read_text_file"]),
            MAX_CONTROLLED_PARALLEL_READ_BRANCHES
        );
        assert_eq!(
            controlled_parallel_cap_for_tools([] as [&str; 0]),
            MAX_CONTROLLED_PARALLEL_BRANCHES
        );
    }

    #[test]
    fn local_file_read_branches_count_as_readonly_fanout() {
        let lf = |action: &str, path: &str| {
            serde_json::json!({"tool": "local_file", "action": action, "params": {"path": path}})
        };
        let four_reads: Vec<_> = (0..4).map(|i| lf("read", &format!("{i}.rs"))).collect();
        assert!(validate_controlled_parallel_batch(&four_reads).is_ok());
        let five_reads: Vec<_> = (0..5).map(|i| lf("read", &format!("{i}.rs"))).collect();
        assert!(validate_controlled_parallel_batch(&five_reads)
            .unwrap_err()
            .contains("最多允许 4"));
        // action 写在 params 里也算
        let nested: Vec<_> = (0..4)
            .map(|i| serde_json::json!({"tool": "local_file", "params": {"action": "read", "path": format!("{i}.rs")}}))
            .collect();
        assert!(validate_controlled_parallel_batch(&nested).is_ok());
        // 混入 write 或缺 action 的 local_file 仍是严格上限 2
        let with_write = vec![lf("read", "a"), lf("write", "b"), lf("read", "c")];
        assert!(validate_controlled_parallel_batch(&with_write)
            .unwrap_err()
            .contains("最多允许 2"));
        // 缺 action 的 local_file 执行端默认就是 read，按只读计
        let no_action = vec![
            serde_json::json!({"tool": "local_file", "params": {"path": "a"}}),
            lf("read", "b"),
            lf("read", "c"),
        ];
        assert!(validate_controlled_parallel_batch(&no_action).is_ok());
        // local_shell 取不到命令串不放宽
        let shell: Vec<_> = (0..3)
            .map(|_| serde_json::json!({"tool": "local_shell", "action": "read"}))
            .collect();
        assert!(validate_controlled_parallel_batch(&shell).is_err());
        // local_shell 纯只读命令放宽到 4；混入有副作用命令则回到 2
        let sh = |c: &str| serde_json::json!({"tool": "local_shell", "command": {"command": c}});
        let reads: Vec<_> = ["ls out", "cat a.txt", "ffprobe x.mp4", "git status"].iter().map(|c| sh(c)).collect();
        assert!(validate_controlled_parallel_batch(&reads).is_ok());
        let mixed = vec![sh("ls"), sh("node render.mjs"), sh("cat a")];
        assert!(validate_controlled_parallel_batch(&mixed).is_err());
        let str_cmd: Vec<_> = (0..3).map(|_| serde_json::json!({"tool": "local_shell", "command": "ls -la"})).collect();
        assert!(validate_controlled_parallel_batch(&str_cmd).is_ok());
    }

    #[test]
    fn plan_tree_fanout_walker_uses_tiered_caps_per_group() {
        let parse = |v: serde_json::Value| -> PlanNode { serde_json::from_value(v).unwrap() };

        // 纯只读 4 分支的嵌套并行组放行
        let ok_read_tree = parse(json!({
            "id": "wide_read", "parallel": [
                { "tool": "remote-http-mcp/read_text_file", "args": {} },
                { "tool": "remote-http-mcp/read_text_file", "args": {} },
                { "tool": "remote-http-mcp/read_multiple_files", "args": {} },
                { "tool": "remote-http-mcp/read_text_file", "args": {} }
            ]
        }));
        assert!(validate_plan_tree_parallel_fanout(&ok_read_tree).is_ok());

        // 只读 5 分支越界，错误指向只读上限 4
        let bad_read_tree = parse(json!({
            "id": "too_wide_read", "parallel": [
                { "tool": "remote-http-mcp/read_text_file", "args": {} },
                { "tool": "remote-http-mcp/read_text_file", "args": {} },
                { "tool": "remote-http-mcp/read_text_file", "args": {} },
                { "tool": "remote-http-mcp/read_text_file", "args": {} },
                { "tool": "remote-http-mcp/read_text_file", "args": {} }
            ]
        }));
        let err = validate_plan_tree_parallel_fanout(&bad_read_tree).unwrap_err();
        assert!(err.contains("最多允许 4"), "错误需指明只读上限: {}", err);

        // 嵌套串行子树视为未知工具（不放宽）：3 分支仍拒绝
        let nested_seq_tree = parse(json!({
            "id": "mixed", "parallel": [
                { "tool": "remote-http-mcp/read_text_file", "args": {} },
                { "tool": "remote-http-mcp/read_text_file", "args": {} },
                [ { "tool": "remote-http-mcp/read_text_file", "args": {} } ]
            ]
        }));
        let err = validate_plan_tree_parallel_fanout(&nested_seq_tree).unwrap_err();
        assert!(err.contains("最多允许 2"), "嵌套串行子树不应放宽: {}", err);
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

