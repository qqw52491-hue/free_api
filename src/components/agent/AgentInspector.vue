<template>
    <aside class="manus-inspector" :class="{ 'pure-preview-mode': activeTab === 'preview' }" aria-label="执行详情与成果画布">
        <!-- 模式 A：纯净成果画布模式 (仅在用户显式点击“展开预览”时进入) -->
        <template v-if="activeTab === 'preview'">
            <header class="canvas-top-bar">
                <div class="canvas-title">
                    <span class="pulse-dot">●</span>
                    <span>成果画布 · 预览</span>
                </div>
                <div class="canvas-actions">
                    <!-- 独立窗口 / 浏览器打开 (HTML专属) -->
                    <button
                        v-if="pureHtmlCode"
                        type="button"
                        class="btn-canvas-action"
                        title="在独立窗口中打开运行此网页"
                        @click="handleOpenCanvasStandalone"
                    >
                        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><path d="M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20"/><path d="M2 12h20"/></svg>
                        <span>单独页面打开</span>
                    </button>

                    <!-- Markdown 专属：导出为 Word (.doc) -->
                    <button
                        v-if="isMarkdownPreview"
                        type="button"
                        class="btn-canvas-action"
                        :title="canvasWordExported ? '已成功导出 Word' : '将当前 Markdown 文档导出为 Word (.doc)'"
                        @click="exportCanvasToWord"
                    >
                        <svg v-if="!canvasWordExported" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="16" y1="13" x2="8" y2="13"/><line x1="16" y1="17" x2="8" y2="17"/><polyline points="10 9 9 9 8 9"/></svg>
                        <svg v-else width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="#22c55e" stroke-width="2.5"><polyline points="20 6 9 17 4 12"/></svg>
                        <span>{{ canvasWordExported ? "已导出Word" : "导出Word" }}</span>
                    </button>

                    <!-- Markdown 专属：导出为 PDF / 打印 -->
                    <button
                        v-if="isMarkdownPreview"
                        type="button"
                        class="btn-canvas-action"
                        title="将当前文档打印或另存为 PDF"
                        @click="exportCanvasToPdf"
                    >
                        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="6 9 6 2 18 2 18 9"/><path d="M6 18H4a2 2 0 0 1-2-2v-5a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2v5a2 2 0 0 1-2 2h-2"/><rect width="12" height="8" x="6" y="14"/></svg>
                        <span>导出PDF</span>
                    </button>

                    <!-- 另存为 -->
                    <button
                        type="button"
                        class="btn-canvas-action"
                        title="另存为本地文件"
                        @click="handleSaveCanvas"
                    >
                        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V4M7 10l5 5 5-5M12 15V3"/></svg>
                        <span>{{ canvasSaved ? '已保存' : '另存为' }}</span>
                    </button>

                    <!-- 返回详情 -->
                    <button type="button" class="btn-exit-canvas" @click="activeTab = 'detail'" title="退出画布并返回调用详情">
                        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>
                        <span>返回详情</span>
                    </button>
                </div>
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

        <!-- 模式 B：标准调用详情与执行状态模式 (专注思维链、参数、报错与上下文) -->
        <template v-else>
            <header class="inspector-top">
                <div>
                    <span>EXECUTION &amp; DEBUG</span>
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
                <div v-if="tokenUsage" :title="`已用 ${tokenUsage.total_tokens} / 窗口 ${tokenUsage.context_window > 0 ? tokenUsage.context_window : '-'}`">
                    <span>上下文</span><strong>{{ tokenUsage.usage_percent.toFixed(1) }}%</strong>
                </div>
                <div v-else-if="failedToolCount"><span>异常</span><strong class="danger">{{ failedToolCount }}</strong></div>
            </div>

            <nav class="detail-tabs">
                <button type="button" :class="{ active: activeTab === 'detail' }" @click="activeTab = 'detail'">调用详情</button>
                <button type="button" :class="{ active: activeTab === 'team' }" @click="activeTab = 'team'">
                    团队协作
                    <i v-if="teamState?.workers?.length">({{ teamState.workers.length }})</i>
                    <span v-else-if="teamState && teamState.activeWorkerCount > 0" class="team-tab-pulse"></span>
                </button>
                <button type="button" :class="{ active: activeTab === 'context' }" @click="activeTab = 'context'">
                    上下文调试 <i v-if="contextTurnsCount">({{ contextTurnsCount }})</i>
                </button>
                <button type="button" :class="{ active: activeTab === 'resources' }" @click="activeTab = 'resources'">资源 <i>{{ resources.length }}</i></button>
            </nav>

            <!-- 1. 调用详情 Tab (默认) -->
            <section v-if="activeTab === 'detail'" class="inspector-scroll">
                <div v-if="selectedHuman" class="narrative-detail">
                    <div class="selected-step-heading">
                        <span>人工交互</span>
                        <h3>你补充了说明</h3>
                        <div class="step-meta">
                            <b class="human">人工输入</b>
                            <em>{{ formatClock(selectedHuman.timestamp) }}</em>
                        </div>
                    </div>
                    <div class="data-section">
                        <div class="section-title-row">
                            <h4>补充内容</h4>
                            <button type="button" class="btn-copy" @click="copyText(selectedHuman.message, '人工说明')">
                                {{ copiedKey === '人工说明' ? '已复制 ✓' : '复制说明' }}
                            </button>
                        </div>
                        <p class="narrative-copy">{{ selectedHuman.message }}</p>
                    </div>
                </div>
                <div v-else-if="selectedActiveThought" class="narrative-detail">
                    <div class="selected-step-heading">
                        <span>实时思考</span>
                        <h3>正在思考</h3>
                        <div class="step-meta"><b class="thought">思考中</b></div>
                    </div>
                    <div class="data-section">
                        <div class="section-title-row">
                            <h4>思考过程</h4>
                            <button v-if="activeThoughtText" type="button" class="btn-copy" @click="copyText(activeThoughtText, '思考内容')">
                                {{ copiedKey === '思考内容' ? '已复制 ✓' : '复制思考' }}
                            </button>
                        </div>
                        <p class="narrative-copy">{{ activeThoughtText || '正在等待模型返回思考内容…' }}</p>
                    </div>
                </div>
                <div v-else-if="selectedStep && selectedSubpart === 'thought'" class="narrative-detail">
                    <div class="selected-step-heading">
                        <span>步骤 {{ selectedStep.stepId + 1 }} · 思考过程</span>
                        <h3>{{ selectedStep.description || '执行步骤' }}</h3>
                        <div class="step-meta">
                            <b class="thought">已分析</b>
                            <em>{{ formatClock(selectedStep.timestamp) }}</em>
                        </div>
                    </div>
                    <div class="data-section">
                        <div class="section-title-row">
                            <h4>思考推理</h4>
                            <button type="button" class="btn-copy" @click="copyText(selectedStep.thought || selectedStep.reflection || '', '步骤思考')">
                                {{ copiedKey === '步骤思考' ? '已复制 ✓' : '复制思考' }}
                            </button>
                        </div>
                        <p class="narrative-copy">{{ selectedStep.thought || selectedStep.reflection || '该步骤没有单独的思考内容。' }}</p>
                    </div>
                </div>
                <div v-else-if="!selectedStep" class="detail-placeholder">
                    <div class="placeholder-glyph">›_</div>
                    <h3>选择一个过程步骤</h3>
                    <p>中间对话流中的每个工具步骤都可点击。这里会显示该次真实调用的请求参数、执行输出与完整错误诊断。</p>
                </div>
                <template v-else>
                    <div class="selected-step-heading">
                        <span>{{ selectedStep.source }} · 步骤 {{ selectedStep.stepId + 1 }}</span>
                        <h3>{{ selectedStep.tool || 'Agent 操作' }}</h3>
                        <p>{{ selectedStep.description || selectedStep.action }}</p>
                        <div class="step-meta">
                            <b :class="`status-${selectedStep.status}`">{{ statusLabel(selectedStep.status) }}</b>
                            <em v-if="selectedStep.durationMs">{{ formatMilliseconds(selectedStep.durationMs) }}</em>
                            <em>{{ formatClock(selectedStep.timestamp) }}</em>
                        </div>
                    </div>

                    <!-- 错误诊断区 (如果步骤失败或存在错误) -->
                    <div v-if="selectedStep.status === 'failed' || stepErrorText" class="error-diagnostic-card">
                        <div class="card-header">
                            <span class="error-badge">⚠️ 异常报错与失败诊断</span>
                            <button type="button" class="btn-copy danger" @click="copyText(stepErrorText, '错误日志')">
                                {{ copiedKey === '错误日志' ? '已复制 ✓' : '复制报错' }}
                            </button>
                        </div>
                        <pre class="error-pre">{{ stepErrorText }}</pre>
                    </div>

                    <p v-if="selectedStep.reflection || selectedStep.explanation" class="step-explanation">
                        {{ selectedStep.reflection || selectedStep.explanation }}
                    </p>

                    <!-- 调用输入 -->
                    <section class="data-section">
                        <div class="section-title-row">
                            <h4>调用输入 (Request Payload)</h4>
                            <button type="button" class="btn-copy" @click="copyText(formattedInput, '调用输入')">
                                {{ copiedKey === '调用输入' ? '已复制 ✓' : '复制参数' }}
                            </button>
                        </div>
                        <pre class="code-pre">{{ formattedInput }}</pre>
                    </section>

                    <!-- 执行输出 -->
                    <section class="data-section">
                        <div class="section-title-row">
                            <h4>执行输出 (Execution Output)</h4>
                            <button type="button" class="btn-copy" @click="copyText(formattedOutput, '执行输出')">
                                {{ copiedKey === '执行输出' ? '已复制 ✓' : '复制结果' }}
                            </button>
                        </div>
                        <pre class="code-pre">{{ formattedOutput }}</pre>
                    </section>
                </template>
            </section>

            <!-- 2. 上下文调试 Tab -->
            <section v-else-if="activeTab === 'context'" class="inspector-scroll context-pane">
                <div class="context-heading">
                    <div>
                        <h3>运行时上下文 (Context Debugger)</h3>
                        <p>大模型在每一轮规划时接收的完整上下文与三明治状态卡。</p>
                    </div>
                    <span v-if="logContext" class="log-disk-badge" title="调试模式开启时，日志将自动同步写入本地 agent_logs/ 目录">📁 已同步落盘</span>
                </div>

                <!-- 大模型格式异常与原始返回记录 (LLM Parse Errors & Raw Outputs) -->
                <div v-if="llmParseErrors && llmParseErrors.length" class="context-card parse-error-card">
                    <div class="card-header">
                        <span class="tag-badge danger">💥 模型输出格式异常与原始返回 ({{ llmParseErrors.length }})</span>
                    </div>
                    <div class="parse-error-list">
                        <div v-for="(err, idx) in llmParseErrors" :key="idx" class="parse-error-item">
                            <div class="parse-error-meta">
                                <strong>步骤 #{{ err.stepId + 1 }} · 尝试第 {{ err.tryIndex }} 次</strong>
                                <button type="button" class="btn-copy small" @click="copyText(err.rawResponse, `原始返回#${idx + 1}`)">
                                    {{ copiedKey === `原始返回#${idx + 1}` ? '已复制 ✓' : '复制原始返回' }}
                                </button>
                            </div>
                            <div class="parse-error-reason">
                                <b>❌ 解析报错:</b> <span>{{ err.error }}</span>
                            </div>
                            <div v-if="err.thinkingText" class="parse-error-thinking">
                                <b>🧠 当时思考:</b>
                                <pre class="pre-block">{{ err.thinkingText }}</pre>
                            </div>
                            <div class="parse-error-raw">
                                <b>📄 模型原始输出 (Raw Output):</b>
                                <pre class="pre-block">{{ err.rawResponse }}</pre>
                            </div>
                        </div>
                    </div>
                </div>

                <!-- Token 容量与自适应压缩监控卡片 -->
                <div class="context-card budget-status-card">
                    <div class="budget-header">
                        <div class="budget-title">
                            <span class="budget-icon">📊</span>
                            <strong>Token 预算与归档监控</strong>
                        </div>
                        <span class="budget-tag" :class="compactionCardsCount > 0 ? 'compacted' : 'safe'">
                            {{ compactionCardsCount > 0 ? `📦 已归档 (${compactionCardsCount} 张归档卡)` : `🟢 容量健康 (${tokenUsagePercent}%)` }}
                        </span>
                    </div>
                    <div class="budget-progress-bar">
                        <div
                            class="progress-fill"
                            :style="{ width: `${tokenUsagePercent}%` }"
                            :class="{ 'near-limit': tokenUsagePercent > 75, compacted: compactionCardsCount > 0 }"
                        ></div>
                    </div>
                    <div class="budget-meta-row">
                        <span>当前历史: <b>~{{ estimatedHistoryTokens.toLocaleString() }}</b> Tokens</span>
                        <span>80% 触发线: <b>{{ highWatermarkTokens.toLocaleString() }}</b> Tokens</span>
                        <span>窗口总量: <b>{{ Math.round(contextWindowTokens / 1000) }}k</b></span>
                    </div>
                    <div v-if="compactionCardsCount > 0" class="compaction-notice">
                        ℹ️ 当前会话已触发自适应归档压缩，早期历史已提炼为归档摘要卡，释放了大量上下文空间。
                    </div>
                </div>

                <!-- 任务主目标 -->
                <div class="context-card goal-card">
                    <div class="card-header">
                        <span class="tag-badge">🎯 当前目标</span>
                        <button v-if="effectiveGoal" type="button" class="btn-copy" @click="copyText(effectiveGoal, '目标')">
                            {{ copiedKey === '目标' ? '已复制 ✓' : '复制' }}
                        </button>
                    </div>
                    <p class="card-text">{{ effectiveGoal || '暂无任务目标' }}</p>
                </div>

                <!-- 避坑指南 (如果存在失败教训) -->
                <div v-if="failedAttempts.length" class="context-card failure-card">
                    <div class="card-header">
                        <span class="tag-badge danger">⚠️ 避坑教训库 ({{ failedAttempts.length }})</span>
                    </div>
                    <div class="failure-list">
                        <div v-for="(attempt, idx) in failedAttempts" :key="idx" class="failure-item">
                            <strong>步骤 #{{ attempt.step_id }}: {{ attempt.tool }}/{{ attempt.action }}</strong>
                            <p>{{ attempt.error_summary }}</p>
                        </div>
                    </div>
                </div>

                <!-- 事实便签 (Memories) -->
                <div v-if="memoriesList.length" class="context-card memory-card">
                    <div class="card-header">
                        <span class="tag-badge memory">📌 核心事实便签 ({{ memoriesList.length }})</span>
                    </div>
                    <div class="memory-grid">
                        <div v-for="item in memoriesList" :key="item.key" class="memory-item">
                            <span class="mem-key">{{ item.key }}</span>
                            <p class="mem-val">{{ item.value }}</p>
                        </div>
                    </div>
                </div>

                <!-- 完整对话消息流 (Turns History) -->
                <div class="context-turns-section">
                    <div class="section-title-row">
                        <h4>提示词消息流水 ({{ contextTurnsList.length }} 条)</h4>
                        <button v-if="contextTurnsList.length" type="button" class="btn-copy" @click="copyAllMessages">
                            {{ copiedKey === '全部消息' ? '已复制 ✓' : '复制全部上下文' }}
                        </button>
                    </div>

                    <!-- 消息分类筛选胶囊 -->
                    <div v-if="contextTurnsList.length" class="context-filter-pills">
                        <button
                            type="button"
                            class="filter-pill"
                            :class="{ active: messageFilter === 'dialog' }"
                            @click="messageFilter = 'dialog'"
                        >
                            💬 对话与执行观测 ({{ dialogTurnsCount }})
                        </button>
                        <button
                            type="button"
                            class="filter-pill"
                            :class="{ active: messageFilter === 'all' }"
                            @click="messageFilter = 'all'"
                        >
                            全部流水 ({{ contextTurnsList.length }})
                        </button>
                        <button
                            v-if="systemTurnsCount"
                            type="button"
                            class="filter-pill"
                            :class="{ active: messageFilter === 'system' }"
                            @click="messageFilter = 'system'"
                        >
                            ⚙️ 系统提示词 ({{ systemTurnsCount }})
                        </button>
                    </div>

                    <div v-if="!filteredTurnsList.length" class="empty-context-tip">
                        当前筛选下无消息内容。
                    </div>

                    <article
                        v-for="msg in filteredTurnsList"
                        :key="msg.originalIndex"
                        class="turn-message-card"
                        :class="[isArchiveMessage(msg.content) ? 'role-archive' : `role-${msg.role}`, { collapsed: collapsedMessages[`msg-${msg.originalIndex}`] }]"
                    >
                        <header
                            class="turn-header"
                            @click="toggleMessageCollapse(`msg-${msg.originalIndex}`)"
                        >
                            <div class="role-badge" :class="isArchiveMessage(msg.content) ? 'badge-archive' : `badge-${msg.role}`">
                                <span class="collapse-arrow">{{ collapsedMessages[`msg-${msg.originalIndex}`] ? '▶' : '▼' }}</span>
                                <span v-if="isArchiveMessage(msg.content)">📦 历史归档卡片</span>
                                <span v-else>{{ msg.role.toUpperCase() }}</span>
                                <small>#{{ msg.originalIndex + 1 }} · {{ messageCharCount(msg.content) }} 字</small>
                            </div>
                            <div class="turn-header-actions" @click.stop>
                                <button
                                    type="button"
                                    class="btn-copy small"
                                    @click="copyText(formatMessageContent(msg.content), `消息#${msg.originalIndex + 1}`)"
                                >
                                    {{ copiedKey === `消息#${msg.originalIndex + 1}` ? '已复制 ✓' : '复制' }}
                                </button>
                            </div>
                        </header>

                        <!-- 折叠时显示的简要单行预览 -->
                        <div
                            v-if="collapsedMessages[`msg-${msg.originalIndex}`]"
                            class="turn-body-snippet"
                            @click="toggleMessageCollapse(`msg-${msg.originalIndex}`)"
                        >
                            {{ formatMessageContent(msg.content).slice(0, 140) }}… <span class="snippet-expand">（点击展开）</span>
                        </div>

                        <!-- 展开时的完整正文 -->
                        <pre
                            v-else
                            class="turn-body"
                        >{{ formatMessageContent(msg.content) }}</pre>
                    </article>
                </div>
            </section>

            <!-- 3. 团队协作 Tab (右侧实时监控区：仅展示活跃运行中的子 Agent) -->
            <section v-else-if="activeTab === 'team'" class="inspector-scroll team-pane">
                <AgentTeamDeck :team-state="teamState" :only-active="true" />
            </section>

            <!-- 4. 资源 Tab -->
            <section v-else-if="activeTab === 'resources'" class="inspector-scroll resources-pane">
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
import type { AgentRunState, AgentRunStateMeta, AgentStepItem, AgentTaskResource, AgentTeamState, AgentTokenUsage, AgentToolStatus, LlmParseErrorItem } from "./types";
import type { AgentArtifactInput } from "../../types/artifact";
import { formatClock, formatMilliseconds, formatNumber, formatPayload } from "./format";
import RichContentRenderer from "../charts/RichContentRenderer.vue";
import HtmlPreviewBlock from "../charts/HtmlPreviewBlock.vue";
import AgentTeamDeck from "./AgentTeamDeck.vue";

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
        finalArtifacts?: AgentArtifactInput[];
        activePreviewContent?: string;
        latestContext?: Record<string, unknown> | null;
        currentGoal?: string;
        logContext?: boolean;
        llmParseErrors?: LlmParseErrorItem[];
        teamState?: AgentTeamState;
    }>(),
    {
        finalSummary: "",
        finalArtifacts: () => [],
        activePreviewContent: "",
        latestContext: null,
        currentGoal: "",
        logContext: false,
        llmParseErrors: () => [],
    }
);

