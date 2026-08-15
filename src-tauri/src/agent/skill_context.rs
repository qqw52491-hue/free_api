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
}
