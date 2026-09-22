//! 语义级打转检测（Semantic Stall Detector）。
//!
//! `NoProgressTracker` 只抓"相同工具+相同参数+相同输出"的机械重复；
//! 本组件抓语义级空转：连续多步纯调查（读/搜/探测），未触及任何
//! 此前未读过的路径、未产生任何状态变更——即"把所有相关文件读了三遍
//! 仍未形成方案"的形态（实测：闭卷修复任务 80 步耗尽，后 40 步全为
//! 换窗口复读同一批函数）。
//!
//! 行为契约（fail-open）：只注入收敛提示 / 代花 Pro 升维配额，
//! 绝不拦截或拒绝执行。阈值提示各只发一次，出现真实进展信号后复位。

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// 连续 N 步静默调查 → 注入收敛提示（强制综合假设）。
const STALL_NOTICE_THRESHOLD: u32 = 12;
/// 连续 N 步静默调查 → 宿主代花一次 Pro 升维配额
/// （真卡死的模型往往没有"该呼救"的元认知——实测打转 20+ 步零呼救）。
const STALL_ESCALATE_THRESHOLD: u32 = 20;
/// 已读路径集合有界：超限整体清空（启发式检测，不是审计账本）。
const MAX_TRACKED_PATHS: usize = 256;

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
    /// 连续"静默调查"步数：无状态变更、无新路径证据。
    #[serde(default)]
    streak: u32,
    /// 本会话读过的规范化路径集合（路径级，不含窗口——
    /// 换窗口复读同一文件仍是同一证据面）。
    #[serde(default)]
    seen_paths: BTreeSet<String>,
    /// 收敛提示只发一次：重复广播无信息量且污染前缀。
    #[serde(default)]
    notice_issued: bool,
    /// 自动升维每段打转期只花一次配额。
    #[serde(default)]
    escalation_issued: bool,
}

impl SemanticStallTracker {
    /// 上下文压缩后调用：模型已遗忘旧读取，复读重新合法。
    pub fn reset(&mut self) {
        self.streak = 0;
        self.seen_paths.clear();
        self.notice_issued = false;
        self.escalation_issued = false;
    }

    /// 提取本步读取/列举的规范化路径（若有）。
    /// 与 read_ledger 共用同一口径，但接受目录（list/tree 的新目录同样是新证据）。
    fn extract_read_path(
        instruction: &crate::agent::types::AgentInstruction,
        workspace_dir: &std::path::Path,
    ) -> Option<String> {
        let tool = instruction.get_tool().to_lowercase();
        let action = instruction.get_action().to_lowercase();
        let is_native_read = tool == "local_file"
            && (action.starts_with("read") || action.starts_with("list"));
        let is_read_tool = matches!(
            tool.as_str(),
            "read_file" | "read_text_file" | "read_media_file" | "read_multiple_files"
                | "get_file_content" | "get_code_snippet" | "list_directory"
                | "list_directory_with_sizes" | "directory_tree" | "get_file_info"
        );
        if !is_native_read && !is_read_tool {
            return None;
        }
        let params = instruction.get_params();
        let raw = params
            .get("path")
            .or_else(|| params.get("filePath"))
            .or_else(|| params.get("file_path"))
            .or_else(|| params.get("dir"))
            .or_else(|| params.get("directory"))
            .and_then(|v| v.as_str())?;
        if raw.trim().is_empty() || raw.chars().count() > 1_000 {
            return None;
        }
        let candidate = std::path::PathBuf::from(raw.trim());
        let target = if candidate.is_relative() {
            workspace_dir.join(candidate)
        } else {
            candidate
        };
        // 优先 canonicalize；失败（不存在的路径）退化为规范化拼接——
        // 读不存在路径同样是"重复踩同一坑"的证据，不能因 canonicalize 失败而漏记。
        Some(
            target
                .canonicalize()
                .map(|c| c.to_string_lossy().to_string())
                .unwrap_or_else(|_| target.to_string_lossy().to_string()),
        )
    }

    /// 进展信号：本步是否产生了真实状态变更/新证据/新失败。
    /// 任一成立即非静默——宁漏判不误判（fail-open）。
    fn has_progress_signal(
        instruction: &crate::agent::types::AgentInstruction,
        result_success: bool,
        novel_path: bool,
    ) -> bool {
        // 失败即新信息（诊断失败原因本身就是进展）。
        if !result_success {
            return true;
        }
        // 真实写入/副作用。
        if crate::agent::context::AuditLedger::is_mutating_side_effect_with_action(
            &instruction.get_tool(),
            &instruction.get_action(),
            &instruction.get_params(),
        ) {
            return true;
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
        if matches!(tool.as_str(), "ask_human" | "finish")
            || tool.contains("child_")
            || tool.contains("dispatch")
        {
            return true;
        }
        // 读到了本会话从未读过的新路径 = 新证据。
        novel_path
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
        workspace_dir: &std::path::Path,
    ) -> Option<StallSignal> {
        if Self::is_neutral_step(instruction) {
            return None;
        }
        let read_path = Self::extract_read_path(instruction, workspace_dir);
        let novel_path = match &read_path {
            Some(p) => {
                if self.seen_paths.len() >= MAX_TRACKED_PATHS {
                    self.seen_paths.clear();
                }
                self.seen_paths.insert(p.clone())
            }
            None => false,
        };
        if Self::has_progress_signal(instruction, result_success, novel_path) {
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

    #[test]
    fn silent_streak_fires_notice_then_escalate() {
        let mut t = SemanticStallTracker::default();
        let ws = std::path::Path::new("/nonexistent-ws");
        // 同一文件反复读（canonicalize 失败也照常记录）：累计静默
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
    fn novel_reads_reset_streak() {
        let mut t = SemanticStallTracker::default();
        let ws = std::path::Path::new("/nonexistent-ws");
        for i in 0..30 {
            assert!(t
                .record(&read_step(&format!("src/f{}.rs", i)), true, ws)
                .is_none());
        }
        assert_eq!(t.streak, 0);
    }

    #[test]
    fn write_and_search_reset() {
        let mut t = SemanticStallTracker::default();
        let ws = std::path::Path::new("/nonexistent-ws");
        for _ in 0..11 {
            t.record(&search_step(), true, ws);
        }
        assert!(t.record(&write_step(), true, ws).is_none());
        assert_eq!(t.streak, 0);
        // 失败步也是进展
        t.record(&search_step(), false, ws);
        assert_eq!(t.streak, 0);
    }

    #[test]
    fn wait_jobs_is_neutral() {
        let mut t = SemanticStallTracker::default();
        let ws = std::path::Path::new("/nonexistent-ws");
        let mut i = crate::agent::types::AgentInstruction::default();
        i.tool = Some("wait_jobs".to_string());
        t.record(&i, true, ws);
        assert_eq!(t.streak, 0); // 不累计
    }
}
