//! child_writer 派发臂：隔离写入工兵派发与 apply 机械合入通道。

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

pub(super) async fn dispatch_child_writer(
    ctx: &mut DispatchCtx<'_>,
    inst: &AgentInstruction,
) -> Result<ArmOutcome, String> {
    Ok(ArmOutcome::Done({
        // 机械合入通道：审阅 writer diff 后由宿主把补丁原样搬回真实工作区。
        // 任一文件指纹冲突即整体拒绝，绝不盲目覆盖（详见 apply_writer_result）。
        {
            let top_params = inst.get_params();
            let action_lc = inst.get_action().to_ascii_lowercase();
            let nested_lc = top_params
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(action_lc.as_str(), "apply" | "apply_diff" | "apply_changes")
                || matches!(nested_lc.as_str(), "apply" | "apply_diff" | "apply_changes")
            {
                let child_run_id = top_params
                    .get("child_run_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if child_run_id.is_empty() {
                    return Ok(ArmOutcome::Early(DispatchResult { view: None,
                        stdout: String::new(),
                        stderr: "❌ child_writer.apply 缺少必要参数 child_run_id。".to_string(),
                        success: false,
                        route: "child_writer:apply_missing_id".to_string(),
                    }));
                }
                return Ok(ArmOutcome::Early(
                    match crate::agent::child_writer::apply_writer_result(
                        ctx.state.inner(),
                        &child_run_id,
                        ctx.final_session_id,
                        ctx.workspace_dir,
                    )
                    .await
                    {
                        Ok(report) => DispatchResult { view: None,
                            stdout: report,
                            stderr: String::new(),
                            success: true,
                            route: "child_writer:applied".to_string(),
                        },
                        Err(error) => DispatchResult { view: None,
                            stdout: String::new(),
                            stderr: error,
                            success: false,
                            route: "child_writer:apply_rejected".to_string(),
                        },
                    },
                ));
            }
        }
        let routing = crate::agent::router::AgentRouter::route_for_role(
            ctx.context,
            ctx.model_routing,
            ctx.step_id,
            crate::agent::router::roles::WRITER,
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

        for (idx, writer_params) in items.iter().enumerate() {
            let item_num = idx + 1;
            if let Err(error) = crate::agent::tools::input_validation::validate_child_item(ctx.state.inner(), "child_writer", writer_params) {
                item_reports.push(format!("[{}/{}] ❌ {}", item_num, total, error));
                fail_count += 1;
                dispatch_input_incomplete = true;
                continue;
            }
            let mut writer_persona = String::new();
            let mut writer_model = routing.model_id.clone();
            let mut effective_params = writer_params.clone();
            if let Some(obj) = effective_params.as_object_mut() {
                obj.remove("max_concurrency");
            }

            let mut writer_custom_schema = None;
            if let Some(role_name) = writer_params.get("role").and_then(|v| v.as_str()) {
                if crate::agent::tools::role_manager::is_card_disabled_for(&ctx.model_routing.disabled_role_cards, role_name, "writer") {
                    item_reports.push(format!(
                        "[{}/{}] ⏸️ 角色卡「{}」在当前会话的 writer 工位已被用户排除/休眠；请改用其他角色或直接以默认身份派发。",
                        item_num, total, role_name
                    ));
                    fail_count += 1;
                    continue;
                }
                match crate::agent::tools::role_manager::resolve_enabled_card(ctx.state.inner(), role_name) {
                    Ok(Some(card)) => {
                        if !card.allows_kind("writer") {
                            item_reports.push(format!(
                                "[{}/{}] ❌ 角色卡「{}」未授权挂载为 writer（允许类型: {}）。",
                                item_num, total, role_name, card.allowed_kinds
                            ));
                            fail_count += 1;
                            continue;
                        }
                        writer_persona = crate::agent::tools::role_manager::persona_block(&card);
                        writer_persona += &crate::agent::tools::role_manager::skills_block(ctx.state.inner(), &card);
                        if let Some(schema) = card.parse_custom_schema() {
                            writer_custom_schema = Some(schema);
                        }
                        if !card.model_id.is_empty() {
                            writer_model = card.model_id.clone();
                        }
                        effective_params = crate::agent::tools::role_manager::merge_writer_defaults(&card, writer_params);
                        effective_params["role"] = json!(card.name);
                        effective_params["role_label"] = json!(card.label);
                        if let Some(obj) = effective_params.as_object_mut() {
                            obj.remove("max_concurrency");
                        }
                        if !card.hard_scope.is_empty() {
                            effective_params["hard_scope"] = json!(card.hard_scope);
                        }
                        if card.max_concurrency > 0 {
                            effective_params["max_concurrency"] = json!(card.max_concurrency.min(crate::agent::child_reviewer::MAX_TOTAL_CHILDREN_PER_RUN as i64));
                        }
                        let focus_domains = card.parse_focus_domains();
                        let custom_inputs = writer_params.get("custom_inputs");
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
                        let last_verify_log = writer_params
                            .get("last_verify_log")
                            .or_else(|| writer_params.get("verify_log"))
                            .and_then(|v| v.as_str());
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
                effective_params["role"] = json!("system_writer");
                effective_params["role_label"] = json!("✍️ 隔离沙箱写入工兵");
            }

            let mut envelope = crate::agent::protocol::DispatchEnvelope::writer(
                ctx.parent_run_id,
                ctx.final_session_id,
                &writer_model,
                effective_params.clone(),
            )
            .with_task_id(format!("writer_{}", item_num))
            .with_tier(routing.tier.to_string())
            .with_effort(&reasoning_effort)
            .with_persona(writer_persona);

            if let Some(schema) = writer_custom_schema {
                envelope = envelope.with_custom_schema(schema);
            }
            if let Some(spec) = effective_params.get("specialized_context").and_then(|v| v.as_str()) {
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
                    let task_str = effective_params.get("task").and_then(|v| v.as_str()).unwrap_or("");
                    let task_brief = if task_str.chars().count() > 40 {
                        format!("{}...", task_str.chars().take(40).collect::<String>())
                    } else {
                        task_str.to_string()
                    };
                    item_reports.push(format!(
                        "[{}/{}] ✅ 已派发 child_run_id={} 写入任务: {}",
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
                let writer_params = &items[0];
                let mut stdout = format!(
                    "写入 worker 已在隔离副本上开工，child_run_id: {}。它完成修改并自行运行你指定的测试命令后，会以结构化回执（changed_files 含逐文件 diff、test_status、findings）回到你的上下文；用 wait_jobs(job_ids:[\"{}\"]) 回收。注意：你（root）才是真实工作区的唯一写入者——审阅其 diff 后可调用 child_writer.apply(child_run_id:\"{}\") 由宿主机械合入（任一文件指纹冲突即整体拒绝），或用常规写工具自行落实，最终以真实工作区验证为准。",
                    child_run_id, child_run_id, child_run_id
                );
                let task_str = writer_params.get("task").and_then(|v| v.as_str()).unwrap_or("");
                let has_excerpts = writer_params.get("context_excerpts").is_some();
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
                    route: "child_writer:spawned".to_string(),
                }
            } else {
                let err_msg = item_reports.into_iter().next().unwrap_or_else(|| "派发失败".to_string());
                let route = if dispatch_input_incomplete {
                    "child_dispatch:input_incomplete"
                } else {
                    "child_writer:rejected_or_failed"
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
                "已批量处理 {} 个写入 worker 派发请求（{} 成功，{} 失败）：\n{}",
                total, success_count, fail_count, item_reports.join("\n")
            );
            if !spawned_ids.is_empty() {
                let ids_formatted: Vec<String> = spawned_ids.iter().map(|id| format!("\"{}\"", id)).collect();
                summary.push_str(&format!(
                    "\n\n所有已派发的写入 worker 均在各自的私有副本上独立执行；完成后请调用 wait_jobs(job_ids:[{}]) 回收 diff 审查。",
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
                    route: format!("child_writer:batch:{}ok{}fail", success_count, fail_count),
                }
            } else {
                let mut err = format!("批量写入 worker 派发全部失败（共 {} 项）：\n{}", total, item_reports.join("\n"));
                if let Some(alert) = breaker_alert {
                    err = format!("{}\n\n{}", err, alert);
                }
                DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: err,
                    success: false,
                    route: "child_writer:rejected_or_failed".to_string(),
                }
            }
        }
    // --- B2.1 L2 只读侦察 worker：异步派发独立动作-观测循环，结果经 durable inbox 回收 ---
    }))
}
