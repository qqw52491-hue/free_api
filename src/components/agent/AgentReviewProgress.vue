<template>
    <section v-if="reviews.length" class="review-progress" aria-label="终审进度与诊断">
        <header>
            <strong>{{ reviews.length > 1 ? '联合终审' : '终审' }} · {{ reviews.length }} 位成员</strong>
            <button type="button" class="diagnostic-button" @click="copyDiagnostics">复制脱敏诊断</button>
        </header>
        <p v-if="copyStatus" class="copy-status" role="status">{{ copyStatus }}</p>
        <div v-if="manualCopy" class="manual-copy">
            <label for="review-diagnostic-copy">复制未成功，请选择下面的脱敏摘要手动复制</label>
            <textarea id="review-diagnostic-copy" readonly :value="manualCopy" @focus="selectDiagnostic" />
        </div>
        <article v-for="review in reviews" :key="review.id" class="review-member" :data-tone="reviewDiagnosis(review, isRunning)?.tone || 'neutral'">
            <div class="review-heading">
                <i :class="{ 'is-active': isRunning && !isReviewTerminal(review.phase), 'is-passed': review.phase === 'passed' }" />
                <span class="review-identity" :title="review.label">{{ review.label }}</span>
                <span class="review-activity" role="status">{{ reviewStatusLabel(review, isRunning) }}</span>
                <time>{{ Math.floor(review.elapsedMs / 1000) }} 秒</time>
            </div>
            <p class="review-action" role="status">{{ reviewDiagnosis(review, isRunning)?.title || review.description || '等待当前审查阶段的进度事件' }}</p>
            <p v-if="reviewDiagnosis(review, isRunning)" class="review-result">{{ reviewDiagnosis(review, isRunning)?.detail }}</p>
            <div class="review-stats" aria-label="当前成员审查统计">
                <span>第 {{ review.round }} 次逻辑请求</span>
                <span :title="'取证预算不是主 Agent 总步数，不代表完成百分比'">已取证 {{ review.toolSteps }}/{{ review.toolBudget }} 步</span>
                <span v-if="review.phase === 'responding' && isRunning">本次已收到 {{ review.receivedChars }} 字符片段</span>
                <span v-if="review.timeoutRetries">超时重发 {{ review.timeoutRetries }} 次</span>
                <span v-if="review.formatCorrections">格式补正 {{ review.formatCorrections }} 次</span>
            </div>
            <p v-if="review.lastNotice && review.lastNotice !== review.description" class="review-notice">最近异常：{{ review.lastNotice }}</p>
            <details class="diagnostic-details">
                <summary>查看诊断详情</summary>
                <div class="review-expanded">
                <p class="review-model">模型：{{ review.model || '未知' }}</p>
                <dl>
                    <div><dt>逻辑请求</dt><dd>{{ review.round }} 次</dd></div>
                    <div><dt>超时重发</dt><dd>{{ review.timeoutRetries === null ? '未知' : `${review.timeoutRetries} 次` }}</dd></div>
                    <div><dt>格式补正</dt><dd>{{ review.formatCorrections === null ? '未知' : `${review.formatCorrections} 次` }}</dd></div>
                    <div><dt>取证工具调用</dt><dd>{{ review.toolSteps }}/{{ review.toolBudget }} 次</dd></div>
                </dl>
                <p class="review-caption">取证预算不是主 Agent 总步数；格式补正、超时重试另计。逻辑请求不等于 HTTP 尝试。仅当前成员最新尝试，耗时以最后事件为准。</p>
                </div>
            </details>
        </article>
    </section>
</template>
<script setup lang="ts">
import { ref, watch } from 'vue';
import { isReviewTerminal, type ReviewProgressItem } from './reviewProgress';
import { reviewDiagnosis, reviewStatusLabel, buildReviewDiagnostics, copyReviewDiagnostics } from './reviewDiagnostics';
const props = defineProps<{ reviews: ReviewProgressItem[]; isRunning: boolean }>();

