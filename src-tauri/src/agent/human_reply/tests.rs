use super::*;
use crate::agent::review_team::tests::TestDb;

fn fixture(reply: &str) -> (TestDb, SandwichContext, AgentCheckpoint) {
    let data = TestDb::new();
    let context = SandwichContext::new("sys".into(), "task".into());
    let mut cp = AgentCheckpoint::new("human-reply-fixture".into(), &context);
    let command = "git checkout -- config/";
    cp.block_git_destructive(command);
    cp.git_destructive_scopes
        .insert(command.into(), "scope-before-reply".into());
    cp.git_destructive_mark_asked("是否执行 `git checkout -- config/`？");
    cp.register_human_reply("是否执行 git checkout -- config/？", reply, false);
    (data, context, cp)
}

#[test]
fn free_text_never_approves_or_rejects_without_a_model_decision() {
    for reply in [
        "不可以",
        "我还没有同意",
        "请先解释怎么执行",
        "确认执行",
        "可以，先说明用途",
    ] {
        let (_, _, cp) = fixture(reply);
        assert!(cp.git_destructive_approved_scopes.is_empty());
        assert_eq!(cp.pending_human_reply.as_ref().unwrap().reply, reply);
        let card = cp.plan_gate_projection();
        let prompt = crate::agent::context::render::ContextRenderer::render_plan_gate(Some(&card));
        assert!(prompt.contains(reply) && prompt.contains("human_reply_decision"));
    }
}

#[test]
fn model_judgment_is_source_bound_atomic_and_survives_restore() {
    let (data, mut context, cp) = fixture("不用操作，先解释");
    let mut cp: AgentCheckpoint =
        serde_json::from_str(&serde_json::to_string(&cp).unwrap()).unwrap();
    let source = cp.pending_human_reply.clone().unwrap();
    let mut decision = HumanReplyDecision {
        reply_id: "old-reply".into(),
        git_decisions: vec![GitReplyDecision {
            command: "git checkout -- config/".into(),
            approved: true,
        }],
        plan_approved: None,
        reason: "model judgment".into(),
    };
    assert!(cp
        .apply_human_reply_decision(&data.db, &mut context, &decision)
        .is_err());
    assert!(cp.git_destructive_approved_scopes.is_empty());
    decision.reply_id = source.reply_id;
    decision.git_decisions.push(GitReplyDecision {
        command: "git reset --hard".into(),
        approved: true,
    });
    assert!(cp
        .apply_human_reply_decision(&data.db, &mut context, &decision)
        .is_err());
    assert!(cp.git_destructive_approved_scopes.is_empty());
    decision.git_decisions.truncate(1);
    decision.git_decisions[0].approved = false;
    cp.apply_human_reply_decision(&data.db, &mut context, &decision)
        .unwrap();
    assert!(cp.git_destructive_approved_scopes.is_empty());
    assert!(cp.pending_human_reply.is_none());
    assert_eq!(
        cp.last_human_reply_resolution
            .as_ref()
            .unwrap()
            .source
            .reply,
        "不用操作，先解释"
    );
    cp.apply_human_reply_decision(&data.db, &mut context, &decision)
        .unwrap();
    assert!(
        cp.git_destructive_approved_scopes.is_empty(),
        "恢复重放不能新发许可"
    );
}

#[test]
fn model_plan_approval_cannot_be_borrowed_from_an_unrelated_question() {
    let (data, mut context, mut cp) = fixture("确认执行");
    let id = cp.pending_human_reply.as_ref().unwrap().reply_id.clone();
    let decision = HumanReplyDecision {
        reply_id: id,
        git_decisions: vec![],
        plan_approved: Some(true),
        reason: "model judgment".into(),
    };
    assert!(cp
        .apply_human_reply_decision(&data.db, &mut context, &decision)
        .is_err());
    assert_ne!(cp.plan_phase, PlanPhase::Approved);
    cp.register_human_reply("批准这个完整方案吗？", "我还没有同意", true);
    assert_ne!(cp.plan_phase, PlanPhase::Approved);
    let mut decision = decision;
    decision.reply_id = cp.pending_human_reply.as_ref().unwrap().reply_id.clone();
    decision.plan_approved = Some(false);
    cp.apply_human_reply_decision(&data.db, &mut context, &decision)
        .unwrap();
    assert_eq!(cp.plan_phase, PlanPhase::Reviewing);
}
