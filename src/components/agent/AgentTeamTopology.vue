<template>
    <section class="team-map" aria-label="多智能体协作图">
        <header class="map-heading">
            <h3>协作图</h3>
            <span>{{ summary }}</span>
        </header>
        <button class="map-node root-node" type="button" :aria-pressed="selectedId === 'master'" @click="$emit('select', 'master')">
            <span class="node-marker" :data-status="masterStatus" aria-hidden="true"></span>
            <span class="node-copy"><strong>主 Agent</strong><small v-if="goal" :title="goal">{{ goal }}</small></span>
            <span class="node-state">{{ workerStatusText(masterStatus) }}</span>
        </button>
        <div v-if="rows.length" class="map-branches">
            <template v-for="(row, index) in rows" :key="row.worker.id">
                <div v-if="!row.connected && (index === 0 || rows[index - 1].connected)" class="unlinked">父级未记录或不在当前范围</div>
                <div class="branch" :class="{ related: row.depth > 0 }" :style="{ '--indent': Math.min(row.depth, 6) * 14 + 'px' }">
                    <button
                        class="map-node worker-node"
                        type="button"
                        :title="row.worker.name"
                        :aria-pressed="selectedId === row.worker.id"
                        @click="$emit('select', row.worker.id)"
                    >
                        <span class="node-marker" :data-status="row.worker.waitingForHuman ? 'waiting' : row.worker.status" aria-hidden="true"></span>
                        <span class="node-copy">
                            <strong>{{ row.worker.name }}</strong>
                            <small v-if="row.worker.currentStep || row.worker.currentTool">{{ row.worker.currentStep || row.worker.currentTool }}</small>
                        </span>
                        <span class="node-meta">
                            <span class="node-state">{{ status(row.worker) }}</span>
                            <small :title="[row.worker.roleLabel, row.worker.model].filter(Boolean).join(' · ')">{{ row.worker.roleLabel }}</small>
                        </span>
                    </button>
                </div>
            </template>
        </div>
        <p v-else class="map-empty">当前没有子任务</p>
    </section>
</template>

<script setup lang="ts">
import { computed } from "vue";
import type { AgentToolStatus, AgentWorkerItem } from "./types";
import { workerStatusText } from "./workerUtils";
import { workerTopology } from "./workerTopology";

const props = defineProps<{ workers: AgentWorkerItem[]; masterStatus: AgentToolStatus; goal?: string; selectedId: string }>();
defineEmits<{ select: [id: string] }>();
const rows = computed(() => workerTopology(props.workers));
function status(worker: AgentWorkerItem) {
    if (worker.reviewStopped) return "已停止";
    if (worker.waitingForHuman) return "等回答";
    return workerStatusText(worker.status);
}
const summary = computed(() => {
    const counts = new Map<string, number>();
    for (const worker of props.workers) {
        const key = status(worker);
        counts.set(key, (counts.get(key) ?? 0) + 1);
    }
    return Array.from(counts, ([label, count]) => `${label} ${count}`).join(" · ");
});
</script>

<style scoped>
.team-map {
    --map-line: rgba(var(--theme-foreground-rgb), .18);
    --map-focus: var(--theme-status-info, #4ba6dc);
    border: 1px solid var(--map-line);
    border-radius: 10px;
    padding: 12px;
    display: grid;
    gap: 9px;
    background: var(--theme-bg-panel, #171a20);
    color: var(--theme-text-primary, #f3f4f6);
}
.map-heading { display: flex; flex-wrap: wrap; align-items: baseline; justify-content: space-between; gap: 4px 12px; }
.map-heading h3 { margin: 0; font-size: 14px; }
.map-heading span, .map-empty, .unlinked, .node-copy small, .node-meta small {
    color: var(--theme-text-secondary, #a4a9b5);
    font-size: 11px;
}
.map-node {
    display: grid;
    grid-template-columns: 8px minmax(0, 1fr) auto;
    gap: 8px;
    align-items: center;
    width: 100%;
    min-height: 44px;
    text-align: start;
    padding: 7px 9px;
    color: inherit;
    background: var(--theme-bg-raised, #20232a);
    border: 1px solid var(--map-line);
    border-radius: 7px;
    cursor: pointer;
    user-select: none;
    -webkit-user-select: none;
}
.map-node:hover, .map-node[aria-pressed="true"] { border-color: var(--map-focus); }
.map-node:focus-visible { outline: 2px solid var(--map-focus); outline-offset: 2px; }
.node-copy, .node-meta { display: grid; gap: 3px; min-width: 0; }
.node-copy > *, .node-meta small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.node-copy strong { font-size: 12px; }
.node-meta { justify-items: end; max-width: 93px; }
.node-state { font-size: 11px; white-space: nowrap; }
.node-marker { width: 7px; height: 7px; border-radius: 50%; background: var(--theme-text-secondary, #a4a9b5); }
.node-marker[data-status="running"] { background: var(--map-focus); }
.node-marker[data-status="succeeded"] { background: var(--theme-status-success, #36c295); }
.node-marker[data-status="failed"] { background: var(--theme-status-danger, #e77879); }
.node-marker[data-status="waiting"], .node-marker[data-status="challenged"] { background: var(--theme-status-warning, #d9a44d); }
.map-branches { display: grid; gap: 5px; max-height: 44vh; overflow: auto; padding: 3px; }
.branch { position: relative; margin-inline-start: var(--indent); }
.branch.related::before {
    content: "";
    position: absolute;
    inset-inline-start: -9px;
    top: -7px;
    width: 8px;
    height: calc(50% + 7px);
    border-inline-start: 1px solid var(--map-line);
    border-bottom: 1px solid var(--map-line);
    border-end-start-radius: 4px;
    pointer-events: none;
}
.unlinked { padding: 4px 0 1px; }
.map-empty { margin: 2px 0; }
</style>
