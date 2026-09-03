<template>
    <div ref="streamElement" class="process-tree" :class="{ 'is-running': isRunning }" aria-label="Agent 分层执行过程" @scroll.passive="onStreamScroll">
        <button v-if="isRunning && !followLiveTail" type="button" class="follow-latest" @click="resumeLiveFollow">跟随最新输出</button>

        <div class="stream-centered-content">
            <section v-if="!currentGoal && !timelineEntries.length && !legacyMessages.length" class="empty-conversation">
                <div class="empty-logo">
                    <svg width="22" height="22" viewBox="0 0 24 24" fill="currentColor" stroke="none">
                        <polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"></polygon>
                    </svg>
                </div>
                <h2>有什么可以帮你的？</h2>
                <p>输入一个任务。每个回合会保留自己的过程、成果与总结。</p>
            </section>

            <!-- 早期对话记录 -->
            <section v-if="legacyMessages.length" class="conversation-history" aria-label="早期对话记录">
                <article v-for="message in legacyMessages" :key="message.id" class="saved-message" :class="message.role === 'user' ? 'from-user' : 'from-agent'">
                    <template v-if="message.role === 'agent' || message.role === 'assistant'">
                        <div class="agent-msg-header">
                            <span class="agent-logo-pill">⚡ Free API</span>
                            <time class="msg-clock">{{ formatConversationTime(message.created_at) }}</time>
                        </div>
                        <div class="agent-text-body">
                            <RichContentRenderer :content="message.content" :compact-html="true" @open-preview="$emit('open-preview', $event)" />
                        </div>
                    </template>
                    <template v-else>
                        <!-- 📎 附件缩略图与文件卡片预览托盘 (完全复用 AttachmentCard) -->
                        <div v-if="parseUserMessageAttachments(message.content).attachments.length > 0" class="stream-attachments-tray">
                            <AttachmentCard
                                v-for="(att, aIdx) in parseUserMessageAttachments(message.content).attachments"
                                :key="aIdx"
                                :attachment="att"
                                @preview-image="openImagePreview"
                                @preview-doc="openDocPreview"
                            />
                        </div>
                        <p v-if="parseUserMessageAttachments(message.content).cleanText" class="user-text-content">
                            {{ parseUserMessageAttachments(message.content).cleanText }}
                        </p>
                        <time class="msg-clock">{{ formatConversationTime(message.created_at) }}</time>
                    </template>
                </article>
            </section>

            <template v-for="entry in timelineEntries" :key="entry.id">
                <article v-if="entry.type === 'user'" class="user-message">
                    <!-- 📎 附件缩略图与文件卡片预览托盘 (完全复用 AttachmentCard) -->
                    <div v-if="parseUserMessageAttachments(entry.content).attachments.length > 0" class="stream-attachments-tray">
                        <AttachmentCard
                            v-for="(att, aIdx) in parseUserMessageAttachments(entry.content).attachments"
                            :key="aIdx"
                            :attachment="att"
                            @preview-image="openImagePreview"
                            @preview-doc="openDocPreview"
                        />
                    </div>
                    <!-- 用户指令文本 -->
                    <p v-if="parseUserMessageAttachments(entry.content).cleanText" class="user-text-content">
                        {{ parseUserMessageAttachments(entry.content).cleanText }}
                    </p>
                </article>
                <article v-else-if="entry.type === 'message'" class="saved-message" :class="entry.role === 'user' ? 'from-user' : 'from-agent'" @click="entry.isHuman && $emit('select-human', entry.id)">
                    <template v-if="entry.role === 'agent' || entry.role === 'assistant'">
                        <div class="agent-msg-header">
                            <span class="agent-logo-pill">⚡ Free API</span>
                            <time class="msg-clock">{{ formatClock(entry.timestamp) }}</time>
                        </div>
                        <div class="agent-text-body">
                            <RichContentRenderer :content="entry.content" :compact-html="true" @open-preview="$emit('open-preview', $event)" />
                        </div>
                    </template>
                    <template v-else>
                        <span v-if="entry.isHuman" class="intervention-indicator">人工干预输入</span>
                        <!-- 📎 附件缩略图与文件卡片预览托盘 (完全复用 AttachmentCard) -->
                        <div v-if="parseUserMessageAttachments(entry.content).attachments.length > 0" class="stream-attachments-tray">
                            <AttachmentCard
                                v-for="(att, aIdx) in parseUserMessageAttachments(entry.content).attachments"
                                :key="aIdx"
                                :attachment="att"
                                @preview-image="openImagePreview"
                                @preview-doc="openDocPreview"
                            />
                        </div>
                        <p v-if="parseUserMessageAttachments(entry.content).cleanText" class="user-text-content">
                            {{ parseUserMessageAttachments(entry.content).cleanText }}
                        </p>
                        <time class="msg-clock">{{ formatClock(entry.timestamp) }}</time>
                    </template>
                </article>
                <article v-else-if="entry.type === 'plan'" class="plan-node" :class="[`status-${entry.status}`, { 'is-active-plan': entry.status === 'running' || entry.isThinking, 'is-collapsed': !isPlanExpanded(entry) }]">
                    <div class="plan-row" role="button" tabindex="0" title="点击展开/折叠执行过程" @click="togglePlan(entry.id, entry.status, entry.isThinking)" @keydown.enter="togglePlan(entry.id, entry.status, entry.isThinking)">
                        <span class="plan-expand-arrow" :class="{ open: isPlanExpanded(entry) }">
                            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
                                <polyline points="9 18 15 12 9 6"></polyline>
                            </svg>
                        </span>
                        <span class="plan-mark"><i></i></span>
                        <span class="plan-title">
                            <small>计划</small>{{ entry.title }}
                            <span v-if="!isPlanExpanded(entry)" class="plan-collapsed-hint">（{{ entry.steps.length }} 个步骤 · 点击展开）</span>
                        </span>
                        <span class="row-status">{{ statusLabel(entry.status) }}</span>
                        <time>{{ formatClock(entry.timestamp) }}</time>
                    </div>
                    <div class="plan-children-wrapper" :class="{ expanded: isPlanExpanded(entry) }">
                        <div class="plan-children-inner">
                            <div class="plan-children">
                                <article v-for="step in entry.steps" :key="step.id" class="goal-node" :class="[`status-${step.status}`, { 'is-subagent': isSubAgentTool(step.tool), 'is-waitjobs': isWaitJobsTool(step.tool) }]">
                                    <button class="goal-row" :class="{ selected: step.id === selectedStepId, 'subagent-row': isSubAgentTool(step.tool), 'waitjobs-row': isWaitJobsTool(step.tool) }" type="button" @click="$emit('select-step', step.id)">
                                        <span class="tree-mark"><i></i></span>
                                        <span class="step-tool-emoji">{{ getToolIcon(step.tool, step.action).emoji }}</span>
                                        <span class="goal-label">
                                            <span v-if="isSubAgentTool(step.tool)" class="subagent-tag-badge">子智能体 · 审查</span>
                                            <span v-else-if="isWaitJobsTool(step.tool)" class="waitjobs-tag-badge">异步挂起</span>
                                            {{ step.description || '执行步骤' }}
                                            <small v-if="step.modelTier" class="model-tier">{{ step.modelTier }}</small>
                                        </span>
                                        <span v-if="getStepMetricsPill(step)" class="step-metrics-badge" :title="getStepMetricsTooltip(step)">
                                            {{ getStepMetricsPill(step) }}
                                        </span>
                                        <span class="row-status" :class="`status-tag-${step.status}`">{{ isSubAgentTool(step.tool) && step.status === 'running' ? '后台审查中' : (isWaitJobsTool(step.tool) && step.status === 'running' ? '挂起等待' : statusLabel(step.status)) }}</span>
                                        <time>{{ formatClock(step.timestamp) }}</time>
                                    </button>
                                    <div class="node-children">
                                        <button v-if="step.thought" class="child-row thought-child" :class="{ selected: step.id === selectedStepId && selectedSubpart === 'thought' }" type="button" @click="$emit('select-step-thought', step.id)">
                                            <span class="child-mark"></span>
                                            <span class="tool-tag thought-tag">🧠 思考</span>
                                            <span>{{ shortText(step.thought, 110) }}</span>
                                            <em v-if="step.metrics?.thinkingDurationMs">{{ (step.metrics.thinkingDurationMs / 1000).toFixed(1) }}s</em>
                                            <em v-else>已分析</em>
                                        </button>
                                        <button class="child-row tool-child" :class="{ selected: step.id === selectedStepId && selectedSubpart === 'tool', 'tool-child-subagent': isSubAgentTool(step.tool), 'tool-child-wait': isWaitJobsTool(step.tool) }" type="button" @click="$emit('select-step', step.id)">
                                            <span class="child-mark"></span>
                                            <span class="tool-tag" :style="{ color: getToolIcon(step.tool, step.action).color, background: `${getToolIcon(step.tool, step.action).color}18` }">
                                                {{ getToolIcon(step.tool, step.action).emoji }} {{ isSubAgentTool(step.tool) ? '子审查代理' : (isWaitJobsTool(step.tool) ? '零 Token 挂起' : (step.tool || '工具')) }}
                                            </span>
                                            <span :class="{ 'subagent-action-text': isSubAgentTool(step.tool), 'waitjobs-action-text': isWaitJobsTool(step.tool) }">
                                                {{ isSubAgentTool(step.tool) ? (step.status === 'running' ? '⚡ 正在后台独立执行调查审查，点击右侧抽屉可查看证据包与发现…' : (step.action || step.explanation || '已生成审查结论 · 点击查看详情')) : (isWaitJobsTool(step.tool) ? (step.status === 'running' ? '⏳ 当前协程已反应式挂起等待后台通知，0 Token 0 CPU 消耗…' : '✅ 后台任务已完成，已唤醒并回收全部结论') : shortText(step.action || step.explanation || '执行操作', 110)) }}
                                            </span>
                                            <em v-if="step.metrics?.toolDurationMs">{{ (step.metrics.toolDurationMs / 1000).toFixed(1) }}s · {{ step.source }}</em>
                                            <em v-else>{{ step.source }}</em>
                                        </button>
                                    </div>
                                    <div v-if="isSubAgentTool(step.tool) || isWaitJobsTool(step.tool)" class="subagent-inline-capsule" role="button" tabindex="0" :title="'点击查看当前步骤派发的子任务详情：' + extractSubagentTask(step)" @click="$emit('open-team-deck', step.id)" @keydown.enter="$emit('open-team-deck', step.id)">
                                        <div class="capsule-left">
                                            <span class="capsule-pulse-dot" :class="{ 'is-running': step.status === 'running' }"></span>
                                            <span class="capsule-text">
                                                {{ step.status === 'running' ? '⚡ 正在独立执行：' : '✅ 协同任务已完成：' }}
                                                <strong>{{ extractSubagentTask(step) }}</strong>
                                            </span>
                                        </div>
                                        <div class="capsule-action">
                                            <span>实时看板</span>
                                            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><polyline points="9 18 15 12 9 6"></polyline></svg>
                                        </div>
                                    </div>
                                </article>
                                <article v-if="entry.isThinking && (thought || isRunning)" class="active-thought-stream" :class="{ selected: selectedNarrative === 'active-thought' }" role="button" tabindex="0" @click="$emit('select-active-thought')" @keydown.enter="$emit('select-active-thought')">
                                    <header><span class="thought-pulse-dot" :class="{ 'is-running': isRunning }"><i></i></span><b>思考</b><em>{{ isRunning ? '思考中…' : '思考过程' }}</em><time>{{ formatClock(entry.thinkingTimestamp || entry.timestamp) }}</time></header>
                                    <pre class="active-thought-content">{{ thought || '正在分析下一步操作…' }}</pre>
                                </article>
                            </div>
                        </div>
                    </div>
                </article>
                <article v-else-if="entry.type === 'active-thought'" class="active-thought-stream root-thought-stream" :class="{ selected: selectedNarrative === 'active-thought' }" role="button" tabindex="0" @click="$emit('select-active-thought')" @keydown.enter="$emit('select-active-thought')">
                    <header><span class="thought-pulse-dot" :class="{ 'is-running': isRunning }"><i></i></span><b>思考</b><em>{{ isRunning ? '思考中…' : '思考过程' }}</em><time>{{ formatClock(entry.timestamp) }}</time></header>
                    <div v-if="isRunning && !thought" class="active-thought-initial">
                        <span class="manus-spinner small"></span>
                        <span>正在连接模型并深度思考规划执行步骤…</span>
                    </div>
                    <pre v-else class="active-thought-content">{{ thought || '正在分析下一步操作…' }}</pre>
                </article>
                <article v-else-if="entry.type === 'completion'" class="saved-message from-agent final-response" :class="{ failed: entry.failed }">
                    <div class="agent-msg-header">
                        <span class="agent-logo-pill">⚡ Free API</span>
                        <time class="msg-clock">{{ formatClock(entry.timestamp) }}</time>
                    </div>
                    <div class="summary-rich">
                        <RichContentRenderer :content="(entry.summary || entry.title) + (entry.id === 'live-completion' && isStreamingSummary ? ' ▌' : '')" :compact-html="true" @open-preview="$emit('open-preview', $event)" />
                    </div>
                    <div v-if="entry.artifacts && entry.artifacts.length" class="final-artifacts">
                        <ArtifactList
                            :artifacts="entry.artifacts"
                            @preview="handlePreviewArtifact"
                            @download="handleDownloadArtifact"
                        />
                    </div>
                </article>
            </template>

            <!-- 人工干预/交互选择卡片 (单选/多选/确认/传文件/补充输入) -->
            <AgentHumanInterventionCard
                v-if="isWaitingForHuman"
                :human-form="humanForm"
                :pause-reason="pauseReason"
                :is-sending="isSendingHuman"
                @submit="$emit('submit-human', $event)"
            />

            <p v-if="isRunning && !timelineEntries.length && !legacyMessages.length" class="idle-live"><i></i>{{ stateDescription }}</p>
        </div>

        <!-- 📎 全局全屏附件预览弹窗 (Teleport 到 body) -->
        <AttachmentPreviewModal
            :image-modal="previewImageModal"
            :doc-modal="previewDocModal"
            @close-image="previewImageModal = null"
            @close-doc="previewDocModal = null"
        />
    </div>
