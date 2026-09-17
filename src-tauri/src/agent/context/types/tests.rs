    use super::*;

    #[test]
    fn test_update_memory_blocks_sensitive_secrets() {
        let mut ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let _ = ctx.update_memory("api_key".to_string(), "sk-1234567890abcdef".to_string());
        assert!(
            !ctx.memories.contains_key("api_key"),
            "Sensitive API key must be blocked!"
        );

        let _ = ctx.update_memory("db_port".to_string(), "5432".to_string());
        assert_eq!(
            ctx.memories.get("db_port").map(|s| s.as_str()),
            Some("5432")
        );
    }

    #[test]
    fn test_tool_manual_deduplication_keeps_service_catalog_and_subtool_schema_distinct() {
        let mut ctx = SandwichContext::new("sys".to_string(), "goal".to_string());

        // 初始状态：未加载
        assert!(!ctx.has_tool_manual("remote-hub"));
        assert!(!ctx.has_tool_manual("remote-hub/list_repos"));
        assert!(!ctx.has_tool_manual("remote-hub/read_text_file"));

        // 服务级目录不应被误认为已提供某个具体子工具的参数 schema。
        ctx.fetched_tools.insert("remote-hub".to_string());
        assert!(ctx.has_tool_manual("remote-hub"));
        assert!(!ctx.has_tool_manual("remote-hub/list_repos"));
        assert!(!ctx.has_tool_manual("remote-hub/read_text_file"));

        // 精确子工具手册仅覆盖自身，仍允许其他子工具按需披露。
        ctx.fetched_tools
            .insert("remote-hub/read_text_file".to_string());
        assert!(ctx.has_tool_manual("remote-hub/read_text_file"));
        assert!(!ctx.has_tool_manual("remote-hub/shell_exec"));

        // 2. 模拟从数据库恢复（内存集合为空，但历史流中包含手册文本）
        let mut restored_ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        restored_ctx.turns_history.push(ChatMessage {
            role: "user".to_string(),
            content: json!("【工具/Skill 操作手册: remote-hub】\n60 个工具说明\n(已写入历史流)"),
        });

        // 服务级持久化手册只覆盖服务级目录，不覆盖任意子工具。
        assert!(restored_ctx.has_tool_manual("remote-hub"));
        assert!(!restored_ctx.has_tool_manual("remote-hub/read_text_file"));

        // 执行反向同步后，内存集合也保留精确键语义。
        restored_ctx.sync_fetched_tools_from_history();
        assert!(restored_ctx.fetched_tools.contains("remote-hub"));
        assert!(!restored_ctx.has_tool_manual("remote-hub/search_code"));
    }
    #[test]
    fn repeated_deterministic_failure_blocks_same_parameters_but_not_transient_or_changed_calls() {
        let mut ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let params = json!({"path": "/missing/project"});
        ctx.record_failed_attempt(
            1,
            "remote-hub/list_directory".to_string(),
            "list_directory".to_string(),
            &params,
            "ENOENT: no such file or directory".to_string(),
            Some("out-1".to_string()),
        );
        assert!(ctx
            .repeated_failure_blocker("remote-hub/list_directory", "list_directory", &params)
            .is_some());
        assert!(ctx
            .repeated_failure_blocker(
                "remote-hub/list_directory",
                "list_directory",
                &json!({"path": "/different/project"}),
            )
            .is_none());

        let transient = json!({"query": "retry me"});
        ctx.record_failed_attempt(
            2,
            "web_search".to_string(),
            "search".to_string(),
            &transient,
            "HTTP 429 rate limit".to_string(),
            None,
        );
        assert!(ctx
            .repeated_failure_blocker("web_search", "search", &transient)
            .is_none());
    }

    #[test]
    fn record_side_effect_invalidates_prior_change_verification() {
        let mut ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        ctx.record_side_effect(1, "edit_file".to_string(), "首次写入".to_string());
        ctx.change_verification = Some(ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec!["output_id: check-first".to_string()],
            waived_reason: None,
        });
        ctx.record_side_effect(2, "edit_file".to_string(), "后续写入".to_string());
        assert!(ctx.change_verification.is_none());
        assert_eq!(ctx.side_effects.len(), 2);
    }

    #[test]
    fn test_is_only_ui_or_doc_side_effects() {
        let mut ctx = SandwichContext::new("sys".to_string(), "修改界面".to_string());
        assert!(!ctx.is_only_ui_or_doc_side_effects());

        // 仅包含静态 UI 和文档修改
        ctx.record_side_effect_with_paths(
            1,
            "edit_file".to_string(),
            "修改卡片样式".to_string(),
            vec!["src/components/AgentDeck.css".to_string()],
        );
        ctx.record_side_effect_with_paths(
            2,
            "write_file".to_string(),
            "更新说明文档".to_string(),
            vec!["README.md".to_string()],
        );
        assert!(ctx.is_only_ui_or_doc_side_effects());

        // 混入了含有脚本逻辑的组件文件（.vue 不能被纯静态豁免）
        ctx.record_side_effect_with_paths(
            3,
            "edit_file".to_string(),
            "修改组件代码".to_string(),
            vec!["src/components/AgentDeck.vue".to_string()],
        );
        assert!(!ctx.is_only_ui_or_doc_side_effects());
    }
