//! One turn of the main profile. Iteration is owned by agent::execution.
use super::*;
use std::ops::ControlFlow;

pub(super) struct MainExecution<'a, 'b> {
    pub(super) app: AppHandle,
    pub(super) state: State<'a, DbState>,
    pub(super) registry_state: State<'b, Arc<tokio::sync::Mutex<PluginRegistry>>>,
    pub(super) model_routing: crate::agent::router::ModelRoutingConfig,
    pub(super) goal: String,
    pub(super) auto_pilot: bool,
    pub(super) final_session_id: String,
    pub(super) log_context: Option<bool>,
    pub(super) transient_llm_request_retries: u32,
    pub(super) run_id: String,
    pub(super) plan_mode: Option<String>,
    pub(super) run_budget: Option<AgentRunBudget>,
    pub(super) rx: mpsc::Receiver<HumanControl>,
    pub(super) interrupt_rx: watch::Receiver<u64>,
    pub(super) stall_probe: Arc<std::sync::atomic::AtomicU64>,
    pub(super) run_trace_steps: Vec<serde_json::Value>,
    pub(super) run_machine: AgentRunMachine,
    pub(super) is_explicit_workspace: bool,
    pub(super) workspace_dir: std::path::PathBuf,
    pub(super) run_started_at: chrono::DateTime<Utc>,
    pub(super) conversation_model_id: String,
    pub(super) context: context::SandwichContext,
    pub(super) checkpoint: AgentCheckpoint,
    pub(super) git_baseline: crate::agent::reviewer_gatekeeper::GitBaseline,
    pub(super) task_started_at: chrono::DateTime<Utc>,
    pub(super) recovered_instruction: Option<AgentInstruction>,
    pub(super) action_history: std::collections::VecDeque<String>,
    pub(super) max_steps: usize,
    pub(super) spent_prompt_tokens: i64,
    pub(super) budget_stop_reason: Option<String>,
}