defineEmits<{
    "refresh-resources": [];
    "preview-resource": [resource: AgentTaskResource];
    "agent-submit": [data: any];
    "agent-cancel": [reason: string];
    "close": [];
}>();

const activeTab = ref<"preview" | "detail" | "team" | "context" | "resources">("detail");

function switchTab(tab: "preview" | "detail" | "team" | "context" | "resources") {
    activeTab.value = tab;
}

defineExpose({
    setTab: switchTab,
    switchTab,
    activeTab,
});
const copiedKey = ref<string | null>(null);
let copyTimer: ReturnType<typeof setTimeout> | null = null;

async function copyText(text: string, label: string) {
    if (!text) return;
    try {
        await navigator.clipboard.writeText(text);
        copiedKey.value = label;
        if (copyTimer) clearTimeout(copyTimer);
        copyTimer = setTimeout(() => {
            copiedKey.value = null;
        }, 2000);
    } catch (e) {
        console.error("复制失败:", e);
    }
}

const formattedInput = computed(() => {
    if (!props.selectedStep) return "";
    return formatPayload(props.selectedStep.rawParams ?? props.selectedStep.params ?? {});
});

const formattedOutput = computed(() => {
    if (!props.selectedStep) return "";
    return formatPayload(props.selectedStep.rawResult ?? props.selectedStep.result ?? {});
});

