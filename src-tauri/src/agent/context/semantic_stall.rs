//! 语义级打转检测（Semantic Stall Detector）。
//!
//! `NoProgressTracker` 只抓"相同工具+相同参数+相同输出"的机械重复；
//! 本组件抓语义级空转：连续多步纯调查（读/搜/探测）且未产生任何
//! 状态变更——实测已见两种打转形态：
//!   ① 换窗口复读同一批函数（闭卷修复 80 步耗尽，后 40 步全复读）；
//!   ② 不断漂向新的无关文件（每步都读"新"路径，路径新颖性≠证据新颖性）。
//! 因此判据是"产出性"而非"新颖性"：读永远是准备动作，只有状态变更
//! （写入/计划/契约/派发/自述/升维/人类交互/验证命令成功）才算进展。
//!
//! 行为契约（fail-open）：只注入收敛提示 / 代花 Pro 升维配额，
//! 绝不拦截或拒绝执行。阈值提示各只发一次，出现进展信号后复位。

use serde::{Deserialize, Serialize};

/// 连续 N 步静默调查 → 注入收敛提示（强制综合假设）。
const STALL_NOTICE_THRESHOLD: u32 = 12;
/// 连续 N 步静默调查 → 宿主代花一次 Pro 升维配额
/// （真卡死的模型往往没有"该呼救"的元认知——实测打转 20+ 步零呼救）。
const STALL_ESCALATE_THRESHOLD: u32 = 20;

/// 宿主检测结论——由调用方决定注入提示还是执行升维。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallSignal {
    /// 应注入收敛提示；参数为当前连续静默步数。
    Notice(u32),
    /// 应代花升维配额（配额耗尽由调用方自行降级为提示）；参数为步数。
    Escalate(u32),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SemanticStallTracker {
    /// 连续"静默调查"步数：无状态变更。
    #[serde(default)]
    streak: u32,
    /// 收敛提示只发一次：重复广播无信息量且污染前缀。
    #[serde(default)]
    notice_issued: bool,
    /// 自动升维每段打转期只花一次配额。
    #[serde(default)]
    escalation_issued: bool,
}

impl SemanticStallTracker {
    /// 上下文压缩后调用：旧判定语境已丢失，检测复位。
    pub fn reset(&mut self) {
        self.streak = 0;
        self.notice_issued = false;
        self.escalation_issued = false;
    }

    /// 进展信号：本步是否产生了真实状态变更/验证证据。
    /// 任一成立即非静默——宁漏判不误判（fail-open）。
    /// 注意：门禁打回、格式错误等失败不改变任何状态，同样计入静默——
    /// "被打回→换个姿势再试→再被打回"本身就是打转形态之一。
    fn has_progress_signal(
        instruction: &crate::agent::types::AgentInstruction,
        result_success: bool,
    ) -> bool {
        // 真实写入/副作用。
        if crate::agent::context::AuditLedger::is_mutating_side_effect_with_action(
            &instruction.get_tool(),
            &instruction.get_action(),
            &instruction.get_params(),
        ) {
            return true;
        }
        // 成功的验证/测试命令是证据产出（auto_verify 依赖同一判据口径）。
        if result_success {
            if let Some(cmd) = crate::agent::change_verification::extract_command_from_params(
                &instruction.get_params(),
            ) {
                if crate::agent::change_verification::is_test_or_verification_command(&cmd) {
                    return true;
                }
            }
        }
        // 状态域字段变更：计划/契约/Todo/验证/自述/扩 scope/升维/记忆/摘要。
        if instruction.submit_plan.is_some()
            || instruction.review_contract_update.is_some()
            || instruction.change_verification_update.is_some()
            || !instruction.todo_update.is_empty()
            || instruction.target_blocks_update.is_some()
            || instruction.scope_extension.is_some()
            || instruction.request_escalation == Some(true)
            || instruction.request_exploration_extension.is_some()
            || instruction.prepare_finish.is_some()
            || instruction.self_report.is_some()
            || !instruction.memories_update.is_empty()
            || instruction.progress_summary.is_some()
            || instruction.new_plan == Some(true)
        {
            return true;
        }
        // 编排/派发/人类交互/终止：皆为有意的状态转移。
        if instruction.commands.iter().any(|v| !v.is_null())
            || instruction
                .parallel
                .as_ref()
                .is_some_and(|p| !p.is_empty())
            || instruction.plan_tree.is_some()
            || instruction
                .dispatches
                .as_ref()
                .is_some_and(|d| !d.is_empty())
        {
            return true;
        }
        let tool = instruction.get_tool().to_lowercase();
        matches!(tool.as_str(), "ask_human" | "finish")
            || tool.contains("child_")
            || tool.contains("dispatch")
    }

