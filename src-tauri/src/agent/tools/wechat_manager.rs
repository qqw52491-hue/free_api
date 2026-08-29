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
        "recent_runs" => recent_runs(params),
        _ => DispatchResult {
            stdout: String::new(),
            stderr: format!(
                "wechat_manager 未知 action='{}'。可用：status() / notify(text) / recent_runs(limit?)",
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
    DispatchResult {
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
            DispatchResult {
                stdout: "已发起微信推送（异步送达，桥离线时会在日志中记录失败）。".to_string(),
                stderr: String::new(),
                success: true,
                route: "wechat_manager:notify".to_string(),
            }
        }
        Err(_) => fail("notify", "当前不在异步运行时中，无法发起推送"),
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
    DispatchResult {
        stdout: serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".to_string()),
        stderr: String::new(),
        success: true,
        route: "wechat_manager:recent_runs".to_string(),
    }
}

fn fail(action: &str, message: &str) -> DispatchResult {
    DispatchResult {
        stdout: String::new(),
        stderr: message.to_string(),
        success: false,
        route: format!("wechat_manager:{action}"),
    }
}
