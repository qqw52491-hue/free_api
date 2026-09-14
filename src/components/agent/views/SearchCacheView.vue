<template>
    <div class="tool-view-card is-cache" :class="{ 'is-detail': detail }" @click.stop="$emit('select')">
        <div class="tool-view-head">
            <span class="tool-view-badge cache">去重锚点</span>
            <span class="tool-view-query">复用第 {{ view.step_id }} 步结果 · {{ view.total_hits }} 条命中</span>
        </div>
        <ul class="tool-view-hits">
            <li v-for="(p, pi) in (view.top_paths || [])" :key="pi"><span class="hit-path">{{ p }}</span></li>
        </ul>
        <div v-if="detail && view.output_id" class="view-note">output_id 句柄：<code>{{ view.output_id }}</code>（可用 get_step_output 回查全文）</div>
    </div>
</template>

<script setup lang="ts">
import type { SearchCacheToolView } from "../types";

defineProps<{ view: SearchCacheToolView; detail?: boolean; output?: string; params?: unknown }>();
defineEmits<{ select: [] }>();
</script>

<style scoped src="./tool-views.css"></style>