pub(super) async fn turn_main<'a, 'b>(
    state: Box<MainExecution<'a, 'b>>,
) -> Result<ControlFlow<Option<Box<MainExecution<'a, 'b>>>, Box<MainExecution<'a, 'b>>>, String> {
    if state.checkpoint.step_id >= state.max_steps && !state.checkpoint.auto_plan_approval_pending {
        return Ok(ControlFlow::Break(Some(state)));
    }
    let MainExecution {
        app,
        state,
        registry_state,
        model_routing,
        goal,
        auto_pilot,
        final_session_id,
        log_context,
        transient_llm_request_retries,
        run_id,
        plan_mode,
        run_budget,
        mut rx,
        mut interrupt_rx,
        stall_probe,
        mut run_trace_steps,
        mut run_machine,
        is_explicit_workspace,
        workspace_dir,
        run_started_at,
        conversation_model_id,
        mut context,
        mut checkpoint,
        git_baseline,
        task_started_at,
        mut recovered_instruction,
        mut action_history,
        max_steps,
        mut spent_prompt_tokens,
        mut budget_stop_reason,
    } = *state;
    let flow = 'turn: {
        crate::agent::agent_mailbox::deliver_main(
            state.inner(),
            &crate::agent::agent_mailbox::Actor::new(&run_id, &final_session_id),
            &mut context,
            &mut checkpoint,
        )?;

        if crate::agent::workspace_access::sync_context(
            &final_session_id,
            &workspace_dir,
            &mut context,
        ) {
            context.add_error_feedback(
                "【目录权限更新】使用当前权限表；原任务、方案和审查状态保持不变。",
            );
        }
        // 心跳：每轮步边界刷新——停滞看门狗据此区分"慢"与"死"
        stall_probe.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            std::sync::atomic::Ordering::Relaxed,
        );
        crate::agent::control::mark_stage(&final_session_id, "loop_top:receipt_poll");
        // token 预算在步骤边界检查：一旦跨线立即收敛出报告，不再派发新步骤
        if !checkpoint.auto_plan_approval_pending {
            if let Some(reason) = token_budget_stop_reason(run_budget.as_ref(), spent_prompt_tokens) {
                budget_stop_reason = Some(reason);
                break 'turn ControlFlow::Break(());
            }
        }

        // 🧭 retriage 资源里程点（实验，默认关）：只看资源事实（探索相位 + 步数
        // 阈值 + 从未复核），由 supervisor 判断漫游是否合理——宿主不碰认知。
        let milestone = crate::agent::retriage::retriage_milestone_steps();
        if milestone > 0
            && !checkpoint.auto_plan_approval_pending
            && checkpoint.plan_phase == crate::agent::checkpoint::PlanPhase::Exploring
            && checkpoint.retriage_used == 0
            && checkpoint.step_id >= milestone
        {
            let revision = crate::agent::retriage::run_retriage(
                &app,
                state.inner(),
                &model_routing,
                &checkpoint,
                &context.user_goal,
                "budget_milestone",
                "",
                &[],
                &final_session_id,
                interrupt_rx.clone(),
            )
            .await;
            let feedback = crate::agent::retriage::apply_retriage_revision(
                &mut context,
                &mut checkpoint,
                &revision,
            );
            context.add_error_feedback(&feedback);
            crate::agent::telemetry::record_gate_fire(
                state.inner(),
                &crate::agent::telemetry::GateFireRecord {
                    run_id: &run_id,
                    session_id: &final_session_id,
                    step_id: checkpoint.step_id as i64,
                    gate: "retriage",
                    route: "retriage:milestone",
                },
            );
            let _ = app.emit(
                &format!("agent-log-{}", final_session_id),
                format!(
                    "🧭 分流复核(里程点): {} — {}",
                    revision.decision, revision.reason
                ),
            );
        }
        let step_id = checkpoint.step_id;

        // 🔄 子任务交付回执消费：三条通道（自动轮询 / wait_jobs / 后台信箱）
        // 统一汇聚到 child_receipt reducer——状态变更、故障转移、退款、投影
        // 只有一个事实源，杜绝通道间语义漂移。
        let receipt_env = crate::agent::child_receipt::ReceiptEnv {
            app: Some(&app),
            db: state.inner(),
            parent_run_id: &run_id,
            session_id: &final_session_id,
            workspace_dir: &workspace_dir,
            interrupt_rx: interrupt_rx.clone(),
            reviewer_backup_model: &model_routing.reviewer_backup_model,
        };
        // 阶段审查退款须批后统一结算（逐条结算会误判在途而漏退）
        let mut stage_refund_pending = false;
        // 本批已投递回执的落账清单：applied_at 与 checkpoint 在下方同一事务提交。
        // seen 去重：信箱与台账两个来源可能携带同一子任务结果，跨通道只入账一次。
        let mut applied_receipts: Vec<crate::agent::checkpoint::AppliedReceipt> = Vec::new();
        let mut seen_receipt_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        // 🔄 异步子任务结果轮询（审查/工兵/侦察）：自动注入父 Agent 上下文
        if let Ok(completed) = crate::agent::child_reviewer::poll_completed_delivery_receipts(
            state.inner(),
            &run_id,
            None,
        ) {
            if !completed.is_empty() {
                let mut injection = String::new();
                for receipt in completed {
                    if !seen_receipt_ids.insert(receipt.child_run_id.clone()) {
                        continue;
                    }
                    let effects = crate::agent::child_receipt::reduce_child_receipt(
                        &receipt_env,
                        &mut checkpoint,
                        &mut context,
                        &receipt,
                    )
                    .await;
                    stage_refund_pending |= effects.stage_refund_pending;
                    if effects.application_deferred {
                        context.add_host_feedback(&effects.model_text);
                        continue;
                    }
                    applied_receipts.push(crate::agent::checkpoint::AppliedReceipt {
                        run_id: receipt.child_run_id.clone(),
                        verdict: effects
                            .contract_violation
                            .clone()
                            .map(|err| ("invalid_output".to_string(), err)),
                    });
                    injection.push_str(&format!("\n{}\n", effects.model_text));
                    for notice in &effects.ui_notices {
                        let _ =
                            app.emit(&format!("agent-log-{}", final_session_id), notice.clone());
                    }
                }
                if !injection.is_empty() {
                    context.add_error_feedback(&injection);
                    let _ = app.emit(
                        &format!("agent-log-{}", final_session_id),
                        "🔄 子任务成果（含实时冲突检测）已注入父 Agent 上下文".to_string(),
                    );
                }
            }
        }

        // ⚡ 通用后台任务信箱 (Durable Pending Inbox) 轮询：获取任意后台任务完成结果
        if let Ok(jobs) = crate::agent::job_engine::poll_pending_inbox(state.inner(), &run_id, 5) {
            if !jobs.is_empty() {
                let mut job_injection = String::new();
                for job in jobs {
                    // 子任务回执走统一 reducer（failover/交付/退款语义单点）；
                    // 非子任务的后台任务（watch 等）保持通用倾倒。
                    if job.job_type.starts_with("child_") || job.job_type.contains("reviewer") {
                        if !seen_receipt_ids.insert(job.job_id.clone()) {
                            continue;
                        }
                        let receipt = job.to_delivery_receipt();
                        let effects = crate::agent::child_receipt::reduce_child_receipt(
                            &receipt_env,
                            &mut checkpoint,
                            &mut context,
                            &receipt,
                        )
                        .await;
                        stage_refund_pending |= effects.stage_refund_pending;
                        if effects.application_deferred {
                            context.add_host_feedback(&effects.model_text);
                            continue;
                        }
                        applied_receipts.push(crate::agent::checkpoint::AppliedReceipt {
                            run_id: job.job_id.clone(),
                            verdict: effects
                                .contract_violation
                                .clone()
                                .map(|err| ("invalid_output".to_string(), err)),
                        });
                        job_injection.push_str(&format!("\n{}\n", effects.model_text));
                        for notice in &effects.ui_notices {
                            let _ = app
                                .emit(&format!("agent-log-{}", final_session_id), notice.clone());
                        }
                        continue;
                    }
                    job_injection.push_str(&format!(
                        "\n【⚡ 后台异步任务已完成 (ID: {}, 类型: {})】\n任务描述: {}\n执行状态: {}\n耗时: {}ms\n输出/结论:\n{}\n",
                        job.job_id,
                        job.job_type,
                        job.title,
                        job.status,
                        job.duration_ms,
                        job.model_projection()
                    ));
                    if let Some(ref err) = job.error_kind {
                        job_injection.push_str(&format!("异常类别: {}\n", err));
                    }
                }
                if !job_injection.is_empty() {
                    context.add_error_feedback(&job_injection);
                    let _ = app.emit(
                        &format!("agent-log-{}", final_session_id),
                        "⚡ 后台异步任务结果已注入父 Agent 上下文".to_string(),
                    );
                }
            }
        }
        // 两路回执合并后统一结算一次阶段审查退款
        if stage_refund_pending && checkpoint.settle_stage_review_refund(state.inner()) {
            context.add_error_feedback(
                "\n🔄 阶段审查次数已退还（本轮含 infra 失败且无专家产出 blocking，视为未有效审查）。",
            );
        }

        // 🔒 回执提交点：checkpoint 快照（含 context 最新投影）+ applied_at +
        // 契约裁决回写 同事务原子落账。此点之前崩溃 = 两笔账一起回滚、回执重投；
        // 此点之后 = 永不重投。投递与落账之间不再有"已消费但效果未持久化"的窗口。
        if !applied_receipts.is_empty() {
            crate::agent::checkpoint::save_checkpoint_and_consume(
                state.inner(),
                &mut checkpoint,
                &context,
                &applied_receipts,
            )?;
        }

        if let Some(reason) = checkpoint.pending_task_failure.clone() {
            crate::agent::failure::finish_failed_run(
                &app,
                state.inner(),
                &final_session_id,
                &run_id,
                &goal,
                &run_trace_steps,
                &mut checkpoint,
                &context,
                &mut run_machine,
                reason.clone(),
            )
            .await?;
            return Err(reason);
        }

        crate::agent::channel::set_supplements_closed(&run_id, false);
        if crate::agent::conversation::import_live_messages(
            state.inner(),
            &final_session_id,
            &mut context,
        )? > 0
        {
            checkpoint.pending_finalization = None;
        }

        // 步边界状态刷新：收养旧 checkpoint 的手写名单（升级一次性迁移）
        // 并按台账重算 consumed 标志。名单退役后"在途/齐套"全部由
        // batch_id+consumed_at 推导——孤儿 id 让门禁等死人的死锁类
        // 结构性消失，无需再对账。
        checkpoint.refresh_consumed_flags(state.inner());
        let resume_instruction = if let Some(recovered) = recovered_instruction.take() {
            Some(recovered)
        } else {
            crate::agent::plan_gate::automatic_plan_approval_instruction(&checkpoint, state.inner())?
        };
        // 每轮规划前把方案门禁状态投影进上下文底部状态卡：模型在决策时即可见
        // 当前模式与放行条件（含委派上级方案待通过状态），不再靠写入被拒反推门禁规则。
        crate::agent::plan_gate::sync_plan_gate_projection(
            &checkpoint,
            &mut context,
            state.inner(),
            &final_session_id,
        );
        if resume_instruction.is_none() {
            checkpoint.begin_planning(step_id, &context);
            save_checkpoint(state.inner(), &mut checkpoint)?;
        }
        advance_run_state(
            &app,
            &final_session_id,
            &mut run_machine,
            AgentRunEvent::BeginPlanning { step_id },
        )?;

        // 目标块步数计数保留（升维标记按步复位的载体），不再注入超限提醒——
        // 步数不能代理收敛与否，认知判断归模型。
        if let Some(block) = context.active_target_block_mut() {
            block.on_step_start();
        }

        app.emit(
            &format!("agent-log-{}", final_session_id),
            format!("正在规划第 {} 步...", step_id + 1),
        )
        .map_err(|e| e.to_string())?;

        // 安全边界一：尚未请求模型时直接收束旧运行。
        if let Some(reason) = take_pending_interrupt(&mut rx) {
            finish_interrupted_run(
                &app,
                state.inner(),
                &final_session_id,
                &run_id,
                &goal,
                &run_trace_steps,
                &mut checkpoint,
                &context,
                &mut run_machine,
                reason,
            )
            .await?;
            return Ok(ControlFlow::Break(None));
        }

        // 修复：绝不可以在这里清空 current_observation，因为这会导致 AI 看不到刚获取的页面数据
        // ================================================================
        // 统一重试循环：整个"规划 + 执行"作为一个原子操作，失败就重试
        // （2026-09-13 夜实弹教训：单次 call_llm 硬失败——反代僵死挂 13min、claude 400/流错误
        //   0s 即死——直接 mark_failed 整轮判死。此处把注释承诺的外层重试落地，共 3 次机会；
        //   用户主动中断、内层已耗尽的指令补正不在此重开预算。）
        // ================================================================
        let mut resume_instruction_opt = resume_instruction;
        let mut phases_failure_count: u32 = 0;
        let outcome = loop {
            crate::agent::control::mark_stage(&final_session_id, "phases:call_llm_and_dispatch");
            let phases_err = match run_call_llm_and_run_tool_phases(
                &app,
                &state,
                &registry_state,
                &model_routing,
                &run_id,
                &workspace_dir,
                &goal,
                log_context,
                transient_llm_request_retries,
                &final_session_id,
                step_id,
                &mut context,
                &mut checkpoint,
                &mut run_machine,
                &mut rx,
                &mut interrupt_rx,
                resume_instruction_opt.take(),
                &mut run_trace_steps,
            )
            .await
            {
                Ok(res) => break Ok(res),
                Err(error) => error,
            };
            if phases_err.instruction_recovery_exhausted()
                || phases_err
                    .message()
                    .contains(crate::llm::STREAM_INTERRUPTED_ERROR)
            {
                break Err(phases_err.to_string());
            }
            phases_failure_count += 1;
            if phases_failure_count >= 3 {
                break Err(phases_err.to_string());
            }
            let backoff_secs = 5u64 * phases_failure_count as u64;
            let retry_warn = format!(
                "【plan+exec 第 {} 次失败，{}s 后重试（本步最多 3 次机会）】{}",
                phases_failure_count, backoff_secs, phases_err
            );
            println!("⚠️ {}", retry_warn);
            let _ = app.emit(
                &format!("agent-log-{}", final_session_id),
                retry_warn.clone(),
            );
            context.add_error_feedback(&retry_warn);
            tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
        };
        let outcome = match outcome {
            Ok(res) => {
                // 预算计量放在 outcome 定案处：含被门禁打回的 finish 步，
                // 被拒步骤消耗的 LLM 调用同样计入 token 上限。
                // 展示用累计量不在这里加——call_llm_phase 已按每次实际调用
                // 落账（含步骤内规划重试），此处再加会双计。
                if let Some(u) = res.token_usage.as_ref() {
                    spent_prompt_tokens += u.prompt_tokens;
                }
                res
            }
            Err(error) => {
                crate::agent::failure::finish_failed_run(
                    &app,
                    state.inner(),
                    &final_session_id,
                    &run_id,
                    &goal,
                    &run_trace_steps,
                    &mut checkpoint,
                    &context,
                    &mut run_machine,
                    format!("任务执行失败: {}", error),
                )
                .await?;
                return Err(error);
            }
        };
        let (mut instruction, mut result) = (outcome.instruction, outcome.result);
        // wait_jobs 消费失败回执、或明确 fail_task：不再经过成功 finish / 状态更新段。
        if let Some(reason) = checkpoint.pending_task_failure.clone() {
            run_trace_steps.push(
                json!({"id": step_id, "instruction": instruction, "status":"failed",
                "route":result.route, "output":reason}),
            );
            crate::agent::failure::finish_failed_run(
                &app,
                state.inner(),
                &final_session_id,
                &run_id,
                &goal,
                &run_trace_steps,
                &mut checkpoint,
                &context,
                &mut run_machine,
                reason.clone(),
            )
            .await?;
            return Err(reason);
        }
        // 无效失败请求是参数错误；不得顺带核销 TODO/契约或执行混发内容。
        if instruction.fail_task.is_some() || instruction.get_tool() == "fail_task" {
            instruction = AgentInstruction {
                description: instruction.description.clone(),
                tool: Some("fail_task".into()),
                ..Default::default()
            };
        }
        // 结构化路由信号：按真实 DispatchResult 记录成功/失败（替代对反馈文本 ❌ 的脆弱匹配）
        context.record_step_outcome(result.success);
        // P3 降级逃生口：跨门禁死锁——写门禁拦截→文案指引 ask_human→冷却/配额拒绝→回门禁
        // 再被拦的推诿循环中，每步派发都只产出门禁拒绝而零真实进展（无真人在 ask_human
        // 那头时此循环永不收敛）。连续达到阈值即改写为 RUN_INTERRUPTED_ROUTE 借
        // finish_interrupted_run 强制降级收尾：标记失败、持久化检查点与轨迹、取消子任务，
        // 留事故记录可查可续，而不是无限空转烧 token。任何一步真实产出即清零，
        // 不影响正常门禁的"拦一次→照做→放行"节奏。
        if is_gate_block_route(&result.route) {
            checkpoint.consecutive_gate_blocked =
                checkpoint.consecutive_gate_blocked.saturating_add(1);
            if checkpoint.consecutive_gate_blocked >= MAX_CONSECUTIVE_GATE_BLOCKS {
                let reason = format!(
                    "【宿主强制降级终止】连续 {} 次派发均被门禁/求助拒绝且无任何有效产出，判定为跨门禁死锁（写门禁拦截 + 审查预算耗尽 + ask_human 冷却/配额互相推诿，且人工通道无人值守）。未决阻断：阶段审查 {} 项、计划审查已重试 {} 次、阶段审查已重试 {} 次；最后拒绝路由 {}。已完成的步骤、证据与工作区改动仍保留在轨迹与检查点中，可人工介入后续跑。",
                    checkpoint.consecutive_gate_blocked,
                    checkpoint.stage_review_blockers_open,
                    checkpoint.plan_reviewer_retry_count,
                    checkpoint.reviewer_retry_count,
                    result.route,
                );
                println!("🛑 [GateDeadlock] {}", reason);
                result = DispatchResult {
                    view: None,
                    success: true,
                    stdout: reason,
                    stderr: String::new(),
                    route: RUN_INTERRUPTED_ROUTE.to_string(),
                };
            }
        } else {
            checkpoint.consecutive_gate_blocked = 0;
        }
        if result.route == RUN_INTERRUPTED_ROUTE {
            finish_interrupted_run(
                &app,
                state.inner(),
                &final_session_id,
                &run_id,
                &goal,
                &run_trace_steps,
                &mut checkpoint,
                &context,
                &mut run_machine,
                result.stdout,
            )
            .await?;
            return Ok(ControlFlow::Break(None));
        }

        // ================================================================
        // 以下是成功跳出规划+执行重试循环后的处理逻辑
        // ================================================================

        // --- 网关层硬拦截：死循环检测 ---
        action_history.push_back(instruction.get_action().to_lowercase());
        if action_history.len() > 4 {
            action_history.pop_front();
        }

        // 连续 3 次 extract 才触发（放宽阈值，避免合理的"extract→等待→extract"被误判）
        let is_loop = if action_history.len() >= 3 {
            let len = action_history.len();
            action_history[len - 3] == "extract"
                && action_history[len - 2] == "extract"
                && action_history[len - 1] == "extract"
        } else {
            false
        };

        // extract → scroll → extract 这个死循环模式依然保留
        let is_loop_3 = if action_history.len() >= 3 {
            let len = action_history.len();
            let last_three = (
                &action_history[len - 3],
                &action_history[len - 2],
                &action_history[len - 1],
            );
            last_three.0 == "extract" && last_three.1 == "scroll" && last_three.2 == "extract"
        } else {
            false
        };

        if is_loop || is_loop_3 {
            let error_detail = "【🚨 网关强制阻断】: 系统检测到你陷入了认知死胡同（连续无意义的 extract 循环）！当前页面的目标元素大概率不存在，或者你理解错了页面状态！请立刻通过 `back` 返回上一页，或者更改搜索关键词重新 `goto`，严禁再次执行 extract！";
            context.add_error_feedback(error_detail);
            app.emit(
                &format!("agent-log-{}", final_session_id),
                "🚨 网关层拦截：发现违规连续 extract 操作，强制注入纠偏机制！",
            )
            .ok();
            action_history.clear(); // 阻断后清空历史，重新积累
        }

        // --- 阶段级写入审查（双触发：todo 阶段推进 / 累计写入文件数兜底）---
        // 首次评估先建立覆盖基线（存量副作用不计入"未覆盖写入"）
        if checkpoint.stage_review_baseline_side_effects == 0
            && checkpoint.stage_review_budget_used == 0
        {
            checkpoint.stage_review_baseline_side_effects = context.side_effects.len() as u32;
        }
        // 【shell 批改探针·后】步骤结束时取差值入账（覆盖单发/批量/重试所有执行路径）
        // 路径级差值 = 本步 shell 真实触碰的新增脏文件，入账 side_effect.affected_paths，
        // 使「本 session 写没写过这个文件」对 shell 写入也可机械对账（git 破坏命令门依赖它）。
        {
            let post_paths = crate::agent::review_tools::git_dirty_file_paths(&workspace_dir);
            let post = post_paths.as_ref().map(|p| p.len() as u32);
            match (context.shell_probe_pre_dirty, post) {
                (Some(pre), Some(post)) => {
                    let pre_paths = context
                        .shell_probe_pre_dirty_paths
                        .take()
                        .unwrap_or_default();
                    let post_list = post_paths.unwrap_or_default();
                    let touched: Vec<String> = post_list
                        .iter()
                        .filter(|p| !pre_paths.iter().any(|x| x == *p))
                        .cloned()
                        .collect();
                    let delta = (post.saturating_sub(pre)).min(50);
                    if delta > 0 {
                        checkpoint.uncovered_shell_mutations =
                            checkpoint.uncovered_shell_mutations.saturating_add(delta);
                    }
                    if !touched.is_empty() {
                        context.shell_touched_paths = touched;
                    }
                    // 反向差值：曾脏现净 = 未提交内容本步消失。消失≠销毁——
                    // commit 收走（HEAD 含同内容指纹）与 stash 收纳（stash 数增长）
                    // 是合法消失；还原/删除才是真蒸发。分类靠 HEAD/指纹实测，
                    // 不归模型自报。
                    let lost: Vec<String> = pre_paths
                        .iter()
                        .filter(|p| !post_list.iter().any(|x| x == *p))
                        .cloned()
                        .collect();
                    if !lost.is_empty() {
                        let tool = instruction.get_tool();
                        let post_head = crate::agent::review_tools::git_head_sha(&workspace_dir);
                        let post_stash =
                            crate::agent::review_tools::git_stash_count(&workspace_dir);
                        let head_moved = match (&context.shell_probe_pre_head, &post_head) {
                            (Some(a), Some(b)) => a != b,
                            _ => false,
                        };
                        let stash_grew = match (context.shell_probe_pre_stash_count, post_stash) {
                            (Some(a), Some(b)) => b > a,
                            _ => false,
                        };
                        for p in &lost {
                            let mine = context.written_file_digests.get(p).cloned();
                            let head_has =
                                crate::agent::review_tools::git_head_blob(&workspace_dir, p);
                            let (tag, loud) = match mine {
                                // 我写的消失了：HEAD 持有同指纹 = 被提交（合法，
                                // 内容可从 HEAD 取回）；否则 = 被删/被还原（蒸发）
                                Some(sha) if head_has.as_deref() == Some(sha.as_str()) => {
                                    ("committed".to_string(), false)
                                }
                                Some(_) => ("destroyed_mine".to_string(), true),
                                None if head_moved => ("committed_or_staged".to_string(), false),
                                None if stash_grew => ("stashed".to_string(), false),
                                None => ("destroyed_foreign".to_string(), true),
                            };
                            context
                                .git_dirty_lost
                                .push(format!("step{step_id} [{tool}] {p} -> {tag}"));
                            if loud {
                                app.emit(
                                    &format!("agent-log-{}", final_session_id),
                                    format!(
                                        "⚠️ 未提交内容消失: {p}（{}）",
                                        if tag == "destroyed_mine" {
                                            "本 session 写入，无提交副本"
                                        } else {
                                            "外来未提交改动，非 commit/stash 途径"
                                        }
                                    ),
                                )
                                .ok();
                            }
                        }
                        if context.git_dirty_lost.len() > 20 {
                            let overflow = context.git_dirty_lost.len() - 20;
                            context.git_dirty_lost.drain(0..overflow);
                        }
                    }
                }
                // 非 git 环境：按本步骤是否为写类指令保守计 1（但排除 session_tmp 草稿写入）
                (None, _)
                    if crate::agent::dispatcher::is_write_or_side_effect_tool(&instruction) =>
                {
                    let is_tmp_write = instruction.get_tool().eq_ignore_ascii_case("local_file")
                        && crate::agent::tools::local_file::is_dispatch_result_tmp(&result);
                    if !is_tmp_write {
                        checkpoint.uncovered_shell_mutations =
                            checkpoint.uncovered_shell_mutations.saturating_add(1);
                    }
                }
                _ => {}
            }
            context.shell_probe_pre_dirty = None;
            context.shell_probe_pre_dirty_paths = None;
        }
        let side_effects_before = context.side_effects.len();
        post_step::record_side_effect_or_verify(
            &mut context,
            step_id,
            &instruction,
            &result,
            &app,
            &final_session_id,
            &workspace_dir,
        );
        if context.side_effects.len() != side_effects_before {
            checkpoint.finish_reviewed = false;
            checkpoint.finish_challenge_count = 0;
            checkpoint.pending_finalization = None;
        }
        // --- 更新任务规划 + 核心记忆 ---
        // 两类责任账本独立更新，避免审查契约的拒绝路径吞掉写入验证更新。
        // 契约更新被拒时，原因原本只回传给模型，界面/记录都看不到：先干跑一次拿到拒绝原因，再可见化。
        let contract_update_rejection = instruction.review_contract_update.as_ref().and_then(|u| {
            let mut probe = context.review_contract.clone();
            apply_review_contract_update(&mut probe, u.clone()).err()
        });
        let evidence_source = crate::agent::change_verification::EvidenceLookup {
            db: state.inner(),
            session_id: &final_session_id,
            run_id: &run_id,
        };
        let stage_review_instruction = crate::agent::stage_review::milestone_instruction(&context, &instruction);
        let review_contract_started_this_turn =
            crate::agent::finish_guard::apply_instruction_state_updates_with_source(
                &mut context,
                &instruction,
                step_id,
                &evidence_source,
            );
        if let Some(err) = contract_update_rejection {
            let short: String = err.chars().take(100).collect();
            let _ = app.emit(
                &format!("agent-log-{}", final_session_id),
                format!("⚠️ 审查契约更新未被采纳：{}", short),
            );
            crate::agent::telemetry::record_gate_fire(
                state.inner(),
                &crate::agent::telemetry::GateFireRecord {
                    run_id: &run_id,
                    session_id: &final_session_id,
                    step_id: step_id as i64,
                    gate: "contract",
                    route: &format!("contract:update_rejected:{}", short),
                },
            );
        } else if let Some(update) = instruction.review_contract_update.as_ref() {
            let _ = app.emit(
                &format!("agent-log-{}", final_session_id),
                format!(
                    "审查契约申报已保存（{} 项）；未提交项保留，不代表验收通过",
                    update.obligations.len()
                ),
            );
        }

        println!(
            "【阶段审查诊断】step {} | phase={:?} | mode={:?} | uncovered={} | shell_baseline={} | budget={} | side_effects={}",
            step_id,
            checkpoint.plan_phase,
            checkpoint.plan_mode,
            checkpoint.uncovered_shell_mutations,
            checkpoint.stage_review_shell_baseline,
            checkpoint.stage_review_budget_used,
            context.side_effects.len()
        );
        // Reconcile only committed receipt statuses, including contract downgrades.
        let milestones_settled=crate::agent::stage_review::settle_milestones(&mut checkpoint, state.inner());
        if milestones_settled {
            checkpoint.refresh_context(&context);
            save_checkpoint(state.inner(), &mut checkpoint)?;
        }
        if let Some(reason) = crate::agent::stage_review::should_fire(
            &checkpoint,
            state.inner(),
            &context,
            &stage_review_instruction,
            &model_routing,
        ) {
            match crate::agent::stage_review::dispatch(
                &app,
                state.inner(),
                &run_id,
                &final_session_id,
                &workspace_dir,
                &mut checkpoint,
                &context,
                &model_routing,
                step_id,
                interrupt_rx.clone(),
            )
            .await
            {
                Ok(()) => {
                    save_checkpoint(state.inner(), &mut checkpoint).map_err(|error| {
                        format!("阶段审查已派发但父状态保存失败；正在停止本轮：{error}")
                    })?;
                    let _ = app.emit(
                        &format!("agent-log-{}", final_session_id),
                        format!("📋 阶段审查已派发（{}）", reason),
                    );
                }
                Err(err) => {
                    let _ = app.emit(
                        &format!("agent-log-{}", final_session_id),
                        format!("⚠️ 阶段审查派发失败: {}", err),
                    );
                }
            }
        }
        if !instruction.memories_update.is_empty() {
            let rejections = context.update_memories(instruction.memories_update.clone());
            if !rejections.is_empty() {
                context.turns_history.push(crate::agent::context::types::ChatMessage {
                    role: "user".to_string(),
                    content: serde_json::json!(format!(
                        "【⚠️ 记忆安全提示】\n以下记忆项未能存入核心记忆卡：\n{}\n（请勿在记忆中保存明文密钥或超长文本；长数据请存入 TaskStore 数据区）",
                        rejections.iter().map(|r| format!("- {}", r)).collect::<Vec<_>>().join("\n")
                    )),
                });
            }
            println!("📝 AI 更新了核心记忆: {:?}", instruction.memories_update);
        }
        if let Some(ref summary) = instruction.progress_summary {
            context.set_progress_summary(summary);
            println!("📝 AI 更新了进度摘要: {}", summary);
        }

        // --- 核心增强：历史截断与缓存优化 ---
        if instruction.clear_history.unwrap_or(false) {
            println!("🧹 AI 主动触发了历史清理 (clear_history: true)!");
            context.turns_history.clear();
        }

        // --- 核心增强：Token 优化与冷存储检索 ---
        // AI 在本轮请求 "require_memory: true" 或触发了清空历史，下一轮组装消息时就会塞入全部 Fact 内容
        context.carry_memories = instruction.require_memory.unwrap_or(false)
            || instruction.clear_history.unwrap_or(false);

        // --- 预加载下一轮工具说明书 ---
        let next_tool = instruction
            .next_tool_hint
            .clone()
            .unwrap_or_else(|| instruction.get_action());
        context.active_tool = Some(next_tool.clone());

        // --- 加载工具说明书（Write-Once 历史事件 + 未命中纠错 + 现场刷新，见 tool_manual.rs）---
        crate::agent::tool_manual::load_tool_manual_for_next_step(
            &app,
            registry_state.inner(),
            &mut context,
            &instruction,
            &workspace_dir,
            state.inner(),
        )
        .await;

        // --- 刷新独立任务数据区目录，再把轻量引用写进 checkpoint ---
        // 数据行早已由 task_store 自己实时持久化，绝不复制到 context。
        context.set_task_resources(task_store::list_resource_refs(
            state.inner(),
            &final_session_id,
        )?);

        // --- 记录历史 + 更新观测 ---
        let final_stdout = result.stdout.clone();
        let mut output_text = if result.success {
            final_stdout.clone()
        } else {
            format!("❌ {}\n{}", result.stderr, final_stdout)
        };

        let is_finish = is_finish_instruction(&instruction);
        let is_prepare_finish = instruction.prepare_finish.is_some();
        let finish_gate_requested = is_finish || is_prepare_finish;
        let completion_generation = *interrupt_rx.borrow();
        let mut finish_challenged = false;
        let mut reviewer_warnings_delivered = None;
        let late_messages = if finish_gate_requested && result.success {
            crate::agent::channel::set_supplements_closed(&run_id, true);
            crate::agent::conversation::import_live_messages(
                state.inner(),
                &final_session_id,
                &mut context,
            )?
        } else {
            0
        };
        if late_messages > 0 {
            // 事实边界：该请求生成后又收到用户消息，本次 finish 尚未考虑这些消息。
            checkpoint.pending_finalization = None;
            finish_challenged = is_finish;
            output_text = "已收到新的用户补充，本次交付尚未提交；请先处理补充内容后再交付。".into();
            context.add_host_feedback(&output_text);
        }
        if finish_gate_requested && result.success && late_messages == 0 {
            let generation = *interrupt_rx.borrow();
            let evaluation = crate::agent::finalization::evaluate_completion(
                &app,
                state.inner(),
                &mut context,
                &mut checkpoint,
                &run_id,
                &final_session_id,
                step_id,
                &workspace_dir,
                &git_baseline,
                task_started_at,
                &model_routing,
                &mut interrupt_rx,
                &instruction,
                if is_prepare_finish {
                    crate::agent::finalization::CompletionMode::Prepare
                } else {
                    crate::agent::finalization::CompletionMode::Finish
                },
                is_explicit_workspace || auto_pilot,
                review_contract_started_this_turn,
            )
            .await;
            // 成功/失败两条审查返回路径都先尊重停止，不能只在审查服务失败时消费停止。
            if let Some(reason) = take_pending_interrupt(&mut rx).or_else(|| {
                crate::agent::finalization::interrupted_since(&interrupt_rx, generation)
                    .then(|| "已收到停止或替换任务信号，交付未提交".to_string())
            }) {
                finish_interrupted_run(
                    &app,
                    state.inner(),
                    &final_session_id,
                    &run_id,
                    &goal,
                    &run_trace_steps,
                    &mut checkpoint,
                    &context,
                    &mut run_machine,
                    reason,
                )
                .await?;
                return Ok(ControlFlow::Break(None));
            }
            let mut receipt = evaluation.receipt;
            if receipt.is_ready() {
                let delivery_paths = crate::agent::reviewer_gatekeeper::delivery_file_paths(
                    state.inner(), &final_session_id, &receipt.finish_payload,
                );
                let current_hash = crate::agent::reviewer_gatekeeper::task_review_snapshot(
                    state.inner(), &final_session_id, &workspace_dir, &context,
                    &git_baseline, task_started_at, &delivery_paths,
                ).0.hash;
                // 文件变化的意义由终审裁量；此处只披露审查后的窄窗变化。
                // scope/契约和用户停止仍按既有事实边界处理。
                if !receipt.diff_hash.starts_with("unsettled:") {
                    receipt.note_later_material_change(&current_hash);
                }
                if receipt.scope_revision != context.resource_scope.revision
                    || receipt.contract_fingerprint
                        != crate::agent::finalization::contract_fingerprint(
                            context.review_contract.as_ref(),
                        )
                {
                    receipt
                        .blockers
                        .push("验收后范围或契约已变化，本次未提交；请重新核验。".into());
                }
            }
            checkpoint.pending_finalization = Some(receipt.clone());
            if is_prepare_finish {
                output_text = precheck::render_prepare_finish(&mut checkpoint, &context, &receipt);
                if let Some(notice) = instruction.ignored_mixed_entry_notice() {
                    output_text.push_str(&notice);
                }
            } else if !receipt.is_ready() {
                finish_challenged = true;
                checkpoint.finish_challenge_count =
                    checkpoint.finish_challenge_count.saturating_add(1);
                // 每项单独注入，保留命令/退出码/原始诊断；不是 JSON 错误，不追加格式纠正要求。
                for blocker in &receipt.blockers {
                    context.add_host_feedback(&format!("【交付未提交】{}", blocker));
                }
                output_text = format!("交付未提交：仍有 {} 项未满足的验收条件。\n{}\n请补正上列事项后重新 prepare_finish 或 finish；确实无法完成时可如实结束为未完成。",
                    receipt.blockers.len(), receipt.blockers.iter().map(|b| format!("- {}", b)).collect::<Vec<_>>().join("\n"));
                context.add_host_feedback("预检/交付使用同一检查结果；重试次数不会自动核销阻塞项。修正证据或完成验证后再提交，不必重复发送已保存的全部义务。");
                context.record_step_outcome(false);
            } else {
                if matches!(
                    receipt.review,
                    crate::agent::finalization::ReceiptReview::DeliveredWithWarnings { .. }
                ) {
                    reviewer_warnings_delivered = evaluation.review_result;
                }
                if let crate::agent::finalization::ReceiptVerification::Waived { ref reason } =
                    receipt.verification
                {
                    output_text.push_str(&format!("\n\n验证披露：{}", reason));
                }
            }
            crate::agent::telemetry::record_gate_fire(
                state.inner(),
                &crate::agent::telemetry::GateFireRecord {
                    run_id: &run_id,
                    session_id: &final_session_id,
                    step_id: step_id as i64,
                    gate: "finalization",
                    route: if receipt.is_ready() {
                        "finalization:prepared_ready"
                    } else {
                        "finalization:prepared_blocked"
                    },
                },
            );
            if is_prepare_finish || !receipt.is_ready() {
                crate::agent::dispatcher::emit_tool_event(
                    &app,
                    &final_session_id,
                    step_id,
                    &instruction,
                    if is_prepare_finish || receipt.is_ready() {
                        "succeeded"
                    } else {
                        "finish_challenged"
                    },
                    Some(if is_prepare_finish {
                        "prepare_finish".into()
                    } else {
                        "finish".into()
                    }),
                    Some(receipt.is_ready()),
                    &output_text,
                    None,
                    None,
                );
            }
        }
        if let Some(reason) = checkpoint.pending_task_failure.clone() {
            if let Some(reason) = take_pending_interrupt(&mut rx) {
                finish_interrupted_run(
                    &app,
                    state.inner(),
                    &final_session_id,
                    &run_id,
                    &goal,
                    &run_trace_steps,
                    &mut checkpoint,
                    &context,
                    &mut run_machine,
                    reason,
                )
                .await?;
                return Ok(ControlFlow::Break(None));
            }
            crate::agent::failure::finish_failed_run(
                &app,
                state.inner(),
                &final_session_id,
                &run_id,
                &goal,
                &run_trace_steps,
                &mut checkpoint,
                &context,
                &mut run_machine,
                reason.clone(),
            )
            .await?;
            return Err(reason);
        }
        let is_effectively_challenged = finish_challenged;
        let outcome_str = if is_effectively_challenged {
            "finish_challenged"
        } else if result.success {
            "succeeded"
        } else {
            "failed"
        };
        let model_output_text = output_text.clone();
        let has_mcp_evidence = result
            .view
            .as_ref()
            .is_some_and(crate::agent::dispatcher::is_mcp_raw_results_view);
        let durable_output_text =
            crate::agent::dispatcher::take_tool_durable_output(&mut result, &model_output_text);
        let output_id = crate::agent::run_trace::save_step_output_in_group(
            state.inner(),
            &final_session_id,
            step_id,
            &instruction.get_tool(),
            &instruction.description,
            outcome_str,
            &durable_output_text,
            crate::agent::work_history::WorkGroup::capture(&final_session_id, &context, step_id)
                .as_ref(),
        )
        .unwrap_or_else(|e| {
            eprintln!("⚠️ 无法保存 step_output: {}", e);
            "unknown_output_id".to_string()
        });
        context.add_step(&instruction, &model_output_text, &output_id);
        let dedup = deduplicate_mcp_output(
            state.inner(),
            &final_session_id,
            has_mcp_evidence,
            &instruction,
            outcome_str,
            &durable_output_text,
            &model_output_text,
            &run_trace_steps,
            &mut context,
            &output_id,
        );
        output_text = dedup.displayed_output.clone();
        let canonical_output_id = dedup
            .canonical_output_id
            .clone()
            .unwrap_or_else(|| output_id.clone());
        let evidence_output_id = if dedup.call_hash.is_some() {
            canonical_output_id.clone()
        } else {
            output_id.clone()
        };

        // 🌟 工具执行失败时，自动沉淀到「避坑教训库 (Failed Attempts)」，供后续步骤长效避坑
        if !result.success {
            let error_summary = if !result.stderr.trim().is_empty() {
                result.stderr.trim().to_string()
            } else {
                result.stdout.lines().take(3).collect::<Vec<_>>().join(" ")
            };
            let short_error = if error_summary.chars().count() > 160 {
                format!("{}...", error_summary.chars().take(160).collect::<String>())
            } else {
                error_summary
            };
            context.record_failed_attempt(
                step_id,
                instruction.get_tool(),
                instruction.get_action(),
                &instruction.get_params(),
                short_error,
                Some(evidence_output_id.clone()),
            );
        }

        if let Some(block) = context.active_target_block_mut() {
            block.on_step_finish();
        }

        // 读取型工具：把本次读取的 output_id 回填进重复读取台账，
        // 让后续拦截回执指向持久化句柄（脱水/压缩后仍可用 task_store 定向回查）
        if result.success {
            // 读重叠提示（先于记账，拿的是"此前已读"的覆盖）：与已读区间重叠
            // 且期间文件未被写入时，在读结果尾部附一行事实提示。
            // 不拦截——不同行窗仍是不同信息（window_read 判例），提示权归模型。
            if let Some(hint) = context.read_overlap_hint(&instruction) {
                output_text.push_str(&hint);
            }
            context.note_read_output_id(&instruction, &workspace_dir, &evidence_output_id);
            // 取证台账（展示用）：读取/列目录/检索/回查成功即记账，
            // 跨压缩保留——脱水后模型据 output_id 回查而非原样重读
            context.note_evidence_receipt(step_id, &instruction, &evidence_output_id);
        }

        post_step::backfill_todo_evidence(&mut context, &mut instruction, &evidence_output_id);

        post_step::record_lesson_lineage(
            state.inner(),
            &final_session_id,
            step_id,
            &instruction,
            &result,
        );
        // ──────────────────────────────────────────────────────────────────

        context.current_observation = output_text.clone();
        // 复盘轨迹使用与 step_new 相同的语义字段，但与 Agent prompt/checkpoint 隔离。
        // 这使历史会话既能恢复思考、计划和工具调用，又不会放大运行时上下文。
        let metrics_json = step_metrics_json(
            outcome.token_usage.as_ref(),
            outcome.thinking_duration_ms,
            outcome.tool_duration_ms,
        );

        run_trace_steps.push(json!({
            "id": step_id,
            "description": instruction.description.clone(),
            "reflection": instruction.reflection.clone(),
            "thought": instruction.thought.clone(),
            "finding": instruction.finding.clone(),
            "finding_kind": instruction.finding_kind.clone(),
            "thinking": crate::agent::run_trace::clip_thinking_for_trace(&outcome.thinking_text),
            "tool": instruction.get_tool(),
            "action": instruction.get_action(),
            "declared_action": instruction.declared_action(),
            "command": instruction.get_params(),
            "params": instruction.get_params(),
            "plan_id": context.active_plan_id().unwrap_or_default(),
            "plan_title": context.active_plan_title().map(str::to_string).unwrap_or_else(|| crate::agent::context::types::clip_plan_title(context.effective_goal())),
            "status": outcome_str,
            "output": output_text.clone(),
            "output_id": output_id.clone(),
            "mcp_call_hash": dedup.call_hash.clone(),
            "mcp_raw_output_hash": dedup.raw_output_hash.clone(),
            "mcp_model_output_hash": dedup.model_output_hash.clone(),
            "canonical_output_id": dedup.call_hash.as_ref().map(|_| canonical_output_id.clone()),
            "duplicate_of_step": dedup.duplicate_of_step,
            "view": result.view.clone(),
            "resource_scope_revision": context.resource_scope.revision,
            "metrics": metrics_json,
            "token_usage": metrics_json,
            "timestamp": Utc::now().timestamp_millis(),
        }));
        // 增量持久化：每步单行 INSERT 进 agent_run_steps，替代整写 steps_json 的
        // O(n²) 序列化+整行重写。steps_json 列仅在启动/终态快照整写一次，
        // 读者（load_run_steps / 监控 SQL / 检索索引）按行优先、列回退。
        if let Some(step_value) = run_trace_steps.last() {
            if let Err(error) = crate::agent::run_trace::append_step(
                state.inner(),
                &run_id,
                (run_trace_steps.len() - 1) as i64,
                step_value,
            ) {
                eprintln!("[run-trace] 无法追加步骤快照: {error}");
            }
        }
        // 工具结果已经进入可恢复上下文。先提交 durable phase/cursor，再通知前端；
        // 这样崩溃恢复不会把已经完成的普通步骤当作待执行，也不会把 finish 误续跑。
        if is_finish && result.success && !is_effectively_challenged {
            if let Some(reason) = take_pending_interrupt(&mut rx).or_else(|| {
                crate::agent::finalization::interrupted_since(&interrupt_rx, completion_generation)
                    .then(|| "已收到停止或替换任务信号，交付未提交".to_string())
            }) {
                finish_interrupted_run(
                    &app,
                    state.inner(),
                    &final_session_id,
                    &run_id,
                    &goal,
                    &run_trace_steps,
                    &mut checkpoint,
                    &context,
                    &mut run_machine,
                    reason,
                )
                .await?;
                return Ok(ControlFlow::Break(None));
            }
        }
        let is_actually_completed = is_finish && result.success && !is_effectively_challenged;
        let mut committed_emitted_finish: Option<serde_json::Value> = None;
        if is_actually_completed {
            committed_emitted_finish = Some(
                match finalize::commit_completed_run(
                    state.inner(),
                    &mut checkpoint,
                    &mut context,
                    &run_id,
                    &final_session_id,
                    &workspace_dir,
                    &goal,
                    &run_trace_steps,
                    &output_text,
                    &conversation_model_id,
                    reviewer_warnings_delivered.as_ref(),
                ) {
                    Ok(finish) => finish,
                    Err(error) if error == crate::agent::agent_mailbox::replies::PENDING_REPLY => {
                        crate::agent::agent_mailbox::coordination::kick(state.inner());
                        context.add_host_feedback("交付提交前新受理的协作尚未结清；本次未结束，请等待答复并处理交接。");
                        checkpoint.complete_step(step_id, &context);
                        save_checkpoint(state.inner(), &mut checkpoint)?;
                        let _ = app.emit(
                            &format!("agent-log-{}", final_session_id),
                            "交付暂缓：正在等待已登记的协作。",
                        );
                        break 'turn ControlFlow::Continue(());
                    }
                    Err(error) => return Err(error),
                },
            );
        } else {
            // 被质询或失败的 finish 也要推进 durable cursor，恢复后继续下一轮而不是误判终态。
            checkpoint.complete_step(step_id, &context);
            save_checkpoint(state.inner(), &mut checkpoint)?;
        }

        // --- 前端通知 + 任务完成判定（合并） ---
        app.emit(&format!("agent-context-{}", final_session_id), &context)
            .map_err(|e| e.to_string())?;

        if is_actually_completed {
            crate::agent::dispatcher::emit_tool_event(
                &app,
                &final_session_id,
                step_id,
                &instruction,
                "succeeded",
                Some("finish".into()),
                Some(true),
                &output_text,
                None,
                None,
            );
            advance_run_state(
                &app,
                &final_session_id,
                &mut run_machine,
                AgentRunEvent::BeginFinalizing,
            )?;
            let emitted_finish = committed_emitted_finish
                .take()
                .unwrap_or_else(|| json!({ "summary": output_text }));

            let complete_msg = if reviewer_warnings_delivered.is_some() {
                "任务已交付（经人工仲裁放行，附带未收敛警告）"
            } else {
                "任务已通过验收并提交"
            };

            app.emit(
                &format!("agent-progress-{}", final_session_id),
                json!({
                    "type": "complete",
                    "message": complete_msg,
                    "success": true,
                    "output": output_text,      // 原始 JSON 字符串（兼容旧逻辑）
                    "finish": emitted_finish,   // 结构化成果对象，含 details/summary/artifacts
                }),
            )
            .map_err(|e| e.to_string())?;
            // 自动登记运行期间新生成的工作区文件为产物：微信"完成后回推文件"
            // 不再依赖模型记得调用 register_file（漏登记 = 用户只收到文字收不到文件）
            let registered = if is_explicit_workspace {
                crate::agent::artifact_manager::auto_register_run_artifacts(
                    state.inner(),
                    &final_session_id,
                    Some(&run_id),
                    &workspace_dir,
                    run_started_at,
                )
            } else {
                Vec::new()
            };
            if !registered.is_empty() {
                println!(
                    "[run-trace] 自动登记 {} 个运行期产物: {}",
                    registered.len(),
                    registered
                        .iter()
                        .map(|a| a.display_name.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            break 'turn ControlFlow::Break(());
        }

        // 普通步骤：发 step_done / step_error
        app.emit(
            &format!("agent-progress-{}", final_session_id),
            json!({
                "type": if result.success { "step_done" } else { "step_error" },
                "step_id": step_id,
                "description": instruction.description,
                "finding": instruction.finding.as_deref(),
                "finding_kind": instruction.finding_kind.as_deref(),
                "output": output_text,
                "metrics": metrics_json,
                "token_usage": metrics_json
            }),
        )
        .map_err(|e| e.to_string())?;

        sleep(Duration::from_millis(500)).await;
        ControlFlow::Continue(())
    };
    let state = Box::new(MainExecution {
        app,
        state,
        registry_state,
        model_routing,
        goal,
        auto_pilot,
        final_session_id,
        log_context,
        transient_llm_request_retries,
        run_id,
        plan_mode,
        run_budget,
        rx,
        interrupt_rx,
        stall_probe,
        run_trace_steps,
        run_machine,
        is_explicit_workspace,
        workspace_dir,
        run_started_at,
        conversation_model_id,
        context,
        checkpoint,
        git_baseline,
        task_started_at,
        recovered_instruction,
        action_history,
        max_steps,
        spent_prompt_tokens,
        budget_stop_reason,
    });
    Ok(match flow {
        ControlFlow::Continue(()) => ControlFlow::Continue(state),
        ControlFlow::Break(()) => ControlFlow::Break(Some(state)),
    })
}
