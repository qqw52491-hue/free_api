//! 终态预检（prepare_finish）：与 finish 同一套门禁做交付就绪评估。
//! 就绪收据在 diff/scope/契约未变时被 finish 直接复用，不重复烧验证与终审。
//! 从 run_agent_main_loop_in_workspace_inner 抽出，逻辑不变。

use crate::agent::checkpoint::AgentCheckpoint;
use crate::agent::context::types::SandwichContext;
use crate::agent::reviewer_gatekeeper::GitBaseline;
use crate::agent::router::ModelRoutingConfig;
use crate::agent::types::AgentInstruction;
use crate::db::DbState;
use chrono::{DateTime, Utc};
use std::path::Path;
use tauri::AppHandle;
use tokio::sync::watch;

/// 只渲染共享评估链的收据，不再自己执行另一套门禁。
pub(crate) fn render_prepare_finish(
    checkpoint: &mut AgentCheckpoint,
    _context: &SandwichContext,
    receipt: &crate::agent::finalization::FinalizationReceipt,
) -> String {
    // 空转检测（与 finish 质询共用计数字段）：同一组阻塞连续预检未变化，只报告事实，不推断模型行为。
    // 只追加提示，不拦截、不动重试预算。
    let spin_hint = if receipt.is_ready() {
        checkpoint.last_finish_blocker_sig = None;
        checkpoint.repeated_blocker_count = 0;
        String::new()
    } else {
        let sig = prepare_blocker_signature(&receipt.blockers);
        if checkpoint.last_finish_blocker_sig == Some(sig) {
            checkpoint.repeated_blocker_count = checkpoint.repeated_blocker_count.saturating_add(1);
        } else {
            checkpoint.last_finish_blocker_sig = Some(sig);
            checkpoint.repeated_blocker_count = 1;
        }
        prepare_spin_hint(checkpoint.repeated_blocker_count)
    };
    let mut output_text = if receipt.is_ready() {
        format!(
            "✅ 终态预检就绪（receipt {}）：当前版本验收已就绪。\n现在用本次预检的同一份 summary 和 details 调用 finish；交付内容、scope、契约和验收状态未变时复用本收据，不再重复验证与终审。补充或修改正文后需要重新核验。",
            receipt.receipt_id
        )
    } else {
        format!(
            "⚠️ 终态预检未就绪（receipt {}）：\n{}\n请逐项闭合后重新 prepare_finish，或修复后直接调用 finish。{}",
            receipt.receipt_id,
            receipt
                .blockers
                .iter()
                .map(|b| format!("- {}", b))
                .collect::<Vec<_>>()
                .join("\n"),
            spin_hint
        )
    };
    for notice in &receipt.material_notices {
        output_text.push_str(&format!("\n材料变化披露：{notice}"));
    }
    output_text
}

// 验证失败的诊断正文会包含耗时、下载进度等变化；沿用旧的「义务 id + 退出码」计数口径。
// 这里只影响提示计数，不改变收据阻塞项、验证结果或任何放行条件。
fn prepare_blocker_signature(blockers: &[String]) -> u64 {
    use std::hash::{Hash, Hasher};
    let keys: Vec<String> = blockers
        .iter()
        .map(|blocker| {
            if blocker.starts_with("义务 [") && blocker.contains("\n验证命令:") {
                let first_line = blocker.lines().next().unwrap_or(blocker);
                if let Some((prefix, _)) = first_line.split_once('；') {
                    return format!("{}）", prefix);
                }
            }
            blocker.clone()
        })
        .collect();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    keys.hash(&mut hasher);
    hasher.finish()
}

/// 同一组预检阻塞连续未变 >=3 次时的指引：点破 echo / git diff --stat 这类命令不会改变契约状态。
fn prepare_spin_hint(repeated: u32) -> String {
    if repeated < 3 {
        return String::new();
    }
    format!(
        "\n\n【同一组阻塞已连续 {} 次预检未变化】建议核对上方具体失败、有效审查意见或权限边界。验证方法不适用时可用 review_contract_update 显式修订，并提供理由与已有证据；无需仅为同步状态重填契约或写后验证。意见无法解决时可用 ask_human 呈报分歧。",

        repeated
    )
}

#[cfg(test)]
mod spin_hint_tests {
    use super::{prepare_blocker_signature, prepare_spin_hint};

    #[test]
    fn changing_diagnostics_keep_legacy_obligation_failure_signature() {
        let old = vec!["义务 [unit] 验证失败（退出码 1）".to_string()];
        let a = vec!["义务 [unit] 验证失败（退出码 1；验证命令已执行，未通过）\n验证命令: `mvn test`\n宿主启动目录: /tmp/ws\n耗时: 1s".to_string()];
        let b = vec!["义务 [unit] 验证失败（退出码 1；验证命令已执行，未通过）\n验证命令: `mvn test`\n宿主启动目录: /tmp/ws\n耗时: 2s\n不同下载进度".to_string()];
        assert_eq!(
            prepare_blocker_signature(&a),
            prepare_blocker_signature(&old)
        );
        assert_eq!(prepare_blocker_signature(&a), prepare_blocker_signature(&b));
        assert_ne!(
            prepare_blocker_signature(&a),
            prepare_blocker_signature(&[b[0].replace("退出码 1", "退出码 2")])
        );
    }

    #[test]
    fn other_blockers_keep_existing_hash_behavior() {
        use std::hash::{Hash, Hasher};
        let blockers = vec![
            "审查义务 x 尚未闭合".to_string(),
            "scope 越界：src/a.rs".to_string(),
        ];
        let mut old = std::collections::hash_map::DefaultHasher::new();
        blockers.hash(&mut old);
        assert_eq!(prepare_blocker_signature(&blockers), old.finish());
        assert_ne!(
            prepare_blocker_signature(&blockers),
            prepare_blocker_signature(&["审查义务 y 尚未闭合".to_string()])
        );
    }

    #[test]
    fn hint_only_from_third_repeat() {
        assert!(prepare_spin_hint(1).is_empty());
        assert!(prepare_spin_hint(2).is_empty());
        let h = prepare_spin_hint(3);
        assert!(h.contains("review_contract_update") && h.contains("ask_human"));
    }
}
