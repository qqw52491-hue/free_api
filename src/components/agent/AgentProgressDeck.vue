<template>
    <transition name="deck-slide">
        <div v-if="shouldShow" class="agent-progress-deck-wrap">
            <div class="agent-progress-deck" :class="{ 'is-running': isRunning, 'is-expanded': isExpanded }">
                <!-- 头部状态摘要条 (极简顶栏) -->
                <div class="deck-header" @click="allActionItems.length > 3 && toggleExpanded()">
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
                            <span v-else-if="allModifiedFiles.length > 0" class="status-sub">
                                · 修改了 {{ allModifiedFiles.length }} 个文件
                            </span>
                            <span v-else-if="allActionItems.length > 0" class="status-sub">
                                · 完成了 {{ allActionItems.length }} 项操作
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

                        <!-- 完成时的关闭按钮 -->
                        <button
                            v-if="!isRunning"
                            type="button"
                            class="deck-close-btn"
                            title="隐藏任务概括"
                            @click.stop="dismiss"
                        >
                            <svg viewBox="0 0 24 24"><path d="M18 6 6 18M6 6l12 12"/></svg>
                        </button>
                    </div>
                </div>

                <!-- 🌟 用户标杆：改动文件与关键动作托盘 (带多了隐藏、底部半透明遮罩与展开小箭头) -->
                <div v-if="allActionItems.length > 0" class="deck-actions-viewport-wrap">
                    <div
                        class="deck-actions-viewport"
                        :class="{ 'is-collapsed-mask': !isExpanded && allActionItems.length > 3 }"
                    >
                        <div
                            v-for="item in (isExpanded ? allActionItems : allActionItems.slice(0, 10))"
                            :key="item.id"
                            class="deck-action-row"
                            :class="{ 'is-clickable': !!item.chip }"
                            :title="item.chip ? '点击查看代码改动对比' : undefined"
                            @click="item.chip && $emit('open-diff', diffPayloadFromChip(item.chip))"
                        >
                            <FileIcon
                                v-if="item.chip?.fileName"
                                :name="item.chip.fileName"
                                :size="12"
                                class="action-row-file-icon"
                            />
                            <span v-else class="action-row-icon">{{ item.icon }}</span>
                            <span class="action-row-text">{{ item.text }}</span>
                            <span v-if="item.subText" class="action-row-sub">{{ item.subText }}</span>
                            <span v-if="item.additions && item.additions > 0" class="chip-add">+{{ item.additions }}</span>
                            <span v-if="item.deletions && item.deletions > 0" class="chip-del">−{{ item.deletions }}</span>
                        </div>
                    </div>

                    <!-- 底部渐隐折叠悬浮小箭头 (多了隐藏，点击展开更多) -->
                    <button
                        v-if="allActionItems.length > 3"
                        type="button"
                        class="deck-actions-expand-btn"
                        :title="isExpanded ? '收起列表' : `展开全部 (${allActionItems.length} 项)`"
                        @click.stop="toggleExpanded"
                    >
                        <svg viewBox="0 0 24 24" class="chevron-mini-icon" :class="{ 'is-flipped': isExpanded }">
                            <path d="m6 9 6 6 6-6" />
                        </svg>
                    </button>
                </div>
            </div>
        </div>
    </transition>
</template>

<script setup lang="ts">
import { computed, ref, watch, onMounted, onUnmounted } from 'vue';
import type { AgentStepItem, AgentTokenUsage, AgentToolStatus, StageState, StageFailureState, FileChangeChip, FileDiffPayload, ReviewerVerdict, StageActionCapsule } from './types';
import { diffPayloadFromChip, formatStepOperation, parseStepFileChanges, aggregateFileChips, extractStageChronologicalCapsules } from './diffUtils';
import FileIcon from './FileIcon.vue';

export interface DeckActionItem {
    id: string;
    icon: string;
    text: string;
    subText?: string;
    chip?: FileChangeChip;
    additions?: number;
    deletions?: number;
}

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

const isExpanded = ref(false);
const isDismissed = ref(false);

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

