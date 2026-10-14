    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_summary_and_detail_are_normalized_without_rejecting_instruction() {
        let instruction: AgentInstruction = serde_json::from_value(json!({
            "tool": "finish",
            "command": { "summary": "done" },
            "memories_update": [{
                "key": "workbook",
                "summary": "工作簿已创建",
                "detail": "路径: /tmp/report.xlsx"
            }],
            "clear_history": true
        }))
        .expect("legacy memory fields should remain parseable");

        assert_eq!(instruction.get_action(), "finish");
        assert_eq!(instruction.memories_update.len(), 1);
        assert_eq!(instruction.memories_update[0].key, "workbook");
        assert_eq!(
            instruction.memories_update[0].value,
            "工作簿已创建\n路径: /tmp/report.xlsx"
        );
    }

    #[test]
    fn canonical_value_takes_precedence_over_legacy_fields() {
        let memory: MemoryItem = serde_json::from_value(json!({
            "key": "goal",
            "value": "canonical",
            "summary": "ignored",
            "detail": "ignored"
        }))
        .expect("canonical memory should parse");
        assert_eq!(memory.value, "canonical");
    }

    #[test]
    fn executable_shape_rejects_state_only_instruction() {
        let instruction: AgentInstruction = serde_json::from_value(json!({
            "plan_title": "核验完成语义",
            "description": "下一步应读取主循环",
            "progress_summary": "已建立审查义务"
        }))
        .expect("state-only JSON remains deserializable for recoverable validation");

        let error = instruction
            .validate_executable_shape()
            .expect_err("state-only object must not execute as implicit core");
        assert!(error.contains("缺少可执行入口"));
    }

    #[test]
    fn executable_shape_keeps_existing_action_forms_compatible() {
        let tool: AgentInstruction = serde_json::from_value(json!({
            "tool": "read_text_file",
            "command": {"path": "src/lib.rs"}
        }))
        .expect("tool instruction should deserialize");
        assert!(tool.validate_executable_shape().is_ok());

        let legacy_action: AgentInstruction = serde_json::from_value(json!({
            "action": "finish",
            "command": {"summary": "done"}
        }))
        .expect("legacy action instruction should deserialize");
        assert!(legacy_action.validate_executable_shape().is_ok());

        let pipeline: AgentInstruction = serde_json::from_value(json!({
            "commands": [{"tool": "read_text_file", "command": {"path": "src/lib.rs"}}]
        }))
        .expect("pipeline instruction should deserialize");
        assert!(pipeline.validate_executable_shape().is_ok());

        let plan_tree: AgentInstruction = serde_json::from_value(json!({
            "plan_tree": {"mode": "serial", "children": []}
        }))
        .expect("plan tree instruction should deserialize");
        assert!(plan_tree.validate_executable_shape().is_ok());
    }

    #[test]
    fn test_dispatch_context_parsing_and_validation() {
        // 空值或 null
        assert!(parse_dispatch_context(None).unwrap().is_none());
        assert!(parse_dispatch_context(Some(&serde_json::Value::Null)).unwrap().is_none());
        assert!(parse_dispatch_context(Some(&json!({}))).unwrap().is_none());

        // 非对象被拒绝
        assert!(parse_dispatch_context(Some(&json!("string"))).is_err());

        // 正常结构化解析
        let val = json!({
            "background": "项目使用 Tauri + Vue3",
            "todo_slice": ["修复单测", "格式化代码"],
            "file_hints": ["src-tauri/src/agent/child_writer.rs"],
            "prior_findings": ["命令超时已修复"]
        });
        let parsed = parse_dispatch_context(Some(&val)).unwrap().expect("should parse successfully");
        assert_eq!(parsed.background.as_deref(), Some("项目使用 Tauri + Vue3"));
        assert_eq!(parsed.todo_slice.len(), 2);
        assert_eq!(parsed.file_hints.len(), 1);
        assert_eq!(parsed.prior_findings.len(), 1);
    }

    #[test]
    fn test_is_task_thin_heuristics() {
        // 明显的过薄任务（无换行、无路径、无命令，短文本）
        assert!(is_task_thin("修复那个 bug"));
        assert!(is_task_thin("优化一下代码"));
        assert!(is_task_thin(""));

        // 包含文件特征，不属于薄任务
        assert!(!is_task_thin("修复 index.ts 的拼写错误"));
        assert!(!is_task_thin("检查 src/lib.rs"));
        assert!(!is_task_thin("修改 package.json 的版本号"));

        // 包含命令特征，不属于薄任务
        assert!(!is_task_thin("执行 npm test 解决报错"));
        assert!(!is_task_thin("运行 cargo test"));

        // 较长且自包含的指令，不属于薄任务
        assert!(!is_task_thin("请重构用户认证模块的持久化逻辑，增加登录重试机制与失败状态回滚"));

        // 包含换行多段落的指令，不属于薄任务
        assert!(!is_task_thin("任务说明：\n1. 检查配置\n2. 补充日志"));
    }

    #[test]
    fn test_agent_instruction_dispatches_helpers() {
        // 1. 单发（未提供 dispatches）：has_dispatches 为 false，get_dispatch_items 返回 params
        let single = json!({
            "tool": "child_worker",
            "params": { "task": "单人任务" }
        });
        let inst: AgentInstruction = serde_json::from_value(single).unwrap();
        assert!(!inst.has_dispatches());
        assert!(!inst.is_mixed_dispatch_format());
        let items = inst.get_dispatch_items();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["task"], "单人任务");

        // 2. 纯批发（dispatches 数组）：has_dispatches 为 true
        let batch = json!({
            "tool": "child_worker",
            "dispatches": [
                { "task": "任务A" },
                { "task": "任务B" }
            ]
        });
        let inst_b: AgentInstruction = serde_json::from_value(batch).unwrap();
        assert!(inst_b.has_dispatches());
        assert!(!inst_b.is_mixed_dispatch_format());
        assert_eq!(inst_b.get_dispatch_items().len(), 2);

        // 3. 混合格式：既有 dispatches 又有 params
        let mixed = json!({
            "tool": "child_worker",
            "params": { "task": "混淆任务" },
            "dispatches": [
                { "task": "任务1" }
            ]
        });
        let inst_m: AgentInstruction = serde_json::from_value(mixed).unwrap();
        assert!(inst_m.has_dispatches());
        assert!(inst_m.is_mixed_dispatch_format());
    }

    #[test]
    fn test_next_tool_hint_multiple_formats() {
        // 1. 逗号分隔字符串
        let json_comma = json!({
            "tool": "search",
            "next_tool_hint": "code_diagnosis, code_development, code_verify"
        });
        let inst: AgentInstruction = serde_json::from_value(json_comma).unwrap();
        assert_eq!(
            inst.get_next_tool_hints(),
            vec!["code_diagnosis", "code_development", "code_verify"]
        );

        // 2. JSON 数组多选
        let json_array = json!({
            "tool": "search",
            "next_tool_hint": ["chart_visualization", "frontend_design"]
        });
        let inst_arr: AgentInstruction = serde_json::from_value(json_array).unwrap();
        assert_eq!(
            inst_arr.get_next_tool_hints(),
            vec!["chart_visualization", "frontend_design"]
        );

        // 3. 中文标点、空格、分号混合容错
        let json_mixed = json!({
            "tool": "search",
            "next_tool_hint": "code_development；code_verify，deep_research、chart_visualization"
        });
        let inst_mixed: AgentInstruction = serde_json::from_value(json_mixed).unwrap();
        assert_eq!(
            inst_mixed.get_next_tool_hints(),
            vec!["code_development", "code_verify", "deep_research", "chart_visualization"]
        );

        // 4. 未提供 hint 时回退至 action
        let json_no_hint = json!({
            "tool": "remote-hub/read_text_file"
        });
        let inst_fallback: AgentInstruction = serde_json::from_value(json_no_hint).unwrap();
        assert_eq!(inst_fallback.get_next_tool_hints(), vec!["remote-hub/read_text_file"]);

        // 5. finish 或 core 动作自动过滤不加载
        let json_finish = json!({
            "tool": "finish"
        });
        let inst_finish: AgentInstruction = serde_json::from_value(json_finish).unwrap();
        assert!(inst_finish.get_next_tool_hints().is_empty());
    }

    #[test]
    fn prepare_finish_is_bare_executable_control_op() {
        // 纯预检：无工具入口也应通过可执行形状校验
        let inst: AgentInstruction = serde_json::from_value(json!({
            "prepare_finish": { "summary": "修复完成，待交付" }
        }))
        .unwrap();
        assert!(inst.is_bare_prepare_finish());
        assert!(!inst.has_executable_entry());
        assert!(inst.validate_executable_shape().is_ok());

        // 与工具混发 → 仍具可执行入口，run_tool_phase 会按协议误用拒绝
        let mixed: AgentInstruction = serde_json::from_value(json!({
            "tool": "search",
            "params": {"query": "x"},
            "prepare_finish": {}
        }))
        .unwrap();
        assert!(mixed.has_executable_entry());
        assert!(!mixed.is_bare_prepare_finish());
    }

    #[test]
    fn standalone_action_wrapped_as_tool_is_unwrapped() {
        // {"tool":"submit_plan","command":{...}} → 解包为顶层 submit_plan
        let inst: AgentInstruction = serde_json::from_value(json!({
            "tool": "submit_plan",
            "command": {
                "plan_summary": "修复菜单样式：重写 .budget-dropdown-menu 定位",
                "target_blocks": [{"id":"b1","objective":"修样式","checklist":["验证"],"scope_files":[]}],
                "target_files": ["src/components/ProjectWorkspace.vue"]
            }
        }))
        .unwrap();
        let unwrapped = inst
            .unwrap_standalone_tool_alias()
            .expect("wrapped submit_plan should unwrap");
        assert!(unwrapped.submit_plan.is_some());
        assert!(unwrapped.is_bare_plan_submission());
        assert!(!unwrapped.has_executable_entry());

        // params 承载同样解包
        let inst_p: AgentInstruction = serde_json::from_value(json!({
            "tool": "prepare_finish",
            "params": { "summary": "done" }
        }))
        .unwrap();
        let u2 = inst_p
            .unwrap_standalone_tool_alias()
            .expect("wrapped prepare_finish should unwrap");
        assert!(u2.is_bare_prepare_finish());

        // 非顶层动作名 → 不解包
        let normal: AgentInstruction = serde_json::from_value(json!({
            "tool": "search",
            "command": {"query": "x"}
        }))
        .unwrap();
        assert!(normal.unwrap_standalone_tool_alias().is_none());

        // 混发（submit_plan + 真工具）→ 不解包，留协议误用分支判
        let mixed: AgentInstruction = serde_json::from_value(json!({
            "tool": "submit_plan",
            "command": {"plan_summary": "x"},
            "commands": [{"tool": "search", "command": {"query": "y"}}]
        }))
        .unwrap();
        assert!(mixed.unwrap_standalone_tool_alias().is_none());
    }

