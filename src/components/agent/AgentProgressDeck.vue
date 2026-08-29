<template>
    <transition name="deck-slide">
        <div v-if="shouldShow" class="agent-progress-deck-wrap">
            <div class="agent-progress-deck" :class="{ 'is-running': isRunning, 'is-expanded': isExpanded }">
                <!-- 头部状态摘要条 (极简 Manus 顶栏) -->
                <div class="deck-header" @click="toggleExpanded">
                    <div class="header-left">
                        <!-- 状态图标：进行中转圈，完成打勾，等待圆点 -->
                        <div class="status-icon-wrap">
                            <span v-if="isRunning" class="manus-spinner"></span>
                            <span v-else-if="allSucceeded" class="status-check">✓</span>
                            <span v-else class="status-dot">○</span>
                        </div>
                        <div class="status-text-wrap">
                            <span class="status-title">{{ headerTitle }}</span>
                            <span v-if="isRunning && currentExecutingSubStep" class="status-sub">
                                · {{ currentExecutingSubStep }}
                            </span>
                            <span v-else-if="!isRunning && level1Todos.length > 0" class="status-sub">
                                · {{ completedPlansCount }}/{{ level1Todos.length }} 阶段已达成
                            </span>
                        </div>
                    </div>

                    <div class="header-right">
                        <!-- 右侧指标统计 (本轮生成累加与总上下文) -->
                        <div v-if="totalCompletionTokensCount > 0 || displayDurationText || isRunning" class="header-metrics">
                            <span
                                v-if="totalCompletionTokensCount > 0"
                                class="metric-num"
                                :title="`各步骤生成累加: ${totalCompletionTokensCount.toLocaleString()} Tokens\n包含提示词上下文总吞吐: ${totalAllTokensCount.toLocaleString()} Tokens`"
                            >
                                {{ formatTokenCount(totalCompletionTokensCount) }} tok
                                <span v-if="totalAllTokensCount > totalCompletionTokensCount" class="metric-context-hint">
                                    ({{ formatTokenCount(totalAllTokensCount) }} 总)
                                </span>
                            </span>
                            <span v-if="totalCompletionTokensCount > 0 && displayDurationText" class="metric-divider">·</span>
                            <span v-if="displayDurationText" class="metric-timer">
                                {{ displayDurationText }}
                            </span>
                        </div>

                        <!-- 展开/折叠箭头 -->
                        <button type="button" class="deck-toggle-btn" :title="isExpanded ? '收起清单' : '展开清单'">
                            <span class="toggle-count">{{ level1Todos.length > 0 ? `${completedPlansCount}/${level1Todos.length}` : '' }}</span>
                            <svg viewBox="0 0 24 24" class="chevron-icon" :class="{ 'is-flipped': isExpanded }">
                                <path d="m6 9 6 6 6-6" />
                            </svg>
                        </button>

                        <!-- 完成时的关闭按钮 -->
                        <button
                            v-if="!isRunning"
                            type="button"
                            class="deck-close-btn"
                            title="隐藏任务进度"
                            @click.stop="dismiss"
                        >
                            <svg viewBox="0 0 24 24"><path d="M18 6 6 18M6 6l12 12"/></svg>
                        </button>
                    </div>
                </div>

                <!-- 展开的 TodoList 纯一级大任务清单 (高信噪比、极简阶段列表) -->
                <transition name="deck-expand">
                    <div v-if="isExpanded && level1Todos.length > 0" class="deck-body">
                        <div class="todolist-container">
                            <div
                                v-for="(todo, pIdx) in level1Todos"
                                :key="todo.id || pIdx"
                                class="todolist-row level-1-row"
                                :class="[todo.status, { 'is-current': todo.isCurrent }]"
                                @click="handleTodoClick(todo)"
                            >
                                <!-- 状态指示器：进行中转圈、完成打勾、等待圆圈 -->
                                <div class="step-icon-col">
                                    <span v-if="todo.status === 'succeeded'" class="step-icon check">✓</span>
                                    <span v-else-if="todo.status === 'failed'" class="step-icon fail">✕</span>
                                    <span v-else-if="todo.isCurrent && isRunning" class="step-icon spinner">
                                        <span class="manus-spinner small"></span>
                                    </span>
                                    <span v-else class="step-icon circle">○</span>
                                </div>

                                <!-- 一级任务阶段标题 -->
                                <div class="step-content-col">
                                    <div class="step-name-row">
                                        <span class="step-name level-1-name">{{ todo.title }}</span>
                                    </div>

                                    <!-- 进行中时的动态提示（思考文字或当前工具） -->
                                    <div v-if="todo.isCurrent && isRunning" class="step-sub-action-preview">
                                        <span v-if="isThinking && latestThoughtSnippet" class="thought-live">
                                            🧠 {{ latestThoughtSnippet }}
                                        </span>
                                        <span v-else-if="todo.currentSubStepTitle" class="action-live">
                                            ⚡ 正在执行: {{ todo.currentSubStepTitle }}
                                        </span>
                                    </div>
                                </div>

                                <!-- 一级任务右侧汇总数据 (该阶段合并 Token 与耗时) -->
                                <div class="step-meta-col">
                                    <span v-if="todo.isCurrent && isRunning" class="meta-live-timer">
                                        {{ currentStepDurationText }}
                                    </span>
                                    <template v-else>
                                        <span
                                            v-if="todo.completionTokens > 0"
                                            class="meta-tokens"
                                            :title="`该阶段生成: ${todo.completionTokens.toLocaleString()} tok · 上下文: ${todo.allTokens.toLocaleString()} tok`"
                                        >
                                            {{ formatTokenCount(todo.completionTokens) }} tok
                                        </span>
                                        <span v-if="todo.durationMs > 0" class="meta-duration">
                                            {{ formatDurationMs(todo.durationMs) }}
                                        </span>
                                    </template>
                                </div>
                            </div>
                        </div>
                    </div>
                </transition>
            </div>
        </div>
    </transition>
