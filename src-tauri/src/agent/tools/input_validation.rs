//! 无副作用的调用预检：先反馈参数错误，再进入原有权限/方案门禁。
//! 不猜测模型意图，不拒绝未知扩展字段；默认值与既有别名保持不变。
use crate::agent::types::{AgentInstruction, DispatchResult};
use crate::db::DbState;
use serde_json::Value;

pub(crate) const INVALID_INPUT_ROUTE: &str = "tool_input:invalid_parameters";

fn first<'a>(params: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|key| params.get(*key))
}

fn text(tool: &str, params: &Value, keys: &[&str], allow_empty: bool) -> Result<(), String> {
    match first(params, keys) {
        Some(Value::String(s)) if allow_empty || !s.trim().is_empty() => Ok(()),
        None | Some(Value::Null) => Err(format!(
            "❌ {tool} 缺少必填参数 '{}'（期望 string）。请补齐后重试。",
            keys[0]
        )),
        Some(Value::String(_)) => Err(format!("❌ {tool}.{} 不能为空白字符串。", keys[0])),
        Some(v) => Err(type_error(tool, keys[0], "string", v)),
    }
}

fn value_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_error(tool: &str, field: &str, expected: &str, value: &Value) -> String {
    // 只回类型，不回参数值（入参可能包含凭证或用户内容）。
    format!(
        "❌ {tool}.{field} 类型错误：期望 {expected}，实际为 {}。请按字段类型重试。",
        value_type(value)
    )
}

fn optional(tool: &str, params: &Value, field: &str, kind: &str) -> Result<(), String> {
    let Some(v) = params.get(field) else {
        return Ok(());
    };
    let valid = match kind {
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "非负整数" => v.as_u64().is_some(),
        "object" => v.is_object(),
        "string[]" => v.as_array().is_some_and(|a| a.iter().all(Value::is_string)),
        _ => true,
    };
    if valid {
        Ok(())
    } else {
        Err(type_error(tool, field, kind, v))
    }
}

