use serde::{Deserialize, Serialize};
use crate::agent::utils::evidence_refs::normalize_evidence_refs;

// 上限需容纳用户显式规格表（一行一义务，典型 8-10 行）；仍封顶防过度拆分。
const MAX_REVIEW_OBLIGATIONS: usize = 12;
const MAX_SCOPE_CHARS: usize = 500;
const MAX_OBLIGATION_ID_CHARS: usize = 64;
const MAX_OBLIGATION_TEXT_CHARS: usize = 360;
const MAX_WAIVER_REASON_CHARS: usize = 360;

/// 复杂审查、诊断或研究任务的可审计完成契约。
/// 普通任务保持 `None`，不会引入额外终态门槛。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewContract {
    /// 更新时省略则保留既有范围；初次创建仍须提供。
    #[serde(default)]
    pub scope_statement: String,
    pub obligations: Vec<ReviewObligation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewObligation {
    pub id: String,
    /// 已有义务更新时可省略；新增义务必须提供问题原文。
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub waived_reason: Option<String>,
    /// 义务绑定的可执行验证命令（契约播种：模型起草、方案审查批准后冻结）。
    /// 退出码 0 = 该义务客观成立；非 0 = 收尾门禁 blocker。None = 纯主观义务。
    #[serde(default)]
    pub verify_command: Option<String>,
    /// 批准时刻的验证脚本内容快照（冻结副本）。执行时宿主写临时文件运行此副本，
    /// 工作区内同名脚本被模型改动不影响判定——防"模型改自己的考卷"。
    #[serde(default)]
    pub verify_script: Option<String>,
    /// 冻结脚本内容的 SHA-256（执行前核验副本完整性）。
    #[serde(default)]
    pub verify_sha256: Option<String>,
    /// 义务的闭合通道声明（创建时必填）：
    /// - "command"：verify_command/verify_script 由宿主执行判分（客观义务首选）；
    /// - "evidence"：以 evidence_refs 证据闭合，终审语义核销（诊断/审查类）；
    /// - "human"：agent 沙盒内物理不可验证（真人 UI 操作等）——不得以 open 存在，
    ///   创建时即 waived + 理由披露，防止生成"永远闭不上"的义务空磨门禁。
    /// None = 旧契约兼容（既有义务不强制，新 open 义务必须声明）。
    #[serde(default)]
    pub closure_form: Option<String>,
}

impl ReviewContract {
    /// 检查契约中是否声明了任何可执行的验证命令或冻结验证脚本
    pub fn has_executable_verification(&self) -> bool {
        self.obligations.iter().any(|o| o.verify_command.is_some() || o.verify_script.is_some())
    }

