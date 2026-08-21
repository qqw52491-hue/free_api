use crate::agent::context::types::{SandwichContext, SideEffectRecord};

/// 不可逆操作永久台账（安全审计区，无论历史如何压缩，台账永不丢弃）
pub struct AuditLedger;

impl AuditLedger {
    /// 格式化不可逆操作台账（如超过 8 条则进行同类动作聚合折叠，保持体积紧凑）
    pub fn render_ledger(records: &[SideEffectRecord]) -> String {
        if records.is_empty() {
            return String::new();
        }

        let mut lines = Vec::new();
        let total = records.len();

        if total <= 8 {
            for r in records {
                lines.push(format!(
                    "- [Step {}] {} · {}",
                    r.step_id, r.tool, r.description
                ));
            }
        } else {
            // 早期记录聚合
            let omitted = total - 6;
            lines.push(format!(
                "- [... 早期已安全执行 {} 项不可逆动作 (完整记录在 SQLite) ...]",
                omitted
            ));
            for r in &records[omitted..] {
                lines.push(format!(
                    "- [Step {}] {} · {}",
                    r.step_id, r.tool, r.description
                ));
            }
        }

        format!(
            "【已生效的不可逆外部操作 (Side-Effects 台账)】\n(⚠️系统提示：以下动作已被物理执行，请勿重复调用！)\n{}\n\n",
            lines.join("\n")
        )
    }

    /// 判断某个工具及参数是否属于可能产生外部物理副作用（写文件、数据库修改、外部系统写调用等）的操作
    pub fn is_mutating_side_effect(tool: &str, params: &serde_json::Value) -> bool {
        let lower_tool = tool.to_lowercase();
        let action_name = lower_tool.split('/').last().unwrap_or(&lower_tool);

        // 1. 显式只读工具白名单（绝不记录进 Side-Effects 台账）
        let is_readonly_tool = matches!(
            action_name,
            "read_file"
                | "read_text_file"
                | "read_media_file"
                | "read_multiple_files"
                | "list_directory"
                | "list_directory_with_sizes"
                | "directory_tree"
                | "get_file_info"
                | "list_allowed_directories"
                | "vscode_getdiagnostics"
                | "vscode_getsymbollspinfo"
                | "vscode_getreferences"
                | "vscode_listworkspaces"
                | "vscode_health"
                | "ripgrep_search"
                | "ripgrep_list_files"
                | "ripgrep_search_files"
                | "search_graph"
                | "query_graph"
                | "trace_path"
                | "get_code_snippet"
                | "get_graph_schema"
                | "get_architecture"
                | "search_code"
                | "list_projects"
                | "index_status"
                | "check_index_coverage"
                | "detect_changes"
                | "list_workspaces"
                | "git_diff"
                | "health_check"
                | "web_search"
                | "web_fetch"
                | "get_step_output"
                | "search_step_output"
                | "get_output"
                | "list_resources"
                | "get_resource"
                | "list_records"
                | "get_record"
                | "model_manager"
                | "ask_human"
                | "finish"
        );
        if is_readonly_tool {
            return false;
        }

        // 2. shell 命令：细分检测是否为只读命令
        if action_name == "shell_exec" || action_name == "shell_exec_async" {
            return !Self::is_readonly_shell_command(params);
        }

        // 3. 常见写操作与变更类动作
        action_name.contains("write")
            || action_name.contains("edit")
            || action_name.contains("move")
            || action_name.contains("delete")
            || action_name.contains("create")
            || action_name.contains("upsert")
            || action_name.contains("patch")
            || action_name.contains("apply")
            || action_name.contains("remove")
    }

