//! main_loop 小工具函数群：预算、指标、触碰集与路径清单。

use super::*;
use crate::agent::change_verification::{
    apply_change_verification_update, change_verification_blocker, ChangeVerification,
};
use crate::agent::checkpoint::{
    load_checkpoint, recovery_decision, save_checkpoint, AgentCheckpoint, AgentPhase,
    RecoveryDecision,
};
use crate::agent::context;
use crate::agent::control::{
    finish_interrupted_run, intervention_channels, stream_interrupt_channels,
    take_pending_interrupt, HumanControl,
};
use crate::agent::dispatcher::{is_finish_instruction, is_write_or_side_effect_tool};
use crate::agent::evaluation_analysis::{assess_complex_delivery_summary, summary_makes_root_cause_claim};
use crate::agent::loop_runner::run_call_llm_and_run_tool_phases;
use crate::agent::mcp::{McpClient, PluginRegistry};
use crate::agent::review_contract::{apply_review_contract_update, ReviewContract};
use crate::agent::run_state::{AgentRunEvent, AgentRunMachine, AgentRunState};
use crate::agent::task_store;
use crate::agent::types::{AgentInstruction, DispatchResult, TodoItem, RUN_INTERRUPTED_ROUTE};
use crate::db::DbState;
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, watch};
use tokio::time::{sleep, Duration};
use crate::agent::finish_guard::{
    apply_instruction_state_updates, complex_delivery_blocker, finish_blockers,
    finish_blockers_with_diff, review_contract_evidence_blockers, DiffContext,
    should_challenge_finish, should_challenge_finish_with_blockers,
    should_challenge_finish_with_complex_delivery_blocker,
};
use crate::agent::workspace_lock::{acquire_workspace_lock, ChildWorkspaceGuard};

pub(crate) fn effective_max_steps(budget: Option<&AgentRunBudget>) -> usize {
    budget.and_then(|b| b.max_steps).unwrap_or(MAX_STEPS)
}

/// 返回 Some(reason) 表示 token 预算已耗尽，主循环应在步骤边界收敛。
pub(crate) fn token_budget_stop_reason(
    budget: Option<&AgentRunBudget>,
    spent_prompt_tokens: i64,
) -> Option<String> {
    let cap = budget?.max_prompt_tokens?;
    (spent_prompt_tokens >= cap).then(|| {
        format!("任务在消耗 {} prompt tokens 后触发 token 上限 {}", spent_prompt_tokens, cap)
    })
}
/// 正常会话只做短暂态请求重试；后台回归可显式提高到上限以避免 Provider 波动中断取证。

/// P3 降级逃生口阈值：连续 N 次派发全部落在门禁/求助拒绝路由上（零真实产出）
/// 即判定跨门禁死锁，改写为 RUN_INTERRUPTED_ROUTE 强制降级终止。

/// 门禁/求助拒绝路由判定：这些路由代表宿主拒绝了本次派发、未产生任何真实工作。
/// 真实工具失败（路径错、参数错等模型已尝试的调用）不计入——模型在尝试，只是失败，
/// 由失败重试/无进展检测各自兜底；这里只管"每步都被宿主拒之门外"的死锁形态。
pub(crate) fn is_gate_block_route(route: &str) -> bool {
    route.starts_with("plan_gatekeeper:")
        || route.starts_with("finish_review:")
        || route.starts_with("shell_tool_misuse:")
        || matches!(
            route,
            "stage_review:blocking_open"
                | "host_rejection:ask_human_cooldown"
                | "host_rejection:ask_human_duplicate"
                | "host_rejection:ask_human_quota_exhausted"
                | "host_rejection:repeat_read"
                | "host_rejection:batch_circuit_breaker"
                | "parallel_contract_rejected"
                | "plan_tree_parallel_rejected"
                | "plan_liveness:extension_denied"
                | "scope_guard:outside"
                | "watch:rejected"
        )
}

/// 统一步骤记录里的 token/耗时指标块（run_trace_steps 的 metrics/token_usage 字段）。
/// outcome 在派发后被部分 move（instruction/result 拆出），故按字段传参而非借整个结构。
pub(crate) fn step_metrics_json(
    token_usage: Option<&crate::agent::types::TokenUsage>,
    thinking_duration_ms: u64,
    tool_duration_ms: u64,
) -> Option<serde_json::Value> {
    token_usage.map(|u| {
        json!({
            "prompt_tokens": u.prompt_tokens,
            "completion_tokens": u.completion_tokens,
            "total_tokens": u.total_tokens,
            "cached_tokens": u.cached_tokens,
            "cache_hit_rate": u.cache_hit_rate,
            "tokens_per_second": u.tokens_per_second,
            "ttft_ms": u.ttft_ms,
            "thinking_duration_ms": thinking_duration_ms,
            "tool_duration_ms": tool_duration_ms,
            "total_duration_ms": thinking_duration_ms + tool_duration_ms,
        })
    })
}

