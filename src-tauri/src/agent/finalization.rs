//! 终态收口（Finalization）收据与短事务提交。
//!
//! 架构原则：
//! - 单独能力（verify / review / artifact 登记）保持可独立调用，结果沉淀为可复用收据；
//! - `prepare_finish` 运行与 finish 相同的门禁评估，只沉淀收据、不提交终态；
//! - `finish` 的权威状态提交走短事务：checkpoint Done + run trace succeeded +
//!   会话 assistant 消息三者同生同灭，杜绝「checkpoint 已完成但 trace 仍 running」的半截态；
//! - 事件通知与产物登记是提交后的可重放派生动作，不参与事务。
//!
//! 收据保鲜键：diff_hash + resource_scope.revision + 契约内容 + 验收状态指纹。
//! 任一变化都会重新评估交付；审查材料变化不必抹除同运行、同契约、同产物
//! 的宿主冻结验证事实，但仍需 Reviewer 独立裁量其适用性，不能按旧审查结论放行。

use crate::agent::checkpoint::AgentCheckpoint;
use crate::agent::context::SandwichContext;
use crate::db::DbState;
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;

mod evaluate;
pub(crate) use evaluate::*;

/// 交付执行、评估和收据共用的有效参数；与步骤记录采用相同的 command/params 口径。
pub(crate) fn delivery_payload(instruction: &crate::agent::types::AgentInstruction) -> Value {
    let mut value = if let Some(request) = instruction.prepare_finish.as_ref() {
        let mut map = serde_json::Map::new();
        if let Some(s) = &request.summary {
            map.insert("summary".into(), json!(s));
        }
        if let Some(d) = &request.details {
            map.insert("details".into(), json!(d));
        }
        Value::Object(map)
    } else {
        instruction.get_params()
    };
    // 只恢复可严格解析的已知外壳，不按正文中的字段名或 Markdown 猜测边界。
    if let Some(object) = value.as_object_mut() {
        for key in ["summary", "details"] {
            if let Some(Value::String(text)) = object.get_mut(key) {
                *text = finish_summary_text(text);
            }
        }
    }
    value
}

pub(crate) fn finish_output(instruction: &crate::agent::types::AgentInstruction) -> String {
    delivery_body(&delivery_payload(instruction)).unwrap_or_else(|| "任务已完成".into())
}

/// 恢复已知的残缺 finish 外壳：仅当补回固定外层后可严格解析，才读取正文。
/// 不执行其中指令、不提升其他字段，也不截取普通文本/代码示例。
pub(crate) fn finish_summary_text(raw: &str) -> String {
    let text = raw.trim();
    let recover = || -> Option<String> {
        if !text.starts_with("plan-") {
            return None;
        }
        let value: Value = serde_json::from_str(&format!("{{\"plan_id\":\"{text}}}")).ok()?;
        let plan = value.get("plan_id")?.as_str()?.strip_prefix("plan-")?;
        if plan.is_empty() || !plan.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if value.get("tool")?.as_str()? != "finish" || !value.get("plan_title")?.is_string() {
            return None;
        }
        value
            .get("command")?
            .get("summary")?
            .as_str()
            .map(str::to_owned)
    };
    recover().unwrap_or_else(|| raw.to_owned())
}

/// 与实时/历史界面一致的正文投影；评审与落库共用，保留 Markdown 原文。
pub(crate) fn delivery_body(payload: &Value) -> Option<String> {
    let summary = payload.get("summary").and_then(Value::as_str).unwrap_or("");
    let details = payload.get("details").and_then(Value::as_str).unwrap_or("");
    let body = match (summary.is_empty(), details.is_empty()) {
        (false, false) if summary != details => format!("{summary}\n\n{details}"),
        (false, _) => summary.to_string(),
        (_, false) => details.to_string(),
        _ => return None,
    };
    Some(body)
}

/// 收据中的验证结论。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReceiptVerification {
    /// 尚未执行或尚无可接受的验证回执。
    Pending,
    /// 验证确实执行失败，不能被重试计数消除。
    Failed { details: String },
    /// 本轮没有需要宿主验证的新写入。
    NotRequired,
    /// 宿主已执行契约义务命令与自适应验证，全部通过。
    Verified { evidence: String },
    /// 交付被系统豁免（纯静态资源 / 无可推断验证命令）。
    Waived { reason: String },
}

