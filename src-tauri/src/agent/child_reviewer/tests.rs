
    fn blocking_finding(id: &str) -> ReviewFinding {
        ReviewFinding {
            id: id.to_string(),
            severity: "blocking".to_string(),
            finality: "regardless".to_string(),
            location: "src/a.rs:1".to_string(),
            current: "c".to_string(),
            why: "w".to_string(),
            suggestion: "s".to_string(),
            evidence_strength: "confirmed".to_string(),
        }
    }

    fn verdict(id: &str, v: &str) -> ObjectionVerdict {
        ObjectionVerdict {
            finding_id: id.to_string(),
            verdict: v.to_string(),
            reason: "r".to_string(),
        }
    }

    #[test]
    fn resolve_withdrawn_ids_is_fail_closed() {
        let findings = vec![blocking_finding("F1"), blocking_finding("F2")];

        // 正常撤回
        let withdrawn = resolve_withdrawn_ids(&findings, &[verdict("F1", "withdraw")]);
        assert_eq!(withdrawn, vec!["F1".to_string()]);

        // maintain 不撤回
        assert!(resolve_withdrawn_ids(&findings, &[verdict("F1", "maintain")]).is_empty());

        // 对不存在的 id 撤回：无效，不放宽门禁
        assert!(resolve_withdrawn_ids(&findings, &[verdict("F9", "withdraw")]).is_empty());

        // 大小写宽容
        assert_eq!(
            resolve_withdrawn_ids(&findings, &[verdict("F2", "WITHDRAW")]),
            vec!["F2".to_string()]
        );
    }

    #[test]
    fn objection_stats_distinguishes_added_withdrawn_maintained() {
        let mk = |id: &str, sev: &str| ReviewFinding {
            id: id.to_string(),
            severity: sev.to_string(),
            finality: "regardless".to_string(),
            location: String::new(),
            current: String::new(),
            why: String::new(),
            suggestion: String::new(),
            evidence_strength: "confirmed".to_string(),
        };
        // 第二轮：F1 维持、F2 撤回（撤回项仍列在 findings 中，由 verdict 裁定）、F3 新增
        let result = ChildReviewResult {
            child_run_id: "c".into(),
            status: "succeeded".into(),
            findings: vec![mk("F1", "blocking"), mk("F2", "blocking"), mk("F3", "blocking")],
            confidence: "confirmed".into(),
            evidence_refs: vec![],
            missing_evidence: vec![],
            duration_ms: 0,
            completion_tokens: 0,
            prompt_tokens: 0,
            error_kind: None,
            raw_result_json: None,
            kind: None,
            routing: None,
            withdrawn_findings: vec!["F2".to_string()],
            objection_verdicts: vec![verdict("F1", "maintain"), verdict("F2", "withdraw")],
            scorecard: None,
            task_id: None,
            attempt_id: None,
        };
        let (added, withdrawn, maintained) = result.objection_stats();
        assert_eq!(added, 1, "F3 无裁决覆盖，计为新增");
        assert_eq!(withdrawn, 1, "F2 被裁决撤回");
        assert_eq!(maintained, 1, "F1 维持");

        // 膨胀场景：撤回2条旧异议 + 新增2条新异议（F4/F5 无裁决覆盖）
        let inflated = ChildReviewResult {
            findings: vec![
                mk("F1", "blocking"),
                mk("F2", "blocking"),
                mk("F3", "blocking"),
                mk("F4", "blocking"),
                mk("F5", "blocking"),
            ],
            objection_verdicts: vec![verdict("F1", "maintain"), verdict("F2", "withdraw"), verdict("F3", "withdraw")],
            withdrawn_findings: vec!["F2".to_string(), "F3".to_string()],
            ..result
        };
        let (added, withdrawn, maintained) = inflated.objection_stats();
        assert_eq!((added, withdrawn, maintained), (2, 2, 1), "撤回≈新增：膨胀信号可见");
    }

    #[test]
    fn parse_tolerates_fenced_and_prose_wrapped_json() {
        // 09-07 pro 实弹：审查员 33 次 invalid_output 多因大上下文下把 JSON 包进围栏
        // 或前后加说明文字。宽松提取后应正常解析；完全无 JSON 仍判 invalid。
        let json = r#"{"findings":[],"confidence":"confirmed","evidence_refs":[],"missing_evidence":[],"scorecard":{"correctness":90,"risk":85,"style":90,"test_coverage":85}}"#;
        let fenced = format!("```json\n{json}\n```");
        let (f, _, _, _, _, sc, _, _) = parse_controlled_output(&fenced, &[]).unwrap();
        assert!(f.is_empty());
        assert_eq!(sc.unwrap().correctness, 90);

        let prose = format!("好的，以下是我的审查结论：\n{json}\n请查收。");
        let (f2, _, _, _, _, _, _, _) = parse_controlled_output(&prose, &[]).unwrap();
        assert!(f2.is_empty());

        // 完全没有 JSON → 仍报错（真 invalid）
        assert!(parse_controlled_output("我觉得这段代码写得不错。", &[]).is_err());
    }

    #[test]
    fn finality_defaults_to_regardless_and_normalizes_aliases() {
        // 未携带 finality → regardless（fail-closed）
        let raw = r#"{"findings":[{"id":"F1","severity":"blocking","location":"src/a.rs:1","why":"x"}],"confidence":"supported"}"#;
        let (findings, _, _, _, _, _, _, _) = parse_controlled_output(raw, &[]).unwrap();
        assert_eq!(findings[0].finality, "regardless");

        // 别名归一化
        let raw2 = r#"{"findings":[{"id":"F1","severity":"blocking","location":"src/a.rs:1","why":"x","finality":"blocking_if_final"}],"confidence":"supported"}"#;
        let (findings2, _, _, _, _, _, _, _) = parse_controlled_output(raw2, &[]).unwrap();
        assert_eq!(findings2[0].finality, "final_only");

        // stage_blocking_open：final_only 不计入拦截判据
        let result = ChildReviewResult {
            child_run_id: "c".into(),
            status: "succeeded".into(),
            findings: vec![
                blocking_finding("F1"),
                ReviewFinding { finality: "final_only".to_string(), ..blocking_finding("F2") },
            ],
            confidence: "confirmed".into(),
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
        assert_eq!(result.stage_blocking_open(), 1);
        assert_eq!(result.stage_final_only_lines().len(), 1);
    }

    #[test]
    fn parse_rejects_eraser_withdrawals_with_empty_or_thin_reason() {
        let base = |verdict_reason: &str| format!(
            r#"{{"findings":[{{"id":"F1","severity":"blocking","location":"src/a.rs:1","why":"存在空指针解引用"}}],
                "confidence":"supported",
                "objection_verdicts":[{{"finding_id":"F1","verdict":"withdraw","reason":"{}"}}]}}"#,
            verdict_reason
        );
        // 空理由 → 按 maintain，不撤回
        let (findings, _, _, _, _, _, verdicts, withdrawn) =
            parse_controlled_output(&base(""), &[]).unwrap();
        assert_eq!(verdicts[0].verdict, "maintain");
        assert!(withdrawn.is_empty());
        assert_eq!(
            ChildReviewResult::unwithdrawn_blocking_count(&ChildReviewResult {
                child_run_id: "c".into(),
                status: "succeeded".into(),
                findings,
                confidence: "supported".into(),
                evidence_refs: vec![],
                missing_evidence: vec![],
                duration_ms: 0,
                completion_tokens: 0,
                prompt_tokens: 0,
                error_kind: None,
                raw_result_json: None,
                kind: None,
                routing: None,
                withdrawn_findings: withdrawn,
                objection_verdicts: verdicts,
                scorecard: None,
                task_id: None,
                attempt_id: None,
            }),
            1
        );
        // 模板化短理由（<12字）→ 同样按 maintain
        let (_, _, _, _, _, _, verdicts, withdrawn) =
            parse_controlled_output(&base("已修复"), &[]).unwrap();
        assert_eq!(verdicts[0].verdict, "maintain");
        assert!(withdrawn.is_empty());
        // 有实质内容的理由 → 认可 withdraw
        let (_, _, _, _, _, _, verdicts, withdrawn) =
            parse_controlled_output(&base("父Agent已在 a.rs:1 增加判空并补了回归测试，见处理说明第1条"), &[]).unwrap();
        assert_eq!(verdicts[0].verdict, "withdraw");
        assert_eq!(withdrawn, vec!["F1".to_string()]);
    }

    #[test]
    fn test_parse_child_review_rebuttal_output_preserves_withdrawn() {
        let json_raw = r#"{
            "findings": [],
            "confidence": "confirmed",
            "evidence_refs": [],
            "missing_evidence": [],
            "routing": "certain",
            "objection_verdicts": [
                {
                    "finding_id": "F1",
                    "verdict": "withdraw",
                    "reason": "父 Agent 提供的测试证据确实表明已修复该边界问题"
                }
            ]
        }"#;
        let (findings, _confidence, _refs, _missing, _routing, verdicts, withdrawn) =
            parse_child_review_rebuttal_output(json_raw).unwrap();
        assert!(findings.is_empty());
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].verdict, "withdraw");
        assert_eq!(withdrawn, vec!["F1".to_string()]);
    }

    #[test]
    fn unwithdrawn_blocking_count_excludes_withdrawn() {
        let mut result = ChildReviewResult {
            child_run_id: "c1".to_string(),
            status: "succeeded".to_string(),
            findings: vec![blocking_finding("F1"), blocking_finding("F2")],
            confidence: "confirmed".to_string(),
            evidence_refs: vec![],
            missing_evidence: vec![],
            duration_ms: 0,
            completion_tokens: 0,
            prompt_tokens: 0,
            error_kind: None,
            raw_result_json: None,
            kind: None,
            routing: None,
            withdrawn_findings: vec!["F1".to_string()],
            objection_verdicts: vec![verdict("F1", "withdraw")],
            scorecard: Some(ReviewScorecard::new(90, 80, 85, Some(70))),
            task_id: None,
            attempt_id: None,
        };
        // F1 已撤回、F2 仍 blocking：收敛判据为 1
        assert_eq!(result.unwithdrawn_blocking_count(), 1);
        result.withdrawn_findings.push("F2".to_string());
        assert_eq!(result.unwithdrawn_blocking_count(), 0);
    }

    #[test]
    fn review_scorecard_passes_gate_and_clamping() {
        // 1. 正常合格：均分 80，risk 80
        let sc = ReviewScorecard::new(80, 80, 80, Some(80));
        assert_eq!(sc.average_score(), 80);
        assert!(sc.passes_gate());
        assert!(sc.gate_rejection_reason().is_none());

        // 2. 压线放行：均分恰好 75，risk 恰好 60
        let sc_edge = ReviewScorecard::new(80, 60, 80, Some(80));
        assert_eq!(sc_edge.average_score(), 75);
        assert!(sc_edge.passes_gate());

        // 3. 破坏性风险 risk < 60 单项硬拦截（即便综合平均分高达 82 分）
        let sc_risk_fail = ReviewScorecard::new(95, 59, 90, Some(85));
        assert_eq!(sc_risk_fail.average_score(), 82);
        assert!(!sc_risk_fail.passes_gate(), "risk 低于 60 分必须自动拦截拒绝放行");
        let reason = sc_risk_fail.gate_rejection_reason().unwrap();
        assert!(reason.contains("破坏性风险/安全性得分未达 60 分安全阈值"));

        // 4. 综合平均分 < 75 硬拦截（即便 risk 单项高达 90 分）
        let sc_avg_fail = ReviewScorecard::new(60, 90, 70, Some(70));
        assert_eq!(sc_avg_fail.average_score(), 72);
        assert!(!sc_avg_fail.passes_gate(), "综合均分低于 75 分必须自动拦截拒绝放行");
        let reason2 = sc_avg_fail.gate_rejection_reason().unwrap();
        assert!(reason2.contains("综合平均分未达 75 分放行线"));

        // 5. 0~100 数值钳制 (clamping)
        let sc_clamped = ReviewScorecard::new(150, 200, 80, Some(70));
        assert_eq!(sc_clamped.correctness, 100);
        assert_eq!(sc_clamped.risk, 100);

        // 6. 动态分母与 N/A 测试覆盖（UI 微调场景）
        let sc_na = ReviewScorecard::new(90, 95, 95, None);
        assert_eq!(sc_na.average_score(), 93);
        assert!(sc_na.passes_gate());
        assert!(!sc_na.is_abnormally_low());

        // 7. 异常低分识别（放水防御）
        let sc_low = ReviewScorecard::new(50, 90, 80, None);
        assert!(sc_low.is_abnormally_low());
    }
    use super::*;
    use rusqlite::params;

    fn managed_resource_fixture() -> (std::path::PathBuf, DbState, String, String) {
        let base = std::env::temp_dir().join(format!("free-api-child-resource-{}", Uuid::new_v4()));
        let root = base.join("project");
        let resource_path = root.join("evidence.md");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&resource_path, "registered resource body ".repeat(10)).unwrap();

        let conn = crate::db::init_db(&base.join("test.db")).unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let project_id = Uuid::new_v4().to_string();
        let session_id = Uuid::new_v4().to_string();
        let resource_id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO agent_projects (id, name, root_path, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![project_id, "项目资源测试", root.canonicalize().unwrap().to_string_lossy(), now, now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chat_sessions (id, title, model_id, created_at, updated_at, message_count) VALUES (?1, ?2, ?3, ?4, ?5, 0)",
            params![session_id, "项目会话", "model-a", now, now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_project_sessions (project_id, session_id, created_at) VALUES (?1, ?2, ?3)",
            params![project_id, session_id, now],
        )
        .unwrap();
        let canonical_resource = resource_path.canonicalize().unwrap();
        conn.execute(
            "INSERT INTO agent_project_resources (resource_id, project_id, label, relative_path, canonical_path, kind, size_bytes, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                resource_id,
                project_id,
                "evidence.md",
                "evidence.md",
                canonical_resource.to_string_lossy(),
                "text",
                std::fs::metadata(&canonical_resource).unwrap().len() as i64,
                now,
            ],
        )
        .unwrap();
        (
            base,
            DbState(std::sync::Arc::new(std::sync::Mutex::new(conn))),
            session_id,
            resource_id,
        )
    }

    #[test]
    fn request_is_bounded_and_deduplicates_evidence_references() {
        let request = parse_child_review_request(&json!({
            "question": "是否存在能证伪当前主根因的证据？",
            "evidence_excerpt": "日志 A 表明刷新延迟。",
            "evidence_refs": ["out-2", "out-1", "out-2"],
        }))
        .unwrap();
        assert_eq!(request.evidence_refs, vec!["out-1", "out-2"]);
        assert!(parse_child_review_request(&json!({"question": "x", "evidence_refs": ["a", "b", "c", "d", "e", "f", "g"]})).is_err());
        assert!(parse_child_review_request(&json!({"question": "没有证据包时不得调用"})).is_err());
        // 模型把不用的字段显式传 null：视为未传，不应被误报为「必须是 JSON 对象」
        let with_nulls = parse_child_review_request(&json!({"question": "q", "evidence_refs": ["a.rs"], "evidence_excerpt": null, "resource_ids": null, "dispatches": null})).unwrap();
        assert_eq!(with_nulls.evidence_refs, vec!["a.rs".to_string()]);
        assert!(parse_child_review_request(&json!({"question": "q", "evidence_refs": {"x": 1}})).is_err());
    }

    #[test]
    fn question_multiline_is_flattened_not_rejected() {
        // 实测 27 次「控制字符」拒收全是模型写多行问题——换行压平成单空格继续走
        let request = parse_child_review_request(&json!({
            "question": "【方案审查】\n根因假设：\n1. persist 用错 seq\n2. 需核对消费端",
            "evidence_refs": ["out-1"],
        }))
        .unwrap();
        assert_eq!(
            request.question,
            "【方案审查】 根因假设： 1. persist 用错 seq 2. 需核对消费端"
        );
        // 路径类字段仍严格拒收控制符（防止混入隐蔽路径注入）
        assert!(parse_child_review_request(&json!({
            "question": "x",
            "evidence_refs": ["a\nb"],
        }))
        .is_err());
    }

    #[test]
    fn resource_ids_are_deduplicated_and_bounded() {
        let request = parse_child_review_request(&json!({
            "question": "检查已登记资源是否支持当前判断",
            "resource_ids": ["resource-2", "resource-1", "resource-2"],
        }))
        .unwrap();
        assert_eq!(request.resource_ids, vec!["resource-1", "resource-2"]);
        assert!(parse_child_review_request(&json!({
            "question": "x",
            "resource_ids": ["a", "b", "c"],
        }))
        .is_err());
    }

    #[test]
    fn managed_resource_evidence_is_session_scoped_and_not_persisted() {
        let (base, state, session_id, resource_id) = managed_resource_fixture();
        let request = parse_child_review_request(&json!({
            "question": "已登记资源是否支持当前判断？",
            "evidence_refs": ["parent-output"],
            "resource_ids": [resource_id],
        }))
        .unwrap();
        let (evidence, allowed_refs) =
            build_managed_resource_evidence(&state, &session_id, &request).unwrap();
        assert!(evidence.contains("registered resource body"));
        assert_eq!(allowed_refs.len(), 2);
        assert!(allowed_refs.contains(&"parent-output".to_string()));
        assert!(allowed_refs.contains(&resource_id));
        assert!(parse_controlled_output(
            &format!("{{\"findings\":[\"资源内容支持该判断\"],\"confidence\":\"supported\",\"evidence_refs\":[\"{}\"],\"missing_evidence\":[]}}", resource_id),
            &allowed_refs,
        )
        .is_ok());
        assert!(build_managed_resource_evidence(&state, "detached-session", &request)
            .unwrap_err()
            .contains("未关联项目"));

        let conn = state.0.lock().unwrap();
        let mut statement = conn.prepare("PRAGMA table_info(agent_child_runs)").unwrap();
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(!columns.iter().any(|column| {
            matches!(
                column.as_str(),
                "content" | "prompt" | "response" | "thinking" | "evidence_excerpt"
            )
        }));
        drop(statement);
        drop(conn);
        drop(state);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn managed_resource_metadata_and_content_stay_within_total_evidence_budget() {
        let (base, state, session_id, resource_id) = managed_resource_fixture();
        let request = parse_child_review_request(&json!({
            "question": "在近满证据预算下读取资源",
            "evidence_excerpt": "m".repeat(MAX_EVIDENCE_CHARS - 100),
            "resource_ids": [resource_id],
        }))
        .unwrap();
        let (evidence, _) = build_managed_resource_evidence(&state, &session_id, &request).unwrap();
        assert!(evidence.chars().count() <= MAX_EVIDENCE_CHARS);
        assert!(evidence.contains("（已按预算截断）"));
        drop(state);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn output_can_only_reference_parent_supplied_evidence() {
        let error = parse_controlled_output(
            r#"{"findings":["刷新延迟与观察相符"],"confidence":"supported","evidence_refs":["invented"],"missing_evidence":[]}"#,
            &["out-1".to_string()],
        )
        .unwrap_err();
        assert!(error.contains("显式传入"));
    }

    #[test]
    fn controlled_output_is_short_and_structured() {
        let (findings, confidence, evidence_refs, missing, routing, _, _, _) = parse_controlled_output(
            r#"{"findings":[{"id":"F1","severity":"blocking","location":"src/foo.rs:10","current":"旧逻辑","why":"缺少空检查","suggestion":"添加Option安全解包","evidence_strength":"confirmed"}],"confidence":"supported","evidence_refs":["out-1"],"missing_evidence":["需要复现后状态快照"]}"#,
            &["out-1".to_string()],
        )
        .unwrap();
        assert_eq!(confidence, "supported");
        assert_eq!(evidence_refs, vec!["out-1"]);
        assert_eq!(missing, vec!["需要复现后状态快照"]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "F1");
        assert_eq!(findings[0].severity, "blocking");
        assert_eq!(findings[0].location, "src/foo.rs:10");
        assert_eq!(routing, "needs_alignment");
    }

    #[test]
    fn unlocated_blocking_finding_is_automatically_downgraded_to_question() {
        let (findings, _, _, _, routing, _, _, _) = parse_controlled_output(
            r#"{"findings":[{"id":"F1","severity":"blocking","location":"","current":"","why":"感觉架构还要优化","suggestion":"","evidence_strength":"unverified"}],"confidence":"confirmed","evidence_refs":["out-1"],"missing_evidence":[]}"#,
            &["out-1".to_string()],
        )
        .unwrap();
        assert_eq!(findings[0].severity, "question");
        assert_eq!(routing, "certain");
    }

    #[test]
    fn text_only_findings_are_backward_compatible_and_parsed_as_questions() {
        let (findings, _, _, _, routing, _, _, _) = parse_controlled_output(
            r#"{"findings":["发现A: 异步流式正常"],"confidence":"confirmed","evidence_refs":["out-1"],"missing_evidence":[]}"#,
            &["out-1".to_string()],
        )
        .unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "F1");
        assert_eq!(findings[0].severity, "question");
        assert_eq!(findings[0].why, "发现A: 异步流式正常");
        assert_eq!(routing, "certain");
    }

    #[test]
    fn database_enforces_two_children_per_parent_run_and_keeps_only_metadata() {
        let base = std::env::temp_dir().join(format!("free-api-child-reviewer-{}", Uuid::new_v4()));
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));
        let first = begin_child_run(&state, "parent-run", "parent-session", &workspace, "model-a", 2, "review", None).unwrap();
        let second = begin_child_run(&state, "parent-run", "parent-session", &workspace, "model-a", 0, "review", None).unwrap();
        assert_ne!(first, second);
        assert!(begin_child_run(&state, "parent-run", "parent-session", &workspace, "model-a", 0, "review", None).is_err());

        finish_child_run(&state, &first, "succeeded", 12, 34, 0, None, None);
        let conn = state.0.lock().unwrap();
        let (status, tokens, refs, error_kind): (String, i64, i64, Option<String>) = conn
            .query_row(
                "SELECT status, completion_tokens, evidence_ref_count, error_kind FROM agent_child_runs WHERE id=?1",
                params![first],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(status, "succeeded");
        assert_eq!(tokens, 34);
        assert_eq!(refs, 2);
        assert!(error_kind.is_none());
        drop(conn);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn poll_completed_child_runs_consumes_result_json_and_prevents_duplicate_injection() {
        let base = std::env::temp_dir().join(format!("free-api-child-poll-{}", Uuid::new_v4()));
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));
        let run_id = begin_child_run(&state, "parent-run-poll", "session-poll", &workspace, "model-a", 1, "review", None).unwrap();
        let sample_result = ChildReviewResult {
            child_run_id: run_id.clone(),
            status: "succeeded".to_string(),
            findings: vec![ReviewFinding {
                id: "F1".to_string(),
                severity: "suggestion".to_string(),
                finality: "regardless".to_string(),
                location: "src/main.rs:1".to_string(),
                current: "".to_string(),
                why: "异步流式正常".to_string(),
                suggestion: "".to_string(),
                evidence_strength: "confirmed".to_string(),
            }],
            confidence: "confirmed".to_string(),
            evidence_refs: vec!["out-1".to_string()],
            missing_evidence: vec![],
            duration_ms: 150,
            completion_tokens: 45,
            prompt_tokens: 0,
            error_kind: None,
            raw_result_json: None,
            kind: None,
            routing: Some("certain".to_string()),
            withdrawn_findings: vec![],
        objection_verdicts: vec![],
            scorecard: None,
            task_id: None,
            attempt_id: None,
        };
        let sample_json = serde_json::to_string(&sample_result).unwrap();
        finish_child_run(&state, &run_id, "succeeded", 150, 45, 0, None, Some(&sample_json));

        // 第一次轮询：获取到结果
        let polled = poll_completed_child_runs(&state, "parent-run-poll").unwrap();
        assert_eq!(polled.len(), 1);
        assert_eq!(polled[0].child_run_id, run_id);
        assert_eq!(polled[0].confidence, "confirmed");

        // 第二次轮询：投递 ≠ 落账。applied_at 未写时回执必须重新投递
        // （崩溃恢复语义）；重复消费由「投递后同批处理 + 提交时 applied_at
        // 随 checkpoint 原子落账」保证，而不是靠 poll 即销毁。
        let polled_again = poll_completed_child_runs(&state, "parent-run-poll").unwrap();
        assert_eq!(polled_again.len(), 1, "applied_at 未落账的回执必须重投，否则崩溃即丢件");

        // 效果随 checkpoint 提交落账（applied_at）后，回执才真正退出信箱
        {
            let conn = state.0.lock().unwrap();
            conn.execute(
                "UPDATE agent_child_runs SET applied_at = '2024-01-02T00:00:00Z' WHERE id = ?1",
                rusqlite::params![run_id],
            )
            .unwrap();
        }
        let drained = poll_completed_child_runs(&state, "parent-run-poll").unwrap();
        assert_eq!(drained.len(), 0, "applied_at 落账后回执不得重复投递");

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn has_running_child_runs_detects_active_and_terminal_states() {
        let base = std::env::temp_dir().join(format!("free-api-child-active-{}", Uuid::new_v4()));
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));

        // 没有任务时返回 false
        assert!(!has_running_child_runs(&state, "parent-run-1"));

        // 启动一个 child run -> 处于 running 状态
        let child_1 = begin_child_run(&state, "parent-run-1", "session-1", &workspace, "model-a", 1, "review", None).unwrap();
        assert!(has_running_child_runs(&state, "parent-run-1"));
        // 其他 parent_run 不受影响
        assert!(!has_running_child_runs(&state, "parent-run-2"));

        // 完成 child run -> 返回 false
        finish_child_run(&state, &child_1, "succeeded", 100, 20, 0, None, None);
        assert!(!has_running_child_runs(&state, "parent-run-1"));

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn cancel_running_child_runs_cancels_only_running_tasks() {
        let base = std::env::temp_dir().join(format!("free-api-child-cancel-{}", Uuid::new_v4()));
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));

        let child_1 = begin_child_run(&state, "parent-run-c", "session-c", &workspace, "model-a", 0, "review", None).unwrap();
        let child_2 = begin_child_run(&state, "parent-run-c", "session-c", &workspace, "model-a", 0, "review", None).unwrap();

        // child_1 正常成功完成
        finish_child_run(&state, &child_1, "succeeded", 50, 10, 0, None, None);

        // 取消父任务下的所有 running 子任务
        let cancelled_count = cancel_running_child_runs(&state, "parent-run-c").unwrap();
        assert_eq!(cancelled_count, 1);

        // 验证 child_1 依然保持 succeeded
        let conn = state.0.lock().unwrap();
        let status_1: String = conn
            .query_row("SELECT status FROM agent_child_runs WHERE id = ?1", params![child_1], |r| r.get(0))
            .unwrap();
        assert_eq!(status_1, "succeeded");

        // 验证 child_2 变为 cancelled
        let (status_2, error_kind): (String, Option<String>) = conn
            .query_row("SELECT status, error_kind FROM agent_child_runs WHERE id = ?1", params![child_2], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(status_2, "cancelled");
        assert_eq!(error_kind.as_deref(), Some("cancelled"));

        // 此时已无 running 任务
        drop(conn);
        assert!(!has_running_child_runs(&state, "parent-run-c"));

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn multiple_parallel_child_runs_polling_handles_interleaved_completions() {
        let base = std::env::temp_dir().join(format!("free-api-child-interleaved-{}", Uuid::new_v4()));
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));

        let child_1 = begin_child_run(&state, "parent-run-multi", "session-m", &workspace, "model-a", 1, "review", None).unwrap();
        let child_2 = begin_child_run(&state, "parent-run-multi", "session-m", &workspace, "model-a", 1, "review", None).unwrap();

        // 初始无完成
        let polled_0 = poll_completed_child_runs(&state, "parent-run-multi").unwrap();
        assert_eq!(polled_0.len(), 0);

        // child_1 先完成
        let res_1 = ChildReviewResult {
            child_run_id: child_1.clone(),
            status: "succeeded".to_string(),
            findings: vec![ReviewFinding {
                id: "F1".to_string(),
                severity: "suggestion".to_string(),
                finality: "regardless".to_string(),
                location: "src/main.rs:1".to_string(),
                current: "".to_string(),
                why: "审查1通过".to_string(),
                suggestion: "".to_string(),
                evidence_strength: "confirmed".to_string(),
            }],
            confidence: "confirmed".to_string(),
            evidence_refs: vec!["ref-1".to_string()],
            missing_evidence: vec![],
            duration_ms: 200,
            completion_tokens: 30,
            prompt_tokens: 0,
            error_kind: None,
            raw_result_json: None,
            kind: None,
            routing: Some("certain".to_string()),
            withdrawn_findings: vec![],
        objection_verdicts: vec![],
            scorecard: None,
            task_id: None,
            attempt_id: None,
        };
        finish_child_run(&state, &child_1, "succeeded", 200, 30, 0, None, Some(&serde_json::to_string(&res_1).unwrap()));

        // 第一轮轮询：消费 child_1
        let polled_1 = poll_completed_child_runs(&state, "parent-run-multi").unwrap();
        assert_eq!(polled_1.len(), 1);
        assert_eq!(polled_1[0].child_run_id, child_1);
        // 模拟生产时序：投递 → 入账 → applied_at 随 checkpoint 提交落账
        {
            let conn = state.0.lock().unwrap();
            conn.execute(
                "UPDATE agent_child_runs SET applied_at = '2024-01-02T00:00:00Z' WHERE id = ?1",
                rusqlite::params![child_1],
            )
            .unwrap();
        }

        // child_2 后完成
        let res_2 = ChildReviewResult {
            child_run_id: child_2.clone(),
            status: "succeeded".to_string(),
            findings: vec![ReviewFinding {
                id: "F2".to_string(),
                severity: "suggestion".to_string(),
                finality: "regardless".to_string(),
                location: "src/main.rs:2".to_string(),
                current: "".to_string(),
                why: "审查2通过".to_string(),
                suggestion: "".to_string(),
                evidence_strength: "supported".to_string(),
            }],
            confidence: "supported".to_string(),
            evidence_refs: vec!["ref-2".to_string()],
            missing_evidence: vec![],
            duration_ms: 350,
            completion_tokens: 40,
            prompt_tokens: 0,
            error_kind: None,
            raw_result_json: None,
            kind: None,
            routing: Some("certain".to_string()),
            withdrawn_findings: vec![],
        objection_verdicts: vec![],
            scorecard: None,
            task_id: None,
            attempt_id: None,
        };
        finish_child_run(&state, &child_2, "succeeded", 350, 40, 0, None, Some(&serde_json::to_string(&res_2).unwrap()));

        // 第二轮轮询：消费 child_2，不重复投递已落账的 child_1
        let polled_2 = poll_completed_child_runs(&state, "parent-run-multi").unwrap();
        assert_eq!(polled_2.len(), 1);
        assert_eq!(polled_2[0].child_run_id, child_2);
        {
            let conn = state.0.lock().unwrap();
            conn.execute(
                "UPDATE agent_child_runs SET applied_at = '2024-01-02T00:00:00Z' WHERE id = ?1",
                rusqlite::params![child_2],
            )
            .unwrap();
        }

        // 第三轮轮询：全空
        let polled_3 = poll_completed_child_runs(&state, "parent-run-multi").unwrap();
        assert_eq!(polled_3.len(), 0);

        let _ = std::fs::remove_dir_all(base);
    }

    fn sample_success_json(child_run_id: &str, finding: &str) -> String {
        serde_json::to_string(&ChildReviewResult {
            child_run_id: child_run_id.to_string(),
            status: "succeeded".to_string(),
            findings: vec![ReviewFinding {
                id: "F1".to_string(),
                severity: "suggestion".to_string(),
                finality: "regardless".to_string(),
                location: "".to_string(),
                current: "".to_string(),
                why: finding.to_string(),
                suggestion: "".to_string(),
                evidence_strength: "confirmed".to_string(),
            }],
            confidence: "confirmed".to_string(),
            evidence_refs: vec!["ref-1".to_string()],
            missing_evidence: vec![],
            duration_ms: 120,
            completion_tokens: 25,
            prompt_tokens: 0,
            error_kind: None,
            raw_result_json: None,
            kind: None,
            routing: Some("certain".to_string()),
            withdrawn_findings: vec![],
        objection_verdicts: vec![],
            scorecard: None,
            task_id: None,
            attempt_id: None,
        })
        .unwrap()
    }

    #[test]
    fn test_poll_completed_child_runs_preserves_writer_raw_result_and_kind() {
        let base = std::env::temp_dir().join(format!("free-api-child-poll-writer-{}", Uuid::new_v4()));
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));

        let run_id = begin_child_run(&state, "parent-poll-w", "session-pw", &workspace, "model-w", 1, "writer", None).unwrap();
        let writer_json = serde_json::json!({
            "child_run_id": run_id,
            "status": "succeeded",
            "test_status": "passed",
            "findings": ["修改成功"],
            "confidence": "confirmed",
            "evidence_refs": [],
            "missing_evidence": [],
            "duration_ms": 100,
            "completion_tokens": 50,
            "changed_files": []
        }).to_string();

        finish_child_run(&state, &run_id, "succeeded", 100, 50, 0, None, Some(&writer_json));

        let polled = poll_completed_child_runs(&state, "parent-poll-w").unwrap();
        assert_eq!(polled.len(), 1);
        assert_eq!(polled[0].kind.as_deref(), Some("writer"));
        assert!(polled[0].raw_result_json.is_some());
        assert_eq!(polled[0].raw_result_json.as_deref(), Some(writer_json.as_str()));

        // 验证强类型抽取方法正常工作
        let writer_res = polled[0].as_writer_result().expect("as_writer_result 应成功解析");
        assert_eq!(writer_res.findings, vec!["修改成功".to_string()]);
        assert_eq!(writer_res.test_status, "passed");

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn test_poll_completed_delivery_receipts_unified_pipeline() {
        let base = std::env::temp_dir().join(format!("free-api-receipt-poll-{}", Uuid::new_v4()));
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));

        // 1. 模拟子审查任务
        let rev_run_id = begin_child_run(&state, "parent-receipt-p1", "session-rc1", &workspace, "model-r", 1, "review", None).unwrap();
        let rev_json = serde_json::json!({
            "child_run_id": rev_run_id,
            "status": "succeeded",
            "findings": [{
                "id": "F1",
                "severity": "blocking",
                "finality": "regardless",
                "location": "src/main.rs:1",
                "current": "",
                "why": "漏洞",
                "suggestion": "修复",
                "evidence_strength": "confirmed"
            }],
            "confidence": "high",
            "evidence_refs": ["ref1"],
            "missing_evidence": [],
            "duration_ms": 120,
            "completion_tokens": 40,
            "routing": "certain",
            "withdrawn_findings": []
        }).to_string();
        finish_child_run(&state, &rev_run_id, "succeeded", 120, 40, 0, None, Some(&rev_json));

        // 2. 模拟写入工兵任务
        let wrt_run_id = begin_child_run(&state, "parent-receipt-p1", "session-rc1", &workspace, "model-w", 2, "writer", None).unwrap();
        let wrt_json = serde_json::json!({
            "child_run_id": wrt_run_id,
            "status": "succeeded",
            "test_status": "passed",
            "findings": ["已创建新模块"],
            "confidence": "confirmed",
            "evidence_refs": [],
            "missing_evidence": [],
            "duration_ms": 200,
            "completion_tokens": 80,
            "changed_files": []
        }).to_string();
        finish_child_run(&state, &wrt_run_id, "succeeded", 200, 80, 0, None, Some(&wrt_json));

        // 统一轮询回执流
        let receipts = poll_completed_delivery_receipts(&state, "parent-receipt-p1", None).unwrap();
        assert_eq!(receipts.len(), 2);

        let rev_receipt = receipts.iter().find(|r| r.child_run_id == rev_run_id).expect("审查回执存在");
        assert_eq!(rev_receipt.kind, "review");
        assert_eq!(rev_receipt.confidence(), "high");
        assert_eq!(rev_receipt.stage_blocking_open(), 1);
        let delivery_data = rev_receipt.as_reviewer_delivery().expect("应能提取 ReviewerDeliveryData");
        assert_eq!(delivery_data.findings.len(), 1);
        assert_eq!(delivery_data.findings[0].id, "F1");

        let wrt_receipt = receipts.iter().find(|r| r.child_run_id == wrt_run_id).expect("工兵回执存在");
        assert_eq!(wrt_receipt.kind, "writer");
        let writer_data = wrt_receipt.as_writer_result().expect("应能提取 WriterResult");
        assert_eq!(writer_data.findings, vec!["已创建新模块".to_string()]);
        assert_eq!(writer_data.test_status, "passed");

        // 消费幂等性校验：applied_at 落账后再次轮询必须为空
        {
            let conn = state.0.lock().unwrap();
            conn.execute(
                "UPDATE agent_child_runs SET applied_at = consumed_at WHERE parent_run_id = 'parent-receipt-p1' AND consumed_at IS NOT NULL",
                [],
            )
            .unwrap();
        }
        let empty_receipts = poll_completed_delivery_receipts(&state, "parent-receipt-p1", None).unwrap();
        assert!(empty_receipts.is_empty());

        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn wait_for_jobs_returns_completed_child_reviewer_results() {
        let base = std::env::temp_dir().join(format!("free-api-child-wait-{}", Uuid::new_v4()));
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));

        let child_id = begin_child_run(&state, "parent-run-wait", "session-wait", &workspace, "model-a", 1, "review", None).unwrap();
        finish_child_run(
            &state,
            &child_id,
            "succeeded",
            120,
            25,
            0,
            None,
            Some(&sample_success_json(&child_id, "审查结论：主根因成立")),
        );

        // wait_jobs 定向等待 child_run_id：直接取回完整审查结论
        let results = crate::agent::job_engine::wait_for_jobs(
            &state,
            "parent-run-wait",
            Some(&[child_id.clone()]),
            10,
            None,
        )
        .await
        .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].job_id, child_id);
        assert_eq!(results[0].job_type, "child_reviewer");
        assert_eq!(results[0].status, "succeeded");
        assert!(results[0].result_json.contains("主根因成立"));

        // 效果落账（applied_at）后不可重复投递（幂等）；未落账前会重投
        {
            let conn = state.0.lock().unwrap();
            conn.execute(
                "UPDATE agent_child_runs SET applied_at = '2024-01-02T00:00:00Z' WHERE id = ?1",
                params![child_id],
            )
            .unwrap();
        }
        let again = crate::agent::job_engine::wait_for_jobs(
            &state,
            "parent-run-wait",
            Some(&[child_id.clone()]),
            5,
            None,
        )
        .await
        .unwrap();
        assert!(again.is_empty());

        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn wait_for_jobs_reactively_wakes_when_child_finishes() {
        let base = std::env::temp_dir().join(format!("free-api-child-wake-{}", Uuid::new_v4()));
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));

        let child_id = begin_child_run(&state, "parent-run-wake", "session-wake", &workspace, "model-a", 0, "review", None).unwrap();

        // 挂起等待期间子审查在后台完成，watch 版本号变更应瞬间唤醒等待者
        let db_clone = DbState(state.0.clone());
        let child_clone = child_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            finish_child_run(
                &db_clone,
                &child_clone,
                "succeeded",
                45,
                20,
                0,
                None,
                Some(&sample_success_json(&child_clone, "后台完成的结论")),
            );
        });

        let started = std::time::Instant::now();
        let results = crate::agent::job_engine::wait_for_jobs(
            &state,
            "parent-run-wake",
            Some(&[child_id.clone()]),
            10,
            None,
        )
        .await
        .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].job_id, child_id);
        assert!(results[0].result_json.contains("后台完成的结论"));
        // 反应式唤醒：远小于超时上限，也排除 spin 快速假通过
        assert!(elapsed.as_millis() >= 40 && elapsed.as_millis() < 3000);

        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn wait_for_jobs_targeting_running_child_does_not_consume_finished_sibling() {
        let base = std::env::temp_dir().join(format!("free-api-child-target-{}", Uuid::new_v4()));
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));

        let finished = begin_child_run(&state, "parent-run-t", "session-t", &workspace, "model-a", 0, "review", None).unwrap();
        let running = begin_child_run(&state, "parent-run-t", "session-t", &workspace, "model-a", 0, "review", None).unwrap();
        finish_child_run(
            &state,
            &finished,
            "succeeded",
            80,
            15,
            0,
            None,
            Some(&sample_success_json(&finished, "先完成的兄弟任务")),
        );

        // 定向等待仍在 running 的 child：超时返回空，且不得消费已完成的兄弟结果
        let results = crate::agent::job_engine::wait_for_jobs(
            &state,
            "parent-run-t",
            Some(&[running.clone()]),
            1,
            None,
        )
        .await
        .unwrap();
        assert!(results.iter().all(|job| job.job_id != finished));

        // 兄弟结果仍可被后续轮询完整取回
        let later = crate::agent::job_engine::wait_for_jobs(
            &state,
            "parent-run-t",
            Some(&[finished.clone()]),
            5,
            None,
        )
        .await
        .unwrap();
        assert_eq!(later.len(), 1);
        assert_eq!(later[0].job_id, finished);

        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn failed_child_reviewer_result_is_delivered_with_error_kind() {
        let base = std::env::temp_dir().join(format!("free-api-child-fail-{}", Uuid::new_v4()));
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));

        let child_id = begin_child_run(&state, "parent-run-fail", "session-fail", &workspace, "model-a", 0, "review", None).unwrap();
        finish_child_run(
            &state,
            &child_id,
            "timed_out",
            45_000,
            0,
            0,
            Some(ChildErrorKind::Timeout),
            Some(&child_error_result_json(&child_id, "timed_out", &ChildErrorKind::Timeout, 45_000, 0, 0, None)),
        );

        // 主循环信箱轮询也能取到失败终态（此前失败结果会被静默丢弃）
        let polled = poll_completed_child_runs(&state, "parent-run-fail").unwrap();
        assert_eq!(polled.len(), 1);
        assert_eq!(polled[0].status, "timed_out");
        assert_eq!(polled[0].error_kind.as_deref(), Some("timeout"));
        assert!(polled[0].findings.is_empty());

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn corrupt_result_json_row_is_cleared_instead_of_retried_forever() {
        let base = std::env::temp_dir().join(format!("free-api-child-corrupt-{}", Uuid::new_v4()));
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));

        let child_id = begin_child_run(&state, "parent-run-corrupt", "session-corrupt", &workspace, "model-a", 0, "review", None).unwrap();
        finish_child_run(&state, &child_id, "succeeded", 100, 10, 0, None, Some("{not-valid-json"));
        {
            let conn = state.0.lock().unwrap();
            conn.execute(
                "UPDATE agent_child_runs SET status = 'succeeded' WHERE id = ?1",
                params![child_id],
            )
            .unwrap();
        }

        // 第一轮轮询：解析失败，返回空但清除损坏行
        let polled = poll_completed_child_runs(&state, "parent-run-corrupt").unwrap();
        assert!(polled.is_empty());

        // 第二轮轮询：该行已退出信箱，不会无限重试
        let polled_again = poll_completed_child_runs(&state, "parent-run-corrupt").unwrap();
        assert!(polled_again.is_empty());

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn orphaned_running_child_runs_recover_on_session_start() {
        let base = std::env::temp_dir().join(format!("free-api-child-orphan-{}", Uuid::new_v4()));
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));

        let session_id = "session-orphan";
        let child_id = begin_child_run(&state, "parent-run-orphan", session_id, &workspace, "model-a", 0, "review", None).unwrap();
        assert!(has_running_child_runs(&state, "parent-run-orphan"));

        // 同会话的其它子任务不受影响
        let other_session_child =
            begin_child_run(&state, "parent-run-other", "session-other", &workspace, "model-a", 0, "review", None).unwrap();

        let recovered =
            recover_orphaned_child_runs_on_session_start(&state, session_id).unwrap();
        assert_eq!(recovered, 1);
        assert!(!has_running_child_runs(&state, "parent-run-orphan"));
        assert!(has_running_child_runs(&state, "parent-run-other"));

        // 收敛后的孤儿子任务携带可消费的终态结果，父 Agent 能得知子审查未能产出结论
        let polled = poll_completed_child_runs(&state, "parent-run-orphan").unwrap();
        assert_eq!(polled.len(), 1);
        assert_eq!(polled[0].child_run_id, child_id);
        assert_eq!(polled[0].status, "interrupted");
        assert_eq!(polled[0].error_kind.as_deref(), Some("interrupted"));

        // 重复恢复幂等
        let recovered_again =
            recover_orphaned_child_runs_on_session_start(&state, session_id).unwrap();
        assert_eq!(recovered_again, 0);

        let _ = cancel_running_child_runs(&state, "parent-run-other");
        let _ = std::fs::remove_dir_all(base);
        let _ = other_session_child;
    }

    #[tokio::test]
    async fn cancelling_child_runs_wakes_parked_waiters_immediately() {
        let base = std::env::temp_dir().join(format!("free-api-child-cancel-wake-{}", Uuid::new_v4()));
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));

        let child_id = begin_child_run(&state, "parent-run-wake", "session-wake", &workspace, "model-a", 0, "review", None).unwrap();

        let started = std::time::Instant::now();
        let state_clone = DbState(state.0.clone());
        let waiter = tokio::spawn(async move {
            crate::agent::job_engine::wait_for_jobs(
                &state_clone,
                "parent-run-wake",
                Some(&[child_id.clone()]),
                120,
                None,
            )
            .await
            .unwrap()
        });

        // 等待者先挂起，再触发取消：等待者必须立刻被唤醒，而不是等满 120s 超时
        tokio::time::sleep(Duration::from_millis(100)).await;
        let cancelled = cancel_running_child_runs(&state, "parent-run-wake").unwrap();
        assert_eq!(cancelled, 1);

        let results = tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("取消后等待者应在 5 秒内被唤醒")
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(results.is_empty());

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn begin_child_run_respects_custom_cap_and_global_guard() {
        let base = std::env::temp_dir().join(format!("free-api-child-cap-{}", Uuid::new_v4()));
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));

        // 1. 角色卡 custom_cap = Some(1)：第 1 个成功，第 2 个超限被拒
        let _c1 = begin_child_run(&state, "p-run-1", "s-1", &workspace, "m", 0, "worker", Some(1)).unwrap();
        assert!(begin_child_run(&state, "p-run-1", "s-1", &workspace, "m", 0, "worker", Some(1)).is_err(), "超单卡额度1被拒");

        // 2. 角色卡 custom_cap = Some(3)：放宽至 3
        let _c2 = begin_child_run(&state, "p-run-2", "s-2", &workspace, "m", 0, "worker", Some(3)).unwrap();
        let _c3 = begin_child_run(&state, "p-run-2", "s-2", &workspace, "m", 0, "worker", Some(3)).unwrap();
        let _c4 = begin_child_run(&state, "p-run-2", "s-2", &workspace, "m", 0, "worker", Some(3)).unwrap();
        assert!(begin_child_run(&state, "p-run-2", "s-2", &workspace, "m", 0, "worker", Some(3)).is_err(), "超单卡额度3被拒");

        // 3. 全局总看门 MAX_TOTAL_CHILDREN_PER_RUN = 6
        begin_child_run(&state, "p-run-3", "s-3", &workspace, "m", 0, "review", None).unwrap();
        begin_child_run(&state, "p-run-3", "s-3", &workspace, "m", 0, "review", None).unwrap();
        begin_child_run(&state, "p-run-3", "s-3", &workspace, "m", 0, "worker", None).unwrap();
        begin_child_run(&state, "p-run-3", "s-3", &workspace, "m", 0, "worker", None).unwrap();
        begin_child_run(&state, "p-run-3", "s-3", &workspace, "m", 0, "writer", Some(3)).unwrap();
        begin_child_run(&state, "p-run-3", "s-3", &workspace, "m", 0, "writer", Some(3)).unwrap();
        // 第 7 个被全局总看门拦截（6个已满）
        let err = begin_child_run(&state, "p-run-3", "s-3", &workspace, "m", 0, "writer", Some(3)).unwrap_err();
        assert!(err.contains("全局看门上限已满"));

        // 4. 验证真并发机制：释放 1 个 running 任务后，新的任务即可成功进入（证明非累计死锁）
        state.0.lock().unwrap().execute(
            "UPDATE agent_child_runs SET status = 'succeeded' WHERE parent_run_id = 'p-run-3' AND rowid IN (SELECT rowid FROM agent_child_runs WHERE parent_run_id = 'p-run-3' LIMIT 1)",
            [],
        ).unwrap();
        assert!(begin_child_run(&state, "p-run-3", "s-3", &workspace, "m", 0, "writer", Some(3)).is_ok(), "释放一个并发名额后应成功进入");

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn resume_child_load_is_scoped_to_parent_session() {
        // child_run_id 是会话内凭据：跨会话命中必须按"未找到"处理，
        // 不能读取/复活其他会话的历史与工作区副本。
        let base = std::env::temp_dir().join(format!("free-api-resume-scope-{}", Uuid::new_v4()));
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));

        let child_id = begin_child_run(&state, "run-a", "session-a", &workspace, "model-a", 0, "review", None).unwrap();
        finish_child_run(&state, &child_id, "succeeded", 10, 5, 0, None, Some("{}"));

        assert!(load_child_run_for_resume(&state, &child_id, "session-a").is_ok(), "同会话可查");
        assert!(load_child_run_for_resume(&state, &child_id, "session-b").is_err(), "跨会话命中同 id 必须按未找到处理");
        assert!(load_child_run_for_resume(&state, "ghost-id", "session-a").is_err());

        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn targeted_wait_returns_early_when_targets_not_running() {
        // 目标已交付消费、只剩无关任务在跑：定向等待必须立即返回空，
        // 不能对着别人的 running 任务空等满超时。
        let base = std::env::temp_dir().join(format!("free-api-wait-target-{}", Uuid::new_v4()));
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));

        let done = begin_child_run(&state, "run-t", "sess-t", &workspace, "m", 0, "review", None).unwrap();
        finish_child_run(&state, &done, "succeeded", 10, 5, 0, None, Some(&sample_success_json(&done, "ok")));
        let _running = begin_child_run(&state, "run-t", "sess-t", &workspace, "m", 0, "worker", None).unwrap();

        let first = crate::agent::job_engine::wait_for_jobs(&state, "run-t", Some(&[done.clone()]), 1, None).await.unwrap();
        assert_eq!(first.len(), 1, "首次定向等待取回目标结果");
        // 模拟提交落账：applied_at 随 checkpoint 写后，目标才算真正消费完毕
        {
            let conn = state.0.lock().unwrap();
            conn.execute(
                "UPDATE agent_child_runs SET applied_at = '2024-01-02T00:00:00Z' WHERE id = ?1",
                params![done],
            )
            .unwrap();
        }

        let started = std::time::Instant::now();
        let again = crate::agent::job_engine::wait_for_jobs(&state, "run-t", Some(&[done.clone()]), 10, None).await.unwrap();
        assert!(again.is_empty());
        assert!(started.elapsed() < Duration::from_secs(3), "目标已消费后定向等待必须早退，不得被无关 running 任务拖住");

        let started2 = std::time::Instant::now();
        let none = crate::agent::job_engine::wait_for_jobs(&state, "run-t", Some(&["ghost".to_string()]), 10, None).await.unwrap();
        assert!(none.is_empty());
        assert!(started2.elapsed() < Duration::from_secs(3), "不存在/已终态的目标必须立即返回");

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn dispatch_output_projection_preserves_full_receipt_fields() {
        // 投影收窄的是体积不是语义：kind/error_kind/objection_verdicts/scorecard
        // 与 custom_schema 扩展字段都必须出现在模型可见输出里。
        let raw = serde_json::json!({
            "child_run_id": "c1",
            "status": "succeeded",
            "findings": [{"id":"F1","severity":"blocking","finality":"regardless","location":"a.rs:1","current":"c","why":"w","suggestion":"s","evidence_strength":"confirmed"}],
            "confidence": "confirmed",
            "evidence_refs": [],
            "missing_evidence": [],
            "duration_ms": 5,
            "completion_tokens": 7,
            "prompt_tokens": 42,
            "error_kind": null,
            "kind": "review",
            "routing": "certain",
            "withdrawn_findings": ["F0"],
            "objection_verdicts": [{"finding_id":"F0","verdict":"withdraw","reason":"r"}],
            "scorecard": {"correctness":80,"risk":70,"style":90,"test_coverage":null,"null_reason":"n/a","synthesized":false},
            "cve_refs": ["CVE-2024-0001"]
        });
        let mut result: ChildReviewResult = serde_json::from_value(raw.clone()).unwrap();
        result.raw_result_json = Some(raw.to_string());
        let out = result_as_dispatch_output(result);
        let view: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(view["scorecard"]["correctness"], 80);
        assert_eq!(view["objection_verdicts"][0]["verdict"], "withdraw");
        assert_eq!(view["prompt_tokens"], 42);
        assert_eq!(view["kind"], "review");
        assert_eq!(view["cve_refs"][0], "CVE-2024-0001", "custom_schema 扩展字段不丢");
    }

    #[test]
    fn finish_child_run_persists_prompt_tokens_ledger() {
        // 成本台账：prompt_tokens 与 completion_tokens 一样落 agent_child_runs 列，
        // 多 Agent 的隐性成本大头在 prompt 侧（继承历史+证据包）。
        let base = std::env::temp_dir().join(format!("free-api-child-ledger-{}", Uuid::new_v4()));
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));

        let child_id = begin_child_run(&state, "run-ledger", "sess-ledger", &workspace, "m", 0, "review", None).unwrap();
        finish_child_run(&state, &child_id, "succeeded", 10, 500, 12_000, None, Some("{}"));

        let conn = state.0.lock().unwrap();
        let (comp, prompt): (i64, i64) = conn
            .query_row(
                "SELECT completion_tokens, prompt_tokens FROM agent_child_runs WHERE id = ?1",
                params![child_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        drop(conn);
        assert_eq!(comp, 500);
        assert_eq!(prompt, 12_000);

        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn retry_spawn_adopts_existing_live_attempt_instead_of_respawning() {
        // 崩溃重放幂等：备胎行已存在（崩溃时序：备胎 INSERT 已提交、
        // 父 checkpoint 回滚 → 旧失败回执重投 → failover 重走）时，
        // spawn 必须认养在途 attempt 而不是再造一个。
        let base = std::env::temp_dir().join(format!("free-api-adopt-{}", Uuid::new_v4()));
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));

        // 前驱失败行：task_id 固定为 "task-f"，attempt 1
        let prev_identity = ChildRunIdentity {
            task_id: "task-f".to_string(),
            attempt_id: 1,
            purpose: Some("plan_review".to_string()),
            batch_id: Some("batch-f".to_string()),
            contract_id: None,
            contract_schema: None,
        };
        let failed = begin_child_run_extended(
            &state, "parent-f", "sess-f", &workspace, "m", 1, "review", None, None, None, &prev_identity,
        )
        .unwrap();
        finish_child_run(&state, &failed, "timed_out", 100, 0, 0, Some(crate::agent::types::ChildErrorKind::Timeout), Some("{}"));

        // 已存在的备胎：同 task_id，attempt 2，仍在途（崩溃窗口的真实形状）
        let spare_identity = ChildRunIdentity {
            task_id: "task-f".to_string(),
            attempt_id: 2,
            purpose: Some("plan_review".to_string()),
            batch_id: Some("batch-f".to_string()),
            contract_id: None,
            contract_schema: None,
        };
        let spare = begin_child_run_extended(
            &state, "parent-f", "sess-f", &workspace, "m2", 1, "review", None, None, None, &spare_identity,
        )
        .unwrap();

        // 重放 failover：spawn(retry_of=failed) 应认养 spare，不产生新行
        let snapshot = PlanReviewerDispatchSnapshot::new(
            "m3", "tier", "effort", "persona", serde_json::json!({"role": "tech_lead"}),
        );
        let (_tx, rx) = tokio::sync::watch::channel(0u64);
        let adopted = snapshot
            .spawn(None, &state, "parent-f", "sess-f", &workspace, rx, Some(&failed))
            .await
            .unwrap();
        assert_eq!(adopted, spare, "重放必须认养已在途的备胎行而非重派");

        let count: i64 = state
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM agent_child_runs WHERE task_id = 'task-f'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2, "认养不得新增台账行（重复 failover 会被拒）");
        let _ = std::fs::remove_dir_all(base);
    }


    #[test]
    fn reviewer_submission_reports_count_then_exact_reference_from_license_incident() {
        // 10-14 实弹：JSON 完整，但 missing_evidence 有 5 项、引用省略了末尾说明。
        let allowed = vec!["AuthService.java:86-101 登录入口".to_string()];
        let mut raw = json!({
            "findings": [], "confidence": "supported",
            "evidence_refs": ["AuthService.java:86-101"],
            "missing_evidence": ["lic.jar 依赖", "pom 依赖", "拦截顺序", "挂载路径", "pubring 打包"]
        });
        let count_error = parse_controlled_output(&raw.to_string(), &allowed).unwrap_err();
        assert!(count_error.contains("missing_evidence 实际 5 项，最多 3 项"));
        let feedback = review_output_rejection_feedback(&count_error, 1, 20);
        assert!(feedback.contains(&count_error));
        assert!(feedback.contains("提交校验未通过"));
        assert!(feedback.contains("不代表方案被驳回"));
        assert!(!feedback.contains("必须是最终审查 JSON（含 findings 键）或取证请求"));

        raw["missing_evidence"] = json!(["合并后的依赖缺口", "拦截顺序", "路径与打包"]);
        let ref_error = parse_controlled_output(&raw.to_string(), &allowed).unwrap_err();
        assert!(ref_error.contains("引用 \"AuthService.java:86-101\""));
        assert!(ref_error.contains("AuthService.java:86-101 登录入口"));
        assert!(ref_error.contains("原样使用完整引用"));
        raw["evidence_refs"] = json!(allowed);
        assert!(parse_controlled_output(&raw.to_string(), &allowed).is_ok());
    }

    #[test]
    fn reviewer_submission_keeps_json_syntax_and_required_field_errors() {
        let syntax = parse_controlled_output(r#"{"findings":[],"confidence":}"#, &[]).unwrap_err();
        assert!(syntax.contains("JSON 解析失败"));
        assert!(syntax.contains("line 1 column"));
        let missing = parse_controlled_output(r#"{"confidence":"supported"}"#, &[]).unwrap_err();
        assert!(missing.contains("missing field `findings`"));
        let wrong_type = parse_controlled_output(r#"{"findings":[],"confidence":42}"#, &[]).unwrap_err();
        assert!(wrong_type.contains("invalid type: integer `42`, expected a string"));
    }

    #[test]
    fn reviewer_submission_reports_findings_count_and_confidence_value() {
        let raw = json!({"findings": ["a", "b", "c", "d", "e", "f"], "confidence": "supported"});
        let error = parse_controlled_output(&raw.to_string(), &[]).unwrap_err();
        assert!(error.contains("findings 实际 6 项，最多 5 项"));
        let error = parse_controlled_output(r#"{"findings":[],"confidence":"high"}"#, &[]).unwrap_err();
        assert!(error.contains("实际为 \"high\""));
        assert!(error.contains("confirmed、supported 或 unverified"));
    }

    #[test]
    fn reviewer_submission_terminal_feedback_does_not_request_another_retry() {
        let reason = "missing_evidence 实际 5 项，最多 3 项";
        let feedback = review_output_rejection_feedback(reason, 20, 20);
        assert!(feedback.contains(reason));
        assert!(feedback.contains("invalid_output 结束"));
        assert!(!feedback.contains("重新输出"));
    }

    #[test]
    fn reviewer_invalid_submission_reason_survives_persistence_and_parent_poll() {
        let base = std::env::temp_dir().join(format!("free-api-review-invalid-{}", Uuid::new_v4()));
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = DbState(std::sync::Arc::new(std::sync::Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));
        let child = begin_child_run(&state, "parent-invalid", "sess-invalid", &workspace, "m", 1, "review", None).unwrap();
        let raw = json!({"findings": [], "confidence": "supported", "missing_evidence": ["a", "b", "c", "d", "e"]}).to_string();
        let reason = parse_controlled_output(&raw, &[]).unwrap_err();
        let payload = child_invalid_result_json(&child, 10, 20, 30, &raw, &reason);
        finish_child_run(&state, &child, "invalid_output", 10, 20, 30,
            Some(ChildErrorKind::InvalidOutput), Some(&payload));
        {
            let conn = state.0.lock().unwrap();
            let stored: String = conn.query_row("SELECT result_json FROM agent_child_runs WHERE id = ?1",
                params![child], |r| r.get(0)).unwrap();
            let value: Value = serde_json::from_str(&stored).unwrap();
            assert_eq!(value["validation_error"], reason);
            assert_eq!(value["raw_result_json"], raw);
        }
        let results = poll_completed_child_runs(&state, "parent-invalid").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, "invalid_output");
        assert_eq!(results[0].error_kind.as_deref(), Some("invalid_output"));
        assert!(results[0].missing_evidence[0].contains(&reason));
        drop(state);
        let _ = std::fs::remove_dir_all(base);
    }
