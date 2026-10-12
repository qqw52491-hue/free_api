use crate::agent::context::budget::{conservative_token_estimate, ContextBudget};
use crate::agent::context::types::{ChatMessage, SandwichContext};
use crate::agent::types::AgentInstruction;
use serde_json::json;

pub const SAMPLER_VERSION: u32 = 4;

fn targeted_verification_guidance(
    output_id: &str,
    original_tokens: usize,
    ingress_cap: usize,
) -> String {
    format!(
        "[宿主提示：本次执行输出约 {} Tokens，超过单条上下文上限 {} Tokens，已保存为工件 output_id={} 并仅展示采样。当前片段不能证明未展示部分的细节；但无需通读全文。只有当结论确实依赖缺失内容时，才按文件路径、函数/符号、错误码、关键词或 diff hunk 定向搜索，再通过 task_store/search_step_output 或带 offset 的 get_step_output 获取最小必要片段。大范围回查会消耗工作集预算。]",
        original_tokens, ingress_cap, output_id
    )
}

/// 历史动作摘要中单个字符串的保留上限（超出部分省略，完整内容仍在工具输出工件里）。
const HISTORY_STR_MAX_CHARS: usize = 160;
/// 历史动作摘要中数组的保留项数。
const HISTORY_ARRAY_MAX_ITEMS: usize = 8;
/// 历史动作摘要总长上限（字符），超出则丢弃参数仅保留工具与描述。
const HISTORY_ACTION_MAX_CHARS: usize = 3000;

fn compact_history_value(v: &serde_json::Value, depth: usize) -> serde_json::Value {
    use serde_json::Value;
    if depth > 6 {
        return Value::String("…".to_string());
    }
    match v {
        Value::String(s) => {
            let n = s.chars().count();
            if n > HISTORY_STR_MAX_CHARS {
                let head: String = s.chars().take(HISTORY_STR_MAX_CHARS).collect();
                Value::String(format!("{}…(已省略{}字)", head, n - HISTORY_STR_MAX_CHARS))
            } else {
                v.clone()
            }
        }
        Value::Array(a) => {
            let mut out: Vec<Value> = a
                .iter()
                .take(HISTORY_ARRAY_MAX_ITEMS)
                .map(|x| compact_history_value(x, depth + 1))
                .collect();
            if a.len() > HISTORY_ARRAY_MAX_ITEMS {
                out.push(Value::String(format!(
                    "…(另有{}项)",
                    a.len() - HISTORY_ARRAY_MAX_ITEMS
                )));
            }
            Value::Array(out)
        }
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), compact_history_value(x, depth + 1)))
                .collect(),
        ),
        _ => v.clone(),
    }
}

/// 把已执行的指令压成一条紧凑 JSON 写进 assistant 历史。
/// 模型即使模仿这种历史形态，输出也是合法的指令 JSON，不再触发解析失败。
pub(crate) fn history_action_json(instruction: &AgentInstruction, plan_title: Option<&str>) -> String {
    use serde_json::{Map, Value};
    let mut obj = Map::new();
    if let Some(t) = plan_title.filter(|t| !t.is_empty()) {
        obj.insert("plan_title".to_string(), json!(t));
    }
    if !instruction.description.is_empty() {
        obj.insert("description".to_string(), json!(instruction.description));
    }
    let mut params_key: Option<(&str, Value)> = None;
    if let Some(par) = &instruction.parallel {
        params_key = Some(("parallel", serde_json::to_value(par).unwrap_or(Value::Null)));
    } else if let Some(tree) = &instruction.plan_tree {
        params_key = Some(("plan_tree", serde_json::to_value(tree).unwrap_or(Value::Null)));
    } else if !instruction.commands.is_empty() {
        params_key = Some(("commands", serde_json::to_value(&instruction.commands).unwrap_or(Value::Null)));
    } else {
        let tool = instruction.get_tool();
        let action = instruction.get_action();
        if !action.is_empty() && action != tool {
            obj.insert("action".to_string(), json!(action));
        }
        obj.insert("tool".to_string(), json!(tool));
        let params = instruction.get_params();
        let empty = params.is_null() || params.as_object().is_some_and(|m| m.is_empty());
        if !empty {
            params_key = Some(("params", params));
        }
    }
    let mut with_params = obj.clone();
    if let Some((k, v)) = params_key {
        with_params.insert(k.to_string(), compact_history_value(&v, 0));
        let full = Value::Object(with_params).to_string();
        if full.chars().count() <= HISTORY_ACTION_MAX_CHARS {
            return full;
        }
        obj.insert(k.to_string(), json!("(参数过长已省略)"));
    }
    Value::Object(obj).to_string()
}