#[test]
fn status_action_mixed_with_tool_drops_the_tool_entry() {
    // submit_plan + 工具调用：保留 submit_plan，丢弃工具入口并记录
    let mut mixed: AgentInstruction = serde_json::from_value(json!({
        "submit_plan": {"plan_summary": "x"},
        "tool": "local_shell",
        "command": {"command": "ls"}
    }))
    .unwrap();
    assert!(mixed.has_executable_entry());
    assert!(mixed.strip_entry_when_status_action());
    assert!(!mixed.has_executable_entry());
    assert!(mixed.is_bare_plan_submission());
    assert!(mixed.ignored_mixed_entry_notice().unwrap().contains("local_shell"));

    // prepare_finish + parallel 同理
    let mut pf: AgentInstruction = serde_json::from_value(json!({
        "prepare_finish": {"summary": "done"},
        "parallel": [{"tool": "local_file"}]
    }))
    .unwrap();
    assert!(pf.strip_entry_when_status_action());
    assert!(pf.is_bare_prepare_finish());

    // 纯状态动作 / 纯工具调用 / request_retriage 混发不动
    let mut bare: AgentInstruction =
        serde_json::from_value(json!({"submit_plan": {"plan_summary": "x"}})).unwrap();
    assert!(!bare.strip_entry_when_status_action());
    assert!(bare.ignored_mixed_entry_notice().is_none());
    let mut tool_only: AgentInstruction =
        serde_json::from_value(json!({"tool": "local_shell", "command": {"command": "ls"}})).unwrap();
    assert!(!tool_only.strip_entry_when_status_action());
    assert!(tool_only.has_executable_entry());
}


