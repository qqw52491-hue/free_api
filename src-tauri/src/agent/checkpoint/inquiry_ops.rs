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

    /// 历史提问数达到配置阈值时给出建议，不关闭人工输入通道。
    pub fn inquiry_budget_notice(&self, budget: usize) -> Option<String> {
        (self.asked_human_questions.len() >= budget).then(|| format!(
            "【信息提问次数提示】本任务已记录 {} 条不同问题，达到建议阈值 {}。建议复用已有回答并合并必要问题；仍可询问，不要为避开阈值猜测必要参数或凭据。",
            self.asked_human_questions.len(), budget
        ))
    }

    /// 文本相似只说明历史上问过相似问题，不能证明已有答案或本次无须澄清。
    pub fn similar_question_notice(&self, question: &str) -> Option<String> {
        let incoming = Self::normalize_question(question);
        if incoming.is_empty() { return None; }
        self.asked_human_questions.iter().find(|past| {
            let prior = Self::normalize_question(past);
            !prior.is_empty() && (incoming == prior ||
                (incoming.chars().count() >= 8 && prior.chars().count() >= 8 &&
                 question_bigram_similarity(&incoming, &prior) >= 0.88))
        }).map(|_| "【相似提问提示】历史中记录过相同或相似文本。建议先核对已有回答；若此前未答、情况变化或本次需要澄清，可以继续提问。".to_string())
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

    // ─────────────── Git 操作与人工回复来源对账 ───────────────
    // 宿主只登记被询问命令和版本；自然语言含义交给主模型，不做肯定/否定词匹配。

    /// 该指纹是否已被主人确认放行
    pub fn is_git_destructive_confirmed(&self, fp: &str) -> bool {
        self.git_destructive_approved_scopes.contains_key(fp)
    }

    /// 挂起登记（幂等去重，封顶 16 防膨胀）
    pub fn block_git_destructive(&mut self, fp: &str) {
        let fp = fp.trim();
        if fp.is_empty()
            || self.git_destructive_pending.iter().any(|x| x == fp)
            || self.git_destructive_approved_scopes.contains_key(fp)
        {
            return;
        }
        self.git_destructive_pending.push(fp.to_string());
        if self.git_destructive_pending.len() > 16 {
            self.git_destructive_pending.remove(0);
        }
    }

    /// ask_human 派发时调用：问题文本原文引用了哪些 pending 指纹 → 记入 asked。
    /// 按原始命令精确引用，保留引号内的有效空白。
    pub fn git_destructive_mark_asked(&mut self, ask_msg: &str) {
        let norm = ask_msg;
        self.git_destructive_asked = self
            .git_destructive_pending
            .iter()
            .filter(|fp| norm.contains(fp.as_str()))
            .cloned()
            .collect();
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
