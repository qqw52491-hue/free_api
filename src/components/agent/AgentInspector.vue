<template>
    <aside class="manus-inspector" :class="{ 'pure-preview-mode': activeTab === 'preview' }" aria-label="执行详情与成果画布">
        <!-- 模式 A：纯净成果画布模式 (仅在用户显式点击“展开预览”时进入) -->
        <template v-if="activeTab === 'preview'">
            <header class="canvas-top-bar">
                <div class="canvas-title">
                    <span class="pulse-dot">●</span>
                    <span>成果画布 · 独立预览</span>
                </div>
                <button type="button" class="btn-exit-canvas" @click="activeTab = 'detail'" title="退出画布并返回调用详情">
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>
                    <span>返回执行详情</span>
                </button>
            </header>

            <div class="canvas-viewport">
                <HtmlPreviewBlock
                    v-if="pureHtmlCode"
                    :code="pureHtmlCode"
                    class="full-height-preview"
                    @agent-submit="$emit('agent-submit', $event)"
                    @agent-cancel="$emit('agent-cancel', $event)"
                />
                <div v-else-if="hasPreviewContent" class="canvas-scroll-content">
                    <RichContentRenderer :content="combinedPreviewContent" />
                </div>
                <div v-else class="detail-placeholder">
                    <div class="placeholder-glyph">🌐</div>
                    <h3>暂无生成成果</h3>
                    <p>任务生成的 HTML 单页应用或数据看板会直接在此全屏渲染。</p>
                </div>
            </div>
        </template>

        <!-- 模式 B：标准调用详情与执行状态模式 (专注思维链、参数与结果) -->
        <template v-else>
            <header class="inspector-top">
                <div>
                    <span>EXECUTION</span>
                    <h2>{{ stateMeta.category }}</h2>
                    <p>{{ statusDescription }}</p>
                </div>
                <div class="inspector-top-controls">
                    <span class="state-badge" :class="`state-${runState}`">{{ stateMeta.label }}</span>
                    <button type="button" class="btn-close-inspector" title="收起面板" @click="$emit('close')">
                        <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>
                    </button>
                </div>
            </header>

            <div class="inspector-metrics">
                <div><span>运行</span><strong>{{ elapsedLabel }}</strong></div>
                <div><span>完成</span><strong>{{ completedToolCount }}</strong></div>
                <div v-if="tokenUsage && tokenUsage.tokens_per_second !== undefined && tokenUsage.tokens_per_second > 0" :title="`吐字速率: ${tokenUsage.tokens_per_second.toFixed(1)} tok/s, 首字: ${tokenUsage.ttft_ms ?? 0}ms`">
                    <span>速率</span><strong style="color:#38bdf8">{{ tokenUsage.tokens_per_second.toFixed(1) }} <small style="font-size:9px;color:#71717a">t/s</small></strong>
                </div>
                <div v-if="tokenUsage && tokenUsage.cache_hit_rate !== undefined && tokenUsage.cache_hit_rate > 0" :title="`前缀缓存命中: ${tokenUsage.cached_tokens ?? 0} / ${tokenUsage.prompt_tokens} tokens`">
                    <span>缓存</span><strong style="color:#4ade80">{{ tokenUsage.cache_hit_rate.toFixed(0) }}%</strong>
                </div>
                <div v-if="tokenUsage" :title="`已用 ${tokenUsage.total_tokens} / 窗口 ${tokenUsage.context_window > 0 ? tokenUsage.context_window : '32k'}`">
                    <span>上下文</span><strong>{{ tokenUsage.usage_percent.toFixed(1) }}%</strong>
                </div>
                <div v-else-if="failedToolCount"><span>异常</span><strong class="danger">{{ failedToolCount }}</strong></div>
            </div>

            <nav class="detail-tabs">
                <button type="button" :class="{ active: activeTab === 'detail' }" @click="activeTab = 'detail'">调用详情</button>
                <button type="button" :class="{ active: activeTab === 'resources' }" @click="activeTab = 'resources'">资源 <i>{{ resources.length }}</i></button>
            </nav>

            <!-- 1. 调用详情 Tab (默认) -->
            <section v-if="activeTab === 'detail'" class="inspector-scroll">
                <div v-if="selectedHuman" class="narrative-detail">
                    <div class="selected-step-heading"><span>人工交互</span><h3>你补充了说明</h3><div class="step-meta"><b class="human">人工输入</b><em>{{ formatClock(selectedHuman.timestamp) }}</em></div></div>
                    <p class="narrative-copy">{{ selectedHuman.message }}</p>
                </div>
                <div v-else-if="selectedActiveThought" class="narrative-detail">
                    <div class="selected-step-heading"><span>实时思考</span><h3>正在思考</h3><div class="step-meta"><b class="thought">思考中</b></div></div>
                    <p class="narrative-copy">{{ activeThoughtText || '正在等待模型返回思考内容…' }}</p>
                </div>
                <div v-else-if="selectedStep && selectedSubpart === 'thought'" class="narrative-detail">
                    <div class="selected-step-heading"><span>步骤 {{ selectedStep.stepId + 1 }} · 思考过程</span><h3>{{ selectedStep.description || '执行步骤' }}</h3><div class="step-meta"><b class="thought">已分析</b><em>{{ formatClock(selectedStep.timestamp) }}</em></div></div>
                    <p class="narrative-copy">{{ selectedStep.thought || selectedStep.reflection || '该步骤没有单独的思考内容。' }}</p>
                </div>
                <div v-else-if="!selectedStep" class="detail-placeholder">
                    <div class="placeholder-glyph">›_</div>
                    <h3>选择一个过程步骤</h3>
                    <p>中间对话流中的每个工具步骤都可点击。这里会显示该次真实调用的参数和返回结果。</p>
                </div>
                <template v-else>
                    <div class="selected-step-heading">
                        <span>{{ selectedStep.source }} · 步骤 {{ selectedStep.stepId + 1 }}</span>
                        <h3>{{ selectedStep.tool || 'Agent 操作' }}</h3>
                        <p>{{ selectedStep.description || selectedStep.action }}</p>
                        <div class="step-meta"><b :class="`status-${selectedStep.status}`">{{ statusLabel(selectedStep.status) }}</b><em v-if="selectedStep.durationMs">{{ formatMilliseconds(selectedStep.durationMs) }}</em><em>{{ formatClock(selectedStep.timestamp) }}</em></div>
                    </div>
                    <p v-if="selectedStep.reflection || selectedStep.explanation" class="step-explanation">{{ selectedStep.reflection || selectedStep.explanation }}</p>
                    <section class="data-section">
                        <h4>调用输入</h4>
                        <pre>{{ formatPayload(selectedStep.rawParams ?? selectedStep.params ?? {}) }}</pre>
                    </section>
                    <section class="data-section">
                        <h4>执行输出</h4>
                        <pre>{{ formatPayload(selectedStep.rawResult ?? selectedStep.result ?? {}) }}</pre>
                    </section>
                </template>
            </section>

            <!-- 2. 资源 Tab -->
            <section v-else class="inspector-scroll resources-pane">
                <div class="resources-heading">
                    <div><h3>任务资源</h3><p>当前会话的持久化数据。</p></div>
                    <button type="button" :disabled="isResourcesLoading" @click="$emit('refresh-resources')">{{ isResourcesLoading ? '刷新中' : '刷新' }}</button>
                </div>
                <p v-if="resourceLoadError" class="resource-error">{{ resourceLoadError }}</p>
                <p v-else-if="!resources.length && !isResourcesLoading" class="resource-placeholder">任务产生的文件、表格和中间数据会显示在这里。</p>
                <article v-for="resource in resources" :key="resource.resourceId" class="resource-row">
                    <span>{{ resource.kind }}</span>
                    <h4>{{ resource.label }}</h4>
                    <p>{{ resource.summary || '暂无摘要。' }}</p>
                    <footer><small v-if="resource.rowCount !== undefined">{{ formatNumber(Number(resource.rowCount)) }} 条记录</small><button type="button" @click="$emit('preview-resource', resource)">查看记录</button></footer>
                </article>
            </section>
        </template>
    </aside>
