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
                @keydown.ctrl.enter.prevent="submit"
                @keydown.meta.enter.prevent="submit"
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
                        <span class="tool-btn-label">附件</span>
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

                        <div v-if="modelMenuOpen" class="model-routing-popover">
                            <div class="popover-header">
                                <div class="header-left">
                                    <div class="popover-title">Agent 模型路由</div>
                                    <div class="popover-sub">规划、工具执行与视觉分析独立配置</div>
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

                            <div class="popover-body">
                                <!-- 1. 推理模型 (Pro) -->
                                <div class="route-item">
                                    <div class="route-item-head">
                                        <div class="head-left">
                                            <span class="route-badge pro">🧠 推理模型</span>
                                            <small>负责长程规划与故障恢复</small>
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
                                            placeholder="选择推理模型..."
                                            @update:model-value="updateRouteValue('pro', $event)"
                                            size="sm"
                                        />
                                    </div>
                                </div>

                                <!-- 2. 执行模型 (Flash) -->
                                <div class="route-item">
                                    <div class="route-item-head">
                                        <div class="head-left">
                                            <span class="route-badge flash">⚡ 执行模型</span>
                                            <small>负责常规工具快速执行</small>
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
                                            placeholder="选择执行模型..."
                                            @update:model-value="updateRouteValue('flash', $event)"
                                            size="sm"
                                        />
                                    </div>
                                </div>

                                <!-- 3. 视觉模型 (Vision) -->
                                <div class="route-item">
                                    <div class="route-item-head">
                                        <div class="head-left">
                                            <span class="route-badge vision">👁️ 视觉模型</span>
                                            <small>负责截图与多模态分析</small>
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
                                            size="sm"
                                        />
                                    </div>
                                </div>

                                <!-- 4. 路由模式：规则分流 / 难度打分 -->
                                <div class="route-item routing-mode-item">
                                    <div class="route-item-head">
                                        <div class="head-left">
                                            <span class="route-badge mode">🎚️ 路由模式</span>
                                            <small>决定每一步由强模型还是快模型接管</small>
                                        </div>
                                        <div class="effort-slider-group">
                                            <button
                                                type="button"
                                                class="effort-step-btn"
                                                :class="{ active: routingMode === 'rules' }"
                                                title="规则分流：首步规划与失败救场走强模型，其余走快模型"
                                                :disabled="isRunning"
                                                @click="setRoutingMode('rules')"
                                            >
                                                规则
                                            </button>
                                            <button
                                                type="button"
                                                class="effort-step-btn"
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

                                <!-- 5. 角色档位映射：角色固定，跟随档位或钉死 -->
                                <div class="route-item role-mapping-item">
                                    <div class="route-item-head">
                                        <div class="head-left">
                                            <span class="route-badge roles">🎭 角色档位</span>
                                            <small>子任务角色默认跟随上方档位；钉死后无视自动分流</small>
                                        </div>
                                    </div>
                                    <div class="role-mapping-grid">
                                        <div v-for="role in roleMappingItems" :key="role.key" class="role-row">
                                            <div class="role-cell-label" :title="role.desc">
                                                <span class="role-name">{{ role.label }}</span>
                                                <small class="role-desc">{{ role.desc }}</small>
                                            </div>
                                            <select
                                                class="role-tier-select"
                                                :disabled="isRunning"
                                                :value="roleModelValue(role.key)"
                                                @change="setRoleModel(role.key, ($event.target as HTMLSelectElement).value)"
                                            >
                                                <option value="">跟随自动</option>
                                                <option value="pro">🧠 强模型槽</option>
                                                <option value="flash">⚡ 快模型槽</option>
                                                <option value="vision">👁️ 视觉槽</option>
                                            </select>
                                        </div>
                                    </div>
                                </div>
                            </div>
                        </div>
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

                        <div v-if="budgetMenuOpen" class="budget-dropdown-menu">
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
                    </div>
                </div>
                <div class="composer-actions">
                    <button v-if="isRunning" class="stop-control" type="button" :disabled="stopRequested" @click="$emit('stop')">{{ stopRequested ? '停止中' : '停止' }}</button>
                    <button class="submit-control" type="button" :disabled="!canSubmit || isSending" :title="isRunning ? (isWaitingForHuman ? '回答当前请求' : '替换为新任务') : '开始任务'" @click="submit">
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
}>();
const emit = defineEmits<{
    "update:draft": [value: string];
    "update:model-routing": [routing: ModelRouting];
    "update:budget-mode": [mode: ContextBudgetMode];
    submit: [payload?: { text: string; attachments: ParsedAttachment[] }];
    stop: [];
    "open-settings": [];
    "attach-files": [paths: string[]];
}>();

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

