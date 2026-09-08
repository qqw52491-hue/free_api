use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 默认子任务派发超时（秒）
pub const DEFAULT_DISPATCH_TIMEOUT_SECS: u64 = 60;

fn default_attempt_id() -> u32 {
    1
}

fn default_tier() -> String {
    "flash".to_string()
}

fn default_effort() -> String {
    "high".to_string()
}

fn default_timeout_secs() -> u64 {
    DEFAULT_DISPATCH_TIMEOUT_SECS
}

/// 任务与执行尝试联合键：用于宿主幂等收件与排重
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskAttemptKey {
    pub task_id: String,
    pub attempt_id: u32,
}

impl TaskAttemptKey {
    pub fn new(task_id: impl Into<String>, attempt_id: u32) -> Self {
        Self {
            task_id: task_id.into(),
            attempt_id,
        }
    }
}

/// 通用派发信封（Dispatch Envelope）：宿主向子沙箱派发任务的标准数据结构
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DispatchEnvelope {
    /// 逻辑任务标识（如 "plan_review", "fix_bug"）
    pub task_id: String,
    /// 执行尝试计数（从 1 开始，重试递增，解决迟到与幂等识别）
    #[serde(default = "default_attempt_id")]
    pub attempt_id: u32,
    /// 目标角色名（如 "child_reviewer", "child_writer", "child_worker"）
    pub role_id: String,
    /// 绑定的交付契约及版本（如 "reviewer.v1", "writer.v1", "worker.v1"）
    pub contract_id: String,
    /// 父级运行 ID
    pub parent_run_id: String,
    /// 父级会话 ID
    pub parent_session_id: String,
    /// 选用模型
    pub model_id: String,
    /// 模型 Tier（如 "flash", "pro"）
    #[serde(default = "default_tier")]
    pub model_tier: String,
    /// 推理强度（如 "high", "medium", "low"）
    #[serde(default = "default_effort")]
    pub reasoning_effort: String,
    /// 角色提示词设定
    #[serde(default)]
    pub persona: String,
    /// 超时时间（秒）
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// 领域特定入参载荷（由具体角色解析，宿主通用层不干涉内部字段）
    #[serde(default)]
    pub input_payload: Value,
}

impl DispatchEnvelope {
    pub fn attempt_key(&self) -> TaskAttemptKey {
        TaskAttemptKey::new(&self.task_id, self.attempt_id)
    }
}

/// 通用交付回执（Delivery Receipt）：子任务终态出库后交付给宿主记录与消费的凭据
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeliveryReceipt {
    /// 对应的子任务物理运行 ID
    pub child_run_id: String,
    /// 逻辑任务 ID
    pub task_id: String,
    /// 执行尝试号
    pub attempt_id: u32,
    /// 任务类型（"review", "writer", "worker"）
    pub kind: String,
    /// 宿主观测的执行状态（"succeeded", "timed_out", "failed", "cancelled", "invalid_output" 等）
    pub status: String,
    /// 真实执行耗时（毫秒）
    pub duration_ms: u64,
    /// 输出 token 数
    pub completion_tokens: i64,
    /// 输入 token 数
    pub prompt_tokens: i64,
    /// 异常分类（若有）
    pub error_kind: Option<String>,
    /// 结构化领域载荷 JSON（成功时有效）
    pub payload: Option<Value>,
    /// 原始返回文本/JSON（带截断留存，用于半截非法 JSON 排查诊断）
    pub raw_text: Option<String>,
}

impl DeliveryReceipt {
    pub fn attempt_key(&self) -> TaskAttemptKey {
        TaskAttemptKey::new(&self.task_id, self.attempt_id)
    }

    /// 是否属于基础设施/执行异常（超时、中断、崩溃，非业务拒绝）
    pub fn is_infra_failure(&self) -> bool {
        matches!(
            self.error_kind.as_deref(),
            Some("timed_out")
                | Some("timeout")
                | Some("interrupted")
                | Some("cancelled")
                | Some("invalid_output")
        )
    }

    /// 是否正常执行成功
    pub fn is_success(&self) -> bool {
        self.status == "succeeded" && !self.is_infra_failure()
    }

