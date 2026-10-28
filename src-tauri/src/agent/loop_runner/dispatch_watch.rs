//! 持续巡检臂：watch_start / watch_stop。

use crate::agent::checkpoint::{save_checkpoint, AgentCheckpoint};
use crate::agent::child_reviewer::result_as_dispatch_output;
use crate::agent::context::resource_scope::ResourceOperation;
use crate::agent::control::{take_pending_interrupt, HumanControl};
use crate::agent::dispatcher::{
    emit_tool_event, is_finish_instruction, is_write_or_side_effect_tool, run_agent_step,
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


pub(super) async fn dispatch_watch_start(
    ctx: &mut DispatchCtx<'_>,
    inst: &AgentInstruction,
) -> Result<ArmOutcome, String> {
    Ok(ArmOutcome::Done({
        let params = inst.get_params();
        let watch_outcome = async {
            let request = crate::agent::watch_engine::parse_watch_request(&params)?;
            crate::agent::watch_engine::begin_watch(
                ctx.state.inner(),
                ctx.parent_run_id,
                ctx.final_session_id,
                &request,
                Some(ctx.workspace_dir),
            )
            .await
        };
        match watch_outcome.await {
            Ok(watch_id) => {
                if let Some(watch_state) =
                    crate::agent::watch_engine::load_watch_state(ctx.state.inner(), &watch_id)
                {
                    let db_clone = crate::db::DbState(ctx.state.inner().0.clone());
                    let session_clone = ctx.final_session_id.clone();
                    let watch_clone = watch_id.clone();
                    let workspace_clone = ctx.workspace_dir.to_path_buf();
                    tokio::spawn(crate::agent::watch_engine::run_watch_loop(
                        db_clone,
                        Some(ctx.app.clone()),
                        session_clone,
                        watch_clone,
                        watch_state.interval_secs,
                        watch_state.last_snapshot,
                        Some(workspace_clone),
                    ));
                }
                let _ = ctx.app.emit(
                    &format!("agent-log-{}", ctx.final_session_id),
                    format!("👁️ 持续监听已启动: {}", watch_id),
                );
                DispatchResult { view: None,
                    stdout: format!(
                        "持续监听已启动，watch_id: {}。此后每个巡检周期内若目标发生变化，变化事件会作为 watch_event 自动注入你的上下文（无需轮询读取）；若长时间无变化则不会产生任何事件。任务收尾前请调用 watch_stop 释放该监听，否则 finish 会被拦截。",
                        watch_id
                    ),
                    stderr: String::new(),
                    success: true,
                    route: "watch:started".to_string(),
                }
            }
            Err(error) => DispatchResult { view: None,
                stdout: String::new(),
                stderr: error,
                success: false,
                route: "watch:rejected".to_string(),
            },
        }
    }))
}


pub(super) async fn dispatch_watch_stop(
    ctx: &mut DispatchCtx<'_>,
    inst: &AgentInstruction,
) -> Result<ArmOutcome, String> {
    Ok(ArmOutcome::Done({
        let params = inst.get_params();
        if let Some(result) = crate::agent::tools::input_validation::native_rejection("watch_stop", "", &params) {
            return Ok(ArmOutcome::Early(result));
        }
        let stop_outcome = crate::agent::watch_engine::stop_watches_with_summary(
            ctx.state.inner(),
            ctx.parent_run_id,
            params.get("watch_id").and_then(|v| v.as_str()),
        );
        match stop_outcome {
            Ok(summary) => DispatchResult { view: None,
                stdout: summary,
                stderr: String::new(),
                success: true,
                route: "watch:stopped".to_string(),
            },
            Err(error) => DispatchResult { view: None,
                stdout: String::new(),
                stderr: error,
                success: false,
                route: "watch:stop_failed".to_string(),
            },
        }
    // --- B2.2 L3 隔离写入 worker：私有副本上改码跑测试，diff 回执，root 仍是真实工作区唯一写入者 ---
    }))
}
