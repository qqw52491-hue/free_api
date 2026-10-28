<template>
    <AgentTimelineList
        ref="historyList"
        :entries="history"
        lazy
        :scroll-element="scrollElement"
        :view-active="viewActive"
        @before-expand="$emit('before-expand')"
    >
        <template #default="{ entry }"><slot :entry="entry" /></template>
    </AgentTimelineList>
    <AgentTimelineList :entries="live">
        <template #default="{ entry }"><slot :entry="entry" /></template>
    </AgentTimelineList>
</template>

<script setup lang="ts" generic="T extends { id: string }">
import { ref } from "vue";
import AgentTimelineList from "./AgentTimelineList.vue";

const props = defineProps<{
    history: readonly T[];
    live: readonly T[];
    scrollElement: HTMLElement | null;
    viewActive: boolean;
}>();
defineEmits<{ "before-expand": [] }>();
defineSlots<{ default(props: { entry: T }): unknown }>();

const historyList = ref<{ reveal(id: string): Promise<boolean> } | null>(null);
async function reveal(id: string): Promise<boolean> {
    if (props.live.some(entry => entry.id === id)) return true;
    return await historyList.value?.reveal(id) ?? false;
}
defineExpose({ reveal });
</script>
