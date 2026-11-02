//! Durable execution identity, dynamic restrictions and per-execution state shared
//! by the main, action and review adapters. Resource grants remain host-owned.
use crate::db::DbState;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionConfig {
    /// Shared execution is the default; isolated patch production is explicit.
    #[serde(default)]
    pub workspace: Option<String>,
    /// None keeps the host profile; [] permits no business tools. Control actions
    /// and result submission remain available so a restricted task can return.
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub workflow: Workflow,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    /// Existing plan protocol, selected at dispatch time and persisted.
    #[serde(default)]
    pub plan_mode: Option<String>,
    /// Full target delivery uses the existing verification/review/receipt path.
    #[serde(default)]
    pub completion: Option<String>,
}
impl ExecutionConfig {
    pub fn parse(params: &Value, profile: &str) -> Result<Self, String> {
        if !matches!(profile,"task"|"worker"|"writer"|"reviewer"|"review"|"child_worker"|"child_writer"|"child_reviewer") {return Err("未知子任务 profile；允许 task/worker/writer/reviewer。".into());}
        let config: Self = match params.get("execution") {
            None => Self::default(),
            Some(value) => serde_json::from_value(value.clone())
                .map_err(|e| format!("execution 参数错误：{e}；未启动执行。"))?,
        };
        if config.allowed_tools.as_ref().is_some_and(|tools| {
            tools
                .iter()
                .any(|t| t.trim().is_empty() || t != &t.to_ascii_lowercase())
        }) {
            return Err("execution.allowed_tools 必须使用非空、小写的精确工具/动作名。".into());
        }
        if config
            .workflow
            .plan_mode
            .as_deref()
            .is_some_and(|m| !matches!(m, "auto" | "cross_review" | "plan_approval"))
        {
            return Err(
                "execution.workflow.plan_mode 仅允许 auto/cross_review/plan_approval。".into(),
            );
        }
        if config
            .workflow
            .completion
            .as_deref()
            .is_some_and(|m| !matches!(m, "handoff" | "delivery"))
        {
            return Err("execution.workflow.completion 仅允许 handoff/delivery。".into());
        }
        if config.workspace.as_deref().is_some_and(|mode| !matches!(mode,"shared"|"isolated")) {
            return Err("execution.workspace 仅允许 shared/isolated。".into());
        }
        if config.workspace.as_deref()==Some("isolated") && !matches!(profile,"writer"|"child_writer") {
            return Err("隔离补丁模式使用 writer 角色；未启动执行。".into());
        }
        if (matches!(profile,"reviewer"|"review"|"child_reviewer") || config.workspace.as_deref()==Some("isolated"))
            && (config.workflow.plan_mode.is_some()
                || config.workflow.completion.as_deref() == Some("delivery"))
        {
            return Err("当前审查/隔离补丁适配交回专用结果；完整方案流程请使用共享工作区的通用执行方式。".into());
        }
        Ok(config)
    }
    pub fn permits(&self, tool: &str) -> bool {
        matches!(
            tool,
            "finish"
                | "submit_plan"
                | "prepare_finish"
                | "request_retriage"
                | "ask_human"
                | "read_message"
                | "read_agent_result"
                | "team_status"
                | "wait_message"
        ) || self
            .allowed_tools
            .as_ref()
            .is_none_or(|tools| tools.iter().any(|t| permits_name(t, tool)))
    }
}
fn canonical_name(name: &str) -> &str {
    match name {
        "local_file.read" => "read_file",
        "local_file.list" => "list_dir",
        "local_file.write" => "write_file",
        "local_file.edit" => "edit_file",
        "local_shell.execute" => "run_command",
        "child_task.send" => "send_message",
        "child_task.ask" => "ask_agent",
        "child_task.wait" => "wait_message",
        _ => name,
    }
}
fn permits_name(allowed: &str, requested: &str) -> bool {
    canonical_name(allowed) == canonical_name(requested)
        || (allowed == "local_file"
            && matches!(
                canonical_name(requested),
                "read_file" | "list_dir" | "write_file" | "edit_file"
            ))
        || (allowed == "local_shell" && canonical_name(requested) == "run_command")
}
pub fn config(db: &DbState, id: &str) -> Result<ExecutionConfig, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let exists:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='agent_child_runs')",[],|r|r.get(0)).map_err(|e|e.to_string())?;
    if !exists {
        return Ok(ExecutionConfig::default());
    }
    let raw: Option<String> = conn
        .query_row(
            "SELECT execution_config_json FROM agent_child_runs WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    raw.map(|s| serde_json::from_str(&s).map_err(|e| format!("已登记执行配置损坏：{e}")))
        .transpose()
        .map(|c| c.unwrap_or_default())
}
pub fn authorize(db: &DbState, id: &str, tool: &str) -> Result<(), String> {
    authorize_call(db, id, tool, "", &Value::Null)
}
pub fn authorize_call(
    db: &DbState,
    id: &str,
    tool: &str,
    action: &str,
    params: &Value,
) -> Result<(), String> {
    let config = config(db, id)?;
    let effective = if action.is_empty() || action.eq_ignore_ascii_case(tool) {
        params["action"].as_str().unwrap_or(match tool {
            "local_file" => "read",
            "local_shell" => "execute",
            "artifact_manager" => "create_document",
            _ => "",
        })
    } else {
        action
    };
    if tool.eq_ignore_ascii_case("child_task")
        && matches!(
            effective.to_ascii_lowercase().as_str(),
            "status" | "list" | "inbox" | "receipt" | "wait" | "result" | "read_agent_result"
        )
    {
        return Ok(());
    }
    let name = format!(
        "{}.{}",
        tool.to_ascii_lowercase(),
        effective.to_ascii_lowercase()
    );
    if config.permits(&tool.to_ascii_lowercase())
        || config
            .allowed_tools
            .as_ref()
            .is_some_and(|tools| tools.iter().any(|t| permits_name(t, &name)))
    {
        Ok(())
    } else {
        Err(format!("执行配置未允许工具/动作 {name}；本次未执行。"))
    }
}

