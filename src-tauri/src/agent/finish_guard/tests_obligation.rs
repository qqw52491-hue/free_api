//! obligation 证据链与 diff 机械校验测试。

#[cfg(test)]
mod tests_inner {
    use super::super::testkit::*;
    use super::super::*;
    use crate::agent::dispatcher::is_write_or_side_effect_tool;
    use crate::agent::review_contract::ReviewObligation;

    #[test]
    fn test_parse_file_line_ref_formats() {
        assert_eq!(
            parse_file_line_ref("src/views/ChatView.vue:442"),
            Some(("src/views/ChatView.vue".to_string(), 442, 442))
        );
        assert_eq!(
            parse_file_line_ref("src/views/ChatView.vue:87-103"),
            Some(("src/views/ChatView.vue".to_string(), 87, 103))
        );
        assert_eq!(
            parse_file_line_ref("ChatView.vue:L442"),
            Some(("ChatView.vue".to_string(), 442, 442))
        );
        assert_eq!(
            parse_file_line_ref("ChatView.vue#L87-L103"),
            Some(("ChatView.vue".to_string(), 87, 103))
        );
        assert_eq!(
            parse_file_line_ref("[ChatView.vue:442](file:///workspace/ChatView.vue#L442)"),
            Some(("ChatView.vue".to_string(), 442, 442))
        );
        assert_eq!(
            parse_file_line_ref("ChatView.vue:442 (scrollToBottom 优化)"),
            Some(("ChatView.vue".to_string(), 442, 442))
        );
        assert_eq!(
            parse_file_line_ref("`src/components/ModelSelect.vue:55..60`"),
            Some(("src/components/ModelSelect.vue".to_string(), 55, 60))
        );
        // 非代码行号证据返回 None
        assert_eq!(parse_file_line_ref("output_id: test-42"), None);
        assert_eq!(parse_file_line_ref("git diff --stat"), None);
        assert_eq!(parse_file_line_ref("npm test passed"), None);
        assert_eq!(parse_file_line_ref("已核验无异常"), None);
    }

    #[test]
    fn test_single_edit_touching_multiple_obligations_is_not_blocked() {
        // 场景：单次编辑 (side_effect_count = 1) 同时修改了同一个文件中的 3 个函数
        // Diff 包含了 3 个修改 Hunk，分别覆盖 lines 87-103, 126, 445
        let raw_diff = r#"diff --git a/src/views/ChatView.vue b/src/views/ChatView.vue
index 123..456 100644
--- a/src/views/ChatView.vue
+++ b/src/views/ChatView.vue
@@ -87,17 +87,20 @@
-old virtual scroll
+new virtual scroll
@@ -124,5 +126,8 @@
-old markdown render
+new markdown render
@@ -440,6 +445,10 @@
-old scrollToBottom
+new scrollToBottom with rAF
"#;
        let diff_ctx = DiffContext::from_diff_and_paths(
            Some(raw_diff),
            &["src/views/ChatView.vue".to_string()],
        );

        let contract = ReviewContract {
            scope_statement: "优化 ChatView.vue 长列表与渲染性能".to_string(),
            obligations: vec![
                ReviewObligation {
                    id: "scroll".to_string(),
                    question: "是否优化 scrollToBottom 节流？".to_string(),
                    status: "evidenced".to_string(),
                    evidence_refs: vec!["ChatView.vue:445".to_string()],
                    waived_reason: None,
                    verify_command: None,
                    verify_script: None,
                    verify_sha256: None,
                },
                ReviewObligation {
                    id: "render".to_string(),
                    question: "是否优化 Markdown 渲染防抖？".to_string(),
                    status: "evidenced".to_string(),
                    evidence_refs: vec!["src/views/ChatView.vue:126".to_string()],
                    waived_reason: None,
                    verify_command: None,
                    verify_script: None,
                    verify_sha256: None,
                },
                ReviewObligation {
                    id: "virtual".to_string(),
                    question: "是否启用虚拟滚动列表？".to_string(),
                    status: "evidenced".to_string(),
                    evidence_refs: vec!["ChatView.vue:87-103".to_string()],
                    waived_reason: None,
                    verify_command: None,
                    verify_script: None,
                    verify_sha256: None,
                },
            ],
        };

        let verified = ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec!["output_id: test-1".to_string()],
            waived_reason: None,
        };

