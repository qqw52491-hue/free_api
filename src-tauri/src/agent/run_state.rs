use serde::Serialize;
use std::fmt;

/// Agent 运行生命周期的互斥阶段。后端是唯一权威来源；前端根据快照派生展示状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunState {
    Idle,
    Preparing,
    Planning,
    Executing,
    WaitingForHuman,
    Recovering,
    Rescuing,
    Finalizing,
    Succeeded,
    Failed,
    Cancelled,
}

impl AgentRunState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// 对外发送的可序列化运行快照。
#[derive(Debug, Clone, Serialize)]
pub struct AgentRunSnapshot {
    pub state: AgentRunState,
    pub step_id: Option<usize>,
    pub action: Option<String>,
    pub retry_count: u32,
    pub tool_retry_count: u32,
    pub message: Option<String>,
}

/// 驱动状态迁移的领域事件。副作用（调用模型、调用工具、emit）保留在 mod.rs 中。
#[derive(Debug, Clone)]
pub enum AgentRunEvent {
    Start,
    RuntimeReady,
    BeginPlanning { step_id: usize },
    InstructionReady { action: String },
    PlanningFailed { reason: String },
    ToolFailed { reason: String },
    WaitForHuman { question: String },
    HumanReplyReceived,
    RetryPlanning,
    RescueRequested,
    RescueInstructionReady { action: String },
    BeginFinalizing,
    Succeed { summary: String },
    Fail { reason: String },
    Cancel { reason: String },
}

impl AgentRunEvent {
    fn name(&self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::RuntimeReady => "runtime_ready",
            Self::BeginPlanning { .. } => "begin_planning",
            Self::InstructionReady { .. } => "instruction_ready",
            Self::PlanningFailed { .. } => "planning_failed",
            Self::ToolFailed { .. } => "tool_failed",
            Self::WaitForHuman { .. } => "wait_for_human",
            Self::HumanReplyReceived => "human_reply_received",
            Self::RetryPlanning => "retry_planning",
            Self::RescueRequested => "rescue_requested",
            Self::RescueInstructionReady { .. } => "rescue_instruction_ready",
            Self::BeginFinalizing => "begin_finalizing",
            Self::Succeed { .. } => "succeed",
            Self::Fail { .. } => "fail",
            Self::Cancel { .. } => "cancel",
        }
    }
}

#[derive(Debug, Clone)]
pub struct InvalidTransition {
    pub from: AgentRunState,
    pub event: &'static str,
}

impl fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid agent state transition: {:?} -> {}",
            self.from, self.event
        )
    }
}

impl std::error::Error for InvalidTransition {}

/// 仅维护运行状态和计数，不执行网络、浏览器或插件副作用。
#[derive(Debug, Clone)]
pub struct AgentRunMachine {
    state: AgentRunState,
    step_id: Option<usize>,
    action: Option<String>,
    retry_count: u32,
    tool_retry_count: u32,
    message: Option<String>,
}

impl Default for AgentRunMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRunMachine {
    pub fn new() -> Self {
        Self {
            state: AgentRunState::Idle,
            step_id: None,
            action: None,
            retry_count: 0,
            tool_retry_count: 0,
            message: None,
        }
    }

    pub fn state(&self) -> AgentRunState {
        self.state
    }

    pub fn snapshot(&self) -> AgentRunSnapshot {
        AgentRunSnapshot {
            state: self.state,
            step_id: self.step_id,
            action: self.action.clone(),
            retry_count: self.retry_count,
            tool_retry_count: self.tool_retry_count,
            message: self.message.clone(),
        }
    }

