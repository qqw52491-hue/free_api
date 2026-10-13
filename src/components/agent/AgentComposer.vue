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
            <!-- 🌟 顶部槽位：文件改动预览栏 (Progress Deck) -->
            <slot name="deck" />

            <!-- 📎 附件缩略图与文件卡片预览托盘 (Attachment Tray) -->
            <div v-if="attachments.length > 0" class="composer-attachments-tray">
                <AttachmentCard
                    v-for="(item, idx) in attachments"
                    :key="idx"
                    :attachment="item"
                    :removable="true"
                    @preview-image="openImagePreview"
                    @preview-doc="openDocPreview"
                    @unpack="unpackSnippet"
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
                @paste="handlePaste"
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

                                <!-- 🌟 顶部工具条：独立 4 工位 Tab + 横排分流模式开关 -->
                                <div class="routing-top-toolbar">
                                    <div class="routing-tabs-nav station-tabs-nav">
                                        <button
                                            type="button"
                                            class="routing-tab-btn"
                                            :class="{ active: activeRoutingTab === 'brain' }"
                                            @click="activeRoutingTab = 'brain'"
                                        >
                                            <span class="tab-btn-icon">🧠</span>
                                            <span class="tab-btn-text">主脑</span>
                                        </button>
                                        <button
                                            type="button"
                                            class="routing-tab-btn"
                                            :class="{ active: activeRoutingTab === 'writer' }"
                                            @click="activeRoutingTab = 'writer'"
                                        >
                                            <span class="tab-btn-icon">✍️</span>
                                            <span class="tab-btn-text">写入</span>
                                            <span v-if="writerRoleCards.length > 0" class="tab-count-badge" :class="{ 'is-custom': isWriterCustom }">
                                                {{ writerActiveCardsCount }}/{{ writerRoleCards.length }}
                                            </span>
                                        </button>
                                        <button
                                            type="button"
                                            class="routing-tab-btn"
                                            :class="{ active: activeRoutingTab === 'reviewer' }"
                                            @click="activeRoutingTab = 'reviewer'"
                                        >
                                            <span class="tab-btn-icon">🛡️</span>
                                            <span class="tab-btn-text">审查</span>
                                            <span v-if="isStageReviewerDisabled" class="tab-count-badge disabled">免审</span>
                                            <span v-else-if="reviewerRoleCards.length > 0" class="tab-count-badge" :class="{ 'is-custom': isReviewerCustom }">
                                                {{ reviewerActiveCardsCount }}/{{ reviewerRoleCards.length }}
                                            </span>
                                        </button>
                                        <button
                                            type="button"
                                            class="routing-tab-btn"
                                            :class="{ active: activeRoutingTab === 'scout' }"
                                            @click="activeRoutingTab = 'scout'"
                                        >
                                            <span class="tab-btn-icon">🔎</span>
                                            <span class="tab-btn-text">探针</span>
                                            <span v-if="scoutRoleCards.length > 0" class="tab-count-badge" :class="{ 'is-custom': isScoutCustom }">
                                                {{ scoutActiveCardsCount }}/{{ scoutRoleCards.length }}
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
                                    <!-- Station 1: 🧠 主脑算力基线 (推理规划、极速执行、视觉模型) -->
                                    <div v-if="activeRoutingTab === 'brain'" class="tab-pane-content models-unified-pane">
                                        <!-- 1. 推理规划模型 (Pro / 模型 1) -->
                                        <div class="route-item pro-tier">
                                            <div class="route-item-head">
                                                <div class="head-left">
                                                    <span class="route-badge pro">🧠 Pro：推理规划</span>
                                                    <small>长程规划与深层自愈救场</small>
                                                </div>
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
                                                    <span class="route-badge flash">⚡ Flash：极速执行</span>
                                                    <small>高频工具调用与快速推进</small>
                                                </div>
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
                                    </div>

                                    <!-- Station 2: ✍️ 改码写入工位 (Writer) -->
                                    <div v-else-if="activeRoutingTab === 'writer'" class="tab-pane-content station-pane">
                                        <div class="squad-section station-section">
                                            <div class="section-head">
                                                <div class="section-title-wrap">
                                                    <div class="section-title-row">
                                                        <span class="section-title">✍️ 改码写入工位 (Writer)</span>
                                                        <span
                                                            class="section-badge-count"
                                                            :class="{ 'is-custom': isWriterCustom }"
                                                        >
                                                            {{ isWriterCustom ? `出战: ${writerActiveCardsCount} / ${writerRoleCards.length}` : `🌟 全员就绪 (${writerRoleCards.length})` }}
                                                        </span>
                                                    </div>
                                                    <span class="section-desc">
                                                        负责编写与修改代码。此处排除角色仅在改码生效，绝不影响审查与探针。
                                                    </span>
                                                </div>
                                                <div class="squad-head-actions">
                                                    <button
                                                        v-if="isWriterCustom"
                                                        type="button"
                                                        class="squad-action-pill-btn reset"
                                                        title="恢复该工种全部专家出战"
                                                        :disabled="isRunning"
                                                        @click="resetSlotToAllActive('writer')"
                                                    >
                                                        <span>🌟 恢复全员</span>
                                                    </button>
                                                    <button
                                                        v-else
                                                        type="button"
                                                        class="squad-action-pill-btn exclude-all"
                                                        title="改码写入排除所有角色，本次任务改码直接使用工位模型"
                                                        :disabled="isRunning"
                                                        @click="hibernateAllCardsForSlot('writer')"
                                                    >
                                                        <span>🚫 全部排除</span>
                                                    </button>
                                                    <button
                                                        type="button"
                                                        class="manage-roles-link-btn"
                                                        title="打开 Agent 专家工坊"
                                                        @click="openRoleManager"
                                                    >
                                                        <span>工坊管理</span>
                                                        <svg viewBox="0 0 24 24" class="link-icon"><path d="m9 18 6-6-6-6"/></svg>
                                                    </button>
                                                </div>
                                            </div>

                                            <!-- 💻 该工位绑定算力 -->
                                            <div class="station-model-bar">
                                                <div class="station-model-label">
                                                    <span class="bar-icon">💻</span>
                                                    <span>写入算力:</span>
                                                </div>
                                                <div class="station-model-picker">
                                                    <button
                                                        type="button"
                                                        class="slot-choice-pill"
                                                        :class="{ active: getSlotChoice('writer') === 'pro' }"
                                                        :title="`指定使用 Pro: ${getModelDisplayName(modelRouting.pro)}`"
                                                        :disabled="isRunning"
                                                        @click="setSlotChoice('writer', 'pro')"
                                                    >
                                                        🧠 Pro
                                                    </button>
                                                    <button
                                                        type="button"
                                                        class="slot-choice-pill"
                                                        :class="{ active: getSlotChoice('writer') === 'flash' || !getSlotChoice('writer') }"
                                                        :title="`指定使用 Flash: ${getModelDisplayName(modelRouting.flash)}`"
                                                        :disabled="isRunning"
                                                        @click="setSlotChoice('writer', 'flash')"
                                                    >
                                                        ⚡ Flash
                                                    </button>
                                                    <select
                                                        class="slot-model-select"
                                                        :class="{ active: isCustomSpecificModel('writer') }"
                                                        :disabled="isRunning"
                                                        :value="isCustomSpecificModel('writer') ? getSlotChoice('writer') : ''"
                                                        @change="onSlotSelectChange('writer', ($event.target as HTMLSelectElement).value)"
                                                    >
                                                        <option value="">{{ isCustomSpecificModel('writer') ? '✕ 恢复默认' : '更多模型…' }}</option>
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

                                            <!-- 写入专家卡片网格 -->
                                            <div v-if="writerRoleCards.length > 0" class="squad-cards-grid">
                                                <div
                                                    v-for="card in writerRoleCards"
                                                    :key="card.id"
                                                    class="squad-member-card"
                                                    :class="{
                                                        'is-active': !isCardHibernatedForSlot(card.name, 'writer'),
                                                        'is-hibernated': isCardHibernatedForSlot(card.name, 'writer'),
                                                    }"
                                                    :title="`${card.label || card.name} (${card.name})\n${card.persona_prompt || card.scope_hints || '通用改码写入角色卡'}\n${card.hard_scope ? '围栏: ' + card.hard_scope : ''}\n${card.is_system ? '系统基础卡常驻，不可休眠' : '点击切换改码出战/排除'}`" @click="toggleSessionCardForSlot(card.name, 'writer')"
                                                >
                                                    <div class="member-card-header">
                                                        <div class="member-identity">
                                                            <div
                                                                class="member-checkbox"
                                                                :class="{ checked: !isCardHibernatedForSlot(card.name, 'writer') }"
                                                            >
                                                                <svg v-if="!isCardHibernatedForSlot(card.name, 'writer')" viewBox="0 0 24 24" class="check-icon"><path d="M20 6 9 17l-5-5"/></svg>
                                                            </div>
                                                            <div class="member-name-block">
                                                                <strong class="member-label">{{ card.label || card.name }}{{ card.is_system ? ' · 常驻基础卡' : '' }}</strong>
                                                                <span class="member-slug">{{ card.name }}</span>
                                                            </div>
                                                        </div>
                                                        <button
                                                            type="button"
                                                            class="session-toggle-btn"
                                                            :class="{ active: !isCardHibernatedForSlot(card.name, 'writer'), hibernated: isCardHibernatedForSlot(card.name, 'writer') }"
                                                            :title="card.is_system ? '系统基础卡常驻，不可休眠' : (isCardHibernatedForSlot(card.name, 'writer') ? '点击恢复改码出战' : '点击在写入中排除')"
                                                            :disabled="isRunning || card.is_system"
                                                             @click.stop="toggleSessionCardForSlot(card.name, 'writer')"
                                                        >
                                                            <span class="status-indicator"></span>
                                                            <span>{{ isCardHibernatedForSlot(card.name, 'writer') ? '✕ 排除' : '✓ 出战' }}</span>
                                                        </button>
                                                    </div>

                                                    <div class="member-badges-row">
                                                        <div class="member-model-tag" :title="card.model_id ? `专属锁定: ${getCardModelName(card.model_id)}` : '跟随写入算力'">
                                                            <span class="model-icon">🎯</span>
                                                            <span class="model-text">{{ card.model_id ? getCardModelName(card.model_id) : '跟随算力' }}</span>
                                                        </div>
                                                        <span v-if="card.hard_scope" class="scope-fence-badge" :title="`物理写入围栏: ${card.hard_scope}`">
                                                            🚧 {{ card.hard_scope }}
                                                        </span>
                                                        <span v-if="card.default_test_command" class="scope-fence-badge" :title="`默认测试命令: ${card.default_test_command}`">
                                                            🧪 {{ card.default_test_command }}
                                                        </span>
                                                    </div>
                                                </div>
                                            </div>

                                            <div v-else class="squad-empty-box">
                                                <span class="empty-icon">✍️</span>
                                                <div class="empty-text">
                                                    <strong>暂无改码写入专属角色卡</strong>
                                                    <p>当前改码写入使用基础工位模型。可前往专家工坊创建专精前端组件、后端 API 等专精改码角色。</p>
                                                </div>
                                                <button
                                                    type="button"
                                                    class="load-preset-btn"
                                                    @click="openRoleManager"
                                                >
                                                    👉 前往「Agent 专家工坊」管理角色卡
                                                </button>
                                            </div>
                                        </div>
                                    </div>

                                    <!-- Station 3: 🛡️ 阶段审查工位 (Reviewer) -->
                                    <div v-else-if="activeRoutingTab === 'reviewer'" class="tab-pane-content station-pane">
                                        <div class="squad-section gate-section">
                                            <div class="section-head">
                                                <div class="section-title-wrap">
                                                    <span class="section-title">🛡️ 阶段门禁策略</span>
                                                    <span class="section-desc">阶段推进防翻车把关规则 (会话独立)</span>
                                                </div>
                                                <div
                                                    class="gate-current-status-tag"
                                                    :class="{
                                                        'status-disabled': isStageReviewerDisabled,
                                                        'status-committee': !isStageReviewerDisabled && isCommitteeMode,
                                                        'status-default': !isStageReviewerDisabled && !isCommitteeMode,
                                                    }"
                                                >
                                                    <span v-if="isStageReviewerDisabled">⚡ 免审直通</span>
                                                    <span v-else-if="isCommitteeMode">👥 联合会审 ({{ activeReviewerCards.length }})</span>
                                                    <span v-else-if="activeReviewerCards.length === 0">🌐 异构对抗兜底</span>
                                                    <span v-else-if="activeReviewerCards.length === 1">🎭 专属审查专家</span>
                                                    <span v-else>🎯 智能自适应 ({{ activeReviewerCards.length }})</span>
                                                </div>
                                            </div>

                                            <div class="gate-controls-row">
                                                <div class="gate-config-group">
                                                    <span class="gate-group-label">触发策略 (Trigger):</span>
                                                    <div class="gate-mode-pills">
                                                        <button
                                                            type="button"
                                                            class="gate-mode-pill-btn"
                                                            :class="{ active: currentPlanMode === 'auto' && !isStageReviewerDisabled }"
                                                            title="智能风控：按策略自动决定是否审查（高危才审）"
                                                            :disabled="isRunning"
                                                            @click="setTriggerMode('auto')"
                                                        >
                                                            <span class="btn-icon">🎯</span>
                                                            <span>智能风控</span>
                                                        </button>
                                                        <button
                                                            type="button"
                                                            class="gate-mode-pill-btn"
                                                            :class="{ active: currentPlanMode === 'cross_review' && !isStageReviewerDisabled }"
                                                            title="严格把关：强制必定触发专家审查（无视风控策略）"
                                                            :disabled="isRunning"
                                                            @click="setTriggerMode('cross_review')"
                                                        >
                                                            <span class="btn-icon">🛡️</span>
                                                            <span>严格把关</span>
                                                        </button>
                                                        <button
                                                            type="button"
                                                            class="gate-mode-pill-btn btn-disabled"
                                                            :class="{ active: isStageReviewerDisabled }"
                                                            title="极速免审：永久跳过审查"
                                                            :disabled="isRunning"
                                                            @click="setTriggerMode('disabled')"
                                                        >
                                                            <span class="btn-icon">⚡</span>
                                                            <span>极速免审</span>
                                                        </button>
                                                    </div>
                                                </div>
                                                
                                                <div class="gate-config-group" :class="{ 'is-dimmed': isStageReviewerDisabled }">
                                                    <span class="gate-group-label">并发策略 (Concurrency):</span>
                                                    <div class="gate-mode-pills">
                                                        <button
                                                            type="button"
                                                            class="gate-mode-pill-btn"
                                                            :class="{ active: !isCommitteeMode }"
                                                            title="智能挑单：如果触发审查，则自动挑选 1 位最匹配的专家"
                                                            :disabled="isRunning || isStageReviewerDisabled"
                                                            @click="setConcurrencyMode('adaptive')"
                                                        >
                                                            <span class="btn-icon">👤</span>
                                                            <span>智能挑单 (单审)</span>
                                                        </button>
                                                        <button
                                                            type="button"
                                                            class="gate-mode-pill-btn btn-committee"
                                                            :class="{ active: isCommitteeMode }"
                                                            title="联合会审：如果触发审查，则并发唤起至多 3 位专家会审"
                                                            :disabled="isRunning || isStageReviewerDisabled"
                                                            @click="setConcurrencyMode('committee')"
                                                        >
                                                            <span class="btn-icon">👥</span>
                                                            <span>联合会审 (多审)</span>
                                                        </button>
                                                    </div>
                                                </div>

                                                <!-- 动态策略说明条 -->
                                                <div v-if="isStageReviewerDisabled" class="gate-policy-banner disabled">
                                                    ⚡ <strong>免审直通已生效</strong>：代码写入后跳过阶段审查直接推进（适合轻量原型或文档）。
                                                </div>
                                                <div v-else-if="isCommitteeMode" class="gate-policy-banner committee">
                                                    👥 <strong>联合会审模式已开启</strong>：阶段推进时并发唤起匹配专家（至多 3 位）独立质检、一票否决汇总，仅计 1 轮审查预算。
                                                </div>
                                                <div v-else-if="activeReviewerCards.length === 0" class="gate-policy-banner default">
                                                    🌐 <strong>异构对抗兜底生效中（跨模型家族严审）</strong>：未启用质检专家时，由系统跨模型家族严格交叉把关。
                                                </div>
                                                <div v-else-if="activeReviewerCards.length === 1" class="gate-policy-banner custom">
                                                    🎭 <strong>审查专家已挂载</strong>：「{{ activeReviewerCards[0].label || activeReviewerCards[0].name }}」{{ activeReviewerCards[0].model_id ? `（锁定 ${getCardModelName(activeReviewerCards[0].model_id)}）` : '（跟随工位算力）' }}：阶段推进时负责把关。
                                                </div>
                                                <div v-else class="gate-policy-banner adaptive">
                                                    🎯 <strong>智能自适应质检就绪</strong>：已启用 {{ activeReviewerCards.length }} 位审查专家，阶段推进时按改动代码类型自动挑选 1 位把关。
                                                </div>
                                            </div>
                                        </div>

                                        <div class="squad-section station-section">
                                            <div class="section-head">
                                                <div class="section-title-wrap">
                                                    <div class="section-title-row">
                                                        <span class="section-title">👥 审查专家库与算力 (Reviewer)</span>
                                                        <span
                                                            class="section-badge-count"
                                                            :class="{ 'is-custom': isReviewerCustom }"
                                                        >
                                                            {{ isReviewerCustom ? `出战: ${reviewerActiveCardsCount} / ${reviewerRoleCards.length}` : `🌟 全员就绪 (${reviewerRoleCards.length})` }}
                                                        </span>
                                                    </div>
                                                    <span class="section-desc">
                                                        负责阶段交付物验收。独立启用/排除审查角色，绝不影响改码写入与调研探针。
                                                    </span>
                                                </div>
                                                <div class="squad-head-actions">
                                                    <button
                                                        v-if="isReviewerCustom"
                                                        type="button"
                                                        class="squad-action-pill-btn reset"
                                                        title="恢复该工种全部专家出战"
                                                        :disabled="isRunning"
                                                        @click="resetSlotToAllActive('reviewer')"
                                                    >
                                                        <span>🌟 恢复全员</span>
                                                    </button>
                                                    <button
                                                        v-else
                                                        type="button"
                                                        class="squad-action-pill-btn exclude-all"
                                                        title="审查排除所有角色，阶段验收由系统异构模型兜底"
                                                        :disabled="isRunning"
                                                        @click="hibernateAllCardsForSlot('reviewer')"
                                                    >
                                                        <span>🚫 全部排除</span>
                                                    </button>
                                                    <button
                                                        type="button"
                                                        class="manage-roles-link-btn"
                                                        title="打开 Agent 专家工坊"
                                                        @click="openRoleManager"
                                                    >
                                                        <span>工坊管理</span>
                                                        <svg viewBox="0 0 24 24" class="link-icon"><path d="m9 18 6-6-6-6"/></svg>
                                                    </button>
                                                </div>
                                            </div>

                                            <!-- 💻 该工位绑定算力 -->
                                            <div class="station-model-bar">
                                                <div class="station-model-label">
                                                    <span class="bar-icon">💻</span>
                                                    <span>审查算力:</span>
                                                </div>
                                                <div class="station-model-picker">
                                                    <button
                                                        type="button"
                                                        class="slot-choice-pill heter"
                                                        :class="{ active: !getSlotChoice('reviewer') }"
                                                        title="系统异构对抗兜底：自动选择跨模型家族严审"
                                                        :disabled="isRunning"
                                                        @click="setSlotChoice('reviewer', '')"
                                                    >
                                                        🌐 异构兜底
                                                    </button>
                                                    <button
                                                        type="button"
                                                        class="slot-choice-pill"
                                                        :class="{ active: getSlotChoice('reviewer') === 'pro' }"
                                                        :title="`指定使用 Pro: ${getModelDisplayName(modelRouting.pro)}`"
                                                        :disabled="isRunning"
                                                        @click="setSlotChoice('reviewer', 'pro')"
                                                    >
                                                        🧠 Pro
                                                    </button>
                                                    <button
                                                        type="button"
                                                        class="slot-choice-pill"
                                                        :class="{ active: getSlotChoice('reviewer') === 'flash' }"
                                                        :title="`指定使用 Flash: ${getModelDisplayName(modelRouting.flash)}`"
                                                        :disabled="isRunning"
                                                        @click="setSlotChoice('reviewer', 'flash')"
                                                    >
                                                        ⚡ Flash
                                                    </button>
                                                    <select
                                                        class="slot-model-select"
                                                        :class="{ active: isCustomSpecificModel('reviewer') }"
                                                        :disabled="isRunning"
                                                        :value="isCustomSpecificModel('reviewer') ? getSlotChoice('reviewer') : ''"
                                                        @change="onSlotSelectChange('reviewer', ($event.target as HTMLSelectElement).value)"
                                                    >
                                                        <option value="">{{ isCustomSpecificModel('reviewer') ? '✕ 恢复默认' : '更多模型…' }}</option>
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

                                            <!-- 审查卡片网格 -->
                                            <div v-if="reviewerRoleCards.length > 0" class="squad-cards-grid">
                                                <div
                                                    v-for="card in reviewerRoleCards"
                                                    :key="card.id"
                                                    class="squad-member-card"
                                                    :class="{
                                                        'is-active': !isCardHibernatedForSlot(card.name, 'reviewer'),
                                                        'is-hibernated': isCardHibernatedForSlot(card.name, 'reviewer'),
                                                    }"
                                                    :title="`${card.label || card.name} (${card.name})\n${card.persona_prompt || card.scope_hints || '通用质检验收角色卡'}\n${card.focus_domains ? '材料: ' + card.focus_domains : ''}\n${card.is_system ? '系统基础卡常驻，不可休眠' : '点击切换审查出战/排除'}`" @click="toggleSessionCardForSlot(card.name, 'reviewer')"
                                                >
                                                    <div class="member-card-header">
                                                        <div class="member-identity">
                                                            <div
                                                                class="member-checkbox"
                                                                :class="{ checked: !isCardHibernatedForSlot(card.name, 'reviewer') }"
                                                            >
                                                                <svg v-if="!isCardHibernatedForSlot(card.name, 'reviewer')" viewBox="0 0 24 24" class="check-icon"><path d="M20 6 9 17l-5-5"/></svg>
                                                            </div>
                                                            <div class="member-name-block">
                                                                <strong class="member-label">{{ card.label || card.name }}{{ card.is_system ? ' · 常驻基础卡' : '' }}</strong>
                                                                <span class="member-slug">{{ card.name }}</span>
                                                            </div>
                                                        </div>
                                                        <button
                                                            type="button"
                                                            class="session-toggle-btn"
                                                            :class="{ active: !isCardHibernatedForSlot(card.name, 'reviewer'), hibernated: isCardHibernatedForSlot(card.name, 'reviewer') }"
                                                            :title="card.is_system ? '系统基础卡常驻，不可休眠' : (isCardHibernatedForSlot(card.name, 'reviewer') ? '点击恢复审查出战' : '点击在此工位排除')"
                                                            :disabled="isRunning || card.is_system"
                                                             @click.stop="toggleSessionCardForSlot(card.name, 'reviewer')"
                                                        >
                                                            <span class="status-indicator"></span>
                                                            <span>{{ isCardHibernatedForSlot(card.name, 'reviewer') ? '✕ 排除' : '✓ 出战' }}</span>
                                                        </button>
                                                    </div>

                                                    <div class="member-badges-row">
                                                        <div class="member-model-tag" :title="card.model_id ? `专属锁定: ${getCardModelName(card.model_id)}` : '跟随审查算力'">
                                                            <span class="model-icon">🎯</span>
                                                            <span class="model-text">{{ card.model_id ? getCardModelName(card.model_id) : '跟随算力' }}</span>
                                                        </div>
                                                        <span v-if="card.focus_domains && card.focus_domains !== '[]'" class="scope-fence-badge" :title="`预装材料: ${card.focus_domains}`">
                                                            📦 预装材料
                                                        </span>
                                                        <span v-if="card.custom_schema" class="scope-fence-badge" title="定义了自定义契约字段">
                                                            📑 特化契约
                                                        </span>
                                                    </div>
                                                </div>
                                            </div>

                                            <div v-else class="squad-empty-box">
                                                <span class="empty-icon">🛡️</span>
                                                <div class="empty-text">
                                                    <strong>暂无审查验收专属角色卡</strong>
                                                    <p>当前阶段审查将使用跨模型家族异构对抗兜底。可前往专家工坊创建前端门禁官、后端架构审查等角色卡。</p>
                                                </div>
                                                <button
                                                    type="button"
                                                    class="load-preset-btn"
                                                    @click="openRoleManager"
                                                >
                                                    👉 前往「Agent 专家工坊」管理角色卡
                                                </button>
                                            </div>
                                        </div>
                                    </div>

                                    <!-- Station 4: 🔎 调研探针工位 (Scout / Worker) -->
                                    <div v-else-if="activeRoutingTab === 'scout'" class="tab-pane-content station-pane">
                                        <div class="squad-section station-section">
                                            <div class="section-head">
                                                <div class="section-title-wrap">
                                                    <div class="section-title-row">
                                                        <span class="section-title">🔎 调研探针工位 (Scout / Worker)</span>
                                                        <span
                                                            class="section-badge-count"
                                                            :class="{ 'is-custom': isScoutCustom }"
                                                        >
                                                            {{ isScoutCustom ? `出战: ${scoutActiveCardsCount} / ${scoutRoleCards.length}` : `🌟 全员就绪 (${scoutRoleCards.length})` }}
                                                        </span>
                                                    </div>
                                                    <span class="section-desc">
                                                        负责跨文件探索、长文本检索与只读探针。独立配置，不影响改码与审查。
                                                    </span>
                                                </div>
                                                <div class="squad-head-actions">
                                                    <button
                                                        v-if="isScoutCustom"
                                                        type="button"
                                                        class="squad-action-pill-btn reset"
                                                        title="恢复该工种全部专家出战"
                                                        :disabled="isRunning"
                                                        @click="resetSlotToAllActive('worker')"
                                                    >
                                                        <span>🌟 恢复全员</span>
                                                    </button>
                                                    <button
                                                        v-else
                                                        type="button"
                                                        class="squad-action-pill-btn exclude-all"
                                                        title="探针排除所有角色，本次任务探针直接使用工位模型"
                                                        :disabled="isRunning"
                                                        @click="hibernateAllCardsForSlot('worker')"
                                                    >
                                                        <span>🚫 全部排除</span>
                                                    </button>
                                                    <button
                                                        type="button"
                                                        class="manage-roles-link-btn"
                                                        title="打开 Agent 专家工坊"
                                                        @click="openRoleManager"
                                                    >
                                                        <span>工坊管理</span>
                                                        <svg viewBox="0 0 24 24" class="link-icon"><path d="m9 18 6-6-6-6"/></svg>
                                                    </button>
                                                </div>
                                            </div>

                                            <!-- 💻 该工位绑定算力 -->
                                            <div class="station-model-bar">
                                                <div class="station-model-label">
                                                    <span class="bar-icon">💻</span>
                                                    <span>探针算力:</span>
                                                </div>
                                                <div class="station-model-picker">
                                                    <button
                                                        type="button"
                                                        class="slot-choice-pill"
                                                        :class="{ active: getSlotChoice('scout') === 'pro' }"
                                                        :title="`指定使用 Pro: ${getModelDisplayName(modelRouting.pro)}`"
                                                        :disabled="isRunning"
                                                        @click="setSlotChoice('scout', 'pro')"
                                                    >
                                                        🧠 Pro
                                                    </button>
                                                    <button
                                                        type="button"
                                                        class="slot-choice-pill"
                                                        :class="{ active: getSlotChoice('scout') === 'flash' || !getSlotChoice('scout') }"
                                                        :title="`指定使用 Flash: ${getModelDisplayName(modelRouting.flash)}`"
                                                        :disabled="isRunning"
                                                        @click="setSlotChoice('scout', 'flash')"
                                                    >
                                                        ⚡ Flash
                                                    </button>
                                                    <select
                                                        class="slot-model-select"
                                                        :class="{ active: isCustomSpecificModel('scout') }"
                                                        :disabled="isRunning"
                                                        :value="isCustomSpecificModel('scout') ? getSlotChoice('scout') : ''"
                                                        @change="onSlotSelectChange('scout', ($event.target as HTMLSelectElement).value)"
                                                    >
                                                        <option value="">{{ isCustomSpecificModel('scout') ? '✕ 恢复默认' : '更多模型…' }}</option>
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

                                            <!-- 探针卡片网格 -->
                                            <div v-if="scoutRoleCards.length > 0" class="squad-cards-grid">
                                                <div
                                                    v-for="card in scoutRoleCards"
                                                    :key="card.id"
                                                    class="squad-member-card"
                                                    :class="{
                                                        'is-active': !isCardHibernatedForSlot(card.name, 'worker'),
                                                        'is-hibernated': isCardHibernatedForSlot(card.name, 'worker'),
                                                    }"
                                                    :title="`${card.label || card.name} (${card.name})\n${card.persona_prompt || card.scope_hints || '通用调研探针角色卡'}\n${card.hard_scope ? '围栏: ' + card.hard_scope : ''}\n${card.is_system ? '系统基础卡常驻，不可休眠' : '点击切换探针出战/排除'}`" @click="toggleSessionCardForSlot(card.name, 'worker')"
                                                >
                                                    <div class="member-card-header">
                                                        <div class="member-identity">
                                                            <div
                                                                class="member-checkbox"
                                                                :class="{ checked: !isCardHibernatedForSlot(card.name, 'worker') }"
                                                            >
                                                                <svg v-if="!isCardHibernatedForSlot(card.name, 'worker')" viewBox="0 0 24 24" class="check-icon"><path d="M20 6 9 17l-5-5"/></svg>
                                                            </div>
                                                            <div class="member-name-block">
                                                                <strong class="member-label">{{ card.label || card.name }}{{ card.is_system ? ' · 常驻基础卡' : '' }}</strong>
                                                                <span class="member-slug">{{ card.name }}</span>
                                                            </div>
                                                        </div>
                                                        <button
                                                            type="button"
                                                            class="session-toggle-btn"
                                                            :class="{ active: !isCardHibernatedForSlot(card.name, 'worker'), hibernated: isCardHibernatedForSlot(card.name, 'worker') }"
                                                            :title="card.is_system ? '系统基础卡常驻，不可休眠' : (isCardHibernatedForSlot(card.name, 'worker') ? '点击恢复探针出战' : '点击在此工位排除')"
                                                            :disabled="isRunning || card.is_system"
                                                             @click.stop="toggleSessionCardForSlot(card.name, 'worker')"
                                                        >
                                                            <span class="status-indicator"></span>
                                                            <span>{{ isCardHibernatedForSlot(card.name, 'worker') ? '✕ 排除' : '✓ 出战' }}</span>
                                                        </button>
                                                    </div>

                                                    <div class="member-badges-row">
                                                        <div class="member-model-tag" :title="card.model_id ? `专属锁定: ${getCardModelName(card.model_id)}` : '跟随探针算力'">
                                                            <span class="model-icon">🎯</span>
                                                            <span class="model-text">{{ card.model_id ? getCardModelName(card.model_id) : '跟随算力' }}</span>
                                                        </div>
                                                        <span v-if="card.hard_scope" class="scope-fence-badge" :title="`探索围栏: ${card.hard_scope}`">
                                                            🚧 {{ card.hard_scope }}
                                                        </span>
                                                    </div>
                                                </div>
                                            </div>

                                            <div v-else class="squad-empty-box">
                                                <span class="empty-icon">🔎</span>
                                                <div class="empty-text">
                                                    <strong>暂无调研探针专属角色卡</strong>
                                                    <p>当前调研探针使用基础工位模型。可前往专家工坊创建文档查阅、网络抓取、安全侦察等探针角色。</p>
                                                </div>
                                                <button
                                                    type="button"
                                                    class="load-preset-btn"
                                                    @click="openRoleManager"
                                                >
                                                    👉 前往「Agent 专家工坊」管理角色卡
                                                </button>
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
                                <div class="budget-menu-header" style="margin-top:6px;border-top:1px solid var(--border-color,rgba(128,128,128,.2));padding-top:6px;">状态卡携带量</div>
                                <button
                                    v-for="opt in cardLevels"
                                    :key="opt.id"
                                    type="button"
                                    class="budget-menu-item"
                                    :class="{ selected: opt.id === (modelRouting.state_card_level || 'auto') }"
                                    @click="selectCardLevel(opt.id)"
                                >
                                    <div class="opt-text">
                                        <div class="opt-title">
                                            <strong>{{ opt.label }}</strong>
                                        </div>
                                        <small class="opt-sub">{{ opt.description }}</small>
                                    </div>
                                    <svg v-if="opt.id === (modelRouting.state_card_level || 'auto')" class="check-icon" viewBox="0 0 24 24"><path d="M20 6 9 17l-5-5"/></svg>
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
                    <!-- 上下文水位圆环：最近一次调用 prompt 占窗口比（悬浮看分桶明细，静默态回显上次值） -->
                    <div
                        v-if="ringPercent !== null"
                        class="context-ring"
                        :class="contextLevelClass"
                        @mouseenter="contextDetailOpen = true"
                        @mouseleave="contextDetailOpen = false"
                    >
                        <svg viewBox="0 0 36 36" class="context-ring-svg">
                            <circle class="ring-track" cx="18" cy="18" r="15.9155" />
                            <circle
                                class="ring-fill"
                                cx="18" cy="18" r="15.9155"
                                :stroke-dasharray="`${Math.min(100, ringPercent)} 100`"
                            />
                        </svg>
                        <span class="ring-pct">{{ Math.round(ringPercent) }}%</span>
                        <div v-if="contextDetailOpen" class="context-detail-pop">
                            <div class="cd-title">上下文构成 · {{ formatTokens(ringTokens) }} tok</div>
                            <div v-for="row in ringBreakdown" :key="row.name" class="cd-row">
                                <span class="cd-name">{{ row.name }}</span>
                                <span class="cd-bar-wrap"><span class="cd-bar" :style="{ width: row.pct + '%' }"></span></span>
                                <span class="cd-val">{{ row.pct }}%</span>
                            </div>
                            <div v-if="ringBreakdown.length === 0" class="cd-empty">暂无分段数据（下一次调用后产生）</div>
                        </div>
                    </div>
                    <button v-if="isRunning" class="stop-control" type="button" :disabled="stopRequested" @click="$emit('stop')">{{ stopRequested ? '停止中' : '停止' }}</button>
                    
                    <!-- 任务意图是结构化选择，不通过“继续”等自然语言猜测。 -->
                    <button
                        v-if="!isRunning && canResumeTask"
                        type="button"
                        class="mode-switch-pill"
                        :title="stoppedTaskMode === 'resume' ? '保留原目标、方案和审查状态；点击改为新任务' : '开始新任务，重新分流；点击改为继续原任务'"
                        @click.stop="stoppedTaskMode = stoppedTaskMode === 'resume' ? 'new' : 'resume'"
                    >
                        {{ stoppedTaskMode === 'resume' ? '↪ 继续原任务' : '＋ 新任务' }}
                    </button>

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
                        :title="isRunning ? (isWaitingForHuman ? '回答当前请求' : (submitMode === 'interrupt' ? '⚡ 立即打断当前任务并切换为新目标' : '⏳ 加入队列，等当前任务完成后自动执行')) : (taskStartMode === 'resume' ? '继续原任务，保留审查状态' : '开始新任务')"
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
                <span class="t-val">{{ (telemetry?.lastToolDurationMs !== undefined && telemetry.lastToolDurationMs >= 0) ? formatMs(telemetry.lastToolDurationMs) : '--' }}</span>
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
            @unpack-doc="unpackSnippet"
            @update-doc="updateSnippet"
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
    STATE_CARD_LEVELS,
    type AgentHumanForm,
    type AgentModelOption,
    type AgentSessionTelemetry,
    type ContextBudgetMode,
    type ModelRouting,
    type PlanExecutionMode,
    type ReasoningEffort,
    type RoleCard,
    type StateCardLevel,
} from "./types";

