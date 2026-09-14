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
                        <span :key="getRunningProgressText(entry)" class="plan-running-badge badge-step-switch">{{ getRunningProgressText(entry) }}</span>
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

                    <!-- 🌟 正在执行卡片主体：收拢态直接呈现前两步记录与当前极速流光动作 -->
                    <div
                        v-if="!isPlanExpanded(entry) && isPlanRunning(entry)"
                        class="plan-running-preview"
                        @click="togglePlan(entry)"
                    >
                        <!-- 第 1 条数据：前第 2 步操作记录与耗时 -->
                        <div
                            v-if="getPlanRunningPreview(entry).line1"
                            :key="getPlanRunningPreview(entry).line1?.text"
                            class="running-preview-line line-recent-action line-step-switch"
                        >
                            <span class="preview-icon">{{ getPlanRunningPreview(entry).line1?.icon }}</span>
                            <span class="preview-text">{{ getPlanRunningPreview(entry).line1?.text }}</span>
                            <span v-if="getPlanRunningPreview(entry).line1?.duration" class="preview-duration">
                                {{ getPlanRunningPreview(entry).line1?.duration }}
                            </span>
                        </div>

                        <!-- 第 2 条数据：前第 1 步（最近完成）操作记录与耗时 -->
                        <div
                            v-if="getPlanRunningPreview(entry).line2"
                            :key="getPlanRunningPreview(entry).line2?.text"
                            class="running-preview-line line-thought line-step-switch"
                            :title="getPlanRunningPreview(entry).line2?.fullThought || getPlanRunningPreview(entry).line2?.text"
                        >
                            <span class="preview-icon">{{ getPlanRunningPreview(entry).line2?.icon }}</span>
                            <span class="preview-text">{{ getPlanRunningPreview(entry).line2?.text }}</span>
                            <span v-if="getPlanRunningPreview(entry).line2?.duration" class="preview-duration">
                                {{ getPlanRunningPreview(entry).line2?.duration }}
                            </span>
                        </div>

                        <!-- 第 3 条数据：当前正在执行的动作 / 飞速流动的动态思考流 (带字体的流光动画与换行垂直翻牌) -->
                        <div class="running-preview-line line-active-action">
                            <span class="preview-spinner manus-spinner mini"></span>
                            <span :key="getPlanRunningPreview(entry).line3.actionKind" class="preview-tool-icon icon-anim-switch">{{ getPlanRunningPreview(entry).line3.icon }}</span>
                            <span v-if="getPlanRunningPreview(entry).line3.actionKind === 'think'" class="preview-think-tag">思考:</span>
                            <!-- 思考态换行垂直翻牌 / 操作态切换：新行从下方翻入且从头渲染，旧行被顶出 -->
                            <div class="preview-flip-ticker-viewport">
                                <Transition name="ticker-flip">
                                    <span
                                        :key="getLine3Key(entry)"
                                        v-scroll-tail
                                        class="preview-text text-shimmer-flow"
                                        :title="getLine3FullText(entry)"
                                    >
                                        {{ getLine3DisplayText(entry) }}
                                    </span>
                                </Transition>
                            </div>
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
                        <div
                            v-if="isPlanExpanded(entry)"
                            class="plan-children-inner"
                            :ref="(el) => setPlanInnerRef(entry.id, el)"
                            @scroll.passive="onPlanScroll($event, entry)"
                        >
                            <div class="plan-children">
                                <article v-for="step in getVisiblePlanSteps(entry)" :key="step.id" class="goal-node" :class="[`status-${step.status}`, { 'is-subagent': isSubAgentTool(step.tool), 'is-waitjobs': isWaitJobsTool(step.tool) }]">
                                    <!-- 主步骤第1行：干脆利落，只展示步骤意图与执行耗时状态，点击查看整体过程 -->
                                    <button class="goal-row" :class="{ selected: step.id === selectedStepId, 'subagent-row': isSubAgentTool(step.tool), 'waitjobs-row': isWaitJobsTool(step.tool) }" type="button" @click="$emit('select-step', step.id)">
                                        <span class="tree-mark"><i></i></span>
                                        <span class="goal-label">
                                            <span v-if="isSubAgentTool(step.tool)" class="subagent-tag-badge">{{ getSubAgentBadgeText(step) }}</span>
                                            <span v-else-if="isWaitJobsTool(step.tool)" class="waitjobs-tag-badge">异步挂起</span>
                                            {{ step.description && step.description !== '执行操作' ? step.description : formatStepOperation(step).actionText }}
                                            <small v-if="step.modelTier" class="model-tier">{{ step.modelTier }}</small>
                                        </span>
                                        <span v-if="getStepMetricsPill(step)" class="step-metrics-badge" :title="getStepMetricsTooltip(step)">
                                            {{ getStepMetricsPill(step) }}
                                        </span>
                                        <span class="row-status" :class="`status-tag-${step.status}`">{{ isSubAgentTool(step.tool) && step.status === 'running' ? '后台协同中' : (isWaitJobsTool(step.tool) && step.status === 'running' ? '挂起等待' : statusLabel(step.status)) }}</span>
                                        <time>{{ formatClock(step.timestamp) }}</time>
                                    </button>
                                    <div class="node-children">
                                        <!-- 1. 思考行 (如果有思考过程) -->
                                        <button v-if="step.thought" class="child-row thought-child" :class="{ selected: step.id === selectedStepId && selectedSubpart === 'thought' }" type="button" @click="$emit('select-step-thought', step.id)">
                                            <span class="child-mark"></span>
                                            <span class="tool-tag thought-tag">🧠 思考</span>
                                            <span>{{ shortText(step.thought, 110) }}</span>
                                            <em v-if="step.metrics?.thinkingDurationMs != null">{{ formatDurationSeconds(step.metrics.thinkingDurationMs) }}</em>
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
                                            <em v-if="step.metrics?.toolDurationMs != null" class="card-duration">{{ formatDurationSeconds(step.metrics.toolDurationMs) }}</em>
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
                                            <em v-if="step.metrics?.toolDurationMs != null">{{ formatDurationSeconds(step.metrics.toolDurationMs) }} · {{ step.source }}</em>
                                            <em v-else>{{ step.source }}</em>
                                        </button>

                                        <!-- 4. 工具直供结构化视图卡片：后端 view 字段（kind 判别联合），
                                             有值时渲染类型化卡片，脱离文本启发式猜测 -->
                                        <div v-if="step.view?.kind === 'search'" class="tool-view-card" @click.stop="$emit('select-step', step.id)">
                                            <div class="tool-view-head">
                                                <span class="tool-view-badge">检索</span>
                                                <span class="tool-view-query">{{ (step.view as any).query }}</span>
                                                <em class="tool-view-count">{{ (step.view as any).total_hits }} 条命中<template v-if="(step.view as any).truncated">（已截断）</template></em>
                                            </div>
                                            <ul class="tool-view-hits">
                                                <li v-for="(hit, hi) in ((step.view as any).hits || []).slice(0, 6)" :key="hi">
                                                    <span class="hit-path">{{ hit.path }}</span>
                                                    <span v-if="hit.snippet" class="hit-snippet">{{ hit.snippet }}</span>
                                                </li>
                                                <li v-if="((step.view as any).hits || []).length > 6" class="hit-more">…共 {{ (step.view as any).hits.length }} 条，点开看全部</li>
                                            </ul>
                                        </div>
                                        <div v-else-if="step.view?.kind === 'search-cache'" class="tool-view-card is-cache" @click.stop="$emit('select-step', step.id)">
                                            <div class="tool-view-head">
                                                <span class="tool-view-badge cache">去重锚点</span>
                                                <span class="tool-view-query">复用第 {{ (step.view as any).step_id }} 步结果 · {{ (step.view as any).total_hits }} 条命中</span>
                                            </div>
                                            <ul class="tool-view-hits">
                                                <li v-for="(p, pi) in ((step.view as any).top_paths || [])" :key="pi"><span class="hit-path">{{ p }}</span></li>
                                            </ul>
                                        </div>
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

                            <!-- 🌟 当用户在运行中向上滑动查阅时，显示浮动恢复吸底按钮 -->
                            <button
                                v-if="(entry.status === 'running' || isPlanRunning(entry)) && planFollowTailState[entry.id] === false"
                                type="button"
                                class="resume-plan-follow-btn"
                                @click.stop="resumePlanFollow(entry)"
                            >
                                <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><polyline points="6 9 12 15 18 9"></polyline></svg>
                                <span>回到底部 · 跟随最新步骤</span>
                            </button>
                        </div>
                    </div>
                </article>
                <article
                    v-else-if="entry.type === 'active-thought'"
                    class="plan-node status-running is-active-plan is-collapsed"
                    :class="{ selected: selectedNarrative === 'active-thought' }"
                    role="button"
                    tabindex="0"
                    title="点击查看完整思考过程"
                    @click="$emit('select-active-thought')"
                    @keydown.enter="$emit('select-active-thought')"
                >
                    <div class="plan-row is-running-row">
                        <span class="stage-spinner-ring manus-spinner mini"></span>
                        <span class="plan-title-bold">阶段 1 · 规划执行策略</span>
                        <span class="plan-running-badge badge-step-switch">进行中 · 准备中</span>
                        <div class="plan-row-spacer"></div>
                        <time class="plan-running-time">{{ formatClock(entry.timestamp) }}</time>
                    </div>

                    <div class="plan-running-preview">
                        <!-- 第 1 行：环境准备 -->
                        <div class="running-preview-line line-recent-action line-step-switch">
                            <span class="preview-icon">📋</span>
                            <span class="preview-text">准备阶段上下文与工作区环境</span>
                            <span class="preview-duration">0.2s</span>
                        </div>

                        <!-- 第 2 行：锚定任务目标 (绝不与第3行思考重复) -->
                        <div class="running-preview-line line-thought line-step-switch" :title="props.currentGoal ? '目标：' + props.currentGoal : '规划：按阶段目标制定实施方案并有序推进'">
                            <span class="preview-icon">🎯</span>
                            <span class="preview-text">{{ props.currentGoal ? '目标：' + shortText(props.currentGoal, 60) : '规划：按阶段目标制定实施方案并有序推进' }}</span>
                        </div>

                        <!-- 第 3 行：当前高速滚动推演的思考流光 (Ticker 滚动窗口，带有流光与垂直翻牌) -->
                        <div class="running-preview-line line-active-action">
                            <span class="preview-spinner manus-spinner mini"></span>
                            <span class="preview-tool-icon icon-anim-switch">💭</span>
                            <span class="preview-think-tag">思考:</span>
                            <div class="preview-flip-ticker-viewport">
                                <Transition name="ticker-flip">
                                    <span
                                        :key="`init-think-${liveThoughtInfo.lineKey}`"
                                        v-scroll-tail
                                        class="preview-text text-shimmer-flow"
                                        :title="liveThoughtInfo.text"
                                    >
                                        {{ liveThoughtInfo.text || '正在连接模型并深度思考规划执行步骤 …' }}
                                    </span>
                                </Transition>
                            </div>
                        </div>
                    </div>
                </article>

                <article v-else-if="entry.type === 'completion'" class="saved-message from-agent final-response" :class="{ failed: entry.failed }">
                    <div class="agent-msg-header">
                        <span class="agent-logo-pill">⚡ Free API</span>
                        <time class="msg-clock">{{ formatClock(entry.timestamp) }}</time>
                    </div>
                    <div class="summary-rich">
                        <RichContentRenderer :content="entry.summary || entry.title" :compact-html="true" @open-preview="$emit('open-preview', $event)" />
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
import { computed, nextTick, reactive, ref, shallowReactive, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import AttachmentCard from "./AttachmentCard.vue";
import AttachmentPreviewModal from "./AttachmentPreviewModal.vue";
import type { AgentHumanForm, AgentStepItem, AgentToolStatus, AgentTeamState, ParsedAttachment, FileChangeChip, FileDiffPayload, StageFailureState, ReviewerVerdict, StageRunningPreview, StageActionCapsule } from "./types";
import { diffPayloadFromChip, formatStepOperation, parseStepFileChanges, aggregateFileChips, chipHasDiffContent, formatStageTitle, formatStageDuration, formatTokenCount, getRunningStagePreview, extractStageChronologicalCapsules, extractActiveThoughtLineInfo } from "./diffUtils";
import type { AgentArtifactInput, ArtifactDescriptor } from "../../types/artifact";
import { formatClock, formatDurationSeconds, formatMilliseconds, getToolIcon, shortText } from "./format";
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
    let rolePrefix = "";
    if (params) {
        if (typeof params === "object") {
            const p = params as any;
            if (p.role_label) rolePrefix = `[${p.role_label}] `;
            else if (p.role) rolePrefix = `[${p.role}] `;

            if (p.task && typeof p.task === "string") return `${rolePrefix}${p.task}`;
            if (p.question && typeof p.question === "string") return `${rolePrefix}${p.question}`;
            if (p.goal && typeof p.goal === "string") return `${rolePrefix}${p.goal}`;
            if (p.command && typeof p.command === "string") return `${rolePrefix}${p.command}`;
            if (p.instruction && typeof p.instruction === "string") return `${rolePrefix}${p.instruction}`;
        } else if (typeof params === "string") {
            try {
                const parsed = JSON.parse(params);
                if (parsed.role_label) rolePrefix = `[${parsed.role_label}] `;
                else if (parsed.role) rolePrefix = `[${parsed.role}] `;
                if (parsed.task) return `${rolePrefix}${parsed.task}`;
                if (parsed.question) return `${rolePrefix}${parsed.question}`;
                if (parsed.goal) return `${rolePrefix}${parsed.goal}`;
            } catch {}
        }
    }
    if (step.description && step.description !== "执行操作") return `${rolePrefix}${step.description}`;
    if (step.action && step.action !== "执行操作") return `${rolePrefix}${step.action}`;
    return isSubAgentTool(step.tool) ? "独立子任务执行与审查" : "后台协同任务";
}

