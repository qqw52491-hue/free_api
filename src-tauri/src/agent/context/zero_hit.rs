use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// 连续零命中触发宿主提示的保守默认阈值。
/// 重点不是数字本身，而是"只在明确状态变化时发一次有限提示"；
/// 待诊断样本积累误报/收益数据后再评估调整。
pub const ZERO_HIT_NOTICE_THRESHOLD: u32 = 4;

/// 检索工具族：只有明确"以查询为目的"的已声明工具才参与零命中统计。
/// 读文件、列目录、执行命令等即使返回空也不计入（那是合法探查，不是检索偏航）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalFamily {
    /// 内容搜索：关键词/正则在文件内容中检索
    ContentSearch,
    /// 文件名/路径搜索：按名称模式定位文件
    FilenameSearch,
    /// 网络搜索
    WebSearch,
}

impl RetrievalFamily {
    fn as_str(self) -> &'static str {
        match self {
            Self::ContentSearch => "内容检索",
            Self::FilenameSearch => "文件名检索",
            Self::WebSearch => "网络检索",
        }
    }

    /// 按工具 leaf 名归类（`插件名/工具名` 取斜杠后的叶子）。
    /// 保守集合：不在清单内的工具一律不参与统计。
    fn of_tool(tool_identity: &str) -> Option<Self> {
        let leaf = tool_identity.rsplit('/').next().unwrap_or(tool_identity);
        match leaf {
            "search" | "ripgrep_search" | "ripgrep_search_files" | "grep" | "code_search" => {
                Some(Self::ContentSearch)
            }
            "search_files" | "find_files" | "find" | "glob" => Some(Self::FilenameSearch),
            "web_search" | "websearch" => Some(Self::WebSearch),
            _ => None,
        }
    }
}

/// 检索结果是否为"成功执行但明确零命中"。
/// 只认结构化信号：JSON 顶层计数字段为 0，或结果集字段为空数组。
/// 无法解析为 JSON 的文本输出（格式不可知）宁可漏报也不误报，
/// 失败输出不计入（失败走 no_progress 的既有路径）。
fn is_explicit_zero_hit(output: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(output) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    for key in [
        "total_hits",
        "total",
        "count",
        "num_hits",
        "hit_count",
        "matches_count",
    ] {
        if object
            .get(key)
            .and_then(Value::as_i64)
            .is_some_and(|n| n == 0)
        {
            return true;
        }
    }
    for key in ["hits", "results", "items", "matches", "files", "findings"] {
        if object
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        {
            return true;
        }
    }
    false
}

