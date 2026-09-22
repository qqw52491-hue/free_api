//! 中途重新分流（retriage）：探索期的一次性 supervisor 复核。
//!
//! 定位（对应设计宪法的「认知流水线」）：
//! - 触发是事实边界：模型主动申请（request_retriage）或资源里程点（可选 flag）；
//! - 判断是认知外包：宿主只机械组装证据包，由 supervisor 档模型判决
//!   continue / narrow / split / replan / escalate；
//! - 结果持久化：追加进 checkpoint.triage_revisions，原 triage_verdict 永不改写；
//! - 不是常驻监管：每 run 限额（默认 1 次），调用失败一律降级为 continue。
//!
//! 与入口 triage 的区别：triage 在零证据时凭题面分流；retriage 带着
//! 执行轨迹与模型已收集的未决问题复核「分流是否仍然成立」。

use crate::agent::checkpoint::AgentCheckpoint;
use crate::agent::context::SandwichContext;
use crate::agent::router::ModelRoutingConfig;
use crate::agent::types::TriageRevision;
use crate::db::DbState;
use crate::llm::{self, ModelConfig};
use serde_json::json;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;

const RETRIAGE_TIMEOUT_SECS: u64 = 45;
const DEFAULT_MAX_RETRIAGE_PER_RUN: u32 = 1;

/// 每 run 允许的 retriage 次数（0 = 整个机制关闭）。env 可调，封顶 3。
pub(crate) fn max_retriage_per_run() -> u32 {
    std::env::var("FREE_API_RETRIAGE_MAX_PER_RUN")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .map(|v| v.min(3))
        .unwrap_or(DEFAULT_MAX_RETRIAGE_PER_RUN)
}

