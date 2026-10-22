<template>
    <article class="worker-progress" :class="[`status-${rowStatus}`, { 'is-review': !!review, 'review-live': memberActive }]" :data-worker-id="worker?.id" :data-review-id="review?.id">
        <div class="worker-progress-line">
            <span class="worker-status" :aria-label="statusText">{{ statusIcon }}</span>
            <button class="worker-identity" type="button" :title="label" :aria-expanded="review ? expanded : undefined" @click="review ? expanded = !expanded : $emit('inspect')">{{ label }}</button>
            <span v-if="worker?.extra?.resumed" class="worker-resumed">续接</span>
            <span class="worker-activity" :title="activity">{{ activity }}</span>
            <span v-if="review?.lastRequestTokensPerSecond != null || durationMs != null" class="worker-metrics">
                <span v-if="review?.lastRequestTokensPerSecond != null" class="worker-speed" :title="`最近已结束的第 ${review.lastRequestRound} 次请求输出均速；不是当前实时速度，token 按供应商口径`">最近 {{ review.lastRequestTokensPerSecond.toFixed(1) }} tok/s</span>
                <span v-if="durationMs != null" class="worker-duration">{{ (durationMs / 1000).toFixed(1) }}s</span>
            </span>
            <button type="button" class="worker-thought-toggle" :aria-expanded="expanded" @click="expanded = !expanded">{{ expanded ? '收起' : '展开' }}详情</button>
        </div>
        <div v-if="expanded && review" class="review-live-body">
            <div class="review-stats" aria-label="实际请求与取证统计">
                <span>模型请求 {{ review.round }} 次</span>
                <span v-if="review.toolBudget > 0" title="只限制取证工具调用次数，不是总请求数或完成进度；重试、补正与最终结论另计">取证已用 {{ review.toolSteps }} / 上限 {{ review.toolBudget }} 次</span>
                <span v-else>取证已关闭</span>
                <span>本次片段 {{ review.receivedChars.toLocaleString() }} 字符</span>
                <span v-if="review.timeoutRetries">重试 {{ review.timeoutRetries }}</span>
                <span v-if="review.formatCorrections">补正 {{ review.formatCorrections }}</span>
            </div>
            <div class="review-stats review-speed-stats" aria-label="最近结束请求的模型速度">
                <span v-if="review.lastRequestTokensPerSecond != null" title="供应商回报的输出 token ÷ 首片段至请求结束的秒数；不含前置等待、工具时间和其他请求">最近请求均速 {{ review.lastRequestTokensPerSecond.toFixed(1) }} tok/s</span>
                <span v-else>{{ review.lastRequestRound != null || !reviewActive ? 'token 速度未提供' : 'token 速度待回报' }}</span>
                <span v-if="review.lastRequestOutputTokens != null">输出 {{ review.lastRequestOutputTokens.toLocaleString() }} token</span>
                <span v-if="review.lastRequestTtftMs != null">首片段 {{ (review.lastRequestTtftMs / 1000).toFixed(1) }}s</span>
                <span v-if="review.lastRequestRound != null">第 {{ review.lastRequestRound }} 次请求</span>
            </div>
            <div ref="activityPane" class="review-activity-scroll soft-scrollbar" role="region" :aria-label="`${label} · 实时动作`" tabindex="0" @scroll.passive="onActivityScroll" @wheel.passive="followActivity = false" @touchmove.passive="followActivity = false">
                <p v-for="(event, index) in reviewActivities" :key="`${review.id}-${index}`" class="review-activity-event">
                    <time>{{ (event.elapsedMs / 1000).toFixed(1) }}s</time><span>{{ event.description || statusText }}</span>
                </p>
            </div>
            <details v-if="!reviewActive || review.lastNotice" class="review-diagnostics">
                <summary>诊断详情<span class="review-model">{{ review.model }}</span></summary>
                <div class="review-diagnostic-content">
                    <p v-if="diagnosis">{{ diagnosis.title }} · {{ diagnosis.detail }}</p>
                    <p>第 {{ review.round }} 次逻辑请求 · 取证已用 {{ review.toolSteps }} / 上限 {{ review.toolBudget }} 次</p>
                    <p>超时重发 {{ review.timeoutRetries ?? '未知' }} 次 · 格式补正 {{ review.formatCorrections ?? '未知' }} 次</p>
                    <p v-if="review.lastNotice">最近异常：{{ review.lastNotice }}</p>
                    <p>片段数不是 token 数或吞吐速度；耗时以最后事件为准，成员通过不代表整个任务已经交付。</p>
                </div>
            </details>
        </div>
        <div v-if="expanded && worker" class="review-live-body worker-live-body">
            <div v-if="worker.steps?.length" class="review-stats" aria-label="已上报的子任务步骤">
                <span>已记录 {{ worker.steps.length }} 步</span>
                <span v-if="worker.currentTool">当前工具 {{ worker.currentTool }}</span>
            </div>
            <div ref="activityPane" class="review-activity-scroll worker-activity-scroll soft-scrollbar" role="region" :aria-label="`${label} · 实时动作与思考片段`" tabindex="0" @scroll.passive="onActivityScroll" @wheel.passive="followActivity = false" @touchmove.passive="followActivity = false">
                <div v-for="row in workerActionRows" :key="row.step.stepNumber" class="review-activity-event worker-action-event">
                    <span class="worker-step-index">{{ row.step.stepNumber }}</span>
                    <div class="worker-action-content">
                        <p class="worker-action-purpose">{{ row.step.description || row.step.title || row.step.tool }}<span v-if="row.step.durationMs != null" class="worker-action-duration"> · {{ (row.step.durationMs / 1000).toFixed(1) }}s</span></p>
                        <details v-if="row.invocation && row.invocation !== (row.step.description || row.step.title)" class="worker-command">
                            <summary :title="row.invocation"><code>{{ row.invocation }}</code></summary>
                            <pre class="worker-command-full">{{ row.invocation }}</pre>
                        </details>
                    </div>
                </div>
                <p v-if="!workerSteps.length || worker.livePreview || !memberActive" class="worker-current-action">{{ activity }}</p>
                <p v-if="workerThoughtPreview" class="worker-thought-fragment"><span class="worker-thought-source">{{ worker.livePreview?.streamType === 'thinking' && memberActive ? '思考片段' : '此前思考' }}</span>{{ workerThoughtPreview }}</p>
            </div>
            <details v-if="worker.thought" class="review-diagnostics" @toggle="fullThoughtOpen = ($event.target as HTMLDetailsElement).open">
                <summary>完整思考</summary>
                <pre v-if="fullThoughtOpen" class="worker-full-thought">{{ worker.thought }}</pre>
            </details>
        </div>
    </article>
