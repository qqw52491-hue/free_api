<template>
    <div class="tool-view-card is-cache" :class="{ 'is-detail': detail }" @click.stop="$emit('select')">
        <div class="tool-view-head">
            <button v-if="hasBody" class="view-chevron" :class="{ open: effectiveExpanded }" @click.stop="toggle" title="展开/收起">
                <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3"><polyline points="6 9 12 15 18 9"></polyline></svg>
            </button>
            <span class="tool-view-badge cache">去重锚点</span>
            <span class="tool-view-query">复用第 {{ view.step_id }} 步结果 · {{ view.total_hits }} 条命中</span>
        </div>
        <div v-show="effectiveExpanded" class="view-body">
            <ul class="tool-view-hits">
                <li v-for="(p, pi) in (view.top_paths || [])" :key="pi"><span class="hit-path">{{ p }}</span></li>
            </ul>
            <div v-if="detail && view.output_id" class="view-note">output_id 句柄：<code>{{ view.output_id }}</code>（可用 get_step_output 回查全文）</div>
        </div>
    </div>
</template>

<script setup lang="ts">
import { computed } from "vue";
import type { SearchCacheToolView } from "../types";
import { useExpandable } from "./useExpandable";

const props = defineProps<{ view: SearchCacheToolView; detail?: boolean; output?: string; params?: unknown }>();
defineEmits<{ select: [] }>();

const { expanded, toggle } = useExpandable(false);
const effectiveExpanded = computed(() => props.detail || expanded.value);
const hasBody = computed(() => (props.view.top_paths || []).length > 0 || (props.detail && !!props.view.output_id));
</script>

<style scoped src="./tool-views.css"></style>
