// wechat_manager：把微信通道自身暴露为 Agent 原生工具。
// - status()：桥在线/登录状态（最近一次探测缓存）+ 通道配置摘要
// - notify(text)：任务执行途中主动给主人微信捎话（非阻塞、不等待，不影响 run）
// - recent_runs()：最近通道任务的状态流水（含进度摘要与失败原因）
// 与 ask_human 的区别：notify 是「知会」，不阻塞等回复；需要用户决定时仍用 ask_human。

use crate::agent::types::DispatchResult;
use crate::db::DbState;

pub fn dispatch(_db: &DbState, action: &str, params: &serde_json::Value) -> DispatchResult {
    let act = if !action.is_empty() {
        action
    } else {
        params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("status")
    };

    match act {
        "status" => status(),
        "notify" => notify(params),
        "set_heartbeat" => set_heartbeat(params),
        "set_report_style" => set_report_style(params),
        "recent_runs" => recent_runs(params),
        _ => DispatchResult { view: None,
            stdout: String::new(),
            stderr: format!(
                "wechat_manager 未知 action='{}'。可用：status() / notify(text) / set_heartbeat(minutes) / set_report_style(action_milestone_steps?, phase_pushes?) / recent_runs(limit?)",
                act
            ),
            success: false,
            route: "wechat_manager:unknown_action".to_string(),
        },
    }
}

fn status() -> DispatchResult {
    let config = match crate::agent::channel::load_channel_config() {
        Ok(config) => config,
        Err(error) => return fail("status", &format!("读取通道配置失败: {error}")),
    };
    let payload = serde_json::json!({
        "bridge_last_seen": crate::agent::channel::cached_bridge_status(),
        "bridge_port": config.bridge_port,
        "auto_start_bridge": config.auto_start_bridge,
        "heartbeat_minutes": config.heartbeat_minutes,
        "allowlist": config.allowlist,
        "scratch_dir": crate::agent::channel::effective_scratch_dir_pub(&config)
            .to_string_lossy(),
        "hint": "bridge_last_seen 为最近一次成功探测的桥状态；null 表示桥未运行或从未连通。",
    });
    DispatchResult { view: None,
        stdout: serde_json::to_string_pretty(&payload).unwrap_or_default(),
        stderr: String::new(),
        success: true,
        route: "wechat_manager:status".to_string(),
    }
}

fn notify(params: &serde_json::Value) -> DispatchResult {
    let text = params
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() {
        return fail("notify", "缺少 text（要告知用户的内容）");
    }
    if text.chars().count() > 2000 {
        return fail("notify", "text 过长（上限 2000 字符），请精简为知会要点");
    }
    // fire-and-forget：不在工具调用线程上等待网络；仅在 tokio 运行时内可用
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                let config = crate::agent::channel::load_channel_config()
                    .unwrap_or_default();
                crate::agent::channel::notify_wechat(&config, &text).await;
            });
            DispatchResult { view: None,
                stdout: "已发起微信推送（异步送达，桥离线时会在日志中记录失败）。".to_string(),
                stderr: String::new(),
                success: true,
                route: "wechat_manager:notify".to_string(),
            }
        }
        Err(_) => fail("notify", "当前不在异步运行时中，无法发起推送"),
    }
}

/// 对话内调整进度汇报间隔：{"minutes": 10}
fn set_heartbeat(params: &serde_json::Value) -> DispatchResult {
    let minutes = match params.get("minutes").and_then(|v| v.as_u64()) {
        Some(v) if v >= 1 => v.clamp(1, 1440),
        _ => return fail("set_heartbeat", "缺少 minutes（1–1440 分钟）"),
    };
    let mut config = match crate::agent::channel::load_channel_config() {
        Ok(config) => config,
        Err(error) => return fail("set_heartbeat", &format!("读取通道配置失败: {error}")),
    };
    config.heartbeat_minutes = minutes;
    if let Err(error) = crate::agent::channel::save_channel_config(&config) {
        return fail("set_heartbeat", &format!("保存配置失败: {error}"));
    }
    crate::agent::channel::sync_shared_config(&config);
    DispatchResult { view: None,
        stdout: format!("✅ 已调整为每 {minutes} 分钟汇报一次进度（仅影响微信侧任务）。"),
        stderr: String::new(),
        success: true,
        route: "wechat_manager:set_heartbeat".to_string(),
    }
}

