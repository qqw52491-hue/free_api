export interface ParsedAttachment {
    file_name: string;
    file_path: string;
    size_bytes: number;
    content_type: string;
    prompt_text: string;
    image_data_url?: string | null;
    raw_preview?: string | null;
}

export interface AgentModelOption {
    id: string;
    name: string;
    display_name: string;
    platform_id: string;
    platform_name: string;
}

export type AgentRunState =
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

export type AgentTimelineKind = "system" | "planning" | "tool" | "success" | "error" | "human" | "recovery";
export type AgentToolStatus = "running" | "succeeded" | "failed" | "pending" | "challenged";
export type AgentDetailSection = "params" | "result";

export interface AgentRunStateMeta {
    label: string;
    category: string;
    description: string;
}

export interface AgentTokenUsage {
    prompt_tokens: number;
    completion_tokens: number;
    total_tokens: number;
    cached_tokens?: number;
    cache_hit_rate?: number;
    context_window: number;
    usage_percent: number;
    ttft_ms?: number;
    total_duration_ms?: number;
    tokens_per_second?: number;
}

export interface AgentSessionTelemetry {
    stepCount: number;
    completedToolCount: number;
    failedToolCount: number;
    elapsedLabel: string;
    actionPhase?: string;
    lastTtftMs?: number;
    lastToolDurationMs?: number;
    totalToolDurationMs: number;
    tokensPerSecond?: number;
    promptTokens: number;
    completionTokens: number;
    totalTokens: number;
    cachedTokens: number;
    cacheHitRate: number;
    contextWindow: number;
    contextUsagePercent: number;
}
export type AgentHumanFormKind = "single_select" | "multi_select" | "confirm" | "file_picker" | "text" | "custom_html" | "plan_approval";
export interface AgentHumanFormOption {
    id: string;
    label: string;
    description?: string;
}
export interface AgentHumanForm {
    kind: AgentHumanFormKind;
    title: string;
    description?: string;
    placeholder?: string;
    options: AgentHumanFormOption[];
    defaultValue?: string | string[] | boolean;
    html?: string;
}

export interface AgentToolCall {
    name: string;
    action: string;
    source: "内部" | "MCP";
    status: AgentToolStatus;
    durationMs?: number;
    params?: unknown;
    result?: unknown;
    rawParams?: unknown;
    rawResult?: unknown;
}

export interface AgentSubAction {
    id: string;
    actionId: number;
    title: string;
    explanation?: string;
    reflection?: string;
    icon?: string;
    tool: string;
    action: string;
    source: "内部" | "MCP";
    status: AgentToolStatus;
    durationMs?: number;
    params?: unknown;
    result?: unknown;
    rawParams?: unknown;
    rawResult?: unknown;
    timestamp: number;
}

export interface AgentMilestoneGoal {
    id: string;
    goalId: number;
    title: string;
    status: AgentToolStatus;
    subActions: AgentSubAction[];
    expanded?: boolean;
}

export interface AgentStepMetrics {
    promptTokens?: number;
    completionTokens?: number;
    totalTokens?: number;
    cachedTokens?: number;
    cacheHitRate?: number;
    tokensPerSecond?: number;
    ttftMs?: number;
    thinkingDurationMs?: number;
    toolDurationMs?: number;
    totalDurationMs?: number;
}

export interface AgentStepItem {
    id: string;
    stepId: number;
    description: string;
    planId: string;
    planTitle: string;
    modelTier?: string;
    reflection?: string;
    thought?: string;
    explanation?: string;
    tool: string;
    action: string;
    source: "内部" | "MCP";
    status: AgentToolStatus;
    durationMs?: number;
    params?: unknown;
    result?: unknown;
    rawParams?: unknown;
    rawResult?: unknown;
    metrics?: AgentStepMetrics;
    timestamp: number;
}

export interface AgentLogEntry {
    id: string;
    message: string;
    level: "info" | "warn" | "error" | "route" | "thought";
    timestamp: number;
}

export interface AgentTimelineItem {
    id: string;
    kind: AgentTimelineKind;
    title: string;
    detail?: string;
    timestamp: number;
    status?: "running" | "succeeded" | "failed";
    tool?: AgentToolCall;
}

