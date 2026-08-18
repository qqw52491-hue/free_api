use std::collections::BTreeMap;

/// 敏感凭据/密钥正则黑名单（绝对禁止明文存入上下文记忆卡，防止泄露给 API 或落盘明文）
const SENSITIVE_PATTERNS: &[&str] = &[
    "sk-",
    "Bearer ",
    "ghp_",
    "gho_",
    "glpat-",
    "-----BEGIN PRIVATE KEY-----",
    "-----BEGIN RSA PRIVATE KEY-----",
];

/// 结构化记忆项
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuredMemory {
    pub key: String,
    pub value: String,
    pub category: &'static str,
    pub step_id: usize,
    pub history_value: Option<String>, // 冲突与变更历史 (例如 "5432")
    pub pinned: bool,
}

pub struct MemoryManager;

impl MemoryManager {
    pub const MAX_MEMORIES_SLOTS: usize = 8; // 核心事实记忆最多保留 8 个关键槽位，防止事实卡片无限通胀

    /// 校验记忆合法性与安全性拦截
    pub fn is_safe_memory(key: &str, value: &str) -> Result<(), &'static str> {
        let trimmed_key = key.trim();
        let trimmed_val = value.trim();

        if trimmed_key.is_empty() || trimmed_val.is_empty() {
            return Err("Key 或 Value 不能为空");
        }
        if trimmed_key.chars().count() > 40 {
            return Err("Key 长度不能超过 40 字符");
        }
        if trimmed_val.chars().count() > 300 {
            return Err("Value 长度不能超过 300 字符（超长数据请存入 TaskStore 数据区）");
        }

        for pattern in SENSITIVE_PATTERNS {
            if trimmed_val.contains(pattern) {
                return Err("拒绝写入敏感 API Key / Token 明文！请仅存储环境变量名引用 (如 env:API_KEY)");
            }
        }

        Ok(())
    }

    /// 检查记忆卡容量是否已满
    pub fn check_capacity(current_count: usize, new_key: &str, memories: &BTreeMap<String, String>) -> Result<(), &'static str> {
        if !memories.contains_key(new_key) && current_count >= Self::MAX_MEMORIES_SLOTS {
            return Err("核心事实槽位已达上限 (最多 8 条)，请覆盖已有 key 或将持久数据存入 TaskStore 数据区");
        }
        Ok(())
    }

    /// 渲染格式化记忆卡片（按 BTreeMap 严格字母序，保证字节确定性）
    pub fn render_memory_card(memories: &BTreeMap<String, String>) -> String {
        if memories.is_empty() {
            return String::new();
        }

        let mut lines = Vec::new();
        for (key, value) in memories {
            lines.push(format!("- {}: {}", key, value));
        }

        format!("【核心事实记忆库 (Memories)】\n{}\n\n", lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_security_blocker() {
        assert!(MemoryManager::is_safe_memory("db_port", "5432").is_ok());
        assert!(MemoryManager::is_safe_memory("api_key", "sk-1234567890abcdef").is_err());
        assert!(MemoryManager::is_safe_memory("auth", "Bearer eyJhbGciOi...").is_err());
    }

    #[test]
    fn test_memory_card_alphabetical_deterministic() {
        let mut map = BTreeMap::new();
        map.insert("zebra".to_string(), "black and white".to_string());
        map.insert("apple".to_string(), "red fruit".to_string());
        map.insert("banana".to_string(), "yellow fruit".to_string());

        let rendered = MemoryManager::render_memory_card(&map);
        let apple_idx = rendered.find("apple").unwrap();
        let banana_idx = rendered.find("banana").unwrap();
        let zebra_idx = rendered.find("zebra").unwrap();

        assert!(apple_idx < banana_idx);
        assert!(banana_idx < zebra_idx);
    }
}
