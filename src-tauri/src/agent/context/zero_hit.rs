use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// 连续零命中触发宿主提示的保守默认阈值。
/// 重点不是数字本身，而是"只在明确状态变化时发一次有限提示"；
/// 待诊断样本积累误报/收益数据后再评估调整。
pub const ZERO_HIT_NOTICE_THRESHOLD: u32 = 4;

/// 零命中判定只认输出形态证据，不按工具名归类：
/// 任何工具只要返回"结果集字段为空数组"或"计数字段显式为 0"的 JSON 对象，
/// 都视为一次可判定的空结果；无法解析为 JSON 或没有结果集字段的输出不臆测。
/// 这样新接入的 MCP 检索工具无需在中央名单登记即可被覆盖，
/// 而返回非结果集形态的输出（空文本、状态对象）天然不计入。
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

/// 检索调用是否"成功且有结果"（命中）——命中和零命中同样要求结构化证据。
/// 非 JSON 文本输出不判定为命中：无关工具的非空文本（如读文件内容）
/// 不是"检索找到东西"的证据，不该清空已累计的零命中连击；
/// 它也不计入零命中——无法证明时保持中性，既不加分也不清零。
fn is_explicit_hit(output: &str) -> bool {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
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
/// - 只统计成功执行 + 结构化零命中证据（结果集形态由输出自证，不查工具名）；
/// - 按作用域归组独立计数，换域即换组（变阵本身不被惩罚）；
/// - 任何一次命中清空全部状态；
/// - 每组只提示一次，命中/换组后重新累计才可能再提示。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ZeroHitTracker {
    #[serde(default)]
    states: HashMap<String, ZeroHitState>,
    /// 跨作用域全局连续零命中：实弹实测发现模型在每个空搜索上换 path，
    /// 各作用域组永远达不到阈值（换域即换组的隔离规则被无意绕过）。
    /// 该计数只看"连续无结果"，与作用域无关；任何命中清零。
    #[serde(default)]
    global_streak: ZeroHitState,
    /// 本轮连击内是否已发过作用域级提示：发过则抑制全局提示，
    /// 避免同一连击里两条相近文案连发（提示密度每条连击至多一条）。
    #[serde(default)]
    group_notice_fired_in_streak: bool,
}

