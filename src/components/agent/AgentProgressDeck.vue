<template>
    <transition name="deck-slide">
        <div v-if="shouldShow" class="agent-progress-deck-wrap">
            <div class="agent-progress-deck" :class="{ 'is-running': isRunning, 'is-expanded': isExpanded }">
                <!-- 头部状态摘要条 (极简顶栏) -->
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
                            <span
                                v-if="gateBadge"
                                class="gate-status-badge"
                                :class="gateBadge.type"
                                :title="gateBadge.title"
                            >
                                {{ gateBadge.label }}
                            </span>
                            <span v-if="isRunning && currentExecutingSubStep" class="status-sub">
                                · {{ currentExecutingSubStep }}
                            </span>
                            <span v-else-if="!isRunning && level1Todos.length > 0" class="status-sub">
                                · {{ completedPlansCount }}/{{ level1Todos.length }} 阶段已达成
                            </span>
                        </div>
                    </div>

                    <div class="header-right">
                        <!-- 右侧指标统计 (本轮生成累加与总耗时) -->
                        <div v-if="totalCompletionTokensCount > 0 || displayDurationText || isRunning" class="header-metrics">
                            <span
                                v-if="totalCompletionTokensCount > 0"
                                class="metric-num"
                                :title="`各步骤生成累加: ${totalCompletionTokensCount.toLocaleString()} Tokens\n包含上下文总吞吐: ${totalAllTokensCount.toLocaleString()} Tokens`"
                            >
                                {{ formatTokenCount(totalCompletionTokensCount) }} tok
                            </span>
                            <span v-if="totalCompletionTokensCount > 0 && displayDurationText" class="metric-divider">·</span>
                            <span v-if="displayDurationText" class="metric-timer">
                                {{ displayDurationText }}
                            </span>
                        </div>

                        <!-- 展开/折叠箭头 -->
                        <button type="button" class="deck-toggle-btn" :title="isExpanded ? '收起进度概括' : '展开进度概括'">
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

                <!-- 展开的大步骤概括列表 (纯最外层大阶段进度，无二级展开树) -->
                <transition name="deck-expand">
                    <div
                        v-if="isExpanded && level1Todos.length > 0"
                        ref="deckBodyRef"
                        class="deck-body"
                    >
                        <div class="todolist-container">
                            <div
                                v-for="(todo, pIdx) in level1Todos"
                                :key="todo.id || pIdx"
                                class="stage-summary-card"
                                :class="[
                                    todo.status,
                                    `state-${todo.stageState}`,
                                    {
                                        'is-stage-active-pulse': todo.isCurrent && isRunning,
                                        'has-capsules': todo.capsules.length > 0
                                    }
                                ]"
                            >
                                <!-- 🌟 当前执行阶段专属流光穿梭层 (在那个阶段然后条条就闪烁流光) -->
                                <div v-if="todo.isCurrent && isRunning" class="stage-shimmer-beam"></div>

                                <!-- 第一行：大步骤状态、概括标题与耗时/进度 -->
                                <div class="stage-card-row">
                                    <div class="stage-status-icon">
                                        <span v-if="todo.isCurrent && isRunning" class="manus-spinner mini"></span>
                                        <span v-else-if="todo.status === 'succeeded'" class="stage-check-icon">✓</span>
                                        <span v-else-if="todo.status === 'failed'" class="stage-fail-icon">✕</span>
                                        <span v-else class="stage-dot-icon">○</span>
                                    </div>

                                    <div class="stage-title-wrap">
                                        <span class="stage-title-text" :class="{ 'is-active-title': todo.isCurrent && isRunning }">
                                            {{ getTodoDisplayTitle(todo, pIdx) }}
                                        </span>
                                        <span
                                            v-if="todo.failureState === 'recovered'"
                                            class="stage-fail-tag tag-recovered"
                                            title="此阶段曾有子步骤失败，但随后已自动重试恢复成功"
                                        >
                                            已重试恢复
                                        </span>
                                        <span
                                            v-else-if="todo.failureState === 'retrying'"
                                            class="stage-fail-tag tag-retrying"
                                            title="此阶段有步骤失败，正在重试恢复中"
                                        >
                                            失败重试中
                                        </span>
                                    </div>

                                    <div class="stage-right-meta">
                                        <span v-if="todo.isCurrent && isRunning" class="stage-live-badge">
                                            <span class="live-blink-dot"></span>
                                            执行中 · {{ currentStepDurationText }}
                                        </span>
                                        <span v-else class="stage-done-meta">
                                            {{ getTodoMetaSummary(todo) }}
                                        </span>
                                    </div>
                                </div>

                                <!-- 第二行：阶段概括动作胶囊 (整洁单行排列，使用高质量矢量 FileIcon) -->
                                <div v-if="todo.capsules.length > 0 || todo.status === 'succeeded'" class="stage-capsules-row">
                                    <!-- 纯读取阶段徽标 -->
                                    <span v-if="todo.capsules.length === 0" class="pill-pure-read">
                                        <FileIcon kind="read" :size="12" class="capsule-file-icon" />
                                        <span class="pill-text">本阶段无文件改动 (纯读取)</span>
                                    </span>

                                    <!-- 动作胶囊列表 (Excel、Word、PDF、代码修改、Reviewer、等待等) -->
                                    <template v-else>
                                        <template v-for="cap in todo.capsules.slice(0, 4)" :key="cap.id">
                                            <!-- 1. 代码修改/新增 (带行数，点击快速打开 Diff) -->
                                            <div
                                                v-if="cap.kind === 'code'"
                                                class="deck-chip clickable"
                                                :title="cap.isIntercepted
                                                    ? `${cap.label} · 写入已被门禁拦截（点击查看尝试内容）`
                                                    : `${cap.label} · 点击查看代码改动`"
                                                @click.stop="cap.chip && handleChipClick(todo, cap.chip)"
                                            >
                                                <FileIcon :name="cap.chip?.fileName || cap.label" kind="code" :size="12" class="capsule-file-icon" />
                                                <span class="chip-name">{{ cap.label }}</span>
                                                <span v-if="cap.additions && cap.additions > 0" class="chip-add">+{{ cap.additions }}</span>
                                                <span v-if="cap.deletions && cap.deletions > 0" class="chip-del">−{{ cap.deletions }}</span>
                                                <span v-if="cap.isIntercepted" class="chip-blocked">已拦截</span>
                                            </div>

                                            <!-- 2. Excel 数据表格产物 -->
                                            <div
                                                v-else-if="cap.kind === 'excel'"
                                                class="deck-chip capsule-excel"
                                                :class="{ clickable: !!cap.chip }"
                                                :title="`Excel 数据表格: ${cap.label}`"
                                                @click.stop="cap.chip && handleChipClick(todo, cap.chip)"
                                            >
                                                <FileIcon :name="cap.label" kind="excel" :size="12" class="capsule-file-icon" />
                                                <span class="chip-name">{{ cap.label }}</span>
                                                <span v-if="cap.additions && cap.additions > 0" class="chip-add">+{{ cap.additions }}</span>
                                            </div>

                                            <!-- 3. 文档 / 研报 -->
                                            <div
                                                v-else-if="cap.kind === 'doc'"
                                                class="deck-chip capsule-doc"
                                                :class="{ clickable: !!cap.chip }"
                                                :title="`文档/研报: ${cap.label}`"
                                                @click.stop="cap.chip && handleChipClick(todo, cap.chip)"
                                            >
                                                <FileIcon :name="cap.label" kind="doc" :size="12" class="capsule-file-icon" />
                                                <span class="chip-name">{{ cap.label }}</span>
                                                <span v-if="cap.additions && cap.additions > 0" class="chip-add">+{{ cap.additions }}</span>
                                            </div>

                                            <!-- 4. Reviewer 审查结论 -->
                                            <span
                                                v-else-if="cap.kind === 'reviewer'"
                                                class="deck-chip-reviewer"
                                                :class="{ 'is-fail': cap.verdict === 'fail' }"
                                            >
                                                <FileIcon kind="reviewer" :size="12" class="capsule-file-icon" />
                                                <span class="chip-name">{{ cap.label }}</span>
                                            </span>

                                            <!-- 5. 挂起等待 -->
                                            <span
                                                v-else-if="cap.kind === 'wait'"
                                                class="deck-chip-wait"
                                            >
                                                <FileIcon kind="wait" :size="12" class="capsule-file-icon" />
                                                <span class="chip-name">{{ cap.label }}</span>
                                            </span>

                                            <!-- 6. 读取文件上下文 (带专属文件图标与微标签) -->
                                            <div
                                                v-else-if="cap.kind === 'read'"
                                                class="deck-chip capsule-read"
                                                :title="cap.tooltip || `读取上下文: ${cap.label}`"
                                            >
                                                <FileIcon :name="cap.label" :size="12" class="capsule-file-icon" />
                                                <span class="chip-read-tag">读</span>
                                                <span class="chip-name">{{ cap.label }}</span>
                                            </div>

                                            <!-- 7. 终端命令 / 其他工具动作 -->
                                            <span
                                                v-else
                                                class="deck-chip-general"
                                            >
                                                <FileIcon :name="cap.label" :kind="cap.kind" :size="12" class="capsule-file-icon" />
                                                <span class="chip-name">{{ cap.label }}</span>
                                            </span>
                                        </template>

                                        <!-- 超出 4 个动作折叠徽标 -->
                                        <span v-if="todo.capsules.length > 4" class="deck-chip-overflow" :title="`该阶段共涉及 ${todo.capsules.length} 项动作`">
                                            +{{ todo.capsules.length - 4 }}
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
import type { AgentStepItem, AgentTokenUsage, AgentToolStatus, StageState, StageFailureState, FileChangeChip, FileDiffPayload, ReviewerVerdict, StageActionCapsule } from './types';
import { diffPayloadFromChip, parseStepFileChanges, aggregateFileChips, chipHasDiffContent, formatStageTitle, formatStageDuration, extractStageChronologicalCapsules } from './diffUtils';
import FileIcon from './FileIcon.vue';