/// 机械门禁早退分支的链上留痕：被打回的 finish 步骤必须写入 agent_run_steps。
/// 这些分支 `continue` 发生在统一步骤落盘之前——实时事件已发但行没落库，
/// 重启看板后整段门禁交锋人间蒸发，只剩"任务成功"的假象。
/// 与统一步骤落盘（run_trace_steps.push）字段口径保持一致。
#[allow(clippy::too_many_arguments)]
pub(crate) fn persist_challenged_step(
    db: &DbState,
    run_trace_steps: &mut Vec<serde_json::Value>,
    run_id: &str,
    session_id: &str,
    step_id: usize,
    instruction: &AgentInstruction,
    context: &context::SandwichContext,
    status: &str,
    output_text: &str,
    view: &Option<serde_json::Value>,
    metrics: Option<serde_json::Value>,
    thinking_text: &str,
) {
    let output_id = crate::agent::run_trace::save_step_output_with_meta(
        db,
        session_id,
        step_id,
        &instruction.get_tool(),
        &instruction.description,
        status,
        output_text,
    )
    .unwrap_or_else(|e| {
        eprintln!("⚠️ 无法保存 step_output: {}", e);
        "unknown_output_id".to_string()
    });
    run_trace_steps.push(json!({
        "id": step_id,
        "description": instruction.description.clone(),
        "reflection": instruction.reflection.clone(),
        "thought": instruction.thought.clone(),
        "finding": instruction.finding.clone(),
        "finding_kind": instruction.finding_kind.clone(),
        "thinking": crate::agent::run_trace::clip_thinking_for_trace(thinking_text),
        "tool": instruction.get_tool(),
        "action": instruction.get_action(),
        "command": instruction.get_params(),
        "params": instruction.get_params(),
        "plan_id": context.active_plan_id().unwrap_or_default(),
        "plan_title": context.active_plan_title().map(str::to_string).unwrap_or_else(|| crate::agent::context::types::clip_plan_title(context.effective_goal())),
        "status": status,
        "output": output_text,
        "output_id": output_id,
        "view": view.clone(),
        "resource_scope_revision": context.resource_scope.revision,
        "metrics": metrics.clone(),
        "token_usage": metrics,
        "timestamp": Utc::now().timestamp_millis(),
    }));
    if let Some(step_value) = run_trace_steps.last() {
        // seq 必须用 vec 下标而非 step_id：attempt 幽灵步会推高 len()，使下标与
        // step_id 脱节；用 step_id 当 seq 会经 ON CONFLICT(run_id,seq) 覆盖
        // 掉占据该位置的合法步骤行（实测 run e99d53da 的 seq28 被 finish_challenged
        // 覆写，快照 index33 与行表错位）。统一口径 = run_trace_steps.len()-1。
        if let Err(e) = crate::agent::run_trace::append_step(
            db,
            run_id,
            (run_trace_steps.len() - 1) as i64,
            step_value,
        ) {
            eprintln!("⚠️ 无法持久化步骤 {}: {}", step_id, e);
        }
    }
}

/// 汇总"目标被动过"的物理事实集：git changed_files ∪ 副作用受影响路径。
/// 桥接受管产物域：MCP 产物/受管文件写入会话 tmp（物理在工作区外），方案却常以
/// `tmp/<name>` 声明交付目标；把本会话 tmp 内的受影响路径归一为 `tmp/<rel>` 并入
/// 集合，让"产物已产出"的物理事实能对上声明目标。只扩 touched 侧，不反向改写方案声明；
/// 跨会话 tmp 路径不脱 sid 归一，避免撞名。
pub(crate) fn build_touched_set(d: &DiffContext, session_tmp: &std::path::Path) -> Vec<String> {
    let mut touched: Vec<String> = d
        .changed_files
        .keys()
        .cloned()
        .chain(d.all_affected_paths.iter().cloned())
        .collect();
    for p in &d.all_affected_paths {
        if let Ok(rel) = std::path::Path::new(p).strip_prefix(session_tmp) {
            touched.push(format!("tmp/{}", rel.to_string_lossy()));
        }
    }
    touched
}

/// 把会话 tmp 目录实物桥接为 `tmp/<rel>` 声明形态并入对账集。
/// tmp/ 写入不入副作用台账、不进工作区 diff——目录实物是 finish 门唯一的对账判据。
pub(crate) fn extend_touched_with_session_tmp(touched: &mut Vec<String>, session_tmp: &std::path::Path) {
    fn walk(dir: &std::path::Path, root: &std::path::Path, depth: usize, out: &mut Vec<String>) {
        if depth > 6 || out.len() >= 1000 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if let Ok(rel) = p.strip_prefix(root) {
                out.push(format!("tmp/{}", rel.to_string_lossy()));
            }
            if p.is_dir() {
                walk(&p, root, depth + 1, out);
            }
        }
    }
    if session_tmp.is_dir() {
        walk(session_tmp, session_tmp, 0, touched);
    }
}

