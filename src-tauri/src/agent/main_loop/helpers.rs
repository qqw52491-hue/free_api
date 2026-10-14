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
    apply_instruction_state_updates, finish_blockers, finish_blockers_with_diff,
    review_contract_evidence_blockers, should_challenge_finish,
    should_challenge_finish_with_blockers, should_challenge_finish_with_unresolved, DiffContext,
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
        || matches!(
            route,
            "stage_review:blocking_open"
                | "host_rejection:ask_human_cooldown"
                | "host_rejection:ask_human_duplicate"
                | "host_rejection:ask_human_quota_exhausted"
                | "host_rejection:plan_approval_content_missing"
                | "host_rejection:repeat_read"
                | "host_rejection:batch_circuit_breaker"
                | "parallel_contract_rejected"
                | "plan_tree_parallel_rejected"
                | "plan_liveness:retriage_rejected"
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
///
/// 容量纪律（实测病灶）：渲染类任务一次就能产出数万实体（out/frames 数百帧 PNG +
/// node_modules + Chrome 临时剖面），旧实现 1000 条全局上限按 readdir 顺序先到先
/// 得——垃圾路径耗尽预算后，真声明目标（如 src/scenes/x.js）漏收 → 假零交集连拦
/// finish。对策：洪泛目录不下钻（依赖/构建/剖面目录不可能是交付声明），
/// 单目录限收防帧序列吃预算，全局上限放宽。
pub(crate) fn extend_touched_with_session_tmp(touched: &mut Vec<String>, session_tmp: &std::path::Path) {
    /// 不下钻的目录名：依赖/构建/隐藏配置/渲染剖面——交付声明永远指向源文件与产物，
    /// 不会指向这些目录内部。
    const SKIP_DESCEND: &[&str] = &[
        "node_modules", ".git", "target", "dist", "build", ".cache", ".config",
    ];
    const MAX_ENTRIES: usize = 4000;
    const MAX_PER_DIR: usize = 150;
    fn walk(dir: &std::path::Path, root: &std::path::Path, depth: usize, out: &mut Vec<String>) {
        if depth > 6 || out.len() >= MAX_ENTRIES {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut dir_count = 0usize;
        for e in entries.flatten() {
            if dir_count >= MAX_PER_DIR || out.len() >= MAX_ENTRIES {
                break;
            }
            dir_count += 1;
            let p = e.path();
            let name = e.file_name();
            let name = name.to_string_lossy();
            if let Ok(rel) = p.strip_prefix(root) {
                out.push(format!("tmp/{}", rel.to_string_lossy()));
            }
            // 目录本体入集合（声明可能指向目录），但已知洪泛目录不下钻
            if p.is_dir()
                && !SKIP_DESCEND.contains(&name.as_ref())
                && !name.starts_with('.')
                && !name.starts_with("com.google.")
                && !name.starts_with("org.chromium")
            {
                walk(&p, root, depth + 1, out);
            }
        }
    }
    if session_tmp.is_dir() {
        walk(session_tmp, session_tmp, 0, touched);
    }
}

/// 声明形态归一：物理 session_tmp 绝对路径 → `tmp/<rel>`；
/// 显式作用域 URI `session-tmp://<rel>` → `tmp/<rel>`、`workspace://<rel>` → `<rel>`；
/// `tmp/`/`./tmp/` 相对形态与其他目标原样返回。
pub(crate) fn normalize_tmp_declared(target: &str, session_tmp: &std::path::Path) -> String {
    let t = target.trim();
    if let Some(rest) = t.strip_prefix("session-tmp://") {
        return format!("tmp/{}", rest.trim_start_matches('/'));
    }
    if let Some(rest) = t.strip_prefix("workspace://") {
        return rest.trim_start_matches('/').to_string();
    }
    if let Ok(rel) = std::path::Path::new(t).strip_prefix(session_tmp) {
        return format!("tmp/{}", rel.to_string_lossy());
    }
    t.to_string()
}

/// 同名近失配检测：声明目标（已归一）与 touched 产物 basename 相同但路径不匹配 →
/// 生成 "声明 ≠ 产物" 点名对。finish 零交集打回时用——实测病灶：声明
/// `tmp/p5js-test/sketch.html` 而产物落在 `tmp/sketch.html`，文案若不点名
/// 层级差异，模型永远归不到"写对了文件但放错了目录"（连拦 13 次空转）。
pub(crate) fn tmp_target_near_misses(
    targets_norm: &[String],
    touched: &[String],
) -> Vec<String> {
    targets_norm
        .iter()
        .filter_map(|tf| {
            let bn = tf.rsplit('/').next().unwrap_or(tf.as_str());
            touched
                .iter()
                .find(|p| {
                    !crate::agent::finish_guard::diff::file_paths_match(tf, p)
                        && p
                            .rsplit('/')
                            .next()
                            .map(|b| b.eq_ignore_ascii_case(bn))
                            .unwrap_or(false)
                })
                .map(|hit| format!("{}（声明）≠ {}（产物）", tf, hit))
        })
        .collect()
}

/// 声明目标的直接物理核验：解析候选落点 → stat mtime ≥ 本轮 run 起点。
/// 与对账集成员资格互为冗余通道——对账集是「扫目录建名单再查名」（有容量/
/// 枚举顺序问题，12K 垃圾实体会饿死真目标）；本通道是「拿声明路径直接问
/// 文件系统」，O(声明数) 次 stat，永不漏收。
/// 候选落点：tmp/rel·session-tmp://rel·物理 session_tmp 前缀 → session_tmp 下；
/// workspace://rel → 工作区下；裸相对路径两区都试（模型相对其工作根声明，
/// 自由会话下工作根即 session_tmp）。
pub(crate) fn declared_target_touched_on_fs(
    target: &str,
    session_tmp: &std::path::Path,
    workspace_dir: &std::path::Path,
    run_started_ms: i64,
) -> bool {
    let t = target.trim();
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(rest) = t.strip_prefix("session-tmp://") {
        candidates.push(session_tmp.join(rest.trim_start_matches('/')));
    } else if let Some(rest) = t.strip_prefix("workspace://") {
        candidates.push(workspace_dir.join(rest.trim_start_matches('/')));
    } else if let Some(rest) = t.strip_prefix("tmp/").or_else(|| t.strip_prefix("./tmp/")) {
        candidates.push(session_tmp.join(rest));
    } else if std::path::Path::new(t).is_absolute() {
        candidates.push(std::path::PathBuf::from(t));
    } else {
        candidates.push(workspace_dir.join(t));
        candidates.push(session_tmp.join(t));
    }
    candidates.iter().any(|p| {
        std::fs::metadata(p)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|mt| {
                mt.duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_millis() as i64)
            })
            .map(|ms| ms >= run_started_ms)
            .unwrap_or(false)
    })
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
               - 快捷路由: 相对路径以 \"tmp/\" 或 \"./tmp/\" 开头会自动映射路由至此目录；显式作用域 URI \"session-tmp://<相对路径>\" 等价且对内部工具与外部 MCP 工具同样有效；⚠️ 但 shell/run_command 命令里的相对 tmp/ 不会被映射（它指向执行目录下的 tmp/），命令中请写 $SESSION_TMP_DIR/文件名（\"workspace://<相对路径>\" 等价项目相对路径）。\n\
               - 临时与未明确路径产物隔离: 凡未明确指定项目路径的临时文件、数据导出或中间草稿，统一写入此目录（路径如 \"tmp/filename.ext\"），严禁直接写入项目物理工作区根目录！写入后可调用 `artifact_manager(action: \"register_file\", path: \"tmp/filename.ext\")` 上屏供用户下载或系统程序打开。\n\
               - 交付门禁直接认 tmp/ 产物：方案 target_files 声明 \"tmp/x\" 与本目录产物/登记记录自动对账，finish 时无需也不许把产物拷入项目工作区——拷贝副本只会污染用户仓库。\n\
               - 草稿免审: 任务推演计划、分析草稿、临时数据均建议写入此目录；临时草稿免审豁免，不消耗测试配额，不会阻塞交付。\n\
             • 物理牢笼约束: 受控文件工具按项目工作区、会话 tmp、技能只读目录及当前附加目录权限表校验；附加目录是显式扩展，不切换主工作区。Shell/MCP 不是 OS 沙箱。\n",
            workspace_dir.display(),
            session_tmp_dir.display()
        )
    } else {
        format!(
            "【当前会话可操作文件边界与作用域 (Operational Path Scope)】\n\
             • 项目工作区：无（未绑定）\n\
             • 会话专属临时目录（产物与草稿暂存区，非工作区）: `{}`\n\
               - 快捷路由: 相对路径以 \"tmp/\" 或 \"./tmp/\" 开头会自动映射路由至此目录；显式作用域 URI \"session-tmp://<相对路径>\" 等价且对内部工具与外部 MCP 工具同样有效；⚠️ 但 shell/run_command 命令里的相对 tmp/ 不会被映射（它指向执行目录下的 tmp/），命令中请写 $SESSION_TMP_DIR/文件名。\n\
               - 临时产物与草稿隔离: 凡未明确要求存放于项目工程中的临时文件、数据导出或中间脚本，统一写入此目录（路径如 \"tmp/filename.ext\"），严禁直接写入宿主当前运行目录！写入后可调用 `artifact_manager(action: \"register_file\", path: \"tmp/filename.ext\")` 上屏供用户下载或系统程序打开。\n\
               - 交付门禁直接认 tmp/ 产物：方案 target_files 声明 \"tmp/x\" 与本目录产物/登记记录自动对账，finish 时无需也不许把产物拷入当前目录——拷贝副本只会污染宿主项目目录。\n\
               - 草稿免审: 任务推演计划、分析草稿、临时数据均建议写入此目录；临时草稿免审豁免，不消耗测试配额，不会阻塞交付。\n\
             • 物理牢笼约束: 受控文件工具按会话临时目录及应用技能只读目录校验；Shell/MCP 不是 OS 沙箱。\n",
            session_tmp_dir.display()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpOutputDedup {
    pub displayed_output: String,
    pub call_hash: Option<String>,
    pub raw_output_hash: Option<String>,
    pub model_output_hash: Option<String>,
    pub canonical_output_id: Option<String>,
    pub duplicate_of_step: Option<i64>,
}

fn canonical_output_resident_in_history(
    context: &context::SandwichContext,
    canonical_output_id: &str,
) -> bool {
    let needle = format!("output_id: {}", canonical_output_id);
    context.turns_history.iter().any(|msg| {
        let Some(text) = msg.content.as_str() else {
            return false;
        };
        msg.role == "user"
            && text.starts_with("【执行输出")
            && text.lines().any(|line| line.trim() == needle)
            && !text.contains("完整详细数据已安全持久化至 SQLite")
            && !text.contains("已深度压缩为句柄")
    })
}

/// has_mcp_evidence = 本步 DispatchResult.view 被识别为 mcp_raw_results sidecar。
/// 证据驱动准入：直调插件、commands 流水线、plan_tree 一视同仁；
/// 无 sidecar 的步骤（含 MCP 传输层异常）不参与折叠判定。
#[allow(clippy::too_many_arguments)]
pub(crate) fn deduplicate_mcp_output(
    db: &DbState,
    session_id: &str,
    has_mcp_evidence: bool,
    instruction: &AgentInstruction,
    outcome: &str,
    durable_output: &str,
    model_output: &str,
    prior_steps: &[serde_json::Value],
    context: &mut context::SandwichContext,
    current_output_id: &str,
) -> McpOutputDedup {
    let full_result = |call_hash, raw_output_hash, model_output_hash| McpOutputDedup {
        displayed_output: model_output.to_string(),
        call_hash,
        raw_output_hash,
        model_output_hash,
        canonical_output_id: None,
        duplicate_of_step: None,
    };
    if !has_mcp_evidence {
        return full_result(None, None, None);
    }
    let call_doc = json!({
        "tool": instruction.get_tool(),
        "action": instruction.get_action(),
        "params": instruction.get_params(),
    });
    let call_hash = context::determinism::sha256_hex(&call_doc.to_string());
    let raw_output_hash = context::determinism::sha256_hex(durable_output);
    let model_output_hash = context::determinism::sha256_hex(model_output);

    let anchor = prior_steps.iter().rev().find_map(|step| {
        if step.get("status").and_then(|v| v.as_str()) != Some(outcome) {
            return None;
        }
        let prior_call_hash = step
            .get("mcp_call_hash")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())?;
        if prior_call_hash != call_hash {
            return None;
        }
        let prior_raw_hash = step
            .get("mcp_raw_output_hash")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())?;
        if prior_raw_hash != raw_output_hash {
            return None;
        }
        let prior_model_hash = step
            .get("mcp_model_output_hash")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())?;
        if prior_model_hash != model_output_hash {
            return None;
        }
        let canonical_id = step
            .get("canonical_output_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                step.get("output_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
            })?;
        if !canonical_output_resident_in_history(context, canonical_id) {
            return None;
        }
        if !crate::agent::run_trace::step_output_matches_hash(
            db,
            session_id,
            canonical_id,
            &raw_output_hash,
        ) {
            return None;
        }
        Some((
            canonical_id.to_string(),
            step.get("id").and_then(|v| v.as_i64()),
        ))
    });
    let Some((canonical_id, prior_step_id)) = anchor else {
        return full_result(
            Some(call_hash),
            Some(raw_output_hash),
            Some(model_output_hash),
        );
    };

    let folded_text = format!(
        "【MCP 重复结果已折叠】\n本次 MCP 调用已真实执行；规范化参数与完整结果均和本会话先前步骤完全一致。旧结果的上下文表示（全文或相同采样）仍驻留当前上下文，因此未再次注入重复全文。\ncanonical_output_id: {}\n如后续上下文压缩后确需回查，请用 task_store.search_step_output 或 get_step_output 定向取回；当前请直接基于已有证据推进，避免原样重复调用。",
        canonical_id
    );
    let current_needle = format!("output_id: {}", current_output_id);
    let current_msg = context.turns_history.iter_mut().rev().find(|msg| {
        msg.role == "user"
            && msg.content.as_str().is_some_and(|text| {
                text.starts_with("【执行输出")
                    && text.lines().any(|line| line.trim() == current_needle)
            })
    });
    let Some(current_msg) = current_msg else {
        return full_result(
            Some(call_hash),
            Some(raw_output_hash),
            Some(model_output_hash),
        );
    };
    current_msg.content = json!(format!(
        "【执行输出 (v{})】\noutput_id: {}\n{}",
        crate::agent::context::sampling::SAMPLER_VERSION,
        current_output_id,
        folded_text
    ));

    McpOutputDedup {
        displayed_output: folded_text,
        call_hash: Some(call_hash),
        raw_output_hash: Some(raw_output_hash),
        model_output_hash: Some(model_output_hash),
        canonical_output_id: Some(canonical_id),
        duplicate_of_step: prior_step_id,
    }
}

