<template>
    <div class="agent-console">
        <aside class="console-controls">
            <header class="console-brand">
                <span class="eyebrow">AGENT CONSOLE</span>
                <h1>任务执行台</h1>
                <p>会话 {{ panelId.slice(-6) }}</p>
            </header>

            <section class="control-section">
                <div class="section-heading">
                    <span>模型路由</span>
                    <small>运行期间锁定</small>
                </div>
                <label class="field-label" for="model-pro">推理模型</label>
                <select id="model-pro" v-model="modelRouting.pro" :disabled="isRunning">
                    <option value="" disabled>选择复杂推理模型</option>
                    <optgroup v-for="group in groupedModels" :key="group.platform" :label="group.platform">
                        <option v-for="model in group.models" :key="model.id" :value="model.id">
                            {{ model.display_name || model.name }}
                        </option>
                    </optgroup>
                </select>

                <label class="field-label" for="model-flash">执行模型</label>
                <select id="model-flash" v-model="modelRouting.flash" :disabled="isRunning">
                    <option value="" disabled>选择快速执行模型</option>
                    <optgroup v-for="group in groupedModels" :key="group.platform" :label="group.platform">
                        <option v-for="model in group.models" :key="model.id" :value="model.id">
                            {{ model.display_name || model.name }}
                        </option>
                    </optgroup>
                </select>

                <label class="field-label" for="model-vision">视觉模型</label>
                <select id="model-vision" v-model="modelRouting.vision" :disabled="isRunning">
                    <option value="" disabled>选择多模态模型</option>
                    <optgroup v-for="group in groupedModels" :key="group.platform" :label="group.platform">
                        <option v-for="model in group.models" :key="model.id" :value="model.id">
                            {{ model.display_name || model.name }}
                        </option>
                    </optgroup>
                </select>
            </section>

            <section class="control-section compact-section">
                <label class="toggle-row" for="context-log">
                    <span>
                        <strong>记录完整上下文</strong>
                        <small>用于任务复盘与调试</small>
                    </span>
                    <input id="context-log" v-model="logContext" :disabled="isRunning" type="checkbox" />
                </label>
            </section>

            <section class="control-section">
                <div class="section-heading">
                    <span>任务预设</span>
                    <small>一键填充</small>
                </div>
                <div class="preset-grid">
                    <button
                        v-for="preset in presets"
                        :key="preset.label"
                        class="preset-button"
                        :class="{ active: goalInput === preset.goal }"
                        :disabled="isRunning"
                        type="button"
                        @click="goalInput = preset.goal"
                    >
                        {{ preset.label }}
                    </button>
                </div>
            </section>

            <section class="control-section goal-section">
                <label class="field-label" for="agent-goal">任务目标</label>
                <textarea
                    id="agent-goal"
                    v-model="goalInput"
                    :disabled="isRunning"
                    placeholder="描述希望 Agent 完成的任务。建议明确目标、范围和期望结果。"
                    rows="7"
                />
            </section>

            <footer class="control-actions">
                <button
                    class="action-button primary"
                    type="button"
                    :disabled="!canStart"
                    @click="runAgent"
                >
                    {{ isRunning ? "任务运行中" : "开始执行" }}
                </button>
                <button
                    v-if="isRunning"
                    class="action-button danger"
                    type="button"
                    :disabled="stopRequested"
                    @click="requestStop"
                >
                    {{ stopRequested ? "停止请求已发送" : "请求停止" }}
                </button>
                <button
                    v-else-if="timeline.length > 0"
                    class="action-button secondary"
                    type="button"
                    @click="clearAll"
                >
                    清空本次记录
                </button>
                <p class="control-hint">
                    {{ isRunning ? "停止请求将作为高优先级人工指令发送给 Agent。" : "开始后将在右侧持续显示状态和工具活动。" }}
                </p>
            </footer>
        </aside>

        <main class="console-workspace">
            <section class="run-status" :class="`state-${runState}`">
                <div class="status-main">
                    <div class="state-marker" aria-hidden="true"><span></span></div>
                    <div>
                        <span class="eyebrow">当前运行状态</span>
                        <div class="state-line">
                            <h2>{{ stateMeta.label }}</h2>
                            <span class="state-chip">{{ stateMeta.category }}</span>
                        </div>
                        <p>{{ statusDescription }}</p>
                    </div>
                </div>
                <dl class="run-metadata">
                    <div>
                        <dt>当前任务</dt>
                        <dd :title="currentGoal || goalInput">{{ activeGoalLabel }}</dd>
                    </div>
                    <div>
                        <dt>已运行</dt>
                        <dd class="metric-value">{{ elapsedLabel }}</dd>
                    </div>
                    <div>
                        <dt>活动记录</dt>
                        <dd class="metric-value">{{ timeline.length }}</dd>
                    </div>
                </dl>
            </section>

            <section v-if="isRunning" class="live-metrics">
                <span><strong>{{ toolCount }}</strong> 工具调用</span>
                <span><strong>{{ successCount }}</strong> 已完成</span>
                <span v-if="failureCount"><strong>{{ failureCount }}</strong> 异常</span>
                <span v-if="tokenUsage"><strong>{{ formatNumber(tokenUsage.total_tokens) }}</strong> Token</span>
                <span v-if="tokenUsage" class="context-metric">上下文 {{ tokenUsage.usage_percent.toFixed(1) }}%</span>
            </section>

            <section v-if="isRunning || isWaitingForHuman" class="human-input-card" :class="{ waiting: isWaitingForHuman }">
                <div class="human-input-header">
                    <div>
                        <span class="eyebrow">HUMAN INPUT</span>
                        <h2>{{ isWaitingForHuman ? "需要人工输入后才能继续" : "向运行中的 Agent 发送补充说明" }}</h2>
                        <p>{{ isWaitingForHuman ? pauseReason || "请提供确认、选择或补充信息。" : "该输入会作为高优先级上下文发送，不会覆盖已有目标。" }}</p>
                    </div>
                    <span v-if="isWaitingForHuman" class="waiting-badge">WaitingForHuman</span>
                </div>
                <div class="human-input-form">
                    <textarea
                        v-model="humanInput"
                        :placeholder="isWaitingForHuman ? '输入确认、选择结果或补充信息…' : '例如：优先处理当前错误，或将范围限制为指定目录…'"
                        rows="3"
                        @keydown.ctrl.enter.prevent="sendHumanInput"
                        @keydown.meta.enter.prevent="sendHumanInput"
                    />
                    <button class="continue-button" type="button" :disabled="!humanInput.trim() || isSendingHuman" @click="sendHumanInput">
                        {{ isSendingHuman ? "正在发送" : isWaitingForHuman ? "继续执行" : "发送说明" }}
                    </button>
                </div>
            </section>

            <section class="content-grid">
                <article class="timeline-panel">
                    <header class="panel-header">
                        <div>
                            <span class="eyebrow">LIVE TIMELINE</span>
                            <h2>运行时间线</h2>
                        </div>
                        <span class="panel-count">{{ timeline.length }} 条</span>
                    </header>

                    <div v-if="timeline.length === 0" class="timeline-empty">
                        <div class="empty-grid" aria-hidden="true"></div>
                        <h3>等待任务启动</h3>
                        <p>配置模型与目标后，规划、调用、恢复和最终结果会按时间顺序显示在这里。</p>
                    </div>

                    <ol v-else ref="timelineElement" class="timeline-list">
                        <li v-for="item in timeline" :key="item.id" class="timeline-entry" :class="[`kind-${item.kind}`, item.status ? `item-${item.status}` : '']">
                            <div class="timeline-rail" aria-hidden="true"><span></span></div>
                            <div class="timeline-content">
                                <div v-if="item.kind !== 'tool'" class="event-card">
                                    <div class="event-header">
                                        <div>
                                            <span class="event-kind">{{ kindLabel(item.kind) }}</span>
                                            <h3>{{ item.title }}</h3>
                                        </div>
                                        <time :datetime="new Date(item.timestamp).toISOString()">{{ formatClock(item.timestamp) }}</time>
                                    </div>
                                    <p v-if="item.detail">{{ item.detail }}</p>
                                </div>

                                <article v-else-if="item.tool" class="tool-card" :class="`tool-${item.tool.status}`">
                                    <header class="tool-header">
                                        <div class="tool-title-wrap">
                                            <span class="event-kind">TOOL</span>
                                            <h3>{{ item.tool.name }}</h3>
                                            <span class="tool-action">{{ item.tool.action }}</span>
                                        </div>
                                        <div class="tool-header-meta">
                                            <span class="source-tag" :class="item.tool.source === 'MCP' ? 'mcp' : 'internal'">{{ item.tool.source }}</span>
                                            <span class="tool-status" :class="item.tool.status">{{ toolStatusLabel(item.tool.status) }}</span>
                                        </div>
                                    </header>
                                    <div class="tool-summary-row">
                                        <p>{{ item.detail || "等待工具返回结果" }}</p>
                                        <span v-if="item.tool.durationMs !== undefined" class="duration">{{ formatMilliseconds(item.tool.durationMs) }}</span>
                                    </div>
                                    <div class="tool-details">
                                        <button class="detail-toggle" type="button" @click.stop="toggleToolDetail(item.id, 'params')">
                                            <span>参数{{ item.tool.params ? "摘要" : "" }}</span>
                                            <span>{{ isToolDetailExpanded(item.id, 'params') ? "收起" : "展开" }}</span>
                                        </button>
                                        <pre v-if="isToolDetailExpanded(item.id, 'params')">{{ formatPayload(item.tool.rawParams ?? item.tool.params) }}</pre>
                                        <button class="detail-toggle" type="button" @click.stop="toggleToolDetail(item.id, 'result')">
                                            <span>结果{{ item.tool.result ? "摘要" : "" }}</span>
                                            <span>{{ isToolDetailExpanded(item.id, 'result') ? "收起" : "展开" }}</span>
                                        </button>
                                        <pre v-if="isToolDetailExpanded(item.id, 'result')">{{ formatPayload(item.tool.rawResult ?? item.tool.result) }}</pre>
                                    </div>
                                </article>
                            </div>
                        </li>
                    </ol>
                </article>

                <aside class="resource-panel">
                    <header class="panel-header">
                        <div>
                            <span class="eyebrow">TASK DATA</span>
                            <h2>任务资源</h2>
                        </div>
                        <div class="resource-header-actions">
                            <button class="resource-refresh" type="button" :disabled="isResourcesLoading" @click="refreshResources">
                                {{ isResourcesLoading ? "刷新中" : "刷新" }}
                            </button>
                            <span class="panel-count">{{ resources.length }}</span>
                        </div>
                    </header>
                    <p class="resource-intro">资源目录来自 session 的持久化任务数据区；只在需要时分页读取记录，不会把大数据塞进模型上下文。</p>
                    <p v-if="resourceLoadError" class="resource-load-error">{{ resourceLoadError }}</p>
                    <div v-if="resources.length === 0" class="resource-empty">
                        <span>尚未收到资源上下文</span>
                        <small>资源卡会在 Agent 提供数据、游标或任务上下文时自动出现。</small>
                    </div>
                    <div v-else class="resource-list">
                        <article v-for="resource in resources" :key="resource.resourceId" class="resource-card">
                            <header>
                                <span class="resource-kind">{{ resource.kind }}</span>
                                <span class="resource-status" :class="resource.status">{{ resource.status }}</span>
                            </header>
                            <h3>{{ resource.label }}</h3>
                            <dl>
                                <div><dt>resource_id</dt><dd :title="resource.resourceId">{{ resource.resourceId }}</dd></div>
                                <div><dt>row_count</dt><dd>{{ resource.rowCount ?? "—" }}</dd></div>
                                <div><dt>cursor</dt><dd :title="resource.cursor || ''">{{ resource.cursor || "—" }}</dd></div>
                            </dl>
                            <p>{{ resource.summary || "暂无资源摘要。" }}</p>
                            <button class="resource-preview-button" type="button" @click="openResourcePreview(resource)">查看记录</button>
                        </article>
                    </div>
                </aside>
            </section>

            <div v-if="selectedResource" class="resource-preview-backdrop" @click.self="closeResourcePreview">
                <section class="resource-preview-dialog" role="dialog" aria-modal="true" :aria-label="`${selectedResource.label} 的任务记录`">
                    <header class="resource-preview-header">
                        <div>
                            <span class="eyebrow">PERSISTED RECORDS</span>
                            <h2>{{ selectedResource.label }}</h2>
                            <p>{{ selectedResource.resourceId }}</p>
                        </div>
                        <button class="resource-preview-close" type="button" @click="closeResourcePreview">关闭</button>
                    </header>
                    <div v-if="isRecordsLoading" class="resource-preview-loading">正在读取持久化记录…</div>
                    <p v-else-if="recordLoadError" class="resource-load-error">{{ recordLoadError }}</p>
                    <div v-else-if="recordPage.records.length === 0" class="resource-preview-empty">当前资源还没有可展示的记录。</div>
                    <ol v-else class="resource-record-list">
                        <li v-for="record in recordPage.records" :key="record.key">
                            <header>
                                <strong>{{ record.key }}</strong>
                                <time>{{ record.updatedAt || "—" }}</time>
                            </header>
                            <pre>{{ formatPayload(record.data) }}</pre>
                        </li>
                    </ol>
                    <footer class="resource-pagination">
                        <span>{{ recordRangeLabel }}</span>
                        <div>
                            <button type="button" :disabled="isRecordsLoading || recordPage.offset === 0" @click="loadPreviousRecordPage">上一页</button>
                            <button type="button" :disabled="isRecordsLoading || !recordPage.hasMore" @click="loadNextRecordPage">下一页</button>
                        </div>
                    </footer>
                </section>
            </div>
            <section v-if="finalSummary || completionMessage" class="final-result" :class="hasError ? 'failed' : 'succeeded'">
                <div class="final-result-label">{{ hasError ? "执行结束：存在异常" : "执行结束：已返回结果" }}</div>
                <h2>{{ completionMessage || (hasError ? "任务未能完成" : "任务已完成") }}</h2>
                <p v-if="finalSummary">{{ finalSummary }}</p>
                <div v-if="finalArtifacts.length" class="artifact-list">
                    <span>产出文件</span>
                    <code v-for="artifact in finalArtifacts" :key="artifact">{{ artifact }}</code>
                </div>
            </section>
        </main>
    </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