export interface AgentTaskResource {
    resourceId: string;
    kind: string;
    label: string;
    rowCount?: number | string;
    status: string;
    summary?: string;
    cursor?: string;
}

export interface AgentTaskRecord {
    key: string;
    data: unknown;
    updatedAt: string;
}

export interface AgentTaskRecordPage {
    resourceId: string;
    records: AgentTaskRecord[];
    offset: number;
    limit: number;
    total: number;
    hasMore: boolean;
}

export interface LlmParseErrorItem {
    stepId: number;
    tryIndex: number;
    error: string;
    rawResponse: string;
    thinkingText?: string;
    timestamp: number;
}

export const AGENT_RUN_STATE_CATALOG: Record<AgentRunState, AgentRunStateMeta> = {
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

export const AGENT_TASK_PRESETS = [
    { label: "创建笔记", goal: "在桌面创建一个名为 notes.txt 的文件，写入当前日期和时间，然后显示文件内容。" },
    { label: "查看桌面", goal: "列出桌面上所有文件和文件夹，并统计数量。" },
    { label: "磁盘信息", goal: "查看当前磁盘使用情况，以及 CPU 和内存占用信息。" },
    { label: "整理下载", goal: "列出下载文件夹中最近修改的 5 个文件，以及其大小。" },
    { label: "系统信息", goal: "获取系统版本、主机名、当前用户名和 IP 地址。" },
] as const;

export type ContextBudgetMode = "deep_coding" | "cost_saving" | "ultra_fast";

export interface ContextBudgetModeOption {
    id: ContextBudgetMode;
    label: string;
    icon: string;
    tag: string;
    description: string;
    tooltip: string;
}

export const CONTEXT_BUDGET_MODES: ContextBudgetModeOption[] = [
    {
        id: "deep_coding",
        label: "长程",
        icon: "🚀",
        tag: "75% 窗口",
        description: "长程沉浸 (推荐)",
        tooltip: "🚀 长程深度编码：75% 窗口 / 85% 触发 / 50% 回落，保留几十步完整代码记忆",
    },
    {
        id: "cost_saving",
        label: "省流",
        icon: "💰",
        tag: "50% 窗口",
        description: "省流均衡",
        tooltip: "💰 省流均衡模式：50% 窗口 / 75% 触发 / 35% 回落，更积极归档以节省 Token 费用",
    },
    {
        id: "ultra_fast",
        label: "极速",
        icon: "🏎️",
        tag: "35% 窗口",
        description: "极致响应",
        tooltip: "🏎️ 极致响应模式：35% 窗口 / 65% 触发 / 25% 回落，极低首字延迟",
    },
];

export type ReasoningEffort = "auto" | "off" | "low" | "medium" | "high";

export type RoutingMode = "rules" | "scored";

export interface ModelRouting {
    reasoning_effort_version: 2;
    pro: string;
    flash: string;
    vision: string;
    pro_reasoning_effort: ReasoningEffort;
    flash_reasoning_effort: ReasoningEffort;
    vision_reasoning_effort: ReasoningEffort;
    context_budget_mode: ContextBudgetMode;
    /** 路由模式：rules = 规则分流（默认）；scored = 难度打分制 */
    routing_mode: RoutingMode;
    /** 角色档位覆盖：角色 → ""(跟随自动) | "pro"/"flash"/"vision"(档位别名) | 具体模型 UUID */
    role_models: Record<string, string>;
}

export interface AgentWorkerStep {
    stepNumber: number;
    title: string;
    tool?: string;
    status: AgentToolStatus;
    description?: string;
    summary?: string;
    durationMs?: number;
    output?: string;
}

export interface AgentWorkerItem {
    id: string;
    name: string;
    role: "reviewer" | "coder" | "researcher" | "master" | "worker";
    roleLabel: string;
    status: AgentToolStatus;
    currentStep?: string;
    currentTool?: string;
    progress: number; // 0 - 100
    durationMs?: number;
    startedAt: number;
    thought?: string;
    output?: string;
    summary?: string;
    findings?: string[];
    evidenceRefs?: string[];
    confidence?: string;
    steps?: AgentWorkerStep[];
}

export interface AgentTeamState {
    masterStatus: AgentToolStatus;
    activeWorkerCount: number;
    completedWorkerCount: number;
    workers: AgentWorkerItem[];
}

