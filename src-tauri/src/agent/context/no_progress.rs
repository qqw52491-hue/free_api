use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// 无进展重复的运行时检测与策略级提示。
///
/// 只按交接契约认定"坏重复"：相同工具、规范化后相同参数，且连续返回相同
/// 结果指纹（成功）或相同失败（失败）。不同行窗口、不同字段、换措辞的
/// 检索词、截断续读都不计入——它们是合法取证行为。
///
/// 本组件只生成注入历史流的提示事实，绝不修改、补全或拒绝模型提交的参数。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NoProgressTracker {
    #[serde(default)]
    entries: HashMap<String, NoProgressEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct NoProgressEntry {
    occurrences: u32,
    last_fingerprint: u64,
    consecutive_same: u32,
}

/// 相同调用连续第 N 次返回相同成功结果时提示（前两次复查视为合理）。
const NO_PROGRESS_SUCCESS_THRESHOLD: u32 = 3;
/// 相同调用连续第 N 次相同失败时提示（与系统契约"连续 2 次失败必须变阵"对齐）。
const NO_PROGRESS_FAILURE_THRESHOLD: u32 = 2;
/// 有界状态：签名数超限后整体重置，检测是启发式而非审计账本。
const MAX_TRACKED_SIGNATURES: usize = 64;
/// 指纹只取输出头部：尾部的时间戳、耗时类噪声不应掩盖"结果相同"的事实。
const FINGERPRINT_HEAD_CHARS: usize = 512;

fn output_fingerprint(success: bool, output: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    success.hash(&mut hasher);
    output
        .chars()
        .take(FINGERPRINT_HEAD_CHARS)
        .collect::<String>()
        .hash(&mut hasher);
    hasher.finish()
}

fn action_signature(tool: &str, params: &Value) -> String {
    let params_text = params.to_string();
    format!("{} | {}", tool, params_text.chars().take(360).collect::<String>())
}

impl NoProgressTracker {
    /// 记录一次执行，返回应当注入历史流的提示（无进展时才返回 Some）。
    pub fn record(
        &mut self,
        tool: &str,
        params: &Value,
        success: bool,
        output: &str,
    ) -> Option<String> {
        let key = action_signature(tool, params);
        if self.entries.len() >= MAX_TRACKED_SIGNATURES && !self.entries.contains_key(&key) {
            self.entries.clear();
        }
        let fingerprint = output_fingerprint(success, output);
        let entry = self.entries.entry(key).or_default();
        entry.occurrences = entry.occurrences.saturating_add(1);
        if entry.occurrences > 1 && entry.last_fingerprint == fingerprint {
            entry.consecutive_same = entry.consecutive_same.saturating_add(1);
        } else {
            entry.consecutive_same = 1;
            entry.last_fingerprint = fingerprint;
        }

        let consecutive = entry.consecutive_same;
        let occurrences = entry.occurrences;
        if success && consecutive >= NO_PROGRESS_SUCCESS_THRESHOLD {
            return Some(format!(
                "【无进展重复提示】工具 {tool} 以相同参数已连续 {consecutive} 次返回相同结果（累计 {occurrences} 次）。请变更参数或证据源；若结论所需证据已充分，请整理结论并收尾，不要继续原样重复同一调用。"
            ));
        }
        if !success && consecutive >= NO_PROGRESS_FAILURE_THRESHOLD {
            return Some(format!(
                "【重复失败提示】工具 {tool} 以相同参数已连续 {consecutive} 次以相同方式失败。请依据错误与工具契约改换参数、改用目录中已声明的替代工具，或基于已收集证据给出受限结论；不要再次原样重试。"
            ));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identical_success_fires_on_third_repeat_only() {
        let mut tracker = NoProgressTracker::default();
        let params = json!({"pattern": "ResourceScopeObservation", "cwd": "/w/p"});

        assert!(tracker
            .record("p/ripgrep_search", &params, true, "3 hits ...")
            .is_none());
        // 第二次相同结果：复查视为合理，不提示。
        assert!(tracker
            .record("p/ripgrep_search", &params, true, "3 hits ...")
            .is_none());
        // 第三次相同结果：触发提示。
        let notice = tracker
            .record("p/ripgrep_search", &params, true, "3 hits ...")
            .expect("第三次相同成功结果必须提示");
        assert!(notice.contains("无进展重复提示"));
        assert!(notice.contains("3 次"));
    }

    #[test]
    fn identical_failure_fires_on_second_repeat() {
        let mut tracker = NoProgressTracker::default();
        let params = json!({"path": "a.rs", "startLine": 1, "endLine": 50});

        assert!(tracker
            .record("p/read_text_file", &params, false, "path not found")
            .is_none());
        let notice = tracker
            .record("p/read_text_file", &params, false, "path not found")
            .expect("第二次相同失败必须提示");
        assert!(notice.contains("重复失败提示"));
    }

    #[test]
    fn changed_output_or_params_reset_the_streak() {
        let mut tracker = NoProgressTracker::default();
        let params = json!({"pattern": "x"});

        // 结果变化：同一调用但输出带回了新证据，重置计数。
        tracker.record("t", &params, true, "result v1");
        tracker.record("t", &params, true, "result v2");
        assert!(
            tracker.record("t", &params, true, "result v3").is_none(),
            "输出每次不同（新证据）时不得提示"
        );

        // 参数变化：换措辞/换窗口是合法探索。
        let other = json!({"pattern": "x", "glob": "*.rs"});
        assert!(tracker.record("t", &other, true, "result v3").is_none());
    }

    #[test]
    fn different_line_windows_are_not_flagged() {
        // 交接契约：同文件不同 line window 不属于坏重复（分页取证是正常行为）。
        // 每个窗口只读一次，构成真实的顺序分页序列。
        let mut tracker = NoProgressTracker::default();
        for start in (1..=481).step_by(120) {
            let window = json!({"path": "big.rs", "startLine": start, "endLine": start + 119});
            assert!(
                tracker
                    .record("p/read_text_file", &window, true, "lines...")
                    .is_none(),
                "顺序分页窗口 startLine={} 不应被标记",
                start
            );
        }
    }

    #[test]
    fn tracker_survives_serde_roundtrip_with_legacy_default() {
        let mut tracker = NoProgressTracker::default();
        tracker.record("t", &json!({"a": 1}), true, "x");
        let restored: NoProgressTracker =
            serde_json::from_value(serde_json::to_value(&tracker).unwrap()).unwrap();
        assert_eq!(restored.entries.len(), 1);

        let legacy: NoProgressTracker = serde_json::from_value(json!({})).unwrap();
        assert!(legacy.entries.is_empty());
    }
}
