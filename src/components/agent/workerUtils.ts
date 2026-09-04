import type { AgentStepItem, AgentWorkerItem, AgentWorkerStep, ReviewScorecard } from "./types";

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

    // 1. 文件读取意图推导 (read_file)
    if (t === "read_file") {
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

    // 2. 目录结构勘察意图推导 (list_dir)
    if (t === "list_dir") {
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

    // 3. 命令行执行意图推导 (run_command)
    if (t === "run_command") {
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
        return { title: `执行命令分析: ${clean.split(/\s+/)[0]}`, cleanTarget: clean };
    }

    // 默认回退
    return { title: `执行 ${tool}`, cleanTarget: clean };
}

export function parseWorkerArtifacts(raw: unknown): {
    findings?: string[];
    scorecard?: ReviewScorecard;
    actionLog?: string[];
    evidenceRefs?: string[];
    confidence?: string;
    steps?: AgentWorkerStep[];
} {
    if (!raw) return {};
    let obj: any = raw;
    if (typeof raw === "string") {
        try {
            obj = JSON.parse(raw);
        } catch {
            return {};
        }
    }
    if (!obj || typeof obj !== "object") return {};

    const findings: string[] = Array.isArray(obj.findings) ? obj.findings.map(String).filter(Boolean) : [];
    const actionLog: string[] = Array.isArray(obj.action_log) ? obj.action_log.map(String).filter(Boolean) : [];
    const evidenceRefs: string[] = Array.isArray(obj.evidence_refs) ? obj.evidence_refs.map(String).filter(Boolean) : [];
    const confidence = typeof obj.confidence === "string" ? obj.confidence : undefined;

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
    };
}

export function extractWorkerDetails(step: AgentStepItem): { name: string; taskDesc: string; role: "reviewer" | "coder" | "worker"; roleLabel: string } {
    const isReviewer = step.tool === "child_reviewer" || step.action === "child_reviewer";
    const isWriter = step.tool === "child_writer" || step.action === "child_writer";
    const role: "reviewer" | "coder" | "worker" = isReviewer ? "reviewer" : isWriter ? "coder" : "worker";
    const defaultLabel = isReviewer ? "代码审查员" : isWriter ? "沙箱写入员" : "侦察工作者";
    const roleLabel = isReviewer ? "审查" : isWriter ? "写入" : "侦察";

    let taskDesc = "";
    const params = step.params || (step as any).params_summary;
    if (params) {
        if (typeof params === "object") {
            taskDesc = String((params as any).task || (params as any).goal || (params as any).instruction || (params as any).command || "").trim();
        } else if (typeof params === "string") {
            try {
                const parsed = JSON.parse(params);
                taskDesc = String(parsed.task || parsed.goal || parsed.instruction || parsed.command || "").trim();
            } catch {
                taskDesc = params.trim();
            }
        }
    }
    if (!taskDesc && step.description && step.description !== "执行操作") {
        taskDesc = step.description.trim();
    }
    if (!taskDesc && step.action && step.action !== "执行操作") {
        taskDesc = step.action.trim();
    }

    let name = defaultLabel;
    if (taskDesc) {
        const cleanDesc = taskDesc.replace(/^[🔍⚡🛡️🔧\s]+/, "");
        const shortDesc = cleanDesc.length > 18 ? cleanDesc.slice(0, 17) + "…" : cleanDesc;
        name = `${roleLabel}: ${shortDesc}`;
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

                        // 若 wait_jobs 的 resultStr 包含输出块，尝试补充
                        if (resultStr.includes(pw.id) || resultStr.includes("【任务 ID:")) {
                            const arts = parseWorkerArtifacts(resultStr);
                            if (arts.findings && !findings) findings = arts.findings;
                            if (arts.steps && (!steps || steps.length <= 3)) steps = arts.steps;
                        }

                        return {
                            ...pw,
                            status: "succeeded",
                            progress: 100,
                            findings,
                            steps,
                            durationMs,
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
                const taskText = String(sub.task || sub.goal || sub.instruction || `子任务 #${idx + 1}`);
                const cleanDesc = taskText.replace(/^[🔍⚡🛡️🔧\s]+/, "");
                const shortDesc = cleanDesc.length > 16 ? cleanDesc.slice(0, 15) + "…" : cleanDesc;
                workers.push({
                    id: `step-${step.stepId}-sub-${idx}`,
                    name: `${isRev ? '审查' : '侦察'}: ${shortDesc}`,
                    role,
                    roleLabel: isRev ? "审查" : "侦察",
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
        });
    }

    return workers;
}
