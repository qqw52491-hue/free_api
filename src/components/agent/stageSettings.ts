import type { ModelRouting, StageSettings } from './types';

// Match the host: an explicit fixed-stage value takes precedence over the
// legacy reviewer override. Missing values keep the existing automatic policy.
export function stageReviewerIsDisabled(routing: Pick<ModelRouting, 'stages' | 'stage_reviewer_override'>): boolean {
    if (typeof routing.stages?.stage_review === 'boolean') return !routing.stages.stage_review;
    const legacy = routing.stage_reviewer_override?.trim().toLowerCase();
    return legacy === 'disabled' || legacy === 'none';
}

// Only an explicit stage selection may retire the legacy disabled sentinel.
// A named legacy reviewer is a separate selection and must survive changes.
export function withFixedStageChange(routing: ModelRouting, key: keyof StageSettings, mode: string, stages: StageSettings): ModelRouting {
    const legacy = routing.stage_reviewer_override?.trim().toLowerCase();
    const clearLegacyClose = key === 'stage_review' && mode !== 'off' && (legacy === 'disabled' || legacy === 'none');
    return { ...routing, stages, ...(clearLegacyClose ? { stage_reviewer_override: undefined } : {}) };
}
