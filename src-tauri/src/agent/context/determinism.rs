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
        let mut prefix_hasher = Sha256::new();

        let system_prompt_hash = sha256_hex(&context.system_prompt);
        let active_tool_hash = sha256_hex(&context.active_tool_detail);

        for (idx, msg) in messages.iter().enumerate() {
            let content_str = match &msg.content {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };

            // 帧化哈希（role + 字节长度 + content）：裸拼接 content 存在
            // ["ab","c"] vs ["a","bc"] 的边界歧义，会让指纹误报"前缀未变"。
            let step_hash = sha256_hex(&format!("{}:{}", msg.role, content_str));
            if idx < messages.len().saturating_sub(1) {
                prefix_hasher.update(msg.role.as_bytes());
                prefix_hasher.update((content_str.len() as u64).to_le_bytes());
                prefix_hasher.update(content_str.as_bytes());
                history_step_hashes.push(step_hash);
            }
        }

        let bottom_state_hash = messages
            .last()
            .map(|m| match &m.content {
                serde_json::Value::String(s) => sha256_hex(s),
                other => sha256_hex(&other.to_string()),
            })
            .unwrap_or_default();

        let combined_prefix_hash = format!("{:x}", prefix_hasher.finalize());

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
    fn test_prefix_hash_distinguishes_message_boundaries() {
        // 帧化（role+len+content）回归：裸拼接 content 无法区分
        // ["ab","c"] 与 ["a","bc"] 两种消息切分，会让指纹误报"前缀未变"。
        let mut ctx_a = SandwichContext::new("sys".to_string(), "goal".to_string());
        ctx_a.add_assistant_raw_message("ab");
        ctx_a.add_assistant_raw_message("c");
        let mut ctx_b = SandwichContext::new("sys".to_string(), "goal".to_string());
        ctx_b.add_assistant_raw_message("a");
        ctx_b.add_assistant_raw_message("bc");
        assert_ne!(
            ContextFingerprint::capture(&ctx_a).combined_prefix_hash,
            ContextFingerprint::capture(&ctx_b).combined_prefix_hash,
            "消息边界不同必须产生不同前缀指纹"
        );
    }

    #[test]
    fn test_context_fingerprint_invariant_under_input_shuffle() {
        let entries = vec![
            ("db_port", "5432"),
            ("db_user", "postgres"),
            ("api_env", "production"),
            ("jwt_secret", "s3cr3t"),
            ("cache_ttl", "3600"),
        ];

        // 基准指纹：按初始顺序插入
        let mut baseline_ctx = SandwichContext::new(
            "SYSTEM: You are Free API Agent".to_string(),
            "Build a Rust app".to_string(),
        );
        for (k, v) in &entries {
            let _ = baseline_ctx.update_memory(k.to_string(), v.to_string());
        }
        let baseline_fp = ContextFingerprint::capture(&baseline_ctx);

        // 进行 50 次完全打乱顺序的上下文重新构建，验证 BTreeMap 确保指纹 100% 相同
        for i in 1..=50 {
            let mut shuffled = entries.clone();
            // 简单的确定性置换算法，避免依赖外部 rand crate
            let len = shuffled.len();
            shuffled.rotate_left(i % len);
            if i % 2 == 0 {
                shuffled.reverse();
            }

            let mut fresh_ctx = SandwichContext::new(
                "SYSTEM: You are Free API Agent".to_string(),
                "Build a Rust app".to_string(),
            );
            for (k, v) in &shuffled {
                let _ = fresh_ctx.update_memory(k.to_string(), v.to_string());
            }

            let fresh_fp = ContextFingerprint::capture(&fresh_ctx);
            assert_eq!(
                baseline_fp.combined_prefix_hash, fresh_fp.combined_prefix_hash,
                "Prefix hash must be identical regardless of insertion order (iteration {})",
                i
            );
            assert_eq!(
                baseline_fp.bottom_state_hash, fresh_fp.bottom_state_hash,
                "Bottom state hash must be identical regardless of insertion order (iteration {})",
                i
            );
        }
    }
}
