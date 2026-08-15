import { computed, nextTick, onMounted, onUnmounted, ref } from "vue";
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
    const completionMessage = ref("");
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
    const finalSummary = computed(() => {
        const output = finishOutput.value || {};
        return toText(output.summary || output.message || output.result || output.content || "");
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
            summary: toText(completion.summary || completion.details || completion.message || completion.result || completion.content),
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
    function runDeduplicationKey(run: HistoryRunSnapshot) {
        const firstStep = run.steps[0];
        const fingerprint = `${firstStep?.tool || ""}|${firstStep?.description || ""}`;
        return `${run.goal.trim()}|${fingerprint}|${Math.floor(run.startedAt / 30_000)}`;
    }
    function runInformationScore(run: HistoryRunSnapshot) {
        return run.steps.length * 10 + (run.summary ? 4 : 0) + run.artifacts.length * 2
            + (["succeeded", "failed", "cancelled"].includes(run.status) ? 1 : 0);
    }
    function dedupeHistoryRuns(runs: HistoryRunSnapshot[]) {
        const winners = new Map<string, HistoryRunSnapshot>();
        for (const run of runs.sort((a, b) => a.startedAt - b.startedAt || a.id.localeCompare(b.id))) {
            // 空轨迹没有可复盘过程，仍由已有会话消息显示，不生成折叠的空计划卡。
            if (!run.steps.length && !run.summary && !run.artifacts.length) continue;
            const key = runDeduplicationKey(run);
            const existing = winners.get(key);
            if (!existing || runInformationScore(run) >= runInformationScore(existing)) winners.set(key, run);
        }
        return [...winners.values()].sort((a, b) => a.startedAt - b.startedAt || a.id.localeCompare(b.id));
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
            modelRouting.value.pro = findModel(m => {
                const label = `${m.display_name} ${m.name}`;
                return !/flash|mini|haiku|speed|turbo/i.test(label)
                    && /pro|opus|claude-3-5|gpt-4o|deepseek/i.test(label);
            });
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
                planId: toText(rawStep.plan_id) || "plan-default",
                planTitle: toText(rawStep.plan_title) || currentGoal.value || "执行任务",
                modelTier: toText(rawStep.model_tier) || undefined,
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
        // 在 invoke 前打开新的事件世代；上一轮的迟到消息仍被终态屏障拦截。
        runTerminal.value = false;
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
            // 任务结束后立即把当前实时区域折叠为一个已持久化回合；下一轮只会在其下方追加。
            await loadConversation();
            await loadRunTraces();
            const newest = historyRuns.value[historyRuns.value.length - 1];
            if (newest && newest.goal === currentGoal.value && ["succeeded", "failed", "cancelled"].includes(newest.status)) {
                clearSession();
                autoScrollToBottom(false);
            }
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
        loadRunTraces,
        selectStep,
        refreshResources,
        openResourcePreview,
        closeResourcePreview,
        loadPreviousRecordPage,
        loadNextRecordPage,
    };
}
