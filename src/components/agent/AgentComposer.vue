<template>
    <footer class="manus-composer-wrap">
        <div
            class="manus-composer"
            :class="{ waiting: isWaitingForHuman, 'is-dragging': isDraggingOver }"
            @click="handleComposerClick"
            @dragover.prevent="isDraggingOver = true"
            @dragleave.prevent="isDraggingOver = false"
            @drop.prevent="handleFileDrop"
        >
            <!-- 📎 附件缩略图与文件卡片预览托盘 (Attachment Tray) -->
            <div v-if="attachments.length > 0" class="composer-attachments-tray">
                <AttachmentCard
                    v-for="(item, idx) in attachments"
                    :key="idx"
                    :attachment="item"
                    :removable="true"
                    @preview-image="openImagePreview"
                    @preview-doc="openDocPreview"
                    @remove="removeAttachment(idx)"
                />
            </div>

            <!-- ⏳ 排队队列提示栏 (Queued Goals Tray) -->
            <div v-if="queuedGoals && queuedGoals.length > 0" class="composer-queue-tray">
                <span class="queue-icon">⏳</span>
                <span class="queue-label">待执行队列 ({{ queuedGoals.length }})：</span>
                <div class="queue-items-scroll">
                    <div v-for="(q, idx) in queuedGoals" :key="q.id" class="queue-pill">
                        <span class="q-idx">#{{ idx + 1 }}</span>
                        <span class="q-text" :title="q.text">{{ q.text }}</span>
                        <button type="button" class="q-del-btn" @click.stop="$emit('cancel-queue', idx)" title="取消此排队任务">✕</button>
                    </div>
                </div>
            </div>

            <!-- 💡 模式切换放行机制温和引导提示 -->
            <transition name="mode-guide-fade">
                <div v-if="activeModeGuide" class="composer-mode-guide-banner" :class="`is-${activeModeGuide.mode}`">
                    <div class="mode-guide-left">
                        <span class="mode-guide-icon">{{ activeModeGuide.mode === 'cross_review' ? '🔍' : '🛡️' }}</span>
                        <div class="mode-guide-text">
                            <span class="mode-guide-title">{{ activeModeGuide.title }}</span>
                            <span class="mode-guide-desc">{{ activeModeGuide.mechanism }}</span>
                        </div>
                    </div>
                    <button type="button" class="mode-guide-close-btn" @click.stop="dismissModeGuide" title="关闭提示">✕</button>
                </div>
            </transition>

            <!-- 拖拽高亮提示 -->
            <div v-if="isDraggingOver" class="drag-drop-overlay">
                <div class="drag-drop-content">
                    <span class="drag-icon">📥</span>
                    <strong>松开鼠标附加文件到会话</strong>
                    <small>支持图片缩略预览、Excel、PDF、Word 与代码</small>
                </div>
            </div>

            <textarea
                ref="textareaRef"
                :value="draft"
                :disabled="isSending"
                :placeholder="placeholderText"
                rows="1"
                @input="handleInput"
                @keydown.ctrl.enter.exact.prevent="submit('interrupt')"
                @keydown.meta.enter.exact.prevent="submit('interrupt')"
                @keydown.alt.enter.prevent="submit('queue')"
                @keydown.ctrl.shift.enter.prevent="submit('queue')"
                @keydown.meta.shift.enter.prevent="submit('queue')"
            />
            <div class="composer-bottom-row">
                <div class="composer-tools">
                    <button
                        type="button"
                        class="composer-tool-btn attach-btn"
                        title="上传/附加数据文件 (图片、Excel、Word、PDF、代码...)"
                        :disabled="isRunning"
                        @click="promptAttachFiles"
                    >
                        <svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></svg>
                    </button>

                    <span class="tool-divider"></span>

                    <!-- 🤖 模型路由与工位装配浮层卡片 (左下角与上下文模式并排) -->
                    <div class="model-picker-wrap" ref="modelPickerRef">
                        <button
                            class="model-chip"
                            type="button"
                            :class="{ active: modelMenuOpen }"
                            :title="modelChipTooltip"
                            :disabled="isRunning"
                            @click="toggleModelMenu"
                        >
                            <span class="model-icon">🤖</span>
                            <span>{{ currentModelName }}</span>
                            <span v-if="isStageReviewerDisabled" class="chip-gate-badge disabled" title="当前会话已开启免审直通">⚡免审</span>
                            <span v-else class="chip-gate-badge custom" :title="modelChipTooltip">{{ stageReviewerBadgeText }}</span>
                            <svg viewBox="0 0 24 24" class="chevron-icon"><path d="m8 10 4 4 4-4"/></svg>
                        </button>

                        <Teleport to="body">
                            <div
                                v-if="modelMenuOpen"
                                class="model-routing-popover is-teleported"
                                :class="`placement-${popoverPlacement}`"
                                :style="popoverFixedStyle"
                                @click.stop
                            >
                                <div class="popover-header">
                                    <div class="header-left">
                                        <div class="popover-title">🤖 Agent 模型路由与专家小队</div>
                                        <div class="popover-sub">主脑核心算力与业务专家人才库</div>
                                    </div>
                                    <div class="header-actions">
                                        <button
                                            type="button"
                                            class="sync-all-btn"
                                            :title="`将执行与视觉模型一键设为当前推理模型 (${currentModelName})`"
                                            @click="syncAllToPro"
                                        >
                                            一键同款
                                        </button>
                                        <button
                                            type="button"
                                            class="close-popover-btn"
                                            title="关闭 (Esc)"
                                            @click="modelMenuOpen = false"
                                        >
                                            <svg viewBox="0 0 24 24"><path d="M18 6 6 18M6 6l12 12"/></svg>
                                        </button>
                                    </div>
                                </div>

                                <!-- 🌟 顶部工具条：独立分段 Tab + 横排分流模式开关 -->
                                <div class="routing-top-toolbar">
                                    <div class="routing-tabs-nav">
                                        <button
                                            type="button"
                                            class="routing-tab-btn"
                                            :class="{ active: activeRoutingTab === 'models' }"
                                            @click="activeRoutingTab = 'models'"
                                        >
                                            <span class="tab-btn-icon">🤖</span>
                                            <span class="tab-btn-text">主脑算力</span>
                                        </button>
                                        <button
                                            type="button"
                                            class="routing-tab-btn"
                                            :class="{ active: activeRoutingTab === 'roles' }"
                                            @click="activeRoutingTab = 'roles'"
                                        >
                                            <span class="tab-btn-icon">🎭</span>
                                            <span class="tab-btn-text">专家小队与门禁</span>
                                            <span
                                                v-if="allEnabledRoleCards.length > 0"
                                                class="tab-count-badge"
                                                :class="{ 'is-custom': isCustomSquadActive }"
                                                :title="isCustomSquadActive ? `当前出战: ${sessionActiveCardsCount} / 专家库总数: ${allEnabledRoleCards.length}` : `全员就绪: ${allEnabledRoleCards.length}`"
                                            >
                                                {{ sessionActiveCardsCount }}
                                            </span>
                                        </button>
                                    </div>

                                    <!-- 顶部右侧的路由分流模式开关 (横排保证文字绝不竖着折行) -->
                                    <div class="top-mode-switch" title="分流模式：决定每步任务由强模型还是快模型处理">
                                        <span class="mode-switch-label">🎚️ 分流:</span>
                                        <div class="mode-switch-group">
                                            <button
                                                type="button"
                                                class="mode-switch-btn"
                                                :class="{ active: routingMode === 'rules' }"
                                                title="规则分流：首步规划与失败救场走强模型，其余走快模型"
                                                :disabled="isRunning"
                                                @click="setRoutingMode('rules')"
                                            >
                                                规则
                                            </button>
                                            <button
                                                type="button"
                                                class="mode-switch-btn"
                                                :class="{ active: routingMode === 'scored' }"
                                                title="难度打分：按契约/连续失败/观测体量加权评分，≥阈值升维强模型"
                                                :disabled="isRunning"
                                                @click="setRoutingMode('scored')"
                                            >
                                                打分
                                            </button>
                                        </div>
                                    </div>
                                </div>

                                <div class="popover-body">
                                    <!-- Tab 1: 模型分工 (前三个推理、执行、视觉合并展示) -->
                                    <div v-if="activeRoutingTab === 'models'" class="tab-pane-content models-unified-pane">
                                        <!-- 1. 推理规划模型 (Pro) -->
                                        <!-- 1. 推理规划模型 (Pro / 模型 1) -->
                                        <div class="route-item pro-tier">
                                            <div class="route-item-head">
                                                <div class="head-left">
                                                    <span class="route-badge pro">🧠 🥇 模型 1：推理规划 (Pro)</span>
                                                    <small>长程规划与深层自愈救场</small>
                                                </div>
                                                <!-- 🎯 Codex / Claude 风格档位切换器 -->
                                                <div class="effort-segmented-wrap" title="思考档位切换">
                                                    <span class="effort-label">思考:</span>
                                                    <div class="effort-slider-group">
                                                        <button
                                                            v-for="eff in reasoningEffortOptions"
                                                            :key="eff.value"
                                                            type="button"
                                                            class="effort-step-btn"
                                                            :class="{ active: (modelRouting.pro_reasoning_effort || 'auto') === eff.value }"
                                                            :title="eff.tooltip"
                                                            :disabled="isRunning"
                                                            @click="setReasoningEffort('pro', eff.value)"
                                                        >
                                                            {{ eff.label }}
                                                        </button>
                                                    </div>
                                                </div>
                                            </div>
                                            <div class="route-controls">
                                                <ModelSelectDropdown
                                                    :model-value="modelRouting.pro"
                                                    :models="allModels"
                                                    :disabled="isRunning"
                                                    placeholder="选择推理模型 (如 Gemini 2.5 Pro, DeepSeek-R1)..."
                                                    @update:model-value="updateRouteValue('pro', $event)"
                                                    size="md"
                                                />
                                            </div>
                                        </div>

                                        <!-- 2. 极速执行模型 (Flash / 模型 2) -->
                                        <div class="route-item flash-tier">
                                            <div class="route-item-head">
                                                <div class="head-left">
                                                    <span class="route-badge flash">⚡ 🥈 模型 2：极速执行 (Flash)</span>
                                                    <small>高频工具调用与快速改码</small>
                                                </div>
                                                <!-- 🎯 Codex / Claude 风格档位切换器 -->
                                                <div class="effort-segmented-wrap" title="思考档位切换">
                                                    <span class="effort-label">思考:</span>
                                                    <div class="effort-slider-group">
                                                        <button
                                                            v-for="eff in reasoningEffortOptions"
                                                            :key="eff.value"
                                                            type="button"
                                                            class="effort-step-btn"
                                                            :class="{ active: (modelRouting.flash_reasoning_effort || 'auto') === eff.value }"
                                                            :title="eff.tooltip"
                                                            :disabled="isRunning"
                                                            @click="setReasoningEffort('flash', eff.value)"
                                                        >
                                                            {{ eff.label }}
                                                        </button>
                                                    </div>
                                                </div>
                                            </div>
                                            <div class="route-controls">
                                                <ModelSelectDropdown
                                                    :model-value="modelRouting.flash"
                                                    :models="allModels"
                                                    :disabled="isRunning"
                                                    placeholder="选择执行模型 (如 Gemini 2.5 Flash, DeepSeek-V3)..."
                                                    @update:model-value="updateRouteValue('flash', $event)"
                                                    size="md"
                                                />
                                            </div>
                                        </div>

                                        <!-- 3. 多模态视觉模型 (Vision) -->
                                        <div class="route-item vision-tier">
                                            <div class="route-item-head">
                                                <div class="head-left">
                                                    <span class="route-badge vision">👁️ 多模态视觉模型 (Vision)</span>
                                                    <small>截图分析与图表识别</small>
                                                </div>
                                                <!-- 🎯 Codex / Claude 风格档位切换器 -->
                                                <div class="effort-segmented-wrap" title="思考档位切换">
                                                    <span class="effort-label">思考:</span>
                                                    <div class="effort-slider-group">
                                                        <button
                                                            v-for="eff in reasoningEffortOptions"
                                                            :key="eff.value"
                                                            type="button"
                                                            class="effort-step-btn"
                                                            :class="{ active: (modelRouting.vision_reasoning_effort || 'auto') === eff.value }"
                                                            :title="eff.tooltip"
                                                            :disabled="isRunning"
                                                            @click="setReasoningEffort('vision', eff.value)"
                                                        >
                                                            {{ eff.label }}
                                                        </button>
                                                    </div>
                                                </div>
                                            </div>
                                            <div class="route-controls">
                                                <ModelSelectDropdown
                                                    :model-value="modelRouting.vision"
                                                    :models="allModels"
                                                    :disabled="isRunning"
                                                    placeholder="选择视觉模型..."
                                                    @update:model-value="updateRouteValue('vision', $event)"
                                                    size="md"
                                                />
                                            </div>
                                        </div>

                                        <!-- 4. 🎯 核心工种独立算力指派（审查、写入、探索独立选模型 1 或模型 2） -->
                                        <div class="route-item role-slots-section">
                                            <div class="route-item-head">
                                                <div class="head-left">
                                                    <span class="route-badge slots">🎯 核心工种独立算力指派</span>
                                                    <small>各环节完全独立：审查、写入、探索可自由点选模型 1 或模型 2</small>
                                                </div>
                                                <button
                                                    v-if="hasAnyCustomRoleSlot"
                                                    type="button"
                                                    class="reset-slots-btn"
                                                    title="清空独立指定，恢复默认分工"
                                                    :disabled="isRunning"
                                                    @click="resetAllRoleSlots"
                                                >
                                                    恢复默认
                                                </button>
                                            </div>

                                            <div class="role-slots-list">
                                                <div
                                                    v-for="slot in coreTaskSlots"
                                                    :key="slot.key"
                                                    class="role-slot-row"
                                                >
                                                    <div class="slot-info">
                                                        <span class="slot-icon">{{ slot.icon }}</span>
                                                        <div class="slot-text">
                                                            <strong class="slot-name">{{ slot.name }}</strong>
                                                            <small class="slot-desc">{{ slot.desc }}</small>
                                                        </div>
                                                    </div>

                                                    <div class="slot-picker-group">
                                                        <!-- 快捷点选模型 1 -->
                                                        <button
                                                            type="button"
                                                            class="slot-choice-pill"
                                                            :class="{ active: getSlotChoice(slot.key) === 'pro' }"
                                                            :title="`指定使用模型 1: ${getModelDisplayName(modelRouting.pro)}`"
                                                            :disabled="isRunning"
                                                            @click="setSlotChoice(slot.key, 'pro')"
                                                        >
                                                            🥇 模型 1
                                                        </button>

                                                        <!-- 快捷点选模型 2 -->
                                                        <button
                                                            type="button"
                                                            class="slot-choice-pill"
                                                            :class="{ active: getSlotChoice(slot.key) === 'flash' }"
                                                            :title="`指定使用模型 2: ${getModelDisplayName(modelRouting.flash)}`"
                                                            :disabled="isRunning"
                                                            @click="setSlotChoice(slot.key, 'flash')"
                                                        >
                                                            🥈 模型 2
                                                        </button>

                                                        <!-- 审查独有的异构对抗严审 -->
                                                        <button
                                                            v-if="slot.key === 'reviewer'"
                                                            type="button"
                                                            class="slot-choice-pill heter"
                                                            :class="{ active: getSlotChoice(slot.key) === '' }"
                                                            title="系统异构对抗：自动选择跨模型家族严审"
                                                            :disabled="isRunning"
                                                            @click="setSlotChoice(slot.key, '')"
                                                        >
                                                            🌐 异构对抗
                                                        </button>

                                                        <!-- 指定任意具体独立模型 -->
                                                        <select
                                                            class="slot-model-select"
                                                            :class="{ active: isCustomSpecificModel(slot.key) }"
                                                            :disabled="isRunning"
                                                            :value="isCustomSpecificModel(slot.key) ? getSlotChoice(slot.key) : ''"
                                                            @change="onSlotSelectChange(slot.key, ($event.target as HTMLSelectElement).value)"
                                                        >
                                                            <option value="">{{ isCustomSpecificModel(slot.key) ? `指定: ${getModelDisplayName(getSlotChoice(slot.key))}` : '更多模型…' }}</option>
                                                            <option
                                                                v-for="m in allModels"
                                                                :key="m.id"
                                                                :value="m.id"
                                                            >
                                                                {{ m.display_name || m.name }}
                                                            </option>
                                                        </select>
                                                    </div>
                                                </div>
                                            </div>
                                        </div>
                                    </div>

                                    <!-- Tab 2: 专家协作小队与门禁 (Squad & Gate) -->
                                    <div v-else-if="activeRoutingTab === 'roles'" class="tab-pane-content squad-pane">
                                        <!-- 1. 🛡️ 阶段验收与门禁策略卡片 -->
                                        <div class="squad-section gate-section">
                                            <div class="section-head">
                                                <div class="section-title-wrap">
                                                    <span class="section-title">🛡️ 阶段验收与门禁策略</span>
                                                    <span class="section-desc">代码写入与阶段推进时的质量把关规则 (按会话独立配置)</span>
                                                </div>
                                                <div
                                                    class="gate-current-status-tag"
                                                    :class="{
                                                        'status-disabled': isStageReviewerDisabled,
                                                        'status-default': !isStageReviewerDisabled,
                                                    }"
                                                >
                                                    <span v-if="isStageReviewerDisabled">⚡ 免审直通</span>
                                                    <span v-else-if="activeReviewerCards.length === 0">🌐 系统异构对抗兜底</span>
                                                    <span v-else-if="activeReviewerCards.length === 1">🎭 专属审查专家</span>
                                                    <span v-else>🎯 领域自适应质检 ({{ activeReviewerCards.length }})</span>
                                                </div>
                                            </div>

                                            <div class="gate-controls-row">
                                                <div class="gate-mode-pills">
                                                    <button
                                                        type="button"
                                                        class="gate-mode-pill-btn"
                                                        :class="{ active: !isStageReviewerDisabled }"
                                                        title="智能阶段门禁：有质检专家时自动挂载，无专家时自动由系统异构模型交叉严把质量关"
                                                        :disabled="isRunning"
                                                        @click="selectStageReviewer('')"
                                                    >
                                                        <span class="btn-icon">🛡️</span>
                                                        <span>智能阶段门禁</span>
                                                    </button>
                                                    <button
                                                        type="button"
                                                        class="gate-mode-pill-btn btn-disabled"
                                                        :class="{ active: isStageReviewerDisabled }"
                                                        title="免审直通：跳过阶段审查，代码写入后直接推进，0 耗时 0 阻拦（适合纯 UI、原型或文档）"
                                                        :disabled="isRunning"
                                                        @click="selectStageReviewer('disabled')"
                                                    >
                                                        <span class="btn-icon">⚡</span>
                                                        <span>免审直通</span>
                                                    </button>
                                                </div>

                                                <!-- 动态策略说明条（精确契合 3+1 种状态） -->
                                                <div v-if="isStageReviewerDisabled" class="gate-policy-banner disabled">
                                                    ⚡ <strong>免审直通已生效</strong>：代码写入后跳过阶段审查直接推进，0 耗时 0 拦截（适合轻量原型、文档、简单脚本）。
                                                </div>
                                                <div v-else-if="activeReviewerCards.length === 0" class="gate-policy-banner default">
                                                    🌐 <strong>系统异构对抗兜底生效中（跨模型家族严审）</strong>：当前小队未启用质检专家，阶段推进时将由系统自动调度异构模型进行 4 维严格把关。
                                                </div>
                                                <div v-else-if="activeReviewerCards.length === 1" class="gate-policy-banner custom">
                                                    🎭 <strong>自动挂载审查专家「{{ activeReviewerCards[0].label || activeReviewerCards[0].name }}」</strong>{{ activeReviewerCards[0].model_id ? `（锁定 ${getCardModelName(activeReviewerCards[0].model_id)}）` : '（跟随快模型）' }}：当前小队已锁定专属审查官，阶段推进时自动负责审查。
                                                </div>
                                                <div v-else class="gate-policy-banner adaptive">
                                                    🎯 <strong>领域自适应质检就绪</strong>：检测到 {{ activeReviewerCards.length }} 位启用的审查专家（前端/后端/安全），阶段推进时按改动代码类型自适应派发对应专家。
                                                </div>
                                            </div>
                                        </div>

                                        <!-- 2. 👥 本会话出战专家阵容 (Session Squad Roster) -->
                                        <div class="squad-section roster-section">
                                            <div class="section-head">
                                                <div class="section-title-wrap">
                                                    <div class="section-title-row">
                                                        <span class="section-title">👥 本会话出战专家阵容</span>
                                                        <span
                                                            class="section-badge-count"
                                                            :class="{ 'is-custom': isCustomSquadActive }"
                                                        >
                                                            {{ isCustomSquadActive ? `自选出战: ${sessionActiveCardsCount} / ${allEnabledRoleCards.length}` : `🌟 全员就绪 (${allEnabledRoleCards.length})` }}
                                                        </span>
                                                    </div>
                                                    <span class="section-desc">
                                                        {{ isCustomSquadActive ? '已进入主动配置模式：主脑仅会派发已出战的专家。点击卡片可快速切换。' : '默认全员待命模式：所有专家卡片均可被调度。点击专家卡片可自由排除或配置小队。' }}
                                                    </span>
                                                </div>
                                                <div class="squad-head-actions">
                                                    <button
                                                        v-if="isCustomSquadActive"
                                                        type="button"
                                                        class="squad-action-pill-btn reset"
                                                        title="清空排除名单，恢复默认全员出战状态"
                                                        :disabled="isRunning"
                                                        @click="resetToAllActive"
                                                    >
                                                        <span>🌟 恢复全员出战</span>
                                                    </button>
                                                    <button
                                                        v-else
                                                        type="button"
                                                        class="squad-action-pill-btn exclude-all"
                                                        title="全部排除，本次任务仅使用主脑默认通用基线（或点击后自选个别专家）"
                                                        :disabled="isRunning"
                                                        @click="hibernateAllCards"
                                                    >
                                                        <span>🚫 全部排除 (仅基线)</span>
                                                    </button>
                                                    <button
                                                        type="button"
                                                        class="manage-roles-link-btn"
                                                        title="打开 Agent 专家工坊，新建或调整专家角色卡"
                                                        @click="openRoleManager"
                                                    >
                                                        <span>工坊管理</span>
                                                        <svg viewBox="0 0 24 24" class="link-icon"><path d="m9 18 6-6-6-6"/></svg>
                                                    </button>
                                                </div>
                                            </div>

                                            <!-- 🏷️ 工种插槽快速过滤标签 -->
                                            <div class="squad-kind-filter-bar">
                                                <button
                                                    type="button"
                                                    class="kind-filter-btn"
                                                    :class="{ active: squadKindFilter === 'all' }"
                                                    @click="squadKindFilter = 'all'"
                                                >
                                                    🌟 全部 ({{ isCustomSquadActive ? `${sessionActiveCardsCount}/${allEnabledRoleCards.length}` : allEnabledRoleCards.length }})
                                                </button>
                                                <button
                                                    type="button"
                                                    class="kind-filter-btn"
                                                    :class="{
                                                        active: squadKindFilter === 'writer',
                                                        'is-depleted': countActiveByKind('writer') === 0 && countByKind('writer') > 0,
                                                    }"
                                                    :title="countActiveByKind('writer') === 0 ? '该工种专家全部已排除，代码改写将使用默认主脑基线' : `改码写入专家：${countActiveByKind('writer')} 出战 / ${countByKind('writer')} 总计`"
                                                    @click="squadKindFilter = 'writer'"
                                                >
                                                    ✍️ 改码写入 ({{ countActiveByKind('writer') }}<span v-if="countActiveByKind('writer') !== countByKind('writer')" class="count-total">/{{ countByKind('writer') }}</span>)
                                                </button>
                                                <button
                                                    type="button"
                                                    class="kind-filter-btn"
                                                    :class="{
                                                        active: squadKindFilter === 'reviewer',
                                                        'is-depleted': countActiveByKind('reviewer') === 0 && countByKind('reviewer') > 0,
                                                    }"
                                                    :title="countActiveByKind('reviewer') === 0 ? '该工种专家全部已排除，质检验收将由系统异构对抗兜底' : `质检验收专家：${countActiveByKind('reviewer')} 出战 / ${countByKind('reviewer')} 总计`"
                                                    @click="squadKindFilter = 'reviewer'"
                                                >
                                                    🛡️ 质检验收 ({{ countActiveByKind('reviewer') }}<span v-if="countActiveByKind('reviewer') !== countByKind('reviewer')" class="count-total">/{{ countByKind('reviewer') }}</span>)
                                                </button>
                                                <button
                                                    type="button"
                                                    class="kind-filter-btn"
                                                    :class="{
                                                        active: squadKindFilter === 'worker',
                                                        'is-depleted': countActiveByKind('worker') === 0 && countByKind('worker') > 0,
                                                    }"
                                                    :title="countActiveByKind('worker') === 0 ? '该工种专家全部已排除，调研探针将使用默认主脑基线' : `调研探针专家：${countActiveByKind('worker')} 出战 / ${countByKind('worker')} 总计`"
                                                    @click="squadKindFilter = 'worker'"
                                                >
                                                    🔎 调研探针 ({{ countActiveByKind('worker') }}<span v-if="countActiveByKind('worker') !== countByKind('worker')" class="count-total">/{{ countByKind('worker') }}</span>)
                                                </button>
                                            </div>

                                            <!-- 专家卡片网格 -->
                                            <div v-if="filteredRoleCards.length > 0" class="squad-cards-grid">
                                                <div
                                                    v-for="card in filteredRoleCards"
                                                    :key="card.id"
                                                    class="squad-member-card"
                                                    :class="{
                                                        'is-active': !isCardHibernated(card.name),
                                                        'is-hibernated': isCardHibernated(card.name),
                                                    }"
                                                    :title="isCardHibernated(card.name) ? '点击将其加入本会话出战小队' : '点击将此专家从本会话排除'"
                                                    @click="toggleSessionCard(card.name)"
                                                >
                                                    <div class="member-card-header">
                                                        <div class="member-identity">
                                                            <div
                                                                class="member-checkbox"
                                                                :class="{ checked: !isCardHibernated(card.name) }"
                                                            >
                                                                <svg v-if="!isCardHibernated(card.name)" viewBox="0 0 24 24" class="check-icon"><path d="M20 6 9 17l-5-5"/></svg>
                                                            </div>
                                                            <div class="member-name-block">
                                                                <strong class="member-label">{{ card.label || card.name }}</strong>
                                                                <span class="member-slug">{{ card.name }}</span>
                                                            </div>
                                                        </div>
                                                        <!-- 状态徽标与切换按钮 -->
                                                        <button
                                                            type="button"
                                                            class="session-toggle-btn"
                                                            :class="{ active: !isCardHibernated(card.name), hibernated: isCardHibernated(card.name) }"
                                                            :title="isCardHibernated(card.name) ? '点击将其加入本会话出战小队' : '点击将此专家从本会话排除'"
                                                            :disabled="isRunning"
                                                            @click.stop="toggleSessionCard(card.name)"
                                                        >
                                                            <span class="status-indicator"></span>
                                                            <span>{{ isCardHibernated(card.name) ? '✕ 已排除' : '✓ 出战中' }}</span>
                                                        </button>
                                                    </div>

                                                    <div class="member-badges-row">
                                                        <div class="member-model-tag" :title="`绑定的模型: ${getCardModelName(card.model_id)}`">
                                                            <span class="model-icon">🎯</span>
                                                            <span class="model-text">{{ getCardModelName(card.model_id) }}</span>
                                                        </div>
                                                        <span
                                                            v-for="badge in getCardKindBadges(card)"
                                                            :key="badge.key"
                                                            class="kind-badge"
                                                            :class="`kind-${badge.key}`"
                                                        >
                                                            {{ badge.label }}
                                                        </span>
                                                        <span v-if="card.hard_scope" class="scope-fence-badge" :title="`物理写入围栏: ${card.hard_scope}`">
                                                            🚧 范围: {{ card.hard_scope }}
                                                        </span>
                                                    </div>

                                                    <div class="member-persona-excerpt" :title="card.persona_prompt || card.scope_hints">
                                                        {{ card.scope_hints || card.persona_prompt?.slice(0, 72) || '通用专业角色卡' }}
                                                    </div>
                                                </div>
                                            </div>

                                            <!-- 空状态提示 -->
                                            <div v-else class="squad-empty-box">
                                                <span class="empty-icon">🎭</span>
                                                <div class="empty-text">
                                                    <strong>{{ squadKindFilter === 'all' ? '专家库暂无已启用的自定义角色卡' : '该工种插槽暂无已启用的专家卡' }}</strong>
                                                    <p>{{ squadKindFilter === 'all' ? '当前所有子 Agent 均使用主脑默认通用基线。可前往专家工坊载入前端、后端或安全审计专家卡。' : '可前往专家工坊为该插槽创建或启用专精角色卡。' }}</p>
                                                </div>
                                                <button
                                                    type="button"
                                                    class="load-preset-btn"
                                                    @click="openRoleManager"
                                                >
                                                    👉 前往「Agent 专家工坊」一键管理专家
                                                </button>
                                            </div>

                                            <div class="squad-footer-mechanism">
                                                <span class="mechanism-icon">💡</span>
                                                <div class="mechanism-text">
                                                    <strong>插槽自适应与兜底调度机制：</strong>
                                                    主 Agent 按任务工种（改码 writer / 调研 worker / 审查 reviewer）自动从「已入选」的专家中匹配派发。已休眠的专家本会话不会被调用；审查插槽无专家时自动由跨厂商异构对抗模型兜底。
                                                </div>
                                            </div>
                                        </div>
                                    </div>
                                </div>
                            </div>
                        </Teleport>
                    </div>
                    
                    <!-- 🚀 上下文压缩模式胶囊 (左下角与模型并排) -->
                    <div class="budget-mode-picker" ref="budgetPickerRef">
                        <button
                            class="budget-chip"
                            type="button"
                            :class="{ active: budgetMenuOpen }"
                            :disabled="isRunning"
                            :title="currentBudgetTooltip"
                            @click="toggleBudgetMenu"
                        >
                            <span class="budget-icon">{{ currentBudgetIcon }}</span>
                            <span class="budget-label">{{ currentBudgetLabel }}</span>
                            <svg viewBox="0 0 24 24" class="chevron-icon"><path d="m8 10 4 4 4-4"/></svg>
                        </button>

                        <Teleport to="body">
                            <div
                                v-if="budgetMenuOpen"
                                class="budget-dropdown-menu is-teleported"
                                :class="`placement-${budgetPlacement}`"
                                :style="budgetFixedStyle"
                                @click.stop
                            >
                                <div class="budget-menu-header">上下文与压缩策略</div>
                                <button
                                    v-for="opt in budgetModes"
                                    :key="opt.id"
                                    type="button"
                                    class="budget-menu-item"
                                    :class="{ selected: opt.id === (modelRouting.context_budget_mode || 'deep_coding') }"
                                    @click="selectBudgetMode(opt.id)"
                                >
                                    <span class="opt-icon">{{ opt.icon }}</span>
                                    <div class="opt-text">
                                        <div class="opt-title">
                                            <strong>{{ opt.description }}</strong>
                                            <span class="opt-tag">{{ opt.tag }}</span>
                                        </div>
                                        <small class="opt-sub">{{ opt.tooltip }}</small>
                                    </div>
                                    <svg v-if="opt.id === (modelRouting.context_budget_mode || 'deep_coding')" class="check-icon" viewBox="0 0 24 24"><path d="M20 6 9 17l-5-5"/></svg>
                                </button>
                            </div>
                        </Teleport>
                    </div>

                    <!-- 🚀 全自动 vs 🔍 交叉审查 vs 🛡️ 方案审批 门禁模式下拉选择 -->
                    <div class="plan-mode-picker" ref="planPickerRef">
                        <button
                            type="button"
                            class="composer-tool-btn plan-mode-toggle-btn"
                            :class="{
                                'is-plan-first': currentPlanMode === 'plan_approval',
                                'is-cross-review': currentPlanMode === 'cross_review',
                                active: planMenuOpen,
                            }"
                            :title="planModeTooltip"
                            :disabled="isRunning"
                            @click="togglePlanMenu"
                        >
                            <span class="plan-mode-icon">{{ planModeIcon }}</span>
                            <span class="plan-mode-label">{{ planModeLabel }}</span>
                            <svg viewBox="0 0 24 24" class="chevron-icon"><path d="m8 10 4 4 4-4"/></svg>
                        </button>

                        <Teleport to="body">
                            <div
                                v-if="planMenuOpen"
                                class="plan-dropdown-menu is-teleported"
                                :class="`placement-${planPlacement}`"
                                :style="planFixedStyle"
                                @click.stop
                            >
                                <div class="plan-menu-header">执行与方案审批门禁</div>
                                <button
                                    v-for="opt in planModes"
                                    :key="opt.id"
                                    type="button"
                                    class="plan-menu-item"
                                    :class="[
                                        `mode-${opt.id}`,
                                        { selected: opt.id === currentPlanMode },
                                    ]"
                                    @click="selectPlanMode(opt.id)"
                                >
                                    <span class="opt-icon">{{ opt.icon }}</span>
                                    <div class="opt-text">
                                        <div class="opt-title">
                                            <strong>{{ opt.label }}</strong>
                                            <span class="opt-tag" :class="`tag-${opt.id}`">{{ opt.tag }}</span>
                                        </div>
                                        <small class="opt-sub">{{ opt.description }}</small>
                                    </div>
                                    <svg v-if="opt.id === currentPlanMode" class="check-icon" viewBox="0 0 24 24"><path d="M20 6 9 17l-5-5"/></svg>
                                </button>
                            </div>
                        </Teleport>
                    </div>


                    <!-- ⌨️ 快捷键提示小问号 -->
                    <div class="composer-shortcut-hint-wrap">
                        <button
                            type="button"
                            class="composer-tool-btn shortcut-hint-btn"
                            title="键盘快捷键与发送模式说明"
                            @mouseenter="showShortcutTooltip = true"
                            @mouseleave="showShortcutTooltip = false"
                            @click="showShortcutTooltip = !showShortcutTooltip"
                        >
                            <svg viewBox="0 0 24 24" class="hint-q-icon"><circle cx="12" cy="12" r="9"/><path d="M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3M12 17h.01"/></svg>
                        </button>
                        <div v-if="showShortcutTooltip" class="shortcut-popover-card">
                            <div class="shortcut-popover-title">⌨️ 发送与控制快捷键</div>
                            <div class="shortcut-row">
                                <div class="shortcut-keys"><kbd>Ctrl</kbd> + <kbd>↵</kbd></div>
                                <div class="shortcut-desc"><strong class="text-red">⚡ 立即打断</strong>：中断前序，立即切换新任务</div>
                            </div>
                            <div class="shortcut-row">
                                <div class="shortcut-keys"><kbd>Alt</kbd> + <kbd>↵</kbd></div>
                                <div class="shortcut-desc"><strong class="text-amber">⏳ 排队执行</strong>：当前完成后自动无缝执行</div>
                            </div>
                            <div class="shortcut-row">
                                <div class="shortcut-keys"><kbd>Shift</kbd> + <kbd>↵</kbd></div>
                                <div class="shortcut-desc">在输入框中换行</div>
                            </div>
                        </div>
                    </div>
                </div>
                <div class="composer-actions">
                    <button v-if="isRunning" class="stop-control" type="button" :disabled="stopRequested" @click="$emit('stop')">{{ stopRequested ? '停止中' : '停止' }}</button>
                    
                    <!-- 运行中模式切换：立即打断 vs 排队执行 -->
                    <button
                        v-if="isRunning && !isWaitingForHuman"
                        type="button"
                        class="mode-switch-pill"
                        :class="{ 'is-queue': submitMode === 'queue' }"
                        :title="submitMode === 'interrupt' ? '当前模式: ⚡ 立即打断当前任务 (点击切换为排队执行)' : '当前模式: ⏳ 排队等待当前任务完成后执行 (点击切换为立即打断)'"
                        @click="submitMode = submitMode === 'interrupt' ? 'queue' : 'interrupt'"
                    >
                        <span class="mode-icon">{{ submitMode === 'interrupt' ? '⚡' : '⏳' }}</span>
                        <span>{{ submitMode === 'interrupt' ? '立即打断' : '排队执行' }}</span>
                    </button>

                    <button
                        class="submit-control"
                        :class="{
                            'is-queue-submit': isRunning && !isWaitingForHuman && submitMode === 'queue',
                            'is-interrupt-submit': isRunning && !isWaitingForHuman && submitMode === 'interrupt'
                        }"
                        type="button"
                        :disabled="!canSubmit || isSending"
                        :title="isRunning ? (isWaitingForHuman ? '回答当前请求' : (submitMode === 'interrupt' ? '⚡ 立即打断当前任务并切换为新目标' : '⏳ 加入队列，等当前任务完成后自动执行')) : '开始任务'"
                        @click="submit"
                    >
                        <svg viewBox="0 0 24 24"><path d="m7 12 5-5 5 5M12 7v10"/></svg>
                    </button>
                </div>
            </div>
        </div>

        <!-- 🚀 实时性能流式状态行 (连续字体排版 · 柔和间距 · 实时流式跳动) -->
        <div class="composer-telemetry-line">
            <!-- 1. 耗时 -->
            <span class="telemetry-node" title="任务总运行耗时">
                <span class="dot-running" v-if="isRunning"></span>
                <span class="t-label">⏱️ 耗时</span>
                <span class="t-val">{{ telemetry?.elapsedLabel || '00:00' }}</span>
            </span>
            <span class="t-sep">·</span>

            <!-- 2. Token 实时生成速率 -->
            <span class="telemetry-node" title="模型实时生成速率 (tok/s)">
                <span class="t-label">⚡ 速率</span>
                <span class="t-val text-cyan">{{ (telemetry?.tokensPerSecond && telemetry.tokensPerSecond > 0) ? `${telemetry.tokensPerSecond.toFixed(1)} tok/s` : '--' }}</span>
            </span>
            <span class="t-sep">·</span>

            <!-- 3. 首字延迟 (TTFT) -->
            <span class="telemetry-node" title="首字到达延迟 (TTFT)">
                <span class="t-label">⏳ 首次</span>
                <span class="t-val">{{ (telemetry?.lastTtftMs && telemetry.lastTtftMs > 0) ? `${telemetry.lastTtftMs}ms` : '--' }}</span>
            </span>
            <span class="t-sep">·</span>

            <!-- 4. 前缀缓存命中率 -->
            <span class="telemetry-node" title="前缀缓存命中率">
                <span class="t-label">🎯 缓存</span>
                <span class="t-val text-green">{{ (telemetry?.cacheHitRate !== undefined && telemetry.cacheHitRate > 0) ? `${telemetry.cacheHitRate.toFixed(1)}%` : '--%' }}</span>
            </span>
            <span class="t-sep">·</span>

            <!-- 5. 上下文占用 / 窗口水位 -->
            <span class="telemetry-node" title="当前上下文占用 / 窗口上限">
                <span class="t-label">📦 上下文</span>
                <span class="t-val">{{ (telemetry?.totalTokens && telemetry.totalTokens > 0) ? `${formatTokens(telemetry.totalTokens)}/${(telemetry?.contextWindow && telemetry.contextWindow > 0) ? formatTokens(telemetry.contextWindow) : '-'}` : '- / -' }}</span>
                <span v-if="telemetry?.totalTokens && telemetry.totalTokens > 0" class="t-tag" :class="contextLevelClass">({{ (telemetry?.contextUsagePercent ?? 0).toFixed(0) }}%)</span>
            </span>
            <span class="t-sep">·</span>

            <!-- 6. 工具调用时间 -->
            <span class="telemetry-node" title="工具执行耗时">
                <span class="t-label">🔧 工具</span>
                <span class="t-val">{{ (telemetry?.lastToolDurationMs && telemetry.lastToolDurationMs > 0) ? formatMs(telemetry.lastToolDurationMs) : '--' }}</span>
            </span>
            <span class="t-sep">·</span>

            <!-- 7. 总体回合与轮次统计 (一共多少轮/一共多少回合总体数据统计) -->
            <span
                class="telemetry-node turns-stat-node"
                :title="`会话总体轮次：当前第 ${currentTurn || 1} 回合 (总计开启 ${totalTurns || 1} 轮任务)`"
            >
                <span class="t-label">💬 回合</span>
                <span class="t-val text-indigo">{{ isRunning ? `第 ${currentTurn || 1}/${totalTurns || 1} 轮` : `共 ${totalTurns || 1} 回合` }}</span>
            </span>
            <span class="t-sep">·</span>

            <!-- 8. 累计步骤与大阶段统计 -->
            <span
                class="telemetry-node steps-stat-node"
                :title="`会话总体进度：累计执行 ${totalSteps || telemetry?.stepCount || 0} 个步骤，跨越 ${totalStages || 1} 个大阶段`"
            >
                <span class="t-label">🔄 步骤</span>
                <span class="t-val">{{ isRunning ? `本轮第 ${telemetry?.stepCount || 1} 步 · 累${totalSteps || telemetry?.stepCount || 0}` : `累计 ${totalSteps || telemetry?.stepCount || 0} 步` }}</span>
            </span>
        </div>

        <!-- 📎 全局全屏附件预览弹窗 (Teleport 到 body，杜绝容器 backdrop-filter 遮挡) -->
        <AttachmentPreviewModal
            :image-modal="previewImageModal"
            :doc-modal="previewDocModal"
            @close-image="previewImageModal = null"
            @close-doc="previewDocModal = null"
        />
    </footer>