/// 与实际执行层使用同一别名优先级；MCP 命名空间不套本地工具契约。
pub(crate) fn validate_native(tool: &str, action: &str, params: &Value) -> Result<(), String> {
    let tool = tool.to_ascii_lowercase();
    let action = action.to_ascii_lowercase();
    let act = if action.is_empty() {
        params.get("action").and_then(Value::as_str).unwrap_or("")
    } else {
        &action
    };
    match tool.as_str() {
        "local_shell" => {
            if let Err(error) = text(&tool, params, &["command", "cmd", "commandline"], false) {
                return Err(format!("{error}\n示例：{{\"command\":\"cargo check\"}}。若要列目录，请使用 local_file(action=\"list\", path=\".\")。"));
            }
            optional(&tool, params, "args", "string[]")?;
            optional(&tool, params, "cwd", "string")?;
            optional(&tool, params, "background", "boolean")?;
        }
        "local_file" => {
            let act = if act.is_empty() { "read" } else { act };
            let act = super::local_file::resolve_act(act, params);
            let write_shaped = ["content", "target_content", "replacement_content"]
                .iter()
                .any(|k| params.get(*k).is_some());
            let act = if act == "local_file" && !write_shaped {
                "read"
            } else {
                act
            };
            match act {
                "read" | "write" | "edit" => text(&tool, params, &["path"], false)?,
                "list" => optional(&tool, params, "path", "string")?,
                _ => return Ok(()), // 未知动作由原执行层报错，不改变既有动作容错。
            }
            if act == "write" {
                text(&tool, params, &["content"], true)?;
            }
            if act == "edit" {
                text(&tool, params, &["target_content"], false)?;
                text(&tool, params, &["replacement_content"], true)?;
            }
            optional(&tool, params, "force", "boolean")?;
        }
        "mcp_manager" => {
            match act {
                "add_http" | "quick_connect" => text(&tool, params, &["url"], false)?,
                "delete" | "toggle" => text(&tool, params, &["name"], false)?,
                "install_ref" => text(&tool, params, &["reference", "package", "url"], false)?,
                _ => {}
            }
            if act == "toggle" {
                optional(&tool, params, "enabled", "boolean")?;
            }
        }
        "artifact_manager" | "artifact_store" => {
            if matches!(
                act,
                "" | "create_document" | "save_artifact" | "write_document"
            ) {
                // 缺正文不是空文件；显式 content:"" 仍是合法的空产物。
                text(
                    &tool,
                    params,
                    &["content", "content_markdown", "markdown", "code", "html"],
                    true,
                )?;
            }
        }
        "wait_jobs" => {
            optional(&tool, params, "job_ids", "string[]")?;
            optional(&tool, params, "timeout_secs", "非负整数")?;
        }
        "watch_stop" => optional(&tool, params, "watch_id", "string")?,
        "job_cancel" => text(&tool, params, &["job_id", "id"], false)?,
        "child_writer" if matches!(act, "apply" | "apply_diff" | "apply_changes") => {
            text(&tool, params, &["child_run_id"], false)?;
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn rejection(error: String) -> DispatchResult {
    DispatchResult {
        view: None,
        stdout: String::new(),
        stderr: error,
        success: false,
        route: INVALID_INPUT_ROUTE.to_string(),
    }
}

pub(crate) fn native_rejection(tool: &str, action: &str, params: &Value) -> Option<DispatchResult> {
    validate_native(tool, action, params).err().map(rejection)
}

pub(crate) fn validate_child_item(db: &DbState, tool: &str, params: &Value) -> Result<(), String> {
    if !params.is_object() {
        return Err(type_error(tool, "params", "object", params));
    }
    optional(tool, params, "role", "string")?;
    optional(tool, params, "custom_inputs", "object")?;
    if tool == "child_writer" {
        optional(tool, params, "test_command", "string")?;
    }
    let mut effective = params.clone();
    if let Some(role) = params.get("role").and_then(Value::as_str) {
        // 角色不存在/已禁用/DB 读取失败仍交给原权限派发层；预检不替代权限裁决。
        if let Ok(Some(card)) = super::role_manager::resolve_enabled_card(db, role) {
            super::role_manager::validate_dispatch_inputs(&card, params.get("custom_inputs"))?;
            if tool == "child_writer" {
                effective = super::role_manager::merge_writer_defaults(&card, params);
            }
        }
    }
    if tool == "child_writer" {
        optional(tool, params, "focus_paths", "string[]")?;
        crate::agent::child_writer::parse_writer_request(&effective)?;
    }
    Ok(())
}

/// 主循环在任何会改变审查/冷却/读取台账的门禁之前调用。
/// 复合树的每个原生叶子还会在 execute_command_inner 内单独预检。
pub(crate) fn instruction_rejection(
    db: &DbState,
    inst: &AgentInstruction,
) -> Option<DispatchResult> {
    if inst.submit_plan.is_some() || inst.prepare_finish.is_some() {
        return None;
    }
    let raw_tool = inst.get_tool().to_ascii_lowercase();
    let action = inst.get_action();
    let action_lc = action.to_ascii_lowercase();
    let tool = if raw_tool == "core"
        && matches!(
            action_lc.as_str(),
            "wait_jobs"
                | "watch_stop"
                | "job_cancel"
                | "child_writer"
                | "child_worker"
                | "child_reviewer"
        ) {
        action_lc.as_str()
    } else {
        raw_tool.as_str()
    };
    let params = inst.get_params();
    if tool == "child_writer"
        && matches!(action_lc.as_str(), "apply" | "apply_diff" | "apply_changes")
    {
        return native_rejection(tool, &action, &params);
    }
    if matches!(tool, "child_writer" | "child_worker" | "child_reviewer") {
        let items = inst.get_dispatch_items();
        let errors: Vec<_> = items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                validate_child_item(db, tool, item)
                    .err()
                    .map(|e| format!("[{}/{}] {e}", i + 1, items.len()))
            })
            .collect();
        // 保持批量部分成功语义：有合法项就交原派发层逐项裁决，不吞掉合法兄弟项。
        if !errors.is_empty() && errors.len() == items.len() {
            return Some(rejection(errors.join("\n")));
        }
        return None;
    }
    native_rejection(tool, &action, &params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    fn empty_db() -> DbState {
        DbState(Arc::new(Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        )))
    }

    #[test]
    fn malformed_shell_gets_parameter_error_not_review() {
        let inst: AgentInstruction = serde_json::from_value(
            json!({"tool":"local_shell","command":{"path":"src/components/agent","depth":1}}),
        )
        .unwrap();
        let r = instruction_rejection(&empty_db(), &inst).unwrap();
        assert_eq!(r.route, INVALID_INPUT_ROUTE);
        assert!(r.stderr.contains("'command'"));
        assert!(r.stderr.contains("local_file"));
        assert!(!r.stderr.contains("审查"));
    }

    #[test]
    fn local_file_missing_write_fields_are_preflight_errors() {
        for (action, params, missing) in [
            ("write", json!({"path":"a.txt"}), "content"),
            (
                "edit",
                json!({"path":"a.txt","target_content":"a"}),
                "replacement_content",
            ),
        ] {
            let r = native_rejection("local_file", action, &params).unwrap();
            assert!(r.stderr.contains(missing));
        }
        assert!(validate_native(
            "local_file",
            "write",
            &json!({"path":"a","content":"","future_key":true})
        )
        .is_ok());
        assert!(validate_native(
            "local_file",
            "edit",
            &json!({"path":"a","target_content":"a","replacement_content":""})
        )
        .is_ok());
    }

    #[test]
    fn local_file_read_alias_and_inner_read_are_preserved() {
        assert!(validate_native("local_file", "local_file", &json!({"path":"a.txt"})).is_ok());
        assert!(validate_native(
            "local_file",
            "write",
            &json!({"action":"read","path":"a.txt"})
        )
        .is_ok());
        assert!(validate_native("local_file", "list", &json!({})).is_ok());
    }

    #[test]
    fn shell_aliases_and_extra_fields_are_preserved() {
        for k in ["command", "cmd", "commandline"] {
            let mut p = json!({"args":["a"],"future_key":"ok"});
            p[k] = json!("echo");
            assert!(validate_native("LOCAL_SHELL", "exec", &p).is_ok());
        }
        assert!(validate_native("remote/local_shell", "exec", &json!({"arbitrary":42})).is_ok());
    }

    #[test]
    fn mcp_missing_fields_precede_confirmation_and_wrong_enabled_is_not_true() {
        assert!(native_rejection("mcp_manager", "add_http", &json!({}))
            .unwrap()
            .stderr
            .contains("url"));
        assert!(validate_native("mcp_manager", "toggle", &json!({"name":"x"})).is_ok());
        assert!(validate_native(
            "mcp_manager",
            "toggle",
            &json!({"name":"x","enabled":false})
        )
        .is_ok());
        for bad in [json!("false"), json!(null), json!(0)] {
            assert!(
                validate_native("mcp_manager", "toggle", &json!({"name":"x","enabled":bad}))
                    .is_err()
            );
        }
        assert!(validate_native("mcp_manager", "install_ref", &json!({"package":"pkg"})).is_ok());
    }

    #[test]
    fn artifact_content_is_required_but_explicit_empty_and_aliases_are_allowed() {
        for p in [json!({}), json!({"content":null}), json!({"content":42})] {
            assert!(validate_native("artifact_manager", "create_document", &p).is_err());
        }
        for key in ["content", "content_markdown", "markdown", "code", "html"] {
            let mut p = json!({});
            p[key] = json!("");
            assert!(validate_native("artifact_store", "save_artifact", &p).is_ok());
        }
        assert!(validate_native("artifact_manager", "list_artifacts", &json!({})).is_ok());
    }

    #[test]
    fn optional_job_and_watch_fields_distinguish_absent_from_wrong_type() {
        assert!(validate_native("wait_jobs", "", &json!({})).is_ok());
        assert!(validate_native("wait_jobs", "", &json!({"job_ids":[]})).is_ok());
        for p in [
            json!({"job_ids":"id"}),
            json!({"job_ids":["id",1]}),
            json!({"job_ids":null}),
        ] {
            assert!(validate_native("wait_jobs", "", &p).is_err());
        }
        assert!(validate_native("watch_stop", "", &json!({})).is_ok());
        assert!(validate_native("watch_stop", "", &json!({"watch_id":"id"})).is_ok());
        assert!(validate_native("watch_stop", "", &json!({"watch_id":12})).is_err());
    }

    #[test]
    fn writer_request_and_apply_are_validated_before_dispatch() {
        for p in [json!({}), json!({"task":"fix"})] {
            let inst: AgentInstruction =
                serde_json::from_value(json!({"tool":"child_writer","command":p})).unwrap();
            assert!(instruction_rejection(&empty_db(), &inst).is_some());
        }
        assert!(native_rejection("child_writer", "apply_diff", &json!({})).is_some());
        assert!(validate_native("child_writer", "apply", &json!({"child_run_id":"id"})).is_ok());
    }

    #[test]
    fn mixed_child_batch_keeps_existing_partial_success_semantics() {
        let inst: AgentInstruction =
            serde_json::from_value(json!({"tool":"child_writer","dispatches":[
                {"task":"fix","test_command":"cargo check"}, {"task":"fix"}
            ]}))
            .unwrap();
        assert!(instruction_rejection(&empty_db(), &inst).is_none());
    }
    #[test]
    fn invalid_toggle_cannot_update_plugin_row() {
        let db = empty_db();
        db.0.lock().unwrap().execute_batch("CREATE TABLE mcp_plugins(name TEXT, enabled INTEGER); INSERT INTO mcp_plugins VALUES ('test',0);").unwrap();
        let r = super::super::mcp_manager::dispatch(
            &db,
            "toggle",
            &json!({"name":"test","enabled":"false"}),
        );
        assert_eq!(r.route, INVALID_INPUT_ROUTE);
        let enabled: i64 =
            db.0.lock()
                .unwrap()
                .query_row(
                    "SELECT enabled FROM mcp_plugins WHERE name='test'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(enabled, 0);
    }

    #[test]
    fn batch_invalid_item_is_rejected_without_discarding_valid_sibling() {
        let db = empty_db();
        let bad = json!({"task":"fix","test_command":"cargo check","focus_paths":"a.rs"});
        let good = json!({"task":"fix","test_command":"cargo check"});
        assert!(validate_child_item(&db, "child_writer", &bad).is_err());
        assert!(validate_child_item(&db, "child_writer", &good).is_ok());
        let inst: AgentInstruction =
            serde_json::from_value(json!({"tool":"child_writer","dispatches":[bad,good]})).unwrap();
        assert!(instruction_rejection(&db, &inst).is_none());
    }

    #[test]
    fn role_writer_defaults_still_apply_before_required_request_checks() {
        let db = empty_db();
        db.0.lock().unwrap().execute_batch("CREATE TABLE agent_role_cards(id TEXT, name TEXT, label TEXT, model_id TEXT, persona_prompt TEXT, scope_hints TEXT, default_test_command TEXT, enabled INTEGER, allowed_kinds TEXT, hard_scope TEXT, max_concurrency INTEGER, custom_schema TEXT, focus_domains TEXT, input_schema TEXT, allowed_skills TEXT, allowed_mcp TEXT);").unwrap();
        db.0.lock().unwrap().execute_batch("INSERT INTO agent_role_cards VALUES ('id','writer_role','Writer','','','src/a.rs','cargo check',1,'writer','',0,'','','','','');").unwrap();
        let inst: AgentInstruction = serde_json::from_value(
            json!({"tool":"child_writer","command":{"role":"writer_role","task":"fix"}}),
        )
        .unwrap();
        assert!(
            super::super::role_manager::resolve_enabled_card(&db, "writer_role")
                .unwrap()
                .is_some()
        );
        assert!(
            instruction_rejection(&db, &inst).is_none(),
            "角色默认测试命令须先合并，不能被预检误报缺参"
        );
        assert!(
            validate_child_item(
                &db,
                "child_writer",
                &json!({"role":"writer_role","task":"fix","test_command":false})
            )
            .is_err(),
            "传错不能被角色默认值悄悄替换"
        );
    }
}