interface ModelOption {
    id: string;
    name: string;
    display_name: string;
    platform_id: string;
    platform_name: string;
}

type RunState =
    | "idle"
    | "preparing"
    | "planning"
    | "executing"
    | "waiting_for_human"
    | "recovering"
    | "finalizing"
    | "succeeded"
    | "failed"
    | "cancelled";
type TimelineKind = "system" | "planning" | "tool" | "success" | "error" | "human" | "recovery";
type ToolStatus = "running" | "succeeded" | "failed" | "pending";
type DetailSection = "params" | "result";

interface TokenUsageInfo {
    prompt_tokens: number;
    completion_tokens: number;
    total_tokens: number;
    context_window: number;
    usage_percent: number;
}

interface ToolCall {
    name: string;
    action: string;
    source: "内部" | "MCP";
    status: ToolStatus;
    durationMs?: number;
    params?: unknown;
    result?: unknown;
    rawParams?: unknown;
    rawResult?: unknown;
}

interface TimelineItem {
    id: string;
    kind: TimelineKind;
    title: string;
    detail?: string;
    timestamp: number;
    status?: "running" | "succeeded" | "failed";
    tool?: ToolCall;
}

interface TaskResource {
    resourceId: string;
    kind: string;
    label: string;
    rowCount?: number | string;
    status: string;
    summary?: string;
    cursor?: string;
}
interface TaskRecordPreview {
    key: string;
    data: unknown;
    updatedAt: string;
}
interface TaskRecordPage {
    resourceId: string;
    records: TaskRecordPreview[];
    offset: number;
    limit: number;
    total: number;
    hasMore: boolean;
}

const props = defineProps<{
    panelId: string;
    allModels: ModelOption[];
}>();

const stateCatalog: Record<RunState, { label: string; category: string; description: string }> = {
    idle: { label: "Idle", category: "就绪", description: "等待新的任务目标。" },
    preparing: { label: "Preparing", category: "准备", description: "正在初始化会话、模型路由和运行上下文。" },
    planning: { label: "Planning", category: "规划", description: "正在分析目标并生成下一步行动。" },
    executing: { label: "Executing", category: "执行", description: "正在调用工具并处理执行结果。" },
    waiting_for_human: { label: "WaitingForHuman", category: "等待输入", description: "Agent 暂停执行，等待人工确认或补充信息。" },
    recovering: { label: "Recovering", category: "恢复", description: "正在处理异常、重试或恢复任务状态。" },
    finalizing: { label: "Finalizing", category: "收尾", description: "正在整理结果、资源和最终任务状态。" },
    succeeded: { label: "Succeeded", category: "完成", description: "任务已成功结束。" },
    failed: { label: "Failed", category: "失败", description: "任务因错误而结束。" },
    cancelled: { label: "Cancelled", category: "已取消", description: "任务已停止。" },
};

const presets = [
    { label: "创建笔记", goal: "在桌面创建一个名为 notes.txt 的文件，写入当前日期和时间，然后显示文件内容。" },
    { label: "查看桌面", goal: "列出桌面上所有文件和文件夹，并统计数量。" },
    { label: "磁盘信息", goal: "查看当前磁盘使用情况，以及 CPU 和内存占用信息。" },
    { label: "整理下载", goal: "列出下载文件夹中最近修改的 5 个文件，以及其大小。" },
    { label: "系统信息", goal: "获取系统版本、主机名、当前用户名和 IP 地址。" },
];