</template>

<script setup lang="ts">
import { formatFileSize, formatMs, formatTokens } from "./formatUtils";
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import ModelSelectDropdown from "../common/ModelSelectDropdown.vue";
import AttachmentCard from "./AttachmentCard.vue";
import AttachmentPreviewModal from "./AttachmentPreviewModal.vue";
import {
    CONTEXT_BUDGET_MODES,
    type AgentHumanForm,
    type AgentModelOption,
    type AgentSessionTelemetry,
    type ContextBudgetMode,
    type ModelRouting,
    type PlanExecutionMode,
    type ReasoningEffort,
    type RoleCard,
} from "./types";

const props = defineProps<{
    draft: string;
    isRunning: boolean;
    isWaitingForHuman: boolean;
    isSending: boolean;
    stopRequested: boolean;
    canStart: boolean;
    pauseReason: string;
    humanForm: AgentHumanForm | null;
    allModels: AgentModelOption[];
    modelRouting: ModelRouting;
    telemetry?: AgentSessionTelemetry | null;
    totalTurns?: number;
    currentTurn?: number;
    totalStages?: number;
    totalSteps?: number;
    queuedGoals?: Array<{ id: string; text: string; createdAt: number }>;
    autoPilot?: boolean;
    planMode?: PlanExecutionMode;
}>();
const emit = defineEmits<{
    "update:draft": [value: string];
    "update:model-routing": [routing: ModelRouting];
    "update:budget-mode": [mode: ContextBudgetMode];
    "update:autoPilot": [value: boolean];
    "update:planMode": [mode: PlanExecutionMode];
    submit: [payload?: { text: string; attachments: ParsedAttachment[]; mode?: "interrupt" | "queue" }];
    "cancel-queue": [index: number];
    stop: [];
    "open-settings": [];
    "open-roles": [];
    "attach-files": [paths: string[]];
}>();

