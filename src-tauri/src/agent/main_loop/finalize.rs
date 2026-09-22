//! 终态提交：交付载荷构建、产物登记、checkpoint Done + run trace + 会话消息原子提交。
//! 从 run_agent_main_loop_in_workspace_inner 抽出，逻辑不变。

use crate::agent::checkpoint::AgentCheckpoint;
use crate::agent::context::types::SandwichContext;
use crate::agent::reviewer_gatekeeper::ReviewResult;
use crate::agent::types::AgentInstruction;
use crate::db::DbState;
use serde_json::json;

/// finish 确认提交：mark_done → 构建交付载荷（指令 command 优先，收据预填载荷回填）
/// → 规范化本轮 finish 附带的产物路径为受管交付物 → 终态短事务原子提交。
/// 返回结构化 emitted_finish 供前端通知使用。
pub(crate) fn commit_completed_run(
    db: &DbState,
    checkpoint: &mut AgentCheckpoint,
    context: &SandwichContext,
    run_id: &str,
    final_session_id: &str,
    goal: &str,
    run_trace_steps: &[serde_json::Value],
    instruction: &AgentInstruction,
    output_text: &str,
    conversation_model_id: &str,
    reviewer_warnings_delivered: Option<&ReviewResult>,
) -> Result<serde_json::Value, String> {
    checkpoint.mark_done(context);
    // ── 交付载荷在提交前构建：指令 command 优先，收据预填载荷回填 ──
    let finish_payload = instruction
        .command
        .clone()
        .filter(|value| value.is_object())
        .or_else(|| {
            checkpoint
                .pending_finalization
                .as_ref()
                .map(|receipt| receipt.finish_payload.clone())
                .filter(|value| value.is_object())
        })
        .unwrap_or(serde_json::Value::Null);

    let mut persisted_summary = finish_payload
        .get("summary")
        .and_then(|value| value.as_str())
        .or_else(|| {
            finish_payload
                .get("details")
                .and_then(|value| value.as_str())
        })
        .unwrap_or(output_text)
        .to_string();
    // 审查警告必须落在用户可见的持久化摘要里——只塞进 completion_json 结构字段
    // 而聊天消息不展示，等于把"带病交付"静默伪装成"完全成功"。
    if let Some(warnings) = reviewer_warnings_delivered {
        let issue_lines = warnings
            .blocking_issues
            .iter()
            .enumerate()
            .map(|(i, b)| format!("{}. [{}] {}", i + 1, b.file, b.issue))
            .collect::<Vec<_>>()
            .join("\n");
        persisted_summary.push_str(&format!(
            "\n\n---\n\n> ⚠️ **审查警告：经人工仲裁放行，{} 项缺陷未完全收敛**（终审评分 {}/100）：{}\n{}",
            warnings.blocking_issues.len(),
            warnings.score,
            warnings.summary,
            issue_lines
        ));
    }
    // 完成区域展示模型自己的最终回复；若存在审查熔断遗留警告，注入到 emitted_finish
    let mut emitted_finish = match finish_payload {
        serde_json::Value::Object(object) => serde_json::Value::Object(object),
        _ => json!({ "summary": persisted_summary }),
    };
    if let Some(warnings) = reviewer_warnings_delivered {
        if let serde_json::Value::Object(ref mut map) = emitted_finish {
            map.insert(
                "reviewer_warnings".to_string(),
                json!({
                    "score": warnings.score,
                    "summary": warnings.summary,
                    "blocking_issues": warnings.blocking_issues,
                    "nit_issues": warnings.nit_issues,
                }),
            );
        }
    }
    if let Some(ref receipt) = checkpoint.pending_finalization {
        if let serde_json::Value::Object(ref mut map) = emitted_finish {
            map.insert(
                "finalization_receipt".to_string(),
                json!(receipt.receipt_id),
            );
        }
    }

    // 🌟 规范化本轮 finish 交付的产物列表：若本轮模型显式附带了文件路径，自动解析绝对路径并登记受管交付物。
    // 产物登记是提交前的派生写入：路径幂等去重，失败降级为原始条目；
    // 它不在终态事务内（register_workspace_artifact 自带锁，嵌套会死锁），
    // 事务回滚时留下的是可去重的普通产物行，不会污染权威终态。
    if let serde_json::Value::Object(ref mut map) = emitted_finish {
        if let Some(artifacts_val) = map.get_mut("artifacts") {
            if let Some(arr) = artifacts_val.as_array_mut() {
                let mut resolved_artifacts = Vec::new();
                for item in arr.iter() {
                    let raw_candidate = match item {
                        serde_json::Value::String(s) => Some(s.clone()),
                        serde_json::Value::Object(obj) => {
                            obj.get("canonical_path")
                                .or_else(|| obj.get("local_path"))
                                .or_else(|| obj.get("path"))
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        }
                        _ => None,
                    };
                    if let Some(path_str) = raw_candidate {
                        if let Ok(rec) = crate::agent::artifact_manager::register_workspace_artifact(
                            db,
                            final_session_id,
                            Some(run_id),
                            &path_str,
                        ) {
                            resolved_artifacts.push(json!({
                                "artifact_id": rec.id,
                                "name": rec.display_name,
                                "format": rec.format,
                                "kind": rec.kind,
                                "size_bytes": rec.size_bytes,
                                "canonical_path": rec.canonical_path,
                                "local_path": rec.canonical_path,
                            }));
                        } else {
                            resolved_artifacts.push(item.clone());
                        }
                    } else {
                        resolved_artifacts.push(item.clone());
                    }
                }
                *artifacts_val = serde_json::Value::Array(resolved_artifacts);
            }
        }
    }

    // ── 终态短事务：checkpoint Done + run trace succeeded + 会话消息原子提交 ──
    crate::agent::finalization::commit_finalization(
        db,
        checkpoint,
        run_id,
        final_session_id,
        goal,
        run_trace_steps,
        &emitted_finish,
        &persisted_summary,
        conversation_model_id,
    )?;
    Ok(emitted_finish)
}
