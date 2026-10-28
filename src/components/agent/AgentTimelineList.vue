<template>
    <div v-if="entries.length" ref="listElement" class="agent-timeline-list">
        <div v-if="hiddenCount" class="history-load-actions" role="group" aria-label="加载历史记录">
            <button type="button" @click="loadEarlier">加载更早记录（{{ hiddenCount }} 条）</button>
            <button type="button" class="history-load-all" @click="showAll">全部展开</button>
        </div>
        <template v-for="entry in visibleEntries" :key="entry.id">
            <slot :entry="entry" />
        </template>
    </div>
</template>

<script setup lang="ts" generic="T extends { id: string }">
import { computed, nextTick, ref, watch } from "vue";

const props = withDefaults(defineProps<{
    entries: readonly T[];
    lazy?: boolean;
    initialCount?: number;
    batchSize?: number;
    scrollElement?: HTMLElement | null;
    viewActive?: boolean;
}>(), { lazy: false, initialCount: 32, batchSize: 32, scrollElement: null, viewActive: true });
const emit = defineEmits<{ "before-expand": [] }>();
defineSlots<{ default(props: { entry: T }): unknown }>();

const listElement = ref<HTMLElement | null>(null);
const firstVisibleId = ref<string | null>(null);

// Keep the first mounted record by identity when completed runs are appended.
// Loading older records must not discard expanded cards or move the reading position.
watch(() => props.entries, entries => {
    if (!entries.length) firstVisibleId.value = null;
    else if (!firstVisibleId.value || !entries.some(entry => entry.id === firstVisibleId.value)) {
        firstVisibleId.value = entries[Math.max(0, entries.length - props.initialCount)].id;
    }
}, { immediate: true, flush: "sync" });

const hiddenCount = computed(() => {
    if (!props.lazy) return 0;
    const index = props.entries.findIndex(entry => entry.id === firstVisibleId.value);
    return Math.max(0, index);
});
const visibleEntries = computed(() => hiddenCount.value ? props.entries.slice(hiddenCount.value) : props.entries);

async function expandTo(index: number) {
    const entry = props.entries[index];
    if (!entry || index >= hiddenCount.value) return;
    emit("before-expand");
    const scroller = props.scrollElement;
    const canMeasure = props.viewActive && !!scroller?.clientHeight;
    const anchor = canMeasure ? listElement.value?.querySelector<HTMLElement>("[data-tl]") : null;
    const anchorTop = anchor?.getBoundingClientRect().top;
    const previousHeight = canMeasure ? scroller!.scrollHeight : 0;
    const previousTop = canMeasure ? scroller!.scrollTop : 0;
    firstVisibleId.value = entry.id;
    await nextTick();
    if (!canMeasure || !scroller?.isConnected || !props.viewActive) return;
    if (anchor?.isConnected && anchorTop !== undefined) {
        // Account for the browser's own scroll anchoring instead of applying the height twice.
        scroller.scrollTop += anchor.getBoundingClientRect().top - anchorTop;
    } else {
        scroller.scrollTop = previousTop + scroller.scrollHeight - previousHeight;
    }
}

function loadEarlier() { return expandTo(Math.max(0, hiddenCount.value - props.batchSize)); }
function showAll() { return expandTo(0); }
async function reveal(id: string): Promise<boolean> {
    const index = props.entries.findIndex(entry => entry.id === id);
    if (index < 0) return false;
    await expandTo(index);
    return true;
}
defineExpose({ reveal });
</script>

<style scoped>
.agent-timeline-list {
    display: flex;
    flex-direction: column;
    gap: var(--stream-entry-gap, 8px);
    width: 100%;
    min-width: 0;
}
.history-load-actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding: 10px 0 16px;
}
.history-load-actions button {
    padding: 6px 12px;
    border: 1px solid var(--theme-border, rgba(128, 128, 128, 0.2));
    border-radius: 999px;
    background: var(--theme-bg-raised, transparent);
    color: var(--theme-text-secondary, #8b8b8f);
    font: inherit;
    font-size: 12px;
    cursor: pointer;
}
.history-load-actions button:hover,
.history-load-actions button:focus-visible {
    color: var(--theme-text-primary, #e4e4e7);
    border-color: var(--theme-text-muted, #71717a);
}
.history-load-actions .history-load-all {
    border-color: transparent;
    background: transparent;
}
</style>
