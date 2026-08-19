<template>
    <footer class="manus-composer-wrap">
        <div class="manus-composer" :class="{ waiting: isWaitingForHuman }">
            <textarea
                ref="textareaRef"
                :value="draft"
                :disabled="isSending"
                :placeholder="placeholderText"
                rows="2"
                @input="handleInput"
                @keydown.ctrl.enter.prevent="submit"
                @keydown.meta.enter.prevent="submit"
            />
            <div class="composer-bottom-row">
                <div class="composer-tools">
                    <button type="button" title="模型与任务设置" @click="$emit('open-settings')"><svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></svg></button>
                    <button class="model-chip" type="button" title="切换模型" @click="$emit('open-settings')"><span>{{ currentModelName }}</span><svg viewBox="0 0 24 24"><path d="m8 10 4 4 4-4"/></svg></button>
                </div>
                <div class="composer-actions">
                    <button v-if="isRunning" class="stop-control" type="button" :disabled="stopRequested" @click="$emit('stop')">{{ stopRequested ? '停止中' : '停止' }}</button>
                    <button class="submit-control" type="button" :disabled="!canSubmit || isSending" :title="isRunning ? (isWaitingForHuman ? '回答当前请求' : '替换为新任务') : '开始任务'" @click="submit">
                        <svg viewBox="0 0 24 24"><path d="m7 12 5-5 5 5M12 7v10"/></svg>
                    </button>
                </div>
            </div>
        </div>

        <!-- 🚀 实时性能流式状态行 (连续字体排版 · 柔和间距 · 实时流式跳动) -->
        <div class="composer-telemetry-line">
            <!-- 1. 耗时 -->
            <span class="telemetry-node" title="任务总运行耗时">
                <span class="dot-running" v-if="isRunning"></span>
                <span class="t-label">⏱️ 耗时</span>
                <span class="t-val">{{ telemetry?.elapsedLabel || '00:00' }}</span>
            </span>
            <span class="t-sep">·</span>

            <!-- 2. Token 实时生成速率 -->
            <span class="telemetry-node" title="模型实时生成速率 (tok/s)">
                <span class="t-label">⚡ 速率</span>
                <span class="t-val text-cyan">{{ (telemetry?.tokensPerSecond && telemetry.tokensPerSecond > 0) ? `${telemetry.tokensPerSecond.toFixed(1)} tok/s` : '--' }}</span>
            </span>
            <span class="t-sep">·</span>

            <!-- 3. 首字延迟 (TTFT) -->
            <span class="telemetry-node" title="首字到达延迟 (TTFT)">
                <span class="t-label">⏳ 首次</span>
                <span class="t-val">{{ (telemetry?.lastTtftMs && telemetry.lastTtftMs > 0) ? `${telemetry.lastTtftMs}ms` : '--' }}</span>
            </span>
            <span class="t-sep">·</span>

            <!-- 4. 前缀缓存命中率 -->
            <span class="telemetry-node" title="前缀缓存命中率">
                <span class="t-label">🎯 缓存</span>
                <span class="t-val text-green">{{ (telemetry?.cacheHitRate !== undefined && telemetry.cacheHitRate > 0) ? `${telemetry.cacheHitRate.toFixed(1)}%` : '--%' }}</span>
            </span>
            <span class="t-sep">·</span>

            <!-- 5. 上下文占用 / 窗口水位 -->
            <span class="telemetry-node" title="当前上下文占用 / 窗口上限">
                <span class="t-label">📦 上下文</span>
                <span class="t-val">{{ (telemetry?.totalTokens && telemetry.totalTokens > 0) ? formatTokens(telemetry.totalTokens) : '0' }}/{{ (telemetry?.contextWindow && telemetry.contextWindow > 0) ? formatTokens(telemetry.contextWindow) : '32k' }}</span>
                <span class="t-tag" :class="contextLevelClass">({{ (telemetry?.contextUsagePercent ?? 0).toFixed(0) }}%)</span>
            </span>
            <span class="t-sep">·</span>

            <!-- 6. 工具调用时间 -->
            <span class="telemetry-node" title="工具执行耗时">
                <span class="t-label">🔧 工具</span>
                <span class="t-val">{{ (telemetry?.lastToolDurationMs && telemetry.lastToolDurationMs > 0) ? formatMs(telemetry.lastToolDurationMs) : '--' }}</span>
            </span>
            <span class="t-sep">·</span>

            <!-- 7. 对话轮次 / 步骤 -->
            <span class="telemetry-node" title="当前执行轮次与步骤">
                <span class="t-label">🔄 步骤</span>
                <span class="t-val">{{ (telemetry?.stepCount && telemetry.stepCount > 0) ? `第 ${telemetry.stepCount} 步` : '1 轮' }}</span>
            </span>
        </div>
    </footer>
