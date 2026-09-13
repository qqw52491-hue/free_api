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
use crate::agent::types::{AgentInstruction, TodoItem, RUN_INTERRUPTED_ROUTE};
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

/// Agent 每个任务最多执行步数（可按项目需求调整）
const MAX_STEPS: usize = 300;
/// 正常会话只做短暂态请求重试；后台回归可显式提高到上限以避免 Provider 波动中断取证。
pub(crate) const DEFAULT_TRANSIENT_LLM_REQUEST_RETRIES: u32 = 3;
const MAX_TRANSIENT_LLM_REQUEST_RETRIES: u32 = 100;

/// 仅将协议明确定义为未完成的 Todo 视为终态质询信号。
/// 未知状态不做猜测，`done` 与 `failed` 则已是模型显式关闭的事项。

/// 推进后端运行状态，并向前端发送独立于旧进度事件的权威状态快照。
/// 旧的 agent-progress-* 事件保持不变，以便前端逐步迁移。
pub fn advance_run_state(
    app: &AppHandle,
    session_id: &str,
    machine: &mut AgentRunMachine,
    event: AgentRunEvent,
) -> Result<(), String> {
    let snapshot = machine.transition(event).map_err(|err| err.to_string())?;
    app.emit(&format!("agent-run-state-{}", session_id), snapshot)
        .map_err(|err| err.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn run_agent_main_loop(
    app: AppHandle,
    state: State<'_, DbState>,
    registry_state: State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    model_routing: crate::agent::router::ModelRoutingConfig,
    goal: String,
    auto_pilot: bool,
    session_id: Option<String>,
    log_context: Option<bool>,
    transient_llm_request_retries: Option<u32>,
    attachments: Option<Vec<crate::commands::chat::ParsedAttachmentResult>>,
    plan_mode: Option<String>,
) -> Result<(), String> {
    run_agent_main_loop_in_workspace(
        app,
        state,
        registry_state,
        model_routing,
        goal,
        auto_pilot,
        session_id,
        log_context,
        transient_llm_request_retries,
        attachments,
        None,
        None,
        plan_mode,
    )
    .await
}

/// 仅供受宿主校验的后台评测使用；普通会话始终沿用进程当前工作区。
pub(crate) async fn run_agent_main_loop_in_workspace(
    app: AppHandle,
    state: State<'_, DbState>,
    registry_state: State<'_, std::sync::Arc<tokio::sync::Mutex<PluginRegistry>>>,
    model_routing: crate::agent::router::ModelRoutingConfig,
    goal: String,
    auto_pilot: bool,
    session_id: Option<String>,
    log_context: Option<bool>,
    transient_llm_request_retries: Option<u32>,
    attachments: Option<Vec<crate::commands::chat::ParsedAttachmentResult>>,
    workspace_override: Option<std::path::PathBuf>,
    run_id_override: Option<String>,
    plan_mode: Option<String>,
) -> Result<(), String> {
    let final_session_id = session_id.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis().to_string())
            .unwrap_or_else(|_| "default".to_string())
    });
    let run_id = run_id_override.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let retries = transient_llm_request_retries
        .unwrap_or(DEFAULT_TRANSIENT_LLM_REQUEST_RETRIES)
        .clamp(1, MAX_TRANSIENT_LLM_REQUEST_RETRIES);

    let run_guard = crate::agent::channel::SessionRunGuard::new(
        &app,
        &final_session_id,
        &run_id,
        &goal,
        retries,
    );

    let result = run_agent_main_loop_in_workspace_inner(
        app,
        state,
        registry_state,
        model_routing,
        goal,
        auto_pilot,
        final_session_id,
        log_context,
        retries,
        attachments,
        workspace_override,
        run_id,
        plan_mode,
    )
    .await;

    run_guard.finish(&result);
    result
}

