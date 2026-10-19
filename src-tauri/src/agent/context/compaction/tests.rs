use super::*;

use super::*;

#[test]
fn test_low_watermark_dehydration_fires_below_high_watermark() {
    let mut ctx = SandwichContext::new("SYS".to_string(), "Goal".to_string());
    // 1M 窗口：高水位 ≈255k 日常不可达，但历史超过 45k 应触发低水位批量脱水
    ctx.context_window_tokens = 1_000_000;

    let big_payload = "Data content line with information.\n".repeat(200);
    for i in 1..=25 {
        ctx.turns_history.push(ChatMessage {
            role: "assistant".to_string(),
            content: json!(format!("计划: 测试 | 工具: tool_{} | 动作: exec", i)),
        });
        ctx.turns_history.push(ChatMessage {
            role: "user".to_string(),
            content: json!(format!(
                "【执行输出】\noutput_id: out_{}\n{}",
                i, big_payload
            )),
        });
    }

    let before = ctx.history_estimated_tokens();
    assert!(before > ContextBudget::DEHYDRATION_WATERMARK_TOKENS);
    assert!(before < ctx.current_budget().high_watermark_tokens);

    ctx.maybe_compact_history();

    let after = ctx.history_estimated_tokens();
    assert!(after < before, "低水位批量脱水必须削减历史体积");
    // 固定尾窗保护区内消息保持原样（不被脱水）
    let tail_text = ctx
        .turns_history
        .last()
        .unwrap()
        .content
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        tail_text.contains("Data content line"),
        "保护区尾部必须原样保留"
    );
    // 保护区外老输出已定型为 output_id 存根
    let head_text = ctx.turns_history[1].content.as_str().unwrap().to_string();
    assert!(
        head_text.contains("完整详细数据已安全持久化至 SQLite"),
        "保护区外老输出必须脱水为句柄，实际头部: {}",
        head_text.chars().take(80).collect::<String>()
    );
}

#[test]
fn test_rule_based_summarizer_never_fails() {
    let summarizer = RuleBasedSummarizer;
    let messages = vec![
        ChatMessage {
            role: "assistant".to_string(),
            content: json!("计划: 测试 | 工具: shell_exec | 动作: cargo test"),
        },
        ChatMessage {
            role: "user".to_string(),
            content: json!("【执行输出】\noutput_id: 89a7\nFinished 0 errors"),
        },
    ];

    let summary = summarizer.summarize(&messages).unwrap();
    assert!(summary.contains("shell_exec"));
    assert!(summary.contains("output_id=89a7"));
}

#[test]
fn test_hybrid_summarizer_falls_back_gracefully() {
    let summarizer = HybridSummarizer {
        model_config: None,
        system_prompt: None,
        user_goal: None,
    };
    let messages = vec![
        ChatMessage {
            role: "assistant".to_string(),
            content: json!("计划: 依赖分析 | 工具: remote-hub | 动作: read_file"),
        },
        ChatMessage {
            role: "user".to_string(),
            content: json!("【执行输出】\noutput_id: c8db17f5\nCargo.toml contents..."),
        },
    ];

    let summary = summarizer.summarize(&messages).unwrap();
    assert!(summary.contains("remote-hub"));
    assert!(summary.contains("output_id=c8db17f5"));
}

#[test]
fn test_compaction_uses_current_goal_without_replaying_archived_goal() {
    let mut ctx = SandwichContext::new("SYS".to_string(), "初始目标：审计项目依赖".to_string());
    ctx.context_window_tokens = 8000;

    let big = "X".repeat(400);
    for i in 0..40 {
        ctx.turns_history.push(ChatMessage {
            role: "assistant".to_string(),
            content: json!(format!("Step {}: {}", i, big)),
        });
    }

    ctx.turns_history.push(ChatMessage {
        role: "user".to_string(),
        content: json!(
            "【任务目标更新】\n生成执行报告并写入 report.xlsx\n（原目标: 审计项目依赖）"
        ),
    });

    for i in 40..60 {
        ctx.turns_history.push(ChatMessage {
            role: "assistant".to_string(),
            content: json!(format!("Step {}: {}", i, big)),
        });
    }

    ctx.current_goal = "最新要求：只生成 nogpu 报告".into();

    ctx.maybe_compact_history();

    assert_eq!(
        ctx.user_goal, "最新要求：只生成 nogpu 报告",
        "压缩只能从当前任务状态刷新锚点"
    );
    assert_eq!(ctx.current_goal, "最新要求：只生成 nogpu 报告");
}

