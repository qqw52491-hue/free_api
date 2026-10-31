use crate::db::DbState;
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 异步后台任务描述
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackgroundJob {
    pub id: String,
    pub parent_session_id: String,
    pub parent_run_id: String,
    pub job_type: String,
    pub title: String,
    pub params_json: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub result_json: Option<String>,
    pub error_kind: Option<String>,
    pub consumed: bool,
}

/// 注入大模型上下文的已完成任务结果
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackgroundJobResult {
    pub job_id: String,
    pub job_type: String,
    pub title: String,
    pub status: String,
    pub duration_ms: u64,
    pub result_json: String,
    pub error_kind: Option<String>,
    /// 子任务的逻辑任务身份（agent_child_runs.task_id 透出；通用后台任务为 None）
    pub task_id: Option<String>,
    /// 同逻辑任务的第几次尝试（通用后台任务恒为 1）
    pub attempt_id: u32,
}

impl BackgroundJobResult {
    /// 归一为 DeliveryReceipt：wait_jobs / 后台信箱等传输层的结果统一转成
    /// 交付回执，消费语义只由 reducer（child_receipt）决定，与运输通道无关。
    /// job_type → kind 映射：含 "writer"/"worker"/"review" 的归为对应子任务类型，
    /// 其余原样保留（reducer 对未知类型只做投影、不做状态变更）。
    pub fn to_delivery_receipt(&self) -> crate::agent::protocol::DeliveryReceipt {
        let payload = serde_json::from_str::<serde_json::Value>(&self.result_json).ok();
        let kind = if self.job_type=="child_task" {"task".to_string()} else if self.job_type.contains("writer") {
            "writer".to_string()
        } else if self.job_type.contains("worker") {
            "worker".to_string()
        } else if self.job_type.contains("review") {
            "review".to_string()
        } else {
            self.job_type.clone()
        };
        crate::agent::protocol::DeliveryReceipt {
            child_run_id: self.job_id.clone(),
            task_id: self.task_id.clone().unwrap_or_else(|| self.job_id.clone()),
            attempt_id: self.attempt_id.max(1),
            kind,
            status: self.status.clone(),
            duration_ms: self.duration_ms,
            completion_tokens: payload
                .as_ref()
                .and_then(|p| p.get("completion_tokens").and_then(|v| v.as_i64()))
                .unwrap_or(0),
            prompt_tokens: payload
                .as_ref()
                .and_then(|p| p.get("prompt_tokens").and_then(|v| v.as_i64()))
                .unwrap_or(0),
            error_kind: self.error_kind.clone(),
            payload,
            raw_text: Some(self.result_json.clone()),
        }
    }

    /// 模型可见有界投影：保存与展示分离——完整 result_json 持久化在任务
    /// 账本里，模型侧只给收敛视图。writer 走结构化视图（diff 已由
    /// build_changed_files 分级降级，全量补丁经 patch_ref 工件取回）；
    /// 其余类型按字符硬上限截断，防止一个巨型回执淹掉后续所有上下文。
    /// wait_jobs 输出与主循环后台信箱倾倒共用本投影。
    pub fn model_projection(&self) -> String {
        const JOB_RESULT_PROJECTION_MAX_CHARS: usize = 16_000;
        if self.result_json.is_empty() {
            return "（无附加输出）".to_string();
        }
        if self.job_type == "child_writer" {
            if let Ok(wr) =
                serde_json::from_str::<crate::agent::child_writer::WriterResult>(&self.result_json)
            {
                return crate::agent::child_writer::writer_result_model_view(&wr);
            }
        }
        let total = self.result_json.chars().count();
        if total <= JOB_RESULT_PROJECTION_MAX_CHARS {
            return self.result_json.clone();
        }
        let head: String = self
            .result_json
            .chars()
            .take(JOB_RESULT_PROJECTION_MAX_CHARS)
            .collect();
        format!(
            "{}\n…（输出共 {} 字符，已截断展示；完整结果持久化于任务账本 job_id={}）",
            head, total, self.job_id
        )
    }
}

/// 登记并启动一个新的后台异步任务，返回 job_id
pub fn begin_job(
    db: &DbState,
    parent_run_id: &str,
    parent_session_id: &str,
    job_type: &str,
    title: &str,
    params_json: &str,
) -> Result<String, String> {
    let job_id = format!("job-{}", Uuid::new_v4());
    let now = Utc::now().to_rfc3339();
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    conn.execute(
        "INSERT INTO agent_background_jobs 
         (id, parent_session_id, parent_run_id, job_type, title, params_json, status, started_at, consumed)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running', ?7, 0)",
        params![
            job_id,
            parent_session_id,
            parent_run_id,
            job_type,
            title,
            params_json,
            now
        ],
    )
    .map_err(|e| e.to_string())?;

    drop(conn);
    crate::agent::work_history::bind_async(db, parent_session_id, &job_id, None);
    Ok(job_id)
}

use std::sync::LazyLock;
use std::time::Duration;
use tokio::sync::watch;

/// 全局任务状态版本号：任何任务终态落盘时 +1，唤醒所有挂起等待者。
/// 基于 watch 而非 Notify：版本号持久保存最新值，等待者先标记已读版本再轮询信箱，
/// 两条路径恰好覆盖竞态窗口两侧，「初查」与「注册等待」之间完成的任务不会丢通知。
static JOB_WATCH: LazyLock<(watch::Sender<u64>, watch::Receiver<u64>)> =
    LazyLock::new(|| watch::channel(0));

