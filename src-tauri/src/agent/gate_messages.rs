//! 前置写入门禁的拦截文案集中地。
//!
//! 拦截文案是模型行为协议的一部分：它们直接决定被拦截后的 LLM 下一轮动作，
//! 因此统一放在这里做版本化管理，避免大段中文协议字符串散落在门禁判定逻辑中。
//! 修改任何一条文案后，请同步检查 `loop_runner::check_pre_write_gatekeeper` 的
//! route 判定与相关测试是否仍然成立。

/// 方案审批模式：直接写代码 / 派发写入子 Agent / 执行变更工具被拦截
pub fn plan_approval_need_human_approval() -> String {
    "【🚨 前置方案审批门禁拦截：当前处于方案审批模式】\n检测到当前尚未获得用户的人工方案审批放行，禁止直接调用写文件、派发写入子 Agent 或执行变更工具。\n请先调用 ask_human 工具（设置 kind: 'plan_approval'），将经过审查推演的完整技术方案呈现给用户；\n待用户在交互界面上审阅并点击【批准方案】后，门禁将自动放行修改。本次操作已被安全拦截，目标未受影响。".to_string()
}

/// 方案审批模式：通过 resume_child 恢复写入型子 Agent 被拦截
pub fn plan_approval_resume_child_blocked() -> String {
    "【🚨 前置方案审批门禁拦截：当前处于方案审批模式】\n检测到尝试通过 resume_child 恢复写入型子 Agent 继续修改代码，该路径同样需要人工方案审批放行。\n请先调用 ask_human 工具（设置 kind: 'plan_approval'），将经过审查推演的完整技术方案呈现给用户；\n待用户在交互界面上审阅并点击【批准方案】后，门禁将自动放行修改。本次恢复已被安全拦截，目标文件未受影响。".to_string()
}

/// 交叉审查模式：尚未派发 child_reviewer
pub fn cross_review_need_reviewer_dispatch() -> String {
    "【🚨 交叉审查门禁拦截：尚未派发子 Agent 对抗审查】\n当前处于【🔍 双 Agent 交叉审查模式】。在首次动笔修改代码前，必须先派发 child_reviewer 开展反驳推演与架构盲区排查：\n1. 调用 child_reviewer 工具，传入拟定的方案摘录、证据文件路径（如关键核心逻辑与跨模块关联点）及反证推演要求；\n2. 随后调用 wait_jobs 等待审查结论；\n3. 充分研判审查反驳意见后方可写入。\n【建议】对高风险或大范围变更，可并行派发两个使用不同角色卡（不同模型家族）的 child_reviewer，先独立审查再交叉比对两份结论——异构视角的盲区互补收益最大。本次操作已被安全拦截，目标未受影响。".to_string()
}

/// 交叉审查模式：已派发但结论尚未回收（“问过 ≠ 听过”）
pub fn cross_review_need_wait_reviewer_jobs() -> String {
    "【🚨 交叉审查门禁拦截：审查子 Agent 仍在异步推演中】\n检测到你已派发审查任务，但尚未等待并接收其审查结论（“问过 ≠ 听过”）。\n请立即调用 wait_jobs 工具（例如 wait_jobs() 或指定 job_ids）挂起等待审查专家完成对抗推演；\n审查结论注入上下文后，门禁将根据交叉审查结果放行写入。本次操作已被安全拦截，目标未受影响。".to_string()
}

/// 交叉审查模式：派发从未真正启动（无运行中任务、无可回收结论）
pub fn cross_review_reviewer_result_missing() -> String {
    "【🚨 交叉审查门禁拦截：未收到任何有效的审查结论】\n检测到此前已派发过审查任务，但后台当前没有正在运行的审查子任务，也没有可回收的审查结论（通常是派发立即失败：角色卡未授权挂载为 reviewer、role 名称不存在或参数非法）。\n请重新调用 child_reviewer 派发审查（先确认 role 名称可用且与任务匹配），或调用 wait_jobs 收取可能已完成的结论；\n收到审查结论后方可写入。本次操作已被安全拦截，目标未受影响。".to_string()
}