#[test]
fn compaction_does_not_promote_summary_bullets_to_memories() {
    let mut ctx = SandwichContext::new("SYS".into(), "Goal".into());
    ctx.context_window_tokens = 8000;
    ctx.memories.insert("explicit".into(), "明确保存的记忆".into());
    for _ in 0..60 {
        ctx.turns_history.push(ChatMessage {
            role: "assistant".into(),
            content: json!(format!("关键事实：可能是网络问题，尚未确认\n{}", "x".repeat(400))),
        });
    }
    ctx.maybe_compact_history();
    assert!(ctx.turns_history.iter().any(|m| m.content.as_str().is_some_and(|s| s.contains("【历史早期执行归档卡片"))));
    assert_eq!(ctx.memories.len(), 1, "列表外观不能把历史猜测变成工作记忆");
    assert_eq!(ctx.memories["explicit"], "明确保存的记忆");
}

#[test]
fn rule_fallback_retains_observations_after_dehydration_with_a_bounded_index() {
    let original = ChatMessage { role: "user".into(), content: json!(format!(
        "【执行输出】\noutput_id: compile-1\n⚠️ [退出码非零] (exit_code: 1)\n$ mvn compile\n{}\ncannot find symbol SomeClass",
        "过程日志\n".repeat(1000)
    )) };
    let dehydrated = crate::agent::context::assembler::dehydrate_historical_turn(&original);
    let summary = RuleBasedSummarizer.summarize(&[dehydrated]).unwrap();
    assert!(summary.contains("exit_code: 1"));
    assert!(summary.contains("cannot find symbol SomeClass"));
    assert!(summary.contains("output_id=compile-1"));
    assert!(summary.contains("节选"));
    assert!(summary.chars().count() < 250);
}

#[test]
fn compaction_keeps_archive_references_and_legacy_goal_fallback() {
    let mut ctx = SandwichContext::new("SYS".into(), "旧检查点锚点".into());
    ctx.current_goal.clear(); // 旧 checkpoint 无 current_goal，仍回退既有锚点。
    ctx.context_window_tokens = 8000;
    for i in 0..60 {
        ctx.turns_history.push(ChatMessage {
            role: "assistant".into(),
            content: json!(format!("step {} {}", i, "x".repeat(400))),
        });
        ctx.turns_history.push(ChatMessage {
            role: "user".into(),
            content: json!(format!("【执行输出】\noutput_id: out-{}\n{}", i, "y".repeat(400))),
        });
    }
    ctx.maybe_compact_history();
    assert_eq!(ctx.user_goal, "旧检查点锚点");
    assert!(ctx.current_goal.is_empty());
    let archive = ctx.turns_history.iter().find_map(|m| m.content.as_str()
        .filter(|s| s.starts_with("【历史早期执行归档卡片"))).unwrap();
    assert!(archive.contains("output_id=out-0"));
    assert!(archive.contains("output_id=out-1"));
}

#[test]
fn test_compaction_drain_aligns_and_reduces_tokens() {
    let mut ctx = SandwichContext::new("SYS".to_string(), "Goal".to_string());
    ctx.context_window_tokens = 8000;

    // 模拟多轮对话：大输出带 output_id（可被脱水定型）
    let big_payload = "Data content line with information.\n".repeat(40);
    for i in 1..=10 {
        ctx.turns_history.push(ChatMessage {
            role: "assistant".to_string(),
            content: json!(format!("计划: 测试 | 工具: tool_{} | 动作: exec", i)),
        });
        ctx.turns_history.push(ChatMessage {
            role: "user".to_string(),
            content: json!(format!(
                "【执行输出 (v2)】\noutput_id: out_{}\n{}",
                i, big_payload
            )),
        });
    }

    let before_tokens = ctx.history_estimated_tokens();
    assert!(before_tokens > ctx.current_budget().high_watermark_tokens);

    ctx.maybe_compact_history();

    let after_tokens = ctx.history_estimated_tokens();
    // 新契约：高水位事件内先一次性脱水定型；本场景脱水即可回到水位线下，
    // 无需归档切除（保持历史的字节连续性优先）。
    assert!(after_tokens <= ctx.current_budget().high_watermark_tokens);
    assert!(after_tokens < before_tokens);

    // 保护区外的旧输出已定型为证据句柄（存根），原文不再驻留上下文
    let full_text = serde_json::to_string(&ctx.turns_history).unwrap();
    assert!(full_text.contains("完整详细数据已安全持久化至 SQLite"));
}

