<template>
    <aside class="agent-inspector" aria-label="Agent 执行详情">
        <header class="inspector-header">
            <div>
                <span class="inspector-kicker">AGENT SESSION</span>
                <h2>{{ stateMeta.category }}</h2>
                <p>{{ statusDescription }}</p>
            </div>
            <span class="state-chip" :class="`state-${runState}`">{{ stateMeta.label }}</span>
        </header>

        <section class="session-facts">
            <div><span>运行</span><strong>{{ elapsedLabel }}</strong></div>
            <div><span>工具</span><strong>{{ completedToolCount }}</strong></div>
            <div><span>异常</span><strong :class="{ danger: failedToolCount }">{{ failedToolCount }}</strong></div>
            <div v-if="tokenUsage"><span>上下文</span><strong>{{ tokenUsage.usage_percent.toFixed(1) }}%</strong></div>
        </section>

        <nav class="inspector-tabs" aria-label="详情分区">
            <button type="button" :class="{ active: activeTab === 'details' }" @click="activeTab = 'details'">执行详情</button>
            <button type="button" :class="{ active: activeTab === 'resources' }" @click="activeTab = 'resources'">
                资源 <span>{{ resources.length }}</span>
            </button>
        </nav>

        <section v-if="activeTab === 'details'" class="inspector-scroll">
            <div v-if="!selectedItem" class="inspector-empty">
                <div class="empty-mark">↗</div>
                <h3>选择一条执行过程</h3>
                <p>聊天流里的工具行和状态消息都可以点击。工具行会显示真实的调用参数与返回结果。</p>
            </div>
            <template v-else-if="selectedItem.tool">
                <div class="detail-heading">
                    <span class="detail-source">{{ selectedItem.tool.source }}</span>
                    <h3>{{ selectedItem.tool.name }}</h3>
                    <p>{{ selectedItem.tool.action }}</p>
                    <div class="detail-meta">
                        <span :class="`detail-status status-${selectedItem.tool.status}`">{{ toolStatusLabel(selectedItem.tool.status) }}</span>
                        <span v-if="selectedItem.tool.durationMs">{{ formatMilliseconds(selectedItem.tool.durationMs) }}</span>
                        <span>{{ formatClock(selectedItem.timestamp) }}</span>
                    </div>
                </div>
                <p v-if="selectedItem.detail" class="detail-summary">{{ selectedItem.detail }}</p>
                <section class="payload-section">
                    <h4>输入参数</h4>
                    <pre>{{ formatPayload(selectedItem.tool.rawParams ?? selectedItem.tool.params ?? {}) }}</pre>
                </section>
                <section class="payload-section">
                    <h4>执行输出</h4>
                    <pre>{{ formatPayload(selectedItem.tool.rawResult ?? selectedItem.tool.result ?? {}) }}</pre>
                </section>
            </template>
            <template v-else>
                <div class="detail-heading">
                    <span class="detail-source">{{ kindLabel(selectedItem.kind) }}</span>
                    <h3>{{ selectedItem.title }}</h3>
                    <div class="detail-meta"><span>{{ formatClock(selectedItem.timestamp) }}</span></div>
                </div>
                <p class="detail-summary large">{{ selectedItem.detail || '此事件没有附加说明。' }}</p>
            </template>
        </section>

        <section v-else class="inspector-scroll resource-panel">
            <div class="resource-panel-heading">
                <div>
                    <h3>持久化资源</h3>
                    <p>来自当前 session 的任务数据存储。</p>
                </div>
                <button class="refresh-button" type="button" :disabled="isResourcesLoading" @click="$emit('refresh-resources')">
                    {{ isResourcesLoading ? '刷新中' : '刷新' }}
                </button>
            </div>
            <p v-if="resourceLoadError" class="resource-error">{{ resourceLoadError }}</p>
            <div v-if="resources.length === 0 && !isResourcesLoading" class="resource-empty">任务执行后生成的表格、列表和中间数据会出现在这里。</div>
            <article v-for="resource in resources" :key="resource.resourceId" class="resource-card">
                <div class="resource-card-top">
                    <span>{{ resource.kind }}</span>
                    <small>{{ resource.status }}</small>
                </div>
                <h4>{{ resource.label }}</h4>
                <p>{{ resource.summary || '暂无资源摘要。' }}</p>
                <footer>
                    <span v-if="resource.rowCount !== undefined">{{ formatNumber(Number(resource.rowCount)) }} 条记录</span>
                    <button type="button" @click="$emit('preview-resource', resource)">查看记录</button>
                </footer>
            </article>
        </section>
    </aside>