/// 交叉审查模式：ReviewScorecard 评分未达硬门禁
pub fn cross_review_scorecard_failed(reason: &str) -> String {
    format!("【🚨 交叉审查硬门禁拦截：审查评分未达标】\n审查子 Agent 给出的 ReviewScorecard 评分未达放行标准：\n{}\n硬门禁规则：综合平均分必须 >= 75 分，且破坏性风险/安全性评分 (risk) 必须 >= 60 分。\n系统已自动拒绝放行代码写入。请结合审查发现的问题打回重新规划与修复后再尝试写入。", reason)
}

/// 全自动模式：首轮未完成只读排查与方案阐述即尝试写入
pub fn auto_mode_need_plan() -> String {
    "【🚨 前置方案门禁拦截：尚未确立经论证的技术方案】\n检测到在首轮未完成只读排查与方案阐述前尝试直接调用了写代码工具。\n为保证工程契约安全，请先遵循【假设驱动与主动证伪法则】：\n1. 优先通过只读工具（如 read_file/grep_search）排查代码并排除错误假设，锁定唯一根因；\n2. 输出清晰的技术方案与反证推演说明；\n方案确认后再执行修改。本次写入已被安全拦截，目标文件未受影响。".to_string()
}

/// 交叉审查模式：审查结论缺少评分卡（fail-closed，绝不默认放行）
pub fn cross_review_scorecard_missing(budget_exhausted: bool) -> String {
    let escape = if budget_exhausted {
        "审查轮次预算已耗尽，无法再次派发：请调用 ask_human 工具（kind: 'plan_approval'）将当前方案与已有审查结论呈报用户，由人工审批放行。"
    } else {
        "请重新调用 child_reviewer 派发审查，并确保输出包含 scorecard 四维评分；或直接调用 ask_human（kind: 'plan_approval'）走人工审批。"
    };
    format!("【🚨 交叉审查硬门禁拦截：审查结论缺少评分卡】\n检测到审查子任务的结论中没有 ReviewScorecard 评分卡，硬门禁无法核定放行条件，本次写入已被安全拦截（缺失评分时绝不默认放行）。\n{}目标未受影响。", escape)
}

/// 交叉审查模式：评分达标但仍有未解决的 blocking 级异议
pub fn cross_review_blocking_findings_open(count: u32) -> String {
    format!("【🚨 交叉审查硬门禁拦截：存在未解决的 blocking 级异议】\n审查评分虽已达标，但最新一轮审查仍有 {} 条 blocking 级异议未解决。\n硬门禁规则：blocking 异议必须逐条回应并修复，或经再次审查确认已解决后，方可放行写入。\n请针对每条 blocking 异议给出处理说明（修复 / 证伪 / 设计取舍理由），然后重新派发审查确认。\n【重要】重新派发 child_reviewer 时，必须在证据包中逐条附上：原始异议（含编号，如 F1）、你的处理说明与对应证据。审查者将据此对每条异议裁决 maintain 或 withdraw，全部撤回或解决后方可放行。", count)
}

/// 方案审查轮次预算耗尽的统一提示（常规派发与反驳对质共用）
pub fn reviewer_retry_limit() -> String {
    "【审查轮数已达上限（2 轮）】方案审查讨论已达最大重试上限，请停止继续派发审查子 Agent。\n请综合已有审查建议，整理最终技术方案并调用 ask_human（kind: 'plan_approval'）呈报给用户进行人工审批。".to_string()
}

/// 阶段级审查：上一阶段的 blocking_regardless 异议未解决，拦截继续写入。
/// 解套路径 = resume_child 对质（审查者撤回后自动放行）；修复本身不需要先写入。
pub fn stage_review_blocking_open(count: u32, stage_run_id: &str, final_only_summary: &str) -> String {
    let mut text = format!(
        "【🚨 阶段审查拦截：上一阶段存在 {} 条未解决的 blocking_regardless 异议】\n阶段审查发现无论后续阶段如何都成立的结构性缺陷，继续堆叠新写入只会放大返工成本，本次写入已被拦截。\n【解套路径（唯一）】调用 resume_child（child_run_id: \"{}\"），逐条提交处理说明与证据：问题确已解决或经证伪不成立时审查者会撤回（withdraw），全部撤回后自动放行；仍成立的请先按审查建议修正方案。\n修复后的代码由下一轮阶段审查复检。\n",
        count, stage_run_id
    );
    if !final_only_summary.is_empty() {
        text.push_str(&format!("\n【final_only 记账（不拦截，但最终交付前必须解决）】\n{}\n", final_only_summary));
    }
    text
}
