//! 静态提示词与内置工具菜单组装。
//!
//! 从 main_loop 抽出的纯组装逻辑：核心系统提示词加载、task_store 数据区说明、
//! 内置本地工具菜单扫描。输出与原内联实现字节级一致，便于前缀缓存。

use tauri::AppHandle;
use tauri::Manager;

/// 动态构建内置工具紧凑列表字符串（用于决策提示词尾部索引，避免硬编码不同步）
pub fn build_builtin_tools_compact_list() -> String {
    let mut names = BUILTIN_TOOL_SPECS
        .iter()
        .map(|s| s.name)
        .collect::<Vec<_>>();
    // 补齐项目受管资源工具别名（当存在项目时）
    if !names.contains(&"project_resources") {
        names.push("project_resources");
    }
    names.join(", ")
}

/// 加载核心系统提示词：优先资源目录，逐级回退到源码目录，最后用编译期嵌入副本。
pub fn load_base_prompt(app: &AppHandle) -> String {
    let core_logic_embedded = include_str!("../../prompts/core_logic.md");
    let prompt_path = app
        .path()
        .resource_dir()
        .unwrap_or_default()
        .join("prompts/core_logic.md");
    std::fs::read_to_string(&prompt_path)
        .or_else(|_| std::fs::read_to_string("src-tauri/prompts/core_logic.md"))
        .or_else(|_| std::fs::read_to_string("prompts/core_logic.md"))
        .unwrap_or_else(|_| core_logic_embedded.to_string())
}

#[derive(Clone, Copy)]
pub(crate) struct BuiltinToolSpec {
    pub(crate) name: &'static str,
    pub(crate) summary: &'static str,
    pub(crate) actions: &'static [&'static str],
}

/// 能力索引（capability_search）索引内置工具用的只读视图。
pub(crate) fn builtin_tool_specs() -> &'static [BuiltinToolSpec] {
    BUILTIN_TOOL_SPECS
}

