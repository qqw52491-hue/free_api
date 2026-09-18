<template>
    <transition name="deck-slide">
        <div v-if="shouldShow" class="agent-progress-deck-wrap">
            <div class="agent-progress-deck" :class="{ 'is-running': isRunning, 'is-expanded': isExpanded }">
                <!-- 🌟 展开的文件列表 (位于常驻摘要条上方，贴合底部输入框体验) -->
                <transition name="deck-drawer">
                    <div v-if="isExpanded && allModifiedFiles.length > 0" class="deck-files-drawer">
                        <div
                            v-for="file in allModifiedFiles"
                            :key="file.path || file.fileName"
                            class="deck-file-row is-clickable"
                            title="点击查看代码改动对比"
                            @click="$emit('open-diff', diffPayloadFromChip(file))"
                        >
                            <!-- 状态方点 ⊡ -->
                            <span
                                class="file-state-badge"
                                :class="'is-' + (file.actionKind || 'modify')"
                                :title="file.actionKind === 'create' ? '新建' : '修改'"
                            >
                                <span class="file-state-dot"></span>
                            </span>

                            <!-- +新增 -删除 统计 -->
                            <span v-if="file.additions && file.additions > 0" class="diff-stat-add">+{{ file.additions }}</span>
                            <span v-if="file.deletions && file.deletions > 0" class="diff-stat-del">-{{ file.deletions }}</span>

                            <!-- 文件名与路径 -->
                            <span class="file-row-name">{{ file.fileName }}</span>
                            <span v-if="file.path && file.path !== file.fileName" class="file-row-path">{{ file.path }}</span>
                        </div>
                    </div>
                </transition>

                <!-- 🌟 底部摘要条 (点击整条展开/收起) -->
                <div
                    class="deck-summary-bar"
                    :class="{ 'is-clickable': allModifiedFiles.length > 0 }"
                    :title="allModifiedFiles.length > 0 ? (isExpanded ? '点击收起文件列表' : '点击展开文件列表') : ''"
                    @click="allModifiedFiles.length > 0 && toggleExpanded()"
                >
                    <div class="summary-left">
                        <div class="summary-icon-wrap">
                            <span v-if="isRunning" class="manus-spinner"></span>
                            <svg
                                v-else-if="allModifiedFiles.length > 0"
                                class="summary-file-icon"
                                viewBox="0 0 24 24"
                                width="14"
                                height="14"
                                fill="none"
                                stroke="currentColor"
                                stroke-width="2"
                                stroke-linecap="round"
                                stroke-linejoin="round"
                            >
                                <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
                                <polyline points="14 2 14 8 20 8" />
                                <line x1="16" y1="13" x2="8" y2="13" />
                                <line x1="16" y1="17" x2="8" y2="17" />
                            </svg>
                            <span v-else-if="allSucceeded" class="status-check">✓</span>
                            <span v-else class="status-dot">○</span>
                        </div>

                        <div class="summary-text-wrap">
                            <span v-if="isRunning" class="summary-title">{{ runningTitle }}</span>
                            <span v-else-if="allModifiedFiles.length > 0" class="summary-title">
                                {{ allModifiedFiles.length }} {{ allModifiedFiles.length === 1 ? 'File' : 'Files' }} With Changes
                            </span>
                            <span v-else class="summary-title">{{ headerTitle }}</span>

                            <!-- 增删量摘要 (当折叠或概览时一目了然) -->
                            <span v-if="allModifiedFiles.length > 0 && totalAdditions > 0" class="diff-stat-add mini">+{{ totalAdditions }}</span>
                            <span v-if="allModifiedFiles.length > 0 && totalDeletions > 0" class="diff-stat-del mini">-{{ totalDeletions }}</span>

                            <!-- 门禁状态徽标 -->
                            <span
                                v-if="gateBadge"
                                class="gate-status-badge"
                                :class="gateBadge.type"
                                :title="gateBadge.title"
                            >
                                {{ gateBadge.label }}
                            </span>

                            <span v-if="isRunning && currentExecutingSubStep" class="summary-sub">
                                · {{ currentExecutingSubStep }}
                            </span>
                        </div>
                    </div>

                    <div class="summary-right">
                        <!-- 右侧指标统计 (Token 与耗时) -->
                        <div v-if="totalCompletionTokensCount > 0 || displayDurationText || isRunning" class="summary-metrics">
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
                    </div>
                </div>
            </div>
        </div>
    </transition>
