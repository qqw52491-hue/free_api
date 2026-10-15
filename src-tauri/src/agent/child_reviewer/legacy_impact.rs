//! 仅在重放时移除已退出的宿主消费点义务；不改写历史记录或真实 findings。

use crate::agent::protocol::ContractSchema;
use serde_json::Value;

const PACK_START: &str = "【🧨 影响面机械枚举";
const PACK_END: &str =
    "本清单为宿主机械枚举的下限，仅保证列出项有据可查，不替代你主动探查清单外的耦合点。";

pub(crate) fn retire_consumer_schema(schema: &mut ContractSchema) {
    schema.fields.retain(|f| f.name != "consumer_verdicts");
}

fn retire_consumer_text(text: &str) -> String {
    let mut text = text.to_string();
    while let Some(start) = text.find(PACK_START) {
        let tail = &text[start..];
        let end = tail
            .find(PACK_END)
            .map(|n| n + PACK_END.len())
            .unwrap_or_else(|| {
                // 兼容旧版截断材料，保留紧随其后的写方线索或其他材料段。
                tail.find("【✍️ 写方契约情报】")
                    .or_else(|| tail.find("\n\n【"))
                    .unwrap_or(tail.len())
            });
        text.replace_range(start..start + end, "");
    }
    text.split_inclusive('\n')
        .filter(|line| {
            !line.contains("【影响面消费点强制逐条裁决】")
                && !line.trim_start().starts_with("- `consumer_verdicts` (")
                && !line
                    .trim_start()
                    .starts_with("输出 JSON 必须额外包含 \"consumer_verdicts\"")
        })
        .collect()
}

pub(crate) fn retire_consumer_params(params: &Value) -> Value {
    let mut params = params.clone();
    if let Some(map) = params.as_object_mut() {
        map.remove("impact_pack_refs");
        for key in ["evidence_excerpt", "specialized_context"] {
            if let Some(Value::String(text)) = map.get_mut(key) {
                *text = retire_consumer_text(text);
            }
        }
    }
    params
}

pub(crate) fn retire_consumer_history(messages: &mut Vec<Value>) {
    messages.retain_mut(|message| {
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        // 模型历史判决（含旧字段、真实 finding）照常保留。
        if !matches!(role, "system" | "user") {
            return true;
        }
        let Some(Value::String(text)) = message.get_mut("content") else {
            return true;
        };
        let is_coverage_retry = text.starts_with("【系统】")
            && (text.contains("consumer_verdicts 覆盖不完整")
                || (text.contains("契约违例")
                    && (text.contains("consumer_verdicts 漏裁决")
                        || text.contains("缺少必填字段 consumer_verdicts"))));
        if is_coverage_retry {
            return false;
        }
        *text = retire_consumer_text(text);
        true
    });
}

pub(crate) fn retire_consumer_schema_json(schema_json: &mut Option<String>) {
    if let Some(raw) = schema_json {
        if let Ok(mut schema) = serde_json::from_str::<ContractSchema>(raw) {
            retire_consumer_schema(&mut schema);
            if let Ok(cleaned) = serde_json::to_string(&schema) {
                *raw = cleaned;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn old_snapshot_and_resume_history_preserve_evidence_and_other_obligations() {
        let pack = format!("{PACK_START}（宿主静态扫描结果，非模型推断）】\n旧消费点\n【硬义务】输出 consumer_verdicts\n{PACK_END}");
        let write_context = "【✍️ 写方契约情报】src/writer.rs:1";
        let old = json!({"impact_pack_refs": ["agent_logs/old.json:10"],
            "evidence_scope_refs": ["src/other.rs"],
            "evidence_excerpt": format!("真实需求与 diff\n{pack}{write_context}"),
            "specialized_context": format!("领域材料\n{pack}{write_context}")});
        let cleaned = retire_consumer_params(&old);
        assert!(cleaned.get("impact_pack_refs").is_none());
        assert_eq!(cleaned["evidence_scope_refs"], old["evidence_scope_refs"]);
        assert!(cleaned["evidence_excerpt"]
            .as_str()
            .unwrap()
            .contains("真实需求与 diff"));
        assert!(cleaned["specialized_context"]
            .as_str()
            .unwrap()
            .contains(write_context));
        assert!(!cleaned.to_string().contains("consumer_verdicts"));
        assert!(old.get("impact_pack_refs").is_some(), "原快照不改写");

        let historical_result = json!({"role": "assistant", "content":
            "{\"consumer_verdicts\":[],\"findings\":[{\"severity\":\"blocking\"}]}"});
        let mut history = vec![
            json!({"role":"system","content": "审查规则\n        8. 【影响面消费点强制逐条裁决】必须 consumer_verdicts\n        9. 【证据范围强制逐条裁决】保留\n- `consumer_verdicts` (string[], 必填): 旧字段\n- `root_cause_verdict` (string, 必填): 保留"}),
            json!({"role":"user","content":old["evidence_excerpt"]}),
            historical_result.clone(),
            json!({"role":"user","content":"【系统】契约违例（第 1/2 次）：consumer_verdicts 漏裁决跨文件消费点。请补全"}),
            json!({"role":"user","content":"【系统】契约违例（第 1/2 次）：缺少必填字段 consumer_verdicts：审查包附带 2 个跨文件消费点须逐条裁决"}),
            json!({"role":"user","content":"【系统】契约违例：evidence_scope_verdicts 漏裁决证据范围文件"}),
        ];
        retire_consumer_history(&mut history);
        assert_eq!(history.len(), 4);
        assert_eq!(history[2], historical_result);
        assert!(history[0]["content"]
            .as_str()
            .unwrap()
            .contains("root_cause_verdict"));
        assert!(history[0]["content"]
            .as_str()
            .unwrap()
            .contains("证据范围强制逐条裁决"));
        assert!(!history[0]["content"]
            .as_str()
            .unwrap()
            .contains("consumer_verdicts"));
        assert!(history[1]["content"]
            .as_str()
            .unwrap()
            .contains(write_context));
        assert!(history[3]["content"]
            .as_str()
            .unwrap()
            .contains("evidence_scope_verdicts"));
    }

    #[test]
    fn truncated_pack_does_not_swallow_following_write_context() {
        let old = format!("原始证据\n{PACK_START}】被截断的清单\n【✍️ 写方契约情报】现有写方");
        assert_eq!(
            retire_consumer_text(&old),
            "原始证据\n【✍️ 写方契约情报】现有写方"
        );
    }

    #[test]
    fn resumed_identity_retires_only_consumer_field_without_changing_original() {
        use crate::agent::child_reviewer::ChildRunIdentity;
        let raw = serde_json::json!({"strict_closed_set": true, "fields": [
            {"name":"consumer_verdicts","field_type":"string[]","description":"旧字段","required":true},
            {"name":"root_cause_verdict","field_type":"string","description":"病根核对","required":true}
        ]}).to_string();
        let previous = ChildRunIdentity { contract_schema: Some(raw.clone()), ..ChildRunIdentity::fresh("review") };
        let mut resumed = ChildRunIdentity::inherited(&previous, Some("rebuttal".into()));
        retire_consumer_schema_json(&mut resumed.contract_schema);
        let current: ContractSchema = serde_json::from_str(resumed.contract_schema.as_ref().unwrap()).unwrap();
        assert!(current.validate_shape(&json!({"root_cause_verdict":"explains"})).is_ok());
        assert!(current.validate_shape(&json!({})).unwrap_err().contains("root_cause_verdict"));
        assert_eq!(previous.contract_schema, Some(raw));
        assert_eq!(resumed.task_id, previous.task_id);
    }
}