#[test]
fn reality_anchored_watermark_prevents_premature_dehydration() {
    let mut ctx = SandwichContext::new("SYS".to_string(), "Goal".to_string());
    ctx.context_window_tokens = 200_000;

    // 本地估算视角：塞入大量中文文本（保守估算约 150K+ tokens）
    let big_cjk = "审查要点覆盖资源作用域契约、上下文压缩水位与工具参数契约等领域的细节描述。"
        .repeat(600);
    for i in 0..18 {
        ctx.turns_history
            .push(crate::agent::context::types::ChatMessage {
                role: "user".to_string(),
                content: json!(format!("step {} {}", i, big_cjk)),
            });
    }
    let local_estimate = ctx.history_estimated_tokens();
    assert!(
        local_estimate > ctx.current_budget().high_watermark_tokens,
        "前置条件：本地估算应已超高水位（实际 {}）",
        local_estimate
    );

    // 服务端真实回传：总 prompt 仅 30K（真实占用很低）→ 不得触发脱水/压缩。
    // 快照与真实值必须来自同一次完整请求；当前无新增历史。
    ctx.last_reported_prompt_tokens = Some(30_000);
    ctx.last_reported_history_estimate_tokens = Some(local_estimate);
    let before = ctx.turns_history.clone();
    ctx.maybe_compact_history();
    assert_eq!(
        serde_json::to_string(&ctx.turns_history).unwrap(),
        serde_json::to_string(&before).unwrap(),
        "真实占用远低于水位时不得脱水（历史必须字节不变）"
    );

    // 同一请求边界的真实总 prompt 接近上限时，必须触发并回压。
    let before_high = serde_json::to_string(&ctx.turns_history).unwrap();
    ctx.last_reported_prompt_tokens = Some(140_000);
    ctx.maybe_compact_history();
    let after_high = serde_json::to_string(&ctx.turns_history).unwrap();
    assert_ne!(
        before_high, after_high,
        "真实占用超高水位时必须触发压缩（脱水或归档其一）"
    );
    assert!(
        ctx.history_estimated_tokens() <= ctx.current_budget().low_watermark_tokens + 1_000,
        "高水位压缩后必须回压到低水位附近"
    );
}

#[test]
fn real_baseline_plus_double_branch_delta_triggers_compaction() {
    let mut ctx = SandwichContext::new("SYS".to_string(), "Goal".to_string());
    ctx.context_window_tokens = 32_768;
    ctx.turns_history.push(ChatMessage {
        role: "assistant".to_string(),
        content: json!("baseline plan"),
    });
    ctx.turns_history.push(ChatMessage {
        role: "user".to_string(),
        content: json!(format!(
            "【执行输出】\noutput_id: baseline\n{}",
            "seed ".repeat(2_400)
        )),
    });
    let history_at_request = ctx.history_estimated_tokens();
    ctx.last_reported_prompt_tokens = Some(12_000);
    ctx.last_reported_history_estimate_tokens = Some(history_at_request);

    for branch in 0..2 {
        ctx.turns_history.push(ChatMessage {
            role: "assistant".to_string(),
            content: json!(format!("parallel branch {} completed", branch)),
        });
        ctx.turns_history.push(ChatMessage {
            role: "user".to_string(),
            content: json!(format!(
                "【执行输出】\noutput_id: branch_{}\n{}",
                branch,
                "x".repeat(16_000)
            )),
        });
    }

    let current_tokens = ctx.history_estimated_tokens();
    let added_history = current_tokens.saturating_sub(history_at_request);
    let budget = ctx.current_budget();
    let prompt_high_watermark = budget
        .static_prefix_tokens
        .saturating_add(budget.tail_state_tokens)
        .saturating_add(budget.high_watermark_tokens);
    assert!(added_history > 8_000, "双分支新增量必须覆盖旧 6K 缺口");
    assert!(12_000 + added_history > prompt_high_watermark);

    let before = serde_json::to_string(&ctx.turns_history).unwrap();
    ctx.maybe_compact_history();
    assert_ne!(before, serde_json::to_string(&ctx.turns_history).unwrap());
    assert!(ctx.history_estimated_tokens() <= ctx.current_budget().high_watermark_tokens);
}