</template>

<script setup lang="ts">
import { computed, ref, watch, onMounted, onUnmounted } from 'vue';
import type { AgentStepItem, AgentTokenUsage, AgentToolStatus, StageState, StageFailureState, FileChangeChip, FileDiffPayload, ReviewerVerdict, StageActionCapsule } from './types';
import { diffPayloadFromChip, parseStepFileChanges, aggregateFileChips, extractStageChronologicalCapsules } from './diffUtils';

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

defineEmits<{
    (e: 'select-step', step: AgentStepItem): void;
    (e: 'open-diff', payload: FileDiffPayload): void;
}>();

const isExpanded = ref(true);

const totalAdditions = computed(() => {
    return allModifiedFiles.value.reduce((acc, f) => acc + (f.additions || 0), 0);
});

const totalDeletions = computed(() => {
    return allModifiedFiles.value.reduce((acc, f) => acc + (f.deletions || 0), 0);
});

const allModifiedFiles = computed<FileChangeChip[]>(() => {
    const rawChips: FileChangeChip[] = [];
    for (const step of props.steps || []) {
        const changes = parseStepFileChanges(step);
        // 被拦截/失败的写入未真正落地，不计入"修改过的文件"
        if (changes.chip && !changes.chip.isIntercepted && ((changes.chip.additions || 0) > 0 || (changes.chip.deletions || 0) > 0 || changes.chip.actionKind === 'modify' || changes.chip.actionKind === 'create')) {
            rawChips.push(changes.chip);
        }
        for (const c of changes.chips || []) {
            if (!c.isIntercepted && ((c.additions || 0) > 0 || (c.deletions || 0) > 0 || c.actionKind === 'modify' || c.actionKind === 'create')) {
                rawChips.push(c);
            }
        }
    }
    return aggregateFileChips(rawChips);
});

// 计时器
const stepStartTime = ref<number>(Date.now());
const nowTime = ref<number>(Date.now());
let timerInterval: ReturnType<typeof setInterval> | null = null;

