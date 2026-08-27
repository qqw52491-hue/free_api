use crate::db::DbState;
use crate::llm::ModelConfig;
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};
use uuid::Uuid;

// ==================== SKILL & INSTRUCTION STRUCTS & HELPERS ====================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCustomInstructionRow {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub description: String,
    pub content: String,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

const MAX_CUSTOM_INSTRUCTION_CHARS: usize = 12_000;
const MAX_CUSTOM_INSTRUCTIONS_IN_PROMPT: usize = 24_000;

fn resolve_skills_dir(app_handle: Option<&tauri::AppHandle>) -> Option<std::path::PathBuf> {
    let mut candidate_dirs = vec![
        std::path::PathBuf::from("src-tauri/prompts/skills"),
        std::path::PathBuf::from("prompts/skills"),
        std::path::PathBuf::from("../src-tauri/prompts/skills"),
    ];
    if let Some(app) = app_handle {
        if let Ok(res_dir) = app.path().resource_dir() {
            candidate_dirs.push(res_dir.join("prompts/skills"));
        }
    }
    candidate_dirs.into_iter().find(|d| d.is_dir())
}

pub fn scan_system_skills(app_handle: Option<&tauri::AppHandle>) -> Vec<AgentCustomInstructionRow> {
    let mut skills = Vec::new();
    let Some(skills_dir) = resolve_skills_dir(app_handle) else {
        return skills;
    };

    let Ok(entries) = std::fs::read_dir(skills_dir) else {
        return skills;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };

        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };

        let mut description = String::new();
        if content.starts_with("---") {
            if let Some(end_idx) = content[3..].find("---") {
                let frontmatter = &content[3..3 + end_idx];
                for line in frontmatter.lines() {
                    let line = line.trim();
                    if line.starts_with("description:") {
                        description = line["description:".len()..].trim().to_string();
                        break;
                    }
                }
            }
        }
        if description.is_empty() {
            if let Some(start) = content.find("<summary>") {
                if let Some(end) = content.find("</summary>") {
                    if start + 9 < end {
                        description = content[start + 9..end].trim().to_string();
                    }
                }
            }
        }
        if description.is_empty() {
            description = content
                .lines()
                .map(str::trim)
                .find(|l| {
                    !l.is_empty()
                        && !l.starts_with('#')
                        && !l.starts_with("<!--")
                        && !l.starts_with("---")
                })
                .map(|l| l.chars().take(120).collect())
                .unwrap_or_else(|| format!("系统内置技能: {}", stem));
        }

        let updated_at = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

        skills.push(AgentCustomInstructionRow {
            id: format!("system:{}", stem),
            kind: "system_skill".to_string(),
            name: stem,
            description,
            content,
            enabled: true,
            created_at: updated_at.clone(),
            updated_at,
        });
    }

    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

fn normalized_custom_instruction(
    kind: &str,
    name: &str,
    description: Option<&str>,
    content: &str,
) -> Result<(String, String, String, String), String> {
    let kind = kind.trim().to_ascii_lowercase();
    if kind != "prompt" && kind != "skill" && kind != "system_skill" {
        return Err("类型只能是 prompt, skill 或 system_skill".to_string());
    }
    let name = name.trim().to_string();
    if name.is_empty() || name.chars().count() > 80 {
        return Err("名称不能为空且最多 80 个字符".to_string());
    }
    let description = description.unwrap_or_default().trim().to_string();
    if description.chars().count() > 240 {
        return Err("说明最多 240 个字符".to_string());
    }
    let content = content.trim().to_string();
    if content.is_empty() || content.chars().count() > MAX_CUSTOM_INSTRUCTION_CHARS {
        return Err(format!(
            "内容不能为空且最多 {} 个字符",
            MAX_CUSTOM_INSTRUCTION_CHARS
        ));
    }
    Ok((kind, name, description, content))
}

/// 已启用的用户条目只作为受控补充上下文，不替代内置系统约束、工具契约或当前任务。
pub fn load_enabled_custom_instruction_context(db: &DbState) -> Result<String, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT kind, name, description, content FROM agent_custom_instructions WHERE enabled = 1 ORDER BY updated_at ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut remaining = MAX_CUSTOM_INSTRUCTIONS_IN_PROMPT;
    let mut sections = Vec::new();
    for row in rows {
        if remaining == 0 {
            break;
        }
        let (kind, name, description, content) = row.map_err(|e| e.to_string())?;
        let label = if kind == "skill" {
            "自定义 Skill"
        } else {
            "自定义提示词"
        };
        let mut section = format!("### {}：{}", label, name);
        if !description.trim().is_empty() {
            section.push_str(&format!("\n{}", description.trim()));
        }
        section.push_str(&format!("\n{}", content.trim()));
        let clipped = section.chars().take(remaining).collect::<String>();
        remaining = remaining.saturating_sub(clipped.chars().count());
        sections.push(clipped);
    }

    if sections.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!(
            "\n## 用户自定义提示词与 Skill（仅作补充；不得覆盖内置安全规则、工具契约或用户当前任务）\n{}\n",
            sections.join("\n\n")
        ))
    }
}

