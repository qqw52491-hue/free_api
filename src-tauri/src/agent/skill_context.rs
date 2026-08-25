use std::collections::{hash_map::DefaultHasher, HashMap};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const SKILL_FILES: [&str; 2] = ["AGENTS.md", "HUB.md"];
const MAX_FILE_CHARS: usize = 20_000;
const MAX_TOTAL_CHARS: usize = 28_000;

#[derive(Debug, Clone)]
struct CachedSkillFile {
    fingerprint: u64,
    content: String,
}

static SKILL_CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedSkillFile>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<PathBuf, CachedSkillFile>> {
    SKILL_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn fingerprint(content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

fn trim_for_prompt(content: &str, limit: usize) -> String {
    let length = content.chars().count();
    if length <= limit {
        return content.trim().to_string();
    }

    let head_length = limit.saturating_sub(4_000);
    let head = content.chars().take(head_length).collect::<String>();
    let tail = content
        .chars()
        .rev()
        .take(4_000)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!(
        "{}\n\n[中段因上下文预算被省略；需要原文时请读取该文件]\n\n{}",
        head.trim(),
        tail.trim()
    )
}

fn discover_skill_files(workspace: &Path) -> Vec<PathBuf> {
    let mut layers = Vec::new();
    let mut current = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());

    loop {
        let files = SKILL_FILES
            .iter()
            .map(|name| current.join(name))
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
        if !files.is_empty() {
            layers.push(files);
        }
        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
    }

    layers.reverse();
    layers.into_iter().flatten().collect()
}

fn cached_content(path: &Path) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    let next_fingerprint = fingerprint(&raw);
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    if let Ok(mut entries) = cache().lock() {
        if let Some(cached) = entries.get(&canonical) {
            if cached.fingerprint == next_fingerprint {
                return Some(cached.content.clone());
            }
        }
        let content = trim_for_prompt(&raw, MAX_FILE_CHARS);
        entries.insert(
            canonical,
            CachedSkillFile {
                fingerprint: next_fingerprint,
                content: content.clone(),
            },
        );
        return Some(content);
    }

    Some(trim_for_prompt(&raw, MAX_FILE_CHARS))
}

/// 构建工作区 Skill 指令。每次 Agent 新运行与恢复都会调用；未修改文件直接复用
/// 进程内缓存，文件内容变化则通过哈希自动失效并重新读取。
pub fn workspace_skill_context(workspace: &Path) -> String {
    let mut sections = Vec::new();
    let mut remaining = MAX_TOTAL_CHARS;

    for path in discover_skill_files(workspace) {
        if remaining == 0 {
            break;
        }
        let Some(content) = cached_content(&path) else {
            continue;
        };
        let included = trim_for_prompt(&content, remaining);
        if included.is_empty() {
            continue;
        }
        remaining = remaining.saturating_sub(included.chars().count());
        sections.push(format!("### {}\n{}", path.display(), included));
    }

    if sections.is_empty() {
        String::new()
    } else {
        format!(
            "\n## 工作区 Skill 指令（AGENTS.md / HUB.md；文件变更会自动刷新缓存）\n{}\n",
            sections.join("\n\n")
        )
    }
}

use crate::llm::ModelConfig;

#[derive(Debug, Clone)]
struct CachedSkillSummary {
    fingerprint: u64,
    summary: String,
}

static SKILL_SUMMARY_CACHE: OnceLock<Mutex<HashMap<String, CachedSkillSummary>>> = OnceLock::new();

fn skill_summary_cache() -> &'static Mutex<HashMap<String, CachedSkillSummary>> {
    SKILL_SUMMARY_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 异步调用 LLM 从 Skill 文档中提炼高密度触发与能力卡片
