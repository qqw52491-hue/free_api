//! AgentCheckpoint 人工提问配额与豁免操作（impl 分块）。

use super::*;
use crate::agent::context::SandwichContext;
use crate::agent::control::HumanControl;
use crate::agent::types::{AgentInstruction, DispatchResult};
use crate::db::DbState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tauri::AppHandle;

impl AgentCheckpoint {

    /// 规范化提问文本以便去重比对：去除空白、常见中英文标点符号并转为小写
    pub fn normalize_question(text: &str) -> String {
        text.chars()
            .filter(|c| {
                !c.is_whitespace()
                    && !c.is_ascii_punctuation()
                    && !matches!(
                        *c,
                        '？' | '?'
                            | '。'
                            | '.'
                            | '！'
                            | '!'
                            | '，'
                            | ','
                            | '：'
                            | ':'
                            | '；'
                            | ';'
                            | '“'
                            | '”'
                            | '"'
                            | '\''
                            | '`'
                            | '【'
                            | '】'
                            | '['
                            | ']'
                            | '（'
                            | '）'
                            | '('
                            | ')'
                            | '、'
                            | '《'
                            | '》'
                    )
            })
            .flat_map(|c| c.to_lowercase())
            .collect()
    }

    /// 检查信息类提问是否已达到配额上限（私密凭据类提问在调用处豁免）
    pub fn check_inquiry_quota_exceeded(&self, budget: usize) -> Option<String> {
        if self.asked_human_questions.len() >= budget {
            Some(format!(
                "【信息提问配额已耗尽】本任务向用户提问已达上限（{}/{} 次）。为避免过度打扰用户，系统已关闭主动信息提问通道。\n\
请直接结合用户此前的全部输入、上下文已有信息及行业通用规范，采用最合理通用的默认参数与假设坚决推进执行；在最终输出中明确注明所采用的默认设定即可，严禁继续停顿提问。\n\
（注：若确需索取用户私密凭据/API Key，请在表单中设置 kind: \"missing_credential\" 申请凭据豁免通道）。",
                self.asked_human_questions.len(),
                budget
            ))
        } else {
            None
        }
    }

    /// 检查是否为重复提问。如果重复，返回拦截拒绝提示；否则返回 None。
    pub fn check_duplicate_question(&self, question: &str) -> Option<String> {
        let norm_incoming = Self::normalize_question(question);
        if norm_incoming.is_empty() {
            return None;
        }

        for past in &self.asked_human_questions {
            let norm_past = Self::normalize_question(past);
            if norm_past.is_empty() {
                continue;
            }

            // 1. 规范化后完全相同（剔除标点与空格后一致）
            if norm_incoming == norm_past {
                return Some(format!(
                    "【重复提问拦截】检测到你在本任务中此前已向用户询问过相同问题：\"{}\"。\n\
用户此前的回复已存在于上下文（【人类回复】）中。\n\
- 若此前提问已涵盖该信息，请直接结合已有回复或行业默认值继续执行，切勿就同一事项反复打扰用户；\n\
- 若确需针对不同的新参数/新槽位（如不同字段、新实体或不同操作）提问，请换用更具体详细的独立表述重新提问。",
                    past
                ));
            }

            // 2. 包含关系且差异字符纯为语气助词/口癖（如仅多了“呢”、“呀”、“吧”、“啊”、“吗”等，字符差 <= 2）
            let len_diff = norm_incoming.chars().count().abs_diff(norm_past.chars().count());
            if len_diff <= 2
                && (norm_incoming.contains(&norm_past) || norm_past.contains(&norm_incoming))
            {
                let diff_chars: String = if norm_incoming.len() > norm_past.len() {
                    norm_incoming.replace(&norm_past, "")
                } else {
                    norm_past.replace(&norm_incoming, "")
                };
                let is_filler = diff_chars.chars().all(|c| matches!(c, '呢' | '呀' | '吧' | '啊' | '吗' | '哈' | '请' | '问'));
                if is_filler {
                    return Some(format!(
                        "【重复提问拦截】检测到你在本任务中此前已向用户询问过高度相似的问题：\"{}\"。\n\
用户此前的回复已存在于上下文（【人类回复】）中。\n\
- 若此前提问已涵盖该信息，请直接结合已有回复或行业默认值继续执行，切勿就同一事项反复打扰用户；\n\
- 若确需针对不同的新参数/新槽位（如不同字段、新实体或不同操作）提问，请换用更具体详细的独立表述重新提问。",
                        past
                    ));
                }
            }

            // 3. 高度相似比对（Bigram Jaccard 相似度 >= 0.88，且两串长度均至少 8 个字符）
            // 采用 0.88 阈值保护“同模板不同槽位”的合法提问（例如“查询哪个城市” vs “查询哪个日期”）不被误伤
            if norm_incoming.chars().count() >= 8 && norm_past.chars().count() >= 8 {
                if question_bigram_similarity(&norm_incoming, &norm_past) >= 0.88 {
                    return Some(format!(
                        "【重复提问拦截】检测到你在本任务中此前已向用户询问过高度相似的问题：\"{}\"。\n\
用户此前的回复已存在于上下文（【人类回复】）中。\n\
- 若此前提问已涵盖该信息，请直接结合已有回复或行业默认值继续执行，切勿就同一事项反复打扰用户；\n\
- 若确需针对不同的新参数/新槽位（如不同字段、新实体或不同操作）提问，请换用更具体详细的独立表述重新提问。",
                        past
                    ));
                }
            }
        }

        None
    }

    /// 记录已发给人类的提问
    pub fn record_asked_question(&mut self, question: &str) {
        let trimmed = question.trim();
        if !trimmed.is_empty() && !self.asked_human_questions.iter().any(|q| q == trimmed) {
            self.asked_human_questions.push(trimmed.to_string());
        }
    }

    /// 记录人类主人明确批准的特赦/豁免指示（最多保留最近 10 条）
    pub fn record_human_waiver(&mut self, waiver: &str) {
        let trimmed = waiver.trim();
        if trimmed.is_empty() {
            return;
        }
        if !self.human_waivers.iter().any(|w| w == trimmed) {
            self.human_waivers.push(trimmed.to_string());
            if self.human_waivers.len() > 10 {
                self.human_waivers.remove(0);
            }
        }
    }

    /// 汇总当前会话中人类所有生效的特赦与裁决文本
    pub fn human_waiver_text(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(summary) = &self.approved_plan_summary {
            let s = summary.trim();
            if !s.is_empty() {
                parts.push(format!("方案裁决: {}", s));
            }
        }
        for w in &self.human_waivers {
            let item = w.trim();
            if !item.is_empty() && !parts.iter().any(|p| p.contains(item)) {
                parts.push(format!("人工特赦: {}", item));
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n"))
        }
    }
}
