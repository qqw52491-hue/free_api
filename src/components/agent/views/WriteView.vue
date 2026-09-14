<template>
    <div class="tool-view-card" @click.stop="$emit('select')">
        <div class="tool-view-head">
            <span class="tool-view-badge write">写入</span>
            <span class="tool-view-query">{{ view.path }}</span>
            <em class="tool-view-count">
                {{ formatSize(view.size_bytes) }}<template v-if="view.is_tmp"> · 会话临时文件</template>
            </em>
        </div>
    </div>
</template>

<script setup lang="ts">
import type { WriteToolView } from "../types";

defineProps<{ view: WriteToolView; detail?: boolean; output?: string; params?: unknown }>();
defineEmits<{ select: [] }>();

function formatSize(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}
</script>

<style scoped src="./tool-views.css"></style>
