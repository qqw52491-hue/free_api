use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub(crate) struct Action {
    pub(crate) action: String,
    #[serde(default)]
    pub(crate) intent: Option<String>,
    #[serde(default)]
    pub(crate) params: Value,
    #[serde(default)]
    pub(crate) findings: Vec<String>,
    #[serde(default)]
    pub(crate) confidence: String,
    #[serde(default)]
    pub(crate) evidence_refs: Vec<String>,
    #[serde(default)]
    pub(crate) missing_evidence: Vec<String>,
}

/// 只补充解析失败的诊断，不改变 Action 的字段、默认值或兼容范围。
pub(crate) fn parse_action(raw: &str) -> Result<Action, String> {
    let json_text = extract_json(raw).ok_or_else(|| {
        "【宿主反馈】侦查动作 JSON 提取失败：未找到完整的 JSON 对象。本次动作未执行，也未形成有效侦查结论。请重新输出一个完整的 JSON 对象，使用 action 指定动作（收尾使用 action=\"finish\"）。".to_string()
    })?;
    let mut deserializer = serde_json::Deserializer::from_str(json_text);
    let action = serde_path_to_error::deserialize(&mut deserializer).map_err(|error| {
        let path = error.path().to_string();
        action_parse_feedback(error.inner(), &path)
    })?;
    // 与原 serde_json::from_str 保持一致：不能忽略对象后的残留内容。
    deserializer
        .end()
        .map_err(|error| action_parse_feedback(&error, ""))?;
    Ok(action)
}

fn action_parse_feedback(error: &serde_json::Error, path: &str) -> String {
    let layer = if error.is_data() {
        "字段解析失败"
    } else {
        "JSON 语法解析失败"
    };
    let field = if path.is_empty() || path == "." {
        String::new()
    } else {
        format!("；字段位置：{}", path)
    };
    format!(
        "【宿主反馈】侦查动作{}{}。具体原因：{}。本次动作未执行，也未形成有效侦查结论。请按上述原因修正并重新输出完整 JSON。action 是字符串；intent 如提供应为字符串或 null；findings、evidence_refs、missing_evidence 如提供应为字符串数组；confidence 如提供应为字符串。工具参数放在 params 中；收尾使用 action=\"finish\" 并携带 findings。",
        layer, field, error
    )
}

/// 从模型输出中提取首个 JSON 对象（容忍围栏/前后缀噪声）
pub(crate) fn extract_json(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let bytes = raw.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, &byte) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&raw[start..=index]);
                }
            }
            _ => {}
        }
    }
    None
}
