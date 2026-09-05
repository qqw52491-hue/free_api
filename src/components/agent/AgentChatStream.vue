<template>
    <div ref="streamElement" class="process-tree" :class="{ 'is-running': isRunning }" aria-label="Agent 分层执行过程" @scroll.passive="onStreamScroll">
        <button v-if="isRunning && !followLiveTail" type="button" class="follow-latest" @click="resumeLiveFollow">跟随最新输出</button>

        <div class="stream-centered-content">
            <section v-if="!currentGoal && !timelineEntries.length && !legacyMessages.length" class="empty-conversation">
                <div class="empty-logo">
                    <svg width="22" height="22" viewBox="0 0 24 24" fill="currentColor" stroke="none">
                        <polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"></polygon>
                    </svg>
                </div>
                <h2>有什么可以帮你的？</h2>
                <p>输入一个任务。每个回合会保留自己的过程、成果与总结。</p>
            </section>

            <!-- 早期对话记录 -->
            <section v-if="legacyMessages.length" class="conversation-history" aria-label="早期对话记录">
                <article v-for="message in legacyMessages" :key="message.id" class="saved-message" :class="message.role === 'user' ? 'from-user' : 'from-agent'">
                    <template v-if="message.role === 'agent' || message.role === 'assistant'">
                        <div class="agent-msg-header">
                            <span class="agent-logo-pill">⚡ Free API</span>
                            <time class="msg-clock">{{ formatConversationTime(message.created_at) }}</time>
                        </div>
                        <div class="agent-text-body">
                            <RichContentRenderer :content="message.content" :compact-html="true" @open-preview="$emit('open-preview', $event)" />
                        </div>
                    </template>
                    <template v-else>
                        <!-- 📎 附件缩略图与文件卡片预览托盘 (完全复用 AttachmentCard) -->
                        <div v-if="parseUserMessageAttachments(message.content).attachments.length > 0" class="stream-attachments-tray">
                            <AttachmentCard
                                v-for="(att, aIdx) in parseUserMessageAttachments(message.content).attachments"
                                :key="aIdx"
                                :attachment="att"
                                @preview-image="openImagePreview"
                                @preview-doc="openDocPreview"
                            />
                        </div>
                        <p v-if="parseUserMessageAttachments(message.content).cleanText" class="user-text-content">
                            {{ parseUserMessageAttachments(message.content).cleanText }}
                        </p>
                        <time class="msg-clock">{{ formatConversationTime(message.created_at) }}</time>
                    </template>
                </article>
            </section>

            <template v-for="entry in timelineEntries" :key="entry.id">
                <article v-if="entry.type === 'user'" class="user-message">
                    <!-- 📎 附件缩略图与文件卡片预览托盘 (完全复用 AttachmentCard) -->
                    <div v-if="parseUserMessageAttachments(entry.content).attachments.length > 0" class="stream-attachments-tray">
                        <AttachmentCard
                            v-for="(att, aIdx) in parseUserMessageAttachments(entry.content).attachments"
                            :key="aIdx"
                            :attachment="att"
                            @preview-image="openImagePreview"
                            @preview-doc="openDocPreview"
                        />
                    </div>
                    <!-- 用户指令文本 -->
                    <p v-if="parseUserMessageAttachments(entry.content).cleanText" class="user-text-content">
                        {{ parseUserMessageAttachments(entry.content).cleanText }}
                    </p>
                </article>
                <article v-else-if="entry.type === 'message'" class="saved-message" :class="entry.role === 'user' ? 'from-user' : 'from-agent'" @click="entry.isHuman && $emit('select-human', entry.id)">
                    <template v-if="entry.role === 'agent' || entry.role === 'assistant'">
                        <div class="agent-msg-header">
                            <span class="agent-logo-pill">⚡ Free API</span>
                            <time class="msg-clock">{{ formatClock(entry.timestamp) }}</time>
                        </div>
                        <div class="agent-text-body">
                            <RichContentRenderer :content="entry.content" :compact-html="true" @open-preview="$emit('open-preview', $event)" />
                        </div>
                    </template>
                    <template v-else>
                        <span v-if="entry.isHuman" class="intervention-indicator">人工干预输入</span>
                        <!-- 📎 附件缩略图与文件卡片预览托盘 (完全复用 AttachmentCard) -->
                        <div v-if="parseUserMessageAttachments(entry.content).attachments.length > 0" class="stream-attachments-tray">
                            <AttachmentCard
                                v-for="(att, aIdx) in parseUserMessageAttachments(entry.content).attachments"
                                :key="aIdx"
                                :attachment="att"
                                @preview-image="openImagePreview"
                                @preview-doc="openDocPreview"
                            />
                        </div>
                        <p v-if="parseUserMessageAttachments(entry.content).cleanText" class="user-text-content">
                            {{ parseUserMessageAttachments(entry.content).cleanText }}
                        </p>
                        <time class="msg-clock">{{ formatClock(entry.timestamp) }}</time>
                    </template>
                </article>
                <article
                    v-else-if="entry.type === 'plan'"
                    class="plan-node"
                    :class="[
                        `status-${entry.status}`,
                        {
                            'is-active-plan': isPlanRunning(entry),
                            'is-collapsed': !isPlanExpanded(entry),
                            'is-pure-read': getPlanDigest(entry.steps).capsules.length === 0
                        }
                    ]"
                >
                    <!-- 🌟 正在执行卡片顶栏 (Image 2: ○ 阶段 N · 标题  进行中 · 第 4/6 步) -->
                    <div
                        v-if="isPlanRunning(entry)"
                        class="plan-row is-running-row"
                        role="button"
                        tabindex="0"
                        title="点击展开/折叠步骤详情"
                        @click="togglePlan(entry)"
                        @keydown.enter="togglePlan(entry)"
                    >
                        <span class="stage-spinner-ring manus-spinner mini"></span>
                        <span class="plan-title-bold">{{ getStageDisplayTitle(entry) }}</span>
                        <span class="plan-running-badge">{{ getRunningProgressText(entry) }}</span>
                        <div class="plan-row-spacer"></div>
                        <time class="plan-running-time">{{ formatClock(entry.timestamp) }}</time>
                    </div>

                    <!-- 🌟 已结束阶段卡片顶栏 (Image 1: ▶ ✓ 阶段 N · 标题  1 失败已重试  4 步 · 读 3 · 728 tok · 14s) -->
                    <div
                        v-else
                        class="plan-row"
                        role="button"
                        tabindex="0"
                        title="点击展开/折叠执行过程"
                        @click="togglePlan(entry)"
                        @keydown.enter="togglePlan(entry)"
                    >
                        <span class="plan-expand-arrow" :class="{ open: isPlanExpanded(entry) }">
                            <svg width="9" height="9" viewBox="0 0 24 24" fill="currentColor">
                                <polygon points="6 4 20 12 6 20"></polygon>
                            </svg>
                        </span>
                        <span v-if="entry.status === 'succeeded'" class="stage-status-check">✓</span>
                        <span v-else-if="entry.status === 'failed'" class="stage-status-fail">✕</span>

                        <span class="plan-title-bold">{{ getStageDisplayTitle(entry) }}</span>

                        <span v-if="getPlanDigest(entry.steps).failureState === 'recovered'" class="plan-fail-tag tag-recovered" title="阶段内曾有步骤失败，已自动重试恢复成功">
                            {{ getPlanDigest(entry.steps).failedCount }} 失败已重试
                        </span>
                        <span v-else-if="getPlanDigest(entry.steps).failureState === 'retrying'" class="plan-fail-tag tag-retrying" title="此阶段有步骤失败，正在重试中">
                            失败重试中
                        </span>
                        <span v-else-if="getPlanDigest(entry.steps).failureState === 'unrecovered'" class="plan-fail-tag tag-unrecovered" title="此阶段包含未恢复的失败">
                            失败未恢复
                        </span>

                        <div class="plan-row-spacer"></div>

                        <span class="plan-meta-right">
                            {{ getPlanMetaSummary(entry, getPlanDigest(entry.steps)) }}
                        </span>
                    </div>

                    <!-- 🌟 正在执行卡片主体：收拢态直接呈现最后 3 条数据，当前动作带字体的流光动画 (Image 2) -->
                    <div
                        v-if="!isPlanExpanded(entry) && isPlanRunning(entry)"
                        class="plan-running-preview"
                        @click="togglePlan(entry)"
                    >
                        <!-- 第 1 条数据：最近读取/搜索操作与耗时 -->
                        <div v-if="getPlanRunningPreview(entry).line1" class="running-preview-line line-recent-action">
                            <span class="preview-icon">{{ getPlanRunningPreview(entry).line1?.icon }}</span>
                            <span class="preview-text">{{ getPlanRunningPreview(entry).line1?.text }}</span>
                            <span v-if="getPlanRunningPreview(entry).line1?.duration" class="preview-duration">
                                {{ getPlanRunningPreview(entry).line1?.duration }}
                            </span>
                        </div>

                        <!-- 第 2 条数据：规划或思考摘要 -->
                        <div v-if="getPlanRunningPreview(entry).line2" class="running-preview-line line-thought">
                            <span class="preview-icon">{{ getPlanRunningPreview(entry).line2?.icon }}</span>
                            <span class="preview-text">{{ getPlanRunningPreview(entry).line2?.text }}</span>
                        </div>

                        <!-- 第 3 条数据：当前正在执行的动作 (带字体的流光动画 text-shimmer-flow) -->
                        <div class="running-preview-line line-active-action">
                            <span class="preview-spinner manus-spinner mini"></span>
                            <span class="preview-tool-icon">{{ getPlanRunningPreview(entry).line3.icon }}</span>
                            <span class="preview-text text-shimmer-flow">{{ getPlanRunningPreview(entry).line3.text }}</span>
                        </div>
                    </div>

                    <!-- 🌟 已结束阶段卡片第二行：收拢态动作胶囊 (按时间顺序排列) -->
                    <div
                        v-else-if="!isPlanExpanded(entry)"
                        class="plan-chips-row"
                        @click="togglePlan(entry)"
                    >
                        <!-- 纯读取阶段胶囊 (没有任何产物、改动、审查等动作) -->
                        <span v-if="getPlanDigest(entry.steps).capsules.length === 0" class="pill-pure-read">
                            <span class="pill-icon"><FileIcon kind="read" :size="12" /></span> 本阶段无文件改动 (纯读取)
                        </span>

                        <!-- 🌟 按时间顺序排列的所有动作胶囊 -->
                        <template v-else>
                            <template v-for="cap in getPlanDigest(entry.steps).capsules.slice(0, 4)" :key="cap.id">
                                <!-- 1. 代码文件修改/创建 (可点击查看 Diff) -->
                                <span
                                    v-if="cap.kind === 'code'"
                                    class="plan-file-chip clickable"
                                    :title="cap.isIntercepted
                                        ? `${cap.label} · 写入已被门禁拦截（点击查看尝试内容）`
                                        : `${cap.label} · 点击查看代码改动`"
                                    @click.stop="cap.chip && handleChipClick(entry, cap.chip)"
                                >
                                    <span class="chip-icon"><FileIcon :name="cap.chip?.fileName || cap.label" kind="code" :size="13" /></span>
                                    <span class="chip-name">{{ cap.label }}</span>
                                    <span v-if="cap.additions && cap.additions > 0" class="chip-add">+{{ cap.additions }}</span>
                                    <span v-if="cap.deletions && cap.deletions > 0" class="chip-del">−{{ cap.deletions }}</span>
                                    <span v-if="cap.isIntercepted" class="chip-blocked">已拦截</span>
                                </span>

                                <!-- 2. Excel 数据表格产物胶囊 -->
                                <span
                                    v-else-if="cap.kind === 'excel'"
                                    class="plan-capsule-excel"
                                    :class="{ clickable: !!cap.chip }"
                                    :title="`Excel 数据表格: ${cap.label}`"
                                    @click.stop="cap.chip && handleChipClick(entry, cap.chip)"
                                >
                                    <span class="chip-icon"><FileIcon :name="cap.label" kind="excel" :size="13" /></span>
                                    <span class="chip-name">{{ cap.label }}</span>
                                    <span v-if="cap.additions && cap.additions > 0" class="chip-add">+{{ cap.additions }}</span>
                                </span>

                                <!-- 3. 文档 / 研报胶囊 -->
                                <span
                                    v-else-if="cap.kind === 'doc'"
                                    class="plan-capsule-doc"
                                    :class="{ clickable: !!cap.chip }"
                                    :title="`文档/研报: ${cap.label}`"
                                    @click.stop="cap.chip && handleChipClick(entry, cap.chip)"
                                >
                                    <span class="chip-icon"><FileIcon :name="cap.label" kind="doc" :size="13" /></span>
                                    <span class="chip-name">{{ cap.label }}</span>
                                    <span v-if="cap.additions && cap.additions > 0" class="chip-add">+{{ cap.additions }}</span>
                                </span>

                                <!-- 4. Reviewer 审查结论胶囊 -->
                                <span
                                    v-else-if="cap.kind === 'reviewer'"
                                    class="plan-chip-reviewer"
                                    :class="{ 'is-fail': cap.verdict === 'fail' }"
                                >
                                    <FileIcon kind="reviewer" :size="12" style="margin-right: 4px; vertical-align: -2px;" />
                                    {{ cap.label }}
                                </span>

                                <!-- 5. 方案审查挂起 / 异步等待胶囊 -->
                                <span
                                    v-else-if="cap.kind === 'wait'"
                                    class="plan-capsule-wait"
                                >
                                    <span class="chip-icon"><FileIcon kind="wait" :size="12" /></span>
                                    <span class="chip-name">{{ cap.label }}</span>
                                </span>

                                <!-- 6. 读取文件上下文胶囊 (带专属文件图标与微读标) -->
                                <span
                                    v-else-if="cap.kind === 'read'"
                                    class="plan-capsule-read"
                                    :title="cap.tooltip || `读取上下文: ${cap.label}`"
                                >
                                    <span class="chip-icon"><FileIcon :name="cap.label" :size="12" /></span>
                                    <span class="chip-read-tag">读</span>
                                    <span class="chip-name">{{ cap.label }}</span>
                                </span>

                                <!-- 7. 终端命令 / 其他工具动作胶囊 -->
                                <span
                                    v-else
                                    class="plan-capsule-general"
                                >
                                    <span class="chip-icon"><FileIcon :name="cap.label" :kind="cap.kind" :size="13" /></span>
                                    <span class="chip-name">{{ cap.label }}</span>
                                </span>
                            </template>

                            <!-- 超出 4 个动作的折叠计数徽标 -->
                            <span
                                v-if="getPlanDigest(entry.steps).capsules.length > 4"
                                class="plan-chip-overflow"
                                :title="`共 ${getPlanDigest(entry.steps).capsules.length} 个动作`"
                            >
                                +{{ getPlanDigest(entry.steps).capsules.length - 4 }} 个动作
                            </span>
                        </template>
                    </div>

                    <div class="plan-children-wrapper" :class="{ expanded: isPlanExpanded(entry) }">
                        <div v-if="isPlanExpanded(entry)" class="plan-children-inner">
                            <div class="plan-children">
                                <!-- 正在执行阶段折叠早期步骤，聚焦最新 2-3 步 -->
                                <div v-if="hasCollapsedEarlySteps(entry)" class="early-steps-fold-bar" @click.stop="showAllRunningSteps[entry.id] = true">
                                    <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><polyline points="6 9 12 15 18 9"></polyline></svg>
                                    <span>已完成前 {{ getEarlyStepsCount(entry) }} 步 (点击展开全部)</span>
                                </div>
                                <div v-else-if="isPlanRunning(entry) && entry.steps.length > 3 && showAllRunningSteps[entry.id]" class="early-steps-fold-bar is-expanded" @click.stop="showAllRunningSteps[entry.id] = false">
                                    <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><polyline points="18 15 12 9 6 15"></polyline></svg>
                                    <span>收起早期步骤 (仅保持最近 3 步)</span>
                                </div>

                                <article v-for="step in getVisiblePlanSteps(entry)" :key="step.id" class="goal-node" :class="[`status-${step.status}`, { 'is-subagent': isSubAgentTool(step.tool), 'is-waitjobs': isWaitJobsTool(step.tool) }]">
                                    <!-- 主步骤第1行：干脆利落，只展示步骤意图与执行耗时状态，点击查看整体过程 -->
                                    <button class="goal-row" :class="{ selected: step.id === selectedStepId, 'subagent-row': isSubAgentTool(step.tool), 'waitjobs-row': isWaitJobsTool(step.tool) }" type="button" @click="$emit('select-step', step.id)">
                                        <span class="tree-mark"><i></i></span>
                                        <span class="goal-label">
                                            <span v-if="isSubAgentTool(step.tool)" class="subagent-tag-badge">子智能体 · 审查</span>
                                            <span v-else-if="isWaitJobsTool(step.tool)" class="waitjobs-tag-badge">异步挂起</span>
                                            {{ step.description && step.description !== '执行操作' ? step.description : formatStepOperation(step).actionText }}
                                            <small v-if="step.modelTier" class="model-tier">{{ step.modelTier }}</small>
                                        </span>
                                        <span v-if="getStepMetricsPill(step)" class="step-metrics-badge" :title="getStepMetricsTooltip(step)">
                                            {{ getStepMetricsPill(step) }}
                                        </span>
                                        <span class="row-status" :class="`status-tag-${step.status}`">{{ isSubAgentTool(step.tool) && step.status === 'running' ? '后台审查中' : (isWaitJobsTool(step.tool) && step.status === 'running' ? '挂起等待' : statusLabel(step.status)) }}</span>
                                        <time>{{ formatClock(step.timestamp) }}</time>
                                    </button>
                                    <div class="node-children">
                                        <!-- 1. 思考行 (如果有思考过程) -->
                                        <button v-if="step.thought" class="child-row thought-child" :class="{ selected: step.id === selectedStepId && selectedSubpart === 'thought' }" type="button" @click="$emit('select-step-thought', step.id)">
                                            <span class="child-mark"></span>
                                            <span class="tool-tag thought-tag">🧠 思考</span>
                                            <span>{{ shortText(step.thought, 110) }}</span>
                                            <em v-if="step.metrics?.thinkingDurationMs">{{ (step.metrics.thinkingDurationMs / 1000).toFixed(1) }}s</em>
                                            <em v-else>已分析</em>
                                        </button>

                                        <!-- 2. 第2行：简约大气的文件操作卡片 (修改/读取 📄 文件名 #L1-80 +20 -1；有内容看 Diff，无内容看步骤详情) -->
                                        <div
                                            v-if="hasFileOperation(step)"
                                            class="child-action-card is-diff-clickable"
                                            :title="actionCardTitle(step)"
                                            @click.stop="handleActionCardClick(step)"
                                        >
                                            <span class="child-mark"></span>
                                            <span class="card-action-label">{{ formatStepOperation(step).badgeLabel }}</span>
                                            <span class="card-file-icon">
                                                <FileIcon :name="formatStepOperation(step).fileName || getStepMiniChip(step)?.fileName" :size="13" />
                                            </span>
                                            <span class="card-file-name">{{ formatStepOperation(step).fileName || getStepMiniChip(step)?.fileName }}</span>
                                            <span v-if="formatStepOperation(step).lineRange" class="card-line-range">{{ formatStepOperation(step).lineRange }}</span>
                                            <span v-if="(getStepMiniChip(step)?.additions || 0) > 0" class="card-diff-add">+{{ getStepMiniChip(step)?.additions }}</span>
                                            <span v-if="(getStepMiniChip(step)?.deletions || 0) > 0" class="card-diff-del">−{{ getStepMiniChip(step)?.deletions }}</span>
                                            <span v-if="getStepMiniChip(step)?.isIntercepted" class="card-blocked">已拦截</span>
                                            <span v-if="chipHasDiffContent(getStepMiniChip(step))" class="card-view-diff-hint">查看 Diff →</span>
                                            <em v-if="step.metrics?.toolDurationMs" class="card-duration">{{ (step.metrics.toolDurationMs / 1000).toFixed(1) }}s</em>
                                        </div>

                                        <!-- 3. 普通非文件工具调用 (如单纯的后台挂起、协同调度等) -->
                                        <button
                                            v-else
                                            class="child-row tool-child"
                                            :class="{ selected: step.id === selectedStepId && selectedSubpart === 'tool', 'tool-child-subagent': isSubAgentTool(step.tool), 'tool-child-wait': isWaitJobsTool(step.tool) }"
                                            type="button"
                                            @click="$emit('select-step', step.id)"
                                        >
                                            <span class="child-mark"></span>
                                            <span class="tool-tag" :style="{ color: getToolIcon(step.tool, step.action).color, background: `${getToolIcon(step.tool, step.action).color}18` }">
                                                {{ formatStepOperation(step).badgeLabel }}
                                            </span>
                                            <span :class="{ 'subagent-action-text': isSubAgentTool(step.tool), 'waitjobs-action-text': isWaitJobsTool(step.tool) }">
                                                {{ isSubAgentTool(step.tool) ? (step.status === 'running' ? '⚡ 正在后台独立执行调查审查，点击右侧抽屉可查看证据包与发现…' : (step.action || step.explanation || '已生成审查结论 · 点击查看详情')) : (isWaitJobsTool(step.tool) ? (step.status === 'running' ? '⏳ 当前协程已反应式挂起等待后台通知，0 Token 0 CPU 消耗…' : '✅ 后台任务已完成，已唤醒并回收全部结论') : shortText(formatStepOperation(step).actionText || step.action || step.explanation || '执行操作', 110)) }}
                                            </span>
                                            <em v-if="step.metrics?.toolDurationMs">{{ (step.metrics.toolDurationMs / 1000).toFixed(1) }}s · {{ step.source }}</em>
                                            <em v-else>{{ step.source }}</em>
                                        </button>
                                    </div>
                                    <div v-if="isSubAgentTool(step.tool) || isWaitJobsTool(step.tool)" class="subagent-inline-capsule" role="button" tabindex="0" :title="'点击查看当前步骤派发的子任务详情：' + extractSubagentTask(step)" @click="$emit('open-team-deck', step.id)" @keydown.enter="$emit('open-team-deck', step.id)">
                                        <div class="capsule-left">
                                            <span class="capsule-pulse-dot" :class="{ 'is-running': step.status === 'running' }"></span>
                                            <span class="capsule-text">
                                                {{ step.status === 'running' ? '⚡ 正在独立执行：' : '✅ 协同任务已完成：' }}
                                                <strong>{{ extractSubagentTask(step) }}</strong>
                                            </span>
                                        </div>
                                        <div class="capsule-action">
                                            <span>实时看板</span>
                                            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><polyline points="9 18 15 12 9 6"></polyline></svg>
                                        </div>
                                    </div>
                                </article>

                                <!-- 🌟 正在执行流光呼吸指示器 (Running Shimmer Streamer) -->
                                <div v-if="entry.status === 'running' || entry.isThinking" class="plan-running-shimmer">
                                    <span class="shimmer-icon">⚡</span>
                                    <span class="shimmer-text">{{ getRunningStateText(entry) }}</span>
                                    <div class="shimmer-beam"></div>
                                </div>

                                <article v-if="entry.isThinking && (thought || isRunning)" class="active-thought-stream" :class="{ selected: selectedNarrative === 'active-thought' }" role="button" tabindex="0" @click="$emit('select-active-thought')" @keydown.enter="$emit('select-active-thought')">
                                    <header><span class="thought-pulse-dot" :class="{ 'is-running': isRunning }"><i></i></span><b>思考</b><em>{{ isRunning ? '思考中…' : '思考过程' }}</em><time>{{ formatClock(entry.thinkingTimestamp || entry.timestamp) }}</time></header>
                                    <pre class="active-thought-content">{{ thought || '正在分析下一步操作…' }}</pre>
                                </article>
                            </div>
                        </div>
                    </div>
                </article>
                <article v-else-if="entry.type === 'active-thought'" class="active-thought-stream root-thought-stream" :class="{ selected: selectedNarrative === 'active-thought' }" role="button" tabindex="0" @click="$emit('select-active-thought')" @keydown.enter="$emit('select-active-thought')">
                    <header><span class="thought-pulse-dot" :class="{ 'is-running': isRunning }"><i></i></span><b>思考</b><em>{{ isRunning ? '思考中…' : '思考过程' }}</em><time>{{ formatClock(entry.timestamp) }}</time></header>
                    <div v-if="isRunning && !thought" class="active-thought-initial">
                        <span class="manus-spinner small"></span>
                        <span>正在连接模型并深度思考规划执行步骤…</span>
                    </div>
                    <pre v-else class="active-thought-content">{{ thought || '正在分析下一步操作…' }}</pre>
                </article>
                <article v-else-if="entry.type === 'completion'" class="saved-message from-agent final-response" :class="{ failed: entry.failed }">
                    <div class="agent-msg-header">
                        <span class="agent-logo-pill">⚡ Free API</span>
                        <time class="msg-clock">{{ formatClock(entry.timestamp) }}</time>
                    </div>
                    <div class="summary-rich">
                        <RichContentRenderer :content="(entry.summary || entry.title) + (entry.id === 'live-completion' && isStreamingSummary ? ' ▌' : '')" :compact-html="true" @open-preview="$emit('open-preview', $event)" />
                    </div>
                    <div v-if="entry.artifacts && entry.artifacts.length" class="final-artifacts">
                        <ArtifactList
                            :artifacts="entry.artifacts"
                            @preview="handlePreviewArtifact"
                            @download="handleDownloadArtifact"
                        />
                    </div>
                </article>
            </template>

            <!-- 人工干预/交互选择卡片 (单选/多选/确认/传文件/补充输入) -->
            <AgentHumanInterventionCard
                v-if="isWaitingForHuman"
                :human-form="humanForm"
                :pause-reason="pauseReason"
                :is-sending="isSendingHuman"
                @submit="$emit('submit-human', $event)"
            />

            <p v-if="isRunning && !timelineEntries.length && !legacyMessages.length" class="idle-live"><i></i>{{ stateDescription }}</p>
        </div>

        <!-- 📎 全局全屏附件预览弹窗 (Teleport 到 body) -->
        <AttachmentPreviewModal
            :image-modal="previewImageModal"
            :doc-modal="previewDocModal"
            @close-image="previewImageModal = null"
            @close-doc="previewDocModal = null"
        />
    </div>
</template>
<script setup lang="ts">
import { computed, reactive, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import AttachmentCard from "./AttachmentCard.vue";
import AttachmentPreviewModal from "./AttachmentPreviewModal.vue";
import type { AgentHumanForm, AgentStepItem, AgentToolStatus, AgentTeamState, ParsedAttachment, FileChangeChip, FileDiffPayload, StageFailureState, ReviewerVerdict, StageRunningPreview, StageActionCapsule } from "./types";
import { diffPayloadFromChip, formatStepOperation, parseStepFileChanges, aggregateFileChips, chipHasDiffContent, formatStageTitle, formatStageDuration, formatTokenCount, getRunningStagePreview, extractStageChronologicalCapsules } from "./diffUtils";
import type { AgentArtifactInput, ArtifactDescriptor } from "../../types/artifact";
import { formatClock, getToolIcon, shortText } from "./format";
import RichContentRenderer from "../charts/RichContentRenderer.vue";
import AgentHumanInterventionCard from "./AgentHumanInterventionCard.vue";
import ArtifactList from "../artifacts/ArtifactList.vue";
import FileIcon from "./FileIcon.vue";

function parseUserMessageAttachments(content: string): { cleanText: string; attachments: ParsedAttachment[] } {
    if (!content) return { cleanText: "", attachments: [] };
    const attRegex = /📎\s*\[附件:\s*([^\s()\]]+(?:\s+[^\s()\]]+)*?)(?:\s*\(([^)]+)\))?(?:\s*\|\s*路径:\s*([^\]]+))?\]/g;
    const attachments: ParsedAttachment[] = [];
    let match;
    while ((match = attRegex.exec(content)) !== null) {
        const fileName = match[1]?.trim() || "file";
        const fileSizeStr = match[2]?.trim() || "";
        const filePath = match[3]?.trim() || fileName;
        const isImage = /\.(png|jpg|jpeg|webp|gif|bmp|svg)$/i.test(fileName);

        let sizeBytes = 0;
        if (fileSizeStr.includes("MB")) {
            sizeBytes = Math.round(parseFloat(fileSizeStr) * 1024 * 1024);
        } else if (fileSizeStr.includes("KB")) {
            sizeBytes = Math.round(parseFloat(fileSizeStr) * 1024);
        } else if (fileSizeStr.includes("B")) {
            sizeBytes = Math.round(parseFloat(fileSizeStr));
        }

        attachments.push({
            file_name: fileName,
            file_path: filePath,
            size_bytes: sizeBytes,
            content_type: isImage ? "image/jpeg" : "file",
            prompt_text: "",
            image_data_url: null,
            raw_preview: null,
        });
    }
    let cleanText = content.replace(attRegex, "").trim();
    return { cleanText, attachments };
}

