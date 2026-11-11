//! Fixed execution stages, independent of role and model executor.
//! None follows the existing task strategy; false never schedules a new stage.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StageSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_review: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_review: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_review: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{
        checkpoint::{load_checkpoint, save_checkpoint, AgentCheckpoint, PlanExecutionMode},
        context::SandwichContext,
    };
    use serde_json::json;
    #[test]
    fn defaults_and_conflicts_are_explicit_and_legacy_json_loads() {
        let auto: StageSettings = serde_json::from_str("{}").unwrap();
        assert!(auto.is_empty());
        assert_eq!(auto.for_child(true, Some("auto")).plan, Some(false));
        assert!(auto.for_child(false, None).is_empty());
        assert!(auto.for_child(true, Some("plan_approval")).is_empty());
        let off: StageSettings =
            serde_json::from_value(json!({"plan":false,"plan_review":false})).unwrap();
        assert!(off.validate(Some("auto")).is_ok());
        assert!(off.validate(Some("plan_approval")).is_err());
        assert!(off.validate_for_new_task(Some("auto")).is_ok());
        assert!(off.validate_for_new_task(Some("cross_review")).is_err());
        let legacy_spelling = PlanExecutionMode::from_str_or_auto_pilot(Some("cross-review"), true);
        assert!(off.validate_for_new_task(Some(legacy_spelling.as_str())).is_err());
        assert!(off.validate(Some("cross_review")).is_ok(), "legacy checkpoint can still resume");
        assert!(serde_json::from_value::<StageSettings>(json!({"final_review":"off"})).is_err());
    }
    #[test]
    fn disabled_plan_survives_triage_and_checkpoint_round_trip_but_not_human_approval() {
        let db = crate::db::DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&std::path::PathBuf::from(":memory:")).unwrap(),
        )));
        let c = SandwichContext::new("sys".into(), "goal".into());
        let mut cp = AgentCheckpoint::new("owner".into(), &c);
        cp.stages = StageSettings {
            plan: Some(false),
            stage_review: Some(false),
            ..Default::default()
        };
        cp.triage_verdict = Some(crate::agent::triage::TriageVerdict::fallback("offline"));
        let inst=serde_json::from_value(json!({"tool":"local_file","action":"write","params":{"path":"src/a.rs","content":"x"}})).unwrap();
        assert!(!crate::agent::plan_gate::pre_write_plan_required(
            &cp,
            &c,
            &inst,
            "owner",
            std::path::Path::new("/tmp")
        ));
        assert!(!cp.plan_gate_projection().plan_required);
        assert!(
            crate::agent::stage_review::should_fire(&cp, &db, &c, &inst, &Default::default())
                .is_none()
        );
        save_checkpoint(&db, &mut cp).unwrap();
        let mut restored = load_checkpoint(&db, "owner").unwrap().unwrap();
        assert_eq!(restored.stages, cp.stages);
        restored.plan_mode = PlanExecutionMode::PlanApproval;
        assert!(crate::agent::plan_gate::pre_write_plan_required(
            &restored,
            &c,
            &inst,
            "owner",
            std::path::Path::new("/tmp")
        ));
    }
    #[test]
    fn plan_without_review_records_plan_without_inventing_a_review_receipt() {
        let c = SandwichContext::new("sys".into(), "goal".into());
        let mut cp = AgentCheckpoint::new("owner".into(), &c);
        cp.stages.plan = Some(true);
        cp.stages.plan_review = Some(false);
        let plan = serde_json::from_value(
            json!({"plan_summary":"implement a small change","target_files":["src/a.rs"]}),
        )
        .unwrap();
        let result =
            crate::agent::plan_submit::record_plan_without_review(&mut cp, &plan, 3).unwrap();
        assert!(result.success);
        assert!(cp.plan_phase.is_approved());
        assert!(cp.submitted_plan.is_some());
        assert_eq!(cp.plan_review_expected_count, 0);
        assert!(!cp.plan_reviewer_consumed);
        assert!(cp.plan_review_batch_id.is_none());
    }
}

impl StageSettings {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    pub fn validate(&self, plan_mode: Option<&str>) -> Result<(), String> {
        if self.plan == Some(false)
            && (self.plan_review == Some(true) || plan_mode == Some("plan_approval"))
        {
            return Err("正式方案已关闭，不能同时开启方案审查或方案人工批准；请调整配置。".into());
        }
        Ok(())
    }

    pub(crate) fn validate_for_new_task(&self, plan_mode: Option<&str>) -> Result<(), String> {
        self.validate(plan_mode)?;
        if plan_mode == Some("cross_review") && self.plan == Some(false) {
            return Err("先讨论并审查方案需要正式方案；请开启正式方案或调整方案模式。".into());
        }
        Ok(())
    }

    pub fn for_child(&self, handoff: bool, plan_mode: Option<&str>) -> Self {
        let direct = handoff && !matches!(plan_mode, Some("cross_review" | "plan_approval"));
        let default = if direct { Some(false) } else { None };
        Self {
            plan: self.plan.or(if self.plan_review == Some(true) {
                Some(true)
            } else {
                default
            }),
            plan_review: self.plan_review.or(default),
            stage_review: self.stage_review.or(default),
            final_review: self.final_review.or(default),
        }
    }

    fn values_text(&self) -> String {
        let label = |value| match value {
            Some(true) => "开启", Some(false) => "关闭", None => "自动",
        };
        format!("正式方案={}；方案审查={}；阶段审查={}；最终审查={}",
            label(self.plan), label(self.plan_review), label(self.stage_review), label(self.final_review))
    }

    pub fn reviewer_prompt(&self) -> String {
        format!("【当前只读 Reviewer 子任务的内部配置】{}。\n以上只控制当前审查执行器，不是被审查父任务的设置。关闭内部方案/审查用于避免递归派审；你仍须完成本次已派发的方案、阶段或最终审查。父任务的策略见被审查材料中的父任务流程配置快照，不得用这些内部开关推断父任务已免审。", self.values_text())
    }

    pub fn prompt(&self) -> String {
        format!("【宿主固定环节配置】{}。关闭表示不自动触发该环节；仍可自行思考、核对结果。人工批准模式下用 submit_plan 提交正式方案；宿主按配置派审、收齐结果后自动呈报人工批准，Agent 审查不替代人工批准。权限、明确验证、人工批准和已有有效阻塞不因此撤销。", self.values_text())
    }
}