const props = defineProps<{
    draft: string;
    sessionId: string;
    isRunning: boolean;
    isWaitingForHuman: boolean;
    isSending: boolean;
    stopRequested: boolean;
    canStart: boolean;
    canResumeTask?: boolean;
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
    submit: [payload?: { text: string; attachments: ParsedAttachment[]; mode?: "interrupt" | "queue"; taskStartMode?: "resume" | "new" }];
    "cancel-queue": [index: number];
    stop: [];
    "open-settings": [];
    "open-roles": [];
    "attach-files": [paths: string[]];
}>();

const stoppedTaskMode = ref<"resume" | "new">("resume");
watch(() => [props.sessionId, props.canResumeTask], () => { stoppedTaskMode.value = "resume"; });
const taskStartMode = computed<"resume" | "new" | undefined>(() =>
    props.isRunning ? undefined : props.canResumeTask ? stoppedTaskMode.value : undefined,
);

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
        case "plan_approval":
            return "方案审批";
        case "cross_review":
        case "auto":
        default:
            return "智能分发";
    }
});

const planModeTooltip = computed(() => {
    switch (currentPlanMode.value) {
        case "plan_approval":
            return "当前模式: 🛡️ 方案审批 (Plan First) - 方案就绪后须经您确认方可写入代码。点击切换";
        case "cross_review":
        case "auto":
        default:
            return "当前模式: 🚀 智能分发 (Auto Dispatch) - 执行流程全权交由 Agent 内部风控策略管理。点击切换";
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
        label: "智能分发",
        tag: "推荐默认",
        description: "默认推荐：执行流程全权交由内部审查卡片的风控策略管控",
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

    if (mode === "plan_approval") {
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
    is_pasted_snippet?: boolean;
    upload_failed?: boolean;
}

const attachments = ref<ParsedAttachment[]>([]);
const isDraggingOver = ref(false);
const previewImageModal = ref<ParsedAttachment | null>(null);
const previewDocModal = ref<ParsedAttachment | null>(null);

// 异步落盘任务登记簿：submit 前做有界等待，避免极速回车丢掉尚未完成的附件
const pendingAttachmentSaves = new Set<Promise<void>>();

function trackAttachmentSave(task: Promise<void>) {
    pendingAttachmentSaves.add(task);
    void task.finally(() => pendingAttachmentSaves.delete(task));
}

function readFileAsDataUrl(file: File): Promise<string | null> {
    return new Promise((resolve) => {
        const reader = new FileReader();
        reader.onload = (event) => resolve((event.target?.result as string) || null);
        reader.onerror = () => resolve(null);
        reader.readAsDataURL(file);
    });
}

// 后端契约：save_and_parse_uploaded_attachment(sessionId, fileName, base64Data)。
// 失败返回 null，由调用方决定降级形态——绝不在这里静默吞掉假装成功。
async function saveUploadedAttachment(fileName: string, base64Data: string): Promise<ParsedAttachment | null> {
    try {
        return await invoke<ParsedAttachment>("save_and_parse_uploaded_attachment", {
            sessionId: props.sessionId,
            fileName,
            base64Data,
        });
    } catch (err) {
        console.warn("附件落盘失败:", err);
        return null;
    }
}

// 落盘失败的图片仍可作为本条消息的多模态数据发送，但打 upload_failed 标记如实披露
function fallbackImageAttachment(fileName: string, sizeBytes: number, contentType: string, dataUrl: string): ParsedAttachment {
    return {
        file_name: fileName,
        file_path: fileName,
        size_bytes: sizeBytes,
        content_type: contentType,
        prompt_text: `【用户已上传图片附件: \`${fileName}\`】(格式: ${contentType}, 大小: ${(sizeBytes / 1024).toFixed(1)} KB)\n*(图片数据已作为视觉多模态消息携带；落盘未成功，历史记录中无法按路径回显)*\n`,
        image_data_url: dataUrl,
        raw_preview: null,
        upload_failed: true,
    };
}

function openImagePreview(item: ParsedAttachment) {
    previewImageModal.value = item;
}

function openDocPreview(item: ParsedAttachment) {
    previewDocModal.value = item;
}

function removeAttachment(index: number) {
    attachments.value.splice(index, 1);
}

async function attachFilePaths(paths: string[]): Promise<number> {
    const validPaths = paths.filter((p): p is string => typeof p === "string" && !!p);
    if (validPaths.length === 0) return 0;

    emit("attach-files", validPaths);
    let successCount = 0;

    for (const p of validPaths) {
        try {
            const parsed = await invoke<ParsedAttachment>("parse_attachment_file", { path: p });
            if (parsed) {
                attachments.value.push(parsed);
                successCount++;
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
            successCount++;
        }
    }
    return successCount;
}

async function promptAttachFiles() {
    try {
        const selected = await openDialog({
            multiple: true,
            title: "选择要附加到会话的文件 (图片、Excel、Word、PDF、代码...)",
        });
        if (!selected) return;
        const paths = Array.isArray(selected) ? selected : [selected];
        await attachFilePaths(paths);
    } catch (e) {
        console.error("选择文件失败:", e);
    }
}

async function handleFileDrop(e: DragEvent) {
    isDraggingOver.value = false;
    const files = e.dataTransfer?.files;
    if (!files || files.length === 0) return;

    for (const file of Array.from(files)) {
        const task = (async () => {
            const base64Data = await readFileAsDataUrl(file);
            if (!base64Data) return;
            const saved = await saveUploadedAttachment(file.name, base64Data);
            if (saved) {
                attachments.value.push({
                    ...saved,
                    image_data_url: saved.image_data_url ?? (file.type.startsWith("image/") ? base64Data : null),
                });
            } else if (file.type.startsWith("image/")) {
                // 图片仍可作为本条消息的多模态数据携带，但必须如实标记落盘失败
                attachments.value.push(fallbackImageAttachment(file.name, file.size, file.type || "image/png", base64Data));
            }
            // 非图片文件没有可携带的内存数据，落盘失败即不可用——不制造"看似成功"的裸名附件
        })();
        trackAttachmentSave(task);
    }
}

async function handlePaste(e: ClipboardEvent) {
    if (props.isSending) return;
    // WebKitGTK 下 clipboardData 可能为 null——不能直接 return，
    // 那正是需要走原生剪贴板回退的场景。
    const clipboard = e.clipboardData;
    // 诊断锚点：若粘贴仍失效，控制台这行能区分"事件未触发"与"剪贴板无图像数据"
    console.debug(
        "[paste] clipboardData types:",
        clipboard ? Array.from(clipboard.types) : null,
        "items:", clipboard?.items?.length ?? 0,
        "files:", clipboard?.files?.length ?? 0
    );

    // 1. 优先检查剪贴板是否携带图片 (截图或从资源管理器复制的图片文件)
    const imageFiles: File[] = [];
    const seenKeys = new Set<string>();
    const isImageFile = (f: File) => {
        if (!f) return false;
        if (f.type && f.type.startsWith("image/")) return true;
        if (f.name && /\.(png|jpe?g|gif|webp|bmp|svg)$/i.test(f.name)) return true;
        // 部分系统/工具截图无扩展名且类型为空，但有尺寸与 File 实例
        return !f.type && (!f.name || f.name === "image.png");
    };
    const addImageFile = (f: File | null) => {
        if (!f || !isImageFile(f)) return;
        const key = `${f.name || 'img'}_${f.size}_${f.lastModified || 0}`;
        if (seenKeys.has(key)) return;
        seenKeys.add(key);
        imageFiles.push(f);
    };

    if (clipboard) {
        // 1.1 优先提取 clipboard.items (现代浏览器截图优先在 items)
        if (clipboard.items && clipboard.items.length > 0) {
            for (let i = 0; i < clipboard.items.length; i++) {
                const item = clipboard.items[i];
                if (item.kind === "file" || item.type.startsWith("image/")) {
                    try {
                        const f = item.getAsFile();
                        if (f) addImageFile(f);
                    } catch (err) {
                        console.warn("从 clipboard.items 获取文件异常:", err);
                    }
                }
            }
        }

        // 1.2 补充/降级提取 clipboard.files
        if (clipboard.files && clipboard.files.length > 0) {
            for (let i = 0; i < clipboard.files.length; i++) {
                addImageFile(clipboard.files[i]);
            }
        }
    }

    if (imageFiles.length > 0) {
        e.preventDefault();
        for (const file of imageFiles) {
            const task = (async () => {
                const base64Data = await readFileAsDataUrl(file);
                if (!base64Data) return;
                const timestamp = new Date().toTimeString().split(" ")[0].replace(/:/g, "");
                const fileName = file.name && file.name !== "image.png" ? file.name : `screenshot_${timestamp}.png`;
                const contentType = file.type || "image/png";
                const saved = await saveUploadedAttachment(fileName, base64Data);
                attachments.value.push(saved
                    ? { ...saved, image_data_url: saved.image_data_url ?? base64Data }
                    : fallbackImageAttachment(fileName, file.size, contentType, base64Data));
            })();
            trackAttachmentSave(task);
        }
        return;
    }

    // 1.3 原生剪贴板回退：截图(image/*)与文件管理器复制(uri-list)都走这里。
    // 快照当前文本——默认粘贴可能已把 uri/路径文本插进输入框，命中原生数据后回滚。
    // （不能在 await 后才 preventDefault：那时默认动作早已执行完）
    const ta = textareaRef.value;
    const preValue = ta ? ta.value : props.draft;
    const caret = ta ? ta.selectionStart : preValue.length;
    try {
        const payload = await invoke<{ data_urls: string[]; file_paths: string[] } | null>("read_clipboard_image");
        if (payload && (payload.data_urls.length > 0 || payload.file_paths.length > 0)) {
            // 撤销默认粘贴刚插入的文本（uri-list 场景常见：路径被当文本塞进来）
            if (ta && ta.value !== preValue) {
                ta.value = preValue;
                emit("update:draft", preValue);
                await nextTick();
                ta.setSelectionRange(caret, caret);
            }
            for (const dataUrl of payload.data_urls) {
                const task = (async () => {
                    const mime = dataUrl.slice(5, dataUrl.indexOf(";")) || "image/png";
                    const ext = mime === "image/jpeg" ? "jpg" : (mime.split("/")[1] || "png");
                    const timestamp = new Date().toTimeString().split(" ")[0].replace(/:/g, "");
                    const fileName = `screenshot_${timestamp}.${ext}`;
                    const sizeBytes = Math.round((dataUrl.length * 3) / 4);
                    const saved = await saveUploadedAttachment(fileName, dataUrl);
                    attachments.value.push(saved
                        ? { ...saved, image_data_url: saved.image_data_url ?? dataUrl }
                        : fallbackImageAttachment(fileName, sizeBytes, mime, dataUrl));
                })();
                trackAttachmentSave(task);
            }
            // 文件管理器复制的文件复用附件解析管道（图片会生成缩略图卡片）
            if (payload.file_paths.length > 0) {
                await attachFilePaths(payload.file_paths);
            }
            return;
        }
    } catch (err) {
        console.debug("原生剪贴板图片回退检测跳过或未支持:", err);
    }

    // 2. 检查纯文本内容：短文本正常进入输入框光标处；长文本/大段代码智能折叠为虚拟卡片
    if (!clipboard) return;
    const text = clipboard.getData("text/plain");
    if (!text) return;

    const lines = text.split("\n").length;
    const charLen = text.length;

    // 智能判定分水岭：行数 >= 15 或 字符数 >= 500 时，折叠为虚拟文件保护输入框视野
    if (lines >= 15 || charLen >= 500) {
        e.preventDefault();

        // 智能嗅探文件类型扩展名
        let ext = "txt";
        const trimmed = text.trim();
        if ((trimmed.startsWith("{") && trimmed.endsWith("}")) || (trimmed.startsWith("[") && trimmed.endsWith("]"))) {
            try {
                JSON.parse(trimmed);
                ext = "json";
            } catch {
                // ignore
            }
        } else if (trimmed.startsWith("<!DOCTYPE html") || trimmed.startsWith("<html")) {
            ext = "html";
        } else if (trimmed.startsWith("# ") || trimmed.includes("\n## ")) {
            ext = "md";
        } else if (trimmed.includes("fn main(") || trimmed.includes("pub fn ") || trimmed.includes("impl ")) {
            ext = "rs";
        } else if (trimmed.includes("import React") || trimmed.includes("export default") || trimmed.includes("const ") || trimmed.includes("function ")) {
            ext = "ts";
        } else if (trimmed.includes("def ") && trimmed.includes(":\n")) {
            ext = "py";
        }

        const snippetIndex = attachments.value.filter((a) => a.is_pasted_snippet).length + 1;
        const fileName = ext === "json"
            ? `pasted_data_${snippetIndex}.json`
            : ext === "md"
                ? `pasted_notes_${snippetIndex}.md`
                : `pasted_snippet_${snippetIndex}.${ext}`;

        const sizeBytes = new Blob([text]).size;
        attachments.value.push({
            file_name: fileName,
            file_path: `virtual://pasted/${fileName}`,
            size_bytes: sizeBytes,
            content_type: ext === "json" ? "application/json" : "text/plain",
            prompt_text: text,
            image_data_url: null,
            raw_preview: text.slice(0, 500),
            is_pasted_snippet: true,
        });
    }
}

function unpackSnippet(item: ParsedAttachment) {
    const idx = attachments.value.findIndex((a) => a === item || (a.file_name === item.file_name && a.prompt_text === item.prompt_text));
    if (idx !== -1) {
        attachments.value.splice(idx, 1);
    }
    const textToInsert = item.prompt_text || item.raw_preview || "";
    if (!textToInsert) return;

    const el = textareaRef.value;
    if (el && typeof el.selectionStart === "number") {
        const start = el.selectionStart;
        const end = el.selectionEnd;
        const currentDraft = props.draft;
        const before = currentDraft.slice(0, start);
        const after = currentDraft.slice(end);
        const separator = before.length > 0 && !before.endsWith("\n") ? "\n\n" : "";
        const newDraft = before + separator + textToInsert + after;
        emit("update:draft", newDraft);
        nextTick(() => {
            el.focus();
            const newPos = start + separator.length + textToInsert.length;
            el.setSelectionRange(newPos, newPos);
            autoResizeTextarea();
        });
    } else {
        const currentDraft = props.draft.trim();
        const newDraft = currentDraft ? `${currentDraft}\n\n${textToInsert}` : textToInsert;
        emit("update:draft", newDraft);
        nextTick(() => {
            el?.focus();
            autoResizeTextarea();
        });
    }
}

function updateSnippet(item: ParsedAttachment, newText: string) {
    const target = attachments.value.find((a) => a === item || (a.file_name === item.file_name && a.file_path === item.file_path));
    if (target) {
        target.prompt_text = newText;
        target.size_bytes = new Blob([newText]).size;
        target.raw_preview = newText.slice(0, 500);
    }
}

const modelPickerRef = ref<HTMLElement | null>(null);
const modelMenuOpen = ref(false);
const activeRoutingTab = ref<"brain" | "writer" | "reviewer" | "scout">("brain");
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
    if (!modelId) {
        setSlotChoice(slotKey, slotKey === 'reviewer' ? '' : 'flash');
        return;
    }
    setSlotChoice(slotKey, modelId);
}

function openRoleManager() {
    modelMenuOpen.value = false;
    emit("open-roles");
}

function isBasicRoleCard(name: string): boolean {
    return allRoleCards.value.some((c) => c.name === name && c.is_system);
}

function isCardHibernatedForSlot(name: string, slot: string): boolean {
    if (isBasicRoleCard(name)) return false;
    const list = props.modelRouting?.disabled_role_cards || [];
    const nameLower = name.toLowerCase();
    const slotPrefix = `${slot.toLowerCase()}:`;
    return list.some((d) => {
        const dLower = d.toLowerCase();
        if (dLower === nameLower) return true;
        if (dLower.startsWith(slotPrefix)) {
            return dLower.slice(slotPrefix.length) === nameLower;
        }
        if ((slot.toLowerCase() === 'scout' && dLower.startsWith('worker:')) ||
            (slot.toLowerCase() === 'worker' && dLower.startsWith('scout:'))) {
            return dLower.split(':')[1] === nameLower;
        }
        return false;
    });
}

function toggleSessionCardForSlot(name: string, slot: string) {
    if (props.isRunning || isBasicRoleCard(name)) return;
    const currentList = [...(props.modelRouting?.disabled_role_cards || [])];
    const nameLower = name.toLowerCase();
    const slotKey = `${slot.toLowerCase()}:${nameLower}`;
    const isCurrentlyHibernated = isCardHibernatedForSlot(name, slot);

    const filteredList = currentList.filter((d) => {
        const dLower = d.toLowerCase();
        if (dLower === nameLower) return false;
        if (dLower === slotKey) return false;
        if (slot === 'worker' && dLower === `scout:${nameLower}`) return false;
        if (slot === 'scout' && dLower === `worker:${nameLower}`) return false;
        return true;
    });

    const wasLegacyGlobal = currentList.some((d) => d.toLowerCase() === nameLower);
    if (wasLegacyGlobal) {
        const allSlots = ["writer", "reviewer", "worker"];
        for (const s of allSlots) {
            if (s !== slot.toLowerCase()) {
                filteredList.push(`${s}:${nameLower}`);
            }
        }
    }

    if (!isCurrentlyHibernated) {
        filteredList.push(slotKey);
    }

    emit("update:model-routing", {
        ...props.modelRouting,
        disabled_role_cards: filteredList,
    });
}

const writerRoleCards = computed(() => {
    return allEnabledRoleCards.value.filter((c) => {
        const raw = (c.allowed_kinds || "*").toLowerCase();
        return raw === "*" || raw.includes("writer");
    });
});

const reviewerRoleCards = computed(() => {
    return allEnabledRoleCards.value.filter((c) => {
        const raw = (c.allowed_kinds || "*").toLowerCase();
        return raw === "*" || raw.includes("reviewer");
    });
});

const scoutRoleCards = computed(() => {
    return allEnabledRoleCards.value.filter((c) => {
        const raw = (c.allowed_kinds || "*").toLowerCase();
        return raw === "*" || raw.includes("worker") || raw.includes("scout");
    });
});

const writerActiveCardsCount = computed(() => {
    return writerRoleCards.value.filter((c) => !isCardHibernatedForSlot(c.name, 'writer')).length;
});

const reviewerActiveCardsCount = computed(() => {
    return reviewerRoleCards.value.filter((c) => !isCardHibernatedForSlot(c.name, 'reviewer')).length;
});

const scoutActiveCardsCount = computed(() => {
    return scoutRoleCards.value.filter((c) => !isCardHibernatedForSlot(c.name, 'worker')).length;
});

const isWriterCustom = computed(() => {
    return writerRoleCards.value.some((c) => isCardHibernatedForSlot(c.name, "writer"));
});

const isReviewerCustom = computed(() => {
    return reviewerRoleCards.value.some((c) => isCardHibernatedForSlot(c.name, "reviewer"));
});

const isScoutCustom = computed(() => {
    return scoutRoleCards.value.some((c) => isCardHibernatedForSlot(c.name, "worker"));
});

function resetSlotToAllActive(slot: string) {
    if (props.isRunning) return;
    const currentList = [...(props.modelRouting?.disabled_role_cards || [])];
    const slotPrefix = `${slot.toLowerCase()}:`;
    const filtered = currentList.filter((d) => {
        const dLower = d.toLowerCase();
        if (dLower.startsWith(slotPrefix)) return false;
        if (slot === 'scout' && dLower.startsWith('worker:')) return false;
        if (slot === 'worker' && dLower.startsWith('scout:')) return false;
        return true;
    });
    emit("update:model-routing", {
        ...props.modelRouting,
        disabled_role_cards: filtered,
    });
}

function hibernateAllCardsForSlot(slot: string) {
    if (props.isRunning) return;
    const currentList = [...(props.modelRouting?.disabled_role_cards || [])];
    const cards = slot === 'writer' ? writerRoleCards.value : slot === 'reviewer' ? reviewerRoleCards.value : scoutRoleCards.value;
    const slotPrefix = `${slot.toLowerCase()}:`;
    for (const card of cards) {
        if (card.is_system) continue;
        const key = `${slotPrefix}${card.name.toLowerCase()}`;
        if (!currentList.some((d) => d.toLowerCase() === key)) {
            currentList.push(key);
        }
    }
    emit("update:model-routing", {
        ...props.modelRouting,
        disabled_role_cards: currentList,
    });
}

const currentStageReviewerOverride = computed(() => {
    return (props.modelRouting?.stage_reviewer_override || "").trim();
});

const isStageReviewerDisabled = computed(() => {
    const clean = currentStageReviewerOverride.value.toLowerCase();
    return clean === "disabled" || clean === "none";
});

const isCommitteeMode = computed(() => {
    return props.modelRouting?.stage_review_committee === true;
});

const activeReviewerCards = computed(() => {
    return reviewerRoleCards.value.filter((c) => !isCardHibernatedForSlot(c.name, "reviewer"));
});

const stageReviewerBadgeText = computed(() => {
    if (isStageReviewerDisabled.value) return "⚡免审";
    if (isCommitteeMode.value) {
        return activeReviewerCards.value.length > 0
            ? `👥会审(${activeReviewerCards.value.length})`
            : "👥会审";
    }
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
    } else if (isCommitteeMode.value) {
        title += ` | 👥 阶段门禁: 联合会审(${activeReviewerCards.value.length}位专家并发)`;
    } else if (activeReviewerCards.value.length === 0) {
        title += " | 🌐 阶段门禁: 系统异构对抗兜底";
    } else if (activeReviewerCards.value.length === 1) {
        title += ` | 🎭 阶段门禁: 专属专家(${activeReviewerCards.value[0].label || activeReviewerCards.value[0].name})`;
    } else {
        title += ` | 🎯 阶段门禁: 领域自适应质检(${activeReviewerCards.value.length}位专家)`;
    }
    return title;
});

function setTriggerMode(mode: "auto" | "cross_review" | "disabled") {
    if (mode === "disabled") {
        emit("update:model-routing", {
            ...props.modelRouting,
            stage_reviewer_override: "disabled",
        });
    } else {
        emit("update:planMode", mode);
        emit("update:model-routing", {
            ...props.modelRouting,
            stage_reviewer_override: undefined,
        });
    }
}

function setConcurrencyMode(mode: "adaptive" | "committee") {
    emit("update:model-routing", {
        ...props.modelRouting,
        stage_review_committee: mode === "committee",
    });
}

const budgetModes = CONTEXT_BUDGET_MODES;
const cardLevels = STATE_CARD_LEVELS;
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

    const targetWidth = Math.min(540, window.innerWidth - 24);
    let left = rect.left;
    if (left + targetWidth > window.innerWidth - 12) {
        left = Math.max(12, window.innerWidth - targetWidth - 12);
    }
    left = Math.max(12, left);

    const neededHeight = 490;
    let placement: "top" | "bottom" = "top";
    let top = 0;
    let maxHeight = 560;

    if (spaceAbove < neededHeight && spaceBelow > spaceAbove) {
        placement = "bottom";
        top = rect.bottom + 8;
        maxHeight = Math.max(300, Math.min(620, spaceBelow - 20));
    } else {
        placement = "top";
        top = rect.top - 8;
        maxHeight = Math.max(300, Math.min(620, spaceAbove - 20));
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

function selectCardLevel(level: StateCardLevel) {
    emit("update:model-routing", {
        ...props.modelRouting,
        state_card_level: level,
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
    : taskStartMode.value === "resume" ? "补充说明或输入继续，将保留原任务与审查状态…" : "想让 Agent 帮你做什么？",
);
const canSubmit = computed(() => {
    return Boolean(props.draft.trim()) || attachments.value.length > 0;
});

async function submit(explicitMode?: "interrupt" | "queue" | Event) {
    if (!canSubmit.value || props.isSending) return;

    let userText = props.draft.trim();
    const mode = (typeof explicitMode === "string" ? explicitMode : undefined)
        || (props.isRunning && !props.isWaitingForHuman ? submitMode.value : undefined);

    // 先清输入框：等待落盘期间用户的新输入属于下一条消息，不能被本次提交吞掉
    emit("update:draft", "");

    // 极速回车竞态：粘贴/拖拽的落盘是异步的，提交前给在途任务一个有界等待窗口。
    // 窗口耗尽仍未完成的附件不阻塞发送——落盘失败的图片会以 upload_failed 标记如实降级。
    if (pendingAttachmentSaves.size > 0) {
        await Promise.race([
            Promise.allSettled([...pendingAttachmentSaves]),
            new Promise<void>((resolve) => setTimeout(resolve, 3000)),
        ]);
    }

    const currentAtts = [...attachments.value];

    if (currentAtts.length > 0) {
        // 🌟 分流：粘贴的纯文本片段 (is_pasted_snippet) vs 物理附件 (拖拽或上传的文件/图片)
        const pastedSnippets = currentAtts.filter((a) => a.is_pasted_snippet);
        const realAttachments = currentAtts.filter((a) => !a.is_pasted_snippet);

        const promptParts: string[] = [];

        // 1. 真实物理文件：生成附件徽标（携带绝对路径，方便消息流组件精确定位与图片缩略图直接加载），并传递给后端走沙箱与分层挂载
        if (realAttachments.length > 0) {
            const briefBadges = realAttachments
                .map((a) => {
                    const pathInfo = a.file_path && a.file_path !== a.file_name ? ` | 路径: ${a.file_path}` : "";
                    return `📎 [附件: ${a.file_name} (${formatFileSize(a.size_bytes)})${pathInfo}]`;
                })
                .join("\n");
            promptParts.push(briefBadges);
        }

        // 2. 用户的提问文字
        if (userText) {
            promptParts.push(userText);
        }

        // 3. 粘贴的长文本/代码片段：直接作为纯文本/代码块输入给大模型，0延迟秒看秒答，绝不在本地写任何文件！
        if (pastedSnippets.length > 0) {
            const snippetBlocks = pastedSnippets
                .map((s) => {
                    const ext = s.file_name.split(".").pop() || "";
                    return `\`\`\`${ext}\n${s.prompt_text}\n\`\`\``;
                })
                .join("\n\n");
            promptParts.push(snippetBlocks);
        }

        const visiblePrompt = promptParts.join("\n\n");
        attachments.value = [];
        emit("submit", { text: visiblePrompt, attachments: realAttachments, mode, taskStartMode: taskStartMode.value });
    } else {
        emit("submit", { text: userText, attachments: [], mode, taskStartMode: taskStartMode.value });
    }
}

const contextLevelClass = computed(() => {
    const p = ringPercent.value ?? 0;
    if (p >= 80) return "badge-danger";
    if (p >= 60) return "badge-warn";
    return "badge-safe";
});

// 上下文水位快照：运行中吃实时 telemetry，静默态回显库内最近值
const contextSnapshot = ref<any>(null);
const contextDetailOpen = ref(false);

const ringPercent = computed<number | null>(() => {
    const live = props.telemetry?.contextUsagePercent;
    if (live !== undefined && live !== null && (props.telemetry?.contextWindow ?? 0) > 0) return live;
    const snap = contextSnapshot.value?.latest;
    if (snap && snap.context_window > 0) return snap.usage_percent;
    return null;
});
const ringTokens = computed(() =>
    props.telemetry?.totalTokens || contextSnapshot.value?.latest?.prompt_tokens || 0
);
const ringBreakdown = computed(() => {
    const sections = contextSnapshot.value?.breakdown?.sections;
    const total = contextSnapshot.value?.breakdown?.total_bytes;
    if (!sections || !total) return [];
    return Object.entries(sections)
        .map(([name, bytes]: [string, any]) => ({ name, pct: Math.round((bytes / total) * 100) }))
        .sort((a, b) => b.pct - a.pct);
});

const loadContextSnapshot = async () => {
    if (!props.sessionId) return;
    try {
        contextSnapshot.value = await invoke("get_context_snapshot", { sessionId: props.sessionId });
    } catch { /* 快照失败仅影响展示，静默降级 */ }
};
watch(() => props.sessionId, () => loadContextSnapshot(), { immediate: true });
// 运行期 telemetry 是权威值；静默时缓慢刷新快照保证"上次值"不落伍
watch(() => props.telemetry?.totalTokens, (v) => { if (v) loadContextSnapshot(); });

defineExpose({
    attachFilePaths,
    attachments,
});
</script>

<style scoped src="./agent-composer.css"></style>