</template>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import type { AgentRunState, AgentRunStateMeta, AgentStepItem, AgentTaskResource, AgentTokenUsage, AgentToolStatus } from "./types";
import { formatClock, formatMilliseconds, formatNumber, formatPayload } from "./format";
import RichContentRenderer from "../charts/RichContentRenderer.vue";
import HtmlPreviewBlock from "../charts/HtmlPreviewBlock.vue";

const props = withDefaults(
    defineProps<{
        runState: AgentRunState;
        stateMeta: AgentRunStateMeta;
        statusDescription: string;
        elapsedLabel: string;
        tokenUsage: AgentTokenUsage | null;
        completedToolCount: number;
        failedToolCount: number;
        selectedStep: AgentStepItem | null;
        selectedSubpart: "thought" | "tool" | null;
        selectedHuman: { message: string; timestamp: number } | null;
        selectedActiveThought: boolean;
        activeThoughtText: string;
        resources: AgentTaskResource[];
        isResourcesLoading: boolean;
        resourceLoadError: string;
        finalSummary?: string;
        finalArtifacts?: string[];
        activePreviewContent?: string;
    }>(),
    {
        finalSummary: "",
        finalArtifacts: () => [],
        activePreviewContent: "",
    }
);

defineEmits<{
    "refresh-resources": [];
    "preview-resource": [resource: AgentTaskResource];
    "agent-submit": [data: any];
    "agent-cancel": [reason: string];
    "close": [];
}>();

