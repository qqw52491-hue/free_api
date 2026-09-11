//! 前置写入门禁的拦截文案集中地。
//!
//! 拦截文案是模型行为协议的一部分：它们直接决定被拦截后的 LLM 下一轮动作，
//! 因此统一放在这里做版本化管理，避免大段中文协议字符串散落在门禁判定逻辑中。
//! 修改任何一条文案后，请同步检查 `loop_runner::check_pre_write_gatekeeper` 的
//! route 判定与相关测试是否仍然成立。

/// 方案审批模式：直接写代码 / 派发写入子 Agent / 执行变更工具被拦截
pub fn plan_approval_need_human_approval() -> String {
    "【⚠️ 门禁保护触发：写入已被拦截，目标文件未做任何修改！】\n\
     【原因】当前处于【方案审批模式】，尚未获得人工方案审批放行，禁止直接写文件、派发写入子 Agent 或执行变更工具。\n\
     【必须执行的操作】：\n\
     请调用 ask_human 工具（设置 kind: 'plan_approval'），将经过审查推演的技术方案呈现给用户；\n\
     待用户在交互界面审阅并批准方案后，门禁将自动放行修改。严禁未获批准直接假装完成或草率结项！".to_string()
}

/// 方案审批模式：通过 resume_child 恢复写入型子 Agent 被拦截
pub fn plan_approval_resume_child_blocked() -> String {
    "【⚠️ 门禁保护触发：恢复子任务已被拦截，目标文件未做任何修改！】\n\
     【原因】当前处于【方案审批模式】，尝试通过 resume_child 恢复写入型子 Agent 修改代码同样需要人工方案审批放行。\n\
     【必须执行的操作】：\n\
     请调用 ask_human 工具（设置 kind: 'plan_approval'），将经过审查推演的技术方案呈现给用户；\n\
     待用户在交互界面审阅并批准方案后，门禁将自动放行修改。严禁未获批准直接假装完成或草率结项！".to_string()
}

/// 交叉审查模式：尚未派发 child_reviewer
pub fn cross_review_need_reviewer_dispatch() -> String {
    "【⚠️ 门禁保护触发：已阻止未审查写入，目标文件未受任何修改！】\n\
     【原因】当前处于【双 Agent 交叉审查模式】。在首次动笔修改代码前，必须先派发 child_reviewer 开展反驳推演与架构盲区排查。\n\
     【必须执行的操作】：\n\
     1. 调用 child_reviewer 工具，传入 role（如文档任务使用 doc_writer，前端使用 frontend_ui_designer 等）、拟定方案摘录、证据文件路径及反证推演要求；\n\
     2. 随后调用 wait_jobs 等待审查结论；\n\
     3. 充分研判审查反驳意见并闭合后方可写入。\n\
     严禁跳过审查直接贴代码或结项！".to_string()
}

/// 交叉审查模式：已派发但结论尚未回收（“问过 ≠ 听过”）
pub fn cross_review_need_wait_reviewer_jobs() -> String {
    "【⚠️ 门禁保护触发：已阻止未审查写入，目标文件未受任何修改！】\n\
     【原因】审查子 Agent 仍在异步推演中，尚未等待并接收其审查结论（“问过 ≠ 听过”）。\n\
     【必须执行的操作】：\n\
     请立即调用 wait_jobs 工具挂起等待审查专家完成对抗推演；\n\
     审查结论注入上下文后，门禁将根据交叉审查结果放行写入。严禁未回收结论强行结项！".to_string()
}

/// 交叉审查模式：派发从未真正启动（无运行中任务、无可回收结论）
pub fn cross_review_reviewer_result_missing() -> String {
    "【⚠️ 门禁保护触发：已阻止未审查写入，目标文件未受任何修改！】\n\
     【原因】未收到任何有效的审查结论（后台既无运行中审查任务，亦无可回收结论；通常因角色卡未授权、role 名称不存在或参数非法导致派发失败）。\n\
     【必须执行的操作】：\n\
     请重新调用 child_reviewer 派发有效审查（核对 role 名称），或调用 wait_jobs 收取可能已完成的结论；\n\
     收到审查结论后方可写入。严禁直接假装成功！".to_string()
}