watch(() => props.isRunning, (running) => {
    if (running) {
        isExpanded.value = true;
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
    if (props.isRunning) return true;
    return allModifiedFiles.value.length > 0;
});

const runningTitle = computed(() => {
    if (props.isThinking) return '思考规划中...';
    if (props.actionPhaseText) return props.actionPhaseText;
    if (allModifiedFiles.value.length > 0) {
        return `正在修改 (${allModifiedFiles.value.length} 个文件)...`;
    }
    return '正在执行任务...';
});

function toggleExpanded() {
    isExpanded.value = !isExpanded.value;
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
        return props.isThinking ? '思考规划中...' : '执行任务中...';
    }
    if (props.sessionTitle) {
        return props.sessionTitle;
    }
    if (props.summary) {
        const firstLine = props.summary.split('\n')[0].replace(/^[#*\-•\s`>]+/, '').trim();
        if (firstLine && firstLine.length <= 35) return firstLine;
    }
    if (allModifiedFiles.value.length > 0) {
        return `已修改 ${allModifiedFiles.value.length} 个文件`;
    }
    return '文件变更';
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

/* 抽屉平滑展开收起动画 */
.deck-drawer-enter-active,
.deck-drawer-leave-active {
    transition: max-height 0.22s cubic-bezier(0.16, 1, 0.3, 1), opacity 0.18s ease;
    overflow: hidden;
}

.deck-drawer-enter-from,
.deck-drawer-leave-to {
    max-height: 0;
    opacity: 0;
}

.deck-drawer-enter-to,
.deck-drawer-leave-from {
    max-height: 280px;
    opacity: 1;
}

/* 🌟 展开的文件抽屉列表 (在底部摘要条上方) */
.deck-files-drawer {
    display: flex;
    flex-direction: column;
    gap: 1.5px;
    max-height: 240px;
    overflow-y: auto;
    padding: 6px 8px;
    background: rgba(18, 18, 20, 0.95);
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
    box-sizing: border-box;
    scrollbar-width: thin;
    scrollbar-color: rgba(255, 255, 255, 0.18) transparent;
}

.deck-files-drawer::-webkit-scrollbar {
    width: 4px;
}

.deck-files-drawer::-webkit-scrollbar-thumb {
    background: rgba(255, 255, 255, 0.18);
    border-radius: 4px;
}

.deck-files-drawer::-webkit-scrollbar-thumb:hover {
    background: rgba(255, 255, 255, 0.3);
}

/* 单个文件行：对齐用户标杆样式 [⊙] +23 -105 FileName path */
.deck-file-row {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 3.5px 8px;
    border-radius: 5px;
    font-size: 12px;
    line-height: 1.4;
    color: #cbd5e1;
    user-select: text;
    transition: background 0.12s ease;
}

.deck-file-row.is-clickable {
    cursor: pointer;
}

.deck-file-row.is-clickable:hover {
    background: rgba(255, 255, 255, 0.06);
}

.deck-file-row.is-clickable:hover .file-row-name {
    color: #38bdf8;
}

/* 状态方点 ⊡ */
.file-state-badge {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 13px;
    height: 13px;
    border-radius: 3px;
    border: 1.2px solid rgba(245, 158, 11, 0.6);
    background: rgba(245, 158, 11, 0.12);
    flex-shrink: 0;
    box-sizing: border-box;
}

.file-state-badge .file-state-dot {
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: #f59e0b;
}

.file-state-badge.is-create {
    border-color: rgba(16, 185, 129, 0.6);
    background: rgba(16, 185, 129, 0.12);
}

.file-state-badge.is-create .file-state-dot {
    background: #10b981;
}

.file-state-badge.is-delete {
    border-color: rgba(239, 68, 68, 0.6);
    background: rgba(239, 68, 68, 0.12);
}

.file-state-badge.is-delete .file-state-dot {
    background: #ef4444;
}

/* +新增 -删除 统计标签 */
.diff-stat-add {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 11px;
    font-weight: 600;
    color: #22c55e;
    flex-shrink: 0;
}

.diff-stat-del {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 11px;
    font-weight: 600;
    color: #ef4444;
    flex-shrink: 0;
}

.diff-stat-add.mini,
.diff-stat-del.mini {
    font-size: 10.5px;
    opacity: 0.85;
    margin-left: 2px;
}

/* 文件名称与相对路径 */
.file-row-name {
    font-size: 11.5px;
    font-weight: 500;
    color: #f1f5f9;
    flex-shrink: 0;
}

.file-row-path {
    font-size: 10.5px;
    color: #64748b;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
    flex: 1;
}

/* 🌟 底部摘要条 (常驻贴紧输入框) */
.deck-summary-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 6px 12px;
    user-select: none;
    background: #18181b;
    transition: background 0.15s ease;
    box-sizing: border-box;
}

.deck-summary-bar.is-clickable {
    cursor: pointer;
}

.deck-summary-bar.is-clickable:hover {
    background: rgba(255, 255, 255, 0.04);
}

.summary-left {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    flex: 1;
}

.summary-icon-wrap {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    flex-shrink: 0;
}

.summary-file-icon {
    color: #94a3b8;
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

@keyframes spin {
    to { transform: rotate(360deg); }
}

.summary-text-wrap {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    overflow: hidden;
    font-size: 12px;
}

.summary-title {
    font-weight: 500;
    color: #f4f4f5;
    white-space: nowrap;
    flex-shrink: 0;
}

.summary-sub {
    color: #a1a1aa;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    font-size: 11.5px;
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

.summary-right {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
}

.summary-metrics {
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

/* 列表主体容器 (极简大步骤概括，精简紧凑高度) */
.deck-body {
    border-top: 1px solid rgba(255, 255, 255, 0.06);
    max-height: 140px;
    overflow-y: auto;
    padding: 4px 6px;
    position: relative;
    scrollbar-width: thin;
    scrollbar-color: rgba(255, 255, 255, 0.18) transparent;
}

.deck-body::-webkit-scrollbar {
    width: 4px;
}

.deck-body::-webkit-scrollbar-thumb {
    background: rgba(255, 255, 255, 0.18);
    border-radius: 4px;
}

.deck-body::-webkit-scrollbar-thumb:hover {
    background: rgba(255, 255, 255, 0.3);
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