/// 任务状态发生变化（完成/取消/恢复收敛）时，唤醒所有挂起等待中的协程
pub fn notify_job_state_changed() {
    JOB_WATCH.0.send_modify(|version| *version = version.wrapping_add(1));
}

pub(crate) fn job_watch_receiver() -> watch::Receiver<u64> {
    JOB_WATCH.1.clone()
}

/// 结束后台异步任务，写入完成状态与结果，并广播唤醒所有等待者
pub fn finish_job(
    db: &DbState,
    job_id: &str,
    status: &str,
    duration_ms: u64,
    result_json: Option<&str>,
    error_kind: Option<&str>,
) -> Result<(), String> {
    let now = Utc::now().to_rfc3339();
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    let changed = conn.execute(
        "UPDATE agent_background_jobs 
         SET status = ?2, finished_at = ?3, duration_ms = ?4, result_json = ?5, error_kind = ?6
         WHERE id = ?1 AND status = 'running'",
        params![job_id, status, now, duration_ms as i64, result_json, error_kind],
    )
    .map_err(|e| e.to_string())?;

    let session: Option<String> = conn.query_row("SELECT parent_session_id FROM agent_background_jobs WHERE id=?1", [job_id], |r| r.get(0)).ok();
    drop(conn);
    if changed > 0 {
        if let Some(session) = session {
            let raw = serde_json::json!({"job_id":job_id,"status":status,"result":result_json,"error_kind":error_kind}).to_string();
            crate::agent::work_history::record_async_result(db, &session, job_id, status, &raw);
        }
    }

    // ⚡ 广播唤醒挂起等待中的协程
    notify_job_state_changed();

    Ok(())
}

/// 轮询待消费信箱 (Pending Inbox)，批量获取未消费的已完成任务并原子置 consumed = 1
pub fn poll_pending_inbox(
    db: &DbState,
    parent_run_id: &str,
    max_batch: usize,
) -> Result<Vec<BackgroundJobResult>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    let limit = if max_batch == 0 { 5 } else { max_batch };
    let mut stmt = conn
        .prepare(
            "SELECT id, job_type, title, status, duration_ms, result_json, error_kind 
             FROM agent_background_jobs
             WHERE parent_run_id = ?1 
               AND consumed = 0 
               AND status IN ('succeeded', 'failed', 'cancelled', 'timed_out', 'interrupted')
             ORDER BY started_at ASC
             LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;

    let rows: Vec<(String, String, String, String, Option<i64>, Option<String>, Option<String>)> = stmt
        .query_map(params![parent_run_id, limit as i64], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    let mut results = Vec::new();
    for (id, job_type, title, status, duration_ms, result_json, error_kind) in rows {
        let _ = conn.execute(
            "UPDATE agent_background_jobs SET consumed = 1 WHERE id = ?1",
            params![id],
        );

        results.push(BackgroundJobResult {
            job_id: id,
            job_type,
            title,
            status,
            duration_ms: duration_ms.unwrap_or(0) as u64,
            result_json: result_json.unwrap_or_default(),
            error_kind,
            task_id: None,
            attempt_id: 1,
        });
    }

    Ok(results)
}

/// 定向轮询信箱：只消费 `target_job_ids` 列表中指定 ID 的已完成任务，不影响其余任务的 consumed 状态。
///
/// 用于 `wait_jobs` 指定等待特定任务时，防止误消费与本次等待无关的后台任务结果。
pub fn poll_targeted_inbox(
    db: &DbState,
    parent_run_id: &str,
    target_job_ids: &[String],
) -> Result<Vec<BackgroundJobResult>, String> {
    if target_job_ids.is_empty() {
        return Ok(vec![]);
    }

    let conn = db.0.lock().map_err(|e| e.to_string())?;

    let mut results = Vec::new();
    for job_id in target_job_ids {
        let row: Result<(String, String, String, String, Option<i64>, Option<String>, Option<String>), _> = conn
            .query_row(
                "SELECT id, job_type, title, status, duration_ms, result_json, error_kind \
                 FROM agent_background_jobs \
                 WHERE id = ?1 AND parent_run_id = ?2 AND consumed = 0 \
                   AND status IN ('succeeded', 'failed', 'cancelled', 'timed_out', 'interrupted')",
                params![job_id, parent_run_id],
                |row| Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                )),
            );
        if let Ok((id, job_type, title, status, duration_ms, result_json, error_kind)) = row {
            let _ = conn.execute(
                "UPDATE agent_background_jobs SET consumed = 1 WHERE id = ?1",
                params![id],
            );
            results.push(BackgroundJobResult {
                job_id: id,
                job_type,
                title,
                status,
                duration_ms: duration_ms.unwrap_or(0) as u64,
                result_json: result_json.unwrap_or_default(),
                error_kind,
                task_id: None,
                attempt_id: 1,
            });
        }
    }

    Ok(results)
}

/// 检查指定 parent_run 是否仍有处于 running 状态的异步任务（包含通用任务与子审查任务）
pub fn has_any_running_jobs(db: &DbState, parent_run_id: &str) -> bool {
    has_running_jobs(db, parent_run_id) || crate::agent::child_reviewer::has_running_child_runs(db, parent_run_id)
}

