//! A role preset changes task context and defaults, never the business dispatcher.
use serde_json::{json, Value};

pub(crate) fn requested(mut params: Value, preset: Option<&str>) -> Result<Value, String> {
    if !params.is_object() {
        return Err("子任务参数类型错误：期望对象。".into());
    }
    if let Some(preset) = preset {
        params["profile"] = json!(preset);
    }
    let profile = params["profile"].as_str().unwrap_or("task").to_string();
    if profile == "worker" && params.get("task").is_none() {
        if let Some(task) = ["question", "goal", "description", "prompt"]
            .iter()
            .find_map(|key| {
                params[*key]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_owned)
            })
        {
            params["task"] = json!(task);
        }
    }
    if params.get("role").is_none() {
        let role = match profile.as_str() {
            "worker" => Some(crate::agent::tools::role_manager::SYSTEM_WORKER),
            "writer" => Some(crate::agent::tools::role_manager::SYSTEM_WRITER),
            _ => None,
        };
        if let Some(role) = role {
            params["role"] = json!(role);
        }
    }
    Ok(params)
}

/// Explicit follow-up attempts retain declared inputs, while current role and
/// parent permissions are still revalidated by the normal registration path.
pub(crate) fn inherit_resume(
    db: &crate::db::DbState,
    session: &str,
    mut params: Value,
) -> Result<Value, String> {
    use rusqlite::OptionalExtension;
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let raw:Option<String>=conn.query_row("SELECT execution_checkpoint_json FROM agent_child_runs WHERE id=?1 AND parent_session_id=?2 AND kind='task'",rusqlite::params![params["child_run_id"].as_str().unwrap_or_default(),session],|r|r.get(0)).optional().map_err(|e|e.to_string())?.flatten();
    if params.get("model_id").is_none() {
        let model: Option<String> = conn.query_row("SELECT model_id FROM agent_child_runs WHERE id=?1 AND parent_session_id=?2 AND kind='task'", rusqlite::params![params["child_run_id"].as_str().unwrap_or_default(), session], |r| r.get(0)).optional().map_err(|e| e.to_string())?;
        if let Some(model) = model.filter(|m| !m.is_empty()) { params["model_id"] = json!(model); }
    }
    if let Some(raw) = raw {
        let snapshot: Value =
            serde_json::from_str(&raw).map_err(|e| format!("原派发输入快照损坏：{e}"))?;
        let original = &snapshot["state"]["dispatch_input"]["params"];
        for key in [
            "profile",
            "role",
            "custom_inputs",
            "execution",
            "test_command",
            "context_excerpts",
            "focus_paths",
        ] {
            if params.get(key).is_none() {
                if let Some(value) = original.get(key) {
                    params[key] = value.clone();
                }
            }
        }
    }
    Ok(params)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_aliases_select_real_presets_without_replacing_role_or_contract_inputs() {
        let input = json!({"question":"inspect","role":"my_investigator","custom_inputs":{"scope":"api"},"execution":{"allowed_tools":["my_mcp/read"]}});
        let params = requested(input.clone(), Some("worker")).unwrap();
        assert_eq!(params["profile"], "worker");
        assert_eq!(params["task"], "inspect");
        assert_eq!(params["role"], input["role"]);
        assert_eq!(params["custom_inputs"], input["custom_inputs"]);
        assert_eq!(params["execution"], input["execution"]);
        assert!(crate::agent::execution::runtime::uses_main_executor("worker", &params).unwrap());
    }
    #[test]
    fn explicit_resume_retains_inputs_and_configuration_but_never_reads_another_owner() {
        let db = crate::db::DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&std::path::PathBuf::from(":memory:")).unwrap(),
        )));
        let original = json!({"state":{"dispatch_input":{"params":{"profile":"worker","role":"investigator","custom_inputs":{"scope":"api"},"execution":{"allowed_tools":["local_file.read"]}}}}});
        db.0.lock().unwrap().execute("INSERT INTO agent_child_runs(id,parent_run_id,parent_session_id,workspace_root,model_id,status,started_at,kind,execution_checkpoint_json) VALUES ('prior','parent','owner','/tmp','mock','succeeded','now','task',?1)",[original.to_string()]).unwrap();
        let args = json!({"child_run_id":"prior","message":"continue"});
        let inherited = inherit_resume(&db, "owner", args.clone()).unwrap();
        assert_eq!(inherited["model_id"], "mock");
        assert_eq!(inherit_resume(&db,"owner",json!({"child_run_id":"prior","message":"new attempt","model_id":"explicit-pro"})).unwrap()["model_id"],"explicit-pro");
        assert_eq!(inherited["role"], "investigator");
        assert_eq!(inherited["custom_inputs"]["scope"], "api");
        assert_eq!(
            inherited["execution"],
            original["state"]["dispatch_input"]["params"]["execution"]
        );
        assert_eq!(inherit_resume(&db, "outsider", args.clone()).unwrap(), args);
    }
    #[test]
    fn writer_is_shared_by_default_and_patch_production_requires_explicit_selection() {
        let params = requested(json!({"task":"implement"}), Some("writer")).unwrap();
        assert_eq!(params["role"], "system_writer");
        assert!(crate::agent::execution::runtime::uses_main_executor("writer", &params).unwrap());
        let isolated = requested(
            json!({"task":"implement","execution":{"workspace":"isolated"}}),
            Some("writer"),
        )
        .unwrap();
        assert!(
            !crate::agent::execution::runtime::uses_main_executor("writer", &isolated).unwrap()
        );
    }
}