const stepErrorText = computed(() => {
    if (!props.selectedStep) return "";
    const step = props.selectedStep;
    const raw = step.rawResult ?? step.result;
    if (typeof raw === "object" && raw !== null) {
        const obj = raw as Record<string, unknown>;
        if (obj.stderr && String(obj.stderr).trim()) return String(obj.stderr).trim();
        if (obj.error && String(obj.error).trim()) return String(obj.error).trim();
    }
    if (typeof raw === "string" && step.status === "failed") {
        return raw.trim();
    }
    if (step.status === "failed") {
        return "工具调用执行异常，未返回标准成功信号。";
    }
    return "";
});

// 上下文数据解析
const effectiveGoal = computed(() => {
    if (props.currentGoal) return props.currentGoal;
    if (props.latestContext) {
        const ctx = props.latestContext;
        return String(ctx.user_goal || (ctx.context as Record<string, unknown>)?.user_goal || "");
    }
    return "";
});

const failedAttempts = computed(() => {
    if (!props.latestContext) return [];
    const ctx = props.latestContext;
    const list = ctx.failed_attempts || (ctx.context as Record<string, unknown>)?.failed_attempts;
    return Array.isArray(list) ? list : [];
});

const memoriesList = computed(() => {
    if (!props.latestContext) return [];
    const ctx = props.latestContext;
    const mems = ctx.memories || (ctx.context as Record<string, unknown>)?.memories;
    if (mems && typeof mems === "object" && !Array.isArray(mems)) {
        return Object.entries(mems).map(([key, value]) => ({ key, value: String(value) }));
    }
    return [];
});