    /// 检测 shell 命令是否为只读查询（如 git log/show/diff, ls, cat, grep 等）
    pub fn is_readonly_shell_command(params: &serde_json::Value) -> bool {
        let cmd = params.get("command").and_then(|c| c.as_str()).unwrap_or("");
        let args: Vec<&str> = params
            .get("args")
            .and_then(|a| a.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        let base_cmd = cmd.split('/').last().unwrap_or(cmd).trim();

        if base_cmd == "git" {
            let mut skip_next = false;
            for arg in &args {
                if skip_next {
                    skip_next = false;
                    continue;
                }
                if *arg == "-C" || *arg == "--git-dir" || *arg == "--work-tree" {
                    skip_next = true;
                    continue;
                }
                if arg.starts_with('-') {
                    continue;
                }
                // 第一个非 flag 参数即为 git 子命令
                return matches!(
                    *arg,
                    "log"
                        | "show"
                        | "status"
                        | "diff"
                        | "branch"
                        | "tag"
                        | "remote"
                        | "config"
                        | "rev-parse"
                        | "describe"
                        | "check-ignore"
                );
            }
            return false;
        }

        matches!(
            base_cmd,
            "ls" | "cat"
                | "grep"
                | "rg"
                | "ripgrep"
                | "find"
                | "head"
                | "tail"
                | "wc"
                | "echo"
                | "pwd"
                | "which"
                | "whereis"
        )
    }

    /// 提取不可逆副作用的干净结构化参数元数据（保留合法完整的 JSON，绝不用 ... 截断破坏元数据）
    pub fn format_side_effect_params(params: &serde_json::Value) -> String {
        if let serde_json::Value::Object(map) = params {
            let mut cleaned = map.clone();
            // 如果包含庞大的正文内容（如 write_file 的整个文件 content），精简 content 字段避免打爆台账，但保留 path 等核心参数
            if let Some(content) = cleaned.get("content") {
                if content.as_str().map(|s| s.len() > 120).unwrap_or(false) {
                    cleaned.insert(
                        "content".to_string(),
                        serde_json::json!("[...已写入内容已持久化...]"),
                    );
                }
            }
            serde_json::to_string(&cleaned).unwrap_or_else(|_| params.to_string())
        } else {
            params.to_string()
        }
    }
}

impl SandwichContext {
    pub fn render_side_effects_ledger(&self) -> String {
        AuditLedger::render_ledger(&self.side_effects)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_ledger_small() {
        let records = vec![SideEffectRecord {
            step_id: 1,
            tool: "code_editor".to_string(),
            description: "created src/main.rs".to_string(),
            timestamp: "2026-08-17T00:00:00Z".to_string(),
        }];
        let rendered = AuditLedger::render_ledger(&records);
        assert!(rendered.contains("created src/main.rs"));
        assert!(rendered.contains("Step 1"));
    }

    #[test]
    fn test_render_ledger_folding() {
        let mut records = Vec::new();
        for i in 0..15 {
            records.push(SideEffectRecord {
                step_id: i,
                tool: "code_editor".to_string(),
                description: format!("modified file_{}.rs", i),
                timestamp: "2026-08-17T00:00:00Z".to_string(),
            });
        }
        let rendered = AuditLedger::render_ledger(&records);
        assert!(rendered.contains("早期已安全执行"));
        assert!(rendered.contains("modified file_14.rs"));
    }

    #[test]
    fn test_is_mutating_side_effect() {
        use serde_json::json;

        // 纯只读命令应返回 false
        assert!(!AuditLedger::is_mutating_side_effect(
            "remote-hub/shell_exec",
            &json!({"command": "git", "args": ["-C", "/path", "show", "42b105a"]})
        ));
        assert!(!AuditLedger::is_mutating_side_effect(
            "remote-hub/shell_exec",
            &json!({"command": "git", "args": ["log", "--oneline", "-10"]})
        ));
        assert!(!AuditLedger::is_mutating_side_effect(
            "remote-hub/ripgrep_search",
            &json!({"pattern": "fn main"})
        ));
        assert!(!AuditLedger::is_mutating_side_effect(
            "remote-hub/read_text_file",
            &json!({"path": "src/main.rs"})
        ));

        // 产生副作用的命令应返回 true
        assert!(AuditLedger::is_mutating_side_effect(
            "remote-hub/write_file",
            &json!({"path": "src/main.rs", "content": "fn main() {}"})
        ));
        assert!(AuditLedger::is_mutating_side_effect(
            "remote-hub/edit_file",
            &json!({"path": "src/main.rs"})
        ));
        assert!(AuditLedger::is_mutating_side_effect(
            "remote-hub/shell_exec",
            &json!({"command": "git", "args": ["commit", "-m", "fix"]})
        ));
        assert!(AuditLedger::is_mutating_side_effect(
            "remote-hub/shell_exec",
            &json!({"command": "rm", "args": ["-rf", "dist"]})
        ));
    }

    #[test]
    fn test_format_side_effect_params_preserves_valid_json() {
        use serde_json::json;

        let big_content = "X".repeat(500);
        let params = json!({
            "path": "/home/user/project/src/main.rs",
            "content": big_content
        });

        let formatted = AuditLedger::format_side_effect_params(&params);
        assert!(formatted.contains("/home/user/project/src/main.rs"));
        assert!(formatted.contains("[...已写入内容已持久化...]"));
        // 必须能够被重新解析为合法 JSON
        let parsed: serde_json::Value = serde_json::from_str(&formatted).unwrap();
        assert_eq!(parsed["path"], "/home/user/project/src/main.rs");
    }
}
