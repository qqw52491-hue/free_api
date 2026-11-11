use super::*;
use serde_json::json;
#[test]
fn long_goal_and_manifest_cannot_starve_stage_evidence_or_diff() {
    let text = stage(
        &"目标".repeat(10000),
        &"待办".repeat(5000),
        &[
            "文件".repeat(10000),
            "output_id=proof-actual\nfailed test".into(),
        ],
        Some(&"关注".repeat(3000)),
        "REAL_DIFF_LAST_LINE",
    );
    assert!(text.chars().count() <= 6000);
    assert!(text.contains("output_id=proof-actual"));
    assert!(text.contains("REAL_DIFF_LAST_LINE"));
    assert!(text.contains("本节有省略"));
    for n in [0, 1, 100, 3000] {
        assert!(
            section("标题", &"字符".repeat(5000), n, false)
                .chars()
                .count()
                <= n
        );
    }
    assert!(
        stage(
            "目标",
            "待办",
            &vec!["extra".repeat(100); 1000],
            None,
            "diff"
        )
        .chars()
        .count()
            <= 6000
    );
}
#[test]
fn ordinary_material_is_not_repeated_by_role_and_specialization() {
    let p = json!({"task":"目标","custom_inputs":{"key":"UNIQUE"},"specialized_context":"independent focus","context":"caller context","test_command":"cargo test target"});
    let goal = ordinary(
        &p,
        "ROLE CONTRACT",
        std::path::Path::new("/tmp/parent"),
        "child",
    );
    assert_eq!(goal.matches("UNIQUE").count(), 1);
    assert!(goal.contains("cargo test target"));
    assert!(goal.contains("/tmp/parent"));
    let goal = ordinary(
        &json!({"task":"目标","custom_inputs":{"key":"UNIQUE"}}),
        "",
        std::path::Path::new("/tmp"),
        "child",
    );
    assert_eq!(goal.matches("UNIQUE").count(), 1);
}
#[test]
fn review_instructions_are_native_system_materials_are_not_relabelled() {
    let messages = vec![
        json!({"role":"system","content":"SYSTEM_UNIQUE"}),
        json!({"role":"user","content":"GOAL_UNIQUE"}),
        json!({"role":"assistant","content":"OLD_REVIEW"}),
    ];
    let sys = review_system(&messages);
    let goal = review_goal(&messages);
    assert!(sys.contains("SYSTEM_UNIQUE"));
    assert!(!sys.contains("GOAL_UNIQUE"));
    assert!(!goal.contains("SYSTEM_UNIQUE"));
    assert!(goal.contains("GOAL_UNIQUE"));
    assert!(goal.contains("OLD_REVIEW"));
    assert!(sys.contains("不派发子任务"));
    assert!(goal.contains("finish"));
}

#[test]
fn visible_tools_use_runtime_aliases_without_granting_dispatch() {
    let config = crate::agent::execution::runtime::ExecutionConfig::parse(
        &json!({"execution":{"allowed_tools":["read_file"]}}),
        "task",
    )
    .unwrap();
    assert!(visibility::tool(&config, "local_file"));
    assert!(!visibility::tool(&config, "child_task"));
    assert!(!config.permits("local_file.write"));
    assert_eq!(visibility::reviewer_index(&config, 0, false), "finish");
    let index = visibility::reviewer_index(&config, 30, true);
    assert!(index.contains("read_file"));
    assert!(!index.contains("verify"));
    assert!(!index.contains("child_task"));
    let mcp = crate::agent::execution::runtime::ExecutionConfig::parse(
        &json!({"execution":{"allowed_tools":["vendor/readdoc"]}}),
        "task",
    )
    .unwrap();
    assert!(visibility::tool(&mcp, "vendor/readDoc"));
    assert!(visibility::restricted_menu(&config).contains("read_file"));
}