const currentPlanMode = computed<PlanExecutionMode>(() => {
    if (props.planMode) return props.planMode;
    return props.autoPilot === false ? "plan_approval" : "auto";
});

const planModeIcon = computed(() => {
    switch (currentPlanMode.value) {
        case "cross_review":
            return "🔍";
        case "plan_approval":
            return "🛡️";
        case "auto":
        default:
            return "🚀";
    }
});

const planModeLabel = computed(() => {
    switch (currentPlanMode.value) {
        case "cross_review":
            return "交叉审查";
        case "plan_approval":
            return "方案审批";
        case "auto":
        default:
            return "全自动";
    }
});

const planModeTooltip = computed(() => {
    switch (currentPlanMode.value) {
        case "auto":
            return "当前模式: 🚀 全自动 (Auto) - 探索排查后直接实施。点击选择门禁模式";
        case "cross_review":
            return "当前模式: 🔍 交叉审查 (Cross-review) - 动笔前强制由子 Agent 独立推演挑刺。点击选择门禁模式";
        case "plan_approval":
            return "当前模式: 🛡️ 方案审批 (Plan First) - 方案就绪后须经您确认方可写入代码。点击选择门禁模式";
        default:
            return "";
    }
});

interface PlanModeOption {
    id: PlanExecutionMode;
    icon: string;
    label: string;
    tag: string;
    description: string;
}

