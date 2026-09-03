import type { AgentStepItem, AgentWorkerItem, AgentWorkerStep } from "./types";

export function parseWorkerArtifacts(raw: unknown): {
    findings?: string[];
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
            let description = item;
            if (item.includes(":")) {
                const parts = item.split(":");
                tool = parts[0].trim();
                description = parts.slice(1).join(":").trim();
            }
            steps.push({
                stepNumber: idx + 1,
                title: `执行 ${tool}`,
                tool,
                description,
                status: "succeeded",
            });
        });
    }

    return {
        findings: findings.length > 0 ? findings : undefined,
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

export function extractSubagentsFromStep(step: AgentStepItem | null | undefined): AgentWorkerItem[] {
    if (!step) return [];
    const tool = (step.tool || step.action || "").toLowerCase();
    const isSub = tool === "child_worker" || tool === "child_reviewer" || tool === "child_writer";
    const isWait = tool.includes("wait_job");
    if (!isSub && !isWait) return [];

    const workers: AgentWorkerItem[] = [];

    // 1. 检查 params 是否包含批量派发 (如一次开俩 agent)
    let rawParams: any = step.params;
    if (typeof rawParams === "string") {
        try { rawParams = JSON.parse(rawParams); } catch {}
    }

    if (rawParams && Array.isArray(rawParams.dispatches) && rawParams.dispatches.length > 0) {
        rawParams.dispatches.forEach((sub: any, idx: number) => {
            const subTool = sub.tool || tool;
            const isRev = subTool === "child_reviewer";
            const role = isRev ? "reviewer" : "worker";
            const taskText = String(sub.task || sub.goal || `子任务 #${idx + 1}`);
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

    // 2. 如果不是批量 dispatches，则是常规单发子任务或 wait 步骤
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