interface TurnMessage {
    role: string;
    content: unknown;
}

const contextTurnsList = computed<TurnMessage[]>(() => {
    if (!props.latestContext) return [];
    const ctx = props.latestContext;
    const list = ctx.turns_history || (ctx.context as Record<string, unknown>)?.turns_history;
    const res: TurnMessage[] = [];
    
    // 如果存在系统提示词，先加入第一项
    const systemPrompt = ctx.system_prompt || (ctx.context as Record<string, unknown>)?.system_prompt;
    if (systemPrompt && typeof systemPrompt === "string" && systemPrompt.trim()) {
        res.push({
            role: "system",
            content: systemPrompt.trim(),
        });
    }

    if (Array.isArray(list)) {
        for (const item of list) {
            if (item && typeof item === "object") {
                const role = String((item as Record<string, unknown>).role || "user");
                const content = (item as Record<string, unknown>).content;
                res.push({ role, content });
            }
        }
    }
    return res;
});

const contextTurnsCount = computed(() => contextTurnsList.value.length);

function formatMessageContent(content: unknown): string {
    if (typeof content === "string") return content;
    try {
        return JSON.stringify(content, null, 2);
    } catch {
        return String(content);
    }
}

function messageCharCount(content: unknown): number {
    if (typeof content === "string") return content.length;
    return JSON.stringify(content).length;
}