</template>
<script setup lang="ts">
import { computed, reactive, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import AttachmentCard from "./AttachmentCard.vue";
import AttachmentPreviewModal from "./AttachmentPreviewModal.vue";
import type { AgentHumanForm, AgentStepItem, AgentToolStatus, AgentTeamState, ParsedAttachment } from "./types";
import type { AgentArtifactInput, ArtifactDescriptor } from "../../types/artifact";
import { formatClock, getToolIcon, shortText } from "./format";
import RichContentRenderer from "../charts/RichContentRenderer.vue";
import AgentHumanInterventionCard from "./AgentHumanInterventionCard.vue";
import ArtifactList from "../artifacts/ArtifactList.vue";

function parseUserMessageAttachments(content: string): { cleanText: string; attachments: ParsedAttachment[] } {
    if (!content) return { cleanText: "", attachments: [] };
    const attRegex = /📎\s*\[附件:\s*([^\s()\]]+(?:\s+[^\s()\]]+)*?)(?:\s*\(([^)]+)\))?(?:\s*\|\s*路径:\s*([^\]]+))?\]/g;
    const attachments: ParsedAttachment[] = [];
    let match;
    while ((match = attRegex.exec(content)) !== null) {
        const fileName = match[1]?.trim() || "file";
        const fileSizeStr = match[2]?.trim() || "";
        const filePath = match[3]?.trim() || fileName;
        const isImage = /\.(png|jpg|jpeg|webp|gif|bmp|svg)$/i.test(fileName);

        let sizeBytes = 0;
        if (fileSizeStr.includes("MB")) {
            sizeBytes = Math.round(parseFloat(fileSizeStr) * 1024 * 1024);
        } else if (fileSizeStr.includes("KB")) {
            sizeBytes = Math.round(parseFloat(fileSizeStr) * 1024);
        } else if (fileSizeStr.includes("B")) {
            sizeBytes = Math.round(parseFloat(fileSizeStr));
        }

        attachments.push({
            file_name: fileName,
            file_path: filePath,
            size_bytes: sizeBytes,
            content_type: isImage ? "image/jpeg" : "file",
            prompt_text: "",
            image_data_url: null,
            raw_preview: null,
        });
    }
    let cleanText = content.replace(attRegex, "").trim();
    return { cleanText, attachments };
}

