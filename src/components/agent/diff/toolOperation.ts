import type { AgentStepItem } from "../types";
import { cleanFilePath, stepFingerprint } from "./diffTypes";
import { computeLineDiffStats } from "./diffCore";

export interface StepOperationDisplay {
    badgeIcon: string;
    badgeLabel: string;
    actionText: string;
    filePath?: string;
    fileName?: string;
    lineRange?: string;
    hasDiff: boolean;
    diffStat?: { additions: number; deletions: number };
}

const formatOpMemo = new WeakMap<object, { fp: string; val: StepOperationDisplay }>();

/**
 * 将步骤调用的原始参数提取为结构化、极具可读性的操作意图描述 (例如 "AgentProgressDeck.vue :1-100")
 */
export function formatStepOperation(step: AgentStepItem): StepOperationDisplay {
    if (step && typeof step === "object") {
        const fp = stepFingerprint(step);
        const hit = formatOpMemo.get(step);
        if (hit && hit.fp === fp) return hit.val;
        const val = computeFormatStepOperation(step);
        try { formatOpMemo.set(step, { fp, val }); } catch {}
        return val;
    }
    return computeFormatStepOperation(step);
}

export function computeFormatStepOperation(step: AgentStepItem): StepOperationDisplay {
    const rawTool = (step.tool || step.action || "").trim();
    let p: any = step.params || step.rawParams;
    if (typeof p === "string") {
        try { p = JSON.parse(p); } catch { p = {}; }
    }
    if (p && typeof p === "object") {
        if (p.command && typeof p.command === "object") p = { ...p, ...p.command };
        if (p.input && typeof p.input === "object") p = { ...p, ...p.input };
        if (p.arguments && typeof p.arguments === "object") p = { ...p, ...p.arguments };
    } else {
        p = {};
    }

    const rawFilePath = String(
        p.path || p.filePath || p.file_path || p.file || p.TargetFile || p.target_file || p.targetFile || p.fileName || p.filename || ""
    );
    const filePath = cleanFilePath(rawFilePath);
    const fileName = filePath ? (filePath.split("/").pop() || filePath) : "";

    // 提取行号（统一格式化为优雅的 IDE #L 格式，例如 #L1-80, #L1610-1645）
    let lineRange = "";
    if (p.line_start !== undefined && p.line_end !== undefined) {
        lineRange = `#L${p.line_start}-${p.line_end}`;
    } else if (p.start_line !== undefined && p.end_line !== undefined) {
        lineRange = `#L${p.start_line}-${p.end_line}`;
    } else if (p.StartLine !== undefined) {
        lineRange = p.EndLine ? `#L${p.StartLine}-${p.EndLine}` : `#L${p.StartLine}`;
    } else if (p.head !== undefined) {
        lineRange = `#L1-${p.head}`;
    } else if (p.tail !== undefined) {
        lineRange = `tail ${p.tail}`;
    } else if (p.lines !== undefined) {
        lineRange = `#L${p.lines}`;
    } else if (p.offset !== undefined && p.limit !== undefined) {
        lineRange = `#L${p.offset}-${Number(p.offset) + Number(p.limit)}`;
    } else if (Array.isArray(p.edits) && p.edits[0]?.startLine) {
        lineRange = p.edits[0].endLine ? `#L${p.edits[0].startLine}-${p.edits[0].endLine}` : `#L${p.edits[0].startLine}`;
    }

    // 提取增删行数（真实 LCS 统计，与 Diff 视图渲染保持一致，兼容驼峰与蛇形）
    let additions = 0;
    let deletions = 0;
    let hasDiff = false;
    const targetContent = p.TargetContent ?? p.target_content ?? p.targetContent;
    const replacementContent = p.ReplacementContent ?? p.replacement_content ?? p.replacementContent;
    if (Array.isArray(p.edits) && p.edits.length > 0) {
        hasDiff = true;
        for (const e of p.edits) {
            const o = String(e.oldText ?? e.old_text ?? e.old_str ?? "");
            const n = String(e.newText ?? e.new_text ?? e.new_str ?? "");
            const stats = computeLineDiffStats(o, n);
            deletions += stats.deletions;
            additions += stats.additions;
        }
    } else if (targetContent !== undefined || replacementContent !== undefined) {
        hasDiff = true;
        const stats = computeLineDiffStats(
            typeof targetContent === "string" ? targetContent : "",
            typeof replacementContent === "string" ? replacementContent : ""
        );
        additions = stats.additions;
        deletions = stats.deletions;
    } else if (p.old_str !== undefined || p.new_str !== undefined || p.oldText !== undefined || p.newText !== undefined) {
        hasDiff = true;
        const stats = computeLineDiffStats(
            String(p.old_str ?? p.oldText ?? ""),
            String(p.new_str ?? p.newText ?? "")
        );
        additions = stats.additions;
        deletions = stats.deletions;
    } else if (p.CodeContent !== undefined || p.content !== undefined) {
        const text = String(p.CodeContent ?? p.content ?? "");
        hasDiff = true;
        additions = text ? text.split("\n").length : 1;
    }

    const t = rawTool.toLowerCase();
    const isLocalFile = t === "local_file" || t.endsWith("/local_file");
    const isLocalShell = t === "local_shell" || t.endsWith("/local_shell");
    let act = String(step.action || p.action || "").toLowerCase();
    // action 槽有时只是工具名；本地文件缺省读取不能落入默认 MCP 展示。
    if (isLocalFile && (!act || act === "local_file")) {
        act = String(p.action || "read").toLowerCase();
    }

    // 1. 读文件 (包含原生 local_file read)
    if (
        (isLocalFile && (act.startsWith("read") || (!act && !p.content && targetContent === undefined))) ||
        t.includes("read_text") ||
        t.includes("read_file") ||
        t.includes("view_file") ||
        t.includes("cat")
    ) {
        const action = fileName ? `读取 ${fileName}${lineRange ? ` ${lineRange}` : ""}` : "读取文件内容";
        return {
            badgeIcon: "📄",
            badgeLabel: "读取",
            actionText: action,
            filePath,
            fileName,
            lineRange,
            hasDiff: false,
        };
    }

    // 2. 改代码 / 编辑文件 (包含原生 local_file edit)
    if (
        (isLocalFile && (act.startsWith("edit") || act.startsWith("modify") || targetContent !== undefined)) ||
        t.includes("edit_file") ||
        t.includes("replace") ||
        t.includes("patch") ||
        t.includes("apply_diff") ||
        t.includes("modify")
    ) {
        let action = fileName ? `修改 ${fileName}${lineRange ? ` ${lineRange}` : ""}` : "修改代码";
        if (additions > 0 || deletions > 0) {
            action += ` (+${additions} -${deletions})`;
        }
        return {
            badgeIcon: "📄",
            badgeLabel: "修改",
            actionText: action,
            filePath,
            fileName,
            lineRange,
            hasDiff: true,
            diffStat: (additions > 0 || deletions > 0) ? { additions, deletions } : undefined,
        };
    }

    // 3. 写文件 / 新建文件 (包含原生 local_file write)
    if (
        (isLocalFile && (act.startsWith("write") || act.startsWith("create"))) ||
        t.includes("write_file") ||
        t.includes("write_to_file") ||
        t.includes("create_file")
    ) {
        const action = fileName ? `写入 ${fileName}${additions > 0 ? ` (+${additions}行)` : ""}` : "写入文件";
        return {
            badgeIcon: "📄",
            badgeLabel: "写入",
            actionText: action,
            filePath,
            fileName,
            lineRange,
            hasDiff: true,
            diffStat: additions > 0 ? { additions, deletions: 0 } : undefined,
        };
    }

    // 3.5 互联网全网搜索与抓取 (web_search / web_fetch) —— 必须置于代码检索之前，防止串味
    if (t === "web_search" || t.includes("web_search") || t.includes("duckduckgo") || t.includes("bing")) {
        const query = String(p.query || p.q || p.Query || "").trim();
        return {
            badgeIcon: "🌐",
            badgeLabel: "全网搜索",
            actionText: query ? `搜索 "${query}"` : "全网检索",
            hasDiff: false,
        };
    }
    if (t === "web_fetch" || t.includes("web_fetch") || t.includes("fetch_url")) {
        const url = String(p.url || p.Url || "").trim();
        return {
            badgeIcon: "🌍",
            badgeLabel: "抓取网页",
            actionText: url ? `抓取 ${url}` : "提取网页正文",
            hasDiff: false,
        };
    }

    // 4. 搜索代码 / 检索历史日志 (原生 search 与 MCP ripgrep / grep)
    if (t === "search" || t.includes("search") || t.includes("grep") || t.includes("find") || t.includes("ripgrep")) {
        const query = String(p.query || p.pattern || p.regex || p.term || p.Query || "").trim();
        const kinds = String(p.kinds || "").toLowerCase();
        let badgeLabel = "检索";
        let action = query ? `"${query}"` : "检索代码";

        if (kinds === "history") {
            badgeLabel = "日志检索";
            action = query ? `检索历史日志: "${query}"` : "检索历史执行日志";
        } else if (kinds === "all") {
            badgeLabel = "联邦检索";
            action = query ? `联邦检索: "${query}"` : "检索历史与工作区代码";
        } else if (fileName) {
            action += ` in ${fileName}`;
        } else if (p.path || p.dir) {
            const scope = String(p.path || p.dir).split("/").filter(Boolean).slice(-2).join("/");
            if (scope) action += ` (${scope})`;
        }

        return {
            badgeIcon: "🔍",
            badgeLabel,
            actionText: action,
            filePath,
            fileName,
            hasDiff: false,
        };
    }

    // 5. 终端命令 (包含原生 local_shell)
    if (isLocalShell || t.includes("bash") || t.includes("shell") || t.includes("exec") || t.includes("cmd") || t.includes("terminal") || t.includes("run_command")) {
        let cmd = String(p.command || p.cmd || p.CommandLine || "").trim();
        if (Array.isArray(p.args) && p.args.length > 0) {
            cmd = `${cmd} ${p.args.join(" ")}`.trim();
        }
        const action = cmd ? `$ ${cmd}` : "执行终端命令";
        return {
            badgeIcon: "⚡",
            badgeLabel: "终端",
            actionText: action,
            hasDiff: false,
        };
    }

    // 6. 查目录 (包含原生 local_file list)
    if ((isLocalFile && act.startsWith("list")) || t.includes("directory") || t.includes("tree") || t.includes("list_dir") || t.includes("list_files")) {
        const dir = String(p.dir || p.DirectoryPath || p.path || ".").trim();
        return {
            badgeIcon: "📁",
            badgeLabel: "目录",
            actionText: `查看目录 ${dir}`,
            hasDiff: false,
        };
    }

    // 7. 子智能体 (审查 / 写入 / 协同任务 / 追问对质)
    if (t.includes("child_reviewer")) {
        const question = String(p.question || p.message || "").trim();
        const role = String(p.role || "").trim();
        const action = question
            ? `审查: "${question.slice(0, 38)}"${role ? ` [${role}]` : ""}`
            : (step.description && step.description !== "执行操作" ? step.description : "执行协同质量审查");
        return {
            badgeIcon: "🤖",
            badgeLabel: "子审查",
            actionText: action,
            hasDiff: false,
        };
    }
    if (t.includes("child_writer")) {
        const task = String(p.task || "").trim();
        const action = task
            ? `沙箱写入: "${task.slice(0, 35)}"`
            : (fileName ? `子智能体写入 ${fileName}` : (step.description && step.description !== "执行操作" ? step.description : "执行沙箱代码写入"));
        return {
            badgeIcon: "✍️",
            badgeLabel: "子写入",
            actionText: action,
            filePath,
            fileName,
            hasDiff: hasDiff || !!filePath,
            diffStat: (additions > 0 || deletions > 0) ? { additions, deletions } : undefined,
        };
    }
    if (t.includes("child_worker") || t.includes("sub_agent") || t.includes("invoke_subagent")) {
        const task = String(p.task || "").trim();
        const role = String(p.role || "").trim();
        const action = task
            ? `子任务: "${task.slice(0, 38)}"${role ? ` [${role}]` : ""}`
            : (step.description && step.description !== "执行操作" ? step.description : "执行协同子任务");
        return {
            badgeIcon: "🤖",
            badgeLabel: "子代理",
            actionText: action,
            hasDiff: false,
        };
    }
    if (t.includes("resume_child")) {
        const message = String(p.message || "").trim();
        const childId = String(p.child_run_id || "").trim();
        return {
            badgeIcon: "🔄",
            badgeLabel: "追问对质",
            actionText: message ? `追问对质: "${message.slice(0, 35)}"` : (childId ? `追问子任务 #${childId}` : "追问已完成的子任务"),
            hasDiff: false,
        };
    }

    // 8. 挂起等待
    if (t.includes("wait_jobs") || t.includes("wait_job")) {
        const count = Array.isArray(p.job_ids) ? p.job_ids.length : 0;
        return {
            badgeIcon: "⏳",
            badgeLabel: "异步挂起",
            actionText: count > 0 ? `等待 ${count} 个后台任务完成` : "等待后台任务完成",
            hasDiff: false,
        };
    }

    // 8.1 人工交互与问询
    if (t.includes("ask_human") || t.includes("human") || t.includes("confirm")) {
        const question = String(p.message || p.question || p.prompt || "").trim();
        return {
            badgeIcon: "👤",
            badgeLabel: "人工交互",
            actionText: question ? `呈报问询: "${question.slice(0, 45)}"` : "请求人工决策/方案审批",
            hasDiff: false,
        };
    }

    // 8.2 任务终态交付
    if (t === "finish" || t.includes("finish") || t.includes("deliver")) {
        const summary = String(p.summary || p.final_summary || p.message || "").trim();
        return {
            badgeIcon: "🎉",
            badgeLabel: "终态交付",
            actionText: summary ? `交付: "${summary.slice(0, 45)}"` : "提交最终交付物与总结",
            hasDiff: false,
        };
    }

    // 8.3 产物交付与登记
    if (t.includes("artifact")) {
        const title = String(p.title || p.path || p.name || "").trim();
        const subAct = String(p.action || act || "生成交付产物文档");
        let actionDesc = subAct;
        if (subAct.includes("create")) actionDesc = title ? `创建交付产物: ${title}` : "创建交付产物";
        else if (subAct.includes("register")) actionDesc = title ? `登记交付产物: ${title}` : "登记交付产物";
        else if (subAct.includes("read")) actionDesc = title ? `查阅产物: ${title}` : "查阅交付产物";
        else if (subAct.includes("list")) actionDesc = "查看已登记产物清单";
        else if (title) actionDesc = `${subAct}: ${title}`;

        return {
            badgeIcon: "📦",
            badgeLabel: "交付产物",
            actionText: actionDesc,
            filePath: p.path ? cleanFilePath(String(p.path)) : undefined,
            fileName: p.path ? (cleanFilePath(String(p.path)).split("/").pop() || "") : undefined,
            hasDiff: false,
        };
    }

    // 8.4 结构化任务数据暂存
    if (t.includes("task_store")) {
        const subAct = String(p.action || act || "").trim();
        const label = String(p.label || p.resource_id || p.key || "").trim();
        let actionDesc = "存取任务结构化数据";
        if (subAct.includes("create")) actionDesc = label ? `创建数据区: ${label}` : "创建任务数据区";
        else if (subAct.includes("upsert") || subAct.includes("record")) actionDesc = label ? `存入记录: ${label}` : "更新任务记录";
        else if (subAct.includes("list")) actionDesc = label ? `查询列表: ${label}` : "查询任务数据账本";
        else if (subAct.includes("get")) actionDesc = label ? `读取记录: ${label}` : "读取任务数据";
        else if (label) actionDesc = `存取数据: ${label}`;

        return {
            badgeIcon: "💾",
            badgeLabel: "任务暂存",
            actionText: actionDesc,
            hasDiff: false,
        };
    }

    // 8.5 取回历史截断输出
    if (t.includes("get_step_output")) {
        const id = String(p.output_id || p.id || "").trim();
        return {
            badgeIcon: "📜",
            badgeLabel: "输出取回",
            actionText: id ? `取回输出原文 #${id}` : "定向提取截断输出",
            hasDiff: false,
        };
    }

    // 8.6 模型与平台管理
    if (t.includes("model_manager")) {
        const action = String(p.action || act || "").trim();
        const name = String(p.name || p.model_name || p.platform_name || "").trim();
        return {
            badgeIcon: "⚙️",
            badgeLabel: "模型管理",
            actionText: action ? `${action}${name ? ` (${name})` : ""}` : "维护模型平台与渠道配置",
            hasDiff: false,
        };
    }

    // 8.7 领域技能手册管理
    if (t.includes("skill_manager")) {
        const action = String(p.action || act || "").trim();
        const name = String(p.name || "").trim();
        return {
            badgeIcon: "🧠",
            badgeLabel: "技能管理",
            actionText: action ? `${action}${name ? ` [${name}]` : ""}` : "维护领域专家技能手册",
            hasDiff: false,
        };
    }

    // 8.8 MCP 外部插件管理
    if (t.includes("mcp_manager")) {
        const action = String(p.action || act || "").trim();
        return {
            badgeIcon: "🔌",
            badgeLabel: "插件管理",
            actionText: action || "管理外部 MCP 插件协议连接",
            hasDiff: false,
        };
    }

    // 8.9 角色卡体系管理
    if (t.includes("role_manager")) {
        const action = String(p.action || act || "").trim();
        const name = String(p.name || p.label || "").trim();
        return {
            badgeIcon: "🎭",
            badgeLabel: "角色管理",
            actionText: action ? `${action}${name ? ` (${name})` : ""}` : "维护智能体角色专家卡体系",
            hasDiff: false,
        };
    }

    // 8.10 定时计划管理
    if (t.includes("schedule_manager")) {
        const action = String(p.action || act || "").trim();
        const name = String(p.name || p.goal || "").trim();
        return {
            badgeIcon: "⏰",
            badgeLabel: "定时计划",
            actionText: action ? `${action}${name ? ` (${name.slice(0, 20)})` : ""}` : "配置后台定时自动化计划",
            hasDiff: false,
        };
    }

    // 8.11 项目白名单管理
    if (t.includes("project_manager")) {
        const action = String(p.action || act || "").trim();
        const name = String(p.name || p.root_path || "").trim();
        let actionDesc = "管理项目工作区";
        if (action.includes("create")) actionDesc = name ? `创建项目工作区: ${name}` : "创建新项目工作区";
        else if (action.includes("delete")) actionDesc = "删除项目工作区";
        else if (action.includes("list")) actionDesc = "列出所有项目工作区";
        else if (action.includes("resource")) actionDesc = `为项目关联资源: ${p.path || ""}`;
        return {
            badgeIcon: "🏗️",
            badgeLabel: "项目管理",
            actionText: actionDesc,
            hasDiff: false,
        };
    }

    // 8.12 项目受管资源管理
    if (t.includes("project_resources") || t.includes("register_project_resource")) {
        const action = String(p.action || act || "").trim();
        const path = String(p.path || p.resource_id || "").trim();
        let actionDesc = "管理项目受管资源";
        if (action.includes("register")) actionDesc = path ? `登记受管资源: ${path.split("/").pop() || path}` : "登记项目受管资源";
        else if (action.includes("read")) actionDesc = path ? `读取受管资源: ${path}` : "读取项目受管资源";
        else if (action.includes("list")) actionDesc = "列出项目受管资源";
        return {
            badgeIcon: "📚",
            badgeLabel: "受管资源",
            actionText: actionDesc,
            hasDiff: false,
        };
    }

    // 8.13 变动持续监听
    if (t.includes("watch_start") || t.includes("watch_stop") || t.includes("watch")) {
        const target = String(p.target || p.watch_id || "").trim();
        return {
            badgeIcon: "👁️",
            badgeLabel: "持续监听",
            actionText: t.includes("stop") ? `停止监听 ${target}` : `开启监听 ${target}`,
            hasDiff: false,
        };
    }

    // 8.14 系统信息探针
    if (t.includes("system_info")) {
        return {
            badgeIcon: "💻",
            badgeLabel: "系统探针",
            actionText: "探测宿主系统硬件与环境资源",
            hasDiff: false,
        };
    }

    // 8.15 微信服务通道
    if (t.includes("wechat")) {
        const text = String(p.text || p.message || "").trim();
        const action = String(p.action || act || "").trim();
        let actionDesc = "维护微信服务通道";
        if (text || action.includes("notify")) actionDesc = text ? `微信通知: "${text.slice(0, 35)}"` : "发送微信提醒";
        else if (action.includes("status")) actionDesc = "检查微信通道在线状态";
        else if (action.includes("recent")) actionDesc = "查看近期微信任务流水";
        return {
            badgeIcon: "💬",
            badgeLabel: "微信服务",
            actionText: actionDesc,
            hasDiff: false,
        };
    }

    // 9. 默认 MCP 工具
    let cleanTool = rawTool;
    if (cleanTool.includes("/")) {
        cleanTool = cleanTool.split("/").pop() || cleanTool;
    }
    const fallbackText = (step.action && step.action !== rawTool) ? step.action : (fileName || "执行操作");
    return {
        badgeIcon: "🧩",
        badgeLabel: cleanTool || "工具",
        actionText: fallbackText,
        filePath,
        fileName,
        hasDiff,
        diffStat: (additions > 0 || deletions > 0) ? { additions, deletions } : undefined,
    };
}