</template>

<script setup lang="ts">
import { ref } from "vue";
import type {
    AgentRunState,
    AgentRunStateMeta,
    AgentTaskResource,
    AgentTimelineItem,
    AgentTokenUsage,
    AgentToolStatus,
} from "./types";
import { formatClock, formatMilliseconds, formatNumber, formatPayload } from "./format";

defineProps<{
    runState: AgentRunState;
    stateMeta: AgentRunStateMeta;
    statusDescription: string;
    elapsedLabel: string;
    tokenUsage: AgentTokenUsage | null;
    completedToolCount: number;
    failedToolCount: number;
    selectedItem: AgentTimelineItem | null;
    resources: AgentTaskResource[];
    isResourcesLoading: boolean;
    resourceLoadError: string;
}>();

defineEmits<{
    "refresh-resources": [];
    "preview-resource": [resource: AgentTaskResource];
}>();

const activeTab = ref<"details" | "resources">("details");

function toolStatusLabel(status: AgentToolStatus) {
    return { pending: "等待", running: "执行中", succeeded: "已完成", failed: "失败" }[status];
}

function kindLabel(kind: AgentTimelineItem["kind"]) {
    return { system: "运行状态", planning: "规划", tool: "工具", success: "完成", error: "异常", human: "人工输入", recovery: "恢复" }[kind];
}
</script>

