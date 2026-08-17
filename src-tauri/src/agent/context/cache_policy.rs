/// Provider 差异化前缀缓存策略适配层

pub trait CachePolicy: Send + Sync {
    /// 缓存块对齐大小（Tokens）
    fn block_size(&self) -> usize;
    /// 触发缓存的最小 Token 阈值
    fn min_cacheable_tokens(&self) -> usize;
    /// Provider 标识
    fn provider_name(&self) -> &'static str;
}

pub struct DeepSeekCachePolicy;
impl CachePolicy for DeepSeekCachePolicy {
    fn block_size(&self) -> usize {
        64
    }
    fn min_cacheable_tokens(&self) -> usize {
        0 // DeepSeek 自动前缀缓存，无硬性 1024 门槛
    }
    fn provider_name(&self) -> &'static str {
        "deepseek"
    }
}

pub struct OpenAICachePolicy;
impl CachePolicy for OpenAICachePolicy {
    fn block_size(&self) -> usize {
        128
    }
    fn min_cacheable_tokens(&self) -> usize {
        1024 // OpenAI 需 >= 1024 tokens 才启用 Prompt Caching
    }
    fn provider_name(&self) -> &'static str {
        "openai"
    }
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

pub fn get_cache_policy(model_name: &str) -> Box<dyn CachePolicy> {
    let lower = model_name.to_lowercase();
    if lower.contains("deepseek") {
        Box::new(DeepSeekCachePolicy)
    } else if lower.contains("gpt") || lower.contains("openai") {
        Box::new(OpenAICachePolicy)
    } else {
        Box::new(DefaultCachePolicy)
    }
}
