// 定时触发器 (Scheduler)：让 Agent 具备「主动」能力——按时间自动派发任务。
// - 配置存 data_dir/free-api-chat/schedules.json，支持手改文件（每轮 tick 重新加载）
// - kind: "daily"（每天 HH:MM 本地时间）| "interval"（每 N 分钟）
// - 触发 = 复用通道层 register_and_spawn_run 派发（会话自动创建：绑项目 → 项目会话，
//   未绑 → scratch 项目会话；工作区软锁同样生效，项目忙时本轮跳过）
// - 运行结束后把结果经桥主动推送到主人微信（/bridge/notify）；桥不在线只记日志
// - 错过的触发不补跑：daily 取下一个时刻，interval 从当下重新计时

use crate::agent::channel::{
    channel_config_shared, ensure_scratch_project, notify_wechat, register_and_spawn_run,
};
use crate::agent::router::ModelRoutingConfig;
use crate::db::DbState;
use tauri::Manager;
use chrono::{DateTime, Duration as ChronoDuration, Local, NaiveTime};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;

const TICK_SECS: u64 = 30;
const WATCH_INTERVAL_SECS: u64 = 15;
const WATCH_MAX_HOURS: u64 = 24;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleItem {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// daily | interval
    pub kind: String,
    /// daily 的本地时间 "HH:MM"
    #[serde(default)]
    pub time: Option<String>,
    /// interval 的分钟数
    #[serde(default)]
    pub minutes: Option<u64>,
    /// 绑定项目（可选；空 = scratch 临时工作区）
    #[serde(default)]
    pub project_id: Option<String>,
    pub goal: String,
    /// 首次触发后自动创建并复用的会话
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub last_fired_at: Option<String>,
    #[serde(default)]
    pub next_fired_at: Option<String>,
}

fn default_true() -> bool {
    true
}

pub(crate) fn schedules_path() -> PathBuf {
    let home = dirs_next::data_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join("free-api-chat").join("schedules.json")
}

pub(crate) fn load_schedules() -> Result<Vec<ScheduleItem>, String> {
    let path = schedules_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    serde_json::from_str(&raw).map_err(|e| format!("schedules.json 解析失败: {e}"))
}

pub(crate) fn save_schedules(items: &[ScheduleItem]) -> Result<(), String> {
    let path = schedules_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let raw = serde_json::to_string_pretty(items).map_err(|e| e.to_string())?;
    std::fs::write(&path, raw).map_err(|e| e.to_string())
}

/// 计算下一次触发时间（本地时区）
fn compute_next(item: &ScheduleItem) -> Option<DateTime<Local>> {
    let now = Local::now();
    match item.kind.as_str() {
        "daily" => {
            let time = item.time.as_deref()?;
            let naive = NaiveTime::parse_from_str(time, "%H:%M").ok()?;
            let mut candidate = now.date_naive().and_time(naive).and_local_timezone(Local).single()?;
            if candidate <= now {
                candidate += ChronoDuration::days(1);
            }
            Some(candidate)
        }
        "interval" => {
            let minutes = item.minutes?.clamp(1, 60 * 24 * 30);
            Some(now + ChronoDuration::minutes(minutes as i64))
        }
        _ => None,
    }
}

/// 对外展示/保存时补全 next_fired_at
/// 计划字段校验（调度器保存与工具层共用）
pub(crate) fn validate_schedule(kind: &str, time: Option<&str>, minutes: Option<u64>) -> Result<(), String> {
    if kind != "daily" && kind != "interval" {
        return Err("kind 仅支持 daily / interval".to_string());
    }
    if kind == "daily" {
        let ok = time
            .map(|t| chrono::NaiveTime::parse_from_str(t, "%H:%M").is_ok())
            .unwrap_or(false);
        if !ok {
            return Err("daily 需要合法的 HH:MM 时间".to_string());
        }
    }
    if kind == "interval" && minutes.unwrap_or(0) == 0 {
        return Err("interval 需要大于 0 的分钟数".to_string());
    }
    Ok(())
}

