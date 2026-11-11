use crate::agent::change_verification::{
    apply_change_verification_update, change_verification_blocker, ChangeVerification,
};
use crate::agent::checkpoint::{
    load_checkpoint, recovery_decision, save_checkpoint, AgentCheckpoint, AgentPhase,
    RecoveryDecision,
};
use crate::agent::context;
use crate::agent::control::{
    finish_interrupted_run, intervention_channels, stream_interrupt_channels,
    take_pending_interrupt, HumanControl,
};
use crate::agent::dispatcher::{is_finish_instruction, is_write_or_side_effect_tool};
use crate::agent::loop_runner::run_call_llm_and_run_tool_phases;
use crate::agent::mcp::{McpClient, PluginRegistry};
use crate::agent::review_contract::{apply_review_contract_update, ReviewContract};
use crate::agent::run_state::{AgentRunEvent, AgentRunMachine, AgentRunState};
use crate::agent::task_store;
use crate::agent::types::{AgentInstruction, DispatchResult, TodoItem, RUN_INTERRUPTED_ROUTE};
use crate::db::DbState;
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, watch};
use tokio::time::{sleep, Duration};

// ---------------------------------------------------------------- 工作区软锁
// 同一项目目录同时只允许一个 run，防止两个任务并发写同一堆文件。
// 仅项目会话参与锁定（含微信 scratch 项目）；纯 GUI 无项目会话保持原有并行行为，
// 评测工作区彼此隔离也不受影响。守卫按 RAII 随 run 结束（含早退/中断）自动释放。

use crate::agent::finish_guard::{
    apply_instruction_state_updates, finish_blockers, finish_blockers_with_diff,
    review_contract_evidence_blockers, should_challenge_finish,
    should_challenge_finish_with_blockers, should_challenge_finish_with_unresolved, DiffContext,
};
use crate::agent::workspace_lock::{acquire_workspace_lock, ChildWorkspaceGuard};

mod auto_verify;
mod entry;
mod finalize;
mod helpers;
mod post_step;
mod precheck;
mod turn;

pub(crate) use auto_verify::*;
pub use entry::*;
pub(crate) use helpers::*;

pub(crate) const DEFAULT_TRANSIENT_LLM_REQUEST_RETRIES: u32 = 3;
const MAX_TRANSIENT_LLM_REQUEST_RETRIES: u32 = 100;
const MAX_CONSECUTIVE_GATE_BLOCKS: u32 = 8;

/// Agent 每个任务最多执行步数（可按项目需求调整）
const MAX_STEPS: usize = 300;

/// 宿主级运行预算：评测考卷可覆盖步数与 prompt token 上限。
/// 任一触发即在步骤边界立即收敛为 failed 终态并出报告，
/// 触发原因写入 completion summary 供判分/复盘区分（步数 vs token）。
#[derive(Clone, Debug, Default)]
pub(crate) struct AgentRunBudget {
    pub max_steps: Option<usize>,
    pub max_prompt_tokens: Option<i64>,
}