</template>

<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue';
import type { AgentWorkerItem } from './types';
import { workerStepInvocation } from './workerUtils';
import { isReviewTerminal, type ReviewProgressItem } from './reviewProgress';
import { reviewDiagnosis, reviewStatusLabel } from './reviewDiagnostics';
const props = withDefaults(defineProps<{ worker?: AgentWorkerItem; review?: ReviewProgressItem; isRunning?: boolean }>(), { isRunning: false });
defineEmits<{ inspect: [] }>();
const reviewActive = computed(() => !!props.review && props.isRunning && !isReviewTerminal(props.review.phase));
const memberActive = computed(() => props.review ? reviewActive.value : props.isRunning && (props.worker?.status === 'running' || props.worker?.status === 'pending'));
const manualExpanded = ref<boolean | null>(null);
const fullThoughtOpen = ref(false);
const expanded = computed({ get: () => manualExpanded.value ?? memberActive.value, set: value => { manualExpanded.value = value; } });
watch(() => props.review?.id || props.worker?.id, () => { manualExpanded.value = null; fullThoughtOpen.value = false; followActivity.value = true; });
watch(memberActive, (active, previous) => { if (!active && previous) { manualExpanded.value = null; fullThoughtOpen.value = false; } });
watch(expanded, value => { if (!value) fullThoughtOpen.value = false; });
const workerSteps = computed(() => props.worker?.steps?.slice(-4) || []);
const workerActionRows = computed(() => workerSteps.value.map(step => ({ step, invocation: workerStepInvocation(step) })));
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
const durationMs = computed(() => props.review?.elapsedMs ?? props.worker?.durationMs);
const diagnosis = computed(() => props.review ? reviewDiagnosis(props.review, props.isRunning) : null);
const rowStatus = computed(() => {
    if (!props.review) return !props.isRunning && (props.worker?.status === 'running' || props.worker?.status === 'pending') ? 'challenged' : props.worker?.status || 'pending';
    if (props.review.phase === 'passed') return 'succeeded';
    if (props.review.phase === 'rejected') return 'failed';
    if (diagnosis.value?.tone === 'warning') return 'challenged';
    return isReviewTerminal(props.review.phase) ? 'pending' : 'running';
});
const statusText = computed(() => props.review ? reviewStatusLabel(props.review, props.isRunning) : !props.isRunning && (props.worker?.status === 'running' || props.worker?.status === 'pending') ? '未完成' : ({ running: '执行中', pending: '等待中', succeeded: '已完成', failed: '失败', challenged: '待核验' }[rowStatus.value]));
const statusIcon = computed(() => ({ running: '◌', pending: '○', succeeded: '✓', failed: '✕', challenged: '!' }[rowStatus.value]));
const activity = computed(() => {
    if (props.review) {
        return statusText.value;
    }
    const worker = props.worker;
    if (!worker) return statusText.value;
    if (!props.isRunning && (worker.status === 'running' || worker.status === 'pending')) return statusText.value;
    if (worker.status === 'running' && worker.livePreview) {
        return worker.livePreview.streamType === 'thinking' ? '正在思考' : '正在生成结论';
    }
    return worker.currentStep || worker.currentTool || statusText.value;
});
</script>

