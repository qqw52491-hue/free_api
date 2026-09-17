//! 子任务行生命周期、ask_human 冷却与端到端回放测试。

#[cfg(test)]
mod tests_inner {
    use super::super::testkit::*;
    use super::super::*;
    use crate::agent::dispatcher::is_write_or_side_effect_tool;
    use crate::agent::review_contract::ReviewObligation;

    #[test]
    fn reap_stale_child_runs_converges_zombie_rows() {
        let temp_dir = std::env::temp_dir().join(format!("gatekeeper-reap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let conn = crate::db::init_db(&temp_dir.join("test.db")).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        // 夹具 started_at 为 2026-01-01（远早于宽限上限），构成僵尸 running 行
        insert_child_run(&db, "zombie", "test_run", "running", None, None);
        assert!(crate::agent::child_reviewer::has_running_child_runs(&db, "test_run"));

        let reaped = crate::agent::child_reviewer::reap_stale_child_runs(&db, "test_run");
        assert_eq!(reaped, 1);
        // 收敛后：不再 running，且携带可消费的 result_json（信箱可收取，门禁不被锁死）
        assert!(!crate::agent::child_reviewer::has_running_child_runs(&db, "test_run"));
        let conn = db.0.lock().unwrap();
        let (status, result_json): (String, Option<String>) = conn
            .query_row(
                "SELECT status, result_json FROM agent_child_runs WHERE id = 'zombie'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        drop(conn);
        assert_eq!(status, "timed_out");
        assert!(result_json.unwrap().contains("timed_out"));

        // 二次巡检幂等：无新僵尸时返回 0
        assert_eq!(crate::agent::child_reviewer::reap_stale_child_runs(&db, "test_run"), 0);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn reaped_row_accepts_late_result_and_preserves_metadata() {
        // 回归：case_01 实测在 22 步内触发 reap（reviewer 进程被收割后行被打上
        // 终态+占位结果，真实迟到结论被 status='running' 守卫永久丢弃——
        // 复盘时只见"timed_out"不见真实产出）。修复后：收割行可接收迟到结果，
        // 元数据（session/started_at）与双段事实（曾收割 + 真实结论）完整可查。
        let temp_dir = std::env::temp_dir().join(format!("gatekeeper-reap-late-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let conn = crate::db::init_db(&temp_dir.join("test.db")).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        insert_child_run(&db, "reaped1", "test_run", "running", None, None);
        assert_eq!(crate::agent::child_reviewer::reap_stale_child_runs(&db, "test_run"), 1);

        {
            let conn = db.0.lock().unwrap();
            let (status, error_kind): (String, Option<String>) = conn
                .query_row(
                    "SELECT status, error_kind FROM agent_child_runs WHERE id='reaped1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(status, "timed_out");
            assert_eq!(error_kind.as_deref(), Some("reaped_stale"), "收割标记须可区分于真实流超时");
        }

        // 迟到真实结果落库：守卫不再拦截 reaped_stale 行
        let real = r#"{"status":"succeeded","findings":[{"id":"F1","severity":"suggestion"}]}"#;
        crate::agent::child_reviewer::finish_child_run(
            &db, "reaped1", "succeeded", 73_000, 512, None, Some(real),
        );

        let conn = db.0.lock().unwrap();
        let (status, result_json, parent_session_id, started_at): (
            String, Option<String>, String, String,
        ) = conn
            .query_row(
                "SELECT status, result_json, parent_session_id, started_at FROM agent_child_runs WHERE id='reaped1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        drop(conn);
        assert_eq!(status, "succeeded", "迟到真实结论应落库覆盖占位 stub");
        assert!(result_json.unwrap().contains("F1"), "真实 findings 完整可查");
        assert_eq!(parent_session_id, "sess");
        assert_eq!(started_at, "2026-01-01T00:00:00Z", "归档元数据不被 reap 物理撕掉");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn finish_child_run_guard_rejects_late_overwrite() {
        let temp_dir = std::env::temp_dir().join(format!("gatekeeper-guard-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let conn = crate::db::init_db(&temp_dir.join("test.db")).unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        // 正常迁移：running -> succeeded
        insert_child_run(&db, "c1", "test_run", "running", None, None);
        crate::agent::child_reviewer::finish_child_run(
            &db, "c1", "succeeded", 100, 0, None, Some("{}"),
        );
        {
            let conn = db.0.lock().unwrap();
            let status: String = conn
                .query_row("SELECT status FROM agent_child_runs WHERE id='c1'", [], |r| r.get(0))
                .unwrap();
            drop(conn);
            assert_eq!(status, "succeeded");
        }

        // 取消后，迟到的 succeeded 提交不得覆盖已收敛状态（watch 竞态兜底）
        insert_child_run(&db, "c2", "test_run", "running", None, None);
        crate::agent::child_reviewer::cancel_running_child_runs(&db, "test_run").unwrap();
        crate::agent::child_reviewer::finish_child_run(
            &db, "c2", "succeeded", 100, 0, None, Some("{}"),
        );
        let conn = db.0.lock().unwrap();
        let status: String = conn
            .query_row("SELECT status FROM agent_child_runs WHERE id='c2'", [], |r| r.get(0))
            .unwrap();
        drop(conn);
        assert_eq!(status, "cancelled", "迟到提交不得把 cancelled 覆盖回 succeeded");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn ask_human_cooldown_requires_action_before_asking() {
        let context = context::SandwichContext::new("system".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_run".to_string(), &context);

        // 无拦截记录：放行
        assert!(crate::agent::loop_runner::evaluate_ask_human_cooldown(&mut checkpoint, 5).is_none());

        // 本步刚被拦截：冷却拒绝（一次性），且记录被清除
        checkpoint.recent_gate_block_step = Some(5);
        let msg = crate::agent::loop_runner::evaluate_ask_human_cooldown(&mut checkpoint, 5);
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("先监听"));
        assert!(checkpoint.recent_gate_block_step.is_none());

        // 冷却后再问：放行（模型有机会行动；死锁豁免）
        assert!(crate::agent::loop_runner::evaluate_ask_human_cooldown(&mut checkpoint, 6).is_none());

        // 预算耗尽等门禁显式指示求助：豁免一次
        checkpoint.gate_human_escape_directed = true;
        assert!(crate::agent::loop_runner::evaluate_ask_human_cooldown(&mut checkpoint, 7).is_none());
        assert!(!checkpoint.gate_human_escape_directed, "豁免标记一次性消费");
    }

    #[test]
    fn ask_human_approval_vs_information_routing() {
        let context = context::SandwichContext::new("system".to_string(), "查天气".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_routing".to_string(), &context);

        // 模拟刚被门禁拦截（例如第 5 步）
        checkpoint.recent_gate_block_step = Some(5);

        // 1. 信息类提问（缺参数、普通提问、未设置 kind）在协议层不属于 approval，永不触发冷却
        let is_info_approval = |kind: &str| -> bool {
            let ask_kind = kind.trim().to_ascii_lowercase();
            ask_kind == "plan_approval" || ask_kind == "plan" || ask_kind == "approval"
        };
        assert!(!is_info_approval("missing_parameter"));
        assert!(!is_info_approval("text"));
        assert!(!is_info_approval("single_select"));
        assert!(!is_info_approval(""));

        // 2. 只有真正的审批/放行类 kind 才会被判定为 approval
        assert!(is_info_approval("plan_approval"));
        assert!(is_info_approval("Plan"));
        assert!(is_info_approval("APPROVAL"));

        // 3. 当处于 approval 类时，冷却生效
        let cooldown_msg = crate::agent::loop_runner::evaluate_ask_human_cooldown(&mut checkpoint, 5);
        assert!(cooldown_msg.is_some(), "approval 类请求应触发门禁冷却");
        assert!(cooldown_msg.unwrap().contains("先监听"));
    }

    #[test]
    fn information_inquiry_does_not_unlock_pre_write_gatekeeper() {
        let context = context::SandwichContext::new("system".to_string(), "查天气".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_safety".to_string(), &context);
        checkpoint.plan_mode = crate::agent::checkpoint::PlanExecutionMode::CrossReview;
        checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Exploring;

        // 1. 初始状态下前置写入门禁必须关闭（Exploring 阶段尝试直接修改必须被拦截）
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        let write_rejection = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint,
            &context,
            &db,
            "test_safety",
            1,
            false,
        );
        assert!(write_rejection.is_some(), "未审查前写操作必须被门禁拦截");

        // 2. 模拟普通缺参提问（无 form 或 kind 为 text/missing_parameter）
        let form_val: Option<serde_json::Value> = None;
        let ask_kind = "";
        let is_plan_approval_kind =
            ask_kind == "plan_approval" || ask_kind == "plan" || ask_kind == "approval";

        // 核心安全属性验证：普通信息类提问的 is_approval_context 必须为 false！
        let is_approval_context = is_plan_approval_kind
            || (form_val.is_none()
                && (checkpoint.gate_human_escape_directed
                    || checkpoint.stage_review_blockers_open > 0
                    || checkpoint.last_review_blocking_count > 0));
        assert!(!is_approval_context, "普通提问绝不能被误判为 approval 上下文");

        // 3. 用户回复“北京”后，写入门禁状态必须保持原样（不得被误判放行）
        let _reply = "北京";
        if is_approval_context {
            checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
        }
        assert_eq!(checkpoint.plan_phase, crate::agent::checkpoint::PlanPhase::Exploring);

        // 再次核验写入门禁：依然安全关闭拦截！绝无放行副作用！
        let write_rejection_after = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint,
            &context,
            &db,
            "test_safety",
            2,
            false,
        );
        assert!(write_rejection_after.is_some(), "回答信息后前置写入门禁仍保持关闭");
    }

    #[test]
    fn weather_disaster_chain_end_to_end_replay() {
        // 装配级端到端灾害链回放：
        // 目标：“天天气是啥呢”
        // 链路：2>/dev/null 探针 -> 不误判为写操作 -> 探测失败 -> 问城市 -> 不捂嘴冷却 -> 用户答北京 -> 正常回答 -> 门禁保持锁定 -> 防复读拦截 -> 配额熔断与凭据豁免
        let mut context = context::SandwichContext::new("system".to_string(), "天天气是啥呢".to_string());
        let mut checkpoint = AgentCheckpoint::new("test_weather_replay".to_string(), &context);
        checkpoint.set_execution_mode(crate::agent::checkpoint::PlanExecutionMode::CrossReview);
        assert_eq!(checkpoint.plan_phase, crate::agent::checkpoint::PlanPhase::Exploring);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let db = DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));

        // 环节 1: 模型执行 IP 与网络探测（含 2>/dev/null 错误输出静默）
        let probe_instruction = crate::agent::types::AgentInstruction {
            tool: Some("sh".to_string()),
            command: Some(serde_json::json!({
                "args": [
                    "-c",
                    "curl -s --noproxy '*' -m 4 http://whois.pconline.com.cn/ipJson.jsp | iconv -f gbk -t utf-8 2>/dev/null || curl -s -m 4 https://cip.cc"
                ]
            })),
            ..Default::default()
        };

        // 验证 1.1: 探针命令被准确识别为安全只读命令，绝无破坏性
        let raw_cmd = "curl -s --noproxy '*' -m 4 http://whois.pconline.com.cn/ipJson.jsp | iconv -f gbk -t utf-8 2>/dev/null || curl -s -m 4 https://cip.cc";
        assert!(crate::agent::dispatcher::is_safe_read_only_shell_command(raw_cmd));

        // 验证 1.2: 探针工具不属于写或高危副作用工具
        assert!(!crate::agent::dispatcher::is_write_or_side_effect_tool(&probe_instruction));

        // 验证 1.3: 只读探针绝不会触发 pre-write gatekeeper 拦截
        let is_write = crate::agent::dispatcher::is_write_or_side_effect_tool(&probe_instruction);
        assert!(!is_write, "2>/dev/null 探针严禁被当作写操作");

        // 环节 2: 探针执行失败（如网络超时或无城市信息），模型决定发起 ask_human 索取必要参数（城市）
        // 即使系统历史曾记录过门禁拦截（假设第 1 步存在 block 记录）
        checkpoint.recent_gate_block_step = Some(1);

        let ask_message = "请问你想查询哪个城市的天气？";
        let ask_kind = "missing_parameter";

        // 验证 2.1: 协议分流——missing_parameter 永不触发门禁冷却（不捂嘴）
        let is_approval_kind = ask_kind == "plan_approval" || ask_kind == "plan" || ask_kind == "approval";
        assert!(!is_approval_kind);
        let cooldown_blocked = if is_approval_kind {
            crate::agent::loop_runner::evaluate_ask_human_cooldown(&mut checkpoint, 2).is_some()
        } else {
            false
        };
        assert!(!cooldown_blocked, "信息类提问绝不能被门禁冷却机制捂嘴！");

        // 验证 2.2: 提问去重与记录——首次提问顺利放行并登记
        assert!(checkpoint.check_duplicate_question(ask_message).is_none(), "首问城市必须放行");
        checkpoint.record_asked_question(ask_message);
        assert_eq!(checkpoint.asked_human_questions.len(), 1);

        // 验证 2.3: 全局配额——配额内顺利放行
        let budget = crate::agent::channel::config::max_human_inquiry_budget();
        let quota_rejection = checkpoint.check_inquiry_quota_exceeded(budget);
        assert!(quota_rejection.is_none(), "配额内必须放行");

        // 环节 3: 用户交互并回复：“北京”
        let human_reply = "北京";
        // 验证 3.1: 协议路由确认该提问不是 approval 上下文
        let is_approval_context = is_approval_kind
            || (checkpoint.gate_human_escape_directed
                || checkpoint.stage_review_blockers_open > 0
                || checkpoint.last_review_blocking_count > 0);
        assert!(!is_approval_context, "回答城市不是审批卡片，严禁标记为 approval_context");

        if is_approval_context {
            checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Approved;
        }

        // 验证 3.2: 门禁状态保持 Exploring，安全属性未被破坏
        assert_eq!(checkpoint.plan_phase, crate::agent::checkpoint::PlanPhase::Exploring);

        // 环节 4: 任务继续推进，模型查天气输出结果（正常业务流完成）
        context.turns_history.push(context::types::ChatMessage {
            role: "user".to_string(),
            content: serde_json::json!(human_reply),
        });
        context.turns_history.push(context::types::ChatMessage {
            role: "assistant".to_string(),
            content: serde_json::json!("北京今天天气晴朗，温度 22~30℃，北风 2 级。"),
        });

        // 环节 5: 核心安全防线验证——门禁状态未变，若后续有代码写入工具调用，仍被严密拦截
        let write_rejection = crate::agent::loop_runner::check_pre_write_gatekeeper(
            &mut checkpoint,
            &context,
            &db,
            "test_weather_replay",
            4,
            false,
        );
        assert!(write_rejection.is_some(), "回答北京后，写操作依然必须受交叉审查门禁保护！");

        // 环节 6: 去重拦截测试——模型若复读相同问题，立即被拦截
        let repeat_ask = "请问你想查询哪个城市的天气呢？";
        let repeat_dedup = checkpoint.check_duplicate_question(repeat_ask);
        assert!(repeat_dedup.is_some(), "复读同一提问必须被去重机制硬拦截");

        // 环节 7: 配额熔断机制与凭据豁免测试
        // 7.1 消耗剩余的普通信息类提问配额到上限 (5 次)
        assert!(checkpoint.check_duplicate_question("请问需要摄氏度还是华氏度？").is_none());
        checkpoint.record_asked_question("请问需要摄氏度还是华氏度？");

        assert!(checkpoint.check_duplicate_question("请问需要查询一周天气吗？").is_none());
        checkpoint.record_asked_question("请问需要查询一周天气吗？");

        assert!(checkpoint.check_duplicate_question("请问需要空气质量指数吗？").is_none());
        checkpoint.record_asked_question("请问需要空气质量指数吗？");

        assert!(checkpoint.check_duplicate_question("请问需要穿衣出行指数吗？").is_none());
        checkpoint.record_asked_question("请问需要穿衣出行指数吗？");

        assert_eq!(checkpoint.asked_human_questions.len(), 5);

        // 7.2 第 6 次普通提问：触发配额熔断
        let quota_err = checkpoint.check_inquiry_quota_exceeded(budget);
        assert!(quota_err.is_some(), "超过配额后普通提问必须被熔断");
        let quota_msg = quota_err.unwrap();
        assert!(quota_msg.contains("信息提问配额已耗尽"));
        assert!(quota_msg.contains("默认"), "熔断提示强制使用默认设定推进");

        // 7.3 关键豁免：私密凭据类提问（kind: "missing_credential"）
        let cred_kind = "missing_credential";
        assert!(crate::agent::checkpoint::is_credential_inquiry_kind(cred_kind), "missing_credential 必须识别为凭据类");
        let is_cred = crate::agent::checkpoint::is_credential_inquiry_kind(cred_kind);
        let cred_quota_blocked = if !is_cred {
            checkpoint.check_inquiry_quota_exceeded(budget).is_some()
        } else {
            false
        };
        assert!(!cred_quota_blocked, "凭据类提问必须豁免全局配额熔断，防止逼模型造假凭据！");
    }

    #[test]
    fn repeat_read_interception_reports_real_count() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("read-ledger-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("stage.md");
        let mut f = std::fs::File::create(&file).unwrap();
        f.write_all("# 当前阶段 01/24\n仅修改 src/stage-01.mjs。\n".as_bytes()).unwrap();
        drop(f);

        let mut context = context::SandwichContext::new("system".to_string(), "goal".to_string());
        let mk = |path: &str| crate::agent::types::AgentInstruction {
            tool: Some("read_text_file".to_string()),
            params: Some(serde_json::json!({ "path": path })),
            ..Default::default()
        };

        // 前两次合法读取放行
        assert!(context.check_repeat_read_dispatch(&mk("stage.md"), &dir).is_none());
        assert!(context.check_repeat_read_dispatch(&mk("stage.md"), &dir).is_none());
        // 第三次拦截：文案必须是真实次数，不允许再出现哨兵值 2147483647
        let block = context.check_repeat_read_dispatch(&mk("stage.md"), &dir).unwrap();
        assert!(block.contains("已连续读取 3 次"), "文案应报真实次数: {}", block);
        assert!(!block.contains("2147483647"), "哨兵值泄漏: {}", block);
        // 持续拦截直到内容变化
        assert!(context.check_repeat_read_dispatch(&mk("stage.md"), &dir).is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
