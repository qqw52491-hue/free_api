import type { AgentStepItem, AgentToolStatus, AgentWorkerItem } from "./types";

// Identity comes from dispatch/wait receipts, never from role, task prose or timestamps.
export interface ChildJobReceipt {
    id: string;
    tool?: string;
    dispatchIndex?: number;
    status?: AgentToolStatus;
    durationMs?: number;
    description?: string;
    output?: string;
    model?: string;
}

function record(value: unknown): Record<string, unknown> {
    if (typeof value === "string") {
        try { return record(JSON.parse(value)); } catch { return {}; }
    }
    return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}
export function stepWorkerTool(step: Pick<AgentStepItem, "tool" | "action">): string {
    const tools = [step.tool, step.action].map(t => (t || "").toLowerCase());
    return tools.find(t => /^(child_task|child_worker|child_reviewer|child_writer|resume_child|wait_jobs|wait_for_jobs|parallel)$/.test(t)) || "";
}
export function isWorkerStep(step: Pick<AgentStepItem, "tool" | "action">): boolean {
    return !!stepWorkerTool(step);
}
export function stepResultText(step: AgentStepItem): string {
    const result = step.rawResult ?? step.result;
    return typeof result === "string" ? result : result == null ? "" : JSON.stringify(result);
}
function jobStatus(status: string): AgentToolStatus | undefined {
    if (status === "succeeded") return "succeeded";
    if (["failed", "invalid_output", "cancelled", "timeout", "timed_out"].includes(status)) return "failed";
    if (status === "running" || status === "pending") return status;
    return undefined;
}
export function parseChildJobReceipts(step: AgentStepItem): ChildJobReceipt[] {
    if (!isWorkerStep(step)) return [];
    const text = stepResultText(step);
    const receipts = new Map<string, ChildJobReceipt>();
    // Anchored host dispatch rows; task text may itself contain job IDs and is not parsed.
    const dispatched = /^(?:\[(\d+)\/\d+\]\s*)?✅\s*已派发 child_run_id=([\w-]+)\s+(?:(审查|写入))?任务:\s*([^\n]*)/gm;
    for (const m of text.matchAll(dispatched)) receipts.set(m[2], {
        id: m[2], dispatchIndex: m[1] ? Number(m[1]) - 1 : undefined,
        tool: m[3] === "审查" ? "child_reviewer" : m[3] === "写入" ? "child_writer" : "child_worker",
        description: m[4].trim(),
    });
    const singles = /^(侦察 worker 已异步派发|子审查任务已异步派发|写入 worker 已在隔离副本上开工)，child_run_id:\s*([\w-]+)/gm;
    for (const m of text.matchAll(singles)) receipts.set(m[2], {
        id: m[2], tool: m[1].startsWith("子审查") ? "child_reviewer" : m[1].startsWith("写入") ? "child_writer" : "child_worker",
    });
    // Resume/failover assigns a NEW child id; the id in resume_child params is the predecessor.
    const restarted = /^(?:✅\s*(审查专家对质|写入 worker 追问|侦察 worker 追问)已成功启动：新|🔁\s*审查者故障转移：[^\n]*已自动[^\n]*重派\s*\()\s*child_run_id[:=]\s*([\w-]+)/gm;
    for (const m of text.matchAll(restarted)) receipts.set(m[2], {
        id: m[2], tool: m[1]?.startsWith("写入") ? "child_writer" : m[1]?.startsWith("侦察") ? "child_worker" : "child_reviewer",
    });
    for (const m of text.matchAll(/^审查专家对质已启动：child_run_id=([\w-]+)/gm)) {
        receipts.set(m[1], { id: m[1], tool: "child_reviewer" });
    }
    const headers = [...text.matchAll(/^-\s*【任务 ID:\s*([\w-]+)\s*\|\s*类型:\s*([^\s】]+)】状态:\s*([\w-]+),\s*耗时:\s*(\d+)ms[^\n]*\n/gm)];
    for (let i = 0; i < headers.length; i++) {
        const m = headers[i];
        if (!/^child_(worker|reviewer|writer)$/.test(m[2])) continue;
        // Do not include host failover notices in the child's output/artifacts.
        const section = text.slice((m.index || 0) + m[0].length, headers[i + 1]?.index ?? text.length).split(/^🔁/m)[0];
        const output = section.match(/^\s*输出:\s*([\s\S]*)/m)?.[1]?.trim();
        let model: string | undefined = undefined;
        if (output) {
            try {
                const parsed = JSON.parse(output);
                if (parsed && typeof parsed === "object" && typeof parsed.model === "string") {
                    model = parsed.model;
                }
            } catch {}
        }
        receipts.set(m[1], { id: m[1], tool: m[2], status: jobStatus(m[3]), durationMs: Number(m[4]),
            description: section.match(/^\s*描述:\s*([^\n]*)/m)?.[1]?.trim(),
            output,
            model,
        });
    }
    const pushJsonReceipt = (rawObj: unknown, dispatchIndex?: number) => {
        const obj = record(rawObj);
        const nestedTask = obj.task && typeof obj.task === "object" ? record(obj.task) : null;
        const target = nestedTask?.child_run_id ? nestedTask : obj;
        const id = typeof target.child_run_id === "string" ? target.child_run_id.trim() : "";
        if (!id) return;
        const purpose = String(target.purpose || target.profile || "").toLowerCase();
        const stepTool = stepWorkerTool(step);
        const inferredTool = purpose === "reviewer" || stepTool === "child_reviewer"
            ? "child_reviewer"
            : purpose === "writer" || stepTool === "child_writer"
                ? "child_writer"
                : "child_worker";
        const execStatus = typeof target.execution_status === "string" ? target.execution_status : "";
        const ended = target.execution_ended === true || ["succeeded", "failed", "cancelled"].includes(execStatus);
        const hasAcceptanceFlags = "delivery_accepted" in target || "handoff_accepted" in target;
        const accepted = target.delivery_accepted === true || target.handoff_accepted === true;
        const finalStatus: AgentToolStatus | undefined = ended
            ? (hasAcceptanceFlags ? (accepted ? "succeeded" : "failed") : jobStatus(execStatus))
            : undefined;
        const statusSummary = ended
            ? (target.delivery_accepted === true
                ? "子目标已正式交付"
                : target.handoff_accepted === true
                    ? "已交接回父任务"
                    : execStatus === "cancelled"
                        ? "子任务已停止"
                        : "子任务未完成正式交付")
            : undefined;
        const desc = typeof target.summary === "string" && target.summary.trim()
            ? target.summary.trim()
            : statusSummary
                ? statusSummary
                : typeof target.latest_step === "string" && target.latest_step.trim()
                    ? target.latest_step.trim()
                    : typeof target.task === "string"
                        ? target.task.split("\n")[0].trim()
                        : undefined;
        const model = typeof target.model_id === "string" && target.model_id.trim()
            ? target.model_id.trim()
            : typeof target.model === "string" && target.model.trim()
                ? target.model.trim()
                : undefined;
        const durationMs = typeof target.duration_ms === "number" ? target.duration_ms : undefined;
        const prev = receipts.get(id);
        receipts.set(id, {
            ...prev,
            id,
            tool: prev?.tool || inferredTool,
            dispatchIndex: dispatchIndex ?? prev?.dispatchIndex,
            status: finalStatus ?? prev?.status,
            durationMs: durationMs ?? prev?.durationMs,
            description: desc || prev?.description,
            output: ended ? JSON.stringify(target) : prev?.output,
            model: model || prev?.model,
        });
    };
    const trimmed = text.trim();
    if (trimmed.startsWith("{")) {
        const parsed = record(trimmed);
        if (Array.isArray(parsed.results)) {
            parsed.results.forEach((item, idx) => {
                const entry = record(item);
                if (entry.output) pushJsonReceipt(entry.output, idx);
            });
        } else {
            pushJsonReceipt(parsed);
        }
    }
    return [...receipts.values()];
}
export function getStepJobIds(step: AgentStepItem): string[] {
    const tool = stepWorkerTool(step);
    if (!tool) return [];
    const ids = new Set<string>();
    if (tool === "wait_jobs" || tool === "wait_for_jobs") {
        const params = record(step.rawParams ?? step.params);
        if (Array.isArray(params.job_ids)) for (const id of params.job_ids) if (typeof id === "string" && id) ids.add(id);
        if (typeof params.job_id === "string" && params.job_id) ids.add(params.job_id);
    }
    for (const receipt of parseChildJobReceipts(step)) ids.add(receipt.id);
    return [...ids];
}
export function getStepWorkers(step: AgentStepItem, workers: AgentWorkerItem[]): AgentWorkerItem[] {
    if (!isWorkerStep(step)) return [];
    const ids = new Set(getStepJobIds(step));
    return workers.filter(w => ids.has(w.id) || (ids.size === 0 && w.extra?.parentStepId === step.id && w.extra?.dispatchPlaceholder === true));
}

// One visible home per physical child in a run. Waits still retain their exact
// associations for inspection; they do not duplicate the live member presentation.
export function workerRowOwners(
    steps: AgentStepItem[], associations: Map<AgentStepItem, AgentWorkerItem[]>,
): Map<AgentStepItem, AgentWorkerItem[]> {
    const owners = new Map<string, AgentStepItem>();
    const isWait = (step: AgentStepItem) => /^(wait_jobs|wait_for_jobs)$/.test(stepWorkerTool(step));
    for (const step of steps) for (const worker of associations.get(step) || []) {
        const previous = owners.get(worker.id);
        if (!previous || (isWait(previous) && !isWait(step))) owners.set(worker.id, step);
    }
    return new Map(steps.map(step => [step,
        (associations.get(step) || []).filter(worker => owners.get(worker.id) === step),
    ]));
}
export interface WorkerDisplayRow {
    id: string; name: string; roleLabel: string; status: AgentToolStatus; taskDesc: string; duration?: string;
}
// Deliberately excludes thought/output/progress: token chunks must not rebuild the brief list.
export function workerDisplayRows(workers: AgentWorkerItem[]): Map<string, WorkerDisplayRow> {
    return new Map(workers.map(w => [w.id, { id: w.id, name: w.name, roleLabel: w.roleLabel, status: w.status,
        taskDesc: w.currentStep || w.currentTool || (w.status === "succeeded" ? "已完成" : "调度中"),
        duration: w.durationMs ? `${(w.durationMs / 1000).toFixed(1)}s` : undefined,
    }]));
}
