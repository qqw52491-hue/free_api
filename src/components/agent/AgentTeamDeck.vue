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

        <!-- ================= 1. 上半部分：竖向 Agent 拓扑树 (Top Half: Vertical Topology Tree) ================= -->
        <section class="topology-overview-section">
            <div class="deck-header-bar">
                <div class="title-with-dot">
                    <span class="live-pulse-dot" :class="`state-${teamState.masterStatus}`"></span>
                    <span class="header-title-text">{{ isMultiAgent ? (isViewingHistoryStep ? '历史协同拓扑' : 'Agent 协同拓扑') : 'Agent 调度拓扑' }}</span>
                </div>
                <div class="header-status-pill" :class="`pill-${teamState.masterStatus}`">
                    {{ isViewingHistoryStep ? '历史回溯' : statusText(teamState.masterStatus) }}
                </div>
            </div>

            <!-- 竖向拓扑连接卡片 -->
            <div class="topo-diagram-card">
                <!-- 👑 主调度 Master 根节点 -->
                <div class="topo-master-node" :class="{ 'is-running': teamState.masterStatus === 'running' }">
                    <div class="master-avatar">👑</div>
                    <div class="master-info-box">
                        <strong class="master-name">主调度 Master</strong>
                        <span class="master-status-desc" :title="currentGoal || '自主执行全流程'">
                            {{ isMultiAgent ? `正在调度 ${displayedWorkers.length} 个活跃子任务` : (currentGoal ? currentGoal.slice(0, 18) + '...' : '自主执行全流程') }}
                        </span>
                    </div>
                    <span class="master-status-badge" :class="`status-${teamState.masterStatus}`">
                        <i v-if="teamState.masterStatus === 'running'" class="spinner-dot"></i>
                        {{ statusText(teamState.masterStatus) }}
                    </span>
                </div>

                <!-- 竖向分支列表 (自上而下垂直展开，舒适间距，点击联动下方 Tab) -->
                <div v-if="isMultiAgent" class="topo-vertical-branches">
                    <div
                        v-for="(worker, wIdx) in displayedWorkers"
                        :key="worker.id"
                        class="topo-vertical-node"
                        :class="[`status-${worker.status}`, { active: activeTab === worker.id, 'is-last': wIdx === displayedWorkers.length - 1 }]"
                        @click="activeTab = worker.id"
                    >
                        <!-- 树干分支连线 -->
                        <div class="tree-spine-col">
                            <div class="spine-line top-line"></div>
                            <div class="spine-curve"></div>
                            <div v-if="wIdx < displayedWorkers.length - 1" class="spine-line bottom-line"></div>
                        </div>

                        <!-- 子 Agent 竖向卡片 -->
                        <div class="topo-worker-card" :title="`${worker.name}: ${worker.currentStep || worker.currentTool}`">
                            <div class="worker-card-left">
                                <span class="worker-role-icon">{{ getRoleIcon(worker.role) }}</span>
                                <strong class="worker-card-name">{{ formatWorkerName(worker.name) }}</strong>
                                <span v-if="worker.currentTool" class="worker-card-tool">
                                    {{ getShortToolName(worker.currentTool) }}
                                </span>
                            </div>
                            <div class="worker-card-right">
                                <span class="worker-status-badge" :class="`status-${worker.status}`">
                                    <i v-if="worker.status === 'running'" class="spinner-dot"></i>
                                    {{ worker.status === 'succeeded' ? '100%' : (worker.status === 'running' ? `${worker.progress}%` : '等待') }}
                                </span>
                            </div>
                        </div>
                    </div>
                </div>

                <!-- 实时模式：所有子任务均已完成时的轻量提示 -->
                <div v-else-if="props.onlyActive && workersList.length > 0" class="topo-all-done-hint">
                    <span class="done-icon">🎉</span>
                    <span class="done-title">所有子任务已完成</span>
                    <span class="done-desc">已成功回传主调度并退出实时队列</span>
                    <button type="button" class="btn-toggle-done" @click="showAllCompleted = !showAllCompleted">
                        {{ showAllCompleted ? '仅看活跃' : `查看全部已完成 (${workersList.length})` }}
                    </button>
                </div>

                <!-- 单 Agent 待命或独立运行状态提示 -->
                <div v-else class="topo-single-hint">
                    <span class="hint-dot">●</span>
                    <span class="hint-text">单 Agent 调度中 · 派发子任务时自动向下展开分支</span>
                </div>
            </div>
        </section>

        <!-- ================= 2. 下半部分：横向短标题多 Tab 链路与独立进度 (Bottom Half: Horizontal Short Tabs) ================= -->
        <section class="agent-chains-tabs-section">
            <!-- 横向排列短标题 Tab 栏 -->
            <div v-if="isMultiAgent" class="chains-tab-nav">
                <button
                    v-for="worker in displayedWorkers"
                    :key="worker.id"
                    type="button"
                    class="chain-tab-btn"
                    :class="{ active: activeTab === worker.id, 'is-running': worker.status === 'running' }"
                    :title="worker.name"
                    @click="activeTab = worker.id"
                >
                    <span class="tab-icon">{{ getRoleIcon(worker.role) }}</span>
                    <span class="tab-title">{{ formatWorkerName(worker.name) }}</span>
                    <span class="tab-progress-tag" :class="`status-${worker.status}`">
                        {{ worker.status === 'succeeded' ? '完成' : `${worker.progress}%` }}
                    </span>
                </button>
            </div>

            <!-- Tab 内容主体区 -->
            <div class="chain-tab-content-panel">
                <!-- 选中的子 Agent 专属独立链路 -->
                <div v-if="selectedWorker" class="chain-detail-view worker-view">
                    <div class="agent-detail-header">
                        <div class="detail-header-left">
                            <span class="agent-big-icon">{{ getRoleIcon(selectedWorker.role) }}</span>
                            <div class="detail-name-meta">
                                <strong class="detail-agent-name" :title="selectedWorker.name">{{ formatWorkerName(selectedWorker.name) }} 链路</strong>
                                <span class="detail-agent-tool">{{ getShortToolName(selectedWorker.currentTool) }} · {{ selectedWorker.roleLabel }}</span>
                            </div>
                        </div>
                        <div class="detail-header-right">
                            <span v-if="selectedWorker.durationMs" class="worker-dur-pill">
                                ⏱ {{ (selectedWorker.durationMs / 1000).toFixed(2) }}s
                            </span>
                            <span class="stage-metric-pill" :class="`status-${selectedWorker.status}`">
                                {{ selectedWorker.status === 'succeeded' ? '已完成' : (selectedWorker.status === 'running' ? '执行中' : '排队中') }}
                            </span>
                        </div>
                    </div>

                    <!-- 子 Agent 独立进度条 -->
                    <div class="chain-progress-bar-wrap">
                        <div class="chain-progress-track">
                            <div
                                class="chain-progress-fill worker-fill"
                                :class="{ 'is-animated': selectedWorker.status === 'running' }"
                                :style="{ width: `${selectedWorker.progress}%` }"
                            ></div>
                        </div>
                    </div>

                    <!-- 📊 ReviewScorecard 审查评分卡 (正确性、破坏性风险、代码规范、测试覆盖) -->
                    <div v-if="selectedWorker.scorecard" class="worker-scorecard-deck">
                        <div class="scorecard-header">
                            <span class="scorecard-icon">📊</span>
                            <strong class="scorecard-title">审查评分卡 (ReviewScorecard)</strong>
                            <span class="scorecard-badge" :class="computeScorecardPass(selectedWorker.scorecard) ? 'pass' : 'fail'">
                                均分: {{ computeScorecardAverage(selectedWorker.scorecard) }} 分 · 
                                {{ computeScorecardPass(selectedWorker.scorecard) ? '✅ 门禁合格放行' : '🚨 未达门禁标准' }}
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
                                    <span class="dim-val" :class="selectedWorker.scorecard.risk < 60 ? 'text-danger' : ''">{{ selectedWorker.scorecard.risk }}/100</span>
                                </div>
                                <div class="progress-track">
                                    <div class="progress-bar risk" :class="selectedWorker.scorecard.risk < 60 ? 'bar-danger' : ''" :style="{ width: `${selectedWorker.scorecard.risk}%` }"></div>
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
                                    <span class="dim-val">{{ selectedWorker.scorecard.test_coverage != null ? `${selectedWorker.scorecard.test_coverage}/100` : 'N/A (免检)' }}</span>
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

                    <div class="worker-steps-chain compact">
                        <!-- 前置步骤 (折叠时展示前3步，展开时展示全部) -->
                        <div
                            v-for="st in headSteps"
                            :key="st.stepNumber"
                            class="worker-step-node compact-node"
                            :class="[`status-${st.status}`, { 'is-active': st.status === 'running' }]"
                        >
                            <div class="step-compact-line">
                                <span class="step-seq-tag">#{{ String(st.stepNumber).padStart(2, '0') }}</span>
                                <span v-if="st.tool" class="step-tool-badge">{{ getShortToolName(st.tool) }}</span>
                                <!-- 第一层：语义概括与意图标题 (加粗醒目，不再是冰冷命令) -->
                                <strong class="step-intent-title" :title="st.title">{{ st.title }}</strong>
                                <span class="step-status-tag" :class="`status-${st.status}`">
                                    {{ st.status === 'running' ? '•' : (st.status === 'succeeded' ? '✔' : (st.status === 'failed' ? '✕' : '•')) }}
                                </span>
                            </div>
                            <!-- 第二层：真实底层调用命令/参数 (浅灰等宽小字，清晰呈现细节) -->
                            <div v-if="st.description && st.description !== st.title" class="step-detail-cmd">
                                <span class="cmd-prompt-icon">›_</span>
                                <span class="cmd-text" :title="st.description">{{ st.description }}</span>
                            </div>
                            <!-- 第三层：关键发现与推导总结 (如有) -->
                            <div v-if="st.summary" class="step-summary-box-compact">
                                <span class="summary-icon">💡</span>
                                <span class="summary-text">{{ st.summary }}</span>
                            </div>
                        </div>

                        <!-- 智能折叠提示条 (点击展开) -->
                        <button
                            v-if="shouldFoldSteps"
                            type="button"
                            class="folded-steps-indicator-bar"
                            title="点击展开中间被收起的常规只读步骤"
                            @click="expandAllSteps = true"
                        >
                            <span class="fold-line"></span>
                            <span class="fold-badge">↕ 期间执行了 {{ foldedCount }} 次只读探查操作 (点击展开)</span>
                            <span class="fold-line"></span>
                        </button>

                        <!-- 后置步骤 (折叠时展示后3步) -->
                        <div
                            v-for="st in tailSteps"
                            :key="st.stepNumber"
                            class="worker-step-node compact-node"
                            :class="[`status-${st.status}`, { 'is-active': st.status === 'running' }]"
                        >
                            <div class="step-compact-line">
                                <span class="step-seq-tag">#{{ String(st.stepNumber).padStart(2, '0') }}</span>
                                <span v-if="st.tool" class="step-tool-badge">{{ getShortToolName(st.tool) }}</span>
                                <!-- 第一层：语义概括与意图标题 (加粗醒目，不再是冰冷命令) -->
                                <strong class="step-intent-title" :title="st.title">{{ st.title }}</strong>
                                <span class="step-status-tag" :class="`status-${st.status}`">
                                    {{ st.status === 'running' ? '•' : (st.status === 'succeeded' ? '✔' : (st.status === 'failed' ? '✕' : '•')) }}
                                </span>
                            </div>
                            <!-- 第二层：真实底层调用命令/参数 (浅灰等宽小字，清晰呈现细节) -->
                            <div v-if="st.description && st.description !== st.title" class="step-detail-cmd">
                                <span class="cmd-prompt-icon">›_</span>
                                <span class="cmd-text" :title="st.description">{{ st.description }}</span>
                            </div>
                            <!-- 第三层：关键发现与推导总结 (如有) -->
                            <div v-if="st.summary" class="step-summary-box-compact">
                                <span class="summary-icon">💡</span>
                                <span class="summary-text">{{ st.summary }}</span>
                            </div>
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

                <!-- 实时模式全部完成时的占位 -->
                <div v-else-if="props.onlyActive && workersList.length > 0" class="no-workers-standby-box">
                    <span class="standby-icon">🎉</span>
                    <strong class="standby-title">子 Agent 均已完成</strong>
                    <p class="standby-desc">当前阶段所有子任务已执行完毕。完整产物与执行过程已汇聚，可在左侧栏查看。</p>
                </div>

                <!-- 无子 Agent 时的极简占位 -->
                <div v-else class="no-workers-standby-box">
                    <span class="standby-icon">🤖</span>
                    <strong class="standby-title">子 Agent 专属工作台</strong>
                    <p class="standby-desc">当前为主调度单兵执行模式。当派发 parallel 或 child_reviewer 时，各子 Agent 的执行链路与独立进度将在此处以多 Tab 形式呈现。</p>
                </div>
            </div>
        </section>
    </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import type { AgentTeamState, AgentWorkerItem, AgentToolStatus, AgentStepItem, AgentWorkerStep, ReviewScorecard } from "./types";