#[test]
fn zero_usage_falls_back_to_conservative_history_estimate() {
    let mut ctx = SandwichContext::new("SYS".to_string(), "Goal".to_string());
    ctx.context_window_tokens = 32_768;
    for i in 0..10 {
        ctx.turns_history.push(ChatMessage {
            role: "user".to_string(),
            content: json!(format!(
                "step {} {}",
                i,
                "plain heavy narrative ".repeat(1_200)
            )),
        });
    }
    let before = ctx.history_estimated_tokens();
    assert!(before > ctx.current_budget().high_watermark_tokens);
    ctx.last_reported_prompt_tokens = Some(0);
    ctx.last_reported_history_estimate_tokens = Some(before);
    ctx.maybe_compact_history();
    assert!(ctx.history_estimated_tokens() < before);
}

#[test]
fn test_compaction_drains_when_dehydration_is_insufficient() {
    let mut ctx = SandwichContext::new("SYS".to_string(), "Goal".to_string());
    ctx.context_window_tokens = 8000;

    // 不可脱水的重型纯文本（无 output_id 标记）：脱水无法缩小，
    // 必须走归档切除 + 摘要卡路径。
    let heavy = "Plain heavy narrative without output markers. ".repeat(30);
    for i in 1..=10 {
        ctx.turns_history.push(ChatMessage {
            role: "assistant".to_string(),
            content: json!(format!("计划: 阶段 {}", i)),
        });
        ctx.turns_history.push(ChatMessage {
            role: "user".to_string(),
            content: json!(format!("step {} {}", i, heavy)),
        });
    }
    assert!(ctx.history_estimated_tokens() > ctx.current_budget().high_watermark_tokens);

    ctx.maybe_compact_history();

    let after_tokens = ctx.history_estimated_tokens();
    assert!(after_tokens <= ctx.current_budget().low_watermark_tokens + 800);
    let first_msg = ctx.turns_history[0].content.as_str().unwrap();
    assert!(
        first_msg.contains("【历史早期执行归档"),
        "脱水不足时必须产生归档摘要卡，实际头部: {}",
        first_msg.chars().take(80).collect::<String>()
    );
}

#[test]
fn test_pairing_invariant_never_broken_by_compaction() {
    let mut ctx = SandwichContext::new("SYS".to_string(), "Goal".to_string());
    ctx.context_window_tokens = 8000;

    for i in 1..=8 {
        ctx.turns_history.push(ChatMessage {
            role: "assistant".to_string(),
            content: json!(format!("计划: 测试 | 工具: tool_{} | 动作: exec", i)),
        });
        ctx.turns_history.push(ChatMessage {
            role: "user".to_string(),
            content: json!(format!(
                "【执行输出 (v3)】\noutput_id: out_{}\n{}",
                i,
                "Large log payload content line\n".repeat(30)
            )),
        });
    }

    ctx.maybe_compact_history();

    // 验证压缩后活跃历史中不存在孤立的 user 或未配对结构
    let non_card_turns: Vec<&ChatMessage> = ctx
        .turns_history
        .iter()
        .filter(|m| match &m.content {
            serde_json::Value::String(s) => !s.contains("【历史早期执行归档"),
            _ => true,
        })
        .collect();

    if !non_card_turns.is_empty() {
        // 第一条非卡片消息必须是 assistant
        assert_eq!(
            non_card_turns[0].role, "assistant",
            "首条活跃历史消息必须是 assistant"
        );
        // 必须严格交替出现 assistant -> user
        for (idx, msg) in non_card_turns.iter().enumerate() {
            if idx % 2 == 0 {
                assert_eq!(msg.role, "assistant", "偶数索引必须是 assistant");
            } else {
                assert_eq!(msg.role, "user", "奇数索引必须是 user");
            }
        }
    }
}