    /// 按 id 原子合并申报更新：未提交的义务保留，不代表删除。
    /// evidenced/waived 是模型申报，不是宿主验收结果；允许补证、纠错或退回 open。
    /// 问题文本与已有验证命令仍保护，最终验收检查合并后的完整契约。
    pub fn apply_update(&mut self, update: ReviewContract) -> Result<(), String> {
        if update.obligations.is_empty() {
            return Err(
                "审查契约更新至少需要一项 obligations；未提交的既有义务会保留，不会删除。"
                    .to_string(),
            );
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut merged = self.clone();
        if !update.scope_statement.trim().is_empty() {
            merged.scope_statement = update.scope_statement;
        }
        for mut item in update.obligations {
            item.evidence_refs = normalize_evidence_refs(&item.evidence_refs)?;
            if !ids.insert(item.id.trim().to_string()) {
                return Err(format!(
                    "审查契约更新重复提交义务 {}；每个 id 只能出现一次。",
                    item.id
                ));
            }
            let existing = self.obligations.iter().find(|o| o.id == item.id);
            if let Some(e) = existing {
                if item.question.trim().is_empty() {
                    item.question = e.question.clone();
                }
                if item.question.trim() != e.question.trim() {
                    return Err(format!(
                        "审查契约义务 {} 的问题文本不能被静默改写；请新增新义务，或保留原问题并以 waived 说明范围变化。",
                        e.id
                    ));
                }
                // 省略冻结字段 = 保持原值，不要求模型每次重抄验证标准。
                if item.verify_command.is_none() {
                    item.verify_command = e.verify_command.clone();
                }
                if item.verify_script.is_none() {
                    item.verify_script = e.verify_script.clone();
                }
                if item.verify_sha256.is_none() {
                    item.verify_sha256 = e.verify_sha256.clone();
                }
                if item.closure_form.is_none() {
                    item.closure_form = e.closure_form.clone();
                }
                let path_only_change = e.verify_script == item.verify_script
                    && e.verify_sha256 == item.verify_sha256
                    && match (&e.verify_command, &item.verify_command) {
                        (Some(old), Some(new)) => same_command_except_paths(old, new),
                        _ => false,
                    };
                if (e.verify_command.is_some()
                    || e.verify_script.is_some()
                    || e.verify_sha256.is_some())
                    && !path_only_change
                    && (item.verify_command != e.verify_command
                        || item.verify_script != e.verify_script
                        || item.verify_sha256 != e.verify_sha256)
                {
                    return Err(format!(
                        "审查契约义务 {} 的验证命令/脚本已冻结，中途不得改写（仅把命令里的文件路径改成真实产物路径是允许的，其它部分需保持一致）；如需更换验证方式，请新增新义务并在方案审查中说明。",
                        e.id
                    ));
                }
            }
            validate_obligation_closure(&item, existing.is_none())?;
            if let Some(index) = merged.obligations.iter().position(|o| o.id == item.id) {
                merged.obligations[index] = item;
            } else {
                merged.obligations.push(item);
            }
        }
        // 校验合并后的总量与全部字段；任意一项失败，本次申报一项也不落账。
        validate_contract(&merged)?;
        *self = merged;
        Ok(())
    }

    pub fn open_descriptions(&self) -> Vec<String> {
        self.obligations
            .iter()
            .filter(|item| normalized_status(item).as_deref() == Some("open"))
            .map(|item| format!("审查义务 {}：{}", item.id, item.question.trim()))
            .collect()
    }

    /// 只列尚未申报的 open 项；其他项无需重抄，验收打回的已有项也可按 id 补正。
    pub fn closure_template(&self) -> Option<String> {
        if self.has_completion_claims() {
            return None;
        }
        let obligations: Vec<serde_json::Value> = self
            .obligations
            .iter()
            .filter(|o| normalized_status(o).as_deref() == Some("open"))
            .map(|o| {
                serde_json::json!({
                    "id": o.id, "status": "evidenced",
                    "evidence_refs": ["<真实工作区相对路径:起止行 或 step_N>"]
                })
            })
            .collect();
        serde_json::to_string(&serde_json::json!({
            "review_contract_update": { "obligations": obligations }
        }))
        .ok()
    }

    /// 仅表示每项都有完成/豁免申报，不代表证据、宿主验证或终审已经通过。
    pub fn has_completion_claims(&self) -> bool {
        self.open_descriptions().is_empty()
    }

}

/// 原子合并模型申报的局部或完整契约更新。校验失败时保留旧状态。
pub fn apply_review_contract_update(
    target: &mut Option<ReviewContract>,
    update: ReviewContract,
) -> Result<(), String> {
    let mut candidate = target.clone().unwrap_or(ReviewContract {
        scope_statement: String::new(),
        obligations: Vec::new(),
    });
    candidate.apply_update(update)?;
    *target = Some(candidate);
    Ok(())
}

fn validate_contract(contract: &ReviewContract) -> Result<(), String> {
    if contract.scope_statement.trim().is_empty() {
        return Err(
            "审查契约缺少 scope_statement；请明确本次审查/诊断覆盖的对象和边界。".to_string(),
        );
    }
    if contract.scope_statement.chars().count() > MAX_SCOPE_CHARS {
        return Err(format!(
            "审查契约 scope_statement 超过 {} 字符。",
            MAX_SCOPE_CHARS
        ));
    }
    if contract.obligations.is_empty() || contract.obligations.len() > MAX_REVIEW_OBLIGATIONS {
        return Err(format!(
            "审查契约 obligations 必须为 1–{} 项，避免空契约或过度拆分。",
            MAX_REVIEW_OBLIGATIONS
        ));
    }

    let mut ids = std::collections::BTreeSet::new();
    for item in &contract.obligations {
        if item.id.trim().is_empty() || item.id.chars().count() > MAX_OBLIGATION_ID_CHARS {
            return Err(format!(
                "审查义务 id 必须为 1–{} 个字符。",
                MAX_OBLIGATION_ID_CHARS
            ));
        }
        if !ids.insert(item.id.clone()) {
            return Err(format!("审查契约包含重复义务 id {}。", item.id));
        }
        if item.question.trim().is_empty()
            || item.question.chars().count() > MAX_OBLIGATION_TEXT_CHARS
        {
            return Err(format!(
                "审查义务 {} 的 question 必须为 1–{} 个字符。",
                item.id, MAX_OBLIGATION_TEXT_CHARS
            ));
        }

        match normalized_status(item).as_deref() {
            Some("open") => {
                if !item.evidence_refs.is_empty() || item.waived_reason.is_some() {
                    return Err(format!(
                        "审查义务 {} 标为 open 时不得同时声明证据或豁免理由。",
                        item.id
                    ));
                }
            }
            Some("evidenced") => {
                if item.evidence_refs.is_empty() {
                    return Err(format!(
                        "审查义务 {} 标为 evidenced 时至少需要一个 evidence_refs。",
                        item.id
                    ));
                }
                if item.waived_reason.is_some() {
                    return Err(format!(
                        "审查义务 {} 不能同时为 evidenced 和 waived。",
                        item.id
                    ));
                }
            }
            Some("waived") => {
                if item
                    .waived_reason
                    .as_deref()
                    .map(str::trim)
                    .filter(|reason| !reason.is_empty())
                    .is_none()
                {
                    return Err(format!(
                        "审查义务 {} 标为 waived 时必须说明 waived_reason。",
                        item.id
                    ));
                }
                if !item.evidence_refs.is_empty() {
                    return Err(format!(
                        "审查义务 {} 标为 waived 时不得同时声明 evidence_refs。",
                        item.id
                    ));
                }
            }
            _ => {
                return Err(format!(
                    "审查义务 {} 的 status 仅可为 open、evidenced 或 waived。",
                    item.id
                ));
            }
        }

        if item
            .waived_reason
            .as_deref()
            .is_some_and(|reason| reason.chars().count() > MAX_WAIVER_REASON_CHARS)
        {
            return Err(format!(
                "审查义务 {} 的 waived_reason 超过 {} 字符。",
                item.id, MAX_WAIVER_REASON_CHARS
            ));
        }
    }
    // 整份合并后的契约共用总预算，不按每个义务重新获得一份容量。
    let references: Vec<String> = contract.obligations.iter()
        .flat_map(|item| item.evidence_refs.iter().cloned()).collect();
    normalize_evidence_refs(&references)?;
    Ok(())
}

/// 义务闭合通道一致性校验（机械层，不问语义）：
/// - 所有声明了 closure_form 的义务：command→必须挂 verify_command/verify_script；
///   human→只能以 waived 存在（真人验证义务不可挂 open）；
/// - 新增 open 义务（id 不在既有契约中）：closure_form 必填，否则拒绝——
///   逼着模型在创建时想好"这条怎么闭合"，而不是 finish 时才发现闭不上。
///   既有 open 义务（旧契约/恢复）不追溯，兼容放行。
fn validate_obligation_closure(item: &ReviewObligation, is_new: bool) -> Result<(), String> {
    let status = item.status.trim().to_ascii_lowercase();
    if let Some(form) = item.closure_form.as_deref().map(str::trim) {
        match form {
            "command" => {
                if item.verify_command.is_none() && item.verify_script.is_none() {
                    return Err(format!(
                        "审查义务 {} 声明 command 闭合通道但未挂 verify_command/verify_script——客观义务请给出宿主可执行的验证命令（exit 0=成立）。",
                        item.id
                    ));
                }
            }
            "evidence" => {}
            "human" => {
                if status != "waived" {
                    return Err(format!(
                        "审查义务 {} 声明 human 闭合通道（agent 沙盒内物理不可验证）却标 {}——此类义务创建时只能以 waived + waived_reason 披露，或改写成机器可验证的等价形式。",
                        item.id, status
                    ));
                }
            }
            _ => {
                return Err(format!(
                    "审查义务 {} 的 closure_form 仅可为 command/evidence/human，收到 {:?}。",
                    item.id, form
                ));
            }
        }
        return Ok(());
    }
    if is_new && status == "open" {
        return Err(format!(
            "审查义务 {} 未声明闭合通道 closure_form——每项 open 义务创建时必须想好怎么验证：\n  * 可机器判定 → closure_form:\"command\" + verify_command（宿主执行判分，exit 0=成立）；\n  * 以证据闭合（文件行号/命令输出/step_N 观测）→ closure_form:\"evidence\"；\n  * 只能真人操作验证（UI 点击/真实设备）→ 不得以 open 创建，改写为机器可验证形式或以 waived + 理由披露。",
            item.id
        ));
    }
    Ok(())
}

fn normalized_status(item: &ReviewObligation) -> Option<String> {
    let status = item.status.trim().to_ascii_lowercase();
    (!status.is_empty()).then_some(status)
}

/// 两条命令是否「除文件路径 token 外完全一致」：按空白切 token，数量相同，逐个比较，
/// 不同的 token 必须两边都像文件路径（含 `/` 或以 `.扩展名` 结尾，且不含 shell 元字符、不以 `-` 开头）。
fn same_command_except_paths(old: &str, new: &str) -> bool {
    fn path_like(t: &str) -> bool {
        if t.is_empty() || t.starts_with('-') {
            return false;
        }
        if t.chars().any(|c| matches!(c, ';' | '|' | '&' | '$' | '`' | '<' | '>' | '(' | ')' | '*' | '?' | '\n')) {
            return false;
        }
        if t.contains('/') {
            return true;
        }
        match t.rsplit_once('.') {
            Some((stem, ext)) => {
                !stem.is_empty() && (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric())
            }
            None => false,
        }
    }
    let a: Vec<&str> = old.split_whitespace().collect();
    let b: Vec<&str> = new.split_whitespace().collect();
    if a.is_empty() || a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(x, y)| x == y || (path_like(x) && path_like(y)))
}

