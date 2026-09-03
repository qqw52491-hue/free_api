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

                <!-- 展开的任务阶段与上下级子步骤树形清单 -->
                <transition name="deck-expand">
                    <div v-if="isExpanded && level1Todos.length > 0" class="deck-body">
                        <div class="todolist-container">
                            <div
                                v-for="(todo, pIdx) in level1Todos"
                                :key="todo.id || pIdx"
                                class="stage-group-wrap"
                                :class="[todo.status, { 'is-current': todo.isCurrent, 'is-open': isTodoExpanded(todo.id) }]"
                            >
                                <!-- 一级阶段卡片栏 (点击切换展开/折叠子步骤详情) -->
                                <div class="todolist-row level-1-row" @click="toggleTodoExpand(todo.id)">
                                    <!-- 状态指示器：进行中转圈、完成打勾、等待圆圈 -->
                                    <div class="step-icon-col">
                                        <span v-if="todo.status === 'succeeded'" class="step-icon check">✓</span>
                                        <span v-else-if="todo.status === 'failed'" class="step-icon fail">✕</span>
                                        <span v-else-if="todo.isCurrent && isRunning" class="step-icon spinner">
                                            <span class="manus-spinner small"></span>
                                        </span>
                                        <span v-else class="step-icon circle">○</span>
                                    </div>

                                    <!-- 一级任务阶段标题与子步骤数量徽标 -->
                                    <div class="step-content-col">
                                        <div class="step-name-row">
                                            <span class="step-name level-1-name">{{ todo.title }}</span>
                                            <span v-if="todo.steps.length > 0" class="sub-step-count-badge">
                                                {{ todo.steps.length }} 步
                                            </span>
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

                                    <!-- 一级任务右侧汇总数据与展开箭头 -->
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
                                        <!-- 子步骤展开/折叠微指示箭头 -->
                                        <button
                                            v-if="todo.steps.length > 0"
                                            type="button"
                                            class="sub-accordion-trigger"
                                            :title="isTodoExpanded(todo.id) ? '收起详情' : '展开详情'"
                                            @click.stop="toggleTodoExpand(todo.id)"
                                        >
                                            <svg viewBox="0 0 24 24" class="sub-chevron-icon" :class="{ 'is-flipped': isTodoExpanded(todo.id) }">
                                                <path d="m6 9 6 6 6-6" />
                                            </svg>
                                        </button>
                                    </div>
                                </div>

                                <!-- 二级子步骤树形详情 (Nested Sub-steps Detail) -->
                                <transition name="sub-expand">
                                    <div
                                        v-if="isTodoExpanded(todo.id) && todo.steps.length > 0"
                                        class="stage-sub-steps-tree"
                                    >
                                        <!-- 局部子步骤垂直主干虚线 -->
                                        <div class="sub-tree-spine"></div>
                                        <div
                                            v-for="(step, sIdx) in todo.steps"
                                            :key="step.id || sIdx"
                                            class="sub-step-row"
                                            :class="[step.status, { 'is-active-step': step.stepId === activeStepNumber }]"
                                            @click.stop="handleSubStepClick(step)"
                                        >
                                            <!-- 树枝导引线与节点圆点 -->
                                            <div class="sub-step-dot-col">
                                                <span class="branch-curve-line"></span>
                                                <span class="sub-status-dot" :class="step.status"></span>
                                            </div>
                                            <div class="sub-step-info-col">
                                                <span class="sub-tool-badge">{{ getStepToolLabel(step) }}</span>
                                                <span class="sub-step-desc" :title="step.description || step.tool || ''">
                                                    {{ step.description || step.tool || `子步骤 #${sIdx + 1}` }}
                                                </span>
                                            </div>
                                            <div class="sub-step-meta-col">
                                                <span v-if="step.durationMs" class="sub-step-duration">
                                                    {{ formatDurationMs(step.durationMs) }}
                                                </span>
                                                <span v-else-if="step.status === 'running'" class="sub-step-live-dot"></span>
                                            </div>
                                        </div>
                                    </div>
                                </transition>
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

// 上下级二级详情展开状态集合
const expandedTodoIds = ref<Set<string>>(new Set());

function isTodoExpanded(id: string): boolean {
    // 当前正在运行的阶段默认展开，或者被用户显式展开
    if (expandedTodoIds.value.has(id)) return true;
    const current = level1Todos.value.find(t => t.id === id);
    return !!(current?.isCurrent && props.isRunning);
}

function toggleTodoExpand(id: string) {
    if (expandedTodoIds.value.has(id)) {
        expandedTodoIds.value.delete(id);
    } else {
        expandedTodoIds.value.add(id);
    }
}

function handleSubStepClick(step: AgentStepItem) {
    emit('select-step', step);
}

