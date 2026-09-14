<template>
    <div class="tool-view-card is-cache" @click.stop="$emit('select')">
        <div class="tool-view-head">
            <span class="tool-view-badge cache">去重锚点</span>
            <span class="tool-view-query">复用第 {{ view.step_id }} 步结果 · {{ view.total_hits }} 条命中</span>
        </div>
        <ul class="tool-view-hits">
            <li v-for="(p, pi) in (view.top_paths || [])" :key="pi"><span class="hit-path">{{ p }}</span></li>
        </ul>
    </div>
</template>

<script setup lang="ts">
import type { SearchCacheToolView } from "../types";

defineProps<{ view: SearchCacheToolView }>();
defineEmits<{ select: [] }>();
</script>

<style scoped src="./tool-views.css"></style>
