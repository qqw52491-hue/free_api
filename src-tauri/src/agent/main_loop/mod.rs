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
use crate::agent::evaluation_analysis::{assess_complex_delivery_summary, summary_makes_root_cause_claim};
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
    apply_instruction_state_updates, complex_delivery_blocker, finish_blockers,
    finish_blockers_with_diff, review_contract_evidence_blockers, DiffContext,
    should_challenge_finish, should_challenge_finish_with_blockers,
    should_challenge_finish_with_complex_delivery_blocker,
};
use crate::agent::workspace_lock::{acquire_workspace_lock, ChildWorkspaceGuard};

mod auto_verify;
mod post_step;
mod precheck;
mod finalize;
mod entry;
mod helpers;

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

async fn run_agent_main_loop_in_workspace_inner(
    app: AppHandle,
    state: State<'_, DbState>,
    registry_state: State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    model_routing: crate::agent::router::ModelRoutingConfig,
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
) -> Result<(), String> {
    // 立即注册人类干预与流式中断接收器，确保任务启动第一毫秒即可被前端即时打断
    let (tx, mut rx) = mpsc::channel::<HumanControl>(5);
    let (interrupt_tx, mut interrupt_rx) = watch::channel(0u64);
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
                let status: Option<String> = w_db
                    .0
                    .lock()
                    .ok()
                    .and_then(|conn| {
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
                if idle <= stall_threshold_secs {
                    continue;
                }
                let phase = crate::agent::checkpoint::load_checkpoint(&w_db, &w_run)
                    .ok()
                    .flatten()
                    .or_else(|| {
                        crate::agent::checkpoint::load_checkpoint(&w_db, &w_session)
                            .ok()
                            .flatten()
                    })
                    .map(|cp| format!("{:?}", cp.phase))
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

    let mut run_trace_steps: Vec<serde_json::Value> =
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
        let _ = crate::agent::run_trace::append_step(
            state.inner(),
            &run_id,
            i as i64,
            step,
        );
    }
    let (project_workspace, project_resources) = if workspace_override.is_none() {
        let conn = state.0.lock().map_err(|error| error.to_string())?;
        let ws = crate::commands::projects::project_workspace_for_session(&conn, &final_session_id)?;
        let res = if ws.is_some() {
            crate::commands::projects::project_resources_for_session(&conn, &final_session_id).unwrap_or_default()
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
    // 自由会话（未选项目）的 workspace_dir 是会话产物目录而非真实工程——
    // 证据域缺失 → 降级为非 git 基线，审查改靠副作用台账/产物链。
    let git_baseline = if is_explicit_workspace || auto_pilot {
        crate::agent::reviewer_gatekeeper::GitBaseline::capture(&workspace_dir)
    } else {
        crate::agent::reviewer_gatekeeper::GitBaseline::non_git()
    };
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
    let base_prompt = crate::agent::prompt_assembly::load_base_prompt(&app);

    // 2. 稳定的内置工具契约首轮常驻；长范例与外部 MCP schema 仍按需加载。
    let local_tools_menu = crate::agent::prompt_assembly::build_local_tools_menu();
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
    let (mcp_tools_menu, compact_tools_index) = {
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
            for (p_name, names) in &per_plugin {
                mcp_lines.push(format!("  * {}({} 个工具)", p_name, names.len()));
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
            "  * （当前未连接任何外部 MCP 插件）".to_string()
        } else {
            mcp_lines.join("\n")
        };
        let builtin_list = crate::agent::prompt_assembly::build_builtin_tools_compact_list();
        let compact = format!(
            "- 原生内置: {}\n- MCP 插件:\n{}",
            builtin_list,
            mcp_part
        );
        (registry.format_tools_menu(), compact)
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

    let project_resource_boundary = if is_project_session {
        "\n\n## 项目工作区与受管资源\n当前会话已绑定专属项目工作区。你可以在该项目工作区内正常使用 search、外部 MCP 工具以及各类子工兵进行工程分析与开发。同时可调用 `project_resources.list_resources()` 获取重点受管文件，或在生成重要项目产物时调用 `project_resources.register_resource(path, label)` 登记为跨会话共享受管资源。"
    } else if has_workspace_override {
        "\n\n## 独立临时工作区 (Scratch Workspace)\n当前会话处于独立临时工作区。该目录与用户的任何主工程项目源码完全物理隔离，仅用于一次性任务演示、临时探索与临时交付物存放，请在该独立目录下执行操作，严禁向用户的其他工程源码目录随意写入。"
    } else {
        "\n\n## 通用会话模式 (无绑定工程)\n当前会话未选定具体工程项目。凡未明确要求存放于工程中的临时文件、数据导出或中间脚本，一律写入 `tmp/` 目录并通过 `artifact_manager` 登记交付，严禁直接散落在当前运行根目录下。交付门禁直接对账 tmp/ 产物与登记记录，严禁为迎合门禁把产物拷入当前目录。"
    };
    let system_prompt = format!(
        "{}{}{}{}{}\n{}\n{}{}",
        base_prompt,
        project_resource_boundary,
        workspace_skill_context,
        custom_instruction_context,
        skill_catalog,
        local_tools_menu,
        mcp_tools_menu,
        role_cards_menu
    );
    println!(
        "--- 最终系统提示词 (级联菜单模式) ---\n{}\n------------------",
        system_prompt
    );
    // 每轮都是独立 Agent 运行，但同一 session_id 始终归属同一个长期对话。
    // 崩溃恢复会沿用既有 checkpoint，不能重复追加用户消息；只有新轮次才写入消息历史。
    let existing_checkpoint = load_checkpoint(state.inner(), &final_session_id)?;
    let is_resuming_existing =
        !crate::agent::conversation::should_start_new_turn(existing_checkpoint.as_ref());
    if !is_resuming_existing {
        crate::agent::conversation::begin_turn(
            state.inner(),
            &final_session_id,
            &conversation_model_id,
            &goal,
        )?;
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

    // 动态可操作文件边界卡片挂载在底层面包（M_Tail），每轮伴随状态卡投射，绝不污染 M0 顶层长前缀缓存
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
            manifest_text.push_str(&format!("- 本项目已登记受管数据文件（共 {} 个，已建立资源句柄可直接读取分析{}）：\n",
                project_resources.len(),
                if project_resources.len() > MAX_MANIFEST_RESOURCES { "，下列仅前 20 个" } else { "" }));
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
    context.turns_history =
        crate::agent::conversation::recent_history(state.inner(), &final_session_id)?;
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
    // checkpoint 是恢复的权威来源：只要上一轮任务尚未到终态 (Done/Failed)，一律无缝续接！
    let mut checkpoint = match existing_checkpoint {
        Some(existing) if !existing.phase.is_terminal() => {
            existing.restore_context(&mut context);
            let mut cp = existing;
            if cp.phase == AgentPhase::WaitUser {
                // 等待中任务的目标以 current_goal 为准（旧 checkpoint 无此字段时回退锚点）
                let waiting_goal = if cp.data.current_goal.is_empty() {
                    &cp.data.user_goal
                } else {
                    &cp.data.current_goal
                };
                if !goal.trim().is_empty() && waiting_goal != &goal {
                    context.add_error_feedback(&format!("【人类回复】{}", goal));
                }
                // 保持原本的任务主目标不变
                context.user_goal = cp.data.user_goal.clone();
                cp.begin_planning(cp.step_id, &context);
            }
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
        let skill_index_text =
            crate::agent::skill_context::skill_routing_index(&crate::agent::skill_context::resolve_skills_dir());
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
                "🧭 任务分流: {} — {}（分流调用降级: {}）",
                verdict.route, verdict.reason, fb
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
        // 取证任务单：方案路由任务把 Pro 分流的调查方向注入探索期上下文——
        // 执行模型按单取证而不是自由漫游；非方案路由（direct 等）不注入。
        if verdict.requires_plan_gate() {
            if let Some(brief) = verdict.investigation_brief_text() {
                context.turns_history.push(crate::agent::context::ChatMessage {
                    role: "user".to_string(),
                    content: json!(format!(
                        "【🧭 取证任务单（入口分流给出）】\n{}\n请按任务单定向取证；证据成熟后输出 submit_plan 提交调查结论与初步方案——Tech Lead 评审时有权直接修订定稿，以定稿版本为执行依据。",
                        brief
                    )),
                });
            }
        }
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
    let target_plan_mode = crate::agent::checkpoint::PlanExecutionMode::from_str_or_auto_pilot(
        plan_mode.as_deref(),
        auto_pilot,
    );
    checkpoint.set_execution_mode(target_plan_mode);
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
                        context.add_image_feedback(
                            &format!("【用户已附加图片: `{}`】(文件路径: {}, 大小: {} 字节)", att.file_name, att.file_path, att.size_bytes),
                            data_url,
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
                        } else if att.file_name.ends_with(".webp") || att.file_path.ends_with(".webp") {
                            "image/webp"
                        } else {
                            "image/jpeg"
                        }
                    } else {
                        &att.content_type
                    };
                    let data_url = format!("data:{};base64,{}", mime, b64);
                    context.add_image_feedback(
                        &format!("【用户已附加图片: `{}`】(文件路径: {}, 大小: {} 字节)", att.file_name, att.file_path, att.size_bytes),
                        &data_url,
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

                let can_inline = inlined_attachment_bytes + (att.size_bytes as usize) <= crate::agent::file_parser::INLINE_TOTAL_BUDGET;
                let parsed_content = if actual_file_path.exists() {
                    crate::agent::file_parser::parse_file(&actual_file_path, None).ok()
                } else if src_path.exists() {
                    crate::agent::file_parser::parse_file(src_path, None).ok()
                } else {
                    None
                };

                let prompt_text = if let Some(ref content) = parsed_content {
                    let (rendered, newly_inlined) = crate::agent::file_parser::format_for_prompt_with_sandbox(
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
                    format!("📎【用户已挂载本地文件】`{}` (沙箱路径: `{}`)\n", att.file_name, sandbox_path_str)
                };

                if !prompt_text.is_empty() {
                    context.turns_history.push(crate::agent::context::ChatMessage {
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
                                    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                                    let mime = if file_name.ends_with(".png") || file_path.ends_with(".png") {
                                        "image/png"
                                    } else if file_name.ends_with(".webp") || file_path.ends_with(".webp") {
                                        "image/webp"
                                    } else {
                                        "image/jpeg"
                                    };
                                    let data_url = format!("data:{};base64,{}", mime, b64);
                                    context.add_image_feedback(
                                        &format!("【用户已附加图片: `{}`】(文件路径: {}, 大小: {} 字节)", file_name, file_path, bytes.len()),
                                        &data_url,
                                    );
                                    injected_image_paths.insert(file_path.to_string());
                                }
                            } else if let Ok(extracted) = crate::agent::file_parser::parse_file(p, None) {
                                // 同样自动拷贝到会话 tmp/ 沙箱目录并生成分层提示词！
                                let tmp_dir = crate::agent::path_guard::get_session_tmp_dir(&final_session_id);
                                let _ = std::fs::create_dir_all(&tmp_dir);
                                let target_file = tmp_dir.join(file_name);
                                if p != target_file {
                                    let _ = std::fs::copy(p, &target_file);
                                }
                                let sandbox_path_str = format!("tmp/{}", file_name);
                                let can_inline = inlined_attachment_bytes <= crate::agent::file_parser::INLINE_TOTAL_BUDGET;
                                let (prompt_text, newly_inlined) = crate::agent::file_parser::format_for_prompt_with_sandbox(
                                    &extracted,
                                    file_name,
                                    Some(&sandbox_path_str),
                                    can_inline,
                                );
                                inlined_attachment_bytes += newly_inlined;
                                context.turns_history.push(crate::agent::context::ChatMessage {
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
    let mut action_history: std::collections::VecDeque<String> = std::collections::VecDeque::new();

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
                Some(HumanControl::Answer(reply)) => reply,
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
    let _ = crate::agent::job_engine::recover_orphaned_jobs_on_session_start(state.inner(), &final_session_id);
    let _ = crate::agent::child_reviewer::recover_orphaned_child_runs_on_session_start(state.inner(), &final_session_id);

    // durable cursor 是调度权威：每个普通步骤提交后推进 step_id；恢复时也从这里继续。
    // 不再使用启动瞬间捕获范围的 for-loop，避免 checkpoint 更新后调度器仍沿旧游标运行。
    let max_steps = effective_max_steps(run_budget.as_ref());
    let mut spent_prompt_tokens: i64 = 0;
    let mut budget_stop_reason: Option<String> = None;
    while checkpoint.step_id < max_steps {
        // 心跳：每轮步边界刷新——停滞看门狗据此区分"慢"与"死"
        stall_probe.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            std::sync::atomic::Ordering::Relaxed,
        );
        crate::agent::control::mark_stage(&final_session_id, "loop_top:receipt_poll");
        // token 预算在步骤边界检查：一旦跨线立即收敛出报告，不再派发新步骤
        if let Some(reason) = token_budget_stop_reason(run_budget.as_ref(), spent_prompt_tokens) {
            budget_stop_reason = Some(reason);
            break;
        }
        let step_id = checkpoint.step_id;

        // 🔄 子任务交付回执消费：三条通道（自动轮询 / wait_jobs / 后台信箱）
        // 统一汇聚到 child_receipt reducer——状态变更、故障转移、退款、投影
        // 只有一个事实源，杜绝通道间语义漂移。
        let receipt_env = crate::agent::child_receipt::ReceiptEnv {
            app: Some(&app),
            db: state.inner(),
            parent_run_id: &run_id,
            session_id: &final_session_id,
            workspace_dir: &workspace_dir,
            interrupt_rx: interrupt_rx.clone(),
        };
        // 阶段审查退款须批后统一结算（逐条结算会误判在途而漏退）
        let mut stage_refund_pending = false;
        // 本批已投递回执的落账清单：applied_at 与 checkpoint 在下方同一事务提交。
        // seen 去重：信箱与台账两个来源可能携带同一子任务结果，跨通道只入账一次。
        let mut applied_receipts: Vec<crate::agent::checkpoint::AppliedReceipt> = Vec::new();
        let mut seen_receipt_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        // 🔄 异步子任务结果轮询（审查/工兵/侦察）：自动注入父 Agent 上下文
        if let Ok(completed) = crate::agent::child_reviewer::poll_completed_delivery_receipts(state.inner(), &run_id, None) {
            if !completed.is_empty() {
                let mut injection = String::new();
                for receipt in completed {
                    if !seen_receipt_ids.insert(receipt.child_run_id.clone()) {
                        continue;
                    }
                    let effects = crate::agent::child_receipt::reduce_child_receipt(
                        &receipt_env,
                        &mut checkpoint,
                        &mut context,
                        &receipt,
                    )
                    .await;
                    stage_refund_pending |= effects.stage_refund_pending;
                    applied_receipts.push(crate::agent::checkpoint::AppliedReceipt {
                        run_id: receipt.child_run_id.clone(),
                        verdict: effects
                            .contract_violation
                            .clone()
                            .map(|err| ("invalid_output".to_string(), err)),
                    });
                    injection.push_str(&format!("\n{}\n", effects.model_text));
                    for notice in &effects.ui_notices {
                        let _ = app.emit(
                            &format!("agent-log-{}", final_session_id),
                            notice.clone(),
                        );
                    }
                }
                if !injection.is_empty() {
                    context.add_error_feedback(&injection);
                    let _ = app.emit(
                        &format!("agent-log-{}", final_session_id),
                        "🔄 子任务成果（含实时冲突检测）已注入父 Agent 上下文".to_string()
                    );
                }
            }
        }

        // ⚡ 通用后台任务信箱 (Durable Pending Inbox) 轮询：获取任意后台任务完成结果
        if let Ok(jobs) = crate::agent::job_engine::poll_pending_inbox(state.inner(), &run_id, 5) {
            if !jobs.is_empty() {
                let mut job_injection = String::new();
                for job in jobs {
                    // 子任务回执走统一 reducer（failover/交付/退款语义单点）；
                    // 非子任务的后台任务（watch 等）保持通用倾倒。
                    if job.job_type.starts_with("child_") || job.job_type.contains("reviewer") {
                        if !seen_receipt_ids.insert(job.job_id.clone()) {
                            continue;
                        }
                        let receipt = job.to_delivery_receipt();
                        let effects = crate::agent::child_receipt::reduce_child_receipt(
                            &receipt_env,
                            &mut checkpoint,
                            &mut context,
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
                        job_injection.push_str(&format!("\n{}\n", effects.model_text));
                        for notice in &effects.ui_notices {
                            let _ = app.emit(
                                &format!("agent-log-{}", final_session_id),
                                notice.clone(),
                            );
                        }
                        continue;
                    }
                    job_injection.push_str(&format!(
                        "\n【⚡ 后台异步任务已完成 (ID: {}, 类型: {})】\n任务描述: {}\n执行状态: {}\n耗时: {}ms\n输出/结论:\n{}\n",
                        job.job_id,
                        job.job_type,
                        job.title,
                        job.status,
                        job.duration_ms,
                        job.model_projection()
                    ));
                    if let Some(ref err) = job.error_kind {
                        job_injection.push_str(&format!("异常类别: {}\n", err));
                    }
                }
                if !job_injection.is_empty() {
                    context.add_error_feedback(&job_injection);
                    let _ = app.emit(
                        &format!("agent-log-{}", final_session_id),
                        "⚡ 后台异步任务结果已注入父 Agent 上下文".to_string()
                    );
                }
            }
        }
        // 两路回执合并后统一结算一次阶段审查退款
        if stage_refund_pending && checkpoint.settle_stage_review_refund(state.inner()) {
            context.add_error_feedback(
                "\n🔄 阶段审查次数已退还（本轮含 infra 失败且无专家产出 blocking，视为未有效审查）。",
            );
        }

        // 🔒 回执提交点：checkpoint 快照（含 context 最新投影）+ applied_at +
        // 契约裁决回写 同事务原子落账。此点之前崩溃 = 两笔账一起回滚、回执重投；
        // 此点之后 = 永不重投。投递与落账之间不再有"已消费但效果未持久化"的窗口。
        if !applied_receipts.is_empty() {
            crate::agent::checkpoint::save_checkpoint_and_consume(
                state.inner(),
                &mut checkpoint,
                &context,
                &applied_receipts,
            )?;
        }

        // 步边界状态刷新：收养旧 checkpoint 的手写名单（升级一次性迁移）
        // 并按台账重算 consumed 标志。名单退役后"在途/齐套"全部由
        // batch_id+consumed_at 推导——孤儿 id 让门禁等死人的死锁类
        // 结构性消失，无需再对账。
        checkpoint.refresh_consumed_flags(state.inner());
        let resume_instruction = recovered_instruction.take();
        // 每轮规划前把方案门禁状态投影进上下文底部状态卡：模型在决策时即可见
        // 当前模式与放行条件，不再靠写入被拒（一整轮 LLM 往返）反推门禁规则。
        context.plan_gate = Some(checkpoint.plan_gate_projection());
        if resume_instruction.is_none() {
            checkpoint.begin_planning(step_id, &context);
            save_checkpoint(state.inner(), &mut checkpoint)?;
        }
        advance_run_state(
            &app,
            &final_session_id,
            &mut run_machine,
            AgentRunEvent::BeginPlanning { step_id },
        )?;

        // 目标块步数计数保留（升维标记按步复位的载体），不再注入超限提醒——
        // 步数不能代理收敛与否，认知判断归模型。
        if let Some(block) = context.active_target_block_mut() {
            block.on_step_start();
        }

        app.emit(
            &format!("agent-log-{}", final_session_id),
            format!("正在规划第 {} 步...", step_id + 1),
        )
        .map_err(|e| e.to_string())?;

        // 安全边界一：尚未请求模型时直接收束旧运行。
        if let Some(reason) = take_pending_interrupt(&mut rx) {
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

        // 修复：绝不可以在这里清空 current_observation，因为这会导致 AI 看不到刚获取的页面数据
        // ================================================================
        // 统一重试循环：整个"规划 + 执行"作为一个原子操作，失败就重试
        // （2026-09-13 夜实弹教训：单次 call_llm 硬失败——反代僵死挂 13min、claude 400/流错误
        //   0s 即死——直接 mark_failed 整轮判死。此处把注释承诺的外层重试落地，共 3 次机会；
        //   用户主动中断不重试。）
        // ================================================================
        let mut resume_instruction_opt = resume_instruction;
        let mut phases_failure_count: u32 = 0;
        let outcome = loop {
            crate::agent::control::mark_stage(&final_session_id, "phases:call_llm_and_dispatch");
            let phases_err = match run_call_llm_and_run_tool_phases(
                &app,
                &state,
                &registry_state,
                &model_routing,
                &run_id,
                &workspace_dir,
                &goal,
                log_context,
                transient_llm_request_retries,
                &final_session_id,
                step_id,
                &mut context,
                &mut checkpoint,
                &mut run_machine,
                &mut rx,
                &mut interrupt_rx,
                resume_instruction_opt.take(),
                &mut run_trace_steps,
            )
            .await
            {
                Ok(res) => break Ok(res),
                Err(error) => error,
            };
            if phases_err.contains(crate::llm::STREAM_INTERRUPTED_ERROR) {
                break Err(phases_err);
            }
            phases_failure_count += 1;
            if phases_failure_count >= 3 {
                break Err(phases_err);
            }
            let backoff_secs = 5u64 * phases_failure_count as u64;
            let retry_warn = format!(
                "【plan+exec 第 {} 次失败，{}s 后重试（本步最多 3 次机会）】{}",
                phases_failure_count, backoff_secs, phases_err
            );
            println!("⚠️ {}", retry_warn);
            let _ = app.emit(
                &format!("agent-log-{}", final_session_id),
                retry_warn.clone(),
            );
            context.add_error_feedback(&retry_warn);
            tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
        };
        let outcome = match outcome {
            Ok(res) => {
                // 预算计量放在 outcome 定案处：含被门禁打回的 finish 步，
                // 被拒步骤消耗的 LLM 调用同样计入 token 上限。
                if let Some(u) = res.token_usage.as_ref() {
                    spent_prompt_tokens += u.prompt_tokens;
                }
                res
            }
            Err(error) => {
                let terminal_error = format!("任务执行失败: {}", error);
                advance_run_state(
                    &app,
                    &final_session_id,
                    &mut run_machine,
                    AgentRunEvent::Fail {
                        reason: terminal_error.clone(),
                    },
                )?;
                checkpoint.mark_failed(terminal_error.clone(), &context);
                save_checkpoint(state.inner(), &mut checkpoint)?;
                let failure_completion = json!({ "summary": terminal_error });
                let _ = crate::agent::run_trace::upsert_snapshot(
                    state.inner(),
                    &run_id,
                    &final_session_id,
                    &goal,
                    "failed",
                    &run_trace_steps,
                    Some(&failure_completion),
                );
                let _ = app.emit(
                    &format!("agent-progress-{}", final_session_id),
                    json!({
                        "type": "complete",
                        "message": terminal_error,
                        "success": false
                    }),
                );
                return Err(error);
            }
        };
        let (mut instruction, mut result) = (outcome.instruction, outcome.result);
        // 结构化路由信号：按真实 DispatchResult 记录成功/失败（替代对反馈文本 ❌ 的脆弱匹配）
        context.record_step_outcome(result.success);
        // P3 降级逃生口：跨门禁死锁——写门禁拦截→文案指引 ask_human→冷却/配额拒绝→回门禁
        // 再被拦的推诿循环中，每步派发都只产出门禁拒绝而零真实进展（无真人在 ask_human
        // 那头时此循环永不收敛）。连续达到阈值即改写为 RUN_INTERRUPTED_ROUTE 借
        // finish_interrupted_run 强制降级收尾：标记失败、持久化检查点与轨迹、取消子任务，
        // 留事故记录可查可续，而不是无限空转烧 token。任何一步真实产出即清零，
        // 不影响正常门禁的"拦一次→照做→放行"节奏。
        if is_gate_block_route(&result.route) {
            checkpoint.consecutive_gate_blocked =
                checkpoint.consecutive_gate_blocked.saturating_add(1);
            if checkpoint.consecutive_gate_blocked >= MAX_CONSECUTIVE_GATE_BLOCKS {
                let reason = format!(
                    "【宿主强制降级终止】连续 {} 次派发均被门禁/求助拒绝且无任何有效产出，判定为跨门禁死锁（写门禁拦截 + 审查预算耗尽 + ask_human 冷却/配额互相推诿，且人工通道无人值守）。未决阻断：阶段审查 {} 项、计划审查已重试 {} 次、阶段审查已重试 {} 次；最后拒绝路由 {}。已完成的步骤、证据与工作区改动仍保留在轨迹与检查点中，可人工介入后续跑。",
                    checkpoint.consecutive_gate_blocked,
                    checkpoint.stage_review_blockers_open,
                    checkpoint.plan_reviewer_retry_count,
                    checkpoint.reviewer_retry_count,
                    result.route,
                );
                println!("🛑 [GateDeadlock] {}", reason);
                result = DispatchResult { view: None,
                    success: true,
                    stdout: reason,
                    stderr: String::new(),
                    route: RUN_INTERRUPTED_ROUTE.to_string(),
                };
            }
        } else {
            checkpoint.consecutive_gate_blocked = 0;
        }
        if result.route == RUN_INTERRUPTED_ROUTE {
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
                result.stdout,
            )
            .await?;
            return Ok(());
        }

        // ================================================================
        // 以下是成功跳出规划+执行重试循环后的处理逻辑
        // ================================================================

        // --- 网关层硬拦截：死循环检测 ---
        action_history.push_back(instruction.get_action().to_lowercase());
        if action_history.len() > 4 {
            action_history.pop_front();
        }

        // 连续 3 次 extract 才触发（放宽阈值，避免合理的"extract→等待→extract"被误判）
        let is_loop = if action_history.len() >= 3 {
            let len = action_history.len();
            action_history[len - 3] == "extract"
                && action_history[len - 2] == "extract"
                && action_history[len - 1] == "extract"
        } else {
            false
        };

        // extract → scroll → extract 这个死循环模式依然保留
        let is_loop_3 = if action_history.len() >= 3 {
            let len = action_history.len();
            let last_three = (
                &action_history[len - 3],
                &action_history[len - 2],
                &action_history[len - 1],
            );
            last_three.0 == "extract" && last_three.1 == "scroll" && last_three.2 == "extract"
        } else {
            false
        };

        if is_loop || is_loop_3 {
            let error_detail = "【🚨 网关强制阻断】: 系统检测到你陷入了认知死胡同（连续无意义的 extract 循环）！当前页面的目标元素大概率不存在，或者你理解错了页面状态！请立刻通过 `back` 返回上一页，或者更改搜索关键词重新 `goto`，严禁再次执行 extract！";
            context.add_error_feedback(error_detail);
            app.emit(
                &format!("agent-log-{}", final_session_id),
                "🚨 网关层拦截：发现违规连续 extract 操作，强制注入纠偏机制！",
            )
            .ok();
            action_history.clear(); // 阻断后清空历史，重新积累
        }

        // --- 更新任务规划 + 核心记忆 ---
        // 两类责任账本独立更新，避免审查契约的拒绝路径吞掉写入验证更新。
        let review_contract_started_this_turn =
            apply_instruction_state_updates(&mut context, &instruction, step_id);

        // --- 阶段级写入审查（双触发：todo 阶段推进 / 累计写入文件数兜底）---
        // 首次评估先建立覆盖基线（存量副作用不计入"未覆盖写入"）
        if checkpoint.stage_review_baseline_side_effects == 0 && checkpoint.stage_review_budget_used == 0 {
            checkpoint.stage_review_baseline_side_effects = context.side_effects.len() as u32;
        }
        // 【shell 批改探针·后】步骤结束时取差值入账（覆盖单发/批量/重试所有执行路径）
        {
            let post = crate::agent::review_tools::git_dirty_file_count(&workspace_dir);
            match (context.shell_probe_pre_dirty, post) {
                (Some(pre), Some(post)) => {
                    let delta = post.saturating_sub(pre).min(50);
                    if delta > 0 {
                        checkpoint.uncovered_shell_mutations =
                            checkpoint.uncovered_shell_mutations.saturating_add(delta);
                    }
                }
                // 非 git 环境：按本步骤是否为写类指令保守计 1（但排除 session_tmp 草稿写入）
                (None, _) if crate::agent::dispatcher::is_write_or_side_effect_tool(&instruction) => {
                    let is_tmp_write = instruction.get_tool().eq_ignore_ascii_case("local_file")
                        && crate::agent::tools::local_file::is_dispatch_result_tmp(&result);
                    if !is_tmp_write {
                        checkpoint.uncovered_shell_mutations =
                            checkpoint.uncovered_shell_mutations.saturating_add(1);
                    }
                }
                _ => {}
            }
            context.shell_probe_pre_dirty = None;
        }
        println!(
            "【阶段审查诊断】step {} | phase={:?} | mode={:?} | uncovered={} | shell_baseline={} | budget={} | side_effects={}",
            step_id,
            checkpoint.plan_phase,
            checkpoint.plan_mode,
            checkpoint.uncovered_shell_mutations,
            checkpoint.stage_review_shell_baseline,
            checkpoint.stage_review_budget_used,
            context.side_effects.len()
        );
        if let Some(reason) = crate::agent::stage_review::should_fire(&checkpoint, state.inner(), &context, &instruction, &model_routing) {
            match crate::agent::stage_review::dispatch(
                &app,
                state.inner(),
                &run_id,
                &final_session_id,
                &workspace_dir,
                &mut checkpoint,
                &context,
                &model_routing,
                step_id,
                interrupt_rx.clone(),
            )
            .await
            {
                Ok(()) => {
                    let _ = save_checkpoint(state.inner(), &mut checkpoint);
                    let _ = app.emit(
                        &format!("agent-log-{}", final_session_id),
                        format!("📋 阶段审查已派发（{}）", reason),
                    );
                }
                Err(err) => {
                    let _ = app.emit(
                        &format!("agent-log-{}", final_session_id),
                        format!("⚠️ 阶段审查派发失败: {}", err),
                    );
                }
            }
        }
        if !instruction.memories_update.is_empty() {
            let rejections = context.update_memories(instruction.memories_update.clone());
            if !rejections.is_empty() {
                context.turns_history.push(crate::agent::context::types::ChatMessage {
                    role: "user".to_string(),
                    content: serde_json::json!(format!(
                        "【⚠️ 记忆安全提示】\n以下记忆项未能存入核心记忆卡：\n{}\n（请勿在记忆中保存明文密钥或超长文本；长数据请存入 TaskStore 数据区）",
                        rejections.iter().map(|r| format!("- {}", r)).collect::<Vec<_>>().join("\n")
                    )),
                });
            }
            println!("📝 AI 更新了核心记忆: {:?}", instruction.memories_update);
        }
        if let Some(ref summary) = instruction.progress_summary {
            context.set_progress_summary(summary);
            println!("📝 AI 更新了进度摘要: {}", summary);
        }

        // --- 核心增强：历史截断与缓存优化 ---
        if instruction.clear_history.unwrap_or(false) {
            println!("🧹 AI 主动触发了历史清理 (clear_history: true)!");
            context.turns_history.clear();
        }

        // --- 核心增强：Token 优化与冷存储检索 ---
        // AI 在本轮请求 "require_memory: true" 或触发了清空历史，下一轮组装消息时就会塞入全部 Fact 内容
        context.carry_memories = instruction.require_memory.unwrap_or(false)
            || instruction.clear_history.unwrap_or(false);

        // --- 预加载下一轮工具说明书 ---
        let next_tool = instruction
            .next_tool_hint
            .clone()
            .unwrap_or_else(|| instruction.get_action());
        context.active_tool = Some(next_tool.clone());

        // --- 加载工具说明书（Write-Once 历史事件 + 未命中纠错 + 现场刷新，见 tool_manual.rs）---
        crate::agent::tool_manual::load_tool_manual_for_next_step(
            &app,
            registry_state.inner(),
            &mut context,
            &instruction,
            &workspace_dir,
        )
        .await;

        // --- 刷新独立任务数据区目录，再把轻量引用写进 checkpoint ---
        // 数据行早已由 task_store 自己实时持久化，绝不复制到 context。
        context.set_task_resources(task_store::list_resource_refs(
            state.inner(),
            &final_session_id,
        )?);

        // --- 记录历史 + 更新观测 ---
        let final_stdout = result.stdout.clone();
        let mut output_text = if result.success {
            final_stdout.clone()
        } else {
            format!("❌ {}\n{}", result.stderr, final_stdout)
        };

        let is_finish = is_finish_instruction(&instruction);
        // prepare_finish：与 finish 同一套门禁评估，但只沉淀收据、不提交终态。
        let is_prepare_finish = instruction.prepare_finish.is_some();
        let finish_gate_requested = is_finish || is_prepare_finish;

        // 🌟 纯静态资源自动豁免与无测试工具自适应跳过：
        if finish_gate_requested && result.success && context.change_verification.is_none() {
            let has_explicit_verify = context
                .review_contract
                .as_ref()
                .map_or(false, |rc| rc.has_executable_verification());
            if !has_explicit_verify {
                if context.is_only_ui_or_doc_side_effects() {
                    context.change_verification = Some(crate::agent::change_verification::ChangeVerification {
                        status: "waived".to_string(),
                        evidence_refs: Vec::new(),
                        waived_reason: Some(
                            "改动全部为纯静态资源（样式/图片/文档），系统自动豁免写入验证".to_string(),
                        ),
                    });
                    let _ = app.emit(
                        &format!("agent-log-{}", final_session_id),
                        "🎨 系统自动感知：检测到本次改动全部为纯静态资源（样式/图片/文档），已自动豁免写入测试验证 (waived)".to_string(),
                    );
                } else if !context.has_inferrable_verification_command() {
                    // 🌟 泛化降级策略：如果改动了代码文件，但当前工程未检测到任何自动化测试套件或类型检查工具，
                    // 按照“如果一般都可以，这个如果没有就跳过”的原则，系统自适应跳过强制校验
                    context.change_verification = Some(crate::agent::change_verification::ChangeVerification {
                        status: "waived".to_string(),
                        evidence_refs: Vec::new(),
                        waived_reason: Some(
                            "当前工程未检测到类型检查器或自动化测试配置，系统自适应跳过验证".to_string(),
                        ),
                    });
                    let _ = app.emit(
                        &format!("agent-log-{}", final_session_id),
                        "☕ 系统自动感知：当前工程未检测到测试或类型检查工具，已自适应跳过验证 (waived)".to_string(),
                    );
                }
            }
        }

        let side_effect_count = context
            .side_effects
            .len()
            .saturating_sub(context.inherited_side_effects_count);

        // 🧪 宿主权威验证兜底：finish/prepare_finish 时仍有挂账写入而验证未闭合，
        // 且宿主能推断出验证命令 → 宿主亲自执行，真实退出码即判据。
        // 成功 → 写入批次自动闭合 verified（宿主回执，非模型自述）；
        // 红灯 → 直接打回且不消耗质询熔断额度——确定性失败绝不因"重试次数用尽"放行。
        // 模型已显式 waived 的批次不重复执行（豁免理由的正确性由终审核对）。
        if finish_gate_requested
            && result.success
            && side_effect_count > 0
            && context
                .change_verification
                .as_ref()
                .map_or(true, |v| !v.is_resolved())
        {
            if let Some(verify_cmd) = context.inferred_verification_command() {
                let verify_timeout = crate::agent::channel::channel_config_shared()
                    .read()
                    .ok()
                    .and_then(|c| c.verify_timeout_secs)
                    .unwrap_or(120)
                    .clamp(10, 600);
                let verify_outcome = crate::agent::host_verify::run_command(
                    &workspace_dir,
                    &verify_cmd,
                    verify_timeout,
                )
                .await;
                if verify_outcome.passed {
                    context.change_verification = Some(ChangeVerification {
                        status: "verified".to_string(),
                        evidence_refs: vec![format!(
                            "host_verify:step_{} (`{}` → exit {})",
                            step_id, verify_cmd, verify_outcome.exit_code
                        )],
                        waived_reason: None,
                    });
                    let _ = app.emit(
                        &format!("agent-log-{}", final_session_id),
                        format!(
                            "🧪 宿主权威验证通过：`{}` → exit {}，写入批次自动闭合 verified",
                            verify_cmd, verify_outcome.exit_code
                        ),
                    );
                } else if is_finish {
                    let reject_msg = format!(
                        "【🛑 宿主权威验证未通过，finish 未生效！】\n宿主亲自执行: `{}`\n退出码: {}\n输出尾部:\n{}\n\n该判定为宿主实测结果，不接受文本声明覆盖，也不消耗质询熔断额度。\n【必须采取的行动】：请依据上方真实输出修复后重试；若确认该检查不适用于本改动，可用 change_verification_update 提交 waived + waived_reason 显式豁免（终审将核对豁免理由）。",
                        verify_cmd, verify_outcome.exit_code, verify_outcome.output_tail
                    );
                    context.add_error_feedback(&reject_msg);
                    crate::agent::dispatcher::emit_tool_event(
                        &app,
                        &final_session_id,
                        step_id,
                        &instruction,
                        "host_verify_challenged",
                        Some("finish".to_string()),
                        Some(false),
                        &format!(
                            "宿主权威验证未通过：`{}` → 退出码 {}",
                            verify_cmd, verify_outcome.exit_code
                        ),
                        None,
                        None,
                    );
                    context.record_step_outcome(false);
                    checkpoint.finish_reviewed = true;
                    persist_challenged_step(
                        state.inner(),
                        &mut run_trace_steps,
                        &run_id,
                        &final_session_id,
                        step_id,
                        &instruction,
                        &context,
                        "host_verify_challenged",
                        &reject_msg,
                        &result.view,
                        step_metrics_json(outcome.token_usage.as_ref(), outcome.thinking_duration_ms, outcome.tool_duration_ms),
                        &outcome.thinking_text,
                    );
                    let _ = save_checkpoint(state.inner(), &mut checkpoint);
                    continue;
                } else {
                    // prepare_finish：预检不拦截，但把宿主实测失败如实反馈，
                    // 收据中验证项保持未闭合状态
                    context.add_error_feedback(&format!(
                        "【宿主权威验证未通过（预检）】`{}` → 退出码 {}：{}",
                        verify_cmd, verify_outcome.exit_code, verify_outcome.output_tail
                    ));
                }
            }
        }

        let diff_context = if side_effect_count > 0 {
            let affected_paths = context.new_affected_paths();
            let raw_diff = git_baseline.collect_git_diff(&workspace_dir).map(|(d, _)| d);
            Some(DiffContext::from_diff_and_paths(
                raw_diff.as_deref(),
                &affected_paths,
            ))
        } else {
            None
        };

        // 交付摘要：义务证据校验反撒谎拦截与遥测路由判定共用
        // （prepare_finish 的预填摘要与 finish.command.summary 同等参与判定，保证预检口径一致）
        let instruction_summary = instruction
            .get_params()
            .get("command")
            .and_then(|command| command.get("summary"))
            .and_then(|s| s.as_str())
            .map(str::to_string)
            .or_else(|| {
                instruction
                    .prepare_finish
                    .as_ref()
                    .and_then(|req| req.summary.clone())
            });

        let mut open_todos = finish_blockers_with_diff(
            &context.todo_list,
            context.review_contract.as_ref(),
            review_contract_started_this_turn,
            side_effect_count,
            context.change_verification.as_ref(),
            diff_context.as_ref(),
            Some(workspace_dir.as_path()),
            instruction_summary.as_deref(),
        );
        let evidence_blockers = context
            .review_contract
            .as_ref()
            .map(|c| {
                review_contract_evidence_blockers(
                    c,
                    side_effect_count,
                    diff_context.as_ref(),
                    Some(workspace_dir.as_path()),
                    instruction_summary.as_deref(),
                )
            })
            .unwrap_or_default();
        let contract_evidence_unresolved = !evidence_blockers.is_empty();
        // 门禁触发遥测：义务证据三分法（diff 对账 / 修改声明拦截 / 诊断静态校验）的触发频率
        if !evidence_blockers.is_empty() {
            let route = if diff_context
                .as_ref()
                .map(|d| !d.changed_files.is_empty())
                .unwrap_or(false)
            {
                "finish_obligation_evidence:diff_not_matched"
            } else if instruction_summary
                .as_deref()
                .map(crate::agent::finish_guard::delivery_claims_code_modification)
                .unwrap_or(false)
            {
                "finish_obligation_evidence:modification_claim_denied"
            } else {
                "finish_obligation_evidence:static_check_failed"
            };
            crate::agent::telemetry::record_gate_fire(
                state.inner(),
                &crate::agent::telemetry::GateFireRecord {
                    run_id: &run_id,
                    session_id: &final_session_id,
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
                    Some(workspace_dir.as_path()),
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
                state.inner(),
                &crate::agent::telemetry::GateFireRecord {
                    run_id: &run_id,
                    session_id: &final_session_id,
                    step_id: step_id as i64,
                    gate: "finish_obligation_evidence",
                    route: "finish_obligation_evidence:noncode_refs:not_applicable",
                },
            );
        }
        let complex_delivery_blocker =
            complex_delivery_blocker(&context, &instruction, is_finish, result.success);
        let complex_delivery_unresolved = complex_delivery_blocker.is_some();
        if let Some(blocker) = complex_delivery_blocker {
            open_todos.push(blocker);
        }
        // 【廉价完成防线】评测 oracle 的判分进度是任务完成度的客观事实源：
        // 模型自declared的审查契约/TODO 只覆盖它自己申报的范围（实测：1/24 交付被判
        // succeeded）。oracle 未完成时把缺口作为持久 blocker——与 complex 未决同级，
        // 不因 finish_reviewed 置位而失效；熔断上限（MAX_FINISH_CHALLENGES）仍是安全阀。
        let mut oracle_progress_unresolved = false;
        if finish_gate_requested && result.success {
            if let Some((index, total, completed)) =
                crate::agent::eval_oracle::query_active_progress().await
            {
                if !completed && index < total {
                    oracle_progress_unresolved = true;
                    open_todos.push(format!(
                        "宿主判分进度 {}/{} 未完成：本任务以宿主逐阶段判分为唯一完成标准，禁止以部分阶段通过为由交卷；请读取 CURRENT_STAGE.md 继续实现下一阶段并用 npm test 送验",
                        index, total
                    ));
                }
            }
        }
        if finish_gate_requested && (crate::agent::child_reviewer::has_running_child_runs(state.inner(), &run_id) || crate::agent::job_engine::has_running_jobs(state.inner(), &run_id)) {
            open_todos.push("仍有后台异步任务或持续监听正在执行中：后台任务请调用 wait_jobs 挂起等待并接收结论；持续监听（watch）请调用 watch_stop 显式释放后再提交 finish，或执行其他独立操作后重试".to_string());
        }
        if finish_gate_requested && checkpoint.stage_review_blockers_open > 0 {
            open_todos.push(format!(
                "阶段审查仍有 {} 条未解决的 blocking 缺陷：请先调用 resume_child 对质撤回，或调用 ask_human 申请人工仲裁放行",
                checkpoint.stage_review_blockers_open
            ));
        }
        let has_code_modification_intent = context.active_target_block().is_some()
            || context.todo_list.iter().any(|t| {
                let d = t.description.to_lowercase();
                d.contains("修改")
                    || d.contains("修复")
                    || d.contains("编写")
                    || d.contains("重构")
                    || d.contains("实现")
                    || d.contains("write")
                    || d.contains("fix")
                    || d.contains("refactor")
            });
        if finish_gate_requested
            && has_code_modification_intent
            && !checkpoint.plan_phase.is_approved()
            && (checkpoint.last_review_scorecard.as_ref().map_or(false, |s| !s.passes_gate())
                || checkpoint.last_review_blocking_count > 0)
            && (context.side_effects.len() == context.inherited_side_effects_count)
        {
            open_todos.push(
                "方案审查未达标且尚未写入任何代码：禁止直接提交 finish！请根据审查意见修正方案并重新审查，或调用 ask_human 呈报用户申请人工仲裁放行后写入代码".to_string(),
            );
        }
        let finish_challenged = if is_prepare_finish {
            // prepare_finish 只评估、不质询：blockers 沉淀进收据，
            // finish_reviewed / challenge_count / 重试预算一律不动。
            false
        } else {
            should_challenge_finish_with_complex_delivery_blocker(
                is_finish,
                result.success,
                checkpoint.finish_reviewed,
                checkpoint.finish_challenge_count,
                &open_todos,
                complex_delivery_unresolved || oracle_progress_unresolved || contract_evidence_unresolved,
            )
        };

        let mut reviewer_challenged = false;
        let mut reviewer_warnings_delivered: Option<crate::agent::reviewer_gatekeeper::ReviewResult> = None;

        if finish_challenged {
            checkpoint.finish_reviewed = true;
            checkpoint.finish_challenge_count = checkpoint.finish_challenge_count.saturating_add(1);
            let blocker_summary = open_todos
                .iter()
                .map(|todo| format!("- {todo}"))
                .collect::<Vec<_>>()
                .join("\n");
            // 空转检测：同一组 blocker 连续未变 = 模型只重发 finish 不改证据。
            // 第 3 次起升级指引——不再泛泛说"别草率结项"，直接点破两条出路。
            let blocker_sig = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                blocker_summary.hash(&mut h);
                h.finish()
            };
            if checkpoint.last_finish_blocker_sig == Some(blocker_sig) {
                checkpoint.repeated_blocker_count =
                    checkpoint.repeated_blocker_count.saturating_add(1);
            } else {
                checkpoint.last_finish_blocker_sig = Some(blocker_sig);
                checkpoint.repeated_blocker_count = 1;
            }
            let escalation = if checkpoint.repeated_blocker_count >= 3 {
                format!(
                    "\n\n【🚨 同一阻塞已连续 {} 次未变化——判定为空转】你此前的 finish 只是重发相同证据，契约义务里的 evidence_refs 根本没动。立即二选一，禁止再次原样重试：\n① 用 `review_contract_update` 把受阻义务的 evidence_refs 改为上方 diff 区间内的行号（注意是更新契约义务本身，不是改报告正文）；\n② 认为义务不适用或证据无法落入 diff → 调用 `ask_human` 向用户呈报阻塞清单申请人工仲裁。",
                    checkpoint.repeated_blocker_count
                )
            } else {
                String::new()
            };
            context.add_error_feedback(&format!(
                "【⚠️ 交付门禁拦截打回：任务尚未满足结项标准，finish 未生效！】\n【未闭合项/证据缺失】：\n{}\n任务仍在运行中。请勿在未闭合上述事项时草率结项！\n【必须采取的行动】：\n- 若为写入操作，请执行测试验证并在 change_verification_update 中以 verified + evidence_refs 闭合；\n- 若某项不再适用，请在 review_contract_update 或 todo_update 中显式标为 waived / failed 并说明理由；\n- 状态更新并完成验证后，方可再次提交 finish 交付；如需先评估就绪状态而不提交终态，可单独一步输出 prepare_finish 做预检。{}",
                blocker_summary, escalation
            ));
            crate::agent::dispatcher::emit_tool_event(
                &app,
                &final_session_id,
                step_id,
                &instruction,
                "finish_challenged",
                Some("finish".to_string()),
                Some(false),
                &format!(
                    "终态拦截：检测到 {} 项未闭合事项，请先行验证",
                    open_todos.len()
                ),
                None,
                None,
            );
            // 调优：finish 被质询 = 交付未达标，下一轮重写升维处理（规则模式直接升 Pro，
            // 打分模式累加连续失败权重）。实测依据：Flash 档反复守不住交付披露纪律。
            context.record_step_outcome(false);
        } else if is_finish && result.success {
            // 🧾 FinalizationReceipt 复用：prepare_finish 已就绪且 diff/scope/契约指纹未变 →
            // 跳过义务验证、宿主验证与终审 LLM 调用，直接进入终态提交。
            // 动态 blocker（新后台任务、新 Todo 等）已在上方 finish_challenged 判过，此处只省昂贵项。
            let current_diff_hash = git_baseline
                .collect_review_material(&workspace_dir, &context, run_started_at)
                .map(|(_, hash)| hash)
                .unwrap_or_default();
            let contract_fp = crate::agent::finalization::contract_fingerprint(
                context.review_contract.as_ref(),
            );
            let receipt_reusable = checkpoint
                .pending_finalization
                .as_ref()
                .map(|receipt| {
                    receipt.is_ready()
                        && receipt.is_fresh(
                            &current_diff_hash,
                            context.resource_scope.revision,
                            &contract_fp,
                        )
                })
                .unwrap_or(false);
            if receipt_reusable {
                let receipt = checkpoint.pending_finalization.as_ref().unwrap();
                let _ = app.emit(
                    &format!("agent-log-{}", final_session_id),
                    format!(
                        "🧾 终态收据复用（{}）：diff/scope/契约未变，跳过重复的义务验证、宿主验证与终审。",
                        receipt.receipt_id
                    ),
                );
                // 与审查通过分支同款的提交侧状态复位
                checkpoint.last_reviewed_diff_hash = None;
                checkpoint.last_review_result = None;
                if let Some(block) = context.active_target_block_mut() {
                    block.reset_review_retries();
                }
                context.block_manager.advance_to_next();
                crate::agent::telemetry::record_gate_fire(
                    state.inner(),
                    &crate::agent::telemetry::GateFireRecord {
                        run_id: &run_id,
                        session_id: &final_session_id,
                        step_id: step_id as i64,
                        gate: "finalization",
                        route: "finalization:receipt_reused",
                    },
                );
                crate::agent::dispatcher::emit_tool_event(
                    &app,
                    &final_session_id,
                    step_id,
                    &instruction,
                    "succeeded",
                    Some("finish".to_string()),
                    Some(true),
                    "终态收据复用：验证与终审结论在 diff 未变下仍有效，直接提交",
                    None,
                    None,
                );
            } else {
            // Level 0 机械门禁：零 Token 快速拦截（Scope 边界越界等）
            // 适用域：scope 预检的证据是 workspace 的 git diff——自由会话（未选项目）
            // 的 workspace_dir 是会话产物目录而非真实工程，无 git 判据
            // → 证据域缺失 → NotApplicable，移交终审语义判定。
            let scope_violations = if is_explicit_workspace || auto_pilot {
                crate::agent::reviewer_gatekeeper::mechanical_precheck_violations(
                    &workspace_dir,
                    context.active_target_block(),
                )
            } else {
                if context.active_target_block().is_some() {
                    if let Some(notice) =
                        crate::agent::gate_applicability::GateApplicability::not_applicable(
                            "自由会话未绑定真实工作区，scope 预检的 git diff 判据缺失",
                        )
                        .handoff_notice("Scope 边界预检门")
                    {
                        gate_notices.push(notice);
                    }
                    crate::agent::telemetry::record_gate_fire(
                        state.inner(),
                        &crate::agent::telemetry::GateFireRecord {
                            run_id: &run_id,
                            session_id: &final_session_id,
                            step_id: step_id as i64,
                            gate: "finish_review",
                            route: "finish_review:scope_precheck:not_applicable",
                        },
                    );
                }
                Vec::new()
            };
            if !scope_violations.is_empty() {
                checkpoint.finish_reviewed = true;
                context.add_error_feedback(&format!(
                    "【⚠️ 交付门禁拦截打回：代码改动超出目标块预定边界，finish 未生效！】\n{}\n（0 Token 消耗机械打回，范围不合规）。\n【必须采取的行动】：请还原无关文件的意外改动，或在方案中扩展该块的 scope 后重试。",
                    scope_violations.join("\n")
                ));
                crate::agent::dispatcher::emit_tool_event(
                    &app,
                    &final_session_id,
                    step_id,
                    &instruction,
                    "scope_challenged",
                    Some("finish".to_string()),
                    Some(false),
                    &format!("机械拦截：改动超出目标块预定范围（{} 项越界）", scope_violations.len()),
                    None,
                    None,
                );
                context.record_step_outcome(false);
            } else {
                // 零改动短路（实弹实测教训：模型在零 diff 时调 finish，终审烧一整轮
                // Reviewer 才返回"未提交任何代码变更"）：方案已声明 target_files 而当前
                // diff 与其零交集 → 0 Token 机械打回，不进终审。
                let plan_targets = checkpoint
                    .submitted_plan
                    .as_ref()
                    .map(|p| p.target_files.as_slice())
                    .filter(|t| !t.is_empty())
                    .or(checkpoint.approved_target_files.as_deref());
                // 适用域自证（applies_to）：声明目标里至少一个位于工作区内，
                // diff/副作用对账才有判据；目标全部位于工作区外（运维/系统路径，
                // 如 /usr/bin、/opt、~/.config）时 Git diff 物理上不可能命中 →
                // 本门证据域不存在 → NotApplicable 跳过移交终审，
                // 绝不把"前提不成立"硬当"违规"（实测教训：pacman 卸载被此门死锁打回）。
                let declared_but_untouched = plan_targets
                    .map(|targets| {
                        let has_ws_target = targets.iter().any(|t| {
                            crate::agent::gate_applicability::is_workspace_target(
                                t,
                                &workspace_dir,
                            )
                        });
                        if !has_ws_target {
                            if let Some(notice) =
                                crate::agent::gate_applicability::GateApplicability::not_applicable(
                                    "方案声明目标全部位于工作区外（运维/系统路径），Git diff 对账无判据",
                                )
                                .handoff_notice("零交集对账门")
                            {
                                gate_notices.push(notice);
                            }
                            crate::agent::telemetry::record_gate_fire(
                                state.inner(),
                                &crate::agent::telemetry::GateFireRecord {
                                    run_id: &run_id,
                                    session_id: &final_session_id,
                                    step_id: step_id as i64,
                                    gate: "finish_review",
                                    route: "finish_review:plan_target_mismatch:not_applicable",
                                },
                            );
                            return false;
                        }
                        // 判据域：真实 diff ∪ 副作用台账路径 ∪ 会话 tmp 产物实物
                        //（非 git 写入同样构成"目标被动过"的物理事实）
                        let session_tmp =
                            crate::agent::path_guard::get_session_tmp_dir(&final_session_id);
                        let mut touched: Vec<String> = diff_context
                            .as_ref()
                            .map(|d| build_touched_set(d, &session_tmp))
                            .unwrap_or_default();
                        // tmp/ 产物实物对账（scope 卡片承诺的直认通道）：tmp/ 写入按设计
                        // 不入副作用台账、也不在工作区 diff 内——不把目录实物桥接进对账集，
                        // 则声明 tmp/x 的任务结构性零交集 → finish 死锁。
                        extend_touched_with_session_tmp(&mut touched, &session_tmp);
                        !targets.iter().any(|tf| {
                            touched.iter().any(|p| {
                                crate::agent::finish_guard::diff::file_paths_match(
                                    &normalize_tmp_declared(tf, &session_tmp),
                                    p,
                                )
                            })
                        })
                    })
                    .unwrap_or(false);
                if declared_but_untouched {
                    let actual: Vec<String> = diff_context
                        .as_ref()
                        .map(|d| d.changed_files.keys().cloned().collect())
                        .unwrap_or_default();
                    checkpoint.finish_reviewed = true;
                    let reject_msg = format!(
                        "【⚠️ 交付门禁拦截打回：方案声明的目标文件与实际改动零交集，finish 未生效！】\n方案声明目标：{}\n实际改动文件：{}\n（0 Token 机械打回，终审无需介入）。\n【必须采取的行动】：①若尚未真正修改，请先完成对声明目标的实际改动并重跑验证；②若实现路径已变，请重新 submit_plan 更新 target_files（已批准方案须附 revision_reason 说明变更依据，将重开评审）后再 finish；③若本任务本就无需改这些文件，请用 ask_human 说明理由。",
                        plan_targets.unwrap_or(&[]).join(", "),
                        if actual.is_empty() { "（无：当前工作区零改动）".to_string() } else { actual.join(", ") }
                    );
                    context.add_error_feedback(&reject_msg);
                    crate::agent::telemetry::record_gate_fire(
                        state.inner(),
                        &crate::agent::telemetry::GateFireRecord {
                            run_id: &run_id,
                            session_id: &final_session_id,
                            step_id: step_id as i64,
                            gate: "finish_review",
                            route: "finish_review:plan_target_mismatch",
                        },
                    );
                    crate::agent::dispatcher::emit_tool_event(
                        &app,
                        &final_session_id,
                        step_id,
                        &instruction,
                        "plan_target_challenged",
                        Some("finish".to_string()),
                        Some(false),
                        "机械拦截：方案声明目标与实际改动零交集",
                        None,
                        None,
                    );
                    context.record_step_outcome(false);
                    persist_challenged_step(
                        state.inner(),
                        &mut run_trace_steps,
                        &run_id,
                        &final_session_id,
                        step_id,
                        &instruction,
                        &context,
                        "finish_challenged",
                        &reject_msg,
                        &result.view,
                        step_metrics_json(outcome.token_usage.as_ref(), outcome.thinking_duration_ms, outcome.tool_duration_ms),
                        &outcome.thinking_text,
                    );
                    // 跳过终审：机械事实已足够打回，省一次审查
                    let _ = save_checkpoint(state.inner(), &mut checkpoint);
                    continue;
                }
                // Level 1: 确定性门禁与机械门禁 100% 通过后，才调用 Reviewer 终审（带 Checklist 证据链严审）！
                // 【B 路径执行器】契约义务绑定的冻结验证命令：宿主逐条执行，退出码即判定。
                // 失败 = 0 token 机械拦截打回（验证器故障也判失败，绝不放行）。
                let obligation_results = if context.side_effects.len() > context.inherited_side_effects_count {
                    let obligations: Vec<crate::agent::review_contract::ReviewObligation> = context
                        .review_contract
                        .as_ref()
                        .map(|c| c.obligations.clone())
                        .unwrap_or_default();
                    crate::agent::host_verify::run_obligation_verifications(&workspace_dir, &obligations)
                        .await
                } else {
                    Vec::new()
                };
                let obligation_failures: Vec<String> = obligation_results
                    .iter()
                    .filter(|r| !r.passed)
                    .map(|r| {
                        format!(
                            "- 义务 [{}] 验证命令 `{}` 退出码 {}: {}",
                            r.obligation_id, r.command, r.exit_code, r.output_tail
                        )
                    })
                    .collect();
                if !obligation_failures.is_empty() {
                    checkpoint.finish_reviewed = true;
                    let reject_msg = format!(
                        "【⚠️ 交付门禁拦截打回：契约绑定的宿主验证命令未通过，finish 未生效！】\n{}\n（说明：命令退出码即为判定依据，冻结脚本不可修改）。\n【必须采取的行动】：请真实修复对应代码使验证通过后重试；严禁未验证直接提交交付。",
                        obligation_failures.join("\n")
                    );
                    context.add_error_feedback(&reject_msg);
                    crate::agent::dispatcher::emit_tool_event(
                        &app,
                        &final_session_id,
                        step_id,
                        &instruction,
                        "obligation_verify_challenged",
                        Some("finish".to_string()),
                        Some(false),
                        &format!("义务宿主验证失败 {} 项", obligation_failures.len()),
                        None,
                        None,
                    );
                    context.record_step_outcome(false);
                    persist_challenged_step(
                        state.inner(),
                        &mut run_trace_steps,
                        &run_id,
                        &final_session_id,
                        step_id,
                        &instruction,
                        &context,
                        "finish_challenged",
                        &reject_msg,
                        &result.view,
                        step_metrics_json(outcome.token_usage.as_ref(), outcome.thinking_duration_ms, outcome.tool_duration_ms),
                        &outcome.thinking_text,
                    );
                    // 跳过终审：机械事实已足够打回，省一次审查
                    let _ = save_checkpoint(state.inner(), &mut checkpoint);
                    continue;
                }
                // 宿主执行验证：有真实写入且配置了验证命令时，宿主亲自跑一把，
                // 真实退出码进 reviewer 证据包（堵"模型把失败当证据"的幻觉空子）
                let host_verification = {
                    let mut parts: Vec<String> = Vec::new();
                    if context.side_effects.len() > context.inherited_side_effects_count {
                        if let Some(hv) = crate::agent::host_verify::run(&workspace_dir).await {
                            let _ = app.emit(
                                &format!("agent-log-{}", final_session_id),
                                format!("🧪 宿主执行验证完成:\n{}", hv),
                            );
                            parts.push(hv);
                        }
                    }
                    if !obligation_results.is_empty() {
                        let lines: Vec<String> = obligation_results
                            .iter()
                            .map(|r| {
                                format!(
                                    "- [{}] `{}` → 退出码 {}: {}",
                                    r.obligation_id, r.command, r.exit_code, r.output_tail
                                )
                            })
                            .collect();
                        parts.push(format!("契约义务验证（冻结脚本）：\n{}", lines.join("\n")));
                    }
                    (!parts.is_empty()).then(|| parts.join("\n\n"))
                };
                let human_waivers = checkpoint.human_waiver_text();
                let review_res = crate::agent::reviewer_gatekeeper::evaluate_finish_diff(
                    &app,
                    state.inner(),
                    &context,
                    &model_routing,
                    step_id,
                    &workspace_dir,
                    &git_baseline,
                    run_started_at,
                    &final_session_id,
                    &mut interrupt_rx,
                    checkpoint.last_reviewed_diff_hash.as_deref(),
                    checkpoint.last_review_result.as_ref(),
                    &checkpoint.stage_review_final_only,
                    host_verification.as_deref(),
                    human_waivers.as_deref(),
                    &gate_notices,
                    checkpoint.approved_target_files.as_deref(),
                    checkpoint.triage_verdict.as_ref(),
                    Some(checkpoint.plan_mode),
                )
                .await;

                // 审查效能报告：撤回率是误报率的上界（含合理补救型撤回）
                {
                    let (tw, tm) = (checkpoint.review_total_withdrawn, checkpoint.review_total_maintained);
                    if tw + tm > 0 {
                        let rate = (tw as f64 / (tw + tm) as f64 * 100.0).round() as u32;
                        let _ = app.emit(
                            &format!("agent-log-{}", final_session_id),
                            format!(
                                "📊 审查效能: 异议撤回 {}/{}（约 {}%）——撤回率是误报率上界；>50% 提示该 reviewer 判定过松，建议调整角色卡或更换模型。",
                                tw, tw + tm, rate
                            ),
                        );
                    }
                }

                if !review_res.is_passed() {
                // 存在阻塞性缺陷！
                crate::agent::telemetry::record_gate_fire(
                    state.inner(),
                    &crate::agent::telemetry::GateFireRecord {
                        run_id: &run_id,
                        session_id: &final_session_id,
                        step_id: step_id as i64,
                        gate: "finish_review",
                        route: "finish_review:blocking",
                    },
                );
                if checkpoint.reviewer_retry_count < 3 {
                    checkpoint.reviewer_retry_count += 1;
                    checkpoint.last_reviewed_diff_hash = Some(review_res.diff_hash.clone());
                    checkpoint.last_review_result = Some(review_res.clone());
                    reviewer_challenged = true;

                    // 目标块审查熔断联动：超限自动升维 Pro
                    let breaker_alert = if let Some(block) = context.active_target_block_mut() {
                        let tripped = block.record_review_failure();
                        if tripped {
                            Some(format!("\n\n【🚨 审查熔断告警】当前目标块 [{}] 连续 3 次未能通过审查，已触发死循环熔断！系统已强制将该块升维至主力强模型（Pro）接管执行，请利用强推理能力重构并彻底解决上述缺陷。", block.id))
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    let issues_text = review_res
                        .blocking_issues
                        .iter()
                        .enumerate()
                        .map(|(i, b)| {
                            format!(
                                "{}. 【{}】{}\n   👉 修复要求: {}",
                                i + 1,
                                b.file,
                                b.issue,
                                b.requirement
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    let nits_text = if !review_res.nit_issues.is_empty() {
                        let nits = review_res
                            .nit_issues
                            .iter()
                            .map(|n| format!("- [{}] {}", n.file, n.suggestion))
                            .collect::<Vec<_>>()
                            .join("\n");
                        format!("\n\n💡 次要建议（供参考，不阻碍交付）：\n{}", nits)
                    } else {
                        String::new()
                    };

                    context.add_error_feedback(&format!(
                        "【⚠️ 终态审查未通过打回：检测到阻塞性缺陷，finish 未生效！（第 {}/3 次评审）】\n审查模型（{}）评分: {}/100\n审查评价: {}\n\n检测到以下阻塞性缺陷（Blocking Issues）：\n{}{}{}\n\n任务仍在运行中。请直接针对上述缺陷修改代码并运行测试验证，修复完成后再次调用 finish 交付。",
                        checkpoint.reviewer_retry_count,
                        review_res.model_id,
                        review_res.score,
                        review_res.summary,
                        issues_text,
                        nits_text,
                        breaker_alert.as_deref().unwrap_or("")
                    ));

                    crate::agent::dispatcher::emit_tool_event(
                        &app,
                        &final_session_id,
                        step_id,
                        &instruction,
                        "finish_challenged",
                        Some("finish".to_string()),
                        Some(false),
                        &format!(
                            "终态审查未通过（{}分）：发现 {} 项阻塞性缺陷，已打回自愈",
                            review_res.score,
                            review_res.blocking_issues.len()
                        ),
                        None,
                        None,
                    );
                    context.record_step_outcome(false);
                } else {
                    // 达到 3 次熔断上限：一律转人工仲裁，不再按风险分级自动放行。
                    // "低风险自动带警告交付"会把未修复的硬伤静默标成 succeeded——
                    // 阻塞性缺陷未收敛时，成功交付的唯一合法路径是用户显式仲裁。
                    let risk_flags =
                        crate::agent::finalization::detect_risk_flags(&context.new_affected_paths());
                    let human_waived = checkpoint
                        .human_waiver_text()
                        .map_or(false, |w| !w.trim().is_empty());
                    if human_waived {
                        reviewer_warnings_delivered = Some(review_res.clone());
                        crate::agent::telemetry::record_gate_fire(
                            state.inner(),
                            &crate::agent::telemetry::GateFireRecord {
                                run_id: &run_id,
                                session_id: &final_session_id,
                                step_id: step_id as i64,
                                gate: "finish_review",
                                route: "finish_review:human_arbitrated",
                            },
                        );
                        crate::agent::dispatcher::emit_tool_event(
                            &app,
                            &final_session_id,
                            step_id,
                            &instruction,
                            "succeeded",
                            Some("finish".to_string()),
                            Some(true),
                            &format!(
                                "终态审查经人工仲裁放行（重试已用尽）：仍有 {} 项未收敛缺陷，已随交付如实披露",
                                review_res.blocking_issues.len()
                            ),
                            None,
                            None,
                        );
                    } else {
                        reviewer_challenged = true;
                        checkpoint.finish_reviewed = true;
                        crate::agent::telemetry::record_gate_fire(
                            state.inner(),
                            &crate::agent::telemetry::GateFireRecord {
                                run_id: &run_id,
                                session_id: &final_session_id,
                                step_id: step_id as i64,
                                gate: "finish_review",
                                route: if risk_flags.is_empty() {
                                    "finish_review:exhausted_escalated"
                                } else {
                                    "finish_review:high_risk_escalated"
                                },
                            },
                        );
                        let issues_text = review_res
                            .blocking_issues
                            .iter()
                            .enumerate()
                            .map(|(i, b)| {
                                format!(
                                    "{}. 【{}】{}\n   👉 修复要求: {}",
                                    i + 1,
                                    b.file,
                                    b.issue,
                                    b.requirement
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        context.add_error_feedback(&format!(
                            "【⚠️ 终态审查连续 3 次未通过，已熔断升级人工仲裁，finish 未生效！】\n审查模型（{}）评分: {}/100\n审查评价: {}\n{}仍未解决的阻塞性缺陷：\n{}\n\n【必须采取的行动】：\n- 若缺陷属实：继续修复后重试 finish；\n- 若认为审查意见有误或风险可接受：调用 ask_human 向用户呈报缺陷清单申请人工仲裁（用户批准后重试 finish 即放行）；\n- 严禁未经人工仲裁反复提交 finish 消耗步骤预算。",
                            review_res.model_id,
                            review_res.score,
                            review_res.summary,
                            if risk_flags.is_empty() {
                                String::new()
                            } else {
                                format!("\n检测到高风险改动信号：{}\n", risk_flags.join(", "))
                            },
                            issues_text
                        ));
                        crate::agent::dispatcher::emit_tool_event(
                            &app,
                            &final_session_id,
                            step_id,
                            &instruction,
                            "finish_challenged",
                            Some("finish".to_string()),
                            Some(false),
                            &format!(
                                "终态审查连续 3 次未通过{}，已熔断升级人工仲裁",
                                if risk_flags.is_empty() {
                                    String::new()
                                } else {
                                    format!("（{}）", risk_flags.join(", "))
                                }
                            ),
                            None,
                            None,
                        );
                        context.record_step_outcome(false);
                    }
                }
            } else {
                // 审查 100% 通过（或无代码修改 / 仅次要建议放行）
                // 落盘通过态收据（观测向）：复用防敷衍逻辑要求"上次未通过"才复用
                // （reviewer_gatekeeper 侧条件含 !last_res.is_passed()），
                // 存 passed 结果不会误触发复用拦截。
                checkpoint.last_reviewed_diff_hash = Some(review_res.diff_hash.clone());
                checkpoint.last_review_result = Some(review_res.clone());
                crate::agent::telemetry::record_gate_fire(
                    state.inner(),
                    &crate::agent::telemetry::GateFireRecord {
                        run_id: &run_id,
                        session_id: &final_session_id,
                        step_id: step_id as i64,
                        gate: "finish_review",
                        route: "finish_review:passed",
                    },
                );
                // 宿主状态机：复位重试计数并推进当前目标块至 Completed，激活下一块
                if let Some(block) = context.active_target_block_mut() {
                    block.reset_review_retries();
                }
                context.block_manager.advance_to_next();
                crate::agent::dispatcher::emit_tool_event(
                    &app,
                    &final_session_id,
                    step_id,
                    &instruction,
                    "succeeded",
                    Some("finish".to_string()),
                    Some(true),
                    &output_text,
                    None,
                    None,
                );
            }
            }
            }
        } else if is_prepare_finish && result.success {
            // ── 终态预检：与 finish 同一套门禁，只沉淀收据、不提交终态 ──
            // 就绪收据在 diff/scope/契约未变时被 finish 直接复用，不重复烧验证与终审。
            output_text = precheck::run_prepare_finish_precheck(
                &app,
                state.inner(),
                &context,
                &mut checkpoint,
                &run_id,
                &final_session_id,
                step_id,
                &workspace_dir,
                &git_baseline,
                run_started_at,
                &model_routing,
                &mut interrupt_rx,
                &open_todos,
                &gate_notices,
                &instruction,
            )
            .await;
        } else if !is_finish {
            // 只有当常规步骤产生了新的真实副作用写入时，才视为进入了新的交付周期，重置质询标记与计数；
            // 纯只读或临时草稿写入（session_tmp）不重置 finish_reviewed / finish_challenge_count，防止草稿修改打乱交付周期。
            if result.success && is_write_or_side_effect_tool(&instruction) {
                let is_tmp_write = instruction.get_tool().eq_ignore_ascii_case("local_file")
                    && crate::agent::tools::local_file::is_dispatch_result_tmp(&result);
                if !is_tmp_write {
                    checkpoint.finish_reviewed = false;
                    checkpoint.finish_challenge_count = 0;
                }
            }
        }
        let is_effectively_challenged = finish_challenged || reviewer_challenged;
        if is_effectively_challenged {
            let detail = context
                .turns_history
                .last()
                .and_then(|m| m.content.as_str())
                .unwrap_or("【⚠️ 交付门禁拦截打回】任务未满足验收条件，finish 未生效，请依据门禁提示完成修复后重试。");
            output_text = detail.to_string();
        }
        let outcome_str = if is_effectively_challenged {
            "finish_challenged"
        } else if result.success {
            "succeeded"
        } else {
            "failed"
        };
        let model_output_text = output_text.clone();
        let mcp_evidence =
            crate::agent::dispatcher::take_mcp_durable_output(&mut result.view);
        let has_mcp_evidence = mcp_evidence.is_some();
        let durable_output_text =
            mcp_evidence.unwrap_or_else(|| model_output_text.clone());
        let output_id = crate::agent::run_trace::save_step_output_with_meta(
            state.inner(),
            &final_session_id,
            step_id,
            &instruction.get_tool(),
            &instruction.description,
            outcome_str,
            &durable_output_text,
        )
        .unwrap_or_else(|e| {
            eprintln!("⚠️ 无法保存 step_output: {}", e);
            "unknown_output_id".to_string()
        });
        context.add_step(&instruction, &model_output_text, &output_id);
        let dedup = deduplicate_mcp_output(
            state.inner(),
            &final_session_id,
            has_mcp_evidence,
            &instruction,
            outcome_str,
            &durable_output_text,
            &model_output_text,
            &run_trace_steps,
            &mut context,
            &output_id,
        );
        output_text = dedup.displayed_output.clone();
        let canonical_output_id = dedup
            .canonical_output_id
            .clone()
            .unwrap_or_else(|| output_id.clone());
        let evidence_output_id = if dedup.call_hash.is_some() {
            canonical_output_id.clone()
        } else {
            output_id.clone()
        };

        // 🌟 工具执行失败时，自动沉淀到「避坑教训库 (Failed Attempts)」，供后续步骤长效避坑
        if !result.success {
            let error_summary = if !result.stderr.trim().is_empty() {
                result.stderr.trim().to_string()
            } else {
                result.stdout.lines().take(3).collect::<Vec<_>>().join(" ")
            };
            let short_error = if error_summary.chars().count() > 160 {
                format!("{}...", error_summary.chars().take(160).collect::<String>())
            } else {
                error_summary
            };
            context.record_failed_attempt(
                step_id,
                instruction.get_tool(),
                instruction.get_action(),
                &instruction.get_params(),
                short_error,
                Some(evidence_output_id.clone()),
            );
        }

        if let Some(block) = context.active_target_block_mut() {
            block.on_step_finish();
        }

        // 读取型工具：把本次读取的 output_id 回填进重复读取台账，
        // 让后续拦截回执指向持久化句柄（脱水/压缩后仍可用 task_store 定向回查）
        if result.success {
            context.note_read_output_id(&instruction, &workspace_dir, &evidence_output_id);
        }

            post_step::backfill_todo_evidence(&mut context, &mut instruction, &evidence_output_id);

            post_step::record_lesson_lineage(
                state.inner(),
                &final_session_id,
                step_id,
                &instruction,
                &result,
            );
        // ──────────────────────────────────────────────────────────────────

            post_step::record_side_effect_or_verify(
                &mut context,
                step_id,
                &instruction,
                &result,
                &app,
                &final_session_id,
            );

        context.current_observation = output_text.clone();
        // 复盘轨迹使用与 step_new 相同的语义字段，但与 Agent prompt/checkpoint 隔离。
        // 这使历史会话既能恢复思考、计划和工具调用，又不会放大运行时上下文。
        let metrics_json = step_metrics_json(outcome.token_usage.as_ref(), outcome.thinking_duration_ms, outcome.tool_duration_ms);

        run_trace_steps.push(json!({
            "id": step_id,
            "description": instruction.description.clone(),
            "reflection": instruction.reflection.clone(),
            "thought": instruction.thought.clone(),
            "finding": instruction.finding.clone(),
            "finding_kind": instruction.finding_kind.clone(),
            "thinking": crate::agent::run_trace::clip_thinking_for_trace(&outcome.thinking_text),
            "tool": instruction.get_tool(),
            "action": instruction.get_action(),
            "command": instruction.get_params(),
            "params": instruction.get_params(),
            "plan_id": context.active_plan_id().unwrap_or_default(),
            "plan_title": context.active_plan_title().map(str::to_string).unwrap_or_else(|| crate::agent::context::types::clip_plan_title(context.effective_goal())),
            "status": outcome_str,
            "output": output_text.clone(),
            "output_id": output_id.clone(),
            "mcp_call_hash": dedup.call_hash.clone(),
            "mcp_raw_output_hash": dedup.raw_output_hash.clone(),
            "mcp_model_output_hash": dedup.model_output_hash.clone(),
            "canonical_output_id": dedup.call_hash.as_ref().map(|_| canonical_output_id.clone()),
            "duplicate_of_step": dedup.duplicate_of_step,
            "view": result.view.clone(),
            "resource_scope_revision": context.resource_scope.revision,
            "metrics": metrics_json,
            "token_usage": metrics_json,
            "timestamp": Utc::now().timestamp_millis(),
        }));
        // 增量持久化：每步单行 INSERT 进 agent_run_steps，替代整写 steps_json 的
        // O(n²) 序列化+整行重写。steps_json 列仅在启动/终态快照整写一次，
        // 读者（load_run_steps / 监控 SQL / 检索索引）按行优先、列回退。
        if let Some(step_value) = run_trace_steps.last() {
            if let Err(error) = crate::agent::run_trace::append_step(
                state.inner(),
                &run_id,
                (run_trace_steps.len() - 1) as i64,
                step_value,
            ) {
                eprintln!("[run-trace] 无法追加步骤快照: {error}");
            }
        }
        // 工具结果已经进入可恢复上下文。先提交 durable phase/cursor，再通知前端；
        // 这样崩溃恢复不会把已经完成的普通步骤当作待执行，也不会把 finish 误续跑。
        let is_actually_completed = is_finish && result.success && !is_effectively_challenged;
        let mut committed_emitted_finish: Option<serde_json::Value> = None;
        if is_actually_completed {
            committed_emitted_finish = Some(finalize::commit_completed_run(
                state.inner(),
                &mut checkpoint,
                &context,
                &run_id,
                &final_session_id,
                &goal,
                &run_trace_steps,
                &instruction,
                &output_text,
                &conversation_model_id,
                reviewer_warnings_delivered.as_ref(),
            )?);
        } else {
            // 被质询或失败的 finish 也要推进 durable cursor，恢复后继续下一轮而不是误判终态。
            checkpoint.complete_step(step_id, &context);
            save_checkpoint(state.inner(), &mut checkpoint)?;
        }

        // --- 前端通知 + 任务完成判定（合并） ---
        app.emit(&format!("agent-context-{}", final_session_id), &context)
            .map_err(|e| e.to_string())?;

        if is_actually_completed {
            advance_run_state(
                &app,
                &final_session_id,
                &mut run_machine,
                AgentRunEvent::BeginFinalizing,
            )?;
            let emitted_finish = committed_emitted_finish
                .take()
                .unwrap_or_else(|| json!({ "summary": output_text }));

            let complete_msg = if reviewer_warnings_delivered.is_some() {
                "任务已交付（审查达到重试上限，附带未收敛警告）"
            } else {
                "任务已由 AI 标记完成"
            };

            app.emit(
                &format!("agent-progress-{}", final_session_id),
                json!({
                    "type": "complete",
                    "message": complete_msg,
                    "success": true,
                    "output": output_text,      // 原始 JSON 字符串（兼容旧逻辑）
                    "finish": emitted_finish,   // 结构化成果对象，含 details/summary/artifacts
                }),
            )
            .map_err(|e| e.to_string())?;
            // 自动登记运行期间新生成的工作区文件为产物：微信"完成后回推文件"
            // 不再依赖模型记得调用 register_file（漏登记 = 用户只收到文字收不到文件）
            let registered = if is_explicit_workspace {
                crate::agent::artifact_manager::auto_register_run_artifacts(
                    state.inner(),
                    &final_session_id,
                    Some(&run_id),
                    &workspace_dir,
                    run_started_at,
                )
            } else {
                Vec::new()
            };
            if !registered.is_empty() {
                println!(
                    "[run-trace] 自动登记 {} 个运行期产物: {}",
                    registered.len(),
                    registered
                        .iter()
                        .map(|a| a.display_name.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            break;
        }

        // 普通步骤：发 step_done / step_error
        app.emit(
            &format!("agent-progress-{}", final_session_id),
            json!({
                "type": if result.success { "step_done" } else { "step_error" },
                "step_id": step_id,
                "description": instruction.description,
                "finding": instruction.finding.as_deref(),
                "finding_kind": instruction.finding_kind.as_deref(),
                "output": output_text,
                "metrics": metrics_json,
                "token_usage": metrics_json
            }),
        )
        .map_err(|e| e.to_string())?;

        sleep(Duration::from_millis(500)).await;
    }

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
        advance_run_state(
            &app,
            &final_session_id,
            &mut run_machine,
            AgentRunEvent::Fail {
                reason: terminal_error.clone(),
            },
        )?;
        checkpoint.mark_failed(terminal_error.clone(), &context);
        save_checkpoint(state.inner(), &mut checkpoint)?;
        let failure_completion = json!({ "summary": terminal_error });
        if let Err(error) = crate::agent::run_trace::upsert_snapshot(
            state.inner(),
            &run_id,
            &final_session_id,
            &goal,
            "failed",
            &run_trace_steps,
            Some(&failure_completion),
        ) {
            eprintln!("[run-trace] 无法保存失败快照: {error}");
        }
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

#[cfg(test)]
mod budget_tests;

#[cfg(test)]
mod gate_deadlock_tests;

#[cfg(test)]
mod artifact_bridge_tests;

#[cfg(test)]
mod path_scope_manifest_tests;
