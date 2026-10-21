//! 预检与正式交付共用的唯一评估链。检查事实、执行验证、评审、生成收据；不提交终态。
use super::*;
use crate::agent::change_verification::{change_verification_blocker, ChangeVerification};
use crate::agent::reviewer_gatekeeper::{GitBaseline, ReviewResult};
use crate::agent::router::ModelRoutingConfig;
use crate::agent::types::AgentInstruction;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionMode {
    Prepare,
    Finish,
}

pub(crate) struct CompletionEvaluation {
    pub receipt: FinalizationReceipt,
    pub review_result: Option<ReviewResult>,
}

/// “检查过”与重试次数不是验收结果。重试预算只影响修复/仲裁指引。
pub(crate) fn review_acceptance(
    review: &ReviewResult,
    retries: u32,
    human_waived: bool,
) -> ReceiptReview {
    if review.is_passed() {
        ReceiptReview::Passed {
            score: review.score,
            model_id: review.model_id.clone(),
            summary: review.summary.clone(),
        }
    } else if !review.is_service_failure() && retries >= 3 && human_waived {
        ReceiptReview::DeliveredWithWarnings {
            score: review.score,
            model_id: review.model_id.clone(),
            blocking_count: review.blocking_issues.len(),
        }
    } else {
        ReceiptReview::Failed {
            score: review.score,
            model_id: review.model_id.clone(),
            blocking_count: review.blocking_issues.len(),
        }
    }
}

/// 只冻结会影响验收的状态，不把工具观测文本/产物登记混入变更版本。
/// 兼容旧检查点：缺失此指纹的旧收据必须重评，不能继承成“已通过”。
pub(crate) fn acceptance_fingerprint(
    context: &SandwichContext,
    checkpoint: &AgentCheckpoint,
    finish_payload: &Value,
) -> String {
    use sha2::{Digest, Sha256};
    let raw = json!({
        "version": 3,
        "current_goal": context.effective_goal(),
        "effects": context.side_effects,
        "inherited": context.inherited_side_effects_count,
        "verification": context.change_verification,
        "resource_scope_revision": context.resource_scope.revision,
        "contract": context.review_contract,
        "plan": checkpoint.submitted_plan,
        "approved_targets": checkpoint.approved_target_files,
        "human_waivers": checkpoint.human_waiver_text(),
        "stage_findings": checkpoint.stage_review_final_only,
        "stage_blockers": checkpoint.stage_review_blockers_open,
        "plan_phase": checkpoint.plan_phase,
        "plan_mode": checkpoint.plan_mode,
        "todos": context.todo_list.iter().map(|t| (&t.id, &t.status, &t.description)).collect::<Vec<_>>(),
        "active_scope": context.active_target_block().map(|b| (&b.id, &b.scope)),
        "summary": finish_payload.get("summary"),
        "details": finish_payload.get("details"),
    });
    format!("{:x}", Sha256::digest(raw.to_string().as_bytes()))
}

/// 冻结验证结果的输入身份不包含交付说明、证据索引或 Reviewer 判决。
/// 这些变更只应影响审查；实际工具副作用、既有验证状态及范围变化仍需重新核对。
fn verification_effects_fingerprint(context: &SandwichContext) -> String {
    use sha2::{Digest, Sha256};
    let raw = json!({
        "effects": context.side_effects,
        "inherited": context.inherited_side_effects_count,
        "change_verification": context.change_verification,
    });
    format!("{:x}", Sha256::digest(raw.to_string().as_bytes()))
}