/// 交叉审查模式：ReviewScorecard 评分未达硬门禁
pub fn cross_review_scorecard_failed(reason: &str, budget_exhausted: bool) -> String {
    let escape_guide = if budget_exhausted {
        "【审查预算已耗尽】方案审查已达最大重试轮次，无法再次派发审查子 Agent。\n\
         【解套路径（人工仲裁放行）】严禁因审查受阻而直接放弃写入或草率 finish！请立即调用 ask_human 工具（设置 kind: 'plan_approval'），在 message 中向用户说明技术方案与审查员的低分质疑，阐述为何该方案安全或确属任务必需，申请人工审批：\n\
         ```json\n\
         {\n  \"tool\": \"ask_human\",\n  \"command\": {\n    \"kind\": \"plan_approval\",\n    \"message\": \"技术方案说明与审批申请：...（说明方案及安全性，申请人工审批放行）\"\n  }\n}\n\
         ```\n\
         用户在界面确认放行后，前置门禁将立即解除并允许写入！"
    } else {
        "【解套路径 1（方案修正）】若审查员指出的破坏性风险与漏洞属实，请修正方案后重新派发 child_reviewer 审查。\n\
         【解套路径 2（人工仲裁放行）】若经核实审查员意见存在过度防御、偏执假设或与当前功能目标冲突，且方案整体可控，请调用 ask_human 工具（设置 kind: 'plan_approval'），将方案分歧呈报用户申请人工裁决：\n\
         ```json\n\
         {\n  \"tool\": \"ask_human\",\n  \"command\": {\n    \"kind\": \"plan_approval\",\n    \"message\": \"技术方案与审查分歧呈报：...（说明方案及安全性，申请人工审批放行）\"\n  }\n}\n\
         ```\n\
         用户批准后门禁立即放行写入，严禁放弃写入直接在对话中贴代码！"
    };
    format!(
        "【⚠️ 门禁保护触发：代码写入已被拦截并丢弃，目标文件未受任何修改！】\n\
         【原因】审查评分未达标（要求：综合平均分 >= 75 分，且破坏性风险/安全性评分 risk >= 60 分）：\n\
         {}\n\
         【必须采取的行动（严禁假装成功或草率 finish）】：\n\
         {}",
        reason, escape_guide
    )
}

/// 全自动模式：首轮未完成只读排查与方案阐述即尝试写入
pub fn auto_mode_need_plan() -> String {
    "【⚠️ 门禁保护触发：写入已被拦截，目标文件未受任何修改！】\n\
     【原因】首轮未完成只读排查与方案阐述前尝试直接调用了写代码工具。\n\
     【必须执行的操作】：\n\
     1. 优先通过只读工具（如 read_file/grep_search）排查代码并排除错误假设，锁定唯一根因；\n\
     2. 输出清晰的技术方案与反证推演说明；\n\
     方案确认后再执行修改。严禁盲目写入或未写代码假装完成！".to_string()
}

/// 交叉审查模式：审查结论缺少评分卡（fail-closed，绝不默认放行）
pub fn cross_review_scorecard_missing(budget_exhausted: bool) -> String {
    let escape = if budget_exhausted {
        "审查轮次预算已耗尽，无法再次派发：请调用 ask_human 工具（kind: 'plan_approval'）将当前方案与已有审查结论呈报用户，由人工审批放行。"
    } else {
        "请重新调用 child_reviewer 派发审查，并确保输出包含 scorecard 四维评分；或直接调用 ask_human（kind: 'plan_approval'）走人工审批。"
    };
    format!(
        "【⚠️ 门禁保护触发：已阻止无 Scorecard 写入，目标文件未受任何修改！】\n\
         【原因】审查结论中缺少 ReviewScorecard 评分卡，硬门禁无法核定放行条件（缺失评分时绝不默认放行）。\n\
         【必须执行的操作】：\n\
         {}\n\
         严禁直接假装成功或草率结项！",
        escape
    )
}

/// 交叉审查模式：并发审查任务尚未齐套（齐套屏障拦截）
pub fn cross_review_batch_incomplete(delivered: usize, expected: usize) -> String {
    format!(
        "【⚠️ 门禁保护触发：已阻止未齐套写入，目标文件未受任何修改！】\n\
         【原因】当前处于【多审查员并发审查模式】。本批次派发了 {} 个审查任务，当前仅回传 {} 个，尚有审查专家在后台推演中（齐套屏障保护）。\n\
         【必须执行的操作】：\n\
         请调用 wait_jobs 工具挂起等待剩余 {} 个审查结论回传；\n\
         所有并发审查员全部交付并完成现场合流判定后，方可评估放行。严禁在审查未齐套时抢跑写入！",
        expected, delivered, expected.saturating_sub(delivered)
    )
}

