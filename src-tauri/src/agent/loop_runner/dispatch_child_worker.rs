//! Worker is a role preset of the shared main executor.
use super::*;

pub(super) async fn dispatch_child_worker(ctx: &mut DispatchCtx<'_>, inst: &AgentInstruction) -> Result<ArmOutcome,String> {
    super::dispatch_child_task::dispatch_alias(ctx,inst,"worker").await
}
