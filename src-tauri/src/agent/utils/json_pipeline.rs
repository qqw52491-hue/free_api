/// 清洗单行/多行注释、Python 特殊常量，规范化单引号与未转义控制字符等
pub fn sanitize_json_tokens(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let chars: Vec<char> = raw.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut in_double_str = false;
    let mut in_single_str = false;
    let mut escape = false;

    while i < len {
        let ch = chars[i];

        if escape {
            escape = false;
            out.push(ch);
            i += 1;
            continue;
        }

        if ch == '\\' && (in_double_str || in_single_str) {
            escape = true;
            out.push(ch);
            i += 1;
            continue;
        }

        // 双引号字符串边界
        if ch == '"' && !in_single_str {
            in_double_str = !in_double_str;
            out.push(ch);
            i += 1;
            continue;
        }

        // 单引号字符串转换成双引号 (在非双引号内)
        if ch == '\'' && !in_double_str {
            in_single_str = !in_single_str;
            out.push('"');
            i += 1;
            continue;
        }

        // 字符串内部的原始控制字符与换行符自动转义
        if in_double_str || in_single_str {
            if ch == '\n' {
                out.push_str("\\n");
            } else if ch == '\r' {
                out.push_str("\\r");
            } else if ch == '\t' {
                out.push_str("\\t");
            } else if (ch as u32) < 0x20 {
                // 忽略非标准低位控制字符
            } else {
                out.push(ch);
            }
            i += 1;
            continue;
        }

        // 非字符串区域：剥离 // 单行注释
        if ch == '/' && i + 1 < len && chars[i + 1] == '/' {
            i += 2;
            while i < len && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        // 非字符串区域：剥离 /* 多行注释 */
        if ch == '/' && i + 1 < len && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < len && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(len);
            continue;
        }

        // 非字符串区域：替换 Python 式布尔与 None
        if ch.is_alphabetic() {
            let start_word = i;
            while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start_word..i].iter().collect();
            match word.as_str() {
                "True" => out.push_str("true"),
                "False" => out.push_str("false"),
                "None" => out.push_str("null"),
                other => out.push_str(other),
            }
            continue;
        }

        out.push(ch);
        i += 1;
    }

    out
}

/// 消除对象或数组尾部多余的逗号 (Trailing Comma), 如 `{"a": 1,}` -> `{"a": 1}`
pub fn sanitize_trailing_commas(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut in_str = false;
    let mut escape = false;

    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if escape {
            escape = false;
            result.push(ch);
            i += 1;
            continue;
        }
        if ch == '\\' && in_str {
            escape = true;
            result.push(ch);
            i += 1;
            continue;
        }
        if ch == '"' {
            in_str = !in_str;
            result.push(ch);
            i += 1;
            continue;
        }
        if !in_str && ch == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                i += 1;
                continue;
            }
        }
        result.push(ch);
        i += 1;
    }
    result
}

/// 智能修复模型输出的不完整或截断的 JSON 结构 (如未闭合引号、括号、Trailing Comma 或 EOF 截断)
pub fn repair_json(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return Some(trimmed.to_string());
    }

    // 1. 深度清洗 Token (注释、换行转义、单引号、Python 关键字)
    let sanitized = sanitize_json_tokens(trimmed);
    let clean_trailing = sanitize_trailing_commas(&sanitized);
    if serde_json::from_str::<serde_json::Value>(&clean_trailing).is_ok() {
        return Some(clean_trailing);
    }

    // 2. 截取最外层 '{'
    let start = match clean_trailing.find('{') {
        Some(s) => s,
        None => return None,
    };
    let slice = &clean_trailing[start..];
    if serde_json::from_str::<serde_json::Value>(slice).is_ok() {
        return Some(slice.to_string());
    }

    // 3. 处理括号与字符串未闭合的截断情况
    let mut candidate = slice.to_string();
    let mut in_str = false;
    let mut escape = false;
    let mut open_braces = 0i32;
    let mut open_brackets = 0i32;

    for ch in candidate.chars() {
        if escape {
            escape = false;
            continue;
        }
        if ch == '\\' && in_str {
            escape = true;
            continue;
        }
        if ch == '"' {
            in_str = !in_str;
            continue;
        }
        if !in_str {
            if ch == '{' {
                open_braces += 1;
            } else if ch == '}' {
                open_braces = (open_braces - 1).max(0);
            } else if ch == '[' {
                open_brackets += 1;
            } else if ch == ']' {
                open_brackets = (open_brackets - 1).max(0);
            }
        }
    }

    if in_str {
        if candidate.ends_with('\\') && !candidate.ends_with("\\\\") {
            candidate.pop();
        }
        candidate.push('"');
    }

    let mut closed = sanitize_trailing_commas(&candidate);
    for _ in 0..open_brackets {
        closed.push(']');
    }
    for _ in 0..open_braces {
        closed.push('}');
    }
    let final_candidate = sanitize_trailing_commas(&closed);

    if serde_json::from_str::<serde_json::Value>(&final_candidate).is_ok() {
        return Some(final_candidate);
    }

    // Ambiguous string boundaries are a protocol error, not a new finish request.
    // The caller retains the original response and uses its existing format retry.
    // Never scrape summary text or promote tools/state updates from malformed JSON.
    None
}