</template>

<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";
import type { AgentHumanForm, AgentModelOption, AgentSessionTelemetry } from "./types";

const props = defineProps<{
    draft: string;
    isRunning: boolean;
    isWaitingForHuman: boolean;
    isSending: boolean;
    stopRequested: boolean;
    canStart: boolean;
    pauseReason: string;
    humanForm: AgentHumanForm | null;
    allModels: AgentModelOption[];
    currentModelId: string;
    telemetry?: AgentSessionTelemetry | null;
}>();
const emit = defineEmits<{
    "update:draft": [value: string];
    "update:model": [modelId: string];
    submit: [];
    stop: [];
    "open-settings": [];
}>();

const textareaRef = ref<HTMLTextAreaElement | null>(null);

function autoResizeTextarea() {
    nextTick(() => {
        const el = textareaRef.value;
        if (!el) return;
        el.style.height = "auto";
        const targetHeight = Math.min(Math.max(el.scrollHeight, 48), 180);
        el.style.height = `${targetHeight}px`;
    });
}

function handleInput(e: Event) {
    const val = (e.target as HTMLTextAreaElement).value;
    emit("update:draft", val);
    autoResizeTextarea();
}

watch(() => props.draft, autoResizeTextarea);
onMounted(autoResizeTextarea);

const currentModelName = computed(() => props.allModels.find((model) => model.id === props.currentModelId)?.display_name || "Manus Desktop");
const placeholderText = computed(() => props.isRunning
    ? props.isWaitingForHuman
        ? "补充自定义说明后按回车提交…"
        : "输入新任务；旧模型/工具会在安全边界停止…"
    : "想让 Agent 帮你做什么？",
);
const canSubmit = computed(() => props.isRunning ? Boolean(props.draft.trim()) : props.canStart && Boolean(props.draft.trim()));
function submit() { if (canSubmit.value && !props.isSending) emit("submit"); }

function formatTokens(count: number): string {
    if (count >= 1000000) return `${(count / 1000000).toFixed(1)}M`;
    if (count >= 1000) return `${(count / 1000).toFixed(1)}k`;
    return String(count);
}

function formatMs(ms?: number): string {
    if (!ms) return "0ms";
    if (ms >= 1000) return `${(ms / 1000).toFixed(1)}s`;
    return `${ms}ms`;
}

const contextLevelClass = computed(() => {
    const p = props.telemetry?.contextUsagePercent ?? 0;
    if (p >= 80) return "badge-danger";
    if (p >= 60) return "badge-warn";
    return "badge-safe";
});
</script>

<style scoped>
.manus-composer-wrap {
    padding: 0 clamp(16px, 4vw, 48px) 12px;
    background: linear-gradient(0deg, #141416 85%, transparent);
}
.manus-composer {
    display: flex;
    flex-direction: column;
    padding: 10px 14px 10px;
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 12px;
    background: #1c1c1f;
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.25);
    transition: all 0.2s ease;
}
.manus-composer:focus-within {
    border-color: rgba(255, 255, 255, 0.22);
    box-shadow: 0 4px 24px rgba(0, 0, 0, 0.35);
}
.manus-composer.waiting {
    border-color: rgba(251, 191, 36, 0.3);
    box-shadow: 0 0 0 1px rgba(251, 191, 36, 0.15);
}

