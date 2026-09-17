    use super::ModelConfig;

    #[test]
    fn from_conn_self_heals_missing_platform_capability_column() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // 老库结构：platforms 没有 requires_alternating_roles 列
        conn.execute_batch(
            "CREATE TABLE platforms (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, base_url TEXT NOT NULL,
                api_key TEXT NOT NULL, created_at TEXT NOT NULL);
             CREATE TABLE models (
                id TEXT PRIMARY KEY, platform_id TEXT NOT NULL, name TEXT NOT NULL,
                max_tokens INTEGER, temperature REAL, context_window INTEGER, enabled INTEGER DEFAULT 1);
             INSERT INTO platforms VALUES ('p1', '火山订阅', 'https://ark.cn-beijing.volces.com/api/coding/v3', 'k', '2026');
             INSERT INTO models VALUES ('m1', 'p1', 'deepseek-v4-pro', 4096, 0.7, 200000, 1);",
        )
        .unwrap();

        // 老代码在这里会报 no such column；自愈后应成功且取默认值 -1
        let cfg = ModelConfig::from_conn(&conn, "m1").expect("缺列时应自愈补列并成功");
        assert_eq!(cfg.requires_alternating_roles, -1);

        // 列已真实补上：再次查询稳定成功
        let cfg2 = ModelConfig::from_conn(&conn, "m1").unwrap();
        assert_eq!(cfg2.model_name, "deepseek-v4-pro");
    }

    fn config(model_name: &str, base_url: &str) -> ModelConfig {
        ModelConfig {
            model_id: "test-model-id".to_string(),
            platform_name: "test-platform".to_string(),
            base_url: base_url.to_string(),
            api_key: "test".to_string(),
            model_name: model_name.to_string(),
            max_tokens: 512,
            temperature: 0.7,
            is_ollama: false,
            is_lmstudio: false,
            context_window: 128_000,
            provider_capabilities: "{}".to_string(),
            timeout_secs: 120,
            requires_alternating_roles: -1,
        }
    }

    #[test]
    fn configured_profile_injects_response_format_without_url_matching() {
        let body = with_capabilities(r#"{"response_format":{"type":"json_object"}}"#)
            .build_body(&[], "low");
        assert_eq!(body["response_format"]["type"], "json_object");
    }

    fn with_capabilities(profile: &str) -> ModelConfig {
        let mut config = config("configured-model", "https://example.invalid/v1");
        config.provider_capabilities = profile.to_string();
        config
    }

    #[test]
    fn configured_profile_maps_effort_and_removes_sampling_when_enabled() {
        let profile = r#"{
            "thinking": {
                "field": "thinking",
                "enabled_value": { "type": "enabled" },
                "disabled_value": { "type": "disabled" },
                "remove_sampling_when_enabled": true,
                "effort": {
                    "field": "reasoning_effort",
                    "map": { "auto": "low", "medium": "high", "high": "max" },
                    "default": "high"
                }
            }
        }"#;
        let auto = with_capabilities(profile).build_body(&[], "auto");
        assert_eq!(auto["thinking"]["type"], "enabled");
        assert_eq!(auto["reasoning_effort"], "low");
        assert!(auto.get("temperature").is_none());
        assert!(auto.get("top_p").is_none());

        let high = with_capabilities(profile).build_body(&[], "high");
        assert_eq!(high["reasoning_effort"], "max");

        let off = with_capabilities(profile).build_body(&[], "off");
        assert_eq!(off["thinking"]["type"], "disabled");
        assert!(off.get("reasoning_effort").is_none());
        assert!(off.get("temperature").is_some());
    }

    #[test]
    fn configured_profile_can_force_thinking_and_keep_ui_high_below_max() {
        let profile = r#"{
            "thinking": {
                "enabled_value": { "type": "enabled" },
                "force_enabled": true,
                "effort": {
                    "map": { "off": "low", "low": "low", "medium": "high", "high": "high", "max": "max", "auto": "high" }
                }
            }
        }"#;
        let high = with_capabilities(profile).build_body(&[], "high");
        assert_eq!(high["thinking"]["type"], "enabled");
        assert_eq!(high["reasoning_effort"], "high");

        let max = with_capabilities(profile).build_body(&[], "max");
        assert_eq!(max["reasoning_effort"], "max");

        let off = with_capabilities(profile).build_body(&[], "off");
        assert_eq!(off["thinking"]["type"], "enabled");
        assert_eq!(off["reasoning_effort"], "low");
    }

    #[test]
    fn unconfigured_models_do_not_receive_vendor_specific_fields() {
        let body = config("gpt-4o-mini", "https://api.openai.com/v1").build_body(&[], "max");
        assert!(body.get("thinking").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn transient_retry_backoff_is_exponential_but_bounded() {
        use super::transient_retry_backoff_ms;
        assert_eq!(transient_retry_backoff_ms(0), 1_000);
        assert_eq!(transient_retry_backoff_ms(3), 8_000);
        assert_eq!(transient_retry_backoff_ms(4), 15_000);
        assert_eq!(transient_retry_backoff_ms(19), 15_000);
    }