    /// 强类型提取审查员交付产物
    pub fn as_reviewer_delivery(&self) -> Option<crate::agent::child_reviewer::ReviewerDeliveryData> {
        self.payload
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .or_else(|| {
                self.raw_text
                    .as_deref()
                    .and_then(|t| serde_json::from_str(t).ok())
            })
    }

    /// 强类型提取写入工兵交付产物
    pub fn as_writer_result(&self) -> Option<crate::agent::child_writer::WriterResult> {
        self.payload
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .or_else(|| {
                self.raw_text
                    .as_deref()
                    .and_then(|t| serde_json::from_str(t).ok())
            })
    }

    /// 强类型提取只读侦察任务交付产物
    pub fn as_worker_result(&self) -> Option<crate::agent::child_worker::WorkerResult> {
        self.payload
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .or_else(|| {
                self.raw_text
                    .as_deref()
                    .and_then(|t| serde_json::from_str(t).ok())
            })
    }
}

impl From<crate::agent::child_reviewer::ChildReviewResult> for DeliveryReceipt {
    fn from(res: crate::agent::child_reviewer::ChildReviewResult) -> Self {
        let kind = res.kind.clone().unwrap_or_else(|| "review".to_string());
        let raw_text = res.raw_result_json.clone();
        let payload = raw_text
            .as_deref()
            .and_then(|t| serde_json::from_str::<Value>(t).ok())
            .or_else(|| serde_json::to_value(&res).ok());

        Self {
            child_run_id: res.child_run_id.clone(),
            task_id: res.child_run_id.clone(),
            attempt_id: 1,
            kind,
            status: res.status,
            duration_ms: res.duration_ms,
            completion_tokens: res.completion_tokens,
            prompt_tokens: res.prompt_tokens,
            error_kind: res.error_kind,
            payload,
            raw_text,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dispatch_envelope_defaults_and_roundtrip() {
        let envelope = DispatchEnvelope {
            task_id: "task-test-01".to_string(),
            attempt_id: 2,
            role_id: "child_reviewer".to_string(),
            contract_id: "reviewer.v1".to_string(),
            parent_run_id: "parent-r1".to_string(),
            parent_session_id: "parent-s1".to_string(),
            model_id: "gemini-2.5-pro".to_string(),
            model_tier: "pro".to_string(),
            reasoning_effort: "high".to_string(),
            persona: "strict_auditor".to_string(),
            timeout_secs: 120,
            input_payload: serde_json::json!({"question": "security audit"}),
        };

        let key = envelope.attempt_key();
        assert_eq!(key.task_id, "task-test-01");
        assert_eq!(key.attempt_id, 2);

        let json_str = serde_json::to_string(&envelope).unwrap();
        let recovered: DispatchEnvelope = serde_json::from_str(&json_str).unwrap();
        assert_eq!(recovered, envelope);
    }

    #[test]
    fn test_delivery_receipt_infra_failure_detection() {
        let receipt = DeliveryReceipt {
            child_run_id: "child-1".to_string(),
            task_id: "task-1".to_string(),
            attempt_id: 1,
            kind: "review".to_string(),
            status: "timed_out".to_string(),
            duration_ms: 60000,
            completion_tokens: 0,
            prompt_tokens: 100,
            error_kind: Some("timed_out".to_string()),
            payload: None,
            raw_text: None,
        };

        assert!(receipt.is_infra_failure());
        assert!(!receipt.is_success());
    }

    #[test]
    fn test_delivery_receipt_corrupted_raw_text_preservation() {
        let receipt = DeliveryReceipt {
            child_run_id: "child-corrupt".to_string(),
            task_id: "task-corrupt".to_string(),
            attempt_id: 1,
            kind: "review".to_string(),
            status: "invalid_output".to_string(),
            duration_ms: 1500,
            completion_tokens: 20,
            prompt_tokens: 50,
            error_kind: Some("invalid_output".to_string()),
            payload: None,
            raw_text: Some(r#"{"verdict": "pass", broken..."#.to_string()),
        };

        assert!(receipt.is_infra_failure());
        assert_eq!(receipt.raw_text.as_deref(), Some(r#"{"verdict": "pass", broken..."#));
        assert!(receipt.as_reviewer_delivery().is_none());
    }
}
