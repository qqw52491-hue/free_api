use crate::agent::checkpoint::{load_checkpoint, AgentPhase};
use crate::agent::control::{cancel_agent_run, HumanControl};
use crate::agent::evaluation;
use crate::agent::main_loop::run_agent_main_loop_in_workspace;
use crate::agent::mcp::PluginRegistry;
use crate::agent::router::ModelRoutingConfig;
use crate::agent::run_trace;
use crate::db::DbState;
use chrono::Utc;
use rusqlite::params;
use rusqlite::OptionalExtension;
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex as StdMutex,
};
use tauri::{AppHandle, Emitter, Listener, Manager};
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout, Duration};

/// 默认运行 20 分钟；到期后通过现有取消通道在下一个安全边界停止。
pub const DEFAULT_EVALUATION_DURATION_MINUTES: u64 = 20;
/// 长程收敛场景需要 >20 分钟（完工后自查阶段常再花 5+ 分钟）；
/// 交接单协议按 60 分钟预算编排，20 上限曾把 19.4 分钟的运行在 20:00 整点砍成 cancelled。
const MAX_EVALUATION_DURATION_MINUTES: u64 = 60;
const STOP_GRACE_PERIOD: Duration = Duration::from_secs(120);
/// 无人值守评测的自动仲裁上限：ask_human 超过此次数仍无法推进则安全停止
/// （防止坏配对下无限"补充预算→再失败"循环烧钱）。
const MAX_AUTO_ARBITRATIONS: usize = 3;
const HUMAN_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(250);
const STRUCTURED_MONITOR_INTERVAL: Duration = Duration::from_secs(1);
const EVALUATION_ARTIFACTS_DIR: &str = "test-artifacts";

// 回归评测固定使用本机 SQLite 中的火山订阅模型，避免同名模型跨平台串路由。
const DEFAULT_EVALUATION_ROUTE_PRESET: &str = "default";
/// 不传具名预设时，后台回归必须尊重调用方当前选择的 UUID 路由。
/// 这让模型对比不再依赖每新增一个模型都改一次常量表。
const REQUESTED_EVALUATION_ROUTE_PRESET: &str = "requested";
const GPT56_TERRA_LUNA_ROUTE_PRESET: &str = "gpt56_terra_luna";
const VOLC_K2_PRO_ROUTE_PRESET: &str = "volc_k2_pro";
const VOLC_SINGLE_PRO_ROUTE_PRESET: &str = "volc_single_pro";
const EVALUATION_PLATFORM_NAME: &str = "火山订阅";
const EVALUATION_REASONING_MODEL_NAME: &str = "deepseek-v4-pro";
const EVALUATION_EXECUTION_MODEL_NAME: &str = "minimax-m3";
const EVALUATION_GLM_MODEL_NAME: &str = "glm-5.3";
const EVALUATION_GLM_ALIAS_MODEL_NAME: &str = "glm-latest";
const EVALUATION_KIMI_CODE_MODEL_NAME: &str = "kimi-k2.7-code";
const GPT56_PLATFORM_NAME: &str = "中转站";
const GPT56_TERRA_MODEL_NAME: &str = "gpt-5.6-terra";
const GPT56_LUNA_MODEL_NAME: &str = "gpt-5.6-luna";

#[derive(Debug, Clone, Copy)]
struct EvaluationRouteSpec {
    name: &'static str,
    /// 推理（pro）模型候选链：(平台名, 模型名)，按优先级排列，依次解析首个已启用者。
    reasoning_candidates: &'static [(&'static str, &'static str)],
    /// 执行（flash）模型候选链。
    execution_candidates: &'static [(&'static str, &'static str)],
}