/// 内置工具的首轮最小可执行契约。
///
/// 它只承载稳定的工具名、动作、必填字段与确认条件；长范例、罕见边界
/// 和外部 MCP schema 仍由 tool_manual 按需加载。真实 handler 继续负责最终校验。
const BUILTIN_TOOL_SPECS: &[BuiltinToolSpec] = &[
    BuiltinToolSpec {
        name: "fail_task",
        summary: "无法继续时明确失败停止，不是成功交付：单独输出 {\"fail_task\":{\"reason\":\"具体失败原因\"}}。宿主保留已有改动/证据/TODO/审查要求并停止后台任务；无需先完成或豁免义务。不要附带写入或把未完成任务包装为 finish。",
        actions: &["fail_task(reason: non-empty string)"],
    },
    BuiltinToolSpec {
        name: "finish",
        summary: "终态交付：工作完成后优先直接调用 finish，宿主会执行与 prepare_finish 相同的适用验收，复杂任务同样适用；直接交付不表示跳过验证或审查。summary 为简短结论，较长说明放在 command.details；两者正文均支持 Markdown，字符串按 JSON 规则转义。代码类任务先完成适用验证并处理已有待办与审查问题；文件产物路径放进 command.artifacts（支持 tmp/ 会话产物路径）。【直接交付示例】已取得服务状态查询结果时：{\"tool\":\"finish\",\"command\":{\"summary\":\"已确认目标服务正在运行\",\"details\":\"状态查询返回 active；该结论仅覆盖本次服务状态查询。\"}}。示例内容应替换为本次实际结果。【可选预检】需要先查看尚未满足的验收条件时，可单独调用 prepare_finish，并一次提交准备交付的完整 summary 与 details；就绪后沿用同一份正文调用 finish，材料与验收状态未变时复用收据。正文补充或修改后需要重新核验；未就绪时按具体反馈补正后可直接 finish。",
        actions: &["finish(summary: string, details?: string, artifacts?: string[])"],
    },
    BuiltinToolSpec {
        name: "ask_human",
        summary: "请求人类输入、选择或确认；form支持 single_select|multi_select|confirm|text|file_picker|custom_html",
        actions: &["ask_human(message: string, form?: {kind, title, options?: [{id, label}]})"],
    },
    BuiltinToolSpec {
        name: "get_step_output",
        summary: "分页取回已有工具输出的原文",
        actions: &["get_step_output(output_id: string, branch_id?: string [批量回执中的原始分支 ID，选中后 offset 在分支内计数], offset?: number, limit?: number, raw?: boolean)"],
    },
    BuiltinToolSpec {
        name: "capability_search",
        summary: "统一能力检索：按动作关键词搜索时，少量 MCP 候选直接带完整 input_schema，可立即调用；精确填写插件名/工具名返回该工具完整规格。query 只填插件名则分页浏览真实动作目录；has_more 时沿 next_request 继续。已有名称和足够参数时可直接使用，不要求先查询；缺规格的候选可用 load_request 精确加载。next_tool_hint 是可选的指令顶层预加载字段，不放进 command。",
        actions: &[
            "search(query: string [动作关键词或插件名或完整工具 ID], kind?: builtin|mcp|skill|instruction|role, namespace?: string [插件名/目录名，定向搜索可用], limit?: <=40 [默认动作搜索 5、插件目录 40], offset?: number)",
        ],
    },
    BuiltinToolSpec {
        name: "search",
        summary: "代码与历史定位工具：search 用于定位；需要理解完整代码结构时，建议按 file:line 使用 local_file.read 读取目标区块，仍在定位其他符号时可继续搜索",
        actions: &["search(query: string, kinds?: history|code|all, path?: string [kinds 含 code 时必填，缺省会被拒], ...) — 单文件命中带 ±3 行；同文件自上次成功读取后第 6 次非缓存检索起附读取建议，不暂停或拒绝搜索"],
    },
    BuiltinToolSpec {
        name: "task_store",
        summary: "会话内结构化数据写入、分页读取与状态更新",
        actions: &[
            "create_resource(kind: string, label: string)",
            "upsert_records(resource_id: string, records: array)",
            "list_resources()",
            "get_resource(resource_id: string)",
            "list_records(resource_id: string, limit?: <=100, offset?: number)",
            "get_record(resource_id: string, record_key: string)",
            "get_work_group(group_id: string [检索返回的 episode ID 或 parent_id], offset?: number, limit?: <=25) — 分页查看工作目录内的尝试/观察及 output_id；标签不代表完成",
            "update_resource(resource_id: string, label?|summary?|status?|cursor?|schema?)",
            "evidence_record(purpose: string, subject?: string, command?: string, output_id?: string, file_path?: string, reported_outcome?: string, tags?: string[], note?: string, idempotency_key?: string) — 可选登记，每次默认新观察；同一显式键重试返回首次记录，新观察换键或省略。文件快照由宿主在登记时采集，不证明原始验证对应同一版本；无文件的测试同样支持",
            "evidence_find(query?: string, subject?: string, command?: string, limit?: <=30) — 测试/测验/检验等表述可召回候选，匹配不等于通过",
            "evidence_inspect(evidence_id: string) — 读取完整结构化记录，原始回执用 get_step_output",
            "evidence_compare(evidence_id: string) — 对原登记文件重新采样对比；非文件返回 not_applicable，由 Agent 自行结合历史判断",
        ],
    },
    BuiltinToolSpec {
        name: "child_reviewer",
        summary: "受限只读子调查：审查父证据包，回交带 ID/严重级别/物理行号的结构化 findings 与 routing（certain/needs_alignment/blocked）；收到结果后必须先分诊再行动：逐条判定接受/反驳(须附证据)/需澄清，不要直接照改，也不要直接忽略；不会调用工具或写入文件；支持单发或 dispatches 批量并发派发多个专家会审",
        actions: &[
            "investigate(question: string, role?: string [挂载专精角色卡标识], evidence_refs?: string[] [去重后合计 <=16384 字符，含分隔；无固定条数或单条长度限制], evidence_excerpt?: string <=6000 chars, resource_ids?: string[] <=2 [仅当前项目会话的受管资源；不得传路径] 【⚠️证据三选一必填：refs/excerpt/resource_ids 至少供给一项（通常为你刚读过的文件路径列表），本次调用未提供三者中的任一项会被拒】, custom_inputs?: object [专项业务参数；角色卡声明必填项必须供给；verified_facts/hard_constraints/definition_of_done 分节呈现，字段名不认证来源；入口分流另以可修正的 triage_reference 附带；注意 plan_summary 不算证据], dispatches?: Array<{question, role?, evidence_refs|evidence_excerpt|resource_ids 三选一必填, ...}> [一步批量并发派发多个审查员会审])",
        ],
    },
    BuiltinToolSpec {
        name: "child_writer",
        summary: "L3 隔离写入 worker：宿主把工作区复制成私有副本，worker 在副本内修改代码并自行运行你指定的测试命令，回交逐文件 diff 与测试证据；你审阅后可调用 apply 让宿主把补丁原样机械合入真实工作区（任一文件指纹冲突即整体拒绝，绝不覆盖），也可自行用常规写工具落实；支持单发或 dispatches 批量派发",
        actions: &[
            "dispatch(task: string, test_command: string [单条，如 npm test], focus_paths?: string[] <=5, role?: string, context_excerpts?: {background?: string, todo_slice?: string[], file_hints?: string[], prior_findings?: string[]}, custom_inputs?: object [专项业务参数；verified_facts/hard_constraints/definition_of_done 分节呈现，字段名不认证来源；入口分流另以可修正的 triage_reference 附带], dispatches?: Array<{task, test_command, ...}> [一步批量派发多个 writer])",
            "apply(child_run_id: string) [审阅 diff 后机械合入：宿主逐文件现算冲突指纹，全部干净才搬运，任何冲突整体拒绝；副本已回收时自动退化到补丁工件合入]",
        ],
    },
    BuiltinToolSpec {
        name: "child_worker",
        summary: "L2 只读侦察 worker：独立上下文中自行读文件/列目录/执行只读命令完成侦察，回交结构化结论；异步返回 job_id，结果经 wait_jobs 或信箱回收。批量派发直接用 {\"tool\":\"child_worker\",\"dispatches\":[{\"task\":\"调查模块A的调用方\",\"focus_paths\":[\"src/a\"]},{\"task\":\"调查模块B的输入来源\",\"focus_paths\":[\"src/b\"]}]}，替换为实际自包含任务；不要放进同步 parallel 执行树。何时用：互不依赖的多路径调查适合一步批量派发；侦察兵默认使用 Flash 档，主 Agent 负责核对证据与决策",
        actions: &[
            "dispatch(task: string [自包含侦察任务], focus_paths?: string[] <=3 [工作区内路径], role?: string, context_excerpts?: {background?: string, todo_slice?: string[], file_hints?: string[], prior_findings?: string[]}, custom_inputs?: object [专项业务参数；verified_facts/hard_constraints/definition_of_done 分节呈现，字段名不认证来源；入口分流另以可修正的 triage_reference 附带], dispatches?: Array<{task, focus_paths?, role?, ...}> [一步批量派发多个 worker])",
        ],
    },
    BuiltinToolSpec {
        name: "wait_jobs",
        summary: "挂起等待已登记的后台命令或子任务；等待本身不调用模型，完成或中断即唤醒。默认窗口 180s，同批连续等待最多扩到 600s；窗口结束不等于任务失败，继续等待复用 job_id，不要重启任务",
        actions: &["wait_jobs(job_ids?: string[], timeout_secs?: number)"],
    },
    BuiltinToolSpec {
        name: "job_cancel",
        summary: "主动中止仍在运行的后台 shell 任务（local_shell background:true 派发的）：按 pid 发 TERM→KILL 双阶段杀进程组；判定任务卡死（日志长期无进展）、跑偏或不再需要时使用",
        actions: &["job_cancel(job_id: string)"],
    },
    BuiltinToolSpec {
        name: "resume_child",
        summary: "追问/对质已完成的子任务：继承其脱水对话历史（writer 额外继承工作区副本），以半量步数预算继续攻坚；对 reviewer 的追问用于反驳指定 finding ID 并要求其 withdraw(撤回)/uphold(坚持)/revise(修正)；需提供 child_run_id 与追问 message",
        actions: &[
            "dispatch(child_run_id: string, message: string, test_command?: string [writer 类可重设])",
        ],
    },
    BuiltinToolSpec {
        name: "role_manager",
        summary: "专精角色池维护：角色卡=人设提示词+钉死模型+作用域+默认测试命令；派发子任务时传 role 参数即挂载",
        actions: &[
            "list()",
            "upsert(name: string, label?: string, model_id?: string, persona_prompt?: string, scope_hints?: string, default_test_command?: string)",
            "toggle(name: string)",
            "delete(name: string)",
        ],
    },
    BuiltinToolSpec {
        name: "watch_start",
        summary: "持续监听：周期巡检工作区内文件或命令输出，仅在检测到变化时把事件自动注入上下文；finish 前必须释放全部监听",
        actions: &[
            "watch_start(kind: \"file\"|\"command\", target: string, interval_secs?: 5–600)",
        ],
    },
    BuiltinToolSpec {
        name: "watch_stop",
        summary: "停止持续监听；省略 watch_id 时停止当前运行的全部监听",
        actions: &["watch_stop(watch_id?: string)"],
    },
    BuiltinToolSpec {
        name: "web_search",
        summary: "全网检索并返回带来源的结果",
        actions: &["web_search(query: string, max_results?: <=10)"],
    },
    BuiltinToolSpec {
        name: "web_fetch",
        summary: "提取网页正文为 Markdown",
        actions: &["web_fetch(url: string)"],
    },
    BuiltinToolSpec {
        name: "model_manager",
        summary: "维护模型规格、提供商连接与平台模型绑定",
        actions: &[
            "list_specs()",
            "upsert_spec(name: string, context_window?|max_tokens?|supports_thinking?|default_temperature?|description?)",
            "delete_spec(name|id: string, confirm: true)",
            "list_platforms()",
            "add_platform(name: string, base_url: string, api_key?: string, api_keys?: array) [新增提供商；成功后才能 add_model；支持多Key]",
            "update_platform(platform_id|platform_name: string, name?|base_url?|api_key?|api_keys?) [更新提供商配置或多Key列表]",
            "add_model(platform_id|platform_name: string, model_name: string, ...) [仅已有平台；不得传 base_url/api_key]",
            "delete_model(model_id|model_name: string, confirm: true)",
        ],
    },
    BuiltinToolSpec {
        name: "mcp_manager",
        summary: "接入、启停、批量导入和删除外部 MCP 插件",
        actions: &[
            "list()",
            "add_http(url: string, bearer_token?|token?|name?|transport?|model_id?)",
            "add_stdio(name: string, command: string, args?|env?|capability_card?)",
            "batch_import(input_text: string, model_id?)",
            "toggle(name: string, enabled: boolean)",
            "delete(name: string, confirm: true)",
        ],
    },
    BuiltinToolSpec {
        name: "skill_manager",
        summary: "管理自定义 Skill 与提示词；可从 Git 仓库/本地目录自助安装技能资产包",
        actions: &[
            "list()",
            "add(name: string, content: string, kind?|description?|model_id?)",
            "install(url: string, name?: string) —— clone/复制技能仓库到技能目录并自动登记可路由卡片（探测 SKILL.md，无则回结构清单）",
            "delete(id|name: string, confirm: true)",
        ],
    },
    BuiltinToolSpec {
        name: "schedule_manager",
        summary: "定时任务管理：创建/维护按时间自动派发的计划任务，到点自动运行并把结果推送到主人微信",
        actions: &[
            "list()",
            "save(name: string, goal: string, kind: \"daily\"|\"interval\", time?: \"HH:MM\", minutes?: number, project_id?: string, id?: string) [id 省略=新建]",
            "toggle(id: string, enabled: boolean)",
            "delete(id: string, confirm: true)",
            "run_now(id: string) [立即执行一次，不等下一触发点]",
        ],
    },
    BuiltinToolSpec {
        name: "project_manager",
        summary: "项目（工作区白名单）管理：create/delete 会改变可访问文件范围，必须先向用户确认再传 confirm: true",
        actions: &[
            "list()",
            "create(name: string, root_path: string, confirm: true)",
            "delete(project_id: string, confirm: true)",
            "list_resources(project_id: string)",
            "add_resource(project_id: string, path: string, label?: string)",
        ],
    },
    BuiltinToolSpec {
        name: "system_info",
        summary: "本机运行状态：主机/系统、负载、内存、磁盘余量（回答机器状态类问题优先用本工具而非 shell）",
        actions: &["overview()"],
    },
    BuiltinToolSpec {
        name: "wechat_manager",
        summary: "微信通道：查看桥在线/登录状态与最近任务流水；notify 可在任务执行途中主动给主人微信捎话（非阻塞知会，需要用户决定仍用 ask_human）",
        actions: &[
            "status()",
            "notify(text: string)",
            "recent_runs(limit?: <=50)",
        ],
    },
    BuiltinToolSpec {
        name: "project_resources",
        summary: "仅项目会话可用：查看或读取受管文件，支持将项目内关键文件登记为跨会话共享受管资源",
        actions: &[
            "list_resources(offset?: number, limit?: <=200)",
            "read_resource(resource_id: string)",
            "register_resource(path: string, label?: string)",
        ],
    },
    BuiltinToolSpec {
        name: "artifact_manager",
        summary: "生成独立受管交付产物（Markdown分析报告、网页看板、数据导出）并登记持久化账本供用户预览/导出；省略 action 默认 create_document，登记已有文件须显式指定 action=register_file",
        actions: &[
            "create_document(title: string, content: string, format: md|html|json|csv|txt)",
            "register_file(path: string) — 将已生成的产物文件登记入产物账本（会话 tmp/ 目录与工作区内文件均可，tmp/ 为推荐落点；如 Excel/PDF），任务完成时自动回推给用户",
            "list_artifacts()",
            "read_artifact(artifact_id: string)",
        ],
    },
    BuiltinToolSpec {
        name: "local_file",
        summary: "本地文件读写与精准局部替换（受双白名单保护与SHA256乐观并发锁防覆盖保护）：修改已有文件优先使用 edit 局部替换。注：tmp/ 相对路径自动路由到会话专属临时草稿目录",
        actions: &[
            "read(path: string, start_line?: number, end_line?: number) — 读取文件并建立并发哈希基线。≤300 行可整读；大文件先 search 定位一次，再按 file:line 锚点 ±15 行读取完整目标区块。成功读取会重置该文件的检索建议计数；省略 action 默认 read",
            "write(path: string, content: string, force?: boolean) — 全量写入文件。⚠️【前置门禁】调用前必须已完成 child_reviewer 审查且获取 ReviewScorecard，否则系统底层无条件拦截丢弃！",
            "edit(path: string, target_content: string, replacement_content: string, force?: boolean) — 精准局部替换。⚠️【前置门禁】调用前必须已完成 child_reviewer 审查且获取 ReviewScorecard，否则系统底层无条件拦截丢弃！",
            "list(path?: string, depth?: number)",
        ],
    },
    BuiltinToolSpec {
        name: "local_shell",
        summary: "本地工作区终端命令执行工具：执行构建、测试、代码检查与环境探针（受工作区物理边界与超时保护，工具链PATH自动补齐）。⚠️【工具排他约束】严禁挪用为文件阅读器：读取文件必须用 local_file(action: \"read\")，代码检索必须用 search",
        actions: &[
            "调用格式优先 {\"tool\":\"local_shell\",\"params\":{\"command\":\"cargo check\",\"cwd\":\".\"}}。兼容旧 command 参数对象及非空命令字符串简写；简写只整理参数形状，仍走同一权限/目录/审查检查，不自动添加 cwd；附加选项放 params，两个来源冲突时不执行并报具体字段",
            "exec(command: string, args?: string[], cwd?: string, timeout_secs?: number [默认 60s，同步等待窗口上限 300s——到点不杀，自动转后台 job 交你裁决], env?: object, background?: boolean) — 在项目工作区执行 shell 命令行（支持参数数组与自定义环境变量；已注入 $SESSION_TMP_DIR/$TMPDIR=会话产物物理根）。预估超过 5 分钟的任务（视频渲染/大构建/批处理）传 background:true 或 timeout_secs>300 直接后台执行——立即返回 job_id，输出实时写日志文件，wait_jobs 挂起回收 / job_cancel 中止；同步命令跑满 300s 未结束也会自动转为后台任务（不杀进程），由你决定续等还是中止",
            "退出码仅代表外层进程。包装脚本需保留关键检查退出码，例如 sh check.sh >check.log 2>&1; rc=$?; tail -n 40 check.log; exit \"$rc\"。管道、分号、|| true、后台启动或最后的 echo/tail 可能掩盖内部失败；构建与重启建议拆成独立调用，先确认构建结果再重启。set -e 对 &&/|| 条件列表并非全覆盖。success 仅指进程退出，verification_status=not_assessed 不表示编译或业务通过；远端脚本也应传回退出码",
            "⚠️【产物落点】shell 内写交付产物一律用 \"$SESSION_TMP_DIR/文件名\" 落点——裸相对路径 tmp/x 会落在当前执行目录下的 tmp/，既不是会话产物区也不随会话清理（系统会检测并提醒）；tmp/ 前缀路由只在 local_file/register_file 等工具参数中有效",
            "⚠️【基线提示】读取文件建议用 local_file(action: \"read\")——shell 读文件不建立乐观并发哈希基线，后续 write/edit 该既有文件会因缺基线被硬拦截",
        ],
    },
    BuiltinToolSpec {
        name: "view_image",
        summary: "把图片文件拉进你的下一次调用（视觉信道）：截图/渲染帧/图表/原型图生成后想看内容时用；是否查看、看哪张由你判断，非强制。图片仅当轮可见、不落历史——看到的关键信息写进步骤文本，需再看就重新调用。支持 png/jpg/jpeg/webp/gif/bmp，≤8MB；视频不能直看——自行 ffmpeg 抽帧（contact sheet 最省）后看图",
        actions: &["view_image(path: string) — 图片仅注入下一次 LLM 调用；本地白名单（工作区 ∪ 会话 tmp）内路径"],
    },
];