    pub fn transition(
        &mut self,
        event: AgentRunEvent,
    ) -> Result<AgentRunSnapshot, InvalidTransition> {
        let event_name = event.name();
        let from = self.state;
        let valid = match event {
            AgentRunEvent::Start if self.state == AgentRunState::Idle => {
                self.state = AgentRunState::Preparing;
                self.message = Some("正在准备运行环境".to_string());
                true
            }
            AgentRunEvent::RuntimeReady if self.state == AgentRunState::Preparing => {
                self.state = AgentRunState::Planning;
                self.message = Some("运行环境已就绪".to_string());
                true
            }
            AgentRunEvent::BeginPlanning { step_id }
                if matches!(
                    self.state,
                    AgentRunState::Planning | AgentRunState::Executing | AgentRunState::Recovering
                ) =>
            {
                self.state = AgentRunState::Planning;
                self.step_id = Some(step_id);
                self.action = None;
                self.message = Some(format!("正在规划第 {} 步", step_id + 1));
                true
            }
            AgentRunEvent::InstructionReady { action }
                if matches!(
                    self.state,
                    AgentRunState::Planning | AgentRunState::Recovering | AgentRunState::Rescuing
                ) =>
            {
                self.state = AgentRunState::Executing;
                self.action = Some(action);
                self.message = Some("正在执行指令".to_string());
                true
            }
            AgentRunEvent::PlanningFailed { reason } if self.state == AgentRunState::Planning => {
                self.state = AgentRunState::Recovering;
                self.retry_count += 1;
                self.message = Some(reason);
                true
            }
            AgentRunEvent::ToolFailed { reason } if self.state == AgentRunState::Executing => {
                self.state = AgentRunState::Recovering;
                self.retry_count += 1;
                self.tool_retry_count += 1;
                self.message = Some(reason);
                true
            }
            AgentRunEvent::WaitForHuman { question } if self.state == AgentRunState::Executing => {
                self.state = AgentRunState::WaitingForHuman;
                self.message = Some(question);
                true
            }
            AgentRunEvent::HumanReplyReceived if self.state == AgentRunState::WaitingForHuman => {
                self.state = AgentRunState::Executing;
                self.message = Some("已收到人类回复，继续执行".to_string());
                true
            }
            AgentRunEvent::RetryPlanning if self.state == AgentRunState::Recovering => {
                self.state = AgentRunState::Planning;
                self.action = None;
                self.message = Some("正在基于失败反馈重新规划".to_string());
                true
            }
            AgentRunEvent::RescueRequested if self.state == AgentRunState::Recovering => {
                self.state = AgentRunState::Rescuing;
                self.message = Some("正在请求外部救援指令".to_string());
                true
            }
            AgentRunEvent::RescueInstructionReady { action }
                if self.state == AgentRunState::Rescuing =>
            {
                self.state = AgentRunState::Executing;
                self.action = Some(action);
                self.message = Some("正在执行救援指令".to_string());
                true
            }
            AgentRunEvent::BeginFinalizing
                if matches!(
                    self.state,
                    AgentRunState::Planning | AgentRunState::Executing
                ) =>
            {
                self.state = AgentRunState::Finalizing;
                self.message = Some("正在整理任务结果".to_string());
                true
            }
            AgentRunEvent::Succeed { summary } if self.state == AgentRunState::Finalizing => {
                self.state = AgentRunState::Succeeded;
                self.action = None;
                self.message = Some(summary);
                true
            }
            AgentRunEvent::Fail { reason } => {
                self.state = AgentRunState::Failed;
                self.action = None;
                self.message = Some(reason);
                true
            }
            AgentRunEvent::Cancel { reason } => {
                self.state = AgentRunState::Cancelled;
                self.action = None;
                self.message = Some(reason);
                true
            }
            _ => false,
        };

        if valid {
            Ok(self.snapshot())
        } else {
            Err(InvalidTransition {
                from,
                event: event_name,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_reaches_succeeded() {
        let mut machine = AgentRunMachine::new();
        machine.transition(AgentRunEvent::Start).unwrap();
        machine.transition(AgentRunEvent::RuntimeReady).unwrap();
        machine
            .transition(AgentRunEvent::BeginPlanning { step_id: 0 })
            .unwrap();
        machine
            .transition(AgentRunEvent::InstructionReady {
                action: "finish".to_string(),
            })
            .unwrap();
        machine.transition(AgentRunEvent::BeginFinalizing).unwrap();
        let snapshot = machine
            .transition(AgentRunEvent::Succeed {
                summary: "done".to_string(),
            })
            .unwrap();
        assert_eq!(snapshot.state, AgentRunState::Succeeded);
    }

    #[test]
    fn cancel_stops_waiting_human_without_resuming() {
        let mut machine = AgentRunMachine::new();
        machine.transition(AgentRunEvent::Start).unwrap();
        machine.transition(AgentRunEvent::RuntimeReady).unwrap();
        machine
            .transition(AgentRunEvent::InstructionReady {
                action: "ask_human".to_string(),
            })
            .unwrap();
        machine
            .transition(AgentRunEvent::WaitForHuman {
                question: "confirm".to_string(),
            })
            .unwrap();
        let snapshot = machine
            .transition(AgentRunEvent::Cancel {
                reason: "replaced".to_string(),
            })
            .unwrap();
        assert_eq!(snapshot.state, AgentRunState::Cancelled);
        assert!(machine
            .transition(AgentRunEvent::HumanReplyReceived)
            .is_err());
    }
    #[test]
    fn human_reply_only_resumes_a_paused_run() {
        let mut machine = AgentRunMachine::new();
        assert!(machine
            .transition(AgentRunEvent::HumanReplyReceived)
            .is_err());
        machine.transition(AgentRunEvent::Start).unwrap();
        machine.transition(AgentRunEvent::RuntimeReady).unwrap();
        machine
            .transition(AgentRunEvent::InstructionReady {
                action: "ask_human".to_string(),
            })
            .unwrap();
        machine
            .transition(AgentRunEvent::WaitForHuman {
                question: "confirm".to_string(),
            })
            .unwrap();
        let snapshot = machine
            .transition(AgentRunEvent::HumanReplyReceived)
            .unwrap();
        assert_eq!(snapshot.state, AgentRunState::Executing);
    }

    #[test]
    fn tool_failure_enters_recovery_and_tracks_attempts() {
        let mut machine = AgentRunMachine::new();
        machine.transition(AgentRunEvent::Start).unwrap();
        machine.transition(AgentRunEvent::RuntimeReady).unwrap();
        machine
            .transition(AgentRunEvent::InstructionReady {
                action: "click".to_string(),
            })
            .unwrap();
        let snapshot = machine
            .transition(AgentRunEvent::ToolFailed {
                reason: "missing target".to_string(),
            })
            .unwrap();
        assert_eq!(snapshot.state, AgentRunState::Recovering);
        assert_eq!(snapshot.retry_count, 1);
        assert_eq!(snapshot.tool_retry_count, 1);
    }

    #[test]
    fn fail_transitions_are_idempotent() {
        let mut machine = AgentRunMachine::new();
        machine.transition(AgentRunEvent::Start).unwrap();
        machine.transition(AgentRunEvent::Fail { reason: "first error".to_string() }).unwrap();
        assert_eq!(machine.state(), AgentRunState::Failed);

        // Calling Fail again on already failed machine should succeed idempotently and update the message
        let second_snapshot = machine.transition(AgentRunEvent::Fail { reason: "refined error".to_string() }).unwrap();
        assert_eq!(second_snapshot.state, AgentRunState::Failed);
        assert_eq!(second_snapshot.message.as_deref(), Some("refined error"));
    }
}