fn evaluation_route_spec(requested: Option<&str>) -> Result<EvaluationRouteSpec, String> {
    match requested.unwrap_or(DEFAULT_EVALUATION_ROUTE_PRESET) {
        DEFAULT_EVALUATION_ROUTE_PRESET => Ok(EvaluationRouteSpec {
            name: DEFAULT_EVALUATION_ROUTE_PRESET,
            reasoning_candidates: &[(EVALUATION_PLATFORM_NAME, EVALUATION_REASONING_MODEL_NAME)],
            execution_candidates: &[(EVALUATION_PLATFORM_NAME, EVALUATION_EXECUTION_MODEL_NAME)],
        }),
        GPT56_TERRA_LUNA_ROUTE_PRESET => Ok(EvaluationRouteSpec {
            name: GPT56_TERRA_LUNA_ROUTE_PRESET,
            reasoning_candidates: &[(GPT56_PLATFORM_NAME, GPT56_TERRA_MODEL_NAME)],
            execution_candidates: &[(GPT56_PLATFORM_NAME, GPT56_LUNA_MODEL_NAME)],
        }),
        // 火山订阅优先的混合路由：pro 用 DeepSeek V4 Pro（备选 GLM），
        // flash 用 Kimi K2.7 Code（较弱的 M3/Lite 不参与）；订阅模型不可用时
        // 逐级回落，最终落到中转站 gpt-5.6（该通道无稳定前缀缓存，仅作兜底）。
        VOLC_K2_PRO_ROUTE_PRESET => Ok(EvaluationRouteSpec {
            name: VOLC_K2_PRO_ROUTE_PRESET,
            reasoning_candidates: &[
                (EVALUATION_PLATFORM_NAME, EVALUATION_REASONING_MODEL_NAME),
                (EVALUATION_PLATFORM_NAME, EVALUATION_GLM_MODEL_NAME),
                (EVALUATION_PLATFORM_NAME, EVALUATION_GLM_ALIAS_MODEL_NAME),
                (GPT56_PLATFORM_NAME, GPT56_TERRA_MODEL_NAME),
            ],
            execution_candidates: &[
                (EVALUATION_PLATFORM_NAME, EVALUATION_KIMI_CODE_MODEL_NAME),
                (GPT56_PLATFORM_NAME, GPT56_LUNA_MODEL_NAME),
            ],
        }),
        // 单模型路由：规划与执行都用 DeepSeek V4 Pro。消除双模型轮换的缓存分账
        // （v5 实测：连续同模型 82% vs 切换后 65%），代价是执行步也用 pro 价，
        // 但高缓存命中下 cached tokens 便宜，长任务总成本未必更高——适合 A/B 对比。
        VOLC_SINGLE_PRO_ROUTE_PRESET => Ok(EvaluationRouteSpec {
            name: VOLC_SINGLE_PRO_ROUTE_PRESET,
            reasoning_candidates: &[
                (EVALUATION_PLATFORM_NAME, EVALUATION_REASONING_MODEL_NAME),
                (EVALUATION_PLATFORM_NAME, EVALUATION_GLM_MODEL_NAME),
                (EVALUATION_PLATFORM_NAME, EVALUATION_GLM_ALIAS_MODEL_NAME),
                (GPT56_PLATFORM_NAME, GPT56_TERRA_MODEL_NAME),
            ],
            execution_candidates: &[
                (EVALUATION_PLATFORM_NAME, EVALUATION_REASONING_MODEL_NAME),
                (GPT56_PLATFORM_NAME, GPT56_LUNA_MODEL_NAME),
            ],
        }),
        unknown => Err(format!("未知评测路由预设：{}", unknown)),
    }
}

/// 独立后台评测的启动回执。会话 ID 与评测 ID 均可用于定位实时事件、日志与最终报告。
#[derive(Debug, Clone, Serialize)]
pub struct BackgroundEvaluationStart {
    pub session_id: String,
    pub evaluation_id: String,
    pub scenario_id: String,
    pub route_preset: String,
    pub max_duration_minutes: u64,
    /// SQLite 模型 UUID：固定为火山订阅的 DeepSeek V4 Pro。
    pub reasoning_model_id: String,
    /// SQLite 模型 UUID：固定为火山订阅的 MiniMax M3。
    pub execution_model_id: String,    /// 写入型评测的判分 oracle（127.0.0.1 随机端口 + nonce；None = 本场景无 oracle）
    pub verify_port: Option<u16>,
    pub verify_nonce: Option<String>,

}

/// 在不依赖 UI、模型或 MCP 的情况下监督一个后台工作单元。
/// 返回 true 表示工作单元达到时限且已执行安全停止回调。
async fn supervise_worker<F, C, CFut>(
    worker: &mut Pin<Box<F>>,
    deadline: Duration,
    stop_grace_period: Duration,
    on_deadline: C,
) -> bool
where
    F: Future,
    C: FnOnce() -> CFut,
    CFut: Future<Output = ()>,
{
    let timed_out = tokio::select! {
        _ = worker.as_mut() => false,
        _ = sleep(deadline) => true,
    };
    if timed_out {
        on_deadline().await;
        let _ = timeout(stop_grace_period, worker.as_mut()).await;
    }
    timed_out
}

fn agent_paused_reason(payload: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(payload).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("agent_paused") {
        return None;
    }
    Some(
        value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Agent 请求人工协助，但事件未提供原因。")
            .to_string(),
    )
}

fn is_agent_paused_payload(payload: &str) -> bool {
    agent_paused_reason(payload).is_some()
}

#[derive(Debug, Clone, Serialize)]
struct EvaluationMonitorSnapshot {
    schema_version: u32,
    observed_at: String,
    evaluation_id: String,
    session_id: String,
    trace_count: usize,
    latest_trace_id: Option<String>,
    latest_trace_status: Option<String>,
    latest_trace_updated_at: Option<String>,
    latest_step_count: usize,
    latest_step: Option<Value>,
    checkpoint_phase: Option<String>,
    checkpoint_version: Option<i64>,
    checkpoint_updated_at: Option<String>,
    human_wait_reason: Option<String>,
}

