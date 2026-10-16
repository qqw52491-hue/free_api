use crate::agent::mcp::PluginRegistry;
use crate::agent::mcp_catalog::invalidate_plugin;
use crate::agent::plan_tree_dispatch::{
    run_plan_tree_sync_with_images, validate_controlled_parallel_batch, MAX_CONTROLLED_PARALLEL_BRANCHES,
};
use crate::agent::retrieval;
use crate::agent::task_store;
use crate::agent::types::{AgentInstruction, DispatchResult};
use crate::agent::utils::{run_osascript, run_shell};
use crate::agent::web_search;
use crate::db::DbState;
use chrono::Utc;
use reqwest::Url;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

/// `finish` 是 Agent 协议的终态动作，不是本地工具或 MCP 工具。
/// 统一判定可防止某个调用路径把已完成任务派发为未知 action。
pub fn is_finish_instruction(instruction: &AgentInstruction) -> bool {
    instruction.get_tool().eq_ignore_ascii_case("finish")
        || instruction.get_action().eq_ignore_ascii_case("finish")
}

/// MCP 服务总名本身不是可调用工具。调用必须指定 `服务/具体工具`，或以服务名配合不同的具体 action。
/// 该检查只拒绝无效形态，不从参数中猜测或迁移任何工具、cwd、path 或 project。
fn mcp_service_requires_concrete_tool(tool: &str, action: &str) -> bool {
    !tool.contains('/')
        && (action.trim().is_empty() || tool.trim().eq_ignore_ascii_case(action.trim()))
}

mod shell_safety;
mod tool_events;
mod project_dispatch;
mod command_exec;

#[cfg(test)]
mod action_contract_tests;

pub(crate) use shell_safety::*;
pub(crate) use tool_events::*;
pub(crate) use project_dispatch::*;
pub(crate) use command_exec::*;