import { extractSubagentsFromStep, formatFinding } from "./workerUtils";
import ExtraContractDeck from "./ExtraContractDeck.vue";

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

function computeScorecardPass(sc?: ReviewScorecard): boolean {
    if (!sc) return false;
    return computeScorecardAverage(sc) >= 75 && sc.risk >= 60;
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
        masterStepDescription?: string;
        onlyActive?: boolean;
        selectedStep?: AgentStepItem | null;
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
        masterStepDescription: "",
        onlyActive: false,
        selectedStep: null,
    }
);

defineEmits<{
    "clear-selected-step": [];
}>();

// 从当前选中的历史步骤中解析出绑定的子 Agent (用于历史回看，支持 wait_jobs 溯源关联)
const stepWorkers = computed(() => {
    return extractSubagentsFromStep(props.selectedStep, props.steps);
});

const isViewingHistoryStep = computed(() => {
    return stepWorkers.value.length > 0;
});

const workersList = computed(() => {
    if (isViewingHistoryStep.value) {
        return stepWorkers.value;
    }
    return props.teamState?.workers || [];
});

const showAllCompleted = ref(false);

const displayedWorkers = computed(() => {
    // 若正在回看历史步骤派发的子任务，哪怕已完成也全量呈现，绝不过滤！
    if (isViewingHistoryStep.value) {
        return stepWorkers.value;
    }
    const all = workersList.value;
    if (!props.onlyActive || showAllCompleted.value) {
        return all;
    }
    // 实时模式：只展示正在运行或排队中的活跃子 Agent，跑完立即收起，不霸占实时界面
    return all.filter((w) => w.status === "running" || w.status === "pending");
});

