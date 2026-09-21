use super::*;
use crate::agent::types::{ChildErrorKind, TokenUsage};
use crate::commands::projects::read_project_resource_excerpts_for_session;
use crate::db::DbState;
use crate::llm::{self, ModelConfig};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;
use uuid::Uuid;


/// 子任务逻辑身份（DispatchEnvelope 落库元数据）。
/// task_id 在同一逻辑任务的多次物理尝试间保持稳定——failover 换模型重派、
/// resume 追问续跑都继承同一个 task_id，attempt_id 递增；
/// purpose 标记用途（plan_review/stage_review/rebuttal/writer/worker），
/// batch_id 归组同批联合会审成员，contract_id 记录交付契约版本。
/// 落库后"这道题烧了多少钱/谁在等它的结果"变成台账查询，
/// 而非 checkpoint 里靠约定维护的名单。
#[derive(Debug, Clone)]
pub(crate) struct ChildRunIdentity {
    pub task_id: String,
    pub attempt_id: u32,
    pub purpose: Option<String>,
    pub batch_id: Option<String>,
    pub contract_id: Option<String>,
    /// 交付契约模式 JSON（custom_schema 序列化），消费端按它做运行时校验
    pub contract_schema: Option<String>,
}

impl ChildRunIdentity {
    /// 全新逻辑任务：task_id 生成一次，attempt=1
    pub fn fresh(purpose: impl Into<String>) -> Self {
        Self {
            task_id: format!("task-{}", Uuid::new_v4()),
            attempt_id: 1,
            purpose: Some(purpose.into()),
            batch_id: None,
            contract_id: None,
            contract_schema: None,
        }
    }

    /// 从派发信封取身份
    pub fn from_envelope(env: &crate::agent::protocol::DispatchEnvelope) -> Self {
        Self {
            task_id: env.task_id.clone(),
            attempt_id: env.attempt_id,
            purpose: env.purpose.clone(),
            batch_id: env.batch_id.clone(),
            contract_id: Some(env.contract_id.clone()),
            contract_schema: if env.custom_schema.is_empty() {
                None
            } else {
                serde_json::to_string(&env.custom_schema).ok()
            },
        }
    }

    /// 继承前驱 run 的逻辑身份（resume/追问/对质续跑）：
    /// task_id 与 batch/contract 一脉相承，attempt_id 由落库侧自动递增。
    /// purpose 可覆盖（如对质重审标 rebuttal），未给则继承原用途。
    pub fn inherited(prev: &ChildRunIdentity, purpose: Option<String>) -> Self {
        Self {
            task_id: prev.task_id.clone(),
            attempt_id: 1,
            purpose: purpose.or_else(|| prev.purpose.clone()),
            batch_id: prev.batch_id.clone(),
            contract_id: prev.contract_id.clone(),
            contract_schema: prev.contract_schema.clone(),
        }
    }
}

/// 查询 run 的逻辑任务身份（failover/resume 继承用）。
/// 返回 (task_id, attempt_id, purpose, batch_id)；旧行 task_id 可能为 NULL
/// 时回退为 run_id 本身（旧 run 就是自己的逻辑任务）。
pub(crate) fn load_run_identity(
    db: &DbState,
    child_run_id: &str,
) -> Option<ChildRunIdentity> {
    let conn = db.0.lock().ok()?;
    conn.query_row(
        "SELECT COALESCE(task_id, id), attempt_id, purpose, batch_id, contract_id, contract_schema FROM agent_child_runs WHERE id = ?1",
        params![child_run_id],
        |row| {
            Ok(ChildRunIdentity {
                task_id: row.get::<_, String>(0)?,
                attempt_id: row.get::<_, i64>(1)?.max(1) as u32,
                purpose: row.get::<_, Option<String>>(2)?,
                batch_id: row.get::<_, Option<String>>(3)?,
                contract_id: row.get::<_, Option<String>>(4)?,
                contract_schema: row.get::<_, Option<String>>(5)?,
            })
        },
    )
    .ok()
}

/// 幂等查重：同一 task_id 下是否已有比 prev 更晚的活 attempt
///（running 在途 / 已终态但回执未落账——后者仍在信箱等投递）。
/// 崩溃重放路径依赖它：备胎 INSERT 先于父 checkpoint 提交落盘时，
/// 重放不能再派新备胎，只能认养已存在的那个。
pub(crate) fn live_attempt_for_task(
    db: &DbState,
    parent_run_id: &str,
    task_id: &str,
    exclude_run_id: &str,
    exclude_attempt: u32,
) -> Option<String> {
    let conn = db.0.lock().ok()?;
    conn.query_row(
        "SELECT id FROM agent_child_runs
         WHERE parent_run_id = ?1 AND task_id = ?2 AND id != ?3
           AND COALESCE(attempt_id, 1) > ?4
           AND (status = 'running' OR applied_at IS NULL OR applied_at = '')
         ORDER BY COALESCE(attempt_id, 1) DESC LIMIT 1",
        params![parent_run_id, task_id, exclude_run_id, exclude_attempt],
        |row| row.get::<_, String>(0),
    )
    .ok()
}