/// 交叉审查模式：评分达标但仍有未解决的 blocking 级异议（附带具体缺陷清单）
pub fn cross_review_blocking_findings_with_details(
    count: u32,
    details: &str,
    budget_exhausted: bool,
) -> String {
    let escape_guide = if budget_exhausted {
        "【审查预算已耗尽】方案审查已达最大重试上限。\n\
         【解套路径（人工仲裁放行）】请调用 ask_human 工具（设置 kind: 'plan_approval'），将尚未闭合的 blocking 异议及你的解释呈报用户，由用户进行人工裁决：\n\
         ```json\n\
         {\n  \"tool\": \"ask_human\",\n  \"command\": {\n    \"kind\": \"plan_approval\",\n    \"message\": \"异议仲裁申请：...（向用户说明未闭合的 blocking 异议及为何方案安全或必要）\"\n  }\n}\n\
         ```\n\
         用户批准后门禁将立即解除并放行写入。"
    } else {
        "【解套路径 1（重审确认）】针对每条 blocking 异议给出处理说明（修复 / 证伪 / 设计取舍理由），重新派发 child_reviewer 确认。\n\
         【解套路径 2（人工仲裁放行）】若审查员坚持维持异议（maintain）但确属过度防御或业务特有设计，请调用 ask_human 工具（设置 kind: 'plan_approval'）将分歧与方案呈报用户，由人工裁决强制放行：\n\
         ```json\n\
         {\n  \"tool\": \"ask_human\",\n  \"command\": {\n    \"kind\": \"plan_approval\",\n    \"message\": \"方案与异议分歧呈报：...（申请人工裁决放行）\"\n  }\n}\n\
         ```"
    };

    let details_section = if details.trim().is_empty() {
        String::new()
    } else {
        format!("\n【🚨 待修复 Blocking 致命缺陷清单】：\n{}\n", details.trim())
    };

    format!(
        "【❌ 执行失败：代码写入已被拦截并丢弃，目标文件未做任何修改！】\n\
         【原因】审查评分虽已达标，但最新一轮审查仍有 {} 条 blocking 级异议未解决（硬门禁规则：blocking 异议必须逐条回应并修复，或经再次审查确认已解决后方可放行）。\n\
         {}\
         【必须采取的行动（严禁假装已修改或草率 finish）】：\n\
         {}",
        count, details_section, escape_guide
    )
}

/// 交叉审查模式：评分达标但仍有未解决的 blocking 级异议（简版兼容）
pub fn cross_review_blocking_findings_open(count: u32, budget_exhausted: bool) -> String {
    cross_review_blocking_findings_with_details(count, "", budget_exhausted)
}

/// 方案审查轮次预算耗尽的统一提示（常规派发与反驳对质共用）
pub fn reviewer_retry_limit() -> String {
    "【❌ 操作受限：方案审查轮数已达上限（2 轮）】\n\
     禁止继续派发审查子 Agent。\n\
     【必须执行的操作】：\n\
     请综合已有审查建议，整理最终技术方案并调用 ask_human（kind: 'plan_approval'）呈报给用户进行人工审批。\n\
     用户在界面批准后，前置门禁将立即解除并放行代码写入。".to_string()
}

/// 阶段级审查：上一阶段的 blocking_regardless 异议未解决，拦截继续写入。
/// 解套路径 = resume_child 对质（审查者撤回后自动放行），或 ask_human 人工仲裁放行。
pub fn stage_review_blocking_open(count: u32, stage_run_id: &str, final_only_summary: &str) -> String {
    let mut text = format!(
        "【❌ 执行失败：写入已被拦截，目标文件未做任何修改！】\n\
         【原因】上一阶段存在 {} 条未解决的 blocking_regardless 结构性缺陷，继续堆叠新写入只会放大返工成本。\n\
         【必须执行的操作】：\n\
         - 路径 1（对质撤回）：调用 resume_child（child_run_id: \"{}\"），逐条提交处理说明与证据；问题确已解决或经证伪不成立时审查者会撤回（withdraw），全部撤回后自动放行；\n\
         - 路径 2（人工仲裁）：若与审查者存在无法弥合的分歧，请调用 ask_human 工具向用户呈报阶段审查争议详情，申请人工裁决强制放行。\n",
        count, stage_run_id
    );
    if !final_only_summary.is_empty() {
        text.push_str(&format!("\n【final_only 记账（不拦截，但最终交付前必须解决）】\n{}\n", final_only_summary));
    }
    text
}

/// 方案审查未通过且未发生任何写入时尝试 finish 逃避代码写入的拦截提示
pub fn finish_blocked_by_unapproved_plan(reason: &str) -> String {
    format!(
        "【⚠️ 交付门禁拦截：方案未通过，finish 未生效！】\n\
         【原因】当前任务方案审查尚未通过（{}），且工作区内尚未完成任何代码写入。\n\
         严禁在未获放行的情况下擅自将代码仅以文本形式贴出并直接结束任务！\n\
         【必须采取的行动】：\n\
         1. 若审查意见属实，请修正方案并重新审查；\n\
         2. 若审查员存在过度防御阻碍了开发，请立即调用 ask_human 工具（设置 kind: 'plan_approval'）呈报用户申请人工裁决放行；\n\
         获得用户批准放行后，请真实调用写入工具写入文件并验证通过，方可提交 finish！",
        reason
    )
}
