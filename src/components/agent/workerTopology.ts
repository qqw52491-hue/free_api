import type { AgentWorkerItem } from "./types";

export function workerTopology(workers: readonly AgentWorkerItem[]) {
    const children = new Map<string | null, AgentWorkerItem[]>();
    const ids = new Set(workers.map(worker => worker.id));
    for (const worker of workers) {
        const parent = worker.parentWorkerId;
        if (parent === undefined || (parent !== null && !ids.has(parent))) continue;
        const group = children.get(parent) ?? [];
        group.push(worker);
        children.set(parent, group);
    }

    const seen = new Set<string>();
    const rows: Array<{ worker: AgentWorkerItem; depth: number; connected: boolean }> = [];
    function append(worker: AgentWorkerItem, depth: number, connected: boolean) {
        if (seen.has(worker.id)) return;
        seen.add(worker.id);
        rows.push({ worker, depth, connected });
        for (const child of children.get(worker.id) ?? []) append(child, depth + 1, connected);
    }
    for (const worker of children.get(null) ?? []) append(worker, 1, true);
    // Start unlinked trees at their own roots even when the input order is reversed.
    for (const worker of workers) {
        if (!worker.parentWorkerId || !ids.has(worker.parentWorkerId)) append(worker, 0, false);
    }
    for (const worker of workers) append(worker, 0, false);
    return rows;
}
