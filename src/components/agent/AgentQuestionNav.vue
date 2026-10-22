<template>
    <nav v-if="marks.length" class="question-nav" aria-label="问题导航" :style="{ '--question-count': marks.length }">
        <div class="question-nav-track" @mouseleave="hidePreview">
            <button
                v-for="(mark, index) in marks" :key="mark.id"
                type="button" class="question-nav-tick"
                :class="{ 'is-active': activeId === mark.id, 'is-previewed': previewId === mark.id, 'is-pending': mark.pending }"
                :aria-label="`第 ${index + 1} 个问题：${mark.question}`"
                :aria-current="activeId === mark.id ? 'location' : undefined"
                :aria-describedby="previewId === mark.id ? tooltipId : undefined"
                @mouseenter="showPreview(mark.id, $event)"
                @focus="showPreview(mark.id, $event)"
                @blur="hidePreview"
                @keydown.esc="hidePreview"
                @click="$emit('select', mark.id)"
            ><span></span></button>
        </div>
        <Teleport to="body">
            <div v-if="preview" :id="tooltipId" role="tooltip" class="question-nav-preview" :style="previewStyle">
                <div class="question-nav-preview-heading">问题 {{ previewIndex + 1 }} <span>点击跳转</span></div>
                <p class="question-nav-preview-question">{{ preview.question }}</p>
                <p class="question-nav-preview-answer" :class="{ 'is-empty': !preview.hasAnswer }">{{ questionAnswerPreview(preview) }}</p>
            </div>
        </Teleport>
    </nav>
</template>

<script setup lang="ts">
import { computed, ref, useId } from 'vue';
import { questionAnswerPreview, type QuestionMark } from './questionNavigation';

const props = defineProps<{ marks: QuestionMark[]; activeId: string | null }>();
defineEmits<{ select: [id: string] }>();
const tooltipId = `question-preview-${useId()}`;
const previewId = ref<string | null>(null);
const previewStyle = ref<Record<string, string>>({});
const previewIndex = computed(() => props.marks.findIndex(mark => mark.id === previewId.value));
const preview = computed(() => props.marks[previewIndex.value]);

function showPreview(id: string, event: Event) {
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    const width = Math.min(288, window.innerWidth - 24);
    previewStyle.value = {
        width: `${width}px`,
        left: `${Math.max(12, rect.left - width - 12)}px`,
        top: `${Math.max(12, Math.min(rect.top - 48, window.innerHeight - 174))}px`,
    };
    previewId.value = id;
}
function hidePreview() { previewId.value = null; }
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
    background: #606570;
    transition: width .12s ease, background .12s ease;
}
.question-nav-tick.is-active span { width: 10px; background: #a9b7ca; }
.question-nav-tick.is-pending span { background: #8099b5; }
.question-nav-tick:hover span, .question-nav-tick.is-previewed span { width: 12px; background: #d9e5f3; }
.question-nav-tick:focus-visible { outline: 1px solid #8da9cc; outline-offset: -1px; border-radius: 3px; }
.question-nav-preview {
    position: fixed;
    z-index: 1000;
    box-sizing: border-box;
    padding: 11px 13px;
    border: 1px solid #3a3e47;
    border-radius: 9px;
    color: #e4e4e7;
    background: #22242a;
    box-shadow: 0 8px 28px #0005;
    font: 12px/1.55 -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
    pointer-events: none;
}
.question-nav-preview-heading { display: flex; justify-content: space-between; color: #9199a7; font-size: 10px; }
.question-nav-preview-heading span { color: #697383; }
.question-nav-preview p { display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 2; overflow: hidden; margin: 6px 0 0; overflow-wrap: anywhere; }
.question-nav-preview-question { font-weight: 600; }
.question-nav-preview-answer { color: #b1b7c2; border-top: 1px solid #ffffff0d; padding-top: 6px; }
.question-nav-preview-answer.is-empty { color: #7b8594; }
@media (prefers-reduced-motion: reduce) { .question-nav-tick span { transition: none; } }
</style>
