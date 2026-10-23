<template>
    <section class="manus-agent-shell">
        <header class="manus-chat-header">
            <div class="model-spec-info-row" data-testid="agent-model-routing-trigger">
                <span class="spec-tag pro-tag"><i class="spec-dot"></i>推理 · {{ activeProModelName }}</span>
                <span class="spec-divider">·</span>
                <span class="spec-tag flash-tag">快速 · {{ activeFlashModelName }}</span>
                <span class="spec-divider">·</span>
                <span class="spec-tag policy-tag" data-testid="active-reasoning-policy">档位 · {{ activeReasoningProfileLabel }}</span>
            </div>
            <div class="header-actions">
                <span class="session-status" :class="`state-${session.runState.value}`"><i></i>{{ session.stateMeta.value.category }}</span>
                <button v-if="session.isRunning.value" class="icon-control stop" type="button" :disabled="session.stopRequested.value" title="停止执行" @click="session.requestStop()"><span>■</span></button>
                <button class="icon-control" type="button" :disabled="session.isRunning.value" title="新建对话" @click="emit('new-session')">
                    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 5v14M5 12h14" /></svg>
                </button>
                <button
                    class="icon-control"
                    :class="{ 'is-forking': forking }"
                    type="button"
                    :disabled="session.isRunning.value || panelId.startsWith('draft-') || forking"
                    title="分叉当前会话（完整克隆当前对话全部历史与状态为新会话）"
                    data-testid="fork-current-session"
                    @click="handleFork()"
                >
                    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                        <circle cx="6" cy="6" r="2.5" />
                        <circle cx="18" cy="6" r="2.5" />
                        <circle cx="6" cy="18" r="2.5" />
                        <path d="M6 8.5v7" />
                        <path d="M18 8.5c0 4-4 4-8.5 5.5" />
                    </svg>
                </button>
                <button
                    class="icon-control"
                    type="button"
                    :disabled="session.isRunning.value"
                    data-testid="run-readonly-regression"
                    title="运行约 30 分钟的只读 Agent 回归评测，并自动生成本地报告"
                    @click="session.runReadOnlyRegression()"
                >
                    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M9 3h6"/><path d="M10 3v5.5l-4.7 7.8A3.2 3.2 0 0 0 8 21h8a3.2 3.2 0 0 0 2.7-4.7L14 8.5V3"/><path d="M8.5 16h7"/></svg>
                </button>
                <button
                    class="icon-control"
                    type="button"
                    title="查看当前会话完整系统提示词与上下文流水 (Prompt & Context Debugger)"
                    @click="openPromptContextInspector"
                >
                    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="16" y1="13" x2="8" y2="13"/><line x1="16" y1="17" x2="8" y2="17"/><polyline points="10 9 9 9 8 9"/></svg>
                </button>
                <button
                    class="icon-control"
                    :class="{ active: session.logContext.value }"
                    type="button"
                    :title="session.logContext.value ? '调试模式已开启 (记录并展示完整上下文)' : '开启调试模式 (记录并展示完整上下文)'"
                    @click="session.logContext.value = !session.logContext.value"
                >
                    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="m8 2 1.88 1.88"/><path d="M14.12 3.88 16 2"/><path d="M9 7.13v-1a3.003 3.003 0 1 1 6 0v1"/><path d="M12 20c-3.3 0-6-2.7-6-6v-3a4 4 0 0 1 4-4h4a4 4 0 0 1 4 4v3c0 3.3-2.7 6-6 6"/><path d="M12 20v-9"/><path d="M6.53 9C4.6 8.8 3 7.1 3 5"/><path d="M6 13H2"/><path d="M3 21c0-2.1 1.7-3.9 3.8-4"/><path d="M20.97 5c0 2.1-1.6 3.8-3.5 4"/><path d="M22 13h-4"/><path d="M17.2 17c2.1.1 3.8 1.9 3.8 4"/></svg>
                </button>
                <!-- 👥 多 Agent 协同快捷入口 (存在 Worker 时亮起) -->
                <button
                    v-if="session.teamState.value && session.teamState.value.workers && session.teamState.value.workers.length > 0"
                    class="icon-control team-toggle-btn"
                    :class="{ active: !inspectorCollapsed && inspectorRef?.activeTab === 'team' }"
                    type="button"
                    :title="`查看协作 Worker (${session.teamState.value.workers.length} 个) · 点击在右侧面板打开`"
                    @click="handleOpenTeamDeck()"
                >
                    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                        <path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"></path>
                        <circle cx="9" cy="7" r="4"></circle>
                        <path d="M23 21v-2a4 4 0 0 0-3-3.87"></path>
                        <path d="M16 3.13a4 4 0 0 1 0 7.75"></path>
                    </svg>
                    <span class="team-badge-num">{{ session.teamState.value.workers.length }}</span>
                </button>
                <button
                    class="icon-control preview-toggle-btn"
                    :class="{ active: !inspectorCollapsed }"
                    type="button"
                    :title="inspectorCollapsed ? '展开执行侧栏' : '收起执行侧栏 (全屏对话)'"
                    @click="toggleInspectorCollapse"
                >
                    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="3" width="18" height="18" rx="2"/><path d="M15 3v18"/></svg>
                </button>
                <button class="icon-control" type="button" title="设置" @click="openSettings">
                    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09a1.65 1.65 0 0 0-1-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.6 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.6a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9c.14.37.5.62.9.62H21a2 2 0 0 1 0 4h-.7c-.4 0-.76.25-.9.62Z" /></svg>
                </button>
            </div>
        </header>

        <div
            class="manus-agent-grid"
            :class="{ 'is-resizing': isResizing, 'is-collapsed': inspectorCollapsed }"
            :style="{ gridTemplateColumns: inspectorCollapsed ? 'minmax(0, 1fr) 0px 0px' : `minmax(0, 1fr) 6px ${inspectorWidth}px` }"
        >
            <main class="chat-column">
                <!-- 💬 全宽主对话流 (无左侧狭窄侧栏挤压) -->
                <div class="chat-stream-column">
                    <AgentChatStream
                        :current-goal="session.currentGoal.value"
                        :legacy-messages="session.legacyConversationMessages.value"
                        :history-turns="session.historyTurns.value"
                        :steps="session.steps.value"
                        :selected-step-id="session.selectedStepId.value"
                        :selected-subpart="selectedSubpart"
                        :selected-narrative="selectedNarrativeId"
                        :thought="session.thought.value"
                        :is-thinking="session.isThinking.value"
                        :human-interventions="session.humanInterventions.value"
                        :state-description="session.statusDescription.value"
                        :run-state="session.runState.value"
                        :is-running="session.isRunning.value"
                        :completion-message="session.completionMessage.value"
                        :completion-pending="session.completionPending.value"
                        :final-summary="session.finalSummary.value"
                        :final-artifacts="session.finalArtifacts.value"
                        :has-error="session.hasError.value"
                        :human-form="session.humanForm.value"
                        :pause-reason="session.pauseReason.value"
                        :is-waiting-for-human="session.isWaitingForHuman.value"
                        :is-sending-human="session.isSendingHuman.value"
                        :team-state="session.teamState.value"
                        :review-progress="session.reviewProgress.value"
                        :live-shell-output="session.liveShellOutput.value"
                        @select-step="handleStepSelect"
                        @select-step-thought="handleStepThought"
                        @select-human="handleHumanSelect"
                        @select-active-thought="handleActiveThoughtSelect"
                        @submit-human="session.answerHuman($event)"
                        @open-preview="handleOpenPreview"
                        @open-team-deck="handleOpenTeamDeck"
                        @open-diff="handleOpenDiff"
                        @fork="handleFork"
                    />
                </div>
                <AgentComposer
                    ref="composerRef"
                    :draft="composerDraft"
                    :session-id="panelId"
                    :is-running="session.isRunning.value"
                    :is-waiting-for-human="session.isWaitingForHuman.value"
                    :is-sending="session.isSendingHuman.value"
                    :stop-requested="session.stopRequested.value"
                    :can-start="session.canStart.value"
                    :can-resume-task="session.canResumeTask.value"
                    :pauseReason="session.pauseReason.value"
                    :humanForm="session.humanForm.value"
                    :all-models="allModels"
                    :model-routing="session.modelRouting.value"
                    :telemetry="session.sessionTelemetry.value"
                    :total-turns="totalTurnsCount"
                    :current-turn="currentTurnIndex"
                    :total-stages="totalAllStagesCount"
                    :total-steps="totalAllStepsCount"
                    :queued-goals="session.queuedGoals.value"
                    :auto-pilot="session.autoPilot.value"
                    :plan-mode="session.planMode.value"
                    @update:draft="composerDraft = $event"
                    @update:model-routing="session.modelRouting.value = $event"
                    @update:budget-mode="session.updateBudgetMode($event)"
                    @update:auto-pilot="session.setAutoPilot($event)"
                    @update:plan-mode="session.setPlanMode($event)"
                    @open-settings="openSettings"
                    @open-roles="emit('open-roles')"
                    @attach-files="handleAttachFiles"
                    @submit="submitComposer"
                    @cancel-queue="session.removeQueuedGoal"
                    @stop="session.requestStop"
                >
                    <template #deck>
                        <AgentProgressDeck
                            :steps="activeDeckSteps"
                            :is-running="session.isRunning.value"
                            :is-thinking="session.isThinking.value"
                            :thought="session.thought.value"
                            :action-phase-text="session.actionPhaseText.value"
                            :token-usage="session.tokenUsage.value"
                            :active-step-number="session.activeStepNumber.value"
                            :session-title="deckSessionTitle"
                            :summary="deckSummary"
                            :plan-gate="session.planGate.value"
                            @select-step="handleStepSelect($event.id)"
                            @open-diff="handleOpenDiff"
                        />
                    </template>
                </AgentComposer>
            </main>

            <!-- 可拖拽调节宽度的分割条 (侧栏打开时展示) -->
            <div
                class="grid-resizer"
                :class="{ dragging: isResizing, 'is-hidden': inspectorCollapsed }"
                title="按住左右拖拽调节工作台宽度 / 双击切换宽屏 (580px)"
                @mousedown="startResize"
                @dblclick="toggleInspectorWide"
            ></div>

            <div class="inspector-wrapper" :class="{ 'is-hidden': inspectorCollapsed }">
                <AgentInspector
                    ref="inspectorRef"
                    :run-state="session.runState.value"
                    :state-meta="session.stateMeta.value"
                    :status-description="session.statusDescription.value"
                    :elapsed-label="session.elapsedLabel.value"
                    :token-usage="session.tokenUsage.value"
                    :completed-tool-count="session.completedToolCount.value"
                    :failed-tool-count="session.failedToolCount.value"
                    :selected-step="session.selectedStep.value"
                    :steps="session.steps.value"
                    :selected-subpart="selectedSubpart"
                    :selected-human="selectedHuman"
                    :selected-active-thought="selectedActiveThought"
                    :active-thought-text="session.thought.value"
                    :resources="session.resources.value"
                    :is-resources-loading="session.isResourcesLoading.value"
                    :resource-load-error="session.resourceLoadError.value"
                    :final-summary="session.finalSummary.value"
                    :final-artifacts="session.finalArtifacts.value"
                    :active-preview-content="activePreviewContent"
                    :latest-context="session.latestContext.value"
                    :current-goal="session.currentGoal.value"
                    :log-context="session.logContext.value"
                    :llm-parse-errors="session.llmParseErrors.value"
                    :team-state="session.teamState.value"
                    @refresh-resources="session.refreshResources"
                    @preview-resource="session.openResourcePreview"
                    @agent-submit="handleInspectorAgentSubmit"
                    @select-step="session.selectedStepId.value = $event || null"
                    @open-diff="handleOpenDiff"
                    @close="inspectorCollapsed = true"
                />
            </div>
        </div>

        <AgentRecordPreview
            :resource="session.selectedResource.value"
            :page="session.recordPage.value"
            :loading="session.isRecordsLoading.value"
            :error="session.recordLoadError.value"
            @close="session.closeResourcePreview"
            @previous="session.loadPreviousRecordPage"
            @next="session.loadNextRecordPage"
        />
        <AgentSettingsDrawer
            :open="settingsOpen"
            :session-id="session.activeSessionId.value"
            :project-id="projectId"
            :log-context="session.logContext.value"
            :retry-attempts="session.retryAttempts.value"
            :is-running="session.isRunning.value"
            @close="settingsOpen = false"
            @update:log-context="session.logContext.value = $event"
            @update:retry-attempts="session.retryAttempts.value = $event"
            @open-plugins="openPluginManager"
        />
    </section>
