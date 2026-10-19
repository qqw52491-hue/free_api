//! 子任务交付回执统一 reducer（单一事实源）。
//!
//! 三条消费通道——主循环自动注入（poll_completed_delivery_receipts）、
//! wait_jobs 手动收取、通用后台信箱（poll_pending_inbox）——都把传输层结果
//! 归一为 DeliveryReceipt 后汇聚到这里。checkpoint 状态变更、审查者故障转移、
//! 预算退款记账、模型可见投影只在本文件出现一次，杜绝通道间语义漂移
//! （此前 wait_jobs 内联重写了阶段审查处理，且自动注入路径漏掉
//! 齐套屏障的 expected-id 替换，曾因通道分叉造成死锁级 bug）。

use crate::agent::checkpoint::AgentCheckpoint;
use crate::agent::context::SandwichContext;
use crate::agent::protocol::DeliveryReceipt;
use crate::agent::types::ChildErrorKind;
use crate::db::DbState;
use std::path::Path;
use tauri::AppHandle;
use tokio::sync::watch;

/// reducer 运行环境：派发/故障转移所需依赖打包
pub(crate) struct ReceiptEnv<'a> {
    pub app: Option<&'a AppHandle>,
    pub db: &'a DbState,
    pub parent_run_id: &'a str,
    pub session_id: &'a str,
    pub workspace_dir: &'a Path,
    pub interrupt_rx: watch::Receiver<u64>,
    pub reviewer_backup_model: &'a str,
}

/// reducer 输出：checkpoint 状态变更已就地完成，这里只承载外部可见效果。
#[derive(Debug, Default)]
pub(crate) struct ReceiptEffects {
    /// 给模型看的完整注入块（含标题横幅，投影已按类型收敛）
    pub model_text: String,
    /// 触发了审查者故障转移重派（新子任务 run_id）——旧回执结论已被取代
    pub redispatched_to: Option<String>,
    /// 阶段审查 infra 失败待结算标记：调用方在批循环结束后统一调用
    /// checkpoint.settle_stage_review_refund()（逐条结算会误判在途）
    pub stage_refund_pending: bool,
    /// 需广播到前端的 UI 通知（如 Tech Lead 定稿）
    pub ui_notices: Vec<String>,
    /// 交付契约违规：消费端裁决降级的 error_kind 值（contract_violation:...）。
    /// 调用方在提交点把它回写到台账 status/error_kind，与 applied_at 同事务落账。
    pub contract_violation: Option<String>,
}