const sessionId = props.panelId;
const modelRouting = ref({ pro: "", flash: "", vision: "" });
const logContext = ref(false);
const goalInput = ref("");
const currentGoal = ref("");
const runState = ref<RunState>("idle");
const statusDescription = ref(stateCatalog.idle.description);
const isRunning = ref(false);
const pauseReason = ref("");
const humanInput = ref("");
const isSendingHuman = ref(false);
const stopRequested = ref(false);
const elapsedSeconds = ref(0);
const runStartedAt = ref<number | null>(null);
const timeline = ref<TimelineItem[]>([]);
const resources = ref<TaskResource[]>([]);
const isResourcesLoading = ref(false);
const resourceLoadError = ref("");
const selectedResource = ref<TaskResource | null>(null);
const isRecordsLoading = ref(false);
const recordLoadError = ref("");
const recordPage = ref<TaskRecordPage>({ resourceId: "", records: [], offset: 0, limit: 20, total: 0, hasMore: false });
const tokenUsage = ref<TokenUsageInfo | null>(null);
const completionMessage = ref("");
const finishOutput = ref<Record<string, unknown> | null>(null);
const hasError = ref(false);
const expandedToolDetails = ref(new Set<string>());
const timelineElement = ref<HTMLOListElement>();

let elapsedTimer: ReturnType<typeof setInterval> | null = null;
let unlisteners: UnlistenFn[] = [];
let timelineSequence = 0;
let resourceRefreshTimer: ReturnType<typeof setTimeout> | null = null;
const RESOURCE_PAGE_SIZE = 20;
const activeToolItems = new Map<string, string>();

const groupedModels = computed(() => {
    const groups = new Map<string, ModelOption[]>();
    for (const model of props.allModels) {
        const platform = model.platform_name || "未分类平台";
        groups.set(platform, [...(groups.get(platform) || []), model]);
    }
    return [...groups.entries()].map(([platform, models]) => ({ platform, models }));
});

const stateMeta = computed(() => stateCatalog[runState.value]);
const activeGoalLabel = computed(() => shortText(currentGoal.value || goalInput.value || "尚未启动任务", 96));
const elapsedLabel = computed(() => formatDuration(elapsedSeconds.value));
const recordRangeLabel = computed(() => {
    if (recordPage.value.total === 0) return "0 条记录";
    const start = recordPage.value.offset + 1;
    const end = recordPage.value.offset + recordPage.value.records.length;
    return `第 ${start}–${end} 条，共 ${recordPage.value.total} 条`;
});
const isWaitingForHuman = computed(() => runState.value === "waiting_for_human");
const canStart = computed(() => Boolean(
    !isRunning.value
        && goalInput.value.trim()
        && modelRouting.value.pro
        && modelRouting.value.flash
        && modelRouting.value.vision,
));
const toolCount = computed(() => timeline.value.filter((item) => item.kind === "tool").length);
const successCount = computed(() => timeline.value.filter((item) => item.status === "succeeded").length);
const failureCount = computed(() => timeline.value.filter((item) => item.status === "failed").length);
const finalSummary = computed(() => {
    const payload = finishOutput.value;
    if (!payload) return "";
    const text = payload.details || payload.summary || payload.result_table || payload.output || "";
    return shortText(toText(text), 4000);
});
const finalArtifacts = computed(() => {
    const artifacts = finishOutput.value?.artifacts;
    return Array.isArray(artifacts) ? artifacts.map((item) => toText(item)).filter(Boolean).slice(0, 12) : [];
});

function applyDefaultModels() {
    const firstModel = props.allModels[0]?.id;
    if (!firstModel) return;
    if (!modelRouting.value.pro) modelRouting.value.pro = firstModel;
    if (!modelRouting.value.flash) modelRouting.value.flash = firstModel;
    if (!modelRouting.value.vision) modelRouting.value.vision = firstModel;
}

watch(() => props.allModels, applyDefaultModels, { immediate: true });

function setRunState(nextState: RunState, description?: string) {
    runState.value = nextState;
    statusDescription.value = description || stateCatalog[nextState].description;
    const terminal = ["succeeded", "failed", "cancelled"].includes(nextState);
    if (terminal) {
        isRunning.value = false;
        stopElapsedTimer();
    }
}

function normalizeState(value: unknown): RunState {
    const normalized = String(value || "").replace(/[\s-]/g, "_").replace(/([a-z])([A-Z])/g, "$1_$2").toLowerCase();
    const aliases: Record<string, RunState> = {
        idle: "idle",
        preparing: "preparing",
        starting: "preparing",
        planning: "planning",
        executing: "executing",
        waiting_for_human: "waiting_for_human",
        waitingforhuman: "waiting_for_human",
        awaiting_human: "waiting_for_human",
        recovering: "recovering",
        rescuing: "recovering",
        finalizing: "finalizing",
        succeeded: "succeeded",
        success: "succeeded",
        failed: "failed",
        cancelled: "cancelled",
        canceled: "cancelled",
    };
    return aliases[normalized] || "executing";
}

function startElapsedTimer() {
    stopElapsedTimer();
    elapsedTimer = setInterval(() => {
        if (runStartedAt.value) elapsedSeconds.value = Math.max(0, Math.floor((Date.now() - runStartedAt.value) / 1000));
    }, 1000);
}

function stopElapsedTimer() {
    if (elapsedTimer) clearInterval(elapsedTimer);
    elapsedTimer = null;
}

function addTimeline(item: Omit<TimelineItem, "id" | "timestamp"> & Partial<Pick<TimelineItem, "id" | "timestamp">>) {
    const next: TimelineItem = {
        id: item.id || `timeline-${Date.now()}-${timelineSequence++}`,
        timestamp: item.timestamp || Date.now(),
        kind: item.kind,
        title: item.title,
        detail: item.detail,
        status: item.status,
        tool: item.tool,
    };
    timeline.value.push(next);
    if (timeline.value.length > 160) timeline.value.splice(0, timeline.value.length - 160);
    nextTick(() => timelineElement.value?.scrollTo({ top: timelineElement.value.scrollHeight, behavior: "smooth" }));
    return next.id;
}

function updateTimeline(id: string, patch: Partial<TimelineItem>) {
    const index = timeline.value.findIndex((item) => item.id === id);
    if (index < 0) return;
    const previous = timeline.value[index];
    timeline.value[index] = { ...previous, ...patch, tool: patch.tool ? { ...previous.tool, ...patch.tool } : previous.tool };
}

function upsertTool(key: string, tool: ToolCall, detail: string, timestamp?: number) {
    const existingId = activeToolItems.get(key);
    if (existingId) {
        updateTimeline(existingId, { tool, detail, status: tool.status === "succeeded" ? "succeeded" : tool.status === "failed" ? "failed" : "running", timestamp: timestamp || Date.now() });
        return existingId;
    }
    const id = addTimeline({
        kind: "tool",
        title: tool.name,
        detail,
        tool,
        status: tool.status === "succeeded" ? "succeeded" : tool.status === "failed" ? "failed" : "running",
        timestamp,
    });
    activeToolItems.set(key, id);
    return id;
}

function handleRunState(payload: Record<string, unknown>) {
    const state = normalizeState(payload.state);
    const message = toText(payload.message || payload.description);
    setRunState(state, message || undefined);
    if (state === "waiting_for_human") pauseReason.value = message || stateCatalog.waiting_for_human.description;
    if (state !== "waiting_for_human") pauseReason.value = "";
    if (state === "failed" || state === "cancelled") hasError.value = state === "failed";
    if (message || state !== "executing") {
        addTimeline({
            kind: state === "recovering" ? "recovery" : state === "failed" || state === "cancelled" ? "error" : state === "succeeded" ? "success" : state === "waiting_for_human" ? "human" : "system",
            title: stateMetaFor(state),
            detail: message || stateCatalog[state].description,
            status: state === "succeeded" ? "succeeded" : state === "failed" || state === "cancelled" ? "failed" : undefined,
            timestamp: toTimestamp(payload.timestamp),
        });
    }
}

