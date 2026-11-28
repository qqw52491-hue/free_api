<template>
    <nav v-if="marks.length" class="question-nav" aria-label="问题导航" :style="{ '--question-count': marks.length }">
        <div class="question-nav-track" data-preview-viewport @keydown="previewCard?.escape($event)">
            <button
                v-for="(mark, index) in marks" :key="mark.id"
                type="button" class="question-nav-tick"
                :data-preview-id="mark.id"
                :class="{ 'is-active': activeId === mark.id, 'is-pending': mark.pending }"
                :aria-label="`第 ${index + 1} 个问题：${mark.question}`"
                :aria-current="activeId === mark.id ? 'location' : undefined"
                @pointerenter="enterTick"
                @pointerleave="leaveTick"
                @focus="focusTick"
                @blur="previewCard?.dismiss()"
                @click="selectMark(mark.id)"
            ><span data-preview-point></span></button>
        </div>
        <AgentAnchoredPreview ref="previewCard" :describe="describeMark" />
    </nav>
</template>

<script setup lang="ts">
import { useTemplateRef } from 'vue';
import AgentAnchoredPreview, { type AgentPreviewDetails } from './AgentAnchoredPreview.vue';
import { questionAnswerPreview, type QuestionMark } from './questionNavigation';

const props = defineProps<{ marks: QuestionMark[]; activeId: string | null }>();
const emit = defineEmits<{ select: [id: string] }>();
const previewCard = useTemplateRef('previewCard');

function describeMark(id: string): AgentPreviewDetails | undefined {
    const index = props.marks.findIndex(mark => mark.id === id);
    const preview = props.marks[index];
    if (!preview) return undefined;
    return {
        title: preview.question,
        subtitle: `第 ${index + 1} 轮 · 点击刻度跳转`,
        status: preview.hasAnswer ? '已回答' : preview.pending ? '处理中' : '暂无回答',
        tone: preview.hasAnswer ? 'succeeded' : preview.pending ? 'running' : 'idle',
        rows: preview.hasAnswer ? [{ label: '回答摘要', value: questionAnswerPreview(preview) }] : [],
    };
}

function enterTick(event: PointerEvent) {
    if (event.pointerType !== 'touch') previewCard.value?.show(event.currentTarget as HTMLElement);
}

function leaveTick(event: PointerEvent) {
    if (event.pointerType !== 'touch') previewCard.value?.leave(event.currentTarget as HTMLElement);
}

function focusTick(event: FocusEvent) {
    const tick = event.currentTarget as HTMLElement;
    if (tick.matches(':focus-visible')) previewCard.value?.show(tick, true);
}

function selectMark(id: string) {
    previewCard.value?.dismiss();
    emit('select', id);
}
</script>

<style scoped>
.question-nav {
    position: absolute;
    top: 50%;
    right: 20px;
    height: 0;
    width: 18px;
    z-index: 15;
}
.question-nav-track {
    position: absolute;
    right: 0;
    top: 0;
    transform: translateY(-50%);
    max-height: 58vh;
    overflow-y: auto;
    scrollbar-width: none;
}
.question-nav-track::-webkit-scrollbar { display: none; }
.question-nav-tick {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    height: clamp(4px, calc(58vh / var(--question-count)), 14px);
    width: 18px;
    padding: 0 3px;
    border: 0;
    background: transparent;
    cursor: pointer;
}
.question-nav-tick span {
    width: 6px;
    height: 2px;
    border-radius: 2px;
    background: var(--theme-bg-muted, #606570);
    transition: width .12s ease, background .12s ease;
}
.question-nav-tick.is-active span { width: 10px; background: #a9b7ca; }
.question-nav-tick.is-pending span { background: #8099b5; }
.question-nav-tick:hover span, .question-nav-tick[data-preview="true"] span { width: 12px; background: var(--theme-inverse-bg, #d9e5f3); }
.question-nav-tick[data-preview="true"] {
    border-radius: 4px;
    background: color-mix(in srgb, var(--theme-status-info, #8ab9e8) 13%, transparent);
    box-shadow: inset 0 1px 3px #080d1745;
}
.question-nav-tick:focus-visible { outline: 1px solid #8da9cc; outline-offset: -1px; border-radius: 3px; }
@media (prefers-reduced-motion: reduce) { .question-nav-tick span { transition: none; } }
</style>
