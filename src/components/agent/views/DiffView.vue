<template>
    <div class="diff-card" @click.stop="$emit('select')">
        <!-- GitHub 式文件头：文件名 + 增删统计条 + 行区间 -->
        <div class="diff-filehead">
            <svg class="diff-fileicon" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
                <polyline points="14 2 14 8 20 8" />
            </svg>
            <span class="diff-filename" :title="view.path">{{ fileName }}</span>
            <span class="diff-dir">{{ dirName }}</span>
            <em class="diff-stats">
                <span class="diff-add">+{{ view.added_total ?? view.added_lines.length }}</span>
                <span class="diff-del">−{{ view.removed_total ?? view.removed_lines.length }}</span>
                <span class="diff-statbar"><i
                    v-for="seg in statBar"
                    :key="seg"
                    :class="['seg', seg]"
                ></i></span>
            </em>
        </div>
        <!-- hunk 头 + 逐行差异 -->
        <div class="diff-hunk-head">@@ L{{ view.start_line }}–{{ view.end_line }} @@</div>
        <div class="diff-lines">
            <div v-for="(line, i) in view.removed_lines.slice(0, 12)" :key="'r' + i" class="diff-line del">
                <span class="diff-sign">−</span><span class="diff-text">{{ line }}</span>
            </div>
            <div v-for="(line, i) in view.added_lines.slice(0, 12)" :key="'a' + i" class="diff-line add">
                <span class="diff-sign">+</span><span class="diff-text">{{ line }}</span>
            </div>
        </div>
        <div v-if="view.truncated" class="hit-more">…点开看完整改动</div>
    </div>
</template>

<script setup lang="ts">
import { computed } from "vue";
import type { DiffToolView } from "../types";

const props = defineProps<{ view: DiffToolView }>();
defineEmits<{ select: [] }>();

const fileName = computed(() => props.view.path.split("/").pop() || props.view.path);
const dirName = computed(() => {
    const p = props.view.path;
    const idx = p.lastIndexOf("/");
    return idx > 0 ? p.slice(0, idx + 1) : "";
});

/** GitHub 式 5 格增删比例条 */
const statBar = computed<Array<"add" | "del" | "none">>(() => {
    const a = props.view.added_total ?? props.view.added_lines.length;
    const d = props.view.removed_total ?? props.view.removed_lines.length;
    const total = a + d;
    if (total === 0) return ["none", "none", "none", "none", "none"];
    const addSegs = Math.round((a / total) * 5);
    return Array.from({ length: 5 }, (_, i) => (i < addSegs ? "add" : "del"));
});
</script>

<style scoped src="./tool-views.css"></style>
