//! 通用本地工具臂：spawn_blocking 执行、可中断等待与结果观测注入。

use crate::agent::checkpoint::{save_checkpoint, AgentCheckpoint};
use crate::agent::child_reviewer::result_as_dispatch_output;
use crate::agent::context::resource_scope::ResourceOperation;
use crate::agent::control::{take_pending_interrupt, HumanControl};
use crate::agent::dispatcher::{
    emit_tool_event, is_finish_instruction, is_write_or_side_effect_tool, run_agent_step_with_images,
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
    // 用户可能在 LLM 请求等待期间更新目录授权，执行前刷新作用域而不改审查状态。
    crate::agent::workspace_access::sync_context(ctx.final_session_id, ctx.workspace_dir, ctx.context);
    Ok(ArmOutcome::Done({
        let is_write_tool = is_write_or_side_effect_tool(inst);

        // 敏感访问门：命令连数据库或把凭证/密钥文件内容读进上下文——暂不执行，告诉模型“这条路不走”，
        // 确需执行则用 ask_human 问主人（复用 git 破坏命令的“指纹核销”确认链路：原样引用命令 + 明确同意→重发放行）。
        // 只覆盖顶层 local_shell（parallel 分支不经过此处）。
        if inst.get_tool() == "local_shell" {
            if let Some(cmd_str) = crate::agent::dispatcher::extract_shell_command_string(inst) {
                if let Some(reason) = crate::agent::dispatcher::sensitive_shell_access_review(&cmd_str) {
                    let fp = crate::agent::dispatcher::git_cmd_fingerprint(&cmd_str);
                    if !ctx.checkpoint.is_git_destructive_confirmed(&fp) {
                        ctx.checkpoint.block_git_destructive(&fp);
                        let _ = save_checkpoint(ctx.state.inner(), ctx.checkpoint);
                        let stderr = format!(
                            "【敏感访问待确认】{reason}，已暂停，未执行。\n这条路不该自己走：凭证会明文进入上下文和日志，连库也可能碰到真实数据。\n建议改用不需要凭证的验证方式（编译/语法检查、单元测试、静态读代码）；无法验证的部分在报告里如实标「未验证」即可。\n确需执行：用 ask_human 询问主人，message 中原样包含完整命令 `{fp}`；主人明确同意（确认/可以/批准/执行）后原样重发本命令即可；拒绝或未表态均不放行。"
                        );
                        emit_tool_event(
                            ctx.app,
                            ctx.final_session_id,
                            ctx.step_id,
                            inst,
                            "failed",
                            Some("sensitive_access_needs_confirm".to_string()),
                            Some(false),
                            &stderr,
                            Some(ctx.tool_started_at.elapsed().as_millis()),
                            None,
                        );
                        return Ok(ArmOutcome::Early(DispatchResult {
                            view: None,
                            success: false,
                            stdout: String::new(),
                            stderr,
                            route: "sensitive_access_needs_confirm".to_string(),
                        }));
                    }
                }
            }
        }
        if is_write_tool {

            // git 工作区破坏命令对账门（实测事故 2026-09-25：终审歧义指令被执行为
            // `git checkout --` 销毁他人未提交工作，且全程未进副作用台账）：
            //   · 目标全在本 session 写入台账/计划核准集 → 自清理放行；
            //   · 无路径形态（reset --hard/clean/push -f…）或含外来目标 → 挂起等
            //     人工确认；ask_human 原文引用命令指纹 + 非拒绝回复 → 核销重发。
            if let Some(cmd_str) =
                crate::agent::dispatcher::extract_shell_command_string(inst)
            {
                use crate::agent::dispatcher::{
                    git_cmd_fingerprint, git_destructive_review, GitDestructiveVerdict,
                };
                let verdict = git_destructive_review(&cmd_str);
                if verdict != GitDestructiveVerdict::NotDestructive {
                    let fp = git_cmd_fingerprint(&cmd_str);
                    let mut foreign_reason: Option<String> = None;
                    match &verdict {
                        // 变更型但不销毁（stash pop/apply）：入账即可，不请人
                        GitDestructiveVerdict::SideEffect => {}
                        GitDestructiveVerdict::Pathless => {
                            foreign_reason = Some(
                                "无显式路径目标，影响面无法按台账界定（整树/历史/远端）"
                                    .to_string(),
                            );
                        }
                        GitDestructiveVerdict::Targets { paths, dirty_gated } => {
                            let foreign = foreign_git_targets(
                                paths,
                                *dirty_gated,
                                inst,
                                ctx.context,
                                ctx.checkpoint,
                                ctx.workspace_dir,
                            );
                            if !foreign.is_empty() {
                                foreign_reason = Some(format!(
                                    "目标不在本 session 写入台账或计划核准集: {}",
                                    foreign.join(", ")
                                ));
                            }
                        }
                        GitDestructiveVerdict::NotDestructive => {}
                    }
                    if let Some(reason) = foreign_reason {
                        if !ctx.checkpoint.is_git_destructive_confirmed(&fp) {
                            ctx.checkpoint.block_git_destructive(&fp);
                            let _ = save_checkpoint(ctx.state.inner(), ctx.checkpoint);
                            let stderr = format!(
                                "【git 破坏命令待人工确认】`{fp}` —— {reason}，执行将销毁\
                                 非本 session 产生的未提交工作。\n确需执行：用 ask_human \
                                 询问主人，message 中原样包含完整命令 `{fp}`；主人回复须明确\
                                 同意（确认/可以/批准/执行），拒绝或未表态均不放行；放行后\
                                 原样重发本命令即可。主人拒绝或失联\
                                 → 改用非破坏路径（先复制备份再处理，或放弃该操作）。",
                            );
                            emit_tool_event(
                                ctx.app,
                                ctx.final_session_id,
                                ctx.step_id,
                                inst,
                                "failed",
                                Some("git_destructive_needs_confirm".to_string()),
                                Some(false),
                                &stderr,
                                Some(ctx.tool_started_at.elapsed().as_millis()),
                                None,
                            );
                            return Ok(ArmOutcome::Early(DispatchResult { view: None,
                                success: false,
                                stdout: String::new(),
                                stderr,
                                route: "git_destructive_needs_confirm".to_string(),
                            }));
                        }
                    }
                }
            }

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
        let run_id = ctx.parent_run_id.to_string();
        let ws_root = ctx.workspace_dir.to_path_buf();
        let registry = {
            let guard = registry_arc.lock().await;
            (*guard).clone()
        };

        let pending_images = crate::agent::tools::view_image::PendingImageQueue::default();
        let tool_images = pending_images.clone();
        let work_group = crate::agent::work_history::current_group();
        let mut tool_task = tokio::task::spawn_blocking(move || {
            println!("执行动作: {:?}", inst_c);
            crate::agent::work_history::in_blocking_group(work_group, ||
                run_agent_step_with_images(&db, &sid, &inst_c, &registry, Some(&ws_root), &tool_images, Some(&run_id)))
        });

        let mut dispatch_result = tokio::select! {
            res = &mut tool_task => {
                res.map_err(|e| e.to_string())?
            }
            Some(control) = ctx.rx.recv() => {
                let interrupt_reason = match control {
                    HumanControl::Cancel => "用户请求停止当前任务。".to_string(),
                    HumanControl::ReplaceGoal(goal) => format!("用户提交了新的任务目标：{}（已中断前序工具）", goal),
                    HumanControl::Answer(..) => "用户请求中断当前任务。".to_string(),
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
        crate::agent::tools::view_image::transfer_pending_images(
            &pending_images, &mut ctx.context.pending_view_images,
        )?;
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

/// git 破坏命令的路径对账：返回「不在本 session 写入台账且不在计划核准集」
/// 的目标列表。归一化口径：命令目标按指令 cwd（缺省 workspace_dir）解析为
/// 词法绝对路径；台账路径（非绝对时按 workspace_dir 解析）同规后精确匹配。
/// dirty_gated=true（checkout/restore 形态）时，只把「脏=有未提交内容」的
/// 目标算外来——干净已提交文件上还原是 no-op，无可销毁物，不该烦主人。
/// 目标为目录前缀不展开判定（`.`/`*` 已在 Pathless 分流）——界外宁多判为
/// foreign 触发人工确认，也不放宽到前缀匹配（fail 方向 = 移交而非放行）。
fn foreign_git_targets(
    targets: &[String],
    dirty_gated: bool,
    inst: &AgentInstruction,
    context: &crate::agent::context::types::SandwichContext,
    checkpoint: &crate::agent::checkpoint::AgentCheckpoint,
    workspace_dir: &std::path::Path,
) -> Vec<String> {
    use crate::agent::context::resource_scope::lexically_normalize;
    use std::path::Path;

    let normalize_with = |raw: &str, base: &Path| -> String {
        let p = Path::new(raw);
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            base.join(p)
        };
        lexically_normalize(&joined).to_string_lossy().to_string()
    };

    // 命令的 cwd 基准（指令显式 cwd 优先，相对 cwd 以 workspace_dir 为基）
    let cwd_base = inst
        .get_params()
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(|cwd| {
            let p = Path::new(cwd);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                workspace_dir.join(p)
            }
        })
        .unwrap_or_else(|| workspace_dir.to_path_buf());

    // session 写入台账（含本 session 前几轮 run 的继承写入——同会话内
    // 「自己写的可以删」跨 run 成立；跨 session 的外来文件不在其中，
    // 照常触发人工确认）+ 计划核准目标集
    let mut allowed: Vec<String> = context
        .side_effects
        .iter()
        .flat_map(|e| e.affected_paths.iter().map(|p| normalize_with(p, workspace_dir)))
        .collect();
    if let Some(approved) = &checkpoint.approved_target_files {
        allowed.extend(
            approved
                .iter()
                .map(|p| normalize_with(p, workspace_dir)),
        );
    }

    // dirty_gated：只对「有未提交内容可销毁」的目标算外来。porcelain 输出为
    // 仓库根相对路径，归一化基准 = git toplevel（拿不到则退化为 cwd_base 侧，
    // 仍比「不查脏」精确）。
    let dirty_abs: std::collections::BTreeSet<String> = if dirty_gated {
        let repo_root = std::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(&cwd_base)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| cwd_base.clone());
        crate::agent::review_tools::git_dirty_file_paths(&cwd_base)
            .unwrap_or_default()
            .iter()
            .map(|p| normalize_with(p, &repo_root))
            .collect()
    } else {
        std::collections::BTreeSet::new()
    };

    targets
        .iter()
        .filter(|t| {
            let norm = normalize_with(t, &cwd_base);
            if allowed.iter().any(|a| a == &norm) {
                return false; // 本 session 写的/计划核准的 = 自清理
            }
            // checkout/restore 类：干净已提交文件上还原是 no-op，不算外来
            if dirty_gated && !dirty_abs.contains(&norm) {
                return false;
            }
            true
        })
        .cloned()
        .collect()
}
