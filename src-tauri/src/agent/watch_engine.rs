use crate::db::DbState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

/// 每个 parent_run 同时允许的持续监听数量上限
pub const MAX_ACTIVE_WATCHES_PER_RUN: usize = 3;
/// 单个监听生命周期内最多产生的事件数，防止目标被外部高频改动时刷屏信箱
pub const MAX_WATCH_EVENTS: u64 = 100;
const DEFAULT_INTERVAL_SECS: u64 = 20;
const MIN_INTERVAL_SECS: u64 = 5;
const MAX_INTERVAL_SECS: u64 = 600;
const MAX_TARGET_CHARS: usize = 1_000;
const MAX_COMMAND_OUTPUT_BYTES: usize = 200_000;
const MAX_EVENT_SUMMARY_CHARS: usize = 4_000;
const COMMAND_TIMEOUT_SECS: u64 = 30;
const MAX_FILE_HASH_BYTES: u64 = 2_000_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WatchRequest {
    pub kind: String,
    pub target: String,
    pub interval_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchState {
    pub kind: String,
    pub target: String,
    pub interval_secs: u64,
    pub last_snapshot: String,
    pub events_emitted: u64,
}

pub fn parse_watch_request(params: &Value) -> Result<WatchRequest, String> {
    let kind = params
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("file")
        .trim()
        .to_lowercase();
    if !matches!(kind.as_str(), "file" | "command") {
        return Err("watch_start.kind 仅支持 file 或 command。".to_string());
    }
    let target = params
        .get("target")
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "watch_start 缺少 target（file 传绝对路径，command 传 shell 命令）。".to_string())?;
    if target.chars().count() > MAX_TARGET_CHARS {
        return Err(format!("watch_start.target 最多 {} 个字符。", MAX_TARGET_CHARS));
    }
    let interval_secs = params
        .get("interval_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_INTERVAL_SECS)
        .clamp(MIN_INTERVAL_SECS, MAX_INTERVAL_SECS);
    Ok(WatchRequest {
        kind,
        target,
        interval_secs,
    })
}

/// 校验 file 监听目标必须落在工作区内（与"不得访问工作区外路径"契约对齐）
fn ensure_path_within_workspace(target: &str, workspace_root: Option<&Path>) -> Result<PathBuf, String> {
    let path = PathBuf::from(target);
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("watch_start 无法访问监听目标 {}: {}", target, error))?;
    if let Some(root) = workspace_root {
        let root_canonical = root
            .canonicalize()
            .map_err(|error| format!("watch_start 工作区不可用: {}", error))?;
        if !canonical.starts_with(&root_canonical) {
            return Err("watch_start 只能监听当前工作区内的文件。".to_string());
        }
    }
    Ok(canonical)
}

fn active_watch_count(conn: &rusqlite::Connection, parent_run_id: &str) -> Result<i64, String> {
    conn.query_row(
        "SELECT COUNT(*) FROM agent_background_jobs WHERE parent_run_id = ?1 AND job_type = 'watch' AND status = 'running'",
        rusqlite::params![parent_run_id],
        |row| row.get(0),
    )
    .map_err(|error| error.to_string())
}