const contextWindowTokens = computed(() => {
    const ctx = props.latestContext;
    if (!ctx) return 128000;
    const rawVal = ctx.context_window_tokens || (ctx.context as Record<string, unknown>)?.context_window_tokens;
    const num = Number(rawVal);
    return num > 0 ? num : 128000;
});

const estimatedHistoryTokens = computed(() => {
    let total = 0;
    for (const msg of contextTurnsList.value) {
        const text = formatMessageContent(msg.content);
        total += Math.ceil(text.length * 0.65);
    }
    return total;
});

const highWatermarkTokens = computed(() => {
    const w = contextWindowTokens.value;
    const reserved = 4096 + 3000 + 1000 + Math.ceil(w * 0.15);
    const maxAllowed = Math.max(2048, w - reserved);
    return Math.floor(maxAllowed * 0.80);
});

const tokenUsagePercent = computed(() => {
    if (!highWatermarkTokens.value) return 0;
    return Math.min(100, Math.round((estimatedHistoryTokens.value / highWatermarkTokens.value) * 100));
});

function isArchiveMessage(content: unknown): boolean {
    const text = typeof content === "string" ? content : JSON.stringify(content);
    return text.includes("【历史早期执行归档卡片");
}

const compactionCardsCount = computed(() => {
    return contextTurnsList.value.filter(m => isArchiveMessage(m.content)).length;
});

const messageFilter = ref<"all" | "dialog" | "system">("dialog");
const collapsedMessages = ref<Record<string, boolean>>({ "msg-0": true });

function toggleMessageCollapse(key: string) {
    collapsedMessages.value[key] = !collapsedMessages.value[key];
}

const filteredTurnsList = computed(() => {
    const list = contextTurnsList.value.map((msg, idx) => ({ ...msg, originalIndex: idx }));
    if (messageFilter.value === "dialog") {
        return list.filter(m => m.role !== "system");
    }
    if (messageFilter.value === "system") {
        return list.filter(m => m.role === "system");
    }
    return list;
});

const dialogTurnsCount = computed(() => contextTurnsList.value.filter(m => m.role !== "system").length);
const systemTurnsCount = computed(() => contextTurnsList.value.filter(m => m.role === "system").length);

function copyAllMessages() {
    const full = contextTurnsList.value.map(m => `=== [${m.role.toUpperCase()}] ===\n${formatMessageContent(m.content)}`).join("\n\n");
    copyText(full, "全部消息");
}

