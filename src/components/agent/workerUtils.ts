import type { AgentStepItem, AgentWorkerItem, AgentWorkerStep, ReviewScorecard, ContractSchema } from "./types";

export function inferStepIntent(tool: string, rawTarget: string, explicitIntent?: string): { title: string; cleanTarget: string } {
    let clean = rawTarget.trim();
    // 移除包裹的 {"path":"..."} 或 {"command":"..."}
    if (clean.startsWith("{") && clean.endsWith("}")) {
        try {
            const parsed = JSON.parse(clean);
            if (parsed.path) clean = String(parsed.path);
            else if (parsed.command) clean = String(parsed.command);
        } catch {}
    }
    // 移除命令回显常见的前缀
    clean = clean.replace(/^(read_file|list_dir|run_command)[:：\s]*/i, "").trim();
    clean = clean.replace(/^运行命令[:：\s]*/i, "").trim();

    if (explicitIntent && explicitIntent.trim()) {
        return { title: explicitIntent.trim(), cleanTarget: clean };
    }

    const t = tool.toLowerCase();

    // 0. 原生本地文件分流 (local_file)
    if (t === "local_file" || t.endsWith("/local_file")) {
        if (clean.includes('"action":"edit"') || clean.includes("'action':'edit'") || clean.includes("edit") || clean.includes("target_content")) {
            const name = clean.split("/").pop() || clean;
            return { title: `精准局部替换源码文件 [${name}]`, cleanTarget: clean };
        }
        if (clean.includes('"action":"write"') || clean.includes("'action':'write'") || clean.includes("write")) {
            const name = clean.split("/").pop() || clean;
            return { title: `全量写入源码文件 [${name}]`, cleanTarget: clean };
        }
        if (clean.includes('"action":"list"') || clean.includes("'action':'list'") || clean.includes("list")) {
            return { title: `探测目录 [${clean}] 拓扑结构`, cleanTarget: clean };
        }
        // 默认按读取处理
        const name = clean.split("/").pop() || clean;
        return { title: `读取源码文件 [${name}]`, cleanTarget: clean };
    }

    // 1. 文件读取意图推导 (read_file / view_file / read_text_file)
    if (t === "read_file" || t === "view_file" || t === "read_text_file" || t === "read_text") {
        if (clean.endsWith("package.json")) return { title: "查阅工程依赖清单与构建脚本", cleanTarget: clean };
        if (clean.endsWith("Cargo.toml")) return { title: "审查 Rust 后端依赖与 crate 特性配置", cleanTarget: clean };
        if (clean.endsWith("tsconfig.json")) return { title: "审查 TypeScript 编译与路径别名配置", cleanTarget: clean };
        if (clean.endsWith("tauri.conf.json")) return { title: "检查 Tauri 应用桌面窗口与安全权限配置", cleanTarget: clean };
        if (clean.endsWith("main.ts") || clean.endsWith("index.ts")) return { title: "勘察前端主入口与全局挂载装配", cleanTarget: clean };
        if (clean.endsWith("main.rs")) return { title: "审查 Rust 应用程序后端主入口点", cleanTarget: clean };
        if (clean.endsWith("lib.rs")) return { title: "审查 Tauri 插件注册与命令暴露接口", cleanTarget: clean };
        if (clean.endsWith("App.vue")) return { title: "勘察应用根组件与顶级布局骨架", cleanTarget: clean };
        if (clean.includes("router")) return { title: "分析前端页面路由与导航映射表", cleanTarget: clean };
        if (clean.includes("wechat") || clean.includes("channel")) return { title: "深入分析微信通讯通道与桥接实现", cleanTarget: clean };
        if (clean.includes("child_worker") || clean.includes("loop_runner") || clean.includes("main_loop")) {
            return { title: "审查 Agent 调度引擎与循环状态机", cleanTarget: clean };
        }
        if (clean.endsWith(".vue")) {
            const name = clean.split("/").pop() || clean;
            return { title: `审查 [${name}] 视图组件结构与交互`, cleanTarget: clean };
        }
        if (clean.endsWith(".rs")) {
            const name = clean.split("/").pop() || clean;
            return { title: `分析后端模块 [${name}] 业务逻辑`, cleanTarget: clean };
        }
        return { title: `读取源码文件 [${clean.split("/").pop() || clean}]`, cleanTarget: clean };
    }

    // 2. 目录结构勘察意图推导 (list_dir / list_directory / tree)
    if (t === "list_dir" || t === "list_directory" || t === "tree") {
        if (clean === "." || clean === "") return { title: "查阅工程根目录与项目架构全貌", cleanTarget: clean || "." };
        if (clean.includes("views")) return { title: "梳理前端页面模块与视图体系", cleanTarget: clean };
        if (clean.includes("components")) return { title: "探索前端通用组件库与子组件布局", cleanTarget: clean };
        if (clean.includes("agent")) return { title: "勘察 Agent 核心引擎与调度模块目录", cleanTarget: clean };
        if (clean.includes("src-tauri/src")) return { title: "探索 Tauri 后端核心源码结构", cleanTarget: clean };
        if (clean.includes("src-tauri")) return { title: "勘察桌面端 Rust 工程架构", cleanTarget: clean };
        if (clean.includes("src")) return { title: "勘察前端应用源码根目录", cleanTarget: clean };
        if (clean.includes("wechat")) return { title: "勘察微信服务桥接子项目结构", cleanTarget: clean };
        return { title: `探测目录 [${clean}] 拓扑结构`, cleanTarget: clean };
    }

    // 3. 命令行执行意图推导 (run_command / shell_exec / bash / local_shell)
    if (t === "local_shell" || t.endsWith("/local_shell") || t === "run_command" || t === "shell_exec" || t === "bash") {
        if (clean.startsWith("find ")) return { title: "遍历定位目标源码与资源文件树", cleanTarget: clean };
        if (clean.includes("rg ") || clean.includes("grep ")) {
            if (clean.includes("pub struct") || clean.includes("struct ")) return { title: "检索核心结构体与数据模型声明", cleanTarget: clean };
            if (clean.includes("pub async fn") || clean.includes("async fn") || clean.includes("fn ")) return { title: "定位关键函数签名与异步方法实现", cleanTarget: clean };
            if (clean.includes("<script") || clean.includes("script")) return { title: "定位 Vue 单文件组件脚本代码块", cleanTarget: clean };
            if (clean.includes("wechat") || clean.includes("channel")) return { title: "全文检索微信消息相关收发链路", cleanTarget: clean };
            return { title: "代码特征全文检索与模式匹配", cleanTarget: clean };
        }
        if (clean.startsWith("sed ") || clean.startsWith("head ") || clean.startsWith("tail ")) {
            const target = clean.split(/\s+/).pop() || "";
            return { title: `截取 [${target.split("/").pop() || target}] 核心片段深入分析`, cleanTarget: clean };
        }
        if (clean.startsWith("git ")) {
            if (clean.includes("diff")) return { title: "核验工作区未提交的代码差异", cleanTarget: clean };
            if (clean.includes("log")) return { title: "查阅近期提交历史与代码演进", cleanTarget: clean };
            if (clean.includes("status")) return { title: "检查本地仓库工作树修改状态", cleanTarget: clean };
            return { title: "执行 Git 只读版本库状态检查", cleanTarget: clean };
        }
        return { title: `执行终端命令: ${clean.split(/\s+/)[0]}`, cleanTarget: clean };
    }

    // 4. 全网搜索与网页抓取 (web_search / web_fetch)
    if (t === "web_search") {
        return { title: clean ? `全网检索: "${clean.slice(0, 35)}"` : "执行全网实时搜索", cleanTarget: clean };
    }
    if (t === "web_fetch" || t === "fetch_url") {
        return { title: clean ? `抓取网页正文: ${clean.slice(0, 35)}` : "提取目标网页 Markdown 正文", cleanTarget: clean };
    }

    // 5. 代码特征检索 (grep_search / ripgrep_search / search)
    if (t === "grep_search" || t === "ripgrep_search" || t === "search" || t === "search_code") {
        return { title: clean ? `全文检索特征: "${clean.slice(0, 35)}"` : "检索代码与文档特征", cleanTarget: clean };
    }

    // 6. 代码修改与写入 (edit_file / write_file / replace_file_content)
    if (t.includes("write") || t.includes("edit") || t.includes("replace") || t.includes("patch")) {
        const name = clean.split("/").pop() || clean;
        return { title: `修改/写入源码文件 [${name}]`, cleanTarget: clean };
    }

    // 7. 挂起等待 (wait_jobs)
    if (t.includes("wait_jobs") || t.includes("wait_job")) {
        return { title: "反应式挂起等待后台协同结论", cleanTarget: clean };
    }

    // 8. 人工交互与终态交付
    if (t.includes("ask_human") || t.includes("human")) {
        return { title: clean ? `请求人工裁决: ${clean.slice(0, 35)}` : "向用户呈报方案审批与问询", cleanTarget: clean };
    }
    if (t === "finish" || t.includes("finish")) {
        return { title: "达成任务终态交付与产物归档", cleanTarget: clean };
    }

    // 9. 追问子任务 (resume_child)
    if (t.includes("resume_child")) {
        return { title: clean ? `追问对质子任务: ${clean.slice(0, 35)}` : "追问对质已完成的子任务", cleanTarget: clean };
    }

    // 10. 产物、项目与微信管理
    if (t.includes("artifact")) {
        return { title: "生成并登记交付产物文档", cleanTarget: clean };
    }
    if (t.includes("project_manager")) {
        return { title: "维护工程工作区白名单", cleanTarget: clean };
    }
    if (t.includes("project_resources")) {
        return { title: "管理工程受管共享资源", cleanTarget: clean };
    }
    if (t.includes("wechat")) {
        return { title: "微信服务桥接通道调用", cleanTarget: clean };
    }

    // 默认回退
    return { title: `执行 ${tool}`, cleanTarget: clean };
}

