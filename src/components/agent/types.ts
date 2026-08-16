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
export type AgentToolStatus = "running" | "succeeded" | "failed" | "pending";
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
    context_window: number;
    usage_percent: number;
}
export type AgentHumanFormKind = "single_select" | "multi_select" | "confirm" | "file_picker" | "text";
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