/// 仅接受此前由宿主真正执行且全部通过的同运行冻结命令。
/// 文件/契约/效果签名是必要的保守边界，不是验证适用性证明；
/// 后续 Reviewer 仍需看到旧命令的真实出处并决定是否需要补验。
fn reusable_frozen_verification<'a>(
    previous: Option<&'a FinalizationReceipt>,
    context: &SandwichContext,
    run_id: &str,
    workspace: &Path,
    current_material: &str,
    current_contract: &str,
    obligations: &[crate::agent::review_contract::ReviewObligation],
) -> Option<&'a FrozenVerificationRun> {
    let prior = previous?.frozen_verification_run.as_ref()?;
    let required: Vec<_> = obligations.iter().filter(|o| {
        o.verify_command.as_deref().is_some_and(|c| !c.trim().is_empty())
    }).collect();
    if required.is_empty() || required.len() != prior.results.len()
        || current_material.is_empty() || prior.material_fingerprint != current_material
        || prior.run_id != run_id
        || prior.workspace_root != workspace.canonicalize().ok()?.to_string_lossy()
        || prior.contract_fingerprint != current_contract
        || prior.scope_revision != context.resource_scope.revision
        || prior.effects_fingerprint != verification_effects_fingerprint(context)
        || !prior.results.iter().all(|result| result.passed && result.exit_code == "0")
    {
        return None;
    }
    // 不仅依赖哈希，还逐项匹配已冻结的命令和真实执行目录。
    required.iter().zip(&prior.results).all(|(ob, result)| {
        ob.id == result.obligation_id
            && ob.verify_command.as_deref() == Some(result.command.as_str())
    }).then_some(prior)
}

pub(crate) fn interrupted_since(receiver: &watch::Receiver<u64>, generation: u64) -> bool {
    *receiver.borrow() != generation
}

fn payload(instruction: &AgentInstruction) -> Value {
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
        instruction.command.clone().unwrap_or(Value::Null)
    };
    // 评估、指纹与正式提交使用同一份正文，不能验收一份再展示另一份。
    if let Some(object) = value.as_object_mut() {
        for key in ["summary", "details"] {
            if let Some(Value::String(text)) = object.get_mut(key) {
                *text = finish_summary_text(text);
            }
        }
    }
    value
}

