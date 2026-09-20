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


pub fn poll_completed_child_runs_filtered(
    db: &DbState,
    parent_run_id: &str,
    targets: Option<&[String]>,
) -> Result<Vec<ChildReviewResult>, String> {
    // 每次收割前先巡检僵尸 running 行（panic/悬挂兜底，见 reap_stale_child_runs）
    reap_stale_child_runs(db, parent_run_id);
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, result_json, kind, COALESCE(task_id, id), COALESCE(attempt_id, 1) FROM agent_child_runs
             WHERE parent_run_id = ?1
               AND status IN ('succeeded', 'failed', 'cancelled', 'timed_out', 'interrupted', 'invalid_output')
               AND result_json IS NOT NULL
               AND (consumed_at IS NULL OR consumed_at = '')",
        )
        .map_err(|error| error.to_string())?;
    let rows: Vec<(String, String, String, String, i64)> = stmt
        .query_map(params![parent_run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    let mut results = Vec::new();
    let now = chrono::Utc::now().to_rfc3339();
    for (id, json_str, kind, task_id, attempt_id) in rows {
        // 定向模式：非目标 ID 的结果不消费，保留给后续 wait_jobs 或信箱轮询
        if let Some(targets) = targets {
            if !targets.iter().any(|t| t == &id) {
                continue;
            }
        }
        match serde_json::from_str::<ChildReviewResult>(&json_str) {
            Ok(mut result) => {
                result.raw_result_json = Some(json_str);
                result.kind = Some(kind);
                result.task_id = Some(task_id);
                result.attempt_id = Some(attempt_id.max(1) as u32);
                results.push(result);
                let _ = conn.execute(
                    "UPDATE agent_child_runs SET consumed_at = ?1 WHERE id = ?2",
                    params![now, id],
                );
            }
            Err(error) => {
                // 损坏的结果行无法被消费，标记 consumed_at 让该行退出信箱，同时保留 result_json 供排查。
                eprintln!("[child-reviewer] 结果 JSON 解析失败，跳过损坏行 {}: {}", id, error);
                let _ = conn.execute(
                    "UPDATE agent_child_runs SET consumed_at = ?1 WHERE id = ?2",
                    params![now, id],
                );
            }
        }
    }
    Ok(results)
}

/// 轮询已完成的子审查任务，返回尚未被消费的结果（消费全部终态，供主循环自动注入）。
pub fn poll_completed_child_runs(
    db: &DbState,
    parent_run_id: &str,
) -> Result<Vec<ChildReviewResult>, String> {
    poll_completed_child_runs_filtered(db, parent_run_id, None)
}

/// 轮询已完成的子任务，返回通用交付回执列表 (DeliveryReceipt)。
pub fn poll_completed_delivery_receipts(
    db: &DbState,
    parent_run_id: &str,
    targets: Option<&[String]>,
) -> Result<Vec<crate::agent::protocol::DeliveryReceipt>, String> {
    let raw = poll_completed_child_runs_filtered(db, parent_run_id, targets)?;
    Ok(raw.into_iter().map(Into::into).collect())
}


/// 检查当前 parent_run 是否仍有正在执行中的子审查任务
/// 运行期僵尸巡检：running 行超过对应 kind 预算的宽限上限后收敛为 timed_out，
/// 并写入可消费的 result_json。兜底「子任务 panic / spawn 任务悬挂导致行永远
/// 停在 running → 交叉审查门禁被一具僵尸锁死到重启」的死锁路径。
/// 宽限上限 = 各 kind 自身预算的数倍，正常慢任务不会误伤。
pub fn reap_stale_child_runs(db: &DbState, parent_run_id: &str) -> usize {
    let Ok(conn) = db.0.lock() else { return 0; };
    let rows: Vec<(String, Option<String>, String)> = {
        let mut stmt = match conn.prepare(
            "SELECT id, kind, started_at FROM agent_child_runs WHERE parent_run_id = ?1 AND status = 'running'",
        ) {
            Ok(stmt) => stmt,
            Err(_) => return 0,
        };
        stmt.query_map(params![parent_run_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, String>(2)?))
        })
        .map(|mapped| mapped.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    };
    let now = chrono::Utc::now();
    // reviewer 宽限可通过 channel-config.json 的 reviewer_reap_grace_secs 调整
    // （异构大家族模型在长上下文上推理超 10 分钟并不罕见，默认 600s 偏紧时可调高）
    // 配置不变式：reap 宽限必须严格大于 reviewer 单次超时，否则会出现
    // "超时被 reap 成 timed_out 但子进程还在跑、迟到结果被状态守卫丢弃"的边角。
    let reviewer_budget_secs: i64 = {
        let configured = crate::agent::channel::channel_config_shared()
            .read()
            .ok()
            .and_then(|cfg| cfg.reviewer_reap_grace_secs)
            .map(|v| v.min(3600) as i64)
            .unwrap_or(600);
        configured.max(max_child_timeout_secs() as i64 + 30)
    };
    let mut reaped = 0usize;
    for (child_run_id, kind, started_at) in rows {
        let budget_secs: i64 = match kind.as_deref() {
            Some("writer") => 1200,
            Some("worker") => 900,
            _ => reviewer_budget_secs,
        };
        let age_secs = chrono::DateTime::parse_from_rfc3339(&started_at)
            .map(|t| (now - t.with_timezone(&chrono::Utc)).num_seconds())
            .unwrap_or(0);
        if age_secs <= budget_secs {
            continue;
        }
        let _ = conn.execute(
            "UPDATE agent_child_runs SET status='timed_out', finished_at=?2, error_kind='reaped_stale', result_json=?3 WHERE id=?1 AND status='running'",
            params![
                child_run_id,
                now.to_rfc3339(),
                child_error_result_json(&child_run_id, "timed_out", &ChildErrorKind::ReapedStale, age_secs.max(0) as u64 * 1000, 0, 0, None)
            ],
        );
        // 清理日志：reap 只做标记不撕关联——迟到结果仍允许落库（见 finish 守卫），
        // 复盘时能看到"该行曾被收割"与"真实迟到结论"两段完整事实。
        println!(
            "[reap] child_run={} kind={:?} age={}s → timed_out(reaped_stale)，行保留待迟到结果落库",
            child_run_id, kind, age_secs
        );
        reaped += 1;
    }
    if reaped > 0 {
        crate::agent::job_engine::notify_job_state_changed();
    }
    reaped
}

