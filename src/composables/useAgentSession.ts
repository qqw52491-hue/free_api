import { computed, nextTick, onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useAgentTaskResources } from "./useAgentTaskResources";
import { usePersistedAgentConversation } from "./usePersistedAgentConversation";
import {
    AGENT_RUN_STATE_CATALOG,
    type AgentLogEntry,
    type AgentModelOption,
    type AgentRunState,
    type AgentStepItem,
    type AgentTokenUsage,
    type AgentToolStatus,
} from "../components/agent/types";
import { asRecord, numberOrUndefined, toText, toTimestamp } from "../components/agent/format";

const MAX_LOGS = 600;

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

    // 结构化对话状态（解决反复添加和闪烁的关键数据结构）
    const steps = ref<AgentStepItem[]>([]);
    const selectedStepId = ref<string | null>(null);
    const thought = ref("");
    const isThinking = ref(false);
    const thinkingCollapsed = ref(false);
    const thinkingStartTime = ref<number | null>(null);
    const thinkingDurationMs = ref(0);
    const logs = ref<AgentLogEntry[]>([]);
    const humanInterventions = ref<Array<{ message: string; timestamp: number }>>([]);

    const tokenUsage = ref<AgentTokenUsage | null>(null);
    const completionMessage = ref("");
    const { conversationMessages, isConversationLoading, loadConversation } = usePersistedAgentConversation(
        sessionId,
        (message) => appendLog(`[会话历史加载失败] ${message}`),
    );
    const finishOutput = ref<Record<string, unknown> | null>(null);
    const hasError = ref(false);
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
    const selectedStep = computed(() => steps.value.find((item) => item.id === selectedStepId.value) || null);
    const completedToolCount = computed(() => steps.value.filter((item) => item.status === "succeeded").length);
    const failedToolCount = computed(() => steps.value.filter((item) => item.status === "failed").length);
    const finalSummary = computed(() => {
        const output = finishOutput.value || {};
        return toText(output.summary || output.message || output.result || output.content || "");
    });
    const finalArtifacts = computed(() => {
        const output = finishOutput.value || {};
        const candidates = [output.artifacts, output.files, output.created_files, output.paths];
        return candidates.flatMap((candidate) => Array.isArray(candidate) ? candidate.map((value) => toText(value)).filter(Boolean) : []);
    });

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
            modelRouting.value.pro = findModel(m => /pro|opus|claude-3-5|gpt-4o|deepseek/i.test(`${m.display_name} ${m.name}`));
        }
        if (!modelRouting.value.flash) {
            modelRouting.value.flash = findModel(m => /flash|mini|haiku|speed|turbo/i.test(`${m.display_name} ${m.name}`));
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

        // 原地查找或更新 step
        const existingIdx = steps.value.findIndex(s => s.stepId === stepId);
        if (existingIdx >= 0) {
            const current = steps.value[existingIdx];
            steps.value[existingIdx] = {
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
                id: `step-${stepId}`,
                stepId,
                description: toText(payload.description) || `${name}: ${action}`,
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
        const message = toText(payload.message) || (succeeded ? "任务已全部完成。" : "任务未能顺利完成。");
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
        appendLog(`[任务结束] ${succeeded ? "✅ 成功" : "❌ 失败"}: ${message}`);
        autoScrollToBottom();
    }

    function handleProgress(payload: Record<string, unknown>) {
        const usage = normalizeUsage(payload);
        if (usage) tokenUsage.value = usage;

        const eventType = String(payload.event_type || payload.type || payload.stage || "").toLowerCase();

        // 思考事件
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

        // 任务完成
        if (eventType.includes("finish") || eventType.includes("complete") || eventType.includes("success")) {
            completeRun(payload, true);
            return;
        }

        // 任务失败
        if (eventType.includes("fail") || eventType.includes("error")) {
            completeRun(payload, false);
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

            const existingIdx = steps.value.findIndex(s => s.stepId === stepId);
            const stepItem: AgentStepItem = {
                id: `step-${stepId}`,
                stepId,
                description,
                reflection,
                thought: thoughtText,
                tool,
                action: tool,
                source: sourceFrom("", tool),
                status: "pending",
                params: command,
                rawParams: command,
                timestamp: Date.now(),
            };

            if (existingIdx >= 0) {
                steps.value[existingIdx] = { ...steps.value[existingIdx], ...stepItem };
            } else {
                steps.value.push(stepItem);
            }
            if (!selectedStepId.value) {
                selectedStepId.value = stepItem.id;
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

        // 步骤异常 step_error
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
            appendLog(`[步骤失败 #${stepId + 1}] ${existing?.description || ""}`);
            autoScrollToBottom();
            return;
        }

        const state = payload.state ? normalizeState(payload.state) : undefined;
        if (state) setRunState(state, toText(payload.message || payload.description) || undefined);
    }

    function handleLog(payload: unknown) {
        appendLog(payload);
    }

    function handleContext(payload: unknown) {
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
        setRunState("idle");
    }

    async function runAgent() {
        if (!canStart.value) return;
        clearSession();
        currentGoal.value = goalInput.value.trim();
        goalInput.value = "";
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
            // 主循环返回时，本轮最终摘要已由后端写入 chat_messages；刷新长期会话即可继续对话。
            void loadConversation();
        }
    }
    async function requestStop() {
        if (!isRunning.value || stopRequested.value) return;
        stopRequested.value = true;
        const message = "用户请求停止当前任务。请立即停止后续工具调用，简要整理当前已完成的工作并结束任务。";
        try {
            await invoke("send_human_intervention", { sessionId, message });
            appendLog("[人工干预] 已发送立即停止指令。");
        } catch (error) {
            stopRequested.value = false;
            appendLog(`[停止指令失败] ${toText(error)}`);
        }
    }

    async function sendHumanInput() {
        const message = humanInput.value.trim();
        if (!message || isSendingHuman.value) return;
        isSendingHuman.value = true;
        try {
            await invoke("send_human_intervention", { sessionId, message });
            humanInterventions.value.push({ message, timestamp: Date.now() });
            humanInput.value = "";
            pauseReason.value = "";
            appendLog(`[用户补充] ${message}`);
            if (isWaitingForHuman.value) {
                setRunState("executing", "已发送人工输入，等待 Agent 继续执行。");
            }
            autoScrollToBottom();
        } catch (error) {
            appendLog(`[人工输入失败] ${toText(error)}`);
        } finally {
            isSendingHuman.value = false;
        }
    }

    function selectStep(id: string) {
        selectedStepId.value = id;
    }

    onMounted(async () => {
        void loadConversation();
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
        steps,
        selectedStepId,
        selectedStep,
        thought,
        isThinking,
        thinkingCollapsed,
        thinkingDurationMs,
        logs,
        humanInterventions,
        tokenUsage,
        completionMessage,
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
        sendHumanInput,
        clearSession,
        loadConversation,
        selectStep,
        refreshResources,
        openResourcePreview,
        closeResourcePreview,
        loadPreviousRecordPage,
        loadNextRecordPage,
    };
}