</template>

<script setup lang="ts">
import { computed, nextTick, onMounted, provide, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import AgentChatStream from "./agent/AgentChatStream.vue";
import AgentComposer from "./agent/AgentComposer.vue";
import AgentProgressDeck from "./agent/AgentProgressDeck.vue";
import AgentInspector from "./agent/AgentInspector.vue";
import AgentRecordPreview from "./agent/AgentRecordPreview.vue";
import AgentSettingsDrawer from "./agent/AgentSettingsDrawer.vue";
import type { AgentModelOption, FileDiffPayload } from "./agent/types";
import { useAgentSession } from "../composables/useAgentSession";
import { stopReviewKey } from "./agent/reviewCancellation";

const props = defineProps<{
    panelId: string;
    allModels: AgentModelOption[];
    projectId?: string;
    initialDraft?: string;
}>();
const emit = defineEmits<{
    "new-session": [];
    "open-plugins": [];
    "open-roles": [];
    "update:running": [running: boolean];
    "update:title": [title: string];
    "promoted": [oldId: string, newId: string, title: string, projectId?: string];
    "forked": [payload: { sessionId: string; title: string; promptSeed?: string | null }];
    "show-toast": [text: string, type?: "success" | "info" | "warning"];
}>();
const composerRef = ref<InstanceType<typeof AgentComposer> | null>(null);
const settingsOpen = ref(false);
const selectedSubpart = ref<"thought" | "tool" | null>(null);
const selectedHumanEventId = ref<string | null>(null);
const selectedActiveThought = ref(false);
const session = useAgentSession(props.panelId, {
    projectId: () => props.projectId,
    onPromote: (oldId, newId, title, projectId) => {
        emit("promoted", oldId, newId, title, projectId);
    },
});
provide(stopReviewKey, session.stopReview);

async function openSettings() {
    try {
        // 临时对话可以在首个任务前授权目录；只创建会话，不触发模型请求。
        if (!props.projectId) await session.ensureSession();
        settingsOpen.value = true;
    } catch (error) {
        emit("show-toast", `打开当前对话设置失败：${String(error)}`, "warning");
    }
}

// 分叉落地：把分叉点消息正文预填进输入框，用户可改写后再开跑
watch(() => props.initialDraft, (v) => {
    if (v) session.goalInput.value = v;
}, { immediate: true });


watch(session.sessionTitle, (newTitle) => {
    if (newTitle && newTitle.trim()) {
        emit("update:title", newTitle.trim());
    }
});

watch(session.finalSummary, (summary) => {
    if (summary && summary.trim()) {
        const firstLine = summary.split("\n")[0].replace(/^[#\s\-*`>]+/, "").trim();
        if (firstLine && firstLine.length >= 3 && firstLine.length <= 25) {
            emit("update:title", firstLine);
        }
    }
});

watch(session.isRunning, (val) => {
    emit("update:running", val);
}, { immediate: true });
const selectedNarrativeId = computed(() => selectedActiveThought.value ? "active-thought" : selectedHumanEventId.value);
// 时间线人工应答条目的 id 形如 live-human-{ts}-{idx}（AgentChatStream 生成），
// 本表按 human-{ts}-{idx} 索引——先剥掉 live- 前缀再比对，否则点击永远落空。
const selectedHuman = computed(() => {
    const wanted = (selectedHumanEventId.value || "").replace(/^live-human-/, "human-");
    return session.humanInterventions.value.find((item, index) => `human-${item.timestamp}-${index}` === wanted) || null;
});

const activeDeckSteps = computed(() => {
    if (session.steps.value.length > 0) return session.steps.value;
    const turns = session.historyTurns.value;
    if (turns && turns.length > 0) {
        return turns[turns.length - 1].steps || [];
    }
    return [];
});

const deckSessionTitle = computed(() => {
    return session.sessionTitle.value || session.currentGoal.value || "";
});

const deckSummary = computed(() => {
    if (session.finalSummary.value) return session.finalSummary.value;
    const turns = session.historyTurns.value;
    if (turns && turns.length > 0) {
        return turns[turns.length - 1].summary || turns[turns.length - 1].goal || "";
    }
    return "";
});

// 🌟 会话总体统计数据：一共多少轮 / 一共多少回合 / 累计阶段与步骤
const totalTurnsCount = computed(() => {
    const historyCount = session.historyTurns.value?.length || 0;
    const hasCurrentActive = session.isRunning.value || session.steps.value.length > 0 || Boolean(session.currentGoal.value);
    return Math.max(1, historyCount + (hasCurrentActive ? 1 : 0));
});

const currentTurnIndex = computed(() => {
    const historyCount = session.historyTurns.value?.length || 0;
    const hasCurrentActive = session.isRunning.value || session.steps.value.length > 0 || Boolean(session.currentGoal.value);
    if (hasCurrentActive) {
        return historyCount + 1;
    }
    return Math.max(1, historyCount);
});

const totalAllStepsCount = computed(() => {
    const historySteps = (session.historyTurns.value || []).reduce((acc, t) => acc + (t.steps?.length || 0), 0);
    return historySteps + (session.steps.value?.length || 0);
});

const totalAllStagesCount = computed(() => {
    let stageCount = 0;
    for (const turn of session.historyTurns.value || []) {
        const seenPlans = new Set<string>();
        for (const s of turn.steps || []) {
            const key = s.planId || s.planTitle || "default-plan";
            seenPlans.add(key);
        }
        stageCount += seenPlans.size;
    }
    const currentSeenPlans = new Set<string>();
    for (const s of session.steps.value || []) {
        const key = s.planId || s.planTitle || "default-plan";
        currentSeenPlans.add(key);
    }
    stageCount += currentSeenPlans.size;
    return Math.max(1, stageCount);
});

async function handleAttachFiles(filePaths: string[]) {
    if (props.projectId) {
        for (const p of filePaths) {
            try {
                await invoke("add_agent_project_resource", {
                    projectId: props.projectId,
                    path: p,
                });
            } catch (err) {
                console.error("注册项目受管资源失败:", err);
            }
        }
    }
}

onMounted(() => session.ensureModelRouting(props.allModels));
watch(() => props.allModels, (models) => session.ensureModelRouting(models), { deep: true });

const activeProModelName = computed(() => {
    const id = session.modelRouting.value.pro;
    const model = props.allModels.find((item) => item.id === id);
    return model?.display_name || model?.name || "Manus 1.6";
});
const activeFlashModelName = computed(() => {
    const id = session.modelRouting.value.flash;
    const model = props.allModels.find((item) => item.id === id);
    return model?.display_name || model?.name || "未配置";
});
const reasoningEffortLabel = (value: string) => ({
    auto: "自动",
    off: "关闭",
    low: "低",
    medium: "中",
    high: "高",
    max: "高",
}[value] || "自动");
const activeReasoningProfileLabel = computed(() => {
    const routing = session.modelRouting.value;
    return `推理 ${reasoningEffortLabel(routing.pro_reasoning_effort)} · 执行 ${reasoningEffortLabel(routing.flash_reasoning_effort)} · 视觉 ${reasoningEffortLabel(routing.vision_reasoning_effort)}`;
});
const composerDraft = computed({
    get: () => session.isRunning.value ? session.humanInput.value : session.goalInput.value,
    set: (value: string) => {
        if (session.isRunning.value) session.humanInput.value = value;
        else session.goalInput.value = value;
    },
});

// 💡 保护用户草稿：当任务运行结束 (isRunning 从 true 变为 false) 时，自动将 humanInput 中未发送的草稿平滑保留至 goalInput，绝不清空用户输入
watch(() => session.isRunning.value, (running, prevRunning) => {
    if (prevRunning && !running) {
        const pendingDraft = session.humanInput.value.trim();
        if (pendingDraft) {
            session.goalInput.value = session.humanInput.value;
            session.humanInput.value = "";
        }
    }
});

function clearNarrativeSelection() {
    selectedHumanEventId.value = null;
    selectedActiveThought.value = false;
}

// ⑂ 会话分叉：从某条消息/时间点把历史与任务状态复制为全新会话
interface ForkReport {
    new_session_id: string;
    title: string;
    prompt_seed?: string | null;
}
const forking = ref(false);
async function handleFork(payload: {
    messageId?: string;
    timestampMs?: number;
    upToMessageId?: string;
    upToTimestampMs?: number;
    promptSeed?: string;
} = {}) {
    if (forking.value) return;
    const sessionId = props.panelId;
    if (sessionId.startsWith("draft-")) return;
    if (session.isRunning.value) {
        emit("show-toast", "当前任务正在执行中，请等待完成或停止后再分叉", "warning");
        return;
    }
    forking.value = true;
    try {
        const report = await invoke<ForkReport>("fork_agent_session", {
            sessionId,
            beforeMessageId: payload.messageId ?? null,
            beforeTimestampMs: payload.timestampMs ?? null,
            upToMessageId: payload.upToMessageId ?? null,
            upToTimestampMs: payload.upToTimestampMs ?? null,
            promptSeed: payload.promptSeed ?? null,
        });
        emit("forked", {
            sessionId: report.new_session_id,
            title: report.title,
            promptSeed: report.prompt_seed,
        });
    } catch (e) {
        console.error("[fork] failed:", e);
        emit("show-toast", "分叉会话失败: " + String(e), "warning");
    } finally {
        forking.value = false;
    }
}
function handleStepSelect(id: string) {
    clearNarrativeSelection();
    selectedSubpart.value = "tool";
    session.selectStep(id);
    inspectorCollapsed.value = false;
    inspectorRef.value?.switchTab("detail");
}
function handleStepThought(id: string) {
    clearNarrativeSelection();
    selectedSubpart.value = "thought";
    session.selectStep(id);
    inspectorCollapsed.value = false;
    inspectorRef.value?.switchTab("detail");
}
function handleHumanSelect(id: string) {
    selectedHumanEventId.value = id;
    selectedActiveThought.value = false;
    selectedSubpart.value = null;
    session.selectedStepId.value = null;
    inspectorCollapsed.value = false;
    inspectorRef.value?.switchTab("detail");
}
function handleActiveThoughtSelect() {
    selectedActiveThought.value = true;
    selectedHumanEventId.value = null;
    selectedSubpart.value = null;
    session.selectedStepId.value = null;
    inspectorCollapsed.value = false;
    inspectorRef.value?.switchTab("detail");
}
function handleOpenTeamDeck(stepId?: string) {
    inspectorCollapsed.value = false;
    inspectorRef.value?.switchTab("team");
    if (stepId) {
        session.selectedStepId.value = stepId;
    }
}
function handleOpenDiff(payload: FileDiffPayload) {
    if (payload.stepId && !payload.viewAll) {
        session.selectedStepId.value = payload.stepId;
    }
    inspectorCollapsed.value = false;
    inspectorRef.value?.openDiff(payload);
}
function submitComposer(payload?: { text: string; attachments?: any[]; mode?: "interrupt" | "queue"; taskStartMode?: "resume" | "new" }) {
    const text = (payload?.text ?? composerDraft.value).trim();
    if (!text && (!payload?.attachments || payload.attachments.length === 0)) return;
    if (session.isRunning.value) {
        void session.sendHumanInput(text, payload?.mode || "interrupt", payload?.attachments);
    } else {
        void session.runAgent(text || session.goalInput.value, payload?.attachments, payload?.taskStartMode);
    }
    composerDraft.value = "";
}
function openPluginManager() {
    settingsOpen.value = false;
    emit("open-plugins");
}

const inspectorRef = ref<InstanceType<typeof AgentInspector> | null>(null);
const inspectorWidth = ref(380);
const inspectorCollapsed = ref(true);
const isResizing = ref(false);
const activePreviewContent = ref("");

function startResize(e: MouseEvent) {
    if (inspectorCollapsed.value) return;
    e.preventDefault();
    e.stopPropagation();
    isResizing.value = true;
    document.body.style.userSelect = "none";
    document.body.style.cursor = "col-resize";

    const startX = e.clientX;
    const startWidth = inspectorWidth.value;

    const onMouseMove = (moveEvent: MouseEvent) => {
        moveEvent.preventDefault();
        const delta = startX - moveEvent.clientX;
        const newWidth = Math.min(Math.max(startWidth + delta, 280), window.innerWidth - 380);
        inspectorWidth.value = newWidth;
    };

    const onMouseUp = (upEvent: MouseEvent) => {
        upEvent.preventDefault();
        isResizing.value = false;
        document.body.style.userSelect = "";
        document.body.style.cursor = "";
        window.removeEventListener("mousemove", onMouseMove);
        window.removeEventListener("mouseup", onMouseUp);
    };

    window.addEventListener("mousemove", onMouseMove);
    window.addEventListener("mouseup", onMouseUp);
}

function toggleInspectorCollapse() {
    inspectorCollapsed.value = !inspectorCollapsed.value;
    if (!inspectorCollapsed.value && inspectorWidth.value < 280) {
        inspectorWidth.value = 380;
    }
}

function toggleInspectorWide() {
    if (inspectorCollapsed.value) {
        inspectorCollapsed.value = false;
        inspectorWidth.value = 580;
    } else {
        inspectorWidth.value = inspectorWidth.value >= 540 ? 380 : 580;
    }
}

function openPromptContextInspector() {
    inspectorCollapsed.value = false;
    if (inspectorWidth.value < 460) {
        inspectorWidth.value = 520;
    }
    nextTick(() => {
        inspectorRef.value?.switchTab("context");
    });
}

function handleOpenPreview(code?: string) {
    if (code) {
        activePreviewContent.value = code;
    }
    if (inspectorCollapsed.value) {
        inspectorCollapsed.value = false;
    }
    if (inspectorWidth.value < 540) {
        inspectorWidth.value = 580;
    }
    nextTick(() => {
        inspectorRef.value?.switchTab("preview");
    });
}

async function handleInspectorAgentSubmit(data: any) {
    let text = "";
    if (typeof data === "string") {
        text = data;
    } else {
        try {
            text = JSON.stringify(data, null, 2);
        } catch {
            text = String(data);
        }
    }

    if (session.isWaitingForHuman.value) {
        await session.answerHuman(text);
    } else {
        // 如果当前并非暂停等待状态，自动将用户在沙箱中提交的数据发送给 Agent 作为新一轮目标
        await session.runAgent(`【用户在交互应用中提交了数据】:\n\`\`\`json\n${text}\n\`\`\`\n请基于以上数据继续执行后续分析或入库处理。`);
    }
}

defineExpose({
    isRunning: session.isRunning,
    session,
    runAgent: (goal: string) => session.runAgent(goal),
    attachFiles: (paths: string[]) => composerRef.value?.attachFilePaths(paths),
});
</script>

<style scoped>
.manus-agent-shell {
    display: grid;
    height: 100%;
    min-height: 0;
    grid-template-rows: 52px minmax(0, 1fr);
    overflow: hidden;
    color: #e4e4e7;
    background: #141416;
}

.manus-chat-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 6px 18px 6px 16px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
    background: #18181b;
}

.model-spec-info-row {
    display: inline-flex;
    align-items: center;
    flex-wrap: nowrap;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    gap: 10px;
    padding: 4px 12px;
    margin: 4px 0;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 7px;
    color: #a1a1aa;
    background: rgba(255, 255, 255, 0.025);
    font-size: 11.5px;
    line-height: 1.5;
    user-select: none;
    cursor: default;
}
.model-spec-info-row .spec-tag {
    display: inline-flex;
    align-items: center;
    gap: 6px;
}
.model-spec-info-row .pro-tag {
    color: #f4f4f5;
    font-weight: 550;
}
.model-spec-info-row .spec-dot {
    display: inline-block;
    width: 6.5px;
    height: 6.5px;
    border-radius: 50%;
    background: #38bdf8;
    box-shadow: 0 0 6px rgba(56, 189, 248, 0.7);
}
.model-spec-info-row .flash-tag {
    color: #a1a1aa;
}
.model-spec-info-row .policy-tag {
    color: #71717a;
    font-size: 11px;
}
.model-spec-info-row .spec-divider {
    color: rgba(255, 255, 255, 0.18);
    font-weight: 400;
    font-size: 11px;
    margin: 0 2px;
    user-select: none;
}

.session-picker-btn {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 6px;
    color: #dedee1;
    cursor: pointer;
    background: rgba(255, 255, 255, 0.04);
    font: inherit;
    font-size: 13px;
    font-weight: 550;
    transition: all 0.15s;
}
.session-picker-btn:hover {
    border-color: rgba(255, 255, 255, 0.2);
    color: #ffffff;
    background: rgba(255, 255, 255, 0.08);
}
.session-picker-btn small {
    padding: 1px 5px;
    border-radius: 4px;
    color: #a1a1aa;
    background: rgba(255, 255, 255, 0.06);
    font-size: 11px;
}
.session-picker-btn svg {
    color: #71717a;
    transition: transform 0.2s;
}

.header-actions {
    display: flex;
    align-items: center;
    gap: 8px;
}

.session-status {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 8px;
    border-radius: 5px;
    color: #a1a1aa;
    background: rgba(255, 255, 255, 0.03);
    font-size: 11.5px;
    font-weight: 500;
}
.session-status i {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: #71717a;
}
.session-status.state-running i { background: #3b82f6; box-shadow: 0 0 6px rgba(59, 130, 246, 0.6); }
.session-status.state-waiting_human i { background: #f59e0b; box-shadow: 0 0 6px rgba(245, 158, 11, 0.6); }
.session-status.state-succeeded i { background: #10b981; box-shadow: 0 0 6px rgba(16, 185, 129, 0.6); }
.session-status.state-failed i { background: #ef4444; box-shadow: 0 0 6px rgba(239, 68, 68, 0.6); }

.icon-control {
    display: grid;
    width: 30px;
    height: 30px;
    place-items: center;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 6px;
    color: #a1a1aa;
    cursor: pointer;
    background: rgba(255, 255, 255, 0.03);
    transition: all 0.15s;
}
.icon-control:hover {
    border-color: rgba(255, 255, 255, 0.18);
    color: #ffffff;
    background: rgba(255, 255, 255, 0.08);
}
.icon-control.active {
    color: #fbbf24;
    background: rgba(251, 191, 36, 0.12);
    border-color: rgba(251, 191, 36, 0.3);
}
.icon-control.stop {
    color: var(--red-light);
    border-color: rgba(255, 71, 87, 0.3);
    background: rgba(255, 71, 87, 0.1);
}
.icon-control:disabled {
    cursor: not-allowed;
    opacity: 0.4;
}
.icon-control.is-forking {
    opacity: 0.8;
    color: #a5b4fc;
    border-color: rgba(129, 140, 248, 0.4);
    background: rgba(129, 140, 248, 0.1);
    animation: icon-fork-pulse 1s ease-in-out infinite;
}
@keyframes icon-fork-pulse {
    0%, 100% { transform: scale(1); opacity: 0.8; }
    50% { transform: scale(1.08); opacity: 1; }
}

.manus-agent-shell {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
    background: #141416;
}

.manus-agent-grid {
    display: grid;
    flex: 1;
    min-height: 0;
    min-width: 0;
    position: relative;
    overflow: hidden;
    transition: grid-template-columns 0.35s cubic-bezier(0.16, 1, 0.3, 1);
    will-change: grid-template-columns;
}
.manus-agent-grid.is-resizing {
    transition: none !important;
    user-select: none !important;
    cursor: col-resize !important;
}
.manus-agent-grid.is-resizing * {
    user-select: none !important;
}
.manus-agent-grid.is-resizing :deep(iframe) {
    pointer-events: none !important;
}

/* 分隔拖拽条 */
.grid-resizer {
    position: relative;
    width: 6px;
    height: 100%;
    cursor: col-resize;
    background: transparent;
    z-index: 10;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: opacity 0.25s ease, background 0.15s ease;
    opacity: 1;
}
.grid-resizer.is-hidden {
    opacity: 0;
    pointer-events: none;
}
.grid-resizer:hover, .grid-resizer.dragging {
    background: rgba(251, 191, 36, 0.25);
}
.grid-resizer::after {
    content: "";
    width: 2px;
    height: 32px;
    border-radius: 2px;
    background: rgba(255, 255, 255, 0.15);
    transition: all 0.15s ease;
}
.grid-resizer:hover::after, .grid-resizer.dragging::after {
    height: 48px;
    background: #fbbf24;
}

/* 侧栏包裹器与平滑展开/收起过渡 */
.inspector-wrapper {
    display: flex;
    flex-direction: column;
    height: 100%;
    max-height: 100%;
    min-height: 0;
    min-width: 0;
    overflow: hidden;
    opacity: 1;
    transform: translateX(0);
    transition: opacity 0.3s cubic-bezier(0.16, 1, 0.3, 1), transform 0.35s cubic-bezier(0.16, 1, 0.3, 1);
    will-change: opacity, transform;
}
.inspector-wrapper.is-hidden {
    opacity: 0;
    transform: translateX(24px);
    pointer-events: none;
}

.chat-column {
    display: flex;
    min-width: 0;
    min-height: 0;
    flex-direction: column;
    overflow: hidden;
    background: #141416;
}

/* 💬 全宽主对话流列 */
.chat-stream-column {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
}

/* 顶部协作按钮与徽标 */
.team-toggle-btn {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: #38bdf8;
}

.team-toggle-btn.active {
    background: rgba(56, 189, 248, 0.18);
    color: #38bdf8;
    border-color: rgba(56, 189, 248, 0.35);
}

.team-badge-num {
    font-size: 10px;
    font-weight: 700;
    line-height: 1;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

/* 窄屏自适应：禁止向下堆叠！保持 100% 满高单列，展开时以抽屉浮层覆盖在右侧 */
@media (max-width: 900px) {
    .manus-agent-shell {
        height: 100% !important;
        overflow: hidden !important;
    }
    .manus-agent-grid {
        grid-template-columns: 1fr 0px 0px !important;
        position: relative !important;
    }
    .grid-resizer {
        display: none !important;
    }
    .chat-column {
        height: 100% !important;
        min-height: 0 !important;
    }
    .session-status {
        display: none;
    }
    .manus-agent-grid:not(.is-collapsed) :deep(.agent-inspector) {
        position: absolute !important;
        top: 0 !important;
        right: 0 !important;
        bottom: 0 !important;
        width: min(520px, 94vw) !important;
        z-index: 35 !important;
        box-shadow: -16px 0 40px rgba(0, 0, 0, 0.75) !important;
        border-left: 1px solid rgba(255, 255, 255, 0.15) !important;
        background: #121824 !important;
    }
}

/* ==========================================================================
   📱 移动端专属工作台 Bottom Sheet 抽屉化 (AgentPanel <= 768px)
   ========================================================================== */
@media (max-width: 768px) {
    .manus-chat-header {
        padding: 6px 10px !important;
        gap: 6px !important;
    }
    .model-spec-info-row {
        max-width: 140px !important;
        padding: 3px 6px !important;
        font-size: 11px !important;
    }
    .model-spec-info-row .spec-divider,
    .model-spec-info-row .policy-tag {
        display: none !important;
    }
    .icon-control {
        width: 30px !important;
        height: 30px !important;
    }
    /* 手机端团队侧栏收起隐藏，避免挤占单列 */
    .team-rail-column {
        display: none !important;
    }
    /* 协同检查器在手机端以原生触控质感的底部抽屉 (Bottom Sheet) 呈现 */
    .manus-agent-grid:not(.is-collapsed) :deep(.agent-inspector) {
        position: fixed !important;
        top: auto !important;
        bottom: 0 !important;
        left: 0 !important;
        right: 0 !important;
        width: 100vw !important;
        max-height: 85vh !important;
        height: 85vh !important;
        z-index: 95 !important;
        border-radius: 20px 20px 0 0 !important;
        box-shadow: 0 -12px 48px rgba(0, 0, 0, 0.85) !important;
        border-top: 1px solid rgba(255, 255, 255, 0.18) !important;
        border-left: 0 !important;
        background: #141923 !important;
        backdrop-filter: blur(24px) saturate(180%) !important;
        animation: sheet-slide-up 0.28s cubic-bezier(0.16, 1, 0.3, 1) !important;
    }
    /* 顶部触控把手 (Drag Handle) */
    .manus-agent-grid:not(.is-collapsed) :deep(.agent-inspector)::before {
        content: "";
        position: absolute;
        top: 8px;
        left: 50%;
        transform: translateX(-50%);
        width: 36px;
        height: 4px;
        border-radius: 99px;
        background: rgba(255, 255, 255, 0.25);
        z-index: 100;
    }
}

@keyframes sheet-slide-up {
    from { transform: translateY(100%); }
    to { transform: translateY(0); }
}
</style>
