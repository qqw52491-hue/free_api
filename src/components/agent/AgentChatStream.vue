<template>
    <div ref="streamElement" class="process-tree" :class="{ 'is-running': isRunning }" aria-label="Agent 分层执行过程" @scroll.passive="onStreamScroll">
        <div class="stream-sticky-actions">
            <button v-if="isRunning && !followLiveTail" type="button" class="follow-latest" @click="resumeLiveFollow">跟随最新输出</button>
            <div class="stream-view-toggle floating-view-toggle" @click.stop>
                <button
                    type="button"
                    class="toggle-btn"
                    :class="{ active: streamViewMode === 'ambient' }"
                    title="简报模式：完全无框极简叙事流，突出大字号关键突破"
                    @click.stop="setStreamViewMode('ambient')"
                >
                    <span class="toggle-text">🍃 简报</span>
                </button>
                <button
                    type="button"
                    class="toggle-btn"
                    :class="{ active: streamViewMode === 'detailed' }"
                    title="全量模式：阶段卡片与底层工具调用细节"
                    @click.stop="setStreamViewMode('detailed')"
                >
                    <span class="toggle-text">⚡ 全量</span>
                </button>
            </div>
        </div>

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
                            <div class="msg-header-actions">
                                <button
                                    type="button"
                                    class="msg-fork-btn"
                                    :disabled="isRunning"
                                    :title="isRunning ? '任务正在执行中，请等待完成或停止后再分叉' : '分叉此轮回答（以此回答为基准克隆新会话继续探索）'"
                                    @click.stop="$emit('fork', { upToMessageId: message.id })"
                                >
                                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                                        <circle cx="6" cy="6" r="2.5" /><circle cx="18" cy="6" r="2.5" /><circle cx="6" cy="18" r="2.5" />
                                        <path d="M6 8.5v7" /><path d="M18 8.5c0 4-4 4-8.5 5.5" />
                                    </svg>
                                    <span>分叉</span>
                                </button>
                                <button
                                    type="button"
                                    class="msg-copy-btn"
                                    :class="{ copied: copiedMessageId === `legacy-${message.id}` }"
                                    :title="copiedMessageId === `legacy-${message.id}` ? '已复制到剪贴板' : '复制回答内容 (Markdown)'"
                                    @click.stop="copyMessageContent(`legacy-${message.id}`, message.content)"
                                >
                                    <svg v-if="copiedMessageId === `legacy-${message.id}`" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="#34d399" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
                                        <polyline points="20 6 9 17 4 12" />
                                    </svg>
                                    <svg v-else width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                                        <rect width="14" height="14" x="8" y="8" rx="2" ry="2" />
                                        <path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" />
                                    </svg>
                                    <span>{{ copiedMessageId === `legacy-${message.id}` ? "已复制" : "复制" }}</span>
                                </button>
                            </div>
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
                        <CollapsibleUserText
                            v-if="parseUserMessageAttachments(message.content).cleanText"
                            :text="parseUserMessageAttachments(message.content).cleanText"
                        />
                        <div class="msg-side-actions">
                            <time class="msg-clock">{{ formatConversationTime(message.created_at) }}</time>
                            <button
                                type="button"
                                class="msg-fork-btn"
                                :disabled="isRunning"
                                :title="isRunning ? '任务正在执行中，请等待完成或停止后再分叉' : '在此分叉新会话（带入该对话内容作为草稿继续探索）'"
                                @click.stop="$emit('fork', { messageId: message.id })"
                            >
                                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                                    <circle cx="6" cy="6" r="2.5" /><circle cx="18" cy="6" r="2.5" /><circle cx="6" cy="18" r="2.5" />
                                    <path d="M6 8.5v7" /><path d="M18 8.5c0 4-4 4-8.5 5.5" />
                                </svg>
                                <span>分叉</span>
                            </button>
                            <button
                                type="button"
                                class="msg-copy-btn"
                                :class="{ copied: copiedMessageId === `legacy-${message.id}` }"
                                :title="copiedMessageId === `legacy-${message.id}` ? '已复制到剪贴板' : '复制消息内容'"
                                @click.stop="copyMessageContent(`legacy-${message.id}`, parseUserMessageAttachments(message.content).cleanText || message.content)"
                            >
                                <svg v-if="copiedMessageId === `legacy-${message.id}`" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="#34d399" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
                                    <polyline points="20 6 9 17 4 12" />
                                </svg>
                                <svg v-else width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                                    <rect width="14" height="14" x="8" y="8" rx="2" ry="2" />
                                    <path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" />
                                </svg>
                                <span>{{ copiedMessageId === `legacy-${message.id}` ? "已复制" : "复制" }}</span>
                            </button>
                        </div>
                    </template>
                </article>
            </section>

            <template v-for="entry in timelineEntries" :key="entry.id">
                <article v-if="entry.type === 'user'" class="user-message" :data-tl="entry.id">
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
                    <CollapsibleUserText
                        v-if="parseUserMessageAttachments(entry.content).cleanText"
                        :text="parseUserMessageAttachments(entry.content).cleanText"
                    />
                    <div class="msg-side-actions">
                        <time class="msg-clock">{{ formatClock(entry.timestamp) }}</time>
                        <button
                            type="button"
                            class="msg-fork-btn"
                            :disabled="isRunning"
                            :title="isRunning ? '任务正在执行中，请等待完成或停止后再分叉' : '在此分叉新会话（带入该对话内容作为草稿继续探索）'"
                            @click.stop="$emit('fork', { timestampMs: entry.timestamp, promptSeed: parseUserMessageAttachments(entry.content).cleanText || entry.content })"
                        >
                            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                                <circle cx="6" cy="6" r="2.5" /><circle cx="18" cy="6" r="2.5" /><circle cx="6" cy="18" r="2.5" />
                                <path d="M6 8.5v7" /><path d="M18 8.5c0 4-4 4-8.5 5.5" />
                            </svg>
                            <span>分叉</span>
                        </button>
                        <button
                            type="button"
                            class="msg-copy-btn"
                            :class="{ copied: copiedMessageId === `user-${entry.id}` }"
                            :title="copiedMessageId === `user-${entry.id}` ? '已复制到剪贴板' : '复制消息内容'"
                            @click.stop="copyMessageContent(`user-${entry.id}`, parseUserMessageAttachments(entry.content).cleanText || entry.content)"
                        >
                            <svg v-if="copiedMessageId === `user-${entry.id}`" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="#34d399" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
                                <polyline points="20 6 9 17 4 12" />
                            </svg>
                            <svg v-else width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                                <rect width="14" height="14" x="8" y="8" rx="2" ry="2" />
                                <path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" />
                            </svg>
                            <span>{{ copiedMessageId === `user-${entry.id}` ? "已复制" : "复制" }}</span>
                        </button>
                    </div>
                </article>
                <!-- 人工干预：折叠为步骤条内的单行标记（用户的选择/应答），不再渲成独立气泡 -->
                <div
                    v-else-if="entry.type === 'message' && entry.isHuman"
                    class="intervention-marker"
                    :data-tl="entry.id"
                    :title="interventionSummary(entry.content)"
                    @click="$emit('select-human', entry.id)"
                >
                    <span class="intervention-marker-icon">👤</span>
                    <span class="intervention-marker-label">人工干预</span>
                    <span class="intervention-marker-text">{{ interventionSummary(entry.content) }}</span>
                    <span v-if="parseUserMessageAttachments(entry.content).attachments.length" class="intervention-marker-att">📎{{ parseUserMessageAttachments(entry.content).attachments.length }}</span>
                    <button
                        v-if="entry.anchorId"
                        type="button"
                        class="intervention-anchor-btn"
                        title="跳转到该应答对应的步骤位置"
                        @click.stop="scrollToTimelineEntry(entry.anchorId)"
                    >↩</button>
                    <time class="msg-clock">{{ formatClock(entry.timestamp) }}</time>
                </div>
                <article v-else-if="entry.type === 'message'" :data-tl="entry.id" class="saved-message" :class="entry.role === 'user' ? 'from-user' : 'from-agent'" @click="entry.isHuman && $emit('select-human', entry.id)">
                    <template v-if="entry.role === 'agent' || entry.role === 'assistant'">
                        <div class="agent-msg-header">
                            <span class="agent-logo-pill">⚡ Free API</span>
                            <time class="msg-clock">{{ formatClock(entry.timestamp) }}</time>
                            <div class="msg-header-actions">
                                <button
                                    type="button"
                                    class="msg-fork-btn"
                                    :disabled="isRunning"
                                    :title="isRunning ? '任务正在执行中，请等待完成或停止后再分叉' : '分叉此轮回答（以此回答为基准克隆新会话继续探索）'"
                                    @click.stop="$emit('fork', { upToTimestampMs: entry.timestamp })"
                                >
                                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                                        <circle cx="6" cy="6" r="2.5" /><circle cx="18" cy="6" r="2.5" /><circle cx="6" cy="18" r="2.5" />
                                        <path d="M6 8.5v7" /><path d="M18 8.5c0 4-4 4-8.5 5.5" />
                                    </svg>
                                    <span>分叉</span>
                                </button>
                                <button
                                    type="button"
                                    class="msg-copy-btn"
                                    :class="{ copied: copiedMessageId === `entry-${entry.id}` }"
                                    :title="copiedMessageId === `entry-${entry.id}` ? '已复制到剪贴板' : '复制回答内容 (Markdown)'"
                                    @click.stop="copyMessageContent(`entry-${entry.id}`, entry.content)"
                                >
                                    <svg v-if="copiedMessageId === `entry-${entry.id}`" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="#34d399" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
                                        <polyline points="20 6 9 17 4 12" />
                                    </svg>
                                    <svg v-else width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                                        <rect width="14" height="14" x="8" y="8" rx="2" ry="2" />
                                        <path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" />
                                    </svg>
                                    <span>{{ copiedMessageId === `entry-${entry.id}` ? "已复制" : "复制" }}</span>
                                </button>
                            </div>
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
                        <CollapsibleUserText
                            v-if="parseUserMessageAttachments(entry.content).cleanText"
                            :text="parseUserMessageAttachments(entry.content).cleanText"
                        />
                        <div class="msg-side-actions">
                            <time class="msg-clock">{{ formatClock(entry.timestamp) }}</time>
                            <button
                                type="button"
                                class="msg-fork-btn"
                                :disabled="isRunning"
                                :title="isRunning ? '任务正在执行中，请等待完成或停止后再分叉' : '在此分叉新会话（带入该对话内容作为草稿继续探索）'"
                                @click.stop="$emit('fork', { timestampMs: entry.timestamp })"
                            >
                                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                                    <circle cx="6" cy="6" r="2.5" /><circle cx="18" cy="6" r="2.5" /><circle cx="6" cy="18" r="2.5" />
                                    <path d="M6 8.5v7" /><path d="M18 8.5c0 4-4 4-8.5 5.5" />
                                </svg>
                                <span>分叉</span>
                            </button>
                            <button
                                type="button"
                                class="msg-copy-btn"
                                :class="{ copied: copiedMessageId === `human-${entry.id}` }"
                                :title="copiedMessageId === `human-${entry.id}` ? '已复制到剪贴板' : '复制消息内容'"
                                @click.stop="copyMessageContent(`human-${entry.id}`, parseUserMessageAttachments(entry.content).cleanText || entry.content)"
                            >
                                <svg v-if="copiedMessageId === `human-${entry.id}`" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="#34d399" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
                                    <polyline points="20 6 9 17 4 12" />
                                </svg>
                                <svg v-else width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                                    <rect width="14" height="14" x="8" y="8" rx="2" ry="2" />
                                    <path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" />
                                </svg>
                                <span>{{ copiedMessageId === `human-${entry.id}` ? "已复制" : "复制" }}</span>
                            </button>
                        </div>
                    </template>
                </article>
                <article
                    v-else-if="entry.type === 'plan'"
                    :data-tl="entry.id"
                    :class="streamViewMode === 'ambient' ? 'ambient-plan-wrapper' : [
                        'plan-node',
                        `status-${entry.status}`,
                        {
                            'is-active-plan': isPlanRunning(entry),
                            'is-collapsed': !isPlanExpanded(entry),
                            'is-pure-read': getPlanDigest(entry.steps).capsules.length === 0
                        }
                    ]"
                >
                    <!-- 🌟 模式 A：极简高信噪比动作流 (融合智谱纯净度与精美矢量图标，支持超长步骤智能折叠与底部渐隐) -->
                    <div v-if="streamViewMode === 'ambient'" class="ambient-plan-stream-wrap">
                        <div
                            class="ambient-plan-stream"
                            :class="{ 'is-collapsed-mask': shouldCollapseAmbientStream(entry) }"
                        >
                        <template v-for="item in getAmbientDisplayItems(entry)" :key="item.id">
                            <!-- 0. 阶段解说段落 (每轮 thought 叙述：学到什么/当前判断/下一步意图) -->
                            <RichContentRenderer
                                v-if="item.type === 'narrative'"
                                class="ambient-narrative"
                                :content="item.text || ''"
                                :compact-html="true"
                                @open-preview="$emit('open-preview', $event)"
                            />

                            <!-- 0.5 解说抑制小行：关键时刻之外的高频 thought 降级为灰色小行 (保持单行紧凑截断) -->
                            <p
                                v-else-if="item.type === 'narrative-mini'"
                                class="ambient-narrative-mini"
                            >{{ item.text }}</p>

                            <!-- 1. 单项动作行 (修改/新建/读取/终端/单次检索/突破) -->
                            <article
                                v-else-if="item.type === 'step'"
                                class="ambient-text-row"
                                :class="[
                                    `status-${item.step.status}`,
                                    `action-${item.action.actionKind}`,
                                    {
                                        'is-milestone': item.action.isMilestone,
                                        'is-warning': item.action.isWarning,
                                        [`finding-${item.step.findingKind}`]: item.action.isMilestone && item.step.findingKind
                                    }
                                ]"
                                :title="item.action.tooltip"
                                @click="$emit('select-step', item.step.id)"
                            >
                                <span class="ambient-text-content">
                                    <span
                                        class="ambient-action-lead clickable"
                                        :class="`lead-${item.action.actionKind}`"
                                        :title="item.action.tooltip"
                                        @click.stop="handleAmbientChipClick(item.step, item.action)"
                                    >
                                        <!-- 1.1 动作类型微标 (有特定业务意图才显示，无修改不加'编辑'标签，通用操作统一兜底保证左侧垂直扫描对齐) -->
                                        <span v-if="item.action.actionKind === 'subagent'" class="action-tag action-tag-subagent">{{ item.action.subagentTag || '🤖 子代理' }}</span>
                                        <span v-else-if="item.action.actionKind === 'write'" class="action-tag action-tag-write">📄 新建</span>
                                        <span v-else-if="item.action.actionKind === 'read'" class="action-tag action-tag-read">📖 读取</span>
                                        <span v-else-if="item.action.actionKind === 'shell'" class="action-tag action-tag-shell">⚡ 终端</span>
                                        <span v-else-if="item.action.actionKind === 'search'" class="action-tag action-tag-search">🔍 检索</span>
                                        <span v-else-if="item.action.actionKind === 'web'" class="action-tag action-tag-web">{{ item.action.iconEmoji || '🌐' }} 网页</span>
                                        <span v-else-if="item.action.actionKind === 'mcp'" class="action-tag action-tag-mcp">🧩 MCP</span>
                                        <span v-else class="action-tag action-tag-general">⚡ 操作</span>

                                        <!-- 1.2 专属语言/文件矢量图标 (FileIcon，过滤占位文件名避免出现假标签) -->
                                        <FileIcon
                                            v-if="item.action.fileName && item.action.fileName !== '新文件' && item.action.fileName !== '文件内容' && item.action.fileName !== '代码变更'"
                                            :name="item.action.fileName"
                                            :kind="item.action.iconKind || ''"
                                            :size="14"
                                            class="chip-file-icon"
                                        />

                                        <!-- 1.3 核心文件名或命令 (milestone 类型标题已由动作标签渲染，避免重复；过滤占位名) -->
                                        <span v-if="item.action.actionKind !== 'milestone' && item.action.capsuleTitle && item.action.capsuleTitle !== '新文件' && item.action.capsuleTitle !== '文件内容' && item.action.capsuleTitle !== '代码变更'" class="chip-file-name">{{ item.action.capsuleTitle }}</span>

                                        <!-- 1.4 所在目录简写 (如 src/bin/) -->
                                        <span v-if="item.action.filePath && getDirName(item.action.filePath)" class="chip-file-dir">
                                            {{ getDirName(item.action.filePath) }}
                                        </span>

                                        <!-- 1.5 行号或耗时 (如 #L70-120, 0.5s，排除重复的 'MCP'/'新建'/'修改' 标签) -->
                                        <span v-if="item.action.capsuleBadge && item.action.capsuleBadge !== 'MCP' && item.action.capsuleBadge !== '新建' && item.action.capsuleBadge !== '修改'" class="chip-line-range">
                                            {{ item.action.capsuleBadge }}
                                        </span>

                                        <!-- 1.6 Diff 增删行微标 (如 +22 -163) -->
                                        <span v-if="(item.action.additions || 0) > 0" class="chip-diff-add">+{{ item.action.additions }}</span>
                                        <span v-if="(item.action.deletions || 0) > 0" class="chip-diff-del">−{{ item.action.deletions }}</span>
                                        <span v-if="item.action.isIntercepted" class="chip-intercepted">{{ item.action.actionKind === 'subagent' ? '未闭合' : '已拦截' }}</span>
                                    </span>

                                    <!-- 1.7 说明文字 -->
                                    <span v-if="item.action.explanation" class="ambient-text-body" :class="{ 'is-milestone': item.action.isMilestone }">
                                        {{ item.action.explanation }}
                                    </span>
                                </span>
                                <time class="ambient-text-time">{{ formatClock(item.step.timestamp) }}</time>
                            </article>

                            <!-- 2. 连续检索智能微折叠卡片 (大量 ripgrep 静默汇聚) -->
                            <div
                                v-else-if="item.type === 'search-group'"
                                class="ambient-search-group"
                                :class="{ 'is-expanded': isSearchGroupExpanded(item.id) }"
                            >
                                <div class="ambient-search-group-header" @click="toggleSearchGroup(item.id)">
                                    <div class="search-group-left">
                                        <span class="action-tag action-tag-search">🔍 检索</span>
                                        <span class="search-group-summary">{{ item.summary }}</span>
                                        <span v-if="item.targetFiles.length > 0" class="search-group-files">
                                            · {{ item.targetFiles.slice(0, 3).join(', ') }}{{ item.targetFiles.length > 3 ? ' 等' : '' }}
                                        </span>
                                        <span v-if="item.durationBadge" class="chip-duration">{{ item.durationBadge }}</span>
                                    </div>
                                    <div class="search-group-right">
                                        <button type="button" class="search-group-toggle-btn" @click.stop="toggleSearchGroup(item.id)">
                                            <span>{{ isSearchGroupExpanded(item.id) ? '收起明细' : `展开 ${item.count} 项检索` }}</span>
                                            <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" :class="{ 'rotate-180': isSearchGroupExpanded(item.id) }">
                                                <polyline points="6 9 12 15 18 9" />
                                            </svg>
                                        </button>
                                        <time class="ambient-text-time">{{ formatClock(item.timestamp) }}</time>
                                    </div>
                                </div>

                                <!-- 展开后的检索明细列表 -->
                                <div v-if="isSearchGroupExpanded(item.id)" class="ambient-search-group-list">
                                    <article
                                        v-for="subStep in item.steps"
                                        :key="subStep.id"
                                        class="ambient-search-sub-row"
                                        @click="$emit('select-step', subStep.id)"
                                    >
                                        <span class="search-sub-bullet">·</span>
                                        <span class="search-sub-lead">
                                            <FileIcon
                                                v-if="getAmbientAction(subStep).fileName"
                                                :name="getAmbientAction(subStep).fileName || ''"
                                                :size="12"
                                                class="chip-file-icon"
                                            />
                                            <span class="search-sub-title">{{ getAmbientAction(subStep).capsuleTitle }}</span>
                                        </span>
                                        <span class="search-sub-body">{{ getAmbientAction(subStep).explanation }}</span>
                                        <time class="ambient-text-time mini">{{ formatClock(subStep.timestamp) }}</time>
                                    </article>
                                </div>
                            </div>

                            <!-- 2.5 连续 MCP 操作智能微折叠卡片 (高信噪比收拢连续工具调用) -->
                            <div
                                v-else-if="item.type === 'mcp-group'"
                                class="ambient-action-group ambient-mcp-group"
                                :class="{ 'is-expanded': isMcpGroupExpanded(item.id) }"
                            >
                                <div class="ambient-action-group-header" @click="toggleMcpGroup(item.id)">
                                    <div class="action-group-left">
                                        <span class="action-tag action-tag-mcp">🧩 MCP</span>
                                        <span class="action-group-summary">{{ item.summary }}</span>
                                        <span v-if="item.durationBadge" class="chip-duration">{{ item.durationBadge }}</span>
                                    </div>
                                    <div class="action-group-right">
                                        <button type="button" class="action-group-toggle-btn" @click.stop="toggleMcpGroup(item.id)">
                                            <span>{{ isMcpGroupExpanded(item.id) ? '收起明细' : `展开 ${item.count} 项操作` }}</span>
                                            <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" :class="{ 'rotate-180': isMcpGroupExpanded(item.id) }">
                                                <polyline points="6 9 12 15 18 9" />
                                            </svg>
                                        </button>
                                        <time class="ambient-text-time">{{ formatClock(item.timestamp) }}</time>
                                    </div>
                                </div>

                                <!-- 展开后的 MCP 明细列表 -->
                                <div v-if="isMcpGroupExpanded(item.id)" class="ambient-action-group-list">
                                    <article
                                        v-for="subStep in item.steps"
                                        :key="subStep.id"
                                        class="ambient-action-sub-row"
                                        @click="$emit('select-step', subStep.id)"
                                    >
                                        <span class="action-sub-bullet">·</span>
                                        <span class="action-sub-title">{{ getAmbientAction(subStep).capsuleTitle }}</span>
                                        <span v-if="getAmbientAction(subStep).capsuleBadge" class="chip-line-range mini">{{ getAmbientAction(subStep).capsuleBadge }}</span>
                                        <span class="action-sub-body">{{ getAmbientAction(subStep).explanation }}</span>
                                        <time class="ambient-text-time mini">{{ formatClock(subStep.timestamp) }}</time>
                                    </article>
                                </div>
                            </div>
                        </template>

                        </div>

                        <!-- 底部渐隐折叠悬浮小箭头 (具体步骤多了隐藏，点击展开更多) -->
                        <button
                            v-if="shouldShowAmbientExpandBtn(entry)"
                            type="button"
                            class="ambient-stream-expand-btn"
                            :title="isAmbientStreamExpanded(entry.id) ? '收起具体步骤' : `展开全部 (${getAmbientStepCount(entry)} 项步骤)`"
                            @click.stop="toggleAmbientStreamExpand(entry.id)"
                        >
                            <svg viewBox="0 0 24 24" class="chevron-mini-icon" :class="{ 'is-flipped': isAmbientStreamExpanded(entry.id) }">
                                <path d="m6 9 6 6 6-6" />
                            </svg>
                        </button>

                        <!-- 正在思考与实时推进流 (独立放在下方，保证折叠时也清晰可见) -->
                        <div
                            v-if="entry.status === 'running' && isRunning"
                            class="ambient-live-text-row"
                        >
                            <div class="ambient-live-text-bar" @click="toggleLiveThoughtExpanded">
                                <span class="live-think-label">
                                    <span class="live-think-square-icon" :class="{ 'is-action': getPlanRunningPreview(entry).line3.actionKind !== 'think' }">
                                        <svg v-if="getPlanRunningPreview(entry).line3.actionKind === 'think'" class="thought-square-svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                                            <path d="M12 5a3 3 0 1 0-5.997.125 4 4 0 0 0-2.526 5.77 4 4 0 0 0 .556 6.588A4 4 0 1 0 12 18Z"/>
                                            <path d="M12 5a3 3 0 1 1 5.997.125 4 4 0 0 1 2.526 5.77 4 4 0 0 1-.556 6.588A4 4 0 1 1 12 18Z"/>
                                            <path d="M12 5v13"/>
                                            <path d="M15.5 8.5a4 4 0 0 0-7 0"/>
                                            <path d="M16 12a4 4 0 0 0-8 0"/>
                                            <path d="M15.5 15.5a4 4 0 0 0-7 0"/>
                                        </svg>
                                        <svg v-else class="thought-square-svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
                                            <polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/>
                                        </svg>
                                    </span>
                                    <span class="live-think-title">{{ getPlanRunningPreview(entry).line3.actionKind === 'think' ? '思考中：' : '推进中：' }}</span>
                                </span>
                                <div class="ambient-live-viewport">
                                    <Transition name="ticker-flip">
                                        <span
                                            :key="getLine3Key(entry)"
                                            v-scroll-tail
                                            class="ambient-live-scroll-text"
                                            :title="getLine3FullText(entry)"
                                        >
                                            {{ getLine3DisplayText(entry) }}
                                        </span>
                                    </Transition>
                                </div>
                                <button
                                    type="button"
                                    class="thought-expand-btn"
                                    :title="isLiveThoughtExpanded ? '收起完整思考' : '展开查看完整思考过程'"
                                    @click.stop="toggleLiveThoughtExpanded"
                                >
                                    <span class="thought-expand-text">{{ isLiveThoughtExpanded ? '收起' : '展开' }}</span>
                                    <svg
                                        width="11"
                                        height="11"
                                        viewBox="0 0 24 24"
                                        fill="none"
                                        stroke="currentColor"
                                        stroke-width="2.5"
                                        stroke-linecap="round"
                                        stroke-linejoin="round"
                                        :class="{ 'rotate-180': isLiveThoughtExpanded }"
                                    >
                                        <polyline points="6 9 12 15 18 9" />
                                    </svg>
                                </button>
                            </div>
                            <Transition name="thought-accordion">
                                <div v-if="isLiveThoughtExpanded" class="ambient-live-thought-expanded">
                                    <div class="thought-expanded-header">
                                        <span class="thought-expanded-title">实时思考推演</span>
                                        <span class="thought-char-count">{{ (props.thought || liveThoughtInfo.text || '').length }} 字</span>
                                    </div>
                                    <div class="thought-expanded-body soft-scrollbar">
                                        {{ props.thought || liveThoughtInfo.text || '正在深入分析全局上下文与决策规划 …' }}
                                    </div>
                                </div>
                            </Transition>
                        </div>
                    </div>

                    <!-- 🌟 模式 B：工程全量链路 (保留原有的阶段卡片、顶栏条条与折叠展开树) -->
                    <template v-else>
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

                        <Transition name="live-term-collapse">
                            <div v-if="props.liveShellOutput" class="live-term-box" @click.stop>
                                <div class="live-term-head">
                                    <span class="live-term-dot" :class="{ done: props.liveShellOutput.done }"></span>
                                    <span class="live-term-cmd" :title="props.liveShellOutput.command">$ {{ props.liveShellOutput.command }}</span>
                                    <span class="live-term-elapsed">{{ (props.liveShellOutput.elapsedMs / 1000).toFixed(1) }}s</span>
                                </div>
                                <pre :ref="setLiveTermBodyRef" class="live-term-body soft-scrollbar" @scroll="onLiveTermScroll">{{ props.liveShellOutput.tail }}</pre>
                            </div>
                        </Transition>
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
                                            <span class="tool-tag thought-tag">
                                                <svg class="thought-icon-svg mini" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                                                    <path d="M12 5a3 3 0 1 0-5.997.125 4 4 0 0 0-2.526 5.77 4 4 0 0 0 .556 6.588A4 4 0 1 0 12 18Z"/>
                                                    <path d="M12 5a3 3 0 1 1 5.997.125 4 4 0 0 1 2.526 5.77 4 4 0 0 1-.556 6.588A4 4 0 1 1 12 18Z"/>
                                                    <path d="M12 5v13"/>
                                                    <path d="M15.5 8.5a4 4 0 0 0-7 0"/>
                                                    <path d="M16 12a4 4 0 0 0-8 0"/>
                                                    <path d="M15.5 15.5a4 4 0 0 0-7 0"/>
                                                </svg>
                                                思考
                                            </span>
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
                                                <FileIcon :name="formatStepOperation(step).fileName || getStepMiniChip(step)?.fileName" :size="15" />
                                            </span>
                                            <span class="card-file-name">{{ formatStepOperation(step).fileName || getStepMiniChip(step)?.fileName }}</span>
                                            <span v-if="formatStepOperation(step).lineRange" class="card-line-range">{{ formatStepOperation(step).lineRange }}</span>
                                            <span v-if="(getStepMiniChip(step)?.additions || 0) > 0" class="card-diff-add">+{{ getStepMiniChip(step)?.additions }}</span>
                                            <span v-if="(getStepMiniChip(step)?.deletions || 0) > 0" class="card-diff-del">−{{ getStepMiniChip(step)?.deletions }}</span>
                                            <span v-if="getStepMiniChip(step)?.isIntercepted" class="card-blocked">已拦截</span>
                                            <span v-if="getStepMiniChip(step) || hasFileOperation(step)" class="card-view-diff-hint">查看 Diff →</span>
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
                                            <!-- 子 agent 运行中：换成 worker 实时动作行（翻牌+尾随滚动），不再是死文案 -->
                                            <span v-if="workerLiveLines.get(step.id)" class="subagent-live-wrap">
                                                <Transition name="ticker-flip">
                                                    <span :key="workerLiveLines.get(step.id)!.key" v-scroll-tail class="ambient-live-scroll-text subagent-live-line">
                                                        ⚡ {{ workerLiveLines.get(step.id)!.text }}
                                                    </span>
                                                </Transition>
                                            </span>
                                            <span v-else :class="{ 'subagent-action-text': isSubAgentTool(step.tool), 'waitjobs-action-text': isWaitJobsTool(step.tool) }">
                                                {{ isSubAgentTool(step.tool) ? (step.status === 'running' ? '⚡ 正在后台独立执行调查审查，点击右侧抽屉可查看证据包与发现…' : (step.action || step.explanation || '已生成审查结论 · 点击查看详情')) : (isWaitJobsTool(step.tool) ? waitJobsTileText(step) : shortText(formatStepOperation(step).actionText || step.action || step.explanation || '执行操作', 110)) }}
                                            </span>
                                            <em v-if="step.metrics?.toolDurationMs != null">{{ formatDurationSeconds(step.metrics.toolDurationMs) }} · {{ step.source }}</em>
                                            <em v-else>{{ step.source }}</em>
                                        </button>

                                        <!-- 4. 工具直供结构化视图卡片：后端 view 字段（kind 判别联合），
                                             注册表分发到 views/ 下类型化组件，脱离文本启发式猜测；
                                             kind 未注册时不渲染（回退上方通用工具行） -->
                                        <component
                                            v-if="step.view && toolViewComponents[step.view.kind]"
                                            :is="toolViewComponents[step.view.kind]"
                                            :view="step.view"
                                            @select="$emit('select-step', step.id)"
                                        />
                                    </div>
                                    <!-- 👥 模式1：内嵌多 Agent 协作微卡片 (轻量清爽，点击直通右侧 Inspector 查看) -->
                                    <div
                                        v-if="isSubAgentTool(step.tool) || isWaitJobsTool(step.tool)"
                                        class="subagent-orchestration-card"
                                        role="button"
                                        tabindex="0"
                                        :title="'点击在右侧抽屉查看多 Agent 协同链路：' + extractSubagentTask(step)"
                                        @click="$emit('open-team-deck', step.id)"
                                        @keydown.enter="$emit('open-team-deck', step.id)"
                                    >
                                        <!-- 卡片顶栏：标题与右侧查看提示 -->
                                        <div class="orchestration-header">
                                            <div class="header-main">
                                                <span class="team-icon">👥</span>
                                                <span class="orchestration-title">
                                                    {{ step.status === 'running' ? '并发协作 · 正在调度子任务' : '并发协作 · 协同任务已交付' }}
                                                </span>
                                                <span v-if="getStepActiveWorkers(step).length > 0" class="worker-count-pill">
                                                    {{ getStepActiveWorkers(step).length }} 个 Worker
                                                </span>
                                            </div>
                                            <div class="header-action">
                                                <span>在右侧查看详情</span>
                                                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5">
                                                    <polyline points="9 18 15 12 9 6" />
                                                </svg>
                                            </div>
                                        </div>

                                        <!-- 卡片内容体：展示 Workers 并行列表或具体子任务 -->
                                        <div class="orchestration-body">
                                            <div v-if="getStepActiveWorkers(step).length > 0" class="worker-chips-list">
                                                <div
                                                    v-for="worker in getStepActiveWorkers(step)"
                                                    :key="worker.id"
                                                    class="worker-chip-item"
                                                    :class="`status-${worker.status}`"
                                                >
                                                    <span class="worker-state-indicator">
                                                        <span v-if="worker.status === 'running'" class="manus-spinner mini"></span>
                                                        <span v-else-if="worker.status === 'succeeded'" class="worker-check">✓</span>
                                                        <span v-else-if="worker.status === 'failed'" class="worker-cross">✕</span>
                                                        <span v-else class="worker-dot">○</span>
                                                    </span>
                                                    <span class="worker-name">{{ worker.name }}</span>
                                                    <span v-if="worker.roleLabel" class="worker-role-tag">{{ worker.roleLabel }}</span>
                                                    <span class="worker-task-desc">
                                                        {{ worker.currentStep || worker.currentTool || (worker.status === 'succeeded' ? '已完成并回传主调度' : '等待派发') }}
                                                    </span>
                                                    <span v-if="worker.durationMs" class="worker-duration">
                                                        {{ (worker.durationMs / 1000).toFixed(1) }}s
                                                    </span>
                                                </div>
                                            </div>
                                            <div v-else class="single-subagent-task">
                                                <span class="capsule-pulse-dot" :class="{ 'is-running': step.status === 'running' }"></span>
                                                <span class="task-desc-text">
                                                    {{ step.status === 'running' ? '正在独立执行：' : '协同已交付：' }}
                                                    <strong>{{ extractSubagentTask(step) }}</strong>
                                                </span>
                                            </div>
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
                                    <pre class="active-thought-content soft-scrollbar">{{ thought || '正在分析下一步操作…' }}</pre>
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
                    </template>
                </article>
                <article
                    v-else-if="entry.type === 'active-thought'"
                    :data-tl="entry.id"
                    :class="[
                        streamViewMode === 'ambient' ? 'ambient-plan-wrapper' : 'plan-node status-running is-active-plan is-collapsed',
                        { selected: selectedNarrative === 'active-thought' }
                    ]"
                    role="button"
                    tabindex="0"
                    title="点击查看完整思考过程"
                    @click="$emit('select-active-thought')"
                    @keydown.enter="$emit('select-active-thought')"
                >
                    <div v-if="streamViewMode === 'ambient'" class="ambient-plan-stream">
                        <div class="ambient-live-text-row">
                            <div class="ambient-live-text-bar" @click="toggleLiveThoughtExpanded">
                                <span class="live-think-label">
                                    <span class="live-think-square-icon">
                                        <svg class="thought-square-svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                                            <path d="M12 5a3 3 0 1 0-5.997.125 4 4 0 0 0-2.526 5.77 4 4 0 0 0 .556 6.588A4 4 0 1 0 12 18Z"/>
                                            <path d="M12 5a3 3 0 1 1 5.997.125 4 4 0 0 1 2.526 5.77 4 4 0 0 1-.556 6.588A4 4 0 1 1 12 18Z"/>
                                            <path d="M12 5v13"/>
                                            <path d="M15.5 8.5a4 4 0 0 0-7 0"/>
                                            <path d="M16 12a4 4 0 0 0-8 0"/>
                                            <path d="M15.5 15.5a4 4 0 0 0-7 0"/>
                                        </svg>
                                    </span>
                                    <span class="live-think-title">思考中：</span>
                                </span>
                                <div class="ambient-live-viewport">
                                    <Transition name="ticker-flip">
                                        <span
                                            :key="`ambient-init-think-${liveThoughtInfo.lineKey}`"
                                            v-scroll-tail
                                            class="ambient-live-scroll-text"
                                            :title="liveThoughtInfo.text"
                                        >
                                            {{ liveThoughtInfo.text || '正在深度思考规划执行策略 …' }}
                                        </span>
                                    </Transition>
                                </div>
                                <button
                                    type="button"
                                    class="thought-expand-btn"
                                    :title="isLiveThoughtExpanded ? '收起完整思考' : '展开查看完整思考过程'"
                                    @click.stop="toggleLiveThoughtExpanded"
                                >
                                    <span class="thought-expand-text">{{ isLiveThoughtExpanded ? '收起' : '展开' }}</span>
                                    <svg
                                        width="11"
                                        height="11"
                                        viewBox="0 0 24 24"
                                        fill="none"
                                        stroke="currentColor"
                                        stroke-width="2.5"
                                        stroke-linecap="round"
                                        stroke-linejoin="round"
                                        :class="{ 'rotate-180': isLiveThoughtExpanded }"
                                    >
                                        <polyline points="6 9 12 15 18 9" />
                                    </svg>
                                </button>
                            </div>
                            <Transition name="thought-accordion">
                                <div v-if="isLiveThoughtExpanded" class="ambient-live-thought-expanded">
                                    <div class="thought-expanded-header">
                                        <span class="thought-expanded-title">实时思考推演</span>
                                        <span class="thought-char-count">{{ (props.thought || liveThoughtInfo.text || '').length }} 字</span>
                                    </div>
                                    <div class="thought-expanded-body soft-scrollbar">
                                        {{ props.thought || liveThoughtInfo.text || '正在深度思考规划执行策略 …' }}
                                    </div>
                                </div>
                            </Transition>
                        </div>
                    </div>
                    <template v-else>
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
                                <span class="preview-tool-icon icon-anim-switch">
                                    <svg class="thought-icon-svg mini" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                                        <path d="M12 5a3 3 0 1 0-5.997.125 4 4 0 0 0-2.526 5.77 4 4 0 0 0 .556 6.588A4 4 0 1 0 12 18Z"/>
                                        <path d="M12 5a3 3 0 1 1 5.997.125 4 4 0 0 1 2.526 5.77 4 4 0 0 1-.556 6.588A4 4 0 1 1 12 18Z"/>
                                        <path d="M12 5v13"/>
                                        <path d="M15.5 8.5a4 4 0 0 0-7 0"/>
                                        <path d="M16 12a4 4 0 0 0-8 0"/>
                                        <path d="M15.5 15.5a4 4 0 0 0-7 0"/>
                                    </svg>
                                </span>
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
                    </template>
                </article>

                <article
                    v-else-if="entry.type === 'completion'"
                    :data-tl="entry.id"
                    class="saved-message from-agent final-response"
                    :class="{
                        failed: entry.failed,
                        'is-streaming': isRunning && entry.id === 'live-completion'
                    }"
                >
                    <div class="agent-msg-header">
                        <span class="agent-logo-pill" :class="{ 'is-shimmering': isRunning && entry.id === 'live-completion' }">
                            <span v-if="isRunning && entry.id === 'live-completion'" class="agent-pulse-dot"></span>
                            ⚡ Free API
                        </span>
                        <time class="msg-clock">{{ formatClock(entry.timestamp) }}</time>
                        <div class="msg-header-actions">
                            <button
                                type="button"
                                class="msg-fork-btn"
                                :disabled="isRunning"
                                :title="isRunning ? '任务正在执行中，请等待完成或停止后再分叉' : '分叉此轮总结（以此为基准克隆新独立会话继续探索）'"
                                @click.stop="$emit('fork', { upToTimestampMs: entry.timestamp })"
                            >
                                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                                    <circle cx="6" cy="6" r="2.5" /><circle cx="18" cy="6" r="2.5" /><circle cx="6" cy="18" r="2.5" />
                                    <path d="M6 8.5v7" /><path d="M18 8.5c0 4-4 4-8.5 5.5" />
                                </svg>
                                <span>分叉</span>
                            </button>
                            <button
                                type="button"
                                class="msg-copy-btn"
                                :class="{ copied: copiedMessageId === `comp-hdr-${entry.id}` }"
                                :title="copiedMessageId === `comp-hdr-${entry.id}` ? '已复制到剪贴板' : '复制最终回答 (Markdown)'"
                                @click.stop="copyMessageContent(`comp-hdr-${entry.id}`, entry.summary || entry.title)"
                            >
                                <svg v-if="copiedMessageId === `comp-hdr-${entry.id}`" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="#34d399" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
                                    <polyline points="20 6 9 17 4 12" />
                                </svg>
                                <svg v-else width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                                    <rect width="14" height="14" x="8" y="8" rx="2" ry="2" />
                                    <path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" />
                                </svg>
                                <span>{{ copiedMessageId === `comp-hdr-${entry.id}` ? "已复制" : "复制" }}</span>
                            </button>
                        </div>
                    </div>
                    <div
                        class="summary-rich"
                        :class="{ 'is-streaming-text': isStreamingCompletion(entry) }"
                        :title="isStreamingCompletion(entry) ? '点击可直接瞬间完成渐显' : undefined"
                        @click="isStreamingCompletion(entry) && finishTypewriterInstantly(entry.id, entry.summary || entry.title)"
                    >
                        <RichContentRenderer :content="getCompletionDisplayText(entry)" :compact-html="true" :streaming="isStreamingCompletion(entry)" @open-preview="$emit('open-preview', $event)" />
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
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, shallowReactive, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import AttachmentCard from "./AttachmentCard.vue";
import CollapsibleUserText from "./CollapsibleUserText.vue";
import AttachmentPreviewModal from "./AttachmentPreviewModal.vue";
import type { AgentHumanForm, AgentStepItem, AgentToolStatus, AgentTeamState, AgentWorkerItem, ParsedAttachment, FileChangeChip, FileDiffPayload, StageFailureState, ReviewerVerdict, StageRunningPreview, StageActionCapsule, AgentRunState } from "./types";
import { diffPayloadFromChip, extractDiffFromStep, formatStepOperation, parseStepFileChanges, aggregateFileChips, formatStageTitle, formatStageDuration, formatTokenCount, getRunningStagePreview, extractStageChronologicalCapsules, extractActiveThoughtLineInfo } from "./diffUtils";
import type { StepOperationDisplay } from "./diff/toolOperation";
import type { AgentArtifactInput, ArtifactDescriptor } from "../../types/artifact";
import { formatClock, formatDurationSeconds, formatMilliseconds, getToolIcon, shortText } from "./format";
import RichContentRenderer from "../charts/RichContentRenderer.vue";
import AgentHumanInterventionCard from "./AgentHumanInterventionCard.vue";
import { toolViewComponents } from "./views";
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