const planModes: PlanModeOption[] = [
    {
        id: "auto",
        icon: "🚀",
        label: "全自动",
        tag: "极速流畅",
        description: "完成只读排查后直接动笔修改代码，端到端自动推进不打扰",
    },
    {
        id: "cross_review",
        icon: "🔍",
        label: "交叉审查",
        tag: "双 Agent",
        description: "动笔前强制派发子 Agent 对抗推演，消费审查结论后方可动笔",
    },
    {
        id: "plan_approval",
        icon: "🛡️",
        label: "方案审批",
        tag: "人工确认",
        description: "动笔前必须呈报完整技术方案，经您在界面确认批准后才放行",
    },
];

function selectPlanMode(mode: PlanExecutionMode) {
    emit("update:planMode", mode);
    emit("update:autoPilot", mode !== "plan_approval");
    planMenuOpen.value = false;

    if (mode === "cross_review") {
        activeModeGuide.value = {
            mode: "cross_review",
            title: "交叉审查模式已就绪",
            mechanism: "放行机制：AI 动笔写代码前，必须派发独立 Reviewer 子 Agent 对抗推演，并通过审查结论后方可放行写入。",
        };
    } else if (mode === "plan_approval") {
        activeModeGuide.value = {
            mode: "plan_approval",
            title: "方案审批模式已就绪",
            mechanism: "放行机制：AI 制定完技术方案后会暂停并呈报审批卡片，经您在界面确认批准后才放行代码修改。",
        };
    } else {
        activeModeGuide.value = null;
    }
}