/// 检索调用是否"成功且有结果"（命中）——任何命中都说明调查在推进，
/// 应清空全部零命中状态，而不是只清当前组。
fn is_explicit_hit(output: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(output) else {
        // 文本输出视为有内容：非空即命中（零命中必须结构化显式声明）
        return !output.trim().is_empty();
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    for key in [
        "total_hits",
        "total",
        "count",
        "num_hits",
        "hit_count",
        "matches_count",
    ] {
        if let Some(n) = object.get(key).and_then(Value::as_i64) {
            if n > 0 {
                return true;
            }
            if n == 0 {
                return false;
            }
        }
    }
    for key in ["hits", "results", "items", "matches", "files", "findings"] {
        if let Some(array) = object.get(key).and_then(Value::as_array) {
            return !array.is_empty();
        }
    }
    false
}

/// 从检索参数提取规范化作用域键：path/cwd/dir 的字符串值（去尾部斜杠）。
/// 未提供时用 "default"——同一会话的默认作用域视为同组。
fn scope_key(params: &Value) -> String {
    if let Some(object) = params.as_object() {
        for key in ["path", "cwd", "dir", "directory", "root", "scope"] {
            if let Some(locator) = object.get(key).and_then(Value::as_str) {
                return locator.trim_end_matches('/').to_string();
            }
        }
    }
    "default".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ZeroHitState {
    consecutive_zero_hits: u32,
    notice_issued: bool,
}

/// 受限检索偏航提示：针对 no_progress 的认知空洞——
/// 模型换措辞换关键词连续检索同一作用域全部零命中（签名各不相同，
/// 重复检测永不触发，但这是同一条死路换姿势）。
///
/// 语义约束（与 no_progress 同一纪律）：
/// - 只注入提示事实，不修改、不拒绝、不强制 finish；
/// - 只统计已声明检索工具族 + 成功执行 + 结构化零命中；
/// - 按 (工具族, 作用域) 归组独立计数，换族/换域即换组（变阵本身不被惩罚）；
/// - 任何一次命中清空全部状态；
/// - 每组只提示一次，命中/换组后重新累计才可能再提示。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ZeroHitTracker {
    #[serde(default)]
    states: HashMap<String, ZeroHitState>,
}

impl ZeroHitTracker {
    /// 记录一次工具执行，返回应注入的宿主提示（若有）。
    pub fn record(
        &mut self,
        tool_identity: &str,
        params: &Value,
        success: bool,
        output: &str,
    ) -> Option<String> {
        let family = RetrievalFamily::of_tool(tool_identity)?;
        if !success {
            // 失败不计入：失败重试由 no_progress 按"相同签名重复"覆盖
            return None;
        }
        let group_key = format!("{:?}/{}", family, scope_key(params));

        if is_explicit_hit(output) {
            // 任何命中都说明调查在推进：清空全部组的零命中状态
            self.states.clear();
            return None;
        }
        if !is_explicit_zero_hit(output) {
            // 无法明确判定为零命中的输出（空文本、无结构化计数的 JSON）不计入
            return None;
        }

        let state = self
            .states
            .entry(group_key.clone())
            .or_insert(ZeroHitState {
                consecutive_zero_hits: 0,
                notice_issued: false,
            });
        state.consecutive_zero_hits = state.consecutive_zero_hits.saturating_add(1);
        if state.consecutive_zero_hits >= ZERO_HIT_NOTICE_THRESHOLD && !state.notice_issued {
            state.notice_issued = true;
            return Some(format!(
                "【检索零命中提示】连续 {} 次在当前作用域内以{}未命中。请先检查关键词、目录范围、目标是否存在或是否属于另一个系统；若已有两个独立方向均未命中，可说明证据边界或向用户澄清前提。不要只替换近义词继续盲搜。",
                ZERO_HIT_NOTICE_THRESHOLD,
                family.as_str()
            ));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn zero_output() -> &'static str {
        r#"{"query":"x","total_hits":0,"hits":[]}"#
    }

    fn hit_output() -> &'static str {
        r#"{"query":"x","total_hits":3,"hits":[{"id_or_path":"src/lib.rs"}]}"#
    }

    #[test]
    fn four_distinct_zero_hit_queries_trigger_notice() {
        let mut tracker = ZeroHitTracker::default();
        for query in ["alpha", "beta", "gamma", "delta"] {
            let params = json!({"query": query, "path": "/repo"});
            let notice = tracker.record("p/search", &params, true, zero_output());
            if query == "delta" {
                assert!(notice.is_some(), "第 4 次零命中应触发提示");
                assert!(notice.unwrap().contains("零命中"));
            } else {
                assert!(notice.is_none(), "前 3 次不应提示");
            }
        }
    }

    #[test]
    fn notice_is_issued_once_per_group() {
        let mut tracker = ZeroHitTracker::default();
        let params = json!({"query": "q"});
        // 累计满阈值：恰在第 4 次触发一次
        let mut notices = 0;
        for _ in 0..ZERO_HIT_NOTICE_THRESHOLD {
            if tracker
                .record("p/ripgrep_search", &params, true, zero_output())
                .is_some()
            {
                notices += 1;
            }
        }
        assert_eq!(notices, 1, "满阈值恰好提示一次");
        // 已触发过：第 5、6、7 次零命中不再重复注入同样状态卡
        for _ in 0..3 {
            assert!(tracker
                .record("p/ripgrep_search", &params, true, zero_output())
                .is_none());
        }
    }

    #[test]
    fn any_hit_resets_all_states_and_allows_new_notice() {
        let mut tracker = ZeroHitTracker::default();
        let params = json!({"query": "q", "path": "/repo"});
        let other = json!({"query": "q2", "path": "/repo"});
        for _ in 0..ZERO_HIT_NOTICE_THRESHOLD - 1 {
            tracker.record("p/search", &params, true, zero_output());
        }
        // 另一族同作用域也积累了部分零命中
        tracker.record("p/search_files", &other, true, r#"{"files":[]}"#);

        // 一次命中清空全部组
        tracker.record("p/search", &params, true, hit_output());
        assert!(tracker.states.is_empty(), "命中应清空全部零命中状态");

        // 重新累计满阈值可再次提示（新的一轮偏航）
        let mut notices = 0;
        for _ in 0..ZERO_HIT_NOTICE_THRESHOLD {
            if tracker
                .record("p/search", &params, true, zero_output())
                .is_some()
            {
                notices += 1;
            }
        }
        assert_eq!(notices, 1, "重置后重新累计应再次且仅再次提示一次");
    }

    #[test]
    fn switching_scope_or_family_isolated_from_each_other() {
        let mut tracker = ZeroHitTracker::default();
        let repo_a = json!({"query": "q", "path": "/repo_a"});
        let repo_b = json!({"query": "q", "path": "/repo_b"});
        // 不同作用域各自计数：A 积累 3 次不触发
        for _ in 0..ZERO_HIT_NOTICE_THRESHOLD - 1 {
            assert!(tracker
                .record("p/search", &repo_a, true, zero_output())
                .is_none());
        }
        // B 作用域从零开始：3 次也不触发
        for _ in 0..ZERO_HIT_NOTICE_THRESHOLD - 1 {
            assert!(tracker
                .record("p/search", &repo_b, true, zero_output())
                .is_none());
        }
        // A 再 1 次达到阈值触发（组内累计，未被 B 干扰）
        assert!(tracker
            .record("p/search", &repo_a, true, zero_output())
            .is_some());

        // 换工具族（内容→文件名）：新组从零计数，不继承旧族计数
        let mut tracker2 = ZeroHitTracker::default();
        for _ in 0..ZERO_HIT_NOTICE_THRESHOLD {
            tracker2.record("p/search", &repo_a, true, zero_output());
        }
        for _ in 0..ZERO_HIT_NOTICE_THRESHOLD - 1 {
            assert!(
                tracker2
                    .record("p/search_files", &repo_a, true, r#"{"files":[]}"#)
                    .is_none(),
                "文件名族是新组：阈值内不应提示"
            );
        }
    }

    #[test]
    fn empty_command_output_and_failures_do_not_count() {
        let mut tracker = ZeroHitTracker::default();
        let params = json!({"query": "q", "path": "/repo"});

        // 失败调用不计入（走 no_progress 路径）
        for _ in 0..10 {
            assert!(tracker
                .record("p/search", &params, false, "error: timeout")
                .is_none());
        }
        // 空文本成功输出（如命令无 stdout、目录为空的文本报告）不可判定为零命中
        for _ in 0..10 {
            assert!(tracker.record("p/search", &params, true, "").is_none());
            assert!(tracker
                .record("p/search", &params, true, "(no output)")
                .is_none());
        }
        // 无计数/结果集字段的 JSON 不臆测为零命中
        for _ in 0..10 {
            assert!(tracker
                .record("p/search", &params, true, r#"{"status":"ok"}"#)
                .is_none());
        }
        assert!(tracker.states.is_empty());
    }

    #[test]
    fn non_retrieval_tools_are_ignored_entirely() {
        let mut tracker = ZeroHitTracker::default();
        let params = json!({"path": "/repo"});
        // 读文件/列目录/执行命令即使输出为空也不计入（合法探查）
        for _ in 0..10 {
            assert!(tracker
                .record("p/read_text_file", &params, true, "")
                .is_none());
            assert!(tracker
                .record("p/list_directory", &params, true, r#"{"items":[]}"#)
                .is_none());
            assert!(tracker.record("p/shell_exec", &params, true, "").is_none());
            // 分页/继续读取（read 类）不属于检索族
            assert!(tracker
                .record("p/read_multiple_files", &params, true, r#"{"results":[]}"#)
                .is_none());
        }
        assert!(tracker.states.is_empty());
    }

    #[test]
    fn survives_serialization_roundtrip_and_legacy_default() {
        let mut tracker = ZeroHitTracker::default();
        let params = json!({"query": "q"});
        for _ in 0..ZERO_HIT_NOTICE_THRESHOLD - 1 {
            tracker.record("p/web_search", &params, true, zero_output());
        }
        let restored: ZeroHitTracker =
            serde_json::from_value(serde_json::to_value(&tracker).unwrap()).unwrap();
        assert_eq!(restored.states.len(), 1);

        // 旧会话（无该字段）恢复为空 tracker 不 panic
        let legacy: ZeroHitTracker = serde_json::from_value(json!({})).unwrap();
        assert!(legacy.states.is_empty());
    }
}
