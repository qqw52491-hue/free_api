<template>
    <div class="agent-workspace-deck">
        <!-- 📍 历史步骤子任务回看通知条 -->
        <div v-if="isViewingHistoryStep && selectedStep" class="history-step-notice-bar">
            <div class="notice-info">
                <span class="notice-icon">📍</span>
                <span class="notice-text">
                    正在回看 <strong>步骤 #{{ selectedStep.stepId + 1 }}</strong> 派发的子任务 (共 {{ displayedWorkers.length }} 个)
                </span>
            </div>
            <button type="button" class="btn-clear-history-step" title="退出历史回看，返回实时监控" @click="$emit('clear-selected-step')">
                退出回看 ✕
            </button>
        </div>

        <AgentTeamTopology
            :workers="displayedWorkers"
            :master-status="teamState.masterStatus"
            :goal="currentGoal"
            :selected-id="selectedWorker?.id || 'master'"
            @select="handleWorkerClick"
        />

        <!-- ================= 2. 下半部分：当前选中的 Agent 专属全高流程工作台 ================= -->
        <section v-if="selectedWorker" class="agent-chains-tabs-section">
            <div class="chain-tab-content-panel">
                <!-- 选中的子 Agent 专属独立链路 -->
                <div v-if="selectedWorker" class="chain-detail-view worker-view">
                    <div class="agent-detail-header">
                        <div class="detail-header-left">
                            <span class="agent-big-icon">{{ getRoleIcon(selectedWorker.role) }}</span>
                            <div class="detail-name-meta">
                                <strong class="detail-agent-name" :title="selectedWorker.name">{{ formatWorkerName(selectedWorker.name) }} 链路</strong>
                                <span class="detail-agent-tool">{{ selectedWorker.roleLabel }}<template v-if="selectedWorker.currentTool"> · {{ getShortToolName(selectedWorker.currentTool) }}</template></span>
                                <span v-if="selectedWorker.model" class="worker-card-model" :title="`模型: ${selectedWorker.model}`">{{ selectedWorker.model }}</span>
                            </div>
                        </div>
                        <div class="detail-header-right">
                            <span v-if="selectedWorker.durationMs" class="worker-dur-pill">
                                ⏱ {{ (selectedWorker.durationMs / 1000).toFixed(2) }}s
                            </span>
                            <span class="stage-metric-pill" :class="`status-${selectedWorker.status}`">
                                {{ statusText(selectedWorker.status) }}
                            </span>
                        </div>
                    </div>

                    <!-- 🌟 子 Agent 流式思考滚动条 (与主 Agent ambient-live-text-bar 完全一致，支持横向平滑滚动 + 展开纵向深度推演) -->
                    <div v-if="isWorkerThinking" class="ambient-live-text-row worker-live-thought-row">
                        <div class="ambient-live-text-bar" @click="isWorkerThoughtExpanded = !isWorkerThoughtExpanded">
                            <span class="live-think-label">
                                <span class="live-think-square-icon">
                                    <svg class="thought-square-svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                                        <path d="M12 5a3 3 0 1 0-5.997.125 4 4 0 0 0-2.526 5.77 4 4 0 0 0 .556 6.588A4 4 0 1 0 12 18Z"/>
                                        <path d="M12 5a3 3 0 1 1 5.997.125 4 4 0 0 1 2.526 5.77 4 4 0 0 1-.556 6.588A4 4 0 1 1 12 18Z"/>
                                        <path d="M12 5v13"/>
                                        <path d="M15.5 8.5a4 4 0 0 0-7 0"/>
                                        <path d="M16 12a4 4 0 0 0-8 0"/>
                                        <path d="M15.5 15.5a4 4 0 0 0-7 0"/>
                                    </svg>
                                </span>
                                <span class="live-think-title">{{ selectedWorker.status === 'running' ? '思考中：' : '思考过程：' }}</span>
                            </span>
                            <div class="ambient-live-viewport">
                                <Transition name="ticker-flip">
                                    <span
                                        :key="`worker-think-${selectedWorker?.id || 'none'}-${workerThoughtInfo.lineKey}`"
                                        v-scroll-tail
                                        class="ambient-live-scroll-text"
                                        :title="workerThoughtInfo.text"
                                    >
                                        {{ workerThoughtInfo.text || '正在后台深入推导与研判相关代码与风险…' }}
                                    </span>
                                </Transition>
                            </div>
                            <button
                                type="button"
                                class="thought-expand-btn"
                                :title="isWorkerThoughtExpanded ? '收起完整思考' : '展开查看完整思考过程'"
                                @click.stop="isWorkerThoughtExpanded = !isWorkerThoughtExpanded"
                            >
                                <span class="thought-expand-text">{{ isWorkerThoughtExpanded ? '收起' : '展开' }}</span>
                                <svg
                                    width="11"
                                    height="11"
                                    viewBox="0 0 24 24"
                                    fill="none"
                                    stroke="currentColor"
                                    stroke-width="2.5"
                                    stroke-linecap="round"
                                    stroke-linejoin="round"
                                    :class="{ 'rotate-180': isWorkerThoughtExpanded }"
                                >
                                    <polyline points="6 9 12 15 18 9" />
                                </svg>
                            </button>
                        </div>
                        <Transition name="thought-accordion">
                            <div v-if="isWorkerThoughtExpanded" class="ambient-live-thought-expanded">
                                <div class="thought-expanded-header">
                                    <span class="thought-expanded-title">实时思考推演</span>
                                    <span class="thought-char-count">{{ workerThoughtText.length }} 字</span>
                                </div>
                                <div class="thought-expanded-body soft-scrollbar">
                                    {{ workerThoughtText || '正在后台深入推导与研判相关代码与风险…' }}
                                </div>
                            </div>
                        </Transition>
                    </div>

                    <!-- 📊 ReviewScorecard 审查评分卡 (正确性、破坏性风险、代码规范、测试覆盖) -->
                    <div v-if="selectedWorker.scorecard" class="worker-scorecard-deck">
                        <div class="scorecard-header">
                            <span class="scorecard-icon">📊</span>
                            <strong class="scorecard-title">审查评分卡 (ReviewScorecard)</strong>
                            <span class="scorecard-badge">
                                均分: {{ computeScorecardAverage(selectedWorker.scorecard) }} 分 · 
                                仅供参考，不决定放行
                            </span>
                        </div>
                        <div class="scorecard-grid">
                            <div class="scorecard-item">
                                <div class="item-label-row">
                                    <span class="dim-name">正确性 (Correctness)</span>
                                    <span class="dim-val">{{ selectedWorker.scorecard.correctness }}/100</span>
                                </div>
                                <div class="progress-track">
                                    <div class="progress-bar correctness" :style="{ width: `${selectedWorker.scorecard.correctness}%` }"></div>
                                </div>
                            </div>
                            <div class="scorecard-item">
                                <div class="item-label-row">
                                    <span class="dim-name">破坏性风险/安全 (Risk)</span>
                                    <span class="dim-val">{{ selectedWorker.scorecard.risk }}/100</span>
                                </div>
                                <div class="progress-track">
                                    <div class="progress-bar risk" :style="{ width: `${selectedWorker.scorecard.risk}%` }"></div>
                                </div>
                            </div>
                            <div class="scorecard-item">
                                <div class="item-label-row">
                                    <span class="dim-name">代码规范 (Style)</span>
                                    <span class="dim-val">{{ selectedWorker.scorecard.style }}/100</span>
                                </div>
                                <div class="progress-track">
                                    <div class="progress-bar style" :style="{ width: `${selectedWorker.scorecard.style}%` }"></div>
                                </div>
                            </div>
                            <div class="scorecard-item">
                                <div class="item-label-row">
                                    <span class="dim-name">测试覆盖 (Test Coverage)</span>
                                    <span class="dim-val">{{ selectedWorker.scorecard.test_coverage != null ? `${selectedWorker.scorecard.test_coverage}/100` : 'N/A（不适用）' }}</span>
                                </div>
                                <div class="progress-track">
                                    <div class="progress-bar test" :style="{ width: `${selectedWorker.scorecard.test_coverage ?? 100}%` }"></div>
                                </div>
                            </div>
                        </div>
                    </div>

                    <!-- 🎯 核心结论与发现 (当子任务产出 findings 时结构化高亮展示) -->
                    <div v-if="selectedWorker.findings && selectedWorker.findings.length > 0" class="worker-findings-deck">
                        <div class="findings-header">
                            <span class="findings-icon">🎯</span>
                            <strong class="findings-title">任务产出与结论总结</strong>
                            <span class="findings-count">{{ selectedWorker.findings.length }} 条发现</span>
                            <span v-if="selectedWorker.confidence" class="confidence-tag" :class="`conf-${selectedWorker.confidence}`">
                                {{ selectedWorker.confidence === 'confirmed' ? '高信度 (confirmed)' : (selectedWorker.confidence === 'supported' ? '中信度 (supported)' : '待核验') }}
                            </span>
                        </div>
                        <ul class="findings-list">
                            <li
                                v-for="(finding, fIdx) in selectedWorker.findings"
                                :key="fIdx"
                                class="finding-item"
                                :class="{
                                    'is-blocking': isBlockingFinding(finding),
                                    'is-warning': isWarningFinding(finding),
                                }"
                            >
                                <span class="finding-num">{{ fIdx + 1 }}</span>
                                <span class="finding-text">{{ formatFinding(finding) }}</span>
                            </li>
                        </ul>
                    </div>

                    <!-- 🏷️ 角色特化交付契约扩展卡 (Custom Schema 扩展字段) -->
                    <ExtraContractDeck
                        v-if="selectedWorker.extra || selectedWorker.customSchema"
                        :extra="selectedWorker.extra"
                        :schema="selectedWorker.customSchema"
                    />

                    <!-- 子 Agent 执行轨迹条 (支持多步收纳与一键展开/折叠) -->
                    <div class="steps-deck-bar">
                        <div class="steps-deck-title">
                            <span class="steps-deck-icon">⚡</span>
                            <span>执行轨迹 (共 {{ activeWorkerSteps.length }} 步)</span>
                        </div>
                        <button
                            v-if="activeWorkerSteps.length > 8"
                            type="button"
                            class="btn-toggle-steps-fold"
                            @click="expandAllSteps = !expandAllSteps"
                        >
                            {{ expandAllSteps ? '折叠常规步骤' : `展开全部 (${activeWorkerSteps.length})` }}
                        </button>
                    </div>

                    <p v-if="!activeWorkerSteps.length" class="worker-steps-empty">暂无已记录的执行步骤</p>
                    <div v-else class="worker-steps-stream-deck">
                        <div
                            v-for="st in displayedSteps"
                            :key="st.stepNumber"
                            class="worker-step-item-wrap"
                        >
                            <AgentStepRow
                                :step="st"
                                @open-diff="$emit('open-diff', $event)"
                                @select="toggleStepExpand(st.stepNumber)"
                            />

                            <!-- 就地展开步骤调用详情与证据 (绝不跳出子 Agent 视图) -->
                            <Transition name="step-expand-slide">
                                <div v-if="expandedStepNumbers.has(st.stepNumber)" class="worker-step-inline-detail">
                                    <div class="inline-detail-header">
                                        <div class="inline-title-meta">
                                            <span class="inline-step-num">步骤 #{{ st.stepNumber }}</span>
                                            <strong class="inline-step-title">{{ st.title }}</strong>
                                            <span v-if="st.durationMs" class="inline-step-dur">⏱ {{ (st.durationMs / 1000).toFixed(2) }}s</span>
                                        </div>
                                        <button
                                            v-if="st.diffPayload || (st.additions && st.additions > 0)"
                                            type="button"
                                            class="btn-inline-open-diff"
                                            @click.stop="$emit('open-diff', st.diffPayload || { path: st.fileName, rawDiff: '' })"
                                        >
                                            查看代码 Diff 变动 ↗
                                        </button>
                                    </div>

                                    <!-- 💭 步骤内部思考与推理 -->
                                    <div v-if="st.thought" class="inline-detail-section">
                                        <div class="detail-sec-head">
                                            <span class="sec-icon">💭</span>
                                            <span class="sec-title">思考与推导过程</span>
                                        </div>
                                        <p class="sec-text thought-text">{{ st.thought }}</p>
                                    </div>

                                    <!-- ⚙️ 调用入参 (params) -->
                                    <div v-if="hasPayload(st.params)" class="inline-detail-section">
                                        <div class="detail-sec-head">
                                            <span class="sec-icon">⚙️</span>
                                            <span class="sec-title">调用入参 (Parameters)</span>
                                            <button type="button" class="btn-mini-copy" @click.stop="copyTextContent(formatPayload(st.params), `p-${st.stepNumber}`)">
                                                {{ copiedKey === `p-${st.stepNumber}` ? '已复制 ✓' : '复制' }}
                                            </button>
                                        </div>
                                        <pre class="sec-code">{{ formatPayload(st.params) }}</pre>
                                    </div>

                                    <!-- 📋 执行输出 / 结果证据 (output / result) -->
                                    <div v-if="hasPayload(st.output) || hasPayload(st.result)" class="inline-detail-section">
                                        <div class="detail-sec-head">
                                            <span class="sec-icon">📋</span>
                                            <span class="sec-title">执行输出 / 结果证据 (Output)</span>
                                            <button type="button" class="btn-mini-copy" @click.stop="copyTextContent(formatPayload(st.output || st.result), `o-${st.stepNumber}`)">
                                                {{ copiedKey === `o-${st.stepNumber}` ? '已复制 ✓' : '复制' }}
                                            </button>
                                        </div>
                                        <pre class="sec-code">{{ formatPayload(st.output || st.result) }}</pre>
                                    </div>

                                    <!-- 🎯 意图说明 / 阶段总结 -->
                                    <div v-if="st.description || st.summary" class="inline-detail-section">
                                        <div class="detail-sec-head">
                                            <span class="sec-icon">🎯</span>
                                            <span class="sec-title">意图说明</span>
                                        </div>
                                        <p class="sec-text">{{ st.description || st.summary }}</p>
                                    </div>
                                </div>
                            </Transition>
                        </div>
                    </div>

                    <!-- 子 Agent 实时执行输出预览 (如果有输出内容) -->
                    <div v-if="selectedWorker.output" class="worker-output-terminal">
                        <div class="terminal-bar">
                            <span>执行输出</span>
                            <button type="button" class="btn-copy-out" @click="copyOutput(selectedWorker.output)">复制</button>
                        </div>
                        <pre class="terminal-body">{{ selectedWorker.output }}</pre>
                    </div>
                </div>

            </div>
        </section>
    </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import type { AgentTeamState, AgentWorkerItem, AgentToolStatus, AgentStepItem, AgentWorkerStep, ReviewScorecard } from "./types";
