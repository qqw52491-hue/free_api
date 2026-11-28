<template>
    <section class="team-map" aria-label="多智能体协作图">
        <header class="map-heading">
            <h3>协作图</h3>
            <span>{{ summary }}</span>
        </header>
        <div class="map-scroll" data-preview-viewport @pointerover="enterNode" @pointerout="leaveNode" @focusin="focusNode" @focusout="blurNode" @keydown="preview?.escape($event)">
            <button class="map-node root-node" data-preview-id="master" type="button" :aria-pressed="selectedId === 'master'" @click="$emit('select', 'master')">
                <span class="node-marker" :data-status="masterWait ? 'waiting' : masterStatus" aria-hidden="true"></span>
                <span class="node-copy">
                    <strong>主 Agent</strong>
                    <small v-if="goal">{{ goal }}</small>
                    <span v-if="masterWait" class="wait-line">{{ masterWait }}</span>
                </span>
                <span class="node-state" data-preview-align :class="{ 'is-waiting': masterWait }">{{ masterWait ? '等待中' : workerStatusText(masterStatus) }}</span>
            </button>
            <template v-for="(branches, group) in groups" :key="group">
                <template v-if="branches.length">
                    <p v-if="group === 'unlinked'" class="unlinked">父级未记录或不在当前范围</p>
                    <AgentTeamBranches :branches="branches" :unlinked="group === 'unlinked'" v-slot="{ worker }">
                        <button
                            class="map-node"
                            type="button"
                            :data-preview-id="worker.id"
                            :aria-pressed="selectedId === worker.id"
                            @click="$emit('select', worker.id)"
                        >
                            <span class="node-marker" :data-status="workerWaits.get(worker.id) ? 'waiting' : worker.status" aria-hidden="true"></span>
                            <span class="node-copy">
                                <strong>{{ worker.name }}</strong>
                                <span v-if="workerWaits.get(worker.id)" class="wait-line">{{ workerWaits.get(worker.id) }}</span>
                                <small v-if="!receiptViews.has(worker.id) && (worker.currentStep || worker.currentTool)">{{ worker.currentStep || worker.currentTool }}</small>
                                <span v-if="receiptViews.get(worker.id)" class="receipt-line" :data-state="receiptViews.get(worker.id)?.state">
                                    {{ receiptViews.get(worker.id)?.label }}
                                </span>
                            </span>
                            <span class="node-meta">
                                <span class="node-state" data-preview-align :class="{ 'is-waiting': workerWaits.has(worker.id) }">{{ status(worker) }}</span>
                                <small>{{ worker.roleLabel }}</small>
                            </span>
                        </button>
                    </AgentTeamBranches>
                </template>
            </template>
            <p v-if="!workers.length" class="map-empty">当前没有子任务</p>
        </div>
        <dl v-if="selectedReceipt && (selectedReceipt.summary || selectedReceipt.finishedAt || selectedReceipt.collectedAt)" class="receipt-detail">
            <div v-if="selectedReceipt.summary"><dt>结果摘要</dt><dd>{{ selectedReceipt.summary }}</dd></div>
            <div v-if="selectedReceipt.finishedAt"><dt>结果保存</dt><dd>{{ selectedReceipt.finishedAt }}</dd></div>
            <div v-if="selectedReceipt.collectedAt"><dt>父任务收取</dt><dd>{{ selectedReceipt.collectedAt }}</dd></div>
        </dl>
        <AgentAnchoredPreview ref="preview" :describe="describeNode" />
    </section>
</template>

<script setup lang="ts">
import { computed, useTemplateRef, watch } from "vue";
import AgentTeamBranches from "./AgentTeamBranches.vue";
import AgentAnchoredPreview, { type AgentPreviewDetails } from "./AgentAnchoredPreview.vue";
import type { AgentTeamReceipt, AgentTeamWait, AgentToolStatus, AgentWorkerItem } from "./types";
import { workerStatusText } from "./workerUtils";
import { workerTopology } from "./workerTopology";