const isMultiAgent = computed(() => {
    return displayedWorkers.value.length > 0;
});

const activeTab = ref<string>("");

// 当有新的子 Agent 派发出来时，优先聚焦到正在 running 的活跃子 Agent Tab
watch(
    () => displayedWorkers.value.map((w) => `${w.id}-${w.status}`).join(","),
    () => {
        const workers = displayedWorkers.value;
        if (workers.length > 0) {
            if (workers.some((w) => w.id === activeTab.value)) {
                return;
            }
            const running = workers.find((w) => w.status === "running");
            activeTab.value = running ? running.id : workers[0].id;
        } else {
            activeTab.value = "";
        }
    },
    { immediate: true }
);

const selectedWorker = computed(() => {
    return displayedWorkers.value.find((w) => w.id === activeTab.value) || displayedWorkers.value[0];
});

const activeWorkerSteps = computed<AgentWorkerStep[]>(() => {
    if (!selectedWorker.value) return [];
    if (selectedWorker.value.steps && selectedWorker.value.steps.length > 0) {
        return selectedWorker.value.steps;
    }
    // 自适应生成动态分层步骤链
    const w = selectedWorker.value;
    const isDone = w.status === "succeeded";
    const isFailed = w.status === "failed";
    const firstFinding = w.findings && w.findings[0] ? formatFinding(w.findings[0]) : undefined;

    return [
        {
            stepNumber: 1,
            title: "任务拆解与环境安全隔离",
            tool: "task_init",
            description: "解析父级任务约束，初始化只读隔离上下文",
            status: "succeeded",
        },
        {
            stepNumber: 2,
            title: w.currentStep || "执行核心操作与事实证据探查",
            tool: w.currentTool || w.role,
            description: `调用工具 [${getShortToolName(w.currentTool || w.role)}] 深入分析代码与运行日志`,
            summary: w.thought ? (w.thought.slice(0, 110) + (w.thought.length > 110 ? "…" : "")) : undefined,
            status: isDone ? "succeeded" : isFailed ? "failed" : "running",
        },
        {
            stepNumber: 3,
            title: isDone ? "提炼核心发现并交付主调度" : (isFailed ? "任务异常中断并上报主调度" : "等待分析收敛与结果汇总"),
            tool: "finish",
            description: isDone ? "事实证据已充分，成功将全部成果回传至主调度会话" : "等待子任务循环结束",
            summary: firstFinding,
            status: isDone ? "succeeded" : isFailed ? "failed" : "pending",
        },
    ];
});