function handleProgress(payload: Record<string, unknown>) {
    const type = String(payload.type || "").toLowerCase();
    const step = asRecord(payload.step);
    const stepId = payload.step_id ?? step.id;
    const toolName = toText(step.tool || payload.tool || "内部工具") || "内部工具";
    const action = toText(step.description || payload.description || payload.action || "执行操作") || "执行操作";
    const key = `legacy:${String(stepId ?? `${toolName}:${action}`)}`;

    if (type === "planning") {
        setRunState("planning", toText(payload.message) || undefined);
        addTimeline({ kind: "planning", title: "正在规划", detail: toText(payload.message) || stateCatalog.planning.description, timestamp: toTimestamp(payload.timestamp) });
        return;
    }
    if (type === "plan") {
        setRunState("executing");
        const plannedSteps = Array.isArray(payload.steps) ? payload.steps.length : 0;
        addTimeline({ kind: "planning", title: "计划已生成", detail: plannedSteps ? `已生成 ${plannedSteps} 个执行步骤。` : "Agent 已生成执行计划。", timestamp: toTimestamp(payload.timestamp) });
        return;
    }
    if (type === "step_new") {
        addTimeline({ kind: "planning", title: "新增执行步骤", detail: action, timestamp: toTimestamp(payload.timestamp) });
        return;
    }
    if (type === "step_start") {
        setRunState("executing");
        upsertTool(key, {
            name: toolName,
            action,
            source: sourceFrom(step.route || payload.route, toolName),
            status: "running",
            params: step.command || payload.params_summary || payload.params,
            rawParams: step.command || payload.params,
        }, shortText(toText(step.thought || payload.message || action), 280), toTimestamp(payload.timestamp));
        return;
    }
    if (type === "step_done" || type === "step_error") {
        const succeeded = type === "step_done";
        const existing = timeline.value.find((item) => item.id === activeToolItems.get(key))?.tool;
        upsertTool(key, {
            name: existing?.name || toolName,
            action: existing?.action || action,
            source: existing?.source || sourceFrom(step.route || payload.route, toolName),
            status: succeeded ? "succeeded" : "failed",
            params: existing?.params || step.command || payload.params_summary,
            rawParams: existing?.rawParams || step.command || payload.params,
            result: payload.output || step.output || payload.result_summary,
            rawResult: payload.output || step.output || payload.result,
            durationMs: numberOrUndefined(payload.duration_ms),
        }, shortText(toText(payload.output || step.output || (succeeded ? "工具已完成。" : "工具执行失败。")), 280), toTimestamp(payload.timestamp));
        return;
    }
    if (type === "agent_paused") {
        const reason = toText(payload.message) || stateCatalog.waiting_for_human.description;
        pauseReason.value = reason;
        setRunState("waiting_for_human", reason);
        addTimeline({ kind: "human", title: "等待人工输入", detail: reason, timestamp: toTimestamp(payload.timestamp) });
        return;
    }
    if (type === "thinking") {
        setRunState("planning", toText(payload.message) || undefined);
        if (payload.done) addTimeline({ kind: "planning", title: "规划分析已完成", detail: shortText(toText(payload.content), 280), timestamp: toTimestamp(payload.timestamp) });
        return;
    }
    if (type === "token_usage") {
        tokenUsage.value = {
            prompt_tokens: Number(payload.prompt_tokens || 0),
            completion_tokens: Number(payload.completion_tokens || 0),
            total_tokens: Number(payload.total_tokens || 0),
            context_window: Number(payload.context_window || 0),
            usage_percent: Number(payload.usage_percent || 0),
        };
        return;
    }
    if (type === "complete") {
        completeRun(payload, Boolean(payload.success));
        return;
    }
    if (type === "error") {
        const message = toText(payload.message) || "任务执行出现错误。";
        completionMessage.value = message;
        hasError.value = true;
        setRunState("failed", message);
        addTimeline({ kind: "error", title: "任务错误", detail: message, status: "failed", timestamp: toTimestamp(payload.timestamp) });
        return;
    }
    if (type) addTimeline({ kind: "system", title: "运行事件", detail: shortText(toText(payload.message || payload.detail || type), 280), timestamp: toTimestamp(payload.timestamp) });
}

function handleToolEvent(payload: Record<string, unknown>) {
    const eventType = String(payload.event_type || payload.type || "started").toLowerCase();
    const name = toText(payload.tool) || "未命名工具";
    const action = toText(payload.action) || "执行操作";
    const key = `tool:${name}:${action}`;
    const status: ToolStatus = eventType === "succeeded" || payload.success === true ? "succeeded" : eventType === "failed" || payload.success === false ? "failed" : "running";
    const result = payload.result ?? payload.result_summary;
    upsertTool(key, {
        name,
        action,
        source: sourceFrom(payload.route, name),
        status,
        durationMs: numberOrUndefined(payload.duration_ms),
        params: payload.params_summary ?? payload.params,
        result,
        rawParams: payload.params ?? payload.params_summary,
        rawResult: payload.result ?? payload.result_summary,
    }, shortText(toText(result || payload.params_summary || (status === "running" ? "工具调用已开始。" : "工具事件已更新。")), 280), toTimestamp(payload.timestamp));
}

function handleLog(payload: unknown) {
    const data = asRecord(payload);
    const message = toText(typeof payload === "string" ? payload : data.message || data.log || data.content || payload);
    if (!message) return;
    const lower = message.toLowerCase();
    const kind: TimelineKind = /error|failed|失败|错误/.test(lower) ? "error" : /recover|retry|重试|恢复/.test(lower) ? "recovery" : "system";
    addTimeline({ kind, title: kind === "error" ? "运行日志：异常" : kind === "recovery" ? "运行日志：恢复" : "运行日志", detail: shortText(message, 280), timestamp: toTimestamp(data.timestamp) });
}

function handleContext(payload: unknown) {
    const data = asRecord(payload);
    const candidates = [data.resources, data.resource, asRecord(data.context).resources, asRecord(data.data).resources].flatMap((entry) => Array.isArray(entry) ? entry : entry ? [entry] : []);
    const nextResources = candidates.map((entry, index) => normalizeResource(asRecord(entry), index)).filter((item): item is TaskResource => Boolean(item));
    if (nextResources.length) {
        const merged = new Map(resources.value.map((resource) => [resource.resourceId, resource]));
        nextResources.forEach((resource) => merged.set(resource.resourceId, { ...merged.get(resource.resourceId), ...resource }));
        resources.value = [...merged.values()].slice(0, 40);
        scheduleResourceRefresh();
    }
    const summary = toText(data.summary || data.message || asRecord(data.context).summary);
    if (summary || nextResources.length) addTimeline({ kind: "system", title: "任务上下文已更新", detail: shortText(summary || `已更新 ${nextResources.length} 个资源卡片。`, 280), timestamp: toTimestamp(data.timestamp) });
}

function mergePersistedResources(nextResources: TaskResource[]) {
    const merged = new Map(resources.value.map((resource) => [resource.resourceId, resource]));
    nextResources.forEach((resource) => merged.set(resource.resourceId, { ...merged.get(resource.resourceId), ...resource }));
    resources.value = [...merged.values()].slice(0, 40);
}
async function refreshResources() {
    if (isResourcesLoading.value) return;
    isResourcesLoading.value = true;
    resourceLoadError.value = "";
    try {
        const payload = await invoke<unknown[]>("get_agent_task_resources", { sessionId });
        const nextResources = (Array.isArray(payload) ? payload : [])
            .map((entry, index) => normalizeResource(asRecord(entry), index))
            .filter((resource): resource is TaskResource => resource !== null);
        mergePersistedResources(nextResources);
    } catch (error) {
        resourceLoadError.value = toText(error) || "无法读取当前 session 的任务资源。";
    } finally {
        isResourcesLoading.value = false;
    }
}
function scheduleResourceRefresh() {
    if (resourceRefreshTimer) return;
    resourceRefreshTimer = setTimeout(() => {
        resourceRefreshTimer = null;
        void refreshResources();
    }, 250);
}
function normalizeRecordPage(payload: unknown, fallbackResourceId: string): TaskRecordPage {
    const data = asRecord(payload);
    const page = asRecord(data.page);
    const records = (Array.isArray(data.records) ? data.records : []).map((entry, index) => {
        const record = asRecord(entry);
        return {
            key: toText(record.key || `record-${index + 1}`),
            data: record.data ?? record,
            updatedAt: toText(record.updated_at || record.updatedAt || ""),
        };
    });
    return {
        resourceId: toText(data.resource_id || data.resourceId || fallbackResourceId),
        records,
        offset: numberOrUndefined(page.offset) ?? 0,
        limit: numberOrUndefined(page.limit) ?? RESOURCE_PAGE_SIZE,
        total: numberOrUndefined(page.total) ?? records.length,
        hasMore: page.has_more === true || page.hasMore === true,
    };
}
async function loadResourceRecords(offset = 0) {
    const resource = selectedResource.value;
    if (!resource || isRecordsLoading.value) return;
    isRecordsLoading.value = true;
    recordLoadError.value = "";
    try {
        const payload = await invoke<unknown>("get_agent_task_records", {
            sessionId,
            resourceId: resource.resourceId,
            offset,
            limit: RESOURCE_PAGE_SIZE,
        });
        recordPage.value = normalizeRecordPage(payload, resource.resourceId);
    } catch (error) {
        recordLoadError.value = toText(error) || "无法读取该资源的记录。";
    } finally {
        isRecordsLoading.value = false;
    }
}
function openResourcePreview(resource: TaskResource) {
    selectedResource.value = resource;
    recordPage.value = { resourceId: resource.resourceId, records: [], offset: 0, limit: RESOURCE_PAGE_SIZE, total: 0, hasMore: false };
    recordLoadError.value = "";
    void loadResourceRecords(0);
}
function closeResourcePreview() {
    selectedResource.value = null;
    recordLoadError.value = "";
}
function loadPreviousRecordPage() {
    void loadResourceRecords(Math.max(0, recordPage.value.offset - recordPage.value.limit));
}
function loadNextRecordPage() {
    if (recordPage.value.hasMore) void loadResourceRecords(recordPage.value.offset + recordPage.value.records.length);
}
function completeRun(payload: Record<string, unknown>, succeeded: boolean) {
    const message = toText(payload.message) || (succeeded ? "任务已完成。" : "任务未能完成。")
    completionMessage.value = message;
    hasError.value = !succeeded;
    if (asRecord(payload.finish) && Object.keys(asRecord(payload.finish)).length) {
        finishOutput.value = { ...asRecord(payload.finish), screenshot: payload.screenshot };
    } else if (payload.output) {
        const output = toText(payload.output);
        try {
            const parsed = JSON.parse(output);
            finishOutput.value = typeof parsed === "object" && parsed ? { ...parsed, screenshot: payload.screenshot } : { summary: output };
        } catch {
            finishOutput.value = { summary: output };
        }
    }
    scheduleResourceRefresh();
    setRunState(succeeded ? "succeeded" : "failed", message);
    addTimeline({ kind: succeeded ? "success" : "error", title: succeeded ? "任务已完成" : "任务结束：失败", detail: message, status: succeeded ? "succeeded" : "failed", timestamp: toTimestamp(payload.timestamp) });
}