const previewImageModal = ref<ParsedAttachment | null>(null);
const previewDocModal = ref<ParsedAttachment | null>(null);

async function openImagePreview(att: ParsedAttachment) {
    if (att.image_data_url) {
        previewImageModal.value = att;
        return;
    }
    const path = att.file_path || att.file_name;
    if (path) {
        try {
            const parsed = await invoke<ParsedAttachment>("parse_attachment_file", { path });
            if (parsed && parsed.image_data_url) {
                previewImageModal.value = parsed;
                return;
            }
        } catch (e) {
            console.warn("读取本地图片失败:", e);
        }
    }
    previewImageModal.value = att;
}

async function openDocPreview(att: ParsedAttachment) {
    if (att.prompt_text || att.raw_preview) {
        previewDocModal.value = att;
        return;
    }
    const path = att.file_path || att.file_name;
    if (path) {
        try {
            const parsed = await invoke<ParsedAttachment>("parse_attachment_file", { path });
            if (parsed) {
                previewDocModal.value = parsed;
                return;
            }
        } catch (e) {
            console.warn("读取本地文档失败:", e);
        }
    }
    previewDocModal.value = att;
}

type HistoryMessage = { id: string; role: string; content: string; created_at: string; timestamp: number };
type HistoryTurn = { id: string; goal: string; status: string; steps: AgentStepItem[]; messages: HistoryMessage[]; summary: string; artifacts: AgentArtifactInput[]; startedAt: number; completedAt: number };
type PlanEntry = { id: string; type: "plan"; timestamp: number; title: string; status: AgentToolStatus; steps: AgentStepItem[]; isThinking: boolean; thinkingTimestamp?: number };
type TimelineEntry =
    | PlanEntry
    | { id: string; type: "user"; timestamp: number; content: string }
    | { id: string; type: "message"; timestamp: number; role: string; content: string; isHuman: boolean }
    | { id: string; type: "completion"; timestamp: number; title: string; summary: string; artifacts: AgentArtifactInput[]; failed: boolean }
    | { id: "active-thought"; type: "active-thought"; timestamp: number };