/// 收据中的终审结论。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReceiptReview {
    /// 尚未进入终审，不能当作无需终审。
    Pending,
    /// 无审查材料（无 diff），终审跳过。
    NotRequired,
    /// 终审通过。
    Passed {
        score: u32,
        model_id: String,
        summary: String,
    },
    /// 终审发现阻塞缺陷（未通过）。
    Failed {
        score: u32,
        model_id: String,
        blocking_count: usize,
    },
    /// 修复多次仍未收敛，经人工明确仲裁后带警告交付。
    DeliveredWithWarnings {
        score: u32,
        model_id: String,
        blocking_count: usize,
    },
}

impl ReceiptReview {
    /// 该审查结论是否允许进入终态提交。
    pub fn is_accepted(&self) -> bool {
        matches!(
            self,
            ReceiptReview::Passed { .. }
                | ReceiptReview::DeliveredWithWarnings { .. }
                | ReceiptReview::NotRequired
        )
    }
}

/// 宿主真实执行的冻结义务结果。与 Reviewer 结论分别保存：
/// 上一轮即使审查失败，验证事实仍可供下一轮审查参考。
/// 复用必须核对运行、验收契约、产物材料和执行副作用；旧回执本身不是新 PASS。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrozenVerificationRun {
    pub run_id: String,
    pub workspace_root: String,
    pub material_fingerprint: String,
    pub contract_fingerprint: String,
    pub scope_revision: u64,
    pub effects_fingerprint: String,
    pub executed_at: String,
    pub executed_step_id: usize,
    pub results: Vec<crate::agent::host_verify::ObligationVerifyResult>,
}

/// 终态收据：一次通过门禁评估的冻结快照。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FinalizationReceipt {
    pub receipt_id: String,
    /// 提交时冻结的审查材料哈希；diff 变化即收据失效。
    pub diff_hash: String,
    /// resource_scope.revision：工作区资源可见性版本。
    pub scope_revision: u64,
    /// 审查契约义务指纹：契约变更后旧收据不得复用。
    pub contract_fingerprint: String,
    #[serde(default)]
    pub acceptance_fingerprint: String,
    /// 可选登记材料变化时重新评估；旧收据无此字段，不据此证明证据仍未变化。
    #[serde(default)]
    pub evidence_index_fingerprint: Option<String>,
    /// 用户选择的多人审查及专家配置；旧单审收据默认为空。
    #[serde(default)]
    pub review_policy_fingerprint: String,
    /// 独立的真实冻结验证执行回执；旧 checkpoint 没有时必须重新验证。
    #[serde(default)]
    pub frozen_verification_run: Option<FrozenVerificationRun>,
    pub step_id: usize,
    pub verification: ReceiptVerification,
    pub review: ReceiptReview,
    /// 材料变化的观察与审查覆盖边界，只作披露，不自动阻断交付。
    #[serde(default)]
    pub material_notices: Vec<String>,
    /// 未闭合项；为空且 review 被接受 = ready-to-commit。
    pub blockers: Vec<String>,
    /// 高风险路径信号（鉴权/密钥/迁移/并发/安全策略）。
    pub risk_flags: Vec<String>,
    /// 预检时冻结的交付载荷；finish 指令缺 command 时由它回填。
    #[serde(default)]
    pub finish_payload: Value,
    pub created_at: String,
}

impl FinalizationReceipt {
    /// 结束后仍在变化时披露未知，不把未审版本标为可复用，也不合成质量缺陷。
    pub(crate) fn note_later_material_change(&mut self, current_hash: &str) {
        if self.diff_hash != current_hash {
            self.material_notices.push("审查结束后材料仍有变化；后续变化未取得独立审查结论，本次按已有审查结果交付，不能视为对持续变化的最终版本保证。".into());
            self.diff_hash = format!("unsettled:{}", self.diff_hash);
        }
    }

    /// 收据是否允许 finish 直接提交。
    pub fn is_ready(&self) -> bool {
        self.blockers.is_empty()
            && self.review.is_accepted()
            && !matches!(
                self.verification,
                ReceiptVerification::Pending | ReceiptVerification::Failed { .. }
            )
    }

