<template>
    <ul class="branch-list" :class="{ 'unlinked-roots': unlinked }" role="list">
        <li v-for="{ worker, children } in branches" :key="worker.id" class="branch">
            <div class="branch-row">
                <slot :worker="worker" />
            </div>
            <AgentTeamBranches v-if="children.length" :branches="children" v-slot="{ worker: child }">
                <slot :worker="child" />
            </AgentTeamBranches>
        </li>
    </ul>
</template>

<script setup lang="ts">
import type { AgentWorkerItem } from "./types";
import type { WorkerBranch } from "./workerTopology";

defineProps<{ branches: WorkerBranch[]; unlinked?: boolean }>();
defineSlots<{ default(props: { worker: AgentWorkerItem }): unknown }>();
</script>

<style scoped>
.branch-list {
    --branch-indent: 22px;
    --branch-rail: 13.5px;
    --branch-gap: 8px;
    position: relative;
    margin: 0;
    padding: 0;
    padding-inline-start: var(--branch-indent);
    list-style: none;
}
.branch { position: relative; }
.branch-row {
    position: relative;
    padding-block-start: var(--branch-gap);
}
.branch-list:not(.unlinked-roots) > .branch:not(:last-child)::before,
.branch-list:not(.unlinked-roots) > .branch > .branch-row::before {
    content: "";
    position: absolute;
    inset-inline-start: calc(var(--branch-rail) - var(--branch-indent));
    pointer-events: none;
}
.branch-list:not(.unlinked-roots) > .branch:not(:last-child)::before {
    inset-block: 0;
    border-inline-start: 1px solid var(--map-branch-line);
}
.branch-list:not(.unlinked-roots) > .branch > .branch-row::before {
    inset-block-start: 0;
    inline-size: calc(var(--branch-indent) - var(--branch-rail));
    block-size: calc((100% + var(--branch-gap)) / 2);
    border-block-end: 1px solid var(--map-branch-line);
}
.branch-list:not(.unlinked-roots) > .branch:last-child > .branch-row::before {
    border-inline-start: 1px solid var(--map-branch-line);
    border-end-start-radius: 5px;
}
.branch-list.unlinked-roots { padding-inline-start: 0; }
</style>
