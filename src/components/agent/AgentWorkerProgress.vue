<template>
    <article class="worker-progress" :class="`status-${worker.status}`" :data-worker-id="worker.id">
        <div class="worker-progress-line">
            <span class="worker-status" :aria-label="statusText">{{ statusIcon }}</span>
            <button class="worker-identity" type="button" @click="$emit('inspect')">{{ worker.roleLabel || worker.name || '子 Agent' }}</button>
            <span class="worker-activity" :title="activity">{{ activity }}</span>
            <span v-if="worker.durationMs != null" class="worker-duration">{{ (worker.durationMs / 1000).toFixed(1) }}s</span>
            <button v-if="worker.thought" type="button" class="worker-thought-toggle" :aria-expanded="expanded" @click="expanded = !expanded">{{ expanded ? '收起思考' : '展开思考' }}</button>
        </div>
        <div v-if="worker.status === 'running' && worker.livePreview?.streamType === 'thinking'" class="worker-thought-preview" :title="worker.livePreview.text">{{ worker.livePreview.text }}</div>
        <div v-if="expanded && worker.thought" class="worker-thought-detail">
            <span class="worker-thought-label">{{ worker.roleLabel || worker.name }} · 思考过程</span>
            <pre>{{ worker.thought }}</pre>
        </div>
    </article>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue';
import type { AgentWorkerItem } from './types';
const props = defineProps<{ worker: AgentWorkerItem }>();
defineEmits<{ inspect: [] }>();
const expanded = ref(false);
const statusText = computed(() => ({ running: '执行中', pending: '等待中', succeeded: '已完成', failed: '失败', challenged: '待核验' }[props.worker.status]));
const statusIcon = computed(() => ({ running: '◌', pending: '○', succeeded: '✓', failed: '✕', challenged: '!' }[props.worker.status]));
const activity = computed(() => {
    const worker = props.worker;
    if (worker.status === 'running' && worker.livePreview) {
        return worker.livePreview.streamType === 'thinking' ? '正在思考' : '正在生成结论';
    }
    return worker.currentStep || worker.currentTool || statusText.value;
});
</script>

<style scoped>
.worker-progress { min-width: 0; font-size: 11.5px; line-height: 1.5; color: #94a3b8; }
.worker-progress-line { display: flex; align-items: center; gap: 6px; min-width: 0; min-height: 24px; }
.worker-status { width: 12px; flex-shrink: 0; color: #94a3b8; text-align: center; }
.status-running .worker-status { color: #38bdf8; }
.status-succeeded .worker-status { color: #34d399; }
.status-failed .worker-status { color: #f87171; }
button { font: inherit; cursor: pointer; border-radius: 3px; }
.worker-identity { flex-shrink: 1; min-width: 0; max-width: 45%; padding: 0 4px; border: 1px solid rgba(165,180,252,.15); background: rgba(165,180,252,.06); color: #c7d2fe; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.worker-activity { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.worker-duration { flex-shrink: 0; font-size: 10.5px; font-family: ui-monospace, monospace; color: #64748b; }
.worker-thought-toggle { flex-shrink: 0; padding: 1px 4px; border: 0; background: transparent; color: #94a3b8; }
button:hover { color: #f1f5f9; background: rgba(255,255,255,.08); }
button:focus-visible { outline: 1px solid #818cf8; outline-offset: 2px; }
.worker-thought-preview { margin-left: 18px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: #a1a1aa; }
.worker-thought-detail { margin: 4px 0 6px 18px; padding: 8px 10px; background: rgba(255,255,255,.025); border: 1px solid rgba(255,255,255,.07); border-radius: 5px; }
.worker-thought-label { font-size: 10.5px; color: #94a3b8; }
pre { max-height: 220px; overflow: auto; margin: 5px 0 0; font: inherit; line-height: 1.65; white-space: pre-wrap; overflow-wrap: anywhere; color: #cbd5e1; user-select: text; }
</style>
