<template>
    <aside class="manus-inspector" aria-label="执行详情">
        <header class="inspector-top">
            <div>
                <span>EXECUTION</span>
                <h2>{{ stateMeta.category }}</h2>
                <p>{{ statusDescription }}</p>
            </div>
            <span class="state-badge" :class="`state-${runState}`">{{ stateMeta.label }}</span>
        </header>

        <div class="inspector-metrics">
            <div><span>运行</span><strong>{{ elapsedLabel }}</strong></div>
            <div><span>完成</span><strong>{{ completedToolCount }}</strong></div>
            <div><span>异常</span><strong :class="{ danger: failedToolCount }">{{ failedToolCount }}</strong></div>
            <div v-if="tokenUsage"><span>上下文</span><strong>{{ tokenUsage.usage_percent.toFixed(1) }}%</strong></div>
        </div>

        <nav class="detail-tabs">
            <button type="button" :class="{ active: activeTab === 'detail' }" @click="activeTab = 'detail'">调用详情</button>
            <button type="button" :class="{ active: activeTab === 'resources' }" @click="activeTab = 'resources'">资源 <i>{{ resources.length }}</i></button>
        </nav>

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
    </aside>
</template>

<script setup lang="ts">
import { ref } from "vue";
import type { AgentRunState, AgentRunStateMeta, AgentStepItem, AgentTaskResource, AgentTokenUsage, AgentToolStatus } from "./types";
import { formatClock, formatMilliseconds, formatNumber, formatPayload } from "./format";

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
}>();

defineEmits<{
    "refresh-resources": [];
    "preview-resource": [resource: AgentTaskResource];
}>();

const activeTab = ref<"detail" | "resources">("detail");
function statusLabel(status: AgentToolStatus) { return { pending: "等待", running: "执行中", succeeded: "完成", failed: "失败" }[status]; }
</script>

<style scoped>
.manus-inspector { display: flex; min-width: 0; min-height: 0; flex-direction: column; border-left: 1px solid #343438; color: #dedee1; background: #1a1a1d; }
.inspector-top { display: flex; justify-content: space-between; gap: 12px; padding: 17px 16px 13px; border-bottom: 1px solid #343438; }.inspector-top span { color: #85858a; font-size: 9px; font-weight: 800; letter-spacing: .1em; }.inspector-top h2 { margin: 5px 0 3px; color: #efeff0; font-size: 16px; }.inspector-top p { max-width: 210px; margin: 0; color: #85858b; font-size: 11px; line-height: 1.45; }.state-badge { align-self: flex-start; padding: 4px 6px; border: 1px solid #4b4b50; border-radius: 5px; color: #b6b6bb !important; font-size: 9px !important; letter-spacing: 0 !important; }.state-executing, .state-planning { color: #eeeeef !important; }.state-failed { color: #df858a !important; }
.inspector-metrics { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); border-bottom: 1px solid #343438; }.inspector-metrics div { display: grid; gap: 3px; padding: 10px 9px; border-right: 1px solid #343438; }.inspector-metrics div:last-child { border-right: 0; }.inspector-metrics span { color: #77777d; font-size: 9px; }.inspector-metrics strong { color: #dcdcdf; font-size: 11px; }.inspector-metrics .danger { color: #dc8186; }
.detail-tabs { display: grid; grid-template-columns: 1fr 1fr; gap: 3px; padding: 7px 9px; border-bottom: 1px solid #343438; }.detail-tabs button { padding: 7px 5px; border: 0; border-radius: 5px; color: #919197; cursor: pointer; background: transparent; font: inherit; font-size: 11px; }.detail-tabs button.active { color: #ededee; background: #2a2a2e; }.detail-tabs i { color: #707076; font-size: 9px; font-style: normal; }
.inspector-scroll { min-height: 0; flex: 1; padding: 15px; overflow-y: auto; }.detail-placeholder { display: grid; min-height: 250px; place-content: center; text-align: center; }.placeholder-glyph { margin-bottom: 10px; color: #a8a8ad; font: 16px ui-monospace, SFMono-Regular, Menlo, monospace; }.detail-placeholder h3 { margin: 0; color: #d8d8db; font-size: 13px; }.detail-placeholder p { max-width: 235px; margin: 8px auto 0; color: #85858b; font-size: 11px; line-height: 1.65; }
.selected-step-heading { padding-bottom: 13px; border-bottom: 1px solid #343438; }.selected-step-heading > span { color: #8e8e94; font-size: 10px; font-weight: 700; }.selected-step-heading h3 { margin: 7px 0 3px; color: #efeff0; font-size: 15px; word-break: break-word; }.selected-step-heading p { margin: 0; color: #97979d; font-size: 11px; line-height: 1.55; }.step-meta { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 10px; color: #77777d; font-size: 10px; }.step-meta b { padding: 2px 4px; border-radius: 3px; font-weight: 700; }.step-meta .status-running { color: #eeeeef; background: #343438; }.step-meta .status-succeeded { color: #c8c8cd; background: #303034; }.step-meta .status-failed { color: #df858a; background: rgba(204,90,96,.13); }.step-meta em { font-style: normal; }
.step-explanation { margin: 13px 0; color: #a5a5ab; font-size: 12px; line-height: 1.65; }.narrative-copy { margin: 15px 0 0; color: #c1c1c5; font-size: 12px; line-height: 1.75; white-space: pre-wrap; }.step-meta .thought { color: #e7e7e9; background: #343438; }.step-meta .human { color: #d4c8c8; background: #373337; }.data-section { margin-top: 14px; }.data-section h4 { margin: 0 0 7px; color: #99999f; font-size: 10px; font-weight: 800; letter-spacing: .04em; }.data-section pre { max-height: 260px; margin: 0; padding: 10px; overflow: auto; border: 1px solid #39393e; border-radius: 7px; color: #c4c4c9; background: #141416; font: 10px/1.55 ui-monospace, SFMono-Regular, Menlo, monospace; white-space: pre-wrap; word-break: break-word; }
.resources-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: 8px; }.resources-heading h3 { margin: 0; color: #dddddf; font-size: 13px; }.resources-heading p { margin: 4px 0 0; color: #77777d; font-size: 10px; }.resources-heading button, .resource-row button { border: 1px solid #47474c; border-radius: 5px; padding: 5px 7px; color: #b7b7bc; cursor: pointer; background: #242428; font: inherit; font-size: 10px; }.resources-heading button:disabled { cursor: wait; opacity: .55; }.resource-error { color: #dc8186; font-size: 11px; line-height: 1.6; }.resource-placeholder { padding: 22px 0; color: #85858b; font-size: 11px; line-height: 1.7; }.resource-row { margin-top: 11px; padding: 10px; border: 1px solid #36363b; border-radius: 7px; background: #202024; }.resource-row > span { color: #77777d; font-size: 9px; font-weight: 700; }.resource-row h4 { margin: 6px 0 3px; color: #e2e2e5; font-size: 12px; }.resource-row p { margin: 0; color: #8a8a90; font-size: 10px; line-height: 1.55; }.resource-row footer { display: flex; align-items: center; justify-content: space-between; gap: 8px; margin-top: 9px; }.resource-row small { color: #74747a; font-size: 9px; }
@media (max-width: 980px) { .manus-inspector { min-height: 460px; border-top: 1px solid #343438; border-left: 0; } }
</style>
