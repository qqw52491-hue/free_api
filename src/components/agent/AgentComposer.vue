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

                    <!-- 🤖 模型路由精简浮层卡片 (左下角与上下文模式并排) -->
                    <div class="model-picker-wrap" ref="modelPickerRef">
                        <button
                            class="model-chip"
                            type="button"
                            :class="{ active: modelMenuOpen }"
                            title="配置模型路由 (推理 / 执行 / 视觉)"
                            :disabled="isRunning"
                            @click="toggleModelMenu"
                        >
                            <span class="model-icon">🤖</span>
                            <span>{{ currentModelName }}</span>
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
                                        <div class="popover-title">🤖 Agent 模型路由</div>
                                        <div class="popover-sub">规划、执行与视觉独立配置</div>
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
                                            <span class="tab-btn-text">模型分工</span>
                                        </button>
                                        <button
                                            type="button"
                                            class="routing-tab-btn"
                                            :class="{ active: activeRoutingTab === 'roles' }"
                                            @click="activeRoutingTab = 'roles'"
                                        >
                                            <span class="tab-btn-icon">🎭</span>
                                            <span class="tab-btn-text">角色档位</span>
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
                                        <div class="route-item pro-tier">
                                            <div class="route-item-head">
                                                <div class="head-left">
                                                    <span class="route-badge pro">🧠 推理规划模型 (Pro)</span>
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

                                        <!-- 2. 极速执行模型 (Flash) -->
                                        <div class="route-item flash-tier">
                                            <div class="route-item-head">
                                                <div class="head-left">
                                                    <span class="route-badge flash">⚡ 极速执行模型 (Flash)</span>
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
                                    </div>

                                    <!-- Tab 2: 角色档位映射 (Roles - 精美分段选择卡片) -->
                                    <div v-else-if="activeRoutingTab === 'roles'" class="tab-pane-content roles-pane">
                                        <div class="role-cards-grid">
                                            <div v-for="role in roleMappingItems" :key="role.key" class="role-card">
                                                <div class="role-card-header">
                                                    <div class="role-card-title-group">
                                                        <span class="role-card-name">{{ role.label }}</span>
                                                        <span class="role-card-desc">{{ role.desc }}</span>
                                                    </div>
                                                    <div class="role-current-tag" :class="roleModelValue(role.key) || 'auto'">
                                                        {{ getRoleTagLabel(role.key) }}
                                                    </div>
                                                </div>
                                                <div class="role-pills-row">
                                                    <button
                                                        v-for="tier in roleTierOptions"
                                                        :key="tier.value"
                                                        type="button"
                                                        class="role-pill-btn"
                                                        :class="[tier.value ? `tier-${tier.value}` : 'tier-auto', { active: (roleModelValue(role.key) || '') === tier.value }]"
                                                        :title="tier.tooltip"
                                                        :disabled="isRunning"
                                                        @click="setRoleModel(role.key, tier.value)"
                                                    >
                                                        <span class="pill-icon">{{ tier.icon }}</span>
                                                        <span class="pill-text">{{ tier.label }}</span>
                                                    </button>
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

            <!-- 7. 对话轮次 / 步骤 -->
            <span class="telemetry-node" title="当前执行轮次与步骤">
                <span class="t-label">🔄 步骤</span>
                <span class="t-val">{{ (telemetry?.stepCount && telemetry.stepCount > 0) ? `第 ${telemetry.stepCount} 步` : '1 轮' }}</span>
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
    type ReasoningEffort,
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
    queuedGoals?: Array<{ id: string; text: string; createdAt: number }>;
}>();
const emit = defineEmits<{
    "update:draft": [value: string];
    "update:model-routing": [routing: ModelRouting];
    "update:budget-mode": [mode: ContextBudgetMode];
    submit: [payload?: { text: string; attachments: ParsedAttachment[]; mode?: "interrupt" | "queue" }];
    "cancel-queue": [index: number];
    stop: [];
    "open-settings": [];
    "attach-files": [paths: string[]];
}>();

const submitMode = ref<"interrupt" | "queue">("interrupt");
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

