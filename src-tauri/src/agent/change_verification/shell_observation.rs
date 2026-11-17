//! 仅用于副作用记账，不改变工具权限，也不把流水线退出 0 当作测试通过。
use crate::agent::context::AuditLedger;
use serde_json::json;

/// 只识别可逐段核对的读取/验证脚本。复杂控制流、写入、未知子命令仍保守记账。
pub fn is_observation_or_validation_script(command: &str) -> bool {
    let Some(sanitized) = observation_substitutions(command) else {
        return false;
    };
    let Some(parts) = segments(&sanitized, false) else {
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

/// 识别单一、字面量 SSH 查询。只用于审查计数和台账分类。
pub(crate) fn is_readonly_ssh_probe(command: &str) -> bool {
    let Some((invocation, quoted)) = command.trim().split_once(" '") else { return false };
    let Some(script) = quoted.strip_suffix('\'') else { return false };
    if script.contains(['\'', '\\', '`']) || script.contains("$(") {
        return false;
    }

    let mut words = invocation.split_whitespace();
    if words.next() != Some("ssh") { return false; }
    let host = loop {
        match words.next() {
            Some("-o") => {
                let Some((key, value)) = words.next().and_then(|word| word.split_once('=')) else {
                    return false;
                };
                let allowed = match key {
                    "ConnectTimeout" => !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()),
                    "BatchMode" => matches!(value, "yes" | "no"),
                    "StrictHostKeyChecking" => matches!(value, "yes" | "no" | "accept-new"),
                    _ => false,
                };
                if !allowed { return false; }
            }
            Some(host) => break host,
            None => return false,
        }
    };
    if words.next().is_some() || host.starts_with('-') || !host.bytes().all(|byte|
        byte.is_ascii_alphanumeric() || matches!(byte, b'@' | b'.' | b'-' | b'_')
    ) {
        return false;
    }

    let Some(parts) = segments(script, true) else { return false };
    !parts.is_empty() && parts.iter().all(|part| {
        let part = part.strip_suffix(" 2>/dev/null").unwrap_or(part);
        let mut words = part.split_whitespace();
        let head = words.next();
        if part.contains('$') && !(head == Some("echo") && words.clone().all(|word| {
            !word.contains('$') || word == "verify_rc=$?"
        })) {
            return false;
        }
        match head {
            Some("systemctl") => matches!(words.next(), Some("show" | "cat" | "is-active" | "is-enabled" | "status"))
                && AuditLedger::is_readonly_shell_command(&json!({"command":part})),
            Some("systemd-analyze") => words.next() == Some("verify")
                && words.next().is_some_and(|operand| !operand.starts_with('-'))
                && words.all(|word| word == "2>&1"),
            Some("echo" | "ls" | "cat" | "grep" | "head" | "tail" | "journalctl") =>
                AuditLedger::is_readonly_shell_command(&json!({"command":part})),
            _ => false,
        }
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

fn segments(s: &str, allow_null_stderr: bool) -> Option<Vec<&str>> {
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
        // 仅 stderr 合并或丢弃，不接受写文件重定向。
        if allow_null_stderr && b[i..].starts_with(b"2>/dev/null")
            && (i == 0 || b[i - 1].is_ascii_whitespace())
            && b.get(i + 11).is_none_or(|c| c.is_ascii_whitespace() || matches!(c, b';' | b'|' | b'&'))
        {
            i += 11;
            continue;
        }
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
    fn ssh_status_and_validation_are_observations_but_mixed_writes_are_not() {
        let ssh = |body: &str| format!("ssh -o ConnectTimeout=8 -o BatchMode=yes -o StrictHostKeyChecking=no admin@example.test '{body}'");
        for body in [
            "systemctl show example.service -p ConditionPathExists --no-pager",
            "echo ===CHECK===; systemctl cat example.service --no-pager | tail -n 10",
            "ls -l /run/example.pid 2>&1; systemctl is-active example.service",
            "systemctl show example.service -p Conditions 2>/dev/null; systemctl show example.service | grep -i condition",
            "systemd-analyze verify example.service 2>&1; echo verify_rc=$?; systemctl cat example.service",
        ] {
            assert!(is_readonly_ssh_probe(&ssh(body)), "{body}");
        }
        for body in [
            "systemctl show example.service; systemctl stop example.service",
            "systemctl show example.service 2>/dev/null.log",
            "cat /etc/example > /tmp/copy",
            "journalctl --rotate",
            "systemd-analyze verify $(touch a)",
        ] {
            assert!(!is_readonly_ssh_probe(&ssh(body)), "{body}");
        }
        assert!(!is_readonly_ssh_probe("ssh -o ProxyCommand=anything admin@example.test 'systemctl show example.service'"));
        assert!(!is_readonly_ssh_probe("ssh admin@example.test 'systemctl show example.service' && touch /tmp/example"));
    }

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
