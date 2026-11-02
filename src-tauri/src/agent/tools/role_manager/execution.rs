//! Role defaults are resolved before dispatch. Tool choices form a ceiling;
//! workflow values are defaults that an explicit dispatch can override.
use super::*;
use crate::agent::execution::runtime::ExecutionConfig;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RoleExecutionDefaults {
    pub profile: String,
    #[serde(default)]
    pub execution: ExecutionConfig,
}
impl RoleExecutionDefaults {
    pub fn parse(value: &Value) -> Result<Self, String> {
        let defaults: Self =
            serde_json::from_value(value.clone()).map_err(|e| format!("执行组合参数错误：{e}"))?;
        if !matches!(
            defaults.profile.as_str(),
            "task" | "worker" | "writer" | "reviewer"
        ) {
            return Err("默认执行类型仅允许 task/worker/writer/reviewer".into());
        }
        ExecutionConfig::parse(&json!({"execution":defaults.execution}), &defaults.profile)?;
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
    let allowed = params["allowed_kinds"].as_str().unwrap_or("");
    if !allowed.is_empty()
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
    let profile = fixed_profile
        .or_else(|| params["profile"].as_str())
        .unwrap_or(&defaults.profile);
    merged["profile"] = json!(profile);
    let mut base = serde_json::to_value(&defaults.execution).map_err(|e| e.to_string())?;
    if profile != defaults.profile {
        base["workflow"] = json!({});
    }
    if let Some(explicit) = params.get("execution") {
        let object = explicit.as_object().ok_or("execution 类型错误：期望对象")?;
        for (key, value) in object {
            if key == "workflow" {
                let workflow = value
                    .as_object()
                    .ok_or("execution.workflow 类型错误：期望对象")?;
                for (field, value) in workflow {
                    base["workflow"][field] = value.clone();
                }
            } else {
                base[key] = value.clone();
            }
        }
    }
    let mut effective = ExecutionConfig::parse(&json!({"execution":base}), profile)?;
    if let Some(ceiling) = defaults.execution.allowed_tools.as_ref() {
        effective.allowed_tools = Some(match effective.allowed_tools {
            None => ceiling.clone(),
            Some(tools) => tools
                .into_iter()
                .filter(|t| defaults.execution.permits(t))
                .collect(),
        });
    }
    merged["execution"] = serde_json::to_value(effective).map_err(|e| e.to_string())?;
    Ok(merged)
}
#[cfg(test)]
mod tests;
