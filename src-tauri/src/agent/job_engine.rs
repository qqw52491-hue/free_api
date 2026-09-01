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

fn job_watch_receiver() -> watch::Receiver<u64> {
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

    conn.execute(
        "UPDATE agent_background_jobs 
         SET status = ?2, finished_at = ?3, duration_ms = ?4, result_json = ?5, error_kind = ?6
         WHERE id = ?1",
        params![job_id, status, now, duration_ms as i64, result_json, error_kind],
    )
    .map_err(|e| e.to_string())?;

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
            });
        }
    }

    Ok(results)
}

/// 检查指定 parent_run 是否仍有处于 running 状态的异步任务（包含通用任务与子审查任务）
pub fn has_any_running_jobs(db: &DbState, parent_run_id: &str) -> bool {
    has_running_jobs(db, parent_run_id) || crate::agent::child_reviewer::has_running_child_runs(db, parent_run_id)
}

/// 将子审查终态结果映射为统一的信箱结果条目
fn collect_completed_child_results(
    db: &DbState,
    parent_run_id: &str,
    target_job_ids: Option<&[String]>,
) -> Vec<BackgroundJobResult> {
    match crate::agent::child_reviewer::poll_completed_child_runs_filtered(db, parent_run_id, target_job_ids) {
        Ok(children) => children
            .into_iter()
            .map(|child| {
                let kind_str = child.kind.as_deref().unwrap_or("review");
                let (job_type, title) = match kind_str {
                    "writer" => ("child_writer".to_string(), "受限写入工兵任务".to_string()),
                    "worker" => ("child_worker".to_string(), "受限侦察任务".to_string()),
                    _ => ("child_reviewer".to_string(), "受限子审查任务".to_string()),
                };
                let result_json = child
                    .raw_result_json
                    .clone()
                    .unwrap_or_else(|| serde_json::to_string(&child).unwrap_or_default());
                BackgroundJobResult {
                    result_json,
                    job_id: child.child_run_id.clone(),
                    job_type,
                    title,
                    status: child.status,
                    duration_ms: child.duration_ms,
                    error_kind: child.error_kind,
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
    let timeout_duration = Duration::from_secs(timeout_secs.clamp(1, 180));
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
    let mut collected = Vec::new();
    match target_job_ids {
        None => {
            if let Ok(immediate) = poll_pending_inbox(db, parent_run_id, 10) {
                collected.extend(immediate);
            }
        }
        Some(targets) => {
            if let Ok(targeted) = poll_targeted_inbox(db, parent_run_id, targets) {
                collected.extend(targeted);
            }
        }
    }
    collected.extend(collect_completed_child_results(db, parent_run_id, target_job_ids));

    if found_target(&collected) || !has_any_running_jobs(db, parent_run_id) {
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
                            collected.extend(new_jobs);
                        }
                    }
                    Some(targets) => {
                        if let Ok(new_jobs) = poll_targeted_inbox(db, parent_run_id, targets) {
                            collected.extend(new_jobs);
                        }
                    }
                }
                collected.extend(collect_completed_child_results(db, parent_run_id, target_job_ids));
                if found_target(&collected) || !has_any_running_jobs(db, parent_run_id) {
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
                            collected.extend(new_jobs);
                        }
                        collected.extend(collect_completed_child_results(db, parent_run_id, None));
                    }
                    Some(targets) => {
                        if let Ok(new_jobs) = poll_targeted_inbox(db, parent_run_id, targets) {
                            collected.extend(new_jobs);
                        }
                        collected.extend(collect_completed_child_results(db, parent_run_id, target_job_ids));
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

/// 将指定 parent_run 下所有仍处于 running 状态的后台任务标记为 cancelled
pub fn cancel_all_running_jobs(db: &DbState, parent_run_id: &str) -> Result<usize, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let now = Utc::now().to_rfc3339();
    let rows_updated = conn
        .execute(
            "UPDATE agent_background_jobs 
             SET status = 'cancelled', finished_at = ?2, error_kind = 'cancelled' 
             WHERE parent_run_id = ?1 AND status = 'running'",
            params![parent_run_id, now],
        )
        .map_err(|e| e.to_string())?;

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
}
