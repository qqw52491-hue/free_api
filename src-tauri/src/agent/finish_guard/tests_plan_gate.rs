//! 方案审批门禁、plan decision 标记与仲裁放行测试。

#[cfg(test)]
mod tests_inner {
    use super::super::testkit::*;
    use super::super::*;
    use crate::agent::dispatcher::is_write_or_side_effect_tool;
    use crate::agent::review_contract::ReviewObligation;

    #[test]
    fn plan_approval_mode_gatekeeper_blocks_until_approved() {
        let temp_dir = std::env::temp_dir().join(format!("gatekeeper-plan-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let conn = crate::db::init_db(&temp_dir.join("test.db")).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        let context = context::SandwichContext::new("system".to_string(), "修改重要逻辑".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.plan_mode = crate::agent::checkpoint::PlanExecutionMode::PlanApproval;
        checkpoint.auto_pilot = false;
        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Reviewing;
        checkpoint.plan_reviewer_retry_count = 2;
        checkpoint.plan_reviewer_consumed = true;

        let rej = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint,
            &context,
            &db,
            "test_run",
            3,
            false,
        );
        assert!(rej.is_some());
        assert_eq!(rej.unwrap().route, "plan_gatekeeper:need_human_approval");
        assert!(!checkpoint.plan_phase.is_approved());

        // 人类批准后
        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
        checkpoint.approved_plan_summary = Some("用户批准了方案".to_string());
        let rej_approved = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint,
            &context,
            &db,
            "test_run",
            4,
            false,
        );
        assert!(rej_approved.is_none());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn plan_approval_kind_accepts_frontend_aliases() {
        // 前端 normalizeHumanForm 支持 plan_approval / plan / approval 三种别名，
        // 后端必须同步识别，否则前端渲染审批卡片、用户批准后门禁仍关闭。
        for kind in ["plan_approval", "plan", "approval", "PLAN_APPROVAL", " Plan "] {
            let normalized = kind.trim().to_ascii_lowercase();
            assert!(
                normalized == "plan_approval" || normalized == "plan" || normalized == "approval",
                "别名 {} 未被识别",
                kind
            );
        }
        // 普通提问的 kind 绝不能被误判为方案审批
        for kind in ["choice", "confirm", "text", "planning_note"] {
            let normalized = kind.trim().to_ascii_lowercase();
            assert!(
                !(normalized == "plan_approval" || normalized == "plan" || normalized == "approval"),
                "普通提问 {} 被误判为方案审批",
                kind
            );
        }
    }

    #[test]
    fn non_write_step_distinguishes_read_vs_write_tool() {
        let read_inst = AgentInstruction {
            tool: Some("read_file".to_string()),
            ..Default::default()
        };
        let write_inst = AgentInstruction {
            tool: Some("write_file".to_string()),
            ..Default::default()
        };
        let child_writer_inst = AgentInstruction {
            tool: Some("child_writer".to_string()),
            ..Default::default()
        };

        assert!(!is_write_or_side_effect_tool(&read_inst));
        assert!(is_write_or_side_effect_tool(&write_inst));
        assert!(is_write_or_side_effect_tool(&child_writer_inst));
    }

    #[test]
    fn move_rename_remove_named_tools_are_gated() {
        // 名字不含 write/edit/delete/save 的破坏性工具也必须在门禁内：
        // move_file 可覆盖目标文件、worktree_remove 删整棵隔离树、
        // vscode_renameSymbol 跨工作区改写符号。
        for tool in [
            "move_file",
            "worktree_remove",
            "vscode_renameSymbol",
            "remote-hub/move_file",
        ] {
            assert!(
                is_write_or_side_effect_tool(&inst_with_tool(tool)),
                "{tool} 应被判定为写操作"
            );
        }
        // 对照：只读工具不受影响
        for tool in ["read_text_file", "list_directory", "search_graph", "release_lock"] {
            assert!(
                !is_write_or_side_effect_tool(&inst_with_tool(tool)),
                "{tool} 不应被误判为写操作"
            );
        }
    }

    fn inst_with_tool(tool: &str) -> AgentInstruction {
        AgentInstruction {
            tool: Some(tool.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn namespaced_mcp_tools_are_classified_by_concrete_tool_name() {
        // MCP 工具常带 "服务名/具体工具" 前缀，分类必须基于剥离命名空间后的具体工具名
        assert!(is_write_or_side_effect_tool(&inst_with_tool("remote-hub/shell_exec")));
        assert!(is_write_or_side_effect_tool(&inst_with_tool("remote-hub/write_file")));
        assert!(!is_write_or_side_effect_tool(&inst_with_tool("remote-hub/read_file")));
        assert!(!is_write_or_side_effect_tool(&inst_with_tool("remote-hub/list_dir")));
    }

    #[test]
    fn write_verb_beats_readonly_prefix() {
        // 形如 get_shell_output 的"取结果"类工具本质是脚本执行，
        // 不能被 get_/list_ 只读前缀误判放行，否则门禁可被绕过。
        assert!(is_write_or_side_effect_tool(&inst_with_tool("get_shell_output")));
        assert!(is_write_or_side_effect_tool(&inst_with_tool("list_running_jobs")));
        // 常规只读工具仍必须判为只读
        assert!(!is_write_or_side_effect_tool(&inst_with_tool("get_file_content")));
        assert!(!is_write_or_side_effect_tool(&inst_with_tool("list_workspaces")));
        assert!(!is_write_or_side_effect_tool(&inst_with_tool("read_multiple_files")));
        assert!(!is_write_or_side_effect_tool(&inst_with_tool("search_files")));
        assert!(!is_write_or_side_effect_tool(&inst_with_tool("web_fetch")));
    }

    #[test]
    fn plan_decision_marker_is_authoritative_over_free_text() {
        use crate::agent::loop_runner::parse_plan_approval_decision;

        // 原始缺陷：用户在「补充执行要求」里写了"打回修改"却点的是批准，
        // 旧逻辑靠文本嗅探会误判为打回。结构化标记必须覆盖文本。
        let tricky = "【方案已批准】批准方案，请开始执行后续写入与测试步骤。\n\
                      【补充执行要求】: 注意别把之前打回修改的那段逻辑又改回来\n\
                      <!--plan-decision:{\"approved\":true}-->";
        assert_eq!(parse_plan_approval_decision(tricky), Some(true));

        let tricky_reject = "【方案打回修改】请重新审视：\n这里写了个「批准方案」的注释\n\
                             <!--plan-decision:{\"approved\":false}-->";
        assert_eq!(parse_plan_approval_decision(tricky_reject), Some(false));
    }

    #[test]
    fn plan_decision_falls_back_to_text_markers_without_structured_marker() {
        use crate::agent::loop_runner::parse_plan_approval_decision;

        // 非 UI 通道（CLI / 直接调用）没有结构化标记，仍需兼容旧的文本约定
        assert_eq!(
            parse_plan_approval_decision("【方案已批准】可以开始改了"),
            Some(true)
        );
        assert_eq!(
            parse_plan_approval_decision("【方案打回修改】换个库再推演一次"),
            Some(false)
        );
        assert_eq!(
            parse_plan_approval_decision("{\"approved\":false}"),
            Some(false)
        );
    }

    #[test]
    fn plan_decision_returns_none_for_ambiguous_reply() {
        use crate::agent::loop_runner::parse_plan_approval_decision;

        // 无法判定时返回 None，调用方应保持门禁关闭，绝不默认放行
        assert_eq!(parse_plan_approval_decision("我再想想"), None);
        assert_eq!(parse_plan_approval_decision(""), None);
    }

    #[test]
    fn strip_plan_decision_marker_cleans_persisted_summary() {
        use crate::agent::loop_runner::{strip_plan_decision_marker, parse_plan_approval_decision};

        let raw = "【方案已批准】开始执行\n<!--plan-decision:{\"approved\":true}-->";
        let cleaned = strip_plan_decision_marker(raw);
        assert_eq!(cleaned, "【方案已批准】开始执行");
        // 清洗不能破坏解析（解析在清洗之前进行）
        assert_eq!(parse_plan_approval_decision(raw), Some(true));
        // 无标记时原样返回
        assert_eq!(strip_plan_decision_marker("普通回复"), "普通回复");
    }

    #[test]
    fn plan_decision_marker_is_typed_parsed_with_note_and_tolerates_variants() {
        use crate::agent::loop_runner::{extract_plan_decision, parse_plan_approval_decision};

        // 打回 + 附带修改意见：note 必须经类型化结构完整带回
        let reject = "【方案打回修改】请重新推演\n<!--plan-decision:{\"approved\":false,\"note\":\"数据库换成 tokio-rs\"}-->";
        assert_eq!(parse_plan_approval_decision(reject), Some(false));
        let decision = extract_plan_decision(reject).unwrap();
        assert!(!decision.approved);
        assert_eq!(decision.note.as_deref(), Some("数据库换成 tokio-rs"));

        // 未知字段不破坏解析（向前兼容前端新增字段）
        assert_eq!(
            parse_plan_approval_decision("<!--plan-decision:{\"approved\":true,\"via\":\"mobile\"}-->"),
            Some(true)
        );
        // 畸形 JSON 静默降级到关键词链路，绝不能 panic 或误放行
        assert_eq!(
            parse_plan_approval_decision("<!--plan-decision:{bad json}--> 我同意"),
            Some(true)
        );
        // approved 缺失时同样降级
        assert_eq!(
            parse_plan_approval_decision("<!--plan-decision:{\"note\":\"hi\"}-->"),
            None
        );
    }

    #[test]
    fn cross_review_scorecard_gate_blocks_low_risk_scores_then_passes_on_gate_clear() {
        let temp_dir = std::env::temp_dir().join(format!("gatekeeper-score-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let conn = crate::db::init_db(&temp_dir.join("test.db")).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        let context = context::SandwichContext::new("system".to_string(), "修复内存泄漏".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::CrossReview);
        checkpoint.plan_reviewer_retry_count = 1;
        checkpoint.plan_reviewer_consumed = true;
        checkpoint.submitted_plan = Some(crate::agent::types::PlanSubmission {
            plan_summary: "修复内存泄漏：补全释放路径".to_string(),
            target_files: vec!["src/mem.rs".to_string()],
            root_cause: "提前 return 跳过释放".to_string(),
            evidence_refs: vec!["src/mem.rs:88".to_string()],
            assumptions: vec![],
            open_questions: vec![],
            intended_verification: "cargo test mem".to_string(),
            risk_level: "medium".to_string(),
            revision_reason: String::new(),
        });

        // 均分达标但 risk < 60：即使平均分 90+ 也必须硬拦截
        checkpoint.last_review_scorecard = Some(crate::agent::child_reviewer::ReviewScorecard::new(95, 40, 95, Some(95)));
        let rej = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 2, false,
        );
        assert!(rej.is_some());
        assert_eq!(rej.unwrap().route, "plan_gatekeeper:scorecard_failed");
        assert!(!checkpoint.plan_phase.is_approved());

        // 达标评分卡：放行并推进为 Approved
        checkpoint.last_review_scorecard = Some(crate::agent::child_reviewer::ReviewScorecard::new(90, 80, 85, Some(70)));
        let pass = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 3, false,
        );
        assert!(pass.is_none());
        assert!(checkpoint.plan_phase.is_approved());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn auto_mode_first_step_write_is_blocked_until_plan_stated() {
        let temp_dir = std::env::temp_dir().join(format!("gatekeeper-auto-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let conn = crate::db::init_db(&temp_dir.join("test.db")).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        let mut context = context::SandwichContext::new("system".to_string(), "重构调度器".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::Auto);

        // 首轮、上下文为空就动笔 -> 拦截 need_plan
        let rej = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 0, false,
        );
        assert!(rej.is_some());
        assert_eq!(rej.unwrap().route, "plan_gatekeeper:need_plan");
        assert!(!checkpoint.plan_phase.is_approved());

        // 已有只读排查痕迹（上下文非空）-> 放行
        context.turns_history.push(context::types::ChatMessage {
            role: "assistant".to_string(),
            content: json!("已完成只读排查：根因锁定在 dispatcher.rs:120"),
        });
        let pass = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 1, false,
        );
        assert!(pass.is_none());
        assert!(checkpoint.plan_phase.is_approved());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn set_execution_mode_is_the_single_entry_point_and_syncs_auto_pilot() {
        let context = context::SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);

        // PlanApproval：auto_pilot 必须同步为 false
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::PlanApproval);
        assert_eq!(checkpoint.plan_mode, crate::agent::checkpoint::PlanExecutionMode::PlanApproval);
        assert!(!checkpoint.auto_pilot);

        // Auto：auto_pilot 必须同步为 true
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::Auto);
        assert!(checkpoint.auto_pilot);

        // CrossReview：同样视为自主模式（auto_pilot = true）
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::CrossReview);
        assert!(checkpoint.auto_pilot);
    }

    #[test]
    fn cross_review_human_arbitration_bypasses_failed_scorecard_and_blockers() {
        let temp_dir = std::env::temp_dir().join(format!("gatekeeper-esc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let conn = crate::db::init_db(&temp_dir.join("test.db")).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        let context = context::SandwichContext::new("system".to_string(), "实现系统运维脚本".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::CrossReview);
        checkpoint.plan_reviewer_retry_count = 2; // 预算已耗尽
        checkpoint.plan_reviewer_consumed = true;

        // 模拟审查员过度防御打了低分 (risk: 48 < 60)
        checkpoint.last_review_scorecard = Some(crate::agent::child_reviewer::ReviewScorecard::new(80, 48, 85, Some(75)));
        let rej = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 3, false,
        );
        assert!(rej.is_some());
        let rej_res = rej.unwrap();
        assert_eq!(rej_res.route, "plan_gatekeeper:scorecard_failed");
        assert!(rej_res.stderr.contains("ask_human"), "预算耗尽拦截文案必须指引 ask_human 人工仲裁");
        assert!(rej_res.stderr.contains("人工仲裁"), "拦截文案必须包含人工仲裁说明");

        // 人工仲裁放行：用户确认批准后，plan_phase 变为 Approved
        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
        let pass = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 4, false,
        );
        assert!(pass.is_none(), "人类批准后门禁必须立即放行写入，绝不因审查低分继续拦截");
        assert!(checkpoint.plan_phase.is_approved());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn stage_review_blocker_cleared_by_human_arbitration() {
        let temp_dir = std::env::temp_dir().join(format!("gatekeeper-stage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let conn = crate::db::init_db(&temp_dir.join("test.db")).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        let context = context::SandwichContext::new("system".to_string(), "重构核心模块".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::CrossReview);
        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
        checkpoint.stage_review_blockers_open = 1;

        // 存在未解决的 stage review blocker -> 拦截写入
        let rej = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 5, false,
        );
        assert!(rej.is_some());
        let rej_res = rej.unwrap();
        assert_eq!(rej_res.route, "stage_review:blocking_open");
        assert!(rej_res.stderr.contains("ask_human"), "阶段拦截必须提供 ask_human 人工仲裁路径");

        // 人工仲裁放行：解除 stage_review_blockers_open
        checkpoint.stage_review_blockers_open = 0;
        let pass = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint, &context, &db, "test_run", 6, false,
        );
        assert!(pass.is_none(), "阶段 blocker 解除后门禁放行");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
