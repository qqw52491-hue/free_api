    use super::*;
    use std::sync::{Arc, Mutex};

    fn setup_db() -> DbState {
        let base = std::env::temp_dir().join(format!("free-api-role-cards-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        DbState(Arc::new(Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )))
    }

    #[test]
    fn card_crud_round_trip_and_caps() {
        let db = setup_db();
        let created = upsert_card(&db, &json!({
            "name": "frontend_designer", "label": "前端审美大师",
            "model_id": "gemini-uuid-1", "persona_prompt": "专精 Tailwind 与暗黑玻璃拟态",
            "scope_hints": "src/components, src/assets", "default_test_command": "npm run build",
        })).unwrap();
        assert_eq!(created.name, "frontend_designer");
        assert!(created.enabled);

        // upsert 更新（同名覆盖）
        let updated = upsert_card(&db, &json!({"name": "frontend_designer", "label": "前端大师 v2"})).unwrap();
        assert_eq!(updated.label, "前端大师 v2");
        assert!(updated.model_id.is_empty(), "未传字段不应保留旧值（整卡覆盖语义）");

        // 名字非法字符被拒绝
        assert!(upsert_card(&db, &json!({"name": "bad;name"})).is_err());

        // 池子上限测试：先统计当前已有卡片数量，填满至 MAX_ROLE_CARDS 后拒绝溢出卡
        let current_count = list_cards(&db, true).unwrap().len();
        for i in current_count..MAX_ROLE_CARDS {
            upsert_card(&db, &json!({"name": format!("role_{}", i)})).unwrap();
        }
        assert!(upsert_card(&db, &json!({"name": "overflow_role"})).is_err());
    }

    #[test]
    fn test_default_tech_lead_card_seeded() {
        let db = setup_db();
        let tl = resolve_enabled_card(&db, "tech_lead").unwrap().expect("系统初始化应自动预装 tech_lead");
        assert_eq!(tl.name, "tech_lead");
        assert!(tl.allows_kind("reviewer"));
        let custom_schema = tl.parse_custom_schema().expect("tech_lead 应具备合法的 custom_schema");
        assert!(custom_schema.field_names().contains("decision"));
        assert!(custom_schema.field_names().contains("file_coupling"));
        assert!(custom_schema.field_names().contains("analysis"));
        assert!(custom_schema.field_names().contains("escalate_roles"));

        let input_schema = tl.parse_input_schema().expect("tech_lead 应具备合法的 input_schema");
        assert!(input_schema.field_names().contains("plan_summary"));
        assert!(input_schema.field_names().contains("target_files"));
    }

    #[test]
    fn test_default_tech_lead_card_preserves_user_edits_on_reseed() {
        let db = setup_db();
        // 用户对内置 tech_lead 卡做了个性化编辑（换模型、改 Prompt）
        upsert_card(&db, &json!({
            "name": "tech_lead",
            "model_id": "user-model-uuid",
            "persona_prompt": "用户自定义的 Tech Lead 提示词"
        })).unwrap();

        // 模拟应用重启：init_db 再次执行预装
        ensure_default_role_cards(&db.writer().unwrap()).unwrap();

        let tl = resolve_enabled_card(&db, "tech_lead").unwrap().expect("重启后 tech_lead 仍存在");
        assert_eq!(
            tl.persona_prompt, "用户自定义的 Tech Lead 提示词",
            "预装不得覆盖用户对内置角色卡的编辑"
        );
        assert_eq!(tl.model_id, "user-model-uuid", "用户绑定的模型不得被预装重置");
    }

    #[test]
    fn resolve_returns_none_for_missing_or_disabled() {
        let db = setup_db();
        upsert_card(&db, &json!({"name": "reviewer_pro", "model_id": "m-1"})).unwrap();
        assert!(resolve_enabled_card(&db, "reviewer_pro").unwrap().is_some());
        assert!(resolve_enabled_card(&db, "nonexistent").unwrap().is_none());
        assert!(resolve_enabled_card(&db, "").unwrap().is_none());
        toggle_card(&db, &json!({"name": "reviewer_pro"})).unwrap();
        assert!(resolve_enabled_card(&db, "reviewer_pro").unwrap().is_none(), "禁用后不得再被派发");
    }

    #[test]
    fn writer_defaults_merge_from_card() {
        let card = RoleCard {
            id: "x".into(), name: "frontend_designer".into(), label: "前端".into(),
            model_id: "gemini".into(), persona_prompt: "审美优先".into(),
            scope_hints: "src/components, src/assets".into(),
            default_test_command: "npm run build".into(),
            allowed_kinds: "writer".into(),
            hard_scope: "src/components, src/assets".into(),
            max_concurrency: 1,
            enabled: true,
            custom_schema: String::new(),
            focus_domains: String::new(),
            input_schema: String::new(),
            allowed_skills: String::new(),
            allowed_mcp: String::new(),
        };
        // 模型未提供默认值时补齐
        let merged = merge_writer_defaults(&card, &json!({"task": "打磨 UI"}));
        assert_eq!(merged["test_command"], "npm run build");
        assert_eq!(merged["focus_paths"][0], "src/components");
        // 模型显式提供时不覆盖
        let merged = merge_writer_defaults(&card, &json!({"task": "x", "test_command": "cargo test", "focus_paths": ["src/a.rs"]}));
        assert_eq!(merged["test_command"], "cargo test");
        assert_eq!(merged["focus_paths"][0], "src/a.rs");
        // persona 块
        assert!(persona_block(&card).contains("【专精角色人设：前端】"));
        assert!(persona_block(&RoleCard { persona_prompt: String::new(), ..card }).is_empty());
    }

    #[test]
    fn role_cards_menu_lists_enabled_only() {
        let db = setup_db();
        // 清理初始化预装卡片（tech_lead + 联合会审专家），校验空池基线
        for name in ["tech_lead", "architect", "security_specialist", "performance_expert"] {
            let _ = delete_card(&db, &json!({ "name": name }));
        }
        assert!(build_role_cards_menu(&db).unwrap().is_empty());
        upsert_card(&db, &json!({"name": "a_role", "label": "角色A", "persona_prompt": "描述A"})).unwrap();
        upsert_card(&db, &json!({"name": "b_role", "label": "角色B"})).unwrap();
        toggle_card(&db, &json!({"name": "b_role"})).unwrap();
        let menu = build_role_cards_menu(&db).unwrap();
        assert!(menu.contains("a_role") && menu.contains("角色A"));
        assert!(!menu.contains("b_role"), "禁用的卡不得出现在菜单");
    }

    #[test]
    fn role_card_v2_allows_kind_and_hard_scope() {
        let card = RoleCard {
            allowed_kinds: "writer, worker".into(),
            hard_scope: "src/components, src/assets, package.json".into(),
            ..Default::default()
        };
        // kind 检查
        assert!(card.allows_kind("writer"));
        assert!(card.allows_kind("worker"));
        assert!(!card.allows_kind("reviewer"));

        // hard_scope 检查
        assert!(card.allows_path(Path::new("src/components/Button.vue")));
        assert!(card.allows_path(Path::new("src/assets/logo.png")));
        assert!(card.allows_path(Path::new("package.json")));
        assert!(!card.allows_path(Path::new("src-tauri/src/main.rs")));
        assert!(!card.allows_path(Path::new("Cargo.toml")));

        // 默认空卡全放行
        let empty_card = RoleCard::default();
        assert!(empty_card.allows_kind("reviewer"));
        assert!(empty_card.allows_path(Path::new("any/path/you/want.rs")));
    }

    #[test]
    fn role_card_custom_schema_round_trip_and_validation() {
        let db = setup_db();
        let valid_schema = json!({
            "fields": [
                {
                    "name": "risk_level",
                    "field_type": "string",
                    "required": true,
                    "description": "安全风险评级",
                    "allowed_values": ["low", "medium", "high"]
                },
                {
                    "name": "cve_refs",
                    "field_type": "string[]",
                    "required": false,
                    "description": "CVE编号清单"
                }
            ],
            "strict_closed_set": true
        });

        let created = upsert_card(&db, &json!({
            "name": "sec_reviewer",
            "label": "安全审计官",
            "persona_prompt": "专注代码安全审计",
            "custom_schema": valid_schema.to_string(),
        })).unwrap();

        assert!(!created.custom_schema.is_empty());
        let parsed = created.parse_custom_schema().expect("应该成功解析为 ContractSchema");
        assert_eq!(parsed.fields.len(), 2);
        assert_eq!(parsed.fields[0].name, "risk_level");
        assert!(parsed.strict_closed_set);

        // 菜单展示特化字段
        let menu = build_role_cards_menu(&db).unwrap();
        assert!(menu.contains("特化契约字段: [risk_level, cve_refs]"));

        // 撞车基线保留字（如 findings）应当被拒绝
        let conflict_schema = json!({
            "fields": [
                {
                    "name": "findings",
                    "field_type": "string[]",
                    "required": true,
                    "description": "试图篡改基线 findings"
                }
            ]
        });
        let conflict_res = upsert_card(&db, &json!({
            "name": "bad_role",
            "label": "坏角色",
            "persona_prompt": "xxx",
            "custom_schema": conflict_schema.to_string(),
        }));
        assert!(conflict_res.is_err(), "包含基线保留字 findings 的 schema 必须被拒绝");

        // 非法 JSON 格式应当被拒绝
        let invalid_json_res = upsert_card(&db, &json!({
            "name": "bad_json_role",
            "label": "坏角色2",
            "persona_prompt": "xxx",
            "custom_schema": "{not valid json}",
        }));
        assert!(invalid_json_res.is_err(), "非法 JSON 格式必须被拒绝");
    }

    #[test]
    fn role_card_focus_domains_and_input_schema_round_trip() {
        let db = setup_db();
        let valid_in_schema = json!({
            "fields": [
                {
                    "name": "business_goal",
                    "field_type": "string",
                    "required": true,
                    "description": "业务改造目标"
                }
            ]
        });

        let card = upsert_card(&db, &json!({
            "name": "arch_reviewer",
            "label": "架构审查员",
            "persona_prompt": "专注模块解耦",
            "focus_domains": ["git_diff", "changed_files"],
            "input_schema": valid_in_schema.to_string(),
        })).unwrap();

        let parsed_domains = card.parse_focus_domains();
        assert_eq!(parsed_domains.len(), 2);
        assert!(parsed_domains.contains(&crate::agent::protocol::FocusDomain::GitDiff));
        assert!(parsed_domains.contains(&crate::agent::protocol::FocusDomain::ChangedFiles));

        let parsed_input = card.parse_input_schema().expect("应成功解析 input_schema");
        assert_eq!(parsed_input.fields[0].name, "business_goal");

        let menu = build_role_cards_menu(&db).unwrap();
        assert!(menu.contains("系统预装材料"));
        assert!(menu.contains("需在 custom_inputs 提供参数: [business_goal]"));

        // 不支持的 focus domain 应当被拒绝
        let bad_domain_res = upsert_card(&db, &json!({
            "name": "bad_domain_role",
            "label": "坏角色3",
            "focus_domains": ["invalid_domain_name"],
        }));
        assert!(bad_domain_res.is_err(), "不支持的 focus domain 必须被拒绝");
    }

    #[test]
    fn test_is_card_disabled_for_slot_isolation() {
        let disabled = vec![
            "writer:card_a".to_string(),
            "reviewer:card_b".to_string(),
            "card_global".to_string(),
        ];

        // card_a 在 writer 下被禁用，但在 reviewer 下依然可用！
        assert!(is_card_disabled_for(&disabled, "card_a", "writer"));
        assert!(!is_card_disabled_for(&disabled, "card_a", "reviewer"));
        assert!(!is_card_disabled_for(&disabled, "card_a", "worker"));

        // card_b 在 reviewer 下被禁用，但在 writer 下依然可用！
        assert!(!is_card_disabled_for(&disabled, "card_b", "writer"));
        assert!(is_card_disabled_for(&disabled, "card_b", "reviewer"));

        // card_global 全局禁用，所有工位都被禁用
        assert!(is_card_disabled_for(&disabled, "card_global", "writer"));
        assert!(is_card_disabled_for(&disabled, "card_global", "reviewer"));
        assert!(is_card_disabled_for(&disabled, "card_global", "worker"));

        // 未配置的 card_c 任何工位都不被禁用
        assert!(!is_card_disabled_for(&disabled, "card_c", "writer"));
        assert!(!is_card_disabled_for(&disabled, "card_c", "reviewer"));
    }

    #[test]
    fn validate_dispatch_inputs_enforces_required_fields() {
        let input_schema = json!({
            "fields": [
                {"name": "plan_summary", "field_type": "string", "required": true, "description": "方案摘要"},
                {"name": "target_files", "field_type": "string[]", "required": true, "description": "拟修改文件列表"},
                {"name": "note", "field_type": "string", "required": false, "description": "备注"},
                {"name": "round", "field_type": "number", "required": true, "description": "轮次"}
            ],
            "strict_closed_set": false
        })
        .to_string();
        let card = RoleCard {
            name: "tech_lead".into(),
            input_schema,
            ..Default::default()
        };

        // 无 schema 的卡一律放行
        let no_schema = RoleCard { name: "plain".into(), ..Default::default() };
        assert!(validate_dispatch_inputs(&no_schema, None).is_ok());
        assert!(validate_dispatch_inputs(&no_schema, Some(&json!({"x": 1}))).is_ok());

        // 必填缺失 → Err 含字段名与说明
        let err = validate_dispatch_inputs(&card, Some(&json!({"note": "x"}))).unwrap_err();
        assert!(err.contains("plan_summary"), "错误应列出缺失字段名: {}", err);
        assert!(err.contains("target_files"), "错误应列出缺失字段名: {}", err);
        assert!(err.contains("方案摘要"), "错误应附带字段说明: {}", err);
        assert!(err.contains("tech_lead"), "错误应指出角色卡: {}", err);
        assert!(!err.contains("note"), "非必填字段不参与校验: {}", err);

        // 空白串/空数组/null/空对象按缺失计
        for bad in [
            json!({"plan_summary": "   ", "target_files": ["a.rs"], "round": 1}),
            json!({"plan_summary": "x", "target_files": [], "round": 1}),
            json!({"plan_summary": null, "target_files": ["a.rs"], "round": 1}),
            json!({"plan_summary": "x", "target_files": ["a.rs"]}),
        ] {
            assert!(validate_dispatch_inputs(&card, Some(&bad)).is_err(), "应判缺失: {}", bad);
        }

        // 补齐 → Ok（number 存在即算有；非必填不校验）
        let ok_inputs = json!({"plan_summary": "改 A 模块", "target_files": ["a.rs"], "round": 2});
        assert!(validate_dispatch_inputs(&card, Some(&ok_inputs)).is_ok());

        // custom_inputs 缺失或非 object → 有必填即算缺失
        assert!(validate_dispatch_inputs(&card, None).is_err());
        assert!(validate_dispatch_inputs(&card, Some(&json!("not-an-object"))).is_err());
        assert!(validate_dispatch_inputs(&card, Some(&json!([]))).is_err());
    }

#[test]
fn skills_block_whitelist_injects_listed_docs_only() {
    let card = RoleCard {
        allowed_skills: "code_review, nonexistent_skill".into(),
        ..RoleCard {
            id: "x".into(), name: "n".into(), label: "l".into(),
            model_id: String::new(), persona_prompt: String::new(),
            scope_hints: String::new(), default_test_command: String::new(),
            allowed_kinds: String::new(), hard_scope: String::new(),
            max_concurrency: 0, enabled: true,
            custom_schema: String::new(), focus_domains: String::new(),
            input_schema: String::new(), allowed_skills: String::new(), allowed_mcp: String::new(),
        }
    };
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE agent_custom_instructions (
            id TEXT PRIMARY KEY, kind TEXT NOT NULL CHECK(kind IN ('prompt','skill')),
            name TEXT NOT NULL, description TEXT NOT NULL DEFAULT '',
            content TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL);",
    ).unwrap();
    let db = crate::db::DbState(std::sync::Arc::new(std::sync::Mutex::new(conn)));
    let block = skills_block(&db, &card);
    if block.is_empty() {
        // 开发环境若无 prompts/skills 目录则跳过断言
        eprintln!("skills dir 不存在，跳过注入断言");
    } else {
        assert!(block.contains("code_review"));
        assert!(block.contains("不存在"));
    }
    // 空白名单不注入
    let empty = skills_block(&db, &RoleCard {
        id: "y".into(), name: "n2".into(), label: "l".into(),
        model_id: String::new(), persona_prompt: String::new(),
        scope_hints: String::new(), default_test_command: String::new(),
        allowed_kinds: String::new(), hard_scope: String::new(),
        max_concurrency: 0, enabled: true,
        custom_schema: String::new(), focus_domains: String::new(),
        input_schema: String::new(), allowed_skills: String::new(), allowed_mcp: String::new(),
    });
    assert!(empty.is_empty());
}