#[cfg(test)]
mod path_only_change_tests {
    use super::same_command_except_paths as same;

    #[test]
    fn only_paths_may_differ() {
        assert!(same(
            "ffprobe -v error -show_entries format=duration -i out/reel.mp4",
            "ffprobe -v error -show_entries format=duration -i out/history.mp4"
        ));
        // 骨架/参数变了不行
        assert!(!same("ffprobe -i a.mp4", "ffprobe -v quiet a.mp4"));
        assert!(!same("ffprobe -i a.mp4", "true -i a.mp4"));
        // 把路径换成带元字符的东西不行
        assert!(!same("ffprobe -i a.mp4", "ffprobe -i a.mp4;true"));
        assert!(!same("cargo test -p core", "cargo test -p other"));
        assert!(!same("", ""));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obligation(id: &str, question: &str, status: &str) -> ReviewObligation {
        ReviewObligation {
            verify_command: None,
            verify_script: None,
            verify_sha256: None,
            closure_form: if status == "open" {
                Some("evidence".to_string())
            } else {
                None
            },
            id: id.to_string(),
            question: question.to_string(),
            status: status.to_string(),
            evidence_refs: Vec::new(),
            waived_reason: None,
        }
    }

    fn contract(obligations: Vec<ReviewObligation>) -> ReviewContract {
        ReviewContract {
            scope_statement: "审查上下文引擎的实现与验证边界".to_string(),
            obligations,
        }
    }

    #[test]
    fn evidenced_and_waived_claims_still_require_evidence_or_reason() {
        let mut current = contract(vec![obligation("implementation", "调用链是否真实接通？", "open")]);
        let mut o = obligation("implementation", "调用链是否真实接通？", "evidenced");
        assert!(current.apply_update(contract(vec![o.clone()])).is_err());
        o.evidence_refs = vec!["main_loop.rs:695-905".into()];
        current.apply_update(contract(vec![o.clone()])).unwrap();
        o.status = "waived".into(); o.evidence_refs.clear();
        assert!(current.apply_update(contract(vec![o])).is_err());
    }

    #[test]
    fn completion_claim_can_be_reopened_without_deleting_the_obligation() {
        let mut o = obligation("scope", "范围是否明确？", "evidenced");
        o.evidence_refs = vec!["git diff --stat".into()];
        let mut current = contract(vec![o]);
        current.apply_update(contract(vec![obligation("scope", "范围是否明确？", "open")])).unwrap();
        assert_eq!(current.open_descriptions().len(), 1);
        assert!(!current.has_completion_claims());
        let saved = current.clone();
        assert!(current.apply_update(contract(vec![])).is_err());
        assert_eq!(current, saved);
    }

    #[test]
    fn open_descriptions_only_include_explicit_open_obligations() {
        let mut evidenced = obligation("evidence", "证据是否可回溯？", "evidenced");
        evidenced.evidence_refs = vec!["output:42".to_string()];
        let mut waived = obligation("optional", "可选前端范围是否读取？", "waived");
        waived.waived_reason = Some("用户限定后端审查".to_string());
        let review = contract(vec![
            obligation("scope", "审查范围是否明确？", "open"),
            evidenced,
            waived,
        ]);

        assert_eq!(
            review.open_descriptions(),
            vec!["审查义务 scope：审查范围是否明确？"]
        );
        assert!(!review.has_completion_claims());
    }

    #[test]
    fn new_open_obligations_must_declare_closure_channel() {
        // 未声明 closure_form 的新 open 义务 → 拒绝（e2e 空磨事故防线）
        let mut unverifiable = obligation("e2e", "粘贴/拖拽是否经真实验证？", "open");
        unverifiable.closure_form = None;
        assert!(ReviewContract {
            scope_statement: "s".into(),
            obligations: vec![]
        }
        .apply_update(contract(vec![unverifiable.clone()]))
        .is_err());

        // command 通道没挂 verify_* → 拒绝
        let mut bad_cmd = unverifiable.clone();
        bad_cmd.closure_form = Some("command".to_string());
        assert!(ReviewContract {
            scope_statement: "s".into(),
            obligations: vec![]
        }
        .apply_update(contract(vec![bad_cmd]))
        .is_err());

        // human 通道挂 open → 拒绝；waived+理由 → 放行
        let mut human_open = unverifiable.clone();
        human_open.closure_form = Some("human".to_string());
        assert!(ReviewContract {
            scope_statement: "s".into(),
            obligations: vec![]
        }
        .apply_update(contract(vec![human_open]))
        .is_err());
        let mut human_waived = unverifiable.clone();
        human_waived.closure_form = Some("human".to_string());
        human_waived.status = "waived".to_string();
        human_waived.waived_reason = Some("需真人 UI 操作，沙盒不可验证".to_string());
        assert!(ReviewContract {
            scope_statement: "s".into(),
            obligations: vec![]
        }
        .apply_update(contract(vec![human_waived]))
        .is_ok());

        // command + verify_command → 放行
        let mut good_cmd = unverifiable.clone();
        good_cmd.closure_form = Some("command".to_string());
        good_cmd.verify_command = Some("npm test -- upload".to_string());
        assert!(ReviewContract {
            scope_statement: "s".into(),
            obligations: vec![]
        }
        .apply_update(contract(vec![good_cmd]))
        .is_ok());

        // 既有 open 义务无 closure_form（旧契约兼容）→ 不追溯
        let mut existing = ReviewContract {
            scope_statement: "s".into(),
            obligations: vec![unverifiable.clone()],
        };
        let mut keep = unverifiable.clone();
        keep.evidence_refs = vec!["step_5".to_string()];
        keep.status = "evidenced".to_string();
        assert!(existing.apply_update(contract(vec![keep])).is_ok());
    }
}

#[cfg(test)]
mod omitted_frozen_fields_tests {
    use super::*;