textarea {
    width: 100%;
    min-height: 48px;
    max-height: 180px;
    border: 0;
    padding: 2px 0;
    resize: none;
    color: #f4f4f5;
    background: transparent;
    font: inherit;
    font-size: 13.5px;
    line-height: 1.6;
    outline: none;
}
textarea::placeholder {
    color: #71717a;
}

.composer-bottom-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-top: 6px;
    padding-top: 6px;
    border-top: 1px solid rgba(255, 255, 255, 0.05);
}

.composer-tools {
    display: flex;
    align-items: center;
    gap: 8px;
}
.composer-tools button {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 6px;
    color: #a1a1aa;
    cursor: pointer;
    background: rgba(255, 255, 255, 0.03);
    transition: all 0.15s;
}
.composer-tools button:hover {
    color: #ffffff;
    background: rgba(255, 255, 255, 0.08);
    border-color: rgba(255, 255, 255, 0.15);
}
.composer-tools button svg {
    width: 14px;
    height: 14px;
    stroke: currentColor;
    stroke-width: 2;
    fill: none;
}

.model-chip {
    width: auto !important;
    height: 28px !important;
    padding: 0 10px !important;
    display: inline-flex !important;
    align-items: center !important;
    gap: 6px !important;
    font-size: 12px !important;
    font-weight: 500 !important;
    color: #d4d4d8 !important;
}

.composer-actions {
    display: flex;
    align-items: center;
    gap: 8px;
}

.stop-control {
    padding: 5px 12px;
    border: 1px solid rgba(255, 71, 87, 0.3);
    border-radius: 7px;
    color: #ff4757;
    cursor: pointer;
    background: rgba(255, 71, 87, 0.1);
    font: 600 12px inherit;
    transition: all 0.15s;
}
.stop-control:hover:not(:disabled) {
    background: rgba(255, 71, 87, 0.2);
}

.submit-control {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    border: 0;
    border-radius: 8px;
    color: #141416;
    cursor: pointer;
    background: #ffffff;
    transition: all 0.15s;
}
.submit-control:hover:not(:disabled) {
    background: #f4f4f5;
    transform: translateY(-1px);
}
.submit-control:disabled {
    opacity: 0.3;
    cursor: not-allowed;
    background: #71717a;
    color: #27272a;
}
.submit-control svg {
    width: 16px;
    height: 16px;
    stroke: currentColor;
    stroke-width: 2.4;
    fill: none;
}

/* ---- 底部实时指标连续状态行 ---- */
.composer-telemetry-line {
    display: flex;
    align-items: center;
    justify-content: center;
    flex-wrap: nowrap;
    gap: 8px;
    margin-top: 7px;
    padding: 2px 6px;
    font-size: 11.5px;
    color: #71717a;
    overflow-x: auto;
    scrollbar-width: none;
    white-space: nowrap;
    user-select: none;
}
.composer-telemetry-line::-webkit-scrollbar {
    display: none;
}

.telemetry-node {
    display: inline-flex;
    align-items: baseline;
    gap: 4.5px;
}

.t-label {
    color: #71717a;
    font-size: 11px;
}

.t-val {
    color: #e4e4e7;
    font-family: 'JetBrains Mono', monospace;
    font-weight: 600;
    font-size: 11.5px;
}

.t-sep {
    color: rgba(255, 255, 255, 0.15);
    font-size: 10px;
    font-weight: bold;
    user-select: none;
}

.t-tag {
    font-size: 10px;
    font-family: 'JetBrains Mono', monospace;
    font-weight: 600;
    margin-left: 2px;
}

.text-cyan {
    color: #38bdf8 !important;
}

.text-green {
    color: #4ade80 !important;
}

.badge-safe {
    color: #4ade80;
}
.badge-warn {
    color: #fbbf24;
}
.badge-danger {
    color: #f87171;
}

.dot-running {
    display: inline-block;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: #38bdf8;
    box-shadow: 0 0 6px #38bdf8;
    animation: pulse 1.4s ease-in-out infinite;
}

@keyframes pulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50% { opacity: 0.4; transform: scale(0.85); }
}

.composer-disclaimer {
    margin: 0;
    text-align: center;
    color: #52525b;
    font-size: 11px;
}
</style>

