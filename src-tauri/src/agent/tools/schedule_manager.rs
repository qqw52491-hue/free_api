// schedule_manager：把定时任务管理暴露为 Agent 原生工具。
// 用户在对话里说「每天早上九点巡检项目并微信发我简报」时，模型调用本工具
// 创建/维护调度计划；到点由 scheduler 派发任务，结果经桥推送到主人微信。
// 与 GUI「定时任务中心」共用同一份 schedules.json，两侧互通。

use crate::agent::scheduler::{load_schedules, refresh_next, save_schedules, ScheduleItem};
use crate::agent::types::DispatchResult;
use crate::db::DbState;
use tauri::Manager;

pub fn dispatch(db: &DbState, action: &str, params: &serde_json::Value) -> DispatchResult {
    let act = if !action.is_empty() {
        action
    } else {
        params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list")
    };

    match act {
        "list" => list(),
        "save" => save(params),
        "toggle" => toggle(params),
        "delete" => delete(params),
        "run_now" => run_now(db, params),
        _ => DispatchResult {
            stdout: String::new(),
            stderr: format!(
                "schedule_manager 未知 action='{}'。可用：list() / save(name, goal, kind, ...) / toggle(id, enabled) / delete(id, confirm: true) / run_now(id)",
                act
            ),
            success: false,
            route: "schedule_manager:unknown_action".to_string(),
        },
    }
}

fn list() -> DispatchResult {
    match load_schedules() {
        Ok(items) => {
            let rows: Vec<serde_json::Value> = items
                .iter()
                .map(|item| {
                    serde_json::json!({
                        "id": item.id,
                        "name": item.name,
                        "enabled": item.enabled,
                        "kind": item.kind,
                        "time": item.time,
                        "minutes": item.minutes,
                        "project_id": item.project_id,
                        "goal": item.goal,
                        "next_fired_at": item.next_fired_at,
                        "last_fired_at": item.last_fired_at,
                    })
                })
                .collect();
            DispatchResult {
                stdout: serde_json::to_string_pretty(&rows)
                    .unwrap_or_else(|_| "[]".to_string()),
                stderr: String::new(),
                success: true,
                route: "schedule_manager:list".to_string(),
            }
        }
        Err(error) => DispatchResult {
            stdout: String::new(),
            stderr: format!("读取定时任务失败: {error}"),
            success: false,
            route: "schedule_manager:list".to_string(),
        },
    }
}

fn save(params: &serde_json::Value) -> DispatchResult {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let goal = params.get("goal").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let kind = params
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("daily")
        .trim()
        .to_lowercase();
    let time = params.get("time").and_then(|v| v.as_str()).map(|s| s.trim().to_string());
    let minutes = params.get("minutes").and_then(|v| v.as_u64());
    let project_id = params
        .get("project_id")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();

    if name.is_empty() {
        return fail("save", "缺少 name");
    }
    if goal.is_empty() {
        return fail("save", "缺少 goal（任务内容）");
    }
    if let Err(error) = crate::agent::scheduler::validate_schedule(&kind, time.as_deref(), minutes) {
        return fail("save", &error);
    }

    let mut items = match load_schedules() {
        Ok(items) => items,
        Err(error) => return fail("save", &format!("读取计划失败: {error}")),
    };

    let result = if id.is_empty() {
        let mut item = ScheduleItem {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            enabled: true,
            kind,
            time,
            minutes,
            project_id,
            goal,
            session_id: None,
            last_fired_at: None,
            next_fired_at: None,
        };
        refresh_next(&mut item);
        let created = item.id.clone();
        items.push(item);
        Ok(format!("已创建定时任务 {created}"))
    } else {
        match items.iter_mut().find(|item| item.id == id) {
            Some(existing) => {
                existing.name = name;
                existing.kind = kind;
                existing.time = time;
                existing.minutes = minutes;
                if project_id.is_some() {
                    existing.project_id = project_id;
                }
                existing.goal = goal;
                refresh_next(existing);
                Ok(format!("已更新定时任务 {}", existing.id))
            }
            None => Err(format!("定时任务 {id} 不存在")),
        }
    };

    match result {
        Ok(message) => {
            if let Err(error) = save_schedules(&items) {
                return fail("save", &format!("保存计划失败: {error}"));
            }
            DispatchResult {
                stdout: format!("{message}。调度器每 30 秒巡检一次，到点自动派发；完成后结果会推送到主人微信。"),
                stderr: String::new(),
                success: true,
                route: "schedule_manager:save".to_string(),
            }
        }
        Err(message) => fail("save", &message),
    }
}