/// 定向语义：给定目标 id 集合中是否还有处于 running 的任务（两张表都查）。
/// 定向 wait 的早退判据必须是「目标是否仍在途」，不能用全局 running 计数——
/// 否则目标已全部终态/已消费、只有无关任务在跑时，调用方会被迫空等满超时。
fn has_running_targets(db: &DbState, parent_run_id: &str, targets: &[String]) -> bool {
    if targets.is_empty() {
        return false;
    }
    let Ok(conn) = db.0.lock() else { return false; };
    let marks = std::iter::repeat("?")
        .take(targets.len())
        .collect::<Vec<_>>()
        .join(",");
    for table in ["agent_background_jobs", "agent_child_runs"] {
        let sql = format!(
            "SELECT COUNT(*) FROM {} WHERE parent_run_id = ? AND status = 'running' AND id IN ({})",
            table, marks
        );
        let params_vec: Vec<&dyn rusqlite::ToSql> = std::iter::once(&parent_run_id as &dyn rusqlite::ToSql)
            .chain(targets.iter().map(|t| t as &dyn rusqlite::ToSql))
            .collect();
        let count: i64 = conn
            .query_row(&sql, rusqlite::params_from_iter(params_vec), |row| row.get(0))
            .unwrap_or(0);
        if count > 0 {
            return true;
        }
    }
    false
}

/// 批内去重投递：同一 job_id 在一次 wait 内只入列一次。
/// 子任务回执的 applied_at 要等消费方 checkpoint 提交才写，行在此之前
/// 会在多轮收割里重复出现；信箱 consumed 置位先于返回，但兜底仍按 id 幂等。
fn push_dedup(
    collected: &mut Vec<BackgroundJobResult>,
    seen: &mut std::collections::HashSet<String>,
    jobs: Vec<BackgroundJobResult>,
) {
    for job in jobs {
        if seen.insert(job.job_id.clone()) {
            collected.push(job);
        }
    }
}