export interface Level1TodoItem {
    id: string;
    planId: string;
    title: string;
    status: AgentToolStatus;
    stageState: StageState;
    failureState: StageFailureState;
    steps: AgentStepItem[];
    completedStepCount: number;
    totalStepCount: number;
    completionTokens: number;
    allTokens: number;
    durationMs: number;
    isCurrent: boolean;
    currentSubStepTitle?: string;
    readCount: number;
    searchCount: number;
    failedCount: number;
    fileChips: FileChangeChip[];
    capsules: StageActionCapsule[];
    readFiles: string[];
    hasMoreChipsCount: number;
    reviewerVerdict?: ReviewerVerdict;
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
    planGate?: any;
}>();

const emit = defineEmits<{
    (e: 'select-step', step: AgentStepItem): void;
    (e: 'open-diff', payload: FileDiffPayload): void;
}>();

const isExpanded = ref(true);
const isDismissed = ref(false);
const deckBodyRef = ref<HTMLElement | null>(null);

// 计时器
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

function getTodoDisplayTitle(todo: Level1TodoItem, idx: number): string {
    return formatStageTitle(todo.title, idx + 1);
}

function getTodoMetaSummary(todo: Level1TodoItem): string {
    const parts: string[] = [];
    parts.push(`${todo.steps.length} 步`);
    if (todo.searchCount > 0) parts.push(`搜 ${todo.searchCount}`);
    if (todo.readCount > 0) parts.push(`读 ${todo.readCount}`);
    if (todo.fileChips.length > 0) parts.push(`${todo.fileChips.length} 改动`);
    if (todo.durationMs > 0) parts.push(formatStageDuration(todo.durationMs));
    return parts.join(' · ');
}

