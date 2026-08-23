pub mod json_pipeline;
pub mod llm_client;
pub mod log_sampler;
pub mod subprocess;

pub use json_pipeline::*;
pub use llm_client::*;
pub use log_sampler::*;
pub use subprocess::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::types::AgentInstruction;

    #[test]
    fn test_repair_json_trailing_commas_and_comments() {
        let raw = r#"{
            // 这是一个测试动作
            "tool": "ask_human",
            "command": {
                "message": "hello",
            },
        }"#;
        let repaired = repair_json(raw).expect("should repair trailing commas and comments");
        let val: serde_json::Value =
            serde_json::from_str(&repaired).expect("must parse valid JSON");
        assert_eq!(val["tool"], "ask_human");
        assert_eq!(val["command"]["message"], "hello");
    }

    #[test]
    fn test_repair_json_unescaped_newlines_in_strings() {
        let raw = "{\n\"tool\": \"ask_human\",\n\"command\": {\n\"message\": \"Line 1\nLine 2 with \ttab\"\n}\n}";
        let repaired = repair_json(raw).expect("should repair unescaped newlines in strings");
        let val: serde_json::Value =
            serde_json::from_str(&repaired).expect("must parse valid JSON");
        assert_eq!(val["tool"], "ask_human");
        assert!(val["command"]["message"]
            .as_str()
            .unwrap()
            .contains("Line 1"));
    }

    #[test]
    fn test_repair_json_python_literals_and_single_quotes() {
        let raw = "{'tool': 'model_manager', 'command': {'confirm': True, 'null_field': None, 'flag': False}}";
        let repaired = repair_json(raw).expect("should repair single quotes and python literals");
        let val: serde_json::Value =
            serde_json::from_str(&repaired).expect("must parse valid JSON");
        assert_eq!(val["tool"], "model_manager");
        assert_eq!(val["command"]["confirm"], true);
        assert_eq!(val["command"]["flag"], false);
        assert!(val["command"]["null_field"].is_null());
    }

    #[test]
    fn test_repair_json_eof_truncation() {
        let raw =
            r#"{"tool": "ask_human", "command": {"message": "Incomplete text that was cut off"#;
        let repaired = repair_json(raw).expect("should repair truncated JSON");
        let val: serde_json::Value =
            serde_json::from_str(&repaired).expect("must parse valid JSON");
        assert_eq!(val["tool"], "ask_human");
        assert!(val["command"]["message"]
            .as_str()
            .unwrap()
            .contains("Incomplete text"));
    }

    #[test]
    fn test_extract_json_from_markdown_with_extra_text() {
        let raw = "Here is my response:\n```json\n{\n  \"tool\": \"web_search\",\n  \"command\": {\"query\": \"rust\"}\n}\n```\nHope that helps!";
        let extracted = extract_json_from_text(raw).expect("should extract json from markdown");
        let val: serde_json::Value =
            serde_json::from_str(&extracted).expect("must parse valid JSON");
        assert_eq!(val["tool"], "web_search");
        assert_eq!(val["command"]["query"], "rust");
    }

    #[test]
    fn test_normalize_raw_instruction_value() {
        let mut val = serde_json::json!({
            "tool": "web_search",
            "command": "{\"query\": \"rust agent\"}",
            "reflection": null,
            "thought": null,
            "description": null,
            "todo_update": "[{\"id\": \"1\", \"task\": \"Search web\", \"status\": \"done\"}]",
            "memories_update": null,
            "commands": null,
            "parallel": false,
            "applied_lessons": "lesson_cache_1"
        });
        normalize_raw_instruction_value(&mut val);
        let inst: AgentInstruction =
            serde_json::from_value(val).expect("must deserialize AgentInstruction");
        assert_eq!(inst.tool, Some("web_search".to_string()));
        assert_eq!(inst.command.unwrap()["query"], "rust agent");
        assert_eq!(inst.reflection, "");
        assert_eq!(inst.thought, "");
        assert_eq!(inst.description, "");
        assert_eq!(inst.todo_update.len(), 1);
        assert!(inst.memories_update.is_empty());
        assert!(inst.commands.is_empty());
        assert_eq!(inst.parallel, None);
        assert_eq!(inst.todo_update[0].id, 1);
        assert_eq!(inst.todo_update[0].description, "Search web");
        assert_eq!(
            inst.applied_lessons,
            Some(vec!["lesson_cache_1".to_string()])
        );
    }

    #[test]
    fn test_sample_execution_log_short() {
        let short = "Everything is ok! Exit code 0.";
        let sampled = sample_execution_log(short, 300, 500);
        assert_eq!(sampled, short);
    }

    #[test]
    fn test_sample_execution_log_long_with_errors() {
        let mut long_text = String::from("Compiling crate xyz v0.1.0...\n");
        for i in 0..50 {
            if i == 25 {
                long_text.push_str("error: failed to resolve type FooBar\n");
            } else if i == 30 {
                long_text.push_str("panic: index out of bounds at main.rs:42\n");
            } else {
                long_text.push_str(&format!("warning: unused variable var_{}\n", i));
            }
        }
        long_text.push_str("Finished with error code 101.\n");

        let sampled = sample_execution_log(&long_text, 50, 60);
        assert!(sampled.contains("Compiling crate"));
        assert!(sampled.contains("error: failed to resolve type FooBar"));
        assert!(sampled.contains("panic: index out of bounds"));
        assert!(sampled.contains("Finished with error code 101"));
        assert!(sampled.contains("折叠中间"));
    }

    #[test]
    fn test_sample_execution_log_deduplication() {
        let mut text = String::from("Start log...\n");
        for _ in 0..100 {
            text.push_str("error: connection reset by peer\n");
        }
        text.push_str("End log with exit status 1\n");
        let sampled = sample_execution_log(&text, 20, 30);
        assert!(sampled.contains("×100") || sampled.contains("connection reset by peer"));
    }

    #[test]
    fn test_extract_streaming_json_field() {
        let partial_json = "{\"tool\": \"finish\", \"command\": {\"summary\": \"### 报告\\n- 第一行\\n- 带有\\\"引号\\\"的内容";
        let extracted = extract_streaming_json_field(partial_json, &["summary"]);
        assert_eq!(
            extracted,
            Some("### 报告\n- 第一行\n- 带有\"引号\"的内容".to_string())
        );

        let closed_json =
            "{\"tool\": \"finish\", \"command\": {\"summary\": \"完成了！\", \"other\": 123}}";
        let extracted_closed = extract_streaming_json_field(closed_json, &["summary"]);
        assert_eq!(extracted_closed, Some("完成了！".to_string()));
    }
}