/// 从稳定的内置契约渲染首轮工具目录。
///
/// 目录不再解析 Markdown，因此手册格式和资源目录可用性不会改变可执行工具集。
pub fn build_local_tools_menu() -> String {
    let mut local_tools_menu = String::from("\n## 内置本地工具 (Built-in Tools)\n");
    for tool in BUILTIN_TOOL_SPECS {
        local_tools_menu.push_str(&format!("- {}: {}\n", tool.name, tool.summary));
        for action in tool.actions {
            local_tools_menu.push_str(&format!("  - {}\n", action));
        }
    }
    local_tools_menu
}

#[cfg(test)]
mod tests {
    use super::{build_local_tools_menu, BUILTIN_TOOL_SPECS};

    #[test]
    fn builtin_catalog_exposes_first_turn_required_fields_and_confirmation() {
        let menu = build_local_tools_menu();
        assert!(menu.contains("add(name: string, content: string"));
        assert!(menu.contains("delete(id|name: string, confirm: true)"));
        assert!(menu.contains("upsert_records(resource_id: string, records: array)"));
        assert!(menu.contains("get_step_output(output_id: string"));
        assert!(menu.contains("read_resource(resource_id: string)"));
        assert!(menu.contains("add_platform(name: string, base_url: string"));
        assert!(menu.contains("update_platform(platform_id|platform_name: string"));
        assert!(menu.contains(
            "add_model(platform_id|platform_name: string, model_name: string, ...) [仅已有平台"
        ));
    }

