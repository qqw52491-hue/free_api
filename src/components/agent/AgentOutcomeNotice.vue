<template>
    <section class="outcome-notice" :class="[`is-${kind}`, { 'is-compact': isCompact }]" role="status" :aria-label="title">
        <header class="outcome-heading">
            <span class="outcome-icon" aria-hidden="true">
                <svg v-if="kind === 'cancelled'" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round">
                    <path d="M9 6v12M15 6v12" />
                </svg>
                <svg v-else width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
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
const fullText = computed(() => {
    const content = props.content.trim();
    // Older and live run records may quote the entire next goal in the stop reason.
    // The next round already displays that text as its user message.
    if (props.kind === 'cancelled'
        && content.startsWith('用户提交了新的任务目标：')
        && content.endsWith(' 已在安全边界停止旧任务。')) {
        return '收到新的任务目标，本轮已在安全边界停止。';
    }
    return content || (props.kind === 'cancelled' ? '任务已停止。' : '本次运行未能完成。');
});
const hasDetails = computed(() => fullText.value.length > 900 || fullText.value.split('\n').length > 12);
const isCompact = computed(() => !hasDetails.value && !fullText.value.includes('\n') && !fullText.value.includes('```') && fullText.value.length <= 140);

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
    --outcome-accent: var(--theme-status-danger, #f87171);
    --outcome-border: color-mix(in srgb, var(--outcome-accent) 24%, transparent);
    --outcome-bg: color-mix(in srgb, var(--outcome-accent) 5%, var(--theme-bg-panel, transparent));
    margin-top: 4px;
    padding: 10px 14px;
    min-width: 0;
    border: 1px solid var(--outcome-border);
    border-radius: 10px;
    background: var(--outcome-bg);
    color: var(--theme-text-primary);
}
.outcome-notice.is-cancelled {
    --outcome-accent: var(--theme-status-warning, #dfb575);
}
.outcome-notice.is-compact {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 12px;
    padding: 7px 12px;
    border-radius: 9px;
}
.outcome-heading {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 6px;
}
.outcome-notice.is-compact .outcome-heading {
    margin-bottom: 0;
    flex-shrink: 0;
    padding-right: 12px;
    border-right: 1px solid rgba(var(--theme-foreground-rgb), .1);
}
.outcome-icon {
    display: grid;
    place-items: center;
    flex: 0 0 20px;
    height: 20px;
    border-radius: 50%;
    color: var(--outcome-accent);
    background: color-mix(in srgb, var(--outcome-accent) 14%, transparent);
}
.outcome-labels {
    display: flex;
    flex-wrap: nowrap;
    align-items: center;
    gap: 6px;
    min-width: 0;
    white-space: nowrap;
}
.outcome-labels h3 {
    margin: 0;
    font-size: 12.5px;
    font-weight: 600;
    line-height: 1.35;
    color: var(--theme-text-primary, #e4e4e7);
}
.outcome-caption {
    color: var(--theme-text-muted);
    font-size: 11.5px;
    line-height: 1.35;
}
.outcome-body, .outcome-detail-body {
    min-width: 0;
    overflow-wrap: anywhere;
    font-size: 12.5px;
    line-height: 1.5;
    color: var(--theme-text-secondary, #a9b1c1);
}
.outcome-notice.is-compact .outcome-body {
    flex: 1;
    font-size: 12.5px;
}
.outcome-notice :deep(.markdown-segment *) { animation: none; }
.outcome-body :deep(.markdown-content > :first-child), .outcome-detail-body :deep(.markdown-content > :first-child) { margin-top: 0; }
.outcome-body :deep(.markdown-content > :last-child), .outcome-detail-body :deep(.markdown-content > :last-child) { margin-bottom: 0; }
.outcome-details { margin-top: 8px; border-top: 1px solid rgba(var(--theme-foreground-rgb), .08); width: 100%; }
.outcome-details > summary {
    display: flex; align-items: center; gap: 6px; width: fit-content;
    padding: 8px 0 0; list-style: none; color: var(--theme-text-secondary);
    cursor: pointer; font-size: 11.5px;
}
.outcome-details > summary::-webkit-details-marker { display: none; }
.outcome-details > summary svg { transition: transform .15s ease; }
.outcome-details[open] > summary svg { transform: rotate(90deg); }
.outcome-details > summary:hover { color: var(--theme-text-primary); }
.outcome-details > summary:focus-visible { outline: 2px solid var(--outcome-accent); outline-offset: 4px; border-radius: 2px; }
.outcome-detail-body { margin-top: 8px; max-height: 420px; overflow: auto; }
@media (max-width: 560px) {
    .outcome-notice { padding: 10px 12px; }
    .outcome-notice.is-compact .outcome-heading { border-right: 0; padding-right: 0; }
}
@media (prefers-reduced-motion: reduce) { .outcome-details > summary svg { transition: none; } }
</style>