pub fn is_write_or_side_effect_tool(inst: &AgentInstruction) -> bool {
    // 调用参数里的 read_only/readonly 不是可信工具属性，不能覆盖有效动作。

    // 分类前统一剥离命名空间（remote-hub/read_file -> read_file）
    let orig_tool = inst.get_tool();
    let raw_tool = orig_tool.to_ascii_lowercase();
    let tool = raw_tool.rsplit('/').next().unwrap_or(&raw_tool);
    let mut action = inst.get_action().to_ascii_lowercase();
    if action == tool || action.is_empty() {
        if let Some(act) = inst.get_params().get("action").and_then(|v| v.as_str()) {
            action = act.to_ascii_lowercase();
        }
    }

    // 0. local_file 原生工具精准判定：read/list 绝不拦截；write/edit 必须严格由门禁把关！
    if tool.eq_ignore_ascii_case("local_file") {
        // 与 local_file::resolve_act 同一容错口径：外层 action 槽误填写动词、
        // 内层 params.action 显式只读且无内容字段时按内层归类——否则一个纯读
        // 请求会被外层标签误判为写并撞高危门禁（实测 {"action":"edit",
        // "command":{"action":"read",...}} 读 plan_submit.rs 被 src-tauri/** 拦）。
        let params = inst.get_params();
        let act = crate::agent::tools::local_file::resolve_act(&action, &params);
        if act.starts_with("read") || act.starts_with("list") {
            return false;
        }
        if act.starts_with("write") || act.starts_with("edit") {
            return true;
        }
    }

    // 1. 核心代码与文件写盘工具（edit_file / write_file / child_writer 等）：无论何时必须由门禁严格把关！
    // move/rename/remove 同属破坏性写：move_file 可覆盖目标、worktree_remove 可删
    // 整棵隔离树、vscode_renameSymbol 会跨工作区改写符号——名字不含写动词不等于安全。
    let is_code_write_tool = tool.contains("write")
        || tool.contains("edit")
        || tool.contains("delete")
        || tool.contains("save")
        || tool.contains("move")
        || tool.contains("rename")
        || tool.contains("remove")
        || tool.contains("excel")
        || tool.eq_ignore_ascii_case("child_writer")
        || action.starts_with("write")
        || action.starts_with("edit")
        || action.starts_with("delete")
        || action.starts_with("save")
        || action.starts_with("move")
        || action.starts_with("rename")
        || action.starts_with("remove");

    if is_code_write_tool {
        return true;
    }

    // 2. Shell / 终端执行类工具分两层：
    //    强壳标记（shell/bash/terminal/powershell/cmd/command 出现在名字里）——
    //        参数语言就是 shell，走命令级分析；
    //    弱执行标记（exec/run/eval）——参数语言可能是 SQL/JS/任何 DSL
    //        （exec_sql 里 "DELETE FROM" 过不了 shell 破坏性检查会被放行 = 漏记），
    //        命名空间工具上不信任弱标记，落到 2.5 的只读动词 fail-closed 判定。
    let strong_shell_marker = tool.contains("shell")
        || tool.contains("bash")
        || tool.contains("terminal")
        || tool.contains("powershell")
        || tool.contains("cmd")
        || tool.contains("command");
    let weak_exec_marker = tool.contains("exec")
        || tool.contains("run")
        || tool.contains("eval")
        || action.starts_with("exec")
        || action.starts_with("run");

    let is_shell_tool = if raw_tool.contains('/') {
        // 命名空间工具只信强壳标记
        strong_shell_marker
    } else {
        strong_shell_marker || weak_exec_marker
    };

    if is_shell_tool {
        if let Some(cmd_str) = extract_shell_command_string(inst) {
            return is_destructive_shell_command(&cmd_str);
        }
        // 如果未指定具体命令行（纯工具名嗅探），保守按写操作处理，保持向后兼容
        return true;
    }

    // 2.5 MCP 命名空间工具（plugin/tool）：写动词词表不可枚举——审计台账同款
    // fail-closed：只读动词前缀外的形态一律按写/副作用处理，否则 format_range、
    // rename_sheet、exec_sql(非 shell 语言) 这类 MCP 变更会绕过分阶段审查与验证绑定。
    // （须在强壳检测之后：remote-hub/shell_exec 这类壳工具走命令级分析）
    if raw_tool.contains('/') {
        let orig_name = orig_tool.rsplit('/').next().unwrap_or(&orig_tool);
        return !crate::agent::context::AuditLedger::mcp_tool_is_readonly_verb(orig_name);
    }

    // 3. task_store 的变更型动作（不带 create/update 前缀时由 action.contains 兜底识别）
    if tool == "task_store"
        && (action.contains("create") || action.contains("upsert") || action.contains("update"))
    {
        return true;
    }

    // 其余一律视为非写操作
    false
}


pub fn run_agent_step(
    db: &DbState,
    session_id: &str,
    instruction: &AgentInstruction,
    registry: &PluginRegistry,
    workspace_root: Option<&std::path::Path>,
) -> DispatchResult {
    run_agent_step_internal(db, session_id, instruction, registry, workspace_root, None, None)
}

pub(crate) fn run_agent_step_with_images(
    db: &DbState,
    session_id: &str,
    instruction: &AgentInstruction,
    registry: &PluginRegistry,
    workspace_root: Option<&std::path::Path>,
    pending: &crate::agent::tools::view_image::PendingImageQueue,
    run_id: Option<&str>,
) -> DispatchResult {
    run_agent_step_internal(db, session_id, instruction, registry, workspace_root, Some(pending), run_id)
}