/// 通道 B（实验，默认关）：资源里程点步数阈值。>0 时，探索相位走到该步数
/// 且本轮尚未 retriage，宿主自动发起一次复核。缺省 0 = 只保留模型主动申请通道。
pub(crate) fn retriage_milestone_steps() -> usize {
    std::env::var("FREE_API_RETRIAGE_MILESTONE_STEPS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0)
}

/// supervisor 结构化判决容错解析：未知 decision 收敛到 continue（不动 = 最安全判决）。
fn normalize_revision(mut rev: TriageRevision) -> TriageRevision {
    rev.decision = match rev.decision.trim().to_ascii_lowercase().as_str() {
        "continue" | "narrow" | "split" | "replan" | "escalate" => {
            rev.decision.trim().to_ascii_lowercase()
        }
        _ => "continue".to_string(),
    };
    rev.next_questions.retain(|q| !q.trim().is_empty());
    rev.next_questions.truncate(6);
    rev.split_tasks.retain(|t| !t.trim().is_empty());
    rev.split_tasks.truncate(6);
    rev
}

/// 机械组装 supervisor 证据包：宿主事实（轨迹/相位/预算）与模型自述
/// （reason/unresolved_questions）分层标注，防止自述被误当系统事实。
fn build_evidence_packet(
    db: &DbState,
    checkpoint: &AgentCheckpoint,
    goal: &str,
    session_id: &str,
    request_reason: &str,
    request_questions: &[String],
) -> String {
    let triage_json = checkpoint
        .triage_verdict
        .as_ref()
        .and_then(|v| serde_json::to_string_pretty(v).ok())
        .unwrap_or_else(|| "（未执行入口分流）".to_string());
    let trace =
        crate::agent::reviewer_gatekeeper::collect_step_trace_digest(db, session_id)
    .unwrap_or_else(|| "（无执行轨迹）".to_string());
    let questions = if request_questions.is_empty() {
        "（执行模型未列出未决问题）".to_string()
    } else {
        request_questions
            .iter()
            .enumerate()
            .map(|(i, q)| format!("{}. {}", i + 1, q))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "【任务目标】\n{goal}\n\n\
         【入口分流判决】（先验，可被证据推翻）\n{triage_json}\n\n\
         【当前相位】{} | 已执行步数 {}\n\n\
         【执行轨迹摘要】（宿主事实：最近步骤实际做过什么）\n{trace}\n\n\
         【执行模型自述】（模型声明，非系统事实）\n申请理由: {}\n未决问题:\n{}",
        checkpoint.plan_phase.as_str(),
        checkpoint.step_id,
        if request_reason.trim().is_empty() {
            "（宿主资源里程点自动触发，无模型自述）"
        } else {
            request_reason.trim()
        },
        questions,
    )
}

/// 一次性 retriage 调用：supervisor 档（Pro）看证据包做结构化复核。
/// 任何失败（模型缺失/超时/解析失败）都降级为 continue，绝不中断主循环。
pub(crate) async fn run_retriage<R: tauri::Runtime>(
    app: &AppHandle<R>,
    db: &DbState,
    model_routing: &ModelRoutingConfig,
    checkpoint: &AgentCheckpoint,
    goal: &str,
    trigger: &str,
    request_reason: &str,
    request_questions: &[String],
    session_id: &str,
    mut interrupt_rx: watch::Receiver<u64>,
) -> TriageRevision {
    let mut revision = TriageRevision {
        at_step: checkpoint.step_id,
        trigger: trigger.to_string(),
        ..Default::default()
    };
    let model_id = if !model_routing.pro.trim().is_empty() {
        model_routing.pro.trim().to_string()
    } else if !model_routing.flash.trim().is_empty() {
        model_routing.flash.trim().to_string()
    } else {
        model_routing.vision.trim().to_string()
    };
    if model_id.is_empty() {
        revision.decision = "continue".to_string();
        revision.fallback = Some("未配置可用 supervisor 模型".to_string());
        return revision;
    }

    let _ = app.emit(
        &format!("agent-progress-{}", session_id),
        json!({
            "type": "action_phase",
            "phase": "retriaging",
            "description": "🧭 正在对执行中的任务做重新分流复核..."
        }),
    );

    let packet =
        build_evidence_packet(db, checkpoint, goal, session_id, request_reason, request_questions);
    let system_prompt = r#"你是任务分流复核员（Retriage Supervisor）。一个执行中的 Agent 探索了一段时间仍未收敛，请求你复核「最初的任务分流是否仍然成立」。
你必须且仅输出一个合法的 JSON 对象，禁止输出任何 Markdown 标记、解释或多余文本：
{
  "decision": "continue | narrow | split | replan | escalate",
  "reason": "判决理由（必须引用证据包中的具体事实）",
  "next_questions": ["narrow 判决时给出 1~6 条窄化的取证问题，其他判决留空数组"],
  "split_tasks": ["split 判决时给出 1~6 个可并行派发的窄子任务描述，其他判决留空数组"],
  "recommended_tier": "current | stronger"
}

【判决语义】
- continue：当前探索方向合理，漫游只是任务本身广——不做任何干预；
- narrow：方向对但太散——给出有限的下步取证问题，让执行模型按单取证；
- split：任务实际可拆——给出窄子任务清单，宿主将建议并行派发侦察 worker；
- replan：入口分流被新证据推翻（如预判是小改实际是系统性问题）——执行模型须携 revision_reason 重新提交方案；
- escalate：任务本身超出当前执行模型的决策含量——建议后续关键步骤换更强模型。

【判定纪律】
- 探索步数多不等于失败：广泛任务本来就需要大量取证，证据在推进就判 continue；
- 只在轨迹显示「同区域反复取证无新信息」或「原判决被证据证伪」时才改判；
- recommended_tier 只在 decision=escalate 时有意义；
- 拿不准判 continue——错判干预的代价比不干预高。"#;

    let messages = vec![
        json!({ "role": "system", "content": system_prompt }),
        json!({ "role": "user", "content": packet }),
    ];

    let model_config = match ModelConfig::from_db_state(db, &model_id) {
        Ok(cfg) => cfg,
        Err(e) => {
            revision.decision = "continue".to_string();
            revision.model_id = model_id;
            revision.fallback = Some(format!("加载 supervisor 模型配置失败: {}", e));
            return revision;
        }
    };

    let mut ignore_stream = |_: llm::StreamEvent<'_>| true;
    let call = llm::stream_with_retry(
        &model_config,
        &messages,
        "auto",
        &mut interrupt_rx,
        1,
        &mut ignore_stream,
    );

    let stream_result =
        match tokio::time::timeout(Duration::from_secs(RETRIAGE_TIMEOUT_SECS), call).await {
            Ok(res) => res,
            Err(_) => {
                crate::agent::utils::llm_client::log_auxiliary_llm_exchange(
                    session_id,
                    "retriage",
                    1,
                    &model_config,
                    &messages,
                    None,
                    "request_error",
                    Some("timeout"),
                );
                revision.decision = "continue".to_string();
                revision.model_id = model_id;
                revision.fallback = Some("分流复核调用超时(>45s)".to_string());
                return revision;
            }
        };

    match stream_result {
        Ok(output) => {
            crate::agent::utils::llm_client::log_auxiliary_llm_exchange(
                session_id,
                "retriage",
                1,
                &model_config,
                &messages,
                Some(&output),
                "success",
                None,
            );
            let clean = crate::agent::triage::extract_json_str(&output.full_response);
            match serde_json::from_str::<TriageRevision>(&clean) {
                Ok(parsed) => {
                    let mut rev = normalize_revision(parsed);
                    rev.at_step = checkpoint.step_id;
                    rev.trigger = trigger.to_string();
                    rev.model_id = model_id;
                    rev
                }
                Err(_) => {
                    revision.decision = "continue".to_string();
                    revision.model_id = model_id;
                    revision.fallback = Some("分流复核判决 JSON 解析失败".to_string());
                    revision
                }
            }
        }
        Err(e) => {
            crate::agent::utils::llm_client::log_auxiliary_llm_exchange(
                session_id,
                "retriage",
                1,
                &model_config,
                &messages,
                None,
                "request_error",
                Some(&e),
            );
            revision.decision = "continue".to_string();
            revision.model_id = model_id;
            revision.fallback = Some(format!("分流复核调用失败: {}", e));
            revision
        }
    }
}

/// 判决落地：追加审计历史 + 消耗配额（仅真实判决消耗，降级不烧配额）+
/// 按 decision 接入现有通道。返回给执行模型的反馈文本。
///
/// 不对称规则：narrow/split/escalate（加严/加监管）自动执行；
/// supervisor 无权放宽任何门禁——本函数不产生任何放宽效果。
pub(crate) fn apply_retriage_revision(
    context: &mut SandwichContext,
    checkpoint: &mut AgentCheckpoint,
    revision: &TriageRevision,
) -> String {
    if revision.fallback.is_none() {
        checkpoint.retriage_used += 1;
    }
    checkpoint.triage_revisions.push(revision.clone());

    let header = format!(
        "【🧭 分流复核结论】{}",
        if let Some(ref fb) = revision.fallback {
            format!("复核调用降级（{}），按 continue 处理。", fb)
        } else {
            format!("decision={}。{}", revision.decision, revision.reason)
        }
    );

    match revision.decision.as_str() {
        "narrow" => {
            let qs = if revision.next_questions.is_empty() {
                "（supervisor 未给出具体问题，按判决理由自行收敛）".to_string()
            } else {
                revision
                    .next_questions
                    .iter()
                    .enumerate()
                    .map(|(i, q)| format!("{}. {}", i + 1, q))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            format!("{}\n【后续取证清单】按此清单取证，不再扩散搜索：\n{}", header, qs)
        }
        "split" => {
            let tasks = if revision.split_tasks.is_empty() {
                "（supervisor 未给出子任务，按判决理由自行拆分）".to_string()
            } else {
                revision
                    .split_tasks
                    .iter()
                    .enumerate()
                    .map(|(i, t)| format!("{}. {}", i + 1, t))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            format!(
                "{}\n【建议拆分】以下子任务可用 child_worker 的 dispatches 数组并行派发侦察：\n{}",
                header, tasks
            )
        }
        "replan" => format!(
            "{}\n【动作要求】入口分流已被新证据推翻：修正方案后重新输出 submit_plan，并携带 revision_reason 说明本次修订依据（将重开 Tech Lead 评审）。",
            header
        ),
        "escalate" => {
            let escalation_note = if let Some(block) = context.active_target_block_mut() {
                match block.request_pro_escalation() {
                    Ok(_) => format!(
                        "目标块 [{}] 已获准升维：下一步骤将使用强模型执行。",
                        block.id
                    ),
                    Err(err) => format!("升维配额申请未通过（{}），请按判决理由继续。", err),
                }
            } else {
                "当前无活跃目标块可升维；请在后续关键步骤中按判决理由提高论证强度。".to_string()
            };
            format!("{}\n【升维处理】{}", header, escalation_note)
        }
        _ => format!("{}\n当前探索方向经复核合理，按原计划继续。", header),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::context::SandwichContext;

    fn revision(decision: &str) -> TriageRevision {
        TriageRevision {
            at_step: 30,
            trigger: "agent_request".to_string(),
            decision: decision.to_string(),
            reason: "测试判决".to_string(),
            next_questions: vec!["查 checkpoint 保存路径".to_string()],
            split_tasks: vec![],
            recommended_tier: String::new(),
            model_id: "m".to_string(),
            fallback: None,
        }
    }

    #[test]
    fn normalize_unknown_decision_converges_to_continue() {
        let mut rev = revision("  NUKE ");
        rev = normalize_revision(rev);
        assert_eq!(rev.decision, "continue");
        let rev = normalize_revision(revision("NARROW"));
        assert_eq!(rev.decision, "narrow");
    }

    #[test]
    fn real_verdict_consumes_quota_fallback_does_not() {
        let context = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("s".to_string(), &context);
        let mut ctx = SandwichContext::new("sys".to_string(), "goal".to_string());

        let rev = revision("narrow");
        apply_retriage_revision(&mut ctx, &mut cp, &rev);
        assert_eq!(cp.retriage_used, 1);
        assert_eq!(cp.triage_revisions.len(), 1);

        let mut degraded = revision("escalate");
        degraded.fallback = Some("timeout".to_string());
        apply_retriage_revision(&mut ctx, &mut cp, &degraded);
        assert_eq!(cp.retriage_used, 1, "降级判决不得消耗配额");
        assert_eq!(cp.triage_revisions.len(), 2, "降级判决仍须入审计历史");
        assert_eq!(cp.triage_revisions[1].decision, "escalate");
    }

    #[test]
    fn narrow_verdict_surfaces_questions() {
        let context = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("s".to_string(), &context);
        let mut ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let msg = apply_retriage_revision(&mut ctx, &mut cp, &revision("narrow"));
        assert!(msg.contains("checkpoint 保存路径"), "narrow 必须把取证清单带回给执行模型");
    }

    #[test]
    fn escalate_without_active_block_degrades_to_guidance() {
        let context = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("s".to_string(), &context);
        let mut ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let msg = apply_retriage_revision(&mut ctx, &mut cp, &revision("escalate"));
        assert!(msg.contains("无活跃目标块"), "无块时不应 panic 也不应假装已升维");
    }

    #[test]
    fn escalate_with_block_consumes_block_quota() {
        let context = SandwichContext::new("sys".to_string(), "goal".to_string());
        let mut cp = AgentCheckpoint::new("s".to_string(), &context);
        let mut ctx = SandwichContext::new("sys".to_string(), "goal".to_string());
        let block = crate::agent::target_block::TargetBlock::new("b1", "重构调度");
        ctx.block_manager.set_blocks(vec![block]);
        let msg = apply_retriage_revision(&mut ctx, &mut cp, &revision("escalate"));
        assert!(msg.contains("b1"));
        assert_eq!(ctx.active_target_block().unwrap().escalate_quota, 1);
    }
}