const combinedPreviewContent = computed(() => {
    if (props.activePreviewContent && props.activePreviewContent.trim()) {
        return props.activePreviewContent;
    }
    const parts: string[] = [];
    if (props.finalSummary) parts.push(props.finalSummary);
    if (props.finalArtifacts && props.finalArtifacts.length) {
        for (const art of props.finalArtifacts) {
            const str = typeof art === "string" ? art : (art.content || art.localPath || art.name || "");
            if (str && !parts.includes(str)) parts.push(str);
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
    return Boolean(text && text.trim().length > 0);
});

const isMarkdownPreview = computed(() => {
    return !pureHtmlCode.value && Boolean(hasPreviewContent.value);
});

const canvasSaved = ref(false);
const canvasWordExported = ref(false);

async function handleOpenCanvasStandalone() {
    if (pureHtmlCode.value) {
        const { openWebviewWindow } = await import("../../composables/useBrowserPreview");
        const blob = new Blob([pureHtmlCode.value], { type: "text/html" });
        const url = URL.createObjectURL(blob);
        await openWebviewWindow(url, "成果画布 · 独立窗口");
    }
}

async function exportCanvasToWord() {
    const rawContent = combinedPreviewContent.value;
    if (!rawContent) return;

    let contentHtml = rawContent;
    try {
        const { marked } = await import("marked");
        const DOMPurify = (await import("dompurify")).default;
        contentHtml = DOMPurify.sanitize(marked.parse(rawContent) as string);
    } catch {
        contentHtml = `<pre>${rawContent}</pre>`;
    }

    const title = props.currentGoal || "文档成果导出";

    const wordDocContent = `<!DOCTYPE html>
<html xmlns:o='urn:schemas-microsoft-com:office:office' xmlns:w='urn:schemas-microsoft-com:office:word' xmlns='http://www.w3.org/TR/REC-html40'>
<head>
<meta charset='utf-8'>
<title>${title}</title>
<!--[if gte mso 9]>
<xml>
<w:WordDocument>
<w:View>Print</w:View>
<w:Zoom>100</w:Zoom>
<w:DoNotOptimizeForBrowser/>
</w:WordDocument>
</xml>
<![endif]-->
<style>
  body {
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "PingFang SC", "Microsoft YaHei", sans-serif;
    font-size: 11pt;
    line-height: 1.6;
    color: #1e293b;
    padding: 20px;
  }
  h1 { font-size: 20pt; font-weight: bold; color: #0f172a; border-bottom: 2px solid #6366f1; padding-bottom: 6px; margin-bottom: 16px; }
  h2 { font-size: 15pt; font-weight: bold; color: #1e293b; margin-top: 20px; margin-bottom: 10px; border-bottom: 1px solid #e2e8f0; padding-bottom: 4px; }
  h3 { font-size: 13pt; font-weight: 600; color: #334155; margin-top: 14px; margin-bottom: 8px; }
  p { margin: 8px 0; }
  ul, ol { padding-left: 24px; margin: 8px 0; }
  li { margin: 4px 0; }
  code { font-family: Consolas, Monaco, "Courier New", monospace; font-size: 10pt; background: #f1f5f9; padding: 2px 5px; border-radius: 4px; color: #475569; }
  pre { background: #0f172a; color: #f8fafc; padding: 14px; border-radius: 6px; font-family: Consolas, monospace; font-size: 9.5pt; line-height: 1.4; overflow-x: auto; }
  pre code { background: transparent; color: inherit; padding: 0; }
  table { border-collapse: collapse; width: 100%; margin: 16px 0; font-size: 10pt; }
  th, td { border: 1px solid #cbd5e1; padding: 8px 12px; text-align: left; }
  th { background-color: #f8fafc; font-weight: 600; color: #334155; }
  tr:nth-child(even) { background-color: #f8fafc; }
  blockquote { border-left: 4px solid #6366f1; margin: 12px 0; padding: 8px 16px; background: #f8fafc; color: #64748b; }
  hr { border: none; border-top: 1px solid #e2e8f0; margin: 20px 0; }
</style>
</head>
<body>
  <h1>${title}</h1>
  ${contentHtml}
</body>
</html>`;

    try {
        const { save } = await import("@tauri-apps/plugin-dialog");
        const targetPath = await save({
            defaultPath: `${title}.doc`,
            filters: [{ name: "Word Document (*.doc)", extensions: ["doc"] }],
        });
        if (!targetPath) return;

        const { invoke } = await import("@tauri-apps/api/core");
        await invoke("export_artifact_to_file", {
            artifactId: null,
            sourcePath: null,
            content: wordDocContent,
            targetPath,
        });

        canvasWordExported.value = true;
        setTimeout(() => {
            canvasWordExported.value = false;
        }, 2500);
    } catch (err) {
        console.warn("导出 Word 失败:", err);
    }
}

async function exportCanvasToPdf() {
    const rawContent = combinedPreviewContent.value;
    if (!rawContent) return;

    let contentHtml = rawContent;
    try {
        const { marked } = await import("marked");
        const DOMPurify = (await import("dompurify")).default;
        contentHtml = DOMPurify.sanitize(marked.parse(rawContent) as string);
    } catch {
        contentHtml = `<pre>${rawContent}</pre>`;
    }

    const title = props.currentGoal || "文档成果导出";

    const printWindow = window.open("", "_blank");
    if (!printWindow) {
        window.print();
        return;
    }

    printWindow.document.write(`<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>${title}</title>
<style>
  @page { margin: 20mm 15mm; size: A4; }
  body {
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "PingFang SC", "Microsoft YaHei", sans-serif;
    font-size: 11pt;
    line-height: 1.6;
    color: #111827;
    margin: 0;
    padding: 0;
  }
  h1 { font-size: 22pt; font-weight: bold; color: #111827; border-bottom: 2px solid #4f46e5; padding-bottom: 8px; margin-bottom: 20px; }
  h2 { font-size: 16pt; font-weight: bold; color: #1f2937; margin-top: 24px; margin-bottom: 12px; border-bottom: 1px solid #e5e7eb; padding-bottom: 4px; }
  h3 { font-size: 13pt; font-weight: 600; color: #374151; margin-top: 16px; margin-bottom: 8px; }
  p { margin: 8px 0; }
  ul, ol { padding-left: 24px; margin: 8px 0; }
  code { font-family: Consolas, monospace; font-size: 9.5pt; background: #f3f4f6; padding: 2px 5px; border-radius: 4px; }
  pre { background: #f8fafc; border: 1px solid #e2e8f0; color: #0f172a; padding: 12px; border-radius: 6px; font-family: Consolas, monospace; font-size: 9pt; page-break-inside: avoid; }
  pre code { background: transparent; padding: 0; }
  table { border-collapse: collapse; width: 100%; margin: 16px 0; font-size: 9.5pt; page-break-inside: avoid; }
  th, td { border: 1px solid #d1d5db; padding: 8px 12px; text-align: left; }
  th { background-color: #f9fafb; font-weight: 600; }
  blockquote { border-left: 4px solid #4f46e5; margin: 12px 0; padding: 6px 14px; background: #f9fafb; color: #4b5563; }
</style>
</head>
<body>
  <h1>${title}</h1>
  ${contentHtml}
</body>
</html>`);
    printWindow.document.close();
    printWindow.focus();
    setTimeout(() => {
        printWindow.print();
        printWindow.close();
    }, 250);
}

async function handleSaveCanvas() {
    try {
        const { save } = await import("@tauri-apps/plugin-dialog");
        const isHtml = Boolean(pureHtmlCode.value);
        const targetPath = await save({
            defaultPath: isHtml ? "dashboard.html" : "document.md",
            filters: isHtml
                ? [{ name: "HTML Document (*.html)", extensions: ["html", "htm"] }]
                : [{ name: "Markdown Document (*.md)", extensions: ["md", "txt"] }],
        });
        if (!targetPath) return;

        const { invoke } = await import("@tauri-apps/api/core");
        await invoke("export_artifact_to_file", {
            artifactId: null,
            sourcePath: null,
            content: pureHtmlCode.value || combinedPreviewContent.value,
            targetPath,
        });

        canvasSaved.value = true;
        setTimeout(() => { canvasSaved.value = false; }, 2000);
    } catch (err) {
        console.warn("另存为失败:", err);
    }
}

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

function statusLabel(status: AgentToolStatus) { return { pending: "等待", running: "执行中", succeeded: "完成", failed: "失败", challenged: "验证未闭合" }[status] || "未知"; }
</script>

<style scoped>
.manus-inspector {
    display: flex;
    min-width: 0;
    min-height: 0;
    height: 100%;
    max-height: 100%;
    flex-direction: column;
    border-left: 1px solid rgba(255, 255, 255, 0.08);
    color: #dedee1;
    background: #18181b;
    overflow: hidden;
}

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
.canvas-actions {
    display: flex;
    align-items: center;
    gap: 6px;
}
.btn-canvas-action {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 4px 8px;
    border-radius: 6px;
    border: 1px solid rgba(255, 255, 255, 0.12);
    background: rgba(255, 255, 255, 0.05);
    color: #e4e4e7;
    font-size: 11px;
    font-weight: 500;
    cursor: pointer;
    transition: all 0.15s ease;
}
.btn-canvas-action:hover {
    background: rgba(255, 255, 255, 0.12);
    color: #ffffff;
    border-color: rgba(255, 255, 255, 0.2);
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

.detail-tabs { display: grid; grid-template-columns: 1fr 1fr 1fr; gap: 3px; padding: 6px 8px; border-bottom: 1px solid rgba(255, 255, 255, 0.06); background: #141416; }
.detail-tabs button { display: inline-flex; align-items: center; justify-content: center; gap: 4px; padding: 6px 4px; border: 0; border-radius: 5px; color: #a1a1aa; cursor: pointer; background: transparent; font: inherit; font-size: 11px; font-weight: 550; transition: all 0.15s; }
.detail-tabs button:hover { color: #ffffff; }
.inspector-scroll {
    min-height: 0;
    flex: 1 1 0%;
    height: 0;
    padding: 14px;
    overflow-y: auto !important;
    overflow-x: hidden !important;
    scrollbar-gutter: stable;
    scrollbar-width: thin;
    scrollbar-color: rgba(255, 255, 255, 0.35) transparent;
}
.inspector-scroll::-webkit-scrollbar {
    width: 8px;
}
.inspector-scroll::-webkit-scrollbar-track {
    background: rgba(0, 0, 0, 0.25);
}
.inspector-scroll::-webkit-scrollbar-thumb {
    background: rgba(255, 255, 255, 0.32);
    border-radius: 4px;
    border: 1px solid rgba(0, 0, 0, 0.4);
}
.inspector-scroll::-webkit-scrollbar-thumb:hover {
    background: rgba(255, 255, 255, 0.55);
}

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
.step-meta .status-challenged { color: #e6a23c; background: rgba(230, 162, 60, 0.14); border: 1px solid rgba(230, 162, 60, 0.28); }
.step-meta em { font-style: normal; }

.step-explanation { margin: 12px 0; color: #a1a1aa; font-size: 12px; line-height: 1.65; }
.narrative-copy { margin: 8px 0 0; color: #d4d4d8; font-size: 12px; line-height: 1.7; white-space: pre-wrap; word-break: break-word; }
.step-meta .thought { color: #a78bfa; background: rgba(167, 139, 250, 0.12); }
.step-meta .human { color: #fbbf24; background: rgba(251, 191, 36, 0.12); }

/* 错误诊断高亮卡片 */
.error-diagnostic-card {
    margin-top: 14px;
    padding: 10px 12px;
    border: 1px solid rgba(255, 71, 87, 0.35);
    border-radius: 8px;
    background: rgba(255, 71, 87, 0.08);
}
.error-diagnostic-card .card-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 8px;
}
.error-badge {
    color: #ff4757;
    font-size: 11px;
    font-weight: 650;
}
.error-pre {
    margin: 0;
    color: #ff6b81;
    font: 11px/1.6 ui-monospace, SFMono-Regular, Menlo, monospace;
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 200px;
    overflow-y: auto;
}

/* 数据段与复制按钮 */
.data-section { margin-top: 14px; }
.section-title-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 6px;
}
.section-title-row h4 {
    margin: 0;
    color: #a1a1aa;
    font-size: 10.5px;
    font-weight: 750;
    letter-spacing: .04em;
}
.btn-copy {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 3px 8px;
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 4px;
    background: rgba(255, 255, 255, 0.05);
    color: #d4d4d8;
    font-size: 10px;
    font-weight: 550;
    cursor: pointer;
    transition: all 0.15s ease;
}
.btn-copy:hover {
    border-color: rgba(255, 255, 255, 0.25);
    background: rgba(255, 255, 255, 0.12);
    color: #ffffff;
}
.btn-copy.danger {
    border-color: rgba(255, 71, 87, 0.3);
    color: #ff6b81;
}
.btn-copy.danger:hover {
    background: rgba(255, 71, 87, 0.18);
    color: #fff;
}
.btn-copy.small {
    padding: 2px 6px;
    font-size: 9.5px;
}

.code-pre {
    max-height: 320px;
    margin: 0;
    padding: 10px;
    overflow: auto;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 7px;
    color: #d4d4d8;
    background: #141416;
    font: 11px/1.6 ui-monospace, SFMono-Regular, Menlo, monospace;
    white-space: pre-wrap;
    word-break: break-word;
}

/* 上下文调试面板样式 */
.context-pane {
    display: flex;
    flex-direction: column;
    gap: 12px;
}
.context-heading h3 {
    margin: 0;
    color: #f4f4f5;
    font-size: 13px;
    font-weight: 650;
}
.context-heading p {
    margin: 3px 0 0;
    color: #71717a;
    font-size: 10.5px;
}

.context-card {
    padding: 10px 12px;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 8px;
    background: #1f1f23;
}

/* Token 容量与自适应压缩监控卡片样式 */
.budget-status-card {
    border-color: rgba(56, 189, 248, 0.2);
    background: linear-gradient(180deg, rgba(56, 189, 248, 0.04) 0%, rgba(31, 31, 35, 1) 100%);
}
.budget-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 8px;
}
.budget-title {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    color: #f4f4f5;
}
.budget-icon {
    font-size: 12px;
}
.budget-tag {
    padding: 2px 7px;
    border-radius: 10px;
    font-size: 9.5px;
    font-weight: 650;
}
.budget-tag.safe {
    color: #4ade80;
    background: rgba(74, 222, 128, 0.12);
    border: 1px solid rgba(74, 222, 128, 0.25);
}
.budget-tag.compacted {
    color: #fbbf24;
    background: rgba(251, 191, 36, 0.12);
    border: 1px solid rgba(251, 191, 36, 0.25);
}
.budget-progress-bar {
    height: 4px;
    width: 100%;
    border-radius: 2px;
    background: rgba(255, 255, 255, 0.08);
    overflow: hidden;
    margin-bottom: 8px;
}
.progress-fill {
    height: 100%;
    border-radius: 2px;
    background: linear-gradient(90deg, #38bdf8, #818cf8);
    transition: width 0.3s ease;
}
.progress-fill.near-limit {
    background: linear-gradient(90deg, #fbbf24, #ff4757);
}
.progress-fill.compacted {
    background: linear-gradient(90deg, #a78bfa, #fbbf24);
}
.budget-meta-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-size: 10px;
    color: #71717a;
}
.budget-meta-row b {
    color: #d4d4d8;
}
.compaction-notice {
    margin-top: 8px;
    padding: 5px 8px;
    border-radius: 5px;
    background: rgba(251, 191, 36, 0.08);
    border: 1px solid rgba(251, 191, 36, 0.18);
    color: #fbbf24;
    font-size: 10px;
    line-height: 1.45;
}

.context-card .card-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 6px;
}
.tag-badge {
    font-size: 10.5px;
    font-weight: 650;
    color: #38bdf8;
}
.tag-badge.danger { color: #ff4757; }
.tag-badge.memory { color: #fbbf24; }

.card-text {
    margin: 0;
    color: #d4d4d8;
    font-size: 11.5px;
    line-height: 1.6;
    white-space: pre-wrap;
    word-break: break-word;
}

.failure-list {
    display: grid;
    gap: 6px;
}
.failure-item {
    padding: 6px 8px;
    border-radius: 6px;
    background: rgba(255, 71, 87, 0.06);
    border: 1px solid rgba(255, 71, 87, 0.15);
}
.failure-item strong {
    color: #ff6b81;
    font-size: 11px;
    display: block;
}
.failure-item p {
    margin: 3px 0 0;
    color: #a1a1aa;
    font-size: 10.5px;
    line-height: 1.45;
}

.memory-grid {
    display: grid;
    gap: 6px;
}
.memory-item {
    padding: 6px 8px;
    border-radius: 6px;
    background: rgba(251, 191, 36, 0.05);
    border: 1px solid rgba(251, 191, 36, 0.15);
}
.mem-key {
    color: #fbbf24;
    font-size: 10.5px;
    font-weight: 600;
    display: block;
}
.mem-val {
    margin: 2px 0 0;
    color: #d4d4d8;
    font-size: 11px;
    line-height: 1.45;
    white-space: pre-wrap;
    word-break: break-word;
}

.context-turns-section {
    display: grid;
    gap: 8px;
    margin-top: 6px;
}
.empty-context-tip {
    padding: 16px 0;
    color: #71717a;
    font-size: 11px;
    text-align: center;
    line-height: 1.6;
}

.turn-message-card {
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 7px;
    background: #141416;
    overflow: hidden;
}
.turn-message-card.role-system {
    border-color: rgba(167, 139, 250, 0.25);
    background: rgba(167, 139, 250, 0.03);
}
.turn-message-card.role-user {
    border-color: rgba(56, 189, 248, 0.2);
    background: rgba(56, 189, 248, 0.02);
}
.turn-message-card.role-assistant {
    border-color: rgba(74, 222, 128, 0.2);
    background: rgba(74, 222, 128, 0.02);
}
.turn-message-card.role-archive {
    border-color: rgba(251, 191, 36, 0.35);
    background: linear-gradient(180deg, rgba(251, 191, 36, 0.06) 0%, rgba(20, 20, 22, 1) 100%);
    box-shadow: inset 0 0 12px rgba(251, 191, 36, 0.04);
}

/* 消息分类筛选胶囊与折叠样式 */
.context-filter-pills {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin: 4px 0 8px;
}
.filter-pill {
    padding: 3px 9px;
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 12px;
    background: rgba(255, 255, 255, 0.03);
    color: #a1a1aa;
    font-size: 10px;
    font-weight: 500;
    cursor: pointer;
    transition: all 0.15s ease;
}
.filter-pill:hover {
    color: #f4f4f5;
    background: rgba(255, 255, 255, 0.08);
}
.filter-pill.active {
    border-color: rgba(56, 189, 248, 0.4);
    background: rgba(56, 189, 248, 0.12);
    color: #38bdf8;
    font-weight: 650;
}

.turn-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 6px 10px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.05);
    background: rgba(255, 255, 255, 0.02);
    cursor: pointer;
    user-select: none;
    transition: background 0.15s ease;
}
.turn-header:hover {
    background: rgba(255, 255, 255, 0.04);
}
.collapse-arrow {
    font-size: 8px;
    color: #71717a;
    width: 10px;
    display: inline-block;
}
.turn-body-snippet {
    padding: 7px 10px;
    color: #71717a;
    font: 10.5px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace;
    cursor: pointer;
    background: rgba(0, 0, 0, 0.2);
    transition: background 0.15s ease;
}
.turn-body-snippet:hover {
    background: rgba(255, 255, 255, 0.03);
    color: #a1a1aa;
}
.snippet-expand {
    color: #38bdf8;
    font-size: 10px;
}
.role-badge {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 10px;
    font-weight: 750;
}
.badge-system span { color: #a78bfa; }
.badge-user span { color: #38bdf8; }
.badge-assistant span { color: #4ade80; }
.badge-archive span { color: #fbbf24; font-weight: 800; }
.role-badge small {
    color: #71717a;
    font-size: 9px;
    font-weight: normal;
}

.turn-body {
    margin: 0;
    padding: 10px;
    max-height: 280px;
    overflow-y: auto;
    color: #d4d4d8;
    font: 11px/1.6 ui-monospace, SFMono-Regular, Menlo, monospace;
    white-space: pre-wrap;
    word-break: break-word;
}

/* 资源面板样式 */
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

/* 模型解析异常卡片样式 */
.parse-error-card {
    border-color: rgba(239, 68, 68, 0.35);
    background: rgba(239, 68, 68, 0.05);
}
.parse-error-list {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin-top: 8px;
}
.parse-error-item {
    padding: 10px;
    border-radius: 8px;
    border: 1px solid rgba(239, 68, 68, 0.2);
    background: rgba(0, 0, 0, 0.35);
    display: flex;
    flex-direction: column;
    gap: 8px;
}
.parse-error-meta {
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-size: 11px;
    color: #fca5a5;
}
.parse-error-reason {
    font-size: 11px;
    color: #ef4444;
    line-height: 1.5;
}
.parse-error-reason b {
    color: #f87171;
}
.parse-error-thinking b,
.parse-error-raw b {
    display: block;
    font-size: 10.5px;
    color: #a1a1aa;
    margin-bottom: 4px;
}
.parse-error-item .pre-block {
    max-height: 220px;
    overflow-y: auto;
    background: #09090b;
    border-color: rgba(239, 68, 68, 0.2);
}

@media (max-width: 980px) { .manus-inspector { min-height: 460px; border-top: 1px solid rgba(255, 255, 255, 0.08); border-left: 0; } }
</style>