        // 关键断言：side_effect_count = 1，但 3 条义务均在 diff 中找到真实修改行号，绝不误伤拦截！
        let blockers = finish_blockers_with_diff(
            &[],
            Some(&contract),
            false,
            1,
            Some(&verified),
            Some(&diff_ctx),
            None,
            None,
            &[]        );
        assert!(
            blockers.is_empty(),
            "单次编辑同时覆盖多项义务时必须放行，不按写入次数误伤：{:?}",
            blockers
        );
    }

    #[test]
    fn test_false_completion_extract_py_only_is_blocked() {
        // 场景（复现用户真实 Bug）：
        // Agent 只写了 extract.py，却声称完成了 ChatView.vue 的性能优化并在契约中标为 evidenced
        let raw_diff = r#"【新增未跟踪文件】:
--- 新增文件: extract.py ---
def main():
    pass
"#;
        let diff_ctx = DiffContext::from_diff_and_paths(
            Some(raw_diff),
            &["extract.py".to_string()],
        );

        let contract = ReviewContract {
            scope_statement: "性能优化".to_string(),
            obligations: vec![
                ReviewObligation {
                    id: "scroll".to_string(),
                    question: "优化 scrollToBottom".to_string(),
                    status: "evidenced".to_string(),
                    evidence_refs: vec!["ChatView.vue:442".to_string()],
                    waived_reason: None,
                    verify_command: None,
                    verify_script: None,
                    verify_sha256: None,
                },
            ],
        };

        let verified = ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec!["output_id: test-1".to_string()],
            waived_reason: None,
        };

        let blockers = finish_blockers_with_diff(
            &[],
            Some(&contract),
            false,
            1,
            Some(&verified),
            Some(&diff_ctx),
            None,
            None,
            &[]        );

        assert_eq!(blockers.len(), 1);
        assert!(
            blockers[0].contains("scroll"),
            "必须点名未真实履约的义务 id: {}",
            blockers[0]
        );
        assert!(
            blockers[0].contains("ChatView.vue:442"),
            "必须指出无效的证据引用: {}",
            blockers[0]
        );
        assert!(
            blockers[0].contains("不在本次真实改动 (diff) 覆盖区间内"),
            "拦截文案必须明确原因: {}",
            blockers[0]
        );
        assert!(
            blockers[0].contains("review_contract_update"),
            "拦截文案必须给出可执行修法: {}",
            blockers[0]
        );
    }

    #[test]
    fn test_partial_fix_blocks_only_unmodified_obligations() {
        // 场景：Agent 真实修改了 scrollToBottom (line 445)，但没有修改 Markdown 渲染 (line 126)
        let raw_diff = r#"diff --git a/src/views/ChatView.vue b/src/views/ChatView.vue
index 123..456 100644
--- a/src/views/ChatView.vue
+++ b/src/views/ChatView.vue
@@ -440,6 +445,10 @@
-old scrollToBottom
+new scrollToBottom
"#;
        let diff_ctx = DiffContext::from_diff_and_paths(
            Some(raw_diff),
            &["src/views/ChatView.vue".to_string()],
        );

        let contract = ReviewContract {
            scope_statement: "优化性能".to_string(),
            obligations: vec![
                ReviewObligation {
                    id: "scroll".to_string(),
                    question: "优化 scrollToBottom".to_string(),
                    status: "evidenced".to_string(),
                    evidence_refs: vec!["ChatView.vue:445".to_string()],
                    waived_reason: None,
                    verify_command: None,
                    verify_script: None,
                    verify_sha256: None,
                },
                ReviewObligation {
                    id: "render".to_string(),
                    question: "优化 Markdown 渲染".to_string(),
                    status: "evidenced".to_string(),
                    evidence_refs: vec!["ChatView.vue:126".to_string()],
                    waived_reason: None,
                    verify_command: None,
                    verify_script: None,
                    verify_sha256: None,
                },
            ],
        };

        let verified = ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec!["output_id: test-1".to_string()],
            waived_reason: None,
        };

        let blockers = finish_blockers_with_diff(
            &[],
            Some(&contract),
            false,
            1,
            Some(&verified),
            Some(&diff_ctx),
            None,
            None,
            &[]        );

        // scroll 命中 line 445 放行；render 引用 line 126 未被修改，被精准拦截！
        assert_eq!(blockers.len(), 1);
        assert!(blockers[0].contains("render"));
        assert!(!blockers[0].contains("scroll"));
    }

    #[test]
    fn test_missing_line_evidence_in_write_task_is_blocked() {
        // 场景：在写操作任务中，义务声称 evidenced 但没有给出形如 file:line 的证据
        let raw_diff = r#"diff --git a/src/views/ChatView.vue b/src/views/ChatView.vue
@@ -440,6 +445,10 @@
+new
"#;
        let diff_ctx = DiffContext::from_diff_and_paths(
            Some(raw_diff),
            &["src/views/ChatView.vue".to_string()],
        );

        let contract = ReviewContract {
            scope_statement: "优化性能".to_string(),
            obligations: vec![
                ReviewObligation {
                    id: "scroll".to_string(),
                    question: "优化 scrollToBottom".to_string(),
                    status: "evidenced".to_string(),
                    evidence_refs: vec!["已完成优化".to_string()],
                    waived_reason: None,
                    verify_command: None,
                    verify_script: None,
                    verify_sha256: None,
                },
            ],
        };

        let verified = ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec!["output_id: test-1".to_string()],
            waived_reason: None,
        };

        let blockers = finish_blockers_with_diff(
            &[],
            Some(&contract),
            false,
            1,
            Some(&verified),
            Some(&diff_ctx),
            None,
            None,
            &[]        );

        assert_eq!(blockers.len(), 1);
        assert!(blockers[0].contains("缺少具体代码行号"));
    }

    #[test]
    fn test_read_only_task_and_waived_obligations_are_not_blocked() {
        // 场景 1：只读任务 (side_effect_count = 0) 不做 diff 校验
        let contract = ReviewContract {
            scope_statement: "代码走查".to_string(),
            obligations: vec![
                ReviewObligation {
                    id: "audit".to_string(),
                    question: "是否存在内存泄漏？".to_string(),
                    status: "evidenced".to_string(),
                    evidence_refs: vec!["git diff --stat".to_string()],
                    waived_reason: None,
                    verify_command: None,
                    verify_script: None,
                    verify_sha256: None,
                },
            ],
        };
        let blockers = finish_blockers_with_diff(
            &[],
            Some(&contract),
            false,
            0,
            None,
            None,
            None,
            None,
            &[]        );
        assert!(blockers.is_empty());

        // 场景 2：豁免义务 (waived) 不做 diff 校验
        let contract_waived = ReviewContract {
            scope_statement: "性能优化".to_string(),
            obligations: vec![
                ReviewObligation {
                    id: "virtual".to_string(),
                    question: "虚拟滚动".to_string(),
                    status: "waived".to_string(),
                    evidence_refs: Vec::new(),
                    waived_reason: Some("经评估长列表数据量较小，暂无需引入虚拟滚动".to_string()),
                    verify_command: None,
                    verify_script: None,
                    verify_sha256: None,
                },
            ],
        };
        let empty_diff = DiffContext::default();
        let verified = ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec!["output_id: test-1".to_string()],
            waived_reason: None,
        };
        let blockers = finish_blockers_with_diff(
            &[],
            Some(&contract_waived),
            false,
            1,
            Some(&verified),
            Some(&empty_diff),
            None,
            None,
            &[]        );
        assert!(blockers.is_empty());

        // 场景 3：带有可执行 verify_command 的义务由 host_verify 处理，免除 diff 行号检查
        let contract_command = ReviewContract {
            scope_statement: "单元测试".to_string(),
            obligations: vec![
                ReviewObligation {
                    id: "unit_test".to_string(),
                    question: "单元测试是否通过？".to_string(),
                    status: "evidenced".to_string(),
                    evidence_refs: vec!["output_id: run-10".to_string()],
                    waived_reason: None,
                    verify_command: Some("npm test".to_string()),
                    verify_script: None,
                    verify_sha256: None,
                },
            ],
        };
        let blockers = finish_blockers_with_diff(
            &[],
            Some(&contract_command),
            false,
            1,
            Some(&verified),
            Some(&empty_diff),
            None,
            None,
            &[]        );
        assert!(blockers.is_empty());
    }

    #[test]
    fn test_readonly_step_preserves_finish_reviewed_and_challenge_count() {
        use crate::agent::dispatcher::is_write_or_side_effect_tool;
        use crate::agent::types::AgentInstruction;

        let read_inst = AgentInstruction {
            tool: Some("read_text_file".to_string()),
            description: "读取代码进行分析".to_string(),
            ..Default::default()
        };
        assert!(!is_write_or_side_effect_tool(&read_inst));

        let write_inst = AgentInstruction {
            tool: Some("write_to_file".to_string()),
            description: "写入修复代码".to_string(),
            ..Default::default()
        };
        assert!(is_write_or_side_effect_tool(&write_inst));
    }

    #[test]
    fn triage_diagnostic_obligation_uses_static_location_not_diff() {
        // 事故现场：有真实 diff 时，triage 下发的诊断义务引用"读过的文件"（diff 外）
        // 应放行；同一义务若不在取证单里则仍按 diff 对账拦截。
        let raw_diff = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -10,3 +10,4 @@\n fn a() {}\n";
        let ctx = DiffContext::from_diff_and_paths(Some(raw_diff), &[]);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

        let mut contract =
            diagnostic_contract_with_refs(&["src-tauri/src/agent/finish_guard/diff.rs:1"]);
        contract.obligations[0].question =
            "历史消息重载时的图片路径解析逻辑是否存在偏差？".to_string();
        let questions =
            vec!["历史消息重载时的图片路径解析逻辑是否存在偏差？".to_string()];

        let pass = review_contract_evidence_blockers(
            &contract,
            1,
            Some(&ctx),
            Some(root),
            None,
            &questions,
        );
        assert!(pass.is_empty(), "诊断义务的静态可定位引用应放行: {pass:?}");

        let blocked = review_contract_evidence_blockers(
            &contract,
            1,
            Some(&ctx),
            Some(root),
            None,
            &[],
        );
        assert_eq!(blocked.len(), 1, "非取证义务引用 diff 外位置必须拦截");
    }

    #[test]
    fn diagnostic_ref_with_fullwidth_annotation_resolves_by_path() {
        // 实测形态：模型给引用挂中文注释（全角括号）导致行号解析失败，
        // 诊断义务按"文件客观存在"放行，行级语义由终审核验。
        let raw_diff = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -10,3 +10,4 @@\n fn a() {}\n";
        let ctx = DiffContext::from_diff_and_paths(Some(raw_diff), &[]);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

        let mut contract = diagnostic_contract_with_refs(&[
            "src-tauri/src/agent/finish_guard/diff.rs:999999（某函数注释）",
        ]);
        contract.obligations[0].question =
            "历史消息重载时的图片路径解析逻辑是否存在偏差？".to_string();
        let questions =
            vec!["历史消息重载时的图片路径解析逻辑是否存在偏差？".to_string()];

        let pass = review_contract_evidence_blockers(
            &contract,
            1,
            Some(&ctx),
            Some(root),
            None,
            &questions,
        );
        assert!(pass.is_empty(), "行号损坏但文件存在的诊断引用应放行: {pass:?}");
    }

    fn diagnostic_contract_with_refs(refs: &[&str]) -> ReviewContract {
        ReviewContract {
            scope_statement: "排查默认对话工作区绑定根因".to_string(),
            obligations: vec![ReviewObligation {
                id: "root-cause".to_string(),
                question: "根因是否已定位并证实？".to_string(),
                status: "evidenced".to_string(),
                evidence_refs: refs.iter().map(|r| r.to_string()).collect(),
                waived_reason: None,
                verify_command: None,
                verify_script: None,
                verify_sha256: None,
            }],
        }
    }

    #[test]
    fn diagnostic_task_with_locatable_refs_passes_without_diff() {
        // 事故现场形态：sed 排查命令产生副作用（side_effect_count = 1），但无任何代码改动
        let ctx = DiffContext::from_diff_and_paths(None, &[]);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let blockers = review_contract_evidence_blockers(
            &diagnostic_contract_with_refs(&["src-tauri/src/agent/finish_guard/diff.rs:1"]),
            1,
            Some(&ctx),
            Some(root),
            None,
            &[]        );
        assert!(blockers.is_empty(), "诊断引用静态可定位时应放行: {blockers:?}");

        // 工作区相对路径（不带 src-tauri/ 前缀）同样可定位
        let blockers = review_contract_evidence_blockers(
            &diagnostic_contract_with_refs(&["src/agent/finish_guard/diff.rs:1"]),
            1,
            Some(&ctx),
            Some(root),
            None,
            &[]        );
        assert!(blockers.is_empty(), "工作区相对诊断引用应放行: {blockers:?}");
    }

    #[test]
    fn lying_writer_claiming_fix_with_empty_diff_is_blocked() {
        let ctx = DiffContext::from_diff_and_paths(None, &[]);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let delivery_text = "任务完成，已修复 main_loop.rs 的工作区绑定问题";
        let blockers = review_contract_evidence_blockers(
            &diagnostic_contract_with_refs(&["src/agent/finish_guard/diff.rs:1"]),
            1,
            Some(&ctx),
            Some(root),
            Some(delivery_text),
            &[],
        );
        assert_eq!(blockers.len(), 1, "声称修改但无 diff 必须拦截");
        assert!(blockers[0].contains("声称已修改/修复代码"));
    }

    #[test]
    fn fabricated_line_numbers_fail_static_check() {
        let ctx = DiffContext::from_diff_and_paths(None, &[]);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let blockers = review_contract_evidence_blockers(
            &diagnostic_contract_with_refs(&["src/agent/finish_guard/diff.rs:999999"]),
            1,
            Some(&ctx),
            Some(root),
            None,
            &[]        );
        assert!(!blockers.is_empty(), "行号越界的引用必须拦截");
        assert!(blockers[0].contains("未能在当前工作区源码中定位"));
    }

    #[test]
    fn real_diff_still_requires_evidence_inside_diff() {
        // 有真实改动时严格对账行为不变：证据命中 diff 放行，不在 diff 拦截
        let raw_diff = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -10,3 +10,4 @@\n fn a() {}\n";
        let ctx = DiffContext::from_diff_and_paths(Some(raw_diff), &[]);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

        let hit = review_contract_evidence_blockers(
            &diagnostic_contract_with_refs(&["src/lib.rs:10"]),
            1,
            Some(&ctx),
            Some(root),
            None,
            &[]        );
        assert!(hit.is_empty(), "证据落入真实 diff 应放行: {hit:?}");

        let miss = review_contract_evidence_blockers(
            &diagnostic_contract_with_refs(&["src/other_never_touched.rs:10"]),
            1,
            Some(&ctx),
            Some(root),
            None,
            &[]        );
        assert!(!miss.is_empty(), "证据未落入真实 diff 必须拦截");
    }

    #[test]
    fn noncode_evidence_skips_mechanical_gate_and_leaves_notice() {
        // 运维任务场景（pacman 卸载）：义务证据是命令输出/退出码，
        // file:line 对账域不存在 → 机械门不适用不拦截，但必须留下移交终审的 skip 原因
        let ctx = DiffContext::from_diff_and_paths(None, &[]);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let contract = diagnostic_contract_with_refs(&[
            "pacman -Qs workbuddy 返回空",
            "output_id: step_7 显示 package not found",
        ]);
        let blockers = review_contract_evidence_blockers(
            &contract,
            1,
            Some(&ctx),
            Some(root),
            None,
            &[]        );
        assert!(
            blockers.is_empty(),
            "非代码证据不应被 file:line 对账拦截: {blockers:?}"
        );
        let skips = crate::agent::finish_guard::noncode_evidence_skip_reasons(
            &contract,
            1,
            Some(&ctx),
            Some(root),
        );
        assert_eq!(skips.len(), 1, "跳过的义务必须留交接原因供终审核验");
        assert!(skips[0].contains("root-cause"));
        assert!(skips[0].contains("非代码形态"));
    }

    #[test]
    fn noncode_evidence_skips_even_with_real_diff() {
        // 有真实 diff 时同样适用：证据形态决定适用域，不是"有 diff 就强制 file:line"
        let raw_diff = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -10,3 +10,4 @@\n fn a() {}\n";
        let ctx = DiffContext::from_diff_and_paths(Some(raw_diff), &[]);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let contract = diagnostic_contract_with_refs(&["systemctl status x 返回 inactive（预期状态）"]);
        let blockers = review_contract_evidence_blockers(
            &contract,
            1,
            Some(&ctx),
            Some(root),
            None,
            &[]        );
        assert!(blockers.is_empty(), "非代码证据在有 diff 时同样不适用对账: {blockers:?}");
    }

    #[test]
    fn malformed_file_ref_stays_in_code_domain_and_blocks() {
        // src/x.rs:abc 是"编错的 file:line"——仍属代码证据域，必须拦截而非当非代码证据放行
        let ctx = DiffContext::from_diff_and_paths(None, &[]);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let contract =
            diagnostic_contract_with_refs(&["src/agent/finish_guard/diff.rs:abc"]);
        let blockers = review_contract_evidence_blockers(
            &contract,
            1,
            Some(&ctx),
            Some(root),
            None,
            &[]        );
        assert!(
            !blockers.is_empty(),
            "形似 file:line 但行号非法的引用必须拦截（代码证据域）"
        );
    }

    #[test]
    fn bare_existing_path_ref_is_code_domain() {
        // 裸路径且磁盘可定位 → 代码证据域（缺行号照样拦）
        let ctx = DiffContext::from_diff_and_paths(None, &[]);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let contract =
            diagnostic_contract_with_refs(&["src/agent/finish_guard/diff.rs"]);
        let blockers = review_contract_evidence_blockers(
            &contract,
            1,
            Some(&ctx),
            Some(root),
            None,
            &[]        );
        assert!(!blockers.is_empty(), "裸路径引用属代码域，缺行号应拦截");
    }

    #[test]
    fn mixed_code_and_noncode_refs_still_judge_the_code_part() {
        // 混合引用：代码域引用对账失败 → 拦截；非代码引用不参与判定
        let raw_diff = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -10,3 +10,4 @@\n fn a() {}\n";
        let ctx = DiffContext::from_diff_and_paths(Some(raw_diff), &[]);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let contract = diagnostic_contract_with_refs(&[
            "src/never_touched.rs:10",
            "cargo test 输出显示全部通过",
        ]);
        let blockers = review_contract_evidence_blockers(
            &contract,
            1,
            Some(&ctx),
            Some(root),
            None,
            &[]        );
        assert!(!blockers.is_empty(), "代码域引用未命中 diff 必须拦截");
        assert!(blockers[0].contains("never_touched"));
        // 混合场景不走"全非代码"跳过通道
        let skips = crate::agent::finish_guard::noncode_evidence_skip_reasons(
            &contract,
            1,
            Some(&ctx),
            Some(root),
        );
        assert!(skips.is_empty(), "含代码域引用的义务不生成 skip 交接");
    }

    #[test]
    fn bare_claim_without_observation_is_blocked_not_skipped() {
        // 三分法的第三条腿："已完成优化"这类裸结论既非代码位置也非观测记录，
        // 不能伪装成"非代码证据"逃过机械门——证据不成立仍拦截，且不产生 skip 交接
        let ctx = DiffContext::from_diff_and_paths(None, &[]);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let contract = diagnostic_contract_with_refs(&["已完成优化", "验证一切正常"]);
        let blockers = review_contract_evidence_blockers(
            &contract,
            1,
            Some(&ctx),
            Some(root),
            None,
            &[]        );
        assert!(!blockers.is_empty(), "裸结论不是证据，必须拦截");
        assert!(blockers[0].contains("不构成可核验"));
        let skips = crate::agent::finish_guard::noncode_evidence_skip_reasons(
            &contract,
            1,
            Some(&ctx),
            Some(root),
        );
        assert!(skips.is_empty(), "裸结论不生成 skip 交接");
    }