fn read_structured_monitor_snapshot(
    db: &DbState,
    evaluation_id: &str,
    session_id: &str,
    event_pause_reason: Option<String>,
) -> Result<EvaluationMonitorSnapshot, String> {
    // 轻量化采样：monitor 每 1s 跑一次，此前整载全部 trace 版本（steps_json 随步数
    // 无界膨胀）+ 反序列化整个 checkpoint，长会话后期每 tick 数十 MB JSON 解析，
    // 22 个并发 tick 打爆 DB 连接池，主循环饿死在连接互斥锁上整轮停摆（实测线程栈）。
    // 快照只需要 ~200 字节的状态字段，全部走 SQL 内提取，绝不进 Rust 反序列化。
    let conn = db.reader()?;
    let trace_row = conn
        .query_row(
            "SELECT id, status, updated_at,
                    CASE WHEN EXISTS(SELECT 1 FROM agent_run_steps WHERE run_id=t.id)
                         THEN (SELECT COUNT(*) FROM agent_run_steps WHERE run_id=t.id)
                         ELSE json_array_length(steps_json) END,
                    COALESCE((SELECT step_json FROM agent_run_steps WHERE run_id=t.id ORDER BY seq DESC LIMIT 1),
                             json_extract(steps_json, '$[#-1]')),
                    (SELECT COUNT(*) FROM agent_run_traces WHERE session_id=t.session_id)
             FROM agent_run_traces t WHERE t.session_id=?1
             ORDER BY t.started_at DESC, t.id DESC LIMIT 1",
            params![session_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let (trace_id, trace_status, trace_updated_at, steps_count, latest_step_raw, trace_count) =
        trace_row.unwrap_or((None, None, None, None, None, 0));
    let latest_step: Option<Value> = latest_step_raw
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok());

    let checkpoint_row = conn
        .query_row(
            "SELECT phase, version, updated_at, json_extract(snapshot_json, '$.last_error')
             FROM agent_checkpoints WHERE run_id=?1 ORDER BY rowid DESC LIMIT 1",
            params![session_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let (checkpoint_phase, checkpoint_version, checkpoint_updated_at, checkpoint_last_error) =
        checkpoint_row
            .map(|(phase, version, updated_at, last_error)| (Some(phase), Some(version), Some(updated_at), last_error))
            .unwrap_or((None, None, None, None));
    // 原语义：仅 WaitUser 阶段的 last_error 作为人工等待原因
    let checkpoint_reason = match checkpoint_phase.as_deref() {
        Some("wait_user") => checkpoint_last_error,
        _ => None,
    };

    Ok(EvaluationMonitorSnapshot {
        schema_version: 1,
        observed_at: Utc::now().to_rfc3339(),
        evaluation_id: evaluation_id.to_string(),
        session_id: session_id.to_string(),
        trace_count: trace_count as usize,
        latest_trace_id: trace_id,
        latest_trace_status: trace_status,
        latest_trace_updated_at: trace_updated_at,
        latest_step_count: steps_count.unwrap_or(0) as usize,
        latest_step: latest_step,
        checkpoint_phase,
        checkpoint_version,
        checkpoint_updated_at,
        human_wait_reason: event_pause_reason.or(checkpoint_reason),
    })
}

fn persist_structured_monitor_snapshot(
    db: &DbState,
    evaluation_id: &str,
    session_id: &str,
    event_pause_reason: Option<String>,
) -> Result<EvaluationMonitorSnapshot, String> {
    let snapshot =
        read_structured_monitor_snapshot(db, evaluation_id, session_id, event_pause_reason)?;
    let project_dir = std::env::current_dir().map_err(|error| error.to_string())?;
    let monitor_dir = project_dir.join("agent_evaluations").join("monitor");
    fs::create_dir_all(&monitor_dir).map_err(|error| error.to_string())?;
    let target = monitor_dir.join(format!("{}.json", evaluation_id));
    let temporary = monitor_dir.join(format!("{}.json.tmp", evaluation_id));
    let payload = serde_json::to_vec_pretty(&snapshot).map_err(|error| error.to_string())?;
    fs::write(&temporary, payload).map_err(|error| error.to_string())?;
    fs::rename(&temporary, &target).map_err(|error| error.to_string())?;
    Ok(snapshot)
}

fn resolve_enabled_model_id(
    db: &DbState,
    platform_name: &str,
    model_name: &str,
) -> Result<String, String> {
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    conn.query_row(
        "SELECT m.id
         FROM models m JOIN platforms p ON p.id = m.platform_id
         WHERE p.name = ?1 AND m.name = ?2 AND m.enabled = 1",
        params![platform_name, model_name],
        |row| row.get::<_, String>(0),
    )
    .map_err(|error| {
        format!(
            "回归评测模型未就绪：平台‘{}’中未找到已启用的模型‘{}’：{}",
            platform_name, model_name, error
        )
    })
}

/// 按候选链依次解析首个已启用的模型；全部不可用时汇总报错，
/// 让调用方清楚知道尝试过哪些 (平台, 模型) 组合。
fn validate_requested_model_id(db: &DbState, model_id: &str, role: &str) -> Result<(), String> {
    if model_id.trim().is_empty() {
        return Err(format!("回归评测未配置 {} 模型", role));
    }
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    let enabled: i64 = conn
        .query_row(
            "SELECT enabled FROM models WHERE id = ?1",
            params![model_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("回归评测 {} 模型不可用：{}", role, error))?;
    if enabled != 1 {
        return Err(format!("回归评测 {} 模型未启用", role));
    }
    Ok(())
}

fn resolve_first_enabled_model_id(
    db: &DbState,
    candidates: &[(&str, &str)],
) -> Result<String, String> {
    let mut attempted: Vec<String> = Vec::new();
    for (platform_name, model_name) in candidates {
        match resolve_enabled_model_id(db, platform_name, model_name) {
            Ok(model_id) => return Ok(model_id),
            Err(_) => attempted.push(format!("‘{}’/‘{}’", platform_name, model_name)),
        }
    }
    Err(format!(
        "回归评测模型未就绪，候选链全部不可用：{}",
        attempted.join(" → ")
    ))
}

fn fixed_evaluation_model_routing(
    db: &DbState,
    mut requested: ModelRoutingConfig,
    route_preset: Option<&str>,
) -> Result<(ModelRoutingConfig, evaluation::EvaluationRoutingMetadata), String> {
    if route_preset.is_none() {
        validate_requested_model_id(db, &requested.pro, "Pro")?;
        validate_requested_model_id(db, &requested.flash, "Flash")?;
        validate_requested_model_id(db, &requested.vision, "Vision")?;
        requested.force_pro_on_first_step = true;
        return Ok((
            requested.clone(),
            evaluation::EvaluationRoutingMetadata {
                preset: REQUESTED_EVALUATION_ROUTE_PRESET.to_string(),
                reasoning_model_id: requested.pro,
                execution_model_id: requested.flash,
            },
        ));
    }

    let spec = evaluation_route_spec(route_preset)?;
    let reasoning_model_id = resolve_first_enabled_model_id(db, spec.reasoning_candidates)?;
    let execution_model_id = resolve_first_enabled_model_id(db, spec.execution_candidates)?;

    requested.pro = reasoning_model_id.clone();
    requested.flash = execution_model_id.clone();
    // 该只读评测不含图像；如意外出现视觉分流，仍落到受核验的推理模型。
    requested.vision = reasoning_model_id.clone();
    requested.force_pro_on_first_step = true;
    Ok((
        requested,
        evaluation::EvaluationRoutingMetadata {
            preset: spec.name.to_string(),
            reasoning_model_id,
            execution_model_id,
        },
    ))
}

fn resolve_isolated_evaluation_workspace(
    workspace_override: Option<String>,
) -> Result<Option<PathBuf>, String> {
    let Some(raw_path) = workspace_override.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    let requested = PathBuf::from(raw_path.trim());
    if !requested.is_absolute() {
        return Err("评测隔离工作区必须是绝对路径".to_string());
    }
    let workspace = requested
        .canonicalize()
        .map_err(|error| format!("评测隔离工作区不可用：{error}"))?;
    if !workspace.is_dir() {
        return Err("评测隔离工作区必须是目录".to_string());
    }
    let project_root = std::env::current_dir()
        .map_err(|error| format!("无法确定项目工作区：{error}"))?
        .canonicalize()
        .map_err(|error| format!("项目工作区不可用：{error}"))?;
    // 默认锁定在 test-artifacts 下；本地实弹评测可通过
    // FREE_API_EVALUATION_WS_ROOT 显式放行项目外的隔离根（避免写入项目目录触发热重载/污染仓库）。
    let allowed_root = match std::env::var("FREE_API_EVALUATION_WS_ROOT")
        .ok()
        .map(|value| PathBuf::from(value.trim().to_string()))
        .filter(|value| !value.as_os_str().is_empty())
    {
        Some(root) => root
            .canonicalize()
            .map_err(|error| format!("评测工作区根（env）不可用：{error}"))?,
        None => project_root
            .join(EVALUATION_ARTIFACTS_DIR)
            .canonicalize()
            .map_err(|error| format!("评测工件目录不可用：{error}"))?,
    };
    if !workspace.starts_with(&allowed_root) {
        return Err(format!(
            "评测隔离工作区必须位于 {} 下",
            allowed_root.display()
        ));
    }
    Ok(Some(workspace))
}

/// 启动独立的真实 Agent 回归评测。
///
/// 此命令不走图形化页面，也不复制或改写普通会话主循环。后台任务从 AppHandle
/// 取得已托管的数据库与 MCP 注册表，并直接复用 run_agent_main_loop；运行结束后只读
/// 分析本次隔离会话的真实日志与轨迹并写入报告。
///
/// 无人值守时若 Agent 调用 ask_human，主循环会发布 agent_paused 事件。本运行器会
/// 自动使用现有取消通道终止该评测并产出报告，而不会等待人类回复。
#[tauri::command]
pub async fn start_background_agent_evaluation(
    app: AppHandle,
    model_routing: ModelRoutingConfig,
    max_duration_minutes: Option<u64>,
    scenario: Option<String>,
    routing_preset: Option<String>,
    workspace_override: Option<String>,
    plan_mode: Option<String>,
    max_steps: Option<u64>,
    max_prompt_tokens: Option<u64>,
) -> Result<BackgroundEvaluationStart, String> {
    let (evaluation_model_routing, routing_metadata) = {
        let state = app.state::<DbState>();
        fixed_evaluation_model_routing(state.inner(), model_routing, routing_preset.as_deref())?
    };
    let reasoning_model_id = routing_metadata.reasoning_model_id.clone();
    let execution_model_id = routing_metadata.execution_model_id.clone();
    let resolved_route_preset = routing_metadata.preset.clone();
    let duration_minutes = max_duration_minutes
        .unwrap_or(DEFAULT_EVALUATION_DURATION_MINUTES)
        .clamp(1, MAX_EVALUATION_DURATION_MINUTES);
    let evaluation_workspace = resolve_isolated_evaluation_workspace(workspace_override)?;
    let session_id = format!("agent-evaluation-{}", uuid::Uuid::new_v4());
    // 普通入口不传场景时保持标准回归；本地自动化可显式用环境变量选择
    // 已注册的只读场景，避免为了测试额外改动 UI 或普通会话。
    let requested_scenario = scenario.or_else(|| {
        std::env::var("FREE_API_EVALUATION_SCENARIO")
            .ok()
            .filter(|value| !value.trim().is_empty())
    });
    if requested_scenario.as_deref() == Some(evaluation::ISOLATED_WRITE_FIXTURE_SCENARIO)
        && evaluation_workspace.is_none()
    {
        return Err("写入型评测必须提供位于 test-artifacts 下的隔离工作区".to_string());
    }
    if requested_scenario.as_deref() == Some(evaluation::CUSTOM_GOAL_SCENARIO)
        && evaluation_workspace.is_none()
    {
        return Err("custom_goal 真实任务必须提供位于评测白名单根内的隔离工作区".to_string());
    }
    let evaluation_run = evaluation::begin_agent_evaluation_for_scenario(
        &session_id,
        requested_scenario.as_deref(),
    )?;
    // 写入型评测启动服务端判分 oracle：题目/输入/期望输出不再落盘，杜绝客户端批量解题
    let oracle = if evaluation_run.scenario_id == evaluation::ISOLATED_WRITE_FIXTURE_SCENARIO {
        let handle = crate::agent::eval_oracle::spawn().map_err(|e| format!("判分 oracle 启动失败: {e}"))?;
        // 注册为活动 oracle：finish 门禁据此把宿主判分进度作为交付的客观事实源
        // （实测教训：无此注册时模型自declared契约只覆盖 stage-01，1/24 交付被判 succeeded）
        crate::agent::eval_oracle::register_active_oracle(handle.port, &handle.nonce);
        Some(handle)
    } else {
        None
    };
    // 修复实测 catch-22：npm test（取题通道）会被写前门禁拦截，模型不看题无法过审。
    // 首阶段题面在启动时注入目标；后续阶段仍由 CURRENT_STAGE.md 在解锁后披露。
    let goal = match (&oracle, evaluation_run.goal.clone()) {
        (Some(_), goal) => format!(
            "{goal}\n\n【当前阶段任务（宿主判分，逐阶段推进）】\n{}\n（完成并 npm test 通过后，CURRENT_STAGE.md 会披露下一阶段题面；答案在宿主侧，禁止尝试伪造判分或修改受保护文件。）",
            crate::agent::eval_oracle::initial_stage_brief()
        ),
        (None, goal) => goal,
    };
    let evaluation_id = evaluation_run.evaluation_id.clone();
    let scenario_id = evaluation_run.scenario_id.clone();
    // 注意：此处不得再用 evaluation_run.goal 覆盖 goal——那会把上面的
    // 首阶段题面注入整个丢弃（实测教训：该覆盖曾让 catch-22 修复静默失效）。

    let event_app = app.clone();
    let worker_app = app.clone();
    let worker_session_id = session_id.clone();
    let worker_evaluation_id = evaluation_id.clone();
    let worker_routing_metadata = routing_metadata.clone();
    let worker_workspace = evaluation_workspace.clone();
    let human_wait_detected = Arc::new(AtomicBool::new(false));
    let human_wait_reason = Arc::new(StdMutex::new(None::<String>));
    let human_wait_for_listener = human_wait_detected.clone();
    let human_wait_reason_for_listener = human_wait_reason.clone();
    let progress_event = format!("agent-progress-{}", session_id);
    let listener_id = app.listen(progress_event, move |event| {
        if let Some(reason) = agent_paused_reason(event.payload()) {
            if let Ok(mut stored_reason) = human_wait_reason_for_listener.lock() {
                *stored_reason = Some(reason);
            }
            human_wait_for_listener.store(true, Ordering::SeqCst);
        }
    });

    tauri::async_runtime::spawn(async move {
        let state_owner = worker_app.clone();
        let state = state_owner.state::<DbState>();
        let registry = state_owner.state::<Arc<Mutex<PluginRegistry>>>();
        // 【冒烟实测抓到的坑】isolated_write_fixture 每次新建 fixture 目录，但 MCP Hub
        // 白名单只含历史手工注册的目录——模型的一切 write_file/shell_exec 都会
        // "Access denied / cwd must be inside allowed workspaces"，npm test 根本跑不起来。
        // 评测启动时宿主自动注册隔离工作区，环境自举不消耗模型步数与预算。
        // 注意：此处不能走全局 registry（评测会话的插件引导尚未执行，registry 仍为空，
        // 实测注册请求从未上线）——直接从 DB 插件表构造临时客户端调用。
        if let Some(ws) = worker_workspace.as_ref() {
            let ws_display = ws.display().to_string();
            let register = || async {
                let managed = crate::commands::load_managed_mcp_plugins(state.inner())?;
                let config = managed
                    .into_iter()
                    .find(|p| p.enabled && p.config.name == "remote-http-mcp")
                    .map(|p| p.config)
                    .ok_or_else(|| "remote-http-mcp 插件未配置".to_string())?;
                let client = crate::agent::mcp::client::McpClient::spawn(config)?;
                client
                    .call_tool(
                        "add_allowed_directory",
                        serde_json::json!({
                            "path": ws_display,
                            "reason": "评测启动：隔离工作区自动注册",
                        }),
                    )
                    .await
                    .map(|_| ())
            };
            match timeout(Duration::from_secs(15), register()).await {
                Ok(Ok(())) => println!("✅ [evaluation] 隔离工作区已注册到 MCP 白名单: {}", ws.display()),
                Ok(Err(e)) => println!("⚠️ [evaluation] 隔离工作区白名单注册失败（模型可自行调用 add_allowed_directory 兜底）: {e}"),
                Err(_) => println!("⚠️ [evaluation] 隔离工作区白名单注册超时（15s），继续启动评测"),
            }
        }
        let mut worker = Box::pin(run_agent_main_loop_in_workspace(
            worker_app.clone(),
            state,
            registry,
            evaluation_model_routing,
            goal,
            true,
            Some(worker_session_id.clone()),
            Some(true),
            Some(20),
            None,
            worker_workspace,
            None,
            plan_mode,
            // 考卷预算：max_steps/max_prompt_tokens 任一触发即在步骤边界收敛出报告
            (max_steps.is_some() || max_prompt_tokens.is_some()).then(|| {
                crate::agent::main_loop::AgentRunBudget {
                    max_steps: max_steps.map(|v| v as usize),
                    max_prompt_tokens: max_prompt_tokens.map(|v| v as i64),
                }
            }),
            Some(crate::agent::conversation::TaskStartMode::New),
        ));

        let monitor_app = event_app.clone();
        let monitor_session_id = worker_session_id.clone();
        let monitor_evaluation_id = worker_evaluation_id.clone();
        let monitor_pause_reason = human_wait_reason.clone();
        let structured_monitor = tauri::async_runtime::spawn(async move {
            loop {
                let event_reason = monitor_pause_reason
                    .lock()
                    .ok()
                    .and_then(|reason| reason.clone());
                let monitor_state = monitor_app.state::<DbState>();
                match persist_structured_monitor_snapshot(
                    monitor_state.inner(),
                    &monitor_evaluation_id,
                    &monitor_session_id,
                    event_reason,
                ) {
                    Ok(snapshot) => {
                        let _ = monitor_app.emit(
                            &format!("agent-evaluation-monitor-{}", monitor_session_id),
                            &snapshot,
                        );
                    }
                    Err(error) => {
                        let _ = monitor_app.emit(
                            &format!("agent-log-{}", monitor_session_id),
                            format!("[回归监控] 读取最新结构化记录失败：{}", error),
                        );
                    }
                }
                sleep(STRUCTURED_MONITOR_INTERVAL).await;
            }
        });

        let human_wait_app = event_app.clone();
        let human_wait_session_id = worker_session_id.clone();
        let human_wait_evaluation_id = worker_evaluation_id.clone();
        let human_wait_reason_for_observer = human_wait_reason.clone();
        // 【无人值守自动仲裁】ask_human 不再必然终止评测：注入合成裁决（方案审查类
        // 暂停自动补 2 轮预算），封顶 3 次后才安全停止。实测教训：pro×glm 坏配对下
        // 模型按门禁指引 ask_human，评测器却因"无人应答"直接取消——问人反而成了死法。
        // FREE_API_EVAL_ASK_HUMAN_MODE=cancel 可回退旧行为。
        let ask_human_auto = std::env::var("FREE_API_EVAL_ASK_HUMAN_MODE")
            .ok()
            .as_deref()
            != Some("cancel");
        let auto_arbitrations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let auto_arbitrations_for_observer = auto_arbitrations.clone();
        let human_wait_observer = tauri::async_runtime::spawn(async move {
            loop {
                if human_wait_detected.load(Ordering::SeqCst) {
                    let reason = human_wait_reason_for_observer
                        .lock()
                        .ok()
                        .and_then(|reason| reason.clone())
                        .unwrap_or_else(|| "Agent 请求人工协助，但未捕获到具体原因。".to_string());
                    let monitor_state = human_wait_app.state::<DbState>();
                    if let Ok(snapshot) = persist_structured_monitor_snapshot(
                        monitor_state.inner(),
                        &human_wait_evaluation_id,
                        &human_wait_session_id,
                        Some(reason.clone()),
                    ) {
                        let _ = human_wait_app.emit(
                            &format!("agent-evaluation-monitor-{}", human_wait_session_id),
                            &snapshot,
                        );
                    }
                    let arbitration_index =
                        auto_arbitrations_for_observer.fetch_add(1, Ordering::SeqCst);
                    let budget_related = reason.contains("审查轮数")
                        || reason.contains("预算已耗尽")
                        || reason.contains("预算上限");
                    if ask_human_auto && arbitration_index < MAX_AUTO_ARBITRATIONS {
                        // 方案审查类暂停：从 checkpoint 退还 2 轮预算后再注入裁决
                        if budget_related {
                            let arb_state = human_wait_app.state::<DbState>();
                            if let Ok(Some(mut cp)) =
                                crate::agent::checkpoint::load_checkpoint(arb_state.inner(), &human_wait_session_id)
                            {
                                cp.plan_reviewer_retry_count =
                                    cp.plan_reviewer_retry_count.saturating_sub(2);
                                cp.plan_reviewer_consumed = false;
                                let _ = crate::agent::checkpoint::save_checkpoint(arb_state.inner(), &mut cp);
                            }
                        }
                        let reply = format!(
                            "【无人值守自动仲裁 {}/{}】人工不在场，按以下裁决继续：{}                             1) 若为审查轮次耗尽——已补充 2 轮方案审查预算，请继续按门禁流程与审查员迭代；\n                             2) 若审查员持续技术故障（invalid_output/超时/连接失败），请在 reflection 中记录后改用其他解套路径，不要反复派发同一审查；\n                             3) 在没有新的实质性阻塞前，禁止再次 ask_human。",
                            arbitration_index + 1,
                            MAX_AUTO_ARBITRATIONS,
                            if budget_related { "本暂停属审查预算耗尽，预算已补充。" } else { "" }
                        );
                        let channels = crate::agent::control::intervention_channels().lock().await;
                        let answered: Result<(), String> = channels
                            .get(&human_wait_session_id)
                            .map(|sender| {
                                sender
                                    .try_send(HumanControl::Answer(reply, None))
                                    .map_err(|e| e.to_string())
                            })
                            .unwrap_or(Err("通道不存在".to_string()));
                        drop(channels);
                        if answered.is_ok() {
                            let _ = human_wait_app.emit(
                                &format!("agent-log-{}", human_wait_session_id),
                                format!(
                                    "[回归评测] 检测到 ask_human：{}；已注入自动仲裁 {}/{}，评测继续。",
                                    reason,
                                    arbitration_index + 1,
                                    MAX_AUTO_ARBITRATIONS
                                ),
                            );
                            human_wait_detected.store(false, Ordering::SeqCst);
                            sleep(HUMAN_WAIT_POLL_INTERVAL).await;
                            continue;
                        }
                    }
                    let _ = human_wait_app.emit(
                        &format!("agent-log-{}", human_wait_session_id),
                        format!(
                            "[回归评测] 检测到 ask_human：{}；已保存最新结构化记录，正在安全停止。",
                            reason
                        ),
                    );
                    let _ = cancel_agent_run(human_wait_session_id.clone()).await;
                    break;
                }
                sleep(HUMAN_WAIT_POLL_INTERVAL).await;
            }
        });

        let _ = supervise_worker(
            &mut worker,
            Duration::from_secs(duration_minutes * 60),
            STOP_GRACE_PERIOD,
            || async {
                let _ = event_app.emit(
                    &format!("agent-log-{}", worker_session_id),
                    "[回归评测] 已到达时限，已请求在下一个安全边界停止。",
                );
                let _ = cancel_agent_run(worker_session_id.clone()).await;
            },
        )
        .await;

        human_wait_observer.abort();
        structured_monitor.abort();
        worker_app.unlisten(listener_id);

        // 结束（或超过安全等待期）后释放主循环借用的受托管状态，再读取同一会话的报告。
        drop(worker);
        let report_state = state_owner.state::<DbState>();
        match evaluation::build_and_persist_report(
            report_state.inner(),
            &worker_session_id,
            Some(&worker_evaluation_id),
            Some(worker_routing_metadata),
        ) {
            Ok(report) => {
                let final_pause_reason = human_wait_reason
                    .lock()
                    .ok()
                    .and_then(|reason| reason.clone());
                match persist_structured_monitor_snapshot(
                    report_state.inner(),
                    &worker_evaluation_id,
                    &worker_session_id,
                    final_pause_reason,
                ) {
                    Ok(snapshot) => {
                        let _ = event_app.emit(
                            &format!("agent-evaluation-monitor-{}", worker_session_id),
                            &snapshot,
                        );
                    }
                    Err(error) => {
                        let _ = event_app.emit(
                            &format!("agent-log-{}", worker_session_id),
                            format!("[回归监控] 写入终态结构化记录失败：{}", error),
                        );
                    }
                }
                let _ = event_app.emit(
                    &format!("agent-log-{}", worker_session_id),
                    format!(
                        "[回归报告] 判定：{}；已写入 {}",
                        report.verdict, report.report_markdown_path
                    ),
                );
            }
            Err(error) => {
                let _ = event_app.emit(
                    &format!("agent-log-{}", worker_session_id),
                    format!("[回归报告生成失败] {}", error),
                );
            }
        }
    });

    Ok(BackgroundEvaluationStart {
        session_id,
        evaluation_id,
        scenario_id,
        route_preset: resolved_route_preset,
        max_duration_minutes: duration_minutes,
        reasoning_model_id,
        execution_model_id,
            verify_port: oracle.as_ref().map(|o| o.port),
        verify_nonce: oracle.as_ref().map(|o| o.nonce.clone()),
})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulated_agent_paused_event_is_detected_without_a_tauri_window() {
        assert!(is_agent_paused_payload(
            r#"{"type":"agent_paused","message":"需要人工确认"}"#
        ));
        assert!(!is_agent_paused_payload(r#"{"type":"agent_progress"}"#));
        assert!(!is_agent_paused_payload("not-json"));
    }

    #[tokio::test]
    async fn simulated_worker_completion_does_not_request_stop() {
        let stop_requested = Arc::new(AtomicBool::new(false));
        let flag = stop_requested.clone();
        let mut worker = Box::pin(async {});

        let timed_out = supervise_worker(
            &mut worker,
            Duration::from_millis(10),
            Duration::from_millis(1),
            move || async move {
                flag.store(true, Ordering::SeqCst);
            },
        )
        .await;

        assert!(!timed_out);
        assert!(!stop_requested.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn simulated_deadline_requests_stop_without_model_or_ui() {
        let stop_requested = Arc::new(AtomicBool::new(false));
        let flag = stop_requested.clone();
        let mut worker = Box::pin(std::future::pending::<()>());

        let timed_out = supervise_worker(
            &mut worker,
            Duration::from_millis(1),
            Duration::from_millis(1),
            move || async move {
                flag.store(true, Ordering::SeqCst);
            },
        )
        .await;

        assert!(timed_out);
        assert!(stop_requested.load(Ordering::SeqCst));
    }

    #[test]
    fn requested_enabled_model_ids_are_preserved_without_a_named_preset() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE models (id TEXT PRIMARY KEY, enabled INTEGER NOT NULL);\n                 INSERT INTO models (id, enabled) VALUES ('glm-test', 1), ('sense-test', 1);",
            )
            .unwrap();
        let db = DbState(Arc::new(StdMutex::new(connection)));
        let requested = ModelRoutingConfig {
            pro: "glm-test".to_string(),
            flash: "sense-test".to_string(),
            vision: "glm-test".to_string(),
            reasoning_effort: "auto".to_string(),
            pro_reasoning_effort: String::new(),
            flash_reasoning_effort: String::new(),
            vision_reasoning_effort: String::new(),
            force_pro_on_first_step: false,
            context_budget_mode: "deep_coding".to_string(),
            state_card_level: String::new(),
            routing_mode: String::new(),
            role_models: Default::default(),
            stage_reviewer_override: None,
            stage_review_committee: false,
            disabled_role_cards: Default::default(),
            auto_cross_family_reviewer: false,
            reviewer_backup_model: String::new(),
        };

        let (resolved, metadata) = fixed_evaluation_model_routing(&db, requested, None).unwrap();

        assert_eq!(metadata.preset, REQUESTED_EVALUATION_ROUTE_PRESET);
        assert_eq!(metadata.reasoning_model_id, "glm-test");
        assert_eq!(metadata.execution_model_id, "sense-test");
        assert_eq!(resolved.pro, "glm-test");
        assert_eq!(resolved.flash, "sense-test");
        assert_eq!(resolved.vision, "glm-test");
        assert!(resolved.force_pro_on_first_step);
    }

    #[test]
    fn named_route_presets_keep_default_and_reject_unknown_names() {
        let default = evaluation_route_spec(None).unwrap();
        assert_eq!(default.name, DEFAULT_EVALUATION_ROUTE_PRESET);
        assert_eq!(
            default.reasoning_candidates,
            &[(EVALUATION_PLATFORM_NAME, EVALUATION_REASONING_MODEL_NAME)]
        );
        assert_eq!(
            default.execution_candidates,
            &[(EVALUATION_PLATFORM_NAME, EVALUATION_EXECUTION_MODEL_NAME)]
        );

        let terra_luna = evaluation_route_spec(Some(GPT56_TERRA_LUNA_ROUTE_PRESET)).unwrap();
        assert_eq!(
            terra_luna.reasoning_candidates,
            &[(GPT56_PLATFORM_NAME, GPT56_TERRA_MODEL_NAME)]
        );
        assert_eq!(
            terra_luna.execution_candidates,
            &[(GPT56_PLATFORM_NAME, GPT56_LUNA_MODEL_NAME)]
        );
        assert!(evaluation_route_spec(Some("arbitrary-model-id")).is_err());
    }

    #[test]
    fn volc_k2_pro_preset_orders_candidates_with_relay_fallback() {
        let spec = evaluation_route_spec(Some(VOLC_K2_PRO_ROUTE_PRESET)).unwrap();
        // pro 首选 DeepSeek V4 Pro，GLM 备选，最终回落中转站 terra。
        assert_eq!(
            spec.reasoning_candidates.first().unwrap(),
            &(EVALUATION_PLATFORM_NAME, EVALUATION_REASONING_MODEL_NAME)
        );
        assert_eq!(
            spec.reasoning_candidates.last().unwrap(),
            &(GPT56_PLATFORM_NAME, GPT56_TERRA_MODEL_NAME)
        );
        // flash 首选 Kimi K2.7 Code（较弱的 M3/Lite 不参与），回落中转站 luna。
        assert_eq!(
            spec.execution_candidates.first().unwrap(),
            &(EVALUATION_PLATFORM_NAME, EVALUATION_KIMI_CODE_MODEL_NAME)
        );
        assert_eq!(
            spec.execution_candidates.last().unwrap(),
            &(GPT56_PLATFORM_NAME, GPT56_LUNA_MODEL_NAME)
        );
        for candidate in spec
            .reasoning_candidates
            .iter()
            .chain(spec.execution_candidates.iter())
        {
            // 候选链里的每一项都必须是真实平台/模型名常量，不允许占位符。
            assert!(!candidate.0.is_empty() && !candidate.1.is_empty());
        }
    }
    #[test]
    fn evaluation_workspace_omitted_keeps_current_workspace_behavior() {
        assert_eq!(resolve_isolated_evaluation_workspace(None).unwrap(), None);
    }
    #[test]
    fn isolated_evaluation_workspace_rejects_relative_paths() {
        let error = resolve_isolated_evaluation_workspace(Some("fixture".to_string())).unwrap_err();
        assert!(error.contains("绝对路径"));
    }
}
