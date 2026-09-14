<template>
    <div class="tool-view-card" :class="{ 'is-detail': detail }" @click.stop="$emit('select')">
        <div class="tool-view-head">
            <button v-if="hasBody" class="view-chevron" :class="{ open: effectiveExpanded }" @click.stop="toggle" title="展开/收起">
                <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3"><polyline points="6 9 12 15 18 9"></polyline></svg>
            </button>
            <span class="tool-view-badge">检索</span>
            <span class="tool-view-query">{{ view.query }}</span>
            <em class="tool-view-count">{{ view.total_hits }} 条命中<template v-if="view.truncated">（已截断）</template></em>
        </div>
        <div v-show="effectiveExpanded" class="view-body">
            <ul class="tool-view-hits" :class="{ 'detail-list': detail }">
                <li v-for="(hit, hi) in shownHits" :key="hi">
                    <span class="hit-path">{{ hit.path }}</span>
                    <span v-if="hit.snippet" class="hit-snippet">{{ hit.snippet }}</span>
                </li>
                <li v-if="!detail && (view.hits || []).length > 6" class="hit-more">…共 {{ view.hits.length }} 条，点开看全部</li>
            </ul>
            <!-- 详情态补充：检索备注与错误明细 -->
            <template v-if="detail">
                <div v-if="view.resolved_root" class="view-note">检索根目录：{{ view.resolved_root }}</div>
                <div v-for="(n, i) in view.notes || []" :key="'n' + i" class="view-note">💡 {{ n }}</div>
                <div v-for="(e, i) in view.errors || []" :key="'e' + i" class="view-note err">⚠️ {{ e }}</div>
            </template>
        </div>
    </div>
</template>

<script setup lang="ts">
import { computed } from "vue";
import type { SearchToolView } from "../types";
import { useExpandable } from "./useExpandable";

const props = defineProps<{ view: SearchToolView; detail?: boolean; output?: string; params?: unknown }>();
defineEmits<{ select: [] }>();

// 默认收起：命中列表是次要细节
const { expanded, toggle } = useExpandable(false);
const effectiveExpanded = computed(() => props.detail || expanded.value);
const hasBody = computed(() => (props.view.hits || []).length > 0 || props.detail);
const shownHits = computed(() => (props.detail ? props.view.hits || [] : (props.view.hits || []).slice(0, 6)));
</script>

<style scoped src="./tool-views.css"></style>