function getStepToolLabel(step: AgentStepItem): string {
    if (!step.tool) return '思考分析';
    const name = step.tool.includes('/') ? step.tool.split('/').pop()! : step.tool;
    const map: Record<string, string> = {
        'search': '搜索检索',
        'ripgrep_search': '代码检索',
        'read_text_file': '读取代码',
        'edit_file': '精确修改',
        'write_file': '写入文件',
        'shell_exec': '执行终端',
        'task_store': '归档状态',
        'finish': '成果交付',
        'ask_human': '用户协同',
        'web_search': '全网调研',
        'web_fetch': '抓取网页'
    };
    return map[name] || name;
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

/* ==========================================================================
   🌳 上下级树形子步骤折叠详情 (Hierarchical Nested Tree Styles)
   ========================================================================== */
.todolist-container {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 4px 6px 8px;
    position: relative;
}

.stage-group-wrap {
    display: flex;
    flex-direction: column;
    border-radius: 8px;
    transition: background 0.15s ease;
    position: relative;
}

.stage-group-wrap.is-open {
    background: rgba(255, 255, 255, 0.025);
}

.sub-step-count-badge {
    display: inline-flex;
    align-items: center;
    padding: 1px 6px;
    border-radius: 99px;
    font-size: 10px;
    font-weight: 550;
    background: rgba(255, 255, 255, 0.06);
    color: #a1a1aa !important;
    border: 1px solid rgba(255, 255, 255, 0.08);
    margin-left: 4px;
}

.sub-accordion-trigger {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    border-radius: 4px;
    border: none;
    background: transparent;
    color: #71717a;
    cursor: pointer;
    margin-left: 2px;
    transition: color 0.15s ease, background 0.15s ease;
}

.sub-accordion-trigger:hover {
    color: #e4e4e7;
    background: rgba(255, 255, 255, 0.06);
}

.sub-chevron-icon {
    width: 12px;
    height: 12px;
    fill: none;
    stroke: currentColor;
    stroke-width: 2.2;
    transition: transform 0.2s cubic-bezier(0.16, 1, 0.3, 1);
}

.sub-chevron-icon.is-flipped {
    transform: rotate(180deg);
}

/* 二级子步骤树形容器：形成清晰的缩进与贯穿垂直主干线 */
.stage-sub-steps-tree {
    display: flex;
    flex-direction: column;
    padding: 2px 8px 8px 34px;
    position: relative;
}

/* 贯穿子树的垂直主干引导线 (Sub-tree Spine) */
.sub-tree-spine {
    position: absolute;
    top: 0;
    bottom: 14px;
    left: 17px;
    width: 1px;
    background: linear-gradient(to bottom, rgba(255, 255, 255, 0.18), rgba(255, 255, 255, 0.05));
    pointer-events: none;
}

.sub-step-row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 8px;
    border-radius: 6px;
    cursor: pointer;
    transition: all 0.15s ease;
    font-size: 11.5px;
    color: #d4d4d8 !important;
    position: relative;
}

.sub-step-row:hover {
    background: rgba(255, 255, 255, 0.05);
}

.sub-step-row.is-active-step {
    background: rgba(99, 102, 241, 0.14);
    box-shadow: inset 2px 0 0 #6366f1;
}

/* 树形引导线与节点圆点 */
.sub-step-dot-col {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 14px;
    height: 14px;
    position: relative;
    flex-shrink: 0;
}

/* 从中轴线弯折伸向子节点的水平导引虚线 */
.branch-curve-line {
    position: absolute;
    top: 50%;
    left: -17px;
    width: 14px;
    height: 1px;
    border-top: 1px dashed rgba(255, 255, 255, 0.22);
    pointer-events: none;
}

.sub-status-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: #52525b;
    position: relative;
    z-index: 2;
}

.sub-status-dot.succeeded {
    background: #4ade80;
    box-shadow: 0 0 6px rgba(74, 222, 128, 0.5);
}

.sub-status-dot.running {
    background: #6366f1;
    box-shadow: 0 0 8px #6366f1;
    animation: sub-dot-pulse 1s infinite alternate;
}

.sub-status-dot.failed {
    background: #f87171;
}

@keyframes sub-dot-pulse {
    from { transform: scale(0.85); opacity: 0.6; }
    to { transform: scale(1.25); opacity: 1; }
}

.sub-step-info-col {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    flex: 1;
    overflow: hidden;
}

.sub-tool-badge {
    font-size: 9.5px;
    padding: 1px 5px;
    border-radius: 4px;
    background: rgba(99, 102, 241, 0.16);
    color: #a5b4fc !important;
    font-weight: 550;
    white-space: nowrap;
    flex-shrink: 0;
    border: 1px solid rgba(99, 102, 241, 0.25);
}

.sub-step-desc {
    color: #e4e4e7 !important;
    font-size: 11.5px !important;
    line-height: 1.4 !important;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif !important;
}

.sub-step-meta-col {
    flex-shrink: 0;
    font-size: 10px;
    color: #a1a1aa !important;
}

.sub-step-duration {
    color: #71717a !important;
    font-variant-numeric: tabular-nums;
}

.sub-step-live-dot {
    display: inline-block;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: #6366f1;
    animation: sub-dot-pulse 1s infinite alternate;
}

/* 子步骤展开平滑动画 */
.sub-expand-enter-active,
.sub-expand-leave-active {
    transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1);
    overflow: hidden;
}

.sub-expand-enter-from,
.sub-expand-leave-to {
    opacity: 0;
    transform: translateY(-4px);
    max-height: 0;
}

/* ==========================================================================
   📱 移动端进度条专属极致紧凑优化 (AgentProgressDeck <= 768px)
   ========================================================================== */
@media (max-width: 768px) {
    .agent-progress-deck-wrap {
        padding: 0 8px 6px !important;
    }
    .agent-progress-deck {
        border-radius: 10px !important;
    }
    .deck-header {
        padding: 6px 10px !important;
    }
    .header-metrics {
        display: none !important;
    }
    .status-title {
        font-size: 12px !important;
    }
    .status-sub {
        font-size: 11px !important;
        max-width: 140px !important;
    }
    .todolist-row {
        padding: 6px 8px !important;
    }
    .level-1-name {
        font-size: 12px !important;
    }
    .stage-sub-steps-tree {
        padding-left: 20px !important;
    }
    .sub-step-row {
        padding: 4px 6px !important;
        font-size: 11px !important;
    }
    .meta-tokens {
        display: none !important;
    }
}
</style>