    /// 收据是否仍然新鲜：diff、scope 版本与契约指纹三者全等才可复用。
    pub fn is_fresh(
        &self,
        diff_hash: &str,
        scope_revision: u64,
        contract_fingerprint: &str,
    ) -> bool {
        self.diff_hash == diff_hash
            && self.scope_revision == scope_revision
            && self.contract_fingerprint == contract_fingerprint
    }
}

/// 审查契约义务的确定性指纹：契约内容变化即收据失效。
pub fn contract_fingerprint(
    contract: Option<&crate::agent::review_contract::ReviewContract>,
) -> String {
    use sha2::{Digest, Sha256};
    let Some(contract) = contract else {
        return String::new();
    };
    let raw = serde_json::to_string(contract).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    format!("{:x}", hasher.finalize())
}

const RISK_PATTERNS: &[(&str, &[&str])] = &[
    (
        "auth",
        &["auth", "login", "session", "token", "oauth", "jwt", "sso"],
    ),
    (
        "secret",
        &[
            "secret",
            "credential",
            "password",
            "private_key",
            ".env",
            "apikey",
            "api_key",
        ],
    ),
    ("migration", &["migration", "schema", ".sql"]),
    (
        "concurrency",
        &["mutex", "lock", "atomic", "transaction", "channel"],
    ),
    (
        "security_policy",
        &["cors", "permission", "policy", "acl", "rbac", "sanitize"],
    ),
];

/// 机械风险信号：仅按路径名匹配，确定性、零 Token。
/// 命中的改动达到审查重试上限后不得自动带警告放行，必须升级人工仲裁。
pub fn detect_risk_flags(paths: &[String]) -> Vec<String> {
    let mut flags: Vec<String> = Vec::new();
    for path in paths {
        let lower = path.to_lowercase();
        for (flag, needles) in RISK_PATTERNS {
            if needles.iter().any(|needle| lower.contains(needle))
                && !flags.iter().any(|existing| existing == flag)
            {
                flags.push((*flag).to_string());
            }
        }
    }
    flags
}

/// 终态短事务提交：checkpoint Done + run trace succeeded + 会话 assistant 消息。
///
/// 复用 save_checkpoint / upsert_snapshot / append_message 的 SQL 语义，但收进同一
/// transaction：任一写入失败整体回滚，不产生「一处已提交、一处还在 running」的半截态。
/// checkpoint 沿用乐观锁版本谓词；冲突时回滚并返回错误，调用方下一步重试即可。
pub fn commit_finalization(
    db: &DbState,
    checkpoint: &mut AgentCheckpoint,
    run_id: &str,
    session_id: &str,
    goal: &str,
    steps: &[Value],
    completion: &Value,
    assistant_message: &str,
    model_id: &str,
) -> Result<(), String> {
    commit_terminal(
        db,
        checkpoint,
        run_id,
        session_id,
        goal,
        steps,
        completion,
        assistant_message,
        model_id,
        "succeeded",
    )
}

pub(crate) fn commit_failure(
    db: &DbState,
    checkpoint: &mut AgentCheckpoint,
    run_id: &str,
    session_id: &str,
    goal: &str,
    steps: &[Value],
    completion: &Value,
    assistant_message: &str,
) -> Result<(), String> {
    if checkpoint.phase != crate::agent::checkpoint::AgentPhase::Failed {
        return Err("失败提交要求 Failed 检查点，不得冒充成功交付".into());
    }
    commit_terminal(
        db,
        checkpoint,
        run_id,
        session_id,
        goal,
        steps,
        completion,
        assistant_message,
        "",
        "failed",
    )
}

fn commit_terminal(
    db: &DbState,
    checkpoint: &mut AgentCheckpoint,
    run_id: &str,
    session_id: &str,
    goal: &str,
    steps: &[Value],
    completion: &Value,
    assistant_message: &str,
    model_id: &str,
    status: &str,
) -> Result<(), String> {
    // 任何 SQL 失败均保持调用方 version 不变，不仅仅处理乐观锁冲突。
    let mut candidate = checkpoint.clone();
    commit_terminal_inner(
        db,
        &mut candidate,
        run_id,
        session_id,
        goal,
        steps,
        completion,
        assistant_message,
        model_id,
        status,
    )?;
    *checkpoint = candidate;
    Ok(())
}