const props = withDefaults(
    defineProps<{
        currentGoal: string;
        legacyMessages: { id: string; role: string; content: string; created_at: string }[];
        historyTurns: HistoryTurn[];
        steps: AgentStepItem[];
        selectedStepId: string | null;
        selectedSubpart: "thought" | "tool" | null;
        selectedNarrative: string | null;
        thought: string;
        isThinking: boolean;
        humanInterventions: { message: string; timestamp: number }[];
        stateDescription: string;
        isRunning: boolean;
        completionMessage: string;
        finalSummary: string;
        finalArtifacts: AgentArtifactInput[];
        hasError: boolean;
        humanForm?: AgentHumanForm | null;
        pauseReason?: string;
        isWaitingForHuman?: boolean;
        isSendingHuman?: boolean;
        teamState?: AgentTeamState;
    }>(),
    {
        humanForm: null,
        pauseReason: "",
        isWaitingForHuman: false,
        isSendingHuman: false,
        finalArtifacts: () => [],
    }
);

const emit = defineEmits<{
    "select-step": [id: string];
    "select-step-thought": [id: string];
    "select-human": [id: string];
    "select-active-thought": [];
    "submit-human": [answer: string];
    "open-preview": [code: string];
    "open-team-deck": [stepId?: string];
    "open-diff": [payload: FileDiffPayload];
}>();

function extractSubagentTask(step: any): string {
    if (!step) return "后台子任务";
    const params = step.params || step.params_summary;
    if (params) {
        if (typeof params === "object") {
            const p = params as any;
            if (p.task && typeof p.task === "string") return p.task;
            if (p.goal && typeof p.goal === "string") return p.goal;
            if (p.command && typeof p.command === "string") return p.command;
            if (p.instruction && typeof p.instruction === "string") return p.instruction;
        } else if (typeof params === "string") {
            try {
                const parsed = JSON.parse(params);
                if (parsed.task) return parsed.task;
                if (parsed.goal) return parsed.goal;
            } catch {}
        }
    }
    if (step.description && step.description !== "执行操作") return step.description;
    if (step.action && step.action !== "执行操作") return step.action;
    return isSubAgentTool(step.tool) ? "独立子任务执行与审查" : "后台协同任务";
}

async function handlePreviewArtifact(artifact: ArtifactDescriptor) {
    if (!artifact.content) {
        if (artifact.artifactId) {
            const { readArtifactById } = await import("../../utils/artifact");
            const text = await readArtifactById(artifact.artifactId);
            if (text) artifact.content = text;
        } else if (artifact.localPath) {
            const { readArtifactFile } = await import("../../utils/artifact");
            const text = await readArtifactFile(artifact.localPath);
            if (text) artifact.content = text;
        }
    }
    const content = artifact.content || artifact.localPath || "";
    emit("open-preview", content);
}

function handleDownloadArtifact(_artifact: ArtifactDescriptor) {
    // 下载逻辑由 ArtifactCard 自带处理，此处保留钩子
}
const streamElement = ref<HTMLElement | null>(null);
const activeThoughtStartedAt = ref<number | null>(null);
const followLiveTail = ref(true);
const planExpandedState = reactive<Record<string, boolean>>({});

// ── 🛠️ 步骤文件变更与计划摘要解析 ──
/**
 * Chip 点击：有真实 Diff 内容 → 打开 Diff；否则回落到对应步骤详情
 * （如仅声明目标文件的写入派发步骤），避免打开空 Diff。
 */
function handleChipClick(entry: PlanEntry, chip: FileChangeChip | undefined) {
    if (!chip) return;
    if (chipHasDiffContent(chip)) {
        emit('open-diff', diffPayloadFromChip(chip));
        return;
    }
    const step = entry.steps.find(s => s.id === chip.stepId);
    if (step) emit('select-step', step.id);
}

/** 子步骤操作卡片点击：有内容看 Diff，无内容看步骤详情 */
function handleActionCardClick(step: AgentStepItem) {
    const chip = getStepMiniChip(step);
    if (chipHasDiffContent(chip)) {
        emit('open-diff', diffPayloadFromChip(chip!));
        return;
    }
    emit('select-step', step.id);
}

/** 操作卡片悬浮提示 */
function actionCardTitle(step: AgentStepItem): string {
    const chip = getStepMiniChip(step);
    if (chip?.isIntercepted) return `${chip.path} · 写入已被门禁拦截（点击查看尝试内容）`;
    if (chipHasDiffContent(chip)) return '点击在右侧查看代码改动 Diff';
    return formatStepOperation(step).filePath || '点击查看步骤详情';
}

function getStepMiniChip(step: AgentStepItem): FileChangeChip | undefined {
    const parsed = parseStepFileChanges(step);
    return parsed.chips?.[0] || parsed.chip;
}

function hasFileOperation(step: AgentStepItem): boolean {
    const parsed = parseStepFileChanges(step);
    const op = formatStepOperation(step);
    return !!(parsed.chips.length > 0 || parsed.chip || parsed.readFiles.length > 0 || op.fileName || (op.hasDiff && op.filePath));
}

const showAllRunningSteps = ref<Record<string, boolean>>({});

function getVisiblePlanSteps(entry: PlanEntry): AgentStepItem[] {
    if (entry.status !== 'running' && !entry.isThinking) {
        return entry.steps;
    }
    if (showAllRunningSteps.value[entry.id] || entry.steps.length <= 3) {
        return entry.steps;
    }
    return entry.steps.slice(-3);
}

function hasCollapsedEarlySteps(entry: PlanEntry): boolean {
    return (entry.status === 'running' || !!entry.isThinking) && entry.steps.length > 3 && !showAllRunningSteps.value[entry.id];
}

function getEarlyStepsCount(entry: PlanEntry): number {
    return Math.max(0, entry.steps.length - 3);
}

function getRunningStateText(entry: PlanEntry): string {
    if (entry.isThinking) {
        return props.thought ? `🧠 ${props.thought.slice(0, 80)}...` : '🧠 正在深入思考与规划下一步行动…';
    }
    const runningStep = entry.steps.find(s => s.status === 'running') || entry.steps[entry.steps.length - 1];
    if (runningStep) {
        const op = formatStepOperation(runningStep);
        const desc = (runningStep.description && runningStep.description !== '执行操作')
            ? runningStep.description
            : op.actionText;
        return `⚡ 正在执行: ${desc}`;
    }
    return '⚡ 正在协同执行任务中…';
}