pub(crate) async fn run_agent_main_loop_in_workspace_inner(
    app: AppHandle,
    state: State<'_, DbState>,
    registry_state: State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    mut model_routing: crate::agent::router::ModelRoutingConfig,
    goal: String,
    auto_pilot: bool,
    final_session_id: String,
    log_context: Option<bool>,
    transient_llm_request_retries: u32,
    attachments: Option<Vec<crate::commands::chat::ParsedAttachmentResult>>,
    workspace_override: Option<std::path::PathBuf>,
    run_id: String,
    plan_mode: Option<String>,
    run_budget: Option<AgentRunBudget>,
    task_start_mode: Option<crate::agent::conversation::TaskStartMode>,
    delegation: Option<crate::agent::child_task::TaskBinding>,
) -> Result<(), String> {
    // 立即注册人类干预与流式中断接收器，确保任务启动第一毫秒即可被前端即时打断
    let (tx, mut rx) = mpsc::channel::<HumanControl>(5);
    let (interrupt_tx, interrupt_rx) = watch::channel(0u64);
    {
        let mut channels = intervention_channels().lock().await;
        channels.insert(final_session_id.clone(), tx);
    }
    stream_interrupt_channels()
        .lock()
        .await
        .insert(final_session_id.clone(), interrupt_tx);

    // 🐕 会话停滞看门狗：主循环每轮刷新原子心跳；若超过阈值仍无新步且会话
    // 仍 running，说明任务在 await 死锁/panic 逃逸（实弹已见三场僵尸会话：
    // 进程活着、线程全闲、updated_at 停摆、内部时限无法自救）。看门狗把会话
    // 落为 failed 终态并记录最后相位，止损 + 留证，而非永久假装存活。
    // 阈值取 900s：高于单步最长合法耗时（call_llm 看门狗上限 660s + 派发余量），
    // 不误伤慢模型。
    let stall_probe = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    ));
    {
        let probe = stall_probe.clone();
        let w_app = app.clone();
        let w_db = crate::db::DbState(state.inner().0.clone());
        let w_session = final_session_id.clone();
        let w_run = run_id.clone();
        let w_goal = goal.clone();
        tauri::async_runtime::spawn(async move {
            // 阈值 env 可调：评测短周期迭代用小阈值快速暴露挂点，
            // 生产默认 900s 高于单步最长合法耗时（LLM 看门狗 660s + 派发余量）。
            let stall_threshold_secs: u64 = std::env::var("FREE_API_STALL_SECS")
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .filter(|v| *v >= 60)
                .unwrap_or(900);
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let status: Option<String> = w_db.0.lock().ok().and_then(|conn| {
                    conn.query_row(
                        "SELECT status FROM agent_run_traces WHERE id=?1",
                        rusqlite::params![w_run],
                        |r| r.get(0),
                    )
                    .ok()
                });
                match status.as_deref() {
                    Some("running") | Some("waiting_human") => {}
                    // 已到终态（含无行）：看门狗使命结束
                    _ => break,
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let idle = now.saturating_sub(probe.load(std::sync::atomic::Ordering::Relaxed));
                if idle <= stall_threshold_secs || crate::agent::child_task::is_delegating(&w_session) || crate::agent::child_task::runtime::paused(&w_run).is_some() {
                    continue;
                }
                let saved = crate::agent::checkpoint::load_checkpoint(&w_db, &w_run)
                    .ok()
                    .flatten()
                    .or_else(|| {
                        crate::agent::checkpoint::load_checkpoint(&w_db, &w_session)
                            .ok()
                            .flatten()
                    });
                // 人类还在看审批卡不是程序死锁，不能按无心跳超时终止。
                if saved.as_ref().is_some_and(|cp| cp.phase == crate::agent::checkpoint::AgentPhase::WaitUser) {
                    continue;
                }
                let phase = saved.map(|cp| format!("{:?}", cp.phase))
                    .unwrap_or_else(|| "unknown".to_string());
                let stage = crate::agent::control::read_stage(&w_session)
                    .unwrap_or_else(|| "unknown".to_string());
                let reason = format!(
                    "会话看门狗：主循环已停滞 {}s（相位={}, 阶段={}），判定任务挂死并安全终止",
                    idle, phase, stage
                );
                let _ = crate::agent::run_trace::upsert_snapshot(
                    &w_db,
                    &w_run,
                    &w_session,
                    &w_goal,
                    "failed",
                    &[],
                    Some(&serde_json::json!({ "summary": reason })),
                );
                let _ = w_app.emit(
                    &format!("agent-log-{}", w_session),
                    format!("⏱️ {}", reason),
                );
                intervention_channels().lock().await.remove(&w_session);
                stream_interrupt_channels().lock().await.remove(&w_session);
                break;
            }
        });
    }

    let run_trace_steps: Vec<serde_json::Value> =
        crate::agent::run_trace::load_run_steps(state.inner(), &run_id).unwrap_or_default();
    let mut run_machine = AgentRunMachine::new();
    advance_run_state(
        &app,
        &final_session_id,
        &mut run_machine,
        AgentRunEvent::Start,
    )?;
    if let Err(error) = crate::agent::run_trace::upsert_snapshot(
        state.inner(),
        &run_id,
        &final_session_id,
        &goal,
        "running",
        &run_trace_steps,
        None,
    ) {
        eprintln!("[run-trace] 无法创建运行轨迹: {error}");
    }
    // 行表回填：恢复的历史步（来自 steps_json 列的旧格式）补进 agent_run_steps，
    // 使行表自启动起即为完整事实源；INSERT OR REPLACE 幂等，崩溃重跑安全。
    for (i, step) in run_trace_steps.iter().enumerate() {
        let _ = crate::agent::run_trace::append_step(state.inner(), &run_id, i as i64, step);
    }
    let (project_workspace, project_resources) = if workspace_override.is_none() {
        let conn = state.0.lock().map_err(|error| error.to_string())?;
        let ws =
            crate::commands::projects::project_workspace_for_session(&conn, &final_session_id)?;
        let res = if ws.is_some() {
            crate::commands::projects::project_resources_for_session(&conn, &final_session_id)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        (ws, res)
    } else {
        (None, Vec::new())
    };
    let is_project_session = project_workspace.is_some();
    let has_workspace_override = workspace_override.is_some();
    let is_explicit_workspace = is_project_session || has_workspace_override;
    // Skill、插件与 shell 工具共享本轮运行时工作目录；恢复时会重新读取并按哈希复用未变更的 Skill。
    let workspace_dir =
        resolve_workspace_dir(workspace_override, project_workspace, &final_session_id);
    if !workspace_dir.is_dir() {
        return Err(format!("Agent 工作区不可用：{}", workspace_dir.display()));
    }
    // 运行起始时间：成功完成时自动登记运行期间新生成的工作区产物（微信回推文件链路）
    let run_started_at = chrono::Utc::now();
    // 捕获任务起点 Git 基准快照（用于 finish 时进行精准增量 Diff 审查）。
    {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        if delegation.is_none() {
            crate::commands::projects::directories::bind_session_directories(
                &conn,
                &final_session_id,
                &workspace_dir,
                is_project_session,
            )?;
        }
    }
    // 自由会话（未选项目）的 workspace_dir 是会话产物目录而非真实工程——
    // 证据域缺失 → 降级为非 git 基线，审查改靠副作用台账/产物链。
    // 续跑时优先恢复原任务基线，稍后在 checkpoint 选择后登记。
    // 项目会话进入前先拿工作区软锁（独占该目录直到本次 run 结束）
    let _workspace_run_lock = if is_project_session {
        Some(acquire_workspace_lock(&workspace_dir, &final_session_id, &run_id, &goal).await?)
    } else {
        None
    };
    // P5 副本生命周期：父 run 终态时自动统一回收所有子 writer 的隔离副本
    let _child_workspaces_guard = ChildWorkspaceGuard {
        db: state.inner(),
        run_id: &run_id,
    };
    // Resolve task identity and frozen review routing before building the tool/role context.
    let existing_checkpoint = load_checkpoint(state.inner(), &final_session_id)?;
    let is_resuming_existing = !crate::agent::conversation::should_start_new_turn(
        existing_checkpoint.as_ref(), task_start_mode,
    )?;
    if is_resuming_existing {
        if let Some(snapshot) = existing_checkpoint.as_ref().and_then(|cp| cp.task_review_routing.as_ref()) {
            model_routing.stage_reviewer_override = snapshot.stage_reviewer_override.clone();
            model_routing.stage_review_committee = snapshot.stage_review_committee;
        }
    }
    let auto_pilot = if is_resuming_existing {
        existing_checkpoint.as_ref().map_or(auto_pilot, |cp| cp.auto_pilot)
    } else { auto_pilot };

    // 1. 智能 MCP 插件同步：首次任务全量初始化，后续任务仅增量保活（见 mcp/session_bootstrap.rs）
    crate::agent::mcp::session_bootstrap::sync_mcp_plugins_for_session(
        &app,
        state.inner(),
        registry_state.inner(),
        &workspace_dir,
        &final_session_id,
    )
    .await?;

    // 2. 初始化三明治上下文：加载系统提示词 + 可视化/HTML沙箱 Skill + 动态注入 MCP 工具
    let _ = app.emit(
        &format!("agent-progress-{}", final_session_id),
        json!({
            "type": "action_phase",
            "phase": "skills",
            "description": "📚 正在装配系统提示词与 Skill 缓存..."
        }),
    );
    let initial_execution_config = crate::agent::execution::runtime::config(state.inner(), &run_id)?;
    let initial_review = crate::agent::child_reviewer::main_executor::load(state.inner(), &run_id)?;
    let base_prompt = crate::agent::prompt_assembly::load_base_prompt(&app);

    // 2. 稳定的内置工具契约首轮常驻；长范例与外部 MCP schema 仍按需加载。
    let local_tools_menu = if initial_review.is_some() { String::new() }
        else if initial_execution_config.allowed_tools.is_some() { crate::agent::task_context::visibility::restricted_menu(&initial_execution_config) }
        else { crate::agent::prompt_assembly::build_local_tools_menu() };
    // 角色池清单：主 Agent 由此知道池子里有哪些专精角色可派发（自动过滤当前会话休眠的角色卡）
    let role_cards_menu = crate::agent::tools::role_manager::build_role_cards_menu_filtered(
        state.inner(),
        &model_routing.disabled_role_cards,
    )
    .unwrap_or_default();

    // 3. 从当前工作区向上读取 AGENTS.md / HUB.md；未显式选定项目/工作区时保持为空。
    let workspace_skill_context = if is_explicit_workspace {
        crate::agent::skill_context::workspace_skill_context(&workspace_dir)
    } else {
        String::new()
    };
    // 4. 用户在能力管理页维护的条目只作为补充上下文；读取失败不能阻断正常 Agent 运行。
    let custom_instruction_context = crate::commands::load_enabled_custom_instruction_context(
        state.inner(),
    )
    .unwrap_or_else(|error| {
        eprintln!("加载自定义提示词与 Skill 失败: {}", error);
        String::new()
    });
    let conversation_model_id = if !model_routing.pro.trim().is_empty() {
        model_routing.pro.clone()
    } else if !model_routing.flash.trim().is_empty() {
        model_routing.flash.clone()
    } else {
        model_routing.vision.clone()
    };
    let model_cfg_res = crate::llm::ModelConfig::from_db(&state, &conversation_model_id);
    let maybe_model_cfg = model_cfg_res.as_ref().ok();

    // 5. 查询所有 MCP Server 的工具列表并提前预热/升级当前会话的智能能力卡
    let (mcp_tools_menu, compact_tools_index, review_mcp_menu) = {
        let registry = registry_state.lock().await;
        registry
            .prime_tool_catalog_for_session(&final_session_id, maybe_model_cfg)
            .await;
        registry.prewarm_all_for_session(&final_session_id);

        // Progressive disclosure：工具总量超阈值时不再平铺全部工具名
        // （每轮决策指令的固定开销），降级为"插件名(数量)"，细节走
        // M0 工具目录与 next_tool_hint 按需取回。
        const MCP_INDEX_NAME_LIMIT: usize = 40;
        let mut mcp_lines = Vec::new();
        let mut total_tool_count = 0usize;
        let mut per_plugin: Vec<(String, Vec<String>)> = Vec::new();
        for (p_name, p_client) in &registry.clients {
            let tools = crate::agent::mcp_catalog::cached_tools(&p_client.config)
                .unwrap_or_else(|| p_client.last_known_tools());
            let names = tools
                .iter()
                .filter(|t| initial_review.is_none() && crate::agent::task_context::visibility::tool(&initial_execution_config,&format!("{}/{}",p_name,t.name)))
                .map(|t| format!("{}/{}", p_name, t.name))
                .collect::<Vec<_>>();
            total_tool_count += names.len();
            if !names.is_empty() {
                per_plugin.push((p_name.clone(), names));
            }
        }
        // 确定性排序：HashMap 遍历顺序跨进程不稳定，决策指令每轮注入，
        // 乱序会破坏跨会话的前缀缓存复用与输出可复现性。
        per_plugin.sort_by(|a, b| a.0.cmp(&b.0));
        for (_, names) in &mut per_plugin {
            names.sort();
        }
        if total_tool_count > MCP_INDEX_NAME_LIMIT {
            // 塌缩行必须带能力摘要——只有 "插件名(N)" 时模型不知道它是干嘛的，
            // 就不会去 capability_search 查名字，等于工具存在但语义隐形。
            for (p_name, names) in &per_plugin {
                let card_hint = registry
                    .clients
                    .get(p_name)
                    .and_then(|c| crate::agent::mcp_catalog::cached_capability_card(&c.config))
                    .map(|c| capability_card_hint(&c))
                    .unwrap_or_default();
                if card_hint.is_empty() {
                    mcp_lines.push(format!("  * {}({} 个工具)", p_name, names.len()));
                } else {
                    mcp_lines.push(format!(
                        "  * {}({} 个工具): {}",
                        p_name,
                        names.len(),
                        card_hint
                    ));
                }
            }
            mcp_lines.push(
                "  * 不知确切工具名时先用 capability_search(query) 检索全部插件工具".to_string(),
            );
        } else {
            for (p_name, names) in &per_plugin {
                mcp_lines.push(format!("  * {}: {}", p_name, names.join(", ")));
            }
        }
        let mcp_part = if mcp_lines.is_empty() {
            "  * （本次未列出可用 MCP 工具；可能未连接或受权限限制）".to_string()
        } else {
            mcp_lines.join("\n")
        };
        let builtin_list = if let Some(review) = &initial_review {
            crate::agent::task_context::visibility::reviewer_index(&initial_execution_config,
                crate::agent::review_tools::forensic_budget(), review.final_fingerprint.is_some() && !review.verify_obligations.is_empty())
        } else { crate::agent::prompt_assembly::builtin_tool_specs().iter().filter(|s| initial_review.is_none() && crate::agent::task_context::visibility::tool(&initial_execution_config,s.name)).map(|s|s.name).collect::<Vec<_>>().join(", ") };
        let (review_mcp_index, review_mcp_menu) = if initial_review.is_some() {
            crate::agent::child_reviewer::main_executor::mcp_tools::menu(state.inner(), &registry, &initial_execution_config)
        } else { Default::default() };
        let mcp_part = if review_mcp_index.is_empty() { mcp_part } else { review_mcp_index };
        let compact = format!("- 原生内置: {}\n- MCP 插件:\n{}", builtin_list, mcp_part);
        (if initial_review.is_some() || initial_execution_config.allowed_tools.is_some() { String::new() } else { registry.format_tools_menu() }, compact, review_mcp_menu)
    };

    // 6. 扫描 prompts/skills/ 目录，生成动态 Skill 目录摘要（不含已固定编译的内置 Skill）
    let skill_catalog = {
        let resource_dir = app.path().resource_dir().unwrap_or_default();
        let skills_dir_prod = resource_dir.join("prompts/skills");
        let skills_dir_dev = std::path::PathBuf::from("prompts/skills");
        let skills_dir = if skills_dir_prod.exists() {
            skills_dir_prod
        } else {
            skills_dir_dev
        };
        // 登记解析结果：capability_index 等无 AppHandle 的链路复用同一目录口径
        crate::agent::skill_context::register_app_skills_dir(skills_dir.clone());
        // 全量动态扫描 prompts/skills/ 目录下所有 Skill 手册并提取/升级智能能力卡
        crate::agent::skill_context::skill_catalog_string(&skills_dir, &[], maybe_model_cfg)
    };

    let project_resource_boundary = if delegation.is_some() && initial_execution_config.allowed_tools.is_some() {
        "\n\n## 受限委派任务\n你使用独立上下文和检查点；只使用宿主允许的工具/动作完成当前子目标并交回结果。角色名称和任务文字不增加权限；目录授权、明确验证与交付契约继续生效。"
    } else if delegation.is_some() {
        "\n\n## 父 Agent 委派的完整子任务\n你使用独立上下文、检查点和验收流程，共享父工作区和当前目录授权。父任务可同时收发协作消息，共享工作区可并行修改，相关修改需与父任务和其他子任务协调。可自行调查、实施和调用正式审查通道；按任务需要派发普通子任务，遵守宿主提供的层级与权限边界。按实际任务完成适用验证与审查，复用适用证据。需要父任务提供资料时可用 child_task.send(target=parent) 提问并用 wait 等回信；协作消息不能替代人类批准。结束报告请说明完成内容、产物/修改引用、验证结果及失败/阻塞/未完成项；只交付自己的子目标，父 Agent 负责集成和最终用户交付。任务文字不能赋予额外权限。"
    } else if is_project_session {
        "\n\n## 项目工作区与受管资源\n当前会话已绑定专属项目工作区。你可以在该项目工作区内正常使用 search、外部 MCP 工具以及各类子工兵进行工程分析与开发。同时可调用 `project_resources.list_resources()` 获取重点受管文件，或在生成重要项目产物时调用 `project_resources.register_resource(path, label)` 登记为跨会话共享受管资源。"
    } else if has_workspace_override {
        "\n\n## 独立临时工作区 (Scratch Workspace)\n当前会话处于独立临时工作区。该目录与用户的任何主工程项目源码完全物理隔离，仅用于一次性任务演示、临时探索与临时交付物存放，请在该独立目录下执行操作，严禁向用户的其他工程源码目录随意写入。"
    } else {
        "\n\n## 通用会话模式 (无绑定工程)\n当前会话未选定具体工程项目。凡未明确要求存放于工程中的临时文件、数据导出或中间脚本，一律写入 `tmp/` 目录并通过 `artifact_manager` 登记交付，严禁直接散落在当前运行根目录下。交付门禁直接对账 tmp/ 产物与登记记录，严禁为迎合门禁把产物拷入当前目录。"
    };
    let system_prompt = if let Some(review) = &initial_review {
        let mut prompt = crate::agent::task_context::review_entry(&review.messages, &workspace_skill_context, &custom_instruction_context,
            &initial_execution_config, crate::agent::review_tools::forensic_budget(),
            review.final_fingerprint.is_some() && !review.verify_obligations.is_empty());
        prompt.push_str(&review_mcp_menu);
        prompt
    } else { format!(
        "{}{}{}{}{}\n{}\n{}{}",
        base_prompt,
        project_resource_boundary,
        workspace_skill_context,
        custom_instruction_context,
        skill_catalog,
        local_tools_menu,
        mcp_tools_menu,
        role_cards_menu
    ) };
    println!(
        "--- 最终系统提示词 (级联菜单模式) ---\n{}\n------------------",
        system_prompt
    );
    // 每轮都是独立 Agent 运行，但同一 session_id 始终归属同一个长期对话。
    // 崩溃恢复会沿用既有 checkpoint，不能重复追加用户消息；只有新轮次才写入消息历史。
    let resume_from_failed = is_resuming_existing
        && existing_checkpoint
            .as_ref()
            .is_some_and(|cp| cp.phase == AgentPhase::Failed);
    if !is_resuming_existing {
        crate::agent::conversation::begin_turn(
            state.inner(),
            &final_session_id,
            &conversation_model_id,
            &goal,
        )?;
    } else {
        // 续接未完成 checkpoint 时本条用户消息仍是新发言，须落盘成气泡——
        // 否则桌面/网页会话页静默吞掉这句话（用户发了却看不到）。
        // 恢复路径由新 goal 驱动上下文，此处纯记账不改语义。
        if let Err(error) = crate::agent::conversation::append_human_intervention(
            state.inner(),
            &final_session_id,
            &goal,
        ) {
            eprintln!("[conversation] 续接轮次用户消息落盘失败: {error}");
        }
    }
    let mut context = context::SandwichContext::new(system_prompt, goal.clone());
    let session_tmp_dir = crate::agent::path_guard::get_session_tmp_dir(&final_session_id);

    // 绑定主文件系统作用域（仅显式项目会话或评测自动驾驶模式声明物理工作区，默认会话保持工作区为空）
    if is_explicit_workspace || auto_pilot {
        context.resource_scope =
            context::resource_scope::ResourceScopeContract::bind_primary_filesystem(
                &workspace_dir.to_string_lossy(),
                context::resource_scope::ResourceScopeSource::SessionSelected,
            );
    }

    // 可操作文件边界契约：内容会话入口一次定型 → 挂 M1 静态前缀吃缓存
    // （此前放尾部状态卡，~1.9KB 静态散文每步全价重发，且 Minimal 档整段
    // 丢失——门禁级契约不应随档位裁剪）。尾部只留一行速记指针。
    let mut manifest_text = build_path_scope_manifest(
        &workspace_dir,
        &session_tmp_dir,
        is_explicit_workspace || auto_pilot,
    );

    if is_project_session {
        if project_resources.is_empty() {
            manifest_text.push_str("- 当前项目暂无登记的受管数据文件。当在工作区生成重要产物或分析报告时，可调用 `project_resources.register_resource(path, label)` 登记为跨会话共享资源。\n");
        } else {
            // 资源清单有界：登记量大时尾部状态卡不逐条全列（该卡每轮重发），
            // 完整清单经 `project_resources.list_resources()` 按需分页取回。
            const MAX_MANIFEST_RESOURCES: usize = 20;
            manifest_text.push_str(&format!(
                "- 本项目已登记受管数据文件（共 {} 个，已建立资源句柄可直接读取分析{}）：\n",
                project_resources.len(),
                if project_resources.len() > MAX_MANIFEST_RESOURCES {
                    "，下列仅前 20 个"
                } else {
                    ""
                }
            ));
            for r in project_resources.iter().take(MAX_MANIFEST_RESOURCES) {
                let size_kb = (r.size_bytes as f64) / 1024.0;
                manifest_text.push_str(&format!(
                    "  - [{}] `{}` (resource_id: `{}`, 大小: {:.1} KB, 标签: \"{}\")\n",
                    r.kind.to_uppercase(),
                    r.relative_path,
                    r.resource_id,
                    size_kb,
                    r.label
                ));
            }
            if project_resources.len() > MAX_MANIFEST_RESOURCES {
                manifest_text.push_str(&format!("  - …另有 {} 个未列出，调用 `project_resources.list_resources(offset?, limit?)` 分页取回完整清单。\n", project_resources.len() - MAX_MANIFEST_RESOURCES));
            }
            manifest_text.push_str("- 提示：调用 `project_resources.read_resource(resource_id)` 可直接读取上述文件内容；在工作区发现或生成新核心交付物时可调用 `project_resources.register_resource(path, label)` 登记为跨会话共享资源。\n");
        }
    } else if has_workspace_override {
        manifest_text.push_str("- 提示：本会话为独立任务工作区，所有代码与操作严格限定在该目录下，严禁向用户的其他工程项目源码目录写入。\n");
    }

    context.project_resource_manifest = Some(manifest_text);
    crate::agent::workspace_access::sync_context(&final_session_id, &workspace_dir, &mut context);
    context.path_scope_pointer = Some(if is_explicit_workspace || auto_pilot {
        format!(
            "【路径速记】项目工作区 `{}`；临时产物/草稿一律 `tmp/`；附加目录及权限见开头当前项目配置段；受控文件工具越界访问会被 path_guard 拦截。",
            workspace_dir.display()
        )
    } else {
        "【路径速记】本会话无项目工作区；临时产物/草稿一律 `tmp/`；越界访问会被 path_guard 拦截。完整契约见会话开头静态段。"
            .to_string()
    });
    context.compact_tools_index = Some(compact_tools_index);
    // 注入当前路由模型的上下文窗口大小与压缩配置，用于历史压缩水位动态计算与双层智能归档。
    // 双模型路由下同一段历史会分别发给推理与执行模型，预算必须按两者窗口的
    // 较小者计算，否则执行模型会先于推理模型触碰爆窗边界。
    if let Ok(cfg) = &model_cfg_res {
        let mut effective_window = cfg.context_window as usize;
        if !model_routing.flash.trim().is_empty() {
            if let Ok(flash_cfg) = crate::llm::ModelConfig::from_db(&state, &model_routing.flash) {
                effective_window = effective_window.min(flash_cfg.context_window as usize);
            }
        }
        context.context_window_tokens = effective_window;
        context.compaction_model_config = Some(cfg.clone());
    } else {
        context.context_window_tokens = 32_768;
    }
    context.budget_config = match model_routing.context_budget_mode.as_str() {
        "cost_saving" => crate::agent::context::budget::ContextBudgetConfig::cost_saving(),
        "ultra_fast" => crate::agent::context::budget::ContextBudgetConfig::ultra_fast(),
        _ => crate::agent::context::budget::ContextBudgetConfig::deep_coding(),
    };
    // 状态卡携带量独立覆盖：显式指定时压过预算模式自带的档位
    match model_routing.state_card_level.as_str() {
        "off" => {
            context.budget_config.state_card_level =
                crate::agent::context::budget::StateCardLevel::Off
        }
        "minimal" => {
            context.budget_config.state_card_level =
                crate::agent::context::budget::StateCardLevel::Minimal
        }
        "full" => {
            context.budget_config.state_card_level =
                crate::agent::context::budget::StateCardLevel::Full
        }
        _ => {}
    }
    let entry_history = crate::agent::conversation::history_for_entry(
        state.inner(), &final_session_id, existing_checkpoint.as_ref(),
    )?;
    let pending_live_goals: Vec<String> = if is_resuming_existing {
        entry_history.messages.iter().filter(|m| m.role == "user")
            .filter_map(|m| m.content.as_str().map(str::to_owned)).collect()
    } else { Vec::new() };
    context.turns_history = entry_history.messages;
    context.conversation_cursor = entry_history.cursor;
    // 审查任务在首次模型调用前注入轻量项目地图。它只包含目录、入口和 Git 元数据，
    // 不读取源码正文；恢复中的运行沿用 checkpoint，避免反复追加同一张地图。
    if !is_resuming_existing && is_explicit_workspace {
        if let Some(review_map) = crate::agent::project_map::review_bootstrap(&goal, &workspace_dir)
        {
            context.turns_history.push(context::types::ChatMessage {
                role: "user".to_string(),
                content: json!(review_map),
            });
        }
    }
    context.sync_fetched_tools_from_history();
    // 检查点负责任务状态；显式续跑也可恢复 Failed，不能把“继续”作为新题重新分流。
    let mut checkpoint = match existing_checkpoint {
        Some(existing) if is_resuming_existing => {
            let mut cp = existing;
            if cp.phase == AgentPhase::Failed {
                cp.retire_stopped_stage_wait(state.inner())?;
            }
            cp.restore_task_for_entry(&mut context, &goal);
            cp
        }
        Some(existing) => {
            // 同一 session 上一轮已 finish/终态后，新用户消息仍继承脱水夹心与状态索引（Claude/Codex 会话内前缀追加）。
            existing.data.inherit_filling_into(&mut context);
            let mut fresh = AgentCheckpoint::new(final_session_id.clone(), &context);
            fresh.version = existing.version;
            fresh
        }
        None => AgentCheckpoint::new(final_session_id.clone(), &context),
    };
    if delegation.is_some() && !is_resuming_existing {
        crate::agent::execution::runtime::bind_registered_requirements(state.inner(),&run_id,&mut context)?;
    }
    let execution_config=crate::agent::execution::runtime::config(state.inner(),&run_id)?;
    if !is_resuming_existing {
        checkpoint.stages = if delegation.is_some() {
            execution_config.stages.for_child(execution_config.workflow.completion.as_deref()==Some("handoff"), execution_config.workflow.plan_mode.as_deref())
        } else { model_routing.stages.clone() };
    }
    let legacy_review_routing = checkpoint.bind_task_review_routing(is_resuming_existing, &mut model_routing);
    if legacy_review_routing {
        let _ = app.emit(&format!("agent-log-{}", final_session_id),
            "⚠️ 旧任务未保存审查人数和旧审查覆盖的原始配置；本次沿用当前设置并记录，无法还原此前值。执行模式和固定环节仍沿用原检查点。");
    }
    if is_resuming_existing {
        checkpoint.stages.validate(Some(checkpoint.plan_mode.as_str()))?;
    } else {
        let mode = crate::agent::checkpoint::PlanExecutionMode::from_str_or_auto_pilot(
            plan_mode.as_deref(), auto_pilot,
        );
        checkpoint.stages.validate_for_new_task(Some(mode.as_str()))?;
    }
    let review_program = crate::agent::child_reviewer::main_executor::load(state.inner(), &run_id)?;
    context.add_host_feedback(&if review_program.is_some() {
        "【当前审查职责】只读取证并交回审查结果；不能实施修改或再次委派。父任务流程配置见审查材料，内部防递归配置不改变父任务设置。".to_string()
    } else { checkpoint.stages.prompt() });
    if delegation.is_some() && review_program.is_none() {
        let depth = crate::agent::child_task::delegation_depth(&final_session_id)?;
        context.add_host_feedback(&format!("【普通任务委派层级】主 Agent 为第 0 层；当前为第 {depth} 层，上限为第 {} 层。达到上限后不可继续派普通子任务，但可自行工作、回传和使用既有正式审查。", crate::agent::child_task::MAX_DELEGATION_DEPTH));
    }

    if review_program.is_some() && checkpoint.triage_verdict.is_none() {
        let mut verdict=crate::agent::triage::TriageVerdict::fallback("宿主已明确委派只读审查，不重新分流");
        verdict.route="direct".into();verdict.stage_review_mode="none".into();verdict.final_review_mode="mechanical_only".into();
        checkpoint.triage_verdict=Some(verdict);
    }
    if let Some(tools)=execution_config.allowed_tools {
        context.add_host_feedback(&format!("【当前宿主执行配置】本子任务的业务工具仅允许：{}；控制与交付入口仍按宿主流程处理。",tools.join(", ")));
    }
    if execution_config.workflow.completion.as_deref()==Some("handoff") && checkpoint.stages.final_review != Some(true) {
        context.add_host_feedback("【当前宿主流程】本轮 finish 交回子任务结果；父任务负责集成及整体正式交付。已明确的验证要求和有效阻塞仍须处理；不要把本轮交接称为已独立终审的完整交付。");
    }
    for message in pending_live_goals {
        crate::agent::conversation::append_live_goal(&mut context, &message);
        checkpoint.pending_finalization = None;
    }
    if let Some(reference) = entry_history.legacy_reference {
        context.turns_history.insert(0, reference);
        context.maybe_compact_history();
        checkpoint.refresh_context(&context);
    }

    // 保存原任务审查起点，避免续跑前的改动被新基线当作用户原有改动排除。
    let git_baseline = checkpoint.task_git_baseline.clone().unwrap_or_else(|| {
        if is_resuming_existing {
            crate::agent::reviewer_gatekeeper::GitBaseline::git_for_session(&final_session_id)
                // 旧存档没有起点，不能用本次 pre_dirty 隐藏已发生的任务写入。
                .unwrap_or_else(|| {
                    let _ = app.emit(&format!("agent-log-{}", final_session_id),
                        "⚠️ 旧检查点未保存原始 diff 基线，按当前 HEAD 收集审查材料，可能包含任务开始前的未提交改动；请核对归属。");
                    crate::agent::reviewer_gatekeeper::GitBaseline::capture(&workspace_dir)
                })
        } else {
            if is_explicit_workspace || auto_pilot {
                crate::agent::reviewer_gatekeeper::GitBaseline::capture_at_run_start(&workspace_dir)
            } else {
                crate::agent::reviewer_gatekeeper::GitBaseline::non_git()
            }
        }
    });
    checkpoint.task_git_baseline = Some(git_baseline.clone());
    if !is_resuming_existing {
        checkpoint.task_additional_git_baselines =
            crate::agent::reviewer_gatekeeper::GitBaseline::capture_additional_git_baselines(
                &final_session_id,
                &workspace_dir,
            );
    }
    if !is_resuming_existing && !git_baseline.pre_dirty.is_empty() {
        context.add_error_feedback(&git_baseline.origin_notice(&workspace_dir));
    }
    let task_started_at = *checkpoint.task_started_at.get_or_insert_with(|| {
        if is_resuming_existing {
            chrono::DateTime::parse_from_rfc3339(&checkpoint.created_at)
                .map(|t| t.with_timezone(&chrono::Utc))
                .unwrap_or(run_started_at)
        } else {
            run_started_at
        }
    });
    git_baseline.register_for_session(&final_session_id);
    if is_resuming_existing {
        let message = "↪️ 继续原任务：已恢复目标、方案、契约和审查要求；模型配置使用本次选择。";
        let _ = app.emit(&format!("agent-log-{}", final_session_id), message);
    }

    // 🧭 任务入口 LLM 分流（Triage）：新 turn 起步时由 Pro 档做一次结构化复杂度判决，
    // 判决落 checkpoint 后各门禁按 route 分流；恢复中的 run 沿用既有判决，不重复烧 Token。
    // 任何失败都返回保守 fallback（等价维持旧行为），机械安全地板不被判决推翻。
    if !is_resuming_existing && checkpoint.triage_verdict.is_none() {
        let reviewer_names: Vec<String> =
            crate::agent::tools::role_manager::list_cards(state.inner(), false)
                .unwrap_or_default()
                .into_iter()
                .filter(|c| {
                    c.allows_kind("reviewer")
                        && !crate::agent::tools::role_manager::is_card_disabled_for(
                            &model_routing.disabled_role_cards,
                            &c.name,
                            "reviewer",
                        )
                })
                .map(|c| c.name)
                .collect();
        let scope_manifest = context
            .project_resource_manifest
            .clone()
            .unwrap_or_default();
        // Skill 路由输入 1：紧凑触发清单（name + when_to_use/摘要），
        // 不含手册正文——triage 只负责"选哪个"，加载由宿主执行。
        // 文件目录 ∪ DB 自定义 Skill 统一入索引，同名 DB 优先。
        let skill_index_text = crate::agent::skill_context::skill_routing_index_with_db(
            state.inner(),
            &crate::agent::skill_context::resolve_skills_dir(),
        );
        // Skill 路由输入 2：有界的最近对话背景——"上一轮宣称修复 + 本轮用户
        // 反馈无效"这类证据只有带着历史才能看见，只传末尾几条并截断防膨胀。
        let recent_context = {
            let tail: Vec<String> = context
                .turns_history
                .iter()
                .rev()
                .take(4)
                .filter_map(|m| {
                    m.content.as_str().map(|s| {
                        let clipped: String = s.chars().take(800).collect();
                        format!("{}: {}", m.role, clipped)
                    })
                })
                .collect();
            tail.into_iter().rev().collect::<Vec<_>>().join("\n---\n")
        };
        let mut verdict = crate::agent::triage::run_triage(
            &app,
            state.inner(),
            &model_routing,
            &goal,
            &scope_manifest,
            &reviewer_names,
            &skill_index_text,
            &recent_context,
            &final_session_id,
            interrupt_rx.clone(),
        )
        .await;
        // Skill 路由执行：宿主按判决自动预载手册（与 next_tool_hint 同一
        // Write-Once 事件 + fetched_tools 去重口径），执行模型不再需要
        // "想起"主动请求——合规性从模型自觉移到宿主保证。
        if !verdict.skill_recommendations.is_empty() {
            let skills_dir = crate::agent::skill_context::resolve_skills_dir();
            crate::agent::tool_manual::apply_triage_skill_recommendations(
                &mut context,
                &mut verdict,
                &skills_dir,
                state.inner(),
            );
            if !verdict.loaded_skills.is_empty() || !verdict.skill_load_failures.is_empty() {
                let _ = app.emit(
                    &format!("agent-log-{}", final_session_id),
                    format!(
                        "📚 Skill 路由：已加载 [{}]{}",
                        verdict.loaded_skills.join(", "),
                        if verdict.skill_load_failures.is_empty() {
                            String::new()
                        } else {
                            format!("（未命中: {}）", verdict.skill_load_failures.join(", "))
                        }
                    ),
                );
            }
        }
        let telemetry_route = if verdict.fallback.is_some() {
            "triage:fallback".to_string()
        } else {
            format!("triage:{}", verdict.route)
        };
        crate::agent::telemetry::record_gate_fire(
            state.inner(),
            &crate::agent::telemetry::GateFireRecord {
                run_id: &run_id,
                session_id: &final_session_id,
                step_id: 0,
                gate: "triage",
                route: &telemetry_route,
            },
        );
        let log_line = if let Some(ref fb) = verdict.fallback {
            format!(
                "🧭 任务分流未完成: {}（原因: {}；字段使用兼容默认，不代表模型判决）",
                verdict.reason, fb
            )
        } else {
            format!("🧭 任务分流: {} — {}", verdict.route, verdict.reason)
        };
        let _ = app.emit(&format!("agent-log-{}", final_session_id), log_line);
        // 需求歧义判决：提示模型先 ask_human 澄清，澄清前不要写文件（仅注入提示，不改门禁）
        if verdict.needs_user_clarification() {
            context.turns_history.push(crate::agent::context::ChatMessage {
                role: "user".to_string(),
                content: json!(format!(
                    "【🧭 任务分流判决：需求存在歧义，须先澄清】\n{}\n【必须执行的操作】：请先调用 ask_human 向用户澄清上述疑点；澄清结论返回前不要写入任何文件。",
                    verdict.reason
                )),
            });
        }
        // 入口调查参考：供执行模型定位，后续证据可修正；非方案路由不注入。
        if verdict.requires_plan_gate() && checkpoint.stages.plan != Some(false) {
            if let Some(brief) = verdict.investigation_brief_text() {
                let investigation_hint = if delegation.is_some() {
                    "本轮由当前子任务调查并定稿；可按任务需要在宿主层级与权限范围内派发普通子任务。正式审查与同树消息通信仍可使用。"
                } else {
                    "先按任务需要调查环境和材料。范围明确、可独立交回证据的问题可派给 worker，主任务核对证据并定稿；无需为分工而派发。普通角色可在共享工作区并行，相关任务由主 Agent 协调；逐项核对实际受理回执，结果通过 wait_jobs 或信箱回收。创意类可返工的歧义可在方案中写明假设；证据成熟后 submit_plan，沿用现有方案审查流程。"
                };
                context.turns_history.push(crate::agent::context::ChatMessage {
                    role: "user".to_string(),
                    content: json!(format!(
                        "【🧭 入口调查参考（入口分流给出）】\n{}\n{}\n【审查方式随方案选择】submit_plan 可填写 final_review_mode（mechanical_only|artifact_quality|code_review|operational_verification|panel）。视频/动画/PPT/页面/报表等成品用 artifact_quality（终审员可对视频抽帧、对页面/PPT 截图核对内容）。跨模块测试和运行验证可选 operational_verification，核对实际命令、回执和未覆盖项。入口已选择机械校验的简单任务可沿用 mechanical_only。自动阶段审查沿用入口选择。若调查发现有需要提前复核的风险，可用 stage_review_mode=milestone 补开。用户固定设置优先，放宽已选的独立终审、多人终审或 milestone 由人工决定。\n【非代码产物（视频/PPT/表格/报告等）的方案要求】产出文件时 target_files 填真实产出路径；仅对话报告、只读调查或测试任务可填 []；方案须写明产物规格、分步清单、每步可核对的验证方式与关键假设；验证以真实打开/渲染/检查产物内容为准，不要只确认文件存在。",
                        brief, investigation_hint
                    )),
                });
            }
        }
        // 方案路由保留独立终审，阶段审查沿用入口分流的独立选择。
        verdict.apply_plan_route_final_review_default();
        checkpoint.triage_verdict = Some(verdict);
        // 审查覆盖面观测行：分流判决如何塑形阶段/终审一目了然；判决关闭阶段审查
        // 时额外落 gate 留痕（step_id 此处未就位，与上方 triage 记录一致用 0）。
        if let Some(v) = checkpoint.triage_verdict.as_ref() {
            let _ = app.emit(
                &format!("agent-log-{}", final_session_id),
                format!(
                    "🧭 分流判决审查覆盖面：stage_review={} | final_review={} | final_roles={}",
                    v.stage_review_mode,
                    v.final_review_mode,
                    if v.final_review_roles.is_empty() {
                        "default".to_string()
                    } else {
                        v.final_review_roles.join(",")
                    }
                ),
            );
            if !v.needs_stage_review() {
                crate::agent::telemetry::record_gate_fire(
                    state.inner(),
                    &crate::agent::telemetry::GateFireRecord {
                        run_id: &run_id,
                        session_id: &final_session_id,
                        step_id: 0,
                        gate: "triage",
                        route: "triage:stage_review_disabled",
                    },
                );
            }
        }
        save_checkpoint(state.inner(), &mut checkpoint)?;
    }

    // 模式切换守卫：按目标执行模式（全自动/交叉审查/方案审批）规范化方案阶段；人工确认过的方案永久保留。
    // 同任务续跑保留原执行模式；仅新任务应用入口配置。
    checkpoint.apply_entry_execution_mode(is_resuming_existing, plan_mode.as_deref(), auto_pilot);
    checkpoint.stages.validate(Some(checkpoint.plan_mode.as_str()))?;
    // 初始工作区是会话选择的已声明资源；恢复时保留 checkpoint 中已有的显式契约。
    // 仅在显式项目会话、存在 workspace_override 或 auto_pilot 模式下才声明主工作区。
    // 在未选项目的通用自由会话中，主工作区保持为 None（自由模式），避免将宿主机启动目录误作为资源约束强塞给 Agent 及外部 MCP。
    if (is_explicit_workspace || auto_pilot)
        && context.resource_scope.primary_filesystem().is_none()
    {
        let validation = if workspace_dir.is_dir() {
            context::resource_scope::ResourceScopeValidation::Valid
        } else {
            context::resource_scope::ResourceScopeValidation::Invalid
        };
        let source = if auto_pilot {
            context::resource_scope::ResourceScopeSource::EvaluationConfig
        } else {
            context::resource_scope::ResourceScopeSource::SessionSelected
        };
        context.resource_scope.bind_filesystem_with_validation(
            context::resource_scope::ResourceScopeRole::Primary,
            &workspace_dir.to_string_lossy(),
            source,
            validation,
        );
    }

    crate::agent::workspace_access::sync_context(&final_session_id, &workspace_dir, &mut context);

    // 🌟 注入本轮用户传入的附件 (图片 Base64 多模态 / 分层轻量挂载文档与代码)
    let mut injected_image_paths = std::collections::HashSet::new();
    let mut inlined_attachment_bytes = 0usize;

    // 1. 处理显式传递的 attachments 附件列表
    if let Some(ref atts) = attachments {
        for att in atts {
            let is_image = att.content_type.starts_with("image/")
                || att.file_name.ends_with(".jpg")
                || att.file_name.ends_with(".jpeg")
                || att.file_name.ends_with(".png")
                || att.file_name.ends_with(".webp")
                || att.file_name.ends_with(".gif")
                || att.file_name.ends_with(".bmp")
                || att.file_path.ends_with(".jpg")
                || att.file_path.ends_with(".jpeg")
                || att.file_path.ends_with(".png")
                || att.file_path.ends_with(".webp");

            if is_image {
                if let Some(ref data_url) = att.image_data_url {
                    if !data_url.is_empty() {
                        crate::agent::attachment_vision::add_attachment_image(
                            &mut context, &att.file_name, data_url, &final_session_id,
                        );
                        injected_image_paths.insert(att.file_path.clone());
                        continue;
                    }
                }
                if let Ok(bytes) = std::fs::read(&att.file_path) {
                    use base64::Engine;
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    let mime = if att.content_type.is_empty() || att.content_type == "file" {
                        if att.file_name.ends_with(".png") || att.file_path.ends_with(".png") {
                            "image/png"
                        } else if att.file_name.ends_with(".webp")
                            || att.file_path.ends_with(".webp")
                        {
                            "image/webp"
                        } else {
                            "image/jpeg"
                        }
                    } else {
                        &att.content_type
                    };
                    let data_url = format!("data:{};base64,{}", mime, b64);
                    crate::agent::attachment_vision::add_attachment_image(
                        &mut context, &att.file_name, &data_url, &final_session_id,
                    );
                    injected_image_paths.insert(att.file_path.clone());
                }
            } else {
                // 📂 非图片附件：统一存放至当前会话的 tmp/ 临时沙箱并执行分层挂载（绝不污染项目工作区代码仓库）
                let src_path = std::path::Path::new(&att.file_path);
                let tmp_dir = crate::agent::path_guard::get_session_tmp_dir(&final_session_id);
                let _ = std::fs::create_dir_all(&tmp_dir);
                let target_file = tmp_dir.join(&att.file_name);
                if src_path.exists() && src_path != target_file {
                    let _ = std::fs::copy(src_path, &target_file);
                } else if !target_file.exists() && !att.prompt_text.is_empty() {
                    let _ = std::fs::write(&target_file, att.prompt_text.as_bytes());
                }
                let sandbox_path_str = format!("tmp/{}", att.file_name);
                let actual_file_path = target_file;

                let can_inline = inlined_attachment_bytes + (att.size_bytes as usize)
                    <= crate::agent::file_parser::INLINE_TOTAL_BUDGET;
                let parsed_content = if actual_file_path.exists() {
                    crate::agent::file_parser::parse_file(&actual_file_path, None).ok()
                } else if src_path.exists() {
                    crate::agent::file_parser::parse_file(src_path, None).ok()
                } else {
                    None
                };

                let prompt_text = if let Some(ref content) = parsed_content {
                    let (rendered, newly_inlined) =
                        crate::agent::file_parser::format_for_prompt_with_sandbox(
                            content,
                            &att.file_name,
                            Some(&sandbox_path_str),
                            can_inline,
                        );
                    inlined_attachment_bytes += newly_inlined;
                    rendered
                } else if !att.prompt_text.is_empty() {
                    att.prompt_text.clone()
                } else {
                    format!(
                        "📎【用户已挂载本地文件】`{}` (沙箱路径: `{}`)\n",
                        att.file_name, sandbox_path_str
                    )
                };

                if !prompt_text.is_empty() {
                    context
                        .turns_history
                        .push(crate::agent::context::ChatMessage {
                            role: "user".to_string(),
                            content: serde_json::json!(prompt_text),
                        });
                }
            }
        }
    }

    // 2. 兜底扫描：若 goal 文本中带有 📎 [附件: ... | 路径: ...] 标记，自动读取本地图片或文档解析注入
    if goal.contains("📎") || goal.contains("路径:") {
        for segment in goal.split("📎") {
            if let Some(att_start) = segment.find("[附件:") {
                let rest = &segment[att_start + "[附件:".len()..];
                if let Some(end_bracket) = rest.find(']') {
                    let att_body = &rest[..end_bracket];
                    let mut file_name = "file";
                    let mut file_path = "";
                    if let Some((name_part, path_part)) = att_body.split_once("路径:") {
                        file_path = path_part.trim();
                        let name_candidate = name_part.trim();
                        if let Some((name_only, _)) = name_candidate.split_once('(') {
                            file_name = name_only.trim();
                        } else if let Some((name_only, _)) = name_candidate.split_once('|') {
                            file_name = name_only.trim();
                        } else {
                            file_name = name_candidate;
                        }
                    }
                    if !file_path.is_empty() && !injected_image_paths.contains(file_path) {
                        let p = std::path::Path::new(file_path);
                        if p.exists() {
                            let is_image = file_name.ends_with(".jpg")
                                || file_name.ends_with(".jpeg")
                                || file_name.ends_with(".png")
                                || file_name.ends_with(".webp")
                                || file_name.ends_with(".gif")
                                || file_name.ends_with(".bmp")
                                || file_path.ends_with(".jpg")
                                || file_path.ends_with(".jpeg")
                                || file_path.ends_with(".png")
                                || file_path.ends_with(".webp");

                            if is_image {
                                if let Ok(bytes) = std::fs::read(p) {
                                    use base64::Engine;
                                    let b64 =
                                        base64::engine::general_purpose::STANDARD.encode(&bytes);
                                    let mime = if file_name.ends_with(".png")
                                        || file_path.ends_with(".png")
                                    {
                                        "image/png"
                                    } else if file_name.ends_with(".webp")
                                        || file_path.ends_with(".webp")
                                    {
                                        "image/webp"
                                    } else {
                                        "image/jpeg"
                                    };
                                    let data_url = format!("data:{};base64,{}", mime, b64);
                                    crate::agent::attachment_vision::add_attachment_image(
                                        &mut context, file_name, &data_url, &final_session_id,
                                    );
                                    injected_image_paths.insert(file_path.to_string());
                                }
                            } else if let Ok(extracted) =
                                crate::agent::file_parser::parse_file(p, None)
                            {
                                // 同样自动拷贝到会话 tmp/ 沙箱目录并生成分层提示词！
                                let tmp_dir = crate::agent::path_guard::get_session_tmp_dir(
                                    &final_session_id,
                                );
                                let _ = std::fs::create_dir_all(&tmp_dir);
                                let target_file = tmp_dir.join(file_name);
                                if p != target_file {
                                    let _ = std::fs::copy(p, &target_file);
                                }
                                let sandbox_path_str = format!("tmp/{}", file_name);
                                let can_inline = inlined_attachment_bytes
                                    <= crate::agent::file_parser::INLINE_TOTAL_BUDGET;
                                let (prompt_text, newly_inlined) =
                                    crate::agent::file_parser::format_for_prompt_with_sandbox(
                                        &extracted,
                                        file_name,
                                        Some(&sandbox_path_str),
                                        can_inline,
                                    );
                                inlined_attachment_bytes += newly_inlined;
                                context
                                    .turns_history
                                    .push(crate::agent::context::ChatMessage {
                                        role: "user".to_string(),
                                        content: serde_json::json!(prompt_text),
                                    });
                            }
                        }
                    }
                }
            }
        }
    }

    // 数据区本体是权威来源；每次启动都刷新目录，checkpoint 仅保留这份目录快照。
    context.set_task_resources(task_store::list_resource_refs(
        state.inner(),
        &final_session_id,
    )?);
    checkpoint.refresh_context(&context);
    save_checkpoint(state.inner(), &mut checkpoint)?;
    // 仅在从 phase=RunTool 且尚无 pending 的 checkpoint 恢复时使用。
    let mut recovered_instruction: Option<AgentInstruction> = None;
    let action_history: std::collections::VecDeque<String> = std::collections::VecDeque::new();

    advance_run_state(
        &app,
        &final_session_id,
        &mut run_machine,
        AgentRunEvent::RuntimeReady,
    )?;
    // WaitUser 与 pending 调用不能从普通上下文可靠反推；恢复时显式读取 checkpoint。
    match recovery_decision(&checkpoint) {
        RecoveryDecision::ResumePlanning | RecoveryDecision::Terminal => {}
        RecoveryDecision::ResumeTool => {
            recovered_instruction = checkpoint.prepared_instruction.clone();
            if recovered_instruction.is_none() {
                return Err("checkpoint phase=run_tool 但缺少 prepared instruction".to_string());
            }
            // 方案已落账且自动审批已登记：不要重放已执行的提交；交给自动等待/审批接续。
            if checkpoint.auto_plan_approval_pending
                && recovered_instruction.as_ref().is_some_and(|i| i.submit_plan.is_some()) {
                recovered_instruction = None;
            }
        }
        RecoveryDecision::ResumeWaiting if checkpoint.auto_plan_approval_pending
            && checkpoint.prepared_instruction.as_ref().is_some_and(|i| i.get_tool().eq_ignore_ascii_case("ask_human")) => {
            // 恢复已展示的审批卡：复用原 ask_human 表单与结构化按钮处理。
            recovered_instruction = checkpoint.prepared_instruction.clone();
        }
        RecoveryDecision::ResumeWaiting
        | RecoveryDecision::RequireHumanReview
        | RecoveryDecision::RetryPending
        | RecoveryDecision::ProbePending => {
            let reason = match checkpoint.pending.as_ref() {
                Some(pending) => format!(
                    "系统从 checkpoint 恢复：工具 {} / 动作 {} 的结果不确定（call_id: {}，已尝试 {} 次）。为避免重复副作用，请确认是否继续。",
                    pending.tool, pending.action, pending.call_id, pending.attempts
                ),
                None => checkpoint
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "系统恢复后正在等待人类确认。".to_string()),
            };

            checkpoint.wait_for_human(reason.clone(), &context);
            save_checkpoint(state.inner(), &mut checkpoint)?;
            advance_run_state(
                &app,
                &final_session_id,
                &mut run_machine,
                AgentRunEvent::InstructionReady {
                    action: "checkpoint_recovery".to_string(),
                },
            )?;
            advance_run_state(
                &app,
                &final_session_id,
                &mut run_machine,
                AgentRunEvent::WaitForHuman {
                    question: reason.clone(),
                },
            )?;
            app.emit(
                &format!("agent-progress-{}", final_session_id),
                json!({ "type": "agent_paused", "message": reason }),
            )
            .map_err(|error| error.to_string())?;

            let reply = match rx.recv().await {
                Some(HumanControl::Answer(reply, cursor)) => {
                    context.acknowledge_chat(cursor);
                    reply
                },
                Some(HumanControl::ReplaceGoal(next_goal)) => {
                    let reason = format!("用户提交了新的任务目标：{}", next_goal);
                    finish_interrupted_run(
                        &app,
                        state.inner(),
                        &final_session_id,
                        &run_id,
                        &goal,
                        &run_trace_steps,
                        &mut checkpoint,
                        &context,
                        &mut run_machine,
                        reason,
                    )
                    .await?;
                    return Ok(());
                }
                Some(HumanControl::Cancel) => {
                    finish_interrupted_run(
                        &app,
                        state.inner(),
                        &final_session_id,
                        &run_id,
                        &goal,
                        &run_trace_steps,
                        &mut checkpoint,
                        &context,
                        &mut run_machine,
                        "用户请求停止当前任务。".to_string(),
                    )
                    .await?;
                    return Ok(());
                }
                None => "人类未回复，通道异常关闭".to_string(),
            };
            context.add_error_feedback(&format!("【checkpoint 恢复后的人工确认】{}", reply));
            checkpoint.begin_planning(checkpoint.step_id, &context);
            save_checkpoint(state.inner(), &mut checkpoint)?;
            advance_run_state(
                &app,
                &final_session_id,
                &mut run_machine,
                AgentRunEvent::HumanReplyReceived,
            )?;
        }
    }

    // 🧹 会话启动时，自动收敛历史由于断电或异常退出的孤儿后台任务与子审查任务
    let _ = crate::agent::job_engine::recover_orphaned_jobs_on_session_start(
        state.inner(),
        &final_session_id,
    );
    let _ = crate::agent::child_reviewer::recover_orphaned_child_runs_on_session_start(
        state.inner(),
        &final_session_id,
    );

    // durable cursor 是调度权威：每个普通步骤提交后推进 step_id；恢复时也从这里继续。
    // 不再使用启动瞬间捕获范围的 for-loop，避免 checkpoint 更新后调度器仍沿旧游标运行。
    // 失败后的显式续跑获得本次运行的有限预算，但保留任务步骤游标，不能立即再次触顶。
    let max_steps =
        effective_retry_step_limit(run_budget.as_ref(), checkpoint.step_id, resume_from_failed);
    let spent_prompt_tokens: i64 = 0;
    let budget_stop_reason: Option<String> = None;
    let Some(execution) = crate::agent::execution::run(
        Box::new(turn::MainExecution {
            app,
            state,
            registry_state,
            model_routing,
            goal,
            auto_pilot,
            final_session_id,
            log_context,
            transient_llm_request_retries,
            run_id: run_id.clone(),
            plan_mode,
            run_budget,
            rx,
            interrupt_rx,
            stall_probe,
            run_trace_steps,
            run_machine,
            is_explicit_workspace,
            workspace_dir,
            run_started_at,
            conversation_model_id,
            context,
            checkpoint,
            git_baseline,
            task_started_at,
            recovered_instruction,
            action_history,
            max_steps,
            spent_prompt_tokens,
            budget_stop_reason,
        }),
        turn::turn_main,
    )
    .await?
    else {
        return Ok(());
    };
    let turn::MainExecution {
        app,
        state,
        final_session_id,
        run_id,
        goal,
        run_trace_steps,
        mut run_machine,
        mut checkpoint,
        context,
        max_steps,
        budget_stop_reason,
        ..
    } = *execution;

    if run_machine.state() == AgentRunState::Finalizing {
        advance_run_state(
            &app,
            &final_session_id,
            &mut run_machine,
            AgentRunEvent::Succeed {
                summary: "任务已完成".to_string(),
            },
        )?;
    } else if !run_machine.state().is_terminal() {
        let terminal_error = budget_stop_reason
            .unwrap_or_else(|| format!("任务在达到最大步数 {} 后未完成", max_steps));
        crate::agent::failure::finish_failed_run(
            &app,
            state.inner(),
            &final_session_id,
            &run_id,
            &goal,
            &run_trace_steps,
            &mut checkpoint,
            &context,
            &mut run_machine,
            terminal_error.clone(),
        )
        .await?;
        return Err(terminal_error);
    }
    {
        let mut channels = intervention_channels().lock().await;
        channels.remove(&final_session_id);
        stream_interrupt_channels()
            .lock()
            .await
            .remove(&final_session_id);
    }

    Ok(())
}

