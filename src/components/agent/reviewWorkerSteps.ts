import { asRecord, numberOrUndefined, toText } from "./format";
import type { AgentStepItem, AgentWorkerItem, AgentWorkerStep } from "./types";

export function reviewWorkerStep(value: unknown, fallbackStatus: AgentWorkerStep["status"]): AgentWorkerStep | undefined {
    const raw = asRecord(value);
    const id = raw.id == null ? undefined : numberOrUndefined(raw.id);
    if (id == null || !Number.isSafeInteger(id) || id < 0) return;
    const tool = toText(raw.tool || raw.action);
    const description = toText(raw.description);
    const status = toText(raw.status);
    return {
        stepNumber: id + 1,
        title: description || tool || "审查步骤",
        tool,
        action: toText(raw.action) || tool,
        description: description || undefined,
        status: status === "succeeded" || status === "failed" || status === "running" || status === "pending"
            ? status : fallbackStatus,
        params: raw.params,
        durationMs: raw.duration_ms == null ? undefined : numberOrUndefined(raw.duration_ms),
    };
}

export function updateReviewWorkerSteps(previous: AgentWorkerStep[], event: Record<string, unknown>): AgentWorkerStep[] {
    const type = event.type;
    if (type === "returned" && previous.some(step => step.status === "running" || step.status === "pending")) {
        return previous.map(step => step.status === "running" || step.status === "pending"
            ? { ...step, status: "challenged" }
            : step);
    }
    if (type === "step_new") {
        const step = reviewWorkerStep(event.step, "pending");
        if (!step) return previous;
        const latest = previous.at(-1);
        if (latest?.stepNumber === step.stepNumber && latest.status === "pending") {
            return [...previous.slice(0, -1), step];
        }
        return [...previous, step].slice(-6);
    }
    if (type !== "step_start" && type !== "step_done" && type !== "step_error") return previous;
    const id = event.step_id == null ? undefined : numberOrUndefined(event.step_id);
    if (id == null) return previous;
    const index = previous.map(step => step.stepNumber).lastIndexOf(id + 1);
    const current = previous[index];
    if (!current || (current.status !== "pending" && current.status !== "running") ||
        (type === "step_start" && current.status !== "pending")) return previous;
    const status = type === "step_start" ? "running" : type === "step_done" ? "succeeded" : "failed";
    const durationMs = event.duration_ms == null ? current.durationMs : numberOrUndefined(event.duration_ms) ?? current.durationMs;
    return previous.map((step, i) => i === index ? { ...step, status, durationMs } : step);
}

export function historicalReviewer(value: unknown): AgentWorkerItem | undefined {
    const raw = asRecord(value);
    const id = toText(raw.id);
    if (!id) return;
    const purpose = toText(raw.purpose);
    const label = toText(raw.label) || ({
        ad_hoc: "主动审查员", plan_review: "方案审查员", stage_review: "阶段审查员", final_review: "终审员", rebuttal: "复审员",
    } as Record<string, string>)[purpose] || "审查员";
    const verdict = toText(raw.verdict);
    const errorDetail = toText(raw.error_detail);
    const summary = toText(raw.summary);
    const parentStepId = typeof raw.parent_step_id === "number" && Number.isSafeInteger(raw.parent_step_id)
        ? raw.parent_step_id
        : undefined;
    const status = raw.status === "succeeded"
        ? (verdict === "rejected" ? "failed" : verdict === "failed_open" ? "challenged" : "succeeded")
        : raw.status === "running"
            ? "challenged"
            : "failed";
    const currentStep = raw.status === "succeeded"
        ? (verdict === "rejected" ? "审查不通过" : verdict === "failed_open" ? "审查未得出结论" : "已保存审查结果")
        : raw.status === "running"
            ? "审查结果待核对"
            : raw.status === "interrupted"
                ? "审查已中断"
                : raw.status === "cancelled"
                    ? "审查已停止"
                    : "审查执行未完成";
    return {
        id, name: label, roleLabel: label, role: "reviewer", status, currentStep,
        model: toText(raw.model), durationMs: raw.duration_ms == null ? undefined : numberOrUndefined(raw.duration_ms),
        startedAt: 0, progress: status === "challenged" ? 0 : 100,
        summary: summary || undefined,
        steps: Array.isArray(raw.steps) ? raw.steps.flatMap(step => reviewWorkerStep(step, "challenged") ?? []) : [],
        extra: {
            hostTaskState: currentStep,
            ...(parentStepId !== undefined ? { parentStepId } : {}),
            ...(verdict ? { verdict } : {}),
            ...(errorDetail ? { errorDetail } : {}),
        },
    };
}

export function placeHistoricalStepReviewers(
    steps: AgentStepItem[],
    reviewers: AgentWorkerItem[],
    coveredIds: Set<string>,
): { owners: Map<AgentStepItem, AgentWorkerItem[]>; placedIds: Set<string> } {
    const owners = new Map<AgentStepItem, AgentWorkerItem[]>();
    const placedIds = new Set<string>();
    const finishByStepNumber = new Map<number, AgentStepItem[]>();
    for (const step of steps) {
        if (!["finish", "prepare_finish"].includes(step.tool || "")) continue;
        const list = finishByStepNumber.get(step.stepId);
        if (list) list.push(step);
        else finishByStepNumber.set(step.stepId, [step]);
    }
    for (const reviewer of reviewers) {
        if (coveredIds.has(reviewer.id)) continue;
        const parentStepId = reviewer.extra?.parentStepId;
        if (typeof parentStepId !== "number" || !Number.isSafeInteger(parentStepId)) continue;
        const candidates = finishByStepNumber.get(parentStepId);
        if (!candidates || candidates.length !== 1) continue;
        const owner = candidates[0];
        const existing = owners.get(owner);
        if (existing) existing.push(reviewer);
        else owners.set(owner, [reviewer]);
        placedIds.add(reviewer.id);
    }
    return { owners, placedIds };
}