#[test]
fn test_prompt_prefix_byte_stability_across_steps() {
    let mut ctx = SandwichContext::new("SYS PROMPT".to_string(), "GOAL".to_string());
    ctx.context_window_tokens = 32768;

    let instruction = crate::agent::types::AgentInstruction {
        tool: Some("exec".to_string()),
        description: "run test".to_string(),
        ..Default::default()
    };

    ctx.add_step(&instruction, "Output step 1", "out_1");
    let step1_snapshot = ctx.turns_history[1].content.as_str().unwrap().to_string();

    ctx.add_step(&instruction, "Output step 2", "out_2");
    let step1_after_step2 = ctx.turns_history[1].content.as_str().unwrap().to_string();

    // 严格字节级前缀一致性断言：历史追加绝不改变旧历史的任何字节（KV Cache 100% 命中）
    assert_eq!(
        step1_snapshot, step1_after_step2,
        "步骤 1 的输出内容在步骤 2 追加后必须 100% 字节不变"
    );
}

#[test]
fn test_sample_output_token_cap_strict_invariant() {
    let mut ctx = SandwichContext::new("SYS".to_string(), "Goal".to_string());
    ctx.context_window_tokens = 16000;

    let instruction = crate::agent::types::AgentInstruction {
        tool: Some("exec".to_string()),
        description: "run huge test".to_string(),
        ..Default::default()
    };

    // 巨型全中文 + 密集转义 JSON + 巨量报错
    let mut giant_output =
        "这是一段非常非常长的中文字符串用于测试单步硬截断闭环保证。".repeat(200);
    giant_output.push_str("\n{\"dense_json_key\": \"dense_val\", \"nested\": [1,2,3,4,5]}\n");
    for i in 0..500 {
        giant_output.push_str(&format!(
            "error: database connection timeout occurred at step {}\n",
            i
        ));
    }

    ctx.add_step(&instruction, &giant_output, "giant_out");

    let last_user_msg = ctx.turns_history.last().unwrap();
    let content_str = match &last_user_msg.content {
        serde_json::Value::String(s) => s.as_str(),
        _ => "",
    };

    let est = conservative_token_estimate(content_str);
    assert!(
        est <= ContextBudget::SINGLE_OBSERVATION_INGRESS_CAP,
        "闭环复检保证：截断后输出 Token 量 ({}) 必须 <= 上限 ({})",
        est,
        ContextBudget::SINGLE_OBSERVATION_INGRESS_CAP
    );
    assert!(
        content_str.contains("search_step_output"),
        "提示中必须包含 search_step_output 检索指引"
    );
}

#[test]
fn test_long_running_fuzz_never_overflows_and_watermark_gap() {
    let mut ctx = SandwichContext::new("SYS".to_string(), "Goal".to_string());
    ctx.context_window_tokens = 8000; // 较小窗口，测试高频压缩与边界稳定性

    let instruction = crate::agent::types::AgentInstruction {
        tool: Some("bash".to_string()),
        description: "fuzz step".to_string(),
        ..Default::default()
    };

    // 模拟 40 步长程任务，每步输入从 500 到 20,000 字符随机波动
    for step in 1..=40 {
        let payload_size = (step * 7919) % 20000 + 500;
        let payload = format!("Step {} payload: {}\n", step, "X".repeat(payload_size));
        ctx.add_step(&instruction, &payload, &format!("fuzz_{}", step));

        let current_tokens = ctx.history_estimated_tokens();
        let budget = ctx.current_budget();

        // 核心不变量 1：任何时刻历史 Tokens 绝不超出 max_allowed_history_tokens + 500 缓冲
        assert!(
            current_tokens <= budget.max_allowed_history_tokens + 1000,
            "第 {} 步：历史 Tokens ({}) 超出了最大安全历史上限 ({})",
            step,
            current_tokens,
            budget.max_allowed_history_tokens
        );
    }

    // 核心不变量 2：压缩后必须有摘要归档卡存在
    let has_summary_cards = ctx.turns_history.iter().any(|m| match &m.content {
        serde_json::Value::String(s) => s.contains("【历史早期执行归档卡片"),
        _ => false,
    });
    assert!(has_summary_cards, "长任务多轮压缩后必须留存结构化归档卡片");
}
