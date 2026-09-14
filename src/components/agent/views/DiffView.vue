<template>
    <div class="diff-card" :class="{ 'is-detail': detail }" @click.stop="$emit('select')">
        <!-- GitHub 式文件头：文件名 + 增删统计条 + 行区间 -->
        <div class="diff-filehead">
            <button class="view-chevron" :class="{ open: effectiveExpanded }" @click.stop="toggle" title="展开/收起">
                <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3"><polyline points="6 9 12 15 18 9"></polyline></svg>
            </button>
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
                    v-for="(seg, i) in statBar"
                    :key="i"
                    :class="['seg', seg]"
                ></i></span>
            </em>
        </div>
        <!-- hunk 头 + 逐行差异（详情态若 params 里有完整 target/replacement 则不受 30 行截断） -->
        <div v-show="effectiveExpanded">
            <div class="diff-hunk-head">@@ L{{ view.start_line }}–{{ view.end_line }} @@</div>
            <div class="diff-lines">
                <div v-for="(line, i) in shownRemoved" :key="'r' + i" class="diff-line del">
                    <span class="diff-sign">−</span><span class="diff-text">{{ line }}</span>
                </div>
                <div v-for="(line, i) in shownAdded" :key="'a' + i" class="diff-line add">
                    <span class="diff-sign">+</span><span class="diff-text">{{ line }}</span>
                </div>
            </div>
            <div v-if="view.truncated && !hasFullFromParams" class="hit-more">…点开看完整改动</div>
        </div>
    </div>
</template>

<script setup lang="ts">
import { computed } from "vue";
import type { DiffToolView } from "../types";
import { useExpandable } from "./useExpandable";

const props = defineProps<{ view: DiffToolView; detail?: boolean; output?: string; params?: unknown }>();
defineEmits<{ select: [] }>();

// 约定：diff 默认展开——"改了什么"是编辑步骤的核心信息
const { expanded, toggle } = useExpandable(true);
const effectiveExpanded = computed(() => props.detail || expanded.value);

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

/** 详情态：从调用参数里取完整 target/replacement（view 里只截存了 30 行） */
const fullFromParams = computed<{ removed: string[]; added: string[] } | null>(() => {
    if (!props.detail || !props.params || typeof props.params !== "object") return null;
    const p = props.params as Record<string, unknown>;
    const t = typeof p.target_content === "string" ? p.target_content : null;
    const r = typeof p.replacement_content === "string" ? p.replacement_content : null;
    if (t == null && r == null) return null;
    return {
        removed: t != null ? t.split("\n") : [],
        added: r != null ? r.split("\n") : [],
    };
});

const hasFullFromParams = computed(() => fullFromParams.value != null);
const shownRemoved = computed(() => fullFromParams.value?.removed ?? props.view.removed_lines.slice(0, props.detail ? 30 : 12));
const shownAdded = computed(() => fullFromParams.value?.added ?? props.view.added_lines.slice(0, props.detail ? 30 : 12));
</script>

<style scoped src="./tool-views.css"></style>