</template>

<script setup lang="ts">
import { computed, ref, watch, onMounted, onUnmounted } from 'vue';
import type { AgentStepItem, AgentTokenUsage, AgentToolStatus } from './types';

export interface Level1TodoItem {
    id: string;
    planId: string;
    title: string;
    status: AgentToolStatus;
    steps: AgentStepItem[];
    completedStepCount: number;
    totalStepCount: number;
    completionTokens: number;
    allTokens: number;
    durationMs: number;
    isCurrent: boolean;
    currentSubStepTitle?: string;
}

const props = defineProps<{
    steps: AgentStepItem[];
    isRunning: boolean;
    isThinking: boolean;
    thought?: string;
    actionPhaseText?: string;
    tokenUsage?: AgentTokenUsage | null;
    activeStepNumber?: number;
    sessionTitle?: string;
    summary?: string;
}>();

const emit = defineEmits<{
    (e: 'select-step', step: AgentStepItem): void;
}>();

const isExpanded = ref(true);
const isDismissed = ref(false);
const stepStartTime = ref<number>(Date.now());
const nowTime = ref<number>(Date.now());
let timerInterval: ReturnType<typeof setInterval> | null = null;

watch(() => props.isRunning, (running) => {
    if (running) {
        isDismissed.value = false;
        stepStartTime.value = Date.now();
        startTimer();
    } else {
        stopTimer();
    }
}, { immediate: true });

watch(() => props.steps.length, () => {
    stepStartTime.value = Date.now();
});

function startTimer() {
    if (timerInterval) clearInterval(timerInterval);
    timerInterval = setInterval(() => {
        nowTime.value = Date.now();
    }, 100);
}

function stopTimer() {
    if (timerInterval) {
        clearInterval(timerInterval);
        timerInterval = null;
    }
}

onMounted(() => {
    if (props.isRunning) startTimer();
});

onUnmounted(() => {
    stopTimer();
});

const shouldShow = computed(() => {
    if (isDismissed.value) return false;
    return props.isRunning || props.steps.length > 0;
});

