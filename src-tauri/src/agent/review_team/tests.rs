use super::*;
pub(crate) struct TestDb {
    pub root: std::path::PathBuf,
    pub db: DbState,
}
impl TestDb {
    pub fn new() -> Self {
        let root = std::env::temp_dir().join(format!("review-team-db-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&root.join("test.db")).unwrap(),
        )));
        Self { root, db }
    }
}
impl Drop for TestDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
use serde_json::json;

pub(crate) fn seed_cards(db: &DbState, count: usize) -> Vec<RoleCard> {
    db.0.lock()
        .unwrap()
        .execute("DELETE FROM agent_role_cards", [])
        .unwrap();
    (0..count)
        .map(|i| {
            crate::agent::tools::role_manager::upsert_card(db,&json!({
        "name":format!("review_{i}"),"label":format!("独立专家{i}"),"enabled":true,
        "model_id":"mock","allowed_kinds":"reviewer","persona_prompt":format!("REVIEW_MEMBER_{i}"),
        "scope_hints":if i==1 {"frontend"}else{"backend"}
    })).unwrap()
        })
        .collect()
}

#[tokio::test]
async fn review_team_selection_caps_three_preserves_primary_and_respects_exclusions() {
    let mock = TestDb::new();
    let cards = seed_cards(&mock.db, 5);
    let context = SandwichContext::new("sys".into(), "goal".into());
    let single = review_team(&mock.db, &context, &[], &[], false, Some(cards[4].clone()));
    assert_eq!(
        single.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        vec!["review_4"]
    );
    let selected = review_team(
        &mock.db,
        &context,
        &["src/a.vue".into()],
        &["review_1".into()],
        true,
        Some(cards[4].clone()),
    );
    assert_eq!(selected.len(), 3);
    assert_eq!(selected[0].name, "review_4");
    assert!(!selected.iter().any(|c| c.name == "review_1"));
    seed_cards(&mock.db, 1);
    assert_eq!(
        review_team(&mock.db, &context, &[], &[], true, None).len(),
        1
    );
    seed_cards(&mock.db, 0);
    assert!(review_team(&mock.db, &context, &[], &[], true, None).is_empty());
}

#[tokio::test]
async fn review_policy_fingerprint_tracks_people_and_role_changes_without_changing_defaults() {
    let mock = TestDb::new();
    seed_cards(&mock.db, 3);
    let mut config = crate::agent::router::ModelRoutingConfig::default();
    assert_eq!(policy_fingerprint(&mock.db, &config), "");
    config.stage_review_committee = true;
    let before = policy_fingerprint(&mock.db, &config);
    assert!(!before.is_empty());
    assert_eq!(before, policy_fingerprint(&mock.db, &config));
    config.disabled_role_cards.push("review_1".into());
    assert_ne!(before, policy_fingerprint(&mock.db, &config));
    config.disabled_role_cards.clear();
    crate::agent::tools::role_manager::upsert_card(
        &mock.db,
        &json!({"name":"review_0","label":"changed"}),
    )
    .unwrap();
    assert_ne!(before, policy_fingerprint(&mock.db, &config));
}