    /// 中性步：挂起等待/监听注册——在等异步结果，不是打转。
    /// 既不累计也不复位（等待期间不打断，但也不掩盖持续打转）。
    fn is_neutral_step(instruction: &crate::agent::types::AgentInstruction) -> bool {
        matches!(
            instruction.get_tool().to_lowercase().as_str(),
            "wait_jobs" | "watch_start" | "watch_stop"
        )
    }

    /// 每步收尾时调用；返回应执行的宿主动作（无进展 → None）。
    pub fn record(
        &mut self,
        instruction: &crate::agent::types::AgentInstruction,
        result_success: bool,
        _workspace_dir: &std::path::Path,
    ) -> Option<StallSignal> {
        if Self::is_neutral_step(instruction) {
            return None;
        }
        if Self::has_progress_signal(instruction, result_success) {
            // 出现真实进展：整段打转期结束，提示标记一并复位。
            self.streak = 0;
            self.notice_issued = false;
            self.escalation_issued = false;
            return None;
        }
        self.streak = self.streak.saturating_add(1);
        if self.streak >= STALL_ESCALATE_THRESHOLD && !self.escalation_issued {
            self.escalation_issued = true;
            return Some(StallSignal::Escalate(self.streak));
        }
        if self.streak >= STALL_NOTICE_THRESHOLD && !self.notice_issued {
            self.notice_issued = true;
            return Some(StallSignal::Notice(self.streak));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read_step(path: &str) -> crate::agent::types::AgentInstruction {
        let mut i = crate::agent::types::AgentInstruction::default();
        i.tool = Some("local_file".to_string());
        i.action = Some("read".to_string());
        i.params = Some(json!({"path": path}));
        i
    }

    fn write_step() -> crate::agent::types::AgentInstruction {
        let mut i = crate::agent::types::AgentInstruction::default();
        i.tool = Some("local_file".to_string());
        i.action = Some("write".to_string());
        i.params = Some(json!({"path": "src/x.rs", "content": "x"}));
        i
    }

    fn search_step() -> crate::agent::types::AgentInstruction {
        let mut i = crate::agent::types::AgentInstruction::default();
        i.tool = Some("search".to_string());
        i
    }

    fn verify_step() -> crate::agent::types::AgentInstruction {
        let mut i = crate::agent::types::AgentInstruction::default();
        i.tool = Some("local_shell".to_string());
        i.params = Some(json!({"command": "npx vue-tsc --noEmit"}));
        i
    }

    #[test]
    fn silent_streak_fires_notice_then_escalate() {
        let mut t = SemanticStallTracker::default();
        let ws = std::path::Path::new("/nonexistent-ws");
        let mut signals = Vec::new();
        for _ in 0..25 {
            if let Some(s) = t.record(&read_step("src/a.rs"), true, ws) {
                signals.push(s);
            }
        }
        assert!(signals.contains(&StallSignal::Notice(12)));
        assert!(signals.contains(&StallSignal::Escalate(20)));
        // 每个阈值只发一次
        assert_eq!(signals.len(), 2);
    }

    #[test]
    fn drifting_through_new_files_is_still_silent() {
        // 实测形态②：每步读"新"路径但零产出——仍然是打转。
        let mut t = SemanticStallTracker::default();
        let ws = std::path::Path::new("/nonexistent-ws");
        let mut signals = Vec::new();
        for i in 0..15 {
            if let Some(s) = t.record(&read_step(&format!("src/f{}.rs", i)), true, ws) {
                signals.push(s);
            }
        }
        assert!(signals.contains(&StallSignal::Notice(12)));
    }

    #[test]
    fn state_changes_and_verify_reset() {
        let mut t = SemanticStallTracker::default();
        let ws = std::path::Path::new("/nonexistent-ws");
        for _ in 0..11 {
            t.record(&search_step(), true, ws);
        }
        assert!(t.record(&write_step(), true, ws).is_none());
        assert_eq!(t.streak, 0);
        // 成功的验证命令同样是证据产出
        for _ in 0..11 {
            t.record(&search_step(), true, ws);
        }
        t.record(&verify_step(), true, ws);
        assert_eq!(t.streak, 0);
    }

    #[test]
    fn failures_and_wait_jobs_do_not_reset() {
        let mut t = SemanticStallTracker::default();
        let ws = std::path::Path::new("/nonexistent-ws");
        // 失败不产出状态变更 → 计入静默（门禁打回循环是打转形态之一）
        for _ in 0..12 {
            t.record(&search_step(), false, ws);
        }
        assert!(t.streak >= 12);
        // wait_jobs 中性：不累计也不复位
        let mut i = crate::agent::types::AgentInstruction::default();
        i.tool = Some("wait_jobs".to_string());
        t.record(&i, true, ws);
        assert_eq!(t.streak, 12);
    }
}