interface ModeGuideTip {
    mode: "cross_review" | "plan_approval";
    title: string;
    mechanism: string;
}

const activeModeGuide = ref<ModeGuideTip | null>(null);

function dismissModeGuide() {
    activeModeGuide.value = null;
}

const submitMode = ref<"interrupt" | "queue">("queue");
const showShortcutTooltip = ref(false);

const textareaRef = ref<HTMLTextAreaElement | null>(null);

interface ParsedAttachment {
    file_name: string;
    file_path: string;
    size_bytes: number;
    content_type: string;
    prompt_text: string;
    image_data_url?: string | null;
    raw_preview?: string | null;
}

const attachments = ref<ParsedAttachment[]>([]);
const isDraggingOver = ref(false);
const previewImageModal = ref<ParsedAttachment | null>(null);
const previewDocModal = ref<ParsedAttachment | null>(null);

function openImagePreview(item: ParsedAttachment) {
    previewImageModal.value = item;
}

function openDocPreview(item: ParsedAttachment) {
    previewDocModal.value = item;
}

function removeAttachment(index: number) {
    attachments.value.splice(index, 1);
}

async function promptAttachFiles() {
    try {
        const selected = await openDialog({
            multiple: true,
            title: "选择要附加到会话的文件 (图片、Excel、Word、PDF、代码...)",
        });
        if (!selected) return;
        const paths = Array.isArray(selected) ? selected : [selected];
        const validPaths = paths.filter((p): p is string => typeof p === "string" && !!p);
        if (validPaths.length === 0) return;

        emit("attach-files", validPaths);

        for (const p of validPaths) {
            try {
                const parsed = await invoke<ParsedAttachment>("parse_attachment_file", { path: p });
                if (parsed) {
                    attachments.value.push(parsed);
                }
            } catch (err) {
                console.warn("解析文件失败，使用路径降级:", err);
                const fileName = p.split("/").pop() || p;
                attachments.value.push({
                    file_name: fileName,
                    file_path: p,
                    size_bytes: 0,
                    content_type: "file",
                    prompt_text: `【附加文件】: ${p}`,
                    image_data_url: null,
                    raw_preview: null,
                });
            }
        }
    } catch (e) {
        console.error("选择文件失败:", e);
    }
}

