<template>
    <section class="manus-agent-shell">
        <header class="manus-chat-header">
            <button class="model-selector" data-testid="agent-model-routing-trigger" type="button" title="当前模型路由；点击配置推理、快速与视觉模型" @click="settingsOpen = true">
                <span>推理 · {{ activeProModelName }}</span>
                <small>快速 · {{ activeFlashModelName }}</small>
                <small data-testid="active-reasoning-policy">档位 · {{ activeReasoningProfileLabel }}</small>
                <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><path d="M6 9l6 6 6-6" /></svg>
            </button>
            <div class="header-actions">
                <span class="session-status" :class="`state-${session.runState.value}`"><i></i>{{ session.stateMeta.value.category }}</span>
                <button v-if="session.isRunning.value" class="icon-control stop" type="button" :disabled="session.stopRequested.value" title="停止执行" @click="session.requestStop()"><span>■</span></button>
                <button class="icon-control" type="button" :disabled="session.isRunning.value" title="新建对话" @click="emit('new-session')">
                    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 5v14M5 12h14" /></svg>
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
                <button
                    class="icon-control preview-toggle-btn"
                    :class="{ active: !inspectorCollapsed }"
                    type="button"
                    :title="inspectorCollapsed ? '展开执行侧栏' : '收起执行侧栏 (全屏对话)'"
                    @click="toggleInspectorCollapse"
                >
                    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="3" width="18" height="18" rx="2"/><path d="M15 3v18"/></svg>
                </button>
                <button class="icon-control" type="button" title="设置" @click="settingsOpen = true">
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
                <div class="chat-workspace-split">
                    <!-- 🤖 中间左列：动态 Agent 协作流面板 (自适应竖排，支持一键折叠) -->
                    <aside
                        class="team-side-rail"
                        :class="{ 'is-collapsed': teamRailCollapsed }"
                    >
                        <div class="team-rail-header">
                            <span class="rail-title">⚡ 协作 Worker</span>
                            <button
                                type="button"
                                class="btn-rail-collapse"
                                :title="teamRailCollapsed ? '展开协作列' : '收起协作列'"
                                @click="teamRailCollapsed = !teamRailCollapsed"
                            >
                                <svg viewBox="0 0 24 24" class="rail-chevron" :class="{ flipped: teamRailCollapsed }">
                                    <polyline points="15 18 9 12 15 6"></polyline>
                                </svg>
                            </button>
                        </div>
                        <div v-show="!teamRailCollapsed" class="team-rail-inner">
                            <AgentTeamDeck
                                :team-state="session.teamState.value"
                                :steps="session.steps.value"
                                :current-goal="session.currentGoal.value"
                                :master-step-description="session.statusDescription.value"
                                :only-active="true"
                            />
                        </div>
                    </aside>

                    <!-- 💬 中间右列：主对话流历史记录 (Chat Stream) -->
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
                            :is-running="session.isRunning.value"
                            :completion-message="session.completionMessage.value"
                            :final-summary="session.finalSummary.value"
                            :final-artifacts="session.finalArtifacts.value"
                            :has-error="session.hasError.value"
                            :human-form="session.humanForm.value"
                            :pause-reason="session.pauseReason.value"
                            :is-waiting-for-human="session.isWaitingForHuman.value"
                            :is-sending-human="session.isSendingHuman.value"
                            :team-state="session.teamState.value"
                            @select-step="handleStepSelect"
                            @select-step-thought="handleStepThought"
                            @select-human="handleHumanSelect"
                            @select-active-thought="handleActiveThoughtSelect"
                            @submit-human="session.answerHuman($event)"
                            @open-preview="handleOpenPreview"
                            @open-team-deck="handleOpenTeamDeck"
                        />
                    </div>
                </div>
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
                    @select-step="handleStepSelect($event.id)"
                />
                <AgentComposer
                    :draft="composerDraft"
                    :is-running="session.isRunning.value"
                    :is-waiting-for-human="session.isWaitingForHuman.value"
                    :is-sending="session.isSendingHuman.value"
                    :stop-requested="session.stopRequested.value"
                    :can-start="session.canStart.value"
                    :pauseReason="session.pauseReason.value"
                    :humanForm="session.humanForm.value"
                    :all-models="allModels"
                    :model-routing="session.modelRouting.value"
                    :telemetry="session.sessionTelemetry.value"
                    :queued-goals="session.queuedGoals.value"
                    @update:draft="composerDraft = $event"
                    @update:model-routing="session.modelRouting.value = $event"
                    @update:budget-mode="session.updateBudgetMode($event)"
                    @open-settings="settingsOpen = true"
                    @attach-files="handleAttachFiles"
                    @submit="submitComposer"
                    @cancel-queue="session.removeQueuedGoal"
                    @stop="session.requestStop"
                />
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
import { computed, nextTick, onMounted, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import AgentChatStream from "./agent/AgentChatStream.vue";
import AgentComposer from "./agent/AgentComposer.vue";
import AgentProgressDeck from "./agent/AgentProgressDeck.vue";
import AgentInspector from "./agent/AgentInspector.vue";
import AgentRecordPreview from "./agent/AgentRecordPreview.vue";
import AgentSettingsDrawer from "./agent/AgentSettingsDrawer.vue";
import AgentTeamDeck from "./agent/AgentTeamDeck.vue";
import type { AgentModelOption } from "./agent/types";
import { useAgentSession } from "../composables/useAgentSession";

const props = defineProps<{
    panelId: string;
    allModels: AgentModelOption[];
    projectId?: string;
}>();
const emit = defineEmits<{
    "new-session": [];
    "open-plugins": [];
    "update:running": [running: boolean];
    "update:title": [title: string];
    "promoted": [oldId: string, newId: string, title: string, projectId?: string];
}>();
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
const selectedHuman = computed(() => session.humanInterventions.value.find((item, index) => `human-${item.timestamp}-${index}` === selectedHumanEventId.value) || null);

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

function clearNarrativeSelection() {
    selectedHumanEventId.value = null;
    selectedActiveThought.value = false;
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
    teamRailCollapsed.value = false;
    if (stepId) {
        session.selectedStepId.value = stepId;
    }
}
function submitComposer(payload?: { text: string; attachments?: any[]; mode?: "interrupt" | "queue" }) {
    const text = (payload?.text ?? composerDraft.value).trim();
    if (!text && (!payload?.attachments || payload.attachments.length === 0)) return;
    if (session.isRunning.value) {
        void session.sendHumanInput(text, payload?.mode || "interrupt", payload?.attachments);
    } else {
        void session.runAgent(text || session.goalInput.value, payload?.attachments);
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
const teamRailCollapsed = ref(true);

watch(
    () => session.teamState.value.workers.length,
    (count) => {
        if (count > 0) {
            teamRailCollapsed.value = false;
        }
    },
    { immediate: true }
);

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
});
</script>

<style scoped>
.manus-agent-shell {
    display: grid;
    height: 100%;
    min-height: 0;
    grid-template-rows: 48px minmax(0, 1fr);
    overflow: hidden;
    color: #e4e4e7;
    background: #141416;
}

.manus-chat-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 16px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
    background: #18181b;
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

.chat-workspace-split {
    display: flex;
    flex: 1;
    min-height: 0;
    min-width: 0;
    overflow: hidden;
    position: relative;
}

/* 🤖 中间左侧动态 Agent 协作列 */
.team-side-rail {
    width: 280px;
    max-width: 320px;
    flex-shrink: 0;
    border-right: 1px solid rgba(255, 255, 255, 0.08);
    background: #16161a;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    transition: width 0.25s cubic-bezier(0.16, 1, 0.3, 1);
}

.team-side-rail.is-collapsed {
    width: 32px;
}

.team-rail-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 10px;
    background: #1b1b22;
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
    flex-shrink: 0;
}

.rail-title {
    font-size: 11px;
    font-weight: 700;
    color: #e4e4e7;
    white-space: nowrap;
}

.is-collapsed .rail-title {
    display: none;
}

.btn-rail-collapse {
    background: transparent;
    border: none;
    color: #71717a;
    cursor: pointer;
    padding: 2px;
    display: grid;
    place-items: center;
    border-radius: 4px;
    transition: color 0.15s;
}

.btn-rail-collapse:hover {
    color: #f4f4f5;
    background: rgba(255, 255, 255, 0.06);
}

.rail-chevron {
    width: 13px;
    height: 13px;
    fill: none;
    stroke: currentColor;
    stroke-width: 2.2;
    transition: transform 0.2s ease;
}

.rail-chevron.flipped {
    transform: rotate(180deg);
}

.team-rail-inner {
    flex: 1;
    overflow-y: auto;
    padding: 6px;
}

/* 💬 中间右侧主对话流列 */
.chat-stream-column {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
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
</style>