#[test]
fn review_finding_todos_cannot_be_silently_dropped() {
    let mut context = context::SandwichContext::new("system".to_string(), "goal".to_string());
    context.todo_list.push(crate::agent::types::TodoItem {
        id: 5,
        status: "pending".to_string(),
        description: "[审查发现] [QUESTION] at plan_gate.rs:135 冷却豁免白名单漏加新路由".to_string(),
        evidence_output_id: None,
    });

    // 模型提交的 todo_update 刻意剔除了审查发现条目 → 必须自动恢复
    let drop_instruction = AgentInstruction {
        tool: Some("todo_update".to_string()),
        todo_update: vec![crate::agent::types::TodoItem {
            id: 1,
            status: "done".to_string(),
            description: "无关待办".to_string(),
            evidence_output_id: None,
        }],
        ..Default::default()
    };
    apply_instruction_state_updates(&mut context, &drop_instruction, 10);

    assert!(
        context.todo_list.iter().any(|t| t.description.starts_with("[审查发现]")),
        "被剔除的审查发现必须自动恢复，禁止静默销账"
    );
    assert!(context.todo_list.iter().any(|t| t.description == "无关待办"));
}

#[test]
fn review_finding_kept_with_explicit_closure_is_accepted() {
    let mut context = context::SandwichContext::new("system".to_string(), "goal".to_string());
    context.todo_list.push(crate::agent::types::TodoItem {
        id: 5,
        status: "pending".to_string(),
        description: "[审查发现] [QUESTION] at plan_gate.rs:135 冷却豁免白名单漏加新路由".to_string(),
        evidence_output_id: None,
    });

    // 模型保留该条目并显式闭合（done）→ 合法销账，不恢复不告警
    let close_instruction = AgentInstruction {
        tool: Some("todo_update".to_string()),
        todo_update: vec![crate::agent::types::TodoItem {
            id: 5,
            status: "done".to_string(),
            description: "[审查发现] [QUESTION] at plan_gate.rs:135 冷却豁免白名单漏加新路由".to_string(),
            evidence_output_id: None,
        }],
        ..Default::default()
    };
    apply_instruction_state_updates(&mut context, &close_instruction, 10);

    assert_eq!(context.todo_list.len(), 1);
    assert_eq!(context.todo_list[0].status, "done");
}
}