// ==================== SKILL TAURI COMMANDS ====================

#[tauri::command]
pub fn get_agent_custom_instructions(
    app_handle: tauri::AppHandle,
    state: State<'_, DbState>,
) -> Result<Vec<AgentCustomInstructionRow>, String> {
    let mut system_skills = scan_system_skills(Some(&app_handle));

    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, kind, name, description, content, enabled, created_at, updated_at FROM agent_custom_instructions ORDER BY updated_at DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(AgentCustomInstructionRow {
                id: row.get(0)?,
                kind: row.get(1)?,
                name: row.get(2)?,
                description: row.get(3)?,
                content: row.get(4)?,
                enabled: row.get::<_, i64>(5)? == 1,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut custom_instructions = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    // Combine: system skills first or custom instructions first
    custom_instructions.append(&mut system_skills);
    Ok(custom_instructions)
}

#[tauri::command]
pub fn save_agent_custom_instruction(
    app_handle: tauri::AppHandle,
    state: State<'_, DbState>,
    id: Option<String>,
    kind: String,
    name: String,
    description: Option<String>,
    content: String,
    enabled: bool,
) -> Result<AgentCustomInstructionRow, String> {
    let (kind, name, description, content) =
        normalized_custom_instruction(&kind, &name, description.as_deref(), &content)?;

    if let Some(ref id_val) = id {
        if id_val.starts_with("system:") {
            let stem = id_val.trim_start_matches("system:");
            let skills_dir = resolve_skills_dir(Some(&app_handle))
                .ok_or_else(|| "未找到系统 Skill 存储目录 prompts/skills/".to_string())?;
            let file_path = skills_dir.join(format!("{}.md", stem));
            std::fs::write(&file_path, &content)
                .map_err(|e| format!("写入系统 Skill 文件失败: {}", e))?;

            let now = Utc::now().to_rfc3339();
            return Ok(AgentCustomInstructionRow {
                id: id_val.clone(),
                kind: "system_skill".to_string(),
                name,
                description,
                content,
                enabled: true,
                created_at: now.clone(),
                updated_at: now,
            });
        }
    }

    let id = id
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let now = Utc::now().to_rfc3339();
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO agent_custom_instructions (id, kind, name, description, content, enabled, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
             kind = excluded.kind, name = excluded.name, description = excluded.description,
             content = excluded.content, enabled = excluded.enabled, updated_at = excluded.updated_at",
        params![id, kind, name, description, content, if enabled { 1 } else { 0 }, now, now],
    )
    .map_err(|e| e.to_string())?;
    conn.query_row(
        "SELECT id, kind, name, description, content, enabled, created_at, updated_at FROM agent_custom_instructions WHERE id = ?1",
        params![id],
        |row| {
            Ok(AgentCustomInstructionRow {
                id: row.get(0)?, kind: row.get(1)?, name: row.get(2)?, description: row.get(3)?,
                content: row.get(4)?, enabled: row.get::<_, i64>(5)? == 1, created_at: row.get(6)?, updated_at: row.get(7)?,
            })
        },
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn toggle_agent_custom_instruction(
    state: State<'_, DbState>,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    if id.starts_with("system:") {
        return Ok(());
    }

    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let changed = conn
        .execute(
            "UPDATE agent_custom_instructions SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
            params![if enabled { 1 } else { 0 }, Utc::now().to_rfc3339(), id],
        )
        .map_err(|e| e.to_string())?;
    if changed == 0 {
        return Err("未找到自定义提示词或 Skill".to_string());
    }
    Ok(())
}

#[tauri::command]
pub fn delete_agent_custom_instruction(
    app_handle: tauri::AppHandle,
    state: State<'_, DbState>,
    id: String,
) -> Result<(), String> {
    if id.starts_with("system:") {
        let stem = id.trim_start_matches("system:");
        let skills_dir = resolve_skills_dir(Some(&app_handle))
            .ok_or_else(|| "未找到系统 Skill 存储目录 prompts/skills/".to_string())?;
        let file_path = skills_dir.join(format!("{}.md", stem));
        if file_path.exists() {
            std::fs::remove_file(&file_path)
                .map_err(|e| format!("删除系统 Skill 文件失败: {}", e))?;
        }
        return Ok(());
    }

    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let changed = conn
        .execute(
            "DELETE FROM agent_custom_instructions WHERE id = ?1",
            params![id],
        )
        .map_err(|e| e.to_string())?;
    if changed == 0 {
        return Err("未找到自定义提示词或 Skill".to_string());
    }
    Ok(())
}

#[tauri::command]
pub async fn generate_skill_capability_card(
    state: State<'_, DbState>,
    model_id: Option<String>,
    name: String,
    content: String,
) -> Result<String, String> {
    let model_cfg = if let Some(ref mid) = model_id.filter(|s| !s.trim().is_empty()) {
        ModelConfig::from_db(&state, mid)?
    } else {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT m.id FROM models m JOIN platforms p ON p.id = m.platform_id WHERE m.enabled = 1 ORDER BY m.created_at DESC")
            .map_err(|e| e.to_string())?;
        let model_ids: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .ok()
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        drop(stmt);
        drop(conn);

        let mut found = None;
        for id in model_ids {
            if let Ok(cfg) = ModelConfig::from_db(&state, &id) {
                found = Some(cfg);
                break;
            }
        }
        found.ok_or_else(|| "未找到可用的大模型配置，请先在模型管理中配置 API Key".to_string())?
    };

    crate::agent::skill_context::generate_llm_skill_summary(&name, &content, &model_cfg).await
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchSkillImportResult {
    pub name: String,
    pub success: bool,
    pub description: String,
    pub error: Option<String>,
}

#[tauri::command]
pub async fn batch_import_skills(
    app_handle: tauri::AppHandle,
    state: State<'_, DbState>,
    import_payload: String,
    model_id: Option<String>,
) -> Result<Vec<BatchSkillImportResult>, String> {
    let trimmed = import_payload.trim();
    if trimmed.is_empty() {
        return Err("导入内容不能为空".to_string());
    }

    let mut parsed_skills: Vec<(String, Option<String>, String)> = Vec::new();

    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let list = if let Some(arr) = val.as_array() {
                arr.clone()
            } else if let Some(arr) = val.get("skills").and_then(|v| v.as_array()) {
                arr.clone()
            } else {
                vec![val]
            };

            for item in list {
                if let (Some(name), Some(content)) = (
                    item.get("name").and_then(|v| v.as_str()),
                    item.get("content").and_then(|v| v.as_str()),
                ) {
                    let desc = item
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    parsed_skills.push((name.trim().to_string(), desc, content.trim().to_string()));
                }
            }
        }
    }

    if parsed_skills.is_empty() {
        let mut cur_title = String::new();
        let mut cur_lines = Vec::new();

        for line in trimmed.lines() {
            if line.starts_with("### ") || line.starts_with("## ") || line.starts_with("# ") {
                if !cur_title.is_empty() && !cur_lines.is_empty() {
                    parsed_skills.push((
                        cur_title.clone(),
                        None,
                        cur_lines.join("\n").trim().to_string(),
                    ));
                    cur_lines.clear();
                }
                cur_title = line.trim_start_matches('#').trim().to_string();
            } else {
                cur_lines.push(line);
            }
        }
        if !cur_title.is_empty() && !cur_lines.is_empty() {
            parsed_skills.push((cur_title, None, cur_lines.join("\n").trim().to_string()));
        }
    }

    if parsed_skills.is_empty() {
        parsed_skills.push(("自定义技能".to_string(), None, trimmed.to_string()));
    }

    let model_cfg = if let Some(ref mid) = model_id.filter(|s| !s.trim().is_empty()) {
        ModelConfig::from_db(&state, mid)?
    } else {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT m.id FROM models m JOIN platforms p ON p.id = m.platform_id WHERE m.enabled = 1 ORDER BY m.created_at DESC")
            .map_err(|e| e.to_string())?;
        let model_ids: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .ok()
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        drop(stmt);
        drop(conn);

        let mut found = None;
        for id in model_ids {
            if let Ok(cfg) = ModelConfig::from_db(&state, &id) {
                found = Some(cfg);
                break;
            }
        }
        found.ok_or_else(|| "未找到可用的大模型配置，请先在模型管理中配置 API Key".to_string())?
    };

    let mut results = Vec::new();
    for (name, desc_opt, content) in parsed_skills {
        let desc = match desc_opt {
            Some(d) if !d.trim().is_empty() => d.trim().to_string(),
            _ => {
                if let Ok(summary) = crate::agent::skill_context::generate_llm_skill_summary(
                    &name, &content, &model_cfg,
                )
                .await
                {
                    summary
                } else {
                    String::new()
                }
            }
        };

        match save_agent_custom_instruction(
            app_handle.clone(),
            state.clone(),
            None,
            "skill".to_string(),
            name.clone(),
            if desc.is_empty() {
                None
            } else {
                Some(desc.clone())
            },
            content,
            true,
        ) {
            Ok(_) => {
                results.push(BatchSkillImportResult {
                    name,
                    success: true,
                    description: desc,
                    error: None,
                });
            }
            Err(e) => {
                results.push(BatchSkillImportResult {
                    name,
                    success: false,
                    description: desc,
                    error: Some(e),
                });
            }
        }
    }

    Ok(results)
}