const previewImageModal = ref<ParsedAttachment | null>(null);
const previewDocModal = ref<ParsedAttachment | null>(null);

async function openImagePreview(att: ParsedAttachment) {
    if (att.image_data_url) {
        previewImageModal.value = att;
        return;
    }
    const path = att.file_path || att.file_name;
    if (path) {
        try {
            const parsed = await invoke<ParsedAttachment>("parse_attachment_file", { path });
            if (parsed && parsed.image_data_url) {
                previewImageModal.value = parsed;
                return;
            }
        } catch (e) {
            console.warn("读取本地图片失败:", e);
        }
    }
    previewImageModal.value = att;
}

async function openDocPreview(att: ParsedAttachment) {
    if (att.prompt_text || att.raw_preview) {
        previewDocModal.value = att;
        return;
    }
    const path = att.file_path || att.file_name;
    if (path) {
        try {
            const parsed = await invoke<ParsedAttachment>("parse_attachment_file", { path });
            if (parsed) {
                previewDocModal.value = parsed;
                return;
            }
        } catch (e) {
            console.warn("读取本地文档失败:", e);
        }
    }
    previewDocModal.value = att;
}

type HistoryMessage = { id: string; role: string; content: string; created_at: string; timestamp: number };
type HistoryTurn = { id: string; goal: string; status: string; steps: AgentStepItem[]; messages: HistoryMessage[]; summary: string; artifacts: AgentArtifactInput[]; startedAt: number; completedAt: number };
type PlanEntry = { id: string; type: "plan"; timestamp: number; title: string; status: AgentToolStatus; steps: AgentStepItem[]; isThinking: boolean; thinkingTimestamp?: number };
type TimelineEntry =
    | PlanEntry
    | { id: string; type: "user"; timestamp: number; content: string }
    | { id: string; type: "message"; timestamp: number; role: string; content: string; isHuman: boolean }
    | { id: string; type: "completion"; timestamp: number; title: string; summary: string; artifacts: AgentArtifactInput[]; failed: boolean }
    | { id: "active-thought"; type: "active-thought"; timestamp: number };

const props = withDefaults(
    defineProps<{
        currentGoal: string;
        legacyMessages: { id: string; role: string; content: string; created_at: string }[];
        historyTurns: HistoryTurn[];
        steps: AgentStepItem[];
        selectedStepId: string | null;
        selectedSubpart: "thought" | "tool" | null;
        selectedNarrative: string | null;
        thought: string;
        isThinking: boolean;
        humanInterventions: { message: string; timestamp: number }[];
        stateDescription: string;
        isRunning: boolean;
        completionMessage: string;
        finalSummary: string;
        finalArtifacts: AgentArtifactInput[];
        hasError: boolean;
        humanForm?: AgentHumanForm | null;
        pauseReason?: string;
        isWaitingForHuman?: boolean;
        isSendingHuman?: boolean;
        teamState?: AgentTeamState;
    }>(),
    {
        humanForm: null,
        pauseReason: "",
        isWaitingForHuman: false,
        isSendingHuman: false,
        finalArtifacts: () => [],
    }
);

const emit = defineEmits<{
    "select-step": [id: string];
    "select-step-thought": [id: string];
    "select-human": [id: string];
    "select-active-thought": [];
    "submit-human": [answer: string];
    "open-preview": [code: string];
    "open-team-deck": [stepId?: string];
}>();

function extractSubagentTask(step: any): string {
    if (!step) return "后台子任务";
    const params = step.params || step.params_summary;
    if (params) {
        if (typeof params === "object") {
            const p = params as any;
            if (p.task && typeof p.task === "string") return p.task;
            if (p.goal && typeof p.goal === "string") return p.goal;
            if (p.command && typeof p.command === "string") return p.command;
            if (p.instruction && typeof p.instruction === "string") return p.instruction;
        } else if (typeof params === "string") {
            try {
                const parsed = JSON.parse(params);
                if (parsed.task) return parsed.task;
                if (parsed.goal) return parsed.goal;
            } catch {}
        }
    }
    if (step.description && step.description !== "执行操作") return step.description;
    if (step.action && step.action !== "执行操作") return step.action;
    return isSubAgentTool(step.tool) ? "独立子任务执行与审查" : "后台协同任务";
}

async function handlePreviewArtifact(artifact: ArtifactDescriptor) {
    if (!artifact.content) {
        if (artifact.artifactId) {
            const { readArtifactById } = await import("../../utils/artifact");
            const text = await readArtifactById(artifact.artifactId);
            if (text) artifact.content = text;
        } else if (artifact.localPath) {
            const { readArtifactFile } = await import("../../utils/artifact");
            const text = await readArtifactFile(artifact.localPath);
            if (text) artifact.content = text;
        }
    }
    const content = artifact.content || artifact.localPath || "";
    emit("open-preview", content);
}

function handleDownloadArtifact(_artifact: ArtifactDescriptor) {
    // 下载逻辑由 ArtifactCard 自带处理，此处保留钩子
}
const streamElement = ref<HTMLElement | null>(null);
const activeThoughtStartedAt = ref<number | null>(null);
const followLiveTail = ref(true);
const planExpandedState = reactive<Record<string, boolean>>({});

function isPlanExpanded(plan: PlanEntry): boolean {
    if (planExpandedState[plan.id] !== undefined) {
        return planExpandedState[plan.id];
    }
    // 1. 如果当前任务正在执行中（isRunning），计划链条始终保持展开，杜绝步骤切换时的反复收缩与弹跳
    if (props.isRunning) {
        return true;
    }
    // 2. 当前回合的最新计划（live-）或正在运行/思考中的计划，默认保持展开供用户查看执行细节
    if (plan.id.startsWith("live-") || plan.status === "running" || plan.isThinking) {
        return true;
    }
    // 3. 只有历史轮次的已完成归档计划（history-），默认收起
    return false;
}

function togglePlan(planId: string, currentStatus: AgentToolStatus, isThinking: boolean) {
    const defaultState = props.isRunning || planId.startsWith("live-") || currentStatus === "running" || isThinking;
    const currentState = planExpandedState[planId] !== undefined
        ? planExpandedState[planId]
        : defaultState;
    planExpandedState[planId] = !currentState;
}