const activeTab = ref<"preview" | "detail" | "resources">("detail");

const combinedPreviewContent = computed(() => {
    if (props.activePreviewContent && props.activePreviewContent.trim()) {
        return props.activePreviewContent;
    }
    const parts: string[] = [];
    if (props.finalSummary) parts.push(props.finalSummary);
    if (props.finalArtifacts && props.finalArtifacts.length) {
        for (const art of props.finalArtifacts) {
            if (!parts.includes(art)) parts.push(art);
        }
    }
    return parts.join("\n\n");
});

const pureHtmlCode = computed(() => {
    const text = combinedPreviewContent.value;
    const match = text.match(/```html[\s\r\n]+([\s\S]*?)```/i);
    if (match) return match[1].trim();
    if (/<(?:!doctype|html|body|div|button|canvas|svg|form)/i.test(text)) {
        return text.trim();
    }
    return "";
});

const hasPreviewContent = computed(() => {
    const text = combinedPreviewContent.value;
    return Boolean(
        text.includes("```html") ||
        text.includes("```echarts") ||
        text.includes("```chart") ||
        text.includes("```mermaid") ||
        pureHtmlCode.value
    );
});

// 仅当用户在左侧点击了“展开预览”传入 activePreviewContent 时，才切换到 preview 模式
watch(
    () => props.activePreviewContent,
    (val) => {
        if (val && val.trim()) {
            activeTab.value = "preview";
        }
    }
);

// 当用户在左侧对话流中点击了某个步骤、思维链或介入记录时，自动切换回详情视图
watch(
    () => [props.selectedStep, props.selectedHuman, props.selectedActiveThought],
    ([step, human, thought]) => {
        if (step || human || thought) {
            activeTab.value = "detail";
        }
    }
);

function statusLabel(status: AgentToolStatus) { return { pending: "等待", running: "执行中", succeeded: "完成", failed: "失败" }[status]; }

defineExpose({
    switchTab: (tab: "preview" | "detail" | "resources") => {
        activeTab.value = tab;
    },
});
</script>

<style scoped>
.manus-inspector { display: flex; min-width: 0; min-height: 0; flex-direction: column; border-left: 1px solid rgba(255, 255, 255, 0.08); color: #dedee1; background: #18181b; }
.manus-inspector.pure-preview-mode { background: #141416; }

/* 纯净画布顶栏 */
.canvas-top-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 12px;
    height: 38px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
    background: #18181b;
}
.btn-exit-canvas {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 4px 10px;
    border-radius: 6px;
    border: 1px solid rgba(255, 255, 255, 0.12);
    background: rgba(255, 255, 255, 0.06);
    color: #d4d4d8;
    font-size: 11px;
    font-weight: 550;
    cursor: pointer;
    transition: all 0.15s ease;
}
.btn-exit-canvas:hover {
    background: rgba(255, 255, 255, 0.12);
    color: #ffffff;
}
.canvas-title {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    font-weight: 650;
    color: #f4f4f5;
}
.pulse-dot {
    color: #fbbf24;
    font-size: 10px;
    animation: pulse 2s infinite;
}
@keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.4; }
}

