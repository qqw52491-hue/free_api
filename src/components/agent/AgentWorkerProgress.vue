<template>
    <article class="worker-progress" :class="[`status-${rowStatus}`, { 'is-review': !!review, 'review-live': memberActive }]" :data-worker-id="worker?.id" :data-review-id="review?.id">
        <div class="worker-progress-line">
            <span class="worker-status" :aria-label="statusText">
                <span v-if="rowStatus === 'running'" class="worker-spinner-ring" aria-hidden="true"></span>
                <svg v-else-if="rowStatus === 'succeeded'" class="worker-status-svg is-success" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                    <polyline points="20 6 9 17 4 12" />
                </svg>
                <svg v-else-if="rowStatus === 'failed'" class="worker-status-svg is-failed" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                    <line x1="18" y1="6" x2="6" y2="18" /><line x1="6" y1="6" x2="18" y2="18" />
                </svg>
                <svg v-else-if="rowStatus === 'challenged'" class="worker-status-svg is-warning" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                    <circle cx="12" cy="12" r="10" /><line x1="12" y1="8" x2="12" y2="12" /><line x1="12" y1="16" x2="12.01" y2="16" />
                </svg>
                <span v-else class="worker-status-dot" aria-hidden="true"></span>
            </span>
            <button class="worker-identity" :class="{ 'is-review-member': !!review }" type="button" :title="label" :aria-expanded="review ? expanded : undefined" @click="review ? expanded = !expanded : $emit('inspect')">
                <svg v-if="review" class="member-shield-icon" width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                    <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />
                </svg>
                <span>{{ label }}</span>
            </button>
            <span v-if="displayModel" class="review-model-pill worker-line-model" :title="`模型: ${displayModel}`">{{ displayModel }}</span>
            <span v-if="worker?.extra?.resumed" class="worker-resumed">续接</span>
            <span class="worker-activity" :title="activity">{{ activity }}</span>
            <span v-if="review?.lastRequestTokensPerSecond != null || durationMs != null" class="worker-metrics">
                <span v-if="review?.lastRequestTokensPerSecond != null" class="worker-speed" :title="`最近已结束的第 ${review.lastRequestRound} 次请求输出均速；不是当前实时速度，token 按供应商口径`">
                    <svg class="speed-bolt-icon" width="9" height="9" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2" /></svg>
                    <span>{{ review.lastRequestTokensPerSecond.toFixed(1) }} tok/s</span>
                </span>
                <span v-if="durationMs != null" class="worker-duration">{{ (durationMs / 1000).toFixed(1) }}s</span>
            </span>
            <button v-if="canStopReview" type="button" class="worker-stop-review" :disabled="stopping" title="停止此审查；当前任务后续复审也不再派发该模型，新任务恢复原配置" @click.stop="stopThisReview">{{ stopping ? '停止中…' : '停止并排除' }}</button>
            <button type="button" class="worker-thought-toggle" :class="{ 'is-open': expanded }" :aria-expanded="expanded" @click="expanded = !expanded">
                <span>{{ expanded ? '收起' : '详情' }}</span>
                <svg class="toggle-arrow" width="9" height="9" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
                    <polyline points="6 9 12 15 18 9" />
                </svg>
            </button>
        </div>
        <p v-if="stopError" class="review-stop-error" role="alert">{{ stopError }}</p>
        <div v-if="expanded && review" class="review-live-body">
            <div class="review-stats-grid" aria-label="实际请求与取证统计">
                <span class="stat-chip">
                    <span class="stat-k">请求</span>
                    <span class="stat-v">{{ review.round }} 次</span>
                </span>
                <span class="stat-chip" :title="review.toolBudget > 0 ? '只限制取证工具调用次数，不是总请求数或完成进度；重试、补正与最终结论另计' : undefined">
                    <span class="stat-k">取证</span>
                    <span class="stat-v">{{ review.toolBudget > 0 ? `${review.toolSteps}/${review.toolBudget} 步` : '关闭' }}</span>
                </span>
                <span class="stat-chip">
                    <span class="stat-k">片段</span>
                    <span class="stat-v">{{ review.receivedChars.toLocaleString() }} 字</span>
                </span>
                <span v-if="review.lastRequestTokensPerSecond != null" class="stat-chip speed-chip" title="供应商回报的输出 token ÷ 首片段至请求结束的秒数；不含前置等待、工具时间和其他请求">
                    <span class="stat-k">均速</span>
                    <span class="stat-v">{{ review.lastRequestTokensPerSecond.toFixed(1) }} tok/s</span>
                </span>
                <span v-if="review.lastRequestTtftMs != null" class="stat-chip">
                    <span class="stat-k">首片</span>
                    <span class="stat-v">{{ (review.lastRequestTtftMs / 1000).toFixed(1) }}s</span>
                </span>
                <span v-if="review.timeoutRetries" class="stat-chip warn-chip">
                    <span class="stat-k">重试</span>
                    <span class="stat-v">{{ review.timeoutRetries }}</span>
                </span>
                <span v-if="review.formatCorrections" class="stat-chip warn-chip">
                    <span class="stat-k">补正</span>
                    <span class="stat-v">{{ review.formatCorrections }}</span>
                </span>
            </div>
            <div ref="activityPane" class="review-activity-scroll soft-scrollbar" role="region" :aria-label="`${label} · 实时动作`" tabindex="0" @scroll.passive="onActivityScroll" @wheel.passive="followActivity = false" @touchmove.passive="followActivity = false">
                <p v-for="(event, index) in reviewActivities" :key="`${review.id}-${index}`" class="review-activity-event">
                    <time>{{ (event.elapsedMs / 1000).toFixed(1) }}s</time><span>{{ event.description || statusText }}</span>
                </p>
            </div>
            <details class="review-diagnostics">
                <summary>
                    <span>诊断详情</span>
                    <span v-if="review.model" class="review-model-pill">{{ review.model }}</span>
                </summary>
                <div class="review-diagnostic-content">
                    <p v-if="diagnosis" class="diag-banner" :class="`tone-${diagnosis.tone || 'neutral'}`">{{ diagnosis.title }} · {{ diagnosis.detail }}</p>
                    <p>第 {{ review.round }} 次逻辑请求 · 取证已用 {{ review.toolSteps }} / 上限 {{ review.toolBudget }} 次</p>
                    <p>超时重发 {{ review.timeoutRetries ?? '未知' }} 次 · 格式补正 {{ review.formatCorrections ?? '未知' }} 次</p>
                    <p v-if="review.lastNotice" class="diag-notice">最近异常：{{ review.lastNotice }}</p>
                    <p class="diag-caption">片段数不是 token 数或吞吐速度；耗时以最后事件为准，成员通过不代表整个任务已经交付。</p>
                </div>
            </details>
        </div>
        <div v-if="expanded && worker" class="review-live-body worker-live-body">
            <div v-if="worker.steps?.length || workerModel || worker.currentTool" class="review-stats-grid" aria-label="已上报的子任务步骤">
                <span v-if="worker.steps?.length" class="stat-chip">
                    <span class="stat-k">记录</span>
                    <span class="stat-v">{{ worker.steps.length }} 步</span>
                </span>
                <span v-if="worker.currentTool" class="stat-chip">
                    <span class="stat-k">当前工具</span>
                    <span class="stat-v">{{ worker.currentTool }}</span>
                </span>
                <span v-if="workerModel" class="stat-chip" :title="`执行模型: ${workerModel}`">
                    <span class="stat-k">模型</span>
                    <span class="stat-v">{{ workerModel }}</span>
                </span>
            </div>
            <div ref="activityPane" class="review-activity-scroll worker-activity-scroll soft-scrollbar" role="region" :aria-label="`${label} · 实时动作与思考片段`" tabindex="0" @scroll.passive="onActivityScroll" @wheel.passive="followActivity = false" @touchmove.passive="followActivity = false">
                <div v-for="row in workerActionRows" :key="row.step.stepNumber" class="review-activity-event worker-action-event">
                    <span class="worker-step-index">{{ row.step.stepNumber }}</span>
                    <div class="worker-action-content">
                        <details v-if="row.invocation" class="worker-command" :class="{ 'has-purpose': !!row.explanation }">
                            <summary>
                                <code :title="row.invocation">{{ row.invocation }}</code>
                                <span v-if="row.explanation" class="worker-action-purpose" :title="row.explanation">{{ row.explanation }}</span>
                                <span v-if="row.step.durationMs != null" class="worker-action-duration">{{ (row.step.durationMs / 1000).toFixed(1) }}s</span>
                            </summary>
                            <pre class="worker-command-full">{{ row.invocation }}</pre>
                        </details>
                        <p v-else class="worker-action-purpose" :title="row.explanation">{{ row.explanation }}</p>
                    </div>
                </div>
                <p v-if="!workerSteps.length || worker.livePreview || !memberActive" class="worker-current-action">{{ activity }}</p>
                <p v-if="workerThoughtPreview" class="worker-thought-fragment"><span class="worker-thought-source">{{ worker.livePreview?.streamType === 'thinking' && memberActive ? '思考片段' : '此前思考' }}</span>{{ workerThoughtPreview }}</p>
            </div>
            <details v-if="worker.thought" class="review-diagnostics" @toggle="fullThoughtOpen = ($event.target as HTMLDetailsElement).open">
                <summary>完整思考</summary>
                <pre v-if="fullThoughtOpen" class="worker-full-thought">{{ worker.thought }}</pre>
            </details>
            <details v-if="workerModel" class="review-diagnostics">
                <summary>
                    <span>审查详情</span>
                    <span class="review-model-pill">{{ workerModel }}</span>
                </summary>
                <div class="review-diagnostic-content">
                    <p>执行模型：{{ workerModel }}</p>
                    <p v-if="worker.currentTool">子任务类型：{{ worker.currentTool }}</p>
                    <p v-if="worker.steps?.length">已上报步骤：{{ worker.steps.length }} 步</p>
                </div>
            </details>
        </div>
    </article>