function formatFileSize(bytes?: number): string {
    if (!bytes || bytes === 0) return "0 B";
    if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${bytes} B`;
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

function toggleModelMenu() {
    if (props.isRunning) return;
    if (!modelMenuOpen.value) {
        calculatePopoverPosition(modelPickerRef.value);
        budgetMenuOpen.value = false;
        modelMenuOpen.value = true;
    } else {
        modelMenuOpen.value = false;
    }
}

function handleComposerClick(e: MouseEvent) {
    const target = e.target as HTMLElement | null;
    if (target && !target.closest("button") && !target.closest(".model-routing-popover") && !target.closest(".budget-dropdown-menu")) {
        textareaRef.value?.focus();
    }
}

function toggleBudgetMenu() {
    if (props.isRunning) return;
    if (!budgetMenuOpen.value) {
        calculateBudgetPosition(budgetPickerRef.value);
        modelMenuOpen.value = false;
        budgetMenuOpen.value = true;
    } else {
        budgetMenuOpen.value = false;
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

type RoleKey = "planner" | "executor" | "reviewer" | "scout" | "writer";
const roleMappingItems: Array<{ key: RoleKey; label: string; desc: string }> = [
    { key: "planner", label: "🧭 规划", desc: "首步全局规划与任务拆解" },
    { key: "executor", label: "⚙️ 执行", desc: "常规工具步骤" },
    { key: "reviewer", label: "🔍 审查", desc: "受限只读子审查（质量关口）" },
    { key: "scout", label: "🛰️ 侦察 L2", desc: "只读侦察 worker（读文件/只读命令）" },
    { key: "writer", label: "🛠️ 写入 L3", desc: "隔离副本改码 worker（diff 回执）" },
];

const roleTierOptions: Array<{ value: string; label: string; icon: string; tooltip: string }> = [
    { value: "", label: "跟随自动", icon: "⚡", tooltip: "跟随自动分流策略 (Auto)" },
    { value: "pro", label: "强模型 Pro", icon: "🧠", tooltip: "钉死为强模型槽 (Pro)" },
    { value: "flash", label: "快模型 Flash", icon: "⚡", tooltip: "钉死为快模型槽 (Flash)" },
    { value: "vision", label: "视觉 Vision", icon: "👁️", tooltip: "钉死为视觉模型槽 (Vision)" },
];

function getRoleTagLabel(key: RoleKey): string {
    const val = roleModelValue(key);
    const found = roleTierOptions.find(t => t.value === val);
    return found ? found.label : "跟随自动";
}
const routingMode = computed(() => (props.modelRouting as { routing_mode?: string }).routing_mode || "rules");

function setRoutingMode(mode: "rules" | "scored") {
    emit("update:model-routing", {
        ...props.modelRouting,
        routing_mode: mode,
    } as ModelRouting);
}

function roleModelValue(key: RoleKey): string {
    const map = (props.modelRouting as { role_models?: Record<string, string> }).role_models || {};
    return map[key] || "";
}

function setRoleModel(key: RoleKey, value: string) {
    const map = { ...((props.modelRouting as { role_models?: Record<string, string> }).role_models || {}) };
    if (value) {
        map[key] = value;
    } else {
        delete map[key];
    }
    emit("update:model-routing", {
        ...props.modelRouting,
        role_models: map,
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
}

function handleKeyDown(e: KeyboardEvent) {
    if (e.key === "Escape") {
        if (modelMenuOpen.value) modelMenuOpen.value = false;
        if (budgetMenuOpen.value) budgetMenuOpen.value = false;
    }
}

function handleScrollOrResize() {
    if (modelMenuOpen.value) calculatePopoverPosition(modelPickerRef.value);
    if (budgetMenuOpen.value) calculateBudgetPosition(budgetPickerRef.value);
}

onMounted(() => {
    document.addEventListener("click", handleClickOutside, true);
    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("scroll", handleScrollOrResize, true);
    window.addEventListener("resize", handleScrollOrResize);
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
    const hasContent = Boolean(props.draft.trim()) || attachments.value.length > 0;
    return props.isRunning ? hasContent : (props.canStart && hasContent);
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

function formatTokens(count: number): string {
    if (count >= 1000000) return `${(count / 1000000).toFixed(1)}M`;
    if (count >= 1000) return `${(count / 1000).toFixed(1)}k`;
    return String(count);
}

function formatMs(ms?: number): string {
    if (!ms) return "0ms";
    if (ms >= 1000) return `${(ms / 1000).toFixed(1)}s`;
    return `${ms}ms`;
}

const contextLevelClass = computed(() => {
    const p = props.telemetry?.contextUsagePercent ?? 0;
    if (p >= 80) return "badge-danger";
    if (p >= 60) return "badge-warn";
    return "badge-safe";
});
</script>

<style scoped>
.manus-composer-wrap {
    padding: 0 clamp(16px, 4vw, 48px) 12px;
    background: linear-gradient(0deg, #141416 85%, transparent);
}
.manus-composer {
    position: relative;
    display: flex;
    flex-direction: column;
    padding: 10px 14px 10px;
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 12px;
    background: #1c1c1f;
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.25);
    transition: all 0.2s ease;
}
.manus-composer.is-dragging {
    border-color: #6366f1;
    box-shadow: 0 0 0 2px rgba(99, 102, 241, 0.3);
}
.manus-composer:focus-within {
    border-color: rgba(255, 255, 255, 0.22);
    box-shadow: 0 4px 24px rgba(0, 0, 0, 0.35);
}
.manus-composer.waiting {
    border-color: rgba(251, 191, 36, 0.3);
    box-shadow: 0 0 0 1px rgba(251, 191, 36, 0.15);
}

/* 📎 附件预览托盘 (Attachment Tray) */
.composer-attachments-tray {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    padding-bottom: 8px;
    margin-bottom: 6px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
}

.attachment-chip-card {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 4px 8px 4px 6px;
    border-radius: 8px;
    background: rgba(255, 255, 255, 0.05);
    border: 1px solid rgba(255, 255, 255, 0.1);
    box-shadow: 0 2px 8px rgba(0, 0, 0, 0.25);
    max-width: 240px;
    transition: all 0.15s ease;
}

.attachment-chip-card:hover {
    background: rgba(255, 255, 255, 0.08);
    border-color: rgba(255, 255, 255, 0.18);
}

.attachment-chip-card.is-image {
    padding: 3px 6px 3px 3px;
}

/* 图片缩略图容器 */
.thumb-wrap {
    position: relative;
    width: 38px;
    height: 38px;
    border-radius: 6px;
    overflow: hidden;
    cursor: pointer;
    flex-shrink: 0;
    background: #000;
}

.img-thumbnail {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
}

.thumb-overlay {
    position: absolute;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    display: grid;
    place-items: center;
    opacity: 0;
    transition: opacity 0.15s ease;
}

.thumb-wrap:hover .thumb-overlay {
    opacity: 1;
}

.thumb-overlay svg {
    width: 14px;
    height: 14px;
    stroke: #ffffff;
    stroke-width: 2;
    fill: none;
}

.file-icon-badge {
    width: 32px;
    height: 32px;
    border-radius: 6px;
    display: grid;
    place-items: center;
    font-size: 15px;
    flex-shrink: 0;
    background: rgba(255, 255, 255, 0.07);
    border: 1px solid rgba(255, 255, 255, 0.08);
}

.file-icon-badge.badge-excel { background: rgba(16, 185, 129, 0.15); border-color: rgba(16, 185, 129, 0.3); }
.file-icon-badge.badge-pdf   { background: rgba(239, 68, 68, 0.15);  border-color: rgba(239, 68, 68, 0.3); }
.file-icon-badge.badge-word  { background: rgba(59, 130, 246, 0.15); border-color: rgba(59, 130, 246, 0.3); }
.file-icon-badge.badge-zip   { background: rgba(245, 158, 11, 0.15); border-color: rgba(245, 158, 11, 0.3); }

.attachment-meta {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
    flex: 1;
    cursor: pointer;
}

.attachment-meta .file-name {
    font-size: 11.5px;
    font-weight: 500;
    color: #f4f4f5;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    line-height: 1.2;
}

.attachment-meta .file-size {
    font-size: 10px;
    color: #71717a;
}

.remove-attachment-btn {
    display: grid !important;
    place-items: center !important;
    width: 18px !important;
    height: 18px !important;
    border-radius: 50% !important;
    border: 0 !important;
    background: rgba(255, 255, 255, 0.08) !important;
    color: #a1a1aa !important;
    cursor: pointer;
    padding: 0 !important;
    flex-shrink: 0;
    transition: all 0.12s;
}

.remove-attachment-btn:hover {
    background: rgba(239, 68, 68, 0.25) !important;
    color: #fca5a5 !important;
}

.remove-attachment-btn svg {
    width: 10px;
    height: 10px;
    stroke: currentColor;
    stroke-width: 2.5;
}

/* 拖拽上传遮罩 */
.drag-drop-overlay {
    position: absolute;
    inset: 4px;
    border-radius: 10px;
    background: rgba(24, 24, 28, 0.95);
    border: 2px dashed rgba(99, 102, 241, 0.6);
    display: grid;
    place-items: center;
    z-index: 50;
    pointer-events: none;
    animation: fadeIn 0.15s ease;
}

.drag-drop-content {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
    color: #c7d2fe;
}

.drag-drop-content .drag-icon { font-size: 24px; }
.drag-drop-content strong { font-size: 13px; font-weight: 600; }
.drag-drop-content small { font-size: 11px; color: #818cf8; }

/* 图片全屏预览弹窗 */
.image-preview-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.8);
    backdrop-filter: blur(8px);
    z-index: 2000;
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
    z-index: 2000;
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
    box-shadow: 0 24px 60px rgba(0, 0, 0, 0.85);
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
    font-size: 20px;
    line-height: 1;
}

.dialog-file-meta {
    display: flex;
    flex-direction: column;
    gap: 2px;
}

.dialog-file-meta h3 {
    margin: 0;
    font-size: 13.5px;
    font-weight: 650;
    color: #f4f4f5;
}

.dialog-file-meta small {
    font-size: 11px;
    color: #71717a;
}

.dialog-actions {
    display: flex;
    align-items: center;
    gap: 10px;
}

.btn-copy-doc {
    display: inline-flex;
    align-items: center;
    padding: 4px 10px;
    border-radius: 6px;
    font-size: 11.5px;
    font-weight: 500;
    background: rgba(255, 255, 255, 0.08);
    border: 1px solid rgba(255, 255, 255, 0.12);
    color: #e4e4e7;
    cursor: pointer;
    transition: all 0.15s;
}

.btn-copy-doc:hover {
    background: rgba(255, 255, 255, 0.14);
    color: #ffffff;
}

.doc-dialog-body {
    flex: 1;
    padding: 20px 24px;
    overflow-y: auto;
    background: #141417;
}

.doc-markdown-content {
    color: #e4e4e7;
    font-size: 13.5px;
    line-height: 1.65;
}

.doc-raw-preview {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 12.5px;
    line-height: 1.55;
    color: #d4d4d8;
    white-space: pre-wrap;
    word-break: break-all;
}

.doc-empty-hint {
    padding: 40px 0;
    text-align: center;
    color: #71717a;
    font-size: 13px;
}

textarea {
    width: 100%;
    min-height: 40px;
    max-height: 160px;
    border: 0;
    padding: 2px 0;
    resize: none;
    color: #f4f4f5;
    background: transparent;
    font: inherit;
    font-size: 13.5px;
    line-height: 1.5;
    outline: none;
    overflow-y: auto;
}
textarea::placeholder {
    color: #71717a;
}

.composer-bottom-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-top: 6px;
    padding-top: 6px;
    border-top: 1px solid rgba(255, 255, 255, 0.05);
}

.composer-tools {
    display: flex;
    align-items: center;
    gap: 8px;
}

.attach-btn {
    width: 28px !important;
    height: 28px !important;
    padding: 0 !important;
    display: inline-flex !important;
    align-items: center !important;
    justify-content: center !important;
    font-size: 14px !important;
    font-weight: 500 !important;
    color: #a1a1aa !important;
    background: rgba(255, 255, 255, 0.04) !important;
    border: 1px solid rgba(255, 255, 255, 0.09) !important;
    border-radius: 6px !important;
    cursor: pointer;
    transition: all 0.15s ease;
}

.attach-btn svg {
    width: 14px;
    height: 14px;
    stroke: currentColor;
    stroke-width: 2.2;
    stroke-linecap: round;
    stroke-linejoin: round;
    fill: none;
}

.attach-btn:hover:not(:disabled) {
    color: #ffffff !important;
    background: rgba(255, 255, 255, 0.09) !important;
    border-color: rgba(255, 255, 255, 0.18) !important;
    transform: translateY(-1px);
}

.attach-btn:active:not(:disabled) {
    transform: translateY(0);
}

.attach-btn:disabled {
    opacity: 0.35 !important;
    cursor: not-allowed !important;
    pointer-events: none !important;
    background: rgba(255, 255, 255, 0.02) !important;
    border-color: rgba(255, 255, 255, 0.04) !important;
    color: #52525b !important;
}

.tool-divider {
    display: inline-block;
    width: 1px;
    height: 16px;
    background: rgba(255, 255, 255, 0.08);
    margin: 0 2px;
}

.composer-tools > button.composer-tool-btn,
.composer-tools > button.attach-btn {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 6px;
    color: #a1a1aa;
    cursor: pointer;
    background: rgba(255, 255, 255, 0.03);
    transition: all 0.15s;
}

.composer-tools > button.composer-tool-btn:hover:not(:disabled),
.composer-tools > button.attach-btn:hover:not(:disabled) {
    color: #ffffff;
    background: rgba(255, 255, 255, 0.08);
    border-color: rgba(255, 255, 255, 0.15);
}

.composer-tools > button.composer-tool-btn:disabled,
.composer-tools > button.attach-btn:disabled {
    opacity: 0.35;
    cursor: not-allowed;
    pointer-events: none;
}
.composer-tools > button svg {
    width: 14px;
    height: 14px;
    stroke: currentColor;
    stroke-width: 2;
    fill: none;
}

.model-picker-wrap {
    position: relative;
    display: inline-flex;
    align-items: center;
}

.model-chip {
    width: auto !important;
    height: 28px !important;
    padding: 0 10px !important;
    display: inline-flex !important;
    align-items: center !important;
    gap: 6px !important;
    font-size: 12px !important;
    font-weight: 500 !important;
    color: #e4e4e7 !important;
    background: rgba(255, 255, 255, 0.05) !important;
    border: 1px solid rgba(255, 255, 255, 0.1) !important;
    border-radius: 6px !important;
    cursor: pointer;
    transition: all 0.15s ease;
}

.model-chip:hover:not(:disabled) {
    background: rgba(255, 255, 255, 0.09) !important;
    border-color: rgba(255, 255, 255, 0.18) !important;
}

.model-chip.active {
    background: rgba(99, 102, 241, 0.18) !important;
    border-color: rgba(99, 102, 241, 0.4) !important;
    color: #a5b4fc !important;
}

.model-chip .model-icon {
    font-size: 13px;
    line-height: 1;
}

.model-chip .chevron-icon {
    width: 12px;
    height: 12px;
    stroke: currentColor;
    stroke-width: 2;
    fill: none;
    opacity: 0.7;
}

.model-routing-popover {
    box-sizing: border-box;
    overflow-y: auto;
    background: #18181c;
    border: 1px solid rgba(255, 255, 255, 0.16);
    border-radius: 12px;
    box-shadow: 0 20px 50px rgba(0, 0, 0, 0.85), 0 0 0 1px rgba(255, 255, 255, 0.08);
    padding: 12px;
    z-index: 99999 !important;
    display: flex;
    flex-direction: column;
    gap: 10px;
    backdrop-filter: blur(20px);
    animation: popIn 0.15s cubic-bezier(0.16, 1, 0.3, 1);
}

.model-routing-popover.placement-top {
    transform-origin: bottom left;
}

.model-routing-popover.placement-bottom {
    transform-origin: top left;
}

.model-routing-popover.align-right {
    transform-origin: bottom right;
}
.model-routing-popover.placement-bottom.align-right {
    transform-origin: top right;
}

.popover-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding-bottom: 8px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
}

.header-left {
    display: flex;
    flex-direction: column;
    gap: 2px;
}

.popover-title {
    font-size: 13px;
    font-weight: 600;
    color: #f4f4f5;
}

.popover-sub {
    font-size: 11px;
    color: #71717a;
}

.header-actions {
    display: flex;
    align-items: center;
    gap: 8px;
}

.sync-all-btn {
    display: inline-flex !important;
    align-items: center !important;
    width: auto !important;
    height: 24px !important;
    padding: 0 9px !important;
    border-radius: 5px !important;
    background: rgba(99, 102, 241, 0.15) !important;
    border: 1px solid rgba(99, 102, 241, 0.3) !important;
    color: #a5b4fc !important;
    font-size: 11px !important;
    font-weight: 500 !important;
    cursor: pointer;
    transition: all 0.15s;
}

.sync-all-btn:hover {
    background: rgba(99, 102, 241, 0.25) !important;
    border-color: rgba(99, 102, 241, 0.5) !important;
    color: #ffffff !important;
}

.close-popover-btn {
    display: grid !important;
    place-items: center !important;
    width: 24px !important;
    height: 24px !important;
    border-radius: 5px !important;
    background: rgba(255, 255, 255, 0.04) !important;
    border: 1px solid rgba(255, 255, 255, 0.1) !important;
    color: #a1a1aa !important;
    cursor: pointer;
    transition: all 0.15s;
}

.close-popover-btn:hover {
    background: rgba(255, 255, 255, 0.1) !important;
    border-color: rgba(255, 255, 255, 0.2) !important;
    color: #ffffff !important;
}

.close-popover-btn svg {
    width: 12px;
    height: 12px;
    stroke: currentColor;
    stroke-width: 2.2;
    fill: none;
}

.popover-body {
    display: flex;
    flex-direction: column;
    gap: 9px;
}

.route-item {
    display: flex;
    flex-direction: column;
    gap: 7px;
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid rgba(255, 255, 255, 0.07);
    border-radius: 8px;
    padding: 10px 12px;
}

.route-item-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
}

.head-left {
    display: flex;
    align-items: center;
    gap: 8px;
}

.route-badge {
    font-size: 11.5px;
    font-weight: 600;
}

.route-badge.pro {
    color: #a5b4fc;
}

.route-badge.flash {
    color: #67e8f9;
}

.route-badge.vision {
    color: #fcd34d;
}

.route-item-head small {
    font-size: 10.5px;
    color: #71717a;
}

/* 🎯 Codex / Claude 风格平滑档位滑动切换器 */
.effort-segmented-wrap {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
}

.effort-label {
    font-size: 11px;
    color: #71717a;
    font-weight: 550;
}

.effort-slider-group {
    display: inline-flex;
    align-items: center;
    background: rgba(0, 0, 0, 0.5);
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 7px;
    padding: 2px 3px;
    gap: 3px;
}

.effort-step-btn {
    min-width: 26px !important;
    height: 22px !important;
    padding: 0 4px !important;
    display: inline-flex !important;
    align-items: center !important;
    justify-content: center !important;
    border: 0 !important;
    border-radius: 5px !important;
    background: transparent !important;
    color: #a1a1aa !important;
    font-size: 11.5px !important;
    line-height: 1 !important;
    font-weight: 600 !important;
    cursor: pointer;
    transition: all 0.15s cubic-bezier(0.16, 1, 0.3, 1);
}

.effort-step-btn:hover:not(:disabled) {
    color: #ffffff !important;
    background: rgba(255, 255, 255, 0.08) !important;
}

.effort-step-btn.active {
    background: rgba(99, 102, 241, 0.32) !important;
    border: 1px solid rgba(99, 102, 241, 0.5) !important;
    color: #ffffff !important;
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.4);
}

.effort-step-btn.active:hover {
    background: rgba(99, 102, 241, 0.45) !important;
    color: #ffffff !important;
}

.route-controls {
    width: 100%;
}

.route-controls :deep(.model-select-wrapper) {
    width: 100% !important;
    min-width: 0 !important;
    display: block !important;
}

.route-controls :deep(.model-select-trigger) {
    width: 100% !important;
    min-width: 0 !important;
}

.budget-mode-picker {
    position: relative;
    display: inline-flex;
    align-items: center;
}

.budget-chip {
    width: auto !important;
    height: 28px !important;
    padding: 0 9px !important;
    display: inline-flex !important;
    align-items: center !important;
    gap: 5px !important;
    font-size: 12px !important;
    font-weight: 500 !important;
    color: #e4e4e7 !important;
    background: rgba(255, 255, 255, 0.05) !important;
    border: 1px solid rgba(255, 255, 255, 0.1) !important;
    border-radius: 6px !important;
    cursor: pointer;
    transition: all 0.15s ease;
}

.budget-chip:hover:not(:disabled) {
    background: rgba(255, 255, 255, 0.09) !important;
    border-color: rgba(255, 255, 255, 0.18) !important;
}

.budget-chip.active {
    background: rgba(59, 130, 246, 0.15) !important;
    border-color: rgba(59, 130, 246, 0.4) !important;
    color: #93c5fd !important;
}

.budget-chip .budget-icon {
    font-size: 13px;
    line-height: 1;
}

.budget-chip .chevron-icon {
    width: 12px;
    height: 12px;
    stroke: currentColor;
    stroke-width: 2;
    fill: none;
    opacity: 0.7;
}

.budget-dropdown-menu {
    box-sizing: border-box;
    background: #1e1e24;
    border: 1px solid rgba(255, 255, 255, 0.14);
    border-radius: 10px;
    box-shadow: 0 20px 48px rgba(0, 0, 0, 0.8), 0 0 0 1px rgba(255, 255, 255, 0.08);
    padding: 6px;
    z-index: 99999 !important;
    display: flex;
    flex-direction: column;
    gap: 4px;
    backdrop-filter: blur(16px);
    animation: popIn 0.15s cubic-bezier(0.16, 1, 0.3, 1);
}

.budget-dropdown-menu.placement-top {
    transform-origin: bottom left;
}

.budget-dropdown-menu.placement-bottom {
    transform-origin: top left;
}

@keyframes popIn {
    from { opacity: 0; transform: translateY(4px) scale(0.98); }
    to { opacity: 1; transform: translateY(0) scale(1); }
}

.budget-menu-header {
    font-size: 11px;
    font-weight: 600;
    color: #71717a;
    padding: 4px 8px 6px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
    margin-bottom: 2px;
}

.budget-menu-item {
    display: flex !important;
    align-items: center !important;
    width: 100% !important;
    height: auto !important;
    padding: 8px 10px !important;
    gap: 10px !important;
    background: transparent !important;
    border: 1px solid transparent !important;
    border-radius: 6px !important;
    cursor: pointer;
    text-align: left;
    transition: all 0.15s ease;
}

.budget-menu-item:hover {
    background: rgba(255, 255, 255, 0.06) !important;
}

.budget-menu-item.selected {
    background: rgba(59, 130, 246, 0.12) !important;
    border-color: rgba(59, 130, 246, 0.25) !important;
}

.opt-icon {
    font-size: 16px;
    flex-shrink: 0;
}

.opt-text {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
}

.opt-title {
    display: flex;
    align-items: center;
    gap: 6px;
}

.opt-title strong {
    font-size: 12px;
    font-weight: 600;
    color: #f4f4f5;
}

.opt-tag {
    font-size: 10px;
    padding: 1px 5px;
    border-radius: 4px;
    background: rgba(255, 255, 255, 0.08);
    color: #a1a1aa;
}

.budget-menu-item.selected .opt-tag {
    background: rgba(59, 130, 246, 0.2);
    color: #93c5fd;
}

.opt-sub {
    font-size: 10px;
    color: #71717a;
    line-height: 1.3;
    white-space: normal;
}

.check-icon {
    width: 14px;
    height: 14px;
    stroke: #60a5fa;
    stroke-width: 2.5;
    fill: none;
    flex-shrink: 0;
}

.composer-actions {
    display: flex;
    align-items: center;
    gap: 8px;
}

.stop-control {
    padding: 5px 12px;
    border: 1px solid rgba(255, 71, 87, 0.3);
    border-radius: 7px;
    color: #ff4757;
    cursor: pointer;
    background: rgba(255, 71, 87, 0.1);
    font: 600 12px inherit;
    transition: all 0.15s;
}
.stop-control:hover:not(:disabled) {
    background: rgba(255, 71, 87, 0.2);
}

.submit-control {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    border: 0;
    border-radius: 8px;
    color: #141416;
    cursor: pointer;
    background: #ffffff;
    transition: all 0.15s;
}
.submit-control:hover:not(:disabled) {
    background: #f4f4f5;
    transform: translateY(-1px);
}
.submit-control:disabled {
    opacity: 0.3;
    cursor: not-allowed;
    background: #71717a;
    color: #27272a;
}
.submit-control svg {
    width: 16px;
    height: 16px;
    stroke: currentColor;
    stroke-width: 2.4;
    fill: none;
}

/* ⏳ 排队队列横幅 */
.composer-queue-tray {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 8px;
    background: rgba(245, 158, 11, 0.08);
    border: 1px solid rgba(245, 158, 11, 0.25);
    border-radius: 6px;
    margin-bottom: 6px;
    font-size: 11.5px;
    color: #fef3c7;
    overflow-x: auto;
}
.composer-queue-tray .queue-icon {
    font-size: 13px;
    flex-shrink: 0;
}
.composer-queue-tray .queue-label {
    font-weight: 600;
    color: #fbbf24;
    white-space: nowrap;
    flex-shrink: 0;
}
.composer-queue-tray .queue-items-scroll {
    display: flex;
    align-items: center;
    gap: 6px;
    overflow-x: auto;
    flex: 1;
}
.composer-queue-tray .queue-pill {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    background: rgba(0, 0, 0, 0.25);
    border: 1px solid rgba(245, 158, 11, 0.35);
    padding: 2px 6px;
    border-radius: 4px;
    font-size: 11px;
    max-width: 220px;
    white-space: nowrap;
}
.composer-queue-tray .q-idx {
    font-size: 10px;
    color: #fbbf24;
    font-weight: 700;
}
.composer-queue-tray .q-text {
    overflow: hidden;
    text-overflow: ellipsis;
    color: #f3f4f6;
}
.composer-queue-tray .q-del-btn {
    width: 14px;
    height: 14px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: transparent;
    border: none;
    color: #9ca3af;
    cursor: pointer;
    font-size: 10px;
    border-radius: 3px;
    transition: all 0.15s;
    padding: 0;
    margin-left: 2px;
}
.composer-queue-tray .q-del-btn:hover {
    color: #ef4444;
    background: rgba(239, 68, 68, 0.2);
}

/* 运行中模式切换按钮 (立即打断 vs 排队执行) */
.mode-switch-pill {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 0 8px;
    height: 28px;
    border-radius: 6px;
    font-size: 11px;
    font-weight: 600;
    cursor: pointer;
    background: rgba(239, 68, 68, 0.12);
    border: 1px solid rgba(239, 68, 68, 0.35);
    color: #fca5a5;
    transition: all 0.15s ease;
}
.mode-switch-pill:hover {
    background: rgba(239, 68, 68, 0.22);
    border-color: rgba(239, 68, 68, 0.55);
}
.mode-switch-pill.is-queue {
    background: rgba(245, 158, 11, 0.12);
    border-color: rgba(245, 158, 11, 0.35);
    color: #fde68a;
}
.mode-switch-pill.is-queue:hover {
    background: rgba(245, 158, 11, 0.22);
    border-color: rgba(245, 158, 11, 0.55);
}
.submit-control.is-interrupt-submit {
    background: linear-gradient(135deg, #ef4444, #dc2626);
    color: #fff;
    box-shadow: 0 2px 8px rgba(239, 68, 68, 0.35);
}
.submit-control.is-queue-submit {
    background: linear-gradient(135deg, #f59e0b, #d97706);
    color: #fff;
    box-shadow: 0 2px 8px rgba(245, 158, 11, 0.35);
}

/* ⌨️ 快捷键问号提示与弹出卡片 */
.composer-shortcut-hint-wrap {
    position: relative;
    display: inline-flex;
    align-items: center;
}
.shortcut-hint-btn {
    opacity: 0.6;
    transition: opacity 0.15s, color 0.15s;
}
.shortcut-hint-btn:hover {
    opacity: 1;
    color: #38bdf8;
}
.hint-q-icon {
    width: 14px;
    height: 14px;
    stroke: currentColor;
    stroke-width: 2;
    fill: none;
}
.shortcut-popover-card {
    position: absolute;
    bottom: calc(100% + 8px);
    left: 0;
    width: 280px;
    padding: 10px 12px;
    background: #18181b;
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 8px;
    box-shadow: 0 10px 25px -5px rgba(0, 0, 0, 0.5), 0 8px 10px -6px rgba(0, 0, 0, 0.4);
    z-index: 9999;
    pointer-events: none;
    backdrop-filter: blur(12px);
    animation: fadeInScale 0.15s ease-out;
}
.shortcut-popover-title {
    font-size: 11px;
    font-weight: 700;
    color: #e4e4e7;
    margin-bottom: 8px;
    padding-bottom: 5px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
}
.shortcut-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 6px;
    font-size: 11px;
}
.shortcut-row:last-child {
    margin-bottom: 0;
}
.shortcut-keys {
    display: flex;
    align-items: center;
    gap: 3px;
    flex-shrink: 0;
}
.shortcut-keys kbd {
    background: rgba(255, 255, 255, 0.1);
    border: 1px solid rgba(255, 255, 255, 0.18);
    border-radius: 3px;
    padding: 1px 4px;
    font-size: 10px;
    font-family: inherit;
    color: #f4f4f5;
    box-shadow: 0 1px 1px rgba(0, 0, 0, 0.2);
}
.shortcut-desc {
    color: #a1a1aa;
    font-size: 10.5px;
    line-height: 1.3;
}
.shortcut-desc .text-red {
    color: #f87171;
}
.shortcut-desc .text-amber {
    color: #fbbf24;
}

/* ---- 底部实时指标连续状态行 ---- */
.composer-telemetry-line {
    display: flex;
    align-items: center;
    justify-content: center;
    flex-wrap: nowrap;
    gap: 8px;
    margin-top: 7px;
    padding: 2px 6px;
    font-size: 11.5px;
    color: #71717a;
    overflow-x: auto;
    scrollbar-width: none;
    white-space: nowrap;
    user-select: none;
}
.composer-telemetry-line::-webkit-scrollbar {
    display: none;
}

.telemetry-node {
    display: inline-flex;
    align-items: baseline;
    gap: 4.5px;
}

.t-label {
    color: #71717a;
    font-size: 11px;
}

.t-val {
    color: #e4e4e7;
    font-family: 'JetBrains Mono', monospace;
    font-weight: 600;
    font-size: 11.5px;
}

.t-sep {
    color: rgba(255, 255, 255, 0.15);
    font-size: 10px;
    font-weight: bold;
    user-select: none;
}

.t-tag {
    font-size: 10px;
    font-family: 'JetBrains Mono', monospace;
    font-weight: 600;
    margin-left: 2px;
}

.text-cyan {
    color: #38bdf8 !important;
}

.text-green {
    color: #4ade80 !important;
}

.badge-safe {
    color: #4ade80;
}
.badge-warn {
    color: #fbbf24;
}
.badge-danger {
    color: #f87171;
}

.dot-running {
    display: inline-block;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: #38bdf8;
    box-shadow: 0 0 6px #38bdf8;
    animation: pulse 1.4s ease-in-out infinite;
}

@keyframes pulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50% { opacity: 0.4; transform: scale(0.85); }
}

.composer-disclaimer {
    margin: 0;
    text-align: center;
    color: #52525b;
    font-size: 11px;
}

/* 🎭 模型路由 2-Tab 顶栏导航与模式切换开关 */
.routing-top-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    background: rgba(0, 0, 0, 0.4);
    padding: 4px 6px;
    border-radius: 8px;
    border: 1px solid rgba(255, 255, 255, 0.08);
}

.routing-tabs-nav {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex: 0 0 auto;
}

.routing-tab-btn {
    display: inline-flex !important;
    align-items: center !important;
    justify-content: center !important;
    gap: 5px !important;
    width: auto !important;
    height: 28px !important;
    padding: 0 12px !important;
    border-radius: 6px !important;
    border: 1px solid transparent !important;
    background: transparent !important;
    color: #a1a1aa !important;
    font-size: 12px !important;
    font-weight: 600 !important;
    cursor: pointer;
    transition: all 0.15s ease;
    white-space: nowrap !important;
}

.routing-tab-btn:hover {
    background: rgba(255, 255, 255, 0.06) !important;
    color: #ffffff !important;
}

.routing-tab-btn.active {
    background: #242436 !important;
    border-color: rgba(99, 102, 241, 0.45) !important;
    color: #ffffff !important;
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.4);
}

.top-mode-switch {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
    white-space: nowrap;
}

.mode-switch-label {
    font-size: 11px;
    font-weight: 600;
    color: #8e8e9f;
    white-space: nowrap;
}

.mode-switch-group {
    display: inline-flex;
    flex-direction: row;
    align-items: center;
    background: rgba(0, 0, 0, 0.5);
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 6px;
    padding: 2px;
    gap: 2px;
}

.mode-switch-btn {
    white-space: nowrap !important;
    display: inline-flex !important;
    flex-direction: row !important;
    align-items: center !important;
    justify-content: center !important;
    width: auto !important;
    height: 22px !important;
    padding: 0 9px !important;
    min-width: 40px !important;
    border-radius: 4px !important;
    border: 0 !important;
    background: transparent !important;
    color: #a1a1aa !important;
    font-size: 11px !important;
    line-height: 22px !important;
    font-weight: 600 !important;
    cursor: pointer;
    transition: all 0.15s;
}

.mode-switch-btn:hover:not(:disabled) {
    color: #ffffff !important;
}

.mode-switch-btn.active {
    background: rgba(99, 102, 241, 0.35) !important;
    border: 1px solid rgba(99, 102, 241, 0.5) !important;
    color: #ffffff !important;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.35);
}

.tab-pane-content {
    display: flex;
    flex-direction: column;
    gap: 8px;
    animation: tabFadeIn 0.18s cubic-bezier(0.16, 1, 0.3, 1);
}

.models-unified-pane {
    display: flex;
    flex-direction: column;
    gap: 8px;
}

@keyframes tabFadeIn {
    from {
        opacity: 0;
        transform: translateY(3px);
    }
    to {
        opacity: 1;
        transform: translateY(0);
    }
}

/* 🎭 角色档位高级卡片与胶囊开关 */
.roles-pane {
    display: flex;
    flex-direction: column;
    gap: 8px;
}

.role-cards-grid {
    display: flex;
    flex-direction: column;
    gap: 8px;
}

.role-card {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px 10px;
    border-radius: 8px;
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid rgba(255, 255, 255, 0.07);
    transition: all 0.15s ease;
}

.role-card:hover {
    background: rgba(255, 255, 255, 0.05);
    border-color: rgba(255, 255, 255, 0.12);
}

.role-card-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
}

.role-card-title-group {
    display: flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
    overflow: hidden;
}

.role-card-name {
    font-size: 12px;
    font-weight: 600;
    color: #f4f4f5;
    flex-shrink: 0;
}

.role-card-desc {
    font-size: 10.5px;
    color: #71717a;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}

.role-current-tag {
    font-size: 10px;
    font-weight: 600;
    padding: 1px 6px;
    border-radius: 999px;
    background: rgba(255, 255, 255, 0.07);
    color: #a1a1aa;
    border: 1px solid rgba(255, 255, 255, 0.1);
    flex-shrink: 0;
}
.role-current-tag.pro {
    background: rgba(99, 102, 241, 0.15);
    color: #a5b4fc;
    border-color: rgba(99, 102, 241, 0.3);
}
.role-current-tag.flash {
    background: rgba(245, 158, 11, 0.15);
    color: #fde68a;
    border-color: rgba(245, 158, 11, 0.3);
}
.role-current-tag.vision {
    background: rgba(16, 185, 129, 0.15);
    color: #6ee7b7;
    border-color: rgba(16, 185, 129, 0.3);
}

.role-pills-row {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 4px;
    background: rgba(0, 0, 0, 0.35);
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 6px;
    padding: 2px;
    width: 100%;
    box-sizing: border-box;
}

.role-pill-btn {
    display: inline-flex !important;
    align-items: center !important;
    justify-content: center !important;
    gap: 4px !important;
    width: 100% !important;
    min-width: 0 !important;
    height: 24px !important;
    padding: 0 2px !important;
    border-radius: 4px !important;
    border: 1px solid transparent !important;
    background: transparent !important;
    color: #a1a1aa !important;
    font-size: 11px !important;
    font-weight: 550 !important;
    cursor: pointer;
    transition: all 0.15s cubic-bezier(0.16, 1, 0.3, 1);
    white-space: nowrap !important;
    box-sizing: border-box !important;
}

.role-pill-btn .pill-icon {
    font-size: 10.5px;
    line-height: 1;
}

.role-pill-btn:hover:not(:disabled) {
    color: #ffffff !important;
    background: rgba(255, 255, 255, 0.06) !important;
}

/* 自动 */
.role-pill-btn.tier-auto.active {
    background: rgba(255, 255, 255, 0.12) !important;
    border-color: rgba(255, 255, 255, 0.22) !important;
    color: #ffffff !important;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.35);
}

/* Pro 强模型 */
.role-pill-btn.tier-pro.active {
    background: rgba(99, 102, 241, 0.28) !important;
    border-color: rgba(99, 102, 241, 0.5) !important;
    color: #c7d2fe !important;
    box-shadow: 0 1px 4px rgba(99, 102, 241, 0.25);
}

/* Flash 快模型 */
.role-pill-btn.tier-flash.active {
    background: rgba(245, 158, 11, 0.22) !important;
    border-color: rgba(245, 158, 11, 0.5) !important;
    color: #fde68a !important;
    box-shadow: 0 1px 4px rgba(245, 158, 11, 0.2);
}

/* Vision 视觉模型 */
.role-pill-btn.tier-vision.active {
    background: rgba(16, 185, 129, 0.22) !important;
    border-color: rgba(16, 185, 129, 0.5) !important;
    color: #6ee7b7 !important;
    box-shadow: 0 1px 4px rgba(16, 185, 129, 0.2);
}

.route-badge.mode {
    background: linear-gradient(135deg, #5b8def 0%, #7f5bef 100%);
}
.route-badge.roles {
    background: linear-gradient(135deg, #2f9e6e 0%, #5bc48f 100%);
}
</style>