fn commit_terminal_inner(
    db: &DbState,
    checkpoint: &mut AgentCheckpoint,
    run_id: &str,
    session_id: &str,
    goal: &str,
    steps: &[Value],
    completion: &Value,
    assistant_message: &str,
    model_id: &str,
    status: &str,
) -> Result<(), String> {
    let mut conn = db.0.lock().map_err(|error| error.to_string())?;
    let now = Utc::now().to_rfc3339();
    let tx = conn.transaction().map_err(|error| error.to_string())?;

    // 1) checkpoint：乐观锁一次尝试；冲突整体回滚
    checkpoint.updated_at = now.clone();
    let previous_version = checkpoint.version;
    checkpoint.version += 1;
    let snapshot_json = serde_json::to_string(checkpoint).map_err(|error| error.to_string())?;
    if previous_version == 0 {
        tx.execute(
            "INSERT INTO agent_checkpoints (run_id, phase, snapshot_json, version, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                checkpoint.run_id,
                checkpoint.phase.as_str(),
                snapshot_json,
                checkpoint.version,
                checkpoint.created_at,
                checkpoint.updated_at
            ],
        )
        .map_err(|error| error.to_string())?;
    } else {
        let rows = tx
            .execute(
                "UPDATE agent_checkpoints SET phase = ?1, snapshot_json = ?2, version = ?3, updated_at = ?4 WHERE run_id = ?5 AND version = ?6",
                params![
                    checkpoint.phase.as_str(),
                    snapshot_json,
                    checkpoint.version,
                    checkpoint.updated_at,
                    checkpoint.run_id,
                    previous_version
                ],
            )
            .map_err(|error| error.to_string())?;
        if rows != 1 {
            checkpoint.version = previous_version;
            return Err("checkpoint 乐观锁冲突：终态提交已整体回滚".to_string());
        }
    }

    // 2) run trace：与 upsert_snapshot 相同语义的终态 upsert
    let steps_json = serde_json::to_string(steps).map_err(|error| error.to_string())?;
    let completion_json = serde_json::to_string(completion).map_err(|error| error.to_string())?;
    tx.execute(
        "INSERT INTO agent_run_traces (id, session_id, goal, status, steps_json, completion_json, started_at, updated_at, completed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, CASE WHEN ?4 IN ('succeeded', 'failed', 'cancelled') THEN ?7 ELSE NULL END)
         ON CONFLICT(id) DO UPDATE SET
            status=excluded.status,
            steps_json=CASE WHEN excluded.steps_json = '[]' AND agent_run_traces.steps_json != '[]' AND agent_run_traces.steps_json IS NOT NULL THEN agent_run_traces.steps_json ELSE excluded.steps_json END,
            completion_json=COALESCE(excluded.completion_json, agent_run_traces.completion_json),
            updated_at=excluded.updated_at,
            completed_at=CASE WHEN excluded.status IN ('succeeded', 'failed', 'cancelled') THEN excluded.updated_at ELSE agent_run_traces.completed_at END",
        params![run_id, session_id, goal, status, steps_json, completion_json, now],
    )
    .map_err(|error| error.to_string())?;

    // 3) 会话 assistant 消息 + session 计数
    let assistant_message_id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO chat_messages (id, session_id, role, content, attachments, created_at, model_id)
         VALUES (?1, ?2, 'assistant', ?3, NULL, ?4, ?5)",
        params![
            assistant_message_id,
            session_id,
            assistant_message,
            now,
            model_id
        ],
    )
    .map_err(|error| error.to_string())?;
    tx.execute(
        "UPDATE chat_sessions SET updated_at=?1, message_count=message_count+1 WHERE id=?2",
        params![now, session_id],
    )
    .map_err(|error| error.to_string())?;

    // 4) 回合边界 checkpoint 历史快照（append-only，供会话分叉取回该回合的状态）
    tx.execute(
        "INSERT INTO agent_checkpoint_history (session_id, anchor_message_id, phase, snapshot_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            session_id,
            assistant_message_id,
            checkpoint.phase.as_str(),
            snapshot_json,
            now
        ],
    )
    .map_err(|error| error.to_string())?;

    tx.commit().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::context::SandwichContext;
    use rusqlite::Connection;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    #[test]
    fn detailed_verification_failure_remains_blocking_and_keeps_command_directory_output() {
        let result = crate::agent::host_verify::ObligationVerifyResult {
            obligation_id: "unit".into(),
            command: "mvn test".into(),
            working_dir: "/tmp/ws".into(),
            exit_code: "1".into(),
            output_tail: "[ERROR] No tests matching pattern".into(),
            passed: false,
        };
        let mut r = receipt("hash", 0, "fp");
        r.blockers
            .push(crate::agent::host_verify::format_obligation_failure(
                &result,
            ));
        assert!(!r.is_ready());
        let text = &r.blockers[0];
        assert!(text.contains("mvn test") && text.contains("/tmp/ws"));
        assert!(text.contains("退出码 1") && text.contains("No tests matching pattern"));
    }

    fn test_db() -> DbState {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE agent_checkpoints (run_id TEXT PRIMARY KEY, phase TEXT NOT NULL, snapshot_json TEXT NOT NULL, version INTEGER NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
             CREATE TABLE agent_checkpoint_history (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, anchor_message_id TEXT NOT NULL, phase TEXT NOT NULL, snapshot_json TEXT NOT NULL, created_at TEXT NOT NULL);
             CREATE TABLE agent_run_traces (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, goal TEXT NOT NULL, status TEXT NOT NULL, steps_json TEXT, completion_json TEXT, started_at TEXT NOT NULL, updated_at TEXT NOT NULL, completed_at TEXT);
             CREATE TABLE chat_sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL, model_id TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, message_count INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE chat_messages (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, role TEXT NOT NULL, content TEXT NOT NULL, attachments TEXT, created_at TEXT NOT NULL, model_id TEXT);",
        )
        .unwrap();
        DbState(Arc::new(Mutex::new(conn)))
    }

    fn receipt(diff_hash: &str, revision: u64, fingerprint: &str) -> FinalizationReceipt {
        FinalizationReceipt {
            receipt_id: "fr_test".to_string(),
            diff_hash: diff_hash.to_string(),
            scope_revision: revision,
            contract_fingerprint: fingerprint.to_string(),
            acceptance_fingerprint: "test-state".into(),
            evidence_index_fingerprint: None,
        review_policy_fingerprint: String::new(),
            frozen_verification_run: None,
            material_notices: vec![],
            step_id: 10,
            verification: ReceiptVerification::Verified {
                evidence: "cargo test ok".to_string(),
            },
            review: ReceiptReview::Passed {
                score: 92,
                model_id: "reviewer-x".to_string(),
                summary: "ok".to_string(),
            },
            blockers: Vec::new(),
            risk_flags: Vec::new(),
            finish_payload: Value::Null,
            created_at: "2026-09-25T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn legacy_receipt_without_frozen_host_execution_remains_readable_but_cannot_reuse_it() {
        let mut json = serde_json::to_value(receipt("diff", 0, "contract")).unwrap();
        json.as_object_mut().unwrap().remove("frozen_verification_run");
        json.as_object_mut().unwrap().remove("material_notices");
        let restored: FinalizationReceipt = serde_json::from_value(json).unwrap();
        assert!(restored.material_notices.is_empty());
        assert!(restored.frozen_verification_run.is_none());
    }

    #[test]
    fn corrected_claim_or_contract_scope_invalidates_previous_acceptance() {
        use crate::agent::review_contract::ReviewContract;
        let mut c: ReviewContract = serde_json::from_value(serde_json::json!({
            "scope_statement":"验收登录", "obligations":[{
                "id":"login", "question":"登录是否正确", "status":"evidenced",
                "evidence_refs":["AuthService.java:89-93"]
            }]
        }))
        .unwrap();
        let fp = contract_fingerprint(Some(&c));
        let r = receipt("same-diff", 0, &fp);
        assert!(r.is_ready() && r.is_fresh("same-diff", 0, &fp));
        c.apply_update(
            serde_json::from_value(serde_json::json!({"obligations":[{
                "id":"login", "status":"evidenced", "evidence_refs":["src/AuthService.java:89-93"]
            }]}))
            .unwrap(),
        )
        .unwrap();
        assert!(!r.is_fresh("same-diff", 0, &contract_fingerprint(Some(&c))));
        let corrected = contract_fingerprint(Some(&c));
        let r2 = receipt("same-diff", 0, &corrected);
        c.scope_statement = "验收登录与发布包".into();
        assert!(!r2.is_fresh("same-diff", 0, &contract_fingerprint(Some(&c))));
    }

    #[test]
    fn receipt_freshness_requires_all_keys() {
        let r = receipt("abc", 3, "fp");
        assert!(r.is_fresh("abc", 3, "fp"));
        assert!(!r.is_fresh("def", 3, "fp"));
        assert!(!r.is_fresh("abc", 4, "fp"));
        assert!(!r.is_fresh("abc", 3, "other"));
    }

    #[test]
    fn receipt_ready_requires_no_blockers_and_accepted_review() {
        let mut r = receipt("abc", 3, "fp");
        assert!(r.is_ready());
        r.blockers.push("todo open".to_string());
        assert!(!r.is_ready());
        r.blockers.clear();
        r.review = ReceiptReview::Failed {
            score: 40,
            model_id: "m".to_string(),
            blocking_count: 2,
        };
        assert!(!r.is_ready());
        r.review = ReceiptReview::DeliveredWithWarnings {
            score: 55,
            model_id: "m".to_string(),
            blocking_count: 1,
        };
        assert!(r.is_ready());
    }

    #[test]
    fn risk_flags_hit_high_risk_path_patterns() {
        let flags = detect_risk_flags(&[
            "src/auth/login.rs".to_string(),
            "config/.env".to_string(),
            "db/migrations/001_init.sql".to_string(),
            "src/util/plain.rs".to_string(),
        ]);
        assert!(flags.contains(&"auth".to_string()));
        assert!(flags.contains(&"secret".to_string()));
        assert!(flags.contains(&"migration".to_string()));
        assert!(!flags.contains(&"concurrency".to_string()));
        assert!(!flags.contains(&"security_policy".to_string()));
        // 去重：多条同旗路径只记一次
        let flags = detect_risk_flags(&["a/auth/x.rs".to_string(), "b/oauth/y.rs".to_string()]);
        assert_eq!(flags.iter().filter(|f| *f == "auth").count(), 1);
    }

    #[test]
    fn commit_finalization_writes_three_stores_atomically() {
        let db = test_db();
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO chat_sessions (id, title, model_id, created_at, updated_at, message_count) VALUES ('s1', 't', 'm', 'now', 'now', 0)",
            [],
        )
        .unwrap();
        drop(conn);

        let context = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-1".to_string(), &context);
        checkpoint.mark_done(&context);

        commit_finalization(
            &db,
            &mut checkpoint,
            "run-1",
            "s1",
            "goal",
            &[json!({"id": 0, "status": "succeeded"})],
            &json!({"summary": "done"}),
            "交付摘要",
            "model-a",
        )
        .unwrap();

        let conn = db.0.lock().unwrap();
        let phase: String = conn
            .query_row(
                "SELECT phase FROM agent_checkpoints WHERE run_id='run-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(phase, "done");
        let status: String = conn
            .query_row(
                "SELECT status FROM agent_run_traces WHERE id='run-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "succeeded");
        let msg_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM chat_messages WHERE session_id='s1' AND role='assistant'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(msg_count, 1);
        let count: i64 = conn
            .query_row(
                "SELECT message_count FROM chat_sessions WHERE id='s1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn commit_finalization_rolls_back_on_version_conflict() {
        let db = test_db();
        {
            let conn = db.0.lock().unwrap();
            conn.execute(
                "INSERT INTO chat_sessions (id, title, model_id, created_at, updated_at, message_count) VALUES ('s1', 't', 'm', 'now', 'now', 0)",
                [],
            )
            .unwrap();
        }
        let context = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut checkpoint = AgentCheckpoint::new("run-2".to_string(), &context);
        // 先正常落一次，再人为推进版本制造乐观锁冲突
        crate::agent::checkpoint::save_checkpoint(&db, &mut checkpoint).unwrap();
        {
            let conn = db.0.lock().unwrap();
            conn.execute(
                "UPDATE agent_checkpoints SET version = version + 5 WHERE run_id='run-2'",
                [],
            )
            .unwrap();
        }
        checkpoint.mark_done(&context);
        let err = commit_finalization(
            &db,
            &mut checkpoint,
            "run-2",
            "s1",
            "goal",
            &[],
            &json!({}),
            "msg",
            "m",
        )
        .unwrap_err();
        assert!(err.contains("乐观锁冲突"));
        // 事务回滚：trace 与消息不得留下半截终态
        let conn = db.0.lock().unwrap();
        let traces: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_run_traces WHERE id='run-2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(traces, 0);
        let msgs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM chat_messages WHERE session_id='s1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(msgs, 0);
    }
}