const BOTTOM_THRESHOLD_PX = 56;
function isNearBottom(element: HTMLElement) { return element.scrollHeight - element.scrollTop - element.clientHeight <= BOTTOM_THRESHOLD_PX; }
let scrollRafId: number | null = null;
function followLatest() {
    if (followLiveTail.value && props.isRunning && streamElement.value) {
        if (scrollRafId !== null) cancelAnimationFrame(scrollRafId);
        scrollRafId = requestAnimationFrame(() => {
            if (streamElement.value) {
                streamElement.value.scrollTop = streamElement.value.scrollHeight;
            }
            scrollRafId = null;
        });
    }
}
function onStreamScroll(event: Event) { followLiveTail.value = isNearBottom(event.currentTarget as HTMLElement); }
function resumeLiveFollow() { followLiveTail.value = true; followLatest(); }
function followThoughtTail() {
    const el = streamElement.value?.querySelector<HTMLElement>(".active-thought-content");
    if (el) {
        el.scrollTop = el.scrollHeight;
    }
}
watch(() => props.isThinking, (running, wasRunning) => { if (running && !wasRunning) activeThoughtStartedAt.value = Date.now(); if (!running && wasRunning) activeThoughtStartedAt.value = null; }, { immediate: true });

function formatMetricTokenCount(n: number): string {
    if (n >= 1000000) return (n / 1000000).toFixed(1) + 'M';
    if (n >= 1000) return (n / 1000).toFixed(1) + 'k';
    return String(n);
}

function getStepMetricsPill(step: AgentStepItem): string {
    const m = step.metrics;
    if (!m && !step.durationMs) return '';
    const parts: string[] = [];
    if (m?.completionTokens) {
        parts.push(`${formatMetricTokenCount(m.completionTokens)} tok`);
    }
    if (m?.totalDurationMs) {
        parts.push(m.totalDurationMs < 1000 ? `${m.totalDurationMs}ms` : `${(m.totalDurationMs / 1000).toFixed(1)}s`);
    } else if (step.durationMs) {
        parts.push(step.durationMs < 1000 ? `${step.durationMs}ms` : `${(step.durationMs / 1000).toFixed(1)}s`);
    }
    return parts.join(' · ');
}

function getStepMetricsTooltip(step: AgentStepItem): string {
    const m = step.metrics;
    if (!m) return '';
    const lines: string[] = [];
    if (m.promptTokens) lines.push(`📥 输入上下文: ${m.promptTokens.toLocaleString()} tok (缓存命中: ${m.cacheHitRate || 0}%)`);
    if (m.completionTokens) lines.push(`📤 输出生成: ${m.completionTokens.toLocaleString()} tok`);
    if (m.thinkingDurationMs) lines.push(`🧠 思考耗时: ${(m.thinkingDurationMs / 1000).toFixed(1)}s`);
    if (m.toolDurationMs) lines.push(`⚡ 工具耗时: ${(m.toolDurationMs / 1000).toFixed(1)}s`);
    if (m.tokensPerSecond) lines.push(`💨 吐字速率: ${m.tokensPerSecond.toFixed(1)} tok/s`);
    return lines.join('\n');
}

// --- 🌟 真正的实时流式 Markdown 增量渲染与光标 ---
const isStreamingSummary = computed(() => props.isRunning && Boolean(props.finalSummary));

watch(
    () => [props.steps.length, props.isRunning, props.finalSummary, props.thought],
    () => {
        if (props.isRunning && followLiveTail.value) {
            followThoughtTail();
            followLatest();
        }
    }
);

function planStatus(steps: AgentStepItem[]): AgentToolStatus {
    // 聚合每个逻辑 stepId 的最新尝试状态（若重试已成功，阶段整体应为成功）
    const latestStatusByStep = new Map<number, AgentToolStatus>();
    for (const step of steps) {
        latestStatusByStep.set(step.stepId, step.status);
    }
    const statuses = Array.from(latestStatusByStep.values());
    if (statuses.some((s) => s === "running" || s === "pending" || s === "challenged")) return "running";
    if (statuses.some((s) => s === "failed")) return "failed";
    return "succeeded";
}
function planEntries(steps: AgentStepItem[], prefix: string, active = false): PlanEntry[] {
    const groups = new Map<string, PlanEntry>();
    for (const step of steps.filter((item) => Boolean(item.description || item.tool || item.action))) {
        const key = step.planId || "plan-default";
        const existing = groups.get(key) || { id: `${prefix}-plan-${key}`, type: "plan" as const, timestamp: step.timestamp, title: step.planTitle || "执行任务", status: step.status, steps: [], isThinking: false };
        existing.steps.push(step);
        existing.timestamp = Math.min(existing.timestamp, step.timestamp);
        if (step.planTitle) existing.title = step.planTitle;
        groups.set(key, existing);
    }
    const plans = [...groups.values()].map((plan) => ({ ...plan, status: planStatus(plan.steps) }));
    if (active && (props.isThinking || Boolean(props.thought))) {
        const last = plans[plans.length - 1];
        if (last && (props.isThinking || !last.steps.some(s => s.thought))) {
            last.isThinking = true;
            last.thinkingTimestamp = activeThoughtStartedAt.value || Date.now();
        }
    }
    return plans;
}