fn run_agent_step_internal(
    db: &DbState,
    session_id: &str,
    instruction: &AgentInstruction,
    registry: &PluginRegistry,
    workspace_root: Option<&std::path::Path>,
    pending: Option<&crate::agent::tools::view_image::PendingImageQueue>,
    run_id: Option<&str>,
) -> DispatchResult {
    let tool_name = instruction.get_tool();
    if let Some(Err(error)) = instruction.resolved_local_shell_params() {
        return crate::agent::tools::input_validation::rejection(error);
    }
    if is_finish_instruction(instruction) {
        return DispatchResult { view: None,
            stdout: instruction
                .command
                .as_ref()
                .and_then(|value| value.get("summary").or_else(|| value.get("details")))
                .and_then(|value| value.as_str())
                .unwrap_or("任务已完成")
                .to_string(),
            stderr: String::new(),
            success: true,
            route: "finish".to_string(),
        };
    }

    // --- 场景 A0: 树形/并行计划树 PlanTree Execution ---
    if let Some(ref par_cmds) = instruction.parallel {
        if let Err(error) = validate_controlled_parallel_batch(par_cmds) {
            return DispatchResult { view: None,
                stdout: String::new(),
                stderr: error,
                success: false,
                route: "parallel_contract_rejected".to_string(),
            };
        }
        println!(
            "🌳 [PlanTree: {}] 检测到 parallel 并行指令，启动 {} 路并发执行...",
            session_id,
            par_cmds.len()
        );
        let plan_node =
            crate::agent::plan_tree::PlanNode::Obj(crate::agent::plan_tree::PlanNodeObj {
                parallel: Some(
                    par_cmds
                        .iter()
                        .map(|c| {
                            serde_json::from_value(c.clone()).unwrap_or_else(|_| {
                                crate::agent::plan_tree::PlanNode::Obj(
                                    serde_json::from_value(c.clone()).unwrap_or_default(),
                                )
                            })
                        })
                        .collect(),
                ),
                ..Default::default()
            });
        return run_plan_tree_sync_with_images(db, session_id, &plan_node, registry, workspace_root, pending, run_id);
    }
    if let Some(ref tree_val) = instruction.plan_tree {
        if let Ok(plan_node) =
            serde_json::from_value::<crate::agent::plan_tree::PlanNode>(tree_val.clone())
        {
            println!(
                "🌳 [PlanTree: {}] 检测到 plan_tree 复合树形计划，启动树形解释器...",
                session_id
            );
            return run_plan_tree_sync_with_images(db, session_id, &plan_node, registry, workspace_root, pending, run_id);
        }
    }

    // --- 核心修复：兼容 root.commands 和 root.command.commands ---
    let mut batch_commands = instruction.commands.clone();
    if batch_commands.is_empty() {
        if let Some(cmd_val) = &instruction.command {
            if let Some(cmds) = cmd_val.get("commands").and_then(|v| v.as_array()) {
                batch_commands = cmds.clone();
            }
        }
    }

    // --- 场景 A: 组合指令流水线 Batch Execution ---
    if !batch_commands.is_empty() {
        println!(
            "🚀 [Pipeline: {}] 正在按顺序执行 {} 个组合动作...",
            session_id,
            batch_commands.len()
        );
        let mut total_stdout = String::new();
        let mut mcp_raw_items = Vec::new();

        for (idx, cmd_val) in batch_commands.iter().enumerate() {
            let step_idx = idx + 1;
            // 提取单步的 tool 与 action (优先使用子命令自带的 tool，缺省退回根 tool_name)
            let step_tool = cmd_val
                .get("tool")
                .and_then(|v| v.as_str())
                .unwrap_or(&tool_name);
            let step_action = cmd_val
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();

            println!(
                "   ➡️  Step {}: 执行 [{}] 动作 '{}'...",
                step_idx, step_tool, step_action
            );

            // 本地 shell 与单步共用参数解析；其它工具保留既有透传，不改外部 MCP 契约。
            let prepared_params = if step_tool.eq_ignore_ascii_case("local_shell") {
                serde_json::from_value::<AgentInstruction>(cmd_val.clone())
                    .map_err(|error| format!("local_shell 子步骤结构无效：{error}"))
                    .and_then(|mut step| {
                        step.tool = Some(step_tool.to_string());
                        step.resolved_local_shell_params().unwrap()
                    })
            } else {
                Ok(cmd_val.get("command").cloned().unwrap_or_else(|| cmd_val.clone()))
            };
            let step_params = match prepared_params {
                Ok(params) => params,
                Err(error) => return DispatchResult {
                    view: mcp_raw_results_view(mcp_raw_items),
                    stdout: total_stdout,
                    stderr: format!("流水线第 {step_idx} 步参数预检拒绝：当前子步骤与后续步骤尚未执行；此前成功步骤已执行。\n{error}"),
                    success: false,
                    route: format!("pipeline_fail_at_{step_idx}"),
                },
            };
            let res =
                execute_command_with_images(db, session_id, step_tool, &step_action, &step_params, registry, workspace_root, pending, run_id);
            append_mcp_raw_results(&mut mcp_raw_items, res.view.as_ref());

            // 拼接输出结果
            if !res.stdout.is_empty() {
                total_stdout.push_str(&format!("\n[Step {} Result]: {}", step_idx, res.stdout));
            }

            // --- 核心修复：Fail-Fast 快速失败机制 ---
            if !res.success {
                println!(
                    "   🚫 Step {} 执行失败，流水线立即熔断！原因: {}",
                    step_idx, res.stderr
                );
                return DispatchResult { view: mcp_raw_results_view(mcp_raw_items),
                    stdout: total_stdout,
                    stderr: format!("流水线在第 {} 步崩溃: {}", step_idx, res.stderr),
                    success: false,
                    route: format!("pipeline_fail_at_{}", step_idx),
                };
            }
        }

        return DispatchResult { view: mcp_raw_results_view(mcp_raw_items),
            stdout: total_stdout,
            stderr: String::new(),
            success: true,
            route: "pipeline_success".to_string(),
        };
    }

    // --- 场景 B: 传统单步指令 Single Execution ---
    let mut action = instruction.get_action().trim().to_string();

    // --- 幻觉纠偏：如果 action 为空但 extra_fields 里有 tool_name，尝试打捞 ---
    if action.is_empty() {
        if let Some(name) = instruction
            .extra_fields
            .get("tool_name")
            .and_then(|v| v.as_str())
        {
            action = name.trim().to_string();
        }
    }

    let params = instruction.get_params();
    execute_command_with_images(db, session_id, &tool_name, &action, &params, registry, workspace_root, pending, run_id)
}

