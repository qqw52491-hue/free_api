<template>
    <div class="tool-view-card read-view-card" :class="{ 'is-detail': detail }" @click.stop="$emit('select')">
        <template v-if="detail">
            <div class="read-file-heading">
                <FileIcon :name="fileName" :size="18" class="read-file-icon" />
                <div class="read-file-identity">
                    <strong>{{ fileName }}</strong>
                    <span :title="view.path">{{ view.path }}</span>
                </div>
                <span class="tool-view-badge read">只读</span>
            </div>
            <div class="read-toolbar">
                <span class="read-range">L{{ view.start_line }}–{{ view.end_line }} <span>· 共 {{ view.total_lines }} 行</span></span>
                <div class="read-actions">
                    <button type="button" :class="{ active: wrapLines }" :aria-pressed="wrapLines" @click.stop="wrapLines = !wrapLines">自动换行</button>
                    <button v-if="displayedOutput" type="button" @click.stop="copyOutput">{{ copyLabel }}</button>
                </div>
            </div>
        </template>
        <div v-else class="tool-view-head">
            <button v-if="hasBody" class="view-chevron" :class="{ open: effectiveExpanded }" @click.stop="toggle" title="展开/收起">
                <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3"><polyline points="6 9 12 15 18 9"></polyline></svg>
            </button>
            <span class="tool-view-badge read">读取</span>
            <span class="tool-view-query">{{ view.path }}</span>
            <em class="tool-view-count">L{{ view.start_line }}–{{ view.end_line }} / 共 {{ view.total_lines }} 行</em>
        </div>
        <div v-show="effectiveExpanded" class="view-body">
            <!-- 连续编号输出使用独立行号栏；未知形态直接展示原文，复制始终保留原始空白。 -->
            <div v-if="detail && highlightedLines" class="read-code-body" :class="{ 'wrap-lines': wrapLines }" tabindex="0" aria-label="文件读取内容">
                <div class="read-code-lines">
                    <div v-for="(line, index) in highlightedLines" :key="index" class="read-code-line" :class="{ 'read-output-note': line.number === null }">
                        <span class="read-line-number">{{ line.number ?? '' }}</span>
                        <code class="read-line-text" v-html="line.html || '&#8203;'"></code>
                    </div>
                </div>
            </div>
            <pre v-else-if="detail" class="tool-view-preview detail-body" :class="{ 'wrap-lines': wrapLines }" tabindex="0" aria-label="文件读取内容" v-html="highlightedOutput"></pre>
            <template v-else>
                <pre v-if="view.preview" class="tool-view-preview">{{ view.preview }}</pre>
                <div v-if="view.preview_truncated" class="hit-more">…点开看完整输出</div>
            </template>
        </div>
    </div>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from "vue";
import type { ReadToolView } from "../types";
import { useExpandable } from "./useExpandable";
import FileIcon from "../FileIcon.vue";
import { readOutputLines, highlightReadLines, highlightReadText } from "./readOutput";

const props = defineProps<{ view: ReadToolView; detail?: boolean; output?: string; params?: unknown }>();
defineEmits<{ select: [] }>();

// 默认收起：时间线扫读优先，细节靠 chevron 内联展开或抽屉深挖
const { expanded, toggle } = useExpandable(false);
const effectiveExpanded = computed(() => props.detail || expanded.value);
const hasBody = computed(() => !!(props.view.preview || (props.detail && props.output)));
const fileName = computed(() => props.view.path.replace(/\\/g, "/").split("/").pop() || props.view.path);
const displayedOutput = computed(() => props.output || props.view.preview || "");
const numberedLines = computed(() => readOutputLines(displayedOutput.value, props.view.start_line, props.view.end_line));
const highlightedLines = computed(() => props.detail && numberedLines.value
    ? highlightReadLines(numberedLines.value, props.view.path) : null);
const highlightedOutput = computed(() => props.detail && !numberedLines.value
    ? highlightReadText(displayedOutput.value, props.view.path) : "");
const wrapLines = ref(true);
const copyLabel = ref("复制内容");
let copyTimer: ReturnType<typeof setTimeout> | undefined;
async function copyOutput() {
    try {
        await navigator.clipboard.writeText(displayedOutput.value);
        copyLabel.value = "已复制";
    } catch {
        copyLabel.value = "复制失败";
    }
    if (copyTimer) clearTimeout(copyTimer);
    copyTimer = setTimeout(() => { copyLabel.value = "复制内容"; }, 2000);
}
onBeforeUnmount(() => { if (copyTimer) clearTimeout(copyTimer); });
</script>

<style scoped src="./tool-views.css"></style>