function completionArtifacts(value: unknown): AgentArtifactInput[] {
    if (!Array.isArray(value)) return [];
    return value.filter((item) => {
        if (!item) return false;
        if (typeof item === "string" && (item.trim() === "[object Object]" || !item.trim())) return false;
        return true;
    });
}
function historyTimeline(): TimelineEntry[] {
    const timeline: TimelineEntry[] = [];
    // 过滤掉当前正在实时运行中的回合，防止 live 与 history 重复渲染双份用户消息
    const turnsToRender = props.historyTurns.filter((turn) => {
        if (props.isRunning) {
            if (turn.status === "running") return false;
            if (props.currentGoal && turn.goal.trim() === props.currentGoal.trim()) return false;
        }
        return true;
    });

    for (const turn of [...turnsToRender].sort((a, b) => a.startedAt - b.startedAt)) {
        timeline.push({ id: `history-user-${turn.id}`, type: "user", timestamp: turn.startedAt, content: turn.goal });
        // 过滤掉最终已由 completion 展示的助手消息，只保留人工干预记录
        const humanMessages = turn.messages.filter((m) => m.role === "user");
        const middle: TimelineEntry[] = [
            ...planEntries(turn.steps, `history-${turn.id}`),
            ...humanMessages.map((message) => ({
                id: `history-message-${turn.id}-${message.id}`,
                type: "message" as const,
                timestamp: message.timestamp,
                role: message.role,
                content: message.content,
                isHuman: true,
            })),
        ].sort((a, b) => a.timestamp - b.timestamp || a.id.localeCompare(b.id));
        timeline.push(...middle);
        if (turn.summary || turn.artifacts.length || ["succeeded", "failed", "cancelled"].includes(turn.status)) {
            const isCancelled = turn.status === "cancelled";
            const failed = turn.status === "failed" || isCancelled;
            const title = isCancelled
                ? "任务已手动停止"
                : (turn.status === "failed" ? (turn.summary || "任务未能顺利完成") : "任务已由 AI 标记完成");
            timeline.push({
                id: `history-completion-${turn.id}`,
                type: "completion",
                timestamp: turn.completedAt || turn.startedAt,
                title,
                summary: turn.summary,
                artifacts: completionArtifacts(turn.artifacts),
                failed,
            });
        }
    }
    return timeline;
}
function currentTimeline(): TimelineEntry[] {
    const timeline: TimelineEntry[] = [];
    // 只有当当前 goal 已经在 historyTurns 中被持久化（且状态已终结）且当前未在运行时才隐藏 live，防止重复渲染双份 completion
    const alreadyPersisted = !props.isRunning && props.historyTurns.length > 0 && props.historyTurns.some((turn) => {
        if (!props.currentGoal) return true;
        const cleanGoal = props.currentGoal.trim();
        const turnGoal = turn.goal.trim();
        return (cleanGoal === turnGoal || cleanGoal.includes(turnGoal) || turnGoal.includes(cleanGoal)) && ["succeeded", "failed", "cancelled"].includes(turn.status);
    });
    if (alreadyPersisted) return timeline;
    const plans = planEntries(props.steps, "live", props.isThinking);
    const firstTimestamp = plans[0]?.timestamp || Date.now();
    if (props.currentGoal) timeline.push({ id: "live-user-goal", type: "user", timestamp: firstTimestamp - 1, content: props.currentGoal });
    const middle: TimelineEntry[] = [
        ...plans,
        ...props.humanInterventions.map((item, index) => ({ id: `live-human-${item.timestamp}-${index}`, type: "message" as const, timestamp: item.timestamp, role: "user", content: item.message, isHuman: true })),
    ].sort((a, b) => a.timestamp - b.timestamp || a.id.localeCompare(b.id));
    timeline.push(...middle);
    if ((props.isThinking || Boolean(props.thought) || props.isRunning) && !plans.length) {
        timeline.push({ id: "active-thought", type: "active-thought", timestamp: activeThoughtStartedAt.value || Date.now() });
    }
    if (props.completionMessage || props.finalSummary || props.finalArtifacts.length) {
        timeline.push({ id: "live-completion", type: "completion", timestamp: Date.now(), title: props.completionMessage || (props.hasError ? "任务未能完成" : "任务已完成"), summary: props.finalSummary || props.completionMessage, artifacts: props.finalArtifacts, failed: props.hasError });
    }
    return timeline;
}
const timelineEntries = computed<TimelineEntry[]>(() => [...historyTimeline(), ...currentTimeline()]);
function statusLabel(status: AgentToolStatus) { return { pending: "等待", running: "执行中", succeeded: "完成", failed: "失败", challenged: "验证未闭合" }[status] || "未知"; }
function isSubAgentTool(tool?: string): boolean {
    const t = (tool || "").toLowerCase();
    return t.includes("child_reviewer") || t.includes("sub_agent");
}
function isWaitJobsTool(tool?: string): boolean {
    const t = (tool || "").toLowerCase();
    return t.includes("wait_jobs") || t.includes("wait_job");
}
function formatConversationTime(value: string) { const date = new Date(value); return Number.isNaN(date.getTime()) ? "" : date.toLocaleString("zh-CN", { hour: "2-digit", minute: "2-digit", month: "numeric", day: "numeric" }); }
watch(() => [timelineEntries.value.length, props.completionMessage, props.historyTurns.length], () => { followLatest(); }, { flush: "post" });
watch(() => props.isRunning, (running, wasRunning) => { if (running && !wasRunning) { followLiveTail.value = true; followLatest(); } if (!running && wasRunning) followLatest(); });
</script>
<style scoped>
.process-tree {
    position: relative;
    display: flex;
    min-height: 0;
    flex: 1;
    flex-direction: column;
    padding: 18px clamp(16px, 4vw, 48px) 24px;
    overflow-y: auto;
    overscroll-behavior: contain;
    background: #141416;
    color: #e4e4e7;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
}

/* 居中阅读流容器 (彻底解决左侧靠边偏离问题) */
.stream-centered-content {
    width: 100%;
    max-width: 820px;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: 12px;
}

/* 浮动跟随最新 */
.follow-latest {
    position: sticky;
    z-index: 4;
    top: 8px;
    align-self: flex-end;
    border: 1px solid rgba(255, 255, 255, 0.15);
    border-radius: 999px;
    padding: 4px 10px;
    color: #d9e7ff;
    cursor: pointer;
    background: rgba(19, 31, 49, 0.94);
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.3);
    font-size: 11px;
    font-weight: 600;
}
.follow-latest:hover {
    border-color: #7aa8e8;
    background: #203552;
}

/* 空状态 */
.empty-conversation {
    display: grid;
    min-height: 60vh;
    align-content: center;
    justify-items: center;
    text-align: center;
    padding: 40px 0;
}
.empty-logo {
    display: grid;
    width: 44px;
    height: 44px;
    place-items: center;
    border-radius: 12px;
    color: #fbbf24;
    background: linear-gradient(135deg, rgba(251, 191, 36, 0.16), rgba(124, 108, 246, 0.16));
    border: 1px solid rgba(251, 191, 36, 0.3);
    box-shadow: 0 4px 20px rgba(251, 191, 36, 0.15);
}
.empty-conversation h2 {
    margin: 12px 0 4px;
    color: #f1f1f1;
    font-size: 20px;
    letter-spacing: -0.03em;
}
.empty-conversation p {
    max-width: 440px;
    margin: 0;
    color: #8b8b8f;
    font-size: 12px;
    line-height: 1.6;
}

/* 对话历史 */
.conversation-history {
    display: flex;
    width: 100%;
    flex-direction: column;
    gap: 10px;
    margin-bottom: 4px;
}

.saved-message {
    position: relative;
    display: flex;
    flex-direction: column;
    box-sizing: border-box;
    width: 100%;
}

/* 👤 用户消息气泡 (深色精致圆角气泡) */
.saved-message.from-user, .user-message {
    align-self: flex-end;
    max-width: min(680px, 88%);
    padding: 9px 15px;
    border-radius: 16px 16px 4px 16px;
    background: #27272a;
    border: 1px solid rgba(255, 255, 255, 0.08);
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.2);
    color: #f4f4f5;
    margin: 2px 0;
    word-break: break-word;
    overflow-wrap: anywhere;
    box-sizing: border-box;
}

.user-text-content {
    margin: 0;
    color: #f4f4f5;
    font-size: 13px;
    line-height: 1.6;
    white-space: pre-wrap;
    word-break: break-word;
    overflow-wrap: anywhere;
}

/* 📎 附件缩略图与文件卡片托盘 */
.stream-attachments-tray {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-bottom: 8px;
    width: 100%;
}

/* 图片全屏预览弹窗 */
.image-preview-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.8);
    backdrop-filter: blur(8px);
    z-index: 99999;
    display: grid;
    place-items: center;
    padding: 30px;
}

.image-preview-dialog {
    max-width: 90vw;
    max-height: 90vh;
    background: #18181c;
    border: 1px solid rgba(255, 255, 255, 0.14);
    border-radius: 12px;
    box-shadow: 0 20px 50px rgba(0, 0, 0, 0.8);
    display: flex;
    flex-direction: column;
    overflow: hidden;
}

.dialog-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 16px;
    background: #1f1f24;
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
}

.dialog-title {
    font-size: 13px;
    font-weight: 600;
    color: #f4f4f5;
}

.btn-close-dialog {
    background: transparent;
    border: 0;
    color: #a1a1aa;
    font-size: 18px;
    cursor: pointer;
    padding: 0 4px;
    line-height: 1;
}

.btn-close-dialog:hover { color: #ffffff; }

.dialog-body {
    padding: 16px;
    display: grid;
    place-items: center;
    overflow: auto;
}

.full-preview-img {
    max-width: 100%;
    max-height: calc(85vh - 70px);
    object-fit: contain;
    border-radius: 6px;
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.5);
}

/* 📄 文档/PDF 在线富文本预览弹窗 */
.doc-preview-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.82);
    backdrop-filter: blur(10px);
    z-index: 99999;
    display: grid;
    place-items: center;
    padding: 24px;
}

.doc-preview-dialog {
    width: min(880px, 94vw);
    height: min(780px, 88vh);
    background: #18181c;
    border: 1px solid rgba(255, 255, 255, 0.14);
    border-radius: 12px;
    box-shadow: 0 20px 50px rgba(0, 0, 0, 0.8);
    display: flex;
    flex-direction: column;
    overflow: hidden;
}