async function handleFileDrop(e: DragEvent) {
    isDraggingOver.value = false;
    const files = e.dataTransfer?.files;
    if (!files || files.length === 0) return;

    for (let i = 0; i < files.length; i++) {
        const file = files[i];
        const reader = new FileReader();
        reader.onload = async (event) => {
            const base64Data = event.target?.result as string;
            if (!base64Data) return;
            try {
                if (file.type.startsWith("image/")) {
                    attachments.value.push({
                        file_name: file.name,
                        file_path: file.name,
                        size_bytes: file.size,
                        content_type: file.type,
                        prompt_text: `【用户已上传图片附件: \`${file.name}\`】(格式: ${file.type}, 大小: ${(file.size / 1024).toFixed(1)} KB)\n*(图片数据已作为视觉多模态消息携带)*\n`,
                        image_data_url: base64Data,
                        raw_preview: null,
                    });
                } else {
                    const parsed = await invoke<ParsedAttachment>("save_and_parse_uploaded_attachment", {
                        sessionId: "temp-session",
                        fileName: file.name,
                        base64Data,
                    });
                    if (parsed) {
                        attachments.value.push(parsed);
                    }
                }
            } catch (err) {
                console.warn("解析拖拽文件失败:", err);
            }
        };
        reader.readAsDataURL(file);
    }
}
const modelPickerRef = ref<HTMLElement | null>(null);
const modelMenuOpen = ref(false);
const activeRoutingTab = ref<"models" | "roles">("models");
const budgetPickerRef = ref<HTMLElement | null>(null);
const budgetMenuOpen = ref(false);
const planPickerRef = ref<HTMLElement | null>(null);
const planMenuOpen = ref(false);
const planPlacement = ref<"top" | "bottom">("top");
const planCoords = ref<{ top: number; left: number; width: number; maxHeight: number }>({
    top: 0,
    left: 0,
    width: 290,
    maxHeight: 320,
});

const allRoleCards = ref<RoleCard[]>([]);
const reviewerCards = ref<RoleCard[]>([]);

async function loadAllRoleCards() {
    try {
        const cards = await invoke<RoleCard[]>("list_role_cards");
        allRoleCards.value = cards || [];
        reviewerCards.value = (cards || []).filter((c) => {
            if (!c.enabled) return false;
            const kinds = c.allowed_kinds?.trim() || "";
            if (!kinds || kinds === "*") return true;
            return kinds.split(/[,;，；\s]+/).map(s => s.trim().toLowerCase()).includes("reviewer");
        });
    } catch {
        allRoleCards.value = [];
        reviewerCards.value = [];
    }
}

const allEnabledRoleCards = computed(() => {
    return allRoleCards.value.filter((c) => c.enabled);
});

function getCardModelName(modelId?: string): string {
    if (!modelId) return "跟随主脑快模型";
    const found = props.allModels.find((m) => m.id === modelId);
    return found ? (found.display_name || found.name) : modelId;
}

const coreTaskSlots = [
    {
        key: "reviewer",
        icon: "🛡️",
        name: "阶段审查 (Reviewer)",
        desc: "质量验收与防翻车门禁",
    },
    {
        key: "writer",
        icon: "✍️",
        name: "改码写入 (Writer)",
        desc: "代码编写与 diff 修改",
    },
    {
        key: "scout",
        icon: "🔎",
        name: "探索阅读 (Scout / 套索)",
        desc: "文件查阅与只读探针",
    },
    {
        key: "planner",
        icon: "🧭",
        name: "任务规划 (Planner)",
        desc: "首步全局架构与任务拆解",
    },
    {
        key: "executor",
        icon: "⚙️",
        name: "常规执行 (Executor)",
        desc: "日常工具调度与执行推进",
    },
];

function getModelDisplayName(modelId?: string): string {
    return getCardModelName(modelId);
}

function getSlotChoice(slotKey: string): string {
    const roleModels = props.modelRouting?.role_models || {};
    if (roleModels[slotKey] !== undefined) {
        return roleModels[slotKey];
    }
    if (slotKey === "reviewer") return "";
    if (slotKey === "planner") return "pro";
    return "flash";
}

function isCustomSpecificModel(slotKey: string): boolean {
    const choice = getSlotChoice(slotKey);
    return choice !== "" && choice !== "pro" && choice !== "flash" && choice !== "vision";
}

function setSlotChoice(slotKey: string, choice: string) {
    if (props.isRunning) return;
    const roleModels = { ...(props.modelRouting?.role_models || {}) };
    if (choice === "" && slotKey === "reviewer") {
        delete roleModels[slotKey];
    } else if (choice) {
        roleModels[slotKey] = choice;
    } else {
        delete roleModels[slotKey];
    }
    emit("update:model-routing", {
        ...props.modelRouting,
        role_models: roleModels,
    });
}

function onSlotSelectChange(slotKey: string, modelId: string) {
    if (!modelId) return;
    setSlotChoice(slotKey, modelId);
}

const hasAnyCustomRoleSlot = computed(() => {
    const roleModels = props.modelRouting?.role_models || {};
    return Object.keys(roleModels).length > 0;
});

function resetAllRoleSlots() {
    if (props.isRunning) return;
    emit("update:model-routing", {
        ...props.modelRouting,
        role_models: {},
    });
}

function getCardKindBadges(card: RoleCard): Array<{ key: string; label: string }> {
    const raw = (card.allowed_kinds || "*").trim();
    if (raw === "*" || !raw) {
        return [{ key: "all", label: "🌟 全能调度 (*)" }];
    }
    const map: Record<string, string> = {
        writer: "✍️ 代码编写",
        worker: "🔎 调研探针",
        scout: "🛰️ 侦察分析",
        reviewer: "🛡️ 质量审查",
    };
    return raw.split(/[,;，；\s]+/).filter(Boolean).map((k) => {
        const lower = k.toLowerCase();
        return {
            key: lower,
            label: map[lower] || `🏷️ ${k}`,
        };
    });
}

