    use super::*;

    fn ctx() -> SandwichContext {
        SandwichContext::new("system".to_string(), "目标".to_string())
    }

    #[test]
    fn repeat_read_allows_first_two_advises_on_third_and_resets_on_change() {
        let mut context = ctx();
        let content = "line1\nline2\nline3";
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", content).is_none(), "第 1 次放行");
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", content).is_none(), "第 2 次放行");
        let blocked = context.check_repeat_read("read_text_file", "/ws/a.rs", content);
        assert!(blocked.is_some(), "第 3 次同内容给出提示");
        assert!(blocked.unwrap().contains("已连续读取"), "提示消息要给抓手");

        // 内容变化（改后重读）→ 放行并重置
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", "changed").is_none());

        // 重置后计数从 1 起：再连续两次放行，第 3 次同内容再提示
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", "changed").is_none(), "重置后第 2 次放行");
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", "changed").is_some(), "第 3 次同内容再提示");
    }

    #[test]
    fn sequential_window_reads_of_same_file_are_not_deduped() {
        // 回归：顺序窗口精读大文件是规定动作——同一文件不同行窗各自计数，
        // 只有"同窗口+同内容"第 3 次才拦（实测 L1-150/L151-300/L301-500
        // 曾被误拦，把模型逼回搜索循环；与 search→read 硬闸形成双堵死锁）。
        let mut context = ctx();
        let ws = std::env::temp_dir().join(format!("eval-window-read-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let file = ws.join("big.txt");
        std::fs::write(
            &file,
            (1..=400)
                .map(|i| format!("line{i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let path_str = file.to_string_lossy().to_string();

        let read_window = |s: i64, e: i64| crate::agent::types::AgentInstruction {
            tool: Some("local_file".to_string()),
            action: Some("read".to_string()),
            command: Some(serde_json::json!({
                "path": path_str.clone(),
                "start_line": s,
                "end_line": e,
            })),
            ..Default::default()
        };

        // 三个不同行窗顺序精读：全部放行（修前第三窗会被"哈希一致"误拦）
        assert!(context.check_repeat_read_dispatch(&read_window(1, 150), &ws).is_none(), "窗口1 首读放行");
        assert!(context.check_repeat_read_dispatch(&read_window(151, 300), &ws).is_none(), "窗口2 首读放行");
        assert!(context.check_repeat_read_dispatch(&read_window(301, 400), &ws).is_none(), "窗口3 首读放行");

        // 同一窗口同内容复读：第 3 次仅提示（保护原语义不退化）
        assert!(context.check_repeat_read_dispatch(&read_window(1, 150), &ws).is_none(), "窗口1 第 2 次放行");
        assert!(
            context.check_repeat_read_dispatch(&read_window(1, 150), &ws).is_some(),
            "窗口1 第 3 次同内容给出提示"
        );

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn local_file_native_reads_are_observed_like_other_reads() {
        let mut context = ctx();
        let ws = std::env::temp_dir().join(format!("eval-repeat-read-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let file = ws.join("legacy.txt");
        std::fs::write(&file, "stable content").unwrap();
        let path_str = file.to_string_lossy().to_string();

        let read_inst = |action: &str| crate::agent::types::AgentInstruction {
            tool: Some("local_file".to_string()),
            action: Some(action.to_string()),
            command: Some(serde_json::json!({ "path": path_str.clone() })),
            ..Default::default()
        };

        // 原生 local_file read：与 MCP 读取同一口径——第 1/2 次放行，第 3 次同内容拦截
        assert!(context.check_repeat_read_dispatch(&read_inst("read"), &ws).is_none(), "第 1 次放行");
        assert!(context.check_repeat_read_dispatch(&read_inst("read"), &ws).is_none(), "第 2 次放行");
        let blocked = context.check_repeat_read_dispatch(&read_inst("read"), &ws);
        assert!(blocked.is_some(), "第 3 次同内容给出提示（原生工具与其他读取使用同一观察口径）");

        // write/edit 不按重复读取判据处理（写操作归写前门禁管辖）
        assert!(context.check_repeat_read_dispatch(&read_inst("write"), &ws).is_none());
        assert!(context.check_repeat_read_dispatch(&read_inst("edit"), &ws).is_none());

        // 内容变化后放行并重置
        std::fs::write(&file, "changed content").unwrap();
        assert!(context.check_repeat_read_dispatch(&read_inst("read"), &ws).is_none(), "内容变化后重读合法");

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn ledger_keys_are_per_path_and_reset_clears() {
        let mut context = ctx();
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", "A").is_none());
        assert!(context.check_repeat_read("read_text_file", "/ws/b.rs", "B").is_none(), "不同路径互不干扰");
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", "A").is_none());
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", "A").is_some());
        context.reset_read_ledger();
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", "A").is_none(), "清零后重读合法");
    }

    #[test]
    fn batch_failures_only_advise_and_legacy_cooldown_is_cleared() {
        let mut context = ctx();
        assert!(context.record_batch_dispatch_outcome(false).is_none());
        let notice = context.record_batch_dispatch_outcome(false).unwrap();
        assert!(notice.contains("两批均未成功启动"));
        assert!(context.batch_dispatch_notice(true).is_none());
        context.batch_dispatch_circuit_breaker_remaining = 3;
        let restored: super::SandwichContext = serde_json::from_value(serde_json::to_value(&context).unwrap()).unwrap();
        let mut context = restored;
        assert!(context.batch_dispatch_notice(true).unwrap().contains("已清除"));
        assert_eq!(context.batch_dispatch_circuit_breaker_remaining, 0);
        assert!(context.batch_dispatch_notice(true).is_none());
        assert!(context.record_batch_dispatch_outcome(true).is_none());
    }

    // ═══ 取证台账（展示投影）═══

    fn inst(tool: &str, action: &str, params: serde_json::Value) -> crate::agent::types::AgentInstruction {
        crate::agent::types::AgentInstruction {
            tool: Some(tool.to_string()),
            action: Some(action.to_string()),
            command: Some(params),
            ..Default::default()
        }
    }

    #[test]
    fn evidence_receipt_aggregates_reads_searches_and_retrievals() {
        let mut context = ctx();

        // 同一文件同一窗口读两次 → 聚合为一条，步号取首末
        let read = inst(
            "local_file",
            "read",
            serde_json::json!({"path": "src/views/ChatView.vue", "start_line": 1, "end_line": 120}),
        );
        context.note_evidence_receipt(3, &read, "out-a");
        context.note_evidence_receipt(57, &read, "out-b");
        // 不同窗口 → 不同条目
        let read2 = inst(
            "local_file",
            "read",
            serde_json::json!({"path": "src/views/ChatView.vue", "start_line": 200, "end_line": 300}),
        );
        context.note_evidence_receipt(58, &read2, "out-c");
        // 列目录 / 检索 / 回查
        context.note_evidence_receipt(4, &inst("local_file", "list", serde_json::json!({"path": "src", "depth": 2})), "out-d");
        context.note_evidence_receipt(9, &inst("search", "search", serde_json::json!({"query": "paste", "path": "src/"})), "out-e");
        context.note_evidence_receipt(60, &inst("get_step_output", "get_step_output", serde_json::json!({"output_id": "out-a"})), "out-f");
        // 写操作不进取证台账
        context.note_evidence_receipt(61, &inst("local_file", "write", serde_json::json!({"path": "src/x.rs"})), "out-g");

        assert_eq!(context.evidence_ledger.len(), 5, "读×2条目+list+search+retrieve，write 不记账");
        let read_key = "read|src/views/ChatView.vue|L1-120";
        let e = &context.evidence_ledger[read_key];
        assert_eq!(e.count, 2);
        assert_eq!(e.first_step, 3);
        assert_eq!(e.last_step, 57);
        assert_eq!(e.output_id.as_deref(), Some("out-b"), "句柄取最近一次");
    }

    #[test]
    fn evidence_ledger_survives_blocking_ledger_reset() {
        let mut context = ctx();
        context.note_evidence_receipt(
            10,
            &inst("local_file", "read", serde_json::json!({"path": "a.rs"})),
            "out-1",
        );
        // 压缩时只清读取观察台账；展示台账保留——脱水后模型更需要这张索引
        context.reset_read_ledger();
        assert!(context.read_ledger.is_empty());
        assert_eq!(context.evidence_ledger.len(), 1, "取证台账跨压缩保留");
    }

    #[test]
    fn evidence_ledger_action_normalization_matches_dispatcher() {
        let mut context = ctx();
        // action 字段缺省为工具名、真实动作在 params.action（实弹轨迹形态）
        // → 与 dispatcher 同口径归一为 list
        let i = inst("local_file", "local_file", serde_json::json!({"action": "list", "path": "src/components"}));
        context.note_evidence_receipt(2, &i, "out-1");
        assert!(context.evidence_ledger.contains_key("list|src/components|"));
        // action=read + params.action=list 的混合形态：与 dispatcher 一致仍按 read 记
        // （归一仅在 action 缺省/同名时取 params.action）
        let i = inst("local_file", "read", serde_json::json!({"action": "list", "path": "src/components"}));
        context.note_evidence_receipt(3, &i, "out-2");
        assert!(context.evidence_ledger.contains_key("read|src/components|整读"));
    }

    #[test]
    fn read_overlap_hint_fires_on_overlap_and_suppresses_after_write() {
        let mut context = ctx();
        context.note_evidence_receipt(
            3,
            &inst("local_file", "read", serde_json::json!({"path": "src/a.rs", "start_line": 1, "end_line": 150})),
            "out-a",
        );
        // 重叠读（不同行窗）→ 提示
        let overlap = inst(
            "local_file", "read",
            serde_json::json!({"path": "src/a.rs", "start_line": 100, "end_line": 260}),
        );
        let hint = context.read_overlap_hint(&overlap);
        assert!(hint.is_some(), "重叠读应出提示");
        assert!(hint.as_deref().unwrap().contains("L1-150"), "提示应含已覆盖区间: {}", hint.as_deref().unwrap());
        assert!(hint.as_deref().unwrap().contains("out-a"), "提示应带句柄");
        // 不重叠读 → 不提示
        let fresh = inst(
            "local_file", "read",
            serde_json::json!({"path": "src/a.rs", "start_line": 500, "end_line": 650}),
        );
        assert!(context.read_overlap_hint(&fresh).is_none());
        // 读后该文件被写过 → 重读正当，不提示
        context.side_effects.push(crate::agent::context::types::SideEffectRecord {
            step_id: 10,
            tool: "local_file".into(),
            description: "edit".into(),
            timestamp: "t".into(),
            affected_paths: vec!["src/a.rs".into()],
        });
        assert!(context.read_overlap_hint(&overlap).is_none(), "写后重读不应提示");
        // 非读指令忽略
        let w = inst("local_file", "write", serde_json::json!({"path": "src/a.rs", "content": "x"}));
        assert!(context.read_overlap_hint(&w).is_none());
    }

    #[test]
    fn evidence_ledger_render_sorted_bounded_and_shows_handle() {
        let mut context = ctx();
        for i in 0..25usize {
            context.note_evidence_receipt(
                i,
                &inst("local_file", "read", serde_json::json!({"path": format!("f{:02}.rs", i)})),
                &format!("out-{}", i),
            );
        }
        let rendered = crate::agent::context::render::ContextRenderer::render_evidence_ledger(
            &context.evidence_ledger,
        );
        assert!(rendered.contains("【取证台账】"));
        assert!(rendered.contains("get_step_output"));
        assert!(rendered.contains("out-24"), "最近步的记录应含句柄: {}", rendered);
        assert!(rendered.contains("另有 5 项更早记录未列出"));
        // 最近步优先，超出上限的最旧记录不渲染
        assert!(rendered.contains("f24.rs"), "最近记录必须渲染: {}", rendered);
        assert!(rendered.find("f00.rs").is_none(), "超出 20 行上限的最旧记录不渲染");
    }

    #[test]
    fn run_telemetry_renders_facts_only() {
        use crate::agent::context::render::ContextRenderer;
        // 首步之前无遥测
        assert!(ContextRenderer::render_run_telemetry(0, 0, 0, None, 0).is_empty());
        let out = ContextRenderer::render_run_telemetry(12, 1_234_567, 45_678, Some(67_890), 1_000_000);
        assert!(out.contains("step=12"));
        assert!(out.contains("1234567"));
        assert!(out.contains("45678"));
        assert!(out.contains("67890"));
        assert!(out.contains("(7%)"));
        // 不含任何节奏暗示词
        for banned in ["收敛", "停止", "上限", "尽快", "太多"] {
            assert!(!out.contains(banned), "遥测卡不得含认知暗示词 {}: {}", banned, out);
        }
    }
