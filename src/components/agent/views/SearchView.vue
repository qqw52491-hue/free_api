<template>
    <div class="tool-view-card" @click.stop="$emit('select')">
        <div class="tool-view-head">
            <span class="tool-view-badge">检索</span>
            <span class="tool-view-query">{{ view.query }}</span>
            <em class="tool-view-count">{{ view.total_hits }} 条命中<template v-if="view.truncated">（已截断）</template></em>
        </div>
        <ul class="tool-view-hits">
            <li v-for="(hit, hi) in (view.hits || []).slice(0, 6)" :key="hi">
                <span class="hit-path">{{ hit.path }}</span>
                <span v-if="hit.snippet" class="hit-snippet">{{ hit.snippet }}</span>
            </li>
            <li v-if="(view.hits || []).length > 6" class="hit-more">…共 {{ view.hits.length }} 条，点开看全部</li>
        </ul>
    </div>
</template>

<script setup lang="ts">
import type { SearchToolView } from "../types";

defineProps<{ view: SearchToolView }>();
defineEmits<{ select: [] }>();
</script>

<style scoped src="./tool-views.css"></style>