pub(crate) fn refresh_next(item: &mut ScheduleItem) {
    item.next_fired_at = compute_next(item).map(|t| t.to_rfc3339());
}

fn parse_local(ts: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(ts).ok().map(|t| t.with_timezone(&Local))
}

pub fn spawn_scheduler(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        println!("[scheduler] 定时触发器已启动（每 {TICK_SECS}s 巡检一次）");
        loop {
            if let Err(error) = tick(&app).await {
                eprintln!("[scheduler] 巡检失败: {error}");
            }
            tokio::time::sleep(std::time::Duration::from_secs(TICK_SECS)).await;
        }
    });
}

async fn tick(app: &tauri::AppHandle) -> Result<(), String> {
    let config = channel_config_shared().read().map_err(|e| e.to_string())?.clone();
    let mut items = load_schedules()?;
    let now = Local::now();
    let mut changed = false;

    for item in &mut items {
        if !item.enabled {
            continue;
        }
        let due = match item.next_fired_at.as_deref().and_then(parse_local) {
            Some(next) => next <= now,
            // 从未设置过 next（手工新建/老文件）→ 立即算一个，不即刻触发
            None => {
                refresh_next(item);
                changed = true;
                continue;
            }
        };
        if !due {
            continue;
        }
        changed = true;
        item.last_fired_at = Some(now.to_rfc3339());
        refresh_next(item);
        match fire_run(app, &config, item).await {
            Ok(run_id) => {
                println!("[scheduler] 定时任务「{}」已触发 run={run_id}", item.name);
                spawn_completion_watcher(config.clone(), item.clone(), run_id);
            }
            Err(error) => {
                eprintln!("[scheduler] 定时任务「{}」触发失败: {error}", item.name);
                notify_wechat(
                    &config,
                    &format!("⚠️ 定时任务「{}」触发失败：{error}", item.name),
                )
                .await;
            }
        }
    }
    if changed {
        save_schedules(&items)?;
    }
    Ok(())
}

async fn fire_run(
    app: &tauri::AppHandle,
    config: &crate::agent::channel::ChannelConfig,
    item: &mut ScheduleItem,
) -> Result<String, String> {
    if item.goal.trim().is_empty() {
        return Err("任务内容为空".to_string());
    }
    let db_state = app.state::<DbState>();
    let conn_guard = db_state.0.lock().map_err(|e| e.to_string())?;

    // 会话：优先复用已创建的；失效/未创建则按项目绑定新建
    let session_id = match item.session_id.as_deref() {
        Some(sid) if !sid.trim().is_empty() => {
            let exists: Option<String> = conn_guard
                .query_row(
                    "SELECT id FROM chat_sessions WHERE id=?1",
                    rusqlite::params![sid],
                    |row| row.get(0),
                )
                .ok();
            match exists {
                Some(_) => sid.to_string(),
                None => create_schedule_session(&conn_guard, config, item)?,
            }
        }
        _ => create_schedule_session(&conn_guard, config, item)?,
    };
    item.session_id = Some(session_id.clone());
    drop(conn_guard);

    // 路由：配置三槽，缺省回退会话模型
    let mut routing = ModelRoutingConfig {
        pro: config.pro.clone(),
        flash: config.flash.clone(),
        vision: config.vision.clone(),
        ..Default::default()
    };
    if routing.pro.is_empty() {
        routing.pro = config.model_id.clone();
    }
    if routing.pro.is_empty() {
        return Err("未配置默认模型，请在微信绑定页填写".to_string());
    }
    if routing.flash.is_empty() {
        routing.flash = routing.pro.clone();
    }
    if routing.vision.is_empty() {
        routing.vision = routing.pro.clone();
    }

    // 项目会话走项目工作区（软锁生效）；未绑定会话由运行时覆盖到 scratch 目录
    let has_project = {
        let state = app.state::<DbState>();
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        crate::commands::projects::project_for_session(&conn, &session_id)
            .map_err(|e| e.to_string())?
            .is_some()
    };
    let workspace_override = if has_project {
        None
    } else {
        Some(crate::agent::channel::effective_scratch_dir_pub(config))
    };
    register_and_spawn_run(
        app.clone(),
        session_id,
        item.goal.clone(),
        routing,
        workspace_override,
        config.llm_retries,
        config.action_milestone_steps,
        config.phase_pushes,
        None,
    )
    .map_err(
        |error| match error {
            crate::agent::channel::SubmitError::InFlight => {
                "该会话已有在途任务（上一轮尚未结束，本轮跳过）".to_string()
            }
            crate::agent::channel::SubmitError::Other(message) => message,
        },
    )
}