const expandAllSteps = ref(false);

const shouldFoldSteps = computed(() => {
    return activeWorkerSteps.value.length > 8 && !expandAllSteps.value;
});

const headSteps = computed(() => {
    if (!shouldFoldSteps.value) return activeWorkerSteps.value;
    return activeWorkerSteps.value.slice(0, 3);
});

const tailSteps = computed(() => {
    if (!shouldFoldSteps.value) return [];
    return activeWorkerSteps.value.slice(-3);
});

const foldedCount = computed(() => {
    if (!shouldFoldSteps.value) return 0;
    return activeWorkerSteps.value.length - headSteps.value.length - tailSteps.value.length;
});

function formatWorkerName(name: string): string {
    if (!name) return "子 Agent";
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
    if (name.length > 8) {
        return name.slice(0, 7) + "…";
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
.agent-workspace-deck {
    display: flex;
    flex-direction: column;
    gap: 10px;
    width: 100%;
    background: #141419;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 12px;
    padding: 10px;
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.4);
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
    color: #fef08a;
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
    color: #ffffff;
}

.btn-clear-history-step {
    padding: 2px 7px;
    font-size: 9px;
    background: rgba(255, 255, 255, 0.1);
    border: 1px solid rgba(255, 255, 255, 0.2);
    color: #f4f4f5;
    border-radius: 4px;
    cursor: pointer;
    white-space: nowrap;
    transition: all 0.2s;
}

.btn-clear-history-step:hover {
    background: rgba(234, 179, 8, 0.25);
    border-color: #fef08a;
    color: #fef08a;
}

.deck-header-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding-bottom: 6px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
}

