import type { AgentStepItem } from "./types";

type RepeatKind = "read" | "search" | "wait";

function parsedParams(value: unknown): Record<string, unknown> | undefined {
    if (typeof value === "string") {
        try { value = JSON.parse(value); } catch { return undefined; }
    }
    return value && typeof value === "object" && !Array.isArray(value)
        ? value as Record<string, unknown>
        : undefined;
}

function canonicalJson(value: unknown): string {
    return JSON.stringify(value, (_key, entry) => {
        if (!entry || typeof entry !== "object" || Array.isArray(entry)) return entry;
        return Object.fromEntries(Object.keys(entry).sort().map(key => [key, entry[key]]));
    });
}

export function timelineRepeat(step: AgentStepItem): { kind: RepeatKind; key: string } | undefined {
    if (step.status !== "succeeded" || step.source !== "内部" || step.finding) return;
    const read = step.tool === "local_file" && step.view?.kind === "read";
    const search = step.tool === "search" && step.view?.kind !== "search-cache";
    const wait = step.tool === "wait_jobs";
    if (!read && !search && !wait) return;

    const params = parsedParams(step.rawParams ?? step.params);
    if (!params) return;
    const result = step.rawResult ?? step.result;
    if (!result || (typeof result !== "string" && typeof result !== "object")) return;

    if (wait) {
        if (typeof result !== "string" || !Array.isArray(params.job_ids)) return;
        const ids = params.job_ids;
        if (!ids.length || !ids.every(id => typeof id === "string" && id.trim())) return;
        const targets = [...new Set(ids)].sort();
        // Only fold waits where every explicitly selected job is still running.
        const line = result.trim();
        const waiting = /^等待结束：仍有 (\d+) 个目标后台任务在运行；本次等待窗口为 \d+s，未收到新结果。/.exec(line);
        if (waiting && Number(waiting[1]) === targets.length && !line.includes("\n")) {
            return { kind: "wait", key: canonicalJson(["wait", targets]) };
        }
        return;
    }

    const target = read ? (params.path ?? params.filePath ?? params.file_path)
        : (params.query ?? params.pattern ?? params.keyword);
    if (typeof target !== "string" || !target.trim()) return;
    const kind = read ? "read" : "search";
    return { kind, key: canonicalJson([kind, step.tool, step.action, params, step.view, result]) };
}
