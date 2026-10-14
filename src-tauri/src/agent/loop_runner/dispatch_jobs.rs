//! 后台任务回收臂：wait_jobs 挂起等待与完成结果注入。

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


pub(super) async fn dispatch_wait_jobs(
    ctx: &mut DispatchCtx<'_>,
    inst: &AgentInstruction,
) -> Result<ArmOutcome, String> {
    Ok(ArmOutcome::Done({
        let params = inst.get_params();
        if let Some(result) = crate::agent::tools::input_validation::native_rejection("wait_jobs", "", &params) {
            return Ok(ArmOutcome::Early(result));
        }
        let target_ids: Option<Vec<String>> = params
            .get("job_ids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            });
        let timeout_secs = params
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(180);

        // 同批连等自适应窗口：模型对同一 job 集合反复 wait_jobs 时，
        // 每多一圈等于烧一次完整 LLM 往返只为「还在跑吗」——实测单场
        // 21 次轮询 ≈ 1M prompt token。窗口按连等次数递增封顶 600s，
        // 换批即重置；返回值里披露放大后的窗口，模型仍可自主决定继续等
        // 或转去推进其他操作（不剥夺选择权，只减少空转往返）。
        let wait_key = target_ids
            .as_ref()
            .map(|ids| {
                let mut v = ids.clone();
                v.sort();
                v.join(",")
            })
            .unwrap_or_else(|| "<all>".to_string());
        if ctx.context.wait_jobs_last_key == wait_key {
            ctx.context.wait_jobs_streak = ctx.context.wait_jobs_streak.saturating_add(1);
        } else {
            ctx.context.wait_jobs_last_key = wait_key;
            ctx.context.wait_jobs_streak = 0;
        }
        let streak = ctx.context.wait_jobs_streak;
        let effective_timeout = (timeout_secs << streak.min(3)).min(600);

        let _ = ctx.app.emit(
            &format!("agent-log-{}", ctx.final_session_id),
            format!("⏳ 正在挂起等待后台异步任务完成 (超时限制: {}s)...", timeout_secs),
        );
        let _ = ctx.app.emit(
            &format!("agent-progress-{}", ctx.final_session_id),
            json!({
                "type": "agent_waiting_jobs",
                "message": "正在挂起等待后台任务完成...",
                "job_ids": target_ids.clone().unwrap_or_default(),
            }),
        );

        let mut rx_watch = ctx.interrupt_rx.clone();
        crate::agent::control::mark_stage(ctx.final_session_id, "wait_jobs:enter");
        match crate::agent::job_engine::wait_for_jobs(
            ctx.state.inner(),
            ctx.parent_run_id,
            target_ids.as_deref(),
            effective_timeout,
            Some(&mut rx_watch),
        )
        .await
        {
            Ok(completed_jobs) => {
                crate::agent::control::mark_stage(
                    ctx.final_session_id,
                    "wait_jobs:reduce_receipts",
                );
                // 消费语义统一汇聚到 child_receipt reducer（与主循环自动注入同一
                // 事实源）：阶段审查、故障转移、交付聚合、退款记账只在一处定义。
                // 每条回执的模型可见投影由 reducer 产出，按下方的 job_id 投影表
                // 替换输出循环里的原始 result_json 倾倒。
                let receipt_env = crate::agent::child_receipt::ReceiptEnv {
                    app: Some(ctx.app),
                    db: ctx.state.inner(),
                    parent_run_id: ctx.parent_run_id,
                    session_id: ctx.final_session_id,
                    workspace_dir: ctx.workspace_dir,
                    interrupt_rx: ctx.interrupt_rx.clone(),
                    reviewer_backup_model: &ctx.model_routing.reviewer_backup_model,
                };
                let mut stage_refund_pending = false;
                let mut applied_receipts: Vec<crate::agent::checkpoint::AppliedReceipt> =
                    Vec::new();
                let mut seen_receipt_ids: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                let mut projections: std::collections::HashMap<String, String> =
                    std::collections::HashMap::new();
                for job in completed_jobs.iter() {
                    if job.job_type.starts_with("child_") || job.job_type.contains("reviewer") {
                        if !seen_receipt_ids.insert(job.job_id.clone()) {
                            continue;
                        }
                        let receipt = job.to_delivery_receipt();
                        let effects = crate::agent::child_receipt::reduce_child_receipt(
                            &receipt_env,
                            ctx.checkpoint,
                            ctx.context,
                            &receipt,
                        )
                        .await;
                        stage_refund_pending |= effects.stage_refund_pending;
                        applied_receipts.push(crate::agent::checkpoint::AppliedReceipt {
                            run_id: job.job_id.clone(),
                            verdict: effects
                                .contract_violation
                                .clone()
                                .map(|err| ("invalid_output".to_string(), err)),
                        });
                        projections.insert(job.job_id.clone(), effects.model_text);
                        for notice in &effects.ui_notices {
                            let _ = ctx.app.emit(
                                &format!("agent-log-{}", ctx.final_session_id),
                                notice.clone(),
                            );
                        }
                    }
                }
                // 阶段审查退款统一结算（批循环结束后才判断在途/产出终态）
                let refund_note = if stage_refund_pending
                    && ctx.checkpoint.settle_stage_review_refund(ctx.state.inner())
                {
                    "\n🔄 阶段审查次数已退还（本轮含 infra 失败且无专家产出 blocking，视为未有效审查）。"
                } else {
                    ""
                };
                // 回执提交点：checkpoint（含 context 投影）+ applied_at + 裁决回写
                // 同事务原子落账——崩溃重投/重放安全由此保证。
                if !applied_receipts.is_empty() {
                    crate::agent::checkpoint::save_checkpoint_and_consume(
                        ctx.state.inner(),
                        ctx.checkpoint,
                        ctx.context,
                        &applied_receipts,
                    )?;
                }
                let mut out = String::new();
                if completed_jobs.is_empty() {
                    // 空结果消歧：区分「仍在途 / 结论待回收 / 已全部收口 / 从未派发」，
                    // 避免模型对已交付完毕的结果反复空轮询（旧会话曾连刷 295 步）。
                    if crate::agent::child_reviewer::has_running_child_runs(
                        ctx.state.inner(),
                        ctx.parent_run_id,
                    ) {
                        // 未完成待办数是纯事实披露：有可并行工作时干等是浪费，
                        // 没有可做的时等待本身合理——数字摆出来由模型判断。
                        let open_todos = ctx
                            .context
                            .todo_list
                            .iter()
                            .filter(|t| t.status == "pending" || t.status == "in_progress")
                            .count();
                        let todo_hint = if open_todos > 0 {
                            format!("（当前另有 {} 项未完成待办可并行推进）", open_todos)
                        } else {
                            String::new()
                        };
                        if streak > 0 {
                            out.push_str(&format!(
                                "等待结束：仍有后台任务在运行中（本次为第 {} 次连等同一批任务，窗口已自适应扩至 {}s）{}。可再次 wait_jobs 挂起等待，或先推进其他独立操作（结论产出后会自动注入）。",
                                streak + 1,
                                effective_timeout,
                                todo_hint
                            ));
                        } else {
                            out.push_str(&format!(
                                "等待结束：仍有后台任务在运行中，本次等待窗口内无新完成{}。可再次 wait_jobs 挂起等待，或先推进其他独立操作（结论产出后会自动注入）。",
                                todo_hint
                            ));
                        }
                    } else if crate::agent::child_reviewer::has_pending_review_results(
                        ctx.state.inner(),
                        ctx.parent_run_id,
                    ) {
                        out.push_str("等待结束：存在已终态但尚未回收的审查结论，请再次调用 wait_jobs 收取。");
                    } else {
                        let total_dispatched: i64 = ctx.state
                            .inner()
                            .0
                            .lock()
                            .ok()
                            .and_then(|conn| {
                                conn.query_row(
                                    "SELECT COUNT(*) FROM agent_child_runs WHERE parent_run_id = ?1",
                                    rusqlite::params![ctx.parent_run_id],
                                    |r| r.get(0),
                                )
                                .ok()
                            })
                            .unwrap_or(0);
                        if total_dispatched == 0 {
                            out.push_str("等待结束：当前没有已派发的后台任务（wait_jobs 仅对已派发的 child 任务生效）。请直接推进后续行动。");
                        } else {
                            out.push_str("等待结束：所有已派发任务的结论均已交付并消费完毕，当前无挂起任务——结果已在你的上下文中，请直接基于其推进后续行动，无需再次 wait_jobs。");
                        }
                    }
                } else {
                    out.push_str(refund_note);
                    out.push_str(&format!("✅ 已成功等待并接收到 {} 个后台任务结果：\n", completed_jobs.len()));
                    for job in completed_jobs {
                        out.push_str(&format!(
                            "\n- 【任务 ID: {} | 类型: {}】状态: {}, 耗时: {}ms\n  描述: {}\n  输出: {}\n",
                            job.job_id,
                            job.job_type,
                            job.status,
                            job.duration_ms,
                            job.title,
                            // 子任务回执用 reducer 产出的收敛投影；非子任务走通用限界投影
                            projections
                                .get(&job.job_id)
                                .cloned()
                                .unwrap_or_else(|| project_job_result_for_model(&job))
                        ));
                    }
                }
                DispatchResult { view: None,
                    stdout: out,
                    stderr: String::new(),
                    success: true,
                    route: "wait_jobs:resolved".to_string(),
                }
            }
            Err(err) => DispatchResult { view: None,
                stdout: String::new(),
                stderr: err,
                success: false,
                route: "wait_jobs:interrupted_or_failed".to_string(),
            },
        }
    // --- B2.6 特殊工具：resume_child 追问已完成的子任务 ---
    }))
}

/// wait_jobs 输出的模型可见投影（委托统一实现 BackgroundJobResult::model_projection）。
fn project_job_result_for_model(job: &crate::agent::job_engine::BackgroundJobResult) -> String {
    job.model_projection()
}

/// 后台任务主动中止（job_cancel）：模型判定后台任务卡死/跑偏/不再需要时
/// 显式终止——只处理 job_type='shell'（有 pid 的进程组可杀）；watch 走
/// watch_stop、子任务走对质/仲裁通道，不在此合并。
/// 杀信号走 TERM→存活检测→KILL 双阶段；终态落库由 spawn 时的看守线程
/// （wait() 返回后 finish_job）负责——本臂只发信号、不抢写状态，保持
/// "信号来源可多方，状态写只有看守一处"的单写者纪律。
pub(super) async fn dispatch_job_cancel(
    ctx: &mut DispatchCtx<'_>,
    inst: &AgentInstruction,
) -> Result<ArmOutcome, String> {
    Ok(ArmOutcome::Done({
        let params = inst.get_params();
        let job_id = params
            .get("job_id")
            .or_else(|| params.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if job_id.is_empty() {
            return Ok(ArmOutcome::Done(DispatchResult { view: None,
                stdout: String::new(),
                stderr: "❌ 缺少必填参数 job_id".to_string(),
                success: false,
                route: "job_cancel:missing".to_string(),
            }));
        }

        let conn = match ctx.state.inner().0.lock() {
            Ok(c) => c,
            Err(e) => {
                return Ok(ArmOutcome::Done(DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("❌ DB 锁获取失败: {}", e),
                    success: false,
                    route: "job_cancel:db".to_string(),
                }));
            }
        };
        let row: Option<(String, String, String)> = conn
            .query_row(
                "SELECT status, job_type, params_json FROM agent_background_jobs WHERE id=?1",
                rusqlite::params![job_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok();
        drop(conn);

        let (status, job_type, params_json) = match row {
            Some(r) => r,
            None => {
                return Ok(ArmOutcome::Done(DispatchResult { view: None,
                    stdout: String::new(),
                    stderr: format!("❌ 未找到后台任务 {}", job_id),
                    success: false,
                    route: "job_cancel:not_found".to_string(),
                }));
            }
        };

        if status != "running" {
            return Ok(ArmOutcome::Done(DispatchResult { view: None,
                stdout: format!("ℹ️ 任务 {} 已是终态（{}），无需中止。", job_id, status),
                stderr: String::new(),
                success: true,
                route: "job_cancel:already_terminal".to_string(),
            }));
        }

        if job_type != "shell" {
            return Ok(ArmOutcome::Done(DispatchResult { view: None,
                stdout: String::new(),
                stderr: format!(
                    "⚠️ 任务类型 [{}] 不支持 job_cancel 中止：watch 监听用 watch_stop，子任务用 resume_child 对质或人工仲裁。",
                    job_type
                ),
                success: false,
                route: "job_cancel:unsupported_type".to_string(),
            }));
        }

        let pid = serde_json::from_str::<serde_json::Value>(&params_json)
            .ok()
            .and_then(|v| v.get("pid").and_then(|p| p.as_u64()))
            .map(|p| p as u32);
        let Some(pid) = pid else {
            return Ok(ArmOutcome::Done(DispatchResult { view: None,
                stdout: String::new(),
                stderr: format!(
                    "⚠️ 任务 {} 行内无 pid 记录（可能来自旧版本启动），无法精确中止；可用 local_shell 按日志路径名手查进程后 kill。",
                    job_id
                ),
                success: false,
                route: "job_cancel:no_pid".to_string(),
            }));
        };

        // TERM 软杀 → 800ms 后存活则 KILL 补刀（卡死场景 TERM 可能无效）
        crate::agent::tools::local_shell::kill_process_group(pid, "TERM");
        let still_alive = {
            std::thread::sleep(std::time::Duration::from_millis(800));
            #[cfg(unix)]
            {
                std::path::Path::new(&format!("/proc/{}", pid)).exists()
            }
            #[cfg(not(unix))]
            {
                false // windows taskkill /F 已足够；不做二次探测
            }
        };
        if still_alive {
            crate::agent::tools::local_shell::kill_process_group(pid, "KILL");
        }

        DispatchResult { view: None,
            stdout: format!(
                "🛑 已向后台任务 {} (pid {}, 进程组) 发送终止信号{}。终态与退出码由看守线程落库，稍后可用 wait_jobs 收取结论。",
                job_id,
                pid,
                if still_alive { "（TERM 未响应已补 SIGKILL）" } else { "" }
            ),
            stderr: String::new(),
            success: true,
            route: "job_cancel:sent".to_string(),
        }
    }))
}