    #[test]
    fn direct_finish_example_is_an_executable_instruction_with_versioned_body() {
        use crate::agent::{checkpoint::AgentCheckpoint, context::SandwichContext,
            finalization::{acceptance_fingerprint, delivery_body}, types::AgentInstruction};
        let spec = BUILTIN_TOOL_SPECS.iter().find(|s| s.name == "finish").unwrap();
        let start = spec.summary.find('{').expect("交付示例缺失");
        let value = serde_json::Deserializer::from_str(&spec.summary[start..])
            .into_iter::<serde_json::Value>().next().unwrap().unwrap();
        let inst: AgentInstruction = serde_json::from_value(value).unwrap();
        assert!(crate::agent::dispatcher::is_finish_instruction(&inst));
        assert!(inst.prepare_finish.is_none());
        let mut payload = inst.command.unwrap();
        let body = delivery_body(&payload).unwrap();
        assert!(body.contains(payload["summary"].as_str().unwrap()));
        assert!(body.contains(payload["details"].as_str().unwrap()));
        let c = SandwichContext::new("system".into(), "查询服务状态".into());
        let cp = AgentCheckpoint::new("example-run".into(), &c);
        let fingerprint = acceptance_fingerprint(&c, &cp, &payload);
        payload["details"] = serde_json::json!("服务状态尚未核实");
        assert_ne!(fingerprint, acceptance_fingerprint(&c, &cp, &payload));
    }