interface PlanDigest {
    readCount: number;
    searchCount: number;
    editCount: number;
    failedCount: number;
    failureState: StageFailureState;
    fileChips: FileChangeChip[];
    capsules: StageActionCapsule[];
    readFiles: string[];
    hasMoreChipsCount: number;
    reviewerVerdict?: ReviewerVerdict;
    completionTokens: number;
    durationMs: number;
}

function getPlanDigest(steps: AgentStepItem[]): PlanDigest {
    let readCount = 0;
    let searchCount = 0;
    let failedCount = 0;
    let completionTokens = 0;
    let durationMs = 0;
    const rawChips: FileChangeChip[] = [];
    const readFiles: string[] = [];
    let reviewerVerdict: ReviewerVerdict | undefined;

    for (const s of steps) {
        if (s.status === 'failed') failedCount++;
        const parsed = parseStepFileChanges(s);
        readCount += parsed.readCount;
        searchCount += parsed.searchCount;
        if (parsed.chips && parsed.chips.length > 0) {
            rawChips.push(...parsed.chips);
        } else if (parsed.chip) {
            rawChips.push(parsed.chip);
        }
        if (parsed.readFiles && parsed.readFiles.length > 0) {
            for (const rf of parsed.readFiles) {
                if (!readFiles.includes(rf)) readFiles.push(rf);
            }
        }
        if (parsed.reviewerVerdict && !reviewerVerdict) {
            reviewerVerdict = parsed.reviewerVerdict;
        }
        if (s.metrics?.completionTokens) completionTokens += s.metrics.completionTokens;
        if (s.durationMs) durationMs += s.durationMs;
    }

    // 失败状态分级：失败后有成功步骤才算"已重试"；运行中未重试为 retrying；终态未恢复才告警
    let failureState: StageFailureState = 'none';
    if (failedCount > 0) {
        let lastFailedIdx = -1;
        let lastOkIdx = -1;
        steps.forEach((s, i) => {
            if (s.status === 'failed') lastFailedIdx = i;
            if (s.status === 'succeeded') lastOkIdx = i;
        });
        if (lastOkIdx > lastFailedIdx) {
            failureState = 'recovered';
        } else if (!props.isRunning) {
            failureState = 'unrecovered';
        } else {
            failureState = 'retrying';
        }
    }

    const aggregated = aggregateFileChips(rawChips);
    const hasMoreChipsCount = Math.max(0, aggregated.length - 4);
    const editCount = aggregated.length;
    const capsules = extractStageChronologicalCapsules(steps);

    if (durationMs === 0 && steps.length > 1) {
        const first = steps[0]?.timestamp;
        const last = steps[steps.length - 1]?.timestamp;
        if (first && last && last > first) {
            durationMs = last - first;
        }
    }

    return {
        readCount,
        searchCount,
        editCount,
        failedCount,
        failureState,
        fileChips: aggregated,
        capsules,
        readFiles,
        hasMoreChipsCount,
        reviewerVerdict,
        completionTokens,
        durationMs,
    };
}

const planOrderMap = computed(() => {
    const map = new Map<string, number>();
    let count = 1;
    for (const entry of timelineEntries.value) {
        if (entry.type === 'plan') {
            map.set(entry.id, count++);
        }
    }
    return map;
});

function getStageDisplayTitle(entry: PlanEntry): string {
    const idx = planOrderMap.value.get(entry.id) || 1;
    return formatStageTitle(entry.title, idx);
}

function getRunningProgressText(entry: PlanEntry): string {
    const total = entry.steps.length;
    const completed = entry.steps.filter((s) => s.status === 'succeeded').length;
    const current = Math.min(total, completed + 1);
    if (total > 0) {
        return `进行中 · 第 ${current}/${Math.max(total, current)} 步`;
    }
    return '进行中 · 准备中';
}

function getPlanRunningPreview(entry: PlanEntry): StageRunningPreview {
    return getRunningStagePreview(entry.steps, entry.isThinking || props.isThinking, props.thought);
}

function getPlanMetaSummary(entry: PlanEntry, digest: PlanDigest): string {
    const parts: string[] = [];
    parts.push(`${entry.steps.length} 步`);
    if (digest.searchCount > 0) parts.push(`搜 ${digest.searchCount}`);
    if (digest.readCount > 0) parts.push(`读 ${digest.readCount}`);
    if (digest.editCount > 0) parts.push(`${digest.editCount} 编辑`);
    if (digest.completionTokens > 0) parts.push(`${formatTokenCount(digest.completionTokens)} tok`);
    if (digest.durationMs > 0) parts.push(formatStageDuration(digest.durationMs));
    return parts.join(' · ');
}

function isPlanExpanded(plan: PlanEntry): boolean {
    if (planExpandedState[plan.id] !== undefined) {
        return planExpandedState[plan.id];
    }
    // 1. 如果包含未恢复的失败，保持展开以便排查
    const digest = getPlanDigest(plan.steps);
    if (digest.failureState === 'unrecovered') {
        return true;
    }
    // 2. 默认保持收拢态：
    //    - 运行中默认呈现 3 行流光紧凑摘要卡片 (图片 2)
    //    - 已完成默认呈现 2 行富摘要胶囊卡片 (图片 1)
    //    - 仅在用户手动点击后才展开完整步骤树
    return false;
}

function togglePlan(plan: PlanEntry) {
    const isCurrentlyExpanded = isPlanExpanded(plan);
    planExpandedState[plan.id] = !isCurrentlyExpanded;
}

const BOTTOM_THRESHOLD_PX = 56;
function isNearBottom(element: HTMLElement) { return element.scrollHeight - element.scrollTop - element.clientHeight <= BOTTOM_THRESHOLD_PX; }
let scrollRafId: number | null = null;
function followLatest() {
    if (followLiveTail.value && props.isRunning && streamElement.value) {
        if (scrollRafId !== null) cancelAnimationFrame(scrollRafId);
        scrollRafId = requestAnimationFrame(() => {
            if (streamElement.value) {
                streamElement.value.scrollTop = streamElement.value.scrollHeight;
            }
            scrollRafId = null;
        });
    }
}
function onStreamScroll(event: Event) { followLiveTail.value = isNearBottom(event.currentTarget as HTMLElement); }
function resumeLiveFollow() { followLiveTail.value = true; followLatest(); }
function followThoughtTail() {
    const el = streamElement.value?.querySelector<HTMLElement>(".active-thought-content");
    if (el) {
        el.scrollTop = el.scrollHeight;
    }
}
watch(() => props.isThinking, (running, wasRunning) => { if (running && !wasRunning) activeThoughtStartedAt.value = Date.now(); if (!running && wasRunning) activeThoughtStartedAt.value = null; }, { immediate: true });

function formatMetricTokenCount(n: number): string {
    if (n >= 1000000) return (n / 1000000).toFixed(1) + 'M';
    if (n >= 1000) return (n / 1000).toFixed(1) + 'k';
    return String(n);
}

function getStepMetricsPill(step: AgentStepItem): string {
    const m = step.metrics;
    if (!m && !step.durationMs) return '';
    const parts: string[] = [];
    if (m?.completionTokens) {
        parts.push(`${formatMetricTokenCount(m.completionTokens)} tok`);
    }
    if (m?.totalDurationMs) {
        parts.push(m.totalDurationMs < 1000 ? `${m.totalDurationMs}ms` : `${(m.totalDurationMs / 1000).toFixed(1)}s`);
    } else if (step.durationMs) {
        parts.push(step.durationMs < 1000 ? `${step.durationMs}ms` : `${(step.durationMs / 1000).toFixed(1)}s`);
    }
    return parts.join(' · ');
}

function getStepMetricsTooltip(step: AgentStepItem): string {
    const m = step.metrics;
    if (!m) return '';
    const lines: string[] = [];
    if (m.promptTokens) lines.push(`📥 输入上下文: ${m.promptTokens.toLocaleString()} tok (缓存命中: ${m.cacheHitRate || 0}%)`);
    if (m.completionTokens) lines.push(`📤 输出生成: ${m.completionTokens.toLocaleString()} tok`);
    if (m.thinkingDurationMs) lines.push(`🧠 思考耗时: ${(m.thinkingDurationMs / 1000).toFixed(1)}s`);
    if (m.toolDurationMs) lines.push(`⚡ 工具耗时: ${(m.toolDurationMs / 1000).toFixed(1)}s`);
    if (m.tokensPerSecond) lines.push(`💨 吐字速率: ${m.tokensPerSecond.toFixed(1)} tok/s`);
    return lines.join('\n');
}

// --- 🌟 真正的实时流式 Markdown 增量渲染与光标 ---
const isStreamingSummary = computed(() => props.isRunning && Boolean(props.finalSummary));

watch(
    () => [props.steps.length, props.isRunning, props.finalSummary, props.thought],
    () => {
        if (props.isRunning && followLiveTail.value) {
            followThoughtTail();
            followLatest();
        }
    }
);

function isPlanRunning(entry: PlanEntry): boolean {
    if (!props.isRunning) return false;
    // 如果已经开始输出最终总结报告，则所有阶段工具调用均已收敛完成，立即停止流光与 spinner
    if (props.finalSummary && props.finalSummary.trim().length > 0) return false;
    if (props.completionMessage && props.completionMessage.trim().length > 0) return false;
    if (!entry) return false;
    return entry.status === "running" || Boolean(entry.isThinking);
}

function planStatus(steps: AgentStepItem[]): AgentToolStatus {
    // 聚合每个逻辑 stepId 的最新尝试状态（若重试已成功，阶段整体应为成功）
    const latestStatusByStep = new Map<number, AgentToolStatus>();
    for (const step of steps) {
        latestStatusByStep.set(step.stepId, step.status);
    }
    const statuses = Array.from(latestStatusByStep.values());
    const isTaskActive = props.isRunning && !props.finalSummary?.trim() && !props.completionMessage?.trim();
    if (isTaskActive && statuses.some((s) => s === "running" || s === "pending" || s === "challenged")) return "running";
    if (statuses.some((s) => s === "failed")) return "failed";
    return "succeeded";
}
function planEntries(steps: AgentStepItem[], prefix: string, active = false): PlanEntry[] {
    const isTaskActive = props.isRunning && !props.finalSummary?.trim() && !props.completionMessage?.trim();
    const groups = new Map<string, PlanEntry>();
    for (const step of steps.filter((item) => Boolean(item.description || item.tool || item.action))) {
        const key = step.planId || "plan-default";
        const existing = groups.get(key) || { id: `${prefix}-plan-${key}`, type: "plan" as const, timestamp: step.timestamp, title: step.planTitle || "执行任务", status: step.status, steps: [], isThinking: false };
        existing.steps.push(step);
        existing.timestamp = Math.min(existing.timestamp, step.timestamp);
        if (step.planTitle) existing.title = step.planTitle;
        groups.set(key, existing);
    }
    const rawPlans = [...groups.values()];
    const plans = rawPlans.map((plan, idx) => {
        const isLatest = idx === rawPlans.length - 1;
        const calculatedStatus = planStatus(plan.steps);

        let status: AgentToolStatus;
        if (isTaskActive) {
            if (isLatest) {
                // 🌟 用户核心诉求：只要任务还在执行中，最新的大阶段就稳定保持为 running！
                // 绝不因单个微步骤完成与下一步调度之间的间隙而反复在"运行中"与"缩略合并"之间横跳！
                status = "running";
            } else {
                // 🌟 确定开启了下一大阶段（isLatest 为 false），前面的大阶段才确认完成并恢复缩略合并状态！
                status = calculatedStatus === "running" ? "succeeded" : calculatedStatus;
            }
        } else {
            // 任务整体已结束（产出最终回复或停止运行）：所有阶段稳定落定
            status = calculatedStatus === "running" ? "succeeded" : calculatedStatus;
        }

        return {
            ...plan,
            status,
        };
    });
    if (active && isTaskActive && (props.isThinking || Boolean(props.thought))) {
        const last = plans[plans.length - 1];
        if (last && (props.isThinking || !last.steps.some(s => s.thought))) {
            last.isThinking = true;
            last.thinkingTimestamp = activeThoughtStartedAt.value || Date.now();
        }
    }
    return plans;
}