/**
 * 安全格式化单个发现项 (Finding)，防止任何 [object Object] 泄露到 UI
 * 深度适配 child_reviewer 的 ReviewFinding 结构以及通用的键值结构
 */
export function formatFinding(item: any): string {
    if (item === null || item === undefined) return "";

    // 1. 如果已经是字符串，检查是否是残留的 [object Object] 或未解包的 JSON 字符串
    if (typeof item === "string") {
        const trimmed = item.trim();
        if (!trimmed || trimmed === "[object Object]") return "";
        if (trimmed.startsWith("{") && trimmed.endsWith("}")) {
            try {
                const parsed = JSON.parse(trimmed);
                if (parsed && typeof parsed === "object") {
                    return formatFinding(parsed);
                }
            } catch {
                // 不是有效 JSON，直接按原字符串展示
            }
        }
        return trimmed;
    }

    if (typeof item !== "object") {
        return String(item);
    }

    // 2. 结构化对象提取（支持 child_reviewer 专属 ReviewFinding 与通用模型契约）
    const id = typeof item.id === "string" && item.id.trim() !== "[object Object]" ? item.id.trim() : "";
    const severity = typeof item.severity === "string" ? item.severity.toLowerCase().trim() : "";
    const location = (typeof item.location === "string" && item.location.trim() !== "[object Object]" ? item.location.trim() : "")
        || (typeof item.file === "string" && item.file.trim() !== "[object Object]" ? item.file.trim() : "")
        || (typeof item.path === "string" && item.path.trim() !== "[object Object]" ? item.path.trim() : "");
    const why = typeof item.why === "string" && item.why.trim() !== "[object Object]" ? item.why.trim() : "";
    const current = typeof item.current === "string" && item.current.trim() !== "[object Object]" ? item.current.trim() : "";
    const suggestion = (typeof item.suggestion === "string" && item.suggestion.trim() !== "[object Object]" ? item.suggestion.trim() : "")
        || (typeof item.fix === "string" && item.fix.trim() !== "[object Object]" ? item.fix.trim() : "")
        || (typeof item.recommendation === "string" && item.recommendation.trim() !== "[object Object]" ? item.recommendation.trim() : "");
    const title = typeof item.title === "string" && item.title.trim() !== "[object Object]" ? item.title.trim() : "";
    const detail = typeof item.detail === "string" && item.detail.trim() !== "[object Object]" ? item.detail.trim() : "";

    // 候选主要说明文本（按优先级采纳）
    const mainContent = [
        why,
        item.finding,
        item.description,
        item.text,
        item.message,
        item.claim,
        item.summary,
        item.reason,
        item.issue,
        item.content,
        current,
    ].find((v) => typeof v === "string" && v.trim().length > 0 && v.trim() !== "[object Object]")?.trim() || "";

    const parts: string[] = [];

    // 严重级别徽标 / 标识
    if (severity) {
        let sevTag = severity.toUpperCase();
        if (severity === "blocking") sevTag = "🚫 阻断";
        else if (severity === "warning") sevTag = "⚠️ 警告";
        else if (severity === "question") sevTag = "❓ 存疑";
        else if (severity === "info") sevTag = "ℹ️ 提示";
        parts.push(id ? `[${id} | ${sevTag}]` : `[${sevTag}]`);
    } else if (id) {
        parts.push(`[${id}]`);
    }

    // 定位代码行或文件
    if (location) {
        parts.push(`${location}:`);
    }

    // 主干描述与标题
    if (title) {
        if (mainContent && mainContent !== title) {
            parts.push(`${title} - ${mainContent}`);
        } else {
            parts.push(title);
        }
    } else if (mainContent) {
        parts.push(mainContent);
    } else if (detail) {
        parts.push(detail);
    }

    // 修复/改进建议
    if (suggestion) {
        parts.push(`(💡 建议: ${suggestion})`);
    }

    const assembled = parts.join(" ").trim();
    if (assembled.length > 0) {
        return assembled;
    }

    // 3. 兜底转换：转换为精简 JSON 字符串，绝不输出 [object Object]
    try {
        const json = JSON.stringify(item);
        if (json && json !== "{}" && json !== "[]") return json;
    } catch {
        // ignore
    }

    return "";
}

