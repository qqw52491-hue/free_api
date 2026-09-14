<template>
    <div class="tool-view-card" @click.stop="$emit('select')">
        <div class="tool-view-head">
            <span class="tool-view-badge read">读取</span>
            <span class="tool-view-query">{{ view.path }}</span>
            <em class="tool-view-count">L{{ view.start_line }}–{{ view.end_line }} / 共 {{ view.total_lines }} 行</em>
        </div>
        <pre v-if="view.preview" class="tool-view-preview">{{ view.preview }}</pre>
        <div v-if="view.preview_truncated" class="hit-more">…点开看完整输出</div>
    </div>
</template>

<script setup lang="ts">
import type { ReadToolView } from "../types";

defineProps<{ view: ReadToolView }>();
defineEmits<{ select: [] }>();
</script>

<style scoped src="./tool-views.css"></style>
