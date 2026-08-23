/// Provider 差异化前缀缓存策略适配层

pub trait CachePolicy: Send + Sync {
    /// 缓存块对齐大小（Tokens）
    fn block_size(&self) -> usize;
    /// 触发缓存的最小 Token 阈值
    fn min_cacheable_tokens(&self) -> usize;
    /// Provider 标识
    fn provider_name(&self) -> &str;
}


pub struct DefaultCachePolicy;
impl CachePolicy for DefaultCachePolicy {
    fn block_size(&self) -> usize {
        64
    }
    fn min_cacheable_tokens(&self) -> usize {
        0
    }
    fn provider_name(&self) -> &'static str {
        "default"
    }
}

/// 根据模型级 provider_capabilities JSON 选择缓存策略。
/// profile 的 cache 对象可声明 block_size、min_cacheable_tokens 与 provider_name；
/// 缺失或无效时退回保守默认，绝不按模型名猜测厂商。
pub fn get_cache_policy(provider_capabilities: Option<&str>) -> Box<dyn CachePolicy> {
    if let Some(caps) = provider_capabilities {
        if let Ok(profile) = serde_json::from_str::<serde_json::Value>(caps) {
            if let Some(cache) = profile.get("cache").and_then(|value| value.as_object()) {
                let block_size = cache
                    .get("block_size")
                    .and_then(|value| value.as_u64())
                    .filter(|size| *size > 0)
                    .unwrap_or(64) as usize;
                let min_cacheable_tokens = cache
                    .get("min_cacheable_tokens")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0) as usize;
                let provider_name = cache
                    .get("provider_name")
                    .and_then(|value| value.as_str())
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or("configured");
                return Box::new(ConfiguredCachePolicy {
                    block_size,
                    min_cacheable_tokens,
                    provider_name: provider_name.to_string(),
                });
            }
        }
    }
    Box::new(DefaultCachePolicy)
}

/// 从 provider_capabilities JSON 配置的缓存策略。
pub struct ConfiguredCachePolicy {
    block_size: usize,
    min_cacheable_tokens: usize,
    provider_name: String,
}

impl CachePolicy for ConfiguredCachePolicy {
    fn block_size(&self) -> usize {
        self.block_size
    }

    fn min_cacheable_tokens(&self) -> usize {
        self.min_cacheable_tokens
    }

    fn provider_name(&self) -> &str {
        &self.provider_name
    }
}

#[cfg(test)]
mod tests {
    use super::get_cache_policy;

    #[test]
    fn configured_cache_policy_uses_profile_values_without_model_name() {
        let policy = get_cache_policy(Some(
            r#"{"cache":{"block_size":128,"min_cacheable_tokens":1024,"provider_name":"openai"}}"#,
        ));
        assert_eq!(policy.block_size(), 128);
        assert_eq!(policy.min_cacheable_tokens(), 1024);
        assert_eq!(policy.provider_name(), "openai");
    }

    #[test]
    fn absent_or_invalid_profile_uses_conservative_default() {
        let absent = get_cache_policy(None);
        assert_eq!(absent.block_size(), 64);
        assert_eq!(absent.min_cacheable_tokens(), 0);
        assert_eq!(absent.provider_name(), "default");

        let invalid = get_cache_policy(Some(r#"{"cache":{"block_size":0}}"#));
        assert_eq!(invalid.block_size(), 64);
    }
}
