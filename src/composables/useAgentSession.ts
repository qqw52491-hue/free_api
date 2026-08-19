import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useAgentTaskResources } from "./useAgentTaskResources";
import { usePersistedAgentConversation, type PersistedConversationMessage } from "./usePersistedAgentConversation";
export interface AgentHistoryTurn {
    id: string;
    goal: string;
    status: string;
    steps: AgentStepItem[];
    messages: Array<PersistedConversationMessage & { timestamp: number }>;
    summary: string;
    artifacts: string[];
    startedAt: number;
    completedAt: number;
}
interface HistoryRunSnapshot extends Omit<AgentHistoryTurn, "messages"> {}
import {
    AGENT_RUN_STATE_CATALOG,
    type AgentHumanForm,
    type AgentHumanFormKind,
    type AgentLogEntry,
    type AgentModelOption,
    type AgentRunState,
    type AgentStepItem,
    type AgentTokenUsage,
    type AgentSessionTelemetry,
    type AgentToolStatus,
} from "../components/agent/types";
import { asRecord, numberOrUndefined, toText, toTimestamp } from "../components/agent/format";

const MAX_LOGS = 600;
type ReasoningEffort = "auto" | "off" | "low" | "medium" | "high";
type StoredReasoningEffort = ReasoningEffort | "max";
type ModelRouting = {
    reasoning_effort_version: 2;
    pro: string;
    flash: string;
    vision: string;
    pro_reasoning_effort: ReasoningEffort;
    flash_reasoning_effort: ReasoningEffort;
    vision_reasoning_effort: ReasoningEffort;
};
const MODEL_ROUTING_STORAGE_KEY = "free-api.agent.model-routing.v1";
const REASONING_EFFORTS: ReasoningEffort[] = ["auto", "off", "low", "medium", "high"];
const STORED_REASONING_EFFORTS: StoredReasoningEffort[] = [...REASONING_EFFORTS, "max"];

function migrateReasoningEffort(value: unknown, fallback: ReasoningEffort, isLegacy: boolean): ReasoningEffort {
    const stored = STORED_REASONING_EFFORTS.includes(value as StoredReasoningEffort)
        ? value as StoredReasoningEffort
        : fallback;
    if (stored === "max") return "high";
    if (isLegacy && stored === "high") return "medium";
    return stored;
}

function loadModelRouting(): ModelRouting {
    const fallback: ModelRouting = {
        reasoning_effort_version: 2,
        pro: "",
        flash: "",
        vision: "",
        pro_reasoning_effort: "auto",
        flash_reasoning_effort: "auto",
        vision_reasoning_effort: "auto",
    };
    try {
        const stored = JSON.parse(localStorage.getItem(MODEL_ROUTING_STORAGE_KEY) || "{}") as Partial<ModelRouting> & { reasoning_effort?: unknown; reasoning_effort_version?: unknown };
        const isLegacy = stored.reasoning_effort_version !== 2;
        const legacyEffort = migrateReasoningEffort(stored.reasoning_effort, fallback.pro_reasoning_effort, isLegacy);
        return {
            reasoning_effort_version: 2,
            pro: typeof stored.pro === "string" ? stored.pro : fallback.pro,
            flash: typeof stored.flash === "string" ? stored.flash : fallback.flash,
            vision: typeof stored.vision === "string" ? stored.vision : fallback.vision,
            pro_reasoning_effort: migrateReasoningEffort(stored.pro_reasoning_effort, legacyEffort, isLegacy),
            flash_reasoning_effort: migrateReasoningEffort(stored.flash_reasoning_effort, legacyEffort, isLegacy),
            vision_reasoning_effort: migrateReasoningEffort(stored.vision_reasoning_effort, legacyEffort, isLegacy),
        };
    } catch {
        return fallback;
    }
}

