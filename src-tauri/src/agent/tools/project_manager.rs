// project_manager：把项目（工作区白名单）管理暴露为 Agent 原生工具。
// 用户在对话里说「把 ~/work/foo 登记成项目」时，模型调用本工具完成登记。
// 创建/删除会改变 Agent 可访问的文件边界，均要求显式 confirm: true
// （表示用户在对话中明确提出了该要求）。

use crate::agent::types::DispatchResult;
use crate::db::DbState;

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
        "list" => list(db),
        "create" => create(db, params),
        "delete" => delete(db, params),
        "list_resources" => list_resources(db, params),
        "add_resource" => add_resource(db, params),
        _ => DispatchResult { view: None,
            stdout: String::new(),
            stderr: format!(
                "project_manager 未知 action='{}'。可用：list() / create(name, root_path, confirm: true) / delete(project_id, confirm: true) / list_resources(project_id) / add_resource(project_id, path, label?)",
                act
            ),
            success: false,
            route: "project_manager:unknown_action".to_string(),
        },
    }
}

fn list(db: &DbState) -> DispatchResult {
    let conn = match db.0.lock() {
        Ok(c) => c,
        Err(e) => return fail("list", &format!("获取数据库锁失败: {e}")),
    };
    let result = conn.prepare(
        "SELECT id, name, root_path, updated_at FROM agent_projects ORDER BY name",
    );
    let mut stmt = match result {
        Ok(s) => s,
        Err(e) => return fail("list", &format!("查询失败: {e}")),
    };
    let rows: Vec<serde_json::Value> = stmt
        .query_map([], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "root_path": row.get::<_, String>(2)?,
                "updated_at": row.get::<_, String>(3)?,
            }))
        })
        .map(|iter| iter.flatten().collect())
        .unwrap_or_default();
    DispatchResult { view: None,
        stdout: serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".to_string()),
        stderr: String::new(),
        success: true,
        route: "project_manager:list".to_string(),
    }
}

fn create(db: &DbState, params: &serde_json::Value) -> DispatchResult {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let root_path = params.get("root_path").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let confirm = params.get("confirm").and_then(|v| v.as_bool()).unwrap_or(false);
    if name.is_empty() || root_path.is_empty() {
        return fail("create", "缺少 name 或 root_path");
    }
    if !confirm {
        return fail(
            "create",
            "创建项目会扩大 Agent 可访问的文件范围：请先向用户确认目录，再显式传 confirm: true",
        );
    }
    let conn = match db.0.lock() {
        Ok(c) => c,
        Err(e) => return fail("create", &format!("获取数据库锁失败: {e}")),
    };
    match crate::commands::projects::create_project_row(&conn, &name, &root_path) {
        Ok(project) => DispatchResult { view: None,
            stdout: serde_json::to_string_pretty(&serde_json::json!({
                "message": "项目已创建，之后可在任务/定时计划里通过 project_id 绑定该工作区。",
                "id": project.id,
                "name": project.name,
                "root_path": project.root_path,
            }))
            .unwrap_or_default(),
            stderr: String::new(),
            success: true,
            route: "project_manager:create".to_string(),
        },
        Err(error) => fail("create", &error),
    }
}

