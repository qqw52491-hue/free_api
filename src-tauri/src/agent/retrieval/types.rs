use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchParams {
    pub query: String,
    #[serde(default)]
    pub kinds: Option<Vec<String>>, // ["history", "code", "all"]，默认仅 history
    #[serde(default)]
    pub outcome: Option<String>, // "failed", "succeeded", "any"（大小写不敏感）
    #[serde(default)]
    pub since_step: Option<usize>,
    #[serde(default)]
    pub path: Option<String>, // code 检索必填：要搜索的目录绝对路径
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub kind: String, // "history" | "code" | "lesson" | "summary"
    pub id_or_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    pub snippet: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>, // 命中的上下文佐证或关联总结
}

fn default_confidence() -> i64 {
    100
}

/// 浅层三级记忆节点模型 (Level 0: 事件, Level 1: Episode片段, Level 2: 阶段摘要/Lesson)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryNode {
    pub id: String,
    pub parent_id: Option<String>,
    pub level: u8,    // 0 | 1 | 2
    pub kind: String, // "event" | "episode" | "lesson" | "decision"
    pub session_id: String,
    pub plan_id: Option<String>,
    pub step_index: Option<usize>,
    pub content: String,
    pub metadata_json: String,
    #[serde(default = "default_confidence")]
    pub confidence_score: i64,
    #[serde(default)]
    pub hit_count: i64,
    pub created_at: String,
}
