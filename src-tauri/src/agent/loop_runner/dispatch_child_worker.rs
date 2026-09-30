//! child_worker 派发臂：只读侦察子 Agent 的异步派发。

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

pub(super) async fn dispatch_child_worker(
    ctx: &mut DispatchCtx<'_>,
    inst: &AgentInstruction,
) -> Result<ArmOutcome, String> {
    Ok(ArmOutcome::Done({
        let routing = crate::agent::router::AgentRouter::route_for_role(
            ctx.context,
            ctx.model_routing,
            ctx.step_id,
            crate::agent::router::roles::SCOUT,
        );
        let reasoning_effort = ctx.model_routing.reasoning_effort_for(&routing.tier).to_string();
        let items = inst.get_dispatch_items();
        let total = items.len();
        let is_batch = total > 1;

        let mut item_reports = Vec::new();
        let mut spawned_ids = Vec::new();
        let mut success_count = 0;
        let mut fail_count = 0;
        // 单发场景标记：唯一派发项因角色卡必填入参缺失被拒时，route 透出 input_incomplete 便于门禁归因
        let mut dispatch_input_incomplete = false;

        for (idx, worker_params) in items.iter().enumerate() {
            let item_num = idx + 1;
            let mut scout_persona = String::new();
            let mut scout_model = routing.model_id.clone();
            let mut effective_worker_params = worker_params.clone();
            if let Some(obj) = effective_worker_params.as_object_mut() {
                obj.remove("max_concurrency");
            }

            let mut scout_custom_schema = None;
            if let Some(role_name) = worker_params.get("role").and_then(|v| v.as_str()) {
                if crate::agent::tools::role_manager::is_card_disabled_for(&ctx.model_routing.disabled_role_cards, role_name, "worker") {
                    item_reports.push(format!(
                        "[{}/{}] ⏸️ 角色卡「{}」在当前会话的 worker/scout 工位已被用户排除/休眠；请改用其他角色或直接以默认身份派发。",
                        item_num, total, role_name
                    ));
                    fail_count += 1;
                    continue;
                }
                match crate::agent::tools::role_manager::resolve_enabled_card(ctx.state.inner(), role_name) {
                    Ok(Some(card)) => {
                        if !card.allows_kind("worker") {
                            item_reports.push(format!(
                                "[{}/{}] ❌ 角色卡「{}」未授权挂载为 worker（允许类型: {}）。",
                                item_num, total, role_name, card.allowed_kinds
                            ));
                            fail_count += 1;
                            continue;
                        }
                        scout_persona = crate::agent::tools::role_manager::persona_block(&card);
                        scout_persona += &crate::agent::tools::role_manager::skills_block(ctx.state.inner(), &card);
                        effective_worker_params["role"] = json!(card.name);
                        effective_worker_params["role_label"] = json!(card.label);
                        if let Some(schema) = card.parse_custom_schema() {
                            scout_custom_schema = Some(schema);
                        }
                        if !card.model_id.is_empty() {
                            scout_model = card.model_id.clone();
                        }
                        if !card.hard_scope.is_empty() {
                            effective_worker_params["hard_scope"] = json!(card.hard_scope);
                        }
                        if card.max_concurrency > 0 {
                            effective_worker_params["max_concurrency"] = json!(card.max_concurrency.min(crate::agent::child_reviewer::MAX_TOTAL_CHILDREN_PER_RUN as i64));
                        }
                        let focus_domains = card.parse_focus_domains();
                        let custom_inputs = worker_params.get("custom_inputs");
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
                        let last_verify_log = worker_params
                            .get("last_verify_log")
                            .or_else(|| worker_params.get("verify_log"))
                            .and_then(|v| v.as_str());
                        if let Some(spec_ctx) = crate::agent::context_collector::assemble_focus_context(
                            &focus_domains,
                            ctx.workspace_dir,
                            last_verify_log,
                            custom_inputs,
                        ).await {
                            effective_worker_params["specialized_context"] = json!(spec_ctx);
                        }
                        if let Some(merged) = merged_inputs {
                            effective_worker_params["custom_inputs"] = merged;
                        }
                    }
                    Ok(None) => {
                        item_reports.push(format!(
                            "[{}/{}] ❌ 角色卡不存在或已禁用：{}。",
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
                effective_worker_params["role"] = json!("system_worker");
                effective_worker_params["role_label"] = json!("🔍 独立只读侦察专员");
            }

            let mut envelope = crate::agent::protocol::DispatchEnvelope::worker(
                ctx.parent_run_id,
                ctx.final_session_id,
                &scout_model,
                effective_worker_params.clone(),
            )
            .with_task_id(format!("worker_{}", item_num))
            .with_tier(routing.tier.to_string())
            .with_effort(&reasoning_effort)
            .with_persona(scout_persona);

            if let Some(schema) = scout_custom_schema {
                envelope = envelope.with_custom_schema(schema);
            }
            if let Some(spec) = effective_worker_params.get("specialized_context").and_then(|v| v.as_str()) {
                envelope = envelope.with_specialized_context(spec);
            }

            match envelope
                .dispatch(
                    Some(ctx.app.clone()),
                    ctx.state.inner(),
                    ctx.workspace_dir,
                    ctx.interrupt_rx.clone(),
                )
                .await
            {
                Ok(child_run_id) => {
                    let task_str = effective_worker_params.get("task").and_then(|v| v.as_str()).unwrap_or("");
                    let task_brief = if task_str.chars().count() > 40 {
                        format!("{}...", task_str.chars().take(40).collect::<String>())
                    } else {
                        task_str.to_string()
                    };
                    item_reports.push(format!(
                        "[{}/{}] ✅ 已派发 child_run_id={} 任务: {}",
                        item_num, total, child_run_id, task_brief
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

        if !is_batch {
            if success_count == 1 {
                let child_run_id = &spawned_ids[0];
                let worker_params = &items[0];
                let mut stdout = format!(
                    "侦察 worker 已异步派发，child_run_id: {}。worker 拥有独立上下文并自行执行只读侦察（读文件/列目录/只读命令），完成后以结构化结论（findings/confidence/evidence_refs/missing_evidence）回到你的上下文；需要结论时调用 wait_jobs(job_ids:[\"{}\"]) 挂起等待，或先推进其他独立操作。finish 前须确认其已结束。",
                    child_run_id, child_run_id
                );
                let task_str = worker_params.get("task").and_then(|v| v.as_str()).unwrap_or("");
                let has_excerpts = worker_params.get("context_excerpts").is_some();
                if crate::agent::types::is_task_thin(task_str) && !has_excerpts {
                    stdout = format!("⚠️ 本次派发任务描述较简略，子 Agent 可能缺乏必要背景，建议在后续派发时携带 context_excerpts（包含 background、file_hints 或 todo_slice）。\n\n{}", stdout);
                }
                if inst.is_mixed_dispatch_format() {
                    stdout = format!("⚠️ 检测到混合格式（同时包含 dispatches 与单发字段），系统已优先采用 dispatches。\n\n{}", stdout);
                }
                DispatchResult { view: None,
                    stdout,
                    stderr: String::new(),
                    success: true,
                    route: "child_worker:spawned".to_string(),
                }
            } else {
                let err_msg = item_reports.into_iter().next().unwrap_or_else(|| "派发失败".to_string());
                let route = if dispatch_input_incomplete {
                    "child_dispatch:input_incomplete"
                } else {
                    "child_worker:rejected_or_failed"
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
                "已批量处理 {} 个 worker 派发请求（{} 成功，{} 失败）：\n{}",
                total, success_count, fail_count, item_reports.join("\n")
            );
            if !spawned_ids.is_empty() {
                let ids_formatted: Vec<String> = spawned_ids.iter().map(|id| format!("\"{}\"", id)).collect();
                summary.push_str(&format!(
                    "\n\n已成功派发的 worker 均在后台异步执行；需要其结论时调用 wait_jobs(job_ids:[{}]) 挂起等待，或先推进其他独立操作。",
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
                    route: format!("child_worker:batch:{}ok{}fail", success_count, fail_count),
                }
            } else {
                let mut err = format!("批量派发全部失败（共 {} 项）：\n{}", total, item_reports.join("\n"));
                if let Some(alert) = breaker_alert {
                    err = format!("{}\n\n{}", err, alert);
                }
                DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: err,
                    success: false,
                    route: "child_worker:rejected_or_failed".to_string(),
                }
            }
        }
    // --- B2.5 特殊工具：wait_jobs 反应式零 Token 挂起等待 ---
    }))
}