/**
 * 安全格式化单个 Action Log 项
 */
export function formatActionLog(item: any): string {
    if (item === null || item === undefined) return "";
    if (typeof item === "string") {
        const trimmed = item.trim();
        return trimmed === "[object Object]" ? "" : trimmed;
    }
    if (typeof item !== "object") return String(item);

    const candidate = [item.action, item.tool, item.log, item.message, item.step, item.title, item.description]
        .find((v) => typeof v === "string" && v.trim().length > 0 && v.trim() !== "[object Object]");
    if (candidate) return candidate.trim();

    try {
        const json = JSON.stringify(item);
        if (json && json !== "{}") return json;
    } catch {}
    return "";
}

/**
 * 安全格式化单个证据引用项
 */
export function formatEvidenceRef(item: any): string {
    if (item === null || item === undefined) return "";
    if (typeof item === "string") {
        const trimmed = item.trim();
        return trimmed === "[object Object]" ? "" : trimmed;
    }
    if (typeof item !== "object") return String(item);

    const candidate = [item.ref, item.id, item.path, item.file, item.source, item.name, item.url]
        .find((v) => typeof v === "string" && v.trim().length > 0 && v.trim() !== "[object Object]");
    if (candidate) return candidate.trim();

    try {
        const json = JSON.stringify(item);
        if (json && json !== "{}") return json;
    } catch {}
    return "";
}