/// 人工干预标记行的摘要文本：剥掉附件标记与 HTML 注释型元数据
/// （如 <!--plan-decision:{...}-->——协议载荷不该上屏），压缩空白成一行。
function interventionSummary(content: string): string {
    const { cleanText } = parseUserMessageAttachments(content || "");
    return cleanText
        .replace(/<!--[\s\S]*?-->/g, " ")
        .replace(/\s+/g, " ")
        .trim();
}

/// 给排好序的条目序列中的人工干预回填 anchorId = 紧邻上一条非干预条目
/// （通常是它应答的那个 plan/step 条目）——供"↩ 跳转到应答位置"滚动锚定。
function assignInterventionAnchors(entries: TimelineEntry[]) {
    let lastAnchor: string | null = null;
    for (const e of entries) {
        if (e.type === "message" && e.isHuman) {
            if (lastAnchor) e.anchorId = lastAnchor;
        } else {
            lastAnchor = e.id;
        }
    }
}

function scrollToTimelineEntry(id: string) {
    const el = document.querySelector(`[data-tl="${CSS.escape(id)}"]`);
    if (!el) return;
    el.scrollIntoView({ behavior: "smooth", block: "center" });
    // 短暂高亮锚定目标，让"跳到的是哪步"肉眼可辨
    el.classList.add("tl-anchor-flash");
    setTimeout(() => el.classList.remove("tl-anchor-flash"), 1600);
}