pub fn has_running_child_runs(db: &DbState, parent_run_id: &str) -> bool {
    let Ok(conn) = db.0.lock() else { return false; };
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_child_runs WHERE parent_run_id = ?1 AND status = 'running'",
            params![parent_run_id],
            |row| row.get(0),
        )
        .unwrap_or(0);
    count > 0
}

/// 检查当前 parent_run 是否存在「已到终态但尚未被回收」的审查结论（wait_jobs 可直接收取）。
///
/// 与 `has_running_child_runs` 互补：前者代表结论还在路上，后者代表结论已产出但还没进入主 Agent 上下文。
/// 门禁据此区分「应当挂起等待」与「应当重新派发」，避免模型对着空信箱反复 wait_jobs。
pub fn has_pending_review_results(db: &DbState, parent_run_id: &str) -> bool {
    let Ok(conn) = db.0.lock() else { return false; };
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_child_runs
             WHERE parent_run_id = ?1
               AND status IN ('succeeded', 'failed', 'cancelled', 'timed_out', 'interrupted', 'invalid_output')
               AND result_json IS NOT NULL
               AND (consumed_at IS NULL OR consumed_at = '')
               AND (kind IS NULL OR kind = '' OR kind = 'review')",
            params![parent_run_id],
            |row| row.get(0),
        )
        .unwrap_or(0);
    count > 0
}

