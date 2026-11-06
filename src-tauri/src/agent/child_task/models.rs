//! Explicit dispatch selection, independent of task role, complexity and retries.
use crate::{agent::router::ModelRoutingConfig, db::DbState};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Selection {
    pub model_id: String,
    pub source: String,
}

pub(crate) fn requested(params: &Value) -> Result<Option<&str>, String> {
    match params.get("model_id") {
        None => Ok(None),
        Some(Value::String(id)) if !id.trim().is_empty() && id == id.trim() => Ok(Some(id)),
        _ => Err("model_id 必须是已配置模型的非空精确 ID；本次未派发。".into()),
    }
}

pub(crate) fn select(
    params: &Value,
    parent: &str,
    locked: Option<(&str, &str)>,
) -> Result<Selection, String> {
    let requested = requested(params)?;
    let (id, source) = if let Some((id, source)) = locked.filter(|(id, _)| !id.is_empty()) {
        if requested.is_some_and(|r| r != id) {
            return Err(format!(
                "model_id 与人工模型配置 {id} 冲突（{source}）；未派发，请沿用该配置或由人工修改。"
            ));
        }
        (id, source)
    } else if let Some(id) = requested {
        (id, "dispatch_override")
    } else {
        (parent, "parent_execution")
    };
    if id.is_empty() {
        return Err("未找到可继承的父执行模型；请明确指定 model_id。".into());
    }
    Ok(Selection {
        model_id: id.into(),
        source: source.into(),
    })
}

pub(crate) fn validate(db: &DbState, selection: &Selection) -> Result<(), String> {
    let enabled: bool =
        db.0.lock()
            .map_err(|e| e.to_string())?
            .query_row(
                "SELECT enabled FROM models WHERE id=?1",
                [&selection.model_id],
                |r| r.get(0),
            )
            .map_err(|e| format!("派发模型不存在或无法读取；未启动：{e}"))?;
    if !enabled {
        return Err(format!("模型 {} 已停用；未启动。", selection.model_id));
    }
    crate::llm::ModelConfig::from_db_state(db, &selection.model_id)
        .map(|_| ())
        .map_err(|e| format!("派发模型 {} 配置不可用；未启动：{e}", selection.model_id))
}

