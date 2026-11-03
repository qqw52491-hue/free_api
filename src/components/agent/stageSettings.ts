import type { ModelRouting } from './types';

// Match the host: an explicit fixed-stage value takes precedence over the
// legacy reviewer override. Missing values keep the existing automatic policy.
export function stageReviewerIsDisabled(routing: Pick<ModelRouting, 'stages' | 'stage_reviewer_override'>): boolean {
    if (typeof routing.stages?.stage_review === 'boolean') return !routing.stages.stage_review;
    const legacy = routing.stage_reviewer_override?.trim().toLowerCase();
    return legacy === 'disabled' || legacy === 'none';
}

export function withStageTriggerMode(routing: ModelRouting, mode: 'auto' | 'cross_review' | 'disabled'): ModelRouting {
    const stages = { ...routing.stages };
    if (mode === 'disabled') stages.stage_review = false;
    else delete stages.stage_review;
    return { ...routing, stages, stage_reviewer_override: mode === 'disabled' ? 'disabled' : undefined };
}