import { extractSubagentsFromStep, formatFinding } from "./workerUtils";
import { isWorkerStep } from "./workerAssociation";
import ExtraContractDeck from "./ExtraContractDeck.vue";
import AgentStepRow from "./AgentStepRow.vue";
import AgentTeamTopology from "./AgentTeamTopology.vue";

function computeScorecardAverage(sc?: ReviewScorecard): number {
    if (!sc) return 0;
    let sum = sc.correctness + sc.risk + sc.style;
    let count = 3;
    if (sc.test_coverage != null) {
        sum += sc.test_coverage;
        count += 1;
    }
    return Math.round(sum / count);
}

function isBlockingFinding(finding: any): boolean {
    const text = formatFinding(finding).toLowerCase();
    return text.includes("阻断") || text.includes("blocking");
}

function isWarningFinding(finding: any): boolean {
    const text = formatFinding(finding).toLowerCase();
    return text.includes("警告") || text.includes("warning");
}

const props = withDefaults(
    defineProps<{
        teamState?: AgentTeamState;
        steps?: AgentStepItem[];
        currentGoal?: string;
        selectedStep?: AgentStepItem | null;
        activeWorkerId?: string;
    }>(),
    {
        teamState: () => ({
            masterStatus: "pending" as AgentToolStatus,
            activeWorkerCount: 0,
            completedWorkerCount: 0,
            workers: [] as AgentWorkerItem[],
        }),
        steps: () => [],
        currentGoal: "",
        selectedStep: null,
        activeWorkerId: "",
    }
);

