use super::*;
use crate::agent::context::SandwichContext;
use crate::agent::review_team::tests::TestDb;

#[test]
fn repeated_reads_and_a_formerly_missing_file_reach_the_native_dispatcher() {
    let fixture = TestDb::new();
    let workspace = fixture.root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let registry = PluginRegistry::new();
    let session = "flexible-native-read";
    let mut context = SandwichContext::new("sys".into(), "核对文件".into());
    let read: AgentInstruction = serde_json::from_value(json!({
        "tool":"local_file", "params":{"action":"read","path":"report.txt"}
    })).unwrap();
    let first = run_agent_step(&fixture.db, session, &read, &registry, Some(&workspace));
    assert!(!first.success);
    context.record_failed_attempt(1, read.get_tool(), read.get_action(), &read.get_params(),
        "ENOENT: no such file or directory".into(), Some("old-output".into()));
    // 条件变化而参数不变：历史错误不能代替本次现场判断。
    std::fs::write(workspace.join("report.txt"), "CURRENT-CONTENT").unwrap();
    for _ in 0..5 {
        record_dispatch_advice(&mut context, &read, &workspace);
        let result = run_agent_step(&fixture.db, session, &read, &registry, Some(&workspace));
        assert!(result.success, "{}", result.stderr);
        assert!(result.stdout.contains("CURRENT-CONTENT"));
    }
    let history = serde_json::to_string(&context.turns_history).unwrap();
    assert!(history.contains("历史同参失败提示"));
    assert_eq!(history.matches("重复读取提示").count(), 1, "只提示一次，读取继续执行");
    assert!(!history.contains("如需重读请先修改文件"));
    assert!(history.contains("无需先修改文件"));

    // 当前事实边界仍成立：提示不能把越界读取或参数缺失变成成功。
    let outside = fixture.root.join("outside.txt");
    std::fs::write(&outside, "PRIVATE-OUTSIDE").unwrap();
    let outside_read: AgentInstruction = serde_json::from_value(json!({
        "tool":"local_file", "params":{"action":"read","path":outside}
    })).unwrap();
    record_dispatch_advice(&mut context, &outside_read, &workspace);
    let denied = run_agent_step(&fixture.db, session, &outside_read, &registry, Some(&workspace));
    assert!(!denied.success);
    assert!(!denied.stdout.contains("PRIVATE-OUTSIDE"));
    let invalid: AgentInstruction = serde_json::from_value(json!({"tool":"local_file","params":{"action":"read"}})).unwrap();
    assert!(crate::agent::tools::input_validation::instruction_rejection(&fixture.db, &invalid).is_some());
}

#[test]
fn recovered_question_history_only_advises_and_does_not_approve_a_plan() {
    let context = SandwichContext::new("sys".into(), "澄清参数".into());
    let mut cp = AgentCheckpoint::new("flexible-inquiry".into(), &context);
    cp.plan_mode = crate::agent::checkpoint::PlanExecutionMode::PlanApproval;
    for i in 0..6 { cp.record_asked_question(&format!("参数{i}怎么选？")); }
    cp.recent_gate_block_step = Some(8);
    let mut cp: AgentCheckpoint = serde_json::from_value(serde_json::to_value(cp).unwrap()).unwrap();
    assert!(cp.inquiry_budget_notice(5).unwrap().contains("仍可询问"));
    assert!(cp.similar_question_notice("参数0怎么选？").unwrap().contains("此前未答"));
    assert!(ask_human_recent_gate_notice(&mut cp, 8).unwrap().contains("人工求助仍正常执行"));
    assert!(cp.approved_plan_summary.is_none());
    assert!(cp.human_waivers.is_empty());
    assert!(cp.git_destructive_approved_scopes.is_empty());
    assert!(ask_human_recent_gate_notice(&mut cp, 8).is_none());
}