async function handlePreviewArtifact(artifact: ArtifactDescriptor) {
    const { isArtifactPreviewable, openArtifactById, openArtifactInSystem } = await import("../../utils/artifact");
    if (!isArtifactPreviewable(artifact)) {
        if (artifact.artifactId) {
            await openArtifactById(artifact.artifactId);
        } else if (artifact.localPath) {
            await openArtifactInSystem(artifact.localPath);
        }
        return;
    }

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
    if (artifact.content) {
        emit("open-preview", artifact.content);
    }
}

function handleDownloadArtifact(_artifact: ArtifactDescriptor) {
    // 下载逻辑由 ArtifactCard 自带处理，此处保留钩子
}
const streamElement = ref<HTMLElement | null>(null);
const activeThoughtStartedAt = ref<number | null>(null);
const followLiveTail = ref(true);
const planExpandedState = reactive<Record<string, boolean>>({});

// 🌟 换行感知实时思考流与上下垂直翻牌切换 (Flip-up Ticker)
// 1. 严格保留大模型思考过程中的换行 \n 节奏，提取当前正在活跃输出的最后有效行并从头渲染；
// 2. 换行触发 lineKey 自增，配合 Vue Transition 触发上下翻牌推挤（旧行被顶出，新行从下方翻入）；
// 3. 在同一行内持续吐字时原地追加，若超出视口宽度则配合 v-scroll-tail 尾随平滑滚动。
const liveThoughtInfo = computed(() => {
    const raw = props.thought;
    return extractActiveThoughtLineInfo(raw || "");
});