    fn frozen() -> ReviewObligation {
        ReviewObligation {
            id: "verify_build".into(),
            question: "编译是否通过？".into(),
            status: "open".into(),
            evidence_refs: vec![],
            waived_reason: None,
            verify_command: Some("cargo check".into()),
            verify_script: Some("#!/bin/sh\ncargo check".into()),
            verify_sha256: Some("abc".into()),
            closure_form: Some("command".into()),
        }
    }

    #[test]
    fn update_omitting_frozen_fields_keeps_them() {
        let mut c = ReviewContract { scope_statement: "s".into(), obligations: vec![frozen()] };
        let mut o = frozen();
        o.verify_command = None;
        o.verify_script = None;
        o.verify_sha256 = None;
        o.closure_form = None;
        o.status = "evidenced".into();
        o.evidence_refs = vec!["step_3".into()];
        let upd = ReviewContract { scope_statement: "s".into(), obligations: vec![o] };
        c.apply_update(upd).expect("省略冻结字段应视为保持原值");
        assert_eq!(c.obligations[0].verify_command.as_deref(), Some("cargo check"));
        assert_eq!(c.obligations[0].verify_sha256.as_deref(), Some("abc"));
        assert_eq!(c.obligations[0].status, "evidenced");
    }

    #[test]
    fn explicit_different_command_still_rejected() {
        let mut c = ReviewContract { scope_statement: "s".into(), obligations: vec![frozen()] };
        let mut o = frozen();
        o.verify_command = Some("true".into());
        let upd = ReviewContract { scope_statement: "s".into(), obligations: vec![o] };
        assert!(c.apply_update(upd).is_err());
    }