function completionArtifacts(value: unknown): AgentArtifactInput[] {
    if (!Array.isArray(value)) return [];
    return value.filter((item) => {
        if (!item) return false;
        if (typeof item === "string" && (item.trim() === "[object Object]" || !item.trim())) return false;
        return true;
    });
}
const historyTimelineEntries = computed<TimelineEntry[]>(() => {
    const timeline: TimelineEntry[] = [];
    // 过滤掉当前正在实时运行中的回合，由 live 视图独立渲染，防止双份重复
    const turnsToRender = props.historyTurns.filter((turn) => {
        if (props.isRunning && turn.status === "running") return false;
        return true;
    });

    for (const turn of [...turnsToRender].sort((a, b) => a.startedAt - b.startedAt)) {
        timeline.push({ id: `history-user-${turn.id}`, type: "user", timestamp: turn.startedAt, content: turn.goal });
        // 过滤掉最终已由 completion 展示的助手消息，只保留人工干预记录
        const humanMessages = turn.messages.filter((m) => m.role === "user");
        const middle: TimelineEntry[] = [
            ...planEntries(turn.steps, `history-${turn.id}`),
            ...humanMessages.map((message) => ({
                id: `history-message-${turn.id}-${message.id}`,
                type: "message" as const,
                timestamp: message.timestamp,
                role: message.role,
                content: message.content,
                isHuman: true,
            })),
        ].sort((a, b) => a.timestamp - b.timestamp || a.id.localeCompare(b.id));
        timeline.push(...middle);
        if (turn.summary || turn.artifacts.length || ["succeeded", "failed", "cancelled"].includes(turn.status)) {
            const isCancelled = turn.status === "cancelled";
            const failed = turn.status === "failed" || isCancelled;
            const title = isCancelled
                ? "任务已手动停止"
                : (turn.status === "failed" ? (turn.summary || "任务未能顺利完成") : "任务已由 AI 标记完成");
            timeline.push({
                id: `history-completion-${turn.id}`,
                type: "completion",
                timestamp: turn.completedAt || turn.startedAt,
                title,
                summary: turn.summary,
                artifacts: completionArtifacts(turn.artifacts),
                failed,
            });
        }
    }
    return timeline;
});

function currentTimeline(): TimelineEntry[] {
    const timeline: TimelineEntry[] = [];
    const latestTurn = props.historyTurns[props.historyTurns.length - 1];
    // 仅在任务未运行、且最新一轮已完成的 turn 与当前目标完全一致时才隐藏 live，防止重复渲染双份 completion
    // 严禁使用 cleanGoal.includes(turnGoal) 遍历所有 80 轮历史，避免短文本匹配误判导致新发送的消息不显示
    const alreadyPersisted = !props.isRunning && Boolean(
        latestTurn &&
        props.currentGoal &&
        latestTurn.goal.trim() === props.currentGoal.trim() &&
        ["succeeded", "failed", "cancelled"].includes(latestTurn.status)
    );
    if (alreadyPersisted) return timeline;

    const plans = planEntries(props.steps, "live", props.isThinking);
    const firstTimestamp = plans[0]?.timestamp || Date.now();
    if (props.currentGoal) timeline.push({ id: "live-user-goal", type: "user", timestamp: firstTimestamp - 1, content: props.currentGoal });
    const middle: TimelineEntry[] = [
        ...plans,
        ...props.humanInterventions.map((item, index) => ({ id: `live-human-${item.timestamp}-${index}`, type: "message" as const, timestamp: item.timestamp, role: "user", content: item.message, isHuman: true })),
    ].sort((a, b) => a.timestamp - b.timestamp || a.id.localeCompare(b.id));
    timeline.push(...middle);
    if ((props.isThinking || Boolean(props.thought) || props.isRunning) && !plans.length) {
        timeline.push({ id: "active-thought", type: "active-thought", timestamp: activeThoughtStartedAt.value || Date.now() });
    }
    if (props.completionMessage || props.finalSummary || props.finalArtifacts.length) {
        timeline.push({ id: "live-completion", type: "completion", timestamp: Date.now(), title: props.completionMessage || (props.hasError ? "任务未能完成" : "任务已完成"), summary: props.finalSummary || props.completionMessage, artifacts: props.finalArtifacts, failed: props.hasError });
    }
    return timeline;
}

const timelineEntries = computed<TimelineEntry[]>(() => [...historyTimelineEntries.value, ...currentTimeline()]);
function statusLabel(status: AgentToolStatus) { return { pending: "等待", running: "执行中", succeeded: "完成", failed: "失败", challenged: "验证未闭合" }[status] || "未知"; }
function isSubAgentTool(tool?: string): boolean {
    const t = (tool || "").toLowerCase();
    return t.includes("child_reviewer") || t.includes("sub_agent");
}
function isWaitJobsTool(tool?: string): boolean {
    const t = (tool || "").toLowerCase();
    return t.includes("wait_jobs") || t.includes("wait_job");
}
function formatConversationTime(value: string) { const date = new Date(value); return Number.isNaN(date.getTime()) ? "" : date.toLocaleString("zh-CN", { hour: "2-digit", minute: "2-digit", month: "numeric", day: "numeric" }); }
watch(() => [timelineEntries.value.length, props.completionMessage, props.historyTurns.length], () => { followLatest(); }, { flush: "post" });
watch(() => props.isRunning, (running, wasRunning) => { if (running && !wasRunning) { followLiveTail.value = true; followLatest(); } if (!running && wasRunning) followLatest(); });
</script>
<style scoped>
.process-tree {
    position: relative;
    display: flex;
    min-height: 0;
    flex: 1;
    flex-direction: column;
    padding: 18px clamp(16px, 4vw, 48px) 24px;
    overflow-y: auto;
    overscroll-behavior: contain;
    background: #141416;
    color: #e4e4e7;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
}

/* 居中阅读流容器 (彻底解决左侧靠边偏离问题) */
.stream-centered-content {
    width: 100%;
    max-width: 820px;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: 8px;
}

/* 浮动跟随最新 */
.follow-latest {
    position: sticky;
    z-index: 4;
    top: 8px;
    align-self: flex-end;
    border: 1px solid rgba(255, 255, 255, 0.15);
    border-radius: 999px;
    padding: 4px 10px;
    color: #d9e7ff;
    cursor: pointer;
    background: rgba(19, 31, 49, 0.94);
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.3);
    font-size: 11px;
    font-weight: 600;
}
.follow-latest:hover {
    border-color: #7aa8e8;
    background: #203552;
}

/* 空状态 */
.empty-conversation {
    display: grid;
    min-height: 60vh;
    align-content: center;
    justify-items: center;
    text-align: center;
    padding: 40px 0;
}
.empty-logo {
    display: grid;
    width: 44px;
    height: 44px;
    place-items: center;
    border-radius: 12px;
    color: #fbbf24;
    background: linear-gradient(135deg, rgba(251, 191, 36, 0.16), rgba(124, 108, 246, 0.16));
    border: 1px solid rgba(251, 191, 36, 0.3);
    box-shadow: 0 4px 20px rgba(251, 191, 36, 0.15);
}
.empty-conversation h2 {
    margin: 12px 0 4px;
    color: #f1f1f1;
    font-size: 20px;
    letter-spacing: -0.03em;
}
.empty-conversation p {
    max-width: 440px;
    margin: 0;
    color: #8b8b8f;
    font-size: 12px;
    line-height: 1.6;
}

/* 对话历史 */
.conversation-history {
    display: flex;
    width: 100%;
    flex-direction: column;
    gap: 10px;
    margin-bottom: 4px;
}

.saved-message {
    position: relative;
    display: flex;
    flex-direction: column;
    box-sizing: border-box;
    width: 100%;
}

/* 👤 用户消息气泡 (深色精致圆角气泡) */
.saved-message.from-user, .user-message {
    align-self: flex-end;
    max-width: min(680px, 88%);
    padding: 9px 15px;
    border-radius: 16px 16px 4px 16px;
    background: #27272a;
    border: 1px solid rgba(255, 255, 255, 0.08);
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.2);
    color: #f4f4f5;
    margin: 2px 0;
    word-break: break-word;
    overflow-wrap: anywhere;
    box-sizing: border-box;
}

.user-text-content {
    margin: 0;
    color: #f4f4f5;
    font-size: 13px;
    line-height: 1.6;
    white-space: pre-wrap;
    word-break: break-word;
    overflow-wrap: anywhere;
}

/* 📎 附件缩略图与文件卡片托盘 */
.stream-attachments-tray {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-bottom: 8px;
    width: 100%;
}

/* 图片全屏预览弹窗 */
.image-preview-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.8);
    backdrop-filter: blur(8px);
    z-index: 99999;
    display: grid;
    place-items: center;
    padding: 30px;
}

.image-preview-dialog {
    max-width: 90vw;
    max-height: 90vh;
    background: #18181c;
    border: 1px solid rgba(255, 255, 255, 0.14);
    border-radius: 12px;
    box-shadow: 0 20px 50px rgba(0, 0, 0, 0.8);
    display: flex;
    flex-direction: column;
    overflow: hidden;
}

.dialog-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 16px;
    background: #1f1f24;
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
}

.dialog-title {
    font-size: 13px;
    font-weight: 600;
    color: #f4f4f5;
}

.btn-close-dialog {
    background: transparent;
    border: 0;
    color: #a1a1aa;
    font-size: 18px;
    cursor: pointer;
    padding: 0 4px;
    line-height: 1;
}

.btn-close-dialog:hover { color: #ffffff; }

.dialog-body {
    padding: 16px;
    display: grid;
    place-items: center;
    overflow: auto;
}

.full-preview-img {
    max-width: 100%;
    max-height: calc(85vh - 70px);
    object-fit: contain;
    border-radius: 6px;
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.5);
}

/* 📄 文档/PDF 在线富文本预览弹窗 */
.doc-preview-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.82);
    backdrop-filter: blur(10px);
    z-index: 99999;
    display: grid;
    place-items: center;
    padding: 24px;
}

.doc-preview-dialog {
    width: min(880px, 94vw);
    height: min(780px, 88vh);
    background: #18181c;
    border: 1px solid rgba(255, 255, 255, 0.14);
    border-radius: 12px;
    box-shadow: 0 20px 50px rgba(0, 0, 0, 0.8);
    display: flex;
    flex-direction: column;
    overflow: hidden;
}

.dialog-title-row {
    display: flex;
    align-items: center;
    gap: 10px;
}

.dialog-file-icon {
    font-size: 24px;
}

.dialog-file-meta h3 {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
    color: #f4f4f5;
}

.dialog-file-meta small {
    color: #71717a;
    font-size: 11px;
}

.dialog-actions {
    display: flex;
    align-items: center;
    gap: 8px;
}

.btn-copy-doc {
    background: rgba(255, 255, 255, 0.08);
    border: 1px solid rgba(255, 255, 255, 0.14);
    color: #e4e4e7;
    font-size: 12px;
    padding: 4px 10px;
    border-radius: 6px;
    cursor: pointer;
    transition: all 0.15s ease;
}

.btn-copy-doc:hover {
    background: rgba(255, 255, 255, 0.15);
    color: #fff;
}

.doc-dialog-body {
    flex: 1;
    overflow-y: auto;
    background: #121215;
    padding: 20px;
    display: block !important;
}

.doc-markdown-content {
    color: #e4e4e7;
    font-size: 13px;
    line-height: 1.6;
}

.doc-raw-preview {
    margin: 0;
    white-space: pre-wrap;
    word-break: break-word;
    font-family: monospace;
    font-size: 12px;
    color: #d4d4d8;
}

.doc-empty-hint {
    color: #71717a;
    font-size: 13px;
    text-align: center;
    padding: 40px 0;
}

/* 🤖 Agent 消息：彻底透明无框排版 */
.saved-message.from-agent {
    align-self: flex-start;
    width: 100%;
    max-width: 100%;
    padding: 4px 0 12px;
    border: none !important;
    border-radius: 0 !important;
    background: transparent !important;
    box-shadow: none !important;
}

.agent-msg-header {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 6px;
}

.agent-logo-pill {
    font-size: 12.5px;
    font-weight: 700;
    color: #f4f4f5;
    letter-spacing: -0.01em;
}

.msg-clock {
    font-size: 10px;
    color: #71717a;
}

.from-user .msg-clock {
    text-align: right;
    margin-top: 3px;
}

.intervention-indicator {
    display: block;
    font-size: 10px;
    font-weight: 700;
    color: var(--orange);
    margin-bottom: 3px;
}

/* ── 📝 Markdown 优雅排版 (标题/段落/列表/表格/代码) ── */
.markdown-content,
.summary-rich,
.agent-text-body {
    width: 100%;
    color: #e4e4e7;
    font-size: 13.5px;
    line-height: 1.78;
    letter-spacing: 0.003em;
    overflow-wrap: anywhere;
}