/// 声明形态归一：物理 session_tmp 绝对路径 → `tmp/<rel>`；
/// `tmp/`/`./tmp/` 相对形态与其他目标原样返回。
pub(crate) fn normalize_tmp_declared(target: &str, session_tmp: &std::path::Path) -> String {
    let t = target.trim();
    if let Ok(rel) = std::path::Path::new(t).strip_prefix(session_tmp) {
        return format!("tmp/{}", rel.to_string_lossy());
    }
    t.to_string()
}

/// 渲染每轮投射到 M_Tail 的可操作文件边界卡片。
///
/// 文案与 path_guard 路由规则、artifact_manager 登记入口、finish 门禁的
/// tmp/ 对账行为必须保持一致——见 `path_scope_manifest_*` 测试。
/// 解析会话物理工作区：显式 override > 项目绑定 > 自由会话回落到会话产物目录。
/// 自由会话绝不回落进程 cwd——cwd 是宿主"事故目录"（从哪启动就是哪，常是仓库
/// 本体）：path_guard 双白名单会把它整个放行，提示词承诺的"工作区为空"与物理
/// 现实矛盾，模型 pwd/ls 一探即把宿主目录报成"已绑定工作区"（实测复现）。
pub(crate) fn resolve_workspace_dir(
    workspace_override: Option<std::path::PathBuf>,
    project_workspace: Option<std::path::PathBuf>,
    session_id: &str,
) -> std::path::PathBuf {
    workspace_override.or(project_workspace).unwrap_or_else(|| {
        let t = crate::agent::path_guard::get_session_tmp_dir(session_id);
        // 必须落盘存在：path_guard 每次校验都 canonicalize workspace_root，
        // 目录不存在会让全部路径检查报错（tmp 懒创建只在写入时触发，等不到）。
        let _ = std::fs::create_dir_all(&t);
        t
    })
}

pub(crate) fn build_path_scope_manifest(
    workspace_dir: &std::path::Path,
    session_tmp_dir: &std::path::Path,
    has_workspace_scope: bool,
) -> String {
    if has_workspace_scope {
        format!(
            "【当前会话可操作文件边界与作用域 (Operational Path Scope)】\n\
             • 项目物理工作区: `{}`\n\
               - 相对路径默认基于此目录解析（如 \"src/main.rs\"、\"package.json\"）；修改已有文件优先调用 `local_file` 的 `edit` 局部替换，禁止动辄全量重写。\n\
               - 门禁与台账规则: 对项目源码的写操作受阶段审查门禁保护，成功写入将记入不可逆副作用台账。\n\
             • 会话专属临时目录: `{}`\n\
               - 快捷路由: 相对路径以 \"tmp/\" 或 \"./tmp/\" 开头会自动映射路由至此目录。\n\
               - 临时与未明确路径产物隔离: 凡未明确指定项目路径的临时文件、数据导出或中间草稿，统一写入此目录（路径如 \"tmp/filename.ext\"），严禁直接写入项目物理工作区根目录！写入后可调用 `artifact_manager(action: \"register_file\", path: \"tmp/filename.ext\")` 上屏供用户下载或系统程序打开。\n\
               - 交付门禁直接认 tmp/ 产物：方案 target_files 声明 \"tmp/x\" 与本目录产物/登记记录自动对账，finish 时无需也不许把产物拷入项目工作区——拷贝副本只会污染用户仓库。\n\
               - 草稿免审: 任务推演计划、分析草稿、临时数据均建议写入此目录；临时草稿免审豁免，不消耗测试配额，不会阻塞交付。\n\
             • 物理牢笼约束: 任何超出上述双白名单的物理路径访问均会被 path_guard 拦截拒绝。\n",
            workspace_dir.display(),
            session_tmp_dir.display()
        )
    } else {
        format!(
            "【当前会话可操作文件边界与作用域 (Operational Path Scope)】\n\
             • 项目工作区：无（未绑定）\n\
             • 会话专属临时目录（产物与草稿暂存区，非工作区）: `{}`\n\
               - 快捷路由: 相对路径以 \"tmp/\" 或 \"./tmp/\" 开头会自动映射路由至此目录。\n\
               - 临时产物与草稿隔离: 凡未明确要求存放于项目工程中的临时文件、数据导出或中间脚本，统一写入此目录（路径如 \"tmp/filename.ext\"），严禁直接写入宿主当前运行目录！写入后可调用 `artifact_manager(action: \"register_file\", path: \"tmp/filename.ext\")` 上屏供用户下载或系统程序打开。\n\
               - 交付门禁直接认 tmp/ 产物：方案 target_files 声明 \"tmp/x\" 与本目录产物/登记记录自动对账，finish 时无需也不许把产物拷入当前目录——拷贝副本只会污染宿主项目目录。\n\
               - 草稿免审: 任务推演计划、分析草稿、临时数据均建议写入此目录；临时草稿免审豁免，不消耗测试配额，不会阻塞交付。\n\
             • 物理牢笼约束: 任何超出临时目录白名单的物理路径访问均会被 path_guard 拦截拒绝。\n",
            session_tmp_dir.display()
        )
    }
}