const previewImageModal = ref<ParsedAttachment | null>(null);
const previewDocModal = ref<ParsedAttachment | null>(null);
const copiedMessageId = ref<string | null>(null);
let copyTimer: number | null = null;

async function copyMessageContent(id: string, content: string) {
    if (!content) return;
    try {
        await navigator.clipboard.writeText(content);
    } catch {
        try {
            const ta = document.createElement("textarea");
            ta.value = content;
            ta.style.position = "fixed";
            ta.style.opacity = "0";
            document.body.appendChild(ta);
            ta.select();
            document.execCommand("copy");
            document.body.removeChild(ta);
        } catch (e) {
            console.warn("复制文本失败:", e);
            return;
        }
    }
    copiedMessageId.value = id;
    if (copyTimer !== null) clearTimeout(copyTimer);
    copyTimer = window.setTimeout(() => {
        copiedMessageId.value = null;
        copyTimer = null;
    }, 2000);
}

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
    | { id: string; type: "message"; timestamp: number; role: string; content: string; isHuman: boolean; anchorId?: string }
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
        runState?: AgentRunState;
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
        liveShellOutput?: { command: string; tail: string; elapsedMs: number; done: boolean } | null;
    }>(),
    {
        humanForm: null,
        pauseReason: "",
        isWaitingForHuman: false,
        isSendingHuman: false,
        finalArtifacts: () => [],
        liveShellOutput: null,
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
    "fork": [payload: { messageId?: string; timestampMs?: number; upToMessageId?: string; upToTimestampMs?: number; promptSeed?: string }];
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

function getStepActiveWorkers(step: any): AgentWorkerItem[] {
    const workers = props.teamState?.workers || [];
    if (!workers.length) return [];
    const stepJobId = step?.jobId || (step?.toolCall as any)?.id;
    if (stepJobId) {
        const matched = workers.filter(
            (w) => w.id === stepJobId || (w.extra as any)?.jobId === stepJobId || (w.extra as any)?.stepId === step?.id
        );
        if (matched.length > 0) return matched;
    }
    return workers;
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
        // PDF/Office 文档：经宿主转换管道（soffice → pdf → pdftoppm → 逐页 PNG）
        // 渲染为内联 HTML 直接上画布；转换链不可用则回退系统程序打开。
        const fmt = (artifact.format || "").toLowerCase().replace(/^\./, "");
        const convertibleExts = ["pdf", "pptx", "ppt", "docx", "doc", "xlsx", "xls", "odt", "ods", "odp", "rtf"];
        if (convertibleExts.includes(fmt) && artifact.localPath) {
            try {
                const res = await invoke<{ html?: string }>("render_artifact_preview", { path: artifact.localPath });
                if (res?.html) {
                    emit("open-preview", res.html);
                    return;
                }
            } catch {
                // 缺 LibreOffice/poppler 或转换失败 → 走系统打开兜底
            }
            if (artifact.artifactId) {
                await openArtifactById(artifact.artifactId);
            } else {
                await openArtifactInSystem(artifact.localPath);
            }
            return;
        }
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
const isLiveThoughtExpanded = ref(false);

function toggleLiveThoughtExpanded() {
    isLiveThoughtExpanded.value = !isLiveThoughtExpanded.value;
}

// 🌟 视图模式：'ambient' (环境感知/叙事简报，默认) | 'detailed' (完整工程链路)
const streamViewMode = ref<'ambient' | 'detailed'>((() => {
    try {
        const saved = localStorage.getItem('free_api_stream_view_mode');
        if (saved === 'ambient' || saved === 'detailed') return saved;
    } catch {}
    return 'ambient';
})());

function setStreamViewMode(mode: 'ambient' | 'detailed') {
    streamViewMode.value = mode;
    try {
        localStorage.setItem('free_api_stream_view_mode', mode);
    } catch {}
}

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
        return (liveThoughtInfo.value.text || preview.line3.text || "正在深入分析全局上下文与决策规划 …").trim();
    }
    return (preview.line3.text || "正在执行操作 …").trim();
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

// 🌟 换行感知与动力学平滑尾随滚动指令 (v-scroll-tail)
// 统一抽离至独立模块，支持常规平滑阻尼滑入、未结束被打断暴走冲刺到底部，以及换行垂直翻牌无缝衔接
import { vScrollTail } from "./scrollTailDirective";

// ── 🛠️ 步骤文件变更与计划摘要解析 ──
/**
 * Chip 点击：直接跳转到文件变更 (Diff)，不再回落到步骤详情
 */
function handleChipClick(_entry: PlanEntry | undefined, chip: FileChangeChip | undefined) {
    if (!chip) return;
    emit('open-diff', diffPayloadFromChip(chip));
}

/** 子步骤操作卡片点击：文件操作卡片直接看文件变更 Diff */
function handleActionCardClick(step: AgentStepItem) {
    const chip = getStepMiniChip(step);
    if (chip) {
        emit('open-diff', diffPayloadFromChip(chip));
        return;
    }
    const diff = extractDiffFromStep(step);
    if (diff) {
        emit('open-diff', diff);
        return;
    }
    emit('select-step', step.id);
}

// 🌟 终态交付 Markdown 丝滑流式打字渐显 (Typewriter Stream Reveal)
const typewriterTexts = reactive<Record<string, string>>({});
const typewriterDoneIds = new Set<string>();
const typewriterAnimFrames = new Map<string, number>();
const typewriterTargets = new Map<string, string>();

function updateSmoothStream(id: string, targetText: string) {
    typewriterTargets.set(id, targetText);
    if (typewriterDoneIds.has(id)) {
        typewriterTexts[id] = targetText;
        return;
    }
    if (typewriterAnimFrames.has(id)) {
        // 已经在运行 tick，目标文本已在 typewriterTargets 更新，tick 每帧会读取最新 target
        return;
    }

    if (typewriterTexts[id] === undefined) {
        typewriterTexts[id] = targetText.slice(0, Math.min(2, targetText.length));
    }

    function tick() {
        const target = typewriterTargets.get(id) || '';
        const current = typewriterTexts[id] || '';
        const remaining = target.length - current.length;

        if (remaining <= 0) {
            typewriterTexts[id] = target;
            typewriterAnimFrames.delete(id);
            if (!props.isRunning) {
                typewriterDoneIds.add(id);
            }
            return;
        }

        // 自适应帧级阻尼插值：
        // 1. 如果缓冲区积压很多（后端网络突发大 chunk），动态平滑提速追赶
        // 2. 如果积压平缓（普通字句输出），每帧推进 1-2 字（约 60-100 字/秒），如流水自然延展
        let step = 1;
        if (remaining > 50) {
            step = Math.max(3, Math.ceil(remaining / 6));
        } else if (remaining > 20) {
            step = Math.max(2, Math.ceil(remaining / 8));
        } else if (remaining > 6) {
            step = Math.max(1, Math.ceil(remaining / 10));
        } else {
            step = 1;
        }

        const nextLen = Math.min(target.length, current.length + step);
        typewriterTexts[id] = target.slice(0, nextLen);

        if (nextLen < target.length || props.isRunning) {
            const handle = requestAnimationFrame(tick);
            typewriterAnimFrames.set(id, handle);
        } else {
            typewriterAnimFrames.delete(id);
            typewriterDoneIds.add(id);
        }
    }

    const handle = requestAnimationFrame(tick);
    typewriterAnimFrames.set(id, handle);
}

function getCompletionDisplayText(entry: { id: string; summary?: string; title?: string; timestamp?: number }): string {
    const fullText = entry.summary || entry.title || '';
    if (!fullText) return '';

    // 历史非新鲜会话消息，瞬时渲染，不重复播放流式过渡
    const isFreshCompletion = entry.id.startsWith("history-completion-")
        && !!entry.timestamp && Date.now() - entry.timestamp < 15_000;
    const isHistoric = entry.id.startsWith('history-') && !isFreshCompletion;
    if (isHistoric || typewriterDoneIds.has(entry.id)) {
        typewriterDoneIds.add(entry.id);
        return fullText;
    }

    // 运行中（live-completion）或刚完成的回合，走帧级平滑自适应推进
    updateSmoothStream(entry.id, fullText);
    return typewriterTexts[entry.id] || fullText.slice(0, 1);
}

function isStreamingCompletion(entry: { id: string; summary?: string; title?: string }): boolean {
    if (typewriterDoneIds.has(entry.id)) return false;
    const target = typewriterTargets.get(entry.id) || entry.summary || entry.title || '';
    const current = typewriterTexts[entry.id] || '';
    // runState=finalizing 时模型已交付、后端在做校验/落库收尾——
    // 文本不会再长，摘掉流式外观，否则尾部淡字+光标看起来像"卡死"
    const liveStillGenerating = props.isRunning && entry.id === 'live-completion' && props.runState !== 'finalizing';
    return liveStillGenerating || current.length < target.length;
}

function finishTypewriterInstantly(id: string, fullText?: string) {
    if (typewriterAnimFrames.has(id)) {
        cancelAnimationFrame(typewriterAnimFrames.get(id)!);
        typewriterAnimFrames.delete(id);
    }
    typewriterDoneIds.add(id);
    if (fullText) {
        typewriterTexts[id] = fullText;
        typewriterTargets.set(id, fullText);
    }
}

/** 操作卡片悬浮提示 */
function actionCardTitle(step: AgentStepItem): string {
    const chip = getStepMiniChip(step);
    if (chip?.isIntercepted) return `${chip.path} · 写入已被门禁拦截（点击查看尝试内容）`;
    return '点击在右侧查看代码改动 Diff';
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

export interface AmbientActionDisplay {
    actionKind: 'read' | 'edit' | 'write' | 'shell' | 'search' | 'web' | 'mcp' | 'milestone' | 'subagent' | 'general';
    subagentTag?: string;
    capsuleTitle: string;
    fileName?: string;
    filePath?: string;
    iconKind?: string;
    iconEmoji?: string;
    capsuleBadge?: string;
    additions?: number;
    deletions?: number;
    isIntercepted?: boolean;
    hasDiff?: boolean;
    chip?: FileChangeChip;
    explanation: string;
    isWarning: boolean;
    isMilestone: boolean;
    tooltip: string;
}

export interface AmbientStepItemDisplay {
    type: 'step';
    id: string;
    step: AgentStepItem;
    action: AmbientActionDisplay;
}

export interface AmbientSearchGroupDisplay {
    type: 'search-group';
    id: string;
    steps: AgentStepItem[];
    count: number;
    summary: string;
    targetFiles: string[];
    querySample: string;
    durationBadge?: string;
    timestamp: number;
}

export interface AmbientMcpGroupDisplay {
    type: 'mcp-group';
    id: string;
    steps: AgentStepItem[];
    count: number;
    summary: string;
    durationBadge?: string;
    timestamp: number;
}

export interface AmbientNarrativeDisplay {
    type: 'narrative' | 'narrative-mini';
    id: string;
    text: string;
    timestamp?: number;
}

export type AmbientDisplayItem = AmbientStepItemDisplay | AmbientSearchGroupDisplay | AmbientMcpGroupDisplay | AmbientNarrativeDisplay;

const expandedSearchGroupIds = reactive(new Set<string>());

function toggleSearchGroup(groupId: string) {
    if (expandedSearchGroupIds.has(groupId)) {
        expandedSearchGroupIds.delete(groupId);
    } else {
        expandedSearchGroupIds.add(groupId);
    }
}

function isSearchGroupExpanded(groupId: string): boolean {
    return expandedSearchGroupIds.has(groupId);
}

const expandedMcpGroupIds = reactive(new Set<string>());

function toggleMcpGroup(groupId: string) {
    if (expandedMcpGroupIds.has(groupId)) {
        expandedMcpGroupIds.delete(groupId);
    } else {
        expandedMcpGroupIds.add(groupId);
    }
}

function isMcpGroupExpanded(groupId: string): boolean {
    return expandedMcpGroupIds.has(groupId);
}

// -----------------------------------------------------------------------------
// 🌟 环境感知动作流卡片折叠与展开 (契合多了隐藏、底部渐隐遮罩与居中展开小箭头)
// -----------------------------------------------------------------------------
const expandedAmbientStreamMap = ref<Record<string, boolean>>({});

function isAmbientStreamExpanded(entryId: string): boolean {
    return !!expandedAmbientStreamMap.value[entryId];
}

function toggleAmbientStreamExpand(entryId: string) {
    expandedAmbientStreamMap.value[entryId] = !expandedAmbientStreamMap.value[entryId];
}

function getAmbientStepCount(entry: PlanEntry): number {
    const items = getAmbientDisplayItems(entry);
    return items.filter(item => item.type !== 'narrative').length;
}

function shouldCollapseAmbientStream(entry: PlanEntry): boolean {
    // 1. 任务运行期间（没有运行结束）：绝不折叠，实时全量显示每一个步骤链路
    if (props.isRunning) return false;

    // 2. 若用户已手动展开，则不折叠
    if (isAmbientStreamExpanded(entry.id)) return false;

    // 3. 任务彻底运行完成之后，且步骤很长（> 4 项），才折叠并呈现渐隐遮罩
    return getAmbientStepCount(entry) > 4;
}

function shouldShowAmbientExpandBtn(entry: PlanEntry): boolean {
    // 运行期间保持全量展示，不显示折叠小箭头
    if (props.isRunning) return false;
    // 任务彻底运行完成之后且步骤很长，显示小箭头支持用户展开/收起
    return getAmbientStepCount(entry) > 4;
}

function getDirName(filePath?: string): string {
    if (!filePath) return '';
    const clean = filePath.replace(/\\/g, '/');
    const lastSlash = clean.lastIndexOf('/');
    if (lastSlash <= 0) return '';
    const dir = clean.substring(0, lastSlash);
    const parts = dir.split('/').filter(Boolean);
    if (parts.length === 0) return '';
    if (parts.length >= 2) {
        return parts.slice(-2).join('/') + '/';
    }
    return parts[parts.length - 1] + '/';
}

function buildSearchGroupSummary(steps: AgentStepItem[]): { summary: string; targetFiles: string[]; querySample: string } {
    const files = new Set<string>();
    const queryTerms: string[] = [];
    const explanations: string[] = [];

    for (const s of steps) {
        const op = formatStepOperation(s);
        if (op.fileName) files.add(op.fileName);
        let p: any = s.params || s.rawParams;
        if (typeof p === 'string') {
            try { p = JSON.parse(p); } catch {}
        }
        if (p && typeof p === 'object') {
            const q = String(p.query || p.pattern || p.Query || p.regex || '').trim();
            if (q && !queryTerms.includes(q)) queryTerms.push(q);
        }
        let desc = (s.description || '').replace(/^[💡🎯📌🚀⚠️⚡✏️💭❌]\s*/, '').trim();
        desc = desc.replace(/^(检索|搜索|查找|检查)\s*(代码|相关代码|历史日志)?\s*(以|为了|来|用于|：|:)?\s*/i, '').trim();
        if (desc && desc !== '执行操作' && !explanations.includes(desc)) {
            explanations.push(desc);
        }
    }

    const targetFiles = Array.from(files);
    let summary = '';
    if (explanations.length > 0) {
        summary = explanations[0];
        if (explanations.length > 1) {
            summary += ` 等 ${explanations.length} 处检索`;
        }
    } else if (queryTerms.length > 0) {
        summary = `检索 "${queryTerms.slice(0, 2).map(q => q.length > 16 ? q.slice(0, 14) + '…' : q).join('", "')}"${queryTerms.length > 2 ? ` 等 ${queryTerms.length} 处` : ''}`;
    } else {
        summary = '检索相关关联代码与门禁规则';
    }

    const querySample = queryTerms.slice(0, 3).join(', ');
    return { summary, targetFiles, querySample };
}

function buildMcpGroupSummary(steps: AgentStepItem[]): { summary: string } {
    const titles: string[] = [];
    const explanations: string[] = [];

    for (const s of steps) {
        const action = getAmbientAction(s);
        if (action.capsuleTitle && action.capsuleTitle !== 'MCP 工具' && action.capsuleTitle !== '操作' && !titles.includes(action.capsuleTitle)) {
            titles.push(action.capsuleTitle);
        }
        let desc = (s.description || '').replace(/^[💡🎯📌🚀⚠️⚡✏️💭❌]\s*/, '').trim();
        if (desc && desc !== '执行操作' && !explanations.includes(desc)) {
            explanations.push(desc);
        }
    }

    let summary = '';
    if (explanations.length > 0) {
        summary = explanations[0];
        if (steps.length > 1) {
            summary += ` 等 ${steps.length} 项操作`;
        }
    } else if (titles.length > 0) {
        summary = `${titles.slice(0, 2).join('、')} 等 ${steps.length} 项操作`;
    } else {
        summary = `执行 ${steps.length} 项 MCP 工具操作`;
    }
    return { summary };
}

function isMcpStep(step: AgentStepItem, op?: StepOperationDisplay): boolean {
    if (!step) return false;
    if (step.source === 'MCP') return true;
    if (op?.badgeIcon === '🧩' || op?.badgeLabel === 'MCP') return true;
    const t = (step.tool || '').toLowerCase();
    const act = (step.action || '').toLowerCase();
    if (t.startsWith('mcp_') || t.includes('/') || act.startsWith('mcp_') || act.includes('/')) return true;
    if (t.includes('worksheet') || t.includes('workbook') || t.includes('format_range') || t.includes('create_workbook') || t.includes('get_workbook')) return true;
    return false;
}

function isAmbientKeyStep(step: AgentStepItem): boolean {
    if (!step) return false;
    const tool = (step.tool || '').toLowerCase();
    const desc = (step.description || '').trim();

    // 1. 过滤内部调度与编排工具 (parallel, core, agent_core, wait_jobs, dispatch)
    if (tool === 'parallel' || tool === 'core' || tool === 'agent_core' || tool === 'wait_jobs' || tool === 'dispatch') {
        return false;
    }
    // 过滤流水账式内部心跳/调度描述
    if (desc === '执行下一步操作' || desc === '执行操作' || desc === '推进阶段目标') {
        if (tool === 'core' || tool === 'parallel' || !tool) return false;
    }

    // 2. 纯风险阻碍或执行异常保留展示
    if (step.status === 'failed' || step.status === 'challenged' || step.findingKind === 'warning' || step.findingKind === 'blocker') {
        return true;
    }

    // 3. 常规阶段结论/思考/计划不作为动作行展示 (由顶层 narrative 叙事段落统领，避免重复报账)
    if (!tool || tool === 'thought' || tool === 'thinking' || tool === 'plan' || tool === 'milestone' || tool === 'summary') {
        return false;
    }

    // 4. 有文件操作 (修改、新建、读取、Diff)
    if (hasFileOperation(step)) return true;

    // 5. 其余产生动作行为的步骤 (MCP / 终端 / 网页等)
    return true;
}

// 阶段解说段落：thought 作为面向用户的每轮叙述（区别于 description 一句话进度标签）
function stepNarrative(step: AgentStepItem): string {
    const narr = (step.thought || '').trim();
    if (!narr) return '';
    const desc = (step.description || '').trim();
    if (narr === desc) return '';
    return narr;
}

function getAmbientDisplayItems(entry: PlanEntry): AmbientDisplayItem[] {
    const rawSteps = entry.steps || [];
    const items: AmbientDisplayItem[] = [];

    let i = 0;
    let lastNarrative = '';

    while (i < rawSteps.length) {
        const step = rawSteps[i];

        // 互斥兜底：本步 finding 将渲染时，thought 解说不再单独渲染（防同一步两段大白话）
        const finding = (step.finding || '').trim();
        const findingRenderable = !!finding && (step.findingKind === 'breakthrough' || step.findingKind === 'milestone' || step.findingKind === 'summary' || step.findingKind === 'start');

        // 🌟 阶段解说与持续思考流：允许呈现不同阶段思考，解除单次置位封锁，配合 trim 去除微小扰动重复
        const narr = findingRenderable ? '' : stepNarrative(step);
        const cleanNarr = narr.trim();
        if (cleanNarr && cleanNarr !== lastNarrative) {
            lastNarrative = cleanNarr;
            items.push({ type: 'narrative', id: `narr-${step.id}`, text: cleanNarr, timestamp: step.timestamp });
        }

        // 🌟 用户需求：关键突破/阶段总结/突破发现直接与开头总结一样展示为纯文本叙事段落，不加特效卡片与小标签
        if (findingRenderable) {
            const cleanFinding = finding.replace(/^[💡🎯📌🚀⚠️⚡✏️💭❌]\s*/, '').trim();
            if (cleanFinding && cleanFinding !== lastNarrative) {
                lastNarrative = cleanFinding;
                items.push({ type: 'narrative', id: `finding-${step.id}`, text: cleanFinding, timestamp: step.timestamp });
            }
        }

        if (!isAmbientKeyStep(step)) {
            i++;
            continue;
        }
        const action = getAmbientAction(step);

        // 检索微折叠：只合并"无叙述"的连续检索——带解说的检索步自成一节，不进折叠组
        if (action.actionKind === 'search') {
            const searchSteps: AgentStepItem[] = [step];
            let j = i + 1;
            while (j < rawSteps.length) {
                const nextStep = rawSteps[j];
                if (!isAmbientKeyStep(nextStep)) {
                    if (stepNarrative(nextStep)) break;
                    j++;
                    continue;
                }
                if (getAmbientAction(nextStep).actionKind === 'search' && !stepNarrative(nextStep)) {
                    searchSteps.push(nextStep);
                    j++;
                } else {
                    break;
                }
            }

            // 连续检索 >= 2 条，聚合成微折叠组
            if (searchSteps.length >= 2) {
                const groupId = `search-group-${entry.id}-${step.id}-${searchSteps.length}`;
                const { summary, targetFiles, querySample } = buildSearchGroupSummary(searchSteps);

                let totalDurationMs = 0;
                for (const s of searchSteps) {
                    if (s.metrics?.toolDurationMs) totalDurationMs += s.metrics.toolDurationMs;
                }

                items.push({
                    type: 'search-group',
                    id: groupId,
                    steps: searchSteps,
                    count: searchSteps.length,
                    summary,
                    targetFiles,
                    querySample,
                    durationBadge: totalDurationMs > 0 ? formatDurationSeconds(totalDurationMs) : undefined,
                    timestamp: searchSteps[searchSteps.length - 1].timestamp || step.timestamp,
                });
                i = j;
                continue;
            }
        }

        // 连续 MCP 工具微折叠：将连续 >= 2 个无 narrative 的 MCP 操作汇聚收拢
        if (action.actionKind === 'mcp') {
            const mcpSteps: AgentStepItem[] = [step];
            let j = i + 1;
            while (j < rawSteps.length) {
                const nextStep = rawSteps[j];
                if (!isAmbientKeyStep(nextStep)) {
                    if (stepNarrative(nextStep)) break;
                    j++;
                    continue;
                }
                if (getAmbientAction(nextStep).actionKind === 'mcp' && !stepNarrative(nextStep)) {
                    mcpSteps.push(nextStep);
                    j++;
                } else {
                    break;
                }
            }

            if (mcpSteps.length >= 2) {
                const groupId = `mcp-group-${entry.id}-${step.id}-${mcpSteps.length}`;
                const { summary } = buildMcpGroupSummary(mcpSteps);

                let totalDurationMs = 0;
                for (const s of mcpSteps) {
                    if (s.metrics?.toolDurationMs) totalDurationMs += s.metrics.toolDurationMs;
                }

                items.push({
                    type: 'mcp-group',
                    id: groupId,
                    steps: mcpSteps,
                    count: mcpSteps.length,
                    summary,
                    durationBadge: totalDurationMs > 0 ? formatDurationSeconds(totalDurationMs) : undefined,
                    timestamp: mcpSteps[mcpSteps.length - 1].timestamp || step.timestamp,
                });
                i = j;
                continue;
            }
        }

        // 单个动作行
        items.push({
            type: 'step',
            id: step.id,
            step,
            action,
        });
        i++;
    }

    return items;
}

const ambientActionMemo = new WeakMap<object, AmbientActionDisplay>();

function getAmbientAction(step: AgentStepItem): AmbientActionDisplay {
    if (!step) {
        return {
            actionKind: 'general',
            capsuleTitle: '执行操作',
            explanation: '',
            isWarning: false,
            isMilestone: false,
            tooltip: '',
        };
    }
    const hit = ambientActionMemo.get(step);
    if (hit) return hit;
    const action = computeAmbientAction(step);
    // 步骤内部思考/重试原因保留挂载在 tooltip 中，悬停即可查看完整详情
    const thought = (step.thought || '').trim();
    if (thought && thought !== action.explanation && !action.tooltip?.includes(thought)) {
        action.tooltip = action.tooltip ? `${action.tooltip} · ${thought}` : thought;
    }
    try { ambientActionMemo.set(step, action); } catch {}
    return action;
}

function computeAmbientAction(step: AgentStepItem): AmbientActionDisplay {
    const chip = getStepMiniChip(step);
    const op = formatStepOperation(step);
    const desc = (step.description || '').trim();
    const explicitFinding = (step.finding || '').trim();
    const kind = step.findingKind;
    const toolLower = (step.tool || '').toLowerCase();
    const actionLower = (step.action || '').toLowerCase();

    // 1. 风险阻碍预警 (仅保留真正异常与阻塞情况的醒目预警；常规阶段结论统领于顶层叙事段落中，不生成冗余动作条)
    if (kind === 'warning' || kind === 'blocker') {
        let explanation = explicitFinding || desc;
        explanation = explanation.replace(/^[💡🎯📌🚀⚠️⚡✏️💭❌]\s*/, '').trim();

        return {
            actionKind: 'general',
            capsuleTitle: '风险预警',
            explanation,
            isWarning: true,
            isMilestone: false,
            tooltip: '风险预警 · 点击查看详情',
        };
    }

    // 2. 子智能体审查与独立子任务 (Reviewer / Subagent / Child Worker / Child Writer)
    // 🌟 核心保真：子审查拥有专属徽标与自身标识符号，绝不可误标为"编辑"或笼统"操作"
    const isSubAgent = isSubAgentTool(step.tool) || isSubAgentTool(step.action) ||
        op.badgeLabel === '子审查' || op.badgeLabel === '子代理' || op.badgeLabel === '子写入' || op.badgeLabel === '追问对质' ||
        toolLower.includes('reviewer') || actionLower.includes('reviewer');

    if (isSubAgent) {
        const isReviewer = toolLower.includes('reviewer') || actionLower.includes('reviewer') || op.badgeLabel === '子审查';
        const isWriter = toolLower.includes('writer') || actionLower.includes('writer') || op.badgeLabel === '子写入';
        const isResume = toolLower.includes('resume') || op.badgeLabel === '追问对质';

        let subagentTag = '🤖 子代理';
        if (isReviewer) {
            subagentTag = '🤖 子审查';
        } else if (isWriter) {
            subagentTag = '✍️ 子写入';
        } else if (isResume) {
            subagentTag = '🔄 追问对质';
        } else {
            const roleBadge = getSubAgentBadgeText(step);
            if (roleBadge && roleBadge !== '子任务' && roleBadge.length <= 10) {
                subagentTag = `🤖 ${roleBadge}`;
            }
        }

        const isChallenged = step.status === 'challenged';
        const isFailed = step.status === 'failed';
        const isWarning = isChallenged || isFailed;

        let explanation = explicitFinding || (desc && desc !== '执行操作' ? desc : '') || op.actionText || extractSubagentTask(step);
        explanation = explanation.replace(/^[💡🎯📌🚀⚠️⚡✏️💭❌]\s*/, '').trim();
        explanation = explanation.replace(/^(子审查|子代理|审查|子任务|协同任务)[：:\s]*/i, '').trim();

        const fileName = chip?.fileName || op.fileName;
        // 如果没有特定目标文件，胶囊标题留空，避免和 subagentTag 重复显示
        const capsuleTitle = fileName || '';

        return {
            actionKind: 'subagent',
            subagentTag,
            capsuleTitle,
            fileName,
            filePath: op.filePath || chip?.path,
            iconKind: fileName ? 'code' : (isReviewer ? 'reviewer' : undefined),
            iconEmoji: isReviewer ? '🤖' : undefined,
            isIntercepted: isChallenged,
            isWarning,
            isMilestone: false,
            chip,
            additions: chip?.additions,
            deletions: chip?.deletions,
            hasDiff: !!(chip && ((chip.additions || 0) > 0 || (chip.deletions || 0) > 0)),
            explanation,
            tooltip: isChallenged ? '子审查提出质疑 / 门禁未闭合' : (isFailed ? '子任务执行失败' : (isReviewer ? '子智能体协同审查' : '子代理协同任务')),
        };
    }

    // 3. 异步挂起等待 (Wait Jobs)
    if (isWaitJobsTool(step.tool) || isWaitJobsTool(step.action)) {
        return {
            actionKind: 'subagent',
            subagentTag: '⏳ 协同等待',
            capsuleTitle: '',
            explanation: desc && desc !== '执行操作' ? desc : op.actionText,
            isWarning: step.status === 'failed',
            isMilestone: false,
            tooltip: '等待后台子任务完成',
        };
    }

    // 4. 发生错误 / 门禁拦截 / 挑战失败 (非子代理的常规工具执行失败或拦截)
    if (step.status === 'failed' || step.status === 'challenged' || chip?.isIntercepted) {
        const fileName = chip?.fileName || op.fileName;
        const isInter = chip?.isIntercepted || step.status === 'challenged';

        // 🌟 杜绝误标记：只有真正发生代码修改时才为 'edit'，其他工具保持自身语义或 general
        let actualKind: AmbientActionDisplay['actionKind'] = 'general';
        if (chip && ((chip.additions || 0) > 0 || (chip.deletions || 0) > 0)) {
            actualKind = 'edit';
        } else if (toolLower.includes('shell') || toolLower.includes('bash') || toolLower.includes('command')) {
            actualKind = 'shell';
        } else if (toolLower.includes('search') || toolLower.includes('grep')) {
            actualKind = 'search';
        } else if (isMcpStep(step, op)) {
            actualKind = 'mcp';
        } else if (toolLower.includes('read') || toolLower.includes('view')) {
            actualKind = 'read';
        } else if (toolLower.includes('write') || toolLower.includes('create')) {
            actualKind = 'write';
        }

        return {
            actionKind: actualKind,
            capsuleTitle: fileName || op.badgeLabel || (isInter ? '门禁拦截' : '步骤异常'),
            fileName,
            filePath: op.filePath || chip?.path,
            iconKind: fileName ? 'code' : (actualKind === 'shell' ? 'terminal' : undefined),
            isIntercepted: isInter,
            isWarning: true,
            isMilestone: false,
            chip,
            explanation: (desc || (isInter ? '已被门禁规则安全拦截' : '执行遇到异常或阻碍')).replace(/^[💡🎯📌🚀⚠️⚡✏️💭❌]\s*/, '').trim(),
            tooltip: isInter ? '操作已被安全策略拦截' : '步骤执行失败',
        };
    }

    // 3. 代码修改 (Edit / Diff)
    if (chip && ((chip.additions || 0) > 0 || (chip.deletions || 0) > 0)) {
        const fileName = chip.fileName || op.fileName || '代码变更';
        let reason = (step.explanation || desc).trim();
        const escapedFn = fileName.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
        // 去除开头重复的 "修改/编辑/写入 xxx (以/为了/来/用于/关于/针对)"
        reason = reason.replace(new RegExp(`^(修改|编辑|写入|更新|重构|调整)\\s*(${escapedFn}|[^\\s，,以为了]+)?\\s*(\\d+[-~到至]\\d+\\s*行)?\\s*(以|为了|来|用于|关于|针对|：|:)?\\s*`, 'i'), '').trim();
        reason = reason.replace(/^[💡🎯📌🚀⚠️⚡✏️💭❌]\s*/, '').trim();
        if (reason === fileName || reason === '执行操作') reason = '';

        return {
            actionKind: 'edit',
            capsuleTitle: fileName,
            fileName,
            filePath: op.filePath || chip?.path,
            iconKind: 'code',
            additions: chip.additions,
            deletions: chip.deletions,
            hasDiff: true,
            chip,
            capsuleBadge: op.lineRange || undefined,
            explanation: reason,
            isWarning: false,
            isMilestone: false,
            tooltip: actionCardTitle(step),
        };
    }

    // 4. 新建 / 写入文件 (Write / Create)
    if (op.badgeLabel === '写入' || toolLower.includes('write') || toolLower.includes('create_file') || actionLower.startsWith('write')) {
        const fileName = op.fileName || chip?.fileName || '新文件';
        let reason = (step.explanation || desc).trim();
        const escapedFn = fileName.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
        reason = reason.replace(new RegExp(`^(写入|新建|创建)\\s*(${escapedFn}|[^\\s，,以为了]+)?\\s*(\\d+[-~到至]\\d+\\s*行)?\\s*(以|为了|来|用于|关于|针对|：|:)?\\s*`, 'i'), '').trim();
        reason = reason.replace(/^[💡🎯📌🚀⚠️⚡✏️💭❌]\s*/, '').trim();
        if (reason === fileName || reason === '写入文件' || reason === '执行操作') reason = '';

        return {
            actionKind: 'write',
            capsuleTitle: fileName,
            fileName,
            filePath: op.filePath || chip?.path,
            iconKind: 'create',
            capsuleBadge: op.diffStat?.additions ? `+${op.diffStat.additions}行` : undefined,
            additions: op.diffStat?.additions,
            hasDiff: op.hasDiff,
            chip,
            explanation: reason,
            isWarning: false,
            isMilestone: false,
            tooltip: op.filePath ? `写入 ${op.filePath}` : '新建/写入文件',
        };
    }

    // 5. 查看 / 读取文件 (Read)
    if (toolLower.includes('read') || toolLower.includes('view') || toolLower.includes('cat') || op.badgeLabel === '读取') {
        const fileName = op.fileName || '文件内容';
        let reason = (step.explanation || desc).trim();
        const escapedFn = fileName.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
        reason = reason.replace(new RegExp(`^(读取|查看|检查|分析|浏览)\\s*(${escapedFn}|[^\\s，,以为了]+)?\\s*(\\d+[-~到至]\\d+\\s*行)?\\s*(以|为了|来|用于|关于|针对|：|:)?\\s*`, 'i'), '').trim();
        reason = reason.replace(/^[💡🎯📌🚀⚠️⚡✏️💭❌]\s*/, '').trim();
        if (reason === fileName || reason === '读取文件内容' || reason === '执行操作') reason = '';

        return {
            actionKind: 'read',
            capsuleTitle: fileName,
            fileName,
            filePath: op.filePath || chip?.path,
            iconKind: 'read',
            capsuleBadge: op.lineRange || undefined,
            explanation: reason,
            isWarning: false,
            isMilestone: false,
            tooltip: op.filePath ? `读取 ${op.filePath} ${op.lineRange || ''}` : '读取文件',
        };
    }

    // 6. 终端命令与脚本 (Shell / Command)
    if (toolLower.includes('shell') || toolLower.includes('bash') || toolLower.includes('command') || toolLower.includes('exec') || toolLower.includes('terminal') || op.badgeLabel === '终端') {
        let cmd = '';
        const p: any = step.params || step.rawParams;
        if (p && typeof p === 'object') {
            cmd = String(p.CommandLine || p.command || p.cmd || p.script || '').trim();
        } else if (typeof p === 'string') {
            try {
                const parsed = JSON.parse(p);
                cmd = String(parsed.CommandLine || parsed.command || parsed.cmd || parsed.script || '').trim();
            } catch {
                cmd = p.trim();
            }
        }
        if (!cmd) cmd = op.actionText || '终端命令';
        const shortCmd = cmd.length > 36 ? cmd.slice(0, 33) + '…' : cmd;

        let durationBadge = '';
        if (step.metrics?.toolDurationMs != null) {
            durationBadge = formatDurationSeconds(step.metrics.toolDurationMs);
        }

        let explanation = desc && desc !== '执行操作' ? desc : '';
        explanation = explanation.replace(/^[💡🎯📌🚀⚠️⚡✏️💭❌]\s*/, '').trim();
        explanation = explanation.replace(/^(执行|运行|调用)\s*(终端命令|命令|shell)?\s*(以|为了|来|用于|关于|针对|：|:)?\\s*/i, '').trim();

        return {
            actionKind: 'shell',
            capsuleTitle: shortCmd,
            filePath: op.filePath,
            iconKind: 'terminal',
            capsuleBadge: durationBadge || undefined,
            explanation,
            isWarning: false,
            isMilestone: false,
            tooltip: cmd ? `执行: ${cmd}` : '执行终端命令',
        };
    }

    // 7. 代码检索 / 全局搜索 (Search / Grep)
    if (toolLower.includes('search') || toolLower.includes('grep') || toolLower.includes('find') || toolLower.includes('ripgrep') || op.badgeLabel?.includes('检索')) {
        let pattern = '';
        const p: any = step.params || step.rawParams;
        if (p && typeof p === 'object') {
            pattern = String(p.query || p.pattern || p.Query || p.regex || '').trim();
        }
        // 🌟 核心降噪：胶囊标题优先目标文件，若无文件则展示简短关键词或"代码定义"
        const fileName = op.fileName;
        const capsuleTitle = fileName || (pattern ? (pattern.length > 20 ? pattern.slice(0, 18) + '…' : pattern) : '代码定义');
        
        let explanation = desc && desc !== '执行操作' ? desc : (pattern ? `检索 "${pattern.length > 32 ? pattern.slice(0, 29) + '…' : pattern}"` : '检索相关代码');
        explanation = explanation.replace(/^[💡🎯📌🚀⚠️⚡✏️💭❌]\s*/, '').trim();
        explanation = explanation.replace(/^(检索|搜索|查找|检查)\s*(代码|相关代码|历史日志)?\s*(以|为了|来|用于|关于|针对|：|:)?\\s*/i, '').trim();

        return {
            actionKind: 'search',
            capsuleTitle,
            fileName,
            filePath: op.filePath,
            iconKind: fileName ? 'code' : 'search',
            capsuleBadge: undefined,
            explanation,
            isWarning: false,
            isMilestone: false,
            tooltip: pattern ? `检索: ${pattern}` : (op.actionText || '检索代码'),
        };
    }

    // 8. 全网搜索与网页抓取 (Web Search / Web Fetch)
    if (toolLower.includes('web_search') || toolLower.includes('web_fetch') || toolLower.includes('duckduckgo') || toolLower.includes('bing') || op.badgeLabel?.includes('全网')) {
        let query = '';
        const p: any = step.params || step.rawParams;
        if (p && typeof p === 'object') {
            query = String(p.query || p.q || p.url || p.Url || '').trim();
        }
        const isFetch = toolLower.includes('fetch');
        const capsuleTitle = isFetch ? '网页抓取' : '全网检索';
        let explanation = desc && desc !== '执行操作' ? desc : (query ? `查询 "${query.length > 35 ? query.slice(0, 32) + '…' : query}"` : '');
        explanation = explanation.replace(/^[💡🎯📌🚀⚠️⚡✏️💭❌]\s*/, '').trim();

        return {
            actionKind: 'web',
            capsuleTitle,
            iconEmoji: isFetch ? '🌍' : '🌐',
            explanation,
            isWarning: false,
            isMilestone: false,
            tooltip: query || op.actionText,
        };
    }

    // 9. MCP 工具调用
    if (isMcpStep(step, op)) {
        const title = op.badgeLabel || step.tool || 'MCP 工具';
        let explanation = desc && desc !== '执行操作' ? desc : op.actionText;
        explanation = explanation.replace(/^[💡🎯📌🚀⚠️⚡✏️💭❌]\s*/, '').trim();

        let durationBadge = '';
        if (step.metrics?.toolDurationMs != null) {
            durationBadge = formatDurationSeconds(step.metrics.toolDurationMs);
        }

        return {
            actionKind: 'mcp',
            capsuleTitle: title,
            iconEmoji: '🧩',
            capsuleBadge: durationBadge || undefined,
            explanation,
            isWarning: false,
            isMilestone: false,
            tooltip: `调用 MCP 工具 · ${op.actionText}`,
        };
    }

    // 10. 常规兜底动作
    let text = desc && desc !== '执行操作' ? desc : op.actionText || '执行操作';
    text = text.replace(/^[💡🎯📌🚀⚠️⚡✏️💭❌]\s*/, '').trim();
    return {
        actionKind: 'general',
        capsuleTitle: op.badgeLabel || step.tool || '操作',
        iconEmoji: '⚡',
        explanation: text,
        isWarning: false,
        isMilestone: false,
        tooltip: op.actionText || '查看详情',
    };
}

function handleAmbientChipClick(step: AgentStepItem, action: AmbientActionDisplay) {
    if (action.chip) {
        emit('open-diff', diffPayloadFromChip(action.chip));
        return;
    }
    if (action.hasDiff) {
        const diff = extractDiffFromStep(step);
        if (diff) {
            emit('open-diff', diff);
            return;
        }
    }
    if (action.actionKind === 'subagent') {
        emit('open-team-deck', step.id);
        return;
    }
    emit('select-step', step.id);
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
        return props.thought ? `✦ ${props.thought.slice(0, 80)}...` : '✦ 正在深入思考与规划下一步行动…';
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
let thoughtScrollRaf: number | null = null;
let thoughtScrollRetries = 0;
// 用户手动上翻过的思考面板：脱离自动追尾，滚回底部附近时恢复
// 判定用真实手势（wheel/touchmove），不用 scroll 事件——程序性
// scrollTop 写入也触发 scroll，且新内容到达时距底距离会瞬时拉大，会误伤
const detachedThoughtPanes = new WeakSet<HTMLElement>();
function paneFromEvent(event: Event): HTMLElement | null {
    const el = event.target as HTMLElement;
    if (!(el instanceof HTMLElement)) return null;
    return el.matches(".active-thought-content, .thought-expanded-body") ? el : null;
}
function onPaneWheelCapture(event: Event) {
    const el = paneFromEvent(event);
    if (el && (event as WheelEvent).deltaY < 0) detachedThoughtPanes.add(el); // 上滚=主动离开底部
}
function onPaneTouchCapture(event: Event) {
    const el = paneFromEvent(event);
    if (!el) return;
    if (el.scrollHeight - el.clientHeight - el.scrollTop > 12) detachedThoughtPanes.add(el);
}
function onPaneScrollCapture(event: Event) {
    const el = paneFromEvent(event);
    if (!el) return;
    if (el.scrollHeight - el.clientHeight - el.scrollTop < 12) detachedThoughtPanes.delete(el); // 回底→恢复
}
onMounted(() => {
    streamElement.value?.addEventListener("wheel", onPaneWheelCapture, { capture: true, passive: true });
    streamElement.value?.addEventListener("touchmove", onPaneTouchCapture, { capture: true, passive: true });
    streamElement.value?.addEventListener("scroll", onPaneScrollCapture, true);
});
onBeforeUnmount(() => {
    streamElement.value?.removeEventListener("wheel", onPaneWheelCapture, true);
    streamElement.value?.removeEventListener("touchmove", onPaneTouchCapture, true);
    streamElement.value?.removeEventListener("scroll", onPaneScrollCapture, true);
    if (thoughtScrollRaf !== null) { cancelAnimationFrame(thoughtScrollRaf); thoughtScrollRaf = null; }
});
function followThoughtTail() {
    // 已有一个滚动动画在跑就不用再起——tick 每帧会重新读 scrollHeight，
    // 新内容来了自动把剩余距离拉大 → 步长自动变大，天然"追不上就加速"
    if (thoughtScrollRaf !== null) return;
    const tick = () => {
        // 折叠态的 .active-thought-content 与展开态的 .thought-expanded-body
        // 都可能存在，一起追底（展开面板此前没人滚，一直钉在第一行）
        const els = streamElement.value?.querySelectorAll<HTMLElement>(
            ".active-thought-content, .thought-expanded-body"
        );
        if (!els || els.length === 0) {
            // 思考面板还没挂载（条目晚于内容到达）：短窗口内重试几帧
            if (thoughtScrollRetries++ < 10) {
                thoughtScrollRaf = requestAnimationFrame(tick);
            } else {
                thoughtScrollRaf = null;
                thoughtScrollRetries = 0;
            }
            return;
        }
        thoughtScrollRetries = 0;
        let settled = true;
        els.forEach((el) => {
            // 用户在该面板里手动上翻后松手——不打断阅读，回到底部附近再恢复追尾
            if (detachedThoughtPanes.has(el)) return;
            const target = el.scrollHeight - el.clientHeight;
            const remaining = target - el.scrollTop;
            if (remaining <= 1) {
                if (el.scrollTop !== target) el.scrollTop = target;
                return;
            }
            settled = false;
            // 限速追赶滚：阻尼比例让近处缓行，封顶 ~26px/帧 保证肉眼
            // 看得见滚动过程；积压极大时才允许提速，但绝不瞬移
            let step = Math.ceil(remaining * 0.14);
            if (remaining > 600) step = Math.ceil(remaining * 0.45);
            step = Math.min(step, 26);
            if (step < 3) step = 3;
            if (step > remaining) step = remaining;
            el.scrollTop += step;
        });
        thoughtScrollRaf = settled ? null : requestAnimationFrame(tick);
    };
    thoughtScrollRaf = requestAnimationFrame(tick);
}
const liveTermBodyRef = ref<HTMLElement | null>(null);
// 小框位于阶段 v-for 内：字符串 ref 会被 Vue 收成数组，用函数 ref 取单元素
function setLiveTermBodyRef(el: unknown) {
    liveTermBodyRef.value = el instanceof HTMLElement ? el : null;
}
let liveTermFollowBottom = true;
function onLiveTermScroll() {
    const el = liveTermBodyRef.value;
    if (!el) return;
    liveTermFollowBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
}
watch(
    () => props.liveShellOutput?.tail,
    async (tail, prev) => {
        if (tail === undefined) {
            liveTermFollowBottom = true;
            return;
        }
        if (prev === undefined) liveTermFollowBottom = true;
        await nextTick();
        const el = liveTermBodyRef.value;
        if (el && liveTermFollowBottom) el.scrollTop = el.scrollHeight;
    },
    { flush: "post" }
);
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
        if (props.isRunning) {
            // 思考窗格始终自追尾——外层向上翻历史不该把它钉在第一行
            followThoughtTail();
            if (followLiveTail.value) followLatest();
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
    const rawPlans: PlanEntry[] = [];
    let currentPlan: (PlanEntry & { _key: string }) | null = null;
    let planIndex = 0;

    for (const step of steps.filter((item) => Boolean(item.description || item.tool || item.action))) {
        const key = step.planId || "plan-default";
        // 🌟 核心修复：按时间轴顺序连续分段聚合！
        // 只有连续相邻且相同 planId 的步骤才合并在同一阶段。
        // 若 planId 发生切换（例如：排查 -> 准备环境 -> 排查），在时间轴上必然是新的后续阶段，
        // 绝不能使用全局 Map 跨越时间倒流合并到之前的旧阶段中，否则会导致：
        // 1. 中间阶段（如"工作区准备环境"）被永久固定在底部；
        // 2. 后续执行的新步骤被逆向追加到上方的旧阶段中，颠倒时间线与视觉顺序；
        // 3. 运行状态错误：已结束的历史阶段被标为 running，而最新正在运行的阶段被标为 succeeded。
        if (currentPlan && currentPlan._key === key) {
            currentPlan.steps.push(step);
            currentPlan.timestamp = Math.min(currentPlan.timestamp, step.timestamp);
            if (step.planTitle) currentPlan.title = step.planTitle;
        } else {
            planIndex++;
            const paddedIndex = String(planIndex).padStart(4, "0");
            currentPlan = {
                _key: key,
                id: `${prefix}-plan-${paddedIndex}-${key}`,
                type: "plan" as const,
                timestamp: step.timestamp,
                title: step.planTitle || "执行任务",
                status: step.status,
                steps: [step],
                isThinking: false,
            };
            rawPlans.push(currentPlan);
        }
    }
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
        assignInterventionAnchors(middle);
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
    assignInterventionAnchors(middle);
    timeline.push(...middle);
    if ((props.isThinking || Boolean(props.thought) || props.isRunning) && !plans.length) {
        timeline.push({ id: "active-thought", type: "active-thought", timestamp: activeThoughtStartedAt.value || Date.now() });
    }
    const isCompletedOrSettled = !props.isRunning || Boolean(props.completionMessage);
    // 真流式：finish 的 summary 边生成边推流，一有水就挂 live-completion
    // 边生成边渲染——不必等回合收尾才整段出现
    const summaryStreamingIn = props.isRunning && Boolean(props.finalSummary?.trim());
    if ((isCompletedOrSettled || summaryStreamingIn) && (props.completionMessage || props.finalSummary || props.finalArtifacts.length)) {
        timeline.push({ id: "live-completion", type: "completion", timestamp: Date.now(), title: props.completionMessage || (props.hasError ? "任务未能完成" : "任务已完成"), summary: props.finalSummary || props.completionMessage, artifacts: props.finalArtifacts, failed: props.hasError });
    }
    return timeline;
}

const timelineEntries = computed<TimelineEntry[]>(() => [...historyTimelineEntries.value, ...currentTimeline()]);
function statusLabel(status: AgentToolStatus) { return { pending: "等待", running: "执行中", succeeded: "完成", failed: "失败", challenged: "验证未闭合" }[status] || "未知"; }
function isSubAgentTool(tool?: string): boolean {
    const t = (tool || "").toLowerCase();
    return (
        t.includes("child_reviewer") ||
        t.includes("child_writer") ||
        t.includes("child_worker") ||
        t.includes("sub_agent") ||
        t.includes("invoke_subagent") ||
        t.includes("reviewer")
    );
}
// 运行中的子 agent 步骤 → 匹配活跃 worker 的实时动作行
// 痛点修复：以前"派发审查"步骤只显示死文案"后台协同中"，
// 审查员在干嘛完全看不见。worker.currentStep 逐事件更新、
// thought 逐 chunk 追加——取它们合成一行活文本
const workerLiveLines = computed(() => {
    const map = new Map<string, { key: string; text: string }>();
    const workers = props.teamState?.workers || [];
    const running = workers.filter(w => w.status === 'running');
    if (!running.length) return map;
    for (const step of props.steps) {
        if (step.status !== 'running') continue;
        const isSub = isSubAgentTool(step.tool);
        const isWait = isWaitJobsTool(step.tool);
        if (!isSub && !isWait) continue;
        const tool = (step.tool || '').toLowerCase();
        const wantReviewer = isSub && (tool.includes('reviewer'));
        const candidates = running.filter(w => isWait
            ? true
            : wantReviewer ? w.role === 'reviewer' : w.role !== 'reviewer');
        // 并发多个同类 worker 时取最新启动的——该步骤最可能的执行者
        const w = candidates.sort((a, b) => b.startedAt - a.startedAt)[0];
        if (!w) continue;
        const actionText = (w.currentStep || '').trim();
        const lineInfo = extractActiveThoughtLineInfo(w.thought || '', 120);
        const generic = /^(正在审查|正在后台|正在执行)/.test(actionText);
        const text = (!generic && actionText) ? actionText : (lineInfo.text || actionText || '正在后台执行…');
        map.set(step.id, {
            key: `${w.id}-s${w.steps?.length ?? 0}-l${lineInfo.lineKey}`,
            text: `${w.roleLabel || w.name}：${text}`,
        });
    }
    return map;
});

function getSubAgentBadgeText(step: any): string {
    const tool = (step?.tool || step?.action || "").toLowerCase();
    const isReviewer = tool.includes("child_reviewer") || tool.includes("reviewer");
    const isWriter = tool.includes("child_writer");
    const isWorker = tool.includes("child_worker") || tool.includes("sub_agent");
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
        if (roleLabel.length <= 10) {
            return `${roleLabel} · ${defaultKindLabel}`;
        }
        return roleLabel;
    }

    return isReviewer ? "子审查" : isWriter ? "子写入" : "子代理";
}
function isWaitJobsTool(tool?: string): boolean {
    const t = (tool || "").toLowerCase();
    return t.includes("wait_jobs") || t.includes("wait_job");
}
// wait_jobs 瓷砖文案按真实结果区分：空超时（无新完成）≠ 回收成功。
// 此前完成态一律显示"已唤醒并回收全部结论"，把满屏空等待渲染成了成功假象。
function waitJobsTileText(step: AgentStepItem): string {
    if (step.status === "running") return "⏳ 当前协程已反应式挂起等待后台通知，0 Token 0 CPU 消耗…";
    const out = [step.result, step.rawResult].map((v) => (typeof v === "string" ? v : "")).join(" ");
    if (out.includes("无新完成")) return "⏱️ 等待窗口超时——任务仍在后台运行（本窗口未收到新结论）";
    if (out.includes("尚未回收")) return "📬 后台任务已终态，结论待回收——请再次 wait_jobs 收取";
    if (out.includes("消费完毕")) return "☑️ 全部结论已交付消费，无挂起任务";
    if (out.includes("没有已派发")) return "ℹ️ 当前无已派发后台任务";
    return "✅ 后台任务已完成，已唤醒并回收全部结论";
}
function formatConversationTime(value: string) { const date = new Date(value); return Number.isNaN(date.getTime()) ? "" : date.toLocaleString("zh-CN", { hour: "2-digit", minute: "2-digit", month: "numeric", day: "numeric" }); }
watch(() => props.historyTurns, (turns) => {
    if (Array.isArray(turns)) {
        for (const turn of turns) {
            typewriterDoneIds.add(`history-completion-${turn.id}`);
        }
    }
}, { immediate: true });
watch(() => [timelineEntries.value.length, props.completionMessage, props.historyTurns.length], () => { followLatest(); }, { flush: "post" });
watch(() => props.isRunning, (running, wasRunning) => {
    if (running && !wasRunning) {
        typewriterDoneIds.delete('live-completion');
        delete typewriterTexts['live-completion'];
        typewriterTargets.delete('live-completion');
        followLiveTail.value = true;
        followLatest();
    }
    if (!running && wasRunning) followLatest();
});
</script>
<style scoped src="./agent-chat-stream.css"></style>

