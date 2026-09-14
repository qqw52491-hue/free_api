//! 门禁适用域（applies_to）三值逻辑——所有机械门开火前的自证层。
//!
//! 机械门禁必须是三值的：
//! - Pass：本门适用且证据合规；
//! - Block：本门适用且证据客观违规；
//! - NotApplicable：本门的证据域在当前任务中根本不存在
//!   （如运维任务不产生 Git diff，"目标∩diff 零交集"无从谈起）。
//!
//! 把"前提不成立"硬当"违规"是误伤之源（拿测温枪测电压）。
//! NotApplicable 的正确姿势：跳过本门 → 留下 Gate Notice 交接说明移交终审（语义层）
//! → 记录遥测以便审计 Skip 率（防模型系统性钻空子）。

use std::borrow::Cow;
use std::path::Path;

/// 机械门适用域判定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateApplicability {
    /// 证据域存在，机械门正常判定（Pass/Block 由门自身的证据规则给出）
    Applicable,
    /// 证据域不存在：跳过本门并移交终审。reason 进 Gate Notice 与遥测审计。
    NotApplicable { reason: Cow<'static, str> },
}

impl GateApplicability {
    /// 便捷构造：静态原因
    pub fn not_applicable(reason: &'static str) -> Self {
        Self::NotApplicable {
            reason: Cow::Borrowed(reason),
        }
    }

    /// 便捷构造：带动态细节（义务 id、目标路径等）的原因
    pub fn not_applicable_owned(reason: String) -> Self {
        Self::NotApplicable {
            reason: Cow::Owned(reason),
        }
    }

    pub fn is_applicable(&self) -> bool {
        matches!(self, Self::Applicable)
    }

    /// 移交终审的交接说明（Applicable → None）。
    /// 说明必须指明本门未覆盖什么、终审该看什么语义证据——Skip 绝不静默。
    pub fn handoff_notice(&self, gate_label: &str) -> Option<String> {
        match self {
            Self::Applicable => None,
            Self::NotApplicable { reason } => Some(format!(
                "[Gate Notice] {gate_label}不适用：{reason}；本门未做机械判定，请终审以命令退出码、终端输出、API 响应或系统状态等语义证据核验对应结论"
            )),
        }
    }
}

/// 判定声明目标是否为工作区内路径（diff 对账的判据域）：
/// - 相对路径一律视为工作区内（以工作区为锚）；
/// - 绝对路径须落在 workspace 前缀内；
/// - `~/` 展开后判断；盘符/URL/空串一律视作工作区外。
/// 运维/系统路径（/usr/bin、/opt、~/.config 等）→ 工作区外 → diff 对账无判据。
pub fn is_workspace_target(target: &str, workspace: &Path) -> bool {
    let t = target
        .trim()
        .trim_matches(|c| matches!(c, '`' | '"' | '\'' | '*'));
    if t.is_empty() || t.contains("://") {
        return false;
    }
    // Windows 盘符绝对路径（C:\... 或 C:/...）按外部处理
    let bytes = t.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return false;
    }
    let expanded = if let Some(rest) = t.strip_prefix("~/") {
        match std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
            Ok(home) => format!("{}/{}", home.trim_end_matches('/'), rest),
            Err(_) => return false, // 无法展开家目录 → 保守按外部处理（不施加机械判定）
        }
    } else {
        t.to_string()
    };
    let path = Path::new(&expanded);
    if path.is_relative() {
        return true;
    }
    path.starts_with(workspace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_target_classification() {
        let ws = Path::new("/repo/proj");
        // 工作区内
        assert!(is_workspace_target("src/main.rs", ws));
        assert!(is_workspace_target("./src/main.rs", ws));
        assert!(is_workspace_target("/repo/proj/src/main.rs", ws));
        assert!(is_workspace_target("/repo/proj", ws));
        // 工作区外：系统/运维路径
        assert!(!is_workspace_target("/usr/bin/workbuddy", ws));
        assert!(!is_workspace_target("/opt/WorkBuddy", ws));
        assert!(!is_workspace_target("/repo/other/x.rs", ws));
        assert!(!is_workspace_target("/repo", ws));
        // 非路径形态
        assert!(!is_workspace_target("https://api.example.com/v1", ws));
        assert!(!is_workspace_target("", ws));
        // HOME 与 workspace 无关时，~/ 一律外部
        assert!(!is_workspace_target("~/.config/app.conf", ws));
    }

    #[test]
    fn not_applicable_emits_handoff_notice() {
        assert!(GateApplicability::Applicable
            .handoff_notice("测试门")
            .is_none());
        let notice = GateApplicability::not_applicable("目标全在工作区外")
            .handoff_notice("零交集对账门")
            .expect("N/A 必须生成交接说明");
        assert!(notice.contains("[Gate Notice]"));
        assert!(notice.contains("零交集对账门"));
        assert!(notice.contains("目标全在工作区外"));
    }
}
