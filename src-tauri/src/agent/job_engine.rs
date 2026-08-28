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

/// 结束后台异步任务，写入完成状态与结果
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
        let job_2 = begin_job(&db, parent_run, parent_session, "task", "任务2", "{}").unwrap();

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
        let job_1 = begin_job(&db, parent_run, parent_session, "task", "断电前任务1", "{}").unwrap();
        let job_2 = begin_job(&db, parent_run, parent_session, "task", "断电前任务2", "{}").unwrap();

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
}