.markdown-content :deep(h1),
.markdown-content :deep(h2),
.markdown-content :deep(h3),
.markdown-content :deep(h4),
.markdown-content :deep(h5),
.markdown-content :deep(h6) {
    color: #f4f4f5;
    font-weight: 700;
    line-height: 1.35;
    letter-spacing: -0.015em;
    margin: 16px 0 8px;
}
.markdown-content :deep(h1:first-child),
.markdown-content :deep(h2:first-child),
.markdown-content :deep(h3:first-child) {
    margin-top: 2px;
}
.markdown-content :deep(h1) { font-size: 19px; }
.markdown-content :deep(h2) { font-size: 16.5px; }
.markdown-content :deep(h3) { font-size: 14.5px; }
.markdown-content :deep(h4) { font-size: 13.5px; }

.markdown-content :deep(p) {
    margin: 0 0 10px;
    color: inherit;
    font-size: inherit;
    line-height: inherit;
    white-space: normal;
}
.markdown-content :deep(p:last-child) {
    margin-bottom: 0;
}

.markdown-content :deep(ul),
.markdown-content :deep(ol) {
    margin: 6px 0 12px;
    padding-left: 20px;
}
.markdown-content :deep(li) {
    margin: 3px 0;
    line-height: 1.68;
}
.markdown-content :deep(li > ul),
.markdown-content :deep(li > ol) {
    margin: 2px 0 4px;
}

/* 引用块 */
.markdown-content :deep(blockquote) {
    margin: 10px 0;
    padding: 6px 14px;
    border-left: 3px solid var(--accent);
    background: rgba(255, 255, 255, 0.025);
    border-radius: 0 6px 6px 0;
    color: #a1a1aa;
}

/* 表格 (精细边框与悬停) */
.markdown-content :deep(table) {
    width: 100%;
    margin: 12px 0 16px;
    border-collapse: separate;
    border-spacing: 0;
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 8px;
    overflow: hidden;
    font-size: 12.5px;
}
.markdown-content :deep(th) {
    padding: 7px 12px;
    background: #1f2025;
    color: #f4f4f5;
    font-weight: 600;
    text-align: left;
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
    border-right: 1px solid rgba(255, 255, 255, 0.06);
}
.markdown-content :deep(th:last-child) {
    border-right: none;
}
.markdown-content :deep(td) {
    padding: 6px 12px;
    background: #161619;
    color: #d4d4d8;
    border-bottom: 1px solid rgba(255, 255, 255, 0.05);
    border-right: 1px solid rgba(255, 255, 255, 0.05);
}
.markdown-content :deep(td:last-child) {
    border-right: none;
}
.markdown-content :deep(tr:last-child td) {
    border-bottom: none;
}
.markdown-content :deep(tr:hover td) {
    background: #1e1e24;
}

/* 行内代码与代码块 */
.markdown-content :deep(code) {
    padding: 2px 5px;
    border-radius: 4px;
    color: #e5c07b;
    background: #1e1e23;
    font: 12px ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}
.markdown-content :deep(.code-block-wrapper) {
    margin: 12px 0 16px;
    border-radius: 8px;
    overflow: hidden;
    border: 1px solid rgba(255, 255, 255, 0.08);
    background: #18181c;
}
.markdown-content :deep(.code-block-header) {
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: 30px;
    padding: 0 12px;
    background: #202026;
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
    color: #8e8e93;
    font-size: 11px;
}
.markdown-content :deep(pre) {
    margin: 0;
    padding: 12px 14px;
    overflow: auto;
    background: transparent;
}
.markdown-content :deep(pre code) {
    padding: 0;
    color: #d9dde5;
    background: transparent;
    font-size: 12.5px;
    line-height: 1.6;
}

/* 链接 */
.markdown-content :deep(a) {
    color: #60a5fa;
    text-decoration: none;
}
.markdown-content :deep(a:hover) {
    text-decoration: underline;
}

/* ── 🌟 阶段卡片容器 (对齐设计图：独立圆角卡片，精致紧凑间距) ── */
.plan-node {
    width: 100%;
    margin: 2px 0;
    padding: 7px 11px;
    border-radius: 8px;
    border: 1px solid rgba(255, 255, 255, 0.08);
    background: rgba(255, 255, 255, 0.02);
    box-sizing: border-box;
    transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1);
}

.plan-node + .plan-node {
    margin-top: -4px; /* 紧贴相邻阶段，实际纵向间隙紧凑精致 (约 5~6px) */
}

.plan-node:hover {
    border-color: rgba(255, 255, 255, 0.12);
    background: rgba(255, 255, 255, 0.03);
}

.plan-node.is-active-plan {
    border-color: rgba(59, 130, 246, 0.28);
    background: rgba(59, 130, 246, 0.035);
    box-shadow: 0 2px 12px rgba(59, 130, 246, 0.06);
}

/* 计划阶段标题行 */
.plan-row {
    display: flex;
    align-items: center;
    gap: 7px;
    min-height: 24px;
    color: #f1f5f9;
    font-size: 12.5px;
    cursor: pointer;
    user-select: none;
    transition: background 0.15s ease;
}

.plan-row.is-running-row {
    gap: 8px;
}

.plan-row-spacer {
    flex: 1;
    min-width: 8px;
}

.plan-title-bold {
    font-weight: 600;
    font-size: 13px;
    color: #f1f5f9;
    letter-spacing: -0.01em;
}

.stage-status-check {
    color: #10b981;
    font-weight: 700;
    font-size: 13px;
    line-height: 1;
}

.stage-status-fail {
    color: #ef4444;
    font-weight: 700;
    font-size: 13px;
    line-height: 1;
}

.stage-spinner-ring {
    width: 13px;
    height: 13px;
    border: 2px solid rgba(59, 130, 246, 0.3);
    border-top-color: #3b82f6;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
    flex-shrink: 0;
}

.plan-running-badge {
    display: inline-flex;
    align-items: center;
    padding: 1px 7px;
    border-radius: 10px;
    font-size: 11px;
    font-weight: 500;
    background: rgba(59, 130, 246, 0.12);
    color: #38bdf8;
    border: 1px solid rgba(59, 130, 246, 0.25);
}

.plan-running-time {
    font-size: 11.5px;
    color: #64748b;
}

.plan-meta-right {
    font-size: 12px;
    color: #94a3b8;
    font-variant-numeric: tabular-nums;
    font-weight: 400;
}

.plan-expand-arrow {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    color: #71717a;
    transition: transform 0.2s cubic-bezier(0.16, 1, 0.3, 1), color 0.15s ease;
    flex-shrink: 0;
}

.plan-expand-arrow.open {
    transform: rotate(90deg);
    color: #38bdf8;
}

.plan-collapsed-hint {
    margin-left: 6px;
    font-size: 10.5px;
    color: #71717a;
    font-weight: normal;
}

.plan-mark {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: #52525b;
    flex: 0 0 auto;
    margin: 0 2px;
    transition: all 0.2s ease;
}
.plan-mark i {
    display: none;
}

.plan-node.status-running > .plan-row .plan-mark,
.plan-node.is-active-plan > .plan-row .plan-mark {
    background: #38bdf8;
    box-shadow: 0 0 8px rgba(56, 189, 248, 0.85);
    animation: task-pulse 1.2s ease-in-out infinite;
}
.plan-node.status-running > .plan-row .row-status,
.plan-node.is-active-plan > .plan-row .row-status {
    color: #38bdf8;
    font-weight: 600;
}

.plan-node.status-succeeded:not(.is-active-plan) > .plan-row .plan-mark {
    background: #55d6a2;
    box-shadow: 0 0 4px rgba(85, 214, 162, 0.4);
    animation: none;
}
.plan-node.status-succeeded:not(.is-active-plan) > .plan-row .row-status {
    color: #55d6a2;
}

.plan-node.status-failed > .plan-row .plan-mark {
    background: #f87171;
    box-shadow: 0 0 6px rgba(248, 113, 113, 0.6);
    animation: none !important;
}
.plan-node.status-failed > .plan-row .row-status {
    color: #f87171;
    font-weight: 600;
}

.plan-title {
    display: flex;
    align-items: center;
    gap: 6px;
    font-weight: 550;
    color: #e4e4e7;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
}

.plan-title-text {
    font-size: 12px;
    font-weight: 550;
    color: #f4f4f5;
    letter-spacing: -0.01em;
}

.plan-meta-inline {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 11px;
    color: #71717a;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-weight: 450;
}

.plan-meta-inline .meta-sep {
    color: #52525b;
    font-size: 10px;
}

.plan-fail-tag {
    display: inline-flex;
    align-items: center;
    padding: 0 5px;
    border-radius: 4px;
    font-size: 10px;
    margin-left: 2px;
    line-height: 1.5;
    white-space: nowrap;
}

.plan-fail-tag.tag-recovered {
    background: rgba(245, 158, 11, 0.15);
    color: #fbbf24;
    border: 1px solid rgba(245, 158, 11, 0.28);
    font-weight: 550;
}

.plan-fail-tag.tag-retrying {
    background: rgba(245, 158, 11, 0.12);
    color: #fcd34d;
    border: 1px solid rgba(245, 158, 11, 0.22);
    font-weight: 550;
}

.plan-fail-tag.tag-unrecovered {
    background: rgba(239, 68, 68, 0.18);
    color: #f87171;
    border: 1px solid rgba(239, 68, 68, 0.35);
    font-weight: 600;
}

.model-tier {
    margin-left: 6px;
    color: #71717a;
    font-size: 9px;
    font-weight: 600;
}

.plan-children-wrapper {
    display: grid;
    grid-template-rows: 0fr;
    opacity: 0;
    transform: translateY(-4px);
    transition: grid-template-rows 0.35s cubic-bezier(0.16, 1, 0.3, 1), 
                opacity 0.28s cubic-bezier(0.16, 1, 0.3, 1), 
                transform 0.35s cubic-bezier(0.16, 1, 0.3, 1);
    will-change: grid-template-rows, opacity, transform;
}

.plan-children-wrapper.expanded {
    grid-template-rows: 1fr;
    opacity: 1;
    transform: translateY(0);
}

.plan-children-inner {
    overflow: hidden;
    min-height: 0;
}

.plan-children {
    margin-left: 7px;
    padding-left: 12px;
    border-left: 1px solid rgba(255, 255, 255, 0.08);
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-top: 2px;
}

.goal-node {
    margin: 0;
    display: flex;
    flex-direction: column;
    animation: step-appear 0.25s cubic-bezier(0.16, 1, 0.3, 1);
}

@keyframes step-appear {
    from { opacity: 0; transform: translateY(3px); }
    to { opacity: 1; transform: translateY(0); }
}

/* 所有交互按钮重置 */
button.goal-row,
button.child-row,
button.root-row {
    -webkit-appearance: none;
    appearance: none;
    background: transparent !important;
    border: none !important;
    outline: none !important;
    box-shadow: none !important;
    cursor: pointer;
    font: inherit;
    text-align: left;
}

/* 具体执行步骤行 (极紧凑，高 22px) */
.goal-row {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 22px;
    padding: 2px 6px;
    border-radius: 4px;
    color: #d4d4d8;
    font-size: 12px;
    width: 100%;
    transition: background 0.12s;
}

.goal-row:hover {
    background: rgba(255, 255, 255, 0.04) !important;
}

.goal-row.selected {
    background: rgba(108, 99, 255, 0.12) !important;
    color: #ffffff !important;
}

.tree-mark {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: #52525b;
    flex: 0 0 auto;
    margin: 0 2px;
    transition: all 0.2s ease;
}
.tree-mark i {
    display: none;
}

/* 🌟 执行完成：稳稳常亮绿色，静止不闪 */
.goal-node.status-succeeded > .goal-row .tree-mark {
    background: #55d6a2;
    box-shadow: 0 0 4px rgba(85, 214, 162, 0.5);
    animation: none;
}
.goal-node.status-succeeded > .goal-row .row-status {
    color: #55d6a2;
}

/* 🌟 仅当前正在运行的具体步骤：呼吸闪烁脉冲 */
.goal-node.status-running > .goal-row .tree-mark {
    background: #38bdf8;
    box-shadow: 0 0 8px rgba(56, 189, 248, 0.9);
    animation: task-pulse 1.2s ease-in-out infinite;
}
.goal-node.status-running > .goal-row .row-status {
    color: #38bdf8;
    font-weight: 600;
}
.goal-node.status-running > .goal-row .goal-label {
    color: #f4f4f5;
}

/* 🌟 失败/重试状态：若在运行中闪烁警示，若已停止则静态常亮 */
.goal-node.status-failed > .goal-row .tree-mark,
.goal-node.status-error > .goal-row .tree-mark {
    background: #f87171;
    box-shadow: 0 0 6px rgba(248, 113, 113, 0.6);
    animation: none !important;
}
.goal-node.status-failed > .goal-row .row-status,
.goal-node.status-error > .goal-row .row-status {
    color: #f87171;
    font-weight: 600;
}

