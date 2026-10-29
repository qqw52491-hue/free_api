<template>
    <section class="outcome-notice" :class="`is-${kind}`" role="status" :aria-label="title">
        <header class="outcome-heading">
            <span class="outcome-icon" aria-hidden="true">
                <svg v-if="kind === 'cancelled'" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round">
                    <path d="M9 6v12M15 6v12" />
                </svg>
                <svg v-else width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
                    <path d="M12 8v5m0 3h.01" /><circle cx="12" cy="12" r="9" />
                </svg>
            </span>
            <div class="outcome-labels">
                <h3>{{ title }}</h3>
                <span class="outcome-caption">本轮尚未完成交付</span>
            </div>
        </header>
        <div class="outcome-body">
            <RichContentRenderer :content="preview" :compact-html="true" @open-preview="$emit('open-preview', $event)" />
        </div>
        <details v-if="hasDetails" class="outcome-details" :open="expanded" @toggle="onToggle">
            <summary>
                <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"><path d="m6 3 5 5-5 5" /></svg>
                <span>{{ expanded ? '收起完整信息' : '查看完整信息' }}</span>
            </summary>
            <div v-if="expanded" class="outcome-detail-body">
                <RichContentRenderer :content="detailContent" :compact-html="true" @open-preview="$emit('open-preview', $event)" />
            </div>
        </details>
    </section>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import RichContentRenderer from '../charts/RichContentRenderer.vue';

const props = defineProps<{ kind: 'cancelled' | 'failed'; content: string }>();
defineEmits<{ 'open-preview': [code: string] }>();
const expanded = ref(false);
const title = computed(() => props.kind === 'cancelled' ? '任务已停止' : '任务未能完成');
const fullText = computed(() => props.content.trim() || (props.kind === 'cancelled' ? '任务已停止。' : '本次运行未能完成。'));
const hasDetails = computed(() => fullText.value.length > 900 || fullText.value.split('\n').length > 12);

// Raw JSON and stack traces get code formatting; normal explanations retain Markdown.
function formatted(text: string): string {
    let body = text;
    let language = 'text';
    let diagnostic = /(^|\n)\s*(?:at\s+[\w.$]+\(|Traceback \(most recent call last\)|Caused by:|\[(?:ERROR|FATAL)\])/.test(text);
    if (/^[\[{]/.test(text.trim())) {
        try { body = JSON.stringify(JSON.parse(text), null, 2); language = 'json'; diagnostic = true; } catch { /* ordinary prose */ }
    }
    if (!diagnostic || (language === 'text' && text.includes('```'))) return text;
    let fenceLength = 3;
    for (const match of body.matchAll(/`+/g)) fenceLength = Math.max(fenceLength, match[0].length + 1);
    const fence = '`'.repeat(fenceLength);
    return `${fence}${language}\n${body}\n${fence}`;
}
const preview = computed(() => {
    if (!hasDetails.value) return formatted(fullText.value);
    const paragraph = fullText.value.split(/\n\s*\n/)[0].split('\n')[0].trim();
    // A report is available in full below; keep the default view short and readable.
    const characters = Array.from(paragraph);
    return characters.length > 220 ? `${characters.slice(0, 220).join('')}…` : paragraph;
});
const detailContent = computed(() => formatted(fullText.value));
function onToggle(event: Event) { expanded.value = (event.target as HTMLDetailsElement).open; }
watch(() => [props.kind, props.content], () => { expanded.value = false; });
</script>

<style scoped>
.outcome-notice {
    --outcome-accent: var(--theme-status-danger, #c7364d);
    margin-top: 6px;
    padding: 16px 18px;
    min-width: 0;
    border: 1px solid rgba(var(--theme-foreground-rgb), .10);
    border-left: 3px solid var(--outcome-accent);
    border-radius: 12px;
    background: rgba(var(--theme-foreground-rgb), .025);
    color: var(--theme-text-primary);
}
.outcome-notice.is-cancelled { --outcome-accent: var(--theme-status-warning, #96600f); }
.outcome-heading { display: flex; align-items: center; gap: 11px; margin-bottom: 12px; }
.outcome-icon {
    display: grid; place-items: center; flex: 0 0 34px; height: 34px;
    border-radius: 10px; color: var(--outcome-accent);
    background: rgba(var(--theme-foreground-rgb), .045);
}
.outcome-labels { display: flex; flex-wrap: wrap; align-items: baseline; gap: 5px 12px; min-width: 0; }
.outcome-labels h3 { margin: 0; font-size: 14px; font-weight: 600; line-height: 1.5; }
.outcome-caption { color: var(--theme-text-muted); font-size: 12px; line-height: 1.5; }
.outcome-body, .outcome-detail-body { min-width: 0; overflow-wrap: anywhere; font-size: 13px; line-height: 1.7; }
.outcome-notice :deep(.markdown-segment *) { animation: none; }
.outcome-body :deep(.markdown-content > :first-child), .outcome-detail-body :deep(.markdown-content > :first-child) { margin-top: 0; }
.outcome-body :deep(.markdown-content > :last-child), .outcome-detail-body :deep(.markdown-content > :last-child) { margin-bottom: 0; }
.outcome-details { margin-top: 12px; border-top: 1px solid rgba(var(--theme-foreground-rgb), .08); }
.outcome-details > summary {
    display: flex; align-items: center; gap: 6px; width: fit-content;
    padding: 11px 0 0; list-style: none; color: var(--theme-text-secondary);
    cursor: pointer; font-size: 12px;
}
.outcome-details > summary::-webkit-details-marker { display: none; }
.outcome-details > summary svg { transition: transform .15s ease; }
.outcome-details[open] > summary svg { transform: rotate(90deg); }
.outcome-details > summary:hover { color: var(--theme-text-primary); }
.outcome-details > summary:focus-visible { outline: 2px solid var(--outcome-accent); outline-offset: 4px; border-radius: 2px; }
.outcome-detail-body { margin-top: 12px; max-height: 420px; overflow: auto; }
@media (max-width: 560px) {
    .outcome-notice { padding: 14px; }
    .outcome-labels { flex-direction: column; gap: 2px; }
}
@media (prefers-reduced-motion: reduce) { .outcome-details > summary svg { transition: none; } }
</style>