/// 所有未闭合项使用同一份清单；动态检查每次重跑。
/// 就绪交付收据与真实冻结验证执行事实分别复用，不让说明修正无故重跑命令。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn evaluate_completion<R: tauri::Runtime>(
    app: &AppHandle<R>,
    db: &DbState,
    context: &mut SandwichContext,
    checkpoint: &mut AgentCheckpoint,
    run_id: &str,
    session_id: &str,
    step_id: usize,
    workspace: &Path,
    baseline: &GitBaseline,
    run_started_at: chrono::DateTime<Utc>,
    model_routing: &ModelRoutingConfig,
    interrupt_rx: &mut watch::Receiver<u64>,
    instruction: &AgentInstruction,
    mode: CompletionMode,
    scope_applicable: bool,
    contract_started: bool,
) -> CompletionEvaluation {
    let generation = *interrupt_rx.borrow();
    let request_payload = payload(instruction);
    let body = delivery_body(&request_payload);
    let (mut blockers, mut notices, diff_context) = collect_checks(
        db,
        context,
        checkpoint,
        run_id,
        session_id,
        step_id,
        workspace,
        baseline,
        run_started_at,
        scope_applicable,
        contract_started,
    )
    .await;
    let initial_hash = baseline
        .collect_review_material(workspace, context, run_started_at)
        .map(|(_, h)| h)
        .unwrap_or_default();
    let registered_evidence = crate::agent::evidence_store::review_digest(db, session_id, run_started_at);
    let state_fp = acceptance_fingerprint(context, checkpoint, &request_payload);
    let review_policy_fingerprint = crate::agent::review_team::policy_fingerprint(db, model_routing);
    let evidence_index_fingerprint = registered_evidence.as_ref().ok().and_then(|value| value.as_ref()).map(|digest| {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(digest.as_bytes()))
    });
    let contract_fp = contract_fingerprint(context.review_contract.as_ref());
    // 新增证据后应重新审查；索引无法读取时回到评估链并披露，不沿用未知版本的就绪收据。
    if blockers.is_empty() && registered_evidence.is_ok() && !interrupted_since(interrupt_rx, generation) {
        if let Some(cached) = checkpoint.pending_finalization.as_ref().filter(|r| {
            r.is_ready()
                && r.is_fresh(&initial_hash, context.resource_scope.revision, &contract_fp)
                && !r.acceptance_fingerprint.is_empty()
                && r.acceptance_fingerprint == state_fp
                && r.review_policy_fingerprint == review_policy_fingerprint
                && r.evidence_index_fingerprint == evidence_index_fingerprint
        }) {
            let mut receipt = cached.clone();
            receipt.finish_payload = request_payload;
            crate::agent::telemetry::record_gate_fire(
                db,
                &crate::agent::telemetry::GateFireRecord {
                    run_id,
                    session_id,
                    step_id: step_id as i64,
                    gate: "finalization",
                    route: "finalization:receipt_reused",
                },
            );
            return CompletionEvaluation {
                receipt,
                review_result: checkpoint.last_review_result.clone(),
            };
        }
    }
    let new_effects = context.side_effects.len() > context.inherited_side_effects_count;
    let mut verification = if new_effects {
        ReceiptVerification::Pending
    } else {
        ReceiptVerification::NotRequired
    };
    let mut review = ReceiptReview::Pending;
    let mut review_result = None;
    let mut host_evidence = Vec::new();
    let obligations = context
        .review_contract
        .as_ref()
        .map(|c| c.obligations.clone())
        .unwrap_or_default();
    // 预检可能先因 TODO / 阶段 Blocking 打回；只要执行来源与输入身份仍有效，
    // 也应携带此前真实冻结验证回执，不要让这个新收据抹去唯一的宿主执行事实。
    let prior = if new_effects
        && !checkpoint.triage_verdict.as_ref().is_some_and(|v| {
            v.final_review_mode.eq_ignore_ascii_case("mechanical_only")
        })
    {
        reusable_frozen_verification(
            checkpoint.pending_finalization.as_ref(), context, run_id, workspace,
            &initial_hash, &contract_fp, &obligations,
        ).cloned()
    } else { None };
    let mut frozen_verification_run = prior.clone();
    if blockers.is_empty() && !interrupted_since(interrupt_rx, generation) {
        // 仅有实际宿主旧回执才可能复用；无回执、命令失败、材料/契约/副作用变化，
        // 或 mechanical_only 模式无法进行语义审查时，仍按原路径执行冻结验证。
        let reused_frozen = prior.is_some();
        let results = if let Some(ref previous) = prior {
            host_evidence.push(format!(
                "【旧冻结验证宿主执行回执】执行于 {}（step {}）；本轮未自动重跑。\n相同验收契约与交付材料只是复用候选，可能存在未被文件变更反映的环境/依赖变化；Reviewer 应核查适用性，必要时使用 verify 请求补验。",
                previous.executed_at, previous.executed_step_id
            ));
            previous.results.clone()
        } else if new_effects {
            crate::agent::host_verify::run_obligation_verifications(workspace, &obligations).await
        } else {
            Vec::new()
        };
        blockers.extend(
            results
                .iter()
                .filter(|r| !r.passed)
                .map(crate::agent::host_verify::format_obligation_failure),
        );
        host_evidence.extend(results.iter().map(|r| {
            format!(
                "义务 [{}] `{}`（启动目录：{}）→ 退出码 {}\n{}",
                r.obligation_id, r.command, r.working_dir.display(), r.exit_code, r.output_tail
            )
        }));
        // 冻结验证套件已实测通过，就保存宿主回执；不再要求模型另外抄一遍步骤号。
        if !reused_frozen && save_successful_obligation_receipt(context, step_id, &results) {
            // 捕获真实执行事实；material_fingerprint 在全部宿主验证结束后落定，
            // 避免命令本身产生构建产物导致误判为工作区发生了新的人工修改。
            frozen_verification_run = Some(FrozenVerificationRun {
                run_id: run_id.to_owned(),
                workspace_root: workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf())
                    .to_string_lossy().into_owned(),
                material_fingerprint: String::new(),
                contract_fingerprint: contract_fp.clone(),
                scope_revision: context.resource_scope.revision,
                effects_fingerprint: verification_effects_fingerprint(context),
                executed_at: chrono::Utc::now().to_rfc3339(),
                executed_step_id: step_id,
                results: results.clone(),
            });
        }
        if blockers.is_empty()
            && new_effects
            && context
                .change_verification
                .as_ref()
                .map_or(true, |v| !v.is_resolved())
            && !interrupted_since(interrupt_rx, generation)
        {
            if let Some(command) =
                crate::agent::change_verification::infer_adaptive_verification_command_in(
                    workspace,
                    &context.new_affected_paths(),
                )
            {
                let timeout = crate::agent::channel::channel_config_shared()
                    .read()
                    .ok()
                    .and_then(|c| c.verify_timeout_secs)
                    .unwrap_or(120)
                    .clamp(10, 600);
                let outcome =
                    crate::agent::host_verify::run_command(workspace, &command, timeout).await;
                let report = format!(
                    "验证命令: `{}`\n退出码: {}\n{}",
                    command, outcome.exit_code, outcome.output_tail
                );
                host_evidence.push(report.clone());
                if outcome.passed {
                    context.change_verification = Some(ChangeVerification {
                        status: "verified".into(),
                        evidence_refs: vec![format!(
                            "host_verify:step_{} (`{}` → exit 0)",
                            step_id, command
                        )],
                        waived_reason: None,
                    });
                } else if outcome.tool_unavailable() {
                    // 保留原有 NotApplicable 策略，但必须移交终审并向用户披露，不伪称测试通过。
                    context.change_verification = Some(ChangeVerification {
                        status: "waived".into(),
                        evidence_refs: vec![],
                        waived_reason: Some(format!(
                            "验证工具无法运行，未取得测试通过结论：{}",
                            report
                        )),
                    });
                    notices.push(format!("验证工具不适用，需终审核对替代证据：{}", report));
                } else {
                    blockers.push(format!("宿主验证已执行但未通过；本次未提交交付。\n{}\n请按原始诊断修复后重试；检查确实不适用时可申报 waived + waived_reason，终审核对理由。", report));
                }
            } else {
                let reason = if context.is_only_ui_or_doc_side_effects() {
                    "仅静态资源/文档改动，未执行自动化测试；终审核对交付内容"
                } else {
                    "未推导出可执行的自动验证命令，未取得测试通过结论；终审核对已有证据或豁免理由"
                };
                context.change_verification = Some(ChangeVerification {
                    status: "waived".into(),
                    evidence_refs: vec![],
                    waived_reason: Some(reason.into()),
                });
                notices.push(reason.into());
            }
        }
        if blockers.is_empty() {
            if let Some(b) = change_verification_blocker(
                context
                    .side_effects
                    .len()
                    .saturating_sub(context.inherited_side_effects_count),
                context.change_verification.as_ref(),
            ) {
                blockers.push(b);
            }
        }
        verification =
            verification_receipt(new_effects, context.change_verification.as_ref(), &blockers);
        if blockers.is_empty() && !interrupted_since(interrupt_rx, generation) {
            // 通道配置的额外测试保持原语义：完整回执交给终审，不擅自变更为新的硬门。
            if new_effects {
                if let Some(report) = crate::agent::host_verify::run(workspace).await {
                    host_evidence.push(report);
                }
            }
        }
    }
    // 测试允许生成构建产物；冻结评审前的材料，而非执行测试前的目录快照。
    let mut review_snapshot = baseline.collect_review_snapshot(workspace, context, run_started_at);
    let review_hash = review_snapshot.hash.clone();
    let mut receipt_hash = review_hash.clone();
    let mut material_notices = Vec::new();
    if let Some(ref mut frozen) = frozen_verification_run {
        if frozen.material_fingerprint.is_empty() {
            frozen.material_fingerprint = review_hash.clone();
        }
    }
    if blockers.is_empty() && !interrupted_since(interrupt_rx, generation) {
        let evidence = (!host_evidence.is_empty()).then(|| host_evidence.join("\n\n"));
        let human_waivers = checkpoint.human_waiver_text();
        let mut completed_review = None;
        // 稳定材料仍只审一次。观察到变化时补充一次现有审查，不把哈希变化判作缺陷；
        // 持续变化只披露覆盖边界，不无限追加审查或要求执行模型盲目重交。
        for refresh in 0..=1 {
            let result = crate::agent::reviewer_gatekeeper::evaluate_finish_diff(
                app,
                db,
                context,
                model_routing,
                step_id,
                workspace,
                baseline,
                run_started_at,
                session_id,
                interrupt_rx,
                if refresh == 0 {
                    checkpoint.last_reviewed_diff_hash.as_deref()
                } else {
                    None
                },
                if refresh == 0 {
                    checkpoint.last_review_result.as_ref()
                } else {
                    None
                },
                &checkpoint.stage_review_final_only,
                evidence.as_deref(),
                human_waivers.as_deref(),
                &notices,
                checkpoint.approved_target_files.as_deref(),
                checkpoint.triage_verdict.as_ref(),
                Some(checkpoint.plan_mode),
                body.as_deref(),
            )
            .await;
            let after_review = baseline.collect_review_snapshot(workspace, context, run_started_at);
            if review_snapshot.hash != after_review.hash {
                let paths = after_review.changed_paths_since(&review_snapshot);
                // 已有具体 blocking 或宿主复验失败继续按原路径处理，不能借材料变化
                // 重开一个空白审查环，把本轮真实失败的验证事实丢掉。
                if refresh == 0
                    && result.is_passed()
                    && !interrupted_since(interrupt_rx, generation)
                    && (model_routing.stage_review_committee || !checkpoint
                        .triage_verdict
                        .as_ref()
                        .is_some_and(|v| v.final_review_mode.eq_ignore_ascii_case("mechanical_only")))
                {
                    let mut notice = after_review.change_notice_since(&review_snapshot);
                    notice.push_str(&format!(
                        "\n此前审查针对变化前材料，其结论仅供重新判断参考：{}\n",
                        serde_json::to_string(&result).unwrap_or_default()
                    ));
                    notices.push(notice);
                    material_notices.push(format!(
                        "审查期间材料发生变化，已移交审查员复核；涉及路径：{}。",
                        if paths.is_empty() {
                            "未取得可归属路径".into()
                        } else {
                            paths
                                .iter()
                                .take(12)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join("、")
                        }
                    ));
                    review_snapshot = after_review;
                    receipt_hash = review_snapshot.hash.clone();
                    continue;
                }
                // 未审的最新状态不能冒充已经审查的版本；保留通过/阻塞的实际裁决。
                receipt_hash = format!("unsettled:{}", review_snapshot.hash);
                material_notices.push(format!("审查收口期间材料仍有变化（涉及 {} 个文件）；后续变化未取得独立审查结论，本次按已有审查结果处理，不保证持续变化的最终版本。", paths.len()));
            } else {
                receipt_hash = review_snapshot.hash.clone();
            }
            completed_review = Some(result);
            break;
        }
        let result = completed_review.expect("bounded review produces a result");
        if result.is_service_failure() {
            checkpoint.pending_task_failure = Some(result.summary.clone());
        }
        review = review_acceptance(
            &result,
            checkpoint.reviewer_retry_count,
            human_waivers
                .as_deref()
                .is_some_and(|w| !w.trim().is_empty()),
        );
        if !review.is_accepted() {
            blockers.push(review_failure_report(
                &result,
                checkpoint.reviewer_retry_count,
            ));
            if mode == CompletionMode::Finish && !result.is_service_failure() {
                checkpoint.reviewer_retry_count =
                    checkpoint.reviewer_retry_count.saturating_add(1).min(3);
                if let Some(block) = context.active_target_block_mut() {
                    block.record_review_failure();
                }
            }
        }
        checkpoint.last_reviewed_diff_hash = Some(result.diff_hash.clone());
        checkpoint.last_review_result = Some(result.clone());
        crate::agent::telemetry::record_gate_fire(
            db,
            &crate::agent::telemetry::GateFireRecord {
                run_id,
                session_id,
                step_id: step_id as i64,
                gate: "finish_review",
                route: if result.is_service_failure() {
                    "finish_review:service_failure"
                } else if matches!(review, ReceiptReview::DeliveredWithWarnings { .. }) {
                    "finish_review:human_arbitrated"
                } else if result.is_passed() {
                    "finish_review:passed"
                } else {
                    "finish_review:blocking"
                },
            },
        );
        review_result = Some(result);
    }
    if blockers.is_empty()
        && (crate::agent::child_reviewer::has_running_child_runs(db, run_id)
            || crate::agent::job_engine::has_running_jobs(db, run_id))
    {
        blockers.push("验收期间仍有后台任务在执行；请等待或停止后台任务后再提交。".into());
    }
    if interrupted_since(interrupt_rx, generation) {
        blockers.push("已收到停止或替换任务信号，本次不提交交付。".into());
    }
    let _ = diff_context; // 机械对账使用的同一份材料已在 collect_checks 中消费。
    CompletionEvaluation {
        receipt: FinalizationReceipt {
            receipt_id: format!("fr_{}_s{}", &run_id[..run_id.len().min(8)], step_id),
            diff_hash: receipt_hash,
            scope_revision: context.resource_scope.revision,
            contract_fingerprint: contract_fingerprint(context.review_contract.as_ref()),
            acceptance_fingerprint: acceptance_fingerprint(context, checkpoint, &request_payload),
            review_policy_fingerprint,
            evidence_index_fingerprint,
            frozen_verification_run,
            step_id,
            verification,
            review,
            material_notices,
            blockers,
            risk_flags: detect_risk_flags(&context.new_affected_paths()),
            finish_payload: request_payload,
            created_at: Utc::now().to_rfc3339(),
        },
        review_result,
    }
}

