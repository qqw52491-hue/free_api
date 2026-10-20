//! output_id 只用宿主记录解析，不采信引用中自填的步骤、运行或时间。
use crate::db::DbState;

pub(crate) struct EvidenceLookup<'a> {
    pub db: &'a DbState,
    pub session_id: &'a str,
    pub run_id: &'a str,
}

impl EvidenceLookup<'_> {
    pub(super) fn completed_step(&self, output_id: &str) -> Result<usize, String> {
        let source = crate::agent::run_trace::saved_output_source(self.db, self.session_id, output_id)
            .map_err(|_| format!("output_id:{output_id} 无法在本会话回查；本次未登记 verified。请核对完整句柄或引用已有有效步骤。"))?;
        if source["run_location"].as_str() != Some("exact_output_id") {
            return Err(format!("output_id:{output_id} 的运行来源缺失或有歧义，无法确认时序；请引用来源明确的已有记录，不按时间接近或相同步骤号猜测。"));
        }
        if source["run_id"].as_str() != Some(self.run_id) {
            return Err(format!("output_id:{output_id} 属于其他运行，不能用其步骤号证明本运行最近副作用后的时序；可作为历史参考交终审，但本次未据此登记 verified。"));
        }
        let seq = source["run_seq"]
            .as_i64()
            .ok_or_else(|| "输出记录缺少可回查的运行内位置".to_string())?;
        let conn = self.db.reader()?;
        let status: Option<String> = conn.query_row(
            "SELECT json_extract(step_json,'$.status') FROM agent_run_steps WHERE run_id=?1 AND seq=?2 AND json_extract(step_json,'$.output_id')=?3",
            rusqlite::params![self.run_id, seq, output_id], |row| row.get(0),
        ).map_err(|e| format!("无法回查输出完成状态：{e}"))?;
        let status = status.as_deref().or_else(|| source["outcome"].as_str());
        // 非零退出也可以是有效观测；这里只确认已完成，不把失败结果判成测试通过。
        if !matches!(status, Some("succeeded" | "failed" | "finish_challenged")) {
            return Err(format!(
                "output_id:{output_id} 未能确认步骤已完成（状态：{}）；本次未登记 verified。",
                status.unwrap_or("未知")
            ));
        }
        source["step_id"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| format!("output_id:{output_id} 的步骤位置无效，无法确认时序"))
    }
}

/// 识别 output_id: / output_id= 及独立 UUID。句柄中的 step_ 数字不是步骤引用。
pub(super) fn split_output_references(references: &[String]) -> (Vec<String>, Vec<String>) {
    let mut ids = std::collections::BTreeSet::new();
    let mut steps = Vec::new();
    for reference in references {
        let bare = reference.trim().trim_matches('`');
        if uuid::Uuid::parse_str(bare).is_ok() {
            ids.insert(bare.to_owned());
            continue;
        }
        let mut masked = reference.clone();
        let lower = reference.to_ascii_lowercase();
        for (index, _) in lower.match_indices("output_id") {
            if index > 0
                && (lower.as_bytes()[index - 1].is_ascii_alphanumeric()
                    || lower.as_bytes()[index - 1] == b'_')
            {
                continue;
            }
            let tail = reference[index + 9..]
                .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '`'));
            let Some(tail) = tail.strip_prefix(':').or_else(|| tail.strip_prefix('=')) else {
                continue;
            };
            let tail = tail
                .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '`'));
            let len = tail
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                .count();
            if len == 0 {
                continue;
            }
            ids.insert(tail[..len].to_owned());
            let start = reference.len() - tail.len();
            masked.replace_range(start..start + len, &" ".repeat(len));
        }
        steps.push(masked);
    }
    (ids.into_iter().collect(), steps)
}