/// 能力卡首行摘要（塌缩索引行尾展示用）——按字符截断，绝不按字节切
/// （多字节 UTF-8 落在字符中间会 panic）。
fn capability_card_hint(card: &str) -> String {
    let first_line = card.lines().next().unwrap_or("").trim().to_string();
    let truncated: String = first_line.chars().take(60).collect();
    if truncated.len() < first_line.len() {
        format!("{}…", truncated)
    } else {
        truncated
    }
}

#[cfg(test)]
mod budget_tests;

#[cfg(test)]
mod gate_deadlock_tests;

#[cfg(test)]
mod artifact_bridge_tests;

#[cfg(test)]
mod path_scope_manifest_tests;

#[cfg(test)]
mod stack_tests;

#[cfg(test)]
mod capability_hint_tests {
    #[test]
    fn chinese_card_truncates_without_panic() {
        let card = "视频音频剪辑（ffmpeg 系）：裁剪/拼接/转场/字幕/混轨/变速/去静音等 27 工具，重复填充：这是一段很长的能力卡描述用来测试多字节字符截断边界不会炸。";
        let h = super::capability_card_hint(card);
        assert!(h.ends_with('…'));
        assert!(h.chars().count() <= 61);
    }

    #[test]
    fn short_card_passes_through() {
        assert_eq!(super::capability_card_hint("短视频工具"), "短视频工具");
        assert_eq!(super::capability_card_hint(""), "");
    }
}