function getLine3DisplayText(entry: PlanEntry): string {
    const preview = getPlanRunningPreview(entry);
    if (preview.line3.actionKind === "think") {
        return liveThoughtInfo.value.text || preview.line3.text || "正在深入分析全局上下文与决策规划 …";
    }
    return preview.line3.text || "正在执行操作 …";
}

function getLine3FullText(entry: PlanEntry): string {
    const preview = getPlanRunningPreview(entry);
    if (preview.line3.actionKind === "think") {
        return props.thought || liveThoughtInfo.value.text || preview.line3.text;
    }
    return preview.line3.text;
}

function getLine3Key(entry: PlanEntry): string {
    const preview = getPlanRunningPreview(entry);
    if (preview.line3.actionKind === "think") {
        const keyId = liveThoughtInfo.value.lineKey || preview.line3.lineKey || 0;
        return `think-${entry.id}-${keyId}`;
    }
    return `action-${entry.id}-${preview.line3.actionKind}-${preview.line3.text}`;
}

// 🌟 平滑尾随滚动指令 (Smooth Tail-follow Scroll)
// 1. 统一由 rAF 帧调度：在高频 Token 涌入时合并单帧内的重复更新，杜绝强制同步重排；
// 2. 即时末尾对齐：超长文本时右边每长出一个新字，左边老字被自然顶出视口；
// 3. 动态 class：只有存在左侧溢出时才激活渐隐 mask，确保新行从头渲染时首字符 100% 清晰锐利。
const scrollTailPending = new Set<HTMLElement>();
let scrollTailRafId: number | null = null;