async function runAgent() {
    if (!canStart.value) return;
    currentGoal.value = goalInput.value.trim();
    timeline.value = [];
    resources.value = [];
    resourceLoadError.value = "";
    selectedResource.value = null;
    recordLoadError.value = "";
    recordPage.value = { resourceId: "", records: [], offset: 0, limit: RESOURCE_PAGE_SIZE, total: 0, hasMore: false };
    tokenUsage.value = null;
    completionMessage.value = "";
    finishOutput.value = null;
    hasError.value = false;
    pauseReason.value = "";
    humanInput.value = "";
    stopRequested.value = false;
    expandedToolDetails.value = new Set();
    activeToolItems.clear();
    runStartedAt.value = Date.now();
    elapsedSeconds.value = 0;
    isRunning.value = true;
    setRunState("preparing", "正在建立任务会话并加载模型路由。")
    startElapsedTimer();
    addTimeline({ kind: "system", title: "任务已提交", detail: shortText(currentGoal.value, 280) });

    try {
        await invoke("run_agent_main_loop", {
            modelRouting: modelRouting.value,
            goal: currentGoal.value,
            autoPilot: true,
            sessionId,
            logContext: logContext.value,
        });
    } catch (error) {
        const message = toText(error) || "任务初始化失败。";
        completionMessage.value = message;
        hasError.value = true;
        setRunState("failed", message);
        addTimeline({ kind: "error", title: "启动失败", detail: message, status: "failed" });
    }
}

async function requestStop() {
    if (!isRunning.value || stopRequested.value) return;
    stopRequested.value = true;
    const message = "用户请求停止当前任务。请立即停止后续工具调用，简要整理当前已完成的工作并结束任务。";
    try {
        await invoke("send_human_intervention", { sessionId, message });
        addTimeline({ kind: "human", title: "已发送停止请求", detail: "停止请求已作为高优先级人工指令提交，等待 Agent 确认。" });
    } catch (error) {
        stopRequested.value = false;
        addTimeline({ kind: "error", title: "停止请求发送失败", detail: toText(error) || "无法发送停止请求。", status: "failed" });
    }
}

async function sendHumanInput() {
    const message = humanInput.value.trim();
    if (!message || isSendingHuman.value) return;
    isSendingHuman.value = true;
    try {
        await invoke("send_human_intervention", { sessionId, message });
        addTimeline({ kind: "human", title: "已提交人工输入", detail: shortText(message, 280) });
        humanInput.value = "";
        pauseReason.value = "";
        if (isWaitingForHuman.value) setRunState("executing", "已发送人工输入，等待 Agent 继续执行。")
    } catch (error) {
        addTimeline({ kind: "error", title: "人工输入发送失败", detail: toText(error) || "无法将输入发送给 Agent。", status: "failed" });
    } finally {
        isSendingHuman.value = false;
    }
}

function clearAll() {
    timeline.value = [];
    resources.value = [];
    resourceLoadError.value = "";
    selectedResource.value = null;
    recordLoadError.value = "";
    recordPage.value = { resourceId: "", records: [], offset: 0, limit: RESOURCE_PAGE_SIZE, total: 0, hasMore: false };
    tokenUsage.value = null;
    completionMessage.value = "";
    finishOutput.value = null;
    hasError.value = false;
    currentGoal.value = "";
    pauseReason.value = "";
    stopRequested.value = false;
    elapsedSeconds.value = 0;
    runStartedAt.value = null;
    expandedToolDetails.value = new Set();
    activeToolItems.clear();
    setRunState("idle");
}

function toggleToolDetail(id: string, section: DetailSection) {
    const key = `${id}:${section}`;
    const next = new Set(expandedToolDetails.value);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    expandedToolDetails.value = next;
}

function isToolDetailExpanded(id: string, section: DetailSection) {
    return expandedToolDetails.value.has(`${id}:${section}`);
}

function normalizeResource(data: Record<string, unknown>, index: number): TaskResource | null {
    const resourceId = toText(data.resource_id || data.resourceId || data.id || `context-resource-${index + 1}`);
    if (!resourceId) return null;
    return {
        resourceId,
        kind: toText(data.kind || data.type || "resource"),
        label: toText(data.label || data.name || data.title || resourceId),
        rowCount: data.row_count as number | string | undefined ?? data.rowCount as number | string | undefined,
        status: toText(data.status || "available"),
        summary: shortText(toText(data.summary || data.description || ""), 360),
        cursor: toText(data.cursor || data.next_cursor || data.nextCursor || ""),
    };
}

function sourceFrom(route: unknown, name: string): "内部" | "MCP" {
    return /mcp/i.test(`${toText(route)} ${name}`) ? "MCP" : "内部";
}

function kindLabel(kind: TimelineKind) {
    return { system: "SYSTEM", planning: "PLANNING", tool: "TOOL", success: "SUCCESS", error: "ERROR", human: "HUMAN", recovery: "RECOVERY" }[kind];
}

function toolStatusLabel(status: ToolStatus) {
    return { pending: "等待", running: "运行中", succeeded: "成功", failed: "失败" }[status];
}

function stateMetaFor(state: RunState) {
    return stateCatalog[state].label;
}

function formatDuration(seconds: number) {
    const hours = Math.floor(seconds / 3600);
    const minutes = Math.floor((seconds % 3600) / 60);
    const remainder = seconds % 60;
    return hours ? `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}:${String(remainder).padStart(2, "0")}` : `${String(minutes).padStart(2, "0")}:${String(remainder).padStart(2, "0")}`;
}

function formatMilliseconds(milliseconds: number) {
    return milliseconds >= 1000 ? `${(milliseconds / 1000).toFixed(milliseconds >= 10000 ? 0 : 1)} 秒` : `${milliseconds} 毫秒`;
}

function formatClock(timestamp: number) {
    return new Intl.DateTimeFormat("zh-CN", { hour: "2-digit", minute: "2-digit", second: "2-digit" }).format(timestamp);
}

function formatNumber(value: number) {
    return new Intl.NumberFormat("zh-CN", { notation: "compact", maximumFractionDigits: 1 }).format(value);
}

function formatPayload(value: unknown) {
    if (value === undefined || value === null || value === "") return "未提供。";
    let text: string;
    if (typeof value === "string") {
        try {
            text = JSON.stringify(JSON.parse(value), null, 2);
        } catch {
            text = value;
        }
    } else {
        try {
            text = JSON.stringify(value, null, 2);
        } catch {
            text = toText(value);
        }
    }
    return shortText(text, 12000);
}

function shortText(value: string, max: number) {
    const compact = value.replace(/\s+/g, " ").trim();
    return compact.length > max ? `${compact.slice(0, max)}…（已截断）` : compact;
}

function toText(value: unknown): string {
    if (value === null || value === undefined) return "";
    if (typeof value === "string") return value;
    if (typeof value === "number" || typeof value === "boolean") return String(value);
    try {
        return JSON.stringify(value);
    } catch {
        return String(value);
    }
}

