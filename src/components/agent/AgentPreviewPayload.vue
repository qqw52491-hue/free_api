<template>
    <details class="preview-payload" :data-section="section.id" @toggle="setOpen">
        <summary>{{ section.label }}</summary>
        <template v-if="open">
            <pre v-if="text" ref="payloadText" tabindex="0" :aria-label="section.label">{{ visibleText }}</pre>
            <p v-else class="preview-empty">{{ section.emptyText }}</p>
            <button v-if="visibleText.length < text.length" type="button" class="more-output" @click="showMore">
                显示更多（剩余 {{ text.length - visibleText.length }} 字符）
            </button>
        </template>
    </details>
</template>

<script lang="ts">
export interface AgentPreviewSection {
    id: string;
    label: string;
    value: unknown;
    emptyText: string;
}
</script>

<script setup lang="ts">
import { computed, ref, useTemplateRef } from "vue";
import { formatPayload } from "./format";

const props = defineProps<{ section: AgentPreviewSection }>();
const chunkSize = 16_000;
const open = ref(false);
const limit = ref(chunkSize);
const payloadText = useTemplateRef("payloadText");
const text = computed(() => props.section.value != null ? formatPayload(props.section.value, Infinity) : "");
const visibleText = computed(() => {
    const next = text.value.charCodeAt(limit.value);
    const last = text.value.charCodeAt(limit.value - 1);
    const splitsPair = last >= 0xd800 && last <= 0xdbff && next >= 0xdc00 && next <= 0xdfff;
    return text.value.slice(0, limit.value - Number(splitsPair));
});

function setOpen(event: Event) {
    open.value = (event.currentTarget as HTMLDetailsElement).open;
}

function showMore() {
    const nextLimit = limit.value + chunkSize;
    if (text.value.length <= nextLimit) payloadText.value?.focus({ preventScroll: true });
    limit.value = nextLimit;
}
</script>

<style scoped>
.preview-payload {
    border: 1px solid var(--theme-border, #272d37);
    border-radius: 6px;
    background: color-mix(in srgb, var(--theme-bg-base, #15181e) 42%, transparent);
}
.preview-payload summary { cursor: pointer; padding: 7px 9px; font-weight: 600; }
.preview-payload summary:focus-visible, .preview-payload pre:focus-visible, .more-output:focus-visible {
    outline: 1px solid var(--theme-status-info, #62baf4);
    outline-offset: -1px;
}
.preview-payload pre {
    max-height: min(210px, 33vh);
    margin: 0;
    padding: 8px 9px;
    overflow: auto;
    overscroll-behavior: contain;
    border-top: 1px solid var(--theme-border, #272d37);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font: 11px/1.55 ui-monospace, SFMono-Regular, Consolas, monospace;
    color: var(--theme-text-primary, #f3f4f6);
}
.preview-empty { margin: 0; padding: 3px 9px 8px; color: var(--theme-text-secondary, #a4a9b5); }
.more-output {
    margin: 6px 9px;
    padding: 2px 0;
    border: 0;
    background: transparent;
    color: var(--theme-status-info, #62baf4);
    font-size: 11px;
    cursor: pointer;
}
</style>