export function parseWorkerArtifacts(raw: unknown): {
    findings?: string[];
    scorecard?: ReviewScorecard;
    actionLog?: string[];
    evidenceRefs?: string[];
    confidence?: string;
    steps?: AgentWorkerStep[];
    extra?: Record<string, any>;
    customSchema?: ContractSchema;
} {
    if (!raw) return {};
    let obj: any = raw;
    if (typeof raw === "string") {
        const trimmed = raw.trim();
        try {
            obj = JSON.parse(trimmed);
        } catch {
            // 尝试提取 ```json ... ``` 块
            const codeMatch = trimmed.match(/```(?:json)?\s*([\s\S]*?)\s*```/);
            if (codeMatch) {
                try {
                    obj = JSON.parse(codeMatch[1].trim());
                } catch {
                    const braceMatch = trimmed.match(/\{[\s\S]*\}/);
                    if (braceMatch) {
                        try { obj = JSON.parse(braceMatch[0]); } catch {}
                    }
                }
            } else {
                const braceMatch = trimmed.match(/\{[\s\S]*\}/);
                if (braceMatch) {
                    try { obj = JSON.parse(braceMatch[0]); } catch {}
                }
            }
        }
    }
    if (!obj || typeof obj !== "object") return {};

    // 格式化 findings，过滤无效项与 [object Object]
    let rawFindings: any[] = [];
    if (Array.isArray(obj.findings)) {
        rawFindings = obj.findings;
    } else if (obj.findings) {
        rawFindings = [obj.findings];
    }
    const findings: string[] = rawFindings.map(formatFinding).filter(Boolean);

    // 格式化 action_log
    let rawActionLog: any[] = [];
    if (Array.isArray(obj.action_log)) {
        rawActionLog = obj.action_log;
    } else if (obj.action_log) {
        rawActionLog = [obj.action_log];
    }
    const actionLog: string[] = rawActionLog.map(formatActionLog).filter(Boolean);

    // 格式化 evidence_refs
    let rawEvidenceRefs: any[] = [];
    if (Array.isArray(obj.evidence_refs)) {
        rawEvidenceRefs = obj.evidence_refs;
    } else if (obj.evidence_refs) {
        rawEvidenceRefs = [obj.evidence_refs];
    }
    const evidenceRefs: string[] = rawEvidenceRefs.map(formatEvidenceRef).filter(Boolean);

    const confidence = typeof obj.confidence === "string" ? obj.confidence : undefined;

    // 提取扩展字段（支持嵌套在 obj.extra 中，或扁平在 obj 顶层中的非基线字段）
    const extra: Record<string, any> = {};
    if (obj.extra && typeof obj.extra === "object" && !Array.isArray(obj.extra)) {
        Object.assign(extra, obj.extra);
    }
    const baselineKeys = new Set([
        "findings", "scorecard", "action_log", "evidence_refs", "missing_evidence",
        "confidence", "status", "child_run_id", "duration_ms", "completion_tokens",
        "prompt_tokens", "error_kind", "raw_result_json", "kind", "steps",
        "routing", "withdrawn_findings", "objection_verdicts", "steps_used",
        "task_id", "attempt_id", "extra", "custom_schema", "changed_files",
        "test_command", "test_status", "test_output_tail", "workspace_copy", "patch_ref"
    ]);
    for (const [k, v] of Object.entries(obj)) {
        if (!baselineKeys.has(k) && v !== undefined && v !== null) {
            extra[k] = v;
        }
    }

    const customSchema: ContractSchema | undefined =
        (obj.custom_schema && typeof obj.custom_schema === "object") ? obj.custom_schema : undefined;

    const steps: AgentWorkerStep[] = [];
    if (actionLog.length > 0) {
        actionLog.forEach((item, idx) => {
            let tool = "action";
            let explicitIntent: string | undefined = undefined;
            let rawTarget = item;

            // 支持 action[intent]: params 格式
            const intentMatch = item.match(/^([a-zA-Z0-9_-]+)\[([^\]]+)\]:\s*([\s\S]+)$/);
            if (intentMatch) {
                tool = intentMatch[1].trim();
                explicitIntent = intentMatch[2].trim();
                rawTarget = intentMatch[3].trim();
            } else if (item.includes(":")) {
                const parts = item.split(":");
                tool = parts[0].trim();
                rawTarget = parts.slice(1).join(":").trim();
            }

            const inferred = inferStepIntent(tool, rawTarget, explicitIntent);

            steps.push({
                stepNumber: idx + 1,
                title: inferred.title,
                tool,
                description: inferred.cleanTarget,
                status: "succeeded",
            });
        });
    }

    return {
        findings: findings.length > 0 ? findings : undefined,
        scorecard: (obj.scorecard && typeof obj.scorecard === 'object') ? {
            correctness: Number(obj.scorecard.correctness) || 0,
            risk: Number(obj.scorecard.risk) || 0,
            style: Number(obj.scorecard.style) || 0,
            test_coverage: Number(obj.scorecard.test_coverage) || 0,
        } : undefined,
        actionLog: actionLog.length > 0 ? actionLog : undefined,
        evidenceRefs: evidenceRefs.length > 0 ? evidenceRefs : undefined,
        confidence,
        steps: steps.length > 0 ? steps : undefined,
        extra: Object.keys(extra).length > 0 ? extra : undefined,
        customSchema,
    };
}