function asRecord(value: unknown): Record<string, unknown> {
    return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function toTimestamp(value: unknown) {
    if (typeof value === "number" && Number.isFinite(value)) return value > 10_000_000_000 ? value : value * 1000;
    if (typeof value === "string") {
        const parsed = Date.parse(value);
        if (!Number.isNaN(parsed)) return parsed;
    }
    return Date.now();
}

function numberOrUndefined(value: unknown) {
    const number = Number(value);
    return Number.isFinite(number) && number >= 0 ? number : undefined;
}

onMounted(async () => {
    void refreshResources();
    const register = async (channel: string, handler: (payload: unknown) => void) => {
        try {
            unlisteners.push(await listen<unknown>(channel, (event) => handler(event.payload)));
        } catch (error) {
            addTimeline({ kind: "error", title: "事件监听不可用", detail: `${channel}：${toText(error) || "无法注册监听。"}`, status: "failed" });
        }
    };

    await register(`agent-run-state-${sessionId}`, (payload) => handleRunState(asRecord(payload)));
    await register(`agent-progress-${sessionId}`, (payload) => handleProgress(asRecord(payload)));
    await register(`agent-log-${sessionId}`, handleLog);
    await register(`agent-context-${sessionId}`, handleContext);
    // 后端尚未发送该事件时，监听器保持空闲；一旦可用则自动增强工具卡片信息。
    await register(`agent-tool-event-${sessionId}`, (payload) => handleToolEvent(asRecord(payload)));
});

onUnmounted(() => {
    stopElapsedTimer();
    if (resourceRefreshTimer) clearTimeout(resourceRefreshTimer);
    unlisteners.forEach((unlisten) => unlisten());
    unlisteners = [];
});

defineExpose({ isRunning });
</script>

<style scoped>
.agent-console {
    --console-bg: #090d14;
    --surface-1: #101722;
    --surface-2: #151e2b;
    --surface-3: #1a2636;
    --line: rgba(148, 163, 184, 0.16);
    --line-strong: rgba(148, 163, 184, 0.28);
    --text: #e7edf6;
    --text-soft: #9daabd;
    --text-faint: #64748b;
    --accent: #61a8ff;
    --accent-soft: rgba(97, 168, 255, 0.14);
    --success: #4bd19a;
    --warning: #e7b25c;
    --danger: #ff707d;
    display: grid;
    grid-template-columns: minmax(276px, 318px) minmax(0, 1fr);
    height: 100%;
    min-height: 0;
    overflow: hidden;
    color: var(--text);
    background: var(--console-bg);
    font-variant-numeric: tabular-nums;
}

.console-controls {
    display: flex;
    flex-direction: column;
    gap: 18px;
    min-height: 0;
    padding: 22px 18px 18px;
    overflow-y: auto;
    background: linear-gradient(180deg, #0f1621 0%, #0b1018 100%);
    border-right: 1px solid var(--line);
}

.console-brand {
    padding: 0 4px 4px;
}

.eyebrow {
    display: block;
    color: var(--accent);
    font-size: 10px;
    font-weight: 750;
    letter-spacing: 0.13em;
    line-height: 1.25;
}

.console-brand h1,
.panel-header h2,
.run-status h2,
.human-input-card h2,
.final-result h2 {
    margin: 5px 0 0;
    color: var(--text);
    font-size: 19px;
    font-weight: 690;
    letter-spacing: -0.018em;
}

.console-brand p {
    margin: 6px 0 0;
    color: var(--text-faint);
    font-size: 12px;
}

.control-section {
    padding: 15px;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: rgba(21, 30, 43, 0.68);
}

.compact-section {
    padding-block: 12px;
}

.section-heading,
.toggle-row,
.panel-header,
.tool-header,
.event-header,
.human-input-header,
.resource-card header,
.tool-summary-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
}

.section-heading {
    margin-bottom: 12px;
    color: var(--text);
    font-size: 12px;
    font-weight: 700;
}

.section-heading small,
.toggle-row small {
    color: var(--text-faint);
    font-size: 11px;
    font-weight: 500;
}

.field-label {
    display: block;
    margin: 12px 0 6px;
    color: var(--text-soft);
    font-size: 11px;
    font-weight: 650;
}

.field-label:first-of-type {
    margin-top: 0;
}

select,
textarea {
    box-sizing: border-box;
    width: 100%;
    border: 1px solid var(--line-strong);
    border-radius: 8px;
    outline: none;
    color: var(--text);
    background: #0b111a;
    font: inherit;
    font-size: 12px;
    transition: border-color 160ms ease, box-shadow 160ms ease;
}

select {
    height: 34px;
    padding: 0 10px;
}

textarea {
    resize: vertical;
    padding: 10px;
    line-height: 1.55;
}

select:focus,
textarea:focus {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
}

select:disabled,
textarea:disabled,
button:disabled {
    cursor: not-allowed;
    opacity: 0.48;
}

.toggle-row {
    cursor: pointer;
}

.toggle-row span {
    display: grid;
    gap: 3px;
}

.toggle-row strong {
    color: var(--text-soft);
    font-size: 12px;
}

.toggle-row input {
    width: 15px;
    height: 15px;
    accent-color: var(--accent);
}

.preset-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 7px;
}

.preset-button,
.action-button,
.continue-button,
.detail-toggle {
    border: 0;
    font: inherit;
    cursor: pointer;
}

.preset-button {
    min-height: 31px;
    padding: 0 8px;
    border: 1px solid var(--line);
    border-radius: 7px;
    color: var(--text-soft);
    background: rgba(10, 16, 25, 0.78);
    font-size: 11px;
    transition: color 150ms ease, border-color 150ms ease, background 150ms ease;
}

.preset-button:hover,
.preset-button.active {
    border-color: rgba(97, 168, 255, 0.6);
    color: #dbeafe;
    background: var(--accent-soft);
}

.goal-section {
    flex: 1;
    min-height: 166px;
}

.goal-section textarea {
    min-height: 140px;
}

.control-actions {
    display: grid;
    gap: 8px;
    margin-top: auto;
}

.action-button,
.continue-button {
    min-height: 38px;
    border-radius: 8px;
    font-size: 12px;
    font-weight: 720;
    transition: transform 160ms ease, filter 160ms ease, background 160ms ease;
}

.action-button:not(:disabled):hover,
.continue-button:not(:disabled):hover {
    filter: brightness(1.08);
    transform: translateY(-1px);
}

.action-button.primary,
.continue-button {
    color: #07111f;
    background: linear-gradient(135deg, #76b8ff, #4c8df2);
}

.action-button.secondary {
    color: var(--text-soft);
    background: transparent;
    border: 1px solid var(--line-strong);
}

.action-button.danger {
    color: #ffd8dc;
    background: rgba(255, 112, 125, 0.12);
    border: 1px solid rgba(255, 112, 125, 0.36);
}

.control-hint {
    margin: 1px 1px 0;
    color: var(--text-faint);
    font-size: 11px;
    line-height: 1.5;
}

.console-workspace {
    min-width: 0;
    min-height: 0;
    padding: 20px;
    overflow: auto;
    background:
        radial-gradient(circle at 92% -10%, rgba(65, 125, 211, 0.13), transparent 32%),
        var(--console-bg);
}

.run-status {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 22px;
    padding: 18px 20px;
    border: 1px solid var(--line);
    border-radius: 14px;
    background: linear-gradient(110deg, rgba(20, 31, 46, 0.96), rgba(15, 22, 33, 0.91));
}

.status-main {
    display: flex;
    min-width: 0;
    align-items: center;
    gap: 13px;
}

.state-marker {
    display: grid;
    width: 36px;
    height: 36px;
    flex: 0 0 auto;
    place-items: center;
    border: 1px solid var(--line-strong);
    border-radius: 50%;
    background: rgba(8, 14, 23, 0.72);
}

.state-marker span {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--text-faint);
}