const emit = defineEmits<{
    "clear-selected-step": [];
    "open-diff": [diffPayload: any];
    "select-worker": [workerId: string];
}>();

// ── 🌟 换行感知与动力学平滑尾随滚动指令 (v-scroll-tail) ──
import { vScrollTail } from "./scrollTailDirective";
import { extractActiveThoughtLineInfo } from "./diffUtils";

// ── 💭 子 Agent 实时流式思考与推演 ──
const isWorkerThoughtExpanded = ref(false);

const workerThoughtInfo = computed(() => {
    return extractActiveThoughtLineInfo(selectedWorker.value?.thought || "");
});

const workerThoughtText = computed(() => {
    return selectedWorker.value?.thought?.trim() || "";
});

const isWorkerThinking = computed(() => {
    if (!selectedWorker.value) return false;
    return selectedWorker.value.status === "running" || !!workerThoughtText.value;
});

// ── 👣 子 Agent 步骤就地展开与详情对账 ──
const expandedStepNumbers = ref<Set<number>>(new Set());

function toggleStepExpand(stepNum: number) {
    if (expandedStepNumbers.value.has(stepNum)) {
        expandedStepNumbers.value.delete(stepNum);
    } else {
        expandedStepNumbers.value.add(stepNum);
    }
}

const copiedKey = ref("");
async function copyTextContent(text: string, key: string) {
    if (!text) return;
    try {
        await navigator.clipboard.writeText(text);
        copiedKey.value = key;
        setTimeout(() => {
            if (copiedKey.value === key) copiedKey.value = "";
        }, 1500);
    } catch {}
}

function hasPayload(val: unknown): boolean {
    if (val === undefined || val === null) return false;
    if (typeof val === "string") return val.trim().length > 0;
    if (typeof val === "object") return Object.keys(val).length > 0;
    return true;
}

function formatPayload(payload: unknown): string {
    if (typeof payload === "string") return payload;
    try {
        return JSON.stringify(payload, null, 2);
    } catch {
        return String(payload);
    }
}

function handleWorkerClick(workerId: string) {
    emit("select-worker", workerId);
    activeTab.value = workerId;
}