<style scoped>
.agent-inspector {
    display: flex;
    min-width: 0;
    min-height: 0;
    flex-direction: column;
    border-left: 1px solid var(--chat-line);
    background: var(--chat-inspector-bg);
}
.inspector-header { display: flex; justify-content: space-between; gap: 14px; padding: 20px 18px 14px; border-bottom: 1px solid var(--chat-line); }
.inspector-kicker { display: block; color: var(--chat-text-faint); font-size: 10px; font-weight: 800; letter-spacing: .1em; }
.inspector-header h2 { margin: 5px 0 3px; color: var(--chat-text); font-size: 17px; letter-spacing: -.02em; }
.inspector-header p { margin: 0; color: var(--chat-text-muted); font-size: 11px; line-height: 1.5; }
.state-chip { align-self: flex-start; padding: 5px 7px; border: 1px solid var(--chat-line-strong); border-radius: 7px; color: var(--chat-text-soft); font-size: 10px; font-weight: 750; }
.state-executing, .state-planning { color: var(--chat-accent); border-color: color-mix(in srgb, var(--chat-accent) 46%, transparent); }
.state-succeeded { color: var(--chat-success); }
.state-failed { color: var(--chat-danger); }
.session-facts { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 1px; border-bottom: 1px solid var(--chat-line); background: var(--chat-line); }
.session-facts div { display: grid; gap: 3px; padding: 11px 10px; background: var(--chat-inspector-bg); }
.session-facts span { color: var(--chat-text-faint); font-size: 10px; }
.session-facts strong { color: var(--chat-text); font-size: 12px; }
.session-facts strong.danger { color: var(--chat-danger); }
.inspector-tabs { display: grid; grid-template-columns: 1fr 1fr; padding: 8px 10px; border-bottom: 1px solid var(--chat-line); gap: 4px; }
.inspector-tabs button { padding: 7px 8px; border: 0; border-radius: 7px; color: var(--chat-text-muted); cursor: pointer; background: transparent; font: inherit; font-size: 12px; }
.inspector-tabs button.active { color: var(--chat-text); background: var(--chat-hover); }
.inspector-tabs span { margin-left: 4px; color: var(--chat-text-faint); font-size: 10px; }
.inspector-scroll { min-height: 0; flex: 1; padding: 18px; overflow-y: auto; }
.inspector-empty { display: grid; min-height: 240px; place-content: center; text-align: center; }
.empty-mark { width: 30px; height: 30px; margin: 0 auto 12px; border: 1px solid var(--chat-line-strong); border-radius: 9px; color: var(--chat-accent); font-size: 18px; line-height: 28px; }
.inspector-empty h3 { margin: 0; color: var(--chat-text); font-size: 14px; }
.inspector-empty p { max-width: 240px; margin: 8px auto 0; color: var(--chat-text-muted); font-size: 12px; line-height: 1.65; }
.detail-heading { padding-bottom: 15px; border-bottom: 1px solid var(--chat-line); }
.detail-source { color: var(--chat-accent); font-size: 10px; font-weight: 800; letter-spacing: .08em; }
.detail-heading h3 { margin: 7px 0 3px; color: var(--chat-text); font-size: 17px; word-break: break-word; }
.detail-heading p { margin: 0; color: var(--chat-text-muted); font: 11px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; }
.detail-meta { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 11px; color: var(--chat-text-faint); font-size: 11px; }
.detail-status { padding: 2px 5px; border-radius: 4px; font-weight: 700; }
.status-running { color: var(--chat-warning); background: color-mix(in srgb, var(--chat-warning) 12%, transparent); }
.status-succeeded { color: var(--chat-success); background: color-mix(in srgb, var(--chat-success) 12%, transparent); }
.status-failed { color: var(--chat-danger); background: color-mix(in srgb, var(--chat-danger) 12%, transparent); }
.detail-summary { margin: 15px 0; color: var(--chat-text-soft); font-size: 12px; line-height: 1.65; white-space: pre-wrap; }
.detail-summary.large { font-size: 13px; }
.payload-section { margin-top: 14px; }
.payload-section h4 { margin: 0 0 7px; color: var(--chat-text-muted); font-size: 11px; font-weight: 750; }
.payload-section pre { max-height: 260px; margin: 0; padding: 11px; overflow: auto; border: 1px solid var(--chat-line); border-radius: 9px; color: #c7d4e8; background: #0c111b; font: 11px/1.6 ui-monospace, SFMono-Regular, Menlo, monospace; white-space: pre-wrap; word-break: break-word; }
.resource-panel-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; }
.resource-panel-heading h3 { margin: 0; color: var(--chat-text); font-size: 14px; }
.resource-panel-heading p { margin: 4px 0 0; color: var(--chat-text-muted); font-size: 11px; line-height: 1.45; }
.refresh-button, .resource-card button { border: 1px solid var(--chat-line-strong); border-radius: 7px; color: var(--chat-text-soft); cursor: pointer; background: var(--chat-tool-bg); font: inherit; font-size: 11px; }
.refresh-button { padding: 6px 8px; }
.refresh-button:hover:not(:disabled), .resource-card button:hover { border-color: var(--chat-accent); color: var(--chat-text); }
.refresh-button:disabled { cursor: wait; opacity: .6; }
.resource-error { margin: 12px 0; color: var(--chat-danger); font-size: 12px; line-height: 1.5; }
.resource-empty { padding: 24px 4px; color: var(--chat-text-muted); font-size: 12px; line-height: 1.7; }
.resource-card { margin-top: 12px; padding: 12px; border: 1px solid var(--chat-line); border-radius: 10px; background: var(--chat-tool-bg); }
.resource-card-top { display: flex; justify-content: space-between; gap: 10px; color: var(--chat-text-faint); font-size: 10px; font-weight: 750; }
.resource-card h4 { margin: 9px 0 4px; color: var(--chat-text); font-size: 13px; }
.resource-card p { margin: 0; color: var(--chat-text-muted); font-size: 11px; line-height: 1.6; }
.resource-card footer { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin-top: 11px; color: var(--chat-text-faint); font-size: 10px; }
.resource-card button { padding: 5px 7px; }
@media (max-width: 980px) { .agent-inspector { border-top: 1px solid var(--chat-line); border-left: 0; } }
</style>
