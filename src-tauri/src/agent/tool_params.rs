//! local_shell 的纯参数归一化：只整理明示字段，不猜命令或目录。
use serde_json::Value;

pub(crate) fn value_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub(crate) fn normalize_shell_params(params: &Value) -> Result<Value, String> {
    match params {
        Value::Object(_) => Ok(params.clone()),
        Value::String(command) if !command.trim().is_empty() =>
            Ok(serde_json::json!({"command": command})),
        Value::String(_) => Err("❌ local_shell.params 是空白字符串；期望非空命令字符串或参数对象。本次尚未执行，请填写 params.command。".into()),
        other => Err(format!("❌ local_shell.params 类型错误：期望 object 或非空 string，实际为 {}。本次尚未执行。示例：{{\"tool\":\"local_shell\",\"params\":{{\"command\":\"cargo check\"}}}}", value_type(other))),
    }
}

/// 对象沿用既有参数语义；字符串简写可携带 params 中的选项或同级选项。
/// 两处明示值冲突时不猜、不覆盖；错误只描述位置，不回显命令/环境变量值。
pub(crate) fn resolve_shell_call<'a>(
    command: Option<&Value>,
    params: Option<&Value>,
    extras: impl IntoIterator<Item = (&'a str, &'a Value)>,
) -> Result<Value, String> {
    let selected = match command {
        Some(value @ Value::Object(_)) => value,
        Some(value @ Value::String(_)) => value,
        Some(value) if params.is_none() => value,
        _ => params.unwrap_or(&Value::Null),
    };
    let Value::String(command_text) = selected else {
        return normalize_shell_params(selected);
    };
    let mut result = normalize_shell_params(selected)?
        .as_object()
        .unwrap()
        .clone();
    if command.is_some_and(Value::is_string) {
        match params {
            Some(Value::Object(options)) => {
                for (key, value) in options {
                    if ["command", "cmd", "commandline"].contains(&key.as_str()) {
                        if value.as_str() != Some(command_text.as_str()) {
                            return Err(format!("❌ local_shell 参数冲突：外层 command 与 params.{key} 不一致。本次尚未执行，请只保留一个命令来源，推荐 params 对象。"));
                        }
                    } else { result.insert(key.clone(), value.clone()); }
                }
            }
            Some(Value::Null) | None => {}
            Some(other) => return Err(format!("❌ local_shell.params 类型错误：外层 command 为字符串时，附加选项需为 object，实际为 {}。本次尚未执行，请把命令和选项放进同一个 params 对象。", value_type(other))),
        }
    }
    // 只搬运已声明的 shell 选项；不搬路由字段，也不自动添加 cwd。
    for (key, value) in extras {
        if !["cwd", "args", "env", "timeout_secs", "background"].contains(&key) {
            continue;
        }
        if let Some(existing) = result.get(key) {
            if existing != value {
                return Err(format!("❌ local_shell 参数冲突：同级 {key} 与 params.{key} 不一致。本次尚未执行，请把选项统一放进 params 对象。"));
            }
        } else {
            result.insert(key.into(), value.clone());
        }
    }
    Ok(Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn shell_params_shorthand_is_lossless_and_idempotent() {
        let raw = json!("  printf '%s' '中文;\\n'  ");
        let canonical = normalize_shell_params(&raw).unwrap();
        assert_eq!(canonical["command"], raw);
        assert_eq!(normalize_shell_params(&canonical).unwrap(), canonical);
        assert!(canonical.get("cwd").is_none());
    }
    #[test]
    fn shell_params_objects_keep_aliases_and_unknown_fields() {
        let object = json!({"cmd":"pwd","cwd":"sub","future":true});
        assert_eq!(normalize_shell_params(&object).unwrap(), object);
    }
    #[test]
    fn shell_params_invalid_types_and_blank_strings_are_not_commands() {
        for value in [Value::Null, json!(8), json!(true), json!([]), json!(" \n ")] {
            let error = normalize_shell_params(&value).unwrap_err();
            assert!(error.contains("尚未执行"));
            assert!(!error.contains("审查"));
        }
    }
    #[test]
    fn shell_params_preserve_all_explicit_options_without_guessing_directory() {
        let options = json!({"cwd":"sub","args":["-c","printf ok"],"env":{"CHECK":"ok"},"timeout_secs":90,"background":false,"future":7});
        let actual =
            resolve_shell_call(Some(&json!("sh")), Some(&options), std::iter::empty()).unwrap();
        let mut expected = options.as_object().unwrap().clone();
        expected.insert("command".into(), json!("sh"));
        assert_eq!(actual, Value::Object(expected));
    }
    #[test]
    fn shell_params_conflicting_command_or_options_are_not_silently_overwritten() {
        let error = resolve_shell_call(
            Some(&json!("printf secret-a")),
            Some(&json!({"command":"printf secret-b"})),
            std::iter::empty(),
        )
        .unwrap_err();
        assert!(error.contains("参数冲突") && error.contains("尚未执行"));
        assert!(!error.contains("secret-"));
        let options = json!({"cwd":"one"});
        let extra = json!("two");
        assert!(
            resolve_shell_call(Some(&json!("pwd")), Some(&options), [("cwd", &extra)])
                .unwrap_err()
                .contains("cwd")
        );
    }
}