/// 审查者故障转移重派（invalid_output / timed_out 时走这里）。
/// 返回 Ok(Some(new_run_id)) = 已重派；Ok(None) = 不满足重派条件（封顶/已批准/无派发记录）。
/// 【冒烟实测抓到的坑】failover 原来只挂在 wait_jobs 结果路径上——超时结果若经
/// 通用后台信箱（poll_pending_inbox）路径消费，会绕过 failover 且无条件置 consumed，
/// 主 Agent 将永远等不到备用审查员的结论。两条路径必须共用本函数。
pub(crate) async fn try_plan_reviewer_failover(
    app: &AppHandle,
    state: &DbState,
    run_id: &str,
    session_id: &str,
    workspace_dir: &std::path::Path,
    checkpoint: &mut AgentCheckpoint,
    interrupt_rx: watch::Receiver<u64>,
    // 当前仅 invalid_output/timed_out 共用同一重派动作；保留参数供 〇·二 待议的
    // "invalid_output 纳入退款"设计分化行为（届时启用）
    _error_kind: &str,
) -> Result<Option<String>, String> {
    if checkpoint.plan_reviewer_failover_count >= 2
        || checkpoint.plan_reviewer_last_dispatch.is_none()
        || checkpoint.plan_phase.is_approved()
    {
        return Ok(None);
    }
    let mut dispatch = match checkpoint.plan_reviewer_dispatch_snapshot() {
        Some(d) => d,
        None => return Ok(None),
    };
    let (failover_model, model_name) = {
        let conn = state.0.lock().map_err(|err| err.to_string())?;
        let m = crate::agent::router::reviewer_failover_model(&conn, &dispatch.model);
        let name = m.as_deref().and_then(|id| {
            conn.query_row("SELECT name FROM models WHERE id = ?1", rusqlite::params![id], |r| r.get::<_, String>(0)).ok()
        });
        (m, name)
    };
    if let Some(model) = failover_model {
        dispatch.model = model;
    }
    if let Some(obj) = dispatch.params.as_object_mut() {
        let label = if let Some(ref mn) = model_name {
            format!("🛡️ 故障转移备选审查官 ({})", mn)
        } else {
            "🛡️ 故障转移备选审查官".to_string()
        };
        obj.insert("role_label".to_string(), serde_json::json!(label));
    }
    match dispatch
        .spawn(
            Some(app.clone()),
            state,
            run_id,
            session_id,
            workspace_dir,
            interrupt_rx,
        )
        .await
    {
        Ok(new_run_id) => {
            checkpoint.plan_reviewer_failover_count += 1;
            checkpoint.set_plan_reviewer_dispatch_snapshot(&dispatch);
            Ok(Some(new_run_id))
        }
        Err(spawn_err) => Err(spawn_err),
    }
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
    let workspace_dir = workspace_override
        .or(project_workspace)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    if !workspace_dir.is_dir() {
        return Err(format!("Agent 工作区不可用：{}", workspace_dir.display()));
    }
    // 运行起始时间：成功完成时自动登记运行期间新生成的工作区产物（微信回推文件链路）
    let run_started_at = chrono::Utc::now();
    // 捕获任务起点 Git 基准快照（用于 finish 时进行精准增量 Diff 审查）
    let git_baseline = crate::agent::reviewer_gatekeeper::GitBaseline::capture(&workspace_dir);
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

        let mut mcp_lines = Vec::new();
        for (p_name, p_client) in &registry.clients {
            let tools = crate::agent::mcp_catalog::cached_tools(&p_client.config)
                .unwrap_or_else(|| p_client.last_known_tools());
            let names = tools
                .iter()
                .map(|t| format!("{}/{}", p_name, t.name))
                .collect::<Vec<_>>();
            if !names.is_empty() {
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
        // 全量动态扫描 prompts/skills/ 目录下所有 Skill 手册并提取/升级智能能力卡
        crate::agent::skill_context::skill_catalog_string(&skills_dir, &[], maybe_model_cfg)
    };

    let project_resource_boundary = if is_project_session {
        "\n\n## 项目工作区与受管资源\n当前会话已绑定专属项目工作区。你可以在该项目工作区内正常使用 search、外部 MCP 工具以及各类子工兵进行工程分析与开发。同时可调用 `project_resources.list_resources()` 获取重点受管文件，或在生成重要项目产物时调用 `project_resources.register_resource(path, label)` 登记为跨会话共享受管资源。"
    } else if has_workspace_override {
        "\n\n## 独立临时工作区 (Scratch Workspace)\n当前会话处于独立临时工作区。该目录与用户的任何主工程项目源码完全物理隔离，仅用于一次性任务演示、临时探索与临时交付物存放，请在该独立目录下执行操作，严禁向用户的其他工程源码目录随意写入。"
    } else {
        "\n\n## 通用会话模式 (无绑定工程)\n当前会话未选定具体工程项目。凡未明确要求存放于工程中的临时文件、数据导出或中间脚本，一律写入 `tmp/` 目录并通过 `artifact_manager` 登记交付，严禁直接散落在当前运行根目录下。"
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
    let mut manifest_text = if is_explicit_workspace || auto_pilot {
        format!(
            "【当前会话可操作文件边界与作用域 (Operational Path Scope)】\n\
             • 项目物理工作区: `{}`\n\
               - 相对路径默认基于此目录解析（如 \"src/main.rs\"、\"package.json\"）；修改已有文件优先调用 `local_file` 的 `edit` 局部替换，禁止动辄全量重写。\n\
               - 门禁与台账规则: 对项目源码的写操作受阶段审查门禁保护，成功写入将记入不可逆副作用台账。\n\
             • 会话专属临时目录: `{}`\n\
               - 快捷路由: 相对路径以 \"tmp/\" 或 \"./tmp/\" 开头会自动映射路由至此目录。\n\
               - 临时与未明确路径产物隔离: 凡未明确指定项目路径的临时文件、数据导出或中间草稿，统一写入此目录（路径如 \"tmp/filename.ext\"），严禁直接写入项目物理工作区根目录！写入后可调用 `artifact_manager(action: \"register_file\", path: \"tmp/filename.ext\")` 上屏供用户下载或系统程序打开。\n\
               - 草稿免审: 任务推演计划、分析草稿、临时数据均建议写入此目录；临时草稿免审豁免，不消耗测试配额，不会阻塞交付。\n\
             • 物理牢笼约束: 任何超出上述双白名单的物理路径访问均会被 path_guard 拦截拒绝。\n",
            workspace_dir.display(),
            session_tmp_dir.display()
        )
    } else {
        format!(
            "【当前会话可操作文件边界与作用域 (Operational Path Scope)】\n\
             • 通用自由会话（未绑定项目工作区）\n\
               - 当前对话为通用会话模式，未绑定具体项目工作区，工作区保持为空。请勿臆造或宣称已绑定任何本地项目工程目录。\n\
             • 会话专属临时目录: `{}`\n\
               - 快捷路由: 相对路径以 \"tmp/\" 或 \"./tmp/\" 开头会自动映射路由至此目录。\n\
               - 临时产物与草稿隔离: 凡未明确要求存放于项目工程中的临时文件、数据导出或中间脚本，统一写入此目录（路径如 \"tmp/filename.ext\"），严禁直接写入宿主当前运行目录！写入后可调用 `artifact_manager(action: \"register_file\", path: \"tmp/filename.ext\")` 上屏供用户下载或系统程序打开。\n\
               - 草稿免审: 任务推演计划、分析草稿、临时数据均建议写入此目录；临时草稿免审豁免，不消耗测试配额，不会阻塞交付。\n\
             • 物理牢笼约束: 任何超出临时目录白名单的物理路径访问均会被 path_guard 拦截拒绝。\n",
            session_tmp_dir.display()
        )
    };

    if is_project_session {
        if project_resources.is_empty() {
            manifest_text.push_str("- 当前项目暂无登记的受管数据文件。当在工作区生成重要产物或分析报告时，可调用 `project_resources.register_resource(path, label)` 登记为跨会话共享资源。\n");
        } else {
            manifest_text.push_str(&format!("- 本项目已登记受管数据文件（共 {} 个，已建立资源句柄可直接读取分析）：\n", project_resources.len()));
            for r in &project_resources {
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
                if !goal.trim().is_empty() && cp.data.user_goal != goal {
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
    while checkpoint.step_id < MAX_STEPS {
        let step_id = checkpoint.step_id;

        // 🔄 异步子任务结果轮询（审查/工兵/侦察）：自动注入父 Agent 上下文
        if let Ok(completed) = crate::agent::child_reviewer::poll_completed_delivery_receipts(state.inner(), &run_id, None) {
            if !completed.is_empty() {
                let mut injection = String::new();
                let git_available = crate::agent::child_writer::is_git_available(&workspace_dir).await;
                for receipt in completed {
                    // 阶段审查结果走独立语义（不占用方案审查状态，基础设施失败不烧预算且视为未审）
                    if checkpoint.matches_stage_review_run(&receipt.child_run_id)
                        || Some(receipt.child_run_id.as_str())
                            == checkpoint.stage_review_rebuttal_run_id.as_deref()
                    {
                        let oracle_index = crate::agent::eval_oracle::query_active_progress()
                            .await
                            .map(|(idx, _, _)| idx as u32);
                        let note = crate::agent::stage_review::apply_stage_receipt(
                            &mut checkpoint,
                            &receipt,
                            &context,
                            oracle_index,
                        );
                        injection.push_str(&format!(
                            "\n【📋 阶段审查 (child_run_id: {})】\n{}\n",
                            receipt.child_run_id, note
                        ));
                        continue;
                    }
                    match receipt.kind.as_str() {
                        "writer" => {
                            let mut writer_res = receipt.as_writer_result();
                            if let Some(ref mut wr) = writer_res {
                                // 🌟 核心：在父 Agent 消费回执的瞬间现算指纹，闭合 TOCTOU 竞争窗口！
                                crate::agent::child_writer::refresh_file_conflicts(&mut wr.changed_files, &workspace_dir, git_available).await;
                                injection.push_str(&format!(
                                    "\n【🛠️ 写入工兵已完成 (child_run_id: {})】\n状态: {}, 测试: {}\n",
                                    wr.child_run_id, wr.status, wr.test_status
                                ));
                                if !wr.test_output_tail.is_empty() {
                                    injection.push_str(&format!("测试输出摘要:\n{}\n", wr.test_output_tail));
                                }
                                injection.push_str("修改文件与冲突状态（主工作区实时现算）:\n");
                                for cf in &wr.changed_files {
                                    let status_label = cf.conflict_status.map(|s| s.label()).unwrap_or("未知");
                                    injection.push_str(&format!("- {} [{}]\n", cf.path, status_label));
                                    if !cf.diff.is_empty() {
                                        injection.push_str(&format!("```diff\n{}\n```\n", cf.diff));
                                    }
                                }
                                if let Some(ref pr) = wr.patch_ref {
                                    injection.push_str(&format!("完整补丁工件: 可用 artifact_manager(action: \"get\", id: \"{}\") 查看\n", pr));
                                }
                                if !wr.findings.is_empty() {
                                    injection.push_str(&format!("修改说明:\n- {}\n", wr.findings.join("\n- ")));
                                }
                            } else {
                                let findings = receipt.findings();
                                let formatted_findings = if findings.is_empty() {
                                    "（无具体发现）".to_string()
                                } else {
                                    findings.iter().map(|f| f.display_line()).collect::<Vec<_>>().join("\n- ")
                                };
                                injection.push_str(&format!(
                                    "\n【🛠️ 写入工兵已完成 (child_run_id: {})】\n结论: {}\n发现:\n- {}\n",
                                    receipt.child_run_id, receipt.confidence(), formatted_findings
                                ));
                            }
                        }
                        "worker" => {
                            let (confidence, formatted_findings, action_logs) = if let Some(ref wrk) = receipt.as_worker_result() {
                                let findings_str = if wrk.findings.is_empty() {
                                    "（无具体发现）".to_string()
                                } else {
                                    wrk.findings.join("\n- ")
                                };
                                (wrk.confidence.clone(), findings_str, wrk.action_log.clone())
                            } else {
                                let findings = receipt.findings();
                                let findings_str = if findings.is_empty() {
                                    "（无具体发现）".to_string()
                                } else {
                                    findings.iter().map(|f| f.display_line()).collect::<Vec<_>>().join("\n- ")
                                };
                                (receipt.confidence(), findings_str, Vec::new())
                            };
                            injection.push_str(&format!(
                                "\n【🕵️ 只读侦察任务已完成 (child_run_id: {})】\n结论: {}\n发现:\n- {}\n",
                                receipt.child_run_id, confidence, formatted_findings
                            ));
                            if !action_logs.is_empty() {
                                injection.push_str(&format!("执行动作记录:\n- {}\n", action_logs.join("\n- ")));
                            }
                        }
                        _ => {
                            let findings = receipt.findings();
                            let formatted_findings = if findings.is_empty() {
                                "（无具体发现）".to_string()
                            } else {
                                findings.iter().map(|f| f.display_line()).collect::<Vec<_>>().join("\n- ")
                            };
                            let routing_opt = receipt.routing();
                            let routing_str = routing_opt.as_deref().unwrap_or("certain");
                            let scorecard_desc = if let Some(ref sc) = receipt.scorecard() {
                                let tc_str = sc.test_coverage.map_or_else(|| "N/A".to_string(), |tc| format!("{}/100", tc));
                                format!(
                                    "\n审查评分卡: 综合平均 {} 分 (正确性: {}/100, 破坏性风险: {}/100, 代码规范: {}/100, 测试覆盖: {}) -> 门禁状态: [{}]\n",
                                    sc.average_score(), sc.correctness, sc.risk, sc.style, tc_str,
                                    if sc.passes_gate() { "✅合格放行" } else { "🚨未达标拦截(均分需>=75且risk需>=60)" }
                                )
                            } else {
                                String::new()
                            };
                            injection.push_str(&format!(
                                "\n【🔍 子审查代理已完成 (child_run_id: {})】\n结论: {}\n建议路由: [{}]{}成本: 输入 {} / 输出 {} tokens, 耗时 {}ms\n发现:\n- {}\n",
                                receipt.child_run_id, receipt.confidence(), routing_str, scorecard_desc, receipt.prompt_tokens, receipt.completion_tokens, receipt.duration_ms, formatted_findings
                            ));
                            // 审查发现自动闭环：登记为待办条目，finish 门禁要求逐条闭合——
                            // 修复第八轮实测的"pro 审查员发现真缺口但被主脑静默丢弃"问题
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
                                        continue; // 去重：同一发现不重复登记
                                    }
                                    context.todo_list.push(crate::agent::types::TodoItem {
                                        id: next_id + offset,
                                        status: "pending".to_string(),
                                        description: desc,
                                        evidence_output_id: None,
                                    });
                                }
                            }
                            // 【审查者故障转移链】invalid_output → 同参数重试；超时 → 切换跨平台备用审查员。
                            // 封顶 2 次；两次都失败 → 退款 + 升级人工（ask_human 路径不变）。
                            let failover_eligible = matches!(
                                receipt.error_kind.as_deref(),
                                Some("invalid_output") | Some("timed_out") | Some("timeout")
                            );
                            if failover_eligible {
                                match try_plan_reviewer_failover(
                                    &app,
                                    state.inner(),
                                    &run_id,
                                    &final_session_id,
                                    &workspace_dir,
                                    &mut checkpoint,
                                    interrupt_rx.clone(),
                                    receipt.error_kind.as_deref().unwrap_or("?"),
                                )
                                .await
                                {
                                    Ok(Some(new_run_id)) => {
                                        injection.push_str(&format!(
                                            "\n🔁 审查者故障转移：上次结论异常（{}），已自动改用备用审查员重派 (child_run_id: {})\n",
                                            receipt.error_kind.as_deref().unwrap_or("?"), new_run_id
                                        ));
                                        continue;
                                    }
                                    Ok(None) => {}
                                    Err(spawn_err) => {
                                        injection.push_str(&format!(
                                            "\n⚠️ 审查者故障转移重派失败: {}（转人工裁决路径不受影响）\n", spawn_err
                                        ));
                                    }
                                }
                            }
                            // 基础设施失败：退款、不算消费过（超时不是审查，不应烧预算），同时记录批次失败收口
                            if receipt.is_infra_failure() {
                                checkpoint.record_reviewer_failure_for_run(
                                    &receipt.child_run_id,
                                    receipt.error_kind.as_deref().unwrap_or("infra_failure"),
                                );
                                checkpoint.refund_infra_failed_review_round(receipt.error_kind.as_deref());
                            } else {
                                let delivery = receipt.as_reviewer_delivery().unwrap_or_else(|| {
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
                                });
                                checkpoint.record_reviewer_delivery_for_run(Some(&receipt.child_run_id), &delivery);
                                injection.push_str(&format!(
                                    "\n📊 本轮异议动态: 新增 {} / 撤回 {} / 维持 {}（连续多轮撤回≈新增且总数不降 = 审查空转，请评估转人工）\n",
                                    checkpoint.review_objection_added,
                                    checkpoint.review_objection_withdrawn,
                                    checkpoint.review_objection_maintained
                                ));
                                if !delivery.withdrawn_findings.is_empty() {
                                    injection.push_str(&format!(
                                        "\n♻️ 审查者已撤回异议: {}（以最新裁决为准，撤回理由见审查结论）\n",
                                        delivery.withdrawn_findings.join(", ")
                                    ));
                                }
                            }
                        }
                    }
                    let missing = receipt.missing_evidence();
                    if !missing.is_empty() {
                        injection.push_str(&format!("缺失证据/未完成项: {}\n", missing.join(", ")));
                    }
                    if let Some(ref err) = receipt.error_kind {
                        injection.push_str(&format!("异常类别: {}（子任务未正常完成，请谨慎对待输出）\n", err));
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
                    if job.job_type == "child_reviewer" || job.job_type.contains("reviewer") {
                        // 与 wait_jobs 结果路径共用 failover 链：失败类结论（超时/非法输出）
                        // 先尝试重派备用审查员，不置 consumed——否则主 Agent 永远等不到结论
                        let failed_kind = job
                            .error_kind
                            .as_deref()
                            .filter(|k| {
                                matches!(*k, "timed_out" | "invalid_output" | "timeout")
                            });
                        if let Some(kind) = failed_kind {
                            match try_plan_reviewer_failover(
                                &app,
                                state.inner(),
                                &run_id,
                                &final_session_id,
                                &workspace_dir,
                                &mut checkpoint,
                                interrupt_rx.clone(),
                                kind,
                            )
                            .await
                            {
                                Ok(Some(new_run_id)) => {
                                    job_injection.push_str(&format!(
                                        "\n🔁 审查者故障转移：上次结论异常（{}），已自动改用备用审查员重派 (child_run_id: {})\n",
                                        kind, new_run_id
                                    ));
                                    continue;
                                }
                                Ok(None) => {
                                    checkpoint.plan_reviewer_consumed = true;
                                }
                                Err(spawn_err) => {
                                    checkpoint.plan_reviewer_consumed = true;
                                    job_injection.push_str(&format!(
                                        "\n⚠️ 审查者故障转移重派失败: {}（转人工裁决路径不受影响）\n",
                                        spawn_err
                                    ));
                                }
                            }
                        } else {
                            checkpoint.plan_reviewer_consumed = true;
                        }
                    }
                    job_injection.push_str(&format!(
                        "\n【⚡ 后台异步任务已完成 (ID: {}, 类型: {})】\n任务描述: {}\n执行状态: {}\n耗时: {}ms\n输出/结论:\n{}\n",
                        job.job_id,
                        job.job_type,
                        job.title,
                        job.status,
                        job.duration_ms,
                        if job.result_json.is_empty() { "（无附加输出）" } else { &job.result_json }
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

        // 宿主状态机：追踪当前目标块步数并触发硬顶防翻车警报
        let hard_limit_warning = if let Some(block) = context.active_target_block_mut() {
            block.on_step_start();
            if block.is_hard_limit_exceeded() {
                Some((block.id.clone(), block.steps_count, block.hard_step_limit))
            } else {
                None
            }
        } else {
            None
        };

        if let Some((id, count, limit)) = hard_limit_warning {
            context.add_error_feedback(&format!(
                "【⚠️ 目标块步数超限警报】当前活跃目标块 [{}] 已经执行了 {} 步（达到硬顶上限 {} 步）。\n请停止零散动作，检查当前方案是否受阻。若受阻请在 progress_summary 中指出并申请验收或进入收敛。",
                id, count, limit
            ));
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
            Ok(res) => res,
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
        let (mut instruction, result) = (outcome.instruction, outcome.result);
        // 结构化路由信号：按真实 DispatchResult 记录成功/失败（替代对反馈文本 ❌ 的脆弱匹配）
        context.record_step_outcome(result.success);
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
            apply_instruction_state_updates(&mut context, &instruction);

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
        if let Some(reason) = crate::agent::stage_review::should_fire(&checkpoint, &context, &instruction, &model_routing) {
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
            context.add_error_feedback(&format!(
                "【⚠️ 交付门禁拦截打回：任务尚未满足结项标准，finish 未生效！】\n【未闭合项/证据缺失】：\n{}\n任务仍在运行中。请勿在未闭合上述事项时草率结项！\n【必须采取的行动】：\n- 若为写入操作，请执行测试验证并在 change_verification_update 中以 verified + evidence_refs 闭合；\n- 若某项不再适用，请在 review_contract_update 或 todo_update 中显式标为 waived / failed 并说明理由；\n- 状态更新并完成验证后，方可再次提交 finish 交付；如需先评估就绪状态而不提交终态，可单独一步输出 prepare_finish 做预检。",
                blocker_summary
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
                );
            } else {
            // Level 0 机械门禁：零 Token 快速拦截（Scope 边界越界等）
            let scope_violations = crate::agent::reviewer_gatekeeper::mechanical_precheck_violations(
                &workspace_dir,
                context.active_target_block(),
            );
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
                );
                context.record_step_outcome(false);
            } else {
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
                    context.add_error_feedback(&format!(
                        "【⚠️ 交付门禁拦截打回：契约绑定的宿主验证命令未通过，finish 未生效！】\n{}\n（说明：命令退出码即为判定依据，冻结脚本不可修改）。\n【必须采取的行动】：请真实修复对应代码使验证通过后重试；严禁未验证直接提交交付。",
                        obligation_failures.join("\n")
                    ));
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
                    );
                    context.record_step_outcome(false);
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
                    checkpoint.approved_target_files.as_deref(),
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
                    );
                    context.record_step_outcome(false);
                } else {
                    // 达到 3 次熔断上限：低风险带警告交付；高风险（鉴权/密钥/迁移/并发/安全策略）
                    // 不享受熔断放行额度——继续按未通过处理并引导 ask_human 人工仲裁。
                    let risk_flags =
                        crate::agent::finalization::detect_risk_flags(&context.new_affected_paths());
                    if risk_flags.is_empty() {
                        reviewer_warnings_delivered = Some(review_res.clone());
                        crate::agent::dispatcher::emit_tool_event(
                            &app,
                            &final_session_id,
                            step_id,
                            &instruction,
                            "succeeded",
                            Some("finish".to_string()),
                            Some(true),
                            &format!(
                                "终态审查熔断放行（已重试 3 次）：仍有 {} 项未收敛缺陷已附带警告交付",
                                review_res.blocking_issues.len()
                            ),
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
                                route: "finish_review:high_risk_escalated",
                            },
                        );
                        context.add_error_feedback(&format!(
                            "【⚠️ 高风险交付审查未收敛（已重试 3 次），禁止自动带警告交付！】\n检测到高风险改动信号：{}\n仍有 {} 项阻塞性缺陷未解决。\n【必须采取的行动】：高风险改动不享受熔断放行额度——请继续修复缺陷后重试 finish；若认为审查意见有误，请调用 ask_human 申请人工仲裁。",
                            risk_flags.join(", "),
                            review_res.blocking_issues.len()
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
                                "高风险改动审查未收敛（{}），已熔断升级人工仲裁",
                                risk_flags.join(", ")
                            ),
                            None,
                        );
                        context.record_step_outcome(false);
                    }
                }
            } else {
                // 审查 100% 通过（或无代码修改 / 仅次要建议放行）
                checkpoint.last_reviewed_diff_hash = None;
                checkpoint.last_review_result = None;
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
                );
            }
            }
            }
        } else if is_prepare_finish && result.success {
            // ── 终态预检：与 finish 同一套门禁，只沉淀收据、不提交终态 ──
            // 就绪收据在 diff/scope/契约未变时被 finish 直接复用，不重复烧验证与终审。
            let receipt = crate::agent::finalization::evaluate_prepare_finish(
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
                instruction.prepare_finish.as_ref(),
            )
            .await;
            checkpoint.pending_finalization = Some(receipt.clone());
            crate::agent::telemetry::record_gate_fire(
                state.inner(),
                &crate::agent::telemetry::GateFireRecord {
                    run_id: &run_id,
                    session_id: &final_session_id,
                    step_id: step_id as i64,
                    gate: "finalization",
                    route: if receipt.is_ready() {
                        "finalization:prepared_ready"
                    } else {
                        "finalization:prepared_blocked"
                    },
                },
            );
            output_text = if receipt.is_ready() {
                format!(
                    "✅ 终态预检就绪（receipt {}）：所有门禁已通过且证据已冻结。\n现在调用 finish 将直接复用本收据完成提交——diff/scope/契约未变时不再重复验证与终审。",
                    receipt.receipt_id
                )
            } else {
                format!(
                    "⚠️ 终态预检未就绪（receipt {}）：\n{}\n请逐项闭合后重新 prepare_finish，或修复后直接调用 finish。",
                    receipt.receipt_id,
                    receipt
                        .blockers
                        .iter()
                        .map(|b| format!("- {}", b))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            };
            crate::agent::dispatcher::emit_tool_event(
                &app,
                &final_session_id,
                step_id,
                &instruction,
                "succeeded",
                Some("prepare_finish".to_string()),
                Some(receipt.is_ready()),
                &output_text,
                None,
            );
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
        let output_id = crate::agent::run_trace::save_step_output_with_meta(
            state.inner(),
            &final_session_id,
            step_id,
            &instruction.get_tool(),
            &instruction.description,
            outcome_str,
            &output_text,
        )
        .unwrap_or_else(|e| {
            eprintln!("⚠️ 无法保存 step_output: {}", e);
            "unknown_output_id".to_string()
        });

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
                Some(output_id.clone()),
            );
        }

        if let Some(block) = context.active_target_block_mut() {
            block.on_step_finish();
        }

        context.add_step(&instruction, &output_text, &output_id);

        // ── 防虚假勾选：将当步真实 output_id 回填到标记为 done 但缺少证据的 Todo（需在 output_id 已知后执行）
        if !instruction.todo_update.is_empty() {
            for item in &mut instruction.todo_update {
                if item.status == "done" && item.evidence_output_id.is_none() {
                    item.evidence_output_id = Some(output_id.clone());
                }
            }
            // 证据关联后同步更新 todo_list（走监管继承守卫：[审查发现] 禁止被静默剔除）
            crate::agent::finish_guard::apply_todo_update_guarded(&mut context, &instruction.todo_update);
        }

        // ── Lesson 血缘召回记录：若当步工具是 search，将命中的 lesson 类型结果写入血缘表 ───────
        if instruction.get_tool().eq_ignore_ascii_case("search") && result.success {
            if let Ok(search_result) = serde_json::from_str::<serde_json::Value>(&result.stdout) {
                let recalled_lesson_ids: Vec<String> = search_result
                    .get("hits")
                    .and_then(|h| h.as_array())
                    .map(|hits| {
                        hits.iter()
                            .filter_map(|h| {
                                let kind = h.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                                if kind == "lesson" || kind == "summary" {
                                    h.get("id_or_path")
                                        .and_then(|id| id.as_str())
                                        .map(String::from)
                                } else {
                                    None
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if !recalled_lesson_ids.is_empty() {
                    crate::agent::lesson::record_lesson_recall(
                        state.inner(),
                        &final_session_id,
                        step_id,
                        &recalled_lesson_ids,
                    );
                }
            }
        }

        // ── Lesson 显式血缘因果反馈打分 ────────────────────────────────────
        if let Some(ref applied) = instruction.applied_lessons {
            if !applied.is_empty() {
                crate::agent::lesson::feedback_on_applied_lessons(
                    state.inner(),
                    &final_session_id,
                    step_id,
                    applied,
                    result.success,
                );
            }
        }
        // ──────────────────────────────────────────────────────────────────

        // --- 记录不可逆副作用安全台账 (Side-Effects Ledger) ---
        // 关键不可逆操作（如写入文件、修改数据库、执行写操作命令等）成功执行后记录台账，
        // 纯只读命令（如 git log/show/diff, search 等）及临时草稿 (session_tmp) 不入台账；参数保持完整合法 JSON，绝不使用 ... 截断元数据
        let is_tmp_write = instruction.get_tool().eq_ignore_ascii_case("local_file")
            && crate::agent::tools::local_file::is_dispatch_result_tmp(&result);
        if result.success
            && !is_tmp_write
            && crate::agent::context::AuditLedger::is_mutating_side_effect_with_action(
                &instruction.get_tool(),
                &instruction.get_action(),
                &instruction.get_params(),
            )
        {
            let safe_params = crate::agent::context::AuditLedger::format_side_effect_params(
                &instruction.get_params(),
            );
            let affected_paths = crate::agent::change_verification::extract_file_paths_from_params(
                &instruction.get_params(),
            );
            context.record_side_effect_with_paths(
                step_id,
                instruction.get_tool(),
                format!("{}: {}", instruction.description, safe_params),
                affected_paths,
            );
        } else if result.success {
            // 🌟 写入验证智能自愈与自动感知 (Auto-verification):
            // 当存在未闭合的本地写入，且当前执行的命令是测试/验证命令且执行成功时，系统自动闭合为 verified！
            try_auto_verify_change(&mut context, step_id, &instruction, &app, &final_session_id);
        }

        context.current_observation = output_text.clone();
        // 复盘轨迹使用与 step_new 相同的语义字段，但与 Agent prompt/checkpoint 隔离。
        // 这使历史会话既能恢复思考、计划和工具调用，又不会放大运行时上下文。
        let metrics_json = outcome.token_usage.as_ref().map(|u| {
            json!({
                "prompt_tokens": u.prompt_tokens,
                "completion_tokens": u.completion_tokens,
                "total_tokens": u.total_tokens,
                "cached_tokens": u.cached_tokens,
                "cache_hit_rate": u.cache_hit_rate,
                "tokens_per_second": u.tokens_per_second,
                "ttft_ms": u.ttft_ms,
                "thinking_duration_ms": outcome.thinking_duration_ms,
                "tool_duration_ms": outcome.tool_duration_ms,
                "total_duration_ms": outcome.thinking_duration_ms + outcome.tool_duration_ms,
            })
        });

        run_trace_steps.push(json!({
            "id": step_id,
            "description": instruction.description.clone(),
            "reflection": instruction.reflection.clone(),
            "thought": instruction.thought.clone(),
            "tool": instruction.get_tool(),
            "action": instruction.get_action(),
            "command": instruction.get_params(),
            "params": instruction.get_params(),
            "plan_id": context.active_plan_id().unwrap_or_default(),
            "plan_title": context.active_plan_title().map(str::to_string).unwrap_or_else(|| crate::agent::context::types::clip_plan_title(&context.user_goal)),
            "status": outcome_str,
            "output": output_text.clone(),
            "resource_scope_revision": context.resource_scope.revision,
            "metrics": metrics_json,
            "token_usage": metrics_json,
            "timestamp": Utc::now().timestamp_millis(),
        }));
        if let Err(error) = crate::agent::run_trace::upsert_snapshot(
            state.inner(),
            &run_id,
            &final_session_id,
            &goal,
            "running",
            &run_trace_steps,
            None,
        ) {
            eprintln!("[run-trace] 无法保存步骤快照: {error}");
        }
        // 工具结果已经进入可恢复上下文。先提交 durable phase/cursor，再通知前端；
        // 这样崩溃恢复不会把已经完成的普通步骤当作待执行，也不会把 finish 误续跑。
        let is_actually_completed = is_finish && result.success && !is_effectively_challenged;
        let mut committed_emitted_finish: Option<serde_json::Value> = None;
        if is_actually_completed {
            checkpoint.mark_done(&context);
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

            let persisted_summary = finish_payload
                .get("summary")
                .and_then(|value| value.as_str())
                .or_else(|| {
                    finish_payload
                        .get("details")
                        .and_then(|value| value.as_str())
                })
                .unwrap_or(&output_text)
                .to_string();
            // 完成区域展示模型自己的最终回复；若存在 3 次审查熔断遗留警告，注入到 emitted_finish
            let mut emitted_finish = match finish_payload {
                serde_json::Value::Object(object) => serde_json::Value::Object(object),
                _ => json!({ "summary": persisted_summary }),
            };
            if let Some(ref warnings) = reviewer_warnings_delivered {
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
                                    state.inner(),
                                    &final_session_id,
                                    Some(&run_id),
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
                state.inner(),
                &mut checkpoint,
                &run_id,
                &final_session_id,
                &goal,
                &run_trace_steps,
                &emitted_finish,
                &persisted_summary,
                &conversation_model_id,
            )?;
            committed_emitted_finish = Some(emitted_finish);
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
        let terminal_error = format!("任务在达到最大步数 {} 后未完成", MAX_STEPS);
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

fn try_auto_verify_change(
    context: &mut context::SandwichContext,
    step_id: usize,
    instruction: &AgentInstruction,
    app: &AppHandle,
    session_id: &str,
) {
    if context.side_effects.len() <= context.inherited_side_effects_count
        || context
            .change_verification
            .as_ref()
            .map_or(false, crate::agent::change_verification::ChangeVerification::is_resolved)
    {
        return;
    }

    let Some(cmd) =
        crate::agent::change_verification::extract_command_from_params(&instruction.get_params())
    else {
        return;
    };

    let is_test_cmd = crate::agent::change_verification::is_test_or_verification_command(&cmd);
    let is_inferred_cmd = context
        .inferred_verification_command()
        .as_deref()
        .map_or(false, |ic| {
            let lower_cmd = cmd.trim().to_lowercase();
            let lower_ic = ic.trim().to_lowercase();
            lower_cmd.contains(&lower_ic) || lower_ic.contains(&lower_cmd)
        });
    let matches_contract_verify = context.review_contract.as_ref().map_or(false, |rc| {
        rc.obligations.iter().any(|o| {
            o.verify_command
                .as_deref()
                .map_or(false, |vc| vc.trim() == cmd.trim())
        })
    });

    if is_test_cmd || matches_contract_verify || is_inferred_cmd {
        let snippet = if cmd.chars().count() > 60 {
            format!("{}...", cmd.chars().take(60).collect::<String>())
        } else {
            cmd
        };
        context.change_verification = Some(crate::agent::change_verification::ChangeVerification {
            status: "verified".to_string(),
            evidence_refs: vec![format!("step_{} ({})", step_id, snippet)],
            waived_reason: None,
        });
        let _ = app.emit(
            &format!("agent-log-{}", session_id),
            format!(
                "🧪 系统自动感知：检测到验证命令 `{}` 成功执行，已自动标记写入验证通过 (verified)",
                snippet
            ),
        );
    }
}