// 从当前选中的历史步骤中解析出绑定的子 Agent (用于历史回看，支持 wait_jobs 溯源关联)
const stepWorkers = computed(() => {
    return extractSubagentsFromStep(props.selectedStep, props.steps, props.teamState?.workers || []);
});

const isViewingHistoryStep = computed(() => {
    return !!props.selectedStep && isWorkerStep(props.selectedStep);
});

const displayedWorkers = computed(() => isViewingHistoryStep.value ? stepWorkers.value : props.teamState.workers);

const activeTab = ref("master");

// 监听外部指定的 activeWorkerId 同步激活 Tab
watch(
    () => props.activeWorkerId,
    (val) => {
        activeTab.value = val || "master";
    },
    { immediate: true }
);

const selectedWorker = computed(() => displayedWorkers.value.find(worker => worker.id === activeTab.value) ?? null);
const activeWorkerSteps = computed<AgentWorkerStep[]>(() => selectedWorker.value?.steps ?? []);

const expandAllSteps = ref(false);

const displayedSteps = computed(() => {
    if (expandAllSteps.value || activeWorkerSteps.value.length <= 8) {
        return activeWorkerSteps.value;
    }
    return activeWorkerSteps.value.slice(0, 8);
});

function formatWorkerName(name: string): string {
    if (!name) return "子 Agent";
    // 优先识别专精角色卡（带有 Emoji 或明确职称如审查官/架构师/专家等）
    if (/[🎨🛡️🔧⚡🔍🧠🎭🤖💻]/.test(name) || /审查官|审查员|专家|架构师|写入员|工作者|设计官/.test(name)) {
        if (name.includes("：") || name.includes(":")) {
            const parts = name.split(/[:：]/);
            if (/角色|专家|挂载/.test(parts[0]) && parts[1]?.trim()) {
                return parts[1].trim().slice(0, 14);
            }
        }
        return name.length > 14 ? name.slice(0, 13) + "…" : name;
    }
    // 提取精简短标题 (例如 "Agent A：计算1+1" -> "Agent A", "子Agent-B 计算1+1" -> "Agent B")
    if (name.includes("：") || name.includes(":")) {
        const prefix = name.split(/[:：]/)[0].trim();
        if (prefix.length <= 10) return prefix;
    }
    // 匹配 "Agent A" / "子Agent A"
    const match = name.match(/(Agent\s*[-_]?[A-Za-z0-9]+)/i);
    if (match && match[1]) {
        return match[1].replace(/[-_]/g, " ");
    }
    if (name.length > 12) {
        return name.slice(0, 11) + "…";
    }
    return name;
}

function getShortToolName(tool?: string): string {
    if (!tool) return "shell_exec";
    if (tool.includes("/")) {
        return tool.split("/")[1];
    }
    return tool;
}

function getRoleIcon(role: string): string {
    switch (role) {
        case "reviewer":
            return "🛡️";
        case "coder":
            return "⚡";
        case "researcher":
            return "🔍";
        case "master":
            return "👑";
        default:
            return "⚡";
    }
}

function statusText(status: AgentToolStatus): string {
    const map: Record<AgentToolStatus, string> = {
        running: "执行中",
        succeeded: "已完成",
        failed: "失败",
        pending: "待命中",
        challenged: "验证未闭合",
    };
    return map[status] || "待命";
}

async function copyOutput(text?: string) {
    if (!text) return;
    try {
        await navigator.clipboard.writeText(text);
    } catch {}
}
</script>