const props = defineProps<{
    workers: AgentWorkerItem[];
    masterStatus: AgentToolStatus;
    masterRunId?: string;
    masterWaitingForHuman?: boolean;
    waitingOn?: readonly AgentTeamWait[];
    waitTargetNames?: ReadonlyMap<string, string>;
    receipts?: ReadonlyMap<string, AgentTeamReceipt>;
    historical?: boolean;
    goal?: string;
    selectedId: string;
}>();
defineEmits<{ select: [id: string] }>();
const preview = useTemplateRef("preview");
watch([() => props.masterRunId, () => props.historical], () => preview.value?.dismiss());

function nodeAt(target: EventTarget | null) {
    return target instanceof Element ? target.closest<HTMLElement>(".map-node[data-preview-id]") : null;
}

function enterNode(event: PointerEvent) {
    const node = nodeAt(event.target);
    if (event.pointerType !== "touch" && node && node !== nodeAt(event.relatedTarget)) preview.value?.show(node);
}

function leaveNode(event: PointerEvent) {
    const node = nodeAt(event.target);
    if (event.pointerType !== "touch" && node && node !== nodeAt(event.relatedTarget)) preview.value?.leave(node);
}

function focusNode(event: FocusEvent) {
    const node = nodeAt(event.target);
    if (node?.matches(":focus-visible")) preview.value?.show(node, true);
}

function blurNode(event: FocusEvent) {
    const node = nodeAt(event.target);
    if (node) preview.value?.dismiss(node);
}

const groups = computed(() => workerTopology(props.workers));
const workersById = computed(() => new Map(props.workers.map(worker => [worker.id, worker])));
const waitsByRun = computed(() => {
    const waits = new Map<string, Set<string>>();
    for (const wait of props.waitingOn ?? []) {
        const names = waits.get(wait.waiter_run_id) ?? new Set<string>();
        if (wait.target_ids.length) {
            for (const id of wait.target_ids) {
                if (id === props.masterRunId) names.add("主 Agent");
                else names.add(workersById.value.get(id)?.name || props.waitTargetNames?.get(id)
                    || (wait.kind === "jobs" ? "后台任务" : "成员（名称未知）"));
            }
        } else {
            names.add(wait.kind === "jobs" ? "后台任务" : "信箱消息");
        }
        waits.set(wait.waiter_run_id, names);
    }
    return new Map(Array.from(waits, ([id, names]) => [id, `等 ${[...names].join("、")}`]));
});
const masterWait = computed(() => props.masterWaitingForHuman ? "等你的回答"
    : props.masterStatus === "running" && props.masterRunId ? waitsByRun.value.get(props.masterRunId) : undefined);
const workerWaits = computed(() => {
    const waits = new Map<string, string>();
    if (props.historical) return waits;
    for (const worker of props.workers) {
        if (worker.status !== "running" && worker.status !== "pending") continue;
        const wait = worker.waitingForHuman ? "等你的回答" : waitsByRun.value.get(worker.id);
        if (wait) waits.set(worker.id, wait);
    }
    return waits;
});

const dateFormatter = new Intl.DateTimeFormat(undefined, { dateStyle: "short", timeStyle: "medium" });
function formatTime(value?: string | null) {
    if (!value) return;
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? undefined : dateFormatter.format(date);
}
const receiptViews = computed(() => new Map(props.workers.flatMap((worker) => {
    if (worker.extra?.dispatchPlaceholder) return [];
    const receipt = props.receipts?.get(worker.id);
    const state = receipt?.collected_at ? "collected"
        : receipt?.result_available && receipt.collected_at === null ? "pending"
        : (receipt?.result_available || worker.status === "succeeded" && receipt?.result_available !== false) ? "unknown" : undefined;
    if (!state) return [];
    const owner = worker.parentWorkerId === null || (receipt?.parent_run_id && receipt.parent_run_id === props.masterRunId) ? "主任务"
        : workersById.value.get(receipt?.parent_run_id ?? worker.parentWorkerId ?? "")?.name || "父任务";
    const result = (receipt?.execution_status ?? worker.status) === "succeeded" ? "已交付" : "回执已保存";
    const collection = state === "collected" ? `${owner}已收取`
        : state === "pending" ? `待${owner}收取` : "收取状态未知";
    const label = state !== "unknown" && receipt?.result_available ? `${result} · ${collection}` : collection;
    return [[worker.id, {
        state, label, summary: receipt?.summary,
        finishedAt: receipt?.finished_at, collectedAt: receipt?.collected_at,
    }] as const];
})));
const selectedReceipt = computed(() => {
    const receipt = receiptViews.value.get(props.selectedId);
    if (!receipt) return undefined;
    return { ...receipt, finishedAt: formatTime(receipt.finishedAt), collectedAt: formatTime(receipt.collectedAt) };
});