/// 将指定 parent_run 下所有仍处于 running 状态的子审查任务标记为 cancelled
pub fn cancel_running_child_runs(db: &DbState, parent_run_id: &str) -> Result<usize, String> {
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    let now = chrono::Utc::now().to_rfc3339();
    let rows_updated = conn
        .execute(
            "UPDATE agent_child_runs
             SET status = 'cancelled', finished_at = ?2, error_kind = 'cancelled'
             WHERE parent_run_id = ?1 AND status = 'running'",
            params![parent_run_id, now],
        )
        .map_err(|error| error.to_string())?;
    if rows_updated > 0 {
        // 与 cancel_all_running_jobs 对齐：状态收敛必须广播，否则挂在
        // wait_for_jobs 里的协程只能靠 180s 超时兜底才能醒来。
        crate::agent::job_engine::notify_job_state_changed();
    }
    Ok(rows_updated)
}

/// 会话重新启动时，将历史异常未退出的 running 子审查任务收敛为 interrupted。
/// 与 job_engine::recover_orphaned_jobs_on_session_start 配对：进程崩溃后
/// spawn 出去的 tokio 任务已不存在，若不收敛，这些行将永远停留在 running。
pub fn recover_orphaned_child_runs_on_session_start(
    db: &DbState,
    parent_session_id: &str,
) -> Result<usize, String> {
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    let stale_ids: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT id FROM agent_child_runs
                 WHERE parent_session_id = ?1 AND status = 'running'",
            )
            .map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map(params![parent_session_id], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };
    let now = chrono::Utc::now().to_rfc3339();
    for child_run_id in &stale_ids {
        let _ = conn.execute(
            "UPDATE agent_child_runs
             SET status = 'interrupted', finished_at = ?2, error_kind = 'interrupted',
                 result_json = ?3
             WHERE id = ?1",
            params![
                child_run_id,
                now,
                child_error_result_json(child_run_id, "interrupted", &ChildErrorKind::Interrupted, 0, 0, 0, None)
            ],
        );
    }
    if !stale_ids.is_empty() {
        crate::agent::job_engine::notify_job_state_changed();
    }
    Ok(stale_ids.len())
}

/// 子任务结果的模型可见投影：强类型字段全量保留，raw_result_json 里的
/// custom_schema 扩展字段（角色特化产物，如 cve_refs/risk_level）也合并进来——
/// 投影要收窄的是体积，不是语义；强类型外字段丢失会让角色卡契约形同虚设。
pub fn result_as_dispatch_output(result: ChildReviewResult) -> String {
    let mut view = json!({
        "child_run_id": result.child_run_id,
        "kind": result.kind,
        "status": result.status,
        "findings": result.findings,
        "confidence": result.confidence,
        "evidence_refs": result.evidence_refs,
        "missing_evidence": result.missing_evidence,
        "duration_ms": result.duration_ms,
        "completion_tokens": result.completion_tokens,
        "prompt_tokens": result.prompt_tokens,
        "error_kind": result.error_kind,
        "routing": result.routing,
        "withdrawn_findings": result.withdrawn_findings,
        "objection_verdicts": result.objection_verdicts,
        "scorecard": result.scorecard,
    });
    if let (Some(raw), Some(view_map)) = (
        result.raw_result_json.as_deref(),
        view.as_object_mut(),
    ) {
        if let Ok(serde_json::Value::Object(raw_map)) = serde_json::from_str::<Value>(raw) {
            for (k, v) in raw_map {
                view_map.entry(k).or_insert(v);
            }
        }
    }
    view.to_string()
}

pub fn child_result_token_usage(result: &ChildReviewResult) -> TokenUsage {
    TokenUsage {
        prompt_tokens: result.prompt_tokens,
        completion_tokens: result.completion_tokens,
        total_tokens: result.prompt_tokens + result.completion_tokens,
        ..TokenUsage::default()
    }
}

