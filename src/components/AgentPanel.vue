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
                <button class="icon-control" type="button" title="设置" @click="settingsOpen = true">
                    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09a1.65 1.65 0 0 0-1-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.6 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.6a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9c.14.37.5.62.9.62H21a2 2 0 0 1 0 4h-.7c-.4 0-.76.25-.9.62Z" /></svg>
                </button>
            </div>
        </header>

        <div class="manus-agent-grid">
            <main class="chat-column">
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
                    @select-step="handleStepSelect"
                    @select-step-thought="handleStepThought"
                    @select-human="handleHumanSelect"
                    @select-active-thought="handleActiveThoughtSelect"
                />
                <AgentComposer
                    :draft="composerDraft"
                    :is-running="session.isRunning.value"
                    :is-waiting-for-human="session.isWaitingForHuman.value"
                    :is-sending="session.isSendingHuman.value"
                    :stop-requested="session.stopRequested.value"
                    :can-start="session.canStart.value"
                    :pause-reason="session.pauseReason.value"
                    :human-form="session.humanForm.value"
                    :all-models="allModels"
                    :current-model-id="session.modelRouting.value.pro"
                    @update:draft="composerDraft = $event"
                    @update:model="handleModelSelect"
                    @open-settings="settingsOpen = true"
                    @submit="submitComposer"
                    @submit-form="(value) => void session.submitHumanForm(value)"
                    @stop="session.requestStop"
                />
            </main>

            <AgentInspector
                :run-state="session.runState.value"
                :state-meta="session.stateMeta.value"
                :status-description="session.statusDescription.value"
                :elapsed-label="session.elapsedLabel.value"
                :token-usage="session.tokenUsage.value"
                :completed-tool-count="session.completedToolCount.value"
                :failed-tool-count="session.failedToolCount.value"
                :selected-step="session.selectedStep.value"
                :selected-subpart="selectedSubpart"
                :selected-human="selectedHuman"
                :selected-active-thought="selectedActiveThought"
                :active-thought-text="session.thought.value"
                :resources="session.resources.value"
                :is-resources-loading="session.isResourcesLoading.value"
                :resource-load-error="session.resourceLoadError.value"
                @refresh-resources="session.refreshResources"
                @preview-resource="session.openResourcePreview"
            />
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
            :all-models="allModels"
            :model-routing="session.modelRouting.value"
            :log-context="session.logContext.value"
            :is-running="session.isRunning.value"
            @close="settingsOpen = false"
            @update:model-routing="session.modelRouting.value = $event"
            @update:log-context="session.logContext.value = $event"
            @apply-preset="applyPreset"
            @open-plugins="openPluginManager"
        />
    </section>
</template>

<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { useRouter } from "vue-router";
import AgentChatStream from "./agent/AgentChatStream.vue";
import AgentComposer from "./agent/AgentComposer.vue";
import AgentInspector from "./agent/AgentInspector.vue";
import AgentRecordPreview from "./agent/AgentRecordPreview.vue";
import AgentSettingsDrawer from "./agent/AgentSettingsDrawer.vue";
import type { AgentModelOption } from "./agent/types";
import { useAgentSession } from "../composables/useAgentSession";

const props = defineProps<{ panelId: string; allModels: AgentModelOption[] }>();
const emit = defineEmits<{ "new-session": [] }>();
const router = useRouter();
const settingsOpen = ref(false);
const selectedSubpart = ref<"thought" | "tool" | null>(null);
const selectedHumanEventId = ref<string | null>(null);
const selectedActiveThought = ref(false);
const session = useAgentSession(props.panelId);
const selectedNarrativeId = computed(() => selectedActiveThought.value ? "active-thought" : selectedHumanEventId.value);
const selectedHuman = computed(() => session.humanInterventions.value.find((item, index) => `human-${item.timestamp}-${index}` === selectedHumanEventId.value) || null);

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
}
function handleStepThought(id: string) {
    clearNarrativeSelection();
    selectedSubpart.value = "thought";
    session.selectStep(id);
}
function handleHumanSelect(id: string) {
    selectedHumanEventId.value = id;
    selectedActiveThought.value = false;
    selectedSubpart.value = null;
    session.selectedStepId.value = null;
}
function handleActiveThoughtSelect() {
    selectedActiveThought.value = true;
    selectedHumanEventId.value = null;
    selectedSubpart.value = null;
    session.selectedStepId.value = null;
}
function handleModelSelect(modelId: string) {
    session.modelRouting.value = {
        reasoning_effort_version: 2,
        pro: modelId,
        flash: modelId,
        vision: modelId,
        pro_reasoning_effort: "auto",
        flash_reasoning_effort: "auto",
        vision_reasoning_effort: "auto",
    };
}
function submitComposer() {
    if (session.isRunning.value) void session.sendHumanInput();
    else void session.runAgent();
}
function applyPreset(goal: string) {
    session.goalInput.value = goal;
    settingsOpen.value = false;
}
function openPluginManager() {
    settingsOpen.value = false;
    void router.push("/plugins");
}

defineExpose({ isRunning: session.isRunning });
</script>

<style scoped>
.manus-agent-shell { display: grid; height: 100%; min-height: 0; grid-template-rows: 46px minmax(0, 1fr); overflow: hidden; color: #e8e8ea; background: #202022; }
.manus-chat-header { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 0 16px; border-bottom: 1px solid #363638; background: #202022; }.model-selector { display: inline-flex; align-items: center; gap: 6px; border: 0; color: #e5e5e7; cursor: pointer; background: transparent; font: 600 13px inherit; }.model-selector small { color: #929298; font-size: 10px; font-weight: 600; }.model-selector:hover { color: #fff; }.header-actions { display: flex; align-items: center; gap: 8px; }.session-status { display: inline-flex; align-items: center; gap: 6px; color: #96969b; font-size: 11px; }.session-status i { width: 6px; height: 6px; border-radius: 50%; background: #737378; }.session-status.state-planning i,.session-status.state-executing i { background: #e8e8ea; box-shadow: 0 0 0 3px rgba(232,232,234,.1); }.session-status.state-failed { color: #e08489; }.session-status.state-failed i { background: #e08489; }.icon-control { display: grid; width: 29px; height: 29px; place-items: center; border: 1px solid transparent; border-radius: 6px; color: #a9a9ae; cursor: pointer; background: transparent; }.icon-control:hover { border-color: #49494e; color: #f0f0f1; background: #2a2a2e; }.icon-control.stop { color: #e0a2a5; }.icon-control:disabled { cursor: not-allowed; opacity: .45; }
.manus-agent-grid { display: grid; min-height: 0; grid-template-columns: minmax(0, 1fr) minmax(300px, 342px); }.chat-column { display: flex; min-width: 0; min-height: 0; flex-direction: column; overflow: hidden; background: #202022; }
@media (max-width: 980px) { .manus-agent-shell { height: auto; overflow: visible; }.manus-agent-grid { grid-template-columns: 1fr; }.chat-column { min-height: 620px; }.session-status { display: none; } }
</style>
