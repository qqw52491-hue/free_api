//! child_reviewer 派发臂：受限审查子 Agent 的异步派发。

//! 子 Agent 派发臂：child_reviewer / child_writer / child_worker / resume_child。

use crate::agent::checkpoint::{save_checkpoint, AgentCheckpoint};
use crate::agent::child_reviewer::result_as_dispatch_output;
use crate::agent::context::resource_scope::ResourceOperation;
use crate::agent::control::{take_pending_interrupt, HumanControl};
use crate::agent::dispatcher::{
    emit_tool_event, is_finish_instruction, is_write_or_side_effect_tool, run_agent_step,
};
use crate::agent::mcp::PluginRegistry;
use crate::agent::run_state::{AgentRunEvent, AgentRunMachine};
use crate::agent::types::{AgentInstruction, DispatchResult, RUN_INTERRUPTED_ROUTE};
use crate::agent::utils::call_llm;
use crate::db::DbState;
use crate::llm::ModelConfig;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, watch};

use super::*;

pub(super) async fn dispatch_child_reviewer(
    ctx: &mut DispatchCtx<'_>,
    inst: &AgentInstruction,
) -> Result<ArmOutcome, String> {
    Ok(ArmOutcome::Done({
        // 记录派发前的消费状态：若本次派发一个任务都没起来，需原样回滚，避免白耗一轮审查预算。
        let pre_dispatch_consumed = ctx.checkpoint.plan_reviewer_consumed;
        // 方案审查轮数上限只约束批准前的方案审查讨论；
        // 已批准后的 child_reviewer 属于交付验证等其他用途，不消耗该预算、也不改变方案阶段。
        if !ctx.checkpoint.plan_phase.is_approved() {
            if ctx.checkpoint.plan_reviewer_retry_count
                >= crate::agent::checkpoint::max_plan_reviewer_rounds()
            {
                let limit_msg = crate::agent::gate_messages::reviewer_retry_limit();
                let result = DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: limit_msg,
                    success: false,
                    route: "plan_gatekeeper:reviewer_retry_limit".to_string(),
                };
                crate::agent::telemetry::record_gate_fire(
                    ctx.state.inner(),
                    &crate::agent::telemetry::GateFireRecord {
                        run_id: ctx.parent_run_id,
                        session_id: ctx.final_session_id,
                        step_id: ctx.step_id as i64,
                        gate: "pre_write",
                        route: &result.route,
                    },
                );
                emit_tool_event(
                    ctx.app,
                    ctx.final_session_id,
                    ctx.step_id,
                    inst,
                    "failed",
                    Some(result.route.clone()),
                    Some(false),
                    &result.stderr,
                    Some(ctx.tool_started_at.elapsed().as_millis()),
                    None,
                );
                return Ok(ArmOutcome::Early(result));
            }

            // 仅在尚未批准时进入审查中阶段；若已批准，保留 Approved 状态，避免后续补充审查打回 Reviewing 阻塞修改
            if !ctx.checkpoint.plan_phase.is_approved() {
                ctx.checkpoint.plan_phase = crate::agent::checkpoint::PlanPhase::Reviewing;
            }
            // 统一走 reserve：与反驳对质共用同一预扣入口，预算语义单一事实源
            if ctx.checkpoint.reserve_plan_review_round().is_err() {
                // 上方已做过上限检查，正常不会到这里；兜底不递增
            }
        }

        let routing = crate::agent::router::AgentRouter::route_for_role(
            ctx.context,
            ctx.model_routing,
            ctx.step_id,
            crate::agent::router::roles::REVIEWER,
        );
        let model_tier = routing.tier.to_string();
        let reasoning_effort = ctx.model_routing.reasoning_effort_for(&routing.tier).to_string();
        // 会话级 reviewer 钉死（role_models.reviewer）优先级高于角色卡默认模型：
        // 用户显式指定审查模型时，角色卡的 model_id 默认值不得反杀。
        let explicit_reviewer_slot = ctx.model_routing
            .role_models
            .get(crate::agent::router::roles::REVIEWER)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let items = inst.get_dispatch_items();
        let total = items.len();
        let is_batch = total > 1;
        // 同批联合会审归组：全部成员共享 batch_id，台账可按批联查
        let batch_id = format!("batch-{}", uuid::Uuid::new_v4());

        let mut item_reports = Vec::new();
        let mut spawned_ids = Vec::new();
        let mut success_count = 0;
        let mut fail_count = 0;
        // 单发场景标记：唯一派发项因角色卡必填入参缺失被拒时，route 透出 input_incomplete 便于门禁归因
        let mut dispatch_input_incomplete = false;

        for (idx, child_params) in items.iter().enumerate() {
            let item_num = idx + 1;
            let mut reviewer_persona = String::new();
            let mut reviewer_model = routing.model_id.clone();
            let mut effective_params = child_params.clone();
            if let Some(obj) = effective_params.as_object_mut() {
                obj.remove("max_concurrency");
            }

            let mut reviewer_custom_schema = None;
            if let Some(role_name) = child_params.get("role").and_then(|v| v.as_str()) {
                if crate::agent::tools::role_manager::is_card_disabled_for(&ctx.model_routing.disabled_role_cards, role_name, "reviewer") {
                    item_reports.push(format!(
                        "[{}/{}] ⏸️ 角色卡「{}」在当前会话的 reviewer 工位已被用户排除/休眠；请改用其他角色或直接以默认身份派发。",
                        item_num, total, role_name
                    ));
                    fail_count += 1;
                    continue;
                }
                match crate::agent::tools::role_manager::resolve_enabled_card(ctx.state.inner(), role_name) {
                    Ok(Some(card)) => {
                        if !card.allows_kind("reviewer") {
                            item_reports.push(format!(
                                "[{}/{}] ❌ 角色卡「{}」未授权挂载为 reviewer（允许类型: {}）。",
                                item_num, total, role_name, card.allowed_kinds
                            ));
                            fail_count += 1;
                            continue;
                        }
                        reviewer_persona = crate::agent::tools::role_manager::persona_block(&card);
                        reviewer_persona += &crate::agent::tools::role_manager::skills_block(&card);
                        effective_params["role"] = json!(card.name);
                        effective_params["role_label"] = json!(card.label);
                        if let Some(schema) = card.parse_custom_schema() {
                            reviewer_custom_schema = Some(schema);
                        }
                        if !card.model_id.is_empty() && !explicit_reviewer_slot {
                            reviewer_model = card.model_id.clone();
                        }
                        if card.max_concurrency > 0 {
                            effective_params["max_concurrency"] = json!(card.max_concurrency.min(crate::agent::child_reviewer::MAX_TOTAL_CHILDREN_PER_RUN as i64));
                        }
                        let focus_domains = card.parse_focus_domains();
                        let custom_inputs = child_params.get("custom_inputs");
                        // 角色卡 input_schema 必填校验（输入侧硬约束）：必填入参未供给按项拒绝派发
                        if let Err(err) = crate::agent::tools::role_manager::validate_dispatch_inputs(&card, custom_inputs) {
                            item_reports.push(format!("[{}/{}] ❌ {}", item_num, total, err));
                            fail_count += 1;
                            dispatch_input_incomplete = true;
                            continue;
                        }
                        // 任务入口分流判决简报并入 custom_inputs（verified_facts/hard_constraints 保留键升格一级小节）
                        let merged_inputs = ctx.checkpoint
                            .triage_verdict
                            .as_ref()
                            .map(|v| crate::agent::triage::merge_brief_into_inputs(custom_inputs, v));
                        let custom_inputs = merged_inputs.as_ref().or(custom_inputs);
                        let last_verify_log = child_params
                            .get("last_verify_log")
                            .or_else(|| child_params.get("verify_log"))
                            .and_then(|v| v.as_str());
                        crate::agent::control::mark_stage(
                            ctx.final_session_id,
                            "child_reviewer:focus_context",
                        );
                        if let Some(spec_ctx) = crate::agent::context_collector::assemble_focus_context(
                            &focus_domains,
                            ctx.workspace_dir,
                            last_verify_log,
                            custom_inputs,
                        ).await {
                            effective_params["specialized_context"] = json!(spec_ctx);
                        }
                        if let Some(merged) = merged_inputs {
                            effective_params["custom_inputs"] = merged;
                        }
                    }
                    Ok(None) => {
                        item_reports.push(format!(
                            "[{}/{}] ❌ 角色卡不存在或已禁用：{}；可用角色见角色池菜单，或先用 role_manager.upsert 创建。",
                            item_num, total, role_name
                        ));
                        fail_count += 1;
                        continue;
                    }
                    Err(error) => {
                        item_reports.push(format!(
                            "[{}/{}] ❌ 角色卡查询失败：{}。",
                            item_num, total, error
                        ));
                        fail_count += 1;
                        continue;
                    }
                }
            } else {
                // 模型未显式指定 role：根据任务问题与证据包智能匹配专精角色卡（如运维/技术文档自动匹配 doc_writer）
                let auto_card = {
                    let cards = crate::agent::tools::role_manager::list_cards(ctx.state.inner(), false).unwrap_or_default();
                    let active: Vec<_> = cards.into_iter().filter(|c| {
                        c.enabled && c.allows_kind("reviewer") && !crate::agent::tools::role_manager::is_card_disabled_for(&ctx.model_routing.disabled_role_cards, &c.name, "reviewer")
                    }).collect();

                    let q_text = child_params.get("question").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
                    let ev_text = child_params.get("evidence_excerpt").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
                    let corpus = format!("{} {}", q_text, ev_text);

                    let is_doc = corpus.contains("文档") || corpus.contains("手册") || corpus.contains("规范") || corpus.contains("markdown") || corpus.contains(".md") || corpus.contains("doc");
                    let is_fe = corpus.contains("前端") || corpus.contains("ui") || corpus.contains("vue") || corpus.contains("react") || corpus.contains("css") || corpus.contains("html");
                    let is_be = corpus.contains("后端") || corpus.contains("架构") || corpus.contains("api") || corpus.contains("rust") || corpus.contains("sql") || corpus.contains("数据库");

                    let matched = if is_doc {
                        active.iter().find(|c| c.name.contains("doc") || c.label.contains("文档")).cloned()
                    } else if is_fe {
                        active.iter().find(|c| c.name.contains("frontend") || c.name.contains("ui") || c.label.contains("前端")).cloned()
                    } else if is_be {
                        active.iter().find(|c| c.name.contains("arch") || c.name.contains("backend") || c.label.contains("架构")).cloned()
                    } else {
                        None
                    };
                    matched.or_else(|| if active.len() == 1 { active.into_iter().next() } else { None })
                };

                if let Some(card) = auto_card {
                    reviewer_persona = crate::agent::tools::role_manager::persona_block(&card);
                    reviewer_persona += &crate::agent::tools::role_manager::skills_block(&card);
                    effective_params["role"] = json!(card.name);
                    effective_params["role_label"] = json!(card.label);
                    if let Some(schema) = card.parse_custom_schema() {
                        reviewer_custom_schema = Some(schema);
                    }
                    if !card.model_id.is_empty() && !explicit_reviewer_slot {
                        reviewer_model = card.model_id.clone();
                    }
                    if card.max_concurrency > 0 {
                        effective_params["max_concurrency"] = json!(card.max_concurrency.min(crate::agent::child_reviewer::MAX_TOTAL_CHILDREN_PER_RUN as i64));
                    }
                } else {
                    effective_params["role"] = json!("system_reviewer");
                    effective_params["role_label"] = json!("🛡️ 跨模型综合审查官");
                }
            }

            if reviewer_model == routing.model_id && !explicit_reviewer_slot {
                // 未被角色卡钉死且未被会话显式指定时应用配置级异构优选；说明写进派发报告，人工可见
                let (preferred, note) = crate::agent::router::prefer_cross_family_reviewer(
                    &*ctx.state.inner().0.lock().unwrap(),
                    &reviewer_model,
                );
                reviewer_model = preferred;
                if let Some(note) = note {
                    item_reports.push(format!("[{}/{}]{}", item_num, total, note));
                }
            }

            // ---- 证据范围对账（investigate 通道）----
            // submit_plan 通道在 handle_submit_plan 已做同样计算；investigate 直连
            // Tech Lead 的派发若缺这一步，模型就能用自定义 custom_inputs 绕过证据
            // 校验——实弹实测（workspace_leak）正是此路径漏掉了「引而未改」的文件。
            // 未批准阶段的方案审查才计算；diff 只认声明字段的集合差，不判语义对错。
            if !ctx.checkpoint.plan_phase.is_approved()
                && effective_params.get("evidence_scope_refs").is_none()
            {
                let targets: Vec<String> = effective_params
                    .get("custom_inputs")
                    .and_then(|v| v.get("target_files"))
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let cited_refs: Vec<String> = effective_params
                    .get("evidence_refs")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let scope = crate::agent::impact_pack::evidence_scope_refs(
                    &targets,
                    &cited_refs,
                    Some(ctx.workspace_dir),
                );
                if !scope.is_empty() {
                    if let Some(obj) = effective_params.as_object_mut() {
                        obj["evidence_scope_refs"] = json!(scope);
                        let exc = obj
                            .get("evidence_excerpt")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        obj["evidence_excerpt"] = json!(format!(
                            "{}\n\n【📎 证据范围对账】以下文件被方案引为证据但未列入拟修改目标：{}",
                            exc,
                            obj["evidence_scope_refs"]
                                .as_array()
                                .map(|a| a
                                    .iter()
                                    .filter_map(|x| x.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", "))
                                .unwrap_or_default()
                        ));
                    }
                }
            }

            let mut snapshot = crate::agent::child_reviewer::PlanReviewerDispatchSnapshot::new(
                &reviewer_model,
                &model_tier,
                &reasoning_effort,
                &reviewer_persona,
                effective_params.clone(),
            )
            .with_purpose("plan_review")
            .with_batch_id(batch_id.clone());
            if let Some(schema) = reviewer_custom_schema {
                snapshot = snapshot.with_custom_schema(schema);
            }
            if let Some(spec) = effective_params.get("specialized_context").and_then(|v| v.as_str()) {
                snapshot = snapshot.with_specialized_context(spec);
            }

            crate::agent::control::mark_stage(
                ctx.final_session_id,
                "child_reviewer:envelope_spawn",
            );
            match snapshot
                .spawn(
                    Some(ctx.app.clone()),
                    ctx.state.inner(),
                    ctx.parent_run_id,
                    ctx.final_session_id,
                    ctx.workspace_dir,
                    ctx.interrupt_rx.clone(),
                    None,
                )
                .await
            {
                Ok(child_run_id) => {
                    crate::agent::control::mark_stage(
                        ctx.final_session_id,
                        "child_reviewer:spawn_returned",
                    );
                    // 派发快照：供消费端在 reviewer 故障（invalid_output/超时）时自动故障转移重派
                    if !ctx.checkpoint.plan_phase.is_approved() {
                        ctx.checkpoint.set_plan_reviewer_dispatch_snapshot(&snapshot);
                    }
                    let question_str = effective_params.get("question").and_then(|v| v.as_str()).unwrap_or("");
                    let question_brief = if question_str.chars().count() > 40 {
                        format!("{}...", question_str.chars().take(40).collect::<String>())
                    } else {
                        question_str.to_string()
                    };
                    item_reports.push(format!(
                        "[{}/{}] ✅ 已派发 child_run_id={} 审查任务: {}",
                        item_num, total, child_run_id, question_brief
                    ));
                    spawned_ids.push(child_run_id);
                    success_count += 1;
                }
                Err(error) => {
                    item_reports.push(format!(
                        "[{}/{}] ❌ 派发失败：{}",
                        item_num, total, error
                    ));
                    fail_count += 1;
                }
            }
        }

        // 一个审查任务都没起来（角色卡未授权/不存在、参数非法等）时回滚本轮审查预算：
        // 否则失败也会消耗仅有的 2 轮额度，额度耗尽后派发被限流、结论又永远收不到，
        // 交叉审查模式将陷入「既派不出、也写不了」的永久阻塞。
        if success_count == 0 && !ctx.checkpoint.plan_phase.is_approved() {
            ctx.checkpoint.rollback_plan_review_dispatch(pre_dispatch_consumed);
        }

        if success_count > 0 {
            ctx.checkpoint.set_plan_review_batch(&batch_id, spawned_ids.len());
        }

        if !is_batch {
            if success_count == 1 {
                let child_run_id = &spawned_ids[0];
                let mut stdout = format!(
                    "子审查任务已异步派发，child_run_id: {}。需要结论时调用 wait_jobs(job_ids:[\"{}\"]) 挂起等待，返回值中将直接给出完整审查结论；若先执行其他独立操作，结果也会在后续步骤自动注入。",
                    child_run_id, child_run_id
                );
                if inst.is_mixed_dispatch_format() {
                    stdout = format!("⚠️ 检测到混合格式（同时包含 dispatches 与单发字段），系统已优先采用 dispatches。\n\n{}", stdout);
                }
                DispatchResult { view: None,
                    stdout,
                    stderr: String::new(),
                    success: true,
                    route: "child_reviewer:spawned".to_string(),
                }
            } else {
                let err_msg = item_reports.into_iter().next().unwrap_or_else(|| "派发失败".to_string());
                let route = if dispatch_input_incomplete {
                    "child_dispatch:input_incomplete"
                } else {
                    "child_reviewer:rejected_or_failed"
                };
                DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: err_msg,
                    success: false,
                    route: route.to_string(),
                }
            }
        } else {
            let mut summary = format!(
                "已批量处理 {} 个审查任务派发请求（{} 成功，{} 失败）：\n{}",
                total, success_count, fail_count, item_reports.join("\n")
            );
            if !spawned_ids.is_empty() {
                let ids_formatted: Vec<String> = spawned_ids.iter().map(|id| format!("\"{}\"", id)).collect();
                summary.push_str(&format!(
                    "\n\n已成功派发的审查任务均在后台异步执行；需要结论时调用 wait_jobs(job_ids:[{}]) 挂起等待，或先推进其他独立操作。",
                    ids_formatted.join(", ")
                ));
            }
            if inst.is_mixed_dispatch_format() {
                summary = format!("⚠️ 检测到混合格式（同时包含 dispatches 与单发字段），系统已优先采用 dispatches 批量派发。\n\n{}", summary);
            }
            let breaker_alert = ctx.context.record_batch_dispatch_outcome(success_count > 0);
            if success_count > 0 {
                DispatchResult { view: None,
                    stdout: summary,
                    stderr: String::new(),
                    success: true,
                    route: format!("child_reviewer:batch:{}ok{}fail", success_count, fail_count),
                }
            } else {
                let mut err = format!("批量审查派发全部失败（共 {} 项）：\n{}", total, item_reports.join("\n"));
                if let Some(alert) = breaker_alert {
                    err = format!("{}\n\n{}", err, alert);
                }
                DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: err,
                    success: false,
                    route: "child_reviewer:rejected_or_failed".to_string(),
                }
            }
        }
    // --- B2.6 持续监听：注册周期巡检，外部变化以 watch_event 经 durable inbox 自动注入 ---
    }))
}