pub(crate) fn begin_child_run(
    db: &DbState,
    parent_run_id: &str,
    parent_session_id: &str,
    workspace_root: &Path,
    model_id: &str,
    evidence_ref_count: usize,
    kind: &str,
    custom_cap: Option<i64>,
) -> Result<String, String> {
    begin_child_run_extended(
        db,
        parent_run_id,
        parent_session_id,
        workspace_root,
        model_id,
        evidence_ref_count,
        kind,
        custom_cap,
        None,
        None,
        &ChildRunIdentity::fresh(kind),
    )
}

pub(crate) fn begin_child_run_extended(
    db: &DbState,
    parent_run_id: &str,
    parent_session_id: &str,
    workspace_root: &Path,
    model_id: &str,
    evidence_ref_count: usize,
    kind: &str,
    custom_cap: Option<i64>,
    workspace_copy: Option<&str>,
    resumed_from: Option<&str>,
    identity: &ChildRunIdentity,
) -> Result<String, String> {
    let child_run_id = Uuid::new_v4().to_string();
    let root = workspace_root
        .canonicalize()
        .map_err(|error| format!("child_reviewer 工作区不可用: {}", error))?;
    let conn = db.0.lock().map_err(|error| error.to_string())?;

    // 第一道看门：全局瞬时并发看门（所有正在 running 的子任务数，上限 MAX_TOTAL_CHILDREN_PER_RUN = 6）
    let running_children: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_child_runs WHERE parent_run_id=?1 AND status='running'",
            params![parent_run_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if running_children >= i64::from(MAX_TOTAL_CHILDREN_PER_RUN) {
        return Err(format!(
            "每个父运行最多同时并发执行 {} 个子任务（全局看门上限已满，当前正在运行 {} 个）。请等待现有任务完成或使用 wait_jobs 等待。",
            MAX_TOTAL_CHILDREN_PER_RUN, running_children
        ));
    }

    // 第二道看门：单 kind / 角色卡弹性瞬时并发配额（角色卡 max_concurrency 优先覆盖，0 则跟随默认）
    let effective_kind_cap = match custom_cap {
        Some(cap) if cap > 0 => cap,
        _ => i64::from(MAX_CHILDREN_PER_KIND_PER_RUN),
    };

    let running_kind_children: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_child_runs WHERE parent_run_id=?1 AND kind=?2 AND status='running'",
            params![parent_run_id, kind],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if running_kind_children >= effective_kind_cap {
        return Err(format!(
            "每个父运行最多同时并发执行 {} 个 {} 子任务（当前配额 {} 已满）。请等待现有任务完成或使用 wait_jobs 等待。",
            effective_kind_cap, kind, effective_kind_cap
        ));
    }
    // attempt_id 按 (parent_run_id, task_id) 自动推导：同逻辑任务的已有
    // 最大 attempt + 1，调用方显式给的更大的值优先。重试/failover/resume
    // 只需继承 task_id，attempt 计数在这里自动正确，调用方想传错都难。
    let derived_attempt: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(attempt_id), 0) + 1 FROM agent_child_runs WHERE task_id = ?1 AND parent_run_id = ?2",
            params![identity.task_id, parent_run_id],
            |row| row.get(0),
        )
        .unwrap_or(1);
    let attempt_id = derived_attempt.max(identity.attempt_id as i64);

    conn.execute(
        "INSERT INTO agent_child_runs (id, parent_run_id, parent_session_id, workspace_root, model_id, status, started_at, evidence_ref_count, kind, workspace_copy, resumed_from, task_id, attempt_id, purpose, batch_id, contract_id, contract_schema) VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            child_run_id,
            parent_run_id,
            parent_session_id,
            root.to_string_lossy(),
            model_id,
            chrono::Utc::now().to_rfc3339(),
            evidence_ref_count as i64,
            kind,
            workspace_copy,
            resumed_from,
            identity.task_id,
            attempt_id,
            identity.purpose,
            identity.batch_id,
            identity.contract_id,
            identity.contract_schema,
        ],
    )
    .map_err(|error| error.to_string())?;
    Ok(child_run_id)
}

/// resume_child 专用加载：必须带会话归属谓词——child_run_id 是会话内凭据，
/// 跨会话命中按"未找到"处理，不能读取/复活其他会话的历史与工作区副本
/// （与 child_writer.apply 的会话校验同纪律）。返回
/// (status, kind, workspace_copy, message_history_json, model_id, workspace_root, result_json, identity)。
#[allow(clippy::type_complexity)]
pub(crate) fn load_child_run_for_resume(
    db: &DbState,
    child_run_id: &str,
    parent_session_id: &str,
) -> Result<(String, String, Option<String>, Option<String>, String, String, Option<String>, ChildRunIdentity), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.query_row(
        "SELECT status, kind, workspace_copy, message_history_json, model_id, workspace_root, result_json, COALESCE(task_id, id), COALESCE(attempt_id, 1), purpose, batch_id, contract_id, contract_schema FROM agent_child_runs WHERE id = ?1 AND parent_session_id = ?2",
        params![child_run_id, parent_session_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                ChildRunIdentity {
                    task_id: row.get::<_, String>(7)?,
                    attempt_id: row.get::<_, i64>(8)?.max(1) as u32,
                    purpose: row.get::<_, Option<String>>(9)?,
                    batch_id: row.get::<_, Option<String>>(10)?,
                    contract_id: row.get::<_, Option<String>>(11)?,
                    contract_schema: row.get::<_, Option<String>>(12)?,
                },
            ))
        },
    )
    .map_err(|e| format!("未找到子任务「{}」记录: {}", child_run_id, e))
}

