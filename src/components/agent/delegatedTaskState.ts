// 子任务面板与对话流共用的纯状态逻辑：终态单向、自动展开只认新事件、刷新串行补查、结算重试。
export interface ChildTaskSnapshot {
    child_run_id: string;
    execution_status?: string;
    execution_ended?: boolean;
    delivery_accepted?: boolean;
    handoff_accepted?: boolean;
    human_request?: unknown;
    updated_at?: string;
}

/** 宿主已结算：执行结束才算终态；complete 事件本身不是终态证据。 */
export function isSettled(task: Pick<ChildTaskSnapshot, 'execution_ended' | 'execution_status'> | undefined | null): boolean {
    if (!task) return false;
    return task.execution_ended === true || ['succeeded', 'failed', 'cancelled'].includes(task.execution_status || '');
}

/** 迟到的“运行中”快照不得覆盖已经看到的终态。 */
export function mergeSnapshot<T extends ChildTaskSnapshot>(previous: T | undefined, next: T): T {
    if (previous && isSettled(previous) && !isSettled(next)) return previous;
    return next;
}

/**
 * 只有第一次看到的活动任务 / 第一次出现的人工请求才触发自动展开。
 * 轮询中持续存在的等待项不会重新展开用户已手动收起的面板。
 */
export function detectAutoOpen(
    seenActive: Set<string>, seenWaiting: Set<string>, tasks: ChildTaskSnapshot[],
): boolean {
    let open = false;
    const active = new Set<string>();
    const waiting = new Set<string>();
    for (const task of tasks) {
        if (!isSettled(task)) {
            active.add(task.child_run_id);
            if (!seenActive.has(task.child_run_id)) open = true;
        }
        if (task.human_request) {
            waiting.add(task.child_run_id);
            if (!seenWaiting.has(task.child_run_id)) open = true;
        }
    }
    seenActive.clear(); active.forEach((id) => seenActive.add(id));
    seenWaiting.clear(); waiting.forEach((id) => seenWaiting.add(id));
    // 已结束的任务保留在 seenActive 之外；它不会再次触发展开。
    return open;
}

export interface FullDetailCache {
    settled: boolean;
}

/** 运行中取得的缓存在任务结算后失效（交付结论此时才产生），需要重新读取；运行中的进度更新不会使目标全文失效。 */
export function fullCacheIsFresh(cache: FullDetailCache | undefined, task: ChildTaskSnapshot): boolean {
    if (!cache) return false;
    return cache.settled === isSettled(task);
}

/** 刷新串行化：执行中再次请求只记一次，当前轮结束后补跑，避免切换会话漏刷新。 */
export function createSerialRefresher(run: () => Promise<void>): { request(): Promise<void> } {
    let running: Promise<void> | null = null;
    let pending = false;
    const loop = async () => {
        do {
            pending = false;
            try { await run(); } catch { /* run 自行披露错误 */ }
        } while (pending);
    };
    return {
        request() {
            if (running) { pending = true; return running; }
            running = loop().finally(() => { running = null; });
            return running;
        },
    };
}

/** complete 之后结算不保证已提交：未结算或查询失败都按退避重试，且乱序结果由 merge 保护。 */
export async function settleWithRetry<T extends ChildTaskSnapshot>(
    fetchStatus: () => Promise<T>,
    apply: (task: T) => void,
    options: { delaysMs?: number[]; sleep?: (ms: number) => Promise<void>; shouldStop?: () => boolean } = {},
): Promise<'settled' | 'unsettled'> {
    const delays = options.delaysMs ?? [120, 400, 1000, 2000, 4000];
    const sleep = options.sleep ?? ((ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)));
    for (const delay of delays) {
        await sleep(delay);
        if (options.shouldStop?.()) return 'unsettled';
        try {
            const task = await fetchStatus();
            apply(task);
            if (isSettled(task)) return 'settled';
        } catch { /* 查询失败：继续重试，面板轮询仍会补正 */ }
    }
    return 'unsettled';
}

/** 历史记录的派发回执不是终态；按真实子任务 ID 叠加宿主快照。 */
export function historicalWorkerSnapshot<T extends { id: string; status: string; currentStep?: string; progress?: number; extra?: Record<string, unknown> }>(worker: T, snapshot: ChildTaskSnapshot | undefined): T {
    if (!snapshot || snapshot.child_run_id !== worker.id) return worker;
    const ended = isSettled(snapshot);
    const label = !ended ? (snapshot.human_request ? '等待回答' : '执行中')
        : snapshot.delivery_accepted ? '已交付'
        : snapshot.handoff_accepted ? '已交接'
        : snapshot.execution_status === 'cancelled' ? '已停止' : '未完成';
    const status = !ended ? 'running' : snapshot.delivery_accepted || snapshot.handoff_accepted ? 'succeeded' : 'failed';
    return { ...worker, status, currentStep: label, progress: ended ? 100 : worker.progress,
        extra: { ...worker.extra, hostTaskState: label } } as T;
}