/// 从大模型回复中稳健提取 JSON 纯文本（过滤思考标签与 Markdown 代码块，支持环绕说明文本）
pub fn extract_json_from_text(text: &str) -> Option<String> {
    // 1. 过滤可能混入正文的 <think>...</think> 思考标签或未闭合的前置 <think>
    let without_think = if let Some(start_think) = text.find("<think>") {
        if let Some(end_think) = text.find("</think>") {
            format!("{}{}", &text[..start_think], &text[end_think + 8..])
        } else {
            text.to_string()
        }
    } else {
        text.to_string()
    };

    let cleaned = without_think.trim();

    // 2. 优先提取 ```json ... ``` 或 ``` ... ``` 代码块中的内容
    if let Some(start_code) = cleaned.find("```json") {
        let after_code = &cleaned[start_code + 7..];
        if let Some(end_code) = after_code.find("```") {
            let inner = after_code[..end_code].trim();
            if let Some(repaired) = repair_json(inner) {
                return Some(repaired);
            }
        }
    } else if let Some(start_code) = cleaned.find("```") {
        let after_code = &cleaned[start_code + 3..];
        if let Some(end_code) = after_code.find("```") {
            let inner = after_code[..end_code].trim();
            if let Some(repaired) = repair_json(inner) {
                return Some(repaired);
            }
        }
    }

    if let Some(repaired) = repair_json(cleaned) {
        return Some(repaired);
    }

    // 3. 寻找最外层匹配的 '{' 与 '}'
    let chars: Vec<char> = cleaned.chars().collect();
    let start = chars.iter().position(|c| *c == '{')?;

    let mut depth = 0i32;
    let mut end = start;
    let mut in_string = false;
    let mut escape_next = false;

    for i in start..chars.len() {
        let ch = chars[i];
        if escape_next {
            escape_next = false;
            continue;
        }
        if ch == '\\' && in_string {
            escape_next = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if !in_string {
            if ch == '{' {
                depth += 1;
            }
            if ch == '}' {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
        }
    }

    if depth == 0 && end >= start {
        let json_str: String = chars[start..=end].iter().collect();
        if let Some(repaired) = repair_json(&json_str) {
            return Some(repaired);
        }
    }

    // 4. 尝试从第一个 '{' 到字符串结尾整体修复
    let from_first_brace: String = chars[start..].iter().collect();
    repair_json(&from_first_brace)
}

/// 兼容接口别名
pub fn extract_json_from_markdown(text: &str) -> Option<String> {
    extract_json_from_text(text)
}

/// 智能规范化大模型生成的未整理 Value 对象
pub fn normalize_raw_instruction_value(val: &mut serde_json::Value) {
    if let serde_json::Value::Object(map) = val {
        // 1. 如果 command 是字符串形式的 JSON，尝试解析成 Object
        if let Some(serde_json::Value::String(cmd_str)) = map.get("command") {
            if let Ok(parsed_cmd) = serde_json::from_str::<serde_json::Value>(cmd_str) {
                map.insert("command".to_string(), parsed_cmd);
            }
        }

        // 2. 如果 command 缺失但 params 存在，提升 params 到 command
        if !map.contains_key("command") || map.get("command") == Some(&serde_json::Value::Null) {
            if let Some(params_val) = map.get("params") {
                if !params_val.is_null() {
                    map.insert("command".to_string(), params_val.clone());
                }
            }
        }

        // 3. 如果 todo_update 是 JSON 字符串（如 "[]"），解析；无法解析时保留原值供校验报错。
        if let Some(serde_json::Value::String(todo_str)) = map.get("todo_update") {
            if let Ok(parsed_todo) = serde_json::from_str::<serde_json::Value>(todo_str) {
                map.insert("todo_update".to_string(), parsed_todo);
            }
        }

        // 3.5 模型（尤其 deepseek）常按模板把所有字段填成 ""：
        // 对非字符串类型的顶层协议字段（数字/布尔/对象/数组），空串与省略同义，直接删除，
        // 否则 serde 报 invalid type 导致整次回答作废（实测 775b167b step 17：`"id": ""`）。
        // 仅处理白名单协议字段，绝不改写 command/params 等工具参数。
        for field in [
            "id",
            "new_plan",
            "request_escalation",
            "require_memory",
            "submit_plan",
            "prepare_finish",
            "fail_task",
            "review_contract_update",
            "change_verification_update",
            "self_report",
            "scope_extension",
            "request_retriage",
            "target_blocks_update",
            "commands",
            "memories_update",
            "dispatches",
            "parallel",
            "plan_tree",
            "applied_lessons",
        ] {
            if matches!(map.get(field), Some(serde_json::Value::String(v)) if v.trim().is_empty()) {
                map.remove(field);
            }
        }
        // 数字型 id 被写成字符串数字（"12"）：解析为数字
        if let Some(serde_json::Value::String(id_str)) = map.get("id") {
            if let Ok(n) = id_str.trim().parse::<u64>() {
                map.insert("id".to_string(), serde_json::json!(n));
            }
        }

        // 4. 如果 applied_lessons 是单个字符串，转为数组
        if let Some(serde_json::Value::String(lesson_str)) = map.get("applied_lessons") {
            map.insert(
                "applied_lessons".to_string(),
                serde_json::json!([lesson_str]),
            );
        }
        // 5. 这些字段在指令协议中是集合；模型输出 null 与省略字段语义相同，统一为空数组。
        // 仅处理白名单集合，绝不改写 command/params 等工具参数。
        for field in ["todo_update", "memories_update", "commands"] {
            if map.get(field).is_some_and(serde_json::Value::is_null) {
                map.insert(field.to_string(), serde_json::json!([]));
            }
        }

        // 5.1 批量派发 dispatches 规范化：提升嵌套在 command/params 内的 dispatches；单对象转为单元素数组
        if !map.contains_key("dispatches") {
            if let Some(cmd) = map.get("command").or_else(|| map.get("params")) {
                if let Some(d) = cmd.get("dispatches") {
                    map.insert("dispatches".to_string(), d.clone());
                }
            }
        }
        if let Some(d) = map.get("dispatches") {
            if d.is_object() {
                map.insert("dispatches".to_string(), serde_json::json!([d.clone()]));
            }
        }

        // 6. serde 的 default 仅处理缺失字段；对默认字符串字段，null 与省略同义。
        for field in ["reflection", "thought", "description"] {
            if map.get(field).is_some_and(serde_json::Value::is_null) {
                map.insert(field.to_string(), serde_json::Value::String(String::new()));
            }
        }

        // 7. parallel 是可选集合；false 是模型常见的“未并行”表示，归一为 None。
        if map
            .get("parallel")
            .is_some_and(|value| value.is_null() || value == &serde_json::Value::Bool(false))
        {
            map.insert("parallel".to_string(), serde_json::Value::Null);
        }
    }
}

/// 从未闭合或已闭合的流式 JSON 字符串中，实时提取指定字段的文本内容（自动解码转义字符）
pub fn extract_streaming_json_field(json_text: &str, field_names: &[&str]) -> Option<String> {
    for field in field_names {
        let pattern = format!("\"{}\"", field);
        if let Some(field_idx) = json_text.find(&pattern) {
            let after_field = &json_text[field_idx + pattern.len()..];
            let mut colon_found = false;
            let mut start_quote_offset = None;

            for (idx, ch) in after_field.char_indices() {
                if !colon_found {
                    if ch == ':' {
                        colon_found = true;
                    }
                } else {
                    if ch == '"' {
                        start_quote_offset = Some(field_idx + pattern.len() + idx + 1);
                        break;
                    } else if !ch.is_whitespace() {
                        break;
                    }
                }
            }

            if let Some(start_pos) = start_quote_offset {
                if start_pos >= json_text.len() {
                    return Some(String::new());
                }
                let content_slice = &json_text[start_pos..];
                let mut decoded = String::with_capacity(content_slice.len());
                let mut is_escaped = false;
                let mut chars = content_slice.chars().peekable();

                while let Some(c) = chars.next() {
                    if is_escaped {
                        match c {
                            'n' => decoded.push('\n'),
                            'r' => decoded.push('\r'),
                            't' => decoded.push('\t'),
                            '\\' => decoded.push('\\'),
                            '"' => decoded.push('"'),
                            '/' => decoded.push('/'),
                            'u' => {
                                let mut hex = String::new();
                                for _ in 0..4 {
                                    if let Some(&hc) = chars.peek() {
                                        if hc.is_ascii_hexdigit() {
                                            hex.push(hc);
                                            chars.next();
                                        } else {
                                            break;
                                        }
                                    }
                                }
                                if hex.len() == 4 {
                                    if let Ok(code) = u32::from_str_radix(&hex, 16) {
                                        if let Some(unicode_char) = char::from_u32(code) {
                                            decoded.push(unicode_char);
                                        }
                                    }
                                }
                            }
                            other => decoded.push(other),
                        }
                        is_escaped = false;
                    } else if c == '\\' {
                        is_escaped = true;
                    } else if c == '"' {
                        return Some(decoded);
                    } else {
                        decoded.push(c);
                    }
                }

                return Some(decoded);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归：模型把所有字段填成 ""（含 id/applied_lessons/parallel）不再导致结构转换失败。
    #[test]
    fn empty_string_template_fields_are_treated_as_missing() {
        let mut v = serde_json::json!({
            "reflection": "r", "thought": "", "description": "d",
            "finding": "", "finding_kind": "", "plan_id": "p", "plan_title": "t",
            "session_title": "", "new_plan": "", "applied_lessons": "",
            "tool": "local_shell", "action": "run", "command": {"cmd": "ls"},
            "id": "", "submit_plan": "", "prepare_finish": "",
            "commands": "", "parallel": "", "dispatches": "", "request_escalation": ""
        });
        normalize_raw_instruction_value(&mut v);
        let inst: crate::agent::types::AgentInstruction = serde_json::from_value(v).unwrap();
        assert_eq!(inst.id, None);
        assert!(inst.applied_lessons.is_none());
        assert!(inst.parallel.is_none());
        assert!(inst.submit_plan.is_none());
        assert_eq!(inst.tool.as_deref(), Some("local_shell"));

        // 数字字符串 id 解析为数字；非空 applied_lessons 仍转数组
        let mut v2 = serde_json::json!({"id": "12", "applied_lessons": "L1", "tool": "x"});
        normalize_raw_instruction_value(&mut v2);
        let i2: crate::agent::types::AgentInstruction = serde_json::from_value(v2).unwrap();
        assert_eq!(i2.id, Some(12));
        assert_eq!(i2.applied_lessons, Some(vec!["L1".to_string()]));
    }

    #[test]
    fn test_normalize_raw_instruction_value_dispatches() {
        // 1. command 内的 dispatches 提升到顶层
        let mut v1 = serde_json::json!({
            "tool": "child_worker",
            "command": {
                "dispatches": [
                    { "task": "A" },
                    { "task": "B" }
                ]
            }
        });
        normalize_raw_instruction_value(&mut v1);
        assert!(v1.get("dispatches").is_some());
        assert_eq!(v1["dispatches"].as_array().unwrap().len(), 2);

        // 2. 单对象 dispatches 转为单元素数组
        let mut v2 = serde_json::json!({
            "tool": "child_worker",
            "dispatches": { "task": "单对象" }
        });
        normalize_raw_instruction_value(&mut v2);
        assert!(v2["dispatches"].is_array());
        assert_eq!(v2["dispatches"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_repair_json_ambiguous_summary_does_not_salvage_state_updates() {
        // Unescaped quotes do not establish where summary ends or state updates begin.
        let broken = concat!(
            "{\"tool\": \"finish\", \"command\": {\"summary\": \"报告引用 \"target_files\" 字段，分析完成。\"\n  },\n  ",
            "\"change_verification_update\": null}"
        );
        assert!(serde_json::from_str::<serde_json::Value>(broken).is_err());
        assert!(repair_json(broken).is_none());
    }
}

