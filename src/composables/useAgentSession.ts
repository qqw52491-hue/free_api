import { computed, nextTick, onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useAgentTaskResources } from "./useAgentTaskResources";
import {
    AGENT_RUN_STATE_CATALOG,
    type AgentDetailSection,
    type AgentRunState,
    type AgentTimelineItem,
    type AgentTimelineKind,
    type AgentTokenUsage,
    type AgentToolCall,
    type AgentToolStatus,
} from "../components/agent/types";
import { asRecord, numberOrUndefined, shortText, toText, toTimestamp } from "../components/agent/format";

const MAX_TIMELINE_ITEMS = 160;

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

function stateTimelineKind(state: AgentRunState): AgentTimelineKind {
    if (state === "recovering") return "recovery";
    if (state === "failed" || state === "cancelled") return "error";
    if (state === "succeeded") return "success";
    if (state === "waiting_for_human") return "human";
    if (state === "planning") return "planning";
    return "system";
}

function sourceFrom(route: unknown, name: string): "内部" | "MCP" {
    return /mcp/i.test(`${toText(route)} ${name}`) ? "MCP" : "内部";
}

function normalizeUsage(payload: Record<string, unknown>): AgentTokenUsage | null {
    const usage = asRecord(payload.usage || payload.token_usage || payload.tokens);
    if (!Object.keys(usage).length) return null;
    const total = numberOrUndefined(usage.total_tokens ?? usage.total);
    const contextWindow = numberOrUndefined(usage.context_window ?? usage.contextWindow ?? usage.max_tokens);
    if (total === undefined) return null;
    return {
        prompt_tokens: numberOrUndefined(usage.prompt_tokens ?? usage.prompt) ?? 0,
        completion_tokens: numberOrUndefined(usage.completion_tokens ?? usage.completion) ?? 0,
        total_tokens: total,
        context_window: contextWindow ?? 0,
        usage_percent: contextWindow ? (total / contextWindow) * 100 : numberOrUndefined(usage.usage_percent ?? usage.usagePercent) ?? 0,
    };
}

