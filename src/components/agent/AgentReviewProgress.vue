<template>
    <section v-if="reviews.length" class="review-progress" aria-label="终审进度">
        <header><strong>终审进度</strong><span>阶段与实际计时 · 非完成百分比</span></header>
        <article v-for="review in reviews" :key="review.memberKey" :class="{ 'is-failed': ['failed', 'incomplete', 'rejected'].includes(review.phase) }">
            <div class="review-heading">
                <i :class="{ 'is-active': isRunning && !isReviewTerminal(review.phase), 'is-passed': review.phase === 'passed' }" />
                <strong>{{ review.label }}</strong><span class="review-model">{{ review.model }}</span>
                <time>{{ Math.floor(review.elapsedMs / 1000) }} 秒</time>
            </div>
            <p role="status">{{ !isRunning && !isReviewTerminal(review.phase) ? '运行已结束，未收到该成员的最终审查状态' : review.description }}</p>
            <p v-if="review.lastNotice && review.lastNotice !== review.description" class="review-notice">最近异常：{{ review.lastNotice }}</p>
            <small>第 {{ review.round }} 次逻辑请求 · 已取证 {{ review.toolSteps }}/{{ review.toolBudget }} 步<span v-if="review.phase === 'responding'"> · 本次已收到 {{ review.receivedChars }} 字符片段</span></small>
        </article>
    </section>
</template>
<script setup lang="ts">
import { isReviewTerminal, type ReviewProgressItem } from './reviewProgress';
defineProps<{ reviews: ReviewProgressItem[]; isRunning: boolean }>();
</script>
<style scoped>
.review-progress { flex: 0 0 auto; max-height: 320px; overflow-y: auto; margin: 8px 16px; padding: 10px 14px; border: 1px solid var(--border-color, #64748b40); border-radius: 10px; background: var(--bg-secondary, #64748b0a); font-size: 12px; }
header, .review-heading { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
header { justify-content: space-between; margin-bottom: 6px; }
header span, small, .review-model, time { opacity: .7; }
article { padding: 8px 0; }
article + article { border-top: 1px solid #64748b26; }
.review-heading strong { overflow-wrap: anywhere; }
.review-model { overflow-wrap: anywhere; }
time { margin-left: auto; font-variant-numeric: tabular-nums; }
p { margin: 5px 0; line-height: 1.6; overflow-wrap: anywhere; }
i { width: 7px; height: 7px; flex: 0 0 7px; border-radius: 50%; background: #94a3b8; }
i.is-active { background: #60a5fa; animation: review-pulse 1.5s ease-in-out infinite; }
i.is-passed { background: #34b981; }
.review-notice { color: var(--warning-color, #b97b27); font-size: 11px; }
.is-failed i { background: #d99b35; }
@keyframes review-pulse { 50% { opacity: .35; } }
@media (prefers-reduced-motion: reduce) { i.is-active { animation: none; } }
</style>