function describeNode(id: string): AgentPreviewDetails | undefined {
    const rows: AgentPreviewDetails["rows"] = [];
    if (id === "master") {
        if (props.goal) rows.push({ label: "当前目标", value: props.goal });
        if (masterWait.value) rows.push({ label: "等待", value: masterWait.value, tone: "waiting" });
        return {
            title: "主 Agent", tone: masterWait.value ? "waiting" : props.masterStatus,
            status: masterWait.value ? "等待中" : workerStatusText(props.masterStatus), rows,
        };
    }
    const worker = workersById.value.get(id);
    if (!worker) return undefined;
    const wait = workerWaits.value.get(id);
    const step = worker.currentStep || worker.currentTool;
    const receipt = receiptViews.value.get(id);
    if (wait) rows.push({ label: "等待", value: wait, tone: "waiting" });
    if (step) rows.push({ label: "当前进度", value: step });
    if (receipt) {
        rows.push({ label: "结果回执", value: receipt.label, tone: receipt.state });
        if (receipt.summary) rows.push({ label: "结果摘要", value: receipt.summary });
    }
    return {
        title: worker.name, status: status(worker),
        tone: worker.reviewStopped ? "stopped" : wait ? "waiting" : worker.status,
        subtitle: [worker.roleLabel, worker.model].filter(Boolean).join(" · "), rows,
    };
}
function status(worker: AgentWorkerItem) {
    if (worker.reviewStopped) return "已停止";
    if (workerWaits.value.has(worker.id)) return "等待中";
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
    --map-branch-line: rgba(var(--theme-foreground-rgb), .35);
    --map-focus: var(--theme-status-info, #4ba6dc);
    border: 1px solid var(--map-line);
    border-radius: 10px;
    padding: 12px;
    display: grid;
    gap: 6px;
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
    min-inline-size: 174px;
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
.map-node[data-preview="true"] {
    border-color: var(--map-focus);
    background: color-mix(in srgb, var(--map-focus) 8%, var(--theme-bg-panel, #171a20));
    box-shadow: inset 0 2px 5px #07131f42, inset 2px 0 var(--map-focus);
}
.map-node:focus-visible { outline: 2px solid var(--map-focus); outline-offset: 2px; }
.node-copy, .node-meta { display: grid; gap: 3px; min-width: 0; }
.node-copy > *, .node-meta small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.node-copy .wait-line, .node-copy .receipt-line { white-space: normal; overflow-wrap: anywhere; font-size: 11px; line-height: 1.45; }
.wait-line, .node-state.is-waiting, .receipt-line[data-state="pending"] { color: var(--theme-status-warning, #d9a44d); }
.receipt-line { color: var(--theme-text-secondary, #a4a9b5); }
.receipt-line[data-state="collected"] { color: var(--theme-status-success, #36c295); }
.node-copy strong { font-size: 12px; }
.node-meta { justify-items: end; max-width: 93px; }
.node-state { font-size: 11px; white-space: nowrap; }
.node-marker { width: 7px; height: 7px; border-radius: 50%; background: var(--theme-text-secondary, #a4a9b5); }
.node-marker[data-status="running"] { background: var(--map-focus); }
.node-marker[data-status="succeeded"] { background: var(--theme-status-success, #36c295); }
.node-marker[data-status="failed"] { background: var(--theme-status-danger, #e77879); }
.node-marker[data-status="waiting"], .node-marker[data-status="challenged"] { background: var(--theme-status-warning, #d9a44d); }
.map-scroll { max-height: calc(44vh + 63px); overflow: auto; padding: 3px; }
.unlinked { margin: 12px 0 0; }
.map-empty { margin: 8px 0 0; }
.receipt-detail { display: grid; gap: 4px; margin: 3px; padding: 9px 10px; border-top: 1px solid var(--map-line); font-size: 11px; }
.receipt-detail > div { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: 10px; }
.receipt-detail dt { color: var(--theme-text-secondary, #a4a9b5); }
.receipt-detail dd { margin: 0; overflow-wrap: anywhere; }
</style>