export function extractWorkerDetails(step: AgentStepItem): { name: string; taskDesc: string; role: "reviewer" | "coder" | "worker"; roleLabel: string } {
    const isReviewer = step.tool === "child_reviewer" || step.action === "child_reviewer";
    const isWriter = step.tool === "child_writer" || step.action === "child_writer";
    const role: "reviewer" | "coder" | "worker" = isReviewer ? "reviewer" : isWriter ? "coder" : "worker";
    const defaultLabel = isReviewer ? "代码审查员" : isWriter ? "沙箱写入员" : "侦察工作者";
    const defaultRoleLabel = isReviewer ? "审查" : isWriter ? "写入" : "侦察";

    let taskDesc = "";
    let explicitRoleLabel = "";
    let explicitRole = "";

    const params = step.params || (step as any).params_summary;
    if (params) {
        let p: any = params;
        if (typeof params === "string") {
            try { p = JSON.parse(params); } catch { taskDesc = params.trim(); }
        }
        if (p && typeof p === "object") {
            taskDesc = String(p.task || p.question || p.goal || p.instruction || p.command || "").trim();
            explicitRoleLabel = String(p.role_label || p.roleLabel || p.card_label || "").trim();
            explicitRole = String(p.role || p.role_name || p.roleName || "").trim();
        }
    }
    if (!taskDesc && step.description && step.description !== "执行操作") {
        taskDesc = step.description.trim();
    }
    if (!taskDesc && step.action && step.action !== "执行操作") {
        taskDesc = step.action.trim();
    }

    // 智能推导：若未显式传 role_label，从任务描述或步骤描述中正则提取角色名称（如“阶段级写入审查（专家角色：🎨 前端视觉审查官）”）
    if (!explicitRoleLabel) {
        const fullSearchText = `${taskDesc} ${step.description || ""} ${step.action || ""}`;
        const match = fullSearchText.match(/(?:专家角色|挂载专家|角色)[：:]\s*([^\s，,）)\n]+)/);
        if (match && match[1]) {
            explicitRoleLabel = match[1].trim();
        }
    }

    // 优先使用明确的角色身份（例如“🎨 前端视觉与交互审查官”或“🛡️ 安全架构专家”）
    let name = explicitRoleLabel || (explicitRole ? `${defaultRoleLabel} · ${explicitRole}` : defaultLabel);
    let roleLabel = explicitRoleLabel || defaultRoleLabel;

    if (!explicitRoleLabel && !explicitRole && taskDesc) {
        const cleanDesc = taskDesc.replace(/^[🔍⚡🛡️🔧\s]+/, "");
        const shortDesc = cleanDesc.length > 18 ? cleanDesc.slice(0, 17) + "…" : cleanDesc;
        name = `${defaultRoleLabel}: ${shortDesc}`;
    }

    return {
        name,
        taskDesc: taskDesc || (isReviewer ? "深入审查代码变更与安全隐患" : isWriter ? "在隔离沙箱中编写代码与测试" : "在工作区执行独立侦察与分析"),
        role,
        roleLabel,
    };
}

