/** 终审观测投影：不把流活动、心跳或取证步数解释成质量/完成百分比。 */
export interface ReviewProgressItem {
    id: string;
    memberKey: string;
    label: string;
    model: string;
    phase: string;
    description: string;
    lastNotice: string;
    startedAt: number;
    elapsedMs: number;
    round: number;
    toolSteps: number;
    toolBudget: number;
    receivedChars: number;
}
const phases = new Set(['preparing', 'waiting', 'responding', 'tool', 'verifying', 'retrying', 'format_retry', 'converging', 'passed', 'rejected', 'failed', 'incomplete', 'skipped']);
const terminal = new Set(['passed', 'rejected', 'failed', 'incomplete', 'skipped']);
export function isReviewTerminal(phase: string): boolean { return terminal.has(phase); }
const number = (value: unknown): number => typeof value === 'number' && Number.isFinite(value) ? Math.max(0, value) : 0;
const text = (value: unknown, limit = 200): string => typeof value === 'string' ? value.slice(0, limit) : '';

export function applyReviewProgress(items: ReviewProgressItem[], payload: Record<string, unknown>, runStartedAt: number): ReviewProgressItem[] {
    const id = text(payload.review_id), memberKey = text(payload.member_key), phase = text(payload.phase);
    const startedAt = number(payload.started_at_ms);
    if (!id || !memberKey || !phases.has(phase) || startedAt < runStartedAt) return items;
    const next: ReviewProgressItem = {
        id, memberKey, phase, startedAt,
        label: text(payload.role_label, 80) || '终审员', model: text(payload.model, 120),
        description: text(payload.description, 300), lastNotice: text(payload.last_notice, 300), elapsedMs: number(payload.elapsed_ms),
        round: number(payload.round), toolSteps: number(payload.tool_steps), toolBudget: number(payload.tool_budget),
        receivedChars: number(payload.received_chars),
    };
    const index = items.findIndex(item => item.memberKey === memberKey);
    if (index < 0) return [...items, next];
    const old = items[index];
    if (old.startedAt > startedAt || (old.id === id && (old.elapsedMs > next.elapsedMs || (isReviewTerminal(old.phase) && !isReviewTerminal(phase))))) return items;
    return items.map((item, i) => i === index ? next : item);
}