#[test]
fn communication_aliases_show_the_same_index_without_granting_dispatch() {
    use crate::agent::execution::runtime::ExecutionConfig;
    for (allowed, action) in [
        ("send_message", "child_task.send"),
        ("child_task.send", "child_task.send"),
        ("ask_agent", "child_task.ask"),
        ("child_task.ask", "child_task.ask"),
    ] {
        let config = ExecutionConfig::parse(
            &json!({"execution":{"allowed_tools":[allowed]}}),
            "task",
        )
        .unwrap();
        assert!(config.permits(action), "{allowed}");
        assert!(visibility::tool(&config, "child_task"), "{allowed}");
        assert!(!config.permits("child_task.dispatch"), "{allowed}");
    }
    let config = ExecutionConfig::parse(&json!({"execution":{"allowed_tools":[]}}), "task").unwrap();
    assert!(!visibility::tool(&config, "child_task"));
    assert!(!config.permits("child_task.send"));
    assert!(!config.permits("child_task.ask"));
}

#[test]
fn review_entry_keeps_project_rules_and_uses_effective_permissions() {
    use crate::agent::execution::runtime::ExecutionConfig;
    let config = ExecutionConfig::parse(
        &json!({"execution":{"allowed_tools":["local_file","child_task","send_message"]}}),
        "reviewer",
    )
    .unwrap();
    let messages = vec![json!({"role":"system","content":"original review contract"})];
    let system = review_entry(
        &messages,
        "AGENTS_SENTINEL\nHUB_SENTINEL",
        "CUSTOM_SENTINEL",
        &config,
        30,
        false,
    );
    for marker in [
        "AGENTS_SENTINEL",
        "HUB_SENTINEL",
        "CUSTOM_SENTINEL",
        "original review contract",
    ] {
        assert!(system.contains(marker));
    }
    let menu = visibility::reviewer_menu(&config, 30, false);
    assert!(!menu.contains("child_task"));
    assert!(!menu.contains("send_message"));
    assert!(menu.contains("read_file"));
    assert!(!menu.contains("write_file"));
    assert!(menu.contains("{\"parallel\":"));
    let index = visibility::reviewer_index(&config, 30, false);
    assert!(menu.contains(&index));
    let no_budget = visibility::reviewer_menu(&config, 0, false);
    assert!(no_budget.contains("】finish\n"));
    assert!(!no_budget.contains("{\"parallel\":"));
}
#[test]
fn unused_stage_capacity_is_lent_without_losing_required_sections() {
    let diff = "D".repeat(4000);
    let output = stage("goal", "todo", &[], None, &diff);
    assert!(output.contains(&diff));
    assert!(output.chars().count() <= 6000);
    let output = stage(
        &"goal".repeat(10000),
        "TODO_REQUIRED",
        &["RECEIPT_REQUIRED".into()],
        None,
        "DIFF_REQUIRED",
    );
    for marker in ["TODO_REQUIRED", "RECEIPT_REQUIRED", "DIFF_REQUIRED"] {
        assert!(output.contains(marker));
    }
    assert!(output.chars().count() <= 6000);
}
#[test]
fn ordinary_soft_budget_preserves_inputs_and_exposes_original_focus_handle() {
    let p = json!({"task":"GOAL","context":"C".repeat(6000),"custom_inputs":{"constraint":"REQUIRED"},"test_command":"TEST_REQUIRED","specialized_context":"F".repeat(40000)});
    let output = ordinary(
        &p,
        "ROLE_REQUIRED",
        std::path::Path::new("/tmp"),
        "real-child-id",
    );
    assert!(output.chars().count() <= 16000);
    for marker in [
        "GOAL",
        "REQUIRED",
        "ROLE_REQUIRED",
        "TEST_REQUIRED",
        "real-child-id",
        "params.specialized_context",
    ] {
        assert!(output.contains(marker));
    }
    let p = json!({"task":"GOAL","context":"C".repeat(18000),"custom_inputs":{"constraint":"REQUIRED"},"specialized_context":"F".repeat(40000)});
    let output = ordinary(&p, "ROLE_REQUIRED", std::path::Path::new("/tmp"), "child");
    assert!(output.contains(&"C".repeat(18000)));
    assert!(output.contains("REQUIRED"));
    assert!(output.contains("软预算披露"));
}