#[tauri::command]
pub async fn dispatch_agent_step(
    instruction_json: String,
    state: State<'_, DbState>,
    registry_state: State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    session_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let instruction: AgentInstruction =
        serde_json::from_str(&instruction_json).map_err(|e| e.to_string())?;

    // --- Strict JSON Schema Mutual Exclusivity Check ---
    let mut modes = 0;
    if instruction.tool.is_some() || instruction.action.is_some() {
        modes += 1;
    }
    if instruction.parallel.is_some() {
        modes += 1;
    }
    if instruction.plan_tree.is_some() {
        modes += 1;
    }
    if !instruction.commands.is_empty() {
        modes += 1;
    }

    if modes > 1 {
        return Ok(json!({
            "route": "schema_validation_failed",
            "success": false,
            "stdout": "",
            "stderr": "请选择单一步 (tool/action)、parallel 并行数组 或 plan_tree 计划树之一，严禁在同一 JSON 中混用！"
        }));
    }

    let registry_arc = registry_state.inner().clone();
    let db = state.inner().clone();
    let sid = session_id.unwrap_or_else(|| "default".to_string());

    let registry = {
        let guard = registry_arc.lock().await;
        (*guard).clone()
    };

    let result =
        tokio::task::spawn_blocking(move || run_agent_step(&db, &sid, &instruction, &registry, None))
            .await
            .map_err(|e| e.to_string())?;

    Ok(
        json!({ "route": result.route, "success": result.success, "stdout": result.stdout, "stderr": result.stderr }),
    )
}

#[cfg(test)]
mod tests;
