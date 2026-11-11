//! Compatibility for shared Writer dispatch and historical patch application.

use super::*;

pub(super) async fn dispatch_child_writer(
    ctx: &mut DispatchCtx<'_>,
    inst: &AgentInstruction,
) -> Result<ArmOutcome, String> {
    // 机械合入通道：审阅 writer diff 后由宿主把补丁原样搬回真实工作区。
    // 任一文件指纹冲突即整体拒绝，绝不盲目覆盖（详见 apply_writer_result）。
    {
        let top_params = inst.get_params();
        let action_lc = inst.get_action().to_ascii_lowercase();
        let nested_lc = top_params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(action_lc.as_str(), "apply" | "apply_diff" | "apply_changes")
            || matches!(nested_lc.as_str(), "apply" | "apply_diff" | "apply_changes")
        {
            let child_run_id = top_params
                .get("child_run_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if child_run_id.is_empty() {
                return Ok(ArmOutcome::Early(DispatchResult {
                    view: None,
                    stdout: String::new(),
                    stderr: "❌ child_writer.apply 缺少必要参数 child_run_id。".to_string(),
                    success: false,
                    route: "child_writer:apply_missing_id".to_string(),
                }));
            }
            return Ok(ArmOutcome::Early(
                match crate::agent::child_writer::apply_writer_result(
                    ctx.state.inner(),
                    &child_run_id,
                    ctx.final_session_id,
                    ctx.workspace_dir,
                )
                .await
                {
                    Ok(report) => DispatchResult {
                        view: None,
                        stdout: report,
                        stderr: String::new(),
                        success: true,
                        route: "child_writer:applied".to_string(),
                    },
                    Err(error) => DispatchResult {
                        view: None,
                        stdout: String::new(),
                        stderr: error,
                        success: false,
                        route: "child_writer:apply_rejected".to_string(),
                    },
                },
            ));
        }
    }
    if let Some(error) = crate::agent::tools::input_validation::native_rejection(
        "child_writer",
        "dispatch",
        &inst.get_params(),
    ) {
        return Ok(ArmOutcome::Early(error));
    }
    Box::pin(super::dispatch_child_task::dispatch_alias(
        ctx, inst, "writer",
    ))
    .await
}