fn create_schedule_session(
    conn: &rusqlite::Connection,
    config: &crate::agent::channel::ChannelConfig,
    item: &ScheduleItem,
) -> Result<String, String> {
    if config.model_id.trim().is_empty() {
        return Err("未配置默认模型，请在微信绑定页填写".to_string());
    }
    match item.project_id.as_deref().filter(|p| !p.trim().is_empty()) {
        Some(project_id) => {
            let session = crate::commands::projects::create_project_session_row(
                conn,
                project_id,
                &format!("定时 · {}", item.name),
                &config.model_id,
            )?;
            Ok(session.id)
        }
        None => {
            let session = crate::commands::projects::create_plain_session_row(
                conn,
                &format!("定时 · {}", item.name),
                &config.model_id,
            )?;
            Ok(session.id)
        }
    }
}

/// 轮询运行账本直到终态，把结果推送到主人微信
fn spawn_completion_watcher(
    config: crate::agent::channel::ChannelConfig,
    item: ScheduleItem,
    run_id: String,
) {
    tauri::async_runtime::spawn(async move {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(WATCH_MAX_HOURS * 3600);
        loop {
            if std::time::Instant::now() >= deadline {
                notify_wechat(
                    &config,
                    &format!("⚠️ 定时任务「{}」（run {run_id}）超过 24 小时未结束，停止跟进。", item.name),
                )
                .await;
                return;
            }
            tokio::time::sleep(std::time::Duration::from_secs(WATCH_INTERVAL_SECS)).await;
            let snapshot = {
                let runs = crate::agent::channel::runs_store_snapshot();
                match runs.iter().find(|r| r.run_id == run_id) {
                    Some(record) => record.clone(),
                    None => continue,
                }
            };
            match snapshot.status.as_str() {
                "running" | "waiting_human" => continue,
                "done" => {
                    let body = snapshot
                        .result
                        .unwrap_or_else(|| "任务完成（无文本产出，详情见桌面端会话）。".to_string());
                    notify_wechat(
                        &config,
                        &format!("⏰ 定时任务「{}」完成：\n\n{}", item.name, body),
                    )
                    .await;
                    return;
                }
                "failed" => {
                    notify_wechat(
                        &config,
                        &format!(
                            "❌ 定时任务「{}」失败：\n{}",
                            item.name,
                            snapshot.error.unwrap_or_else(|| "未知错误".to_string())
                        ),
                    )
                    .await;
                    return;
                }
                "cancelled" => {
                    notify_wechat(&config, &format!("🛑 定时任务「{}」已被停止。", item.name)).await;
                    return;
                }
                _ => continue,
            }
        }
    });
}

// ---------------------------------------------------------------- Tauri 命令

#[tauri::command]
pub async fn wechat_schedule_list() -> Result<serde_json::Value, String> {
    let mut items = load_schedules()?;
    for item in &mut items {
        if item.next_fired_at.is_none() {
            refresh_next(item);
        }
    }
    Ok(json!({ "schedules": items, "path": schedules_path().to_string_lossy() }))
}

