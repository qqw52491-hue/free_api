use crate::agent::context::types::{SandwichContext, SideEffectRecord};

/// 不可逆操作永久台账（安全审计区，无论历史如何压缩，台账永不丢弃）
pub struct AuditLedger;

impl AuditLedger {
    /// 格式化不可逆操作台账（如超过 8 条则进行同类动作聚合折叠，保持体积紧凑）
    pub fn render_ledger(records: &[SideEffectRecord]) -> String {
        if records.is_empty() {
            return String::new();
        }

        let mut lines = Vec::new();
        let total = records.len();

        if total <= 8 {
            for r in records {
                lines.push(format!("- [Step {}] {} · {}", r.step_id, r.tool, r.description));
            }
        } else {
            // 早期记录聚合
            let omitted = total - 6;
            lines.push(format!("- [... 早期已安全执行 {} 项不可逆动作 (完整记录在 SQLite) ...]", omitted));
            for r in &records[omitted..] {
                lines.push(format!("- [Step {}] {} · {}", r.step_id, r.tool, r.description));
            }
        }

        format!(
            "【已生效的不可逆外部操作 (Side-Effects 台账)】\n(⚠️系统提示：以下动作已被物理执行，请勿重复调用！)\n{}\n\n",
            lines.join("\n")
        )
    }
}

impl SandwichContext {
    pub fn render_side_effects_ledger(&self) -> String {
        AuditLedger::render_ledger(&self.side_effects)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_ledger_small() {
        let records = vec![
            SideEffectRecord {
                step_id: 1,
                tool: "code_editor".to_string(),
                description: "created src/main.rs".to_string(),
                timestamp: "2026-08-17T00:00:00Z".to_string(),
            }
        ];
        let rendered = AuditLedger::render_ledger(&records);
        assert!(rendered.contains("created src/main.rs"));
        assert!(rendered.contains("Step 1"));
    }

    #[test]
    fn test_render_ledger_folding() {
        let mut records = Vec::new();
        for i in 0..15 {
            records.push(SideEffectRecord {
                step_id: i,
                tool: "code_editor".to_string(),
                description: format!("modified file_{}.rs", i),
                timestamp: "2026-08-17T00:00:00Z".to_string(),
            });
        }
        let rendered = AuditLedger::render_ledger(&records);
        assert!(rendered.contains("早期已安全执行"));
        assert!(rendered.contains("modified file_14.rs"));
    }
}