.dialog-title-row {
    display: flex;
    align-items: center;
    gap: 10px;
}

.dialog-file-icon {
    font-size: 24px;
}

.dialog-file-meta h3 {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
    color: #f4f4f5;
}

.dialog-file-meta small {
    color: #71717a;
    font-size: 11px;
}

.dialog-actions {
    display: flex;
    align-items: center;
    gap: 8px;
}

.btn-copy-doc {
    background: rgba(255, 255, 255, 0.08);
    border: 1px solid rgba(255, 255, 255, 0.14);
    color: #e4e4e7;
    font-size: 12px;
    padding: 4px 10px;
    border-radius: 6px;
    cursor: pointer;
    transition: all 0.15s ease;
}

.btn-copy-doc:hover {
    background: rgba(255, 255, 255, 0.15);
    color: #fff;
}

.doc-dialog-body {
    flex: 1;
    overflow-y: auto;
    background: #121215;
    padding: 20px;
    display: block !important;
}

.doc-markdown-content {
    color: #e4e4e7;
    font-size: 13px;
    line-height: 1.6;
}

.doc-raw-preview {
    margin: 0;
    white-space: pre-wrap;
    word-break: break-word;
    font-family: monospace;
    font-size: 12px;
    color: #d4d4d8;
}

.doc-empty-hint {
    color: #71717a;
    font-size: 13px;
    text-align: center;
    padding: 40px 0;
}

/* 🤖 Agent 消息：彻底透明无框排版 */
.saved-message.from-agent {
    align-self: flex-start;
    width: 100%;
    max-width: 100%;
    padding: 4px 0 12px;
    border: none !important;
    border-radius: 0 !important;
    background: transparent !important;
    box-shadow: none !important;
}

.agent-msg-header {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 6px;
}

.agent-logo-pill {
    font-size: 12.5px;
    font-weight: 700;
    color: #f4f4f5;
    letter-spacing: -0.01em;
}

.msg-clock {
    font-size: 10px;
    color: #71717a;
}

.from-user .msg-clock {
    text-align: right;
    margin-top: 3px;
}

.intervention-indicator {
    display: block;
    font-size: 10px;
    font-weight: 700;
    color: var(--orange);
    margin-bottom: 3px;
}

/* ── 📝 Markdown 优雅排版 (标题/段落/列表/表格/代码) ── */
.markdown-content,
.summary-rich,
.agent-text-body {
    width: 100%;
    color: #e4e4e7;
    font-size: 13.5px;
    line-height: 1.78;
    letter-spacing: 0.003em;
    overflow-wrap: anywhere;
}

.markdown-content :deep(h1),
.markdown-content :deep(h2),
.markdown-content :deep(h3),
.markdown-content :deep(h4),
.markdown-content :deep(h5),
.markdown-content :deep(h6) {
    color: #f4f4f5;
    font-weight: 700;
    line-height: 1.35;
    letter-spacing: -0.015em;
    margin: 16px 0 8px;
}
.markdown-content :deep(h1:first-child),
.markdown-content :deep(h2:first-child),
.markdown-content :deep(h3:first-child) {
    margin-top: 2px;
}
.markdown-content :deep(h1) { font-size: 19px; }
.markdown-content :deep(h2) { font-size: 16.5px; }
.markdown-content :deep(h3) { font-size: 14.5px; }
.markdown-content :deep(h4) { font-size: 13.5px; }

.markdown-content :deep(p) {
    margin: 0 0 10px;
    color: inherit;
    font-size: inherit;
    line-height: inherit;
    white-space: normal;
}
.markdown-content :deep(p:last-child) {
    margin-bottom: 0;
}

.markdown-content :deep(ul),
.markdown-content :deep(ol) {
    margin: 6px 0 12px;
    padding-left: 20px;
}
.markdown-content :deep(li) {
    margin: 3px 0;
    line-height: 1.68;
}
.markdown-content :deep(li > ul),
.markdown-content :deep(li > ol) {
    margin: 2px 0 4px;
}

/* 引用块 */
.markdown-content :deep(blockquote) {
    margin: 10px 0;
    padding: 6px 14px;
    border-left: 3px solid var(--accent);
    background: rgba(255, 255, 255, 0.025);
    border-radius: 0 6px 6px 0;
    color: #a1a1aa;
}

/* 表格 (精细边框与悬停) */
.markdown-content :deep(table) {
    width: 100%;
    margin: 12px 0 16px;
    border-collapse: separate;
    border-spacing: 0;
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 8px;
    overflow: hidden;
    font-size: 12.5px;
}
.markdown-content :deep(th) {
    padding: 7px 12px;
    background: #1f2025;
    color: #f4f4f5;
    font-weight: 600;
    text-align: left;
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
    border-right: 1px solid rgba(255, 255, 255, 0.06);
}
.markdown-content :deep(th:last-child) {
    border-right: none;
}
.markdown-content :deep(td) {
    padding: 6px 12px;
    background: #161619;
    color: #d4d4d8;
    border-bottom: 1px solid rgba(255, 255, 255, 0.05);
    border-right: 1px solid rgba(255, 255, 255, 0.05);
}
.markdown-content :deep(td:last-child) {
    border-right: none;
}
.markdown-content :deep(tr:last-child td) {
    border-bottom: none;
}
.markdown-content :deep(tr:hover td) {
    background: #1e1e24;
}

/* 行内代码与代码块 */
.markdown-content :deep(code) {
    padding: 2px 5px;
    border-radius: 4px;
    color: #e5c07b;
    background: #1e1e23;
    font: 12px ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}
.markdown-content :deep(.code-block-wrapper) {
    margin: 12px 0 16px;
    border-radius: 8px;
    overflow: hidden;
    border: 1px solid rgba(255, 255, 255, 0.08);
    background: #18181c;
}
.markdown-content :deep(.code-block-header) {
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: 30px;
    padding: 0 12px;
    background: #202026;
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
    color: #8e8e93;
    font-size: 11px;
}
.markdown-content :deep(pre) {
    margin: 0;
    padding: 12px 14px;
    overflow: auto;
    background: transparent;
}
.markdown-content :deep(pre code) {
    padding: 0;
    color: #d9dde5;
    background: transparent;
    font-size: 12.5px;
    line-height: 1.6;
}

/* 链接 */
.markdown-content :deep(a) {
    color: #60a5fa;
    text-decoration: none;
}
.markdown-content :deep(a:hover) {
    text-decoration: underline;
}

/* ── ⚡ 紧凑分层执行树 ── */
.plan-node {
    width: 100%;
    margin: 3px 0;
    background: transparent !important;
    border: none !important;
}

/* 计划大标题行 (极紧凑，高 26px，支持点击展开/折叠) */
.plan-row {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 26px;
    padding: 3px 6px;
    border-radius: 6px;
    color: #f4f4f5;
    font-size: 12px;
    cursor: pointer;
    user-select: none;
    transition: background 0.15s ease, transform 0.1s ease;
}

.plan-row:hover {
    background: rgba(255, 255, 255, 0.05);
}

.plan-expand-arrow {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    color: #71717a;
    transition: transform 0.2s cubic-bezier(0.16, 1, 0.3, 1), color 0.15s ease;
    flex-shrink: 0;
}

.plan-expand-arrow.open {
    transform: rotate(90deg);
    color: #38bdf8;
}

.plan-collapsed-hint {
    margin-left: 6px;
    font-size: 10.5px;
    color: #71717a;
    font-weight: normal;
}

.plan-mark {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: #52525b;
    flex: 0 0 auto;
    margin: 0 2px;
    transition: all 0.2s ease;
}
.plan-mark i {
    display: none;
}

