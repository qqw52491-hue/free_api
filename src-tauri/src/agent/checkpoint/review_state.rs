//! 审查历史与当前判断分离。身份来自宿主台账，不依赖返回顺序或发现描述。
use super::*;
use crate::agent::child_reviewer::{ReviewFinding, ReviewerDeliveryData};
use crate::agent::context::SandwichContext;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReviewDeliverySource {
    pub batch_id: Option<String>,
    pub task_id: String,
    pub attempt_id: u32,
}

impl ReviewDeliverySource {
    pub(crate) fn from_run(db: &DbState, id: &str) -> Self {
        crate::agent::child_reviewer::load_run_identity(db, id)
            .map(|i| Self {
                batch_id: i.batch_id,
                task_id: i.task_id,
                attempt_id: i.attempt_id,
            })
            .unwrap_or_else(|| Self {
                batch_id: None,
                task_id: id.into(),
                attempt_id: 0,
            })
    }

    pub(crate) fn same_task(&self, other: &Self) -> bool {
        self.batch_id == other.batch_id && self.task_id == other.task_id
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewFindingTodo {
    pub source: ReviewDeliverySource,
    pub finding_id: String,
    // 只自动操作仍与宿主登记内容相同的 TODO，用户重新编辑/复用编号时不猜归属。
    pub description: String,
}

impl AgentCheckpoint {
    /// 人工接受已知风险不会伪造审查撤回；仅核销本批、来源及内容仍匹配的自动待办。
    pub(crate) fn record_human_review_waiver(
        &mut self,
        db: &DbState,
        context: &mut SandwichContext,
        waiver: &str,
    ) {
        self.refresh_review_sources(db);
        let ambiguous = self.sync_review_todos(db, context);
        for todo in &mut context.todo_list {
            if self.review_finding_todos.get(&todo.id).is_some_and(|binding| {
                binding.source.batch_id == self.plan_review_batch_id
                    && binding.description == todo.description
            }) && matches!(todo.status.as_str(), "pending" | "in_progress") {
                todo.status = "done".into();
            }
        }
        self.record_human_waiver(waiver);
        if ambiguous {
            context.add_error_feedback("人工放行已留痕；部分旧审查待办的来源存在歧义，未按 F 编号批量核销，请按具体任务核对。");
        }
    }

    pub(crate) fn refresh_review_sources(&mut self, db: &DbState) {
        for id in self.plan_review_deliveries.keys() {
            self.review_delivery_sources
                .insert(id.clone(), ReviewDeliverySource::from_run(db, id));
        }
        if !self.plan_review_deliveries.is_empty() {
            self.last_review_blocking_count = self.aggregate_unwithdrawn_blockings().len() as u32;
            self.last_review_scorecard = self.aggregate_scorecard();
            let mut roles = BTreeSet::new();
            let mut escalated = false;
            for (_, _, delivery) in self.current_review_deliveries(None) {
                if delivery
                    .get_extra_str("decision")
                    .is_some_and(|d| d.eq_ignore_ascii_case("escalate"))
                {
                    escalated = true;
                    if let Some(values) = delivery
                        .extra
                        .get("escalate_roles")
                        .and_then(|v| v.as_array())
                    {
                        roles.extend(values.iter().filter_map(|v| v.as_str()).map(str::to_owned));
                    }
                }
            }
            self.plan_escalation_required = escalated.then(|| roles.into_iter().collect());
        }
    }

    fn review_source(&self, id: &str, db: Option<&DbState>) -> ReviewDeliverySource {
        db.map(|db| ReviewDeliverySource::from_run(db, id))
            .or_else(|| self.review_delivery_sources.get(id).cloned())
            .unwrap_or_else(|| ReviewDeliverySource {
                batch_id: None,
                task_id: id.into(),
                attempt_id: 0,
            })
    }

    pub(crate) fn review_history(
        &self,
        db: Option<&DbState>,
    ) -> Vec<(String, ReviewDeliverySource, &ReviewerDeliveryData)> {
        let mut history: Vec<_> = self
            .plan_review_deliveries
            .iter()
            .filter_map(|(id, delivery)| {
                let source = self.review_source(id, db);
                if self.plan_review_batch_id.is_some()
                    && source.batch_id.is_some()
                    && source.batch_id != self.plan_review_batch_id
                {
                    return None;
                }
                Some((id.clone(), source, delivery))
            })
            .collect();
        history.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        history
    }

    pub(crate) fn current_review_deliveries(
        &self,
        db: Option<&DbState>,
    ) -> Vec<(String, ReviewDeliverySource, &ReviewerDeliveryData)> {
        let history = self.review_history(db);
        history
            .iter()
            .filter(|(_, source, _)| {
                !history.iter().any(|(_, other, _)| {
                    source.same_task(other) && other.attempt_id > source.attempt_id
                })
            })
            .cloned()
            .collect()
    }

    /// 保留各等级的当前发现：同任务明确更新或撤回才改变旧发现，漏写不等于撤回。
    /// 新尝试失败不会进入合法交付缓存；TODO 与阻塞聚合共用这一完整视图。
    fn current_review_finding_states(&self) -> Vec<(ReviewDeliverySource, ReviewFinding)> {
        let mut findings = BTreeMap::new();
        for (run, source, delivery) in self.review_history(None) {
            for id in &delivery.withdrawn_findings {
                findings.remove(&(source.batch_id.clone(), source.task_id.clone(), id.clone()));
            }
            for (index, finding) in delivery.findings.iter().enumerate() {
                if delivery.withdrawn_findings.contains(&finding.id) {
                    continue;
                }
                let id = if finding.id.is_empty() {
                    format!("{run}:anonymous:{index}")
                } else {
                    finding.id.clone()
                };
                findings.insert(
                    (source.batch_id.clone(), source.task_id.clone(), id),
                    (source.clone(), finding.clone()),
                );
            }
        }
        findings.into_values().collect()
    }

    pub(crate) fn current_review_findings(&self) -> Vec<(ReviewDeliverySource, ReviewFinding)> {
        self.current_review_finding_states()
            .into_iter()
            .filter(|(_, f)| f.severity.eq_ignore_ascii_case("blocking"))
            .collect()
    }

    /// 用有效来源同步自动 TODO；历史格式只在来源唯一时收养，歧义不批量关闭。
    pub(crate) fn sync_review_todos(
        &mut self,
        db: &DbState,
        context: &mut SandwichContext,
    ) -> bool {
        let mut owners: BTreeMap<String, BTreeSet<(Option<String>, String, String)>> =
            BTreeMap::new();
        for (_, source, delivery) in self.review_history(Some(db)) {
            for finding in &delivery.findings {
                if finding.severity.eq_ignore_ascii_case("blocking") {
                    owners
                        .entry(format!("[审查发现] {}", finding.display_line()))
                        .or_default()
                        .insert((
                            source.batch_id.clone(),
                            source.task_id.clone(),
                            finding.id.clone(),
                        ));
                }
            }
        }
        let mut ambiguous = false;
        for todo in &context.todo_list {
            if self.review_finding_todos.contains_key(&todo.id) {
                continue;
            }
            if let Some(candidates) = owners.get(&todo.description) {
                if candidates.len() == 1 {
                    let (batch, task, finding) = candidates.first().unwrap();
                    self.review_finding_todos.insert(
                        todo.id,
                        ReviewFindingTodo {
                            source: ReviewDeliverySource {
                                batch_id: batch.clone(),
                                task_id: task.clone(),
                                attempt_id: 0,
                            },
                            finding_id: finding.clone(),
                            description: todo.description.clone(),
                        },
                    );
                } else {
                    ambiguous = true;
                }
            }
        }
        let active = self.current_review_finding_states();
        let withdrawn: BTreeSet<_> =
            self.review_history(None)
                .into_iter()
                .flat_map(|(_, source, delivery)| {
                    delivery.withdrawn_findings.iter().map(move |id| {
                        (source.batch_id.clone(), source.task_id.clone(), id.clone())
                    })
                })
                .collect();
        for todo in &mut context.todo_list {
            let Some(binding) = self.review_finding_todos.get_mut(&todo.id) else {
                continue;
            };
            if binding.description != todo.description {
                continue;
            }
            // 不触碰旧批次的待办；新方案提交不是旧缺陷已经撤回的证据。
            if self.plan_review_batch_id.is_some()
                && binding.source.batch_id != self.plan_review_batch_id
            {
                continue;
            }
            if let Some((source, finding)) = active.iter().find(|(source, finding)| {
                source.same_task(&binding.source) && finding.id == binding.finding_id
            }) {
                let description = format!("[审查发现] {}", finding.display_line());
                todo.description = description.clone();
                binding.description = description;
                if finding.severity.eq_ignore_ascii_case("blocking") {
                    if source.attempt_id > binding.source.attempt_id && todo.status == "done" {
                        todo.status = "pending".into();
                    }
                } else if (finding.severity.eq_ignore_ascii_case("suggestion")
                    || finding.severity.eq_ignore_ascii_case("question"))
                    && matches!(todo.status.as_str(), "pending" | "in_progress")
                {
                    // 明确降级不是漏写：更新参考内容并解除该来源的旧阻塞待办。
                    todo.status = "done".into();
                }
                binding.source = source.clone();
            } else if withdrawn.contains(&(
                binding.source.batch_id.clone(),
                binding.source.task_id.clone(),
                binding.finding_id.clone(),
            )) && matches!(todo.status.as_str(), "pending" | "in_progress")
            {
                todo.status = "done".into();
            }
        }
        let mut next = context
            .todo_list
            .iter()
            .map(|t| t.id)
            .max()
            .map_or(0, |id| id + 1);
        for (source, finding) in active
            .into_iter()
            .filter(|(_, finding)| finding.severity.eq_ignore_ascii_case("blocking"))
        {
            if self.review_finding_todos.iter().any(|(id, binding)| {
                binding.source.same_task(&source)
                    && binding.finding_id == finding.id
                    && context
                        .todo_list
                        .iter()
                        .any(|t| t.id == *id && t.description == binding.description)
            }) {
                continue;
            }
            let description = format!("[审查发现] {}", finding.display_line());
            self.review_finding_todos.insert(
                next,
                ReviewFindingTodo {
                    source,
                    finding_id: finding.id,
                    description: description.clone(),
                },
            );
            context.todo_list.push(TodoItem {
                id: next,
                status: "pending".into(),
                description,
                evidence_output_id: None,
            });
            next += 1;
        }
        ambiguous
    }
}