/// 登记一个持续监听任务：以 job_type='watch' 的 running 行作为 durable 状态锚点。
/// 返回 watch_id，监听循环由调用方 spawn。
pub async fn begin_watch(
    db: &DbState,
    parent_run_id: &str,
    parent_session_id: &str,
    request: &WatchRequest,
    workspace_root: Option<&Path>,
) -> Result<String, String> {
    let initial_target = if request.kind == "file" {
        ensure_path_within_workspace(&request.target, workspace_root)?
            .to_string_lossy()
            .to_string()
    } else {
        request.target.clone()
    };
    let initial_snapshot = compute_snapshot(&request.kind, &initial_target, workspace_root).await?;

    let conn = db.0.lock().map_err(|error| error.to_string())?;
    if active_watch_count(&conn, parent_run_id)? >= MAX_ACTIVE_WATCHES_PER_RUN as i64 {
        return Err(format!(
            "watch_start 每个运行最多同时持有 {} 个持续监听；请先 watch_stop 释放不再需要的监听。",
            MAX_ACTIVE_WATCHES_PER_RUN
        ));
    }
    let state = WatchState {
        kind: request.kind.clone(),
        target: initial_target,
        interval_secs: request.interval_secs,
        last_snapshot: initial_snapshot,
        events_emitted: 0,
    };
    let params_json = serde_json::to_string(&state).unwrap_or_else(|_| "{}".to_string());
    let title = format!("持续监听[{}] {}", request.kind, request.target);
    drop(conn);
    // watch_id 直接复用 job 引擎生成的行 id，保证与 durable 行一一对应
    crate::agent::job_engine::begin_job(db, parent_run_id, parent_session_id, "watch", &title, &params_json)
}