/// IDs and display metadata only: no provider credentials enter task context.
pub(crate) fn catalog(db: &DbState, offset: u64) -> Result<Value, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt=conn.prepare("SELECT m.id,m.name,m.display_name,p.name FROM models m JOIN platforms p ON p.id=m.platform_id WHERE m.enabled=1 ORDER BY m.id LIMIT 101 OFFSET ?1").map_err(|e|e.to_string())?;
    let mut rows=stmt.query_map([offset.min(i64::MAX as u64) as i64],|r|Ok(serde_json::json!({"model_id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"display_name":r.get::<_,String>(2)?,"platform":r.get::<_,String>(3)?}))).map_err(|e|e.to_string())?.collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
    let has_more = rows.len() > 100;
    rows.truncate(100);
    Ok(
        serde_json::json!({"models":rows,"offset":offset,"has_more":has_more,"next_offset":offset.saturating_add(rows.len() as u64)}),
    )
}

pub(crate) fn pin(routing: &mut ModelRoutingConfig, model: &str) {
    crate::agent::execution::runtime::bind_primary_model(routing, model);
    // A saved attempt never silently changes models on images or recovery.
    routing.vision = model.into();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn inheritance_override_and_human_configuration_have_one_priority() {
        assert_eq!(
            select(&json!({}), "actual-flash", None).unwrap(),
            Selection {
                model_id: "actual-flash".into(),
                source: "parent_execution".into()
            }
        );
        assert_eq!(
            select(&json!({"model_id":"pro"}), "flash", None)
                .unwrap()
                .model_id,
            "pro"
        );
        assert!(select(
            &json!({"model_id":"pro"}),
            "flash",
            Some(("fixed", "role_card"))
        )
        .is_err());
        assert_eq!(
            select(&json!({}), "flash", Some(("fixed", "role_slot")))
                .unwrap()
                .source,
            "role_slot"
        );
        for p in [
            json!({"model_id":null}),
            json!({"model_id":4}),
            json!({"model_id":" "}),
        ] {
            assert!(select(&p, "flash", None).is_err());
        }
    }
    #[test]
    fn unknown_model_fails_before_registration_and_checkpoint_keeps_actual_model() {
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&std::path::PathBuf::from(":memory:")).unwrap(),
        )));
        assert!(validate(
            &db,
            &select(&serde_json::json!({"model_id":"missing"}), "parent", None).unwrap()
        )
        .is_err());
        let mut ctx = crate::agent::context::SandwichContext::new("system".into(), "goal".into());
        ctx.last_execution_model = Some("actual-flash".into());
        let cp = crate::agent::checkpoint::AgentCheckpoint::new("owner".into(), &ctx);

        let raw = serde_json::to_value(&cp).unwrap();
        let mut resumed =
            crate::agent::context::SandwichContext::new("system".into(), "goal".into());
        cp.data.restore_into(&mut resumed);
        assert_eq!(
            resumed.last_execution_model.as_deref(),
            Some("actual-flash")
        );
        let restored: crate::agent::checkpoint::AgentCheckpoint =
            serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(
            restored.data.last_execution_model.as_deref(),
            Some("actual-flash")
        );
        let mut legacy = raw;
        legacy["data"]
            .as_object_mut()
            .unwrap()
            .remove("last_execution_model");
        assert!(
            serde_json::from_value::<crate::agent::checkpoint::AgentCheckpoint>(legacy)
                .unwrap()
                .data
                .last_execution_model
                .is_none()
        );
    }
    #[test]
    fn catalog_excludes_disabled_models_and_credentials_and_selection_validates_real_configuration()
    {
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&std::path::PathBuf::from(":memory:")).unwrap(),
        )));
        {
            let conn = db.0.lock().unwrap();
            conn.execute("INSERT INTO platforms (id,name,base_url,api_key,created_at) VALUES ('p','provider','http://127.0.0.1:9/v1','test-secret','now')",[]).unwrap();
            for (id, enabled) in [("flash", 1), ("pro", 1), ("disabled", 0)] {
                conn.execute("INSERT INTO models(id,platform_id,name,display_name,enabled,created_at) VALUES (?1,'p',?1,?1,?2,'now')",rusqlite::params![id,enabled]).unwrap();
            }
        }
        let list = catalog(&db, 0).unwrap();
        assert_eq!(list["models"].as_array().unwrap().len(), 2);
        assert!(!list.to_string().contains("test-secret"));
        assert_eq!(catalog(&db, 2).unwrap()["models"], serde_json::json!([]));
        assert!(validate(
            &db,
            &select(&serde_json::json!({"model_id":"pro"}), "flash", None).unwrap()
        )
        .is_ok());
        assert!(validate(
            &db,
            &select(&serde_json::json!({"model_id":"disabled"}), "flash", None).unwrap()
        )
        .is_err());
    }
    #[test]
    fn all_child_roles_inherit_actual_parent_but_explicit_slots_win() {
        let mut ctx = crate::agent::context::SandwichContext::new(
            "system".into(),
            "complex investigation".into(),
        );
        ctx.last_execution_model = Some("actual-parent".into());
        let mut routing = ModelRoutingConfig {
            pro: "pro".into(),
            flash: "flash".into(),
            vision: "vision".into(),
            ..Default::default()
        };
        for role in ["scout", "writer", "reviewer"] {
            assert_eq!(
                crate::agent::router::AgentRouter::route_for_role(&ctx, &routing, 100, role)
                    .model_id,
                "actual-parent"
            );
        }
        routing.role_models.insert("reviewer".into(), "pro".into());
        assert_eq!(
            crate::agent::router::AgentRouter::route_for_role(&ctx, &routing, 100, "reviewer")
                .model_id,
            "pro"
        );
    }
    #[test]
    fn pinned_attempt_routes_every_step_to_its_registered_model() {
        let mut routing = ModelRoutingConfig {
            pro: "pro".into(),
            flash: "flash".into(),
            vision: "vision".into(),
            ..Default::default()
        };
        pin(&mut routing, "chosen");
        assert_eq!(routing.pro, "chosen");
        assert_eq!(routing.flash, "chosen");
        assert_eq!(routing.vision, "chosen");
        assert_eq!(routing.role_models["planner"], "chosen");
        assert_eq!(routing.role_models["executor"], "chosen");
    }
}