.plan-node.status-running > .plan-row .plan-mark,
.plan-node.is-active-plan > .plan-row .plan-mark {
    background: #38bdf8;
    box-shadow: 0 0 8px rgba(56, 189, 248, 0.85);
    animation: task-pulse 1.2s ease-in-out infinite;
}
.plan-node.status-running > .plan-row .row-status,
.plan-node.is-active-plan > .plan-row .row-status {
    color: #38bdf8;
    font-weight: 600;
}

.plan-node.status-succeeded:not(.is-active-plan) > .plan-row .plan-mark {
    background: #55d6a2;
    box-shadow: 0 0 4px rgba(85, 214, 162, 0.4);
    animation: none;
}
.plan-node.status-succeeded:not(.is-active-plan) > .plan-row .row-status {
    color: #55d6a2;
}

.plan-node.status-failed > .plan-row .plan-mark {
    background: #f87171;
    box-shadow: 0 0 6px rgba(248, 113, 113, 0.6);
    animation: none !important;
}
.plan-node.status-failed > .plan-row .row-status {
    color: #f87171;
    font-weight: 600;
}

.plan-title {
    font-weight: 600;
    color: #e4e4e7;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
}

.plan-title small {
    margin-right: 6px;
    color: var(--accent-light);
    font-size: 9px;
    font-weight: 700;
    text-transform: uppercase;
}

.model-tier {
    margin-left: 6px;
    color: #71717a;
    font-size: 9px;
    font-weight: 600;
}

.plan-children-wrapper {
    display: grid;
    grid-template-rows: 0fr;
    opacity: 0;
    transform: translateY(-4px);
    transition: grid-template-rows 0.35s cubic-bezier(0.16, 1, 0.3, 1), 
                opacity 0.28s cubic-bezier(0.16, 1, 0.3, 1), 
                transform 0.35s cubic-bezier(0.16, 1, 0.3, 1);
    will-change: grid-template-rows, opacity, transform;
}

.plan-children-wrapper.expanded {
    grid-template-rows: 1fr;
    opacity: 1;
    transform: translateY(0);
}

.plan-children-inner {
    overflow: hidden;
    min-height: 0;
}

.plan-children {
    margin-left: 7px;
    padding-left: 12px;
    border-left: 1px solid rgba(255, 255, 255, 0.08);
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-top: 2px;
}

.goal-node {
    margin: 0;
    display: flex;
    flex-direction: column;
    animation: step-appear 0.25s cubic-bezier(0.16, 1, 0.3, 1);
}

@keyframes step-appear {
    from { opacity: 0; transform: translateY(3px); }
    to { opacity: 1; transform: translateY(0); }
}

/* 所有交互按钮重置 */
button.goal-row,
button.child-row,
button.root-row {
    -webkit-appearance: none;
    appearance: none;
    background: transparent !important;
    border: none !important;
    outline: none !important;
    box-shadow: none !important;
    cursor: pointer;
    font: inherit;
    text-align: left;
}

/* 具体执行步骤行 (极紧凑，高 22px) */
.goal-row {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 22px;
    padding: 2px 6px;
    border-radius: 4px;
    color: #d4d4d8;
    font-size: 12px;
    width: 100%;
    transition: background 0.12s;
}

.goal-row:hover {
    background: rgba(255, 255, 255, 0.04) !important;
}

.goal-row.selected {
    background: rgba(108, 99, 255, 0.12) !important;
    color: #ffffff !important;
}

.tree-mark {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: #52525b;
    flex: 0 0 auto;
    margin: 0 2px;
    transition: all 0.2s ease;
}
.tree-mark i {
    display: none;
}

/* 🌟 执行完成：稳稳常亮绿色，静止不闪 */
.goal-node.status-succeeded > .goal-row .tree-mark {
    background: #55d6a2;
    box-shadow: 0 0 4px rgba(85, 214, 162, 0.5);
    animation: none;
}
.goal-node.status-succeeded > .goal-row .row-status {
    color: #55d6a2;
}

/* 🌟 仅当前正在运行的具体步骤：呼吸闪烁脉冲 */
.goal-node.status-running > .goal-row .tree-mark {
    background: #38bdf8;
    box-shadow: 0 0 8px rgba(56, 189, 248, 0.9);
    animation: task-pulse 1.2s ease-in-out infinite;
}
.goal-node.status-running > .goal-row .row-status {
    color: #38bdf8;
    font-weight: 600;
}
.goal-node.status-running > .goal-row .goal-label {
    color: #f4f4f5;
}

/* 🌟 失败/重试状态：若在运行中闪烁警示，若已停止则静态常亮 */
.goal-node.status-failed > .goal-row .tree-mark,
.goal-node.status-error > .goal-row .tree-mark {
    background: #f87171;
    box-shadow: 0 0 6px rgba(248, 113, 113, 0.6);
    animation: none !important;
}
.goal-node.status-failed > .goal-row .row-status,
.goal-node.status-error > .goal-row .row-status {
    color: #f87171;
    font-weight: 600;
}

/* 🌟 终态自检拦截（验证未闭合）：醒目警示橙黄 */
.goal-node.status-challenged > .goal-row .tree-mark {
    background: #e6a23c;
    box-shadow: 0 0 6px rgba(230, 162, 60, 0.6);
    animation: none !important;
}
.goal-node.status-challenged > .goal-row .row-status {
    color: #e6a23c;
    font-weight: 600;
}
.goal-node.status-challenged > .goal-row .goal-label {
    color: #fcd34d;
}

/* 🌟 等待中 */
.goal-node.status-pending > .goal-row .tree-mark {
    background: #52525b;
    animation: none;
}
.goal-node.status-pending > .goal-row .row-status {
    color: #71717a;
}

.goal-label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 500;
}

.step-metrics-badge {
    font-size: 10px;
    padding: 0 5px;
    border-radius: 3px;
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid rgba(255, 255, 255, 0.06);
    color: #a1a1aa;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    margin-left: auto;
    margin-right: 6px;
    flex-shrink: 0;
}

.row-status {
    margin-left: 0;
    font-size: 10px;
    color: #71717a;
    font-style: normal;
    flex-shrink: 0;
}

.goal-row time,
.plan-row time,
.child-row time {
    font-size: 9.5px;
    color: #52525b;
    font-family: ui-monospace, monospace;
    flex-shrink: 0;
    margin-left: 6px;
}

/* 子步骤与思考 (极紧凑，高 20px) */
.node-children {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 1px;
    margin-left: 6px;
    padding-left: 10px;
    border-left: 1px solid rgba(255, 255, 255, 0.06);
    margin-bottom: 2px;
}

.child-row {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 20px;
    padding: 1px 6px;
    border-radius: 3px;
    font-size: 11px;
    color: #8e8e93;
    width: 100%;
    transition: background 0.12s;
}

.child-row:hover {
    background: rgba(255, 255, 255, 0.03) !important;
    color: #d4d4d8 !important;
}

.child-row.selected {
    background: rgba(108, 99, 255, 0.1) !important;
    color: #ffffff !important;
}

.step-tool-emoji {
    font-size: 11.5px;
    line-height: 1;
    flex-shrink: 0;
    margin-right: 1px;
}

.child-mark {
    width: 3px;
    height: 3px;
    border-radius: 50%;
    background: #71717a;
    flex-shrink: 0;
}

.tool-tag {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding: 1px 6px;
    border-radius: 4px;
    font-size: 10px;
    font-weight: 600;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    flex-shrink: 0;
    line-height: 1.35;
    border: 1px solid rgba(255, 255, 255, 0.06);
}

.thought-tag {
    color: #e879f9;
    background: rgba(232, 121, 249, 0.12);
    border-color: rgba(232, 121, 249, 0.2);
}

.child-row span:not(.child-mark):not(.tool-tag) {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
}

