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
    // ── 阶段审查/对质回执：独立语义（不占方案审查预算，infra 失败视为未审）──
    if checkpoint.matches_stage_review_run(&receipt.child_run_id)
        || checkpoint.stage_review_rebuttal_run_id.as_deref()
            == Some(receipt.child_run_id.as_str())
    {
        let oracle_index = crate::agent::eval_oracle::query_active_progress()
            .await
            .map(|(idx, _, _)| idx as u32);
        let outcome =
            crate::agent::stage_review::apply_stage_receipt(checkpoint, receipt, oracle_index);
        return ReceiptEffects {
            model_text: format!(
                "【📋 阶段审查 (child_run_id: {})】\n{}",
                receipt.child_run_id, outcome.note
            ),
            stage_refund_pending: outcome.refund_pending,
            ..Default::default()
        };
    }

    let mut effects = ReceiptEffects::default();
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
    let scorecard_desc = if let Some(ref sc) = receipt.scorecard() {
        let tc_str = sc
            .test_coverage
            .map_or_else(|| "N/A".to_string(), |tc| format!("{}/100", tc));
        format!(
            "\n审查评分卡: 综合平均 {} 分 (正确性: {}/100, 破坏性风险: {}/100, 代码规范: {}/100, 测试覆盖: {}) -> 门禁状态: [{}]\n",
            sc.average_score(), sc.correctness, sc.risk, sc.style, tc_str,
            if sc.passes_gate() { "✅合格放行" } else { "🚨未达标拦截(均分需>=75且risk需>=60)" }
        )
    } else {
        String::new()
    };
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

    // 审查发现自动闭环：登记为待办条目，finish 门禁要求逐条闭合。
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

    // 审查者故障转移链：invalid_output/超时/请求层失败 → 备用审查员重派。
    // 封顶 2 次；齐套屏障 expected-id 替换由 failover 内部完成（结构性防遗忘）。
    let receipt_kind = receipt
        .error_kind
        .as_deref()
        .map(ChildErrorKind::parse);
    if receipt_kind
        .as_ref()
        .map(|k| k.is_failover_eligible())
        .unwrap_or(false)
    {
        match crate::agent::main_loop::try_plan_reviewer_failover(
            env.app,
            env.db,
            env.parent_run_id,
            env.session_id,
            env.workspace_dir,
            checkpoint,
            env.interrupt_rx.clone(),
            receipt_kind.as_ref().unwrap(),
            Some(&receipt.child_run_id),
        )
        .await
        {
            Ok(Some(new_run_id)) => {
                text.push_str(&format!(
                    "\n🔁 审查者故障转移：上次结论异常（{}），已自动改用备用审查员重派 (child_run_id: {})\n",
                    receipt.error_kind.as_deref().unwrap_or("?"),
                    new_run_id
                ));
                effects.redispatched_to = Some(new_run_id);
                return text;
            }
            Ok(None) => {}
            Err(spawn_err) => {
                text.push_str(&format!(
                    "\n⚠️ 审查者故障转移重派失败: {}（转人工裁决路径不受影响）\n",
                    spawn_err
                ));
            }
        }
    }

    // 基础设施失败（超时/取消/中断/非法输出/请求层失败）不是实质性审查：
    // 记录批次失败 + 退还预算（零产出不烧预算）。
    if receipt.is_infra_failure() {
        checkpoint.record_reviewer_failure_for_run(
            &receipt.child_run_id,
            receipt.error_kind.as_deref().unwrap_or("infra_failure"),
        );
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
        checkpoint.record_reviewer_failure_for_run(&receipt.child_run_id, "corrupted_json_output");
        return text;
    };

    checkpoint.record_reviewer_delivery_for_run(Some(&receipt.child_run_id), &delivery);
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
    use serde_json::json;
    use std::sync::{Arc, Mutex};

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

    #[tokio::test]
    async fn reviewer_delivery_records_batch_and_registers_todos() {
        let (base, db) = test_db("delivery");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let env = test_env(&db, &ws);
        let mut context = SandwichContext::new("sys".into(), "goal".into());
        let mut cp = AgentCheckpoint::new("run-1".into(), &context);

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
    async fn reviewer_infra_failure_records_failure_and_refunds_budget() {
        let (base, db) = test_db("infra");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let env = test_env(&db, &ws);
        let mut context = SandwichContext::new("sys".into(), "goal".into());
        let mut cp = AgentCheckpoint::new("run-1".into(), &context);
        cp.plan_reviewer_retry_count = 1; // 已烧掉一轮预算

        // 无派发快照 → failover 返回 Ok(None) → 落入 infra 记账+退款
        let r = receipt("review", "rev-x", Some("timeout"), json!({"status":"timed_out"}));
        let effects = reduce_child_receipt(&env, &mut cp, &mut context, &r).await;
        assert!(effects.redispatched_to.is_none());
        assert!(cp.plan_review_batch.failed_run_ids.contains_key("rev-x"));
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
        };
        let old_id = crate::agent::child_reviewer::begin_child_run_extended(
            &db, "run-1", "sess-1", &ws, "m1", 0, "review", None, None, None, &old_identity,
        )
        .unwrap();

        let env = test_env(&db, &ws);
        let mut context = SandwichContext::new("sys".into(), "goal".into());
        let mut cp = AgentCheckpoint::new("run-1".into(), &context);
        cp.register_plan_reviewer_batch(&[old_id.clone()]);
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

        let r = receipt("review", &old_id, Some("timed_out"), json!({"status":"timed_out"}));
        let effects = reduce_child_receipt(&env, &mut cp, &mut context, &r).await;

        let new_id = effects
            .redispatched_to
            .expect("failover-eligible 失败应触发重派");
        assert!(
            !cp.plan_review_batch.expected_run_ids.contains(&old_id),
            "旧失败 id 必须从预期集合移除"
        );
        assert!(
            cp.plan_review_batch.expected_run_ids.contains(&new_id),
            "新重派 id 必须接管预期集合，否则齐套屏障永不收口"
        );
        assert_eq!(cp.plan_reviewer_failover_count, 1);

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
        cp.stage_review_run_ids = vec!["stage-1".to_string()];
        cp.stage_review_budget_used = 1;

        let r = receipt("review", "stage-1", Some("timeout"), json!({"status":"timed_out"}));
        let effects = reduce_child_receipt(&env, &mut cp, &mut context, &r).await;
        assert!(effects.stage_refund_pending, "infra 失败应标记待结算");
        assert!(effects.model_text.contains("阶段审查"));

        assert!(cp.settle_stage_review_refund(), "在途清空且无 blocker → 应退款");
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
        cp.stage_review_run_ids = vec!["stage-ok".to_string(), "stage-bad".to_string()];
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
            !cp.settle_stage_review_refund(),
            "已有专家交付真实 blocking → 本轮有实质产出，不得退款"
        );
        assert_eq!(cp.stage_review_budget_used, 1);
        let _ = std::fs::remove_dir_all(base);
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
}