pub(crate) fn finish_child_run(
    db: &DbState,
    child_run_id: &str,
    status: &str,
    duration_ms: u64,
    completion_tokens: i64,
    prompt_tokens: i64,
    error_kind: Option<ChildErrorKind>,
    result_json: Option<&str>,
) {
    finish_child_run_with_history(
        db,
        child_run_id,
        status,
        duration_ms,
        completion_tokens,
        prompt_tokens,
        error_kind,
        result_json,
        None,
    );
}

pub(crate) fn finish_child_run_with_history(
    db: &DbState,
    child_run_id: &str,
    status: &str,
    duration_ms: u64,
    completion_tokens: i64,
    prompt_tokens: i64,
    error_kind: Option<ChildErrorKind>,
    result_json: Option<&str>,
    message_history_json: Option<&str>,
) {
    let Ok(conn) = db.0.lock() else {
        return;
    };
    let _ = conn.execute(
        // 单向迁移：running -> 终态；例外——被 reap 标记为 reaped_stale 的行仍允许
        // 迟到结果落库（真实结论覆盖收割占位结果），归档元数据与会话同寿命保留，
        // 复盘可查"曾超时"与"实际结论"双重事实；cancelled/interrupted 依旧拒绝覆盖。
        "UPDATE agent_child_runs SET status=?2, finished_at=?3, duration_ms=?4, completion_tokens=?5, prompt_tokens=?9, error_kind=?6, result_json=?7, message_history_json=?8 WHERE id=?1 AND (status='running' OR (status='timed_out' AND error_kind='reaped_stale'))",
        params![
            child_run_id,
            status,
            chrono::Utc::now().to_rfc3339(),
            duration_ms as i64,
            completion_tokens,
            error_kind.map(|k| k.db_value()),
            result_json,
            message_history_json,
            prompt_tokens,
        ],
    );
    // ⚡ 唤醒挂起等待中的协程（watch 版本号广播，不存在丢失通知的竞态窗口）
    crate::agent::job_engine::notify_job_state_changed();
}

/// 为失败/超时/取消的子审查构造可被信箱消费的结果 JSON。
/// 统一让所有终态都携带 result_json，消费机制（置 NULL）才能对所有终态生效。
/// invalid_output 专用：在标准错误结果上注入审查员原始输出（截断 4000 字符），
/// 补"invalid 无从诊断"的盲区——宽松解析时代仍失败的案例，raw 是唯一证据。
pub(crate) fn child_invalid_result_json(
    child_run_id: &str,
    duration_ms: u64,
    completion_tokens: i64,
    prompt_tokens: i64,
    raw: &str,
) -> String {    let mut value: serde_json::Value = serde_json::from_str(&child_error_result_json(
        child_run_id,
        "invalid_output",
        &ChildErrorKind::InvalidOutput,
        duration_ms,
        completion_tokens,
        prompt_tokens,
        None,
    ))
    .unwrap_or(serde_json::json!({}));
    let truncated: String = raw.chars().take(4000).collect();
    value["raw_result_json"] = serde_json::Value::String(truncated);
    value.to_string()
}

pub(crate) fn child_error_result_json(
    child_run_id: &str,
    status: &str,
    error_kind: &ChildErrorKind,
    duration_ms: u64,
    completion_tokens: i64,
    prompt_tokens: i64,
    error_detail: Option<&str>,
) -> String {
    // 请求层失败把真实错误（如 "402 Insufficient Balance"）带进 missing_evidence：
    // 主 Agent/人工呈报环节能看到死因，决定重派、换模型还是先修上游，而不是对着
    // 干巴巴的 kind 猜。（截断防超长错误体撑爆注入）
    let missing_msg = match error_detail {
        Some(detail) => {
            let truncated: String = detail.chars().take(240).collect();
            format!("子审查任务未能产出结论（{}: {}）", error_kind, truncated)
        }
        None => format!("子审查任务未能产出结论（{}）", error_kind),
    };
    serde_json::to_string(&ChildReviewResult {
        child_run_id: child_run_id.to_string(),
        status: status.to_string(),
        findings: vec![],
        confidence: "unverified".to_string(),
        evidence_refs: vec![],
        missing_evidence: vec![missing_msg],
        duration_ms,
        completion_tokens,
        prompt_tokens,
        error_kind: Some(error_kind.to_string()),
        raw_result_json: None,
        kind: None,
        routing: Some("blocked".to_string()),
        withdrawn_findings: vec![],
        objection_verdicts: vec![],
        scorecard: None,
        task_id: None,
        attempt_id: None,
    })
    .unwrap_or_default()
}