export function useAgentSession(sessionId: string) {
    const modelRouting = ref({ pro: "", flash: "", vision: "" });
    const logContext = ref(false);
    const goalInput = ref("");
    const currentGoal = ref("");
    const runState = ref<AgentRunState>("idle");
    const statusDescription = ref(AGENT_RUN_STATE_CATALOG.idle.description);
    const isRunning = ref(false);
    const pauseReason = ref("");
    const humanInput = ref("");
    const isSendingHuman = ref(false);
    const stopRequested = ref(false);
    const elapsedSeconds = ref(0);
    const runStartedAt = ref<number | null>(null);
    const timeline = ref<AgentTimelineItem[]>([]);
    const selectedTimelineId = ref<string | null>(null);
    const tokenUsage = ref<AgentTokenUsage | null>(null);
    const completionMessage = ref("");
    const finishOutput = ref<Record<string, unknown> | null>(null);
    const hasError = ref(false);
    const activeToolItems = new Map<string, string>();
    const timelineElement = ref<HTMLElement | null>(null);
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
    let timelineSequence = 0;

    const stateMeta = computed(() => AGENT_RUN_STATE_CATALOG[runState.value]);
    const isWaitingForHuman = computed(() => runState.value === "waiting_for_human");
    const elapsedLabel = computed(() => {
        const minutes = Math.floor(elapsedSeconds.value / 60);
        const seconds = elapsedSeconds.value % 60;
        return `${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
    });
    const canStart = computed(() => Boolean(
        !isRunning.value
        && goalInput.value.trim()
        && modelRouting.value.pro
        && modelRouting.value.flash
        && modelRouting.value.vision,
    ));
    const selectedTimelineItem = computed(() => timeline.value.find((item) => item.id === selectedTimelineId.value) || null);
    const completedToolCount = computed(() => timeline.value.filter((item) => item.tool?.status === "succeeded").length);
    const failedToolCount = computed(() => timeline.value.filter((item) => item.tool?.status === "failed").length);
    const finalSummary = computed(() => {
        const output = finishOutput.value || {};
        return toText(output.summary || output.message || output.result || output.content || "");
    });
    const finalArtifacts = computed(() => {
        const output = finishOutput.value || {};
        const candidates = [output.artifacts, output.files, output.created_files, output.paths];
        return candidates.flatMap((candidate) => Array.isArray(candidate) ? candidate.map((value) => toText(value)).filter(Boolean) : []);
    });

    function setTimelineElement(element: HTMLElement | null) {
        timelineElement.value = element;
    }

    function setRunState(nextState: AgentRunState, description?: string) {
        runState.value = nextState;
        statusDescription.value = description || AGENT_RUN_STATE_CATALOG[nextState].description;
        const terminal = ["succeeded", "failed", "cancelled"].includes(nextState);
        if (terminal) {
            isRunning.value = false;
            stopElapsedTimer();
        }
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

    function addTimeline(item: Omit<AgentTimelineItem, "id" | "timestamp"> & Partial<Pick<AgentTimelineItem, "id" | "timestamp">>) {
        const next: AgentTimelineItem = {
            id: item.id || `timeline-${Date.now()}-${timelineSequence++}`,
            timestamp: item.timestamp || Date.now(),
            kind: item.kind,
            title: item.title,
            detail: item.detail,
            status: item.status,
            tool: item.tool,
        };
        timeline.value.push(next);
        if (timeline.value.length > MAX_TIMELINE_ITEMS) timeline.value.splice(0, timeline.value.length - MAX_TIMELINE_ITEMS);
        if (!selectedTimelineId.value && next.tool) selectedTimelineId.value = next.id;
        nextTick(() => timelineElement.value?.scrollTo({ top: timelineElement.value.scrollHeight, behavior: "smooth" }));
        return next.id;
    }

    function updateTimeline(id: string, patch: Partial<AgentTimelineItem>) {
        const index = timeline.value.findIndex((item) => item.id === id);
        if (index < 0) return;
        const previous = timeline.value[index];
        timeline.value[index] = {
            ...previous,
            ...patch,
            tool: patch.tool ? { ...previous.tool, ...patch.tool } : previous.tool,
        };
    }

    function upsertTool(key: string, tool: AgentToolCall, detail: string, timestamp?: number) {
        const existingId = activeToolItems.get(key);
        const status = tool.status === "succeeded" ? "succeeded" : tool.status === "failed" ? "failed" : "running";
        if (existingId) {
            updateTimeline(existingId, { tool, detail, status, timestamp: timestamp || Date.now() });
            selectedTimelineId.value = existingId;
            return existingId;
        }
        const id = addTimeline({
            kind: "tool",
            title: tool.name,
            detail,
            tool,
            status,
            timestamp,
        });
        activeToolItems.set(key, id);
        selectedTimelineId.value = id;
        return id;
    }

    function handleRunState(payload: Record<string, unknown>) {
        const state = normalizeState(payload.state);
        const message = toText(payload.message || payload.description);
        setRunState(state, message || undefined);
        if (state === "waiting_for_human") pauseReason.value = message || AGENT_RUN_STATE_CATALOG.waiting_for_human.description;
        if (state !== "waiting_for_human") pauseReason.value = "";
        if (state === "failed" || state === "cancelled") hasError.value = state === "failed";
        if (message || state !== "executing") {
            addTimeline({
                kind: stateTimelineKind(state),
                title: AGENT_RUN_STATE_CATALOG[state].label,
                detail: message || AGENT_RUN_STATE_CATALOG[state].description,
                status: state === "succeeded" ? "succeeded" : state === "failed" || state === "cancelled" ? "failed" : undefined,
                timestamp: toTimestamp(payload.timestamp),
            });
        }
    }

    function handleToolEvent(payload: Record<string, unknown>) {
        const eventType = String(payload.event_type || payload.type || "started").toLowerCase();
        const name = toText(payload.tool || payload.name) || "未命名工具";
        const action = toText(payload.action || payload.operation) || "执行操作";
        const status: AgentToolStatus = eventType === "succeeded" || payload.success === true
            ? "succeeded"
            : eventType === "failed" || payload.success === false
                ? "failed"
                : eventType === "pending"
                    ? "pending"
                    : "running";
        const result = payload.result ?? payload.result_summary;
        upsertTool(`${name}:${action}`, {
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

    function completeRun(payload: Record<string, unknown>, succeeded: boolean) {
        const message = toText(payload.message) || (succeeded ? "任务已完成。" : "任务未能完成。");
        completionMessage.value = message;
        hasError.value = !succeeded;
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
        addTimeline({
            kind: succeeded ? "success" : "error",
            title: succeeded ? "任务已完成" : "任务结束：失败",
            detail: message,
            status: succeeded ? "succeeded" : "failed",
            timestamp: toTimestamp(payload.timestamp),
        });
    }

    function handleProgress(payload: Record<string, unknown>) {
        const usage = normalizeUsage(payload);
        if (usage) tokenUsage.value = usage;
        const eventType = String(payload.event_type || payload.type || payload.stage || "").toLowerCase();
        if (eventType.includes("tool")) {
            handleToolEvent(payload);
            return;
        }
        if (eventType.includes("finish") || eventType.includes("complete") || eventType.includes("success")) {
            completeRun(payload, true);
            return;
        }
        if (eventType.includes("fail") || eventType.includes("error")) {
            completeRun(payload, false);
            return;
        }
        const state = payload.state ? normalizeState(payload.state) : undefined;
        if (state) setRunState(state, toText(payload.message || payload.description) || undefined);
        const message = toText(payload.message || payload.detail || payload.content || payload.summary);
        if (!message) return;
        const kind: AgentTimelineKind = /plan|think|分析|规划/.test(eventType) ? "planning" : "system";
        addTimeline({
            kind,
            title: kind === "planning" ? "Agent 正在规划" : "运行进度",
            detail: shortText(message, 420),
            timestamp: toTimestamp(payload.timestamp),
        });
    }

    function handleLog(payload: unknown) {
        const data = asRecord(payload);
        const message = toText(typeof payload === "string" ? payload : data.message || data.log || data.content || payload);
        if (!message) return;
        const lower = message.toLowerCase();
        const kind: AgentTimelineKind = /error|failed|失败|错误/.test(lower)
            ? "error"
            : /recover|retry|重试|恢复/.test(lower)
                ? "recovery"
                : "system";
        addTimeline({
            kind,
            title: kind === "error" ? "运行日志：异常" : kind === "recovery" ? "运行日志：恢复" : "运行日志",
            detail: shortText(message, 280),
            timestamp: toTimestamp(data.timestamp),
        });
    }

    function handleContext(payload: unknown) {
        const data = asRecord(payload);
        const candidates = [data.resources, data.resource, asRecord(data.context).resources, asRecord(data.data).resources]
            .flatMap((entry) => Array.isArray(entry) ? entry : entry ? [entry] : []);
        const nextResources = mergeContextResources(candidates);
        if (nextResources.length) scheduleResourceRefresh();
        const summary = toText(data.summary || data.message || asRecord(data.context).summary);
        if (summary || nextResources.length) {
            addTimeline({
                kind: "system",
                title: "任务上下文已更新",
                detail: shortText(summary || `已更新 ${nextResources.length} 个资源。`, 280),
                timestamp: toTimestamp(data.timestamp),
            });
        }
    }

    function clearSession() {
        timeline.value = [];
        selectedTimelineId.value = null;
        clearResources();
        tokenUsage.value = null;
        completionMessage.value = "";
        finishOutput.value = null;
        hasError.value = false;
        currentGoal.value = "";
        pauseReason.value = "";
        humanInput.value = "";
        stopRequested.value = false;
        elapsedSeconds.value = 0;
        runStartedAt.value = null;
        activeToolItems.clear();
        setRunState("idle");
    }

    async function runAgent() {
        if (!canStart.value) return;
        clearSession();
        currentGoal.value = goalInput.value.trim();
        runStartedAt.value = Date.now();
        isRunning.value = true;
        setRunState("preparing", "正在建立任务会话并加载模型路由。");
        startElapsedTimer();
        addTimeline({ kind: "human", title: "你发起了一个任务", detail: shortText(currentGoal.value, 420) });
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
            addTimeline({ kind: "human", title: "你补充了说明", detail: shortText(message, 420) });
            humanInput.value = "";
            pauseReason.value = "";
            if (isWaitingForHuman.value) setRunState("executing", "已发送人工输入，等待 Agent 继续执行。");
        } catch (error) {
            addTimeline({ kind: "error", title: "人工输入发送失败", detail: toText(error) || "无法将输入发送给 Agent。", status: "failed" });
        } finally {
            isSendingHuman.value = false;
        }
    }

    function selectTimelineItem(id: string) {
        selectedTimelineId.value = id;
    }

    function detailSectionAvailable(item: AgentTimelineItem | null, section: AgentDetailSection) {
        if (!item?.tool) return false;
        return section === "params" ? item.tool.rawParams !== undefined : item.tool.rawResult !== undefined;
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
        await register(`agent-tool-event-${sessionId}`, (payload) => handleToolEvent(asRecord(payload)));
    });

    onUnmounted(() => {
        stopElapsedTimer();
        disposeResources();
        unlisteners.forEach((unlisten) => unlisten());
        unlisteners = [];
    });

    return {
        modelRouting,
        logContext,
        goalInput,
        currentGoal,
        runState,
        statusDescription,
        isRunning,
        pauseReason,
        humanInput,
        isSendingHuman,
        stopRequested,
        elapsedSeconds,
        elapsedLabel,
        timeline,
        selectedTimelineId,
        selectedTimelineItem,
        tokenUsage,
        completionMessage,
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
        setTimelineElement,
        runAgent,
        requestStop,
        sendHumanInput,
        clearSession,
        selectTimelineItem,
        detailSectionAvailable,
        refreshResources,
        openResourcePreview,
        closeResourcePreview,
        loadPreviousRecordPage,
        loadNextRecordPage,
    };
}