function toggleExpanded() {
    isExpanded.value = !isExpanded.value;
}

function dismiss() {
    isDismissed.value = true;
}

function handleTodoClick(todo: Level1TodoItem) {
    const lastStep = todo.steps[todo.steps.length - 1];
    if (lastStep) {
        emit('select-step', lastStep);
    }
}

// 🌟 将物理步骤按一级阶段/计划（Plan / Milestone）聚合成纯一级 TodoList
const level1Todos = computed<Level1TodoItem[]>(() => {
    if (!props.steps.length) return [];
    const groups = new Map<string, Level1TodoItem>();

    for (const step of props.steps) {
        const key = step.planId || step.planTitle || 'default-plan';
        let item = groups.get(key);
        if (!item) {
            item = {
                id: `todo-${key}`,
                planId: step.planId || key,
                title: step.planTitle || step.description || '执行阶段任务',
                status: 'pending',
                steps: [],
                completedStepCount: 0,
                totalStepCount: 0,
                completionTokens: 0,
                allTokens: 0,
                durationMs: 0,
                isCurrent: false,
            };
            groups.set(key, item);
        }
        item.steps.push(step);
        item.totalStepCount++;
        if (step.status === 'succeeded') item.completedStepCount++;
        if (step.metrics?.completionTokens) item.completionTokens += step.metrics.completionTokens;
        if (step.metrics?.totalTokens) item.allTokens += step.metrics.totalTokens;
        if (step.metrics?.totalDurationMs) item.durationMs += step.metrics.totalDurationMs;
        else if (step.durationMs) item.durationMs += step.durationMs;
    }

    const items = [...groups.values()];
    return items.map((todo, idx) => {
        const isCurrent = props.isRunning && idx === items.length - 1;
        const runningStep = todo.steps.find(s => s.status === 'running') || todo.steps[todo.steps.length - 1];

        // 🌟 健壮状态判定：
        // 1. 如果还在运行且是当前最新阶段 -> running (转圈)
        // 2. 如果已经推进到后续阶段 (idx < items.length - 1) -> succeeded (已完成 ✓)
        // 3. 如果本阶段有成功的步骤或任务已终态完成 -> succeeded (已完成 ✓)
        // 4. 只有当任务停止且最后一步终态失败时才标为 failed
        let status: AgentToolStatus = 'succeeded';
        if (isCurrent) {
            status = 'running';
        } else if (idx === items.length - 1 && !props.isRunning) {
            const lastStep = todo.steps[todo.steps.length - 1];
            if (lastStep?.status === 'failed' && todo.completedStepCount === 0) {
                status = 'failed';
            } else {
                status = 'succeeded';
            }
        }

        return {
            ...todo,
            status,
            isCurrent,
            currentSubStepTitle: isCurrent ? runningStep?.description : undefined,
        };
    });
});

const currentExecutingSubStep = computed(() => {
    if (props.actionPhaseText) return props.actionPhaseText;
    const activeTodo = level1Todos.value.find(t => t.isCurrent) || level1Todos.value[level1Todos.value.length - 1];
    return activeTodo?.currentSubStepTitle || '';
});

const completedPlansCount = computed(() => {
    return level1Todos.value.filter(t => t.status === 'succeeded').length;
});

const allSucceeded = computed(() => {
    return level1Todos.value.length > 0 && completedPlansCount.value === level1Todos.value.length;
});