<style scoped>
.worker-progress { min-width: 0; font-size: 11.5px; line-height: 1.5; color: #94a3b8; container-type: inline-size; }
.worker-progress-line { display: flex; align-items: center; gap: 6px; min-width: 0; min-height: 24px; }
.worker-status { width: 12px; flex-shrink: 0; color: #94a3b8; text-align: center; }
.status-running .worker-status { color: #38bdf8; }
.status-succeeded .worker-status { color: #34d399; }
.status-failed .worker-status { color: #f87171; }
.status-challenged .worker-status { color: #fbbf24; }
.is-review { margin: 1px 0; }
.review-live .worker-status { animation: review-breathe 1.5s ease-in-out infinite; }
.review-live-body { margin: 2px 0 6px 18px; padding: 6px 10px; border-left: 2px solid rgba(94,159,232,.4); background: linear-gradient(90deg, rgba(94,159,232,.055), rgba(94,159,232,.015)); border-radius: 0 6px 6px 0; }
.review-stats { display: flex; flex-wrap: wrap; column-gap: 12px; row-gap: 2px; color: #a8b4c6; font-size: 10.5px; font-variant-numeric: tabular-nums; }
.review-activity-scroll { max-height: 66px; overflow-y: auto; overscroll-behavior: contain; scrollbar-width: thin; margin-top: 5px; padding: 0; outline-offset: 2px; }
.review-activity-event { display: flex; gap: 8px; margin: 0; padding: 2px 0; line-height: 18px; color: #cbd5e1; }
.review-activity-event time { flex-shrink: 0; min-width: 30px; color: #94a3b8; font-size: 10px; font-variant-numeric: tabular-nums; }
.review-activity-event span { min-width: 0; overflow-wrap: anywhere; }
.review-activity-event:not(:last-child) { opacity: .7; }
.review-diagnostics { margin-top: 3px; color: #a8b4c6; font-size: 10.5px; }
.review-diagnostics summary { cursor: pointer; padding: 2px 0; }
.review-model { margin-left: 8px; overflow-wrap: anywhere; }
.review-diagnostic-content { max-height: 160px; overflow: auto; overflow-wrap: anywhere; }
.review-diagnostic-content p { margin: 4px 0; }
@keyframes review-breathe { 50% { opacity: .45; } }
@media (prefers-reduced-motion: reduce) { .review-live .worker-status { animation: none; } }
button { font: inherit; cursor: pointer; border-radius: 3px; }
.worker-identity { flex-shrink: 1; min-width: 0; max-width: 45%; padding: 0 4px; border: 1px solid rgba(165,180,252,.15); background: rgba(165,180,252,.06); color: #c7d2fe; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.worker-activity { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.worker-speed { flex-shrink: 0; font-size: 10px; color: #a8b4c6; font-variant-numeric: tabular-nums; }
.worker-metrics { display: inline-flex; align-items: center; gap: 6px; flex-shrink: 0; white-space: nowrap; }
.worker-resumed { flex-shrink: 0; color: #a5b4fc; font-size: 10px; }
.review-speed-stats { margin-top: 2px; }
.worker-duration { flex-shrink: 0; font-size: 10.5px; font-family: ui-monospace, monospace; color: #64748b; }
.worker-thought-toggle { flex-shrink: 0; padding: 1px 4px; border: 0; background: transparent; color: #94a3b8; }
button:hover { color: #f1f5f9; background: rgba(255,255,255,.08); }
button:focus-visible { outline: 1px solid #818cf8; outline-offset: 2px; }
.worker-activity-scroll { max-height: 112px; }
.worker-activity-scroll .review-activity-event { padding: 0; line-height: 22px; }
.worker-action-event { margin-bottom: 5px; }
.worker-action-content { min-width: 0; flex: 1; }
.worker-action-purpose { margin: 0; line-height: 19px; color: #d1d8e3; overflow-wrap: anywhere; }
.worker-action-duration { color: #718198; font-size: 10px; }
.worker-command { min-width: 0; color: #7e8fa7; }
.worker-command summary { display: flex; align-items: center; gap: 4px; cursor: pointer; min-width: 0; line-height: 17px; }
.worker-command summary::before { content: '›'; flex-shrink: 0; }
.worker-command[open] summary::before { content: '⌄'; }
.worker-command code { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font: 10.5px/17px ui-monospace, monospace; }
.worker-command-full { margin: 3px 0; padding: 5px 7px; max-height: 120px; overflow: auto; white-space: pre-wrap; overflow-wrap: anywhere; font: 10.5px/1.6 ui-monospace, monospace; background: rgba(0,0,0,.14); border-radius: 4px; user-select: text; }
.worker-step-index { flex-shrink: 0; min-width: 18px; color: #94a3b8; font-size: 10px; font-variant-numeric: tabular-nums; }
.worker-current-action, .worker-thought-fragment { margin: 0; padding: 0; line-height: 22px; color: #cbd5e1; overflow-wrap: anywhere; }
.worker-thought-fragment { white-space: pre-wrap; color: #a8b4c6; user-select: text; }
.worker-thought-source { margin-right: 6px; color: #718198; font-size: 10px; user-select: none; }
.worker-full-thought { max-height: 160px; overflow: auto; margin: 5px 0 0; font: inherit; line-height: 1.65; white-space: pre-wrap; overflow-wrap: anywhere; color: #cbd5e1; user-select: text; }
@container (max-width: 440px) {
    .worker-progress-line { flex-wrap: wrap; gap: 4px 6px; }
    .worker-identity { max-width: calc(100% - 52px); }
    .worker-activity { flex-basis: 40%; }
    .worker-metrics { margin-left: auto; }
    .worker-thought-toggle { margin-left: auto; }
    .review-live-body { margin-left: 12px; padding: 5px 8px; }
}
</style>
