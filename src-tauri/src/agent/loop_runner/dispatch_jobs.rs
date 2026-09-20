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
        match crate::agent::job_engine::wait_for_jobs(
            ctx.state.inner(),
            ctx.parent_run_id,
            target_ids.as_deref(),
            timeout_secs,
            Some(&mut rx_watch),
        )
        .await
        {
            Ok(completed_jobs) => {
                let mut stage_notes = String::new();
                let mut failover_notes = String::new();
                // 【部分成功+部分 infra failure 退款修复】在 for 循环内无法安全判断
                // has_in_flight_stage_review()：completed_jobs 按顺序处理，后续成功
                // 专家尚未从 run_ids 移除，导致失败专家处理时 has_in_flight == true，
                // 退款窗口永远错过。改为循环后统一结算。
                let mut had_infra_failure_for_refund = false;
                for job in completed_jobs.iter() {
                    // 阶段审查任务走独立语义：不置 plan consumed、基础设施失败退还阶段次数
                    let is_rebuttal_result = Some(job.job_id.as_str())
                        == ctx.checkpoint.stage_review_rebuttal_run_id.as_deref();
                    if ctx.checkpoint.matches_stage_review_run(&job.job_id)
                        || is_rebuttal_result
                    {
                        let value = serde_json::from_str::<serde_json::Value>(&job.result_json)
                            .unwrap_or(serde_json::json!({}));
                        let error_kind = value.get("error_kind").and_then(|e| e.as_str()).map(|s| s.to_string());
                        let parsed_kind = error_kind
                            .as_deref()
                            .map(crate::agent::types::ChildErrorKind::parse);
                        // 对质子任务技术性失败（审查员输出非法，未产生有效裁决）：
                        // 原 blocking 异议保留（不能因空 findings 误清账——那会把"审查
                        // 员失联"当成"异议撤回"），也不计对质拒撤回次数；允许重新
                        // resume_child 对质或等宿主判分越过该阶段自动清账。
                        // 须先于通用 infra 判定——InvalidOutput 同属 infra 集合。
                        if is_rebuttal_result
                            && parsed_kind == Some(crate::agent::types::ChildErrorKind::InvalidOutput)
                        {
                            ctx.checkpoint.remove_stage_review_run(&job.job_id);
                            ctx.checkpoint.stage_review_rebuttal_run_id = None;
                            stage_notes.push_str(
                                "\n⚖️ 对质子任务技术性失败（审查员输出非法，未产生有效裁决）：原 blocking 异议保留，可重新 resume_child 对质，或等宿主判分越过该阶段自动清账。",
                            );
                            continue;
                        }
                        if parsed_kind
                            .as_ref()
                            .map(|k| k.is_infra_failure())
                            .unwrap_or(false)
                        {
                            ctx.checkpoint.remove_stage_review_run(&job.job_id);
                            if is_rebuttal_result {
                                ctx.checkpoint.stage_review_rebuttal_run_id = None;
                            }
                            // 标记本批次有 infra 失败；退款在 for 循环结束后统一结算
                            // （此时 completed_jobs 中的其他成功专家可能还未从 run_ids 移除，
                            // 若在此处判断 has_in_flight_stage_review() 会误判为 true 而漏退款）
                            had_infra_failure_for_refund = true;
                            stage_notes.push_str("\n⚠️ 阶段审查未正常完成，该阶段视为未审。");
                            continue;
                        }
                        let mut withdrawn: Vec<String> = value
                            .get("withdrawn_findings")
                            .and_then(|w| w.as_array())
                            .map(|arr| arr.iter().filter_map(|i| i.as_str().map(|s| s.to_string())).collect())
                            .unwrap_or_default();
                        if let Some(verdicts) = value.get("objection_verdicts").and_then(|v| v.as_array()) {
                            for v in verdicts {
                                let is_withdraw = v.get("verdict").and_then(|s| s.as_str()).map(|s| s.eq_ignore_ascii_case("withdraw") || s.eq_ignore_ascii_case("withdrawn")).unwrap_or(false);
                                if is_withdraw {
                                    if let Some(id) = v.get("finding_id").and_then(|i| i.as_str()) {
                                        if !withdrawn.iter().any(|w| w == id) {
                                            withdrawn.push(id.to_string());
                                        }
                                    }
                                }
                            }
                        }
                        let is_blocked = |f: &serde_json::Value| {
                            f.get("severity").and_then(|s| s.as_str()).map(|s| s.eq_ignore_ascii_case("blocking")).unwrap_or(false)
                                && f.get("finality").and_then(|s| s.as_str()).map(|s| s != "final_only").unwrap_or(true)
                                && !f.get("id").and_then(|i| i.as_str()).map(|id| withdrawn.iter().any(|w| w == id)).unwrap_or(false)
                        };
                        let findings_arr = value.get("findings").and_then(|f| f.as_array()).cloned().unwrap_or_default();
                        ctx.checkpoint.remove_stage_review_run(&job.job_id);
                        if is_rebuttal_result {
                            ctx.checkpoint.record_stage_review_completed(&job.job_id);
                            ctx.checkpoint.stage_review_rebuttal_run_id = None;
                        }

                        let new_blockers = findings_arr.iter().filter(|f| is_blocked(f)).count() as u32;
                        if is_rebuttal_result {
                            let withdrawn_count = withdrawn.len() as u32;
                            ctx.checkpoint.stage_review_blockers_open = ctx.checkpoint
                                .stage_review_blockers_open
                                .saturating_sub(withdrawn_count);
                            if ctx.checkpoint.stage_review_blockers_open == 0 {
                                ctx.checkpoint.stage_review_blocker_source_run_id = None;
                            }
                        } else {
                            ctx.checkpoint.stage_review_blockers_open += new_blockers;
                            if new_blockers > 0 {
                                ctx.checkpoint.stage_review_blocker_source_run_id =
                                    Some(job.job_id.clone());
                            }
                        }

                        // 僵尸 blocker 治理：记录出生时的宿主判分阶段索引；宿主越过它即自动清账
                        if ctx.checkpoint.stage_review_blockers_open > 0 {
                            ctx.checkpoint.stage_review_blocker_stage =
                                crate::agent::eval_oracle::query_active_progress()
                                    .await
                                    .map(|(idx, _, _)| idx as u32);
                        } else if !ctx.checkpoint.has_in_flight_stage_review() {
                            ctx.checkpoint.stage_review_blocker_stage = None;
                        }

                        let reviewer_label = job.title.strip_prefix("审查专家: ")
                            .or_else(|| job.title.strip_prefix("子审查任务: "))
                            .unwrap_or(&job.title);

                        let new_final_only: Vec<String> = findings_arr.iter()
                            .filter(|f| {
                                f.get("severity").and_then(|s| s.as_str()).map(|s| s.eq_ignore_ascii_case("blocking")).unwrap_or(false)
                                    && f.get("finality").and_then(|s| s.as_str()).map(|s| s == "final_only").unwrap_or(false)
                                    && !f.get("id").and_then(|i| i.as_str()).map(|id| withdrawn.iter().any(|w| w == id)).unwrap_or(false)
                            })
                            .map(|f| {
                                let id = f.get("id").and_then(|i| i.as_str()).unwrap_or("?");
                                let why = f.get("why").and_then(|w| w.as_str()).unwrap_or("");
                                format!("[{}] {}: {}", reviewer_label, id, why)
                            })
                            .collect();
                        ctx.checkpoint.stage_review_final_only.extend(new_final_only);
                        ctx.checkpoint.stage_review_final_only.sort();
                        ctx.checkpoint.stage_review_final_only.dedup();

                        // 【对质失败自动降级】对质结果若仍维持 blocking（审查员以 confirmed
                        // 证据拒不撤回）累计失败次数；2 轮后自动降级 final_only 记账放行——
                        // 否则"审查员不撤 + 写入全禁 + 模型无处去"三方锁死（pro×glm 实测死锁）。
                        if is_rebuttal_result {
                            let blocker_lines: Vec<String> = findings_arr
                                .iter()
                                .filter(|f| is_blocked(f))
                                .map(|f| {
                                    format!(
                                        "{}: {}",
                                        f.get("id").and_then(|i| i.as_str()).unwrap_or("?"),
                                        f.get("why").and_then(|w| w.as_str()).unwrap_or("")
                                    )
                                })
                                .collect();
                            if ctx.checkpoint.note_stage_review_confrontation_maintained_for(Some(&job.job_id), blocker_lines, 2) {
                                stage_notes.push_str(
                                    "\n⚖️ 【对质失败自动降级】blocking 异议经 2 轮对质仍被审查员维持，已降级为 final_only 记账带入收尾审查（收尾前必须逐条解决），写入已放行。",
                                );
                            }
                        } else {
                            ctx.checkpoint.stage_review_confrontation_failures = 0;
                            ctx.checkpoint.stage_review_confrontation_target = None;
                        }

                        let in_flight_remaining = ctx.checkpoint.stage_review_run_ids.len();
                        let committee_suffix = if in_flight_remaining > 0 {
                            format!("（联合会审仍在途，剩余 {} 位专家）", in_flight_remaining)
                        } else {
                            String::new()
                        };

                        stage_notes.push_str(&format!(
                            "\n📋 阶段审查结论已回收 (专家: {}): 本次 blocking {} 条 / 累计 blocking_regardless 未解决 {} 条 / final_only 记账 {} 条。{}{}",
                            reviewer_label,
                            new_blockers,
                            ctx.checkpoint.stage_review_blockers_open,
                            ctx.checkpoint.stage_review_final_only.len(),
                            if ctx.checkpoint.stage_review_blockers_open > 0 { "后续写入已被拦截，请用 resume_child 对质解套。" } else { "" },
                            committee_suffix
                        ));
                        continue;
                    }
                    if !(job.job_type == "child_reviewer" || job.job_type.contains("reviewer")) {
                        continue;
                    }
                    // 基础设施失败（超时/取消/中断/非法输出/请求层失败）不是实质性审查：
                    // 可故障转移的先换备用审查员重派，兜底退款且不算消费过。
                    // kind 归一由 ChildErrorKind::parse 承担（"timed_out"/"timeout" 同义）。
                    let infra_kind = serde_json::from_str::<serde_json::Value>(&job.result_json)
                        .ok()
                        .and_then(|v| v.get("error_kind").and_then(|e| e.as_str()).map(|s| s.to_string()));
                    let parsed_kind = infra_kind
                        .as_deref()
                        .map(crate::agent::types::ChildErrorKind::parse);
                    if parsed_kind
                        .as_ref()
                        .map(|k| k.is_infra_failure())
                        .unwrap_or(false)
                    {
                        // 【冒烟实测抓到的坑】失败类结论必须先走 failover 重派备用审查员；
                        // 只退款的话主 Agent 会对着已失败的审查任务永远 wait（信箱无结论可收）。
                        // request_or_stream（402 欠费/断流等零产出失败）与超时同性质，同样换平台重派；
                        // invalid_output 也走同一链（此前单列分支，语义一致故合并）。
                        if parsed_kind
                            .as_ref()
                            .map(|k| k.is_failover_eligible())
                            .unwrap_or(false)
                        {
                            match crate::agent::main_loop::try_plan_reviewer_failover(
                                ctx.app,
                                ctx.state.inner(),
                                ctx.parent_run_id,
                                ctx.final_session_id,
                                ctx.workspace_dir,
                                ctx.checkpoint,
                                ctx.interrupt_rx.clone(),
                                parsed_kind.as_ref().unwrap(),
                            )
                            .await
                            {
                                Ok(Some(new_run_id)) => {
                                    ctx.checkpoint.replace_expected_reviewer_run_id(&job.job_id, &new_run_id);
                                    failover_notes.push_str(&format!(
                                        "\n🔁 审查者故障转移：上次结论异常（{}），已自动改用备用审查员重派 (child_run_id: {})\n",
                                        parsed_kind.as_ref().unwrap(), new_run_id
                                    ));
                                    continue;
                                }
                                _ => {}
                            }
                        }
                        ctx.checkpoint.record_reviewer_failure_for_run(&job.job_id, infra_kind.as_deref().unwrap_or("infra_failure"));
                        ctx.checkpoint.refund_infra_failed_review_round(parsed_kind);
                        continue;
                    }
                    // wait_jobs 交付路径通过 record_reviewer_delivery_for_run 实现齐套屏障、幂等去重与现场真聚合
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&job.result_json) {
                        let delivery: crate::agent::child_reviewer::ReviewerDeliveryData = serde_json::from_value(value.clone()).unwrap_or_else(|_| {
                            let withdrawn: Vec<String> = value
                                .get("withdrawn_findings")
                                .and_then(|w| w.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|i| i.as_str().map(|s| s.to_string()))
                                        .collect()
                                })
                                .unwrap_or_default();
                            let objection_verdicts: Vec<crate::agent::child_reviewer::ObjectionVerdict> = value
                                .get("objection_verdicts")
                                .and_then(|v| serde_json::from_value(v.clone()).ok())
                                .unwrap_or_default();
                            let scorecard: Option<crate::agent::child_reviewer::ReviewScorecard> = value
                                .get("scorecard")
                                .and_then(|v| serde_json::from_value(v.clone()).ok());
                            let findings: Vec<crate::agent::child_reviewer::ReviewFinding> = value
                                .get("findings")
                                .and_then(|f| serde_json::from_value(f.clone()).ok())
                                .unwrap_or_default();
                            crate::agent::child_reviewer::ReviewerDeliveryData {
                                findings,
                                confidence: value.get("confidence").and_then(|v| v.as_str()).unwrap_or("confirmed").to_string(),
                                evidence_refs: Vec::new(),
                                missing_evidence: Vec::new(),
                                routing: value.get("routing").and_then(|v| v.as_str()).map(|s| s.to_string()),
                                withdrawn_findings: withdrawn,
                                objection_verdicts,
                                scorecard,
                                extra: Default::default(),
                            }
                        });
                        ctx.checkpoint.record_reviewer_delivery_for_run(Some(&job.job_id), &delivery);
                        if let Some(rev) =
                            crate::agent::checkpoint::AgentCheckpoint::plan_revision_from(&delivery)
                        {
                            let _ = ctx.app.emit(
                                &format!("agent-log-{}", ctx.final_session_id),
                                format!(
                                    "📝 Tech Lead 已在 1v1 中修订并定稿方案（修订字段: {}），submitted_plan 已切换为审查者版本",
                                    rev.describe_fields()
                                ),
                            );
                        }
                    } else {
                        ctx.checkpoint.record_reviewer_failure_for_run(&job.job_id, "corrupted_json_output");
                    }
                }
                // 【infra failure 退款统一结算】
                // 必须在 for 循环结束后执行：此时所有 completed_jobs 都已被处理，
                // has_in_flight_stage_review() 才能准确反映是否还有真正在途的任务。
                // 仅当无任何成功专家留下 blocker 时才退款——若成功专家已发现真实问题，
                // 本轮审查有实质性输出，不应退还预算。
                if had_infra_failure_for_refund
                    && !ctx.checkpoint.has_in_flight_stage_review()
                    && ctx.checkpoint.stage_review_blockers_open == 0
                    && ctx.checkpoint.reviewer_budget_refunds < 3
                    && ctx.checkpoint.stage_review_budget_used > 0
                {
                    ctx.checkpoint.stage_review_budget_used -= 1;
                    ctx.checkpoint.reviewer_budget_refunds += 1;
                    stage_notes.push_str("\n🔄 阶段审查次数已退还（本轮含 infra 失败且无专家产出 blocking，视为未有效审查）。");
                }
                let mut out = String::new();
                if completed_jobs.is_empty() {
                    // 空结果消歧：区分「仍在途 / 结论待回收 / 已全部收口 / 从未派发」，
                    // 避免模型对已交付完毕的结果反复空轮询（旧会话曾连刷 295 步）。
                    if crate::agent::child_reviewer::has_running_child_runs(
                        ctx.state.inner(),
                        ctx.parent_run_id,
                    ) {
                        out.push_str("等待结束：仍有后台任务在运行中，本次等待窗口内无新完成。可再次 wait_jobs 挂起等待，或先推进其他独立操作（结论产出后会自动注入）。");
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
                    out.push_str(&failover_notes);
                    out.push_str(&stage_notes);
                    out.push_str(&format!("✅ 已成功等待并接收到 {} 个后台任务结果：\n", completed_jobs.len()));
                    for job in completed_jobs {
                        out.push_str(&format!(
                            "\n- 【任务 ID: {} | 类型: {}】状态: {}, 耗时: {}ms\n  描述: {}\n  输出: {}\n",
                            job.job_id,
                            job.job_type,
                            job.status,
                            job.duration_ms,
                            job.title,
                            project_job_result_for_model(&job)
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

/// wait_jobs 输出的模型可见投影：保存与展示分离——完整 result_json 已持久化在
/// agent_child_runs，模型侧只给有界视图。writer 走与主循环自动注入相同的结构化
/// 视图（diff 已由 build_changed_files 分级降级，全量补丁经 patch_ref 工件取回）；
/// 其余类型按字符硬上限截断，防止一个巨型回执淹掉后续所有上下文。
fn project_job_result_for_model(job: &crate::agent::job_engine::BackgroundJobResult) -> String {
    const JOB_RESULT_PROJECTION_MAX_CHARS: usize = 16_000;
    if job.result_json.is_empty() {
        return "（无附加输出）".to_string();
    }
    if job.job_type == "child_writer" {
        if let Ok(wr) = serde_json::from_str::<crate::agent::child_writer::WriterResult>(&job.result_json) {
            return crate::agent::child_writer::writer_result_model_view(&wr);
        }
    }
    let total = job.result_json.chars().count();
    if total <= JOB_RESULT_PROJECTION_MAX_CHARS {
        return job.result_json.clone();
    }
    let head: String = job.result_json.chars().take(JOB_RESULT_PROJECTION_MAX_CHARS).collect();
    format!(
        "{}\n…（输出共 {} 字符，已截断展示；完整结果持久化于子任务账本 child_run_id={}，可经 resume_child 追问复查）",
        head, total, job.job_id
    )
}