impl SandwichContext {
    /// 写入工具执行结果（Append-only 线性历史追加，最大化 Prompt Cache 命中率）。
    ///
    /// 设计原则：
    /// - 历史在平时保持严格 Append-Only，避免在历史中间随机篡改而破坏前缀缓存。
    /// - 单步入口依据保守 Token 估算；超过上限时保留头尾采样，并把完整原文持久化为工件。
    /// - 截断仅表示当前证据不完整，不强迫模型通读原文；应先按符号、关键词或行范围定向核验。
    pub fn add_step(
        &mut self,
        instruction: &AgentInstruction,
        output_summary: &str,
        output_id: &str,
    ) {
        // 历史里的动作摘要用模型自己的输出形态（JSON），而不是「计划: X | 工具: Y」纯文本：
        // 实测 976ca63b：deepseek 见到满屏纯文本的历史后会模仿它，22% 的调用解析失败。
        let action_brief = history_action_json(instruction, self.active_plan_title());
        self.turns_history.push(ChatMessage {
            role: "assistant".to_string(),
            content: json!(action_brief),
        });

        let ingress_tokens = conservative_token_estimate(output_summary);
        let ingress_cap = ContextBudget::SINGLE_OBSERVATION_INGRESS_CAP;
        let char_count = output_summary.chars().count();
        let token_char_ratio = if char_count > 0 {
            ingress_tokens as f64 / char_count as f64
        } else {
            1.0
        };

        let feedback_str = if ingress_tokens > ingress_cap {
            let safe_cap_tokens = ingress_cap.saturating_sub(600) as f64;
            let safe_cap_chars = (safe_cap_tokens / token_char_ratio).max(100.0) as usize;
            let mut head_chars = (safe_cap_chars as f64 * 0.4) as usize;
            let mut tail_chars = (safe_cap_chars as f64 * 0.6) as usize;
            let mut sampled =
                crate::agent::utils::sample_execution_log(output_summary, head_chars, tail_chars);
            let mut candidate = format!(
                "【执行输出采样 (v{})】\noutput_id: {}\nestimated_tokens: {}\ntruncated: true\n{}\n\n{}",
                SAMPLER_VERSION,
                output_id,
                ingress_tokens,
                sampled,
                targeted_verification_guidance(output_id, ingress_tokens, ingress_cap),
            );

            while conservative_token_estimate(&candidate) > ingress_cap
                && (head_chars > 100 || tail_chars > 150)
            {
                head_chars = (head_chars as f64 * 0.70) as usize;
                tail_chars = (tail_chars as f64 * 0.70) as usize;
                sampled = crate::agent::utils::sample_execution_log(
                    output_summary,
                    head_chars,
                    tail_chars,
                );
                candidate = format!(
                    "【执行输出采样 (v{})】\noutput_id: {}\nestimated_tokens: {}\ntruncated: true\n{}\n\n{}",
                    SAMPLER_VERSION,
                    output_id,
                    ingress_tokens,
                    sampled,
                    targeted_verification_guidance(output_id, ingress_tokens, ingress_cap),
                );
            }
            candidate
        } else {
            format!(
                "【执行输出 (v{})】\noutput_id: {}\n{}",
                SAMPLER_VERSION, output_id, output_summary
            )
        };

        self.turns_history.push(ChatMessage {
            role: "user".to_string(),
            content: json!(feedback_str),
        });
        self.maybe_compact_history();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instr(v: serde_json::Value) -> AgentInstruction {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn history_action_is_json_and_mimicking_it_stays_parseable() {
        let i = instr(json!({
            "description": "读取控制器并检索入口",
            "parallel": [
                {"tool": "local_file", "args": {"action": "read", "path": "a.java"}},
                {"tool": "search", "args": {"query": "saveCase"}}
            ]
        }));
        let s = history_action_json(&i, Some("定位入口"));
        assert!(!s.contains("计划:") && !s.contains(" | 工具:"), "{}", s);
        // 模型模仿历史形态输出时，结果仍是带 parallel 的合法指令
        let back: AgentInstruction = serde_json::from_str(&s).unwrap();
        assert_eq!(back.parallel.as_ref().map(|p| p.len()), Some(2));
        assert_eq!(back.description, "读取控制器并检索入口");
    }

    #[test]
    fn history_action_plain_tool_keeps_tool_action_params() {
        let i = instr(json!({
            "description": "读文件",
            "tool": "local_file",
            "action": "read",
            "params": {"path": "src/a.rs"}
        }));
        let s = history_action_json(&i, None);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["tool"], "local_file");
        assert_eq!(v["action"], "read");
        assert_eq!(v["params"]["path"], "src/a.rs");
        assert!(v.get("plan_title").is_none());
    }

    #[test]
    fn history_action_truncates_long_strings_and_caps_total_size() {
        let big = "x".repeat(5000);
        let i = instr(json!({
            "description": "写大文件",
            "tool": "local_file",
            "action": "write",
            "params": {"path": "a.txt", "content": big}
        }));
        let s = history_action_json(&i, None);
        assert!(s.contains("已省略"), "{}", s);
        assert!(s.chars().count() < 600, "len={}", s.chars().count());
        serde_json::from_str::<serde_json::Value>(&s).unwrap();

        let many: Vec<serde_json::Value> = (0..6)
            .map(|n| json!({"path": format!("{}{}", "y".repeat(159), n), "k": "z".repeat(159)}))
            .collect();
        let i2 = instr(json!({"description": "批", "commands": many}));
        let s2 = history_action_json(&i2, None);
        assert!(s2.chars().count() <= HISTORY_ACTION_MAX_CHARS + 200, "{}", s2.chars().count());
        serde_json::from_str::<serde_json::Value>(&s2).unwrap();
    }

    #[test]
    fn truncated_guidance_requires_targeted_evidence_not_full_replay() {
        let guidance = targeted_verification_guidance("out-1", 10_000, 4_000);
        assert!(guidance.contains("无需通读全文"));
        assert!(guidance.contains("定向搜索"));
        assert!(guidance.contains("output_id=out-1"));
        assert!(!guidance.contains("必须首先"));
        assert!(!guidance.contains("绝对禁止"));
    }
}