.state-preparing .state-marker span,
.state-planning .state-marker span,
.state-executing .state-marker span,
.state-finalizing .state-marker span { background: var(--accent); box-shadow: 0 0 0 5px rgba(97, 168, 255, 0.1); }
.state-waiting_for_human .state-marker span { background: var(--warning); box-shadow: 0 0 0 5px rgba(231, 178, 92, 0.1); }
.state-recovering .state-marker span { background: #b48cff; box-shadow: 0 0 0 5px rgba(180, 140, 255, 0.1); }
.state-succeeded .state-marker span { background: var(--success); box-shadow: 0 0 0 5px rgba(75, 209, 154, 0.1); }
.state-failed .state-marker span,
.state-cancelled .state-marker span { background: var(--danger); box-shadow: 0 0 0 5px rgba(255, 112, 125, 0.1); }

.state-line {
    display: flex;
    align-items: center;
    gap: 8px;
}

.state-line h2 {
    font-size: 21px;
}

.state-chip,
.event-kind,
.source-tag,
.tool-status,
.waiting-badge,
.resource-kind,
.resource-status,
.panel-count {
    display: inline-flex;
    align-items: center;
    width: fit-content;
    border-radius: 999px;
    font-size: 10px;
    font-weight: 750;
    letter-spacing: 0.035em;
    line-height: 1;
}

.state-chip {
    padding: 5px 7px;
    color: #c7dcfa;
    background: var(--accent-soft);
}

.status-main p {
    margin: 5px 0 0;
    overflow: hidden;
    color: var(--text-soft);
    font-size: 12px;
    line-height: 1.45;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.run-metadata {
    display: grid;
    grid-template-columns: minmax(150px, 1.7fr) repeat(2, minmax(72px, auto));
    gap: 22px;
    margin: 0;
}

.run-metadata div {
    min-width: 0;
}

.run-metadata dt {
    margin-bottom: 5px;
    color: var(--text-faint);
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.05em;
    text-transform: uppercase;
}

.run-metadata dd {
    max-width: 240px;
    margin: 0;
    overflow: hidden;
    color: var(--text);
    font-size: 12px;
    line-height: 1.35;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.metric-value {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

.live-metrics {
    display: flex;
    flex-wrap: wrap;
    gap: 7px 17px;
    padding: 10px 2px 0;
    color: var(--text-faint);
    font-size: 11px;
}

.live-metrics strong {
    color: var(--text-soft);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-weight: 700;
}

.context-metric {
    color: #d8b471;
}

.human-input-card {
    margin-top: 16px;
    padding: 17px;
    border: 1px solid var(--line);
    border-radius: 13px;
    background: rgba(21, 30, 43, 0.78);
}

.human-input-card.waiting {
    border-color: rgba(231, 178, 92, 0.62);
    background: linear-gradient(120deg, rgba(83, 59, 26, 0.48), rgba(22, 29, 37, 0.92));
}

.human-input-header {
    align-items: flex-start;
}

.human-input-card h2 {
    font-size: 15px;
}

.human-input-card p {
    margin: 5px 0 0;
    color: var(--text-soft);
    font-size: 12px;
    line-height: 1.5;
}

.waiting-badge {
    padding: 6px 8px;
    color: #ffe2a8;
    background: rgba(231, 178, 92, 0.15);
}

.human-input-form {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 116px;
    gap: 10px;
    margin-top: 13px;
}

.human-input-form textarea {
    min-height: 72px;
}

.content-grid {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(252px, 310px);
    align-items: start;
    gap: 16px;
    margin-top: 16px;
}

.timeline-panel,
.resource-panel,
.final-result {
    border: 1px solid var(--line);
    border-radius: 14px;
    background: rgba(15, 22, 33, 0.84);
}

.timeline-panel {
    min-width: 0;
    overflow: hidden;
}

.panel-header {
    padding: 17px 18px;
    border-bottom: 1px solid var(--line);
}

.panel-header h2 {
    font-size: 15px;
}

.panel-count {
    min-width: 25px;
    justify-content: center;
    padding: 5px 7px;
    color: var(--text-soft);
    background: rgba(148, 163, 184, 0.1);
}

.timeline-empty {
    position: relative;
    min-height: 310px;
    padding: 78px 30px 30px;
    overflow: hidden;
    text-align: center;
}

.empty-grid {
    position: absolute;
    top: 43px;
    left: 50%;
    width: 50px;
    height: 50px;
    border: 1px solid rgba(97, 168, 255, 0.46);
    border-radius: 8px;
    transform: translateX(-50%) rotate(45deg);
    background:
        linear-gradient(90deg, transparent 48%, rgba(97, 168, 255, 0.23) 49%, rgba(97, 168, 255, 0.23) 51%, transparent 52%),
        linear-gradient(transparent 48%, rgba(97, 168, 255, 0.23) 49%, rgba(97, 168, 255, 0.23) 51%, transparent 52%);
}

.timeline-empty h3 {
    margin: 0;
    color: var(--text-soft);
    font-size: 14px;
}

.timeline-empty p {
    max-width: 380px;
    margin: 9px auto 0;
    color: var(--text-faint);
    font-size: 12px;
    line-height: 1.65;
}

.timeline-list {
    max-height: min(68vh, 760px);
    margin: 0;
    padding: 18px 18px 24px;
    overflow: auto;
    list-style: none;
    scroll-behavior: smooth;
}

.timeline-entry {
    position: relative;
    display: grid;
    grid-template-columns: 24px minmax(0, 1fr);
    min-width: 0;
}

.timeline-entry:not(:last-child) {
    padding-bottom: 11px;
}

.timeline-rail {
    position: relative;
    display: flex;
    justify-content: center;
}

.timeline-rail::before {
    position: absolute;
    top: 16px;
    bottom: -11px;
    width: 1px;
    background: var(--line-strong);
    content: "";
}

.timeline-entry:last-child .timeline-rail::before {
    display: none;
}

.timeline-rail span {
    position: relative;
    z-index: 1;
    width: 9px;
    height: 9px;
    margin-top: 14px;
    border: 3px solid #111925;
    border-radius: 50%;
    background: #7d8ca1;
}

.kind-planning .timeline-rail span { background: var(--accent); }
.kind-tool .timeline-rail span { background: #8f7cff; }
.kind-success .timeline-rail span { background: var(--success); }
.kind-error .timeline-rail span { background: var(--danger); }
.kind-human .timeline-rail span { background: var(--warning); }
.kind-recovery .timeline-rail span { background: #bc8dff; }

.event-card,
.tool-card {
    min-width: 0;
    padding: 13px 14px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: rgba(22, 31, 45, 0.74);
}

.event-card:hover,
.tool-card:hover {
    border-color: rgba(148, 163, 184, 0.3);
}

.event-header {
    align-items: flex-start;
}

.event-kind {
    padding: 4px 6px;
    color: var(--text-faint);
    background: rgba(148, 163, 184, 0.1);
}

.kind-success .event-kind { color: #a6f2d0; background: rgba(75, 209, 154, 0.1); }
.kind-error .event-kind { color: #ffc0c6; background: rgba(255, 112, 125, 0.1); }
.kind-human .event-kind { color: #ffe1a5; background: rgba(231, 178, 92, 0.11); }
.kind-planning .event-kind { color: #c8e0ff; background: var(--accent-soft); }
.kind-recovery .event-kind { color: #d8c1ff; background: rgba(180, 140, 255, 0.12); }

.event-card h3,
.tool-card h3,
.resource-card h3 {
    margin: 6px 0 0;
    color: var(--text);
    font-size: 13px;
    font-weight: 670;
    line-height: 1.35;
}

.event-card time {
    color: var(--text-faint);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 10px;
    white-space: nowrap;
}

.event-card p,
.tool-summary-row p,
.resource-intro,
.resource-card p {
    margin: 9px 0 0;
    color: var(--text-soft);
    font-size: 12px;
    line-height: 1.6;
}

.tool-card {
    border-left: 3px solid #8f7cff;
}

.tool-card.tool-succeeded { border-left-color: var(--success); }
.tool-card.tool-failed { border-left-color: var(--danger); }

.tool-header {
    align-items: flex-start;
}

.tool-title-wrap {
    min-width: 0;
}

.tool-title-wrap h3 {
    display: inline-block;
    margin-right: 7px;
}

.tool-action {
    color: var(--text-faint);
    font-size: 11px;
}

.tool-header-meta {
    display: flex;
    flex: 0 0 auto;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: 5px;
}

.source-tag,
.tool-status,
.resource-kind,
.resource-status {
    padding: 5px 7px;
}

.source-tag.internal { color: #c6d2e4; background: rgba(148, 163, 184, 0.11); }
.source-tag.mcp { color: #d3c2ff; background: rgba(180, 140, 255, 0.12); }
.tool-status.running { color: #c8e0ff; background: var(--accent-soft); }
.tool-status.succeeded { color: #a6f2d0; background: rgba(75, 209, 154, 0.1); }
.tool-status.failed { color: #ffc0c6; background: rgba(255, 112, 125, 0.1); }
.tool-status.pending { color: #d1d9e5; background: rgba(148, 163, 184, 0.1); }

.tool-summary-row {
    align-items: flex-start;
    margin-top: 1px;
}

.tool-summary-row p {
    min-width: 0;
    margin-right: 10px;
}

.duration {
    margin-top: 9px;
    color: var(--text-faint);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 10px;
    white-space: nowrap;
}

.tool-details {
    display: grid;
    gap: 6px;
    margin-top: 12px;
}

.detail-toggle {
    display: flex;
    justify-content: space-between;
    width: 100%;
    padding: 7px 8px;
    border-radius: 6px;
    color: var(--text-soft);
    background: rgba(5, 10, 16, 0.42);
    font-size: 11px;
    text-align: left;
}

.detail-toggle:hover {
    color: var(--text);
    background: rgba(97, 168, 255, 0.1);
}

.tool-details pre {
    max-height: 250px;
    margin: 0;
    padding: 10px;
    overflow: auto;
    border: 1px solid rgba(148, 163, 184, 0.13);
    border-radius: 6px;
    color: #c5d1e2;
    background: #080d14;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 10.5px;
    line-height: 1.55;
    white-space: pre-wrap;
    word-break: break-word;
}

.resource-panel {
    padding-bottom: 11px;
}

.resource-intro {
    margin: 13px 14px 10px;
    color: var(--text-faint);
    font-size: 11px;
}

.resource-empty {
    display: grid;
    gap: 7px;
    padding: 40px 18px;
    color: var(--text-soft);
    font-size: 12px;
    text-align: center;
}

.resource-empty small {
    color: var(--text-faint);
    font-size: 11px;
    line-height: 1.5;
}

.resource-list {
    display: grid;
    gap: 9px;
    padding: 0 11px;
}

.resource-card {
    padding: 12px;
    border: 1px solid var(--line);
    border-radius: 9px;
    background: rgba(23, 32, 46, 0.65);
}

.resource-card h3 {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.resource-kind {
    color: #c8e0ff;
    background: var(--accent-soft);
    text-transform: uppercase;
}

.resource-status {
    max-width: 100px;
    overflow: hidden;
    color: #bcefd6;
    background: rgba(75, 209, 154, 0.1);
    text-overflow: ellipsis;
    white-space: nowrap;
}

.resource-card dl {
    display: grid;
    gap: 6px;
    margin: 12px 0 0;
}

.resource-card dl div {
    display: grid;
    grid-template-columns: 70px minmax(0, 1fr);
    gap: 7px;
}

.resource-card dt {
    color: var(--text-faint);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 10px;
}

.resource-card dd {
    margin: 0;
    overflow: hidden;
    color: var(--text-soft);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 10px;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.resource-card p {
    display: -webkit-box;
    overflow: hidden;
    margin-top: 10px;
    color: var(--text-faint);
    font-size: 11px;
    line-height: 1.5;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 4;
}

.final-result {
    margin-top: 16px;
    padding: 18px;
    border-left: 3px solid var(--success);
}

.final-result.failed {
    border-left-color: var(--danger);
}

.final-result-label {
    color: var(--success);
    font-size: 10px;
    font-weight: 750;
    letter-spacing: 0.08em;
    text-transform: uppercase;
}

.final-result.failed .final-result-label {
    color: #ffc0c6;
}

.final-result h2 {
    font-size: 15px;
}

.final-result p {
    max-height: 250px;
    margin: 9px 0 0;
    overflow: auto;
    color: var(--text-soft);
    font-size: 12px;
    line-height: 1.65;
    white-space: pre-wrap;
}

.artifact-list {
    display: grid;
    gap: 6px;
    margin-top: 13px;
}

.artifact-list span {
    color: var(--text-faint);
    font-size: 10px;
    font-weight: 700;
}

.artifact-list code {
    overflow: hidden;
    padding: 7px 8px;
    border-radius: 5px;
    color: #c9d5e5;
    background: rgba(5, 10, 16, 0.46);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 10px;
    text-overflow: ellipsis;
    white-space: nowrap;
}

@media (max-width: 1120px) {
    .agent-console {
        grid-template-columns: 280px minmax(0, 1fr);
    }

    .content-grid {
        grid-template-columns: 1fr;
    }

    .resource-panel {
        max-height: 440px;
        overflow: auto;
    }

    .resource-list {
        grid-template-columns: repeat(2, minmax(0, 1fr));
    }
}

@media (max-width: 820px) {
    .agent-console {
        display: block;
        height: auto;
        overflow: visible;
    }

    .console-controls,
    .console-workspace {
        overflow: visible;
    }

    .console-controls {
        gap: 14px;
        border-right: 0;
        border-bottom: 1px solid var(--line);
    }

    .goal-section {
        min-height: 0;
    }

    .run-status {
        grid-template-columns: 1fr;
        gap: 17px;
    }

    .run-metadata {
        grid-template-columns: minmax(0, 1fr) repeat(2, auto);
        gap: 14px;
    }

    .console-workspace {
        padding: 15px;
    }
}

@media (max-width: 540px) {
    .console-controls {
        padding: 16px 13px;
    }

    .console-workspace {
        padding: 12px;
    }

    .run-status,
    .human-input-card,
    .panel-header,
    .timeline-list {
        padding-left: 13px;
        padding-right: 13px;
    }

    .run-metadata {
        grid-template-columns: 1fr 1fr;
    }

    .run-metadata div:first-child {
        grid-column: 1 / -1;
    }

    .human-input-form {
        grid-template-columns: 1fr;
    }

    .continue-button {
        min-height: 36px;
    }

    .resource-list {
        grid-template-columns: 1fr;
    }

    .tool-header {
        display: grid;
        grid-template-columns: 1fr;
    }

    .tool-header-meta {
        justify-content: flex-start;
    }
}
.resource-header-actions {
    display: inline-flex;
    align-items: center;
    gap: 8px;
}
.resource-refresh,
.resource-preview-button,
.resource-preview-close,
.resource-pagination button {
    border: 1px solid var(--line-strong);
    border-radius: 8px;
    color: var(--text-soft);
    background: rgba(15, 23, 34, 0.84);
    cursor: pointer;
    font: inherit;
}
.resource-refresh,
.resource-preview-close,
.resource-pagination button {
    padding: 6px 9px;
    font-size: 12px;
}
.resource-preview-button {
    width: 100%;
    margin-top: 4px;
    padding: 8px 10px;
    color: var(--accent);
    font-size: 12px;
}
.resource-refresh:hover:not(:disabled),
.resource-preview-button:hover,
.resource-preview-close:hover,
.resource-pagination button:hover:not(:disabled) {
    border-color: var(--accent);
    color: var(--text);
    background: var(--accent-soft);
}
.resource-refresh:disabled,
.resource-pagination button:disabled {
    cursor: not-allowed;
    opacity: 0.45;
}
.resource-load-error {
    margin: -2px 0 10px;
    color: var(--danger);
    font-size: 12px;
    line-height: 1.55;
}
.resource-preview-backdrop {
    position: fixed;
    z-index: 30;
    inset: 0;
    display: grid;
    place-items: center;
    padding: 24px;
    background: rgba(2, 6, 13, 0.72);
    backdrop-filter: blur(5px);
}
.resource-preview-dialog {
    display: grid;
    grid-template-rows: auto minmax(0, 1fr) auto;
    width: min(860px, 100%);
    max-height: min(760px, calc(100vh - 48px));
    overflow: hidden;
    border: 1px solid var(--line-strong);
    border-radius: 16px;
    background: #0d1520;
    box-shadow: 0 24px 70px rgba(0, 0, 0, 0.46);
}
.resource-preview-header,
.resource-pagination {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 16px 18px;
    border-bottom: 1px solid var(--line);
}
.resource-preview-header h2 {
    margin: 4px 0 0;
    font-size: 18px;
}
.resource-preview-header p {
    margin: 5px 0 0;
    overflow: hidden;
    color: var(--text-faint);
    font: 11px/1.4 ui-monospace, SFMono-Regular, Menlo, monospace;
    text-overflow: ellipsis;
    white-space: nowrap;
}
.resource-preview-loading,
.resource-preview-empty {
    display: grid;
    min-height: 180px;
    place-items: center;
    padding: 28px;
    color: var(--text-faint);
}
.resource-record-list {
    min-height: 0;
    margin: 0;
    padding: 12px 18px;
    overflow: auto;
    list-style: none;
}
.resource-record-list li {
    margin-bottom: 10px;
    border: 1px solid var(--line);
    border-radius: 10px;
    overflow: hidden;
    background: rgba(21, 30, 43, 0.62);
}
.resource-record-list header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 9px 11px;
    border-bottom: 1px solid var(--line);
    color: var(--text-soft);
    font-size: 12px;
}
.resource-record-list time {
    color: var(--text-faint);
    font-size: 11px;
}
.resource-record-list pre {
    max-height: 220px;
    margin: 0;
    padding: 11px;
    overflow: auto;
    color: #cbd8e8;
    font: 12px/1.55 ui-monospace, SFMono-Regular, Menlo, monospace;
    white-space: pre-wrap;
    word-break: break-word;
}
.resource-pagination {
    border-top: 1px solid var(--line);
    border-bottom: 0;
    color: var(--text-faint);
    font-size: 12px;
}
.resource-pagination > div {
    display: inline-flex;
    gap: 8px;
}
@media (max-width: 720px) {
    .resource-preview-backdrop { padding: 12px; }
    .resource-preview-dialog { max-height: calc(100vh - 24px); }
    .resource-preview-header,
    .resource-pagination { align-items: flex-start; flex-direction: column; }
}
</style>