function normalizeState(value: unknown): AgentRunState {
    const normalized = String(value || "")
        .replace(/[\s-]/g, "_")
        .replace(/([a-z])([A-Z])/g, "$1_$2")
        .toLowerCase();
    const aliases: Record<string, AgentRunState> = {
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

function sourceFrom(route: unknown, name: string): "内部" | "MCP" {
    return /mcp/i.test(`${toText(route)} ${name}`) ? "MCP" : "内部";
}
function normalizeHumanForm(raw: unknown, fallbackTitle: string): AgentHumanForm | null {
    const data = asRecord(raw);
    const rawKind = toText(data.kind).toLowerCase();
    const aliases: Record<string, AgentHumanFormKind> = {
        single: "single_select", select: "single_select", single_select: "single_select", radio: "single_select",
        multi: "multi_select", multiple: "multi_select", multi_select: "multi_select", checkbox: "multi_select",
        confirm: "confirm", confirmation: "confirm", text: "text", input: "text",
        file: "file_picker", file_picker: "file_picker", upload: "file_picker", files: "file_picker",
        custom_html: "custom_html", html: "custom_html", custom: "custom_html",
    };
    const htmlCode = toText(data.html || data.custom_html || (rawKind === "custom_html" || rawKind === "html" ? data.description : ""));
    const kind = aliases[rawKind] || (htmlCode ? "custom_html" : undefined);
    if (!kind) return null;
    const options = (Array.isArray(data.options) ? data.options : []).slice(0, 12).map((item, index) => {
        const option = asRecord(item);
        const id = toText(option.id || option.value || index + 1).slice(0, 80);
        const label = toText(option.label || option.title || option.name || id).slice(0, 160);
        const description = toText(option.description || option.detail).slice(0, 300);
        return description ? { id, label, description } : { id, label };
    }).filter((item) => item.id && item.label);
    if ((kind === "single_select" || kind === "multi_select") && !options.length) return null;
    const defaultRaw = data.default ?? data.default_value ?? data.value;
    const defaultValue = kind === "multi_select"
        ? (Array.isArray(defaultRaw) ? defaultRaw.map((item) => toText(item)).filter(Boolean).slice(0, 12) : undefined)
        : kind === "confirm" ? (typeof defaultRaw === "boolean" ? defaultRaw : undefined)
        : toText(defaultRaw) || undefined;
    return {
        kind,
        title: toText(data.title || data.label || fallbackTitle).slice(0, 220) || fallbackTitle,
        description: kind === "custom_html" ? undefined : (toText(data.description || data.help).slice(0, 600) || undefined),
        placeholder: toText(data.placeholder).slice(0, 160) || undefined,
        options,
        defaultValue,
        html: htmlCode || undefined,
    };
}

function normalizeUsage(payload: Record<string, unknown>): AgentTokenUsage | null {
    const usage = asRecord(payload.usage || payload.token_usage || payload.tokens || (payload.type === "token_usage" ? payload : {}));
    if (!Object.keys(usage).length) return null;
    const total = numberOrUndefined(usage.total_tokens ?? usage.total);
    const contextWindow = numberOrUndefined(usage.context_window ?? usage.contextWindow ?? usage.max_tokens);
    if (total === undefined) return null;
    const prompt = numberOrUndefined(usage.prompt_tokens ?? usage.prompt) ?? 0;
    const cached = numberOrUndefined(usage.cached_tokens ?? usage.cached) ?? 0;
    const cacheHitRate = numberOrUndefined(usage.cache_hit_rate ?? usage.cacheHitRate) ?? (prompt > 0 ? (cached / prompt) * 100 : 0);
    return {
        prompt_tokens: prompt,
        completion_tokens: numberOrUndefined(usage.completion_tokens ?? usage.completion) ?? 0,
        total_tokens: total,
        cached_tokens: cached,
        cache_hit_rate: cacheHitRate,
        context_window: contextWindow ?? 0,
        usage_percent: contextWindow ? (total / contextWindow) * 100 : numberOrUndefined(usage.usage_percent ?? usage.usagePercent) ?? 0,
        ttft_ms: numberOrUndefined(usage.ttft_ms ?? usage.ttft),
        total_duration_ms: numberOrUndefined(usage.total_duration_ms ?? usage.totalDurationMs),
        tokens_per_second: numberOrUndefined(usage.tokens_per_second ?? usage.tokensPerSecond),
    };
}

export function useAgentSession(sessionId: string) {
    const modelRouting = ref<ModelRouting>(loadModelRouting());
    watch(modelRouting, (value) => {
        try { localStorage.setItem(MODEL_ROUTING_STORAGE_KEY, JSON.stringify(value)); } catch { /* 存储不可用时继续使用当前会话配置 */ }
    }, { deep: true });
    const logContext = ref(false);
    const goalInput = ref("");
    const currentGoal = ref("");
    const sessionTitle = ref("");
    const runState = ref<AgentRunState>("idle");
    const statusDescription = ref(AGENT_RUN_STATE_CATALOG.idle.description);
    const isRunning = ref(false);
    const pauseReason = ref("");
    const humanForm = ref<AgentHumanForm | null>(null);
    const humanInput = ref("");
    const pendingReplacementGoal = ref("");
    const isSendingHuman = ref(false);
    const stopRequested = ref(false);
    const elapsedSeconds = ref(0);
    const runStartedAt = ref<number | null>(null);

    // 结构化对话状态（解决反复添加和闪烁的关键数据结构）
    // 当前实时运行与已完成回合分开保存；新任务只重置实时数组。
    const steps = ref<AgentStepItem[]>([]);
    const historyRuns = ref<HistoryRunSnapshot[]>([]);
    const displayedSteps = computed(() => [...historyTurns.value.flatMap((turn) => turn.steps), ...steps.value]);
    const selectedStepId = ref<string | null>(null);
    const thought = ref("");
    const isThinking = ref(false);
    const thinkingCollapsed = ref(false);
    const thinkingStartTime = ref<number | null>(null);
    const thinkingDurationMs = ref(0);
    const logs = ref<AgentLogEntry[]>([]);
    const humanInterventions = ref<Array<{ message: string; timestamp: number }>>([]);

    const tokenUsage = ref<AgentTokenUsage | null>(null);
    const liveSpeed = ref<number | null>(null);
    const liveTtft = ref<number | null>(null);
    const activeStepNumber = ref<number>(1);
    const actionPhaseText = ref<string>("");
    const lastRunTelemetry = ref<AgentSessionTelemetry | null>(null);
    const completionMessage = ref("");
    const streamingSummary = ref("");
    const { conversationMessages, isConversationLoading, loadConversation } = usePersistedAgentConversation(
        sessionId,
        (message) => appendLog(`[会话历史加载失败] ${message}`),
    );
    const historyTurns = computed<AgentHistoryTurn[]>(() => historyRuns.value.map((run) => ({
        ...run,
        messages: messagesForRun(run).map((message) => ({ ...message, timestamp: traceTimestamp(message.created_at) })),
    })));
    // 仅保留没有任何运行轨迹可归属的早期旧消息；有轨迹的消息由对应回合原位显示。
    const legacyConversationMessages = computed(() => conversationMessages.value.filter((message) => !historyRuns.value.some((run) => isMessageInsideRun(message, run))));
    const finishOutput = ref<Record<string, unknown> | null>(null);
    const hasError = ref(false);
    // 每次运行只有首次终态可改变 UI；终态后的异步流事件必须被丢弃，
    // 避免“已完成”之后又显示思考中、工具执行中或新的失败步骤。
    const runTerminal = ref(true);
    const scrollContainerElement = ref<HTMLElement | null>(null);

    const {
        resources,
        isResourcesLoading,
        resourceLoadError,
        selectedResource,
        isRecordsLoading,
        recordLoadError,
        recordPage,
        mergeContextResources,
        refreshResources,
        scheduleResourceRefresh,
        openResourcePreview,
        closeResourcePreview,
        loadPreviousRecordPage,
        loadNextRecordPage,
        clearResources,
        disposeResources,
    } = useAgentTaskResources(sessionId);

    let elapsedTimer: ReturnType<typeof setInterval> | null = null;
    let unlisteners: UnlistenFn[] = [];
    let logSequence = 0;

    const stateMeta = computed(() => AGENT_RUN_STATE_CATALOG[runState.value]);
    const isWaitingForHuman = computed(() => runState.value === "waiting_for_human");
    const elapsedLabel = computed(() => {
        const minutes = Math.floor(elapsedSeconds.value / 60);
        const seconds = elapsedSeconds.value % 60;
        return `${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
    });
    const canStart = computed(() => Boolean(!isRunning.value && goalInput.value.trim()));
    const selectedStep = computed(() => displayedSteps.value.find((item) => item.id === selectedStepId.value) || null);
    const completedToolCount = computed(() => steps.value.filter((item) => item.status === "succeeded").length);
    const failedToolCount = computed(() => steps.value.filter((item) => item.status === "failed").length);

    const sessionTelemetry = computed<AgentSessionTelemetry>(() => {
        const currentSteps = steps.value;
        const lastStepWithDuration = [...currentSteps].reverse().find(s => s.durationMs !== undefined && s.durationMs > 0);
        const totalToolMs = currentSteps.reduce((acc, s) => acc + (s.durationMs ?? 0), 0);
        const usage = tokenUsage.value;

        const promptTokens = usage?.prompt_tokens ?? 0;
        const completionTokens = usage?.completion_tokens ?? 0;
        const totalTokens = usage?.total_tokens ?? 0;
        const cachedTokens = usage?.cached_tokens ?? 0;
        const cacheHitRate = usage?.cache_hit_rate ?? (promptTokens > 0 ? (cachedTokens / promptTokens) * 100 : 0);
        const contextWindow = usage?.context_window ?? 0;
        const contextUsagePercent = usage?.usage_percent ?? (contextWindow > 0 ? (totalTokens / contextWindow) * 100 : 0);

        const currentStepCount = Math.max(
            activeStepNumber.value,
            currentSteps.length,
            (lastRunTelemetry.value?.stepCount ?? 1)
        );

        if (isRunning.value || currentSteps.length > 0 || totalTokens > 0) {
            return {
                stepCount: currentStepCount,
                completedToolCount: completedToolCount.value,
                failedToolCount: failedToolCount.value,
                elapsedLabel: elapsedLabel.value || (lastRunTelemetry.value?.elapsedLabel ?? "00:00"),
                actionPhase: actionPhaseText.value,
                lastTtftMs: isRunning.value ? (liveTtft.value ?? usage?.ttft_ms ?? lastRunTelemetry.value?.lastTtftMs) : (usage?.ttft_ms ?? lastRunTelemetry.value?.lastTtftMs),
                lastToolDurationMs: lastStepWithDuration?.durationMs ?? lastRunTelemetry.value?.lastToolDurationMs,
                totalToolDurationMs: totalToolMs || (lastRunTelemetry.value?.totalToolDurationMs ?? 0),
                tokensPerSecond: isRunning.value ? (liveSpeed.value ?? usage?.tokens_per_second ?? lastRunTelemetry.value?.tokensPerSecond) : (usage?.tokens_per_second ?? lastRunTelemetry.value?.tokensPerSecond),
                promptTokens: promptTokens || (lastRunTelemetry.value?.promptTokens ?? 0),
                completionTokens: completionTokens || (lastRunTelemetry.value?.completionTokens ?? 0),
                totalTokens: totalTokens || (lastRunTelemetry.value?.totalTokens ?? 0),
                cachedTokens: cachedTokens || (lastRunTelemetry.value?.cachedTokens ?? 0),
                cacheHitRate: cacheHitRate || (lastRunTelemetry.value?.cacheHitRate ?? 0),
                contextWindow: contextWindow || (lastRunTelemetry.value?.contextWindow ?? 0),
                contextUsagePercent: contextUsagePercent || (lastRunTelemetry.value?.contextUsagePercent ?? 0),
            };
        }

        if (lastRunTelemetry.value) {
            return lastRunTelemetry.value;
        }

        return {
            stepCount: 0,
            completedToolCount: 0,
            failedToolCount: 0,
            elapsedLabel: "00:00",
            actionPhase: actionPhaseText.value,
            totalToolDurationMs: 0,
            promptTokens: 0,
            completionTokens: 0,
            totalTokens: 0,
            cachedTokens: 0,
            cacheHitRate: 0,
            contextWindow: 0,
            contextUsagePercent: 0,
        };
    });

    const finalSummary = computed(() => {
        const output = finishOutput.value || {};
        const summary = toText(output.summary || output.message || output.result || output.content || "");
        const details = toText(output.details || "");
        if (summary && details && summary !== details) {
            return `${summary}\n\n${details}`;
        }
        return summary || details || streamingSummary.value || "";
    });
    const finalArtifacts = computed(() => {
        const output = finishOutput.value || {};
        const candidates = [output.artifacts, output.files, output.created_files, output.paths];
        return candidates
            .flatMap((candidate) => Array.isArray(candidate) ? candidate.map((value) => toText(value)).filter(Boolean) : [])
            .filter((path) => !isLegacySystemSummary(path));
    });
    function traceStepStatus(value: unknown): AgentToolStatus {
        const normalized = toText(value).toLowerCase();
        if (normalized === "failed" || normalized === "error") return "failed";
        if (normalized === "succeeded" || normalized === "success" || normalized === "complete") return "succeeded";
        if (normalized === "running") return "running";
        return "pending";
    }
    function traceTimestamp(value: unknown, fallback = Date.now()) {
        const parsed = Date.parse(toText(value));
        return Number.isFinite(parsed) ? parsed : fallback;
    }
    function isLegacySystemSummary(path: string) {
        return /(?:^|[/\\])task_summaries(?:[/\\]|$)/i.test(path) || /task[_ -]?summary.*\.md$/i.test(path);
    }
    function artifactPaths(value: Record<string, unknown>) {
        return [value.artifacts, value.files, value.created_files, value.paths]
            .flatMap((candidate) => Array.isArray(candidate) ? candidate.map((item) => toText(item)).filter(Boolean) : [])
            .filter((path) => !isLegacySystemSummary(path));
    }
    function normalizeRunTrace(value: unknown): HistoryRunSnapshot {
        const trace = asRecord(value);
        const runId = toText(trace.id) || "unknown-run";
        const goal = toText(trace.goal) || "历史任务";
        const startedAt = traceTimestamp(trace.started_at);
        const updatedAt = traceTimestamp(trace.updated_at, startedAt);
        const completedAt = traceTimestamp(trace.completed_at, updatedAt);
        const completion = asRecord(trace.completion);
        const rawSteps = Array.isArray(trace.steps) ? trace.steps : [];
        const steps = rawSteps.map((rawStep, index): AgentStepItem => {
            const step = asRecord(rawStep);
            const stepId = numberOrUndefined(step.id ?? step.step_id) ?? index;
            const tool = toText(step.tool || step.action) || "action";
            const sourcePlanId = toText(step.plan_id || step.planId) || "plan-default";
            const planTitle = toText(step.plan_title || step.planTitle) || goal;
            const params = step.command ?? step.params;
            const result = step.output ?? step.result;
            return {
                id: `history-${runId}-step-${stepId}-${index}`,
                stepId,
                description: toText(step.description) || "已执行步骤",
                planId: `history-run-${runId}-${sourcePlanId}`,
                planTitle,
                modelTier: toText(step.model_tier || step.modelTier) || undefined,
                reflection: toText(step.reflection) || undefined,
                thought: toText(step.thought) || undefined,
                explanation: toText(step.explanation) || undefined,
                tool,
                action: toText(step.action) || tool,
                source: sourceFrom(step.route, tool),
                status: traceStepStatus(step.status),
                params,
                result,
                rawParams: params,
                rawResult: result,
                timestamp: numberOrUndefined(step.timestamp) ?? startedAt,
            };
        });
        return {
            id: runId,
            goal,
            status: toText(trace.status) || "running",
            steps,
            summary: (() => {
                const s = toText(completion.summary || completion.message || completion.result || completion.content);
                const d = toText(completion.details || "");
                if (s && d && s !== d) return `${s}\n\n${d}`;
                return s || d || "";
            })(),
            artifacts: artifactPaths(completion),
            startedAt,
            completedAt,
        };
    }
    function isMessageInsideRun(message: PersistedConversationMessage, run: HistoryRunSnapshot) {
        const timestamp = traceTimestamp(message.created_at, 0);
        return timestamp >= run.startedAt - 5000 && timestamp <= run.completedAt + 5000;
    }
    function messagesForRun(run: HistoryRunSnapshot) {
        const normalizedGoal = run.goal.trim();
        const normalizedSummary = run.summary.trim();
        return conversationMessages.value.filter((message) => {
            if (!isMessageInsideRun(message, run)) return false;
            const content = message.content.trim();
            if (message.role === "user" && content === normalizedGoal) return false;
            if (message.role !== "user" && normalizedSummary && content === normalizedSummary) return false;
            return true;
        });
    }
    function summaryFromConversation(run: HistoryRunSnapshot) {
        const assistantMessages = conversationMessages.value.filter((message) =>
            message.role !== "user" && isMessageInsideRun(message, run) && Boolean(message.content.trim()),
        );
        return assistantMessages.length ? assistantMessages[assistantMessages.length - 1].content.trim() : "";
    }
    function dedupeHistoryRuns(runs: HistoryRunSnapshot[]) {
        const byId = new Map<string, HistoryRunSnapshot>();
        for (const run of runs) {
            // 只要有步骤、有总结、有交付物或有明确终态（无论成功、失败还是已停止），都是真实独立历史回合，完整保留！
            if (!run.steps.length && !run.summary && !run.artifacts.length && !["succeeded", "failed", "cancelled"].includes(run.status)) {
                continue;
            }
            byId.set(run.id, run);
        }
        return [...byId.values()].sort((a, b) => a.startedAt - b.startedAt || a.id.localeCompare(b.id));
    }
    async function loadRunTraces() {
        try {
            const traces = await invoke<unknown[]>("get_session_run_traces", { sessionId });
            const normalized = (Array.isArray(traces) ? traces : []).map((trace) => normalizeRunTrace(trace));
            normalized.forEach((run) => {
                if (!run.summary) run.summary = summaryFromConversation(run);
            });
            historyRuns.value = dedupeHistoryRuns(normalized);
        } catch (error) {
            appendLog(`[过程历史加载失败] ${toText(error)}`);
        }
    }
    function setScrollElement(element: HTMLElement | null) {
        scrollContainerElement.value = element;
    }

    function autoScrollToBottom(smooth = true) {
        nextTick(() => {
            const el = scrollContainerElement.value;
            if (el) {
                el.scrollTo({
                    top: el.scrollHeight,
                    behavior: smooth ? "smooth" : "auto",
                });
            }
        });
    }

    function ensureModelRouting(availableModels: AgentModelOption[]) {
        if (!availableModels || availableModels.length === 0) return;
        const first = availableModels[0]?.id || "";
        const findModel = (predicate: (m: AgentModelOption) => boolean) => {
            const found = availableModels.find(predicate);
            return found ? found.id : first;
        };

        if (!modelRouting.value.pro) {
            const deepSeekPro = availableModels.find(m => /deepseek.*v4.*pro|v4.*pro.*deepseek/i.test(`${m.display_name} ${m.name}`));
            modelRouting.value.pro = deepSeekPro?.id || findModel(m => {
                const label = `${m.display_name} ${m.name}`;
                return !/flash|mini|haiku|speed|turbo/i.test(label)
                    && /pro|opus|claude-3-5|gpt-4o|deepseek/i.test(label);
            });
        }
        if (!modelRouting.value.flash) {
            const deepSeekFlash = availableModels.find(m => /deepseek.*v4.*flash|v4.*flash.*deepseek/i.test(`${m.display_name} ${m.name}`));
            modelRouting.value.flash = deepSeekFlash?.id || findModel(m => /flash|mini|haiku|speed|turbo/i.test(`${m.display_name} ${m.name}`));
        }
        if (!modelRouting.value.vision) {
            modelRouting.value.vision = findModel(m => /vision|vl|multimodal|gpt-4o/i.test(`${m.display_name} ${m.name}`));
        }
    }

    function setRunState(nextState: AgentRunState, description?: string) {
        runState.value = nextState;
        statusDescription.value = description || AGENT_RUN_STATE_CATALOG[nextState].description;
        const terminal = ["succeeded", "failed", "cancelled"].includes(nextState);
        if (terminal) {
            runTerminal.value = true;
            isRunning.value = false;
            stopElapsedTimer();
            if (isThinking.value) {
                isThinking.value = false;
                thinkingCollapsed.value = true;
            }
        }
    }

    function startElapsedTimer() {
        stopElapsedTimer();
        elapsedTimer = setInterval(() => {
            if (runStartedAt.value) {
                elapsedSeconds.value = Math.max(0, Math.floor((Date.now() - runStartedAt.value) / 1000));
            }
            if (isThinking.value && thinkingStartTime.value) {
                thinkingDurationMs.value = Date.now() - thinkingStartTime.value;
            }
        }, 1000);
    }

    function stopElapsedTimer() {
        if (elapsedTimer) clearInterval(elapsedTimer);
        elapsedTimer = null;
    }

    function appendLog(rawMessage: unknown) {
        const message = toText(typeof rawMessage === "string" ? rawMessage : asRecord(rawMessage).message || rawMessage);
        if (!message) return;
        let level: AgentLogEntry["level"] = "info";
        const lower = message.toLowerCase();
        if (/error|failed|失败|错误|exception/.test(lower)) level = "error";
        else if (/warn|警告|retry|重试|熔断/.test(lower)) level = "warn";
        else if (/\[网关\]|route|路由/.test(lower)) level = "route";
        else if (/🧠|思考|反思/.test(lower)) level = "thought";

        logs.value.push({
            id: `log-${Date.now()}-${logSequence++}`,
            message,
            level,
            timestamp: Date.now(),
        });
        if (logs.value.length > MAX_LOGS) {
            logs.value.splice(0, logs.value.length - MAX_LOGS);
        }
    }

    function handleRunState(payload: Record<string, unknown>) {
        if (runTerminal.value) return;
        const state = normalizeState(payload.state);
        const message = toText(payload.message || payload.description);
        setRunState(state, message || undefined);
        if (state === "waiting_for_human") {
            pauseReason.value = message || AGENT_RUN_STATE_CATALOG.waiting_for_human.description;
        } else {
            pauseReason.value = "";
        }
        if (state === "failed" || state === "cancelled") {
            hasError.value = state === "failed";
        }
        appendLog(`[状态变更] ${AGENT_RUN_STATE_CATALOG[state].label}${message ? `: ${message}` : ""}`);
    }

    function handleToolEvent(payload: Record<string, unknown>) {
        if (runTerminal.value) return;
        const eventType = String(payload.event_type || payload.type || "started").toLowerCase();
        const stepId = numberOrUndefined(payload.step_id) ?? 0;
        const name = toText(payload.tool || payload.name) || "内置工具";
        const action = toText(payload.action || payload.operation) || "执行操作";
        const durationMs = numberOrUndefined(payload.duration_ms);
        const params = payload.params_summary ?? payload.params;
        const result = payload.result_summary ?? payload.result;
        const rawParams = payload.params ?? payload.params_summary;
        const rawResult = payload.result ?? payload.result_summary;

        const status: AgentToolStatus = eventType === "succeeded" || payload.success === true
            ? "succeeded"
            : eventType === "failed" || payload.success === false
                ? "failed"
                : eventType === "pending"
                    ? "pending"
                    : "running";

        // 查找最新的匹配步骤：若前一个步骤已失败/成功，而新事件重新下发，保留历史错误尝试并追加新条目
        let targetIdx = -1;
        for (let i = steps.value.length - 1; i >= 0; i--) {
            if (steps.value[i].stepId === stepId) {
                targetIdx = i;
                break;
            }
        }

        const isPreviousCompleted = targetIdx >= 0 && (steps.value[targetIdx].status === "failed" || steps.value[targetIdx].status === "succeeded");
        if (targetIdx >= 0 && (!isPreviousCompleted || status === "failed" || status === "succeeded")) {
            const current = steps.value[targetIdx];
            steps.value[targetIdx] = {
                ...current,
                tool: name || current.tool,
                action: action || current.action,
                source: sourceFrom(payload.route, name),
                status,
                durationMs: durationMs ?? current.durationMs,
                params: params !== undefined ? params : current.params,
                result: result !== undefined ? result : current.result,
                rawParams: rawParams !== undefined ? rawParams : current.rawParams,
                rawResult: rawResult !== undefined ? rawResult : current.rawResult,
            };
        } else {
            const newStep: AgentStepItem = {
                id: `step-${stepId}-${Date.now()}`,
                stepId,
                description: toText(payload.description) || `${name}: ${action}`,
                planId: toText(payload.plan_id) || "plan-default",
                planTitle: toText(payload.plan_title) || currentGoal.value || "执行任务",
                modelTier: toText(payload.model_tier) || undefined,
                tool: name,
                action,
                source: sourceFrom(payload.route, name),
                status,
                durationMs,
                params,
                result,
                rawParams,
                rawResult,
                timestamp: toTimestamp(payload.timestamp),
            };
            steps.value.push(newStep);
            if (!selectedStepId.value) {
                selectedStepId.value = newStep.id;
            }
        }

        appendLog(`[工具执行] ${name}/${action} (${status})${durationMs ? ` ${durationMs}ms` : ""}`);
    }

    function completeRun(payload: Record<string, unknown>, succeeded: boolean) {
        if (runTerminal.value) return;
        const message = toText(payload.message) || (succeeded ? "任务已全部完成。" : "任务未能顺利完成。");
        completionMessage.value = message;
        hasError.value = !succeeded;

        // 终态前归档本轮全量遥测数据
        const currentSteps = steps.value;
        const lastStepWithDuration = [...currentSteps].reverse().find(s => s.durationMs !== undefined && s.durationMs > 0);
        const totalToolMs = currentSteps.reduce((acc, s) => acc + (s.durationMs ?? 0), 0);
        const usage = tokenUsage.value;
        const promptTokens = usage?.prompt_tokens ?? 0;
        const completionTokens = usage?.completion_tokens ?? 0;
        const totalTokens = usage?.total_tokens ?? 0;
        const cachedTokens = usage?.cached_tokens ?? 0;
        const cacheHitRate = usage?.cache_hit_rate ?? (promptTokens > 0 ? (cachedTokens / promptTokens) * 100 : 0);
        const contextWindow = usage?.context_window ?? 0;
        const contextUsagePercent = usage?.usage_percent ?? (contextWindow > 0 ? (totalTokens / contextWindow) * 100 : 0);

        lastRunTelemetry.value = {
            stepCount: currentSteps.length || (historyRuns.value.length ? historyRuns.value[historyRuns.value.length - 1].steps.length : 0),
            completedToolCount: completedToolCount.value,
            failedToolCount: failedToolCount.value,
            elapsedLabel: elapsedLabel.value,
            actionPhase: succeeded ? "已完成" : "异常结束",
            lastTtftMs: usage?.ttft_ms,
            lastToolDurationMs: lastStepWithDuration?.durationMs,
            totalToolDurationMs: totalToolMs,
            tokensPerSecond: usage?.tokens_per_second,
            promptTokens,
            completionTokens,
            totalTokens,
            cachedTokens,
            cacheHitRate,
            contextWindow,
            contextUsagePercent,
        };

        const finish = asRecord(payload.finish);
        if (Object.keys(finish).length) {
            finishOutput.value = { ...finish, screenshot: payload.screenshot };
        } else if (payload.output) {
            const output = toText(payload.output);
            try {
                const parsed = JSON.parse(output);
                finishOutput.value = typeof parsed === "object" && parsed ? { ...asRecord(parsed), screenshot: payload.screenshot } : { summary: output };
            } catch {
                finishOutput.value = { summary: output };
            }
        }
        scheduleResourceRefresh();
        setRunState(succeeded ? "succeeded" : "failed", message);
        appendLog(`[任务结束] ${succeeded ? "✅ 成功" : "❌ 失败"}: ${message}`);
        autoScrollToBottom();
    }

    function handleProgress(payload: Record<string, unknown>) {
        if (runTerminal.value) return;
        const usage = normalizeUsage(payload);
        if (usage) tokenUsage.value = usage;

        const eventType = String(payload.event_type || payload.type || payload.stage || "").toLowerCase();

        // 动作阶段事件（如同步 MCP、装配系统提示词、等待首字）
        if (eventType === "action_phase") {
            const desc = toText(payload.description || payload.message);
            if (desc) {
                actionPhaseText.value = desc;
                appendLog(`[执行阶段] ${desc}`);
            }
            return;
        }

        // 大模型提炼的全局精炼任务标题更新
        if (eventType === "session_title_updated" && payload.title) {
            const newTitle = toText(payload.title);
            if (newTitle) {
                sessionTitle.value = newTitle;
            }
            return;
        }

        // 思考与流式内容事件（实时抓取每 chunk 吐字速率和首字 TTFT）
        if (payload.step_id !== undefined && payload.step_id !== null) {
            const parsedStep = Number(payload.step_id);
            if (!isNaN(parsedStep)) {
                activeStepNumber.value = Math.max(activeStepNumber.value, parsedStep + 1);
            }
        }
        if (eventType === "thinking" || eventType === "content_stream" || eventType === "summary_stream") {
            if (payload.live_speed !== undefined) {
                liveSpeed.value = Number(payload.live_speed);
            }
            if (payload.ttft_ms !== undefined) {
                liveTtft.value = Number(payload.ttft_ms);
            }
        }

        // 实时捕获增量流式 Markdown 总结
        if (eventType === "summary_stream" || payload.streaming_summary) {
            const content = toText(payload.content || payload.streaming_summary);
            if (content) {
                streamingSummary.value = content;
            }
            if (eventType === "summary_stream") {
                return;
            }
        }

        if (eventType === "thinking") {
            const content = toText(payload.content);
            const done = payload.done === true;
            if (content) {
                thought.value = content;
            }
            if (done) {
                isThinking.value = false;
                thinkingCollapsed.value = true;
                if (thinkingStartTime.value) {
                    thinkingDurationMs.value = Date.now() - thinkingStartTime.value;
                }
            } else {
                if (!isThinking.value) {
                    isThinking.value = true;
                    thinkingStartTime.value = Date.now();
                    thinkingCollapsed.value = false;
                }
            }
            return;
        }

        // 工具事件直接走专用 handler
        if (eventType.includes("tool")) {
            handleToolEvent(payload);
            return;
        }

        // ask_human 暂停：只接收受控 form JSON，未知结构退回为普通文本回答。
        if (eventType === "agent_paused") {
            const message = toText(payload.message) || "Agent 正在等待你的输入。";
            pauseReason.value = message;
            humanForm.value = normalizeHumanForm(payload.form, message);
            return;
        }

        // 新规划步骤 step_new
        if (eventType === "step_new") {
            const rawStep = asRecord(payload.step);
            const stepId = numberOrUndefined(rawStep.id) ?? steps.value.length;
            const description = toText(rawStep.description) || "执行新步骤";
            const tool = toText(rawStep.tool) || "action";
            const command = rawStep.command;
            const thoughtText = toText(rawStep.thought);
            const reflection = toText(rawStep.reflection);

            let lastIdx = -1;
            for (let i = steps.value.length - 1; i >= 0; i--) {
                if (steps.value[i].stepId === stepId) {
                    lastIdx = i;
                    break;
                }
            }
            const isRetry = lastIdx >= 0 && (steps.value[lastIdx].status === "failed" || steps.value[lastIdx].status === "succeeded");

            const stepItem: AgentStepItem = {
                id: isRetry ? `step-${stepId}-attempt-${steps.value.length}` : `step-${stepId}`,
                stepId,
                description,
                planId: toText(rawStep.plan_id) || "plan-default",
                planTitle: toText(rawStep.plan_title) || currentGoal.value || "执行任务",
                modelTier: toText(rawStep.model_tier) || undefined,
                reflection,
                thought: thoughtText,
                tool,
                action: toText(rawStep.action) || tool,
                source: sourceFrom("", tool),
                status: "pending",
                params: rawStep.params ?? command,
                rawParams: rawStep.params ?? command,
                timestamp: Date.now(),
            };

            if (lastIdx >= 0 && !isRetry) {
                steps.value[lastIdx] = { ...steps.value[lastIdx], ...stepItem };
            } else {
                steps.value.push(stepItem);
            }
            if (!selectedStepId.value) {
                selectedStepId.value = stepItem.id;
            }
            if (!sessionTitle.value && stepItem.planTitle && stepItem.planTitle !== "执行任务" && stepItem.planTitle !== "分析项目环境") {
                sessionTitle.value = stepItem.planTitle;
            }
            if (tool !== "finish") {
                streamingSummary.value = "";
            }
            appendLog(`[规划新步骤 #${stepId + 1}] ${description}`);
            autoScrollToBottom();
            return;
        }

        // 步骤开始 step_start
        if (eventType === "step_start") {
            const stepId = numberOrUndefined(payload.step_id) ?? 0;
            const existing = steps.value.find(s => s.stepId === stepId);
            if (existing) {
                existing.status = "running";
                if (payload.description) existing.description = toText(payload.description);
            }
            appendLog(`[步骤开始 #${stepId + 1}] ${existing?.description || ""}`);
            return;
        }

        // 步骤完成 step_done
        if (eventType === "step_done") {
            const stepId = numberOrUndefined(payload.step_id) ?? 0;
            const existing = steps.value.find(s => s.stepId === stepId);
            if (existing) {
                existing.status = "succeeded";
                if (payload.output) {
                    existing.result = payload.output;
                    existing.rawResult = payload.output;
                }
            }
            appendLog(`[步骤完成 #${stepId + 1}] ${existing?.description || ""}`);
            autoScrollToBottom();
            return;
        }

        // 步骤异常 step_error (单步报错重试，不是整个任务终态)
        if (eventType === "step_error") {
            const stepId = numberOrUndefined(payload.step_id) ?? 0;
            const existing = steps.value.find(s => s.stepId === stepId);
            if (existing) {
                existing.status = "failed";
                if (payload.output) {
                    existing.result = payload.output;
                    existing.rawResult = payload.output;
                }
            }
            appendLog(`[步骤遇到错误 #${stepId + 1}] ${existing?.description || ""}`);
            autoScrollToBottom();
            return;
        }

        // 任务终态：完成 complete / 终态失败 fatal_error
        if (eventType === "complete" || eventType === "finish") {
            const isSuccess = payload.success !== false;
            completeRun(payload, isSuccess);
            return;
        }

        if (eventType === "fatal_error" || eventType === "task_failed") {
            completeRun(payload, false);
            return;
        }

        const state = payload.state ? normalizeState(payload.state) : undefined;
        if (state) setRunState(state, toText(payload.message || payload.description) || undefined);
    }

    function handleLog(payload: unknown) {
        if (runTerminal.value) return;
        appendLog(payload);
    }

    function handleContext(payload: unknown) {
        if (runTerminal.value) return;
        const data = asRecord(payload);
        const candidates = [data.resources, data.resource, asRecord(data.context).resources, asRecord(data.data).resources]
            .flatMap((entry) => Array.isArray(entry) ? entry : entry ? [entry] : []);
        const nextResources = mergeContextResources(candidates);
        if (nextResources.length) scheduleResourceRefresh();
        const summary = toText(data.summary || data.message || asRecord(data.context).summary);
        if (summary) {
            appendLog(`[上下文更新] ${summary}`);
        }
    }

    function clearSession() {
        runTerminal.value = true;
        steps.value = [];
        selectedStepId.value = null;
        thought.value = "";
        isThinking.value = false;
        thinkingCollapsed.value = false;
        thinkingStartTime.value = null;
        thinkingDurationMs.value = 0;
        logs.value = [];
        humanInterventions.value = [];
        clearResources();
        liveSpeed.value = null;
        liveTtft.value = null;
        activeStepNumber.value = 1;
        actionPhaseText.value = "";
        completionMessage.value = "";
        streamingSummary.value = "";
        finishOutput.value = null;
        hasError.value = false;
        currentGoal.value = "";
        pauseReason.value = "";
        humanForm.value = null;
        humanInput.value = "";
        stopRequested.value = false;
        elapsedSeconds.value = 0;
        runStartedAt.value = null;
        setRunState("idle");
    }

    async function runAgent(nextGoal?: string) {
        const requestedGoal = (nextGoal ?? goalInput.value).trim();
        if (!requestedGoal || isRunning.value) return;
        await loadRunTraces();
        clearSession();
        runTerminal.value = false;
        currentGoal.value = requestedGoal;
        if (!nextGoal) goalInput.value = "";
        runStartedAt.value = Date.now();
        isRunning.value = true;
        setRunState("preparing", "正在建立任务会话并加载模型路由。");
        startElapsedTimer();
        autoScrollToBottom();
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
            appendLog(`[启动异常] ${message}`);
        } finally {
            isRunning.value = false;
            isThinking.value = false;
            stopElapsedTimer();
            const queuedGoal = pendingReplacementGoal.value;
            await loadConversation();
            await loadRunTraces();
            const newest = historyRuns.value[historyRuns.value.length - 1];
            if (newest && newest.goal === currentGoal.value && ["succeeded", "failed", "cancelled"].includes(newest.status)) {
                clearSession();
                autoScrollToBottom(false);
            }
            if (queuedGoal) {
                pendingReplacementGoal.value = "";
                await nextTick();
                void runAgent(queuedGoal);
            }
        }
    }
    async function requestStop() {
        if (!isRunning.value || stopRequested.value) return;
        stopRequested.value = true;
        isThinking.value = false;
        try {
            await invoke("cancel_agent_run", { sessionId });
            appendLog("[停止请求] 将在模型或工具的安全边界停止当前任务。");
        } catch (error) {
            stopRequested.value = false;
            appendLog(`[停止指令失败] ${toText(error)}`);
        }
    }
    async function answerHuman(answer: string) {
        const message = answer.trim();
        if (!message || isSendingHuman.value) return;
        isSendingHuman.value = true;
        try {
            await invoke("send_human_intervention", { sessionId, message });
            humanInput.value = "";
            pauseReason.value = "";
            humanForm.value = null;
            setRunState("executing", "已收到人工回答，等待 Agent 继续执行。");
            appendLog(`[人工回答] ${message}`);
            autoScrollToBottom();
        } catch (error) {
            appendLog(`[人工回答失败] ${toText(error)}`);
        } finally {
            isSendingHuman.value = false;
        }
    }
    async function sendHumanInput() {
        const message = humanInput.value.trim();
        if (!message || isSendingHuman.value) return;
        isSendingHuman.value = true;
        try {
            if (isWaitingForHuman.value) {
                // ask_human 的回答属于当前步骤的工具结果，不新增对话消息。
                await invoke("send_human_intervention", { sessionId, message });
                humanInput.value = "";
                pauseReason.value = "";
                humanForm.value = null;
                setRunState("executing", "已收到人工回答，等待 Agent 继续执行。");
                appendLog("[人工回答] 已写入当前 ask_human 步骤。");
            } else {
                // 运行中输入代表新任务；旧回合在后端安全边界取消，随后启动独立新树。
                pendingReplacementGoal.value = message;
                await invoke("replace_agent_goal", { sessionId, goal: message });
                humanInput.value = "";
                appendLog(`[目标替换] 已请求新任务：${message}`);
            }
            autoScrollToBottom();
        } catch (error) {
            pendingReplacementGoal.value = "";
            appendLog(`[输入发送失败] ${toText(error)}`);
        } finally {
            isSendingHuman.value = false;
        }
    }
    function selectStep(id: string) {
        selectedStepId.value = id;
    }

    onMounted(async () => {
        await loadConversation();
        await loadRunTraces();
        void refreshResources();
        const register = async (channel: string, handler: (payload: unknown) => void) => {
            try {
                unlisteners.push(await listen<unknown>(channel, (event) => handler(event.payload)));
            } catch (error) {
                appendLog(`[监听失败] ${channel}: ${toText(error)}`);
            }
        };
        await register(`agent-run-state-${sessionId}`, (payload) => handleRunState(asRecord(payload)));
        await register(`agent-progress-${sessionId}`, (payload) => handleProgress(asRecord(payload)));
        await register(`agent-log-${sessionId}`, handleLog);
        await register(`agent-context-${sessionId}`, handleContext);
        await register(`agent-tool-event-${sessionId}`, (payload) => handleToolEvent(asRecord(payload)));
    });

    onUnmounted(() => {
        stopElapsedTimer();
        disposeResources();
        unlisteners.forEach((unlisten) => unlisten());
        unlisteners = [];
    });

    async function submitHumanForm(value: unknown) {
        if (!humanForm.value || !isWaitingForHuman.value) return;
        const response = {
            type: "human_form_response",
            kind: humanForm.value.kind,
            question: humanForm.value.title,
            value,
        };
        humanInput.value = JSON.stringify(response);
        await sendHumanInput();
    }
    return {
        modelRouting,
        logContext,
        goalInput,
        currentGoal,
        sessionTitle,
        runState,
        statusDescription,
        isRunning,
        pauseReason,
        humanForm,
        humanInput,
        isSendingHuman,
        stopRequested,
        elapsedSeconds,
        elapsedLabel,
        // 对话流只能接收当前回合步骤；历史回合通过 historyTurns 自行渲染，绝不混入新回合。
        steps,
        allSteps: displayedSteps,
        historyTurns,
        legacyConversationMessages,
        selectedStepId,
        selectedStep,
        thought,
        isThinking,
        thinkingCollapsed,
        thinkingDurationMs,
        logs,
        humanInterventions,
        tokenUsage,
        sessionTelemetry,
        completionMessage,
        streamingSummary,
        conversationMessages,
        isConversationLoading,
        hasError,
        stateMeta,
        isWaitingForHuman,
        canStart,
        completedToolCount,
        failedToolCount,
        finalSummary,
        finalArtifacts,
        resources,
        isResourcesLoading,
        resourceLoadError,
        selectedResource,
        isRecordsLoading,
        recordLoadError,
        recordPage,
        setScrollElement,
        ensureModelRouting,
        runAgent,
        requestStop,
        answerHuman,
        sendHumanInput,
        submitHumanForm,
        clearSession,
        loadConversation,
        loadRunTraces,
        selectStep,
        refreshResources,
        openResourcePreview,
        closeResourcePreview,
        loadPreviousRecordPage,
        loadNextRecordPage,
    };
}