export function extractSubagentsFromStep(
    step: AgentStepItem | null | undefined,
    allSteps?: AgentStepItem[]
): AgentWorkerItem[] {
    if (!step) return [];
    const tool = (step.tool || step.action || "").toLowerCase();
    const isSub = tool === "child_worker" || tool === "child_reviewer" || tool === "child_writer";
    const isWait = tool.includes("wait_job");
    if (!isSub && !isWait) return [];

    // 模式 0：如果当前选中的是 wait_jobs 挂起等待步骤，向前追溯对应派发步骤的全部并发子任务！
    if (isWait && allSteps && allSteps.length > 0) {
        const currentIdx = allSteps.findIndex((s) => s.id === step.id || s.stepId === step.stepId);
        const searchRange = currentIdx > 0 ? allSteps.slice(0, currentIdx) : allSteps;

        // 向前查找最近的并发派发步骤 (child_worker/child_reviewer)
        for (let i = searchRange.length - 1; i >= 0; i--) {
            const prevStep = searchRange[i];
            const pTool = (prevStep.tool || prevStep.action || "").toLowerCase();
            if (pTool === "child_worker" || pTool === "child_reviewer" || pTool === "child_writer") {
                const prevWorkers = extractSubagentsFromStep(prevStep);
                if (prevWorkers.length > 0) {
                    const resultStr = typeof step.result === "string" ? step.result : JSON.stringify(step.result || "");
                    return prevWorkers.map((pw) => {
                        let findings = pw.findings;
                        let steps = pw.steps;
                        let durationMs = pw.durationMs || step.metrics?.toolDurationMs;
                        let extra = pw.extra;
                        let customSchema = pw.customSchema;

                        // 若 wait_jobs 的 resultStr 包含输出块，尝试补充
                        if (resultStr.includes(pw.id) || resultStr.includes("【任务 ID:")) {
                            let targetOutput = resultStr;
                            if (resultStr.includes("【任务 ID:")) {
                                const jobSections = resultStr.split(/(?=- 【任务 ID:)/g);
                                for (const sec of jobSections) {
                                    if (sec.includes(pw.id) || jobSections.length === 1) {
                                        const outputMatch = sec.match(/输出:\s*([\s\S]+)$/);
                                        if (outputMatch) targetOutput = outputMatch[1].trim();
                                        break;
                                    }
                                }
                            }
                            const arts = parseWorkerArtifacts(targetOutput);
                            if (arts.findings && arts.findings.length > 0) findings = arts.findings;
                            if (arts.steps && (!steps || steps.length <= 3)) steps = arts.steps;
                            if (arts.extra) extra = arts.extra;
                            if (arts.customSchema) customSchema = arts.customSchema;
                        }

                        return {
                            ...pw,
                            status: "succeeded",
                            progress: 100,
                            findings,
                            steps,
                            durationMs,
                            extra,
                            customSchema,
                        };
                    });
                }
            }
        }
    }

    const workers: AgentWorkerItem[] = [];

    let rawParams: any = step.params;
    if (typeof rawParams === "string") {
        try { rawParams = JSON.parse(rawParams); } catch {}
    }

    const resultStr = typeof step.result === "string" ? step.result : JSON.stringify(step.result || "");

    // 模式 A：如果是 wait_jobs 挂起等待步骤，从结果中拆解多个完成的任务块
    // 格式形如：- 【任务 ID: child-run-xxx | 类型: child_worker】状态: succeeded, 耗时: 12000ms \n 描述: ... \n 输出: ...
    if (isWait && resultStr.includes("【任务 ID:")) {
        const jobSections = resultStr.split(/(?=- 【任务 ID:)/g);
        for (const sec of jobSections) {
            const idMatch = sec.match(/【任务 ID:\s*([^\s|]+)/);
            if (!idMatch) continue;
            const jobId = idMatch[1];
            const typeMatch = sec.match(/类型:\s*([^\s】]+)/);
            const statusMatch = sec.match(/状态:\s*([^\s,]+)/);
            const durationMatch = sec.match(/耗时:\s*(\d+)ms/);
            const descMatch = sec.match(/描述:\s*([^\n]+)/);
            const outputMatch = sec.match(/输出:\s*([\s\S]+)$/);

            const jobType = typeMatch ? typeMatch[1] : "child_worker";
            const jobStatus = statusMatch ? (statusMatch[1] === "succeeded" ? "succeeded" : "failed") : step.status;
            const duration = durationMatch ? Number(durationMatch[1]) : undefined;
            const desc = descMatch ? descMatch[1].trim() : `子任务 ${jobId.slice(0, 8)}`;
            const outputRaw = outputMatch ? outputMatch[1].trim() : "";

            const isRev = jobType === "child_reviewer";
            const role = isRev ? "reviewer" : "worker";
            const arts = parseWorkerArtifacts(outputRaw);

            const cleanDesc = desc.replace(/^[🔍⚡🛡️🔧\s]+/, "");
            const shortDesc = cleanDesc.length > 16 ? cleanDesc.slice(0, 15) + "…" : cleanDesc;

            workers.push({
                id: jobId,
                name: `${isRev ? '审查' : '侦察'}: ${shortDesc}`,
                role,
                roleLabel: isRev ? "审查" : "侦察",
                status: jobStatus as any,
                currentStep: desc,
                currentTool: jobType,
                progress: jobStatus === "succeeded" ? 100 : 70,
                startedAt: step.timestamp,
                durationMs: duration || step.metrics?.toolDurationMs,
                thought: undefined,
                output: outputRaw && outputRaw !== "（无附加输出）" ? outputRaw : undefined,
                findings: arts.findings,
                evidenceRefs: arts.evidenceRefs,
                confidence: arts.confidence,
                steps: arts.steps,
                extra: arts.extra,
                customSchema: arts.customSchema,
            });
        }
    }

    // 模式 B：检查 params 中是否有批量派发数组 (dispatches, workers, tasks, parallel, 或 params 本身是数组)
    if (workers.length === 0) {
        let dispatchList: any[] = [];
        if (Array.isArray(rawParams)) {
            dispatchList = rawParams;
        } else if (rawParams && typeof rawParams === "object") {
            if (Array.isArray(rawParams.dispatches)) dispatchList = rawParams.dispatches;
            else if (Array.isArray(rawParams.workers)) dispatchList = rawParams.workers;
            else if (Array.isArray(rawParams.tasks)) dispatchList = rawParams.tasks;
            else if (Array.isArray(rawParams.parallel)) dispatchList = rawParams.parallel;
        }

        if (dispatchList.length > 0) {
            dispatchList.forEach((sub: any, idx: number) => {
                const subTool = sub.tool || sub.action || tool;
                const isRev = subTool === "child_reviewer";
                const role = isRev ? "reviewer" : "worker";
                const explicitRole = sub.role_label || sub.roleLabel || sub.role || sub.card_label;
                const taskText = String(sub.task || sub.question || sub.goal || sub.instruction || `子任务 #${idx + 1}`);
                const cleanDesc = taskText.replace(/^[🔍⚡🛡️🔧\s]+/, "");
                const shortDesc = cleanDesc.length > 16 ? cleanDesc.slice(0, 15) + "…" : cleanDesc;
                const displayName = explicitRole ? String(explicitRole) : `${isRev ? '审查' : '侦察'}: ${shortDesc}`;
                workers.push({
                    id: `step-${step.stepId}-sub-${idx}`,
                    name: displayName,
                    role,
                    roleLabel: explicitRole ? String(explicitRole) : (isRev ? "审查" : "侦察"),
                    status: step.status,
                    currentStep: taskText,
                    currentTool: subTool,
                    progress: step.status === "succeeded" ? 100 : 70,
                    startedAt: step.timestamp,
                    durationMs: step.metrics?.toolDurationMs,
                });
            });
        }
    }

    // 模式 C：检查派发输出 summary (如: [1/2] ✅ 已派发 child_run_id=xxx 任务: 后端调度... [2/2] ...)
    if (workers.length === 0 && resultStr.includes("已派发 child_run_id=")) {
        const lines = resultStr.split("\n");
        for (const line of lines) {
            const match = line.match(/child_run_id=([^\s]+)\s+任务:\s*([^\n]+)/);
            if (match) {
                const childId = match[1];
                const taskText = match[2].trim();
                const isRev = taskText.includes("审查");
                const role = isRev ? "reviewer" : "worker";
                const cleanDesc = taskText.replace(/^[🔍⚡🛡️🔧\s]+/, "");
                const shortDesc = cleanDesc.length > 16 ? cleanDesc.slice(0, 15) + "…" : cleanDesc;
                workers.push({
                    id: childId,
                    name: `${isRev ? '审查' : '侦察'}: ${shortDesc}`,
                    role,
                    roleLabel: isRev ? "审查" : "侦察",
                    status: step.status,
                    currentStep: taskText,
                    currentTool: isRev ? "child_reviewer" : "child_worker",
                    progress: step.status === "succeeded" ? 100 : 70,
                    startedAt: step.timestamp,
                    durationMs: step.metrics?.toolDurationMs,
                });
            }
        }
    }

    // 模式 D：常规单发子任务回退
    if (workers.length === 0) {
        const details = extractWorkerDetails(step);
        const arts = parseWorkerArtifacts(step.result);
        workers.push({
            id: `step-${step.stepId}-worker`,
            name: details.name,
            role: details.role,
            roleLabel: details.roleLabel,
            status: step.status,
            currentStep: details.taskDesc,
            currentTool: step.tool || details.role,
            progress: step.status === "succeeded" ? 100 : 70,
            startedAt: step.timestamp,
            durationMs: step.metrics?.toolDurationMs,
            thought: step.thought,
            output: typeof step.result === "string" ? step.result : undefined,
            findings: arts.findings,
            evidenceRefs: arts.evidenceRefs,
            confidence: arts.confidence,
            steps: arts.steps,
            extra: arts.extra,
            customSchema: arts.customSchema,
        });
    }

    return workers;
}