function toggleModelMenu() {
    if (props.isRunning) return;
    modelMenuOpen.value = !modelMenuOpen.value;
    if (modelMenuOpen.value) {
        budgetMenuOpen.value = false;
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
    budgetMenuOpen.value = !budgetMenuOpen.value;
    if (budgetMenuOpen.value) {
        modelMenuOpen.value = false;
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

onMounted(() => {
    document.addEventListener("click", handleClickOutside, true);
    window.addEventListener("keydown", handleKeyDown);
});
onBeforeUnmount(() => {
    document.removeEventListener("click", handleClickOutside, true);
    window.removeEventListener("keydown", handleKeyDown);
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
        : "输入新任务；旧模型/工具会在安全边界停止…"
    : "想让 Agent 帮你做什么？",
);
const canSubmit = computed(() => {
    const hasContent = Boolean(props.draft.trim()) || attachments.value.length > 0;
    return props.isRunning ? hasContent : (props.canStart && hasContent);
});

function submit() {
    if (!canSubmit.value || props.isSending) return;

    let userText = props.draft.trim();
    const currentAtts = [...attachments.value];

    if (currentAtts.length > 0) {
        // 🌟 结构化排列附件清单：清晰标注文件名、大小与本地物理路径
        const briefBadges = currentAtts
            .map((a) => `📎 [附件: ${a.file_name} (${formatFileSize(a.size_bytes)}) | 路径: ${a.file_path}]`)
            .join("\n");
        const visiblePrompt = userText ? `${briefBadges}\n\n${userText}` : briefBadges;
        emit("update:draft", "");
        attachments.value = [];
        emit("submit", { text: visiblePrompt, attachments: currentAtts });
    } else {
        emit("update:draft", "");
        emit("submit", { text: userText, attachments: [] });
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
    width: auto !important;
    height: 28px !important;
    padding: 0 10px !important;
    display: inline-flex !important;
    align-items: center !important;
    gap: 5px !important;
    font-size: 12px !important;
    font-weight: 500 !important;
    color: #a1a1aa !important;
    background: rgba(255, 255, 255, 0.04) !important;
    border: 1px solid rgba(255, 255, 255, 0.09) !important;
    border-radius: 6px !important;
    cursor: pointer;
    transition: all 0.15s ease;
}

.attach-btn:hover {
    color: #ffffff !important;
    background: rgba(255, 255, 255, 0.09) !important;
    border-color: rgba(255, 255, 255, 0.18) !important;
}

.tool-btn-label {
    font-size: 12px;
}

.tool-divider {
    display: inline-block;
    width: 1px;
    height: 16px;
    background: rgba(255, 255, 255, 0.08);
    margin: 0 2px;
}

.composer-tools button:not(.attach-btn):not(.model-chip):not(.budget-chip):not(.sync-all-btn):not(.close-popover-btn):not(.effort-step-btn):not(.budget-menu-item) {
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
.composer-tools button:hover {
    color: #ffffff;
    background: rgba(255, 255, 255, 0.08);
    border-color: rgba(255, 255, 255, 0.15);
}
.composer-tools button svg {
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
    position: absolute;
    bottom: calc(100% + 8px);
    left: 0;
    width: min(520px, calc(100vw - 48px));
    max-height: min(480px, calc(100vh - 140px));
    overflow-y: auto;
    background: #18181c;
    border: 1px solid rgba(255, 255, 255, 0.14);
    border-radius: 12px;
    box-shadow: 0 16px 36px rgba(0, 0, 0, 0.6), 0 0 0 1px rgba(255, 255, 255, 0.06);
    padding: 14px;
    z-index: 1300;
    display: flex;
    flex-direction: column;
    gap: 10px;
    backdrop-filter: blur(16px);
    animation: popIn 0.15s ease-out;
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
    gap: 5px;
    flex-shrink: 0;
}

.effort-label {
    font-size: 10.5px;
    color: #71717a;
    font-weight: 500;
}

.effort-slider-group {
    display: inline-flex;
    background: rgba(0, 0, 0, 0.45);
    border: 1px solid rgba(255, 255, 255, 0.09);
    border-radius: 6px;
    padding: 2px;
    gap: 2px;
}

.effort-step-btn {
    width: 22px !important;
    height: 20px !important;
    padding: 0 !important;
    display: grid !important;
    place-items: center !important;
    border: 0 !important;
    border-radius: 4px !important;
    background: transparent !important;
    color: #71717a !important;
    font-size: 11px !important;
    font-weight: 600 !important;
    cursor: pointer;
    transition: all 0.15s cubic-bezier(0.16, 1, 0.3, 1);
}

.effort-step-btn:hover:not(:disabled) {
    color: #e4e4e7 !important;
    background: rgba(255, 255, 255, 0.07) !important;
}

.effort-step-btn.active {
    background: rgba(99, 102, 241, 0.28) !important;
    color: #c7d2fe !important;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.35);
}

.effort-step-btn.active:hover {
    background: rgba(99, 102, 241, 0.4) !important;
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
    position: absolute;
    bottom: calc(100% + 8px);
    left: 0;
    width: 260px;
    background: #1e1e24;
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 10px;
    box-shadow: 0 16px 36px rgba(0, 0, 0, 0.6), 0 0 0 1px rgba(255, 255, 255, 0.08);
    padding: 6px;
    z-index: 1300;
    display: flex;
    flex-direction: column;
    gap: 4px;
    backdrop-filter: blur(12px);
    animation: popIn 0.15s ease-out;
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
</style>


<style scoped>
/* 🎭 角色档位映射 + 路由模式（追加区块） */
.routing-mode-item {
    flex-direction: column;
    align-items: stretch;
    gap: 8px;
}
.role-mapping-item {
    flex-direction: column;
    align-items: stretch;
    gap: 10px;
}
.role-mapping-grid {
    display: flex;
    flex-direction: column;
    gap: 6px;
}
.role-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    padding: 6px 8px;
    border-radius: 8px;
    background: rgba(255, 255, 255, 0.03);
}
.role-row:hover {
    background: rgba(255, 255, 255, 0.06);
}
.role-cell-label {
    display: flex;
    flex-direction: column;
    min-width: 0;
}
.role-name {
    font-size: 12px;
    font-weight: 600;
}
.role-desc {
    font-size: 10px;
    opacity: 0.55;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}
.role-tier-select {
    flex-shrink: 0;
    min-width: 128px;
    padding: 4px 8px;
    border-radius: 6px;
    border: 1px solid rgba(255, 255, 255, 0.12);
    background: rgba(0, 0, 0, 0.35);
    color: inherit;
    font-size: 12px;
}
.route-badge.mode {
    background: linear-gradient(135deg, #5b8def 0%, #7f5bef 100%);
}
.route-badge.roles {
    background: linear-gradient(135deg, #2f9e6e 0%, #5bc48f 100%);
}
</style>