.canvas-tabs {
    display: flex;
    align-items: center;
    gap: 4px;
}
.canvas-tabs .tab-btn {
    padding: 4px 8px;
    border: 0;
    border-radius: 5px;
    background: transparent;
    color: #a1a1aa;
    font-size: 11px;
    font-weight: 550;
    cursor: pointer;
    transition: all 0.15s ease;
}
.canvas-tabs .tab-btn:hover {
    color: #ffffff;
    background: rgba(255, 255, 255, 0.06);
}
.canvas-tabs .tab-btn.active {
    color: #f4f4f5;
    background: #27272a;
}

.canvas-viewport {
    position: relative;
    flex: 1;
    min-height: 0;
    width: 100%;
    overflow: hidden;
    background: #141416;
}
.full-height-preview {
    height: 100% !important;
    border: none !important;
    border-radius: 0 !important;
    margin: 0 !important;
}
.full-height-preview :deep(.card-body) {
    height: 100% !important;
}

.canvas-scroll-content {
    height: 100%;
    overflow-y: auto;
    padding: 14px;
}

/* 标准详情顶栏 */
.inspector-top { display: flex; justify-content: space-between; gap: 12px; padding: 14px 16px 12px; border-bottom: 1px solid rgba(255, 255, 255, 0.06); background: rgba(255, 255, 255, 0.015); }
.inspector-top span { color: #71717a; font-size: 9px; font-weight: 800; letter-spacing: .1em; }
.inspector-top h2 { margin: 4px 0 2px; color: #f4f4f5; font-size: 15px; font-weight: 650; }
.inspector-top p { 
    max-width: 210px; 
    margin: 0; 
    color: #a1a1aa; 
    font-size: 11px; 
    line-height: 1.45; 
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    text-overflow: ellipsis;
    word-break: break-all;
}
.inspector-top-controls { display: flex; align-items: center; gap: 8px; }
.btn-close-inspector {
    display: grid;
    width: 22px;
    height: 22px;
    place-items: center;
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 5px;
    background: rgba(255, 255, 255, 0.04);
    color: #a1a1aa;
    cursor: pointer;
    transition: all 0.15s ease;
    padding: 0;
}
.btn-close-inspector:hover {
    border-color: rgba(255, 255, 255, 0.25);
    color: #ffffff;
    background: rgba(255, 255, 255, 0.12);
}
.state-badge { align-self: center; padding: 3px 6px; border: 1px solid rgba(255, 255, 255, 0.12); border-radius: 5px; color: #a1a1aa !important; font-size: 9.5px !important; }

.inspector-metrics { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); border-bottom: 1px solid rgba(255, 255, 255, 0.06); background: rgba(0, 0, 0, 0.15); }
.inspector-metrics div { display: grid; gap: 2px; padding: 8px 6px; text-align: center; border-right: 1px solid rgba(255, 255, 255, 0.05); }
.inspector-metrics div:last-child { border-right: 0; }
.inspector-metrics span { color: #71717a; font-size: 9px; }
.inspector-metrics strong { color: #f4f4f5; font-size: 11.5px; font-weight: 600; }
.inspector-metrics .danger { color: #ff4757; }

.detail-tabs { display: grid; grid-template-columns: 1fr 1fr; gap: 3px; padding: 6px 8px; border-bottom: 1px solid rgba(255, 255, 255, 0.06); background: #141416; }
.detail-tabs.has-preview { display: grid; grid-template-columns: 1fr 1fr 1.1fr; }
.detail-tabs button { display: inline-flex; align-items: center; justify-content: center; gap: 4px; padding: 6px 4px; border: 0; border-radius: 5px; color: #a1a1aa; cursor: pointer; background: transparent; font: inherit; font-size: 11px; font-weight: 550; transition: all 0.15s; }
.detail-tabs button:hover { color: #ffffff; }
.detail-tabs button.active { color: #f4f4f5; background: #27272a; box-shadow: 0 1px 4px rgba(0, 0, 0, 0.3); }
.detail-tabs i { color: #71717a; font-size: 9px; font-style: normal; }
.preview-dot { color: #fbbf24 !important; font-size: 8px !important; }

.inspector-scroll { min-height: 0; flex: 1; padding: 14px; overflow-y: auto; }

.detail-placeholder { display: grid; min-height: 240px; place-content: center; text-align: center; }
.placeholder-glyph { margin-bottom: 8px; color: #71717a; font-size: 24px; }
.detail-placeholder h3 { margin: 0; color: #e4e4e7; font-size: 13px; font-weight: 600; }
.detail-placeholder p { max-width: 240px; margin: 6px auto 0; color: #71717a; font-size: 11.5px; line-height: 1.6; }

.selected-step-heading { padding-bottom: 12px; border-bottom: 1px solid rgba(255, 255, 255, 0.08); }
.selected-step-heading > span { color: #71717a; font-size: 10px; font-weight: 700; }
.selected-step-heading h3 { margin: 6px 0 3px; color: #f4f4f5; font-size: 14px; font-weight: 650; word-break: break-word; }
.selected-step-heading p { margin: 0; color: #a1a1aa; font-size: 11px; line-height: 1.5; }
.step-meta { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 8px; color: #71717a; font-size: 10px; }
.step-meta b { padding: 2px 5px; border-radius: 3px; font-weight: 650; }
.step-meta .status-running { color: #f4f4f5; background: #27272a; }
.step-meta .status-succeeded { color: #2ed573; background: rgba(46, 213, 115, 0.12); }
.step-meta .status-failed { color: #ff4757; background: rgba(255, 71, 87, 0.12); }
.step-meta em { font-style: normal; }

.step-explanation { margin: 12px 0; color: #a1a1aa; font-size: 12px; line-height: 1.65; }
.narrative-copy { margin: 12px 0 0; color: #d4d4d8; font-size: 12px; line-height: 1.7; white-space: pre-wrap; }
.step-meta .thought { color: #a78bfa; background: rgba(167, 139, 250, 0.12); }
.step-meta .human { color: #fbbf24; background: rgba(251, 191, 36, 0.12); }

.data-section { margin-top: 14px; }
.data-section h4 { margin: 0 0 6px; color: #a1a1aa; font-size: 10px; font-weight: 750; letter-spacing: .04em; }
.data-section pre { max-height: 260px; margin: 0; padding: 10px; overflow: auto; border: 1px solid rgba(255, 255, 255, 0.08); border-radius: 7px; color: #d4d4d8; background: #141416; font: 10.5px/1.55 ui-monospace, SFMono-Regular, Menlo, monospace; white-space: pre-wrap; word-break: break-word; }

.resources-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: 8px; }
.resources-heading h3 { margin: 0; color: #f4f4f5; font-size: 13px; font-weight: 650; }
.resources-heading p { margin: 3px 0 0; color: #71717a; font-size: 10.5px; }
.resources-heading button, .resource-row button { border: 1px solid rgba(255, 255, 255, 0.12); border-radius: 5px; padding: 4px 8px; color: #d4d4d8; cursor: pointer; background: #27272a; font: inherit; font-size: 10.5px; }
.resources-heading button:hover, .resource-row button:hover { background: #3f3f46; color: #fff; }
.resources-heading button:disabled { cursor: wait; opacity: .55; }
.resource-error { color: #ff4757; font-size: 11px; line-height: 1.6; }
.resource-placeholder { padding: 22px 0; color: #71717a; font-size: 11px; line-height: 1.7; }
.resource-row { margin-top: 10px; padding: 10px; border: 1px solid rgba(255, 255, 255, 0.08); border-radius: 7px; background: #1f1f23; }
.resource-row > span { color: #71717a; font-size: 9.5px; font-weight: 700; }
.resource-row h4 { margin: 4px 0 3px; color: #f4f4f5; font-size: 12.5px; font-weight: 600; }
.resource-row p { margin: 0; color: #a1a1aa; font-size: 10.5px; line-height: 1.5; }
.resource-row footer { display: flex; align-items: center; justify-content: space-between; gap: 8px; margin-top: 8px; }
.resource-row small { color: #71717a; font-size: 9.5px; }

@media (max-width: 980px) { .manus-inspector { min-height: 460px; border-top: 1px solid rgba(255, 255, 255, 0.08); border-left: 0; } }
</style>