const allActionItems = computed<DeckActionItem[]>(() => {
    const items: DeckActionItem[] = [];
    const seenKeys = new Set<string>();

    // 1. 优先列出真实改动/新增的文件 (高价值产物，点击可直接打开 Diff 视图)
    for (const f of allModifiedFiles.value) {
        const key = `file-${f.path || f.fileName}`;
        if (seenKeys.has(key)) continue;
        seenKeys.add(key);
        items.push({
            id: key,
            icon: f.actionKind === 'create' ? '✨' : '📄',
            text: f.actionKind === 'create' ? `Created ${f.fileName}` : `Edited ${f.fileName}`,
            subText: f.path && f.path !== f.fileName ? f.path : undefined,
            chip: f,
            additions: f.additions,
            deletions: f.deletions,
        });
    }

    // 2. 依次聚合步骤中的关键操作 (终端命令 Ran command:, 查看文件 Viewed:, 检索 Searched for:, MCP 等)
    for (const s of props.steps || []) {
        const op = formatStepOperation(s);
        const changes = parseStepFileChanges(s);

        // 如果这个步骤产生的改动文件已经在上面展示了，且无独立命令，则跳过避免重复
        if (changes.chip && (changes.chip.additions || 0) > 0 && items.some(it => it.chip?.fileName === changes.chip?.fileName)) {
            continue;
        }

        const rawTool = (s.tool || s.action || '').toLowerCase();
        let formattedText = '';
        let icon = op.badgeIcon || '⚡';

        if (rawTool.includes('shell') || rawTool.includes('command') || rawTool.includes('run_command')) {
            formattedText = op.actionText.startsWith('Ran command:') ? op.actionText : `Ran command: ${op.actionText}`;
            icon = '⚡';
        } else if (rawTool.includes('search') || rawTool.includes('grep')) {
            formattedText = `Searched for ${op.actionText}`;
            icon = '🔍';
        } else if (rawTool.includes('view') || rawTool.includes('read')) {
            formattedText = `Viewed ${op.actionText}`;
            icon = '📖';
        } else {
            const desc = (s.description || '').replace(/^[💡🎯📌🚀⚠️⚡✏️💭❌]\s*/, '').trim();
            formattedText = desc && desc !== '执行操作' ? desc : op.actionText;
        }

        if (!formattedText || formattedText === '执行下一步操作') continue;
        const key = `action-${s.id}-${formattedText}`;
        if (seenKeys.has(key)) continue;
        seenKeys.add(key);

        items.push({
            id: key,
            icon,
            text: formattedText,
            chip: changes.chip,
            additions: changes.chip?.additions,
            deletions: changes.chip?.deletions,
        });
    }

    return items;
});

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
        return `已完成 · 修改了 ${allModifiedFiles.value.length} 个文件`;
    }
    if (allActionItems.value.length > 0) {
        return `已完成 · 执行了 ${allActionItems.value.length} 项操作`;
    }
    return '执行完成';
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

/* 🌟 改动文件与关键动作托盘 */
.deck-actions-viewport-wrap {
    position: relative;
    padding: 6px 12px 10px;
    background: rgba(0, 0, 0, 0.25);
    border-top: 1px solid rgba(255, 255, 255, 0.05);
}

.deck-actions-viewport {
    display: flex;
    flex-direction: column;
    gap: 3px;
    max-height: 280px;
    overflow-y: auto;
    transition: max-height 0.25s cubic-bezier(0.16, 1, 0.3, 1);
}

/* 溢出折叠渐变遮罩 (多了隐藏) */
.deck-actions-viewport.is-collapsed-mask {
    max-height: 84px;
    overflow: hidden;
    -webkit-mask-image: linear-gradient(to bottom, #000 45%, transparent 100%);
    mask-image: linear-gradient(to bottom, #000 45%, transparent 100%);
}

.deck-action-row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 2.5px 6px;
    border-radius: 4px;
    font-size: 11.5px;
    line-height: 1.4;
    color: #cbd5e1;
    user-select: text;
    transition: background 0.12s ease;
}

.deck-action-row.is-clickable {
    cursor: pointer;
}

.deck-action-row.is-clickable:hover {
    background: rgba(255, 255, 255, 0.05);
    color: #38bdf8;
}

.action-row-icon {
    font-size: 11px;
    flex-shrink: 0;
    opacity: 0.85;
}

.action-row-text {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 11px;
    font-weight: 500;
    color: #e2e8f0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
}

.action-row-sub {
    font-size: 10px;
    color: #64748b;
    margin-left: 4px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 180px;
}

/* 底部居中悬浮展开小箭头按钮 */
.deck-actions-expand-btn {
    position: absolute;
    bottom: 3px;
    left: 50%;
    transform: translateX(-50%);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 18px;
    background: #27272a;
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 10px;
    color: #94a3b8;
    cursor: pointer;
    box-shadow: 0 2px 6px rgba(0, 0, 0, 0.4);
    transition: all 0.15s ease;
    z-index: 2;
}

.deck-actions-expand-btn:hover {
    background: #3f3f46;
    color: #f1f5f9;
    border-color: rgba(255, 255, 255, 0.25);
    transform: translateX(-50%) scale(1.08);
}

.chevron-mini-icon {
    width: 12px;
    height: 12px;
    fill: none;
    stroke: currentColor;
    stroke-width: 2.5;
    stroke-linecap: round;
    stroke-linejoin: round;
    transition: transform 0.2s ease;
}

.chevron-mini-icon.is-flipped {
    transform: rotate(180deg);
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