</template>

<script setup lang="ts">
import { computed, inject, nextTick, ref, watch } from 'vue';
import type { AgentWorkerItem } from './types';
import { workerStepInvocation } from './workerUtils';
import { isReviewTerminal, type ReviewProgressItem } from './reviewProgress';
import { reviewDiagnosis, reviewStatusLabel } from './reviewDiagnostics';
import { stopReviewKey } from './reviewCancellation';
const props = withDefaults(defineProps<{ worker?: AgentWorkerItem; review?: ReviewProgressItem; isRunning?: boolean }>(), { isRunning: false });
defineEmits<{ inspect: [] }>();
const stopReview = inject(stopReviewKey, null);
const stopping = ref(false);
const stopError = ref('');
const canStopReview = computed(() => !!stopReview && memberActive.value && (props.review || props.worker?.role === 'reviewer'));
async function stopThisReview() {
    const id = props.review?.id || props.worker?.id;
    if (!stopReview || !id || stopping.value) return;
    stopping.value = true;
    stopError.value = '';
    try { await stopReview(id); }
    catch (error) { stopError.value = String(error); }
    finally { stopping.value = false; }
}
const reviewActive = computed(() => !!props.review && props.isRunning && !isReviewTerminal(props.review.phase));
const memberActive = computed(() => props.review ? reviewActive.value : props.isRunning && !props.worker?.reviewStopped && (props.worker?.status === 'running' || props.worker?.status === 'pending'));
const manualExpanded = ref<boolean | null>(null);
const fullThoughtOpen = ref(false);
const expanded = computed({ get: () => manualExpanded.value ?? memberActive.value, set: value => { manualExpanded.value = value; } });
watch(() => props.review?.id || props.worker?.id, () => { manualExpanded.value = null; fullThoughtOpen.value = false; followActivity.value = true; });
watch(memberActive, (active, previous) => { if (!active && previous) { manualExpanded.value = null; fullThoughtOpen.value = false; } });
watch(expanded, value => { if (!value) fullThoughtOpen.value = false; });
const workerSteps = computed(() => props.worker?.steps?.slice(-4) || []);
const workerActionRows = computed(() => workerSteps.value.map(step => {
    const invocation = workerStepInvocation(step);
    return { step, invocation, explanation: step.description || (step.title !== invocation ? step.title : '') || '' };
}));
// Only real thinking, never raw structured output; keep the default DOM bounded.
const workerThoughtPreview = computed(() => (props.worker?.thought || (props.worker?.livePreview?.streamType === 'thinking' ? props.worker.livePreview.text : '')).slice(-4000));
const activityPane = ref<HTMLElement | null>(null);
const followActivity = ref(true);
function onActivityScroll() {
    const el = activityPane.value;
    if (el) followActivity.value = el.scrollHeight - el.clientHeight - el.scrollTop < 12;
}
const reviewActivities = computed(() => props.review?.activities?.length ? props.review.activities : props.review ? [{ phase: props.review.phase, description: props.review.description, elapsedMs: props.review.elapsedMs, round: props.review.round, toolSteps: props.review.toolSteps }] : []);
watch(() => [reviewActivities.value, expanded.value, workerThoughtPreview.value, props.worker?.currentStep, props.worker?.currentTool, props.worker?.livePreview, workerSteps.value], async () => {
    await nextTick();
    const el = activityPane.value;
    if (el && followActivity.value && typeof el.scrollHeight === 'number') el.scrollTop = el.scrollHeight;
}, { flush: 'post', immediate: true });
const label = computed(() => props.review?.label || props.worker?.roleLabel || props.worker?.name || (props.review ? '终审成员' : '子 Agent'));
const workerModel = computed(() => props.worker?.model || props.worker?.extra?.model || '');
const displayModel = computed(() => props.review?.model || workerModel.value || '');
const durationMs = computed(() => props.review?.elapsedMs ?? props.worker?.durationMs);
const diagnosis = computed(() => props.review ? reviewDiagnosis(props.review, props.isRunning) : null);
const rowStatus = computed(() => {
    if (!props.review) return !props.isRunning && (props.worker?.status === 'running' || props.worker?.status === 'pending') ? 'challenged' : props.worker?.status || 'pending';
    if (props.review.phase === 'passed') return 'succeeded';
    if (props.review.phase === 'rejected') return 'failed';
    if (diagnosis.value?.tone === 'warning') return 'challenged';
    return isReviewTerminal(props.review.phase) ? 'pending' : 'running';
});
const statusText = computed(() => props.review ? reviewStatusLabel(props.review, props.isRunning) : props.worker?.reviewStopped ? '已停止 · 本任务排除' : !props.isRunning && (props.worker?.status === 'running' || props.worker?.status === 'pending') ? '未完成' : ({ running: '执行中', pending: '等待中', succeeded: '已完成', failed: '失败', challenged: '待核验' }[rowStatus.value]));
const activity = computed(() => {
    if (props.review) {
        return statusText.value;
    }
    const worker = props.worker;
    if (!worker) return statusText.value;
    if (worker.reviewStopped) return '已停止 · 本任务排除';
    if (!props.isRunning && (worker.status === 'running' || worker.status === 'pending')) return statusText.value;
    if (worker.status === 'running' && worker.livePreview) {
        return worker.livePreview.streamType === 'thinking' ? '正在思考' : '正在生成结论';
    }
    return worker.currentStep || worker.currentTool || statusText.value;
});
</script>