const headerTitle = computed(() => {
    if (props.isRunning) {
        if (level1Todos.value.length > 0) {
            return `任务阶段 (${completedPlansCount.value}/${level1Todos.value.length})`;
        }
        return props.isThinking ? '思考规划中...' : '执行任务中...';
    }
    // 非运行态（已结束/上一轮步骤展示态）：优先展示对话概括/标题
    if (props.sessionTitle) {
        return props.sessionTitle;
    }
    if (props.summary) {
        const firstLine = props.summary.split('\n')[0].replace(/^[#*\-•\s`>]+/, '').trim();
        if (firstLine && firstLine.length <= 35) return firstLine;
    }
    if (level1Todos.value.length > 0) {
        return `任务达成 (${level1Todos.value.length} 个阶段已完成)`;
    }
    return '就绪';
});

// 实时秒表 (运行中) / 步骤总耗时 (已完成)
const currentStepDurationText = computed(() => {
    if (!props.isRunning) return '';
    const diff = Math.max(0, nowTime.value - stepStartTime.value);
    const secs = (diff / 1000).toFixed(1);
    return `${secs}s`;
});

const displayDurationText = computed(() => {
    if (props.isRunning) return currentStepDurationText.value;
    let sumMs = 0;
    for (const s of props.steps) {
        if (s.metrics?.totalDurationMs) sumMs += s.metrics.totalDurationMs;
        else if (s.durationMs) sumMs += s.durationMs;
    }
    if (sumMs > 0) return formatDurationMs(sumMs) || '';
    return '';
});

// 累计生成 Token (各步骤 completionTokens 的精确相加)
const totalCompletionTokensCount = computed(() => {
    let sum = 0;
    for (const s of props.steps) {
        if (s.metrics?.completionTokens) {
            sum += s.metrics.completionTokens;
        }
    }
    return sum;
});

// 累计全量 Token (包含提示词输入上下文 + 生成 Token)
const totalAllTokensCount = computed(() => {
    let sum = 0;
    for (const s of props.steps) {
        if (s.metrics?.totalTokens) {
            sum += s.metrics.totalTokens;
        } else if (s.metrics?.promptTokens && s.metrics?.completionTokens) {
            sum += s.metrics.promptTokens + s.metrics.completionTokens;
        }
    }
    if (sum === 0 && props.tokenUsage?.total_tokens) {
        sum = props.tokenUsage.total_tokens;
    }
    return sum;
});

const latestThoughtSnippet = computed(() => {
    if (!props.thought) return '';
    const clean = props.thought.trim().replace(/\n+/g, ' ');
    if (clean.length <= 50) return clean;
    return '...' + clean.slice(-45);
});

function formatTokenCount(n: number): string {
    if (n >= 1000000) return (n / 1000000).toFixed(1) + 'M';
    if (n >= 1000) return (n / 1000).toFixed(1) + 'k';
    return String(n);
}

function formatDurationMs(ms?: number): string | null {
    if (!ms || ms <= 0) return null;
    if (ms < 1000) return `${ms}ms`;
    return `${(ms / 1000).toFixed(1)}s`;
}
</script>

<style scoped>
/* 外层容器：与输入框两端精准对齐 */
.agent-progress-deck-wrap {
    padding: 0 clamp(16px, 4vw, 48px) 8px;
    box-sizing: border-box;
    width: 100%;
}

/* 主卡片：采用 Manus 原生灰黑色系，低调统一 */
.agent-progress-deck {
    width: 100%;
    border-radius: 12px;
    background: #1c1c1f;
    border: 1px solid rgba(255, 255, 255, 0.08);
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.2);
    overflow: hidden;
    box-sizing: border-box;
    transition: border-color 0.2s ease;
}

.agent-progress-deck.is-running {
    border-color: rgba(255, 255, 255, 0.15);
}

/* 顶栏 */
.deck-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 7px 12px;
    cursor: pointer;
    user-select: none;
    background: rgba(255, 255, 255, 0.015);
    transition: background 0.15s ease;
}

.deck-header:hover {
    background: rgba(255, 255, 255, 0.035);
}

.header-left {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    flex: 1;
}

.status-icon-wrap {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    flex-shrink: 0;
}

.status-check {
    color: #4ade80;
    font-size: 13px;
    font-weight: 600;
}

.status-dot {
    color: #71717a;
    font-size: 12px;
}

/* Manus 风格的极简转圈动画 */
.manus-spinner {
    width: 12px;
    height: 12px;
    border: 1.5px solid rgba(255, 255, 255, 0.15);
    border-top-color: #d4d4d8;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
    display: inline-block;
}

.manus-spinner.small {
    width: 11px;
    height: 11px;
    border-width: 1.5px;
}

