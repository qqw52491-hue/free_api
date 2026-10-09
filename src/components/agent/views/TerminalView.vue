<template>
    <div class="term-card" :class="{ 'is-error': view.exit_code !== 0 || view.spawn_error, 'is-detail': detail }" @click.stop="$emit('select')">
        <!-- 终端窗口标题栏：三色灯 + 命令 + 退出徽章 -->
        <div class="term-titlebar">
            <button class="view-chevron term-chevron" :class="{ open: effectiveExpanded }" @click.stop="toggle" title="展开/收起">
                <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3"><polyline points="6 9 12 15 18 9"></polyline></svg>
            </button>
            <span class="term-dots"><i class="dot r"></i><i class="dot y"></i><i class="dot g"></i></span>
            <span class="term-cmd" :title="view.cwd">$ {{ view.command }}</span>
            <span class="term-exit" :class="!live && (view.spawn_error || view.exit_code !== 0) ? 'fail' : 'ok'">
                {{ live ? '运行中…' : view.spawn_error ? 'SPAWN FAIL' : `exit ${view.exit_code}` }}
            </span>
        </div>
        <!-- 输出区：详情态显示完整输出，卡片态支持滚动与自动吸底 -->
        <div v-show="effectiveExpanded" ref="bodyRef" class="term-body soft-scrollbar" @scroll="onScroll">
            <pre v-if="live && !displayOutput" class="term-live-hint">执行中…</pre>
            <pre v-else-if="view.spawn_error || displayOutput" :class="{ 'term-err': view.spawn_error }">{{ view.spawn_error || displayOutput }}</pre>
            <div v-if="view.truncated && !detail" class="term-more">…输出已截断，点开看全部</div>
        </div>
        <div v-show="effectiveExpanded" class="term-meta">
            <span v-if="view.duration_ms != null">{{ (view.duration_ms / 1000).toFixed(1) }}s</span>
            <span v-if="view.cwd" class="term-cwd">{{ view.cwd }}</span>
        </div>
    </div>
</template>

<script setup lang="ts">
import { computed, ref, watch, onMounted, nextTick } from "vue";
import type { TerminalToolView } from "../types";
import { useExpandable } from "./useExpandable";

const props = defineProps<{ view: TerminalToolView; detail?: boolean; output?: string; params?: unknown; live?: boolean }>();
defineEmits<{ select: [] }>();

// 约定：失败自动展开（错误要主动跳到眼前），成功默认收起
const failed = computed(() => props.view.exit_code !== 0 || !!props.view.spawn_error);
// live（简报流传入）：执行中默认展开，结束后自动收起（失败仍保持展开）
const { expanded, toggle } = useExpandable(failed.value || !!props.live);
watch(
    () => props.live,
    (now, prev) => {
        if (now) expanded.value = true;
        else if (prev) expanded.value = failed.value;
    },
);
const effectiveExpanded = computed(() => props.detail || expanded.value);
const displayOutput = computed(() => (props.detail && props.output ? props.output : props.view.output_tail));

const bodyRef = ref<HTMLElement | null>(null);
// 默认开启吸底跟随
const isAtBottom = ref(true);

function scrollToBottom() {
    nextTick(() => {
        const el = bodyRef.value;
        if (!el) return;
        el.scrollTop = el.scrollHeight;
    });
}

function onScroll() {
    const el = bodyRef.value;
    if (!el) return;
    // 容差吸底判定：距底部小于等于 12px 视为在底部，继续跟随；主动向上滚动则暂停吸底
    const distanceFromBottom = el.scrollHeight - el.clientHeight - el.scrollTop;
    isAtBottom.value = distanceFromBottom <= 12;
}

// 当输出流式产生新内容时，如果在底部则自动触底
watch(displayOutput, () => {
    if (isAtBottom.value && effectiveExpanded.value) {
        scrollToBottom();
    }
});

// 展开卡片时，若处于吸底跟随态，定位至最新输出末尾
watch(effectiveExpanded, (val) => {
    if (val && isAtBottom.value) {
        scrollToBottom();
    }
});

onMounted(() => {
    if (effectiveExpanded.value) {
        scrollToBottom();
    }
});
</script>

<style scoped src="./tool-views.css"></style>
