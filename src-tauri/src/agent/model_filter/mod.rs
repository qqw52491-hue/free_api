// ========================================================================
// Model Parameter Normalizer & Self-Healing Middleware
// 多厂商模型参数归一化与自愈中间层
//
// 架构分层：
// - family: 规范化模型协议家族定义（ModelFamily）
// - detector: 基于品牌根词与特征的智能协议识别器（detect_model_family）
// - normalizer: 请求体参数清洗、思考强度映射与白名单控制器（sanitize_and_normalize_body）
// - healer: 400 Bad Request 动态自愈与未知参数提取引擎（self_heal_bad_request_payload）
// ========================================================================

pub mod detector;
pub mod family;
pub mod healer;
pub mod normalizer;

#[cfg(test)]
mod tests;

pub use detector::detect_model_family;
pub use family::ModelFamily;
pub use healer::self_heal_bad_request_payload;
pub use normalizer::sanitize_and_normalize_body;