/// 将子审查/工兵/侦察终态回执映射为统一的信箱结果条目
fn collect_completed_child_results(
    db: &DbState,
    parent_run_id: &str,
    target_job_ids: Option<&[String]>,
) -> Vec<BackgroundJobResult> {
    match crate::agent::child_reviewer::poll_completed_delivery_receipts(db, parent_run_id, target_job_ids) {
        Ok(receipts) => receipts
            .into_iter()
            .map(|receipt| {
                let role_label = receipt
                    .payload
                    .as_ref()
                    .and_then(|p| p.get("role_label").or_else(|| p.get("role")).and_then(|v| v.as_str()))
                    .map(|s| s.to_string());
                let (job_type, default_title) = match receipt.kind.as_str() {
                    "writer" => ("child_writer".to_string(), "✍️ 隔离沙箱写入工兵".to_string()),
                    "worker" => ("child_worker".to_string(), "🔍 独立只读侦察专员".to_string()),
                    "task" => ("child_task".to_string(), "通用任务".to_string()),
                    _ => ("child_reviewer".to_string(), "🛡️ 跨模型综合审查官".to_string()),
                };
                let title = role_label.unwrap_or(default_title);
                let result_json = receipt
                    .raw_text
                    .clone()
                    .or_else(|| receipt.payload.as_ref().map(|p| serde_json::to_string(p).unwrap_or_default()))
                    .unwrap_or_else(|| serde_json::to_string(&receipt).unwrap_or_default());
                BackgroundJobResult {
                    result_json,
                    job_id: receipt.child_run_id.clone(),
                    job_type,
                    title,
                    status: receipt.status,
                    duration_ms: receipt.duration_ms,
                    error_kind: receipt.error_kind,
                    task_id: Some(receipt.task_id.clone()),
                    attempt_id: receipt.attempt_id,
                }
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// 在向调用方交付后台任务结果瞬间，重新以主工作区真实状态现算并刷新冲突指纹（闭合 TOC/TOU 竞争窗口）
async fn enrich_and_refresh_collected_results(db: &DbState, collected: &mut [BackgroundJobResult]) {
    for job in collected.iter_mut() {
        if job.job_type == "child_writer" {
            let parent_ws_opt = {
                let Ok(conn) = db.0.lock() else { continue; };
                conn.query_row(
                    "SELECT workspace_root FROM agent_child_runs WHERE id = ?1",
                    rusqlite::params![job.job_id],
                    |r| r.get::<_, String>(0),
                )
                .ok()
            };
            if let Some(ws_path) = parent_ws_opt {
                let parent_path = std::path::PathBuf::from(ws_path);
                if let Ok(mut writer_res) = serde_json::from_str::<crate::agent::child_writer::WriterResult>(&job.result_json) {
                    let git_available = crate::agent::child_writer::is_git_available(&parent_path).await;
                    crate::agent::child_writer::refresh_file_conflicts(&mut writer_res.changed_files, &parent_path, git_available).await;
                    if let Ok(new_json) = serde_json::to_string(&writer_res) {
                        job.result_json = new_json;
                    }
                }
            }
        }
    }
}

/// 与 wait_jobs 的自适应窗口共用上限；完成通知或用户中断会提前唤醒。
pub(crate) const MAX_WAIT_SECS: u64 = 600;

pub(crate) fn wait_timeout_secs(requested: u64, streak: u32) -> u64 {
    requested.clamp(1, MAX_WAIT_SECS)
        .saturating_mul(1u64 << streak.min(3))
        .min(MAX_WAIT_SECS)
}

/// 空回收结果只表示本次没有收到新回执；按同一 run / 同一目标范围查两类任务，
/// 不能用「没有子审查在跑」推断「后台 shell 已完成」。查询失败也不声称全部完成。
pub(crate) fn empty_wait_message(
    db: &DbState,
    parent_run_id: &str,
    targets: Option<&[String]>,
    waited_secs: u64,
) -> Result<String, String> {
    if crate::agent::agent_mailbox::has_pending(db,parent_run_id) {return Ok("收到协作消息，等待已提前返回；原后台任务继续运行，消息将在当前安全边界注入。".into())}
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare(
        "SELECT id, status, consumed, 1 FROM agent_background_jobs WHERE parent_run_id = ?1
         UNION ALL
         SELECT id, status, CASE WHEN applied_at IS NULL OR applied_at = '' THEN 0 ELSE 1 END,
                CASE WHEN result_json IS NULL THEN 0 ELSE 1 END
         FROM agent_child_runs WHERE parent_run_id = ?1"
    ).map_err(|e| e.to_string())?;
    let rows = stmt.query_map(params![parent_run_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?,
            row.get::<_, bool>(2)?, row.get::<_, bool>(3)?))
    }).map_err(|e| e.to_string())?;
    let mut matched = std::collections::HashSet::new();
    let mut running = 0;
    let mut pending = 0;
    let mut unavailable = 0;
    for row in rows {
        let (id, status, consumed, has_result) = row.map_err(|e| e.to_string())?;
        if targets.is_some_and(|ids| !ids.contains(&id)) { continue; }
        matched.insert(id);
        if status == "running" {
            running += 1;
        } else if !consumed {
            if has_result && matches!(status.as_str(), "succeeded" | "failed" | "cancelled" | "timed_out" | "interrupted" | "invalid_output") {
                pending += 1;
            } else {
                unavailable += 1;
            }
        }
    }
    let mut parts = Vec::new();
    if running > 0 {
        parts.push(format!("仍有 {} 个目标后台任务在运行；本次等待窗口为 {}s，未收到新结果。可继续 wait_jobs 等待同一任务，或推进其他独立工作；等待窗口结束不代表任务失败，无需重新启动该任务。", running, waited_secs));
    }
    if pending > 0 {
        parts.push(format!("有 {} 个目标任务已到终态、结果尚未回收，可再次 wait_jobs 收取。", pending));
    }
    if unavailable > 0 {
        parts.push(format!("有 {} 个目标任务尚无可回收结果，不能据此确认任务完成。", unavailable));
    }
    if let Some(ids) = targets {
        let missing: std::collections::HashSet<_> = ids.iter().filter(|id| !matched.contains(*id)).collect();
        if !missing.is_empty() {
            parts.push(format!("当前 run 未找到 {} 个指定任务 ID，请核对派发返回的 ID。", missing.len()));
        }
    }
    if parts.is_empty() {
        parts.push(if matched.is_empty() {
            "当前等待范围内没有已登记的后台任务。".to_string()
        } else {
            "当前等待范围内的任务均已到终态且结果已回收；本次没有新结果，任务是否成功请以已回收的结果为准。".to_string()
        });
    }
    Ok(format!("等待结束：{}", parts.join("\n")))
}

/// 挂起当前协程等待后台异步任务完成，0 大模型 API 调用与 0 CPU 轮询消耗
///
/// 等待范围同时覆盖通用后台任务信箱与子审查（child_reviewer）终态结果：
/// - 若 `target_job_ids` 为 None：等待 parent_run 下所有任务，信箱里有任何完成的立刻返回。
/// - 若 `target_job_ids` 为 Some：**只消费指定 ID 的任务**；信箱里不相关的任务不会被 consumed，
///   保留给后续其他 wait_jobs 或信箱轮询消费，避免误吃他人结果。
pub async fn wait_for_jobs(
    db: &DbState,
    parent_run_id: &str,
    target_job_ids: Option<&[String]>,
    timeout_secs: u64,
    mut interrupt_rx: Option<&mut tokio::sync::watch::Receiver<u64>>,
) -> Result<Vec<BackgroundJobResult>, String> {
    let timeout_duration = Duration::from_secs(wait_timeout_secs(timeout_secs, 0));
    let start_time = std::time::Instant::now();

    let found_target = |results: &[BackgroundJobResult]| -> bool {
        match target_job_ids {
            None => !results.is_empty(),
            Some(targets) => results.iter().any(|job| targets.contains(&job.job_id)),
        }
    };

    // 0. 先标记通知版本为已读，再拉取信箱：任何在「初查」与「注册等待」之间完成的任务
    //    要么已被初查捞到，要么必然触发 changed()，不存在丢通知的竞态窗口。
    //    （borrow_and_update 调用瞬间即标记已读，Ref 守卫随语句结束立即释放）
    let mut job_rx = job_watch_receiver();
    let _ = job_rx.borrow_and_update();

    // 1. 初查：通用信箱 + 已到终态的子审查结果
    //    applied_at 未随 checkpoint 落账前，同一行会在本轮多次收割中重复出现
    //    （投递 ≠ 落账），按 job_id 去重保证一次等待内不重复入账。
    let mut collected = Vec::new();
    let mut seen_job_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    match target_job_ids {
        None => {
            if let Ok(immediate) = poll_pending_inbox(db, parent_run_id, 10) {
                push_dedup(&mut collected, &mut seen_job_ids, immediate);
            }
        }
        Some(targets) => {
            if let Ok(targeted) = poll_targeted_inbox(db, parent_run_id, targets) {
                push_dedup(&mut collected, &mut seen_job_ids, targeted);
            }
        }
    }
    push_dedup(
        &mut collected,
        &mut seen_job_ids,
        collect_completed_child_results(db, parent_run_id, target_job_ids),
    );

    // 早退判据：无目标时看全局 running；有目标时只看目标是否仍在途——
    // 无关任务还在跑不构成继续等待的理由。
    let no_pending = || match target_job_ids {
        None => !has_any_running_jobs(db, parent_run_id),
        Some(targets) => !has_running_targets(db, parent_run_id, targets),
    };

    if crate::agent::agent_mailbox::has_pending(db,parent_run_id) || found_target(&collected) || no_pending() {
        enrich_and_refresh_collected_results(db, &mut collected).await;
        return Ok(collected);
    }

    // 2. 反应式休眠：监听全局版本号变更、超时与打断
    while start_time.elapsed() < timeout_duration {
        let remaining = timeout_duration.saturating_sub(start_time.elapsed());
        if remaining.is_zero() {
            break;
        }

        tokio::select! {
            changed = job_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                let _ = job_rx.borrow_and_update();
                match target_job_ids {
                    None => {
                        if let Ok(new_jobs) = poll_pending_inbox(db, parent_run_id, 10) {
                            push_dedup(&mut collected, &mut seen_job_ids, new_jobs);
                        }
                    }
                    Some(targets) => {
                        if let Ok(new_jobs) = poll_targeted_inbox(db, parent_run_id, targets) {
                            push_dedup(&mut collected, &mut seen_job_ids, new_jobs);
                        }
                    }
                }
                push_dedup(
                    &mut collected,
                    &mut seen_job_ids,
                    collect_completed_child_results(db, parent_run_id, target_job_ids),
                );
                if crate::agent::agent_mailbox::has_pending(db,parent_run_id) || found_target(&collected) || no_pending() {
                    enrich_and_refresh_collected_results(db, &mut collected).await;
                    return Ok(collected);
                }
            }
            _ = tokio::time::sleep(remaining) => {
                // 超时唤醒：未指定目标时无过滤捞全部剩余结果；定向模式只捞目标任务，
                // 其余结果保留给后续 wait_jobs 或主循环信箱轮询，不误吃。
                match target_job_ids {
                    None => {
                        if let Ok(new_jobs) = poll_pending_inbox(db, parent_run_id, 10) {
                            push_dedup(&mut collected, &mut seen_job_ids, new_jobs);
                        }
                        push_dedup(
                            &mut collected,
                            &mut seen_job_ids,
                            collect_completed_child_results(db, parent_run_id, None),
                        );
                    }
                    Some(targets) => {
                        if let Ok(new_jobs) = poll_targeted_inbox(db, parent_run_id, targets) {
                            push_dedup(&mut collected, &mut seen_job_ids, new_jobs);
                        }
                        push_dedup(
                            &mut collected,
                            &mut seen_job_ids,
                            collect_completed_child_results(db, parent_run_id, target_job_ids),
                        );
                    }
                }
                break;
            }
            res = async {
                if let Some(rx) = interrupt_rx.as_mut() {
                    rx.changed().await.ok()
                } else {
                    futures_util::future::pending().await
                }
            } => {
                if res.is_some() {
                    return Err("等待过程中收到任务中断请求".to_string());
                }
            }
        }
    }

    enrich_and_refresh_collected_results(db, &mut collected).await;
    Ok(collected)
}

/// 检查指定 parent_run 是否仍有处于 running 状态的异步任务
pub fn has_running_jobs(db: &DbState, parent_run_id: &str) -> bool {
    let Ok(conn) = db.0.lock() else { return false; };
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_background_jobs WHERE parent_run_id = ?1 AND status = 'running'",
            params![parent_run_id],
            |row| row.get(0),
        )
        .unwrap_or(0);
    count > 0
}