/// 停止单个持续监听（幂等：已结束的监听返回 0）
pub fn stop_watch(db: &DbState, parent_run_id: &str, watch_id: &str) -> Result<usize, String> {
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    let rows = conn
        .execute(
            "UPDATE agent_background_jobs
             SET status = 'cancelled', finished_at = ?3, error_kind = 'watch_stopped'
             WHERE id = ?1 AND parent_run_id = ?2 AND job_type = 'watch' AND status = 'running'",
            rusqlite::params![watch_id, parent_run_id, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|error| error.to_string())?;
    drop(conn);
    if rows > 0 {
        crate::agent::job_engine::notify_job_state_changed();
    }
    Ok(rows)
}

/// 停止指定运行下的全部持续监听（watch_stop 未传 watch_id 时使用）
pub fn stop_all_watches(db: &DbState, parent_run_id: &str) -> Result<usize, String> {
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    let rows = conn
        .execute(
            "UPDATE agent_background_jobs
             SET status = 'cancelled', finished_at = ?2, error_kind = 'watch_stopped'
             WHERE parent_run_id = ?1 AND job_type = 'watch' AND status = 'running'",
            rusqlite::params![parent_run_id, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|error| error.to_string())?;
    drop(conn);
    if rows > 0 {
        crate::agent::job_engine::notify_job_state_changed();
    }
    Ok(rows)
}

/// 读取并校验监听行；行已不再 running 时返回 None（监听循环据此退出）
pub fn load_watch_state(db: &DbState, watch_id: &str) -> Option<WatchState> {
    let conn = db.0.lock().ok()?;
    let (status, params_json): (String, String) = conn
        .query_row(
            "SELECT status, params_json FROM agent_background_jobs WHERE id = ?1 AND job_type = 'watch'",
            rusqlite::params![watch_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()?;
    if status != "running" {
        return None;
    }
    serde_json::from_str::<WatchState>(&params_json).ok()
}

fn persist_watch_state(db: &DbState, watch_id: &str, state: &WatchState) {
    if let Ok(conn) = db.0.lock() {
        if let Ok(params_json) = serde_json::to_string(state) {
            let _ = conn.execute(
                "UPDATE agent_background_jobs SET params_json = ?2 WHERE id = ?1",
                rusqlite::params![watch_id, params_json],
            );
        }
    }
}

async fn compute_snapshot(kind: &str, target: &str, workspace_root: Option<&Path>) -> Result<String, String> {
    match kind {
        "file" => file_snapshot(Path::new(target)),
        "command" => {
            // 初始快照同样单次执行；此处只取哈希，不保留输出
            let (hashed, _) = run_command_snapshot(target, workspace_root).await?;
            Ok(hashed)
        }
        other => Err(format!("未知监听类型 {}", other)),
    }
}

fn file_snapshot(path: &Path) -> Result<String, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("监听目标不可读: {}", error))?;
    let size = metadata.len();
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    // 内容哈希：小文件全量，大文件采样首尾，保证改动可检测且开销有界
    let mut hasher = Sha256::new();
    hasher.update(size.to_le_bytes());
    hasher.update(mtime.to_le_bytes());
    if size <= MAX_FILE_HASH_BYTES {
        if let Ok(bytes) = std::fs::read(path) {
            hasher.update(&bytes);
        }
    } else if let Ok(bytes) = std::fs::read(path) {
        hasher.update(&bytes[..2048]);
        let tail_start = bytes.len().saturating_sub(2048);
        hasher.update(&bytes[tail_start..]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

async fn run_command_snapshot(target: &str, workspace_root: Option<&Path>) -> Result<(String, String), String> {
    run_command_snapshot_cancellable(target, workspace_root, None).await
}

async fn run_command_snapshot_cancellable(
    target: &str, workspace_root: Option<&Path>, cancellation: Option<(&DbState, &str)>,
) -> Result<(String, String), String> {
    let mut command = tokio::process::Command::new("sh");
    command.kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    command.arg("-c").arg(target).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
    if let Some(root) = workspace_root {
        command.current_dir(root);
    }
    let mut changed = crate::agent::job_engine::job_watch_receiver();
    let child = command.spawn().map_err(|error| format!("watch command 启动失败: {error}"))?;
    let pid = child.id();
    let cancel = async {
        if let Some((db, watch_id)) = cancellation {
            loop {
                if load_watch_state(db, watch_id).is_none() { break; }
                if changed.changed().await.is_err() { break; }
            }
        } else {
            std::future::pending::<()>().await;
        }
    };
    let result = tokio::select! {
        output = tokio::time::timeout(Duration::from_secs(COMMAND_TIMEOUT_SECS), child.wait_with_output()) =>
            output.map_err(|_| "watch command 超时".to_string())
                .and_then(|v| v.map_err(|e| format!("watch command 执行失败: {e}"))),
        _ = cancel => Err("watch command 已取消".to_string()),
    };
    if result.is_err() {
        if let Some(pid) = pid { crate::agent::tools::local_shell::kill_process_group(pid, "KILL"); }
    }
    let output = result?;
    let mut stdout = output.stdout;
    stdout.truncate(MAX_COMMAND_OUTPUT_BYTES);
    let detail = truncate_for_event(&stdout);
    let mut hasher = Sha256::new();
    hasher.update(output.status.code().unwrap_or(-1).to_le_bytes());
    hasher.update(&stdout);
    Ok((format!("{:x}", hasher.finalize()), detail))
}

fn truncate_for_event(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut result: String = text.chars().take(MAX_EVENT_SUMMARY_CHARS).collect();
    if text.chars().count() > MAX_EVENT_SUMMARY_CHARS {
        result.push_str("…（已截断）");
    }
    result
}

/// 持续监听循环：周期性对比快照，仅在外部状态发生变化时向信箱投递事件。
/// 监听行被取消（watch_stop / 运行结束级联取消 / 达到事件上限）时自动退出。
pub async fn run_watch_loop(
    db: DbState,
    app: Option<AppHandle>,
    parent_session_id: String,
    watch_id: String,
    interval_secs: u64,
    mut last_snapshot: String,
    workspace_root: Option<PathBuf>,
) {
    let interval = Duration::from_secs(interval_secs);
    let mut changed = crate::agent::job_engine::job_watch_receiver();
    loop {
        let delay = tokio::time::sleep(interval);
        tokio::pin!(delay);
        loop {
            tokio::select! {
                _ = &mut delay => break,
                _ = changed.changed() => {
                    if load_watch_state(&db, &watch_id).is_none() { return; }
                }
            }
        }
        // 每个周期先确认监听行仍处于 running；被取消即退出（取消方负责广播唤醒等待者）
        let Some(current) = load_watch_state(&db, &watch_id) else {
            return;
        };
        // 单次执行同时产出快照哈希与事件详情，避免 command 型监听被重复触发
        let (snapshot, detail) = if current.kind == "command" {
            match run_command_snapshot_cancellable(&current.target, workspace_root.as_deref(), Some((&db, &watch_id))).await {
                Ok((hash, output)) => (hash, output),
                Err(error) => {
                    emit_watch_event(
                        &db,
                        app.as_ref(),
                        &parent_session_id,
                        &watch_id,
                        &current,
                        &format!("监听目标状态异常：{}", error),
                    )
                    .await;
                    continue;
                }
            }
        } else {
            match file_snapshot(Path::new(&current.target)) {
                Ok(hash) => (hash, file_change_detail(Path::new(&current.target))),
                Err(error) => {
                    // 目标暂时不可读（如文件被临时删除）：投递一次异常事件后保持监听，
                    // 让父 Agent 决定是否停止；连续异常由事件上限兜底。
                    emit_watch_event(
                        &db,
                        app.as_ref(),
                        &parent_session_id,
                        &watch_id,
                        &current,
                        &format!("监听目标状态异常：{}", error),
                    )
                    .await;
                    continue;
                }
            }
        };
        if snapshot == last_snapshot {
            continue;
        }
        let summary = format!(
            "持续监听[{}] {} 检测到变化（第 {} 次）\n{}",
            current.kind,
            current.target,
            current.events_emitted + 1,
            detail
        );
        emit_watch_event(&db, app.as_ref(), &parent_session_id, &watch_id, &current, &summary).await;

        last_snapshot = snapshot.clone();
        let mut next = current;
        next.last_snapshot = snapshot;
        next.events_emitted += 1;
        if next.events_emitted >= MAX_WATCH_EVENTS {
            persist_watch_state(&db, &watch_id, &next);
            if let Ok(conn) = db.0.lock() {
                let _ = conn.execute(
                    "UPDATE agent_background_jobs SET status = 'completed', finished_at = ?2, error_kind = 'watch_event_limit'
                     WHERE id = ?1 AND status = 'running'",
                    rusqlite::params![watch_id, chrono::Utc::now().to_rfc3339()],
                );
            }
            crate::agent::job_engine::notify_job_state_changed();
            return;
        }
        persist_watch_state(&db, &watch_id, &next);
    }
}

fn file_change_detail(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(metadata) => format!("当前状态：{} 字节，mtime 已更新", metadata.len()),
        Err(_) => "当前状态：文件已不存在".to_string(),
    }
}

async fn emit_watch_event(
    db: &DbState,
    app: Option<&AppHandle>,
    parent_session_id: &str,
    watch_id: &str,
    state: &WatchState,
    summary: &str,
) {
    let event_payload = json!({
        "watch_id": watch_id,
        "kind": state.kind,
        "target": state.target,
        "summary": summary,
    });
    let Ok(event_job_id) = crate::agent::job_engine::begin_job(
        db,
        &watch_row_parent_run(db, watch_id).unwrap_or_default(),
        parent_session_id,
        "watch_event",
        &format!("监听事件[{}] {}", state.kind, state.target),
        &serde_json::to_string(&event_payload).unwrap_or_default(),
    ) else {
        return;
    };
    let _ = crate::agent::job_engine::finish_job(
        db,
        &event_job_id,
        "succeeded",
        0,
        Some(&serde_json::to_string(&event_payload).unwrap_or_default()),
        None,
    );
    if let Some(handle) = app {
        let _ = handle.emit(
            &format!("agent-progress-{}", parent_session_id),
            json!({
                "type": "watch_event",
                "watch_id": watch_id,
                "kind": state.kind,
                "target": state.target,
                "summary": summary,
            }),
        );
        let _ = handle.emit(
            &format!("agent-log-{}", parent_session_id),
            format!("👁️ {}", summary),
        );
    }
}

fn watch_row_parent_run(db: &DbState, watch_id: &str) -> Option<String> {
    let conn = db.0.lock().ok()?;
    conn.query_row(
        "SELECT parent_run_id FROM agent_background_jobs WHERE id = ?1",
        rusqlite::params![watch_id],
        |row| row.get(0),
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn setup_test_db() -> (std::path::PathBuf, DbState) {
        let base = std::env::temp_dir().join(format!("free-api-watch-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let conn = crate::db::init_db(&base.join("test.db")).unwrap();
        (base, DbState(Arc::new(Mutex::new(conn))))
    }

    #[test]
    fn parse_watch_request_clamps_interval_and_kind() {
        let request = parse_watch_request(&json!({"kind": "file", "target": "/tmp/a.log", "interval_secs": 1})).unwrap();
        assert_eq!(request.interval_secs, MIN_INTERVAL_SECS);
        let request = parse_watch_request(&json!({"kind": "command", "target": "echo hi", "interval_secs": 99999})).unwrap();
        assert_eq!(request.interval_secs, MAX_INTERVAL_SECS);
        let request = parse_watch_request(&json!({"target": "x"})).unwrap();
        assert_eq!(request.kind, "file");
        assert_eq!(request.interval_secs, DEFAULT_INTERVAL_SECS);
        assert!(parse_watch_request(&json!({"kind": "glob", "target": "x"})).is_err());
        assert!(parse_watch_request(&json!({"kind": "file"})).is_err());
    }

    #[tokio::test]
    async fn file_watch_rejects_targets_outside_workspace() {
        let base = std::env::temp_dir().join(format!("free-api-watch-scope-{}", Uuid::new_v4()));
        std::fs::create_dir_all(base.join("ws")).unwrap();
        let inside = base.join("ws/a.log");
        std::fs::write(&inside, "hello").unwrap();
        let outside = base.join("outside.log");
        std::fs::write(&outside, "secret").unwrap();

        let request = WatchRequest { kind: "file".into(), target: inside.to_string_lossy().to_string(), interval_secs: 5 };
        let conn = crate::db::init_db(&base.join("t.db")).unwrap();
        let db = DbState(Arc::new(Mutex::new(conn)));
        let watch_id = begin_watch(&db, "run-w", "sess-w", &request, Some(&base.join("ws"))).await.unwrap();
        assert!(watch_id.starts_with("job-"));

        let bad = WatchRequest { kind: "file".into(), target: outside.to_string_lossy().to_string(), interval_secs: 5 };
        assert!(begin_watch(&db, "run-w", "sess-w", &bad, Some(&base.join("ws")))
            .await
            .unwrap_err()
            .contains("工作区内"));
        let _ = std::fs::remove_dir_all(base);
        let _ = watch_id;
    }

    #[tokio::test]
    async fn watch_cap_enforced_per_run() {
        let (base, db) = setup_test_db();
        let file_path = base.join("cap.log");
        std::fs::write(&file_path, "v1").unwrap();
        for _ in 0..MAX_ACTIVE_WATCHES_PER_RUN {
            let request = WatchRequest { kind: "file".into(), target: file_path.to_string_lossy().to_string(), interval_secs: 5 };
            begin_watch(&db, "run-cap", "sess-cap", &request, None).await.unwrap();
        }
        let request = WatchRequest { kind: "file".into(), target: file_path.to_string_lossy().to_string(), interval_secs: 5 };
        assert!(begin_watch(&db, "run-cap", "sess-cap", &request, None)
            .await
            .unwrap_err()
            .contains("watch_stop"));
        // 其他 run 不受影响
        begin_watch(&db, "run-cap-2", "sess-cap", &request, None).await.unwrap();
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn watch_loop_emits_event_on_change_and_stops_when_cancelled() {
        let (base, db) = setup_test_db();
        let file_path = base.join("mutating.log");
        std::fs::write(&file_path, "v1").unwrap();
        let request = WatchRequest {
            kind: "file".into(),
            target: file_path.to_string_lossy().to_string(),
            interval_secs: 1,
        };
        let watch_id = begin_watch(&db, "run-loop", "sess-loop", &request, None).await.unwrap();
        let state = load_watch_state(&db, &watch_id).unwrap();

        let db_clone = DbState(db.0.clone());
        let watch_clone = watch_id.clone();
        let root = base.clone();
        let handle = tokio::spawn(async move {
            run_watch_loop(
                db_clone,
                None,
                "sess-loop".into(),
                watch_clone,
                state.interval_secs,
                state.last_snapshot,
                Some(root),
            )
            .await;
        });

        // 等一个周期后修改文件，触发变化事件
        tokio::time::sleep(Duration::from_millis(1300)).await;
        std::fs::write(&file_path, "v2 with new content").unwrap();

        // 事件应进入信箱（watch_event 行），wait_for_jobs 可消费
        let results = crate::agent::job_engine::wait_for_jobs(&db, "run-loop", None, 6, None).await.unwrap();
        assert!(results.iter().any(|job| job.job_type == "watch_event" && job.result_json.contains(&watch_id)));

        // 取消后循环退出：再改文件不再产生新事件
        assert_eq!(stop_watch(&db, "run-loop", &watch_id).unwrap(), 1);
        let _ = handle.await;
        std::fs::write(&file_path, "v3 after cancel").unwrap();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let before = count_watch_events(&db, "run-loop");
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(count_watch_events(&db, "run-loop"), before);

        let _ = std::fs::remove_dir_all(base);
    }

    fn count_watch_events(db: &DbState, parent_run_id: &str) -> usize {
        let conn = db.0.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM agent_background_jobs WHERE parent_run_id = ?1 AND job_type = 'watch_event'",
            rusqlite::params![parent_run_id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap() as usize
    }


    #[tokio::test]
    async fn cancelled_watch_interrupts_inflight_command_without_waiting_for_timeout() {
        let (base, db) = setup_test_db();
        let id = crate::agent::job_engine::begin_job(&db,"run-watch-cancel","s1","watch","long probe",
            &serde_json::to_string(&WatchState { kind:"command".into(),target:"sleep 60".into(),interval_secs:5,last_snapshot:String::new(),events_emitted:0 }).unwrap()).unwrap();
        let db2 = DbState(db.0.clone());
        let id2 = id.clone();
        let task = tokio::spawn(async move {
            run_command_snapshot_cancellable("sleep 60",None,Some((&db2,&id2))).await
        });
        tokio::time::sleep(Duration::from_millis(60)).await;
        crate::agent::job_engine::cancel_all_running_jobs(&db,"run-watch-cancel").unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2),task).await.expect("cancellation must wake the active command").unwrap();
        assert!(result.unwrap_err().contains("取消"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn command_watch_detects_output_change() {
        let (base, db) = setup_test_db();
        let counter = base.join("counter");
        std::fs::write(&counter, "0").unwrap();
        // 每次执行把计数 +1 并输出：输出变化 => 快照变化
        let target = format!("cat {}", counter.to_string_lossy());
        let request = WatchRequest { kind: "command".into(), target, interval_secs: 1 };
        let watch_id = begin_watch(&db, "run-cmd", "sess-cmd", &request, Some(&base)).await.unwrap();
        let state = load_watch_state(&db, &watch_id).unwrap();

        let db_clone = DbState(db.0.clone());
        let watch_clone = watch_id.clone();
        let root = base.clone();
        tokio::spawn(async move {
            run_watch_loop(
                db_clone,
                None,
                "sess-cmd".into(),
                watch_clone,
                state.interval_secs,
                state.last_snapshot,
                Some(root),
            )
            .await;
        });

        std::fs::write(&counter, "1").unwrap();
        let results = crate::agent::job_engine::wait_for_jobs(&db, "run-cmd", None, 6, None).await.unwrap();
        assert!(results.iter().any(|job| job.job_type == "watch_event"));
        stop_watch(&db, "run-cmd", &watch_id).unwrap();
        let _ = std::fs::remove_dir_all(base);
    }
}
