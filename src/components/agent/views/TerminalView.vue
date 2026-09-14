<template>
    <div class="term-card" :class="{ 'is-error': view.exit_code !== 0 || view.spawn_error }" @click.stop="$emit('select')">
        <!-- 终端窗口标题栏：三色灯 + 命令 + 退出徽章 -->
        <div class="term-titlebar">
            <span class="term-dots"><i class="dot r"></i><i class="dot y"></i><i class="dot g"></i></span>
            <span class="term-cmd" :title="view.cwd">$ {{ view.command }}</span>
            <span class="term-exit" :class="view.spawn_error || view.exit_code !== 0 ? 'fail' : 'ok'">
                {{ view.spawn_error ? 'SPAWN FAIL' : `exit ${view.exit_code}` }}
            </span>
        </div>
        <!-- 输出区：终端等宽黑底 -->
        <div v-if="view.spawn_error || view.output_tail" class="term-body">
            <pre :class="{ 'term-err': view.spawn_error }">{{ view.spawn_error || view.output_tail }}</pre>
            <div v-if="view.truncated" class="term-more">…输出已截断，点开看全部</div>
        </div>
        <div class="term-meta">
            <span v-if="view.duration_ms != null">{{ (view.duration_ms / 1000).toFixed(1) }}s</span>
            <span v-if="view.cwd" class="term-cwd">{{ view.cwd }}</span>
        </div>
    </div>
</template>

<script setup lang="ts">
import type { TerminalToolView } from "../types";

defineProps<{ view: TerminalToolView }>();
defineEmits<{ select: [] }>();
</script>

<style scoped src="./tool-views.css"></style>