/// 仅用户明确恢复失败任务时给新一轮预算；普通崩溃恢复仍沿用原上限。
pub(super) fn effective_retry_step_limit(budget: Option<&AgentRunBudget>, cursor: usize, retry: bool) -> usize {
    let limit = effective_max_steps(budget);
    if retry { cursor.saturating_add(limit) } else { limit }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::Mutex;

    fn mcp_dedup_test_db() -> DbState {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE agent_step_logs (
                output_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                step_id INTEGER NOT NULL,
                raw_output TEXT NOT NULL,
                created_at TEXT NOT NULL
            );",
            )
            .unwrap();
        DbState(Arc::new(Mutex::new(connection)))
    }

    fn mcp_instruction(params: serde_json::Value) -> AgentInstruction {
        serde_json::from_value(json!({
            "tool": "remote-http-mcp",
            "action": "call_tool",
            "params": params,
        }))
        .unwrap()
    }

    fn prior_step_json(
        step_id: i64,
        params: serde_json::Value,
        status: &str,
        output: &str,
        output_id: &str,
    ) -> serde_json::Value {
        json!({
            "id": step_id,
            "tool": "remote-http-mcp",
            "action": "call_tool",
            "params": params,
            "status": status,
            "output": output,
            "output_id": output_id,
        })
    }

    fn durable_sidecar(params: &serde_json::Value, model_body: &str) -> String {
        serde_json::to_string_pretty(&crate::agent::dispatcher::mcp_raw_result_view(
            "weather",
            "get",
            params,
            &json!({"content": [{"type": "text", "text": model_body}]}),
        ))
        .unwrap()
    }

    fn mcp_prior_step_json(
        step_id: i64,
        params: serde_json::Value,
        status: &str,
        durable: &str,
        model: &str,
        output_id: &str,
    ) -> serde_json::Value {
        let call_doc = json!({
            "tool": "remote-http-mcp",
            "action": "call_tool",
            "params": params.clone(),
        });
        let mut step = prior_step_json(step_id, params, status, model, output_id);
        step["mcp_call_hash"] = json!(context::determinism::sha256_hex(&call_doc.to_string()));
        step["mcp_raw_output_hash"] = json!(context::determinism::sha256_hex(durable));
        step["mcp_model_output_hash"] = json!(context::determinism::sha256_hex(model));
        step
    }

    fn mcp_prior_step_for_instruction(
        instruction: &AgentInstruction,
        step_id: i64,
        status: &str,
        durable: &str,
        model: &str,
        output_id: &str,
    ) -> serde_json::Value {
        let call_doc = json!({
            "tool": instruction.get_tool(),
            "action": instruction.get_action(),
            "params": instruction.get_params(),
        });
        let mut step = prior_step_json(
            step_id,
            instruction.get_params(),
            status,
            model,
            output_id,
        );
        step["tool"] = json!(instruction.get_tool());
        step["action"] = json!(instruction.get_action());
        step["mcp_call_hash"] = json!(context::determinism::sha256_hex(&call_doc.to_string()));
        step["mcp_raw_output_hash"] = json!(context::determinism::sha256_hex(durable));
        step["mcp_model_output_hash"] = json!(context::determinism::sha256_hex(model));
        step
    }

    fn persist_output(db: &DbState, step_id: usize, body: &str) -> String {
        crate::agent::run_trace::save_step_output_with_meta(
            db,
            "s1",
            step_id,
            "remote-http-mcp",
            "call_tool",
            "succeeded",
            body,
        )
        .unwrap()
    }

    fn exec_msg(output_id: &str, body: &str) -> String {
        format!(
            "【执行输出 (v{})】\noutput_id: {}\n{}",
            crate::agent::context::sampling::SAMPLER_VERSION,
            output_id,
            body
        )
    }

    fn ctx_with_messages(messages: Vec<(&str, String)>) -> context::SandwichContext {
        let mut ctx = context::SandwichContext::new("SYS".to_string(), "GOAL".to_string());
        for (role, content) in messages {
            ctx.turns_history.push(context::ChatMessage {
                role: role.to_string(),
                content: json!(content),
            });
        }
        ctx
    }

    fn ctx_after_add_step(
        prior_msgs: Vec<(&str, String)>,
        instruction: &AgentInstruction,
        current_body: &str,
        current_output_id: &str,
    ) -> context::SandwichContext {
        let mut ctx = ctx_with_messages(prior_msgs);
        ctx.add_step(instruction, current_body, current_output_id);
        ctx
    }

    fn run_dedup(
        db: &DbState,
        instruction: &AgentInstruction,
        durable: &str,
        model: &str,
        prior: &[serde_json::Value],
        ctx: &mut context::SandwichContext,
        current_output_id: &str,
    ) -> McpOutputDedup {
        deduplicate_mcp_output(
            db,
            "s1",
            true,
            instruction,
            "succeeded",
            durable,
            model,
            prior,
            ctx,
            current_output_id,
        )
    }

    fn message_with_output_id<'a>(
        ctx: &'a context::SandwichContext,
        output_id: &str,
    ) -> Option<&'a str> {
        let needle = format!("output_id: {}", output_id);
        ctx.turns_history
            .iter()
            .filter_map(|m| m.content.as_str())
            .find(|text| text.lines().any(|line| line.trim() == needle))
    }

    #[test]
    fn mcp_output_dedup_non_plugin_route_returns_full() {
        let db = mcp_dedup_test_db();
        let instruction = mcp_instruction(json!({"q": "x"}));
        let mut ctx = ctx_after_add_step(Vec::new(), &instruction, "正文", "cur-1");
        let history_len = ctx.turns_history.len();
        let dedup = deduplicate_mcp_output(
            &db,
            "s1",
            false,
            &instruction,
            "succeeded",
            "正文",
            "正文",
            &[],
            &mut ctx,
            "cur-1",
        );
        assert_eq!(
            dedup,
            McpOutputDedup {
                displayed_output: "正文".to_string(),
                call_hash: None,
                raw_output_hash: None,
                model_output_hash: None,
                canonical_output_id: None,
                duplicate_of_step: None,
            }
        );
        assert_eq!(ctx.turns_history.len(), history_len);
    }

    #[test]
    fn mcp_output_dedup_first_observation_stays_full() {
        let db = mcp_dedup_test_db();
        let params = json!({"q": "x"});
        let instruction = mcp_instruction(params.clone());
        let body = "正文A";
        let durable = durable_sidecar(&params, body);
        let mut ctx = ctx_after_add_step(Vec::new(), &instruction, body, "cur-1");
        let dedup = run_dedup(&db, &instruction, &durable, body, &[], &mut ctx, "cur-1");
        assert_eq!(dedup.displayed_output, body);
        assert!(dedup.call_hash.is_some());
        assert!(dedup.raw_output_hash.is_some());
        assert!(dedup.model_output_hash.is_some());
        assert_ne!(dedup.raw_output_hash, dedup.model_output_hash);
        assert!(dedup.canonical_output_id.is_none());
        assert!(dedup.duplicate_of_step.is_none());
        assert!(message_with_output_id(&ctx, "cur-1")
            .unwrap()
            .contains(body));
    }

    #[test]
    fn mcp_output_dedup_collapses_when_all_proofs_hold() {
        let db = mcp_dedup_test_db();
        let params = json!({"q": "shanghai"});
        let instruction = mcp_instruction(params.clone());
        let body = "天气结果正文";
        let durable = durable_sidecar(&params, body);
        let canonical_id = persist_output(&db, 0, &durable);
        let current_id = persist_output(&db, 1, &durable);
        let mut ctx = ctx_after_add_step(
            vec![("user", exec_msg(&canonical_id, body))],
            &instruction,
            body,
            &current_id,
        );
        let prior = vec![mcp_prior_step_json(
            3,
            params,
            "succeeded",
            &durable,
            body,
            &canonical_id,
        )];
        let dedup = run_dedup(&db, &instruction, &durable, body, &prior, &mut ctx, &current_id);
        assert!(dedup.displayed_output.contains("【MCP 重复结果已折叠】"));
        assert!(dedup
            .displayed_output
            .contains(&format!("canonical_output_id: {}", canonical_id)));
        assert_eq!(
            dedup.canonical_output_id.as_deref(),
            Some(canonical_id.as_str())
        );
        assert_eq!(dedup.duplicate_of_step, Some(3));

        assert!(message_with_output_id(&ctx, &canonical_id)
            .unwrap()
            .contains(body));
        let current_text = message_with_output_id(&ctx, &current_id).unwrap();
        assert!(current_text.contains("【MCP 重复结果已折叠】"));
        assert!(current_text.contains(&format!("canonical_output_id: {}", canonical_id)));
        assert!(!current_text.contains(body));

        let raw_hash = context::determinism::sha256_hex(&durable);
        assert!(crate::agent::run_trace::step_output_matches_hash(
            &db,
            "s1",
            &current_id,
            &raw_hash
        ));
    }

    #[test]
    fn mcp_output_dedup_rehydrates_when_prior_message_is_stub() {
        let db = mcp_dedup_test_db();
        let params = json!({"q": "shanghai"});
        let instruction = mcp_instruction(params.clone());
        let body = "天气结果正文";
        let durable = durable_sidecar(&params, body);
        let canonical_id = persist_output(&db, 0, &durable);
        let current_id = persist_output(&db, 1, &durable);
        let mut ctx = ctx_after_add_step(
            vec![(
                "user",
                format!(
                    "【执行输出精简句柄】\noutput_id: {}\n[提示: 已深度压缩为句柄，完整输出已存 SQLite，调用 search_step_output / get_step_output(output_id={}) 获取]",
                    canonical_id, canonical_id
                ),
            )],
            &instruction,
            body,
            &current_id,
        );
        let prior = vec![mcp_prior_step_json(
            3,
            params,
            "succeeded",
            &durable,
            body,
            &canonical_id,
        )];
        let dedup = run_dedup(&db, &instruction, &durable, body, &prior, &mut ctx, &current_id);
        assert_eq!(dedup.displayed_output, body);
        assert!(dedup.canonical_output_id.is_none());
        assert!(dedup.duplicate_of_step.is_none());
        assert!(message_with_output_id(&ctx, &current_id)
            .unwrap()
            .contains(body));
    }

    #[test]
    fn mcp_output_dedup_rehydrates_when_prior_message_is_dehydrated() {
        let db = mcp_dedup_test_db();
        let params = json!({"q": "shanghai"});
        let instruction = mcp_instruction(params.clone());
        let body = "天气结果正文";
        let durable = durable_sidecar(&params, body);
        let canonical_id = persist_output(&db, 0, &durable);
        let current_id = persist_output(&db, 1, &durable);
        let mut ctx = ctx_after_add_step(
            vec![(
                "user",
                format!(
                    "{}\n(完整详细数据已安全持久化至 SQLite，可通过 output_id={} 精准检索)",
                    exec_msg(&canonical_id, &body.chars().take(60).collect::<String>()),
                    canonical_id
                ),
            )],
            &instruction,
            body,
            &current_id,
        );
        let prior = vec![mcp_prior_step_json(
            3,
            params,
            "succeeded",
            &durable,
            body,
            &canonical_id,
        )];
        let dedup = run_dedup(&db, &instruction, &durable, body, &prior, &mut ctx, &current_id);
        assert_eq!(dedup.displayed_output, body);
        assert!(dedup.canonical_output_id.is_none());
        assert!(message_with_output_id(&ctx, &current_id)
            .unwrap()
            .contains(body));
    }

    #[test]
    fn mcp_output_dedup_archive_card_and_assistant_message_not_resident() {
        let db = mcp_dedup_test_db();
        let params = json!({"q": "shanghai"});
        let instruction = mcp_instruction(params.clone());
        let body = "天气结果正文";
        let durable = durable_sidecar(&params, body);
        let canonical_id = persist_output(&db, 0, &durable);
        let current_id = persist_output(&db, 1, &durable);
        let prior = vec![mcp_prior_step_json(
            3,
            params.clone(),
            "succeeded",
            &durable,
            body,
            &canonical_id,
        )];

        let mut ctx = ctx_after_add_step(
            vec![("assistant", exec_msg(&canonical_id, body))],
            &instruction,
            body,
            &current_id,
        );
        let dedup = run_dedup(&db, &instruction, &durable, body, &prior, &mut ctx, &current_id);
        assert_eq!(dedup.displayed_output, body);
        assert!(dedup.canonical_output_id.is_none());
        assert!(message_with_output_id(&ctx, &current_id)
            .unwrap()
            .contains(body));

        let mut ctx = ctx_after_add_step(
            vec![(
                "user",
                format!("【历史归档摘要】\noutput_id: {}\n{}", canonical_id, body),
            )],
            &instruction,
            body,
            &current_id,
        );
        let dedup = run_dedup(&db, &instruction, &durable, body, &prior, &mut ctx, &current_id);
        assert_eq!(dedup.displayed_output, body);
        assert!(dedup.canonical_output_id.is_none());
        assert!(message_with_output_id(&ctx, &current_id)
            .unwrap()
            .contains(body));
    }

    #[test]
    fn mcp_output_dedup_sampled_envelope_is_resident_and_folds() {
        let db = mcp_dedup_test_db();
        let params = json!({"q": "shanghai"});
        let instruction = mcp_instruction(params.clone());
        let body = "天气结果正文";
        let durable = durable_sidecar(&params, body);
        let canonical_id = persist_output(&db, 0, &durable);
        let current_id = persist_output(&db, 1, &durable);
        let mut ctx = ctx_after_add_step(
            vec![(
                "user",
                format!(
                    "【执行输出采样 (v{})】\noutput_id: {}\nestimated_tokens: 9000\ntruncated: true\n{}\n\n[宿主提示：已保存为工件]",
                    crate::agent::context::sampling::SAMPLER_VERSION,
                    canonical_id,
                    body.chars().take(30).collect::<String>()
                ),
            )],
            &instruction,
            body,
            &current_id,
        );
        let prior = vec![mcp_prior_step_json(
            3,
            params,
            "succeeded",
            &durable,
            body,
            &canonical_id,
        )];
        let dedup = run_dedup(&db, &instruction, &durable, body, &prior, &mut ctx, &current_id);
        assert!(dedup.displayed_output.contains("【MCP 重复结果已折叠】"));
        assert!(dedup
            .displayed_output
            .contains("旧结果的上下文表示（全文或相同采样）仍驻留当前上下文"));
        assert_eq!(
            dedup.canonical_output_id.as_deref(),
            Some(canonical_id.as_str())
        );
        assert_eq!(dedup.duplicate_of_step, Some(3));
        assert!(message_with_output_id(&ctx, &current_id)
            .unwrap()
            .contains("【MCP 重复结果已折叠】"));
    }

    #[test]
    fn mcp_output_dedup_legacy_prior_without_mcp_hashes_stays_full() {
        let db = mcp_dedup_test_db();
        let params = json!({"q": "shanghai"});
        let instruction = mcp_instruction(params.clone());
        let body = "天气结果正文";
        let durable = durable_sidecar(&params, body);
        let canonical_id = persist_output(&db, 0, &durable);
        let current_id = persist_output(&db, 1, &durable);
        let mut ctx = ctx_after_add_step(
            vec![("user", exec_msg(&canonical_id, body))],
            &instruction,
            body,
            &current_id,
        );
        let prior = vec![prior_step_json(3, params, "succeeded", body, &canonical_id)];
        let dedup = run_dedup(&db, &instruction, &durable, body, &prior, &mut ctx, &current_id);
        assert_eq!(dedup.displayed_output, body);
        assert!(dedup.canonical_output_id.is_none());
        assert!(dedup.duplicate_of_step.is_none());
        assert!(message_with_output_id(&ctx, &current_id)
            .unwrap()
            .contains(body));
    }

    #[test]
    fn mcp_output_dedup_fails_open_when_current_message_missing() {
        let db = mcp_dedup_test_db();
        let params = json!({"q": "shanghai"});
        let instruction = mcp_instruction(params.clone());
        let body = "天气结果正文";
        let durable = durable_sidecar(&params, body);
        let canonical_id = persist_output(&db, 0, &durable);
        let prior = vec![mcp_prior_step_json(
            3,
            params.clone(),
            "succeeded",
            &durable,
            body,
            &canonical_id,
        )];

        let mut ctx = ctx_with_messages(vec![("user", exec_msg(&canonical_id, body))]);
        let dedup = run_dedup(&db, &instruction, &durable, body, &prior, &mut ctx, "cur-absent");
        assert_eq!(dedup.displayed_output, body);
        assert!(dedup.canonical_output_id.is_none());
        assert!(dedup.duplicate_of_step.is_none());
        assert!(message_with_output_id(&ctx, &canonical_id)
            .unwrap()
            .contains(body));

        let mut ctx = ctx_with_messages(vec![
            ("user", exec_msg(&canonical_id, body)),
            (
                "user",
                format!("【历史归档摘要】\noutput_id: cur-bad\n{}", body),
            ),
        ]);
        let dedup = run_dedup(&db, &instruction, &durable, body, &prior, &mut ctx, "cur-bad");
        assert_eq!(dedup.displayed_output, body);
        assert!(dedup.canonical_output_id.is_none());
        assert!(message_with_output_id(&ctx, "cur-bad")
            .unwrap()
            .contains("【历史归档摘要】"));
    }

    #[test]
    fn mcp_output_dedup_fails_open_when_persistence_missing_or_mismatched() {
        let db = mcp_dedup_test_db();
        let params = json!({"q": "shanghai"});
        let instruction = mcp_instruction(params.clone());
        let body = "天气结果正文";
        let durable = durable_sidecar(&params, body);

        let current_id = persist_output(&db, 1, &durable);
        let mut ctx = ctx_after_add_step(
            vec![("user", exec_msg("ghost-output-id", body))],
            &instruction,
            body,
            &current_id,
        );
        let prior = vec![mcp_prior_step_json(
            3,
            params.clone(),
            "succeeded",
            &durable,
            body,
            "ghost-output-id",
        )];
        let dedup = run_dedup(&db, &instruction, &durable, body, &prior, &mut ctx, &current_id);
        assert_eq!(dedup.displayed_output, body);
        assert!(dedup.canonical_output_id.is_none());
        assert!(message_with_output_id(&ctx, &current_id)
            .unwrap()
            .contains(body));

        let overwritten_durable = durable_sidecar(&params, "已被覆盖的不同正文");
        let canonical_id = persist_output(&db, 0, &overwritten_durable);
        let mut ctx = ctx_after_add_step(
            vec![("user", exec_msg(&canonical_id, "已被覆盖的不同正文"))],
            &instruction,
            body,
            &current_id,
        );
        let prior = vec![mcp_prior_step_json(
            3,
            params,
            "succeeded",
            &durable,
            body,
            &canonical_id,
        )];
        let dedup = run_dedup(&db, &instruction, &durable, body, &prior, &mut ctx, &current_id);
        assert_eq!(dedup.displayed_output, body);
        assert!(dedup.canonical_output_id.is_none());
        assert!(message_with_output_id(&ctx, &current_id)
            .unwrap()
            .contains(body));
    }

    #[test]
    fn mcp_output_dedup_different_params_or_output_stays_full() {
        let db = mcp_dedup_test_db();
        let body = "天气结果正文";
        let params = json!({"q": "shanghai"});
        let durable = durable_sidecar(&params, body);
        let canonical_id = persist_output(&db, 0, &durable);
        let current_id = persist_output(&db, 1, &durable);
        let instruction = mcp_instruction(params.clone());
        let mut ctx = ctx_after_add_step(
            vec![("user", exec_msg(&canonical_id, body))],
            &instruction,
            body,
            &current_id,
        );

        let prior = vec![mcp_prior_step_json(
            3,
            json!({"q": "beijing"}),
            "succeeded",
            &durable,
            body,
            &canonical_id,
        )];
        let dedup = run_dedup(&db, &instruction, &durable, body, &prior, &mut ctx, &current_id);
        assert_eq!(dedup.displayed_output, body);
        assert!(dedup.canonical_output_id.is_none());

        let other_body = "另一份结果正文";
        let other_durable = durable_sidecar(&params, other_body);
        let prior = vec![mcp_prior_step_json(
            3,
            json!({"q": "shanghai"}),
            "succeeded",
            &other_durable,
            other_body,
            &canonical_id,
        )];
        let dedup = run_dedup(&db, &instruction, &durable, body, &prior, &mut ctx, &current_id);
        assert_eq!(dedup.displayed_output, body);
        assert!(dedup.canonical_output_id.is_none());

        let prior = vec![mcp_prior_step_json(
            3,
            json!({"q": "shanghai"}),
            "failed",
            &durable,
            body,
            &canonical_id,
        )];
        let dedup = run_dedup(&db, &instruction, &durable, body, &prior, &mut ctx, &current_id);
        assert_eq!(dedup.displayed_output, body);
        assert!(dedup.canonical_output_id.is_none());
        assert!(message_with_output_id(&ctx, &current_id)
            .unwrap()
            .contains(body));
    }

    #[test]
    fn mcp_output_dedup_call_fingerprint_normalizes_key_order() {
        let db = mcp_dedup_test_db();
        let mut ctx = context::SandwichContext::new("SYS".to_string(), "GOAL".to_string());
        let a = deduplicate_mcp_output(
            &db,
            "s1",
            true,
            &mcp_instruction(json!({"a": 1, "b": 2})),
            "succeeded",
            "x",
            "x",
            &[],
            &mut ctx,
            "cur-a",
        );
        let b = deduplicate_mcp_output(
            &db,
            "s1",
            true,
            &mcp_instruction(json!({"b": 2, "a": 1})),
            "succeeded",
            "x",
            "x",
            &[],
            &mut ctx,
            "cur-b",
        );
        assert_eq!(a.call_hash, b.call_hash);

        let c = deduplicate_mcp_output(
            &db,
            "s1",
            true,
            &mcp_instruction(json!({"k": [1, 2]})),
            "succeeded",
            "x",
            "x",
            &[],
            &mut ctx,
            "cur-c",
        );
        let d = deduplicate_mcp_output(
            &db,
            "s1",
            true,
            &mcp_instruction(json!({"k": [2, 1]})),
            "succeeded",
            "x",
            "x",
            &[],
            &mut ctx,
            "cur-d",
        );
        assert_ne!(c.call_hash, d.call_hash);
    }

    #[test]
    fn mcp_output_dedup_folded_row_chains_to_original_canonical() {
        let db = mcp_dedup_test_db();
        let params = json!({"q": "shanghai"});
        let instruction = mcp_instruction(params.clone());
        let body = "天气结果正文";
        let durable = durable_sidecar(&params, body);
        let canonical_id = persist_output(&db, 0, &durable);
        let current_id = persist_output(&db, 1, &durable);
        let mut ctx = ctx_after_add_step(
            vec![("user", exec_msg(&canonical_id, body))],
            &instruction,
            body,
            &current_id,
        );

        let mut folded_row = mcp_prior_step_json(
            2,
            params.clone(),
            "succeeded",
            &durable,
            body,
            "folded-out-id",
        );
        folded_row["output"] = json!(format!(
            "【MCP 重复结果已折叠】\ncanonical_output_id: {}",
            canonical_id
        ));
        folded_row["canonical_output_id"] = json!(canonical_id);
        let prior = vec![
            mcp_prior_step_json(1, params, "succeeded", &durable, body, &canonical_id),
            folded_row,
        ];
        let dedup = run_dedup(&db, &instruction, &durable, body, &prior, &mut ctx, &current_id);
        assert_eq!(
            dedup.canonical_output_id.as_deref(),
            Some(canonical_id.as_str())
        );
        assert_eq!(dedup.duplicate_of_step, Some(2));
        assert!(dedup.displayed_output.contains("【MCP 重复结果已折叠】"));
        assert!(message_with_output_id(&ctx, &current_id)
            .unwrap()
            .contains(&format!("canonical_output_id: {}", canonical_id)));
    }

    #[test]
    fn mcp_output_dedup_raw_meta_difference_stays_full() {
        let db = mcp_dedup_test_db();
        let params = json!({"q": "shanghai"});
        let instruction = mcp_instruction(params.clone());
        let body = "天气结果正文";
        let durable = durable_sidecar(&params, body);
        let meta_changed_durable = serde_json::to_string_pretty(
            &crate::agent::dispatcher::mcp_raw_result_view(
                "weather",
                "get",
                &params,
                &json!({
                    "content": [{"type": "text", "text": body}],
                    "_meta": {"trace_id": "另一个请求"},
                }),
            ),
        )
        .unwrap();
        let canonical_id = persist_output(&db, 0, &meta_changed_durable);
        let current_id = persist_output(&db, 1, &durable);
        let mut ctx = ctx_after_add_step(
            vec![("user", exec_msg(&canonical_id, body))],
            &instruction,
            body,
            &current_id,
        );
        let prior = vec![mcp_prior_step_json(
            3,
            params,
            "succeeded",
            &meta_changed_durable,
            body,
            &canonical_id,
        )];
        let dedup = run_dedup(&db, &instruction, &durable, body, &prior, &mut ctx, &current_id);
        assert_eq!(dedup.displayed_output, body);
        assert!(dedup.canonical_output_id.is_none());
        assert!(message_with_output_id(&ctx, &current_id)
            .unwrap()
            .contains(body));
    }

    #[test]
    fn mcp_output_dedup_model_output_difference_stays_full() {
        let db = mcp_dedup_test_db();
        let params = json!({"q": "shanghai"});
        let instruction = mcp_instruction(params.clone());
        let body = "天气结果正文";
        let durable = durable_sidecar(&params, body);
        let canonical_id = persist_output(&db, 0, &durable);
        let current_id = persist_output(&db, 1, &durable);
        let mut ctx = ctx_after_add_step(
            vec![("user", exec_msg(&canonical_id, "旧模型文本"))],
            &instruction,
            body,
            &current_id,
        );
        let prior = vec![mcp_prior_step_json(
            3,
            params,
            "succeeded",
            &durable,
            "旧模型文本",
            &canonical_id,
        )];
        let dedup = run_dedup(&db, &instruction, &durable, body, &prior, &mut ctx, &current_id);
        assert_eq!(dedup.displayed_output, body);
        assert!(dedup.canonical_output_id.is_none());
        assert!(dedup.duplicate_of_step.is_none());
        assert!(message_with_output_id(&ctx, &current_id)
            .unwrap()
            .contains(body));
    }

    #[test]
    fn mcp_output_dedup_pipeline_evidence_folds_when_identical() {
        let db = mcp_dedup_test_db();
        let instruction: AgentInstruction = serde_json::from_value(json!({
            "commands": [
                {"tool": "remote-hub/read_text_file", "command": {"path": "a.rs"}},
                {"tool": "remote-hub/ripgrep_search", "command": {"pattern": "fn main"}}
            ]
        }))
        .unwrap();
        let model = "\n[Step 1 Result]: 文件内容A\n[Step 2 Result]: 命中B";
        let sidecar = serde_json::to_string_pretty(
            &crate::agent::dispatcher::mcp_raw_results_view(vec![
                crate::agent::dispatcher::mcp_raw_result_view(
                    "remote-hub",
                    "read_text_file",
                    &json!({"path": "a.rs"}),
                    &json!({"content": [{"type": "text", "text": "文件内容A"}]}),
                )["items"][0]
                    .clone(),
                crate::agent::dispatcher::mcp_raw_result_view(
                    "remote-hub",
                    "ripgrep_search",
                    &json!({"pattern": "fn main"}),
                    &json!({"content": [{"type": "text", "text": "命中B"}]}),
                )["items"][0]
                    .clone(),
            ])
            .unwrap(),
        )
        .unwrap();
        let canonical_id = persist_output(&db, 0, &sidecar);
        let current_id = persist_output(&db, 1, &sidecar);
        let mut ctx = ctx_after_add_step(
            vec![("user", exec_msg(&canonical_id, model))],
            &instruction,
            model,
            &current_id,
        );
        let prior = vec![mcp_prior_step_for_instruction(
            &instruction,
            3,
            "succeeded",
            &sidecar,
            model,
            &canonical_id,
        )];
        let dedup = run_dedup(&db, &instruction, &sidecar, model, &prior, &mut ctx, &current_id);
        assert!(dedup.displayed_output.contains("【MCP 重复结果已折叠】"));
        assert_eq!(
            dedup.canonical_output_id.as_deref(),
            Some(canonical_id.as_str())
        );
        assert_eq!(dedup.duplicate_of_step, Some(3));

        let other_instruction: AgentInstruction = serde_json::from_value(json!({
            "commands": [{"tool": "remote-hub/read_text_file", "command": {"path": "b.rs"}}]
        }))
        .unwrap();
        let dedup = deduplicate_mcp_output(
            &db,
            "s1",
            true,
            &other_instruction,
            "succeeded",
            &sidecar,
            model,
            &prior,
            &mut ctx,
            &current_id,
        );
        assert_eq!(dedup.displayed_output, model);
        assert!(dedup.canonical_output_id.is_none());
    }

    #[test]
    fn mcp_output_dedup_without_evidence_sidecar_stays_inert() {
        let db = mcp_dedup_test_db();
        let instruction = mcp_instruction(json!({"q": "x"}));
        let mut ctx = ctx_after_add_step(Vec::new(), &instruction, "传输层超时", "cur-1");
        let dedup = deduplicate_mcp_output(
            &db,
            "s1",
            false,
            &instruction,
            "failed",
            "传输层超时",
            "传输层超时",
            &[],
            &mut ctx,
            "cur-1",
        );
        assert_eq!(dedup.displayed_output, "传输层超时");
        assert!(dedup.call_hash.is_none());
        assert!(dedup.raw_output_hash.is_none());
        assert!(dedup.model_output_hash.is_none());
    }
}