    #[test]
    fn every_builtin_contract_has_a_summary_and_action() {
        assert!(BUILTIN_TOOL_SPECS.iter().all(|tool| {
            !tool.name.is_empty() && !tool.summary.is_empty() && !tool.actions.is_empty()
        }));
    }

    #[test]
    fn register_file_description_names_tmp_as_valid_drop() {
        // path_guard 认 tmp/ 路由，工具文案必须同步说清——上次漏写导致模型
        // 照"工作区"字面意思把会话产物拷进项目目录。
        let menu = build_local_tools_menu();
        let line = menu
            .lines()
            .find(|l| l.contains("register_file(path: string)"))
            .expect("register_file action 缺失");
        assert!(
            line.contains("tmp/"),
            "register_file 描述未提 tmp/：与 path_guard 路由/finish 对账脱节"
        );
    }

    #[test]
    fn search_and_read_actions_document_nonblocking_advisory() {
        let menu = build_local_tools_menu();
        assert!(menu.contains("第 6 次非缓存检索"));
        assert!(menu.contains("不暂停或拒绝搜索"));
        assert!(menu.contains("成功读取会重置该文件的检索建议计数"));
        assert!(!menu.contains("连续 3 次真实检索未 read"));
    }

    #[test]
    fn local_shell_description_names_session_tmp_env() {
        // shell 里裸相对 tmp/ 会落到执行目录而非会话产物区（实测把订单 xlsx
        // 写进了 src-tauri/tmp 差点提交进库）——SESSION_TMP_DIR 环境变量早已注入
        // 但工具文案从未提及，模型无从知晓。此断言防文案与机制再次脱节。
        let menu = build_local_tools_menu();
        let line = menu
            .lines()
            .find(|l| l.contains("产物落点"))
            .expect("local_shell 描述未说明 SESSION_TMP_DIR 产物落点约定");
        assert!(line.contains("$SESSION_TMP_DIR"), "落点说明应给出 SESSION_TMP_DIR 写法");
        assert!(line.contains("tmp/"), "落点说明应同时讲清 tmp/ 前缀的适用层");
    }
}

#[cfg(test)]
mod shell_params_prompt_tests {
    #[test]
    fn shell_params_menu_explains_the_canonical_container_and_compatibility() {
        let menu=super::build_local_tools_menu();
        assert!(menu.contains(r#"{"tool":"local_shell","params":{"command":"cargo check","cwd":"."}}"#));
        assert!(menu.contains("非空命令字符串简写") && menu.contains("不自动添加 cwd"));
        assert!(menu.contains("verification_status=not_assessed"));
        assert!(menu.contains("构建与重启建议拆成独立调用"));
        let core=include_str!("../../prompts/core_logic.md");
        assert!(core.contains(r#"{"tool":"local_shell","params":{"command":"cargo check","cwd":"."}}"#));
    }
}