/// 调整汇报详略：{"action_milestone_steps": 0|1..1000, "phase_pushes": bool}
/// 步长 0 = 关闭"每 N 个动作"汇报；phase_pushes 控制阶段里程碑推送。
/// 只改持久默认（对之后提交的任务生效），正在运行的任务不受影响。
fn set_report_style(params: &serde_json::Value) -> DispatchResult {
    let steps = params.get("action_milestone_steps").and_then(|v| v.as_u64());
    let phase = params.get("phase_pushes").and_then(|v| v.as_bool());
    if steps.is_none() && phase.is_none() {
        return fail(
            "set_report_style",
            "至少提供 action_milestone_steps（0=关闭动作汇报，1–1000）或 phase_pushes（true/false）",
        );
    }
    let mut config = match crate::agent::channel::load_channel_config() {
        Ok(config) => config,
        Err(error) => return fail("set_report_style", &format!("读取通道配置失败: {error}")),
    };
    if let Some(v) = steps {
        config.action_milestone_steps = (v.clamp(0, 1000)) as u32;
    }
    if let Some(v) = phase {
        config.phase_pushes = v;
    }
    if let Err(error) = crate::agent::channel::save_channel_config(&config) {
        return fail("set_report_style", &format!("保存配置失败: {error}"));
    }
    crate::agent::channel::sync_shared_config(&config);
    DispatchResult { view: None,
        stdout: format!(
            "✅ 汇报风格已更新：动作里程碑每 {} 个动作{}；阶段里程碑{}。对之后提交的任务生效。",
            config.action_milestone_steps,
            if config.action_milestone_steps == 0 { "（已关闭）" } else { "" },
            if config.phase_pushes { "开" } else { "关" }
        ),
        stderr: String::new(),
        success: true,
        route: "wechat_manager:set_report_style".to_string(),
    }
}

fn recent_runs(params: &serde_json::Value) -> DispatchResult {
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10).clamp(1, 50) as usize;
    let mut runs = crate::agent::channel::runs_store_snapshot();
    runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    runs.truncate(limit);
    let rows: Vec<serde_json::Value> = runs
        .iter()
        .map(|record| {
            serde_json::json!({
                "run_id": record.run_id,
                "session_id": record.session_id,
                "goal": record.goal_preview,
                "status": record.status,
                "paused_reason": record.paused_reason,
                "last_progress": record.last_progress,
                "event_count": record.event_count,
                "llm_retries": record.llm_retries,
                "started_at": record.started_at,
                "error": record.error,
            })
        })
        .collect();
    DispatchResult { view: None,
        stdout: serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".to_string()),
        stderr: String::new(),
        success: true,
        route: "wechat_manager:recent_runs".to_string(),
    }
}

fn fail(action: &str, message: &str) -> DispatchResult {
    DispatchResult { view: None,
        stdout: String::new(),
        stderr: message.to_string(),
        success: false,
        route: format!("wechat_manager:{action}"),
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
        assert!(result.stderr.contains("set_heartbeat"));
    }

    #[test]
    fn notify_requires_text() {
        let result = dispatch(&dummy_db(), "notify", &serde_json::json!({"text": ""}));
        assert!(!result.success);
        assert!(result.stderr.contains("text"));
    }

    #[test]
    fn set_heartbeat_requires_minutes() {
        let result = dispatch(&dummy_db(), "set_heartbeat", &serde_json::json!({}));
        assert!(!result.success);
        assert!(result.stderr.contains("minutes"));
    }

    #[test]
    fn recent_runs_returns_json_array() {
        let result = dispatch(&dummy_db(), "recent_runs", &serde_json::json!({"limit": 3}));
        assert!(result.success);
        let parsed: serde_json::Value = serde_json::from_str(&result.stdout).unwrap();
        assert!(parsed.is_array());
    }
}

#[cfg(test)]
mod report_style_tests {
    use super::*;
    use crate::db::DbState;
    use std::sync::{Arc, Mutex};

    #[test]
    fn set_report_style_requires_params() {
        let db = DbState(Arc::new(Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        )));
        let result = dispatch(&db, "set_report_style", &serde_json::json!({}));
        assert!(!result.success);
        assert!(result.stderr.contains("action_milestone_steps"));
    }
}