#[test]
fn empty_parallel_placeholder_preserves_single_step() {
    // 对应实际失败形状：有效 local_shell 命令旁携带未使用的 parallel: []。
    let instruction: AgentInstruction = serde_json::from_str(
        r#"{"tool":"local_shell","command":{"command":"printf ok","timeout_secs":120},"parallel":[]}"#,
    ).unwrap();
    assert!(instruction.parallel.is_none());
    assert_eq!(instruction.get_tool(), "local_shell");
    assert_ne!(instruction.get_action(), "parallel");
    assert_eq!(instruction.get_params(), json!({"command":"printf ok","timeout_secs":120}));
    assert!(instruction.validate_executable_shape().is_ok());
    let restored: AgentInstruction = serde_json::from_value(serde_json::to_value(&instruction).unwrap()).unwrap();
    assert!(restored.parallel.is_none());
    assert_eq!(restored.get_params(), instruction.get_params());
}

#[test]
fn empty_parallel_alone_is_not_an_executable_action() {
    let instruction: AgentInstruction = serde_json::from_value(json!({"parallel":[]})).unwrap();
    assert!(instruction.parallel.is_none());
    assert!(!instruction.has_executable_entry());
    assert!(instruction.validate_executable_shape().is_err());
}

#[test]
fn nonempty_parallel_and_invalid_types_keep_existing_semantics() {
    let branches = json!([{"tool":"local_file","command":{"path":"a.txt"}}]);
    let instruction: AgentInstruction = serde_json::from_value(json!({"parallel":branches})).unwrap();
    assert_eq!(instruction.get_tool(), "parallel");
    assert_eq!(instruction.get_action(), "parallel");
    assert_eq!(instruction.get_params()["parallel"], branches);
    for invalid in [json!(true), json!(1), json!({}), json!("not-an-array")] {
        assert!(serde_json::from_value::<AgentInstruction>(json!({"parallel":invalid})).is_err());
    }
}

