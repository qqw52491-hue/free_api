//! 回复含义由主模型在正常下一轮判断；宿主只核对回复来源、操作范围和版本。
use crate::agent::{
    checkpoint::{AgentCheckpoint, PlanPhase},
    context::SandwichContext,
};
use crate::db::DbState;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingHumanReply {
    pub reply_id: String,
    pub question: String,
    pub reply: String,
    pub git_scopes: BTreeMap<String, String>,
    pub plan_requested: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitReplyDecision {
    pub command: String,
    pub approved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HumanReplyDecision {
    pub reply_id: String,
    #[serde(default)]
    pub git_decisions: Vec<GitReplyDecision>,
    pub plan_approved: Option<bool>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedHumanReply {
    pub source: PendingHumanReply,
    pub decision: HumanReplyDecision,
}

impl PendingHumanReply {
    pub fn render(&self) -> String {
        format!("【待理解的人类回复】\n{}\n请结合提问、原始回复和当前任务理解用户意图。宿主没有按文字自动放行。仅在用户授权了对应操作时判 approved=true；拒绝、解释请求及未明确授权不能当作许可。用顶层 human_reply_decision 与下一项工具操作一起提交，也可单独提交判断；不要为填字段另问用户。格式：{{\"human_reply_decision\":{{\"reply_id\":\"{}\",\"git_decisions\":[{{\"command\":\"上列命令原文\",\"approved\":false}}],\"plan_approved\":null,\"reason\":\"结合原话说明判断\"}}}}。git_decisions 只填写上列被询问命令；plan_requested=false 时 plan_approved 留 null。只解释或暂不处理时可以省略判断继续对话。\n\n",
            serde_json::to_string(self).unwrap_or_default(), self.reply_id)
    }
}

impl AgentCheckpoint {
    /// 只登记原话与来源，不解释“同意/不同意”。旧的 asked 绑定在这里消费。
    pub fn register_human_reply(&mut self, question: &str, reply: &str, plan_requested: bool) {
        let git_scopes = std::mem::take(&mut self.git_destructive_asked)
            .into_iter()
            .filter_map(|command| {
                self.git_destructive_scopes
                    .get(&command)
                    .cloned()
                    .map(|scope| (command, scope))
            })
            .collect::<BTreeMap<_, _>>();
        self.pending_human_reply =
            (!git_scopes.is_empty() || plan_requested).then(|| PendingHumanReply {
                reply_id: uuid::Uuid::new_v4().to_string(),
                question: question.to_string(),
                reply: reply.to_string(),
                git_scopes,
                plan_requested,
            });
    }

    pub fn apply_human_reply_decision(
        &mut self,
        db: &DbState,
        context: &mut SandwichContext,
        decision: &HumanReplyDecision,
    ) -> Result<(), String> {
        // prepared_instruction 恢复时幂等重放，不重新生成或扩大许可。
        if self.pending_human_reply.is_none()
            && self
                .last_human_reply_resolution
                .as_ref()
                .is_some_and(|r| r.decision == *decision)
        {
            return Ok(());
        }
        let source = self
            .pending_human_reply
            .as_ref()
            .ok_or("当前没有待理解的人类回复；本次判断未应用")?;
        if source.reply_id != decision.reply_id {
            return Err("reply_id 不属于当前人类回复；本次判断未应用".into());
        }
        if decision.reason.trim().is_empty() {
            return Err("human_reply_decision.reason 不能为空；本次判断未应用".into());
        }
        if decision.plan_approved.is_some() && !source.plan_requested {
            return Err("本次提问不包含方案审批；本次判断未应用".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for item in &decision.git_decisions {
            if !source.git_scopes.contains_key(&item.command) || !seen.insert(&item.command) {
                return Err(
                    "git_decisions 包含未被本次询问的命令或重复命令；本次判断未应用".into(),
                );
            }
        }
        let source = self.pending_human_reply.take().unwrap();
        for item in &decision.git_decisions {
            if item.approved {
                self.git_destructive_approved_scopes.insert(
                    item.command.clone(),
                    source.git_scopes[&item.command].clone(),
                );
            } else {
                self.git_destructive_approved_scopes.remove(&item.command);
            }
            self.git_destructive_pending
                .retain(|command| command != &item.command);
        }
        if let Some(approved) = decision.plan_approved {
            self.apply_explicit_plan_decision(
                db,
                context,
                approved,
                &source.question,
                &source.reply,
            );
        }
        self.last_human_reply_resolution = Some(ResolvedHumanReply {
            source,
            decision: decision.clone(),
        });
        Ok(())
    }

    /// 仅消费结构化按钮或主模型已判定的结果，不解析自然语言。
    pub fn apply_explicit_plan_decision(
        &mut self,
        db: &DbState,
        context: &mut SandwichContext,
        approved: bool,
        question: &str,
        reply: &str,
    ) {
        if !approved {
            self.plan_phase = PlanPhase::Reviewing;
            return;
        }
        self.plan_phase = PlanPhase::Approved;
        self.plan_unreviewed_release = self.plan_review_deliveries.is_empty();
        self.gate_human_escape_directed = false;
        self.recent_gate_block_step = None;
        self.approved_plan_summary = Some(reply.chars().take(500).collect());
        self.record_human_review_waiver(db, context, &format!("问: {question} → 答: {reply}"));
        if self.stage_review_blockers_open > 0 {
            self.clear_stage_review_blockers();
            self.stage_review_blocker_stage = None;
        }
        self.last_review_blocking_count = 0;
        self.last_reviewed_diff_hash = None;
        self.last_review_result = None;
        self.reviewer_retry_count = 0;
    }
}

#[cfg(test)]
mod tests;
