<template>
    <section class="agent-workspace">
        <header class="workspace-header">
            <div class="workspace-brand">
                <div class="brand-mark">A</div>
                <div>
                    <span>FREE_API / AGENT</span>
                    <h1>任务对话</h1>
                </div>
            </div>
            <div class="workspace-status">
                <span class="session-id">会话 {{ panelId.slice(-6) }}</span>
                <span class="run-state" :class="`state-${session.runState.value}`"><i></i>{{ session.stateMeta.value.category }}</span>
                <button class="header-button" type="button" :disabled="session.isRunning.value" @click="session.clearSession()">新对话</button>
                <button class="header-button primary" type="button" @click="settingsOpen = true">设置</button>
            </div>
        </header>

        <div class="workspace-grid">
            <main class="conversation-pane">
                <AgentChatStream
                    :current-goal="session.currentGoal.value"
                    :timeline="session.timeline.value"
                    :selected-item-id="session.selectedTimelineId.value"
                    :completion-message="session.completionMessage.value"
                    :final-summary="session.finalSummary.value"
                    :final-artifacts="session.finalArtifacts.value"
                    :has-error="session.hasError.value"
                    @select="session.selectTimelineItem"
                />
                <AgentComposer
                    :draft="composerDraft"
                    :is-running="session.isRunning.value"
                    :is-waiting-for-human="session.isWaitingForHuman.value"
                    :is-sending="session.isSendingHuman.value"
                    :stop-requested="session.stopRequested.value"
                    :can-start="session.canStart.value"
                    :pause-reason="session.pauseReason.value"
                    @update:draft="composerDraft = $event"
                    @submit="submitComposer"
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
                :selected-item="session.selectedTimelineItem.value"
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
        />
    </section>
</template>

<script setup lang="ts">
import { computed, ref } from "vue";
import AgentChatStream from "./agent/AgentChatStream.vue";
import AgentComposer from "./agent/AgentComposer.vue";
import AgentInspector from "./agent/AgentInspector.vue";
import AgentRecordPreview from "./agent/AgentRecordPreview.vue";
import AgentSettingsDrawer from "./agent/AgentSettingsDrawer.vue";
import type { AgentModelOption } from "./agent/types";
import { useAgentSession } from "../composables/useAgentSession";

const props = defineProps<{
    panelId: string;
    allModels: AgentModelOption[];
}>();

const settingsOpen = ref(false);
const session = useAgentSession(props.panelId);
const composerDraft = computed({
    get: () => session.isRunning.value ? session.humanInput.value : session.goalInput.value,
    set: (value: string) => {
        if (session.isRunning.value) session.humanInput.value = value;
        else session.goalInput.value = value;
    },
});

function submitComposer() {
    if (session.isRunning.value) void session.sendHumanInput();
    else void session.runAgent();
}

function applyPreset(goal: string) {
    session.goalInput.value = goal;
    settingsOpen.value = false;
}

defineExpose({ isRunning: session.isRunning });
</script>

<style scoped>
.agent-workspace {
    --chat-bg: #0b0f16;
    --chat-panel: #101620;
    --chat-inspector-bg: #0f151f;
    --chat-composer-bg: #141b26;
    --chat-tool-bg: rgba(20, 28, 40, .72);
    --chat-hover: rgba(93, 126, 255, .08);
    --chat-user-bg: rgba(53, 91, 176, .18);
    --chat-user-line: rgba(116, 157, 255, .22);
    --chat-line: rgba(148, 163, 184, .13);
    --chat-line-strong: rgba(148, 163, 184, .27);
    --chat-text: #edf3fc;
    --chat-text-soft: #c1cddd;
    --chat-text-muted: #8d9caf;
    --chat-text-faint: #68788f;
    --chat-accent: #8aa7ff;
    --chat-success: #55d6a2;
    --chat-warning: #efbc66;
    --chat-danger: #ff7986;
    display: grid;
    height: 100%;
    min-height: 0;
    grid-template-rows: auto minmax(0, 1fr);
    overflow: hidden;
    color: var(--chat-text);
    background: var(--chat-bg);
    font-variant-numeric: tabular-nums;
}
.workspace-header { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 13px 20px; border-bottom: 1px solid var(--chat-line); background: color-mix(in srgb, var(--chat-panel) 90%, transparent); }
.workspace-brand { display: flex; align-items: center; gap: 10px; min-width: 0; }
.brand-mark { display: grid; width: 29px; height: 29px; flex: 0 0 auto; place-items: center; border-radius: 9px; color: #eff4ff; background: linear-gradient(145deg, #6479ff, #9c6dff); font-size: 12px; font-weight: 850; }
.workspace-brand span { display: block; color: var(--chat-text-faint); font-size: 9px; font-weight: 800; letter-spacing: .1em; }
.workspace-brand h1 { margin: 2px 0 0; color: var(--chat-text); font-size: 15px; letter-spacing: -.02em; }
.workspace-status { display: flex; align-items: center; justify-content: flex-end; gap: 9px; }
.session-id { color: var(--chat-text-faint); font: 10px ui-monospace, SFMono-Regular, Menlo, monospace; }
.run-state { display: inline-flex; align-items: center; gap: 6px; color: var(--chat-text-muted); font-size: 11px; font-weight: 700; }
.run-state i { width: 7px; height: 7px; border-radius: 50%; background: var(--chat-text-faint); }
.run-state.state-planning, .run-state.state-executing { color: var(--chat-accent); }
.run-state.state-planning i, .run-state.state-executing i { background: var(--chat-accent); box-shadow: 0 0 0 4px color-mix(in srgb, var(--chat-accent) 13%, transparent); }
.run-state.state-waiting_for_human { color: var(--chat-warning); }.run-state.state-waiting_for_human i { background: var(--chat-warning); }
.run-state.state-succeeded { color: var(--chat-success); }.run-state.state-succeeded i { background: var(--chat-success); }
.run-state.state-failed { color: var(--chat-danger); }.run-state.state-failed i { background: var(--chat-danger); }
.header-button { border: 1px solid var(--chat-line-strong); border-radius: 8px; padding: 6px 9px; color: var(--chat-text-muted); cursor: pointer; background: transparent; font: inherit; font-size: 11px; }
.header-button:hover:not(:disabled) { border-color: var(--chat-accent); color: var(--chat-text); background: var(--chat-hover); }
.header-button.primary { color: var(--chat-text-soft); background: var(--chat-tool-bg); }
.header-button:disabled { cursor: not-allowed; opacity: .45; }
.workspace-grid { display: grid; min-height: 0; grid-template-columns: minmax(0, 1fr) minmax(300px, 348px); }
.conversation-pane { display: flex; min-width: 0; min-height: 0; flex-direction: column; overflow: hidden; background: radial-gradient(circle at 50% -10%, rgba(101, 124, 255, .07), transparent 38%), var(--chat-bg); }
@media (max-width: 980px) { .agent-workspace { height: auto; overflow: visible; }.workspace-grid { grid-template-columns: minmax(0, 1fr); }.conversation-pane { min-height: 600px; }.workspace-status .session-id { display: none; } }
@media (max-width: 620px) { .workspace-header { padding: 11px 13px; }.workspace-status { gap: 6px; }.run-state { display: none; }.header-button { padding: 6px 8px; } }
</style>