.title-with-dot {
    display: flex;
    align-items: center;
    gap: 6px;
}

.live-pulse-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: #38bdf8;
    box-shadow: 0 0 6px #38bdf8;
}

.live-pulse-dot.state-running {
    animation: pulse-glow 1.2s infinite ease-in-out;
}

@keyframes pulse-glow {
    0%, 100% { transform: scale(0.85); opacity: 0.7; }
    50% { transform: scale(1.3); opacity: 1; box-shadow: 0 0 10px #38bdf8; }
}

.header-title-text {
    font-size: 11.5px;
    font-weight: 700;
    color: #f4f4f5;
}

.header-status-pill {
    font-size: 9px;
    font-weight: 600;
    padding: 1px 6px;
    border-radius: 10px;
    background: rgba(255, 255, 255, 0.06);
    color: #a1a1aa;
}

.header-status-pill.pill-running {
    background: rgba(56, 189, 248, 0.18);
    color: #38bdf8;
    border: 1px solid rgba(56, 189, 248, 0.35);
}

.header-status-pill.pill-succeeded {
    background: rgba(52, 211, 153, 0.18);
    color: #34d399;
}

.topo-diagram-card {
    background: #191922;
    border: 1px solid rgba(255, 255, 255, 0.07);
    border-radius: 7px;
    padding: 5px 6px;
    display: flex;
    flex-direction: column;
    align-items: stretch;
}

.topo-master-node {
    display: flex;
    align-items: center;
    gap: 7px;
    background: #20202c;
    border: 1px solid rgba(255, 255, 255, 0.09);
    border-radius: 6px;
    padding: 4px 6px;
    transition: all 0.15s ease;
}

.topo-master-node.is-running {
    border-color: rgba(56, 189, 248, 0.4);
    background: linear-gradient(90deg, rgba(56, 189, 248, 0.1), #20202c);
}

.master-avatar {
    width: 22px;
    height: 22px;
    border-radius: 5px;
    background: linear-gradient(135deg, #1e293b, #0f172a);
    display: grid;
    place-items: center;
    font-size: 11px;
    flex-shrink: 0;
}

.master-info-box {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
}

.master-name {
    font-size: 10.5px;
    color: #f4f4f5;
}

.master-status-desc {
    font-size: 8.5px;
    color: #a1a1aa;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}

.master-status-badge {
    font-size: 8px;
    padding: 1px 4px;
    border-radius: 3px;
    background: rgba(255, 255, 255, 0.06);
    color: #a1a1aa;
    display: inline-flex;
    align-items: center;
    gap: 3px;
}

.master-status-badge.status-running {
    background: rgba(56, 189, 248, 0.2);
    color: #38bdf8;
}

.master-status-badge.status-succeeded {
    background: rgba(52, 211, 153, 0.2);
    color: #34d399;
}

/* 竖向分支列表 (自上而下延伸，收紧高度) */
.topo-vertical-branches {
    display: flex;
    flex-direction: column;
    margin-top: 3px;
    padding-left: 4px;
    max-height: 120px;
    overflow-y: auto;
}

.topo-vertical-node {
    display: flex;
    align-items: stretch;
    cursor: pointer;
    transition: all 0.15s ease;
}

.tree-spine-col {
    width: 14px;
    position: relative;
    flex-shrink: 0;
}

.spine-line.top-line {
    position: absolute;
    top: 0;
    left: 3px;
    height: 50%;
    width: 1.5px;
    background: rgba(255, 255, 255, 0.15);
}

.spine-curve {
    position: absolute;
    top: 50%;
    left: 3px;
    width: 8px;
    height: 1.5px;
    background: rgba(255, 255, 255, 0.15);
}

.spine-line.bottom-line {
    position: absolute;
    top: 50%;
    left: 3px;
    bottom: 0;
    width: 1.5px;
    background: rgba(255, 255, 255, 0.15);
}

.topo-vertical-node.status-running .spine-line,
.topo-vertical-node.status-running .spine-curve {
    background: #38bdf8;
}

.topo-vertical-node.status-succeeded .spine-line,
.topo-vertical-node.status-succeeded .spine-curve {
    background: #34d399;
}

.topo-worker-card {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 5px;
    padding: 3px 6px;
    margin: 1px 0;
    background: #20202a;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 5px;
    transition: all 0.15s ease;
}

.topo-vertical-node:hover .topo-worker-card {
    border-color: rgba(255, 255, 255, 0.2);
    background: #242432;
}

.topo-vertical-node.active .topo-worker-card {
    border-color: #38bdf8;
    background: linear-gradient(90deg, rgba(56, 189, 248, 0.15), #20202a);
}

.worker-card-left {
    display: flex;
    align-items: center;
    gap: 5px;
    min-width: 0;
}

.worker-role-icon {
    font-size: 11px;
}

.worker-card-name {
    font-size: 10px;
    color: #f4f4f5;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 80px;
}

.worker-card-tool {
    font-size: 8.5px;
    color: #71717a;
    background: rgba(255, 255, 255, 0.04);
    padding: 0 4px;
    border-radius: 3px;
    font-family: ui-monospace, monospace;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}

.worker-status-badge {
    font-size: 8.5px;
    font-weight: 600;
    padding: 1px 5px;
    border-radius: 3px;
    display: inline-flex;
    align-items: center;
    gap: 3px;
}

.worker-status-badge.status-running {
    background: rgba(56, 189, 248, 0.2);
    color: #38bdf8;
}

.worker-status-badge.status-succeeded {
    background: rgba(52, 211, 153, 0.2);
    color: #34d399;
}

.topo-single-hint {
    display: flex;
    align-items: center;
    gap: 5px;
    margin-top: 6px;
    font-size: 9px;
    color: #71717a;
    justify-content: center;
}

.hint-dot {
    font-size: 6px;
    color: #38bdf8;
}

.topo-all-done-hint {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    gap: 3px;
    margin-top: 8px;
    padding: 8px 6px;
    background: rgba(52, 211, 153, 0.08);
    border: 1px dashed rgba(52, 211, 153, 0.25);
    border-radius: 6px;
}

.done-icon {
    font-size: 14px;
}

.done-title {
    font-size: 10px;
    font-weight: 600;
    color: #34d399;
}

.done-desc {
    font-size: 8.5px;
    color: #a1a1aa;
}

.btn-toggle-done {
    margin-top: 4px;
    padding: 2px 8px;
    font-size: 8.5px;
    background: rgba(255, 255, 255, 0.08);
    border: 1px solid rgba(255, 255, 255, 0.15);
    color: #38bdf8;
    border-radius: 4px;
    cursor: pointer;
    transition: all 0.2s;
}

.btn-toggle-done:hover {
    background: rgba(56, 189, 248, 0.15);
    border-color: #38bdf8;
}

/* ================= 2. 下半部分：横向短标题多 Tab 链路与独立进度 ================= */
.agent-chains-tabs-section {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding-top: 6px;
    border-top: 1px solid rgba(255, 255, 255, 0.06);
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
    background: #181820;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 6px;
    color: #a1a1aa;
    font-size: 9.5px;
    font-weight: 600;
    cursor: pointer;
    transition: all 0.15s ease;
    max-width: 130px;
}

.chain-tab-btn:hover {
    color: #f4f4f5;
    background: #21212b;
    border-color: rgba(255, 255, 255, 0.15);
}

.chain-tab-btn.active {
    background: #252533;
    border-color: #38bdf8;
    color: #38bdf8;
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
    color: #38bdf8;
}

.tab-progress-tag.status-succeeded {
    background: rgba(52, 211, 153, 0.2);
    color: #34d399;
}

/* Tab 面板主体 */
.chain-tab-content-panel {
    background: #181822;
    border: 1px solid rgba(255, 255, 255, 0.07);
    border-radius: 8px;
    padding: 8px;
    display: flex;
    flex-direction: column;
    gap: 8px;
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
    color: #f4f4f5;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}

.detail-agent-tool {
    font-size: 8.5px;
    color: #71717a;
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
    background: rgba(255, 255, 255, 0.06);
    color: #a1a1aa;
    white-space: nowrap;
}

.stage-metric-pill.status-running {
    background: rgba(56, 189, 248, 0.2);
    color: #38bdf8;
}

.stage-metric-pill.status-succeeded {
    background: rgba(52, 211, 153, 0.2);
    color: #34d399;
}

.worker-dur-pill {
    font-size: 8.5px;
    color: #71717a;
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
    background: rgba(255, 255, 255, 0.07);
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
    margin-top: 14px;
    padding: 12px 14px;
    background: rgba(18, 22, 34, 0.75);
    border: 1px solid rgba(80, 100, 160, 0.25);
    border-radius: 8px;
}
.scorecard-header {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 12px;
}
.scorecard-icon { font-size: 16px; }
.scorecard-title { font-size: 13px; font-weight: 600; color: #e2e8f0; }
.scorecard-badge {
    margin-left: auto;
    font-size: 11px;
    padding: 2px 8px;
    border-radius: 4px;
    font-weight: 500;
}
.scorecard-badge.pass {
    background: rgba(34, 197, 94, 0.15);
    color: #4ade80;
    border: 1px solid rgba(34, 197, 94, 0.3);
}
.scorecard-badge.fail {
    background: rgba(239, 68, 68, 0.15);
    color: #f87171;
    border: 1px solid rgba(239, 68, 68, 0.3);
}
.scorecard-grid {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: 10px 16px;
}
.scorecard-item {
    display: flex;
    flex-direction: column;
    gap: 4px;
}
.item-label-row {
    display: flex;
    justify-content: space-between;
    font-size: 12px;
}
.dim-name { color: #94a3b8; }
.dim-val { color: #f1f5f9; font-weight: 600; font-family: monospace; }
.dim-val.text-danger { color: #f87171; }
.progress-track {
    width: 100%;
    height: 6px;
    background: rgba(255, 255, 255, 0.08);
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
    background: linear-gradient(135deg, rgba(56, 189, 248, 0.08), rgba(24, 24, 32, 0.95));
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
    color: #38bdf8;
    font-weight: 700;
}

.findings-count {
    font-size: 8.5px;
    color: #a1a1aa;
    background: rgba(255, 255, 255, 0.08);
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
    color: #34d399;
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
    color: #e4e4e7;
    background: rgba(0, 0, 0, 0.2);
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
    color: #ef4444;
    background: rgba(239, 68, 68, 0.18);
}

.finding-item.is-warning {
    border-left-color: #f59e0b;
    background: rgba(245, 158, 11, 0.08);
}

.finding-item.is-warning .finding-num {
    color: #f59e0b;
    background: rgba(245, 158, 11, 0.18);
}

.finding-num {
    font-size: 8.5px;
    font-weight: 700;
    color: #38bdf8;
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
    border-bottom: 1px solid rgba(255, 255, 255, 0.05);
    margin-bottom: 4px;
}

.steps-deck-title {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: 10.5px;
    font-weight: 650;
    color: #e4e4e7;
}

.steps-deck-icon {
    font-size: 12px;
    color: #38bdf8;
}

.btn-toggle-steps-fold {
    background: rgba(255, 255, 255, 0.06);
    border: 1px solid rgba(255, 255, 255, 0.12);
    color: #a1a1aa;
    font-size: 8.5px;
    padding: 2px 6px;
    border-radius: 4px;
    cursor: pointer;
    transition: all 0.15s ease;
}

.btn-toggle-steps-fold:hover {
    color: #ffffff;
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
    background: #171720;
    border: 1px solid rgba(255, 255, 255, 0.05);
    border-radius: 5px;
    transition: all 0.15s ease;
}

.compact-node:hover {
    background: #1b1b26;
    border-color: rgba(255, 255, 255, 0.1);
}

.compact-node.is-active {
    border-color: rgba(56, 189, 248, 0.45);
    background: linear-gradient(90deg, rgba(56, 189, 248, 0.08), #171720);
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
    color: #71717a;
    background: rgba(255, 255, 255, 0.06);
    padding: 1px 4px;
    border-radius: 3px;
    flex-shrink: 0;
}

.compact-node.status-succeeded .step-seq-tag {
    color: #34d399;
    background: rgba(52, 211, 153, 0.12);
}

.compact-node.is-active .step-seq-tag {
    color: #38bdf8;
    background: rgba(56, 189, 248, 0.18);
}

.step-tool-badge {
    font-size: 8px;
    font-family: ui-monospace, SFMono-Regular, monospace;
    padding: 1px 5px;
    border-radius: 3px;
    background: rgba(56, 189, 248, 0.12);
    color: #38bdf8;
    border: 1px solid rgba(56, 189, 248, 0.2);
    flex-shrink: 0;
}

.step-intent-title {
    font-size: 10px;
    font-weight: 650;
    color: #f4f4f5;
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
    color: #71717a;
    line-height: 1.25;
}

.cmd-prompt-icon {
    color: #52525b;
    font-weight: 700;
}

.cmd-text {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    color: #a1a1aa;
}

.compact-node.status-succeeded .step-status-tag {
    color: #34d399;
    font-size: 9px;
    font-weight: 700;
    flex-shrink: 0;
}

.compact-node.is-active .step-status-tag {
    color: #38bdf8;
    font-weight: 700;
    flex-shrink: 0;
}

.step-summary-box-compact {
    display: flex;
    align-items: flex-start;
    gap: 4px;
    font-size: 9px;
    color: #fef08a;
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
    color: #71717a;
    transition: all 0.15s ease;
}

.folded-steps-indicator-bar:hover .fold-badge {
    color: #38bdf8;
    border-color: rgba(56, 189, 248, 0.3);
    background: rgba(56, 189, 248, 0.1);
}

.fold-line {
    flex: 1;
    height: 1px;
    background: rgba(255, 255, 255, 0.08);
}

.fold-badge {
    font-size: 8.5px;
    padding: 2px 8px;
    border-radius: 10px;
    background: #181822;
    border: 1px dashed rgba(255, 255, 255, 0.15);
    white-space: nowrap;
}


/* 描述层级：具体执行内容 */
.step-desc-box {
    font-size: 9.5px;
    color: #a1a1aa;
    line-height: 1.45;
    background: rgba(255, 255, 255, 0.03);
    padding: 4px 6px;
    border-radius: 4px;
    border-left: 2px solid rgba(255, 255, 255, 0.12);
    word-break: break-word;
}

/* 总结层级：执行发现/产出 */
.step-summary-box {
    display: flex;
    align-items: flex-start;
    gap: 5px;
    font-size: 9.5px;
    color: #fef08a;
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
    background: #111116;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 6px;
    overflow: hidden;
    margin-top: 2px;
}

.terminal-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 3px 6px;
    background: rgba(255, 255, 255, 0.04);
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
    font-size: 8.5px;
    color: #71717a;
}

.btn-copy-out {
    background: transparent;
    border: none;
    font-size: 8.5px;
    color: #38bdf8;
    cursor: pointer;
    padding: 0 2px;
}

.terminal-body {
    margin: 0;
    padding: 6px;
    font-size: 8.5px;
    font-family: ui-monospace, SFMono-Regular, monospace;
    color: #e4e4e7;
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
    color: #f4f4f5;
}

.standby-desc {
    margin: 0;
    font-size: 9px;
    color: #71717a;
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
</style>