/**
 * Chip 点击：有真实 Diff 内容 → 打开 Diff；否则选中该步骤
 */
function handleChipClick(todo: Level1TodoItem, chip: FileChangeChip | undefined) {
    if (!chip) return;
    if (chipHasDiffContent(chip)) {
        emit('open-diff', diffPayloadFromChip(chip));
        return;
    }
    const step = todo.steps.find(s => s.id === chip.stepId);
    if (step) {
        emit('select-step', step);
    }
}

// -----------------------------------------------------------------------------
// 🌟 一级 TodoList 聚合构建 (纯大步骤概括)
// -----------------------------------------------------------------------------
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
                stageState: 'pending',
                failureState: 'none',
                steps: [],
                completedStepCount: 0,
                totalStepCount: 0,
                completionTokens: 0,
                allTokens: 0,
                durationMs: 0,
                isCurrent: false,
                readCount: 0,
                searchCount: 0,
                failedCount: 0,
                fileChips: [],
                capsules: [],
                readFiles: [],
                hasMoreChipsCount: 0,
            };
            groups.set(key, item);
        }

        item.steps.push(step);
        item.totalStepCount++;
        if (step.status === 'succeeded') item.completedStepCount++;
        if (step.status === 'failed') item.failedCount++;

        // 提取步骤中的文件改动与读搜统计
        const parsed = parseStepFileChanges(step);
        item.readCount += parsed.readCount;
        item.searchCount += parsed.searchCount;
        if (parsed.chips && parsed.chips.length > 0) {
            item.fileChips.push(...parsed.chips);
        } else if (parsed.chip) {
            item.fileChips.push(parsed.chip);
        }
        if (parsed.readFiles && parsed.readFiles.length > 0) {
            for (const rf of parsed.readFiles) {
                if (!item.readFiles.includes(rf)) item.readFiles.push(rf);
            }
        }

        // 提取 Reviewer 结论
        if (parsed.reviewerVerdict && !item.reviewerVerdict) {
            item.reviewerVerdict = parsed.reviewerVerdict;
        }

        if (step.metrics?.completionTokens) item.completionTokens += step.metrics.completionTokens;
        if (step.metrics?.totalTokens) item.allTokens += step.metrics.totalTokens;
        if (step.metrics?.totalDurationMs) item.durationMs += step.metrics.totalDurationMs;
        else if (step.durationMs) item.durationMs += step.durationMs;
    }

    const items = [...groups.values()];
    return items.map((todo, idx) => {
        const isCurrent = props.isRunning && !props.summary?.trim() && idx === items.length - 1;
        const runningStep = todo.steps.find(s => s.status === 'running') || todo.steps[todo.steps.length - 1];

        // 1. 状态计算
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

        // 2. 失败状态判定
        let failureState: StageFailureState = 'none';
        if (todo.failedCount > 0) {
            let lastFailedIdx = -1;
            let lastOkIdx = -1;
            todo.steps.forEach((s, i) => {
                if (s.status === 'failed') lastFailedIdx = i;
                if (s.status === 'succeeded') lastOkIdx = i;
            });
            if (lastOkIdx > lastFailedIdx) {
                failureState = 'recovered';
            } else if (!props.isRunning) {
                failureState = 'unrecovered';
            } else {
                failureState = 'retrying';
            }
        }

        const stageState: StageState = isCurrent ? 'running' : 'collapsed';

        // 3. 文件 Chips 与胶囊
        const aggregatedChips = aggregateFileChips(todo.fileChips);
        const hasMoreChipsCount = Math.max(0, aggregatedChips.length - 4);
        const capsules = extractStageChronologicalCapsules(todo.steps);

        return {
            ...todo,
            status,
            stageState,
            failureState,
            isCurrent,
            fileChips: aggregatedChips,
            capsules,
            hasMoreChipsCount,
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

const gateBadge = computed(() => {
    const gate = props.planGate;
    if (!gate || typeof gate !== 'object') return null;

    if (gate.blockingCount && gate.blockingCount > 0) {
        return {
            type: 'danger',
            label: `🔴 门禁保护 (${gate.blockingCount} 条 Blocking)`,
            title: gate.blockingSummary || `当前存在 ${gate.blockingCount} 条 Blocking 致命缺陷未闭合，写入工具已物理封锁`,
        };
    }
    if (gate.batchIncomplete) {
        const delivered = gate.deliveredReviewers || 0;
        const expected = gate.expectedReviewers || 0;
        return {
            type: 'warning',
            label: `⏳ 齐套等待中 (${delivered}/${expected})`,
            title: '并发审查专家推演中，齐套屏障保护写入',
        };
    }
    if (gate.humanWaived) {
        return {
            type: 'success',
            label: '🟢 人工特批放行',
            title: '已获人工豁免放行，写入通道已开启',
        };
    }
    return null;
});

const headerTitle = computed(() => {
    if (props.isRunning) {
        if (level1Todos.value.length > 0) {
            return `任务进度 (${completedPlansCount.value}/${level1Todos.value.length})`;
        }
        return props.isThinking ? '思考规划中...' : '执行任务中...';
    }
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

const totalCompletionTokensCount = computed(() => {
    let sum = 0;
    for (const s of props.steps) {
        if (s.metrics?.completionTokens) {
            sum += s.metrics.completionTokens;
        }
    }
    return sum;
});

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
/* 外层容器：与输入框两端对齐 */
.agent-progress-deck-wrap {
    padding: 0 clamp(16px, 4vw, 48px) 8px;
    box-sizing: border-box;
    width: 100%;
}

/* 主卡片：高级深空灰底色，低调现代 */
.agent-progress-deck {
    width: 100%;
    border-radius: 12px;
    background: #18181b;
    border: 1px solid rgba(255, 255, 255, 0.08);
    box-shadow: 0 4px 18px rgba(0, 0, 0, 0.25);
    overflow: hidden;
    box-sizing: border-box;
    transition: border-color 0.2s ease, box-shadow 0.2s ease;
}

.agent-progress-deck.is-running {
    border-color: rgba(99, 102, 241, 0.3);
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.35), 0 0 1px rgba(99, 102, 241, 0.3);
}

/* 顶栏 (精简紧凑高度) */
.deck-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 5px 10px;
    cursor: pointer;
    user-select: none;
    background: rgba(255, 255, 255, 0.02);
    transition: background 0.15s ease;
}

.deck-header:hover {
    background: rgba(255, 255, 255, 0.04);
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
    color: #10b981;
    font-size: 13px;
    font-weight: 600;
}

.status-dot {
    color: #71717a;
    font-size: 12px;
}

.manus-spinner {
    width: 12px;
    height: 12px;
    border: 1.5px solid rgba(255, 255, 255, 0.15);
    border-top-color: #818cf8;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
    display: inline-block;
}

.manus-spinner.mini {
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
    color: #f4f4f5;
    white-space: nowrap;
    flex-shrink: 0;
}

.status-sub {
    color: #a1a1aa;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}

.gate-status-badge {
    display: inline-flex;
    align-items: center;
    font-size: 11px;
    font-weight: 600;
    padding: 1px 7px;
    border-radius: 9999px;
    line-height: 1.4;
    white-space: nowrap;
    flex-shrink: 0;
    transition: all 0.2s ease;
}

.gate-status-badge.danger {
    background: rgba(239, 68, 68, 0.15);
    color: #ef4444;
    border: 1px solid rgba(239, 68, 68, 0.35);
}

.gate-status-badge.warning {
    background: rgba(245, 158, 11, 0.15);
    color: #f59e0b;
    border: 1px solid rgba(245, 158, 11, 0.35);
}

.gate-status-badge.success {
    background: rgba(16, 185, 129, 0.15);
    color: #10b981;
    border: 1px solid rgba(16, 185, 129, 0.35);
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

/* 列表主体容器 (极简大步骤概括，精简紧凑高度) */
.deck-body {
    border-top: 1px solid rgba(255, 255, 255, 0.06);
    max-height: 140px;
    overflow-y: auto;
    padding: 4px 6px;
    position: relative;
    scrollbar-width: thin;
    scrollbar-color: rgba(255, 255, 255, 0.1) transparent;
}

.todolist-container {
    display: flex;
    flex-direction: column;
    gap: 3.5px;
}

/* 🌟 大步骤概括卡片 (更扁平轻巧) */
.stage-summary-card {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 4.5px 8px;
    border-radius: 6px;
    background: rgba(255, 255, 255, 0.025);
    border: 1px solid rgba(255, 255, 255, 0.06);
    overflow: hidden;
    transition: all 0.2s ease;
}

.stage-summary-card:hover {
    background: rgba(255, 255, 255, 0.04);
    border-color: rgba(255, 255, 255, 0.1);
}

/* 🌟 核心需求：在那个阶段，然后这个条条就闪烁一下 (流光呼吸与发光脉冲) */
.stage-summary-card.is-stage-active-pulse {
    background: linear-gradient(90deg, rgba(99, 102, 241, 0.09) 0%, rgba(59, 130, 246, 0.15) 50%, rgba(99, 102, 241, 0.09) 100%);
    background-size: 200% 100%;
    border-color: rgba(99, 102, 241, 0.5);
    box-shadow: 0 0 14px rgba(99, 102, 241, 0.22), inset 0 0 12px rgba(99, 102, 241, 0.08);
    animation: stage-card-breathe 2.4s ease-in-out infinite, stage-bg-flow 3.5s linear infinite;
}

@keyframes stage-card-breathe {
    0%, 100% {
        border-color: rgba(99, 102, 241, 0.35);
        box-shadow: 0 0 8px rgba(99, 102, 241, 0.15);
    }
    50% {
        border-color: rgba(129, 140, 248, 0.7);
        box-shadow: 0 0 18px rgba(99, 102, 241, 0.35);
    }
}

@keyframes stage-bg-flow {
    0% { background-position: 0% 50%; }
    100% { background-position: 200% 50%; }
}

/* 优雅穿梭流光条 */
.stage-shimmer-beam {
    position: absolute;
    top: 0;
    left: -100%;
    width: 60%;
    height: 100%;
    background: linear-gradient(90deg, transparent, rgba(255, 255, 255, 0.08), rgba(129, 140, 248, 0.15), transparent);
    transform: skewX(-20deg);
    animation: shimmer-slide 2.8s cubic-bezier(0.4, 0, 0.2, 1) infinite;
    pointer-events: none;
}

@keyframes shimmer-slide {
    0% { left: -100%; }
    60%, 100% { left: 160%; }
}

/* 第一行布局 */
.stage-card-row {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
}

.stage-status-icon {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    flex-shrink: 0;
}

.stage-check-icon {
    color: #10b981;
    font-size: 13px;
    font-weight: 600;
}

.stage-fail-icon {
    color: #ef4444;
    font-size: 12px;
    font-weight: 600;
}

.stage-dot-icon {
    color: #52525b;
    font-size: 12px;
}

.stage-title-wrap {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    flex: 1;
}

.stage-title-text {
    font-size: 12px;
    font-weight: 500;
    color: #d4d4d8;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}

.stage-title-text.is-active-title {
    color: #ffffff;
    font-weight: 600;
    text-shadow: 0 0 10px rgba(99, 102, 241, 0.4);
}

.stage-fail-tag {
    font-size: 9.5px;
    padding: 1px 4px;
    border-radius: 3px;
    white-space: nowrap;
}

.tag-recovered {
    background: rgba(16, 185, 129, 0.12);
    color: #34d399;
    border: 1px solid rgba(16, 185, 129, 0.25);
}

.tag-retrying {
    background: rgba(245, 158, 11, 0.12);
    color: #fbbf24;
    border: 1px solid rgba(245, 158, 11, 0.25);
}

.stage-right-meta {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
    font-size: 11px;
}

.stage-live-badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: #a5b4fc;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-weight: 500;
}

.live-blink-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: #6366f1;
    box-shadow: 0 0 8px #6366f1;
    animation: dot-blink 1.2s ease-in-out infinite;
}

@keyframes dot-blink {
    0%, 100% { opacity: 0.3; transform: scale(0.85); }
    50% { opacity: 1; transform: scale(1.15); }
}

.stage-done-meta {
    color: #71717a;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

/* 第二行：阶段产物与动作胶囊 */
.stage-capsules-row {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 4px;
    margin-left: 24px;
    margin-top: 1px;
}

.pill-pure-read {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 10.5px;
    color: #71717a;
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid rgba(255, 255, 255, 0.05);
    padding: 1.5px 6px;
    border-radius: 4px;
}

/* 胶囊微样式 */
.deck-chip {
    display: inline-flex;
    align-items: center;
    gap: 3.5px;
    font-size: 10.5px;
    color: #d4d4d8;
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid rgba(255, 255, 255, 0.08);
    padding: 1.5px 5.5px;
    border-radius: 4px;
    transition: all 0.15s ease;
    user-select: none;
}

.deck-chip.clickable {
    cursor: pointer;
}

.deck-chip.clickable:hover {
    background: rgba(255, 255, 255, 0.08);
    border-color: rgba(99, 102, 241, 0.35);
    color: #ffffff;
    transform: translateY(-0.5px);
}

.deck-chip.capsule-excel {
    border-color: rgba(16, 185, 129, 0.2);
    background: rgba(16, 185, 129, 0.06);
}

.deck-chip.capsule-doc {
    border-color: rgba(59, 130, 246, 0.2);
    background: rgba(59, 130, 246, 0.06);
}

.deck-chip.capsule-read {
    border-color: rgba(99, 102, 241, 0.2);
    background: rgba(99, 102, 241, 0.06);
}

.chip-read-tag {
    font-size: 9px;
    padding: 0 2.5px;
    border-radius: 2px;
    background: rgba(99, 102, 241, 0.2);
    color: #a5b4fc;
    font-weight: 600;
}

.deck-chip-reviewer {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 10px;
    background: rgba(99, 102, 241, 0.12);
    border: 1px solid rgba(99, 102, 241, 0.25);
    color: #c7d2fe;
    padding: 1.5px 5px;
    border-radius: 4px;
}

.deck-chip-reviewer.is-fail {
    background: rgba(239, 68, 68, 0.12);
    border-color: rgba(239, 68, 68, 0.25);
    color: #fca5a5;
}

.deck-chip-wait {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 10px;
    background: rgba(245, 158, 11, 0.12);
    border: 1px solid rgba(245, 158, 11, 0.25);
    color: #fde68a;
    padding: 1.5px 5px;
    border-radius: 4px;
}

.deck-chip-general {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 10px;
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid rgba(255, 255, 255, 0.07);
    color: #a1a1aa;
    padding: 1.5px 5px;
    border-radius: 4px;
}

.chip-name {
    max-width: 140px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}

.chip-add {
    color: #34d399;
    font-weight: 500;
    font-size: 10.5px;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

.chip-del {
    color: #f87171;
    font-weight: 500;
    font-size: 10.5px;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

.chip-blocked {
    background: rgba(239, 68, 68, 0.2);
    color: #fca5a5;
    border-radius: 3px;
    padding: 0 3px;
    font-size: 9.5px;
}

.deck-chip-overflow {
    font-size: 10.5px;
    color: #71717a;
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid rgba(255, 255, 255, 0.05);
    padding: 1px 5px;
    border-radius: 4px;
}

/* 展开收起过渡动画 */
.deck-slide-enter-active,
.deck-slide-leave-active {
    transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1);
}

.deck-slide-enter-from,
.deck-slide-leave-to {
    opacity: 0;
    transform: translateY(6px);
}

.deck-expand-enter-active,
.deck-expand-leave-active {
    transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1);
}

.deck-expand-enter-from,
.deck-expand-leave-to {
    opacity: 0;
    max-height: 0;
}
</style>
