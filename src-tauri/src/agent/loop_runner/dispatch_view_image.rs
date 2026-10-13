//! view_image 工具臂：模型主动把图片文件拉进自己的下一轮调用。
//!
//! 信道为一次性注入：图片编码后进 context.pending_view_images 暂存队列，
//! 只随下一次消息组装出现一轮、不落 turns_history（否则其后每次调用都
//! 占视觉档位并重复烧图 token）；该次请求返回即核销清空（call_llm_phase）。
//! router 的 has_pending_vision 认此队列——当轮硬规则切视觉模型档。
//! 对模型而言这是纯能力开放：用不用、何时用由它自己判断，不挂任何门禁。
//! 典型场景：渲染/截图/图表类产物生成后自检（contact sheet、原型截图）。
//! 视频不可直接注入——模型自行 ffmpeg 抽帧后看帧（与 skill 手册约定一致）。

use super::*;

pub(super) async fn dispatch_view_image(
    ctx: &mut DispatchCtx<'_>,
    inst: &AgentInstruction,
) -> Result<ArmOutcome, String> {
    let pending = crate::agent::tools::view_image::PendingImageQueue::default();
    let result = crate::agent::tools::view_image::dispatch(
        &inst.get_params(),
        Some(ctx.workspace_dir),
        ctx.final_session_id,
        &pending,
    );
    crate::agent::tools::view_image::transfer_pending_images(
        &pending,
        &mut ctx.context.pending_view_images,
    )?;
    Ok(ArmOutcome::Done(result))
}