/* 🌟 终态自检拦截（验证未闭合）：醒目警示橙黄 */
.goal-node.status-challenged > .goal-row .tree-mark {
    background: #e6a23c;
    box-shadow: 0 0 6px rgba(230, 162, 60, 0.6);
    animation: none !important;
}
.goal-node.status-challenged > .goal-row .row-status {
    color: #e6a23c;
    font-weight: 600;
}
.goal-node.status-challenged > .goal-row .goal-label {
    color: #fcd34d;
}

/* 🌟 等待中 */
.goal-node.status-pending > .goal-row .tree-mark {
    background: #52525b;
    animation: none;
}
.goal-node.status-pending > .goal-row .row-status {
    color: #71717a;
}

.goal-label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 500;
}

.step-metrics-badge {
    font-size: 10px;
    padding: 0 5px;
    border-radius: 3px;
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid rgba(255, 255, 255, 0.06);
    color: #a1a1aa;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    margin-left: auto;
    margin-right: 6px;
    flex-shrink: 0;
}

.row-status {
    margin-left: 0;
    font-size: 10px;
    color: #71717a;
    font-style: normal;
    flex-shrink: 0;
}

.goal-row time,
.plan-row time,
.child-row time {
    font-size: 9.5px;
    color: #52525b;
    font-family: ui-monospace, monospace;
    flex-shrink: 0;
    margin-left: 6px;
}

/* 子步骤与思考 (极紧凑，高 20px) */
.node-children {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 1px;
    margin-left: 6px;
    padding-left: 10px;
    border-left: 1px solid rgba(255, 255, 255, 0.06);
    margin-bottom: 2px;
}

.child-row {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 20px;
    padding: 1px 6px;
    border-radius: 3px;
    font-size: 11px;
    color: #8e8e93;
    width: 100%;
    transition: background 0.12s;
}

.child-row:hover {
    background: rgba(255, 255, 255, 0.03) !important;
    color: #d4d4d8 !important;
}

.child-row.selected {
    background: rgba(108, 99, 255, 0.1) !important;
    color: #ffffff !important;
}

.step-tool-emoji {
    font-size: 11.5px;
    line-height: 1;
    flex-shrink: 0;
    margin-right: 1px;
}

.child-mark {
    width: 3px;
    height: 3px;
    border-radius: 50%;
    background: #71717a;
    flex-shrink: 0;
}

.tool-tag {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding: 1px 6px;
    border-radius: 4px;
    font-size: 10px;
    font-weight: 600;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    flex-shrink: 0;
    line-height: 1.35;
    border: 1px solid rgba(255, 255, 255, 0.06);
}

.thought-tag {
    color: #e879f9;
    background: rgba(232, 121, 249, 0.12);
    border-color: rgba(232, 121, 249, 0.2);
}

.child-row span:not(.child-mark):not(.tool-tag) {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
}

.child-row em {
    color: #636368;
    font-size: 9.5px;
    font-style: normal;
    margin-left: auto;
    flex-shrink: 0;
}

/* 实时思考流 */
.active-thought-stream {
    margin: 2px 0 2px 6px;
    border-radius: 4px;
    cursor: pointer;
    background: rgba(56, 189, 248, 0.03);
    border: 1px dashed rgba(56, 189, 248, 0.35);
    animation: thought-border-glow 2s ease-in-out infinite;
}

@keyframes thought-border-glow {
    0%, 100% { border-color: rgba(56, 189, 248, 0.2); background: rgba(56, 189, 248, 0.02); }
    50% { border-color: rgba(56, 189, 248, 0.55); background: rgba(56, 189, 248, 0.05); }
}

.thought-pulse-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: #38bdf8;
    box-shadow: 0 0 8px rgba(56, 189, 248, 0.9);
    animation: task-pulse 1.2s ease-in-out infinite;
    flex: 0 0 auto;
    margin: 0 2px;
}
.thought-pulse-dot i {
    display: none;
}

.active-thought-stream header {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 2px 8px;
    font-size: 11px;
    color: #cfcfd4;
}

.active-thought-stream header b { color: var(--accent-light); }
.active-thought-stream header em { color: #38bdf8; font-size: 10px; font-style: normal; margin-left: auto; font-weight: 600; }
.active-thought-stream header time { color: #73737a; font-size: 10px; }

.active-thought-content {
    max-height: 90px;
    margin: 0;
    padding: 4px 8px 6px;
    overflow: auto;
    color: #a9a9af;
    font: 11px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace;
    white-space: pre-wrap;
}

.active-thought-initial {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px 8px;
    font-size: 11.5px;
    color: #93c5fd;
}

/* 交付物与总结 */
.final-response {
    width: 100%;
    max-width: 100%;
    padding: 4px 0 16px;
    border: 0;
    background: transparent;
}

.final-artifacts {
    display: flex;
    margin-top: 8px;
    gap: 4px;
    flex-direction: column;
}

.final-artifacts b { color: #bdbdc4; font-size: 10px; }
.final-artifacts code {
    color: #88888f;
    font: 10px ui-monospace, monospace;
    background: #1c1c20;
    padding: 2px 6px;
    border-radius: 4px;
}

.idle-live {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0;
    padding: 2px 4px;
    color: #85858b;
    font-size: 11px;
}

.idle-live i {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: #eee;
    animation: work-pulse 1s ease-in-out infinite;
}

@keyframes work-pulse {
    0%, 100% { opacity: 0.35; transform: scale(0.8); }
    50% { opacity: 1; transform: scale(1); }
}

@keyframes task-pulse {
    0%, 100% {
        transform: scale(0.8);
        opacity: 0.5;
        box-shadow: 0 0 3px rgba(56, 189, 248, 0.4);
    }
    50% {
        transform: scale(1.35);
        opacity: 1;
        box-shadow: 0 0 12px rgba(56, 189, 248, 1);
    }
}

@keyframes error-pulse {
    0%, 100% {
        transform: scale(0.8);
        opacity: 0.5;
        box-shadow: 0 0 3px rgba(248, 113, 113, 0.4);
    }
    50% {
        transform: scale(1.35);
        opacity: 1;
        box-shadow: 0 0 12px rgba(248, 113, 113, 1);
    }
}

.typing-cursor {
    display: inline-block;
    color: #fbbf24;
    font-size: 14px;
    font-weight: 700;
    margin-left: 2px;
    vertical-align: baseline;
    animation: typing-blink 0.7s infinite;
}

@keyframes typing-blink {
    0%, 100% { opacity: 1; }
    50% { opacity: 0; }
}

/* =========================================================================
   子智能体 (Sub-Agent) 与异步挂起 (Wait-Jobs) 专属胶囊与流光样式
   ========================================================================= */
.subagent-tag-badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 1px 7px;
    border-radius: 6px;
    font-size: 11px;
    font-weight: 600;
    color: #c084fc;
    background: rgba(168, 85, 247, 0.14);
    border: 1px solid rgba(168, 85, 247, 0.32);
    margin-right: 6px;
    vertical-align: middle;
}

.waitjobs-tag-badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 1px 7px;
    border-radius: 6px;
    font-size: 11px;
    font-weight: 600;
    color: #fbbf24;
    background: rgba(245, 158, 11, 0.14);
    border: 1px solid rgba(245, 158, 11, 0.32);
    margin-right: 6px;
    vertical-align: middle;
}

/* 子智能体节点微光卡片 */
.goal-node.is-subagent {
    border-left: 2px solid #a855f7;
    background: rgba(168, 85, 247, 0.03);
    border-radius: 6px;
}

.goal-node.is-subagent.status-running {
    animation: subagent-pulse-glow 2s infinite ease-in-out;
}

@keyframes subagent-pulse-glow {
    0%, 100% {
        background: rgba(168, 85, 247, 0.03);
        box-shadow: 0 0 0 1px rgba(168, 85, 247, 0.15);
    }
    50% {
        background: rgba(168, 85, 247, 0.08);
        box-shadow: 0 0 14px rgba(168, 85, 247, 0.3);
    }
}

/* 挂起节点微光卡片 */
.goal-node.is-waitjobs {
    border-left: 2px solid #f59e0b;
    background: rgba(245, 158, 11, 0.03);
    border-radius: 6px;
}

.goal-node.is-waitjobs.status-running {
    animation: waitjobs-pulse-glow 2.5s infinite ease-in-out;
}

@keyframes waitjobs-pulse-glow {
    0%, 100% {
        background: rgba(245, 158, 11, 0.03);
        box-shadow: 0 0 0 1px rgba(245, 158, 11, 0.15);
    }
    50% {
        background: rgba(245, 158, 11, 0.07);
        box-shadow: 0 0 12px rgba(245, 158, 11, 0.25);
    }
}

.subagent-action-text {
    color: #e9d5ff;
    font-weight: 500;
}

.waitjobs-action-text {
    color: #fef3c7;
    font-weight: 500;
}

.tool-child-subagent {
    border-radius: 6px;
    background: rgba(168, 85, 247, 0.06);
    border: 1px dashed rgba(168, 85, 247, 0.25);
}

.tool-child-wait {
    border-radius: 6px;
    background: rgba(245, 158, 11, 0.06);
    border: 1px dashed rgba(245, 158, 11, 0.25);
}

.subagent-inline-capsule {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin: 4px 0 6px 16px;
    padding: 6px 12px;
    border-radius: 8px;
    background: rgba(56, 189, 248, 0.06);
    border: 1px solid rgba(56, 189, 248, 0.25);
    cursor: pointer;
    transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1);
}

.subagent-inline-capsule:hover {
    background: rgba(56, 189, 248, 0.12);
    border-color: rgba(56, 189, 248, 0.45);
    transform: translateY(-1px);
    box-shadow: 0 4px 12px rgba(56, 189, 248, 0.15);
}

.capsule-left {
    display: flex;
    align-items: center;
    gap: 8px;
}

.capsule-pulse-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: #38bdf8;
    box-shadow: 0 0 8px #38bdf8;
}

.capsule-pulse-dot.is-running {
    animation: capsule-dot-pulse 1.2s infinite ease-in-out;
}

@keyframes capsule-dot-pulse {
    0%, 100% { transform: scale(0.9); opacity: 0.6; }
    50% { transform: scale(1.4); opacity: 1; box-shadow: 0 0 12px #38bdf8; }
}

.capsule-text {
    font-size: 11.5px;
    font-weight: 600;
    color: #e0f2fe;
}

.capsule-action {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: 11px;
    color: #38bdf8;
    font-weight: 600;
}

/* ==========================================================================
   📱 移动端专属微调 (Chat Stream & Step Cards <= 768px)
   ========================================================================== */
@media (max-width: 768px) {
    .chat-stream-scroll {
        padding: 8px 10px 16px !important;
    }
    .stream-centered-content {
        max-width: 100% !important;
        gap: 8px !important;
    }
    .user-bubble {
        max-width: 92% !important;
        padding: 10px 14px !important;
        font-size: 13.5px !important;
        border-radius: 14px !important;
    }
    .assistant-bubble-wrap {
        padding: 6px 4px !important;
    }
    .step-card, .step-item {
        padding: 8px 10px !important;
        border-radius: 10px !important;
    }
    .thought-card {
        padding: 8px 10px !important;
        font-size: 12px !important;
    }
    .artifact-card-wrap {
        width: 100% !important;
        margin: 6px 0 !important;
    }
    .follow-latest {
        top: 4px !important;
        font-size: 10px !important;
        padding: 3px 8px !important;
    }
}

/* ── 📚 计划节点收拢态：正在执行三行流光预览与文件变更胶囊 (Image 1 & 2) ── */
.plan-running-preview {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 8px;
    padding: 8px 4px 4px 18px;
    cursor: pointer;
    border-top: 1px solid rgba(59, 130, 246, 0.12);
}

.running-preview-line {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    line-height: 1.4;
    min-height: 20px;
}

.running-preview-line.line-recent-action {
    color: #94a3b8;
}

.running-preview-line.line-recent-action .preview-duration {
    margin-left: auto;
    color: #64748b;
    font-size: 11.5px;
    font-variant-numeric: tabular-nums;
}

.running-preview-line.line-thought {
    color: #cbd5e1;
    font-size: 12px;
}

.running-preview-line.line-active-action {
    color: #38bdf8;
    font-size: 12.5px;
}

.running-preview-line .preview-icon,
.running-preview-line .preview-tool-icon {
    font-size: 13px;
    flex-shrink: 0;
    line-height: 1;
}

.running-preview-line .preview-spinner {
    width: 11px;
    height: 11px;
    border: 1.5px solid rgba(59, 130, 246, 0.3);
    border-top-color: #38bdf8;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
    flex-shrink: 0;
}

/* 🌟 字体的流光动画 (Text Shimmer Flow) */
@keyframes textShimmerFlow {
    0% {
        background-position: -200% 0;
    }
    100% {
        background-position: 200% 0;
    }
}

.text-shimmer-flow {
    background: linear-gradient(
        90deg,
        #2563eb 0%,
        #38bdf8 20%,
        #ffffff 45%,
        #818cf8 70%,
        #2563eb 100%
    );
    background-size: 200% auto;
    -webkit-background-clip: text;
    -webkit-text-fill-color: transparent;
    animation: textShimmerFlow 2.2s linear infinite;
    font-weight: 550;
    display: inline-block;
}

:root[data-theme='dark'] .text-shimmer-flow,
.dark .text-shimmer-flow {
    background: linear-gradient(
        90deg,
        #60a5fa 0%,
        #38bdf8 20%,
        #ffffff 50%,
        #a78bfa 75%,
        #60a5fa 100%
    );
    background-size: 200% auto;
    -webkit-background-clip: text;
    -webkit-text-fill-color: transparent;
}

/* 纯读取阶段胶囊 (Image 1 Stage 1) */
.pill-pure-read {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 3px 10px;
    border-radius: 12px;
    font-size: 11.5px;
    background: rgba(148, 163, 184, 0.08);
    border: 1px solid rgba(148, 163, 184, 0.15);
    color: #94a3b8;
    user-select: none;
    line-height: 1.35;
}

.pill-pure-read .pill-icon {
    font-size: 12px;
}

/* 一级阶段第二行文件 Chips 容器 */
.plan-chips-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    padding: 6px 4px 2px 18px;
    cursor: pointer;
}