pub(crate) fn verification_receipt(
    required: bool,
    current: Option<&ChangeVerification>,
    failures: &[String],
) -> ReceiptVerification {
    if !failures.is_empty() {
        return ReceiptVerification::Failed {
            details: failures.join("\n\n"),
        };
    }
    if !required {
        return ReceiptVerification::NotRequired;
    }
    match current {
        Some(v) if v.is_resolved() && v.status.eq_ignore_ascii_case("verified") => {
            ReceiptVerification::Verified {
                evidence: v.evidence_refs.join("；"),
            }
        }
        Some(v) if v.is_resolved() && v.status.eq_ignore_ascii_case("waived") => {
            ReceiptVerification::Waived {
                reason: v.waived_reason.clone().unwrap_or_default(),
            }
        }
        _ => ReceiptVerification::Pending,
    }
}

fn save_successful_obligation_receipt(
    context: &mut SandwichContext,
    step_id: usize,
    results: &[crate::agent::host_verify::ObligationVerifyResult],
) -> bool {
    if results.is_empty() || results.iter().any(|r| !r.passed) {
        return false;
    }
    context.change_verification = Some(ChangeVerification {
        status: "verified".into(),
        evidence_refs: vec![format!(
            "host_verify:step_{}：{} 项冻结义务命令全部退出 0",
            step_id,
            results.len()
        )],
        waived_reason: None,
    });
    true
}

