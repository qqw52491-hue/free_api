//! Role defaults are resolved before dispatch. Tool choices form a ceiling;
//! workflow values are defaults that an explicit dispatch can override.
use super::*;
use crate::agent::execution::runtime::ExecutionConfig;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RoleExecutionDefaults {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub profile: String,
    #[serde(default)]
    pub execution: ExecutionConfig,
}
impl RoleExecutionDefaults {
    pub fn parse(value: &Value) -> Result<Self, String> {
        let defaults: Self =
            serde_json::from_value(value.clone()).map_err(|e| format!("执行组合参数错误：{e}"))?;
        if !defaults.profile.is_empty() && !matches!(
            defaults.profile.as_str(),
            "task" | "worker" | "writer" | "reviewer"
        ) {
            return Err("默认执行类型仅允许 task/worker/writer/reviewer".into());
        }
        ExecutionConfig::parse(&json!({"execution":defaults.execution}), if defaults.profile.is_empty() { "task" } else { &defaults.profile })?;
        Ok(defaults)
    }
}
pub(super) fn normalize(params: &Value) -> Result<Option<String>, String> {
    let Some(value) = params.get("execution_defaults") else {
        return Ok(None);
    };
    if value.as_str() == Some("") {
        return Ok(Some(String::new()));
    }
    let value = if let Some(raw) = value.as_str() {
        serde_json::from_str::<Value>(raw).map_err(|e| format!("执行组合不是有效 JSON：{e}"))?
    } else {
        value.clone()
    };
    if value.to_string().chars().count() > 16384 {
        return Err("执行组合超过 16384 字符；未保存。".into());
    }
    let defaults = RoleExecutionDefaults::parse(&value)?;
    if defaults.execution.workspace.as_deref() == Some("isolated") {
        return Err("隔离 Writer 已停止新建；请先将角色工作区改为共享再保存。".into());
    }
    let allowed = params["allowed_kinds"].as_str().unwrap_or("");
    if !defaults.profile.is_empty() && !allowed.is_empty()
        && !(RoleCard {
            allowed_kinds: allowed.into(),
            ..Default::default()
        })
        .allows_kind(&defaults.profile)
    {
        return Err("允许挂载类型未包含默认执行类型；请勾选对应类型后保存。".into());
    }
    serde_json::to_string(&defaults)
        .map(Some)
        .map_err(|e| e.to_string())
}
pub(crate) fn merge_execution_defaults(
    db: &DbState,
    params: &Value,
    fixed_profile: Option<&str>,
) -> Result<Value, String> {
    let mut merged = params.clone();
    let Some(role) = params["role"].as_str() else {
        return Ok(merged);
    };
    let Some(card) = resolve_enabled_card(db, role)? else {
        return Err(format!("角色不存在或已禁用：{role}"));
    };
    if card.execution_defaults.is_empty() {
        if fixed_profile.is_none() && params.get("profile").is_none() {
            let kinds = ["task", "worker", "writer", "reviewer"]
                .into_iter()
                .filter(|kind| card.allows_kind(kind))
                .collect::<Vec<_>>();
            if kinds.len() == 1 {
                merged["profile"] = json!(kinds[0]);
            }
        }
        return Ok(merged);
    }
    let defaults = RoleExecutionDefaults::parse(
        &serde_json::from_str::<Value>(&card.execution_defaults)
            .map_err(|e| format!("角色执行组合损坏：{e}"))?,
    )?;
    let inferred = ["task", "worker", "writer", "reviewer"].into_iter().filter(|kind|card.allows_kind(kind)).collect::<Vec<_>>();
    let profile = fixed_profile
        .or_else(|| params["profile"].as_str())
        .or_else(|| (!defaults.profile.is_empty()).then_some(defaults.profile.as_str()))
        .or_else(|| (inferred.len()==1).then(||inferred[0]))
        .unwrap_or("task");
    merged["profile"] = json!(profile);
    let mut base = serde_json::to_value(&defaults.execution).map_err(|e| e.to_string())?;
    // Shared role presets have the same workflow capabilities. Preserve the
    // configured plan/delivery defaults when selecting another ordinary preset.
    if !defaults.profile.is_empty() && profile != defaults.profile && (fixed_profile.is_some() || profile=="reviewer" || defaults.profile=="reviewer" || defaults.execution.workspace.as_deref()==Some("isolated")) {
        base["workflow"] = json!({});
        base["stages"] = json!({});
    }
    // Ordinary preset changes and dispatch overrides cannot cancel a configured
    // human approval requirement. Formal read-only review keeps its own adapter.
    let role_approval = defaults.execution.workflow.plan_mode.as_deref() == Some("plan_approval")
        && matches!(profile, "task" | "worker" | "writer");
    if role_approval { base["workflow"]["plan_mode"] = json!("plan_approval"); }
    if let Some(explicit) = params.get("execution") {
        let object = explicit.as_object().ok_or("execution 类型错误：期望对象")?;
        for (key, value) in object {
            if key == "workflow" || key == "stages" {
                let workflow = value
                    .as_object()
                    .ok_or("execution.workflow 类型错误：期望对象")?;
                for (field, value) in workflow {
                    if role_approval && key == "workflow" && field == "plan_mode"
                        && value.as_str().is_some_and(|mode| mode != "plan_approval") {
                        return Err("角色配置要求人工批准，派发方案模式与其冲突；请保留 plan_approval，或由用户修改角色流程。未派发。".into());
                    }
                    // Null means no override at this layer, just like omission.
                    // Keep unknown fields for the typed parser to reject.
                    let nullable = if key == "workflow" {
                        matches!(field.as_str(), "plan_mode" | "completion")
                    } else {
                        matches!(field.as_str(), "plan" | "plan_review" | "stage_review" | "final_review")
                    };
                    if nullable && value.is_null() { continue; }
                    base[key][field] = value.clone();
                }
            } else {
                if matches!(key.as_str(), "workspace" | "allowed_tools") && value.is_null() { continue; }
                base[key] = value.clone();
            }
        }
    }
    let mut effective = ExecutionConfig::parse(&json!({"execution":base}), profile)?;
    effective.restrict_tools(&defaults.execution);
    merged["execution"] = serde_json::to_value(effective).map_err(|e| e.to_string())?;
    Ok(merged)
}
#[cfg(test)]
mod tests;
