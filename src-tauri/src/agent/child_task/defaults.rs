//! User-selected defaults for new ordinary child dispatches. No tool/model grants.
use crate::agent::{execution::runtime::ExecutionConfig, stage_settings::StageSettings};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildWorkflowDefaults {
    #[serde(default)]
    pub plan_mode: Option<String>,
    #[serde(default)]
    pub completion: Option<String>,
    #[serde(default)]
    pub stages: StageSettings,
}
impl ChildWorkflowDefaults {
    pub fn validate(&self) -> Result<(), String> {
        if self.plan_mode.as_deref() == Some("cross_review") && self.stages.plan == Some(false) {
            return Err("先讨论并审查方案需要正式方案；请开启正式方案或调整方案模式。".into());
        }
        ExecutionConfig::parse(&json!({"execution":{"workflow":{"plan_mode":self.plan_mode,"completion":self.completion},"stages":self.stages}}), "task").map(|_| ())
    }
    pub fn load(path: &Path) -> Result<Self, String> {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("读取子任务默认流程失败：{e}")),
        };
        let config: Self =
            serde_json::from_str(&raw).map_err(|e| format!("子任务默认流程格式错误：{e}"))?;
        config.validate()?;
        Ok(config)
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(
            path,
            serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("保存子任务默认流程失败：{e}"))
    }
    /// Role and per-dispatch values are already merged; fill only absent values.
    pub(crate) fn apply_after_entry_fallback(
        &self,
        params: &Value,
        implicit_handoff: bool,
    ) -> Result<Value, String> {
        let mut candidate = params.clone();
        if implicit_handoff && self.completion.is_some() {
            candidate["execution"]["workflow"]["completion"] = Value::Null;
        }
        self.apply(&candidate)
    }

    pub(crate) fn apply(&self, params: &Value) -> Result<Value, String> {
        if self == &Self::default() {
            return Ok(params.clone());
        }
        let profile = params["profile"].as_str().unwrap_or("task");
        if !crate::agent::execution::runtime::uses_main_executor(profile, params)? {
            return Ok(params.clone());
        }
        let mut config = ExecutionConfig::parse(params, profile)?;
        if self.plan_mode.as_deref() == Some("plan_approval")
            && config
                .workflow
                .plan_mode
                .as_deref()
                .is_some_and(|mode| mode != "plan_approval")
        {
            return Err("子任务默认流程要求人工批准，角色或派发方案模式与其冲突；请保留 plan_approval，或由用户修改默认流程。未派发。".into());
        }
        config.workflow.plan_mode = config.workflow.plan_mode.or(self.plan_mode.clone());
        config.workflow.completion = config.workflow.completion.or(self.completion.clone());
        config.stages = StageSettings {
            plan: config.stages.plan.or(self.stages.plan),
            plan_review: config.stages.plan_review.or(self.stages.plan_review),
            stage_review: config.stages.stage_review.or(self.stages.stage_review),
            final_review: config.stages.final_review.or(self.stages.final_review),
        };
        config
            .stages
            .validate(config.workflow.plan_mode.as_deref())?;
        if config.workflow.plan_mode.as_deref() == Some("cross_review")
            && config.stages.plan == Some(false)
        {
            return Err("有效子任务流程同时要求方案讨论与关闭正式方案；请调整角色、派发或默认流程。未派发。".into());
        }
        let mut result = params.clone();
        result["execution"] = serde_json::to_value(config).map_err(|e| e.to_string())?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_roundtrip_and_invalid_save_preserves_previous_configuration() {
        let root = std::env::temp_dir().join(format!("child-defaults-{}", uuid::Uuid::new_v4()));
        let path = root.join("defaults.json");
        assert_eq!(
            ChildWorkflowDefaults::load(&path).unwrap(),
            ChildWorkflowDefaults::default()
        );
        let settings = ChildWorkflowDefaults {
            plan_mode: Some("plan_approval".into()),
            ..Default::default()
        };
        settings.save(&path).unwrap();
        assert_eq!(ChildWorkflowDefaults::load(&path).unwrap(), settings);
        let mut invalid = settings.clone();
        invalid.stages.plan = Some(false);
        assert!(invalid.save(&path).is_err());
        assert_eq!(ChildWorkflowDefaults::load(&path).unwrap(), settings);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn new_ordinary_dispatches_use_defaults_but_explicit_values_and_permissions_survive() {
        let settings = ChildWorkflowDefaults {
            plan_mode: Some("cross_review".into()),
            completion: Some("delivery".into()),
            stages: StageSettings {
                stage_review: Some(true),
                ..Default::default()
            },
        };
        let params = json!({"task":"inspect","execution":{"allowed_tools":["read_file"],"workflow":{"completion":"handoff"}}});
        let result = settings.apply(&params).unwrap();
        assert_eq!(
            result["execution"]["workflow"],
            json!({"plan_mode":"cross_review","completion":"handoff"})
        );
        assert_eq!(
            result["execution"]["allowed_tools"],
            params["execution"]["allowed_tools"]
        );
        assert_eq!(result["execution"]["stages"]["stage_review"], true);
        let fallback =
            json!({"profile":"worker","execution":{"workflow":{"completion":"handoff"}}});
        assert_eq!(
            settings
                .apply_after_entry_fallback(&fallback, true)
                .unwrap()["execution"]["workflow"]["completion"],
            "delivery"
        );
        assert_eq!(
            settings
                .apply_after_entry_fallback(&fallback, false)
                .unwrap()["execution"]["workflow"]["completion"],
            "handoff"
        );
        assert_eq!(
            ChildWorkflowDefaults::default()
                .apply_after_entry_fallback(&fallback, true)
                .unwrap(),
            fallback
        );
        for profile in ["task", "worker", "writer"] {
            let params = json!({"task":"work","profile":profile});
            assert_eq!(
                settings.apply(&params).unwrap()["execution"]["workflow"]["completion"],
                "delivery"
            );
            assert_eq!(
                ChildWorkflowDefaults::default().apply(&params).unwrap(),
                params
            );
        }
        let reviewer = json!({"profile":"reviewer"});
        assert_eq!(settings.apply(&reviewer).unwrap(), reviewer);
        let isolated = json!({"profile":"writer","execution":{"workspace":"isolated"}});
        assert_eq!(settings.apply(&isolated).unwrap(), isolated);
    }
    #[test]
    fn role_merge_precedes_defaults_and_human_approval_cannot_be_downgraded() {
        let db = crate::db::DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&std::path::PathBuf::from(":memory:")).unwrap(),
        )));
        crate::agent::tools::role_manager::upsert_card(&db,&json!({"name":"special","allowed_kinds":"task","execution_defaults":{"profile":"task","execution":{"workflow":{"completion":"handoff"},"allowed_tools":["read_file"],"stages":{"final_review":false}}}})).unwrap();
        let params = crate::agent::tools::role_manager::merge_execution_defaults(
            &db,
            &json!({"role":"special","task":"inspect"}),
            None,
        )
        .unwrap();
        let defaults = ChildWorkflowDefaults {
            completion: Some("delivery".into()),
            plan_mode: Some("plan_approval".into()),
            ..Default::default()
        };
        let applied = defaults.apply(&params).unwrap();
        assert_eq!(applied["execution"]["workflow"]["completion"], "handoff");
        assert_eq!(
            applied["execution"]["workflow"]["plan_mode"],
            "plan_approval"
        );
        assert_eq!(applied["execution"]["allowed_tools"], json!(["read_file"]));
        assert_eq!(applied["execution"]["stages"]["final_review"], false);
        assert!(defaults
            .apply(&json!({"execution":{"workflow":{"plan_mode":"auto"}}}))
            .is_err());
        assert!(defaults
            .apply(&json!({"execution":{"stages":{"plan":false}}}))
            .is_err());
    }
    #[test]
    fn registered_configuration_survives_default_changes() {
        let root = std::env::temp_dir().join(format!(
            "child-default-registration-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let db = crate::db::DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&root.join("test.db")).unwrap(),
        )));
        let path = root.join("defaults.json");
        let settings = ChildWorkflowDefaults {
            plan_mode: Some("plan_approval".into()),
            completion: Some("delivery".into()),
            ..Default::default()
        };
        settings.save(&path).unwrap();
        let params = ChildWorkflowDefaults::load(&path)
            .unwrap()
            .apply(&json!({"task":"inspect"}))
            .unwrap();
        let id = crate::agent::child_reviewer::begin_child_run_extended(
            &db,
            "parent",
            "owner",
            &root,
            "model",
            0,
            "task",
            None,
            None,
            None,
            &crate::agent::child_reviewer::ChildRunIdentity {
                execution_config: Some(params["execution"].to_string()),
                dispatch_input: Some(json!({"params":params}).to_string()),
                ..crate::agent::child_reviewer::ChildRunIdentity::fresh("task")
            },
        )
        .unwrap();
        ChildWorkflowDefaults::default().save(&path).unwrap();
        let config = crate::agent::execution::runtime::config(&db, &id).unwrap();
        assert_eq!(config.workflow.plan_mode.as_deref(), Some("plan_approval"));
        let snapshot = crate::agent::execution::runtime::Snapshot::load(&db, &id, "task")
            .unwrap()
            .unwrap();
        assert_eq!(
            snapshot.state["dispatch_input"]["params"]["execution"]["workflow"]["plan_mode"],
            "plan_approval"
        );
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }
}