    #[test]
    fn template_lists_open_claims_and_none_when_all_claimed() {
        let c = ReviewContract { scope_statement: "s".into(), obligations: vec![frozen()] };
        let t = c.closure_template().unwrap();
        assert!(t.contains("verify_build") && t.contains("evidenced") && t.contains("<真实工作区相对路径:起止行"));
        let mut done = frozen();
        done.status = "waived".into();
        done.waived_reason = Some("n/a".into());
        let c2 = ReviewContract { scope_statement: "s".into(), obligations: vec![done] };
        assert!(c2.closure_template().is_none());
    }
}


#[cfg(test)]
mod claim_merge_tests {
    use super::*;
    use serde_json::{json, Value};

    fn contract(v: Value) -> ReviewContract {
        serde_json::from_value(v).unwrap()
    }
    fn current() -> ReviewContract {
        contract(json!({"scope_statement":"登录与发布包验收", "obligations":[
            {"id":"a","question":"登录拒绝","status":"evidenced","evidence_refs":["old.java:89-93"]},
            {"id":"b","question":"正常登录","status":"evidenced","evidence_refs":["step_2"]},
            {"id":"c","question":"上传身份","status":"waived","waived_reason":"用户限定范围"},
            {"id":"d","question":"发布包","status":"open","closure_form":"evidence"},
            {"id":"e","question":"回归","status":"open","closure_form":"command","verify_command":"cargo test"}
        ]}))
    }

