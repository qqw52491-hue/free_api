use crate::agent::context::types::SandwichContext;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// 计算字符串的 SHA-256 十六进制指纹
pub fn sha256_hex(data: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// 上下文分段前缀指纹快照（用于排查缓存命中率异常暴跌的根因）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFingerprint {
    pub system_prompt_hash: String,
    pub active_tool_hash: String,
    pub history_step_hashes: Vec<String>,
    pub bottom_state_hash: String,
    pub combined_prefix_hash: String,
}

impl ContextFingerprint {
    /// 对当前的 SandwichContext 提取各分段哈希指纹
    pub fn capture(context: &SandwichContext) -> Self {
        let messages = context.assemble_messages();
        let mut history_step_hashes = Vec::new();
        let mut prefix_buffer = String::new();

        let mut system_prompt_hash = String::new();
        let mut active_tool_hash = String::new();
        let mut bottom_state_hash = String::new();

        for (idx, msg) in messages.iter().enumerate() {
            let content_str = match &msg.content {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };

            let step_hash = sha256_hex(&content_str);

            if idx == 0 {
                system_prompt_hash = step_hash.clone();
                prefix_buffer.push_str(&content_str);
            } else if idx == 1 && !context.active_tool_detail.is_empty() {
                active_tool_hash = step_hash.clone();
                prefix_buffer.push_str(&content_str);
            } else if idx == messages.len() - 1 {
                bottom_state_hash = step_hash.clone();
            } else {
                history_step_hashes.push(step_hash);
                prefix_buffer.push_str(&content_str);
            }
        }

        let combined_prefix_hash = sha256_hex(&prefix_buffer);

        Self {
            system_prompt_hash,
            active_tool_hash,
            history_step_hashes,
            bottom_state_hash,
            combined_prefix_hash,
        }
    }

    /// 对比两个快照，定位是哪一个分段被篡改砸碎了缓存
    pub fn diff(&self, other: &Self) -> BTreeMap<&'static str, (String, String)> {
        let mut differences = BTreeMap::new();

        if self.system_prompt_hash != other.system_prompt_hash {
            differences.insert(
                "system_prompt",
                (
                    self.system_prompt_hash.clone(),
                    other.system_prompt_hash.clone(),
                ),
            );
        }
        if self.active_tool_hash != other.active_tool_hash {
            differences.insert(
                "active_tool",
                (
                    self.active_tool_hash.clone(),
                    other.active_tool_hash.clone(),
                ),
            );
        }
        if self.combined_prefix_hash != other.combined_prefix_hash {
            differences.insert(
                "combined_prefix",
                (
                    self.combined_prefix_hash.clone(),
                    other.combined_prefix_hash.clone(),
                ),
            );
        }

        differences
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_hex_basic() {
        // 验证幂等性：相同输入永远输出相同 hash
        let h1 = sha256_hex("hello world");
        let h2 = sha256_hex("hello world");
        assert_eq!(h1, h2, "SHA-256 must be deterministic");

        // 验证长度：SHA-256 hex 永远是 64 个字符
        assert_eq!(h1.len(), 64, "SHA-256 hex must be 64 chars");

        // 验证不同输入产生不同结果
        let h3 = sha256_hex("hello world!");
        assert_ne!(h1, h3, "Different inputs must produce different hashes");
    }

    #[test]
    fn test_context_fingerprint_deterministic_1000_iterations() {
        let mut ctx = SandwichContext::new(
            "SYSTEM: You are Free API Agent".to_string(),
            "Build a Rust app".to_string(),
        );
        ctx.update_memory("db_port".to_string(), "5432".to_string());
        ctx.update_memory("db_user".to_string(), "postgres".to_string());
        ctx.update_memory("api_env".to_string(), "production".to_string());

        let base_fp = ContextFingerprint::capture(&ctx);

        for _ in 0..1000 {
            let current_fp = ContextFingerprint::capture(&ctx);
            assert_eq!(
                base_fp.combined_prefix_hash, current_fp.combined_prefix_hash,
                "Prefix hash must be 100% byte-for-byte deterministic across 1000 iterations!"
            );
        }
    }
}