#[tauri::command]
pub async fn wechat_schedule_save(mut item: ScheduleItem) -> Result<serde_json::Value, String> {
    if item.name.trim().is_empty() {
        return Err("任务名称不能为空".to_string());
    }
    if item.goal.trim().is_empty() {
        return Err("任务内容不能为空".to_string());
    }
    validate_schedule(&item.kind, item.time.as_deref(), item.minutes)?;
    let mut items = load_schedules()?;
    if item.id.trim().is_empty() {
        item.id = uuid::Uuid::new_v4().to_string();
        refresh_next(&mut item);
        items.push(item.clone());
    } else {
        match items.iter_mut().find(|existing| existing.id == item.id) {
            Some(existing) => {
                let keep_session = existing.session_id.clone();
                item.session_id = keep_session;
                refresh_next(&mut item);
                *existing = item.clone();
            }
            None => {
                item.id = uuid::Uuid::new_v4().to_string();
                refresh_next(&mut item);
                items.push(item.clone());
            }
        }
    }
    save_schedules(&items)?;
    Ok(json!({ "schedules": items }))
}

#[tauri::command]
pub async fn wechat_schedule_delete(id: String) -> Result<serde_json::Value, String> {
    let mut items = load_schedules()?;
    items.retain(|item| item.id != id);
    save_schedules(&items)?;
    Ok(json!({ "schedules": items }))
}

#[tauri::command]
pub async fn wechat_schedule_toggle(id: String, enabled: bool) -> Result<serde_json::Value, String> {
    let mut items = load_schedules()?;
    match items.iter_mut().find(|item| item.id == id) {
        Some(item) => {
            item.enabled = enabled;
            if enabled {
                refresh_next(item);
            }
            save_schedules(&items)?;
            Ok(json!({ "schedules": items }))
        }
        None => Err("定时任务不存在".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    fn item(kind: &str, time: Option<&str>, minutes: Option<u64>) -> ScheduleItem {
        ScheduleItem {
            id: "test".to_string(),
            name: "测试".to_string(),
            enabled: true,
            kind: kind.to_string(),
            time: time.map(|t| t.to_string()),
            minutes,
            project_id: None,
            goal: "goal".to_string(),
            session_id: None,
            last_fired_at: None,
            next_fired_at: None,
        }
    }

    #[test]
    fn validate_schedule_rejects_bad_fields() {
        assert!(validate_schedule("daily", Some("09:00"), None).is_ok());
        assert!(validate_schedule("interval", None, Some(30)).is_ok());
        assert!(validate_schedule("weekly", None, None).is_err());
        assert!(validate_schedule("daily", Some("25:00"), None).is_err());
        assert!(validate_schedule("daily", None, None).is_err());
        assert!(validate_schedule("interval", None, Some(0)).is_err());
    }

    #[test]
    fn interval_schedules_next_from_now() {
        let item = item("interval", None, Some(30));
        let next = compute_next(&item).expect("interval should compute");
        let expected = Local::now() + ChronoDuration::minutes(30);
        let diff = (next - expected).num_seconds().abs();
        assert!(diff < 30, "interval 偏差过大: {diff}s");
    }

    #[test]
    fn daily_rolls_forward_when_time_in_past() {
        let past = (Local::now() - ChronoDuration::hours(2))
            .time()
            .format("%H:%M")
            .to_string();
        let item = item("daily", Some(&past), None);
        let next = compute_next(&item).expect("daily should compute");
        assert!(next > Local::now(), "过去时间应滚动到未来");
    }

    #[test]
    fn daily_stays_within_26_hours() {
        let future = (Local::now() + ChronoDuration::hours(2))
            .time()
            .format("%H:%M")
            .to_string();
        let item = item("daily", Some(&future), None);
        let next = compute_next(&item).expect("daily should compute");
        let diff = (next - Local::now()).num_minutes();
        assert!(diff > 0 && diff <= 26 * 60, "daily 候选应在 26 小时内: {diff}min");
        assert!(next.time().hour() != Local::now().time().hour() || next > Local::now());
    }

    #[test]
    fn invalid_kind_has_no_next() {
        assert!(compute_next(&item("weekly", None, None)).is_none());
    }
}