pub async fn generate_llm_skill_summary(
    skill_name: &str,
    content: &str,
    config: &ModelConfig,
) -> Result<String, String> {
    let client = config.build_client()?;
    let url = config.endpoint_url();
    let headers = config.auth_headers();

    let capped: String = content.chars().take(4000).collect();
    let prompt = format!(
        "你是一个 AI Agent 技能架构专家。请根据以下 Skill 规范文档 [{}]，生成一份供 Agent 在任务规划时快速识别触发条件的【精炼技能卡片摘要】（严格控制在 1~2 句话、60 字以内）。\n\
        必须说明：1) 核心用途；2) 何时/什么场景下触发预载（next_tool_hint）。\n\
        禁止客套话与 Markdown 代码块，直接输出纯文本内容。\n\n\
        【Skill 文档】\n{}",
        skill_name, capped
    );

    let messages = vec![
        serde_json::json!({
            "role": "system",
            "content": "你是资深的 AI Agent 技能索引专家，擅长将复杂流程提炼为精准的触发卡片。"
        }),
        serde_json::json!({
            "role": "user",
            "content": prompt
        }),
    ];

    let mut body = serde_json::json!({
        "model": config.model_name,
        "messages": messages,
        "max_tokens": 1000,
        "temperature": 0.3,
        "stream": false
    });

    config.apply_provider_capabilities(&mut body, "off");

    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        client.post(&url).headers(headers).json(&body).send(),
    )
    .await
    .map_err(|_| "LLM 生成 Skill 卡片超时 (120s)".to_string())?
    .map_err(|e| format!("LLM 生成 Skill 卡片网络请求失败: {}", e))?;

    let json_resp: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;

    let message = json_resp
        .get("choices")
        .and_then(|arr| arr.get(0))
        .and_then(|c| c.get("message"));

    let summary_opt = message
        .and_then(|m| {
            if let Some(s) = m.get("content").and_then(|v| v.as_str()) {
                Some(s.to_string())
            } else if let Some(arr) = m.get("content").and_then(|v| v.as_array()) {
                let parts: Vec<String> = arr
                    .iter()
                    .filter_map(|item| item.get("text").and_then(|t| t.as_str()).map(|s| s.to_string()))
                    .collect();
                if !parts.is_empty() {
                    Some(parts.join("\n"))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let summary = if let Some(s) = summary_opt {
        s
    } else if let Some(reasoning) = message.and_then(|m| m.get("reasoning_content")).and_then(|v| v.as_str()) {
        let r = reasoning.trim();
        if !r.is_empty() {
            r.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or(r).to_string()
        } else {
            return Err("LLM 生成 Skill 卡片返回内容为空".to_string());
        }
    } else {
        return Err(format!(
            "LLM 响应中缺少 content 字段: {}",
            serde_json::to_string(&json_resp).unwrap_or_default()
        ));
    };

    Ok(summary)
}

fn is_usable_skill_summary(summary: &str) -> bool {
    let normalized = summary.trim();
    !normalized.is_empty() && normalized != "---"
}

fn front_matter_description(content: &str) -> Option<String> {
    let mut lines = content.lines().map(str::trim);
    if lines.next()? != "---" {
        return None;
    }

    for line in lines {
        if line == "---" {
            break;
        }
        if let Some(description) = line.strip_prefix("description:") {
            let description = description.trim().trim_matches(['\"', '\'']).trim();
            if !description.is_empty() {
                return Some(description.chars().take(160).collect());
            }
        }
    }
    None
}

fn fallback_skill_summary(content: &str, name: &str) -> String {
    let mut lines = content.lines().map(str::trim);
    if lines.next() == Some("---") {
        for line in lines.by_ref() {
            if line == "---" {
                break;
            }
        }
    }

    lines
        .find(|line| {
            !line.is_empty()
                && !line.starts_with('#')
                && !line.starts_with("<!--")
                && *line != "---"
        })
        .map(|line| line.chars().take(80).collect())
        .unwrap_or_else(|| name.to_string())
}

pub fn cached_skill_summary(name: &str, content: &str) -> Option<String> {
    let fp = fingerprint(content);
    let cache = skill_summary_cache().lock().ok()?;
    let entry = cache.get(name)?;
    if entry.fingerprint == fp && is_usable_skill_summary(&entry.summary) {
        Some(entry.summary.clone())
    } else {
        None
    }
}

pub fn set_cached_skill_summary(name: &str, content: &str, summary: String) {
    let fp = fingerprint(content);
    if let Ok(mut cache) = skill_summary_cache().lock() {
        cache.insert(
            name.to_string(),
            CachedSkillSummary {
                fingerprint: fp,
                summary,
            },
        );
    }
}

/// 扫描 skills 目录，生成轻量目录摘要（仅名称 + 智能能力卡描述）。
/// excluded 列表中的 Skill 已固定编译进系统提示词，不再重复罗列。
/// 完整手册内容通过 next_tool_hint: "skill_name" 按需加载到 active_tool_detail。
pub fn skill_catalog_string(
    skills_dir: &Path,
    excluded: &[&str],
    model_config: Option<&ModelConfig>,
) -> String {
    let Ok(entries) = std::fs::read_dir(skills_dir) else {
        return String::new();
    };
    let mut files: Vec<_> = entries.flatten().collect();
    files.sort_by_key(|e| e.file_name());

    let mut lines = Vec::new();
    for entry in files {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let name = match path.file_stem().and_then(|s| s.to_str()) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => continue,
        };
        if excluded.contains(&name.as_str()) {
            continue;
        }
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let summary = if let Some(cached) = cached_skill_summary(&name, &content) {
            cached
        } else if let Some(tag_summary) = content.find("<summary>").and_then(|start| {
            content
                .find("</summary>")
                .map(|end| content[start + 9..end].trim().to_string())
        }) {
            set_cached_skill_summary(&name, &content, tag_summary.clone());
            tag_summary
        } else if let Some(front_matter_summary) = front_matter_description(&content) {
            set_cached_skill_summary(&name, &content, front_matter_summary.clone());
            front_matter_summary
        } else {
            let rule_summary = fallback_skill_summary(&content, &name);

            set_cached_skill_summary(&name, &content, rule_summary.clone());

            // 若有模型配置，后台异步升级为 LLM 智能能力卡
            if let Some(cfg) = model_config {
                let name_clone = name.clone();
                let content_clone = content.clone();
                let cfg_clone = cfg.clone();
                tokio::spawn(async move {
                    if let Ok(llm_sum) =
                        generate_llm_skill_summary(&name_clone, &content_clone, &cfg_clone).await
                    {
                        set_cached_skill_summary(&name_clone, &content_clone, llm_sum);
                    }
                });
            }

            rule_summary
        };
        lines.push(format!("- {}: {}", name, summary));
    }

    if lines.is_empty() {
        String::new()
    } else {
        format!(
            "\n## 📚 可按需加载的 Skill 手册（设置 next_tool_hint: \"skill_name\" 加载完整版到当前步骤）\n{}\n",
            lines.join("\n")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_workspace() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("free_api_skill_context_{nonce}"));
        fs::create_dir_all(path.join("nested")).unwrap();
        path
    }

    #[test]
    fn loads_hierarchical_skills_and_refreshes_when_content_changes() {
        let workspace = temporary_workspace();
        fs::write(workspace.join("AGENTS.md"), "root instruction").unwrap();
        fs::write(workspace.join("HUB.md"), "root hub instruction").unwrap();
        fs::write(workspace.join("nested/AGENTS.md"), "nested instruction").unwrap();

        let nested = workspace.join("nested");
        let first = workspace_skill_context(&nested);
        assert!(first.contains("root instruction"));
        assert!(first.contains("root hub instruction"));
        assert!(first.contains("nested instruction"));
        assert!(
            first.find("root instruction").unwrap() < first.find("nested instruction").unwrap()
        );

        fs::write(workspace.join("AGENTS.md"), "root instruction changed").unwrap();
        let refreshed = workspace_skill_context(&nested);
        assert!(refreshed.contains("root instruction changed"));
        assert!(!refreshed.contains("root instruction\n"));

        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn test_skill_catalog_summary_caching() {
        let workspace = temporary_workspace();
        let skills_dir = workspace.join("skills");
        fs::create_dir_all(&skills_dir).unwrap();

        // 1. 带 <summary> 的 Skill
        fs::write(
            skills_dir.join("chart.md"),
            "# Chart\n<summary>图表可视化指南</summary>\n正文内容...",
        )
        .unwrap();

        // 2. 不带 <summary> 的 Skill（自动走第一行有效描述提炼与指纹缓存）
        fs::write(
            skills_dir.join("git_flow.md"),
            "# Git Flow Guide\n用于规范团队分支与提交发布规范。\n## 步骤\n...",
        )
        .unwrap();

        let catalog = skill_catalog_string(&skills_dir, &[], None);
        assert!(catalog.contains("- chart: 图表可视化指南"));
        assert!(catalog.contains("- git_flow: 用于规范团队分支与提交发布规范。"));

        // 验证缓存命中
        let git_content = fs::read_to_string(skills_dir.join("git_flow.md")).unwrap();
        let cached = cached_skill_summary("git_flow", &git_content);
        assert_eq!(cached.as_deref(), Some("用于规范团队分支与提交发布规范。"));

        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn front_matter_description_replaces_placeholder_summary_cache() {
        let workspace = temporary_workspace();
        let skills_dir = workspace.join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        let content = "---\nname: diagnosis\ndescription: 深度根因排查，适用于运行时异常与调用链故障。\n---\n# 诊断专家\n正文";
        fs::write(skills_dir.join("diagnosis.md"), content).unwrap();

        set_cached_skill_summary("diagnosis", content, "---".to_string());
        let catalog = skill_catalog_string(&skills_dir, &[], None);

        assert!(catalog.contains("- diagnosis: 深度根因排查，适用于运行时异常与调用链故障。"));
        assert!(!catalog.contains("- diagnosis: ---"));
        assert_eq!(
            cached_skill_summary("diagnosis", content).as_deref(),
            Some("深度根因排查，适用于运行时异常与调用链故障。")
        );

        let _ = fs::remove_dir_all(workspace);
    }
}