/// 收尾时按真实任务类型给出操作，避免把仍在运行的 reviewer 误当成 watch。
pub(crate) fn running_job_finish_blockers(
    db: &DbState,
    parent_run_id: &str,
) -> Result<Vec<String>, String> {
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    let mut stmt = conn.prepare(
        "SELECT id, job_type, title FROM agent_background_jobs
         WHERE parent_run_id = ?1 AND status = 'running'
         UNION ALL
         SELECT id, 'child_' || COALESCE(NULLIF(kind, ''), 'review'), COALESCE(purpose, '')
         FROM agent_child_runs WHERE parent_run_id = ?1 AND status = 'running'
         ORDER BY 1",
    ).map_err(|error| error.to_string())?;
    let rows = stmt.query_map(params![parent_run_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
    }).map_err(|error| error.to_string())?;
    let mut blockers = Vec::new();
    let mut waiting = Vec::new();
    let mut wait_ids = Vec::new();
    for row in rows {
        let (id, kind, title) = row.map_err(|error| error.to_string())?;
        if kind == "watch" {
            let call = serde_json::json!({"tool": "watch_stop", "params": {"watch_id": id}});
            blockers.push(format!(
                "持续监听仍在运行（watch_id={}）：{}。收尾前请调用 {} 释放该监听。",
                id, title, call
            ));
        } else {
            waiting.push(serde_json::json!({"job_id": id, "type": kind, "title": title}));
            wait_ids.push(id);
        }
    }
    if !waiting.is_empty() {
        let call = serde_json::json!({"tool": "wait_jobs", "params": {"job_ids": wait_ids}});
        blockers.push(format!(
            "后台任务仍在执行：{}。请调用 {} 等待并接收结论；完成后再提交 finish。watch_stop 只能释放持续监听，不能结束这些任务。",
            serde_json::Value::Array(waiting), call
        ));
    }
    Ok(blockers)
}