fn toggle(params: &serde_json::Value) -> DispatchResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let enabled = params.get("enabled").and_then(|v| v.as_bool());
    if id.is_empty() {
        return fail("toggle", "缺少 id");
    }
    let Some(enabled) = enabled else {
        return fail("toggle", "缺少 enabled（boolean）");
    };
    let mut items = match load_schedules() {
        Ok(items) => items,
        Err(error) => return fail("toggle", &format!("读取计划失败: {error}")),
    };
    let toggled = items.iter_mut().find(|item| item.id == id).map(|item| {
        item.enabled = enabled;
        if enabled {
            refresh_next(item);
        }
        item.name.clone()
    });
    match toggled {
        Some(name) => {
            if let Err(error) = save_schedules(&items) {
                return fail("toggle", &format!("保存计划失败: {error}"));
            }
            DispatchResult {
                stdout: format!("定时任务「{name}」已{}", if enabled { "启用" } else { "停用" }),
                stderr: String::new(),
                success: true,
                route: "schedule_manager:toggle".to_string(),
            }
        }
        None => fail("toggle", &format!("定时任务 {id} 不存在")),
    }
}

fn delete(params: &serde_json::Value) -> DispatchResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let confirm = params.get("confirm").and_then(|v| v.as_bool()).unwrap_or(false);
    if id.is_empty() {
        return fail("delete", "缺少 id");
    }
    if !confirm {
        return fail("delete", "删除是破坏性操作：请显式传 confirm: true");
    }
    let mut items = match load_schedules() {
        Ok(items) => items,
        Err(error) => return fail("delete", &format!("读取计划失败: {error}")),
    };
    let before = items.len();
    items.retain(|item| item.id != id);
    if items.len() == before {
        return fail("delete", &format!("定时任务 {id} 不存在"));
    }
    if let Err(error) = save_schedules(&items) {
        return fail("delete", &format!("保存计划失败: {error}"));
    }
    DispatchResult {
        stdout: "已删除定时任务。".to_string(),
        stderr: String::new(),
        success: true,
        route: "schedule_manager:delete".to_string(),
    }
}

