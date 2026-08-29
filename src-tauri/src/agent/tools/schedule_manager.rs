// schedule_manager：把定时任务管理暴露为 Agent 原生工具。
// 用户在对话里说「每天早上九点巡检项目并微信发我简报」时，模型调用本工具
// 创建/维护调度计划；到点由 scheduler 派发任务，结果经桥推送到主人微信。
// 与 GUI「定时任务中心」共用同一份 schedules.json，两侧互通。

use crate::agent::scheduler::{load_schedules, refresh_next, save_schedules, ScheduleItem};
use crate::agent::types::DispatchResult;
use crate::db::DbState;

pub fn dispatch(_db: &DbState, action: &str, params: &serde_json::Value) -> DispatchResult {
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
        _ => DispatchResult {
            stdout: String::new(),
            stderr: format!(
                "schedule_manager 未知 action='{}'。可用：list() / save(name, goal, kind, ...) / toggle(id, enabled) / delete(id, confirm: true)",
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
    if kind != "daily" && kind != "interval" {
        return fail("save", "kind 仅支持 daily / interval");
    }
    if kind == "daily"
        && time
            .as_deref()
            .map(|t| chrono::NaiveTime::parse_from_str(t, "%H:%M").is_ok())
            != Some(true)
    {
        return fail("save", "daily 需要合法的 time（HH:MM）");
    }
    if kind == "interval" && minutes.unwrap_or(0) == 0 {
        return fail("save", "interval 需要大于 0 的 minutes");
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

fn fail(action: &str, message: &str) -> DispatchResult {
    DispatchResult {
        stdout: String::new(),
        stderr: message.to_string(),
        success: false,
        route: format!("schedule_manager:{action}"),
    }
}