/// Role names select knowledge and contracts; only explicit specialised services
/// use a different adapter. This is also used by the public dispatch entrance.
pub fn uses_main_executor(profile: &str, params: &Value) -> Result<bool,String> {
    let config=ExecutionConfig::parse(params,profile)?;
    Ok(!matches!(profile,"reviewer"|"review"|"child_reviewer") && config.workspace.as_deref()!=Some("isolated"))
}

pub fn registered_test_command(db:&DbState,id:&str) -> Result<Option<String>,String> {
    let conn=db.0.lock().map_err(|e|e.to_string())?;
    let raw:Option<String>=conn.query_row("SELECT execution_checkpoint_json FROM agent_child_runs WHERE id=?1",[id],|r|r.get(0)).optional().map_err(|e|e.to_string())?.flatten();
    let Some(raw)=raw else {return Ok(None)};
    let snapshot:Value=serde_json::from_str(&raw).map_err(|e|format!("派发输入快照损坏：{e}"))?;
    Ok(snapshot["state"]["dispatch_input"]["params"]["test_command"].as_str().map(str::to_owned).filter(|s|!s.trim().is_empty()))
}

pub fn bind_registered_requirements(db:&DbState,id:&str,context:&mut crate::agent::context::SandwichContext) -> Result<(),String> {
    if let Some(command)=registered_test_command(db,id)? {
        let contract=serde_json::from_value(json!({"scope_statement":"本次派发明确指定的验证","obligations":[{"id":"dispatch-test","question":"执行派发指定的验证并交回实际结果","status":"open","verify_command":command,"closure_form":"command"}]})).map_err(|e|e.to_string())?;
        crate::agent::review_contract::apply_review_contract_update(&mut context.review_contract,contract)?;
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: u32,
    pub execution_id: String,
    pub profile: String,
    pub phase: String,
    pub state: Value,
    pub version: u64,
}
impl Snapshot {
    pub fn new(id: &str, profile: &str, state: Value) -> Self {
        Self {
            schema_version: 1,
            execution_id: id.into(),
            profile: profile.into(),
            phase: "ready".into(),
            state,
            version: 0,
        }
    }
    pub fn load(db: &DbState, id: &str, profile: &str) -> Result<Option<Self>, String> {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let raw:Option<String>=conn.query_row("SELECT execution_checkpoint_json FROM agent_child_runs WHERE id=?1 AND execution_checkpoint_json IS NOT NULL",[id],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
        let mut value = raw
            .map(|raw| serde_json::from_str::<Self>(&raw).map_err(|e| e.to_string()))
            .transpose()?;
        if let Some(snapshot) = value.as_mut() {
            if snapshot.phase == "registered"
                && snapshot.profile == "reviewer"
                && profile == "reviewer_resume"
            {
                snapshot.profile = profile.into();
            }
        }
        if value
            .as_ref()
            .is_some_and(|s| s.schema_version != 1 || s.execution_id != id || s.profile != profile)
        {
            return Err("执行检查点版本或归属不匹配；未恢复。".into());
        }
        Ok(value)
    }
    pub fn save(&mut self, db: &DbState) -> Result<(), String> {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        self.save_on(&conn)
    }
    pub fn save_on(&mut self, conn: &rusqlite::Connection) -> Result<(), String> {
        let mut next = self.clone();
        next.version += 1;
        let raw = serde_json::to_string(&next).map_err(|e| e.to_string())?;
        let changed=conn.execute("UPDATE agent_child_runs SET execution_checkpoint_json=?2,execution_version=?3 WHERE id=?1 AND execution_version=?4",params![self.execution_id,raw,next.version,self.version]).map_err(|e|e.to_string())?;
        if changed != 1 {
            return Err("执行检查点版本冲突；未覆盖，请重新加载。".into());
        }
        *self = next;
        Ok(())
    }
}

pub fn bind_primary_model(routing: &mut crate::agent::router::ModelRoutingConfig, model: &str) {
    routing.pro = model.into();
    routing.flash = model.into();
    routing
        .role_models
        .insert(crate::agent::router::roles::PLANNER.into(), model.into());
    routing
        .role_models
        .insert(crate::agent::router::roles::EXECUTOR.into(), model.into());
}
pub fn workflow_blocker(config: &ExecutionConfig, plan_approved: bool) -> Option<String> {
    (matches!(config.workflow.plan_mode.as_deref(),Some("cross_review"|"plan_approval"))&&!plan_approved)
        .then(||"当前执行配置要求先完成方案讨论或批准；尚未取得已批准的方案状态，请先 submit_plan 并完成对应流程。".into())
}

pub fn validate_output(db: &DbState, id: &str, payload: &Value) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let exists:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='agent_child_runs')",[],|r|r.get(0)).map_err(|e|e.to_string())?;
    if !exists {
        return Ok(());
    }
    let raw: Option<String> = conn
        .query_row(
            "SELECT contract_schema FROM agent_child_runs WHERE id=?1 AND kind='task'",
            [id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    if let Some(raw) = raw {
        let schema: crate::agent::protocol::ContractSchema =
            serde_json::from_str(&raw).map_err(|e| format!("登记的交付契约损坏：{e}"))?;
        let mut extensions = payload.clone();
        if let Some(object) = extensions.as_object_mut() {
            for key in ["summary", "details", "artifacts"] {
                if !schema.fields.iter().any(|field| field.name == key) {
                    object.remove(key);
                }
            }
        }
        schema.validate_shape(&extensions)?;
    }
    Ok(())
}

pub fn fail_checkpoint(db: &DbState, id: &str, error: String) {
    let kind = crate::agent::types::ChildErrorKind::Other(error);
    let body =
        crate::agent::child_reviewer::child_error_result_json(id, "failed", &kind, 0, 0, 0, None);
    crate::agent::child_reviewer::finish_child_run(
        db,
        id,
        "failed",
        0,
        0,
        0,
        Some(kind),
        Some(&body),
    );
}

#[cfg(test)]
mod tests;