/// 交付契约运行时校验（三条通道汇合后的唯一关口）：
/// run 台账落库的 contract_schema 存在且交付为成功态时，校验 payload 满足
/// 扩展字段约束（必填字段/类型/枚举值/闭集白名单）；违规就地降级为
/// invalid_output，后续走既有的非法输出失败语义（reviewer→记账+可故障转移，
/// 阶段审查→未审不耗预算）。此前 custom_schema 只在派发时渲染进提示词，
/// 没有任何运行期校验。
fn enforce_delivery_contract(db: &DbState, receipt: &mut DeliveryReceipt) {
    if !receipt.is_success() {
        return;
    }
    let schema_json: Option<String> = db
        .0
        .lock()
        .ok()
        .and_then(|conn| {
            conn.query_row(
                "SELECT contract_schema FROM agent_child_runs WHERE id = ?1",
                rusqlite::params![receipt.child_run_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten()
        });
    let Some(schema_json) = schema_json else { return };
    let Ok(mut schema) =
        serde_json::from_str::<crate::agent::protocol::ContractSchema>(&schema_json)
    else {
        return;
    };
    // 旧审查台账可能仍要求 consumer_verdicts；仅在内存中过滤这项已退出义务。
    if matches!(receipt.kind.as_str(), "review" | "reviewer") {
        crate::agent::child_reviewer::retire_consumer_schema(&mut schema);
    }
    if schema.is_empty() {
        return;
    }
    if let Err(err) = receipt.validate_against_schema(&schema) {
        receipt.status = "invalid_output".to_string();
        receipt.error_kind =
            Some(crate::agent::types::ChildErrorKind::ContractViolation(err).db_value());
    }
}

/// 消费一张子任务交付回执：checkpoint 状态变更 + 有界模型投影。
/// 调用方职责：聚合返回的 model_text、批后调用 settle_stage_review_refund、
/// 广播 ui_notices。
pub(crate) async fn reduce_child_receipt(
    env: &ReceiptEnv<'_>,
    checkpoint: &mut AgentCheckpoint,
    context: &mut SandwichContext,
    receipt: &DeliveryReceipt,
) -> ReceiptEffects {
    // ── 交付契约运行时校验（所有种类统一）：违规转 invalid_output ──
    let mut receipt_owned = receipt.clone();
    enforce_delivery_contract(env.db, &mut receipt_owned);
    // 降级发生的判据：原本成功 → 违规后 invalid_output。裁决随提交点回写台账。
    let contract_violation = if receipt.is_success()
        && receipt_owned.status == "invalid_output"
    {
        receipt_owned.error_kind.clone()
    } else {
        None
    };
    let receipt = &receipt_owned;

    // 请求/超时/无效结构是审查通道失败，不是“方案不通过”。
    // 必须先处理备用，再进入各类审查聚合；未配置或备用已失败则进入统一失败出口。
    let kind = receipt.error_kind.as_deref().map(ChildErrorKind::parse);
    let channel_failed = receipt.kind == "review" && !receipt.is_success()
        && kind.as_ref().is_some_and(|k| k.is_failover_eligible()
            || matches!(k, ChildErrorKind::ContractViolation(_)));
    if channel_failed {
        let failover = crate::agent::main_loop::try_plan_reviewer_failover(env.app, env.db,
            env.parent_run_id, env.session_id, env.workspace_dir, checkpoint,
            env.interrupt_rx.clone(), env.reviewer_backup_model, kind.as_ref().unwrap(),
            Some(&receipt.child_run_id)).await;
        match failover {
            Ok(Some(new_id)) => return ReceiptEffects {
                model_text: format!("审查通道失败（{}）；仅改用用户配置的备用重派 child_run_id={}。没有批准方案。", receipt.child_run_id, new_id),
                redispatched_to: Some(new_id), contract_violation, ..Default::default()
            },
            other => {
                let backup_note = match other {
                    Err(e) => format!("备用派发失败：{e}"),
                    _ if env.reviewer_backup_model.trim().is_empty() => "未配置审查备用模型".into(),
                    _ => "配置的备用已失败或与当前模型相同，不再重派".into(),
                };
                let detail: String = receipt.payload.as_ref().map(ToString::to_string).unwrap_or_default().chars().take(1600).collect();
                checkpoint.pending_task_failure.get_or_insert_with(|| format!(
                    "审查通道无法继续：child_run_id={}，异常={}；{}。诊断：{}",
                    receipt.child_run_id, receipt.error_kind.as_deref().unwrap_or("unknown"), backup_note, detail));
            }
        }
    }

    // ── 阶段审查/对质回执：独立语义（不占方案审查预算，infra 失败视为未审）──
    // 归属判定由台账 purpose/batch_id 推导（含已交付成员——对质目标是交付过的审查员）
    if checkpoint.is_stage_review_target(env.db, &receipt.child_run_id) {
        let oracle_index = crate::agent::eval_oracle::query_active_progress()
            .await
            .map(|(idx, _, _)| idx as u32);
        let outcome =
            crate::agent::stage_review::apply_stage_receipt(checkpoint, env.db, receipt, oracle_index);
        return ReceiptEffects {
            model_text: format!(
                "【📋 阶段审查 (child_run_id: {})】\n{}",
                receipt.child_run_id, outcome.note
            ),
            stage_refund_pending: outcome.refund_pending,
            contract_violation,
            ..Default::default()
        };
    }

    let mut effects = ReceiptEffects {
        contract_violation,
        ..Default::default()
    };
    match receipt.kind.as_str() {
        "writer" => {
            let mut writer_res = receipt.as_writer_result();
            if let Some(ref mut wr) = writer_res {
                // 消费回执的瞬间现算主工作区指纹，闭合 TOCTOU 竞争窗口
                let git_available =
                    crate::agent::child_writer::is_git_available(env.workspace_dir).await;
                crate::agent::child_writer::refresh_file_conflicts(
                    &mut wr.changed_files,
                    env.workspace_dir,
                    git_available,
                )
                .await;
                effects.model_text = format!(
                    "【🛠️ 写入工兵已完成 (child_run_id: {})】\n{}",
                    wr.child_run_id,
                    crate::agent::child_writer::writer_result_model_view(wr)
                );
            } else {
                effects.model_text = format!(
                    "【🛠️ 写入工兵已完成 (child_run_id: {})】\n结论: {}\n发现:\n- {}",
                    receipt.child_run_id,
                    receipt.confidence(),
                    format_findings(&receipt.findings())
                );
            }
        }
        "worker" => {
            if let Some(ref wrk) = receipt.as_worker_result() {
                effects.model_text = format!(
                    "【🕵️ 只读侦察任务已完成 (child_run_id: {})】\n{}",
                    receipt.child_run_id,
                    crate::agent::child_worker::worker_result_model_view(wrk)
                );
            } else {
                effects.model_text = format!(
                    "【🕵️ 只读侦察任务已完成 (child_run_id: {})】\n结论: {}\n发现:\n- {}",
                    receipt.child_run_id,
                    receipt.confidence(),
                    format_findings(&receipt.findings())
                );
            }
        }
        "review" => {
            effects.model_text = reduce_reviewer_receipt(env, checkpoint, context, receipt, &mut effects).await;
            if effects.redispatched_to.is_some() {
                return effects;
            }
        }
        _ => {
            // 未知子任务类型：不做状态变更，仅给有界投影（fail-open）
            effects.model_text = format!(
                "【子任务已完成 (child_run_id: {}, 类型: {})】\n结论: {}\n",
                receipt.child_run_id,
                receipt.kind,
                receipt.confidence()
            );
            let findings = receipt.findings();
            if !findings.is_empty() {
                effects
                    .model_text
                    .push_str(&format!("发现:\n- {}\n", format_findings(&findings)));
            }
        }
    }

    // 通用尾部：缺失证据与异常类别（阶段审查与重派路径已提前返回）
    let missing = receipt.missing_evidence();
    if !missing.is_empty() {
        effects
            .model_text
            .push_str(&format!("缺失证据/未完成项: {}\n", missing.join(", ")));
    }
    if let Some(ref err) = receipt.error_kind {
        effects.model_text.push_str(&format!(
            "异常类别: {}（子任务未正常完成，请谨慎对待输出）\n",
            err
        ));
    }
    effects
}

fn review_scorecard_description(sc: &crate::agent::child_reviewer::ReviewScorecard) -> String {
    let tc = sc.test_coverage.map_or_else(|| "N/A".to_string(), |tc| format!("{}/100", tc));
    format!("\n审查评分（仅供参考，不作为放行条件）: 综合平均 {} 分 (正确性: {}/100, 破坏性风险: {}/100, 代码规范: {}/100, 测试覆盖: {})\n", sc.average_score(), sc.correctness, sc.risk, sc.style, tc)
}

/// 审查者（review）回执处理：故障转移 → infra 记账退款 → 交付聚合。
async fn reduce_reviewer_receipt(
    env: &ReceiptEnv<'_>,
    checkpoint: &mut AgentCheckpoint,
    context: &mut SandwichContext,
    receipt: &DeliveryReceipt,
    effects: &mut ReceiptEffects,
) -> String {
    let findings = receipt.findings();
    let formatted_findings = format_findings(&findings);
    let routing_str = receipt.routing().unwrap_or_else(|| "certain".to_string());
    let scorecard_desc = receipt.scorecard().as_ref().map(review_scorecard_description).unwrap_or_default();
    let mut text = format!(
        "【🔍 子审查代理已完成 (child_run_id: {})】\n结论: {}\n建议路由: [{}]{}成本: 输入 {} / 输出 {} tokens, 耗时 {}ms\n发现:\n- {}\n",
        receipt.child_run_id,
        receipt.confidence(),
        routing_str,
        scorecard_desc,
        receipt.prompt_tokens,
        receipt.completion_tokens,
        receipt.duration_ms,
        formatted_findings
    );

    // 只有明确的 blocking 发现进入交付待办；question/suggestion 保留在回执与审查记录中。
    // （原本只在自动注入路径登记——wait_jobs 收取的 findings 曾不进 todo，
    // 统一 reducer 后两条通道行为一致。）
    {
        let next_id = context
            .todo_list
            .iter()
            .map(|t| t.id)
            .max()
            .map_or(0, |m| m + 1);
        for (offset, f) in findings.iter().enumerate() {
            if !f.severity.eq_ignore_ascii_case("blocking") {
                continue;
            }
            let desc = format!("[审查发现] {}", f.display_line());
            if context.todo_list.iter().any(|t| t.description == desc) {
                continue;
            }
            context.todo_list.push(crate::agent::types::TodoItem {
                id: next_id + offset,
                status: "pending".to_string(),
                description: desc,
                evidence_output_id: None,
            });
        }
    }

    let receipt_kind = receipt.error_kind.as_deref().map(ChildErrorKind::parse);

    // 基础设施失败（超时/取消/中断/非法输出/请求层失败）不是实质性审查：
    // 记录批次失败 + 退还预算（零产出不烧预算）。
    if receipt.is_infra_failure() {
        checkpoint.record_reviewer_failure_for_run(env.db, &receipt.child_run_id);
        checkpoint.refund_infra_failed_review_round(receipt_kind);
        return text;
    }

    // 正常交付：载荷需能解析为审查交付数据；连 JSON 都解析不出的按损坏登记失败
    // （与旧 wait_jobs 路径的 corrupted_json_output 语义一致）。
    let delivery = receipt.as_reviewer_delivery().or_else(|| {
        receipt.payload.as_ref().map(|_| {
            crate::agent::child_reviewer::ReviewerDeliveryData {
                findings: receipt.findings(),
                confidence: receipt.confidence(),
                evidence_refs: Vec::new(),
                missing_evidence: receipt.missing_evidence(),
                routing: receipt.routing(),
                withdrawn_findings: Vec::new(),
                objection_verdicts: Vec::new(),
                scorecard: receipt.scorecard(),
                extra: Default::default(),
            }
        })
    });
    let Some(delivery) = delivery else {
        checkpoint.record_reviewer_failure_for_run(env.db, &receipt.child_run_id);
        return text;
    };

    checkpoint.record_reviewer_delivery_for_run(env.db, Some(&receipt.child_run_id), &delivery);
    if let Some(decision) = checkpoint.plan_review_deliveries.get(&receipt.child_run_id)
        .and_then(|stored| stored.get_extra_str("decision"))
    {
        text.push_str(&format!("\n方案审查生效裁决: {}\n", decision));
    }
    // 审查者撤回的异议：同步关闭对应的 [审查发现] 待办，否则 finish 门禁仍会拿已撤回的意见打回。
    for wid in &delivery.withdrawn_findings {
        let prefix = format!("[审查发现] [{}]", wid);
        for t in context.todo_list.iter_mut() {
            if t.description.starts_with(&prefix)
                && (t.status == "pending" || t.status == "in_progress")
            {
                t.status = "done".to_string();
            }
        }
    }
    // Tech Lead 定稿通知：revised_* 已覆盖 submitted_plan 对应字段，
    // 执行方必须以审查者定稿版本为准。
    if let Some(rev) =
        crate::agent::checkpoint::AgentCheckpoint::plan_revision_from(&delivery)
    {
        let notice = format!(
            "📝 Tech Lead 已在 1v1 中修订并定稿方案（修订字段: {}）",
            rev.describe_fields()
        );
        text.push_str(&format!(
            "\n{}——请按定稿版本执行，勿沿用你此前提交的原文。\n",
            notice
        ));
        effects
            .ui_notices
            .push(format!("{}，submitted_plan 已切换为审查者版本", notice));
    }
    text.push_str(&format!(
        "\n📊 本轮异议动态: 新增 {} / 撤回 {} / 维持 {}（连续多轮撤回≈新增且总数不降 = 审查空转，请评估转人工）\n",
        checkpoint.review_objection_added,
        checkpoint.review_objection_withdrawn,
        checkpoint.review_objection_maintained
    ));
    if !delivery.withdrawn_findings.is_empty() {
        text.push_str(&format!(
            "\n♻️ 审查者已撤回异议: {}（以最新裁决为准，撤回理由见审查结论）\n",
            delivery.withdrawn_findings.join(", ")
        ));
    }
    text
}

fn format_findings(findings: &[crate::agent::child_reviewer::ReviewFinding]) -> String {
    if findings.is_empty() {
        "（无具体发现）".to_string()
    } else {
        findings
            .iter()
            .map(|f| f.display_line())
            .collect::<Vec<_>>()
            .join("\n- ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::child_reviewer::PlanReviewerDispatchSnapshot;
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    #[test]
    fn scorecard_reports_only_observation_without_release_thresholds() {
        let score = crate::agent::child_reviewer::ReviewScorecard::new(85, 75, 85, None);
        let text = review_scorecard_description(&score);
        assert!(text.contains("综合平均 81 分"));
        assert!(text.contains("仅供参考，不作为放行条件"));
        assert!(!text.contains("评分条件"));
        assert!(!text.contains("门禁状态:"));
        assert!(!text.contains("合格放行"));
        let failing = crate::agent::child_reviewer::ReviewScorecard::new(80, 40, 80, None);
        assert!(review_scorecard_description(&failing).contains("仅供参考，不作为放行条件"));
    }

    #[tokio::test]
    async fn blocked_review_receipt_with_high_score_is_not_described_as_gate_release() {
        let (base, db) = test_db("score-label");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let env = test_env(&db, &ws);
        let mut ctx = SandwichContext::new("sys".into(), "goal".into());
        let mut cp = AgentCheckpoint::new("run-1".into(), &ctx);
        let delivery = receipt("review", "score-label-run", None, json!({
            "child_run_id":"score-label-run", "status":"succeeded", "confidence":"supported", "routing":"blocked",
            "findings":[{"id":"F1","severity":"blocking","finality":"regardless","location":"a.rs:1","current":"c","why":"真实阻塞项","suggestion":"s","evidence_strength":"confirmed"}],
            "evidence_refs":[], "missing_evidence":[], "duration_ms":10, "completion_tokens":5,
            "scorecard":{"correctness":85,"risk":75,"style":85,"test_coverage":null}
        }));
        let mut effects = ReceiptEffects::default();
        let text = reduce_reviewer_receipt(&env, &mut cp, &mut ctx, &delivery, &mut effects).await;
        assert!(text.contains("建议路由: [blocked]"), "{}", text);
        assert!(text.contains("仅供参考，不作为放行条件"), "{}", text);
        assert!(text.contains("真实阻塞项"), "{}", text);
        assert!(!text.contains("合格放行"), "{}", text);
        std::fs::remove_dir_all(base).unwrap();
    }

    fn test_db(tag: &str) -> (std::path::PathBuf, DbState) {
        let base = std::env::temp_dir().join(format!(
            "free-api-receipt-{}-{}",
            tag,
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&base).unwrap();
        let state = DbState(Arc::new(Mutex::new(
            crate::db::init_db(&base.join("test.db")).unwrap(),
        )));
        (base, state)
    }

    fn test_env<'a>(db: &'a DbState, ws: &'a Path) -> ReceiptEnv<'a> {
        let (_tx, rx) = watch::channel(0u64);
        ReceiptEnv {
            app: None,
            db,
            parent_run_id: "run-1",
            session_id: "sess-1",
            workspace_dir: ws,
            interrupt_rx: rx,
            reviewer_backup_model: "",
        }
    }

    fn receipt(kind: &str, run_id: &str, error_kind: Option<&str>, payload: serde_json::Value) -> DeliveryReceipt {
        DeliveryReceipt {
            child_run_id: run_id.to_string(),
            task_id: run_id.to_string(),
            attempt_id: 1,
            kind: kind.to_string(),
            status: if error_kind.is_some() { "timed_out".to_string() } else { "succeeded".to_string() },
            duration_ms: 10,
            completion_tokens: 5,
            prompt_tokens: 20,
            error_kind: error_kind.map(|s| s.to_string()),
            payload: Some(payload.clone()),
            raw_text: Some(payload.to_string()),
        }
    }

    // 事故形状：approved、无 blocking、正确性 58 分，question 曾导致反复交付被拒。
    #[tokio::test]
    async fn approved_questions_allow_execution_and_finish_with_low_or_missing_scores() {
        let (base, db) = test_db("approved-questions");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let env = test_env(&db, &ws);
        for (index, score) in [json!({"correctness":58,"risk":92,"style":78}), Value::Null].into_iter().enumerate() {
            let mut context = SandwichContext::new("sys".into(), "比较网络传输与 U 盘速度".into());
            let mut cp = AgentCheckpoint::new("run-1".into(), &context);
            cp.plan_mode = crate::agent::checkpoint::PlanExecutionMode::CrossReview;
            cp.plan_reviewer_retry_count = 1;
            cp.submitted_plan = Some(crate::agent::types::PlanSubmission {
                plan_summary: "测量网络上限并披露实际拷贝速度的不确定性".into(),
                ..Default::default()
            });
            let id = format!("review-{index}");
            let batch = format!("batch-{index}");
            seed_run(&db, &id, "run-1", &batch, "plan_review", "succeeded", true);
            cp.set_plan_review_batch(&batch, 1);
            let mut payload = json!({
                "decision":"approved", "root_cause_verdict":"na", "confidence":"supported",
                "findings":[
                    {"id":"F1","severity":"question","why":"磁盘写速尚未实测，请披露推断边界"},
                    {"id":"F2","severity":"suggestion","why":"可进一步比较不同加密算法"}
                ], "evidence_refs":[], "missing_evidence":["当前没有 U 盘，无法实测"]
            });
            if !score.is_null() { payload["scorecard"] = score; }
            // 经生产解析器与回执 reducer，验证实际状态，不只验证某个过滤函数。
            crate::agent::child_reviewer::parse_controlled_output(&payload.to_string(), &[]).unwrap();
            let effects = reduce_child_receipt(&env, &mut cp, &mut context, &receipt("review", &id, None, payload)).await;
            assert!(effects.model_text.contains("磁盘写速尚未实测"));
            assert!(context.todo_list.is_empty(), "疑问必须保留展示，但不生成硬待办");
            assert_eq!(cp.last_review_blocking_count, 0);
            let saved = serde_json::to_string(&cp).unwrap();
            let mut resumed: AgentCheckpoint = serde_json::from_str(&saved).unwrap();
            assert!(crate::agent::plan_gate::check_pre_write_gatekeeper(
                &mut resumed, &context, &db, "run-1", 5, false
            ).is_none());
            assert!(resumed.plan_phase.is_approved());
            assert!(crate::agent::finish_guard::completion_claim_blockers_with_diff(
                &context.todo_list, None, false, 0, None, None, &[]
            ).is_empty());
            assert_eq!(resumed.plan_review_deliveries[&id].findings.len(), 2);
        }
        std::fs::remove_dir_all(base).unwrap();
    }

    #[tokio::test]
    async fn explicit_global_blocking_still_blocks_execution_and_finish_without_scores() {
        let (base, db) = test_db("global-blocking");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let env = test_env(&db, &ws);
        let mut context = SandwichContext::new("sys".into(), "核验报告".into());
        let mut cp = AgentCheckpoint::new("run-1".into(), &context);
        cp.plan_mode = crate::agent::checkpoint::PlanExecutionMode::CrossReview;
        cp.plan_reviewer_retry_count = 1;
        seed_run(&db, "blocking-1", "run-1", "b1", "plan_review", "succeeded", true);
        cp.set_plan_review_batch("b1", 1);
        let payload = json!({"decision":"approved", "confidence":"confirmed",
            "findings":[{"id":"F1","severity":"blocking","location":"", "why":"报告声称校验通过，命令输出却是失败"}],
            "evidence_refs":[],"missing_evidence":[]});
        let (findings, _, _, _, _, _, _, _) = crate::agent::child_reviewer::parse_controlled_output(&payload.to_string(), &[]).unwrap();
        assert_eq!(findings[0].severity, "blocking");
        reduce_child_receipt(&env, &mut cp, &mut context, &receipt("review", "blocking-1", None, payload)).await;
        let blocked = crate::agent::plan_gate::check_pre_write_gatekeeper(&mut cp, &context, &db, "run-1", 5, false).unwrap();
        assert_eq!(blocked.route, "plan_gatekeeper:blocking_findings_open");
        assert_eq!(context.todo_list.len(), 1);
        assert_eq!(crate::agent::finish_guard::open_todo_descriptions(&context.todo_list).len(), 1);
        std::fs::remove_dir_all(base).unwrap();
    }

    /// 名单退役版夹具：往台账插一条带 batch/purpose 盖章的子任务行。
    /// consumed=true 模拟"回执已由 poll 消费"的生产时序（reducer 跑在消费之后）。
    fn seed_run(
        db: &DbState,
        run_id: &str,
        parent_run_id: &str,
        batch: &str,
        purpose: &str,
        status: &str,
        consumed: bool,
    ) {
        db.0.lock().unwrap().execute(
            "INSERT INTO agent_child_runs (id, parent_run_id, parent_session_id, workspace_root, model_id, status, started_at, kind, task_id, attempt_id, purpose, batch_id, consumed_at)
             VALUES (?1, ?2, 'sess-1', '/tmp', 'm', ?3, 't', 'review', ?1, 1, ?4, ?5, ?6)",
            rusqlite::params![
                run_id, parent_run_id, status, purpose, batch,
                if consumed { Some("2024-01-01T00:00:00Z".to_string()) } else { None }
            ],
        ).unwrap();
    }

    #[tokio::test]
    async fn reviewer_delivery_records_batch_and_registers_todos() {
        let (base, db) = test_db("delivery");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let env = test_env(&db, &ws);
        let mut context = SandwichContext::new("sys".into(), "goal".into());
        let mut cp = AgentCheckpoint::new("run-1".into(), &context);
        // 台账夹具：批次 b1 单成员 rev-1，回执已消费（模拟 poll 先标记的时序）
        seed_run(&db, "rev-1", "run-1", "b1", "plan_review", "succeeded", true);
        cp.set_plan_review_batch("b1", 1);

        let r = receipt(
            "review",
            "rev-1",
            None,
            json!({
                "child_run_id": "rev-1",
                "status": "succeeded",
                "findings": [{"id":"F1","severity":"blocking","finality":"regardless","location":"a.rs:1","current":"c","why":"w","suggestion":"s","evidence_strength":"confirmed"}],
                "confidence": "confirmed"
            }),
        );
        let effects = reduce_child_receipt(&env, &mut cp, &mut context, &r).await;
        assert!(effects.model_text.contains("子审查代理已完成"));
        assert!(cp.plan_reviewer_consumed, "单审交付后批次齐套、应置消费完成");
        assert_eq!(cp.last_review_blocking_count, 1, "blocking 应聚合进批次");
        assert!(
            context.todo_list.iter().any(|t| t.description.contains("F1")),
            "审查发现应闭环登记为待办"
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn suggestion_not_registered_and_withdrawn_finding_closes_todo() {
        let (base, db) = test_db("withdrawn");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let env = test_env(&db, &ws);
        let mut context = SandwichContext::new("sys".into(), "goal".into());
        let mut cp = AgentCheckpoint::new("run-1".into(), &context);
        seed_run(&db, "rev-1", "run-1", "b1", "plan_review", "succeeded", true);
        seed_run(&db, "rev-2", "run-1", "b1", "plan_review", "succeeded", true);
        cp.set_plan_review_batch("b1", 2);

        // 第一份：F1 blocking + F2 suggestion。只有 blocking 登记为必须闭合的待办。
        let r1 = receipt(
            "review",
            "rev-1",
            None,
            json!({
                "child_run_id": "rev-1",
                "status": "succeeded",
                "findings": [
                    {"id":"F1","severity":"blocking","finality":"regardless","location":"a.rs:1","current":"c","why":"w1","suggestion":"s","evidence_strength":"confirmed"},
                    {"id":"F2","severity":"suggestion","finality":"regardless","location":"b.rs:1","current":"c","why":"w2","suggestion":"s","evidence_strength":"confirmed"}
                ],
                "confidence": "confirmed"
            }),
        );
        reduce_child_receipt(&env, &mut cp, &mut context, &r1).await;
        assert!(context.todo_list.iter().any(|t| t.description.contains("[F1]")));
        assert!(
            !context.todo_list.iter().any(|t| t.description.contains("[F2]")),
            "suggestion 级不登记为 finish 必须闭合的待办"
        );

        // 第二份：审查者撤回 F1 → 对应待办自动关闭
        let r2 = receipt(
            "review",
            "rev-2",
            None,
            json!({
                "child_run_id": "rev-2",
                "status": "succeeded",
                "findings": [],
                "withdrawn_findings": ["F1"],
                "confidence": "confirmed"
            }),
        );
        reduce_child_receipt(&env, &mut cp, &mut context, &r2).await;
        let f1 = context.todo_list.iter().find(|t| t.description.contains("[F1]")).unwrap();
        assert_eq!(f1.status, "done", "被撤回的异议不应继续挡 finish");
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn reviewer_infra_failure_records_failure_and_refunds_budget() {
        let (base, db) = test_db("infra");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let env = test_env(&db, &ws);
        let mut context = SandwichContext::new("sys".into(), "goal".into());
        let mut cp = AgentCheckpoint::new("run-1".into(), &context);
        cp.plan_reviewer_retry_count = 1; // 已烧掉一轮预算
        // 台账夹具：批次成员 rev-x 已终态失败且回执已消费（失败记录 = 台账 status）
        seed_run(&db, "rev-x", "run-1", "b-x", "plan_review", "timed_out", true);
        cp.set_plan_review_batch("b-x", 1);

        // 无派发快照 → failover 返回 Ok(None) → 落入 infra 记账+退款
        let r = receipt("review", "rev-x", Some("timeout"), json!({"status":"timed_out"}));
        let effects = reduce_child_receipt(&env, &mut cp, &mut context, &r).await;
        assert!(effects.redispatched_to.is_none());
        assert_eq!(cp.plan_review_failed_count(&db), 1, "失败记录 = 台账 status");
        assert!(cp.plan_reviewer_consumed, "批次唯一成员收口 → 齐套放行");
        assert_eq!(cp.plan_reviewer_retry_count, 0, "infra 失败应退还已烧预算");
        assert_eq!(cp.reviewer_budget_refunds, 1);
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn failover_replaces_expected_run_id_in_batch() {
        // 回归：自动注入路径此前 failover 成功但漏换 expected id，
        // 旧 id 永远无法交付 → 齐套屏障死锁。收编进 failover 内部后结构性杜绝。
        let (base, db) = test_db("failover");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        {
            let conn = db.0.lock().unwrap();
            conn.execute(
                "INSERT INTO platforms (id, name, base_url, api_key, created_at) VALUES ('p1','test','http://127.0.0.1:9','','2024-01-01')",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO models (id, platform_id, name, display_name, created_at) VALUES ('m1','p1','test-model','Test','2024-01-01')",
                [],
            ).unwrap();
        }
        // 真实落一行旧 run（带逻辑身份）：备胎应继承其 task_id 且 attempt+1
        let old_identity = crate::agent::child_reviewer::ChildRunIdentity {
            task_id: "task-planreview-x".to_string(),
            attempt_id: 1,
            purpose: Some("plan_review".to_string()),
            batch_id: Some("batch-x".to_string()),
            contract_id: Some("reviewer.v1".to_string()),
            contract_schema: None,
        };
        let old_id = crate::agent::child_reviewer::begin_child_run_extended(
            &db, "run-1", "sess-1", &ws, "m1", 0, "review", None, None, None, &old_identity,
        )
        .unwrap();

        let mut env = test_env(&db, &ws);
        env.reviewer_backup_model = "m2";
        db.0.lock().unwrap().execute("INSERT INTO models (id,platform_id,name,display_name,created_at) VALUES ('m2','p1','test-backup','Backup','2024-01-01')", []).unwrap();
        let mut context = SandwichContext::new("sys".into(), "goal".into());
        let mut cp = AgentCheckpoint::new("run-1".into(), &context);
        cp.set_plan_review_batch("batch-x", 1);
        // 模拟生产时序：poll 发出回执前已把老 run 标记为终态+已消费
        db.0.lock().unwrap().execute(
            "UPDATE agent_child_runs SET status='timed_out', consumed_at='2024-01-01' WHERE id=?1",
            rusqlite::params![old_id],
        ).unwrap();
        cp.set_plan_reviewer_dispatch_snapshot(
            &PlanReviewerDispatchSnapshot::new(
                "m1",
                "flash",
                "high",
                "",
                json!({"question": "审查本方案", "evidence_excerpt": "代码摘录"}),
            )
            .with_purpose("plan_review")
            .with_batch_id("batch-x"),
        );

        cp.reviewer_dispatches.insert(old_id.clone(), cp.plan_reviewer_dispatch_snapshot().unwrap());
        // 最近一次别人的派发不应覆盖此物理 child 的原题。
        cp.set_plan_reviewer_dispatch_snapshot(&PlanReviewerDispatchSnapshot::new(
            "unrelated-model", "pro", "low", "", json!({"question":"别人的审查题目"})));

        let r = receipt("review", &old_id, Some("timed_out"), json!({"status":"timed_out"}));
        let effects = reduce_child_receipt(&env, &mut cp, &mut context, &r).await;

        let new_id = effects
            .redispatched_to
            .expect("failover-eligible 失败应触发重派");
        // 名单退役版断言：备胎靠 batch_id 盖章自动入批——旧成员已消费出列、
        // 新成员自动成为唯一待决，齐套屏障正确续口而不需任何"换名"动作
        assert_eq!(cp.plan_review_member_count(&db), 2, "旧成员+备胎同属一批");
        assert_eq!(cp.plan_review_pending_count(&db), 1, "唯一待决 = 备胎");
        assert!(!cp.plan_reviewer_consumed, "备胎在途 → 屏障继续拦截");
        assert_eq!(cp.plan_reviewer_failover_count, 1);
        assert!(cp.pending_task_failure.is_none());
        assert_eq!(cp.reviewer_dispatches[&new_id].params["question"], "审查本方案");
        assert_eq!(cp.reviewer_dispatches[&new_id].model, "m2");

        // 逻辑身份继承：同一道题两次作答，task_id 不变、attempt 递增、purpose/batch 沿用
        {
            let conn = db.0.lock().unwrap();
            let (new_task, new_attempt, new_purpose, new_batch): (String, i64, Option<String>, Option<String>) =
                conn.query_row(
                    "SELECT task_id, attempt_id, purpose, batch_id FROM agent_child_runs WHERE id = ?1",
                    rusqlite::params![new_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
            assert_eq!(new_task, "task-planreview-x", "备胎必须继承同一逻辑任务身份");
            assert_eq!(new_attempt, 2, "attempt 由落库侧自动递增");
            assert_eq!(new_purpose.as_deref(), Some("plan_review"));
            assert_eq!(new_batch.as_deref(), Some("batch-x"));
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn attempt_id_auto_derives_within_same_task() {
        // 同 (parent_run, task_id) 的多次落库 attempt 自动递增——
        // 调用方无需记忆/传递尝试序号，语义由台账保证。
        let (base, db) = test_db("attempt-derive");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let id1 = crate::agent::child_reviewer::ChildRunIdentity {
            task_id: "task-shared".to_string(),
            attempt_id: 1,
            purpose: Some("worker".to_string()),
            batch_id: None,
            contract_id: Some("worker.v1".to_string()),
            contract_schema: None,
        };
        let r1 = crate::agent::child_reviewer::begin_child_run_extended(
            &db, "run-1", "sess-1", &ws, "m1", 0, "worker", None, None, None, &id1,
        )
        .unwrap();
        // 第二次：同 task_id、调用方仍传 attempt=1 → 落库侧推导为 2
        let r2 = crate::agent::child_reviewer::begin_child_run_extended(
            &db, "run-1", "sess-1", &ws, "m1", 0, "worker", None, None, None, &id1,
        )
        .unwrap();
        let conn = db.0.lock().unwrap();
        let a1: i64 = conn
            .query_row("SELECT attempt_id FROM agent_child_runs WHERE id=?1", rusqlite::params![r1], |r| r.get(0))
            .unwrap();
        let a2: i64 = conn
            .query_row("SELECT attempt_id FROM agent_child_runs WHERE id=?1", rusqlite::params![r2], |r| r.get(0))
            .unwrap();
        assert_eq!(a1, 1);
        assert_eq!(a2, 2);
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn stage_review_infra_failure_defers_refund_to_batch_settle() {
        // 阶段审查退款延迟结算契约：回执只标记 refund_pending，
        // 是否真退由批后 settle 依据「在途清空且无 blocker」终态判定。
        let (base, db) = test_db("stage-refund");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let env = test_env(&db, &ws);
        let mut context = SandwichContext::new("sys".into(), "goal".into());
        let mut cp = AgentCheckpoint::new("run-1".into(), &context);
        // 台账夹具：阶段批次 sb 单成员 stage-1，回执已消费（终态 timed_out）
        seed_run(&db, "stage-1", "run-1", "sb", "stage_review", "timed_out", true);
        cp.stage_review_batch_id = Some("sb".to_string());
        cp.stage_review_budget_used = 1;

        let r = receipt("review", "stage-1", Some("timeout"), json!({"status":"timed_out"}));
        let effects = reduce_child_receipt(&env, &mut cp, &mut context, &r).await;
        assert!(effects.stage_refund_pending, "infra 失败应标记待结算");
        assert!(effects.model_text.contains("阶段审查"));

        assert!(cp.settle_stage_review_refund(&db), "在途清空且无 blocker → 应退款");
        assert_eq!(cp.stage_review_budget_used, 0);
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn stage_review_refund_withheld_when_blockers_remain() {
        let (base, db) = test_db("stage-norefund");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let env = test_env(&db, &ws);
        let mut context = SandwichContext::new("sys".into(), "goal".into());
        let mut cp = AgentCheckpoint::new("run-1".into(), &context);
        // 台账夹具：阶段批次 sb2 两成员，回执均已消费（终态已落库）
        seed_run(&db, "stage-ok", "run-1", "sb2", "stage_review", "succeeded", true);
        seed_run(&db, "stage-bad", "run-1", "sb2", "stage_review", "timed_out", true);
        cp.stage_review_batch_id = Some("sb2".to_string());
        cp.stage_review_budget_used = 1;

        // 先交付一个带 blocker 的成功专家，再收一个 infra 失败专家：
        // 有实质产出 → settle 不退款。
        let ok = receipt(
            "review",
            "stage-ok",
            None,
            json!({
                "status": "succeeded",
                "findings": [{"id":"B1","severity":"blocking","finality":"regardless","location":"a.rs:1","current":"c","why":"w","suggestion":"s","evidence_strength":"confirmed"}],
                "confidence": "confirmed"
            }),
        );
        let eff1 = reduce_child_receipt(&env, &mut cp, &mut context, &ok).await;
        assert!(!eff1.stage_refund_pending);
        assert_eq!(cp.stage_review_blockers_open, 1);

        let bad = receipt("review", "stage-bad", Some("timeout"), json!({"status":"timed_out"}));
        let eff2 = reduce_child_receipt(&env, &mut cp, &mut context, &bad).await;
        assert!(eff2.stage_refund_pending);
        assert!(
            !cp.settle_stage_review_refund(&db),
            "已有专家交付真实 blocking → 本轮有实质产出，不得退款"
        );
        assert_eq!(cp.stage_review_budget_used, 1);
        let _ = std::fs::remove_dir_all(base);
    }


    #[tokio::test]
    async fn unconfigured_or_exhausted_backup_stops_without_third_model_or_approval() {
        for (configured, current) in [("", "primary"), ("backup", "backup")] {
            let (base, db) = test_db("no-auto-backup");
            let mut env = test_env(&db, &base);
            env.reviewer_backup_model = configured;
            let mut context = SandwichContext::new("sys".into(), "goal".into());
            let mut cp = AgentCheckpoint::new("sess-1".into(), &context);
            let phase = cp.plan_phase;
            let identity = crate::agent::child_reviewer::ChildRunIdentity {
                task_id:"review-task".into(), attempt_id:1, purpose:Some("plan_review".into()),
                batch_id:Some("batch-stop".into()), contract_id:None, contract_schema:None,
            };
            let id = crate::agent::child_reviewer::begin_child_run_extended(&db,"run-1","sess-1",&base,current,0,"review",None,None,None,&identity).unwrap();
            cp.reviewer_dispatches.insert(id.clone(), PlanReviewerDispatchSnapshot::new(current,"flash","high","",json!({"question":"审查原方案"})).with_purpose("plan_review").with_batch_id("batch-stop"));
            db.0.lock().unwrap().execute("UPDATE agent_child_runs SET status='failed',consumed_at='2026-01-01' WHERE id=?1",[&id]).unwrap();
            cp.set_plan_review_batch("batch-stop",1);
            let r = receipt("review",&id,Some("request_or_stream"),json!({"error":"402 insufficient balance"}));
            let effects=reduce_child_receipt(&env,&mut cp,&mut context,&r).await;
            assert!(effects.redispatched_to.is_none());
            assert!(cp.pending_task_failure.is_some());
            assert_eq!(cp.plan_phase,phase,"失败不是批准");
            let n:i64=db.0.lock().unwrap().query_row("SELECT COUNT(*) FROM agent_child_runs",[],|r|r.get(0)).unwrap();
            assert_eq!(n,1,"不能自行找第三个模型重派");
            let _=std::fs::remove_dir_all(base);
        }
    }

    #[tokio::test]
    async fn worker_failure_and_review_parameter_rejection_do_not_fail_parent_task() {
        let (base,db)=test_db("repairable");
        let env=test_env(&db,&base);
        let mut context=SandwichContext::new("sys".into(),"goal".into());
        let mut cp=AgentCheckpoint::new("sess-1".into(),&context);
        for (kind,error) in [("worker","request_or_stream"),("review","input_validation:missing_question")] {
            let r=receipt(kind,"no-row",Some(error),json!({"error":error}));
            reduce_child_receipt(&env,&mut cp,&mut context,&r).await;
            assert!(cp.pending_task_failure.is_none(),"{kind}/{error} should remain repairable");
        }
        let _=std::fs::remove_dir_all(base);
    }

    #[test]
    fn job_result_normalizes_into_delivery_receipt() {
        let job = crate::agent::job_engine::BackgroundJobResult {
            job_id: "j-1".to_string(),
            job_type: "child_reviewer".to_string(),
            title: "审查".to_string(),
            status: "succeeded".to_string(),
            duration_ms: 42,
            result_json: json!({"prompt_tokens": 11, "completion_tokens": 7, "findings": []}).to_string(),
            error_kind: None,
            task_id: None,
            attempt_id: 1,
        };
        let r = job.to_delivery_receipt();
        assert_eq!(r.child_run_id, "j-1");
        assert_eq!(r.kind, "review");
        assert_eq!(r.prompt_tokens, 11);
        assert_eq!(r.completion_tokens, 7);
        assert!(r.payload.is_some());
    }

    #[tokio::test]
    async fn contract_schema_violation_downgrades_success_to_invalid_output() {
        // 运行时校验：run 台账带 contract_schema（必填 cve_refs），
        // 交付缺失该字段 → 就地转 invalid_output + contract_violation 标记。
        // 缺必填契约字段不是有效裁决：保留违规台账，未配置备用则失败停止，不能算审查通过。
        let (base, db) = test_db("contract");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let schema = crate::agent::protocol::ContractSchema::new(vec![
            crate::agent::protocol::ContractFieldDef::must_new(
                "cve_refs",
                crate::agent::protocol::ContractFieldType::StringArray,
                "命中的 CVE 编号",
                true,
            ),
        ])
        .with_strict_closed_set(false);
        let identity = crate::agent::child_reviewer::ChildRunIdentity {
            task_id: "task-cve".to_string(),
            attempt_id: 1,
            purpose: Some("plan_review".to_string()),
            batch_id: Some("batch-cve".to_string()),
            contract_id: Some("reviewer.v1".to_string()),
            contract_schema: Some(serde_json::to_string(&schema).unwrap()),
        };
        let run_id = crate::agent::child_reviewer::begin_child_run_extended(
            &db, "run-1", "sess-1", &ws, "m1", 0, "review", None, None, None, &identity,
        )
        .unwrap();

        let env = test_env(&db, &ws);
        let mut context = SandwichContext::new("sys".into(), "goal".into());
        let mut cp = AgentCheckpoint::new("run-1".into(), &context);
        // 批次锚点（行已盖 batch-cve 章）+ 模拟回执已消费的生产时序
        cp.set_plan_review_batch("batch-cve", 1);
        db.0.lock().unwrap().execute(
            "UPDATE agent_child_runs SET status='succeeded', consumed_at='2024-01-01' WHERE id=?1",
            rusqlite::params![run_id],
        ).unwrap();

        // 成功交付但缺必填扩展字段 cve_refs → 契约违规标记透出到模型文本
        let r = receipt("review", &run_id, None, json!({"findings": [], "confidence": "high"}));
        let effects = reduce_child_receipt(&env, &mut cp, &mut context, &r).await;
        assert!(
            effects.model_text.contains("contract_violation"),
            "契约违规必须在模型可见文本中标记 contract_violation，实际: {}",
            effects.model_text
        );

        // 对照组：合规交付（带 cve_refs）不应出现违规标记
        let r2 = receipt(
            "review",
            &run_id,
            None,
            json!({"findings": [], "confidence": "high", "cve_refs": ["CVE-2024-1"]}),
        );
        // 注意：同一 run 已消费过的回执由幂等保护跳过副作用，这里只验证校验不触发标记
        let mut receipt2 = r2.clone();
        enforce_delivery_contract(&db, &mut receipt2);
        assert_eq!(receipt2.status, "succeeded", "合规交付不得降级");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn legacy_consumer_schema_is_ignored_at_receipt_but_other_requirements_remain() {
        use crate::agent::protocol::{ContractFieldDef, ContractFieldType, ContractSchema};
        let (base, db) = test_db("retired-consumer-schema");
        let schema = ContractSchema::new([
            ("consumer_verdicts", ContractFieldType::StringArray),
            ("evidence_scope_verdicts", ContractFieldType::StringArray),
            ("root_cause_verdict", ContractFieldType::String),
        ].into_iter().map(|(name, field_type)| ContractFieldDef {
            name: name.into(), field_type, description: name.into(), required: true, allowed_values: None,
        }).collect());
        let original = serde_json::to_string(&schema).unwrap();
        let identity = crate::agent::child_reviewer::ChildRunIdentity {
            contract_schema: Some(original.clone()),
            ..crate::agent::child_reviewer::ChildRunIdentity::fresh("review")
        };
        let id = crate::agent::child_reviewer::begin_child_run_extended(
            &db, "parent", "session", &base, "mock", 0, "review", None, None, None, &identity,
        ).unwrap();
        let payload = json!({"findings":[{"id":"F1","severity":"blocking","location":"src/a.rs:1",
            "current":"旧调用","why":"参数不匹配","suggestion":"修复调用方","evidence_strength":"confirmed"}],
            "confidence":"confirmed", "evidence_scope_verdicts":[], "root_cause_verdict":"explains"});
        let mut accepted = receipt("review", &id, None, payload.clone());
        enforce_delivery_contract(&db, &mut accepted);
        assert_eq!(accepted.status, "succeeded", "缺少旧消费点字段不应拒收");
        assert_eq!(accepted.findings().len(), 1, "真实 finding 保留");
        for field in ["evidence_scope_verdicts", "root_cause_verdict"] {
            let mut incomplete = payload.clone();
            incomplete.as_object_mut().unwrap().remove(field);
            let mut rejected = receipt("review", &id, None, incomplete);
            enforce_delivery_contract(&db, &mut rejected);
            assert_eq!(rejected.status, "invalid_output");
            assert!(rejected.error_kind.unwrap().contains(field));
        }
        let stored: String = db.0.lock().unwrap().query_row(
            "SELECT contract_schema FROM agent_child_runs WHERE id=?1", rusqlite::params![id], |r| r.get(0),
        ).unwrap();
        assert_eq!(stored, original, "不迁移或清理旧台账");
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn contract_violation_verdict_persists_to_ledger_on_commit() {
        // 消费端裁决一致性：reducer 判 invalid_output 后，提交点把裁决
        // 随 applied_at 同事务回写台账——模型所见与库中所记不得分叉。
        let (base, db) = test_db("contract-wb");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let schema = crate::agent::protocol::ContractSchema::new(vec![
            crate::agent::protocol::ContractFieldDef::must_new(
                "cve_refs",
                crate::agent::protocol::ContractFieldType::StringArray,
                "命中的 CVE 编号",
                true,
            ),
        ]);
        let identity = crate::agent::child_reviewer::ChildRunIdentity {
            task_id: "task-wb".to_string(),
            attempt_id: 1,
            purpose: Some("worker".to_string()),
            batch_id: None,
            contract_id: None,
            contract_schema: Some(serde_json::to_string(&schema).unwrap()),
        };
        let run_id = crate::agent::child_reviewer::begin_child_run_extended(
            &db, "run-1", "sess-1", &ws, "m1", 0, "worker", None, None, None, &identity,
        )
        .unwrap();
        db.0.lock().unwrap().execute(
            "UPDATE agent_child_runs SET status='succeeded', consumed_at='2024-01-01' WHERE id=?1",
            rusqlite::params![run_id],
        ).unwrap();

        let env = test_env(&db, &ws);
        let mut context = SandwichContext::new("sys".into(), "goal".into());
        let mut cp = AgentCheckpoint::new("run-1".into(), &context);
        // worker 缺必填 cve_refs → 违规裁决透出到 effects
        let r = receipt("worker", &run_id, None, json!({"test_status": "passed", "findings": []}));
        let effects = reduce_child_receipt(&env, &mut cp, &mut context, &r).await;
        assert!(
            effects.contract_violation.is_some(),
            "契约违规必须透出裁决供提交点回写台账"
        );

        crate::agent::checkpoint::save_checkpoint_and_consume(
            &db,
            &mut cp,
            &context,
            &[crate::agent::checkpoint::AppliedReceipt {
                run_id: run_id.clone(),
                verdict: effects
                    .contract_violation
                    .clone()
                    .map(|e| ("invalid_output".to_string(), e)),
            }],
        )
        .unwrap();

        let (status, error_kind, applied): (String, Option<String>, Option<String>) = db
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT status, error_kind, applied_at FROM agent_child_runs WHERE id=?1",
                rusqlite::params![run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "invalid_output", "台账 status 须与消费端裁决一致");
        assert!(
            error_kind.as_deref().unwrap_or("").starts_with("contract_violation"),
            "error_kind 须携带 contract_violation 标记"
        );
        assert!(applied.is_some());
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn crash_between_delivery_and_commit_replays_receipt_once() {
        // P0 全链路崩溃恢复：真实台账行 → poll 投递 → reducer 入账 →
        // （模拟崩溃：commit 未发生，checkpoint/context 丢弃）→ 重建后
        // 回执重投 → 再次入账 → 提交 → 退出信箱。效果恰好一次。
        let (base, db) = test_db("crash-replay");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let env = test_env(&db, &ws);

        let run_id = crate::agent::child_reviewer::begin_child_run(
            &db, "run-1", "sess-1", &ws, "m1", 0, "review", None,
        )
        .unwrap();
        let result_json = json!({
            "child_run_id": run_id,
            "status": "succeeded",
            "findings": [{"id":"F1","severity":"blocking","finality":"regardless","location":"a.rs:1","current":"c","why":"w","suggestion":"s","evidence_strength":"confirmed"}],
            "confidence": "confirmed",
            "evidence_refs": [],
            "missing_evidence": [],
            "duration_ms": 10,
            "completion_tokens": 5
        })
        .to_string();
        crate::agent::child_reviewer::finish_child_run(
            &db, &run_id, "succeeded", 10, 5, 0, None, Some(&result_json),
        );

        // ── 第一遍：投递 + 入账，但"进程"在提交点前崩溃（cp/context 丢弃）──
        {
            let mut context = SandwichContext::new("sys".into(), "goal".into());
            let mut cp = AgentCheckpoint::new("run-1".into(), &context);
            let first = crate::agent::child_reviewer::poll_completed_delivery_receipts(
                &db, "run-1", None,
            )
            .unwrap();
            assert_eq!(first.len(), 1);
            let _effects =
                reduce_child_receipt(&env, &mut cp, &mut context, &first[0]).await;
            // 崩溃：save_checkpoint_and_consume 从未调用，cp/context 随作用域丢弃
        }

        // ── 重启：回执必须重投（applied_at 未落账 → 仍在信箱）──
        let mut context = SandwichContext::new("sys".into(), "goal".into());
        let mut cp = AgentCheckpoint::new("run-1".into(), &context);
        let replayed = crate::agent::child_reviewer::poll_completed_delivery_receipts(
            &db, "run-1", None,
        )
        .unwrap();
        assert_eq!(
            replayed.len(),
            1,
            "已投递未落账的回执崩溃后必须重投，否则永久丢件"
        );
        reduce_child_receipt(&env, &mut cp, &mut context, &replayed[0]).await;
        assert_eq!(
            cp.plan_review_deliveries.len(),
            1,
            "重投后的入账应聚合一份交付"
        );

        crate::agent::checkpoint::save_checkpoint_and_consume(
            &db,
            &mut cp,
            &context,
            &[crate::agent::checkpoint::AppliedReceipt {
                run_id: run_id.clone(),
                verdict: None,
            }],
        )
        .unwrap();

        // ── 提交后：退出信箱，永不再投 ──
        let drained = crate::agent::child_reviewer::poll_completed_delivery_receipts(
            &db, "run-1", None,
        )
        .unwrap();
        assert!(drained.is_empty(), "落账后不得重复投递");

        // 磁盘上的 checkpoint 携带入账效果（交付聚合持久化）
        let persisted = crate::agent::checkpoint::load_checkpoint(&db, "run-1")
            .unwrap()
            .expect("checkpoint 已提交");
        assert_eq!(persisted.plan_review_deliveries.len(), 1);
        let _ = std::fs::remove_dir_all(base);
    }

}
