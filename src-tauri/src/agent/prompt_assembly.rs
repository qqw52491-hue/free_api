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
        name: "finish",
        summary: "终态交付；全流程彻底完成的收尾交卷指令。【前置硬约束】：严禁提前交付！若本任务涉及代码修改，必须先完成真实测试验证或确认审查意见已闭合，确保无遗留 blocking 缺陷或未闭合待办；未经验收直接草率 finish 会被系统机械门禁当场拦截打回！若生成或调整了文件交付物，建议在 artifacts 中附带交付物路径列表（`tmp/xxx` 会话产物路径即可，交付门禁直接识别，严禁为迎合门禁把产物拷入项目/当前目录），系统会自动在界面底部上屏生成下载与打开卡片。【可选预检】：正式交付前可单独一步输出 {\"prepare_finish\": {\"summary\": \"...\"}} 跑同一套门禁做就绪评估——只沉淀收据、不提交终态；就绪收据在代码未变时被 finish 直接复用，免重复验证与终审",
        actions: &["finish(summary: string, artifacts?: string[])"],
    },
    BuiltinToolSpec {
        name: "ask_human",
        summary: "请求人类输入、选择或确认；form支持 single_select|multi_select|confirm|text|file_picker|custom_html",
        actions: &["ask_human(message: string, form?: {kind, title, options?: [{id, label}]})"],
    },
    BuiltinToolSpec {
        name: "get_step_output",
        summary: "分页取回已有工具输出的原文",
        actions: &["get_step_output(output_id: string, offset?: number, limit?: number)"],
    },
    BuiltinToolSpec {
        name: "capability_search",
        summary: "统一能力检索：在全部内置工具、MCP 工具、Skill、自定义指令与角色卡中按关键词找候选——当目录被截断或你不确定确切工具/Skill 名时先搜这里，拿到 id 后用 next_tool_hint 加载完整手册或 Schema",
        actions: &[
            "search(query: string, kind?: builtin|mcp|skill|instruction|role, namespace?: string [插件名/目录名], limit?: <=15)",
        ],
    },
    BuiltinToolSpec {
        name: "search",
        summary: "代码与历史定位工具：search 只回答“在哪”；取得 file:line 后必须转 local_file.read 读取完整目标区块，不能靠换关键词拼接代码",
        actions: &["search(query: string, kinds?: history|code|all, path?: string [kinds 含 code 时必填，缺省会被拒], ...) — 单文件命中带 ±3 行；连续 3 次真实检索未 read 时暂停该文件搜索"],
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
            "update_resource(resource_id: string, label?|summary?|status?|cursor?|schema?)",
        ],
    },
    BuiltinToolSpec {
        name: "child_reviewer",
        summary: "受限只读子调查：审查父证据包，回交带 ID/严重级别/物理行号的结构化 findings 与 routing（certain/needs_alignment/blocked）；收到结果后必须先分诊再行动：逐条判定接受/反驳(须附证据)/需澄清，不要直接照改，也不要直接忽略；不会调用工具或写入文件；支持单发或 dispatches 批量并发派发多个专家会审",
        actions: &[
            "investigate(question: string, role?: string [挂载专精角色卡标识], evidence_refs?: string[] <=6, evidence_excerpt?: string <=6000 chars, resource_ids?: string[] <=2 [仅当前项目会话的受管资源；不得传路径] 【⚠️证据三选一必填：refs/excerpt/resource_ids 至少供给一项（通常为你刚读过的文件路径列表），空手派发会被拒】, custom_inputs?: object [专项业务参数；角色卡声明必填项必须供给；保留键 verified_facts/hard_constraints/definition_of_done 会升格为一级小节优先呈现；注意 plan_summary 不算证据], dispatches?: Array<{question, role?, evidence_refs|evidence_excerpt|resource_ids 三选一必填, ...}> [一步批量并发派发多个审查员会审])",
        ],
    },
    BuiltinToolSpec {
        name: "child_writer",
        summary: "L3 隔离写入 worker：宿主把工作区复制成私有副本，worker 在副本内修改代码并自行运行你指定的测试命令，回交逐文件 diff 与测试证据；你审阅后可调用 apply 让宿主把补丁原样机械合入真实工作区（任一文件指纹冲突即整体拒绝，绝不覆盖），也可自行用常规写工具落实；支持单发或 dispatches 批量派发",
        actions: &[
            "dispatch(task: string, test_command: string [单条，如 npm test], focus_paths?: string[] <=5, role?: string, context_excerpts?: {background?: string, todo_slice?: string[], file_hints?: string[], prior_findings?: string[]}, custom_inputs?: object [专项业务参数；保留键 verified_facts/hard_constraints/definition_of_done 升格一级小节], dispatches?: Array<{task, test_command, ...}> [一步批量派发多个 writer])",
            "apply(child_run_id: string) [审阅 diff 后机械合入：宿主逐文件现算冲突指纹，全部干净才搬运，任何冲突整体拒绝；副本已回收时自动退化到补丁工件合入]",
        ],
    },
    BuiltinToolSpec {
        name: "child_worker",
        summary: "L2 只读侦察 worker：独立上下文中自行读文件/列目录/执行只读命令完成侦察，回交结构化结论；支持单发或 dispatches 批量派发；结果经 wait_jobs 或信箱自动回收。何时用：探索面横跨多模块、或预计需多步串行检索才能定位时，优先 dispatches 一步批量扇出多个侦察兵分头排查，比主循环逐步搜索更省步数",
        actions: &[
            "dispatch(task: string [自包含侦察任务], focus_paths?: string[] <=3 [工作区内路径], role?: string, context_excerpts?: {background?: string, todo_slice?: string[], file_hints?: string[], prior_findings?: string[]}, custom_inputs?: object [专项业务参数；保留键 verified_facts/hard_constraints/definition_of_done 升格一级小节], dispatches?: Array<{task, focus_paths?, role?, ...}> [一步批量派发多个 worker])",
        ],
    },
    BuiltinToolSpec {
        name: "wait_jobs",
        summary: "挂起等待后台异步任务或子审查完成；在无其他独立前置操作时使用，不消耗大模型 Token",
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
        summary: "生成独立受管交付产物（Markdown分析报告、网页看板、数据导出）并登记持久化账本供用户预览/导出",
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
            "read(path: string, start_line?: number, end_line?: number) — 读取文件并建立并发哈希基线。≤300 行可整读；大文件先 search 定位一次，再按 file:line 锚点 ±15 行读取完整目标区块。成功读取会解除该文件的搜索暂停",
            "write(path: string, content: string, force?: boolean) — 全量写入文件。⚠️【前置门禁】调用前必须已完成 child_reviewer 审查且获取 ReviewScorecard，否则系统底层无条件拦截丢弃！",
            "edit(path: string, target_content: string, replacement_content: string, force?: boolean) — 精准局部替换。⚠️【前置门禁】调用前必须已完成 child_reviewer 审查且获取 ReviewScorecard，否则系统底层无条件拦截丢弃！",
            "list(path?: string, depth?: number)",
        ],
    },
    BuiltinToolSpec {
        name: "local_shell",
        summary: "本地工作区终端命令执行工具：执行构建、测试、代码检查与环境探针（受工作区物理边界与超时保护，工具链PATH自动补齐）。⚠️【工具排他约束】严禁挪用为文件阅读器：读取文件必须用 local_file(action: \"read\")，代码检索必须用 search",
        actions: &[
            "exec(command: string, args?: string[], cwd?: string, timeout_secs?: number [默认 60s，同步等待窗口上限 300s——到点不杀，自动转后台 job 交你裁决], env?: object, background?: boolean) — 在项目工作区执行 shell 命令行（支持参数数组与自定义环境变量；已注入 $SESSION_TMP_DIR/$TMPDIR=会话产物物理根）。预估超过 5 分钟的任务（视频渲染/大构建/批处理）传 background:true 或 timeout_secs>300 直接后台执行——立即返回 job_id，输出实时写日志文件，wait_jobs 挂起回收 / job_cancel 中止；同步命令跑满 300s 未结束也会自动转为后台任务（不杀进程），由你决定续等还是中止",
            "⚠️【产物落点】shell 内写交付产物一律用 \"$SESSION_TMP_DIR/文件名\" 落点——裸相对路径 tmp/x 会落在当前执行目录下的 tmp/，既不是会话产物区也不随会话清理（系统会检测并提醒）；tmp/ 前缀路由只在 local_file/register_file 等工具参数中有效",
            "⚠️【基线提示】读取文件建议用 local_file(action: \"read\")——shell 读文件不建立乐观并发哈希基线，后续 write/edit 该既有文件会因缺基线被硬拦截",
        ],
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
    fn search_and_read_actions_document_search_read_gate() {
        let menu = build_local_tools_menu();
        assert!(
            menu.contains("连续 3 次真实检索未 read"),
            "search 动作描述应声明同文件 3 次未读即暂停的硬闸"
        );
        assert!(
            menu.contains("成功读取会解除该文件的搜索暂停"),
            "read 动作描述应声明成功读取解除搜索暂停"
        );
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
