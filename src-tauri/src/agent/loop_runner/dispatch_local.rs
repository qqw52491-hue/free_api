//! 通用本地工具臂：spawn_blocking 执行、可中断等待与结果观测注入。

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


pub(super) async fn dispatch_local_tool(
    ctx: &mut DispatchCtx<'_>,
    inst: &AgentInstruction,
) -> Result<ArmOutcome, String> {
    Ok(ArmOutcome::Done({
        let is_write_tool = is_write_or_side_effect_tool(inst);
        if is_write_tool {

            if let Some(scope_guard) = ctx.context
                .resource_scope
                .guard_known_outside_write(&inst.get_tool(), &inst.get_params())
            {
                let result = DispatchResult { view: None,
                    success: false,
                    stdout: String::new(),
                    stderr: format!(
                        "{}\nNo tool call was issued; original parameters were not changed.",
                        scope_guard.render()
                    ),
                    route: "scope_guard:outside".to_string(),
                };
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
        }

        // 外部副作用前先持久化 pending；未知/写操作默认 HumanReview。
        let recovery_policy = ctx.checkpoint.prepare_tool(inst.clone(), ctx.context);
        if recovery_policy.requires_pending() {
            save_checkpoint(ctx.state.inner(), ctx.checkpoint)?;
        }

        // 具体和本地工具交互 (非 ask_human 走普通逻辑)
        let inst_c = inst.clone();
        let registry_arc = ctx.registry_state.inner().clone();
        let db = ctx.state.inner().clone();
        let sid = ctx.final_session_id.clone();
        let ws_root = ctx.workspace_dir.to_path_buf();
        let registry = {
            let guard = registry_arc.lock().await;
            (*guard).clone()
        };

        let mut tool_task = tokio::task::spawn_blocking(move || {
            println!("执行动作: {:?}", inst_c);
            run_agent_step(&db, &sid, &inst_c, &registry, Some(&ws_root))
        });

        let mut dispatch_result = tokio::select! {
            res = &mut tool_task => {
                res.map_err(|e| e.to_string())?
            }
            Some(control) = ctx.rx.recv() => {
                let interrupt_reason = match control {
                    HumanControl::Cancel => "用户请求停止当前任务。".to_string(),
                    HumanControl::ReplaceGoal(goal) => format!("用户提交了新的任务目标：{}（已中断前序工具）", goal),
                    HumanControl::Answer(_) => "用户请求中断当前任务。".to_string(),
                };

                if is_write_tool {
                    // 写盘/事务类工具：给予最多 1.5 秒安全落盘缓冲期，避免破坏正在保存的文件
                    ctx.app.emit(
                        &format!("agent-log-{}", ctx.final_session_id),
                        "⏳ 收到停止指令，正在给予写盘/事务操作 1.5s 安全保存缓冲期...".to_string(),
                    ).ok();

                    tokio::select! {
                        res = &mut tool_task => {
                            ctx.app.emit(
                                &format!("agent-log-{}", ctx.final_session_id),
                                "✅ 写盘操作已安全完成，任务即将终止。".to_string(),
                            ).ok();
                            res.map_err(|e| e.to_string())?
                        }
                        _ = tokio::time::sleep(tokio::time::Duration::from_millis(1500)) => {
                            ctx.app.emit(
                                &format!("agent-log-{}", ctx.final_session_id),
                                "⚠️ 写盘操作耗时超 1.5s，已强制安全终止。".to_string(),
                            ).ok();
                            DispatchResult { view: None,
                                success: false,
                                stdout: interrupt_reason,
                                stderr: String::new(),
                                route: RUN_INTERRUPTED_ROUTE.to_string(),
                            }
                        }
                    }
                } else {
                    // 只读/网络/查询类工具：0 毫秒立刻中断退出
                    DispatchResult { view: None,
                        success: false,
                        stdout: interrupt_reason,
                        stderr: String::new(),
                        route: RUN_INTERRUPTED_ROUTE.to_string(),
                    }
                }
            }
        };


        if dispatch_result.route == RUN_INTERRUPTED_ROUTE {
            return Ok(ArmOutcome::Early(dispatch_result));
        }
        let output = if dispatch_result.success {
            dispatch_result.stdout.clone()
        } else {
            dispatch_result.stderr.clone()
        };
        if let Some(scope_observation) = ctx.context.resource_scope.observe_tool_result(
            &inst.get_tool(),
            &inst.get_params(),
            &output,
            if is_write_tool {
                ResourceOperation::Write
            } else {
                ResourceOperation::Read
            },
        ) {
            // 只有 outside（越界/写守卫）才值得贴给模型——inside 是默认预期、
            // unknown 是零信息量，两者每步都贴纯属噪音（实测 19 条/场）。
            // 内部记账（mismatch/revision）不受影响。
            if scope_observation.relation
                == crate::agent::context::resource_scope::ResourceRelation::Outside
            {
                let annotation = scope_observation.render();
                if dispatch_result.success {
                    dispatch_result.stdout =
                        format!("{}\n\n{}", dispatch_result.stdout, annotation);
                } else {
                    dispatch_result.stderr =
                        format!("{}\n\n{}", dispatch_result.stderr, annotation);
                }
            }
        }
        // 无进展重复检测：只在"相同工具+相同参数+相同输出/失败"反复发生时注入
        // 策略级提示；不修改、不补全、不拒绝模型提交的参数。
        if let Some(no_progress_notice) = ctx.context.no_progress.record(
            &inst.get_tool(),
            &inst.get_params(),
            dispatch_result.success,
            &output,
        ) {
            ctx.context
                .turns_history
                .push(crate::agent::context::types::ChatMessage {
                    role: "user".to_string(),
                    content: serde_json::json!(no_progress_notice),
                });
            println!("🔁 [No-Progress] 注入无进展提示: {}", inst.get_tool());
        }
        // 检索零命中偏航：换措辞换关键词连续零命中不会触发上面的重复检测
        // （签名各不相同）；这里按（工具族×作用域）组计数，只提示一次，
        // 同样只注入事实提示，不修改、不拒绝、不强制 finish。
        if let Some(zero_hit_notice) = ctx.context.zero_hit.record(
            &inst.get_tool(),
            &inst.get_params(),
            dispatch_result.success,
            &output,
        ) {
            ctx.context
                .turns_history
                .push(crate::agent::context::types::ChatMessage {
                    role: "user".to_string(),
                    content: serde_json::json!(zero_hit_notice),
                });
            println!("🎯 [Zero-Hit] 注入检索零命中偏航提示: {}", inst.get_tool());
        }

        dispatch_result
    }))
}