<style scoped>
.worker-stop-review { flex-shrink: 0; padding: 2px 5px; border: 1px solid #e6a23c40; border-radius: 4px; background: transparent; color: var(--theme-status-warning, #d4ad73); font: inherit; font-size: 10px; cursor: pointer; }
.worker-stop-review:hover { background: #e6a23c12; }
.worker-stop-review:disabled { opacity: .6; cursor: wait; }
.worker-stop-review:focus-visible { outline: 2px solid #d4ad73; outline-offset: 2px; }
.review-stop-error { margin: 4px 0; color: var(--theme-status-danger, #f49087); font-size: 11px; }
.worker-progress {
    min-width: 0;
    font-size: 11.5px;
    line-height: 1.5;
    color: var(--theme-status-info, #94a3b8);
    container-type: inline-size;
}

.worker-progress-line {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    min-height: 24px;
}

/* 状态指示器：微型旋转环或矢量状态图标 */
.worker-status {
    width: 14px;
    height: 14px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    line-height: 1;
}

.worker-spinner-ring {
    width: 11px;
    height: 11px;
    border: 1.5px solid rgba(56, 189, 248, 0.25);
    border-top-color: #38bdf8;
    border-radius: 50%;
    animation: worker-spin 0.8s linear infinite;
}

@keyframes worker-spin {
    to { transform: rotate(360deg); }
}

.worker-status-svg {
    width: 12px;
    height: 12px;
}

.worker-status-svg.is-success {
    color: var(--theme-status-success, #34d399);
}

.worker-status-svg.is-failed {
    color: var(--theme-status-danger, #f87171);
}

.worker-status-svg.is-warning {
    color: var(--theme-status-warning, #fbbf24);
}

.worker-status-dot {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: #64748b;
    opacity: 0.6;
}

.is-review {
    margin: 1px 0;
}

button {
    font: inherit;
    cursor: pointer;
    border-radius: 3px;
}

/* 身份标识微徽章：审查专属盾牌微标与平滑微光 */
.worker-identity {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 1;
    min-width: 0;
    max-width: 45%;
    padding: 1px 6px;
    border: 1px solid rgba(165, 180, 252, 0.16);
    background: rgba(165, 180, 252, 0.06);
    color: var(--theme-status-accent, #c7d2fe);
    font-size: 11px;
    font-weight: 550;
    letter-spacing: 0.1px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    border-radius: 4px;
    transition: all 0.15s ease;
}

.worker-identity:hover {
    background: rgba(165, 180, 252, 0.12);
    border-color: rgba(165, 180, 252, 0.3);
    color: var(--theme-text-primary, #e0e7ff);
}

.worker-identity.is-review-member {
    background: linear-gradient(135deg, rgba(99, 102, 241, 0.12), rgba(139, 92, 246, 0.08));
    border-color: rgba(165, 180, 252, 0.22);
    color: var(--theme-status-accent, #c7d2fe);
}

.member-shield-icon {
    color: var(--theme-status-accent, #a5b4fc);
    flex-shrink: 0;
}

.worker-resumed {
    flex-shrink: 0;
    color: var(--theme-status-accent, #a5b4fc);
    font-size: 9.5px;
    padding: 0 4px;
    background: rgba(165, 180, 252, 0.12);
    border-radius: 3px;
}

.worker-activity {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 11.5px;
    color: var(--theme-status-info, #94a3b8);
    transition: color 0.12s ease;
}

.status-running .worker-activity {
    color: var(--theme-text-primary, #cbd5e1);
}

.worker-metrics {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
    white-space: nowrap;
}

/* 速度微标：青蓝极客风格微标签 */
.worker-speed {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding: 0.5px 5px;
    border-radius: 3px;
    background: rgba(56, 189, 248, 0.08);
    border: 1px solid rgba(56, 189, 248, 0.18);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 10px;
    color: var(--theme-status-info, #38bdf8);
    font-weight: 500;
    font-variant-numeric: tabular-nums;
}

.speed-bolt-icon {
    color: var(--theme-status-info, #38bdf8);
    flex-shrink: 0;
}

/* 耗时后置微标 */
.worker-duration {
    flex-shrink: 0;
    font-size: 10.5px;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    color: var(--theme-status-warning, #eab308);
    font-weight: 500;
}

/* 展开微按钮 */
.worker-thought-toggle {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    flex-shrink: 0;
    padding: 1.5px 6px;
    border: 1px solid rgba(var(--theme-foreground-rgb), 0.06);
    background: rgba(var(--theme-foreground-rgb), 0.03);
    color: var(--theme-status-info, #94a3b8);
    font-size: 10.5px;
    border-radius: 3px;
    transition: all 0.12s ease;
}

.worker-thought-toggle:hover {
    color: var(--theme-text-primary, #cbd5e1);
    background: rgba(var(--theme-foreground-rgb), 0.08);
    border-color: rgba(var(--theme-foreground-rgb), 0.12);
}

.toggle-arrow {
    transition: transform 0.18s ease;
}

.worker-thought-toggle.is-open .toggle-arrow {
    transform: rotate(180deg);
}

/* 展开抽屉面板：深色毛玻璃科技感卡片 */
.review-live-body {
    margin: 4px 0 6px 14px;
    padding: 8px 10px;
    border-left: 2px solid #818cf8;
    background: rgba(var(--theme-rgb-panel, 15, 23, 42), 0.45);
    border-radius: 0 6px 6px 0;
    border-top: 1px solid rgba(var(--theme-foreground-rgb), 0.04);
    border-right: 1px solid rgba(var(--theme-foreground-rgb), 0.04);
    border-bottom: 1px solid rgba(var(--theme-foreground-rgb), 0.04);
}

/* 统计微卡片网格 */
.review-stats-grid {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
    margin-bottom: 6px;
}

.stat-chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 1.5px 6px;
    background: rgba(var(--theme-foreground-rgb), 0.04);
    border: 1px solid rgba(var(--theme-foreground-rgb), 0.06);
    border-radius: 4px;
    font-size: 10.5px;
    font-variant-numeric: tabular-nums;
}

.stat-k {
    color: var(--theme-status-info, #64748b);
    font-weight: 500;
}

.stat-v {
    color: var(--theme-text-primary, #cbd5e1);
    font-weight: 500;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

.stat-chip.speed-chip {
    background: rgba(56, 189, 248, 0.06);
    border-color: rgba(56, 189, 248, 0.15);
}

.stat-chip.speed-chip .stat-v {
    color: var(--theme-status-info, #38bdf8);
}

.stat-chip.warn-chip {
    background: rgba(245, 158, 11, 0.08);
    border-color: rgba(245, 158, 11, 0.2);
}

.stat-chip.warn-chip .stat-v {
    color: var(--theme-status-warning, #fbbf24);
}

/* 实时动作滚动列表 */
.review-activity-scroll {
    max-height: 80px;
    overflow-y: auto;
    overscroll-behavior: contain;
    scrollbar-width: thin;
    margin-top: 6px;
    padding: 4px 8px;
    background: var(--theme-inset-bg, rgba(0, 0, 0, 0.2));
    border-radius: 4px;
    border: 1px solid rgba(var(--theme-foreground-rgb), 0.04);
}

.review-activity-event {
    display: flex;
    align-items: baseline;
    gap: 8px;
    margin: 0;
    padding: 2.5px 0;
    line-height: 1.4;
    font-size: 11px;
    color: var(--theme-text-primary, #cbd5e1);
}

.review-activity-event:not(:last-child) {
    opacity: 0.75;
}

.review-activity-event time {
    flex-shrink: 0;
    min-width: 30px;
    color: var(--theme-status-info, #64748b);
    font-size: 10px;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-variant-numeric: tabular-nums;
}

.review-activity-event span {
    min-width: 0;
    overflow-wrap: anywhere;
}

/* 诊断折叠栏 */
.review-diagnostics {
    margin-top: 6px;
    padding-top: 4px;
    border-top: 1px solid rgba(var(--theme-foreground-rgb), 0.05);
    font-size: 10.5px;
    color: var(--theme-status-info, #94a3b8);
}

.review-diagnostics summary {
    cursor: pointer;
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--theme-status-accent, #a5b4fc);
    font-weight: 500;
    padding: 2px 0;
}

.review-model-pill {
    margin-left: 6px;
    padding: 0.5px 5px;
    background: rgba(167, 139, 250, 0.1);
    border: 1px solid rgba(167, 139, 250, 0.22);
    border-radius: 3px;
    color: var(--theme-status-accent, #c4b5fd);
    font-size: 10px;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

.worker-line-model {
    margin-left: 0;
    flex-shrink: 0;
    max-width: 140px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.review-diagnostic-content {
    max-height: 160px;
    overflow: auto;
    overflow-wrap: anywhere;
    padding: 4px 0;
}

.diag-banner {
    padding: 3px 6px;
    border-radius: 3px;
    background: rgba(56, 189, 248, 0.08);
    border: 1px solid rgba(56, 189, 248, 0.18);
    color: var(--theme-status-info, #7dd3fc);
    margin: 4px 0;
}

.diag-banner.tone-warning {
    background: rgba(245, 158, 11, 0.1);
    border-color: rgba(245, 158, 11, 0.25);
    color: var(--theme-status-warning, #fbbf24);
}

.diag-banner.tone-risk {
    background: rgba(239, 68, 68, 0.1);
    border-color: rgba(239, 68, 68, 0.25);
    color: var(--theme-status-danger, #f87171);
}

.diag-banner.tone-success {
    background: rgba(16, 185, 129, 0.1);
    border-color: rgba(16, 185, 129, 0.25);
    color: var(--theme-status-success, #34d399);
}

.diag-notice {
    color: var(--theme-status-danger, #f87171);
    margin: 4px 0;
}

.diag-caption {
    color: var(--theme-status-info, #64748b);
    font-size: 10px;
    margin: 4px 0;
}

/* 子任务专用细节 */
.worker-activity-scroll {
    max-height: 72px;
}

.worker-action-event {
    margin-bottom: 2px;
}

.worker-action-content {
    min-width: 0;
    flex: 1;
}

.worker-action-purpose {
    flex: 1;
    min-width: 0;
    margin: 0;
    line-height: 20px;
    color: var(--theme-status-info, #94a3b8);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}

.worker-action-duration {
    flex-shrink: 0;
    color: var(--theme-status-info, #64748b);
    font-size: 10px;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

.worker-command {
    min-width: 0;
    color: var(--theme-status-info, #7e8fa7);
}

.worker-command summary {
    display: flex;
    align-items: center;
    gap: 6px;
    cursor: pointer;
    min-width: 0;
    line-height: 20px;
}

.worker-command summary::before {
    content: '›';
    flex-shrink: 0;
}

.worker-command[open] summary::before {
    content: '⌄';
}

.worker-command code {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font: 10.5px/20px ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    color: var(--theme-text-primary, #cbd5e1);
}

.worker-command.has-purpose code {
    flex: 0 1 auto;
    max-width: 45%;
}

.worker-command-full {
    margin: 3px 0;
    padding: 5px 7px;
    max-height: 120px;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font: 10.5px/1.6 ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    background: var(--theme-inset-bg, rgba(0, 0, 0, 0.25));
    border-radius: 4px;
    user-select: text;
}

.worker-step-index {
    flex-shrink: 0;
    min-width: 18px;
    color: var(--theme-status-info, #64748b);
    font-size: 10px;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-variant-numeric: tabular-nums;
}

.worker-current-action,
.worker-thought-fragment {
    margin: 0;
    padding: 0;
    line-height: 20px;
    color: var(--theme-text-primary, #cbd5e1);
    overflow-wrap: anywhere;
}

.worker-thought-fragment {
    white-space: pre-wrap;
    color: var(--theme-status-info, #94a3b8);
    user-select: text;
}

.worker-thought-source {
    margin-right: 6px;
    color: var(--theme-status-info, #64748b);
    font-size: 10px;
    user-select: none;
}

.worker-full-thought {
    max-height: 160px;
    overflow: auto;
    margin: 5px 0 0;
    font: inherit;
    line-height: 1.65;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    color: var(--theme-text-primary, #cbd5e1);
    user-select: text;
}

@container (max-width: 440px) {
    .worker-progress-line { flex-wrap: wrap; gap: 4px 6px; }
    .worker-identity { max-width: calc(100% - 52px); }
    .worker-activity { flex-basis: 40%; }
    .worker-metrics { margin-left: auto; }
    .worker-thought-toggle { margin-left: auto; }
    .review-live-body { margin-left: 8px; padding: 5px 8px; }
}
</style>