function flushScrollTail() {
    scrollTailRafId = null;
    for (const el of scrollTailPending) {
        if (el.scrollWidth > el.clientWidth) {
            el.scrollLeft = el.scrollWidth - el.clientWidth;
        } else {
            el.scrollLeft = 0;
        }
        if (el.scrollLeft > 6) {
            el.classList.add("has-left-overflow");
        } else {
            el.classList.remove("has-left-overflow");
        }
    }
    scrollTailPending.clear();
}

// 自定义指令：新行挂载时从 0 开始，溢出时自动平滑尾随
const vScrollTail = {
    mounted(el: HTMLElement) {
        el.scrollLeft = 0;
        el.classList.remove("has-left-overflow");
        scrollTailPending.add(el);
        if (scrollTailRafId === null) {
            scrollTailRafId = requestAnimationFrame(flushScrollTail);
        }
    },
    updated(el: HTMLElement) {
        scrollTailPending.add(el);
        if (scrollTailRafId === null) {
            scrollTailRafId = requestAnimationFrame(flushScrollTail);
        }
    },
};

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

// ── 🌟 运行态计划步骤视窗滚动与智能吸底 ──
const planFollowTailState = reactive<Record<string, boolean>>({});
const planInnerRefs = shallowReactive<Record<string, HTMLElement | null>>({});

