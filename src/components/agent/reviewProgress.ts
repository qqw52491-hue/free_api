import type { AgentStepItem } from './types';

export interface ReviewStepAnchor { stepId: string; stepNumber: number; occurrence: number; runId: string; }
export interface ReviewActivity { phase: string; description: string; elapsedMs: number; round: number; toolSteps: number; }
/** 终审观测投影：不把流活动、心跳或取证步数解释成质量/完成百分比。 */
export interface ReviewProgressItem {
    id: string;
    memberKey: string;
    label: string;
    model: string;
    phase: string;
    streamKind: string;
    description: string;
    lastNotice: string;
    startedAt: number;
    elapsedMs: number;
    round: number;
    toolSteps: number;
    toolBudget: number;
    receivedChars: number;
    lastRequestRound?: number | null;
    lastRequestOutputTokens?: number | null;
    lastRequestTtftMs?: number | null;
    lastRequestTokensPerSecond?: number | null;
    failureKind: string;
    timeoutRetries: number | null;
    formatCorrections: number | null;
    anchor?: ReviewStepAnchor;
    activities?: ReviewActivity[];
}
const phases = new Set(['preparing', 'waiting', 'responding', 'tool', 'verifying', 'retrying', 'format_retry', 'converging', 'passed', 'rejected', 'failed', 'incomplete', 'cancelled', 'skipped']);
const terminal = new Set(['passed', 'rejected', 'failed', 'incomplete', 'cancelled', 'skipped']);
export function isReviewTerminal(phase: string): boolean { return terminal.has(phase); }
const number = (value: unknown): number => typeof value === 'number' && Number.isFinite(value) ? Math.max(0, value) : 0;
const optionalCount = (value: unknown): number | null => typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : null;
const failureKinds = new Set(['timeout', 'request', 'format', 'configuration', 'cancelled']);
const text = (value: unknown, limit = 200): string => typeof value === 'string' ? value.slice(0, limit) : '';

export function applyReviewProgress(items: ReviewProgressItem[], payload: Record<string, unknown>, runStartedAt: number): ReviewProgressItem[] {
    const id = text(payload.review_id), memberKey = text(payload.member_key), phase = text(payload.phase);
    const startedAt = number(payload.started_at_ms);
    if (!id || !memberKey || !phases.has(phase) || startedAt < runStartedAt) return items;
    const next: ReviewProgressItem = {
        id, memberKey, phase, startedAt,
        streamKind: payload.stream_kind === 'thinking' || payload.stream_kind === 'content' ? payload.stream_kind : '',
        label: text(payload.role_label, 80) || '终审员', model: text(payload.model, 120),
        description: text(payload.description, 300), lastNotice: text(payload.last_notice, 300), elapsedMs: number(payload.elapsed_ms),
        round: number(payload.round), toolSteps: number(payload.tool_steps), toolBudget: number(payload.tool_budget),
        receivedChars: number(payload.received_chars),
        lastRequestRound: optionalCount(payload.last_request_round),
        lastRequestOutputTokens: optionalCount(payload.last_request_output_tokens),
        lastRequestTtftMs: optionalCount(payload.last_request_ttft_ms),
        lastRequestTokensPerSecond: optionalCount(payload.last_request_round) && optionalCount(payload.last_request_output_tokens) && typeof payload.last_request_tokens_per_second === 'number' && Number.isFinite(payload.last_request_tokens_per_second) && payload.last_request_tokens_per_second > 0 ? payload.last_request_tokens_per_second : null,
        failureKind: failureKinds.has(String(payload.failure_kind)) ? String(payload.failure_kind) : '',
        timeoutRetries: optionalCount(payload.timeout_retries), formatCorrections: optionalCount(payload.format_corrections),
    };
    const index = items.findIndex(item => item.memberKey === memberKey);
    const old = items[index];
    if (old?.id === id && old.phase === 'cancelled') return items;
    if (old && (old.startedAt > startedAt || (old.id === id && (old.elapsedMs > next.elapsedMs || (isReviewTerminal(old.phase) && !isReviewTerminal(phase)))))) return items;
    next.anchor = old?.anchor;
    if (old && old.id === id) {
        if (next.lastRequestTokensPerSecond == null && old.lastRequestTokensPerSecond != null) {
            next.lastRequestTokensPerSecond = old.lastRequestTokensPerSecond;
            next.lastRequestRound = old.lastRequestRound;
            next.lastRequestOutputTokens = old.lastRequestOutputTokens;
        }
        if (next.lastRequestTtftMs == null && old.lastRequestTtftMs != null) {
            next.lastRequestTtftMs = old.lastRequestTtftMs;
        }
        if (!next.model && old.model) {
            next.model = old.model;
        }
    }
    const history = old?.id === id ? old.activities || [] : [];
    const last = history[history.length - 1];
    const activity = { phase, description: next.description, elapsedMs: next.elapsedMs, round: next.round, toolSteps: next.toolSteps };
    next.activities = !last || last.phase !== phase || last.description !== next.description || last.round !== next.round || last.toolSteps !== next.toolSteps
        ? [...history, activity].slice(-24) : history;
    if (index < 0) return [...items, next];
    return items.map((item, i) => i === index ? next : item);
}

/** 首次事件只绑定唯一正在执行的 finish；不按角色、文案或最近时间猜步骤。 */
export function activeReviewAnchor(steps: AgentStepItem[], runId: string): ReviewStepAnchor | undefined {
    const active = steps.filter(step => ['finish', 'prepare_finish'].includes(step.tool || '') && step.status === 'running');
    if (active.length !== 1) return undefined;
    const step = active[0];
    return { stepId: step.id, stepNumber: step.stepId, occurrence: steps.filter(s => s.stepId === step.stepId).indexOf(step), runId };
}

export interface ReviewLocation { id: string; anchor?: ReviewStepAnchor; }
export interface ReviewPlanLocation { id: string; type: string; runId?: string; steps?: AgentStepItem[]; }
/** 实时按物理步骤 ID 定位；归档后仅允许同 run + 逻辑步骤 + 尝试序号回放。 */
export function placeReviewRows(reviews: ReviewLocation[], entries: ReviewPlanLocation[]) {
    const owners = new Map<string, string[]>();
    const unplaced: string[] = [];
    for (const review of reviews) {
        const anchor = review.anchor;
        const plans = entries.filter(entry => entry.type === 'plan');
        let owner = anchor ? plans.flatMap(entry => entry.steps || []).find(step => step.id === anchor.stepId) : undefined;
        if (!owner && anchor?.runId) {
            owner = plans.filter(entry => entry.runId === anchor.runId).flatMap(entry => entry.steps || [])
                .filter(step => step.stepId === anchor.stepNumber)[anchor.occurrence];
        }
        if (owner && ['finish', 'prepare_finish'].includes(owner.tool || '')) owners.set(owner.id, [...(owners.get(owner.id) || []), review.id]);
        else unplaced.push(review.id);
    }
    return { owners, unplaced };
}