const copyStatus = ref('');
const manualCopy = ref('');
watch(() => props.reviews.map(review => review.id).join('|'), () => { copyStatus.value = ''; manualCopy.value = ''; });
async function copyDiagnostics() {
    const identity = props.reviews.map(review => review.id).join('|');
    const snapshot = buildReviewDiagnostics(props.reviews, props.isRunning);
    const copied = await copyReviewDiagnostics(snapshot);
    if (identity !== props.reviews.map(review => review.id).join('|')) return;
    copyStatus.value = copied ? '已复制脱敏诊断（点击时的观测快照）' : '未能访问剪贴板，可手动复制下方摘要。';
    manualCopy.value = copied ? '' : snapshot;
}
function selectDiagnostic(event: FocusEvent) { (event.target as HTMLTextAreaElement).select(); }
</script>
<style scoped>
.review-progress { --review-muted: var(--text-secondary, #526174); --review-warning: #946014; --review-risk: #b43b32; flex: 0 0 auto; margin: 8px 0; padding: 0; border: 0; background: transparent; font-size: 14px; line-height: 1.5; }
.review-expanded { max-height: 320px; overflow-y: auto; padding: 4px 12px; border-left: 2px solid var(--border-color, #b7c2ce); scrollbar-width: thin; }
header, .review-heading { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
header { justify-content: space-between; margin-bottom: 4px; }
header strong { font-weight: 600; }
header > div { display: flex; flex-direction: column; gap: 4px; }
.review-caption, .review-stats, .review-model, time { color: var(--review-muted); font-size: 14px; }
.review-heading { min-height: 24px; gap: 6px 8px; }
.review-identity { min-width: 0; overflow-wrap: anywhere; font-weight: 600; }
.review-activity { color: var(--review-muted); padding: 1px 6px; border-radius: 4px; background: #64748b12; }
.review-model { overflow-wrap: anywhere; }
.review-member { margin-top: 8px; padding: 4px 0 4px 12px; border-left: 2px solid var(--border-color, #b7c2ce); }
.review-member[data-tone="success"] { border-left-color: #27804d; }
.review-member[data-tone="risk"] { border-left-color: var(--review-risk); }
.review-member[data-tone="warning"] { border-left-color: var(--review-warning); }
.review-member[data-tone="success"] .review-activity { color: #27804d; background: #27804d12; }
.review-action { margin: 4px 0; }
.review-result { color: var(--review-muted); }
.review-stats { display: flex; flex-wrap: wrap; gap: 2px 12px; font-variant-numeric: tabular-nums; }
.review-member[data-tone="risk"] .review-activity { color: var(--review-risk); }
.review-member[data-tone="warning"] .review-activity { color: var(--review-warning); }
time { flex-shrink: 0; margin-left: auto; font-variant-numeric: tabular-nums; }
p { margin: 6px 0; overflow-wrap: anywhere; }
i { width: 8px; height: 8px; flex: 0 0 8px; border-radius: 50%; background: #66778c; }
i.is-active { background: #2783de; animation: review-pulse 1.5s ease-in-out infinite; }
i.is-passed { background: #27804d; }
.review-notice, [data-tone="warning"] .diagnostic-title { color: var(--review-warning); }
[data-tone="risk"] .diagnostic-title { color: var(--review-risk); }
[data-tone="risk"] i { background: var(--review-risk); }
[data-tone="warning"] i { background: var(--review-warning); }
.diagnostic-title { font-weight: 600; }
.diagnostic-button { min-height: 44px; padding: 2px 6px; border: 0; border-radius: 3px; color: var(--review-muted); background: transparent; font: inherit; cursor: pointer; }
.diagnostic-button:hover { background: #64748b14; }
.diagnostic-button:focus-visible, summary:focus-visible, textarea:focus-visible { outline: 2px solid #2783de; outline-offset: 2px; }
summary { cursor: pointer; align-content: center; }
.diagnostic-details > summary { min-height: 44px; color: var(--review-muted); }
dl { margin: 0; display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 8px 16px; }
dl > div { display: flex; flex-wrap: wrap; gap: 8px; justify-content: space-between; }
dt { color: var(--review-muted); } dd { margin: 0; font-variant-numeric: tabular-nums; }
.manual-copy label { display: block; margin: 8px 0; }
textarea { width: 100%; box-sizing: border-box; min-height: 140px; padding: 8px; color: inherit; background: transparent; border: 1px solid var(--border-color, #7b8797); border-radius: 6px; font: inherit; }
.copy-status { color: var(--review-muted); }
@media (max-width: 480px) { .review-expanded { padding: 4px 8px; } dl { grid-template-columns: 1fr; } }
@media (prefers-color-scheme: dark) { .review-progress { --review-muted: #b5beca; --review-warning: #e7b86a; --review-risk: #f49087; border-color: var(--border-color, #677484); }
.review-member[data-tone="success"] .review-activity { color: #72bc8f; } }
@keyframes review-pulse { 50% { opacity: .35; } }
@media (prefers-reduced-motion: reduce) { i.is-active { animation: none; } }
</style>