function openRoleManager() {
    modelMenuOpen.value = false;
    emit("open-roles");
}

const squadKindFilter = ref<"all" | "writer" | "reviewer" | "worker">("all");

const disabledRoleCards = computed(() => {
    return props.modelRouting?.disabled_role_cards || [];
});

const isCustomSquadActive = computed(() => {
    return disabledRoleCards.value.length > 0;
});

function isCardHibernated(name: string): boolean {
    return disabledRoleCards.value.some((d) => d.toLowerCase() === name.toLowerCase());
}

function toggleSessionCard(name: string) {
    if (props.isRunning) return;
    const list = [...(props.modelRouting?.disabled_role_cards || [])];
    const idx = list.findIndex((n) => n.toLowerCase() === name.toLowerCase());
    if (idx >= 0) {
        list.splice(idx, 1);
    } else {
        list.push(name);
    }
    emit("update:model-routing", {
        ...props.modelRouting,
        disabled_role_cards: list,
    });
}

function resetToAllActive() {
    if (props.isRunning) return;
    emit("update:model-routing", {
        ...props.modelRouting,
        disabled_role_cards: [],
    });
}

function hibernateAllCards() {
    if (props.isRunning) return;
    const allNames = allEnabledRoleCards.value.map((c) => c.name);
    emit("update:model-routing", {
        ...props.modelRouting,
        disabled_role_cards: allNames,
    });
}

const sessionActiveCardsCount = computed(() => {
    return allEnabledRoleCards.value.filter((c) => !isCardHibernated(c.name)).length;
});

function countByKind(kind: string): number {
    return allEnabledRoleCards.value.filter((c) => {
        const raw = (c.allowed_kinds || "*").toLowerCase();
        return raw === "*" || raw.includes(kind);
    }).length;
}

function countActiveByKind(kind: string): number {
    return allEnabledRoleCards.value.filter((c) => {
        if (isCardHibernated(c.name)) return false;
        const raw = (c.allowed_kinds || "*").toLowerCase();
        return raw === "*" || raw.includes(kind);
    }).length;
}

const filteredRoleCards = computed(() => {
    if (squadKindFilter.value === "all") return allEnabledRoleCards.value;
    return allEnabledRoleCards.value.filter((c) => {
        const raw = (c.allowed_kinds || "*").toLowerCase();
        return raw === "*" || raw.includes(squadKindFilter.value);
    });
});

const currentStageReviewerOverride = computed(() => {
    return (props.modelRouting?.stage_reviewer_override || "").trim();
});

const isStageReviewerDisabled = computed(() => {
    const clean = currentStageReviewerOverride.value.toLowerCase();
    return clean === "disabled" || clean === "none";
});

const activeReviewerCards = computed(() => {
    return allEnabledRoleCards.value.filter((c) => {
        if (isCardHibernated(c.name)) return false;
        const raw = (c.allowed_kinds || "*").toLowerCase();
        return raw === "*" || raw.includes("reviewer");
    });
});

const stageReviewerBadgeText = computed(() => {
    if (isStageReviewerDisabled.value) return "⚡免审";
    if (activeReviewerCards.value.length === 0) return "🌐异构";
    if (activeReviewerCards.value.length === 1) {
        const c = activeReviewerCards.value[0];
        const l = c.label || c.name;
        return `🎭${l.length > 4 ? l.slice(0, 3) + '…' : l}`;
    }
    return `🎯自适应(${activeReviewerCards.value.length})`;
});

const modelChipTooltip = computed(() => {
    let title = `配置模型路由与专家小队 (当前规划: ${currentModelName.value})`;
    if (isStageReviewerDisabled.value) {
        title += " | ⚡ 阶段门禁: 免审直通";
    } else if (activeReviewerCards.value.length === 0) {
        title += " | 🌐 阶段门禁: 系统异构对抗兜底";
    } else if (activeReviewerCards.value.length === 1) {
        title += ` | 🎭 阶段门禁: 专属专家(${activeReviewerCards.value[0].label || activeReviewerCards.value[0].name})`;
    } else {
        title += ` | 🎯 阶段门禁: 领域自适应质检(${activeReviewerCards.value.length}位专家)`;
    }
    return title;
});

function selectStageReviewer(name: string) {
    emit("update:model-routing", {
        ...props.modelRouting,
        stage_reviewer_override: name ? name : undefined,
    });
}

const budgetModes = CONTEXT_BUDGET_MODES;
const currentBudgetModeObj = computed(() => {
    const mode = props.modelRouting?.context_budget_mode || "deep_coding";
    return budgetModes.find((item) => item.id === mode) || budgetModes[0];
});
const currentBudgetIcon = computed(() => currentBudgetModeObj.value.icon);
const currentBudgetLabel = computed(() => currentBudgetModeObj.value.label);
const currentBudgetTooltip = computed(() => currentBudgetModeObj.value.tooltip);

const popoverPlacement = ref<"top" | "bottom">("top");
const popoverCoords = ref<{ top: number; left: number; width: number; maxHeight: number }>({
    top: 0,
    left: 0,
    width: 520,
    maxHeight: 480,
});

const budgetPlacement = ref<"top" | "bottom">("top");
const budgetCoords = ref<{ top: number; left: number; width: number; maxHeight: number }>({
    top: 0,
    left: 0,
    width: 260,
    maxHeight: 400,
});

function calculatePopoverPosition(triggerEl: HTMLElement | null) {
    if (!triggerEl) return;
    const rect = triggerEl.getBoundingClientRect();
    const spaceAbove = rect.top;
    const spaceBelow = window.innerHeight - rect.bottom;

    const targetWidth = Math.min(520, window.innerWidth - 24);
    let left = rect.left;
    if (left + targetWidth > window.innerWidth - 12) {
        left = Math.max(12, window.innerWidth - targetWidth - 12);
    }
    left = Math.max(12, left);

    const neededHeight = 470;
    let placement: "top" | "bottom" = "top";
    let top = 0;
    let maxHeight = 480;

    if (spaceAbove < neededHeight && spaceBelow > spaceAbove) {
        placement = "bottom";
        top = rect.bottom + 8;
        maxHeight = Math.max(260, Math.min(500, spaceBelow - 20));
    } else {
        placement = "top";
        top = rect.top - 8;
        maxHeight = Math.max(260, Math.min(500, spaceAbove - 20));
    }

    popoverPlacement.value = placement;
    popoverCoords.value = { top, left, width: targetWidth, maxHeight };
}

const popoverFixedStyle = computed(() => {
    if (popoverPlacement.value === "top") {
        return {
            position: "fixed" as const,
            bottom: `${Math.max(10, window.innerHeight - popoverCoords.value.top)}px`,
            left: `${popoverCoords.value.left}px`,
            width: `${popoverCoords.value.width}px`,
            maxHeight: `${popoverCoords.value.maxHeight}px`,
            zIndex: 99999,
        };
    }
    return {
        position: "fixed" as const,
        top: `${popoverCoords.value.top}px`,
        left: `${popoverCoords.value.left}px`,
        width: `${popoverCoords.value.width}px`,
        maxHeight: `${popoverCoords.value.maxHeight}px`,
        zIndex: 99999,
    };
});

function calculateBudgetPosition(triggerEl: HTMLElement | null) {
    if (!triggerEl) return;
    const rect = triggerEl.getBoundingClientRect();
    const spaceAbove = rect.top;
    const spaceBelow = window.innerHeight - rect.bottom;
    const targetWidth = 260;

    let left = rect.left;
    if (left + targetWidth > window.innerWidth - 12) {
        left = Math.max(12, window.innerWidth - targetWidth - 12);
    }
    left = Math.max(12, left);

    let placement: "top" | "bottom" = "top";
    let top = 0;
    let maxHeight = 400;

    if (spaceAbove < 360 && spaceBelow > spaceAbove) {
        placement = "bottom";
        top = rect.bottom + 8;
        maxHeight = Math.max(200, Math.min(420, spaceBelow - 20));
    } else {
        placement = "top";
        top = rect.top - 8;
        maxHeight = Math.max(200, Math.min(420, spaceAbove - 20));
    }

    budgetPlacement.value = placement;
    budgetCoords.value = { top, left, width: targetWidth, maxHeight };
}

const budgetFixedStyle = computed(() => {
    if (budgetPlacement.value === "top") {
        return {
            position: "fixed" as const,
            bottom: `${Math.max(10, window.innerHeight - budgetCoords.value.top)}px`,
            left: `${budgetCoords.value.left}px`,
            width: `${budgetCoords.value.width}px`,
            maxHeight: `${budgetCoords.value.maxHeight}px`,
            zIndex: 99999,
        };
    }
    return {
        position: "fixed" as const,
        top: `${budgetCoords.value.top}px`,
        left: `${budgetCoords.value.left}px`,
        width: `${budgetCoords.value.width}px`,
        maxHeight: `${budgetCoords.value.maxHeight}px`,
        zIndex: 99999,
    };
});

