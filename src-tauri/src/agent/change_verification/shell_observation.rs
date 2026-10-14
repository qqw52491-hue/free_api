//! 仅用于副作用记账，不改变工具权限，也不把流水线退出 0 当作测试通过。
use crate::agent::context::AuditLedger;
use serde_json::json;

/// 只识别可逐段核对的读取/验证脚本。复杂控制流、写入、未知子命令仍保守记账。
pub fn is_observation_or_validation_script(command: &str) -> bool {
    let Some(sanitized) = observation_substitutions(command) else {
        return false;
    };
    let Some(parts) = segments(&sanitized) else {
        return false;
    };
    !parts.is_empty()
        && parts.iter().all(|p| {
            let trimmed = p.trim();
            if assignment_only(trimmed) {
                return true;
            }
            AuditLedger::is_readonly_shell_command(&json!({"command":trimmed}))
                || verification_segment(trimmed)
        })
}

/// 只有单次验证（或 cd && 单次验证）可按工具成功自动认账；管道/分号/|| 会遮盖前段退出码。
pub fn is_simple_verification(command: &str) -> bool {
    if command.contains([';', '|', '\n', '`']) || command.contains("$(") || command.contains("&>") {
        return false;
    }
    let parts: Vec<&str> = command.split("&&").map(str::trim).collect();
    match parts.as_slice() {
        [one] => verification_segment(one),
        [cd, one]
            if cd.starts_with("cd ")
                && AuditLedger::is_readonly_shell_command(&json!({"command":cd})) =>
        {
            verification_segment(one)
        }
        _ => false,
    }
}

fn assignment_only(s: &str) -> bool {
    let Some((name, value)) = s.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && !value.is_empty()
        && name
            .bytes()
            .enumerate()
            .all(|(i, c)| c == b'_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
        && !value.contains(char::is_whitespace)
        && !value.contains(['>', '<', '&', '`', '(', ')'])
}

fn verification_segment(s: &str) -> bool {
    let s = s.replace("2>&1", "");
    if s.contains([';', '|', '&', '>', '<', '`', '\n']) || s.contains("$(") {
        return false;
    }
    let mut tokens: Vec<&str> = s.split_whitespace().collect();
    while tokens.first().is_some_and(|t| assignment_only(t)) {
        tokens.remove(0);
    }
    let Some(head) = tokens.first().copied() else {
        return false;
    };
    match head {
        "cargo" => matches!(
            tokens.get(1),
            Some(&"test" | &"check" | &"build" | &"clippy")
        ),
        "mvn" | "mvnw" | "./mvnw" => {
            tokens
                .iter()
                .any(|t| matches!(*t, "test" | "verify" | "test-compile"))
                && !tokens
                    .iter()
                    .any(|t| matches!(*t, "deploy" | "install" | "clean"))
        }
        "deno" => tokens.get(1) == Some(&"test"),
        "go" => matches!(tokens.get(1), Some(&"test" | &"vet")),
        "npm" | "pnpm" | "bun" | "yarn" => {
            matches!(
                tokens.get(1),
                Some(&"test" | &"typecheck" | &"check" | &"lint" | &"build")
            ) || (tokens.get(1) == Some(&"run")
                && matches!(
                    tokens.get(2),
                    Some(&"test" | &"typecheck" | &"type-check" | &"check" | &"lint" | &"build")
                ))
        }
        "npx" => matches!(
            tokens.get(1),
            Some(&"vitest" | &"jest" | &"tsc" | &"vue-tsc" | &"eslint")
        ),
        "pytest" | "ctest" | "mypy" | "flake8" | "eslint" | "golangci-lint" | "tsc" | "vue-tsc" => {
            true
        }
        "python" | "python3" => {
            tokens.get(1) == Some(&"-m") && matches!(tokens.get(2), Some(&"pytest" | &"unittest"))
        }
        "gradle" | "./gradlew" => tokens.get(1) == Some(&"test"),
        "ruff" | "biome" => tokens.get(1) == Some(&"check"),
        _ => false,
    }
}

fn observation_substitutions(command: &str) -> Option<String> {
    if command.contains('`') || command.contains("<(") || command.contains(">(") {
        return None;
    }
    let mut out = command.to_string();
    while let Some(a) = out.find("$(") {
        let end = out[a + 2..].find(')')? + a + 2;
        let inner = &out[a + 2..end];
        if inner.contains(['(', ')', '\\'])
            || !AuditLedger::is_readonly_shell_command(&json!({"command":inner}))
        {
            return None;
        }
        out.replace_range(a..=end, "__observed_value__");
    }
    Some(out)
}

fn segments(s: &str) -> Option<Vec<&str>> {
    let b = s.as_bytes();
    let mut parts = Vec::new();
    let mut quote = None;
    let mut begin = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            i += 2;
            continue;
        }
        if let Some(q) = quote {
            if b[i] == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if b[i] == b'\'' || b[i] == b'"' {
            quote = Some(b[i]);
            i += 1;
            continue;
        }
        // 仅 stderr 合并，不接受写文件重定向。
        if b[i..].starts_with(b"2>&1") {
            i += 4;
            continue;
        }
        if matches!(b[i], b'>' | b'<' | b'(' | b')' | b'{' | b'}') {
            return None;
        }
        if matches!(b[i], b';' | b'\n' | b'|' | b'&') {
            if b[i] == b'&' && b.get(i + 1) != Some(&b'&') {
                return None;
            }
            parts.push(s[begin..i].trim());
            let c = b[i];
            i += 1;
            if b.get(i) == Some(&c) {
                i += 1;
            }
            begin = i;
        } else {
            i += 1;
        }
    }
    if quote.is_some() {
        return None;
    }
    parts.push(s[begin..].trim());
    parts.retain(|p| !p.is_empty());
    Some(parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_license_queries_and_filtered_maven_tests_do_not_create_new_write_batches() {
        assert!(is_observation_or_validation_script("cd /tmp/ws; ZIP=$(ls target/*.zip | head -1); echo '授权依赖'; unzip -l \"$ZIP\" | grep 'lic-1.0.0.jar'"));
        assert!(is_observation_or_validation_script("cd /tmp/ws; mvn -pl adp-iam -am test -Dtest='A,B' 2>&1 | grep -E 'Tests run:|BUILD' | tail -4"));
        assert!(!is_simple_verification("mvn test 2>&1 | tail -4"));
        assert!(!is_simple_verification("mvn test; echo done"));
        assert!(is_simple_verification(
            "mvn -pl adp-iam -am test -Dtest='A,B'"
        ));
    }
    #[test]
    fn writes_hidden_commands_and_failed_pipeline_success_are_not_verification() {
        for cmd in [
            "cargo check && rm source.rs",
            "echo 'cargo test'",
            "mvn test; cp a b",
            "ZIP=$(rm -rf source); echo ok",
            "cat a > b",
            "for f in *.rs; do rm $f; done",
            "echo $(echo $(rm x))",
            "git checkout -- src/a.rs",
            "echo hi & rm x",
        ] {
            assert!(!is_simple_verification(cmd), "{cmd}");
            if cmd != "echo 'cargo test'" {
                assert!(!is_observation_or_validation_script(cmd), "{cmd}");
            }
        }
    }
}
