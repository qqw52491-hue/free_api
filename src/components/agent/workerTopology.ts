import type { AgentWorkerItem } from "./types";

export interface WorkerBranch {
    worker: AgentWorkerItem;
    children: WorkerBranch[];
}

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
    function collect(group: readonly AgentWorkerItem[]): WorkerBranch[] {
        const branches: WorkerBranch[] = [];
        for (const worker of group) {
            if (seen.has(worker.id)) continue;
            seen.add(worker.id);
            branches.push({
                worker,
                children: collect(children.get(worker.id) ?? []),
            });
        }
        return branches;
    }
    const connected = collect(children.get(null) ?? []);
    // Start unlinked trees at their own roots even when the input order is reversed.
    const unlinked = collect(workers.filter(({ parentWorkerId }) =>
        parentWorkerId === undefined || (parentWorkerId !== null && !ids.has(parentWorkerId)),
    ));
    // If old records contain a cycle, show each member once under an unlinked root.
    unlinked.push(...collect(workers));
    return { connected, unlinked };
}
