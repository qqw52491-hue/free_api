<template>
    <div class="tool-view-card" :class="{ 'is-detail': detail }" @click.stop="$emit('select')">
        <div class="tool-view-head">
            <button v-if="hasBody" class="view-chevron" :class="{ open: effectiveExpanded }" @click.stop="toggle" title="展开/收起">
                <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3"><polyline points="6 9 12 15 18 9"></polyline></svg>
            </button>
            <span class="tool-view-badge read">读取</span>
            <span class="tool-view-query">{{ view.path }}</span>
            <em class="tool-view-count">L{{ view.start_line }}–{{ view.end_line }} / 共 {{ view.total_lines }} 行</em>
        </div>
        <div v-show="effectiveExpanded" class="view-body">
            <!-- 详情态：完整输出（含行号）高框可滚；卡片态：40 行预览 -->
            <pre v-if="detail && output" class="tool-view-preview detail-body">{{ output }}</pre>
            <template v-else>
                <pre v-if="view.preview" class="tool-view-preview">{{ view.preview }}</pre>
                <div v-if="view.preview_truncated" class="hit-more">…点开看完整输出</div>
            </template>
        </div>
    </div>
</template>

<script setup lang="ts">
import { computed } from "vue";
import type { ReadToolView } from "../types";
import { useExpandable } from "./useExpandable";

const props = defineProps<{ view: ReadToolView; detail?: boolean; output?: string; params?: unknown }>();
defineEmits<{ select: [] }>();

// 默认收起：时间线扫读优先，细节靠 chevron 内联展开或抽屉深挖
const { expanded, toggle } = useExpandable(false);
const effectiveExpanded = computed(() => props.detail || expanded.value);
const hasBody = computed(() => !!(props.view.preview || (props.detail && props.output)));
</script>

<style scoped src="./tool-views.css"></style>