.child-row em {
    color: #636368;
    font-size: 9.5px;
    font-style: normal;
    margin-left: auto;
    flex-shrink: 0;
}

/* 实时思考流 */
.active-thought-stream {
    margin: 2px 0 2px 6px;
    border-radius: 4px;
    cursor: pointer;
    background: rgba(56, 189, 248, 0.03);
    border: 1px dashed rgba(56, 189, 248, 0.35);
    animation: thought-border-glow 2s ease-in-out infinite;
}

@keyframes thought-border-glow {
    0%, 100% { border-color: rgba(56, 189, 248, 0.2); background: rgba(56, 189, 248, 0.02); }
    50% { border-color: rgba(56, 189, 248, 0.55); background: rgba(56, 189, 248, 0.05); }
}

.thought-pulse-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: #38bdf8;
    box-shadow: 0 0 8px rgba(56, 189, 248, 0.9);
    animation: task-pulse 1.2s ease-in-out infinite;
    flex: 0 0 auto;
    margin: 0 2px;
}
.thought-pulse-dot i {
    display: none;
}

.active-thought-stream header {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 2px 8px;
    font-size: 11px;
    color: #cfcfd4;
}

.active-thought-stream header b { color: var(--accent-light); }
.active-thought-stream header em { color: #38bdf8; font-size: 10px; font-style: normal; margin-left: auto; font-weight: 600; }
.active-thought-stream header time { color: #73737a; font-size: 10px; }

.active-thought-content {
    max-height: 90px;
    margin: 0;
    padding: 4px 8px 6px;
    overflow: auto;
    color: #a9a9af;
    font: 11px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace;
    white-space: pre-wrap;
}

.active-thought-initial {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px 8px;
    font-size: 11.5px;
    color: #93c5fd;
}

/* 交付物与总结 */
.final-response {
    width: 100%;
    max-width: 100%;
    padding: 4px 0 16px;
    border: 0;
    background: transparent;
}

.final-artifacts {
    display: flex;
    margin-top: 8px;
    gap: 4px;
    flex-direction: column;
}

.final-artifacts b { color: #bdbdc4; font-size: 10px; }
.final-artifacts code {
    color: #88888f;
    font: 10px ui-monospace, monospace;
    background: #1c1c20;
    padding: 2px 6px;
    border-radius: 4px;
}

.idle-live {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0;
    padding: 2px 4px;
    color: #85858b;
    font-size: 11px;
}

.idle-live i {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: #eee;
    animation: work-pulse 1s ease-in-out infinite;
}

@keyframes work-pulse {
    0%, 100% { opacity: 0.35; transform: scale(0.8); }
    50% { opacity: 1; transform: scale(1); }
}

@keyframes task-pulse {
    0%, 100% {
        transform: scale(0.8);
        opacity: 0.5;
        box-shadow: 0 0 3px rgba(56, 189, 248, 0.4);
    }
    50% {
        transform: scale(1.35);
        opacity: 1;
        box-shadow: 0 0 12px rgba(56, 189, 248, 1);
    }
}

@keyframes error-pulse {
    0%, 100% {
        transform: scale(0.8);
        opacity: 0.5;
        box-shadow: 0 0 3px rgba(248, 113, 113, 0.4);
    }
    50% {
        transform: scale(1.35);
        opacity: 1;
        box-shadow: 0 0 12px rgba(248, 113, 113, 1);
    }
}

.typing-cursor {
    display: inline-block;
    color: #fbbf24;
    font-size: 14px;
    font-weight: 700;
    margin-left: 2px;
    vertical-align: baseline;
    animation: typing-blink 0.7s infinite;
}

@keyframes typing-blink {
    0%, 100% { opacity: 1; }
    50% { opacity: 0; }
}

/* =========================================================================
   子智能体 (Sub-Agent) 与异步挂起 (Wait-Jobs) 专属胶囊与流光样式
   ========================================================================= */
.subagent-tag-badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 1px 7px;
    border-radius: 6px;
    font-size: 11px;
    font-weight: 600;
    color: #c084fc;
    background: rgba(168, 85, 247, 0.14);
    border: 1px solid rgba(168, 85, 247, 0.32);
    margin-right: 6px;
    vertical-align: middle;
}

.waitjobs-tag-badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 1px 7px;
    border-radius: 6px;
    font-size: 11px;
    font-weight: 600;
    color: #fbbf24;
    background: rgba(245, 158, 11, 0.14);
    border: 1px solid rgba(245, 158, 11, 0.32);
    margin-right: 6px;
    vertical-align: middle;
}

/* 子智能体节点微光卡片 */
.goal-node.is-subagent {
    border-left: 2px solid #a855f7;
    background: rgba(168, 85, 247, 0.03);
    border-radius: 6px;
}

.goal-node.is-subagent.status-running {
    animation: subagent-pulse-glow 2s infinite ease-in-out;
}

@keyframes subagent-pulse-glow {
    0%, 100% {
        background: rgba(168, 85, 247, 0.03);
        box-shadow: 0 0 0 1px rgba(168, 85, 247, 0.15);
    }
    50% {
        background: rgba(168, 85, 247, 0.08);
        box-shadow: 0 0 14px rgba(168, 85, 247, 0.3);
    }
}

/* 挂起节点微光卡片 */
.goal-node.is-waitjobs {
    border-left: 2px solid #f59e0b;
    background: rgba(245, 158, 11, 0.03);
    border-radius: 6px;
}

.goal-node.is-waitjobs.status-running {
    animation: waitjobs-pulse-glow 2.5s infinite ease-in-out;
}

@keyframes waitjobs-pulse-glow {
    0%, 100% {
        background: rgba(245, 158, 11, 0.03);
        box-shadow: 0 0 0 1px rgba(245, 158, 11, 0.15);
    }
    50% {
        background: rgba(245, 158, 11, 0.07);
        box-shadow: 0 0 12px rgba(245, 158, 11, 0.25);
    }
}

.subagent-action-text {
    color: #e9d5ff;
    font-weight: 500;
}

.waitjobs-action-text {
    color: #fef3c7;
    font-weight: 500;
}

.tool-child-subagent {
    border-radius: 6px;
    background: rgba(168, 85, 247, 0.06);
    border: 1px dashed rgba(168, 85, 247, 0.25);
}

.tool-child-wait {
    border-radius: 6px;
    background: rgba(245, 158, 11, 0.06);
    border: 1px dashed rgba(245, 158, 11, 0.25);
}

.subagent-inline-capsule {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin: 4px 0 6px 16px;
    padding: 6px 12px;
    border-radius: 8px;
    background: rgba(56, 189, 248, 0.06);
    border: 1px solid rgba(56, 189, 248, 0.25);
    cursor: pointer;
    transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1);
}

.subagent-inline-capsule:hover {
    background: rgba(56, 189, 248, 0.12);
    border-color: rgba(56, 189, 248, 0.45);
    transform: translateY(-1px);
    box-shadow: 0 4px 12px rgba(56, 189, 248, 0.15);
}

.capsule-left {
    display: flex;
    align-items: center;
    gap: 8px;
}

.capsule-pulse-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: #38bdf8;
    box-shadow: 0 0 8px #38bdf8;
}

.capsule-pulse-dot.is-running {
    animation: capsule-dot-pulse 1.2s infinite ease-in-out;
}

@keyframes capsule-dot-pulse {
    0%, 100% { transform: scale(0.9); opacity: 0.6; }
    50% { transform: scale(1.4); opacity: 1; box-shadow: 0 0 12px #38bdf8; }
}

.capsule-text {
    font-size: 11.5px;
    font-weight: 600;
    color: #e0f2fe;
}

.capsule-action {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: 11px;
    color: #38bdf8;
    font-weight: 600;
}
</style>