fn review_failure_report(result: &ReviewResult, retries: u32) -> String {
    let issues = result
        .blocking_issues
        .iter()
        .enumerate()
        .map(|(i, b)| {
            format!(
                "{}. [{}] {}\n修复要求：{}",
                i + 1,
                b.file,
                b.issue,
                b.requirement
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "终审未通过（{} 分），本次未提交交付。\n审查评价：{}\n{}\n{}",
        result.score,
        result.summary,
        issues,
        if retries >= 3 {
            "修复后仍可重新核验；认为意见有误或接受遗留风险时，请 ask_human 呈报上述缺陷申请仲裁。重试次数不会自动把失败变成通过。"
        } else {
            "请补正上述缺陷或证据后重新预检/交付。"
        }
    )
}

#[allow(clippy::too_many_arguments)]
async fn collect_checks(
    db: &DbState,
    context: &SandwichContext,
    checkpoint: &AgentCheckpoint,
    run_id: &str,
    session_id: &str,
    step_id: usize,
    workspace: &Path,
    git_baseline: &GitBaseline,
    run_started_at: chrono::DateTime<Utc>,
    scope_applicable: bool,
    review_contract_started_this_turn: bool,
) -> (
    Vec<String>,
    Vec<String>,
    Option<crate::agent::finish_guard::DiffContext>,
) {
    let side_effect_count = context
        .side_effects
        .len()
        .saturating_sub(context.inherited_side_effects_count);
    let diff_context = if side_effect_count > 0 {
        let affected_paths = context.new_affected_paths();
        let raw_diff = git_baseline.collect_git_diff(workspace).map(|(d, _)| d);
        Some(
            crate::agent::finish_guard::DiffContext::from_diff_and_paths(
                raw_diff.as_deref(),
                &affected_paths,
            ),
        )
    } else {
        None
    };

    // 诊断义务（triage 取证单下发的问题）与修复义务分流：
    // 前者证据位于"读过的文件"，静态定位即可成立；
    // 后者维持 diff 对账。判别锚点是宿主持久化的判决原单，模型改不了。
    let diagnostic_questions: &[String] = checkpoint
        .triage_verdict
        .as_ref()
        .map(|v| v.investigation_questions.as_slice())
        .unwrap_or(&[]);
    let mut open_todos = crate::agent::finish_guard::completion_claim_blockers_with_diff(
        &context.todo_list,
        context.review_contract.as_ref(),
        review_contract_started_this_turn,
        side_effect_count,
        diff_context.as_ref(),
        Some(workspace),
        diagnostic_questions,
    );
    let evidence_blockers = context
        .review_contract
        .as_ref()
        .map(|c| {
            crate::agent::finish_guard::review_contract_evidence_blockers(
                c,
                side_effect_count,
                diff_context.as_ref(),
                Some(workspace),
                diagnostic_questions,
            )
        })
        .unwrap_or_default();

    // 门禁触发遥测：义务证据二分法（diff 对账 / 诊断静态校验）的触发频率
    if !evidence_blockers.is_empty() {
        let route = if diff_context
            .as_ref()
            .map(|d| !d.changed_files.is_empty())
            .unwrap_or(false)
        {
            "finish_obligation_evidence:diff_not_matched"
        } else {
            "finish_obligation_evidence:static_check_failed"
        };
        crate::agent::telemetry::record_gate_fire(
            db,
            &crate::agent::telemetry::GateFireRecord {
                run_id: run_id,
                session_id: session_id,
                step_id: step_id as i64,
                gate: "finish_obligation_evidence",
                route,
            },
        );
    }
    // 门禁适用域交接：义务证据全部为非代码形态（命令输出/退出码/运维状态）时，
    // diff/静态定位对账无判据——机械门不适用，生成 Gate Notice 移交终审 + Skip 遥测。
    // Skip 绝不静默：审计侧靠 finish_obligation_evidence:noncode_refs:not_applicable
    // 的触发率发现"系统性钻空子"（如模型故意只填不可解析引用来逃对账）。
    let mut gate_notices: Vec<String> = context
        .review_contract
        .as_ref()
        .map(|c| {
            crate::agent::finish_guard::noncode_evidence_skip_reasons(
                c,
                side_effect_count,
                diff_context.as_ref(),
                Some(workspace),
            )
            .into_iter()
            .filter_map(|reason| {
                crate::agent::gate_applicability::GateApplicability::not_applicable_owned(reason)
                    .handoff_notice("契约义务证据对账门")
            })
            .collect()
        })
        .unwrap_or_default();
    if !gate_notices.is_empty() {
        crate::agent::telemetry::record_gate_fire(
            db,
            &crate::agent::telemetry::GateFireRecord {
                run_id: run_id,
                session_id: session_id,
                step_id: step_id as i64,
                gate: "finish_obligation_evidence",
                route: "finish_obligation_evidence:noncode_refs:not_applicable",
            },
        );
    }
    // 「可定位但未落 diff」披露：证据客观存在但不在改动区间——可能是诊断义务
    // （证据本就在只读文件）或陈旧行号。放行不拦截，移交终审做语义核验。
    let undiffed_notices: Vec<String> = context
        .review_contract
        .as_ref()
        .map(|c| {
            crate::agent::finish_guard::undiffed_locatable_notices(
                c,
                diff_context.as_ref(),
                Some(workspace),
            )
            .into_iter()
            .filter_map(|reason| {
                crate::agent::gate_applicability::GateApplicability::not_applicable_owned(reason)
                    .handoff_notice("契约义务证据对账门")
            })
            .collect()
        })
        .unwrap_or_default();
    if !undiffed_notices.is_empty() {
        crate::agent::telemetry::record_gate_fire(
            db,
            &crate::agent::telemetry::GateFireRecord {
                run_id: run_id,
                session_id: session_id,
                step_id: step_id as i64,
                gate: "finish_obligation_evidence",
                route: "finish_obligation_evidence:undiffed_refs:not_applicable",
            },
        );
        gate_notices.extend(undiffed_notices);
    }
    // 【廉价完成防线】评测 oracle 的判分进度是任务完成度的客观事实源：
    // 模型自declared的审查契约/TODO 只覆盖它自己申报的范围（实测：1/24 交付被判
    // succeeded）。oracle 未完成时把缺口作为持久 blocker——与契约证据未决同级，
    // 不因 finish_reviewed 置位或达到重试次数而失效。

    {
        if let Some((index, total, completed)) =
            crate::agent::eval_oracle::query_active_progress().await
        {
            if !completed && index < total {
                open_todos.push(format!(
                "宿主判分进度 {}/{} 未完成：本任务以宿主逐阶段判分为唯一完成标准，禁止以部分阶段通过为由交卷；请读取 CURRENT_STAGE.md 继续实现下一阶段并用 npm test 送验",
                index, total
            ));
            }
        }
    }
    if crate::agent::child_reviewer::has_running_child_runs(db, run_id)
        || crate::agent::job_engine::has_running_jobs(db, run_id)
    {
        open_todos.push("仍有后台异步任务或持续监听正在执行中：后台任务请调用 wait_jobs 挂起等待并接收结论；持续监听（watch）请调用 watch_stop 显式释放后再提交 finish，或执行其他独立操作后重试".to_string());
    }
    if checkpoint.stage_review_blockers_open > 0 {
        open_todos.push(format!(
        "阶段审查仍有 {} 条未解决的 blocking 缺陷：请先调用 resume_child 对质撤回，或调用 ask_human 申请人工仲裁放行",
        checkpoint.stage_review_blockers_open
    ));
    }
    collect_scope_and_targets(
        context,
        checkpoint,
        workspace,
        session_id,
        run_started_at,
        scope_applicable,
        diff_context.as_ref(),
        &mut open_todos,
        &mut gate_notices,
    );
    (open_todos, gate_notices, diff_context)
}

/// Scope 与方案目标对账必须对预检/正式交付采用同一适用域。
fn collect_scope_and_targets(
    context: &SandwichContext,
    checkpoint: &AgentCheckpoint,
    workspace: &Path,
    session_id: &str,
    started: chrono::DateTime<Utc>,
    scope_applicable: bool,
    diff: Option<&crate::agent::finish_guard::DiffContext>,
    blockers: &mut Vec<String>,
    notices: &mut Vec<String>,
) {
    if scope_applicable {
        blockers.extend(
            crate::agent::reviewer_gatekeeper::mechanical_precheck_violations(
                workspace,
                context.active_target_block(),
            )
            .into_iter()
            .map(|v| format!("scope 越界：{}；请补正改动或按已有流程申请扩展范围。", v)),
        );
    } else if context.active_target_block().is_some() {
        notices.push(
            "Scope 边界预检不适用：自由会话未绑定真实工作区，Git diff 判据缺失；移交终审。".into(),
        );
    }
    let targets = checkpoint
        .submitted_plan
        .as_ref()
        .map(|p| p.target_files.as_slice())
        .filter(|p| !p.is_empty())
        .or(checkpoint.approved_target_files.as_deref());
    let Some(targets) = targets else {
        return;
    };
    if !targets
        .iter()
        .any(|t| crate::agent::gate_applicability::is_workspace_target(t, workspace))
    {
        notices
            .push("零交集对账不适用：方案目标全部位于工作区外，Git diff 无判据；移交终审。".into());
        return;
    }
    let tmp = crate::agent::path_guard::get_session_tmp_dir(session_id);
    let mut touched = diff
        .map(|d| crate::agent::main_loop::build_touched_set(d, &tmp))
        .unwrap_or_default();
    crate::agent::main_loop::extend_touched_with_session_tmp(&mut touched, &tmp);
    if targets.iter().any(|t| {
        touched.iter().any(|p| {
            crate::agent::finish_guard::diff::file_paths_match(
                &crate::agent::main_loop::normalize_tmp_declared(t, &tmp),
                p,
            )
        }) || crate::agent::main_loop::declared_target_touched_on_fs(
            t,
            &tmp,
            workspace,
            started.timestamp_millis(),
        )
    }) {
        return;
    }
    let normalized: Vec<String> = targets
        .iter()
        .map(|t| crate::agent::main_loop::normalize_tmp_declared(t, &tmp))
        .collect();
    let near = crate::agent::main_loop::tmp_target_near_misses(&normalized, &touched);
    touched.sort();
    touched.dedup();
    let total = touched.len();
    touched.truncate(12);
    blockers.push(format!("方案声明目标与实际改动/产物零交集，本次未提交交付。\n声明目标：{}\n已观测改动/产物（共 {} 项，最多显示 12 项）：{}\n{}\n尚未实现请继续实现；实现路径改变请用 submit_plan + revision_reason 说明并重审；确实无需改动时请 ask_human 说明。",
        targets.join(", "), total, touched.join(", "),
        if near.is_empty() { String::new() } else { format!("同名但目录层级不同：{}", near.join("；")) }));
}

#[cfg(test)]
#[path = "evaluate_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "material_change_tests.rs"]
mod material_change_tests;
