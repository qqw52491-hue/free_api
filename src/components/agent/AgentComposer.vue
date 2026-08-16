<template>
    <footer class="manus-composer-wrap">
        <div class="manus-composer" :class="{ waiting: isWaitingForHuman }">
            <textarea
                :value="draft"
                :disabled="isSending"
                :placeholder="placeholderText"
                rows="2"
                @input="$emit('update:draft', ($event.target as HTMLTextAreaElement).value)"
                @keydown.ctrl.enter.prevent="submit"
                @keydown.meta.enter.prevent="submit"
            />
            <div class="composer-bottom-row">
                <div class="composer-tools">
                    <button type="button" title="模型与任务设置" @click="$emit('open-settings')"><svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></svg></button>
                    <button class="model-chip" type="button" title="切换模型" @click="$emit('open-settings')"><span>{{ currentModelName }}</span><svg viewBox="0 0 24 24"><path d="m8 10 4 4 4-4"/></svg></button>
                </div>
                <div class="composer-actions">
                    <button v-if="isRunning" class="stop-control" type="button" :disabled="stopRequested" @click="$emit('stop')">{{ stopRequested ? '停止中' : '停止' }}</button>
                    <button class="submit-control" type="button" :disabled="!canSubmit || isSending" :title="isRunning ? (isWaitingForHuman ? '回答当前请求' : '替换为新任务') : '开始任务'" @click="submit">
                        <svg viewBox="0 0 24 24"><path d="m7 12 5-5 5 5M12 7v10"/></svg>
                    </button>
                </div>
            </div>
        </div>
        <p class="composer-disclaimer">Agent 可能会出错。重要操作请先核对调用详情。</p>
    </footer>
</template>

<script setup lang="ts">
import { computed } from "vue";
import type { AgentHumanForm, AgentModelOption } from "./types";

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
    currentModelId: string;
}>();
const emit = defineEmits<{
    "update:draft": [value: string];
    "update:model": [modelId: string];
    submit: [];
    stop: [];
    "open-settings": [];
}>();

const currentModelName = computed(() => props.allModels.find((model) => model.id === props.currentModelId)?.display_name || "Manus Desktop");
const placeholderText = computed(() => props.isRunning
    ? props.isWaitingForHuman
        ? "补充自定义说明后按回车提交…"
        : "输入新任务；旧模型/工具会在安全边界停止…"
    : "想让 Agent 帮你做什么？",
);
const canSubmit = computed(() => props.isRunning ? Boolean(props.draft.trim()) : props.canStart && Boolean(props.draft.trim()));
function submit() { if (canSubmit.value && !props.isSending) emit("submit"); }
</script>

<style scoped>
.manus-composer-wrap {
    padding: 0 clamp(16px, 4vw, 48px) 14px;
    background: linear-gradient(0deg, #141416 85%, transparent);
}
.manus-composer {
    display: flex;
    flex-direction: column;
    padding: 10px 14px 10px;
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 12px;
    background: #1c1c1f;
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.25);
    transition: all 0.2s ease;
}
.manus-composer:focus-within {
    border-color: rgba(255, 255, 255, 0.22);
    box-shadow: 0 4px 24px rgba(0, 0, 0, 0.35);
}
.manus-composer.waiting {
    border-color: rgba(251, 191, 36, 0.3);
    box-shadow: 0 0 0 1px rgba(251, 191, 36, 0.15);
}

textarea {
    width: 100%;
    min-height: 48px;
    max-height: 180px;
    border: 0;
    padding: 2px 0;
    resize: none;
    color: #f4f4f5;
    background: transparent;
    font: inherit;
    font-size: 13.5px;
    line-height: 1.6;
    outline: none;
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
.composer-tools button {
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

.model-chip {
    width: auto !important;
    height: 28px !important;
    padding: 0 10px !important;
    display: inline-flex !important;
    align-items: center !important;
    gap: 6px !important;
    font-size: 12px !important;
    font-weight: 500 !important;
    color: #d4d4d8 !important;
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

.composer-disclaimer {
    margin: 6px 0 0;
    text-align: center;
    color: #52525b;
    font-size: 11px;
}
</style>
