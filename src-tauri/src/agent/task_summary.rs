use crate::agent::task_store::TaskResourceRef;
use chrono::Utc;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

fn clean_text(value: &str) -> String {
    value.trim().replace('\r', "")
}

fn listed_strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .map(clean_text)
                .filter(|item| !item.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

pub fn render_task_summary_markdown(
    session_id: &str,
    user_goal: &str,
    completion_summary: &str,
    progress_summary: &str,
    finish_payload: &Value,
    resources: &[TaskResourceRef],
) -> String {
    let details = finish_payload
        .get("details")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut artifacts = listed_strings(finish_payload.get("artifacts"));
    if artifacts.is_empty() {
        artifacts.extend(listed_strings(finish_payload.get("files")));
    }

    let mut markdown = format!(
        "# Agent 任务总结\n\n| 字段 | 内容 |\n| --- | --- |\n| 会话 | `{}` |\n| 状态 | 已完成 |\n| 生成时间 | {} |\n\n## 用户目标\n\n{}\n\n## 完成摘要\n\n{}\n",
        session_id,
        Utc::now().to_rfc3339(),
        clean_text(user_goal),
        clean_text(completion_summary),
    );

    if !clean_text(details).is_empty() && clean_text(details) != clean_text(completion_summary) {
        markdown.push_str(&format!("\n## 详细说明\n\n{}\n", clean_text(details)));
    }
    if !clean_text(progress_summary).is_empty() {
        markdown.push_str(&format!(
            "\n## 执行进度摘要\n\n{}\n",
            clean_text(progress_summary)
        ));
    }
    if !artifacts.is_empty() {
        markdown.push_str("\n## 交付物\n\n");
        for artifact in artifacts {
            markdown.push_str(&format!("- `{}`\n", artifact));
        }
    }
    if !resources.is_empty() {
        markdown.push_str("\n## 会话数据资源\n\n| 名称 | 类型 | 状态 | 记录数 | 摘要 |\n| --- | --- | --- | ---: | --- |\n");
        for resource in resources {
            let label = resource.label.replace('|', "\\|");
            let summary = resource.summary.replace('|', "\\|").replace('\n', " ");
            markdown.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                label, resource.kind, resource.status, resource.row_count, summary
            ));
        }
    }
    markdown.push_str("\n---\n\n> 本文件由 free_api Agent 在任务结束时自动生成，可作为后续继续对话、复盘或人工交接的摘要。\n");
    markdown
}

pub fn write_task_summary(
    app: &AppHandle,
    session_id: &str,
    user_goal: &str,
    completion_summary: &str,
    progress_summary: &str,
    finish_payload: &Value,
    resources: &[TaskResourceRef],
) -> Result<PathBuf, String> {
    let root = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法定位应用数据目录: {}", error))?
        .join("task_summaries");
    fs::create_dir_all(&root).map_err(|error| format!("无法创建任务总结目录: {}", error))?;
    let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
    let safe_session = session_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let path = root.join(format!("{}-{}.md", safe_session, timestamp));
    fs::write(
        &path,
        render_task_summary_markdown(
            session_id,
            user_goal,
            completion_summary,
            progress_summary,
            finish_payload,
            resources,
        ),
    )
    .map_err(|error| format!("无法写入任务总结: {}", error))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn markdown_summary_contains_goal_finish_artifacts_and_resources() {
        let resources = vec![TaskResourceRef {
            resource_id: "sheet-1".to_string(),
            kind: "table".to_string(),
            label: "模拟成绩单".to_string(),
            summary: "5 名学生".to_string(),
            status: "ready".to_string(),
            row_count: 5,
            cursor: None,
            updated_at: "2026-08-15T00:00:00Z".to_string(),
        }];
        let markdown = render_task_summary_markdown(
            "session-1",
            "创建成绩单",
            "工作簿已创建",
            "写入并核对数据",
            &json!({"details":"已写入五行", "artifacts":["/tmp/score.xlsx"]}),
            &resources,
        );
        assert!(markdown.contains("创建成绩单"));
        assert!(markdown.contains("/tmp/score.xlsx"));
        assert!(markdown.contains("模拟成绩单"));
    }
}