function setPlanInnerRef(planId: string, el: any) {
    if (el) {
        planInnerRefs[planId] = el as HTMLElement;
    } else {
        delete planInnerRefs[planId];
    }
}

function onPlanScroll(event: Event, entry: PlanEntry) {
    const target = event.currentTarget as HTMLElement;
    if (!target) return;
    const isNearBottom = target.scrollHeight - target.scrollTop - target.clientHeight <= 16;
    planFollowTailState[entry.id] = isNearBottom;
}

function scrollPlanToBottom(planId: string, smooth = false) {
    nextTick(() => {
        requestAnimationFrame(() => {
            const el = planInnerRefs[planId];
            if (el) {
                if (smooth) {
                    el.scrollTo({ top: el.scrollHeight, behavior: 'smooth' });
                } else {
                    el.scrollTop = el.scrollHeight;
                }
            }
        });
    });
}

function resumePlanFollow(entry: PlanEntry) {
    planFollowTailState[entry.id] = true;
    scrollPlanToBottom(entry.id, true);
}

function getVisiblePlanSteps(entry: PlanEntry): AgentStepItem[] {
    return entry.steps;
}

// 🌟 运行中步骤追加或流式更新时，自动保持吸底跟随
watch(
    () => props.steps.map(s => `${s.id}-${s.status}-${s.description || ''}`).join('|'),
    () => {
        if (!props.isRunning) return;
        for (const entry of timelineEntries.value) {
            if (entry.type === 'plan' && isPlanRunning(entry) && isPlanExpanded(entry)) {
                if (planFollowTailState[entry.id] !== false) {
                    scrollPlanToBottom(entry.id, false);
                }
            }
        }
    },
    { flush: 'post' }
);

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
    let currentTurnPlanCount = 1;
    for (const entry of timelineEntries.value) {
        if (entry.type === 'user') {
            // 🌟 核心需求：每次开始新一轮对话/回合，大阶段序号重新从 1 开始独立排序
            currentTurnPlanCount = 1;
        } else if (entry.type === 'plan') {
            map.set(entry.id, currentTurnPlanCount++);
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
    return getRunningStagePreview(entry.steps, entry.isThinking || props.isThinking, props.thought, entry.title || props.currentGoal);
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
    const willExpand = !isCurrentlyExpanded;
    planExpandedState[plan.id] = willExpand;
    if (willExpand && (plan.status === 'running' || isPlanRunning(plan))) {
        planFollowTailState[plan.id] = true;
        scrollPlanToBottom(plan.id, false);
    }
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
    if (!m && step.durationMs == null) return '';
    const parts: string[] = [];
    if (m?.completionTokens) {
        parts.push(`${formatMetricTokenCount(m.completionTokens)} tok`);
    }
    if (m?.totalDurationMs != null) {
        parts.push(formatMilliseconds(m.totalDurationMs));
    } else if (step.durationMs != null) {
        parts.push(formatMilliseconds(step.durationMs));
    }
    return parts.join(' · ');
}

function getStepMetricsTooltip(step: AgentStepItem): string {
    const m = step.metrics;
    if (!m) return '';
    const lines: string[] = [];
    if (m.promptTokens) lines.push(`📥 输入上下文: ${m.promptTokens.toLocaleString()} tok (缓存命中: ${m.cacheHitRate || 0}%)`);
    if (m.completionTokens) lines.push(`📤 输出生成: ${m.completionTokens.toLocaleString()} tok`);
    if (m.thinkingDurationMs != null) lines.push(`🧠 思考耗时: ${formatDurationSeconds(m.thinkingDurationMs)}`);
    if (m.toolDurationMs != null) lines.push(`⚡ 工具耗时: ${formatDurationSeconds(m.toolDurationMs)}`);
    if (m.tokensPerSecond) lines.push(`💨 吐字速率: ${m.tokensPerSecond.toFixed(1)} tok/s`);
    return lines.join('\n');
}

// --- 🌟 真正的实时流式 Markdown 增量渲染与光标 ---
// isStreamingSummary 已移除：解耦光标与富文本内容，避免末尾方块符号与二次全量重绘卡顿

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

function planStatus(steps: AgentStepItem[], isLiveTaskActive = false): AgentToolStatus {
    // 聚合每个逻辑 stepId 的最新尝试状态（若重试已成功，阶段整体应为成功）
    const latestStatusByStep = new Map<number, AgentToolStatus>();
    for (const step of steps) {
        latestStatusByStep.set(step.stepId, step.status);
    }
    const statuses = Array.from(latestStatusByStep.values());
    if (isLiveTaskActive && statuses.some((s) => s === "running" || s === "pending" || s === "challenged")) return "running";
    if (statuses.some((s) => s === "failed")) return "failed";
    return "succeeded";
}
function planEntries(steps: AgentStepItem[], prefix: string, active = false): PlanEntry[] {
    const isLive = prefix === "live";
    const isTaskActive = isLive && props.isRunning && !props.finalSummary?.trim() && !props.completionMessage?.trim();
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
        const calculatedStatus = planStatus(plan.steps, isTaskActive);

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
    const isCompletedOrSettled = !props.isRunning || Boolean(props.completionMessage);
    if (isCompletedOrSettled && (props.completionMessage || props.finalSummary || props.finalArtifacts.length)) {
        timeline.push({ id: "live-completion", type: "completion", timestamp: Date.now(), title: props.completionMessage || (props.hasError ? "任务未能完成" : "任务已完成"), summary: props.finalSummary || props.completionMessage, artifacts: props.finalArtifacts, failed: props.hasError });
    }
    return timeline;
}

const timelineEntries = computed<TimelineEntry[]>(() => [...historyTimelineEntries.value, ...currentTimeline()]);
function statusLabel(status: AgentToolStatus) { return { pending: "等待", running: "执行中", succeeded: "完成", failed: "失败", challenged: "验证未闭合" }[status] || "未知"; }
function isSubAgentTool(tool?: string): boolean {
    const t = (tool || "").toLowerCase();
    return t.includes("child_reviewer") || t.includes("child_writer") || t.includes("child_worker") || t.includes("sub_agent");
}
function getSubAgentBadgeText(step: any): string {
    const tool = (step?.tool || step?.action || "").toLowerCase();
    const isReviewer = tool.includes("child_reviewer");
    const isWriter = tool.includes("child_writer");
    const isWorker = tool.includes("child_worker");
    const defaultKindLabel = isReviewer ? "审查" : isWriter ? "写入" : isWorker ? "侦察" : "子任务";

    const params = step?.params || step?.params_summary;
    let roleLabel = "";
    if (params) {
        let p: any = params;
        if (typeof params === "string") {
            try { p = JSON.parse(params); } catch {}
        }
        if (p && typeof p === "object") {
            roleLabel = String(p.role_label || p.roleLabel || p.card_label || p.role || "").trim();
        }
    }

    if (!roleLabel && (step?.description || step?.action)) {
        const full = `${step.description || ""} ${step.action || ""}`;
        const match = full.match(/(?:专家角色|挂载专家|角色)[：:]\s*([^\s，,）)\n]+)/);
        if (match && match[1]) {
            roleLabel = match[1].trim();
        }
    }

    if (roleLabel) {
        if (roleLabel.length <= 14) {
            return `${roleLabel} · ${defaultKindLabel}`;
        }
        return roleLabel;
    }

    return `子智能体 · ${defaultKindLabel}`;
}
function isWaitJobsTool(tool?: string): boolean {
    const t = (tool || "").toLowerCase();
    return t.includes("wait_jobs") || t.includes("wait_job");
}
function formatConversationTime(value: string) { const date = new Date(value); return Number.isNaN(date.getTime()) ? "" : date.toLocaleString("zh-CN", { hour: "2-digit", minute: "2-digit", month: "numeric", day: "numeric" }); }
watch(() => [timelineEntries.value.length, props.completionMessage, props.historyTurns.length], () => { followLatest(); }, { flush: "post" });
watch(() => props.isRunning, (running, wasRunning) => { if (running && !wasRunning) { followLiveTail.value = true; followLatest(); } if (!running && wasRunning) followLatest(); });
</script>
<style scoped src="./agent-chat-stream.css"></style>