/// 将指定 parent_run 下所有仍处于 running 状态的后台任务标记为 cancelled
pub fn cancel_all_running_jobs(db: &DbState, parent_run_id: &str) -> Result<usize, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let pids: Vec<u32> = {
        let mut stmt = conn.prepare("SELECT params_json FROM agent_background_jobs WHERE parent_run_id=?1 AND status='running' AND job_type='shell'").map_err(|e| e.to_string())?;
        let rows = stmt.query_map(params![parent_run_id], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
        rows.filter_map(Result::ok).filter_map(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .filter_map(|v| v.get("pid").and_then(|v| v.as_u64()))
            .filter_map(|pid| u32::try_from(pid).ok()).filter(|pid| *pid > 1).collect()
    };
    let now = Utc::now().to_rfc3339();
    let rows_updated = conn
        .execute(
            "UPDATE agent_background_jobs 
             SET status = 'cancelled', finished_at = ?2, error_kind = 'cancelled' 
             WHERE parent_run_id = ?1 AND status = 'running'",
            params![parent_run_id, now],
        )
        .map_err(|e| e.to_string())?;

    drop(conn);
    // 只收停本 run 登记的 shell 进程组，不枚举用户的其他进程。
    for pid in pids { crate::agent::tools::local_shell::kill_process_group(pid, "KILL"); }
    notify_job_state_changed();
    Ok(rows_updated)
}

/// 会话重新启动时，将历史异常未退出的 running 任务收敛为 interrupted
pub fn recover_orphaned_jobs_on_session_start(
    db: &DbState,
    parent_session_id: &str,
) -> Result<usize, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let now = Utc::now().to_rfc3339();
    let rows_updated = conn
        .execute(
            "UPDATE agent_background_jobs 
             SET status = 'interrupted', finished_at = ?2, error_kind = 'interrupted' 
             WHERE parent_session_id = ?1 AND status = 'running'",
            params![parent_session_id, now],
        )
        .map_err(|e| e.to_string())?;

    notify_job_state_changed();
    Ok(rows_updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn setup_test_db() -> (std::path::PathBuf, DbState) {
        let base = std::env::temp_dir().join(format!("free-api-job-engine-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let conn = crate::db::init_db(&base.join("test.db")).unwrap();
        let state = DbState(Arc::new(Mutex::new(conn)));
        (base, state)
    }

    #[test]
    fn test_job_lifecycle_begin_finish_and_poll() {
        let (base, db) = setup_test_db();
        let parent_run = "run-100";
        let parent_session = "sess-100";

        // 1. 初始状态：无 running 任务
        assert!(!has_running_jobs(&db, parent_run));

        // 2. 派发任务
        let job_id = begin_job(&db, parent_run, parent_session, "command", "编译测试", r#"{"cmd":"cargo check"}"#).unwrap();
        assert!(has_running_jobs(&db, parent_run));

        // 3. 此时轮询信箱为空（因为任务尚未完成）
        let pending = poll_pending_inbox(&db, parent_run, 10).unwrap();
        assert_eq!(pending.len(), 0);

        // 4. 任务执行完成落盘
        finish_job(&db, &job_id, "succeeded", 120, Some(r#"{"exit_code":0}"#), None).unwrap();
        assert!(!has_running_jobs(&db, parent_run));

        // 5. 轮询信箱：获取到结果，且 consumed 自动置 1
        let pending_after = poll_pending_inbox(&db, parent_run, 10).unwrap();
        assert_eq!(pending_after.len(), 1);
        assert_eq!(pending_after[0].job_id, job_id);
        assert_eq!(pending_after[0].status, "succeeded");
        assert_eq!(pending_after[0].result_json, r#"{"exit_code":0}"#);

        // 6. 第二次轮询：不再重复消费（严格幂等）
        let pending_again = poll_pending_inbox(&db, parent_run, 10).unwrap();
        assert_eq!(pending_again.len(), 0);

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn test_poll_inbox_batch_limit_and_idempotency() {
        let (base, db) = setup_test_db();
        let parent_run = "run-batch";
        let parent_session = "sess-batch";

        let job_1 = begin_job(&db, parent_run, parent_session, "child_reviewer", "审测试点1", "{}").unwrap();
        let job_2 = begin_job(&db, parent_run, parent_session, "child_reviewer", "审测试点2", "{}").unwrap();
        let job_3 = begin_job(&db, parent_run, parent_session, "child_reviewer", "审测试点3", "{}").unwrap();

        finish_job(&db, &job_1, "succeeded", 100, Some("res1"), None).unwrap();
        finish_job(&db, &job_2, "succeeded", 150, Some("res2"), None).unwrap();
        finish_job(&db, &job_3, "succeeded", 200, Some("res3"), None).unwrap();

        // 限流批次拉取：每次最多拉 2 条
        let batch_1 = poll_pending_inbox(&db, parent_run, 2).unwrap();
        assert_eq!(batch_1.len(), 2);
        assert_eq!(batch_1[0].job_id, job_1);
        assert_eq!(batch_1[1].job_id, job_2);

        // 下一次拉取剩下的 1 条
        let batch_2 = poll_pending_inbox(&db, parent_run, 2).unwrap();
        assert_eq!(batch_2.len(), 1);
        assert_eq!(batch_2[0].job_id, job_3);

        // 再次拉取全空
        let batch_3 = poll_pending_inbox(&db, parent_run, 2).unwrap();
        assert_eq!(batch_3.len(), 0);

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn test_has_running_jobs_and_cancellation_cascade() {
        let (base, db) = setup_test_db();
        let parent_run = "run-cancel";
        let parent_session = "sess-cancel";

        let job_1 = begin_job(&db, parent_run, parent_session, "task", "任务1", "{}").unwrap();
        let _job_2 = begin_job(&db, parent_run, parent_session, "task", "任务2", "{}").unwrap();

        // job_1 提前完成
        finish_job(&db, &job_1, "succeeded", 80, Some("ok"), None).unwrap();

        // 此时由于 job_2 还在跑，has_running_jobs 为 true
        assert!(has_running_jobs(&db, parent_run));

        // 触发级联打断
        let cancelled = cancel_all_running_jobs(&db, parent_run).unwrap();
        assert_eq!(cancelled, 1);
        assert!(!has_running_jobs(&db, parent_run));

        // 验证 job_2 变为 cancelled 且可以被信箱拉取通知模型
        let inbox = poll_pending_inbox(&db, parent_run, 10).unwrap();
        assert_eq!(inbox.len(), 2);
        assert_eq!(inbox[0].status, "succeeded");
        assert_eq!(inbox[1].status, "cancelled");
        assert_eq!(inbox[1].error_kind.as_deref(), Some("cancelled"));

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn test_recover_orphaned_jobs_on_session_start() {
        let (base, db) = setup_test_db();
        let parent_run = "run-crash";
        let parent_session = "sess-crash";

        // 模拟断电前两个任务正在跑
        let _job_1 = begin_job(&db, parent_run, parent_session, "task", "断电前任务1", "{}").unwrap();
        let _job_2 = begin_job(&db, parent_run, parent_session, "task", "断电前任务2", "{}").unwrap();

        assert!(has_running_jobs(&db, parent_run));

        // 会话重启恢复
        let recovered = recover_orphaned_jobs_on_session_start(&db, parent_session).unwrap();
        assert_eq!(recovered, 2);
        assert!(!has_running_jobs(&db, parent_run));

        // 信箱里可以消费到 interrupted 状态，通知大模型自愈重试
        let inbox = poll_pending_inbox(&db, parent_run, 10).unwrap();
        assert_eq!(inbox.len(), 2);
        assert_eq!(inbox[0].status, "interrupted");
        assert_eq!(inbox[1].status, "interrupted");

        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn test_wait_for_jobs_immediate_when_already_finished() {
        let (base, db) = setup_test_db();
        let parent_run = "run-wait-imm";
        let parent_session = "sess-wait-imm";

        let job_1 = begin_job(&db, parent_run, parent_session, "build", "构建", "{}").unwrap();
        finish_job(&db, &job_1, "succeeded", 50, Some(r#"{"status":"ok"}"#), None).unwrap();

        let res = wait_for_jobs(&db, parent_run, Some(&[job_1.clone()]), 10, None).await.unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].job_id, job_1);
        assert_eq!(res[0].status, "succeeded");

        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn test_wait_for_jobs_reactively_woken_by_finish_job() {
        let (base, db) = setup_test_db();
        let parent_run = "run-wait-notify";
        let parent_session = "sess-wait-notify";

        let job_1 = begin_job(&db, parent_run, parent_session, "scan", "扫描", "{}").unwrap();

        let db_clone = DbState(db.0.clone());
        let job_id_clone = job_1.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            finish_job(&db_clone, &job_id_clone, "succeeded", 45, Some(r#"{"findings":2}"#), None).unwrap();
        });

        let started = std::time::Instant::now();
        let res = wait_for_jobs(&db, parent_run, Some(&[job_1.clone()]), 10, None).await.unwrap();
        let elapsed = started.elapsed();

        assert_eq!(res.len(), 1);
        assert_eq!(res[0].job_id, job_1);
        assert_eq!(res[0].status, "succeeded");
        assert!(elapsed.as_millis() >= 40 && elapsed.as_millis() < 3000);

        let _ = std::fs::remove_dir_all(base);
    }
    #[test]
    fn wait_window_agrees_with_adaptive_limit_and_handles_extreme_inputs() {
        assert_eq!(wait_timeout_secs(180, 0), 180);
        assert_eq!(wait_timeout_secs(180, 1), 360);
        assert_eq!(wait_timeout_secs(180, 2), 600);
        assert_eq!(wait_timeout_secs(600, 0), 600);
        assert_eq!(wait_timeout_secs(u64::MAX, u32::MAX), 600);
        assert_eq!(wait_timeout_secs(0, 0), 1);
    }

    #[tokio::test]
    async fn shell_wait_timeout_reports_running_then_recovers_same_job() {
        let (base, db) = setup_test_db();
        let id = begin_job(&db, "run-shell", "session", "shell", "archive check", "{}").unwrap();
        let ids = [id.clone()];
        let result = wait_for_jobs(&db, "run-shell", Some(&ids), 1, None).await.unwrap();
        assert!(result.is_empty());
        let message = empty_wait_message(&db, "run-shell", Some(&ids), 1).unwrap();
        assert!(message.contains("仍有 1 个目标后台任务在运行"), "{}", message);
        assert!(!message.contains("均已到终态"));
        assert!(!message.contains("没有已登记"));
        finish_job(&db, &id, "failed", 1010, Some(r#"{"exit_code":124}"#), None).unwrap();
        assert!(empty_wait_message(&db, "run-shell", Some(&ids), 1).unwrap().contains("结果尚未回收"));
        let result = wait_for_jobs(&db, "run-shell", Some(&ids), 600, None).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].job_id, id);
        assert_eq!(result[0].status, "failed");
        assert!(result[0].result_json.contains("124"));
        let message = empty_wait_message(&db, "run-shell", Some(&ids), 600).unwrap();
        assert!(message.contains("均已到终态且结果已回收"));
        assert!(message.contains("是否成功请以已回收的结果为准"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn empty_wait_feedback_covers_child_and_shell_in_same_target_scope() {
        let (base, db) = setup_test_db();
        let shell = begin_job(&db, "run-mixed", "session", "shell", "archive check", "{}").unwrap();
        {
            let conn = db.0.lock().unwrap();
            for (id, kind, run) in [("review", "review", "run-mixed"), ("writer", "writer", "run-mixed"), ("other", "worker", "other-run")] {
                conn.execute("INSERT INTO agent_child_runs (id,parent_run_id,parent_session_id,workspace_root,model_id,status,started_at,kind) VALUES (?1,?2,'session','.','fake','running',?3,?4)", params![id, run, Utc::now().to_rfc3339(), kind]).unwrap();
            }
        }
        assert!(empty_wait_message(&db, "run-mixed", None, 360).unwrap().contains("仍有 3 个目标后台任务在运行"));
        let ids = [shell.clone(), "writer".to_string(), "ghost".to_string(), "ghost".to_string(), "other".to_string()];
        let message = empty_wait_message(&db, "run-mixed", Some(&ids), 360).unwrap();
        assert!(message.contains("仍有 2 个目标后台任务在运行"), "{}", message);
        assert!(message.contains("未找到 2 个指定任务 ID"), "{}", message);
        db.0.lock().unwrap().execute("UPDATE agent_child_runs SET status='succeeded',result_json='{}' WHERE id='writer'", []).unwrap();
        let message = empty_wait_message(&db, "run-mixed", Some(&ids), 360).unwrap();
        assert!(message.contains("仍有 1 个目标后台任务在运行"));
        assert!(message.contains("有 1 个目标任务已到终态、结果尚未回收"));
        db.0.lock().unwrap().execute("UPDATE agent_child_runs SET applied_at='done' WHERE id='writer'", []).unwrap();
        let message = empty_wait_message(&db, "run-mixed", Some(&["writer".to_string()]), 360).unwrap();
        assert!(message.contains("均已到终态且结果已回收"));
        assert!(!message.contains("仍有"), "无关的 running 任务不能影响定向反馈");
        assert!(empty_wait_message(&db, "new-run", None, 180).unwrap().contains("没有已登记"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn empty_wait_feedback_does_not_report_completion_when_status_is_unavailable() {
        let (base, db) = setup_test_db();
        {
            let conn = db.0.lock().unwrap();
            conn.execute("INSERT INTO agent_child_runs (id,parent_run_id,parent_session_id,workspace_root,model_id,status,started_at) VALUES ('incomplete','run','session','.','fake','failed',?1)", params![Utc::now().to_rfc3339()]).unwrap();
        }
        let message = empty_wait_message(&db, "run", None, 180).unwrap();
        assert!(message.contains("尚无可回收结果"));
        assert!(!message.contains("均已到终态"));
        db.0.lock().unwrap().execute("DROP TABLE agent_background_jobs", []).unwrap();
        assert!(empty_wait_message(&db, "run", None, 180).is_err());
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn long_wait_remains_interruptible() {
        let (base, db) = setup_test_db();
        begin_job(&db, "run-stop", "session", "shell", "archive check", "{}").unwrap();
        let (tx, mut rx) = tokio::sync::watch::channel(0u64);
        let waiting = wait_for_jobs(&db, "run-stop", None, 600, Some(&mut rx));
        let interrupt = async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tx.send(1).unwrap();
        };
        let (result, _) = tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(waiting, interrupt) }).await.unwrap();
        assert!(result.unwrap_err().contains("中断"));
        assert!(has_running_jobs(&db, "run-stop"), "等待中断不能伪造任务成功");
        let _ = std::fs::remove_dir_all(base);
    }

}