function calculatePlanPosition(triggerEl: HTMLElement | null) {
    if (!triggerEl) return;
    const rect = triggerEl.getBoundingClientRect();
    const spaceAbove = rect.top;
    const spaceBelow = window.innerHeight - rect.bottom;
    const targetWidth = 290;

    let left = rect.left;
    if (left + targetWidth > window.innerWidth - 12) {
        left = Math.max(12, window.innerWidth - targetWidth - 12);
    }
    left = Math.max(12, left);

    let placement: "top" | "bottom" = "top";
    let top = 0;
    let maxHeight = 320;

    if (spaceAbove < 260 && spaceBelow > spaceAbove) {
        placement = "bottom";
        top = rect.bottom + 8;
        maxHeight = Math.max(180, Math.min(360, spaceBelow - 20));
    } else {
        placement = "top";
        top = rect.top - 8;
        maxHeight = Math.max(180, Math.min(360, spaceAbove - 20));
    }

    planPlacement.value = placement;
    planCoords.value = { top, left, width: targetWidth, maxHeight };
}

const planFixedStyle = computed(() => {
    if (planPlacement.value === "top") {
        return {
            position: "fixed" as const,
            bottom: `${Math.max(10, window.innerHeight - planCoords.value.top)}px`,
            left: `${planCoords.value.left}px`,
            width: `${planCoords.value.width}px`,
            maxHeight: `${planCoords.value.maxHeight}px`,
            zIndex: 99999,
        };
    }
    return {
        position: "fixed" as const,
        top: `${planCoords.value.top}px`,
        left: `${planCoords.value.left}px`,
        width: `${planCoords.value.width}px`,
        maxHeight: `${planCoords.value.maxHeight}px`,
        zIndex: 99999,
    };
});

function toggleModelMenu() {
    if (props.isRunning) return;
    if (!modelMenuOpen.value) {
        void loadAllRoleCards();
        calculatePopoverPosition(modelPickerRef.value);
        budgetMenuOpen.value = false;
        planMenuOpen.value = false;
        modelMenuOpen.value = true;
    } else {
        modelMenuOpen.value = false;
    }
}

function handleComposerClick(e: MouseEvent) {
    const target = e.target as HTMLElement | null;
    if (
        target &&
        !target.closest("button") &&
        !target.closest(".model-routing-popover") &&
        !target.closest(".budget-dropdown-menu") &&
        !target.closest(".plan-dropdown-menu")
    ) {
        textareaRef.value?.focus();
    }
}

function toggleBudgetMenu() {
    if (props.isRunning) return;
    if (!budgetMenuOpen.value) {
        calculateBudgetPosition(budgetPickerRef.value);
        modelMenuOpen.value = false;
        planMenuOpen.value = false;
        budgetMenuOpen.value = true;
    } else {
        budgetMenuOpen.value = false;
    }
}

function togglePlanMenu() {
    if (props.isRunning) return;
    if (!planMenuOpen.value) {
        calculatePlanPosition(planPickerRef.value);
        modelMenuOpen.value = false;
        budgetMenuOpen.value = false;
        planMenuOpen.value = true;
    } else {
        planMenuOpen.value = false;
    }
}

const reasoningEffortOptions: Array<{ value: ReasoningEffort; label: string; tooltip: string }> = [
    { value: "auto", label: "自", tooltip: "自动：按模型自适应" },
    { value: "off", label: "关", tooltip: "关闭思考" },
    { value: "low", label: "低", tooltip: "低思考：更快、更省 Token" },
    { value: "medium", label: "中", tooltip: "中思考：平衡效率与质量" },
    { value: "high", label: "高", tooltip: "高思考：深入推理与规划" },
];

function updateRouteValue(role: "pro" | "flash" | "vision", modelId: string) {
    emit("update:model-routing", {
        ...props.modelRouting,
        [role]: modelId,
    });
}

function setReasoningEffort(role: "pro" | "flash" | "vision", value: ReasoningEffort) {
    const field = `${role}_reasoning_effort` as const;
    emit("update:model-routing", {
        ...props.modelRouting,
        [field]: value,
    });
}

const routingMode = computed(() => (props.modelRouting as { routing_mode?: string }).routing_mode || "rules");

function setRoutingMode(mode: "rules" | "scored") {
    emit("update:model-routing", {
        ...props.modelRouting,
        routing_mode: mode,
    } as ModelRouting);
}

function syncAllToPro() {
    emit("update:model-routing", {
        ...props.modelRouting,
        flash: props.modelRouting.pro,
        vision: props.modelRouting.pro,
    });
}

function selectBudgetMode(mode: ContextBudgetMode) {
    emit("update:budget-mode", mode);
    emit("update:model-routing", {
        ...props.modelRouting,
        context_budget_mode: mode,
    });
    budgetMenuOpen.value = false;
}

function handleClickOutside(e: MouseEvent) {
    const target = e.target as Node;
    // 如果点击的是被 Teleport 挂载到 body 上的模型下拉选择浮层，不要关闭外部的模型配置弹窗
    if (target instanceof Element && (target.closest('.model-dropdown-popover') || target.closest('.is-teleported'))) {
        return;
    }
    if (budgetPickerRef.value && !budgetPickerRef.value.contains(target)) {
        budgetMenuOpen.value = false;
    }
    if (modelPickerRef.value && !modelPickerRef.value.contains(target)) {
        modelMenuOpen.value = false;
    }
    if (planPickerRef.value && !planPickerRef.value.contains(target)) {
        planMenuOpen.value = false;
    }
}

function handleKeyDown(e: KeyboardEvent) {
    if (e.key === "Escape") {
        if (modelMenuOpen.value) modelMenuOpen.value = false;
        if (budgetMenuOpen.value) budgetMenuOpen.value = false;
        if (planMenuOpen.value) planMenuOpen.value = false;
    }
}

function handleScrollOrResize() {
    if (modelMenuOpen.value) calculatePopoverPosition(modelPickerRef.value);
    if (budgetMenuOpen.value) calculateBudgetPosition(budgetPickerRef.value);
    if (planMenuOpen.value) calculatePlanPosition(planPickerRef.value);
}

onMounted(() => {
    document.addEventListener("click", handleClickOutside, true);
    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("scroll", handleScrollOrResize, true);
    window.addEventListener("resize", handleScrollOrResize);
    void loadAllRoleCards();
});
onBeforeUnmount(() => {
    document.removeEventListener("click", handleClickOutside, true);
    window.removeEventListener("keydown", handleKeyDown);
    window.removeEventListener("scroll", handleScrollOrResize, true);
    window.removeEventListener("resize", handleScrollOrResize);
});

function autoResizeTextarea() {
    nextTick(() => {
        const el = textareaRef.value;
        if (!el) return;
        el.style.height = "auto";
        el.style.height = `${Math.min(Math.max(el.scrollHeight, 40), 160)}px`;
    });
}

function handleInput(e: Event) {
    const val = (e.target as HTMLTextAreaElement).value;
    emit("update:draft", val);
    autoResizeTextarea();
}

watch(() => props.draft, autoResizeTextarea);
onMounted(autoResizeTextarea);

const currentModelName = computed(() => {
    const proId = props.modelRouting?.pro;
    const model = props.allModels.find((m) => m.id === proId);
    return model?.display_name || model?.name || "Manus Desktop";
});
const placeholderText = computed(() => props.isRunning
    ? props.isWaitingForHuman
        ? "补充自定义说明后按回车提交…"
        : (submitMode.value === "queue"
            ? "输入后续任务，将在当前任务执行完毕后自动开始…"
            : "输入新任务，立即中断当前任务并以此为新目标执行…")
    : "想让 Agent 帮你做什么？",
);
const canSubmit = computed(() => {
    return Boolean(props.draft.trim()) || attachments.value.length > 0;
});

function submit(explicitMode?: "interrupt" | "queue" | Event) {
    if (!canSubmit.value || props.isSending) return;

    let userText = props.draft.trim();
    const currentAtts = [...attachments.value];
    const mode = (typeof explicitMode === "string" ? explicitMode : undefined)
        || (props.isRunning && !props.isWaitingForHuman ? submitMode.value : undefined);

    if (currentAtts.length > 0) {
        // 🌟 结构化排列附件清单：清晰标注文件名、大小与本地物理路径
        const briefBadges = currentAtts
            .map((a) => `📎 [附件: ${a.file_name} (${formatFileSize(a.size_bytes)}) | 路径: ${a.file_path}]`)
            .join("\n");
        const visiblePrompt = userText ? `${briefBadges}\n\n${userText}` : briefBadges;
        emit("update:draft", "");
        attachments.value = [];
        emit("submit", { text: visiblePrompt, attachments: currentAtts, mode });
    } else {
        emit("update:draft", "");
        emit("submit", { text: userText, attachments: [], mode });
    }
}

const contextLevelClass = computed(() => {
    const p = props.telemetry?.contextUsagePercent ?? 0;
    if (p >= 80) return "badge-danger";
    if (p >= 60) return "badge-warn";
    return "badge-safe";
});
</script>

<style scoped src="./agent-composer.css"></style>