/// 立即执行一次计划（不等到点）：复用该计划的会话与目标直接派发
fn run_now(db: &DbState, params: &serde_json::Value) -> DispatchResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if id.is_empty() {
        return fail("run_now", "缺少 id（可先 list() 获取）");
    }
    let items = match load_schedules() {
        Ok(items) => items,
        Err(error) => return fail("run_now", &format!("读取计划失败: {error}")),
    };
    let Some(item) = items.iter().find(|item| item.id == id) else {
        return fail("run_now", &format!("定时任务 {id} 不存在"));
    };
    if !item.enabled {
        return fail("run_now", &format!("定时任务「{}」已停用，请先 toggle 启用", item.name));
    }
    let Some(app) = crate::agent::channel::app_handle() else {
        return fail("run_now", "应用尚未完成初始化");
    };
    let config = match crate::agent::channel::channel_config_shared().read() {
        Ok(config) => config.clone(),
        Err(e) => return fail("run_now", &format!("读取通道配置失败: {e}")),
    };

    // 会话：复用已绑定的；没有或已失效则按计划的项目绑定新建
    let conn = match db.0.lock() {
        Ok(c) => c,
        Err(e) => return fail("run_now", &format!("获取数据库锁失败: {e}")),
    };
    let session_id = match item.session_id.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(sid) => {
            let exists: Option<String> = conn
                .query_row(
                    "SELECT id FROM chat_sessions WHERE id=?1",
                    rusqlite::params![sid],
                    |row| row.get(0),
                )
                .ok();
            match exists {
                Some(_) => sid.to_string(),
                None => match create_session(&conn, &config, item) {
                    Ok(session) => session,
                    Err(dispatch_fail) => return dispatch_fail,
                },
            }
        }
        None => match create_session(&conn, &config, item) {
            Ok(session) => session,
            Err(dispatch_fail) => return dispatch_fail,
        },
    };

    let mut routing = crate::agent::router::ModelRoutingConfig {
        pro: config.pro.clone(),
        flash: config.flash.clone(),
        vision: config.vision.clone(),
        ..Default::default()
    };
    if routing.pro.is_empty() {
        routing.pro = config.model_id.clone();
    }
    if routing.pro.is_empty() {
        return fail("run_now", "未配置默认模型，请在微信绑定页填写");
    }
    if routing.flash.is_empty() {
        routing.flash = routing.pro.clone();
    }
    if routing.vision.is_empty() {
        routing.vision = routing.pro.clone();
    }

    let has_project = {
        let state = app.state::<DbState>();
        let conn = match state.0.lock() {
            Ok(conn) => conn,
            Err(e) => return fail("run_now", &e.to_string()),
        };
        match crate::commands::projects::project_for_session(&conn, &session_id) {
            Ok(project) => project.is_some(),
            Err(e) => return fail("run_now", &e),
        }
    };
    let workspace_override = if has_project {
        None
    } else {
        Some(crate::agent::channel::effective_scratch_dir_pub(&config))
    };
    match crate::agent::channel::register_and_spawn_run(app, session_id, item.goal.clone(), routing, workspace_override, config.llm_retries, config.action_milestone_steps, config.phase_pushes)
    {
        Ok(run_id) => DispatchResult {
            stdout: format!(
                "已立即派发「{}」（run {run_id}），完成后结果会推送到主人微信。",
                item.name
            ),
            stderr: String::new(),
            success: true,
            route: "schedule_manager:run_now".to_string(),
        },
        Err(crate::agent::channel::SubmitError::InFlight) => {
            fail("run_now", "该计划绑定的会话已有在途任务，请等待完成或先停止")
        }
        Err(crate::agent::channel::SubmitError::Other(message)) => fail("run_now", &message),
    }
}

fn create_session(
    conn: &rusqlite::Connection,
    config: &crate::agent::channel::ChannelConfig,
    item: &ScheduleItem,
) -> Result<String, DispatchResult> {
    if config.model_id.trim().is_empty() {
        return Err(fail("run_now", "未配置默认模型，请在微信绑定页填写"));
    }
    let title = format!("定时 · {}", item.name);
    match item.project_id.as_deref().filter(|p| !p.trim().is_empty()) {
        Some(project_id) => crate::commands::projects::create_project_session_row(
            conn,
            project_id,
            &title,
            &config.model_id,
        )
        .map(|session| session.id)
        .map_err(|e| fail("run_now", &e)),
        None => crate::commands::projects::create_plain_session_row(conn, &title, &config.model_id)
            .map(|session| session.id)
            .map_err(|e| fail("run_now", &e)),
    }
}

fn fail(action: &str, message: &str) -> DispatchResult {
    DispatchResult {
        stdout: String::new(),
        stderr: message.to_string(),
        success: false,
        route: format!("schedule_manager:{action}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbState;
    use std::sync::{Arc, Mutex};

    fn dummy_db() -> DbState {
        DbState(Arc::new(Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        )))
    }

    #[test]
    fn unknown_action_fails_with_hint() {
        let result = dispatch(&dummy_db(), "nope", &serde_json::json!({}));
        assert!(!result.success);
        assert!(result.stderr.contains("run_now"));
    }

    #[test]
    fn save_missing_name_fails_before_io() {
        let result = dispatch(&dummy_db(), "save", &serde_json::json!({"goal": "g"}));
        assert!(!result.success);
        assert!(result.stderr.contains("name"));
    }

    #[test]
    fn delete_requires_confirm() {
        let result = dispatch(&dummy_db(), "delete", &serde_json::json!({"id": "x"}));
        assert!(!result.success);
        assert!(result.stderr.contains("confirm"));
    }

    #[test]
    fn run_now_missing_id_fails() {
        let result = dispatch(&dummy_db(), "run_now", &serde_json::json!({}));
        assert!(!result.success);
    }
}