fn delete(db: &DbState, params: &serde_json::Value) -> DispatchResult {
    let project_id = params
        .get("project_id")
        .or_else(|| params.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let confirm = params.get("confirm").and_then(|v| v.as_bool()).unwrap_or(false);
    if project_id.is_empty() {
        return fail("delete", "缺少 project_id");
    }
    if !confirm {
        return fail("delete", "删除会级联解除项目与会话/资源关联：请显式传 confirm: true");
    }
    let conn = match db.0.lock() {
        Ok(c) => c,
        Err(e) => return fail("delete", &format!("获取数据库锁失败: {e}")),
    };
    match conn.execute(
        "DELETE FROM agent_projects WHERE id=?1",
        rusqlite::params![project_id],
    ) {
        Ok(0) => fail("delete", "项目不存在或已删除"),
        Ok(_) => DispatchResult { view: None,
            stdout: "项目已删除（关联的会话关系与资源登记已级联清除，工作区文件不受影响）。".to_string(),
            stderr: String::new(),
            success: true,
            route: "project_manager:delete".to_string(),
        },
        Err(e) => fail("delete", &e.to_string()),
    }
}

fn list_resources(db: &DbState, params: &serde_json::Value) -> DispatchResult {
    let project_id = params.get("project_id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if project_id.is_empty() {
        return fail("list_resources", "缺少 project_id（可先 list() 获取）");
    }
    let conn = match db.0.lock() {
        Ok(c) => c,
        Err(e) => return fail("list_resources", &format!("获取数据库锁失败: {e}")),
    };
    let mut stmt = match conn.prepare(
        "SELECT resource_id, label, relative_path, kind, size_bytes FROM agent_project_resources WHERE project_id=?1 ORDER BY created_at DESC",
    ) {
        Ok(s) => s,
        Err(e) => return fail("list_resources", &format!("查询失败: {e}")),
    };
    let rows: Vec<serde_json::Value> = stmt
        .query_map(rusqlite::params![project_id], |row| {
            Ok(serde_json::json!({
                "resource_id": row.get::<_, String>(0)?,
                "label": row.get::<_, String>(1)?,
                "relative_path": row.get::<_, String>(2)?,
                "kind": row.get::<_, String>(3)?,
                "size_bytes": row.get::<_, i64>(4)?,
            }))
        })
        .map(|iter| iter.flatten().collect())
        .unwrap_or_default();
    DispatchResult { view: None,
        stdout: serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".to_string()),
        stderr: String::new(),
        success: true,
        route: "project_manager:list_resources".to_string(),
    }
}

fn add_resource(db: &DbState, params: &serde_json::Value) -> DispatchResult {
    let project_id = params.get("project_id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let raw_path = params
        .get("path")
        .or_else(|| params.get("file_path"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if project_id.is_empty() || raw_path.is_empty() {
        return fail("add_resource", "缺少 project_id 或 path");
    }
    let conn = match db.0.lock() {
        Ok(c) => c,
        Err(e) => return fail("add_resource", &format!("获取数据库锁失败: {e}")),
    };
    match crate::commands::projects::add_project_resource_row(&conn, &project_id, &raw_path) {
        Ok(resource) => DispatchResult { view: None,
            stdout: serde_json::to_string_pretty(&serde_json::json!({
                "message": "已登记为受管资源，同项目所有会话可按 resource_id 读取。",
                "resource_id": resource.resource_id,
                "label": resource.label,
                "relative_path": resource.relative_path,
            }))
            .unwrap_or_default(),
            stderr: String::new(),
            success: true,
            route: "project_manager:add_resource".to_string(),
        },
        Err(error) => fail("add_resource", &error),
    }
}

fn fail(action: &str, message: &str) -> DispatchResult {
    DispatchResult { view: None,
        stdout: String::new(),
        stderr: message.to_string(),
        success: false,
        route: format!("project_manager:{action}"),
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
    fn create_requires_confirm_to_expand_scope() {
        let result = dispatch(
            &dummy_db(),
            "create",
            &serde_json::json!({"name": "demo", "root_path": "/tmp/demo"}),
        );
        assert!(!result.success);
        assert!(result.stderr.contains("confirm"));
    }

    #[test]
    fn create_missing_fields_fail_early() {
        let result = dispatch(&dummy_db(), "create", &serde_json::json!({"confirm": true}));
        assert!(!result.success);
    }

    #[test]
    fn delete_requires_confirm() {
        let result = dispatch(
            &dummy_db(),
            "delete",
            &serde_json::json!({"project_id": "whatever"}),
        );
        assert!(!result.success);
        assert!(result.stderr.contains("confirm"));
    }

    #[test]
    fn unknown_action_fails_with_hint() {
        let result = dispatch(&dummy_db(), "nope", &serde_json::json!({}));
        assert!(!result.success);
        assert!(result.stderr.contains("project_manager"));
    }
}