@keyframes spin {
    to { transform: rotate(360deg); }
}

.status-text-wrap {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    overflow: hidden;
    font-size: 12px;
}

.status-title {
    font-weight: 500;
    color: #e4e4e7;
    white-space: nowrap;
    flex-shrink: 0;
}

.status-sub {
    color: #71717a;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}

.header-right {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
}

.header-metrics {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: 11.5px;
    color: #a1a1aa;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

.metric-divider {
    color: #52525b;
}

.metric-timer {
    color: #e4e4e7;
    font-weight: 500;
}

.metric-context-hint {
    font-size: 10px;
    color: #71717a;
    margin-left: 2px;
    font-weight: 400;
}

.deck-toggle-btn {
    display: flex;
    align-items: center;
    gap: 4px;
    background: transparent;
    border: 0;
    padding: 2px 4px;
    color: #71717a;
    font-size: 11px;
    cursor: pointer;
    transition: color 0.15s ease;
}

.deck-toggle-btn:hover {
    color: #d4d4d8;
}

.toggle-count {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

.chevron-icon {
    width: 12px;
    height: 12px;
    stroke: currentColor;
    stroke-width: 2;
    fill: none;
    transition: transform 0.2s ease;
}

.chevron-icon.is-flipped {
    transform: rotate(180deg);
}

.deck-close-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    background: transparent;
    border: 0;
    padding: 2px;
    color: #71717a;
    cursor: pointer;
    transition: color 0.15s ease;
}

.deck-close-btn:hover {
    color: #d4d4d8;
}

.deck-close-btn svg {
    width: 12px;
    height: 12px;
    stroke: currentColor;
    stroke-width: 2;
    fill: none;
}

/* 纯一级 TodoList 列表主体 */
.deck-body {
    border-top: 1px solid rgba(255, 255, 255, 0.05);
    max-height: 180px;
    overflow-y: auto;
    padding: 4px 6px;
}

.todolist-container {
    display: flex;
    flex-direction: column;
    gap: 2px;
}

.todolist-row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 8px;
    border-radius: 6px;
    cursor: pointer;
    transition: background 0.15s ease;
}

.todolist-row:hover {
    background: rgba(255, 255, 255, 0.035);
}

.todolist-row.is-current {
    background: rgba(255, 255, 255, 0.045);
}

.step-icon-col {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 14px;
    height: 14px;
    flex-shrink: 0;
}

.step-icon.check {
    color: #4ade80;
    font-size: 11.5px;
    font-weight: 600;
}

.step-icon.fail {
    color: #f87171;
    font-size: 11.5px;
    font-weight: 600;
}

.step-icon.circle {
    color: #52525b;
    font-size: 11px;
}

.step-content-col {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1;
    min-width: 0;
}

.step-name-row {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
}

.level-1-name {
    font-size: 12.5px;
    font-weight: 500;
    color: #d4d4d8;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}

.todolist-row.succeeded .level-1-name {
    color: #71717a;
}

.step-sub-action-preview {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: 11px;
    color: #a1a1aa;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}

.thought-live,
.action-live {
    overflow: hidden;
    text-overflow: ellipsis;
    animation: pulse-text 2s ease-in-out infinite;
}

@keyframes pulse-text {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.5; }
}

.step-meta-col {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    color: #71717a;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    flex-shrink: 0;
}

.meta-live-timer {
    color: #e4e4e7;
    font-weight: 500;
}

.meta-tokens {
    color: #71717a;
}

.meta-duration {
    color: #71717a;
}

/* 动效 */
.deck-slide-enter-active,
.deck-slide-leave-active {
    transition: all 0.2s ease;
}

.deck-slide-enter-from,
.deck-slide-leave-to {
    opacity: 0;
    transform: translateY(4px);
}

.deck-expand-enter-active,
.deck-expand-leave-active {
    transition: all 0.18s ease-out;
}

.deck-expand-enter-from,
.deck-expand-leave-to {
    opacity: 0;
    max-height: 0;
}
</style>