    #[test]
    fn only_two_of_five_items_are_resubmitted_and_other_three_are_preserved() {
        let mut c = current();
        let kept = c.obligations[..3].to_vec();
        c.apply_update(contract(json!({"obligations":[
            {"id":"d","status":"evidenced","evidence_refs":["step_10"]},
            {"id":"e","status":"evidenced","evidence_refs":["step_11"]}
        ]})))
        .unwrap();
        assert_eq!(c.obligations.len(), 5);
        assert_eq!(&c.obligations[..3], kept.as_slice());
        assert!(c.has_completion_claims());
        assert_eq!(
            c.obligations[4].verify_command.as_deref(),
            Some("cargo test")
        );
        assert!(c.closure_template().is_none());
    }

    #[test]
    fn submitted_evidence_and_waiver_can_be_corrected_or_reopened() {
        let mut c = current();
        c.apply_update(contract(json!({"obligations":[
            {"id":"a","status":"evidenced","evidence_refs":["src/AuthService.java:89-93"]},
            {"id":"c","status":"waived","waived_reason":"用户明确不验收此项"}
        ]})))
        .unwrap();
        assert_eq!(
            c.obligations[0].evidence_refs,
            vec!["src/AuthService.java:89-93"]
        );
        assert_eq!(
            c.obligations[2].waived_reason.as_deref(),
            Some("用户明确不验收此项")
        );
        c.apply_update(contract(json!({"obligations":[
            {"id":"a","status":"open"},{"id":"c","status":"open"}
        ]})))
        .unwrap();
        assert_eq!(c.open_descriptions().len(), 4);
        assert!(c.obligations[0].evidence_refs.is_empty());
        assert!(c.obligations[2].waived_reason.is_none());
        let t = c.closure_template().unwrap();
        assert!(!t.contains("正常登录"));
        assert!(!t.contains("scope_statement") && !t.contains("verify_command"));
    }

