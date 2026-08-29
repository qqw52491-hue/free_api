// system_info：把本机运行状态暴露为 Agent 原生工具。
// 「看看机器现在负载」「磁盘还剩多少」之类的问题直接可答，不必借道 shell。

use crate::agent::types::DispatchResult;
use crate::db::DbState;
use sysinfo::System;

pub fn dispatch(_db: &DbState, action: &str, params: &serde_json::Value) -> DispatchResult {
    let act = if !action.is_empty() {
        action
    } else {
        params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("overview")
    };

    match act {
        "overview" | "list" => overview(),
        _ => DispatchResult {
            stdout: String::new(),
            stderr: format!(
                "system_info 未知 action='{}'。可用：overview()",
                act
            ),
            success: false,
            route: "system_info:unknown_action".to_string(),
        },
    }
}

fn overview() -> DispatchResult {
    let mut system = System::new();
    system.refresh_memory();
    let load = System::load_average();
    let disks = sysinfo::Disks::new_with_refreshed_list();

    let gb = |bytes: u64| format!("{:.1} GB", bytes as f64 / 1024.0 / 1024.0 / 1024.0);

    let disk_rows: Vec<serde_json::Value> = disks
        .iter()
        .filter(|disk| disk.total_space() > 0)
        .map(|disk| {
            serde_json::json!({
                "mount": disk.mount_point().to_string_lossy(),
                "total": gb(disk.total_space()),
                "available": gb(disk.available_space()),
            })
        })
        .collect();

    let payload = serde_json::json!({
        "host": System::host_name().unwrap_or_default(),
        "os": System::long_os_version().unwrap_or_default(),
        "load_average": {
            "1m": format!("{:.2}", load.one),
            "5m": format!("{:.2}", load.five),
            "15m": format!("{:.2}", load.fifteen),
        },
        "memory": {
            "total": gb(system.total_memory()),
            "used": gb(system.used_memory()),
            "usage_percent": format!("{:.1}%", system.used_memory() as f64 * 100.0 / system.total_memory().max(1) as f64),
        },
        "disks": disk_rows,
    });

    DispatchResult {
        stdout: serde_json::to_string_pretty(&payload).unwrap_or_default(),
        stderr: String::new(),
        success: true,
        route: "system_info:overview".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbState;
    use std::sync::{Arc, Mutex};

    #[test]
    fn overview_reports_memory_and_disks() {
        let db = DbState(Arc::new(Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        )));
        let result = dispatch(&db, "overview", &serde_json::json!({}));
        assert!(result.success);
        assert!(result.stdout.contains("memory"));
        assert!(result.stdout.contains("disks"));
    }

    #[test]
    fn unknown_action_fails() {
        let db = DbState(Arc::new(Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        )));
        let result = dispatch(&db, "nope", &serde_json::json!({}));
        assert!(!result.success);
    }
}