<style scoped>
.worker-steps-empty { margin: 6px 0; color: var(--theme-text-secondary, #a4a9b5); font-size: 12px; }
.agent-workspace-deck {
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: 100%;
    background: transparent;
    border: none;
    border-radius: 0;
    padding: 0;
    user-select: none;
}

/* ================= 1. 上半部分：竖向拓扑树 ================= */
.topology-overview-section {
    display: flex;
    flex-direction: column;
    gap: 8px;
}

/* 📍 历史步骤回看通知条 */
.history-step-notice-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 6px 10px;
    margin-bottom: 8px;
    background: linear-gradient(90deg, rgba(234, 179, 8, 0.15), rgba(234, 179, 8, 0.05));
    border: 1px solid rgba(234, 179, 8, 0.35);
    border-radius: 6px;
    font-size: 10px;
    color: var(--theme-status-warning, #fef08a);
}

.notice-info {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
}

.notice-icon {
    font-size: 12px;
}

.notice-text {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}

.notice-text strong {
    color: var(--theme-text-primary, #ffffff);
}

.btn-clear-history-step {
    padding: 2px 7px;
    font-size: 9px;
    background: rgba(var(--theme-foreground-rgb), 0.1);
    border: 1px solid rgba(var(--theme-foreground-rgb), 0.2);
    color: var(--theme-text-primary, #f4f4f5);
    border-radius: 4px;
    cursor: pointer;
    white-space: nowrap;
    transition: all 0.2s;
}

.btn-clear-history-step:hover {
    background: rgba(234, 179, 8, 0.25);
    border-color: #fef08a;
    color: var(--theme-status-warning, #fef08a);
}

.worker-card-model {
    font-size: 8.5px;
    color: var(--theme-status-accent, #c4b5fd);
    background: rgba(167, 139, 250, 0.1);
    border: 1px solid rgba(167, 139, 250, 0.22);
    padding: 0 4px;
    border-radius: 3px;
    font-family: ui-monospace, monospace;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 120px;
}

.agent-chains-tabs-section {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding-top: 6px;
    border-top: 1px solid rgba(var(--theme-foreground-rgb), 0.06);
}

/* 横向排列短标题 Tab 栏 (支持自动流式换行) */
.chains-tab-nav {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px;
    padding-bottom: 2px;
}

.chain-tab-btn {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 4px 7px;
    background: var(--theme-bg-panel, #181820);
    border: 1px solid rgba(var(--theme-foreground-rgb), 0.08);
    border-radius: 6px;
    color: var(--theme-text-secondary, #a1a1aa);
    font-size: 9.5px;
    font-weight: 600;
    cursor: pointer;
    transition: all 0.15s ease;
    max-width: 130px;
}

.chain-tab-btn:hover {
    color: var(--theme-text-primary, #f4f4f5);
    background: var(--theme-bg-raised, #21212b);
    border-color: rgba(var(--theme-foreground-rgb), 0.15);
}

.chain-tab-btn.active {
    background: var(--theme-bg-raised, #252533);
    border-color: #38bdf8;
    color: var(--theme-status-info, #38bdf8);
    box-shadow: 0 0 8px rgba(56, 189, 248, 0.15);
}

.tab-title {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}

.tab-progress-tag {
    font-size: 8.5px;
    padding: 0 3px;
    border-radius: 3px;
}

.tab-progress-tag.status-running {
    background: rgba(56, 189, 248, 0.2);
    color: var(--theme-status-info, #38bdf8);
}

.tab-progress-tag.status-succeeded {
    background: rgba(52, 211, 153, 0.2);
    color: var(--theme-status-success, #34d399);
}

/* Tab 面板主体 */
.chain-tab-content-panel {
    background: transparent;
    border: none;
    border-radius: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
}

.agent-detail-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 6px;
}

.detail-header-left {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
}

.agent-big-icon {
    font-size: 16px;
    flex-shrink: 0;
}

.detail-name-meta {
    display: flex;
    flex-direction: column;
    min-width: 0;
}

.detail-agent-name {
    font-size: 11px;
    color: var(--theme-text-primary, #f4f4f5);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}

.detail-agent-tool {
    font-size: 8.5px;
    color: var(--theme-text-muted, #71717a);
    font-family: ui-monospace, monospace;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}

.stage-metric-pill {
    font-size: 8.5px;
    font-weight: 600;
    padding: 1px 5px;
    border-radius: 4px;
    background: rgba(var(--theme-foreground-rgb), 0.06);
    color: var(--theme-text-secondary, #a1a1aa);
    white-space: nowrap;
}

.stage-metric-pill.status-running {
    background: rgba(56, 189, 248, 0.2);
    color: var(--theme-status-info, #38bdf8);
}

.stage-metric-pill.status-succeeded {
    background: rgba(52, 211, 153, 0.2);
    color: var(--theme-status-success, #34d399);
}

.worker-dur-pill {
    font-size: 8.5px;
    color: var(--theme-text-muted, #71717a);
    margin-right: 4px;
    white-space: nowrap;
}

/* 进度条 */
.chain-progress-bar-wrap {
    width: 100%;
}

.chain-progress-track {
    width: 100%;
    height: 4px;
    background: rgba(var(--theme-foreground-rgb), 0.07);
    border-radius: 2px;
    overflow: hidden;
}

.chain-progress-fill {
    height: 100%;
    border-radius: 2px;
    transition: width 0.3s ease;
}

.worker-fill {
    background: #38bdf8;
}

.worker-fill.is-animated {
    animation: shimmer-progress 1.5s infinite linear;
}

@keyframes shimmer-progress {
    0% { opacity: 0.8; }
    50% { opacity: 1; filter: brightness(1.2); }
    100% { opacity: 0.8; }
}

/* 🎯 核心结论与发现卡片 */
.worker-scorecard-deck {
    margin-top: 6px;
    padding: 6px 10px;
    background: rgba(var(--theme-rgb-panel, 18, 22, 34), 0.6);
    border: 1px solid rgba(80, 100, 160, 0.2);
    border-radius: 6px;
}
.scorecard-header {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 6px;
}
.scorecard-icon { font-size: 13px; }
.scorecard-title { font-size: 11px; font-weight: 600; color: var(--theme-text-primary, #e2e8f0); }
.scorecard-badge {
    margin-left: auto;
    font-size: 9.5px;
    padding: 1px 6px;
    border-radius: 4px;
    font-weight: 500;
}
.scorecard-grid {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: 6px 12px;
}
.scorecard-item {
    display: flex;
    flex-direction: column;
    gap: 3px;
}
.item-label-row {
    display: flex;
    justify-content: space-between;
    font-size: 10px;
}
.dim-name { color: var(--theme-status-info, #94a3b8); }
.dim-val { color: var(--theme-text-primary, #f1f5f9); font-weight: 600; font-family: monospace; }
.dim-val.text-danger { color: var(--theme-status-danger, #f87171); }
.progress-track {
    width: 100%;
    height: 4px;
    background: rgba(var(--theme-foreground-rgb), 0.08);
    border-radius: 999px;
    overflow: hidden;
}
.progress-bar {
    height: 100%;
    border-radius: 999px;
    transition: width 0.3s ease;
}
.progress-bar.correctness { background: linear-gradient(90deg, #38bdf8, #0ea5e9); }
.progress-bar.risk { background: linear-gradient(90deg, #10b981, #059669); }
.progress-bar.risk.bar-danger { background: linear-gradient(90deg, #f87171, #dc2626); }
.progress-bar.style { background: linear-gradient(90deg, #a855f7, #9333ea); }
.progress-bar.test { background: linear-gradient(90deg, #f59e0b, #d97706); }

.worker-findings-deck {
    background: linear-gradient(135deg, rgba(56, 189, 248, 0.08), rgba(var(--theme-rgb-panel, 24, 24, 32), 0.95));
    border: 1px solid rgba(56, 189, 248, 0.25);
    border-radius: 8px;
    padding: 8px 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.25);
}

.findings-header {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 10px;
}

.findings-icon {
    font-size: 13px;
}

.findings-title {
    color: var(--theme-status-info, #38bdf8);
    font-weight: 700;
}

.findings-count {
    font-size: 8.5px;
    color: var(--theme-text-secondary, #a1a1aa);
    background: rgba(var(--theme-foreground-rgb), 0.08);
    padding: 1px 5px;
    border-radius: 4px;
}

.confidence-tag {
    margin-left: auto;
    font-size: 8px;
    font-weight: 600;
    padding: 1px 6px;
    border-radius: 3px;
    background: rgba(52, 211, 153, 0.15);
    color: var(--theme-status-success, #34d399);
}

.findings-list {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 4px;
}

.finding-item {
    display: flex;
    align-items: flex-start;
    gap: 6px;
    font-size: 10px;
    line-height: 1.45;
    color: var(--theme-text-primary, #e4e4e7);
    background: var(--theme-inset-bg, rgba(0, 0, 0, 0.2));
    padding: 4px 6px;
    border-radius: 4px;
    border-left: 2px solid #38bdf8;
    transition: all 0.15s ease;
}

.finding-item.is-blocking {
    border-left-color: #ef4444;
    background: rgba(239, 68, 68, 0.08);
}

.finding-item.is-blocking .finding-num {
    color: var(--theme-status-danger, #ef4444);
    background: rgba(239, 68, 68, 0.18);
}

.finding-item.is-warning {
    border-left-color: #f59e0b;
    background: rgba(245, 158, 11, 0.08);
}

.finding-item.is-warning .finding-num {
    color: var(--theme-status-warning, #f59e0b);
    background: rgba(245, 158, 11, 0.18);
}

.finding-num {
    font-size: 8.5px;
    font-weight: 700;
    color: var(--theme-status-info, #38bdf8);
    background: rgba(56, 189, 248, 0.15);
    border-radius: 50%;
    width: 14px;
    height: 14px;
    display: grid;
    place-items: center;
    flex-shrink: 0;
    margin-top: 1px;
}

.finding-text {
    flex: 1;
    word-break: break-word;
}

/* 顶部操作条：显示步数与折叠切换 */
.steps-deck-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 3px 2px 5px;
    border-bottom: 1px solid rgba(var(--theme-foreground-rgb), 0.05);
    margin-bottom: 4px;
}

.steps-deck-title {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: 10.5px;
    font-weight: 650;
    color: var(--theme-text-primary, #e4e4e7);
}

.steps-deck-icon {
    font-size: 12px;
    color: var(--theme-status-info, #38bdf8);
}

.btn-toggle-steps-fold {
    background: rgba(var(--theme-foreground-rgb), 0.06);
    border: 1px solid rgba(var(--theme-foreground-rgb), 0.12);
    color: var(--theme-text-secondary, #a1a1aa);
    font-size: 8.5px;
    padding: 2px 6px;
    border-radius: 4px;
    cursor: pointer;
    transition: all 0.15s ease;
}

.btn-toggle-steps-fold:hover {
    color: var(--theme-text-primary, #ffffff);
    background: rgba(56, 189, 248, 0.15);
    border-color: rgba(56, 189, 248, 0.3);
}

/* 紧凑版时间轴步骤链 (压缩行距，单行主旨，31步也不臃肿) */
.worker-steps-chain.compact {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding-left: 0;
}

.compact-node {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 3.5px 7px;
    background: var(--theme-bg-panel, #171720);
    border: 1px solid rgba(var(--theme-foreground-rgb), 0.05);
    border-radius: 5px;
    transition: all 0.15s ease;
}

.compact-node:hover {
    background: var(--theme-bg-panel, #1b1b26);
    border-color: rgba(var(--theme-foreground-rgb), 0.1);
}

.compact-node.is-active {
    border-color: rgba(56, 189, 248, 0.45);
    background: linear-gradient(90deg, rgba(56, 189, 248, 0.08), var(--theme-bg-panel, #171720));
}

.step-compact-line {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    line-height: 1.3;
}

.step-seq-tag {
    font-size: 8px;
    font-family: ui-monospace, SFMono-Regular, monospace;
    font-weight: 700;
    color: var(--theme-text-muted, #71717a);
    background: rgba(var(--theme-foreground-rgb), 0.06);
    padding: 1px 4px;
    border-radius: 3px;
    flex-shrink: 0;
}

.compact-node.status-succeeded .step-seq-tag {
    color: var(--theme-status-success, #34d399);
    background: rgba(52, 211, 153, 0.12);
}

.compact-node.is-active .step-seq-tag {
    color: var(--theme-status-info, #38bdf8);
    background: rgba(56, 189, 248, 0.18);
}

.step-tool-badge {
    font-size: 8px;
    font-family: ui-monospace, SFMono-Regular, monospace;
    padding: 1px 5px;
    border-radius: 3px;
    background: rgba(56, 189, 248, 0.12);
    color: var(--theme-status-info, #38bdf8);
    border: 1px solid rgba(56, 189, 248, 0.2);
    flex-shrink: 0;
}

.step-intent-title {
    font-size: 10px;
    font-weight: 650;
    color: var(--theme-text-primary, #f4f4f5);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    flex: 1;
}

.step-detail-cmd {
    display: flex;
    align-items: center;
    gap: 4px;
    padding-left: 18px;
    font-size: 8.5px;
    font-family: ui-monospace, SFMono-Regular, monospace;
    color: var(--theme-text-muted, #71717a);
    line-height: 1.25;
}

.cmd-prompt-icon {
    color: var(--theme-text-dim, #52525b);
    font-weight: 700;
}

.cmd-text {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--theme-text-secondary, #a1a1aa);
}

.compact-node.status-succeeded .step-status-tag {
    color: var(--theme-status-success, #34d399);
    font-size: 9px;
    font-weight: 700;
    flex-shrink: 0;
}

.compact-node.is-active .step-status-tag {
    color: var(--theme-status-info, #38bdf8);
    font-weight: 700;
    flex-shrink: 0;
}

.step-summary-box-compact {
    display: flex;
    align-items: flex-start;
    gap: 4px;
    font-size: 9px;
    color: var(--theme-status-warning, #fef08a);
    background: rgba(234, 179, 8, 0.08);
    border: 1px solid rgba(234, 179, 8, 0.2);
    padding: 3px 6px;
    border-radius: 4px;
    line-height: 1.35;
    margin-top: 1px;
}

/* 折叠提示条 */
.folded-steps-indicator-bar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 6px;
    margin: 2px 0;
    background: transparent;
    border: 0;
    cursor: pointer;
    width: 100%;
    color: var(--theme-text-muted, #71717a);
    transition: all 0.15s ease;
}

.folded-steps-indicator-bar:hover .fold-badge {
    color: var(--theme-status-info, #38bdf8);
    border-color: rgba(56, 189, 248, 0.3);
    background: rgba(56, 189, 248, 0.1);
}

.fold-line {
    flex: 1;
    height: 1px;
    background: rgba(var(--theme-foreground-rgb), 0.08);
}

.fold-badge {
    font-size: 8.5px;
    padding: 2px 8px;
    border-radius: 10px;
    background: var(--theme-bg-panel, #181822);
    border: 1px dashed rgba(var(--theme-foreground-rgb), 0.15);
    white-space: nowrap;
}


/* 描述层级：具体执行内容 */
.step-desc-box {
    font-size: 9.5px;
    color: var(--theme-text-secondary, #a1a1aa);
    line-height: 1.45;
    background: rgba(var(--theme-foreground-rgb), 0.03);
    padding: 4px 6px;
    border-radius: 4px;
    border-left: 2px solid rgba(var(--theme-foreground-rgb), 0.12);
    word-break: break-word;
}

/* 总结层级：执行发现/产出 */
.step-summary-box {
    display: flex;
    align-items: flex-start;
    gap: 5px;
    font-size: 9.5px;
    color: var(--theme-status-warning, #fef08a);
    background: rgba(234, 179, 8, 0.08);
    border: 1px solid rgba(234, 179, 8, 0.2);
    padding: 4px 6px;
    border-radius: 4px;
    line-height: 1.4;
    word-break: break-word;
}

.summary-icon {
    font-size: 11px;
    flex-shrink: 0;
    margin-top: 1px;
}

.summary-text {
    flex: 1;
}

/* 终端输出预览 */
.worker-output-terminal {
    background: var(--theme-bg-base, #111116);
    border: 1px solid rgba(var(--theme-foreground-rgb), 0.08);
    border-radius: 6px;
    overflow: hidden;
    margin-top: 2px;
}

.terminal-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 3px 6px;
    background: rgba(var(--theme-foreground-rgb), 0.04);
    border-bottom: 1px solid rgba(var(--theme-foreground-rgb), 0.06);
    font-size: 8.5px;
    color: var(--theme-text-muted, #71717a);
}

.btn-copy-out {
    background: transparent;
    border: none;
    font-size: 8.5px;
    color: var(--theme-status-info, #38bdf8);
    cursor: pointer;
    padding: 0 2px;
}

.terminal-body {
    margin: 0;
    padding: 6px;
    font-size: 8.5px;
    font-family: ui-monospace, SFMono-Regular, monospace;
    color: var(--theme-text-primary, #e4e4e7);
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 100px;
    overflow-y: auto;
}

/* 无子 Agent 时的极简占位 */
.no-workers-standby-box {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    padding: 16px 8px;
    gap: 4px;
}

.standby-icon {
    font-size: 20px;
    opacity: 0.6;
}

.standby-title {
    font-size: 11px;
    color: var(--theme-text-primary, #f4f4f5);
}

.standby-desc {
    margin: 0;
    font-size: 9px;
    color: var(--theme-text-muted, #71717a);
    line-height: 1.4;
    max-width: 220px;
}

.spinner-dot {
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: currentColor;
    animation: spin-pulse 1s infinite ease-in-out;
}

@keyframes spin-pulse {
    0%, 100% { opacity: 0.4; transform: scale(0.8); }
    50% { opacity: 1; transform: scale(1.2); }
}

/* 🌟 1. 顶部全局 Agent 切换 Tab 栏 */
/* ── 💭 子 Agent 流式思考滚动条 (复刻主 Agent ambient-live-text-bar 动效与滚动机制) ── */
.worker-live-thought-row {
    margin: 8px 0 10px;
    width: 100%;
}

.worker-live-thought-row .ambient-live-text-bar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    border-radius: 6px;
    background: rgba(var(--theme-rgb-panel, 15, 23, 42), 0.45);
    border: 1px solid rgba(56, 189, 248, 0.22);
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.2);
    user-select: none;
    cursor: pointer;
    transition: border-color 0.15s ease, background 0.15s ease;
    width: 100%;
    box-sizing: border-box;
}

.worker-live-thought-row .ambient-live-text-bar:hover {
    background: rgba(var(--theme-rgb-panel, 15, 23, 42), 0.65);
    border-color: rgba(56, 189, 248, 0.4);
}

.live-think-label {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
}

.live-think-square-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    color: var(--theme-status-info, #38bdf8);
}

.thought-square-svg {
    width: 12px;
    height: 12px;
    stroke: currentColor;
    flex-shrink: 0;
    animation: thought-pulse 2s ease-in-out infinite;
}

@keyframes thought-pulse {
    0%, 100% { opacity: 0.8; transform: scale(1); }
    50% { opacity: 1; transform: scale(1.08); filter: drop-shadow(0 0 4px rgba(56, 189, 248, 0.6)); }
}

.live-think-title {
    font-size: 11.5px;
    font-weight: 600;
    color: var(--theme-status-info, #38bdf8);
    letter-spacing: 0.1px;
}

.ambient-live-viewport {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    position: relative;
    height: 20px;
    display: flex;
    align-items: center;
    mask-image: linear-gradient(to right, #000 0, #000 calc(100% - 14px), transparent 100%);
    -webkit-mask-image: linear-gradient(to right, #000 0, #000 calc(100% - 14px), transparent 100%);
}

.ambient-live-viewport.has-left-overflow {
    mask-image: linear-gradient(to right, transparent 0, #000 12px, #000 calc(100% - 14px), transparent 100%);
    -webkit-mask-image: linear-gradient(to right, transparent 0, #000 12px, #000 calc(100% - 14px), transparent 100%);
}

.ambient-live-scroll-text {
    font-size: 11.5px;
    font-weight: 500;
    line-height: 20px;
    height: 20px;
    color: var(--theme-text-primary, #f1f5f9);
    display: block;
    width: 100%;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: clip;
    flex: 1 1 auto;
    min-width: 0;
    letter-spacing: -0.1px;
}

/* 🌟 垂直翻牌 / 上下翻转 Ticker 动效 (Flip-up Ticker Transition) */
.ticker-flip-enter-active,
.ticker-flip-leave-active {
    transition: transform 0.32s cubic-bezier(0.16, 1, 0.3, 1), opacity 0.32s cubic-bezier(0.16, 1, 0.3, 1);
    will-change: transform, opacity;
}

.ticker-flip-leave-active {
    position: absolute !important;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    pointer-events: none;
    width: 100%;
}

.ticker-flip-enter-from {
    opacity: 0;
    transform: translateY(100%);
}

.ticker-flip-enter-to {
    opacity: 1;
    transform: translateY(0);
}

.ticker-flip-leave-from {
    opacity: 1;
    transform: translateY(0);
}

.ticker-flip-leave-to {
    opacity: 0;
    transform: translateY(-100%);
}

.thought-expand-btn {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding: 2px 7px;
    border-radius: 4px;
    background: rgba(var(--theme-foreground-rgb), 0.05);
    border: 1px solid rgba(var(--theme-foreground-rgb), 0.1);
    color: var(--theme-status-info, #94a3b8);
    font-size: 10.5px;
    cursor: pointer;
    flex-shrink: 0;
    transition: all 0.15s ease;
}

.thought-expand-btn:hover {
    color: var(--theme-text-primary, #e2e8f0);
    background: rgba(var(--theme-foreground-rgb), 0.1);
    border-color: rgba(var(--theme-foreground-rgb), 0.2);
}

.thought-expand-btn svg {
    transition: transform 0.2s cubic-bezier(0.16, 1, 0.3, 1);
}

.thought-expand-btn svg.rotate-180 {
    transform: rotate(180deg);
}

.ambient-live-thought-expanded {
    width: 100%;
    margin-top: 6px;
    padding: 10px 12px;
    border-radius: 6px;
    background: rgba(var(--theme-rgb-panel, 15, 23, 42), 0.7);
    backdrop-filter: blur(10px);
    border: 1px solid rgba(56, 189, 248, 0.2);
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.25);
    box-sizing: border-box;
}

.thought-expanded-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 6px;
    padding-bottom: 4px;
    border-bottom: 1px solid rgba(var(--theme-foreground-rgb), 0.06);
}

.thought-expanded-title {
    font-size: 11px;
    font-weight: 500;
    color: var(--theme-status-info, #94a3b8);
}

.thought-char-count {
    font-size: 10px;
    color: var(--theme-status-info, #64748b);
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}

.thought-expanded-body {
    font-size: 11.5px;
    line-height: 1.6;
    color: var(--theme-text-primary, #e2e8f0);
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 220px;
    overflow-y: auto;
    user-select: text;
    padding-right: 6px;
}

.thought-accordion-enter-active,
.thought-accordion-leave-active {
    transition: all 0.25s cubic-bezier(0.16, 1, 0.3, 1);
    overflow: hidden;
}

.thought-accordion-enter-from,
.thought-accordion-leave-to {
    opacity: 0;
    transform: translateY(-4px);
    max-height: 0;
}

/* ── 👣 子 Agent 步骤就地展开卡片 (Inline Step Detail) ── */
.worker-step-item-wrap {
    display: flex;
    flex-direction: column;
    width: 100%;
}

.worker-step-inline-detail {
    margin: 2px 0 8px 0;
    padding: 8px 10px;
    background: rgba(var(--theme-rgb-base, 18, 20, 28), 0.85);
    border: 1px solid rgba(56, 189, 248, 0.2);
    border-radius: 6px;
    box-shadow: var(--theme-shadow, 0 4px 14px rgba(0, 0, 0, 0.3));
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 8px;
}

.inline-detail-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding-bottom: 5px;
    border-bottom: 1px solid rgba(var(--theme-foreground-rgb), 0.06);
}

.inline-title-meta {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
}

.inline-step-num {
    font-size: 9.5px;
    font-weight: 600;
    color: var(--theme-status-info, #38bdf8);
    background: rgba(56, 189, 248, 0.12);
    padding: 0.5px 4.5px;
    border-radius: 3px;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}

.inline-step-title {
    font-size: 11px;
    font-weight: 600;
    color: var(--theme-text-primary, #f1f5f9);
}

.inline-step-dur {
    font-size: 10px;
    color: var(--theme-status-info, #94a3b8);
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}

.btn-inline-open-diff {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 10.5px;
    font-weight: 500;
    color: var(--theme-status-info, #38bdf8);
    background: rgba(56, 189, 248, 0.1);
    border: 1px solid rgba(56, 189, 248, 0.25);
    border-radius: 4px;
    padding: 1.5px 6px;
    cursor: pointer;
    transition: all 0.12s ease;
}

.btn-inline-open-diff:hover {
    background: rgba(56, 189, 248, 0.2);
    border-color: rgba(56, 189, 248, 0.45);
}

.inline-detail-section {
    display: flex;
    flex-direction: column;
    gap: 4px;
}

.detail-sec-head {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: 10px;
    font-weight: 600;
    color: var(--theme-status-info, #94a3b8);
}

.sec-icon {
    font-size: 10px;
}

.sec-title {
    color: var(--theme-text-primary, #cbd5e1);
}

.btn-mini-copy {
    margin-left: auto;
    font-size: 9.5px;
    color: var(--theme-status-info, #38bdf8);
    background: transparent;
    border: none;
    cursor: pointer;
    padding: 0 4px;
}

.btn-mini-copy:hover {
    text-decoration: underline;
}

.sec-code {
    margin: 0;
    padding: 6px 8px;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 10px;
    line-height: 1.45;
    color: var(--theme-text-primary, #e2e8f0);
    background: var(--theme-inset-bg, rgba(0, 0, 0, 0.35));
    border: 1px solid rgba(var(--theme-foreground-rgb), 0.06);
    border-radius: 4px;
    max-height: 160px;
    overflow-y: auto;
    white-space: pre-wrap;
    word-break: break-all;
    scrollbar-width: thin;
    scrollbar-color: rgba(255, 255, 255, 0.25) transparent;
}

.sec-text {
    margin: 0;
    font-size: 10.5px;
    line-height: 1.5;
    color: var(--theme-text-primary, #cbd5e1);
}

.sec-text.thought-text {
    padding: 5px 8px;
    background: rgba(56, 189, 248, 0.05);
    border-left: 2px solid #38bdf8;
    border-radius: 3px;
    color: var(--theme-text-primary, #e2e8f0);
    font-size: 11px;
    line-height: 1.55;
    white-space: pre-wrap;
    word-break: break-word;
}

.step-expand-slide-enter-active,
.step-expand-slide-leave-active {
    transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1);
    overflow: hidden;
}

.step-expand-slide-enter-from,
.step-expand-slide-leave-to {
    opacity: 0;
    transform: translateY(-4px);
    max-height: 0;
}
</style>