impl ZeroHitTracker {
    /// 记录一次工具执行，返回应注入的宿主提示（若有）。
    /// tool_identity 仅保留于签名与日志，不参与"是否检索工具"的判定——
    /// 判定完全由输出的结果集形态承担。
    pub fn record(
        &mut self,
        _tool_identity: &str,
        params: &Value,
        success: bool,
        output: &str,
    ) -> Option<String> {
        if !success {
            // 失败不计入：失败重试由 no_progress 按"相同签名重复"覆盖
            return None;
        }
        let group_key = scope_key(params);

        if is_explicit_hit(output) {
            // 任何命中都说明调查在推进：清空全部组的零命中状态
            self.states.clear();
            self.global_streak = ZeroHitState::default();
            self.group_notice_fired_in_streak = false;
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
        self.global_streak.consecutive_zero_hits =
            self.global_streak.consecutive_zero_hits.saturating_add(1);
        if state.consecutive_zero_hits >= ZERO_HIT_NOTICE_THRESHOLD && !state.notice_issued {
            state.notice_issued = true;
            self.group_notice_fired_in_streak = true;
            return Some(format!(
                "【检索零命中提示】连续 {} 次在当前作用域内查询结果为空。请先检查关键词、目录范围、目标是否存在或是否属于另一个系统；若已有两个独立方向均未命中，可说明证据边界或向用户澄清前提。不要只替换近义词继续盲搜。",
                ZERO_HIT_NOTICE_THRESHOLD
            ));
        }
        if self.global_streak.consecutive_zero_hits >= ZERO_HIT_NOTICE_THRESHOLD
            && !self.global_streak.notice_issued
            && !self.group_notice_fired_in_streak
        {
            self.global_streak.notice_issued = true;
            return Some(format!(
                "【检索零命中提示】已连续 {} 次跨多个作用域检索零命中——更换目录或措辞不等于更换假设。若目标文件已知，改用 local_file.read 分页精读；若不确定目标是否存在，先用文件名检索或列目录确认存在性，再决定下一个查询。",
                ZERO_HIT_NOTICE_THRESHOLD
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
        // 另一种结果集形态同作用域也积累了部分零命中
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
    fn switching_scope_isolated_from_each_other() {
        let mut tracker = ZeroHitTracker::default();
        let repo_a = json!({"query": "q", "path": "/repo_a"});
        let repo_b = json!({"query": "q", "path": "/repo_b"});
        // 不同作用域各自计数：A 积累 3 次不触发组内提示
        for _ in 0..ZERO_HIT_NOTICE_THRESHOLD - 1 {
            assert!(tracker
                .record("p/search", &repo_a, true, zero_output())
                .is_none());
        }
        // 换到 B：作用域组从零计数，但全局连击 A3+B1=4 → 触发全局提示（跨域盲搜的兜底）
        let global = tracker.record("p/search", &repo_b, true, zero_output());
        assert!(global.is_some(), "跨作用域累计零命中应触发全局提示");
        assert!(global.unwrap().contains("跨多个作用域"));
        // B 继续空命中：全局提示每连击只发一次
        for _ in 0..2 {
            assert!(tracker
                .record("p/search", &repo_b, true, zero_output())
                .is_none());
        }
        // A 再 1 次达到组内阈值：作用域级提示仍独立发放，不被全局提示挤占
        let scoped = tracker.record("p/search", &repo_a, true, zero_output());
        assert!(scoped.is_some(), "A 组内达阈值仍应发作用域提示");
        assert!(scoped.unwrap().contains("作用域"));

        // 换工具但同作用域：组键只按作用域归组，不同工具的空结果集累计同组
        let mut tracker2 = ZeroHitTracker::default();
        for _ in 0..ZERO_HIT_NOTICE_THRESHOLD {
            tracker2.record("p/search", &repo_a, true, zero_output());
        }
        // 另一工具同域继续空结果：组内 notice 已发不再重发
        assert!(tracker2
            .record("p/search_files", &repo_a, true, r#"{"files":[]}"#)
            .is_none());
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
    fn non_result_set_outputs_are_ignored_regardless_of_tool() {
        let mut tracker = ZeroHitTracker::default();
        let params = json!({"path": "/repo"});
        // 判定只看输出形态：非结果集形态（空文本、状态对象、非 JSON）一律不计入
        for _ in 0..10 {
            assert!(tracker
                .record("p/read_text_file", &params, true, "")
                .is_none());
            assert!(tracker.record("p/shell_exec", &params, true, "").is_none());
            assert!(tracker
                .record("p/anything", &params, true, r#"{"status":"ok"}"#)
                .is_none());
        }
        assert!(tracker.states.is_empty());

        // 反过来：任何工具返回"结果集为空"的形态都被计入——不查工具名
        tracker.record("p/list_directory", &params, true, r#"{"items":[]}"#);
        assert_eq!(tracker.states.len(), 1);
    }

    #[test]
    fn cross_scope_zero_streak_triggers_global_notice() {
        // case_05 实弹病理：同一问题换 path 连续空搜，作用域隔离规则被无意绕过
        let mut tracker = ZeroHitTracker::default();
        let paths = ["src", "scripts", "src-tauri", "test"];
        let mut notice = None;
        for p in paths {
            let params = json!({"query": "x", "path": p});
            notice = tracker.record("p/search", &params, true, zero_output());
        }
        assert!(
            notice.as_deref().unwrap_or("").contains("跨多个作用域"),
            "跨作用域连续零命中应触发全局提示"
        );
        // 每组作用域内计数未到阈值：不应额外再发
        for p in ["a", "b", "c"] {
            assert!(tracker
                .record("p/search", &json!({"query":"x","path":p}), true, zero_output())
                .is_none());
        }
        // 一次命中后全局清零，重新累计可再提示
        tracker.record("p/search", &json!({"query":"x","path":"z"}), true, hit_output());
        for p in ["m", "n", "o", "p"] {
            notice = tracker.record("p/search", &json!({"query":"x","path":p}), true, zero_output());
        }
        assert!(notice.is_some(), "命中清零后新一轮跨域连击应再次提示");
    }

    #[test]
    fn plain_text_output_is_neutral_neither_hit_nor_zero() {
        let mut tracker = ZeroHitTracker::default();
        let params = json!({"query": "q", "path": "/repo"});

        // 3 次结构化零命中后，穿插无关工具的非空文本输出（读文件/执行命令）
        for _ in 0..ZERO_HIT_NOTICE_THRESHOLD - 1 {
            tracker.record("p/search", &params, true, zero_output());
        }
        tracker.record("p/read_text_file", &json!({"path":"/repo/a.rs"}), true, "fn main() {}");
        tracker.record("p/shell_exec", &params, true, "build ok");

        // 文本不是命中证据：连击不被无关输出清空，第 4 次零命中仍触发提示
        let notice = tracker.record("p/search", &params, true, zero_output());
        assert!(
            notice.as_deref().unwrap_or("").contains("零命中"),
            "非结构化文本输出不得清空零命中连击"
        );
        // 也不是零命中：若误计入，提示会更早触发（上面第 3 次就满阈值了）
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
