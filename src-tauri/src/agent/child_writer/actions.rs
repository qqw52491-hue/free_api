//! 写入工兵动作执行：write_file/edit_file/read/command 与围栏校验。

use super::*;
use crate::db::DbState;
use crate::llm::{self, ModelConfig};
use uuid::Uuid;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;


/// 执行 writer 动作（异步、可单测）。返回 (观测输出, 是否测试命令)
pub async fn execute_writer_action(
    copy_root: &Path,
    action: &str,
    params: &Value,
    changes: &mut BTreeMap<String, WriteBaseline>,
    writes_used: &mut usize,
    test_command: &str,
    test_evidence: &mut Option<(String, bool, String)>,
    hard_scope: &str,
) -> Result<String, String> {
    match action {
        "list_dir" | "read_file" => {
            // 只读动作复用 scout 的实现语义，但根目录换成副本
            let raw = params
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("{} 缺少 path。", action))?;
            let canonical = ensure_contained(copy_root, raw)?;
            if action == "list_dir" {
                let mut entries = std::fs::read_dir(&canonical)
                    .map_err(|error| format!("列目录失败: {}", error))?
                    .filter_map(|entry| entry.ok())
                    .take(200)
                    .collect::<Vec<_>>();
                entries.sort_by_key(|entry| entry.file_name());
                let mut out = String::new();
                for entry in entries {
                    let kind = if entry.path().is_dir() { "dir " } else { "file" };
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    out.push_str(&format!("{} {}\t{}\n", kind, size, entry.file_name().to_string_lossy()));
                }
                Ok(truncate_chars(&out, MAX_ACTION_OUTPUT_CHARS))
            } else {
                let metadata = std::fs::metadata(&canonical)
                    .map_err(|error| format!("无法读取文件元数据: {}", error))?;
                if metadata.len() > 2_000_000 {
                    return Err("文件过大（>2MB），请用 grep/head 定位片段。".to_string());
                }
                let bytes = std::fs::read(&canonical).map_err(|error| format!("读取失败: {}", error))?;
                Ok(truncate_chars(&String::from_utf8_lossy(&bytes), MAX_ACTION_OUTPUT_CHARS))
            }
        }
        "write_file" => {
            if *writes_used >= MAX_WRITES {
                return Err(format!("写入次数已达上限 {}；请合并修改。", MAX_WRITES));
            }
            let raw = params
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "write_file 缺少 path。".to_string())?;
            let content = params
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "write_file 缺少 content（必须写完整文件内容）。".to_string())?;
            if content.len() > MAX_WRITE_FILE_BYTES {
                return Err(format!("write_file 内容过大（{} 字节 > {}）。", content.len(), MAX_WRITE_FILE_BYTES));
            }
            let canonical = ensure_contained(copy_root, raw)?;
            check_hard_scope(copy_root, &canonical, hard_scope)?;
            let relative = canonical
                .strip_prefix(copy_root.canonicalize().unwrap_or_else(|_| copy_root.to_path_buf()))
                .unwrap_or(&canonical)
                .to_string_lossy()
                .to_string();
            // 首次修改前留底，供 diff 生成（同时记录文件当时是否存在）
            if !changes.contains_key(&relative) {
                let before = std::fs::read(&canonical).ok();
                changes.insert(relative.clone(), WriteBaseline {
                    content: before
                        .as_deref()
                        .map(|bytes| String::from_utf8_lossy(bytes).to_string())
                        .unwrap_or_default(),
                    existed_before: before.is_some(),
                });
            }
            if let Some(parent) = canonical.parent() {
                std::fs::create_dir_all(parent).map_err(|error| format!("创建目录失败: {}", error))?;
            }
            std::fs::write(&canonical, content).map_err(|error| format!("写入失败: {}", error))?;
            *writes_used += 1;
            Ok(format!("✅ 已写入 {}（{} 字节）", relative, content.len()))
        }
        "edit_file" => {
            if *writes_used >= MAX_WRITES {
                return Err(format!("写入次数已达上限 {}；请合并修改。", MAX_WRITES));
            }
            let raw = params
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "edit_file 缺少 path。".to_string())?;
            let target_content = params
                .get("target_content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "edit_file 缺少 target_content（待替换的目标原文本）。".to_string())?;
            let replacement_content = params
                .get("replacement_content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "edit_file 缺少 replacement_content（替换后的新内容）。".to_string())?;

            if target_content.is_empty() {
                return Err("edit_file target_content 不能为空。".to_string());
            }

            let canonical = ensure_contained(copy_root, raw)?;
            check_hard_scope(copy_root, &canonical, hard_scope)?;
            let relative = canonical
                .strip_prefix(copy_root.canonicalize().unwrap_or_else(|_| copy_root.to_path_buf()))
                .unwrap_or(&canonical)
                .to_string_lossy()
                .to_string();

            if !canonical.exists() {
                return Err(format!("待编辑的文件不存在: {}", relative));
            }

            // 首次修改前留底，供 diff 生成（同时记录文件当时是否存在）
            if !changes.contains_key(&relative) {
                let before = std::fs::read(&canonical).ok();
                changes.insert(relative.clone(), WriteBaseline {
                    content: before
                        .as_deref()
                        .map(|bytes| String::from_utf8_lossy(bytes).to_string())
                        .unwrap_or_default(),
                    existed_before: before.is_some(),
                });
            }

            let current_content = std::fs::read_to_string(&canonical)
                .map_err(|error| format!("读取待编辑文件失败: {}", error))?;

            let matches: Vec<(usize, &str)> = current_content.match_indices(target_content).collect();
            if matches.is_empty() {
                return Err(format!(
                    "❌ [编辑失败：目标文本未找到] 在文件 \"{}\" 中未找到指定的 target_content。请先使用 read_file 读取最新内容核对精确文本。",
                    relative
                ));
            }
            if matches.len() > 1 {
                let line_numbers: Vec<usize> = matches
                    .iter()
                    .map(|(idx, _)| current_content[..*idx].lines().count() + 1)
                    .collect();
                return Err(format!(
                    "❌ [编辑失败：目标文本不唯一] 在文件 \"{}\" 中找到了 {} 处匹配（位于行号: {:?}）。请在 target_content 中多包含前后上下文确保唯一。",
                    relative, matches.len(), line_numbers
                ));
            }

            let match_offset = matches[0].0;
            let start_line = current_content[..match_offset].lines().count() + 1;
            let replaced_lines_count = target_content.lines().count().max(1);
            let end_line = start_line + replaced_lines_count - 1;

            let new_content = current_content.replacen(target_content, replacement_content, 1);
            std::fs::write(&canonical, new_content)
                .map_err(|error| format!("写入编辑结果失败: {}", error))?;
            *writes_used += 1;
            Ok(format!("✅ 已局部更新 {}（替换第 {} 至 {} 行的内容）", relative, start_line, end_line))
        }
        "run_command" => {
            let raw = params
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "run_command 缺少 command。".to_string())?;
            reject_shell_metachars(raw)?;
            let argv: Vec<&str> = raw.split_whitespace().collect();
            command_allowed(&argv)?;
            let mut cmd = tokio::process::Command::new(argv[0]);
            cmd.args(&argv[1..]).current_dir(copy_root);
            let timeout_dur = Duration::from_secs(COMMAND_TIMEOUT_SECS);
            let output = match tokio::time::timeout(timeout_dur, cmd.output()).await {
                Ok(res) => res.map_err(|error| format!("命令启动失败: {}", error))?,
                Err(_) => {
                    return Ok(format!("⚠️ 命令执行超时（超过 {} 秒），已被强制终止。", COMMAND_TIMEOUT_SECS));
                }
            };
            let mut stdout = output.stdout;
            stdout.truncate(MAX_COMMAND_OUTPUT_BYTES);
            let status = if output.status.success() { "✅ exit 0" } else { "⚠️ 非零退出" };
            let body = truncate_chars(&String::from_utf8_lossy(&stdout), MAX_ACTION_OUTPUT_CHARS);
            if is_test_command(&argv) {
                *test_evidence = Some((test_command.to_string(), output.status.success(), body.clone()));
            }
            Ok(format!("{}\n[stdout {}]", status, body))
        }
        other => Err(format!(
            "未知动作 {}；可用：list_dir、read_file、write_file、edit_file、run_command、finish。",
            other
        )),
    }
}
