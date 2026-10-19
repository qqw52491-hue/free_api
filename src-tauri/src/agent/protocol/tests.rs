    use super::*;

    #[test]
    fn test_dispatch_envelope_defaults_and_roundtrip() {
        let envelope = DispatchEnvelope {
            task_id: "task-test-01".to_string(),
            attempt_id: 2,
            role_id: "child_reviewer".to_string(),
            contract_id: "reviewer.v1".to_string(),
            parent_run_id: "parent-r1".to_string(),
            parent_session_id: "parent-s1".to_string(),
            model_id: "gemini-2.5-pro".to_string(),
            model_tier: "pro".to_string(),
            reasoning_effort: "high".to_string(),
            persona: "strict_auditor".to_string(),
            timeout_secs: 120,
            input_payload: serde_json::json!({"question": "security audit"}),
            custom_schema: ContractSchema::default(),
            focus_domains: vec![FocusDomain::GitDiff],
            specialized_context: None,
            purpose: None,
            batch_id: None,
        };

        let key = envelope.attempt_key();
        assert_eq!(key.task_id, "task-test-01");
        assert_eq!(key.attempt_id, 2);

        let json_str = serde_json::to_string(&envelope).unwrap();
        let recovered: DispatchEnvelope = serde_json::from_str(&json_str).unwrap();
        assert_eq!(recovered, envelope);
    }

    #[test]
    fn test_delivery_receipt_infra_failure_detection() {
        let receipt = DeliveryReceipt {
            child_run_id: "child-1".to_string(),
            task_id: "task-1".to_string(),
            attempt_id: 1,
            kind: "review".to_string(),
            status: "timed_out".to_string(),
            duration_ms: 60000,
            completion_tokens: 0,
            prompt_tokens: 100,
            error_kind: Some("timed_out".to_string()),
            payload: None,
            raw_text: None,
        };

        assert!(receipt.is_infra_failure());
        assert!(!receipt.is_success());
    }

    #[test]
    fn test_delivery_receipt_corrupted_raw_text_preservation() {
        let receipt = DeliveryReceipt {
            child_run_id: "child-corrupt".to_string(),
            task_id: "task-corrupt".to_string(),
            attempt_id: 1,
            kind: "review".to_string(),
            status: "invalid_output".to_string(),
            duration_ms: 1500,
            completion_tokens: 20,
            prompt_tokens: 50,
            error_kind: Some("invalid_output".to_string()),
            payload: None,
            raw_text: Some(r#"{"verdict": "pass", broken..."#.to_string()),
        };

        assert!(receipt.is_infra_failure());
        assert_eq!(receipt.raw_text.as_deref(), Some(r#"{"verdict": "pass", broken..."#));
        assert!(receipt.as_reviewer_delivery().is_none());
    }

    #[test]
    fn test_delivery_receipt_delegators_and_from_conversion() {
        use crate::agent::child_reviewer::{ChildReviewResult, ObjectionVerdict, ReviewFinding, ReviewScorecard};

        let res = ChildReviewResult {
            child_run_id: "c-1".to_string(),
            status: "succeeded".to_string(),
            findings: vec![
                ReviewFinding {
                    id: "F1".to_string(),
                    severity: "blocking".to_string(),
                    finality: "regardless".to_string(),
                    location: "src/lib.rs:10".to_string(),
                    current: "old".to_string(),
                    why: "risk".to_string(),
                    suggestion: "fix".to_string(),
                    evidence_strength: "confirmed".to_string(),
                },
                ReviewFinding {
                    id: "F2".to_string(),
                    severity: "blocking".to_string(),
                    finality: "final_only".to_string(),
                    location: "src/main.rs:20".to_string(),
                    current: "todo".to_string(),
                    why: "clean up".to_string(),
                    suggestion: "clean".to_string(),
                    evidence_strength: "confirmed".to_string(),
                },
            ],
            confidence: "high".to_string(),
            evidence_refs: vec!["ref1".to_string()],
            missing_evidence: vec!["missing1".to_string()],
            duration_ms: 1200,
            completion_tokens: 300,
            prompt_tokens: 500,
            error_kind: None,
            raw_result_json: None,
            kind: Some("review".to_string()),
            routing: Some("certain".to_string()),
            withdrawn_findings: vec!["F1".to_string()],
            objection_verdicts: vec![
                ObjectionVerdict {
                    finding_id: "F1".to_string(),
                    verdict: "withdraw".to_string(),
                    reason: "resolved".to_string(),
                }
            ],
            scorecard: Some(ReviewScorecard::new(80, 70, 85, Some(90))),
            task_id: None,
            attempt_id: None,
        };

        let receipt: DeliveryReceipt = res.into();
        assert_eq!(receipt.child_run_id, "c-1");
        assert_eq!(receipt.kind, "review");
        assert_eq!(receipt.status, "succeeded");
        assert_eq!(receipt.duration_ms, 1200);
        assert_eq!(receipt.prompt_tokens, 500);
        assert_eq!(receipt.completion_tokens, 300);
        assert!(receipt.is_success());
        assert!(!receipt.is_infra_failure());

        // Domain delegators
        assert_eq!(receipt.confidence(), "high");
        assert_eq!(receipt.routing().as_deref(), Some("certain"));
        assert_eq!(receipt.missing_evidence(), vec!["missing1"]);
        assert_eq!(receipt.findings().len(), 2);
        assert_eq!(receipt.unwithdrawn_blocking_count(), 1); // F2 is unwithdrawn
        assert_eq!(receipt.stage_blocking_open(), 0); // F1 withdrawn, F2 is final_only
        assert_eq!(receipt.stage_final_only_lines().len(), 1);
        let (added, withdrawn, maintained) = receipt.objection_stats();
        assert_eq!(withdrawn, 1);
        assert_eq!(maintained, 0);
        assert_eq!(added, 1); // F2 is new blocking
        assert_eq!(receipt.scorecard().unwrap().correctness, 80);
    }

    #[test]
    fn test_dispatch_envelope_builders() {
        let rev = DispatchEnvelope::reviewer("parent-1", "session-1", "model-r", serde_json::json!({"q": 1}))
            .with_task_id("rev_1")
            .with_tier("pro")
            .with_effort("high")
            .with_persona("auditor")
            .with_attempt_id(2)
            .with_timeout_secs(90);

        assert_eq!(rev.task_id, "rev_1");
        assert_eq!(rev.role_id, "child_reviewer");
        assert_eq!(rev.contract_id, "reviewer.v1");
        assert_eq!(rev.parent_run_id, "parent-1");
        assert_eq!(rev.parent_session_id, "session-1");
        assert_eq!(rev.model_id, "model-r");
        assert_eq!(rev.model_tier, "pro");
        assert_eq!(rev.reasoning_effort, "high");
        assert_eq!(rev.persona, "auditor");
        assert_eq!(rev.attempt_id, 2);
        assert_eq!(rev.timeout_secs, 90);
        assert_eq!(rev.attempt_key().task_id, "rev_1");
        assert_eq!(rev.attempt_key().attempt_id, 2);

        let wrt = DispatchEnvelope::writer("parent-2", "session-2", "model-w", serde_json::json!({"task": "fix"}))
            .with_task_id("wrt_1");
        assert_eq!(wrt.role_id, "child_writer");
        assert_eq!(wrt.contract_id, "writer.v1");
        assert_eq!(wrt.timeout_secs, 180);

        let wrk = DispatchEnvelope::worker("parent-3", "session-3", "model-s", serde_json::json!({"task": "scout"}))
            .with_task_id("wrk_1");
        assert_eq!(wrk.role_id, "child_worker");
        assert_eq!(wrk.contract_id, "worker.v1");
        assert_eq!(wrk.timeout_secs, 60);
    }

    #[test]
    fn test_baseline_reserved_fields_completeness() {
        use crate::agent::child_reviewer::{ChildReviewResult, ReviewerDeliveryData};
        use crate::agent::child_writer::WriterResult;
        use crate::agent::child_worker::WorkerResult;

        // 1. 检查 ReviewerDeliveryData 字段全部被黑名单覆盖
        let rev_delivery = ReviewerDeliveryData {
            findings: vec![],
            confidence: "confirmed".to_string(),
            evidence_refs: vec![],
            missing_evidence: vec![],
            routing: None,
            withdrawn_findings: vec![],
            objection_verdicts: vec![],
            scorecard: None,
            extra: Default::default(),
        };
        let val = serde_json::to_value(&rev_delivery).unwrap();
        for key in val.as_object().unwrap().keys() {
            assert!(
                is_reserved_field_name(key),
                "ReviewerDeliveryData 字段 '{}' 未在 BASELINE_RESERVED_FIELDS 中登记，请同步更新保留字黑名单！",
                key
            );
        }

        // 2. 检查 ChildReviewResult 字段
        let child_res = ChildReviewResult {
            child_run_id: "c".to_string(),
            status: "s".to_string(),
            findings: vec![],
            confidence: "c".to_string(),
            evidence_refs: vec![],
            missing_evidence: vec![],
            duration_ms: 0,
            completion_tokens: 0,
            prompt_tokens: 0,
            error_kind: None,
            raw_result_json: None,
            kind: None,
            routing: None,
            withdrawn_findings: vec![],
            objection_verdicts: vec![],
            scorecard: None,
            task_id: None,
            attempt_id: None,
        };
        let val = serde_json::to_value(&child_res).unwrap();
        for key in val.as_object().unwrap().keys() {
            assert!(
                is_reserved_field_name(key),
                "ChildReviewResult 字段 '{}' 未在 BASELINE_RESERVED_FIELDS 中登记，请同步更新保留字黑名单！",
                key
            );
        }

        // 3. 检查 WriterResult 字段
        let writer_res = WriterResult {
            child_run_id: "w".to_string(),
            status: "s".to_string(),
            findings: vec![],
            confidence: "c".to_string(),
            evidence_refs: vec![],
            missing_evidence: vec![],
            duration_ms: 0,
            completion_tokens: 0,
            prompt_tokens: 0,
            error_kind: None,
            steps_used: 0,
            changed_files: vec![],
            test_command: "".to_string(),
            test_status: "".to_string(),
            test_output_tail: "".to_string(),
            workspace_copy: "".to_string(),
            patch_ref: None,
        };
        let val = serde_json::to_value(&writer_res).unwrap();
        for key in val.as_object().unwrap().keys() {
            assert!(
                is_reserved_field_name(key),
                "WriterResult 字段 '{}' 未在 BASELINE_RESERVED_FIELDS 中登记，请同步更新保留字黑名单！",
                key
            );
        }

        // 4. 检查 WorkerResult 字段
        let worker_res = WorkerResult {
            child_run_id: "w".to_string(),
            status: "s".to_string(),
            findings: vec![],
            confidence: "c".to_string(),
            evidence_refs: vec![],
            missing_evidence: vec![],
            duration_ms: 0,
            completion_tokens: 0,
            prompt_tokens: 0,
            error_kind: None,
            steps_used: 0,
            action_log: vec![],
        };
        let val = serde_json::to_value(&worker_res).unwrap();
        for key in val.as_object().unwrap().keys() {
            assert!(
                is_reserved_field_name(key),
                "WorkerResult 字段 '{}' 未在 BASELINE_RESERVED_FIELDS 中登记，请同步更新保留字黑名单！",
                key
            );
        }

        // 5. 检查 DeliveryReceipt 字段
        let receipt = DeliveryReceipt {
            child_run_id: "".to_string(),
            task_id: "".to_string(),
            attempt_id: 1,
            kind: "".to_string(),
            status: "".to_string(),
            duration_ms: 0,
            completion_tokens: 0,
            prompt_tokens: 0,
            error_kind: None,
            payload: None,
            raw_text: None,
        };
        let val = serde_json::to_value(&receipt).unwrap();
        for key in val.as_object().unwrap().keys() {
            assert!(
                is_reserved_field_name(key),
                "DeliveryReceipt 字段 '{}' 未在 BASELINE_RESERVED_FIELDS 中登记，请同步更新保留字黑名单！",
                key
            );
        }

        // 6. 检查 DispatchEnvelope 字段
        let env = DispatchEnvelope::reviewer("p", "s", "m", Value::Null);
        let val = serde_json::to_value(&env).unwrap();
        for key in val.as_object().unwrap().keys() {
            assert!(
                is_reserved_field_name(key),
                "DispatchEnvelope 字段 '{}' 未在 BASELINE_RESERVED_FIELDS 中登记，请同步更新保留字黑名单！",
                key
            );
        }
    }

    #[test]
    fn test_custom_field_name_validation() {
        // 命中保留字必须拒绝
        assert!(validate_custom_field_name("findings").is_err());
        assert!(validate_custom_field_name("confidence").is_err());
        assert!(validate_custom_field_name("status").is_err());
        assert!(validate_custom_field_name("changed_files").is_err());

        // 语法不合规必须拒绝
        assert!(validate_custom_field_name("").is_err());
        assert!(validate_custom_field_name("a").is_err()); // 长度 < 2
        assert!(validate_custom_field_name("1cve").is_err()); // 非小写字母开头
        assert!(validate_custom_field_name("cve-refs").is_err()); // 含连字符
        assert!(validate_custom_field_name("CveRefs").is_err()); // 驼峰大写

        // 合法 snake_case 自定义字段必须通过
        assert!(validate_custom_field_name("cve_refs").is_ok());
        assert!(validate_custom_field_name("risk_level").is_ok());
        assert!(validate_custom_field_name("cvss_score").is_ok());
    }

    #[test]
    fn test_contract_field_def_type_validation() {
        // 1. string 标量及枚举值域校验
        let str_field = ContractFieldDef::must_new("risk_level", ContractFieldType::String, "风险等级", true)
            .with_allowed_values(vec!["critical".into(), "high".into(), "low".into()]);

        assert!(str_field.validate_value(Some(&Value::String("high".into()))).is_ok());
        assert!(str_field.validate_value(Some(&Value::String("unknown".into()))).is_err());
        assert!(str_field.validate_value(Some(&Value::Number(100.into()))).is_err());
        assert!(str_field.validate_value(None).is_err()); // 必填缺失

        // 2. string[] 字符串数组校验
        let arr_field = ContractFieldDef::must_new("cve_refs", ContractFieldType::StringArray, "CVE列表", true);
        assert!(arr_field.validate_value(Some(&serde_json::json!(["CVE-2023-1", "CVE-2023-2"]))).is_ok());
        assert!(arr_field.validate_value(Some(&serde_json::json!([]))).is_ok());
        assert!(arr_field.validate_value(Some(&serde_json::json!(["CVE-1", 123]))).is_err()); // 元素类型错
        assert!(arr_field.validate_value(Some(&serde_json::json!("not_array"))).is_err());

        // 3. 选填字段校验（None 或 Null 允许通过）
        let opt_field = ContractFieldDef::must_new("extra_notes", ContractFieldType::String, "补充说明", false);
        assert!(opt_field.validate_value(None).is_ok());
        assert!(opt_field.validate_value(Some(&Value::Null)).is_ok());
        assert!(opt_field.validate_value(Some(&Value::String("ok".into()))).is_ok());
    }

    #[test]
    fn test_contract_schema_closed_set_and_shape_validation() {
        let schema = ContractSchema::new(vec![
            ContractFieldDef::must_new("cve_refs", ContractFieldType::StringArray, "CVE列表", true),
            ContractFieldDef::must_new("risk_level", ContractFieldType::String, "风险级别", true)
                .with_allowed_values(vec!["critical".into(), "high".into(), "low".into()]),
        ]);

        // 1. 合规输出必须通过
        let valid_payload = serde_json::json!({
            "findings": [],
            "confidence": "confirmed",
            "cve_refs": ["CVE-2024-0001"],
            "risk_level": "high"
        });
        assert!(schema.validate_shape(&valid_payload).is_ok());

        // 2. 模型拼错字段名 (如 cve_ids) 或额外注入垃圾键 → 闭集校验拦截
        let typo_payload = serde_json::json!({
            "findings": [],
            "confidence": "confirmed",
            "cve_ids": ["CVE-2024-0001"], // 拼错字段名！
            "risk_level": "high"
        });
        let err = schema.validate_shape(&typo_payload).unwrap_err();
        assert!(err.contains("闭集校验失败"));
        assert!(err.contains("cve_ids"));

        // 3. 缺少必填项校验
        let missing_payload = serde_json::json!({
            "findings": [],
            "confidence": "confirmed",
            "cve_refs": ["CVE-2024-0001"]
            // 缺少 risk_level
        });
        assert!(schema.validate_shape(&missing_payload).unwrap_err().contains("缺少必填字段: 'risk_level'"));

        // 4. sanitize_extra 清洗测试：仅收纳白名单字段，剔除未声明字段
        let mixed_payload = serde_json::json!({
            "cve_refs": ["CVE-2024-0001"],
            "risk_level": "high",
            "hallucinated_garbage": "some junk",
            "unregistered_flag": true
        });
        let clean = schema.sanitize_extra(&mixed_payload);
        assert_eq!(clean.len(), 2);
        assert!(clean.contains_key("cve_refs"));
        assert!(clean.contains_key("risk_level"));
        assert!(!clean.contains_key("hallucinated_garbage"));
    }

    #[test]
    fn test_contract_schema_prompt_instruction_rendering() {
        let schema = ContractSchema::new(vec![
            ContractFieldDef::must_new("cve_refs", ContractFieldType::StringArray, "关联CVE编号列表", true),
            ContractFieldDef::must_new("risk_level", ContractFieldType::String, "安全风险等级", true)
                .with_allowed_values(vec!["critical".into(), "high".into(), "low".into()]),
            ContractFieldDef::must_new("compliance_notes", ContractFieldType::String, "合规备注", false),
        ]);

        let prompt = schema.render_prompt_instructions();
        assert!(prompt.contains("【输出扩展契约规范】"));
        assert!(prompt.contains("`cve_refs` (string[], 必填): 关联CVE编号列表"));
        assert!(prompt.contains("`risk_level` (string, 必填，可选值: [critical, high, low]): 安全风险等级"));
        assert!(prompt.contains("`compliance_notes` (string, 选填): 合规备注"));
    }

    #[test]
    fn test_dispatch_envelope_validate_delivery_receipt_guard() {
        let envelope = DispatchEnvelope::reviewer("p1", "s1", "m1", Value::Null)
            .with_custom_field(ContractFieldDef::must_new("risk_level", ContractFieldType::String, "风险等级", true))
            .unwrap();

        // 1. 成功交付且字段齐全 → 校验通过
        let mut good_receipt = DeliveryReceipt {
            child_run_id: "r1".to_string(),
            task_id: "t1".to_string(),
            attempt_id: 1,
            kind: "review".to_string(),
            status: "succeeded".to_string(),
            duration_ms: 100,
            completion_tokens: 50,
            prompt_tokens: 50,
            error_kind: None,
            payload: Some(serde_json::json!({
                "findings": [],
                "confidence": "confirmed",
                "risk_level": "critical"
            })),
            raw_text: None,
        };
        assert!(envelope.validate_delivery_receipt(&mut good_receipt).is_ok());
        assert_eq!(good_receipt.status, "succeeded");

        // 2. 交付物缺少自定义必填字段 → 被信封卫士拦截并打标为 invalid_output
        let mut bad_receipt = DeliveryReceipt {
            child_run_id: "r2".to_string(),
            task_id: "t2".to_string(),
            attempt_id: 1,
            kind: "review".to_string(),
            status: "succeeded".to_string(),
            duration_ms: 100,
            completion_tokens: 50,
            prompt_tokens: 50,
            error_kind: None,
            payload: Some(serde_json::json!({
                "findings": [],
                "confidence": "confirmed"
                // 缺失 risk_level
            })),
            raw_text: None,
        };
        let val_err = envelope.validate_delivery_receipt(&mut bad_receipt).unwrap_err();
        assert!(val_err.contains("缺少必填字段: 'risk_level'"));
        assert_eq!(bad_receipt.status, "invalid_output");
        assert!(bad_receipt.error_kind.unwrap().contains("contract_violation"));

        // 3. 提取审查交付产物时，扩展字段安全注入 extra 桶中
        let delivery_data = good_receipt.as_reviewer_delivery().expect("应成功提取");
        assert_eq!(delivery_data.get_extra_str("risk_level"), Some("critical"));
    }