.plan-file-chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 2px 9px;
    border-radius: 12px;
    font-size: 11.5px;
    background: rgba(255, 255, 255, 0.035);
    border: 1px solid rgba(255, 255, 255, 0.08);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    line-height: 1.35;
    transition: all 0.15s ease;
    user-select: none;
}

.plan-file-chip.clickable {
    cursor: pointer;
}

.plan-file-chip.clickable:hover {
    background: rgba(56, 189, 248, 0.09);
    border-color: rgba(56, 189, 248, 0.32);
    box-shadow: 0 2px 8px rgba(0, 0, 0, 0.2);
}

.plan-file-chip .chip-icon {
    font-size: 11px;
    opacity: 0.85;
}

.plan-file-chip .chip-name {
    color: #e2e8f0;
    font-weight: 500;
}

.plan-file-chip .chip-add {
    color: #4ade80;
    font-weight: 600;
}

.plan-file-chip .chip-del {
    color: #f87171;
    font-weight: 600;
}

.plan-chip-overflow {
    display: inline-flex;
    align-items: center;
    padding: 2px 8px;
    border-radius: 12px;
    font-size: 11px;
    background: rgba(255, 255, 255, 0.04);
    color: #a1a1aa;
    border: 1px solid rgba(255, 255, 255, 0.08);
}

.plan-chip-reviewer {
    display: inline-flex;
    align-items: center;
    padding: 2px 9px;
    border-radius: 12px;
    font-size: 11px;
    background: rgba(99, 102, 241, 0.12);
    border: 1px solid rgba(99, 102, 241, 0.25);
    color: #c7d2fe;
    font-weight: 500;
}

.plan-chip-reviewer.is-fail {
    background: rgba(239, 68, 68, 0.14);
    border-color: rgba(239, 68, 68, 0.32);
    color: #fca5a5;
}

/* 🌟 Excel 表格产物胶囊 */
.plan-capsule-excel {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 2px 9px;
    border-radius: 12px;
    font-size: 11.5px;
    background: rgba(16, 185, 129, 0.1);
    border: 1px solid rgba(16, 185, 129, 0.28);
    color: #6ee7b7;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    line-height: 1.35;
    transition: all 0.15s ease;
    user-select: none;
}
.plan-capsule-excel.clickable {
    cursor: pointer;
}
.plan-capsule-excel.clickable:hover {
    background: rgba(16, 185, 129, 0.18);
    border-color: rgba(16, 185, 129, 0.45);
}

/* 🌟 文档 / 研报胶囊 */
.plan-capsule-doc {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 2px 9px;
    border-radius: 12px;
    font-size: 11.5px;
    background: rgba(168, 85, 247, 0.1);
    border: 1px solid rgba(168, 85, 247, 0.28);
    color: #d8b4fe;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    line-height: 1.35;
    transition: all 0.15s ease;
    user-select: none;
}
.plan-capsule-doc.clickable {
    cursor: pointer;
}
.plan-capsule-doc.clickable:hover {
    background: rgba(168, 85, 247, 0.18);
    border-color: rgba(168, 85, 247, 0.45);
}

/* 🌟 方案审查挂起 / 异步等待胶囊 */
.plan-capsule-wait {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 2px 9px;
    border-radius: 12px;
    font-size: 11px;
    background: rgba(245, 158, 11, 0.1);
    border: 1px solid rgba(245, 158, 11, 0.28);
    color: #fde68a;
    font-weight: 500;
}

/* 🌟 终端命令 / 其他通用工具动作胶囊 */
.plan-capsule-general {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 2px 9px;
    border-radius: 12px;
    font-size: 11px;
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid rgba(255, 255, 255, 0.1);
    color: #cbd5e1;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

/* 🌟 读取文件上下文胶囊 */
.plan-capsule-read {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 8px;
    border-radius: 12px;
    font-size: 11px;
    background: rgba(99, 102, 241, 0.08);
    border: 1px solid rgba(99, 102, 241, 0.22);
    color: #c7d2fe;
}

.chip-read-tag {
    font-size: 9.5px;
    line-height: 1.1;
    padding: 1px 3px;
    border-radius: 3px;
    background: rgba(99, 102, 241, 0.2);
    color: #a5b4fc;
    font-weight: 550;
}

/* 被门禁拦截的写入标记 */
.chip-blocked {
    font-size: 9px;
    padding: 0 4px;
    border-radius: 3px;
    background: rgba(239, 68, 68, 0.18);
    color: #fca5a5;
    border: 1px solid rgba(239, 68, 68, 0.35);
    font-weight: 600;
    margin-left: 2px;
}

.card-blocked {
    font-size: 9.5px;
    padding: 0 5px;
    border-radius: 3px;
    background: rgba(239, 68, 68, 0.16);
    color: #f87171;
    border: 1px solid rgba(239, 68, 68, 0.3);
    font-weight: 600;
    margin-left: 4px;
}

/* 正在执行阶段早期步骤折叠栏 */
.early-steps-fold-bar {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 3px 8px;
    margin: 2px 0 4px 6px;
    border-radius: 4px;
    background: rgba(255, 255, 255, 0.025);
    border: 1px dashed rgba(255, 255, 255, 0.08);
    color: #94a3b8;
    font-size: 11px;
    font-weight: 500;
    cursor: pointer;
    transition: all 0.15s ease;
    width: fit-content;
    user-select: none;
}

.early-steps-fold-bar:hover {
    background: rgba(255, 255, 255, 0.05);
    border-color: rgba(255, 255, 255, 0.16);
    color: #f1f5f9;
}

.early-steps-fold-bar svg {
    color: #64748b;
    transition: transform 0.15s ease;
}

.early-steps-fold-bar:hover svg {
    color: #38bdf8;
}

/* 🌟 正在执行流光呼吸指示器 (Running Shimmer Streamer) */
.plan-running-shimmer {
    position: relative;
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 4px 0 2px 6px;
    padding: 5px 12px;
    border-radius: 5px;
    background: linear-gradient(90deg, rgba(99, 102, 241, 0.08) 0%, rgba(56, 189, 248, 0.12) 50%, rgba(99, 102, 241, 0.08) 100%);
    border: 1px solid rgba(56, 189, 248, 0.28);
    overflow: hidden;
    color: #e0f2fe;
    font-size: 11px;
    font-weight: 500;
    user-select: none;
    box-shadow: 0 2px 8px rgba(56, 189, 248, 0.08);
}

.plan-running-shimmer .shimmer-icon {
    font-size: 11px;
    line-height: 1;
    filter: drop-shadow(0 0 4px rgba(56, 189, 248, 0.6));
    animation: shimmer-pulse-icon 1.2s ease-in-out infinite alternate;
}

.plan-running-shimmer .shimmer-text {
    color: #e0f2fe;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 11px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
    z-index: 1;
}

.plan-running-shimmer .shimmer-beam {
    position: absolute;
    top: 0;
    left: -150%;
    width: 150%;
    height: 100%;
    background: linear-gradient(90deg, transparent 0%, rgba(255, 255, 255, 0.04) 30%, rgba(56, 189, 248, 0.3) 50%, rgba(255, 255, 255, 0.12) 65%, transparent 100%);
    animation: shimmer-beam-sweep 2.4s cubic-bezier(0.4, 0, 0.2, 1) infinite;
    pointer-events: none;
}

@keyframes shimmer-beam-sweep {
    0% { transform: translateX(0); }
    100% { transform: translateX(200%); }
}

@keyframes shimmer-pulse-icon {
    0% { transform: scale(0.92); opacity: 0.8; }
    100% { transform: scale(1.08); opacity: 1; }
}

.plan-file-chip.chip-readonly {
    background: rgba(255, 255, 255, 0.03);
    border-color: rgba(255, 255, 255, 0.06);
    cursor: default;
}

.chip-read-tag {
    font-size: 9px;
    padding: 0 3px;
    border-radius: 3px;
    background: rgba(148, 163, 184, 0.12);
    color: #94a3b8;
    margin-left: 2px;
}

.step-file-pill {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding: 1px 5px;
    border-radius: 4px;
    font-size: 10px;
    background: rgba(255, 255, 255, 0.05);
    border: 1px solid rgba(255, 255, 255, 0.08);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    margin-left: 6px;
    white-space: nowrap;
    transition: all 0.15s ease;
}

.step-file-pill.clickable {
    cursor: pointer;
}
.step-file-pill.clickable:hover {
    background: rgba(56, 189, 248, 0.12);
    border-color: rgba(56, 189, 248, 0.35);
}

.step-file-pill .pill-name {
    color: #d4d4d8;
}

.step-file-pill .pill-add {
    color: #4ade80;
    font-weight: 600;
}

.step-file-pill .pill-del {
    color: #f87171;
    font-weight: 600;
}

.step-file-pill .pill-linerange,
.step-op-linerange {
    color: #38bdf8;
    font-size: 10px;
    font-weight: 500;
    margin-left: 3px;
    opacity: 0.9;
}

/* ==========================================================================
   ✨ 现代 IDE 极简第 2 行文件操作卡片 (Action / Diff Card)
   ========================================================================== */
.child-action-card {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 5px 12px;
    margin: 3px 0 3px 18px;
    border-radius: 6px;
    background: rgba(255, 255, 255, 0.025);
    border: 1px solid rgba(255, 255, 255, 0.06);
    font-size: 11.5px;
    color: #cbd5e1;
    position: relative;
    transition: all 0.15s ease;
    user-select: none;
}

.child-action-card.is-diff-clickable {
    cursor: pointer;
}

.child-action-card.is-diff-clickable:hover {
    background: rgba(56, 189, 248, 0.07);
    border-color: rgba(56, 189, 248, 0.32);
    box-shadow: 0 2px 8px rgba(0, 0, 0, 0.15);
}

.child-action-card .card-action-label {
    color: #94a3b8;
    font-weight: 500;
    font-size: 11px;
    letter-spacing: 0.2px;
}

.child-action-card .card-file-icon {
    font-size: 12px;
    line-height: 1;
    opacity: 0.85;
}

.child-action-card .card-file-name {
    color: #e2e8f0;
    font-weight: 500;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 11.5px;
}

.child-action-card .card-line-range {
    color: #38bdf8;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 11px;
    font-weight: 500;
}

.child-action-card .card-diff-add {
    color: #4ade80;
    font-weight: 600;
    font-size: 11px;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

.child-action-card .card-diff-del {
    color: #f87171;
    font-weight: 600;
    font-size: 11px;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

.child-action-card .card-view-diff-hint {
    margin-left: auto;
    font-size: 11px;
    color: #38bdf8;
    opacity: 0.75;
    font-weight: 500;
    transition: all 0.15s ease;
}

.child-action-card:hover .card-view-diff-hint {
    opacity: 1;
    transform: translateX(2px);
    text-decoration: underline;
}

.child-action-card .card-duration {
    color: #64748b;
    font-size: 9.5px;
    font-style: normal;
    margin-left: 6px;
}
</style>