    #[test]
    fn frozen_script_only_stays_protected_while_claim_evidence_can_change() {
        let mut c = current();
        c.obligations[0].verify_script = Some("exit 1".into());
        c.obligations[0].verify_sha256 = Some("original-script-hash".into());
        c.apply_update(contract(json!({"obligations":[{
            "id":"a","status":"evidenced","evidence_refs":["step_20"]
        }]})))
        .unwrap();
        let saved = c.clone();
        assert!(c
            .apply_update(contract(json!({"obligations":[{
                "id":"a","status":"evidenced","evidence_refs":["step_21"],"verify_script":"exit 0"
            }]})))
            .unwrap_err()
            .contains("已冻结"));
        assert_eq!(c, saved);
        // 原有完整更新仍兼容，原样重提不使契约指纹变化。
        let fp = crate::agent::finalization::contract_fingerprint(Some(&c));
        c.apply_update(c.clone()).unwrap();
        assert_eq!(
            fp,
            crate::agent::finalization::contract_fingerprint(Some(&c))
        );
    }

    #[test]
    fn rejected_partial_update_is_atomic_and_does_not_erase_omitted_items() {
        let mut target = Some(current());
        let saved = target.clone();
        for items in [
            json!([{"id":"a","status":"open"},{"id":"b","question":"偷换原问题","status":"open"}]),
            json!([{"id":"a","status":"open"},{"id":"a","status":"open"}]),
            json!([{"id":"a","status":"evidenced","evidence_refs":[]}]),
            json!([{"id":"e","status":"open","verify_command":"echo passed"}]),
            json!([]),
        ] {
            assert!(apply_review_contract_update(
                &mut target,
                contract(json!({"obligations":items}))
            )
            .is_err());
            assert_eq!(target, saved);
        }
    }

    #[test]
    fn creation_and_merged_total_still_obey_full_contract_validation() {
        let mut target = None;
        assert!(apply_review_contract_update(
            &mut target,
            contract(json!({"obligations":[
                {"id":"new","status":"evidenced","evidence_refs":["step_1"]}
            ]}))
        )
        .is_err());
        assert!(target.is_none());
        let items: Vec<Value> = (0..12).map(|i| json!({"id":format!("o{i}"),"question":"核验", "status":"open","closure_form":"evidence"})).collect();
        let mut c = contract(json!({"scope_statement":"s", "obligations":items}));
        let saved = c.clone();
        assert!(c
            .apply_update(contract(json!({"obligations":[{
                "id":"o12","question":"额外项","status":"open","closure_form":"evidence"
            }]})))
            .is_err());
        assert_eq!(c, saved);
    }
}