#[test]
fn empty_parallel_compatibility_does_not_clear_tool_parameter_arrays() {
    let params = json!({"parallel":[],"job_ids":[],"content":""});
    let instruction: AgentInstruction = serde_json::from_value(json!({
        "tool":"example_tool", "params":params, "parallel":[]
    })).unwrap();
    assert!(instruction.parallel.is_none());
    assert_eq!(instruction.get_params(), params);
    let instruction: AgentInstruction = serde_json::from_value(json!({
        "tool":"example_tool", "command":params, "parallel":[]
    })).unwrap();
    assert_eq!(instruction.get_params(), params);
}

#[test]
fn shell_params_instruction_canonical_and_shorthand_forms_agree() {
    for raw in [json!({"tool":"local_shell","params":{"command":"pwd"}}),
        json!({"tool":"local_shell","command":{"command":"pwd"}}),
        json!({"tool":"local_shell","command":"pwd"}),
        json!({"tool":"LOCAL_SHELL","params":"pwd"})] {
        let inst: AgentInstruction=serde_json::from_value(raw).unwrap();
        assert_eq!(inst.get_params(),json!({"command":"pwd"}));
        let restored: AgentInstruction=serde_json::from_value(serde_json::to_value(&inst).unwrap()).unwrap();
        assert_eq!(restored.get_params(),inst.get_params());
    }
}
#[test]
fn shell_params_instruction_shorthand_preserves_options_in_params_and_sibling_fields() {
    let inst: AgentInstruction=serde_json::from_value(json!({"tool":"local_shell","command":"sh",
        "params":{"cwd":"sub","args":["-c","printf ok"],"env":{"CHECK":"ok"}},
        "timeout_secs":90,"background":false})).unwrap();
    assert_eq!(inst.get_params(),json!({"command":"sh","cwd":"sub","args":["-c","printf ok"],
        "env":{"CHECK":"ok"},"timeout_secs":90,"background":false}));
}
#[test]
fn shell_params_other_tools_and_mcp_namespaces_are_not_coerced() {
    for tool in ["local_file","remote/local_shell","remote/shell_exec"] {
        let inst: AgentInstruction=serde_json::from_value(json!({"tool":tool,"command":"a b"})).unwrap();
        assert_eq!(inst.get_params(),json!("a b"));
        assert!(inst.resolved_local_shell_params().is_none());
    }
}
#[test]
fn shell_params_truncated_json_is_still_rejected_before_normalization() {
    assert!(serde_json::from_str::<AgentInstruction>(r#"{"tool":"local_shell","command":"echo"#).is_err());
}
