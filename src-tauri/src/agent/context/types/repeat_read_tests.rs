    use super::*;

    fn ctx() -> SandwichContext {
        SandwichContext::new("system".to_string(), "目标".to_string())
    }

    #[test]
    fn repeat_read_allows_first_two_blocks_third_and_resets_on_change() {
        let mut context = ctx();
        let content = "line1\nline2\nline3";
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", content).is_none(), "第 1 次放行");
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", content).is_none(), "第 2 次放行");
        let blocked = context.check_repeat_read("read_text_file", "/ws/a.rs", content);
        assert!(blocked.is_some(), "第 3 次同内容必须拦截");
        assert!(blocked.unwrap().contains("已连续读取"), "拦截消息要给抓手");

        // 内容变化（改后重读）→ 放行并重置
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", "changed").is_none());

        // 重置后计数从 1 起：再连续两次放行，第 3 次同内容又拦（count ≥ 3）
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", "changed").is_none(), "重置后第 2 次放行");
        assert!(context.check_repeat_read("read_text_file", "/ws/a.rs", "changed").is_some(), "第 3 次同内容再拦");
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

        // 同一窗口同内容复读：第 3 次仍拦（保护原语义不退化）
        assert!(context.check_repeat_read_dispatch(&read_window(1, 150), &ws).is_none(), "窗口1 第 2 次放行");
        assert!(
            context.check_repeat_read_dispatch(&read_window(1, 150), &ws).is_some(),
            "窗口1 第 3 次同内容必须拦截"
        );

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn local_file_native_reads_are_deduped_like_mcp_reads() {
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
        assert!(blocked.is_some(), "第 3 次同内容必须拦截（原生工具此前完全不拦）");

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
    fn test_batch_dispatch_circuit_breaker_trips_and_recovers() {
        let mut context = ctx();
        // 1. 首次失败：不触发熔断
        assert!(context.record_batch_dispatch_outcome(false).is_none());
        assert!(context.check_batch_dispatch_allowed(true).is_ok());

        // 2. 连续第 2 次失败：触发 3 步熔断
        let alert = context.record_batch_dispatch_outcome(false);
        assert!(alert.is_some());
        assert!(alert.unwrap().contains("触发熔断降级"));

        // 3. 熔断期间：阻止批发派发，连续 3 步倒数
        let err1 = context.check_batch_dispatch_allowed(true).unwrap_err();
        assert!(err1.contains("还剩 3 步"));
        let err2 = context.check_batch_dispatch_allowed(true).unwrap_err();
        assert!(err2.contains("还剩 2 步"));
        let err3 = context.check_batch_dispatch_allowed(true).unwrap_err();
        assert!(err3.contains("还剩 1 步"));

        // 4. 熔断保护期结束：恢复允许批发
        assert!(context.check_batch_dispatch_allowed(true).is_ok());
    }
