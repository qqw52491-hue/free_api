//! A batch is one durable parent instruction; each slot has its own receipt.
use super::*;
use crate::agent::types::DispatchResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChildDispatchBatch {
    task_id: String,
    step_id: usize,
    instruction_json: String,
    pub results: Vec<Option<DispatchResult>>,
}

impl AgentCheckpoint {
    pub(crate) fn begin_child_dispatch_batch(
        &mut self,
        instruction: &AgentInstruction,
        context: &SandwichContext,
    ) -> Result<(), String> {
        let instruction_json = serde_json::to_string(instruction).map_err(|e| e.to_string())?;
        let matches = self.child_dispatch_batch.as_ref().is_some_and(|batch| {
            batch.task_id == context.history_task_id
                && batch.step_id == self.step_id
                && batch.instruction_json == instruction_json
        });
        if !matches {
            self.child_dispatch_batch = Some(Box::new(ChildDispatchBatch {
                task_id: context.history_task_id.clone(),
                step_id: self.step_id,
                instruction_json,
                results: vec![None; instruction.get_dispatch_items().len()],
            }));
        }
        // No child has started at this point. Each actual adapter persists its
        // own recovery boundary before it performs a side effect.
        self.phase = AgentPhase::RunTool;
        self.pending = None;
        self.prepared_instruction = Some(instruction.clone());
        self.refresh_context(context);
        Ok(())
    }

    pub(crate) fn prepare_child_dispatch(
        &mut self,
        instruction: &AgentInstruction,
        context: &SandwichContext,
        slot: Option<usize>,
        shared: bool,
    ) -> Result<(), String> {
        let prepared = if let Some(index) = slot {
            let batch = self
                .child_dispatch_batch
                .as_ref()
                .ok_or("批量派发检查点缺失；未派发。")?;
            if index >= batch.results.len() {
                return Err("批量派发索引超出清单；未派发。".into());
            }
            serde_json::from_str(&batch.instruction_json).map_err(|e| e.to_string())?
        } else {
            instruction.clone()
        };
        self.prepare_tool(prepared, context);
        if slot.is_some() && shared {
            // Registration is deduplicated in the child ledger. Restore the
            // whole batch through ResumeTool, not an uncertain business write.
            self.pending = None;
        }
        Ok(())
    }
}
