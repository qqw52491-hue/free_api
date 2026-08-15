<template>
    <footer class="manus-composer-wrap">
        <div v-if="isWaitingForHuman" class="waiting-note"><span></span><p>{{ pauseReason || 'Agent 正在等待你的确认或补充信息。' }}</p></div>
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
                    <button class="submit-control" type="button" :disabled="!canSubmit || isSending" :title="isRunning ? '发送补充说明' : '开始任务'" @click="submit">
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
import type { AgentModelOption } from "./types";

const props = defineProps<{
    draft: string;
    isRunning: boolean;
    isWaitingForHuman: boolean;
    isSending: boolean;
    stopRequested: boolean;
    canStart: boolean;
    pauseReason: string;
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
    ? props.isWaitingForHuman ? "输入确认、选择或补充信息…" : "给正在运行的 Agent 发送补充说明…"
    : "想让 Agent 帮你做什么？",
);
const canSubmit = computed(() => props.isRunning ? Boolean(props.draft.trim()) : props.canStart && Boolean(props.draft.trim()));
function submit() { if (canSubmit.value && !props.isSending) emit("submit"); }
</script>

<style scoped>
.manus-composer-wrap { padding: 0 clamp(22px, 5vw, 76px) 12px; background: linear-gradient(0deg, #202022 78%, transparent); }.waiting-note { display: flex; width: min(805px, 100%); align-items: center; gap: 8px; margin: 0 auto 8px; padding: 8px 10px; border: 1px solid #565048; border-radius: 7px; color: #b9ab95; background: rgba(165,131,77,.08); font-size: 11px; }.waiting-note span { width: 6px; height: 6px; border-radius: 50%; background: #c6a56a; }.waiting-note p { margin: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.manus-composer { width: min(805px, 100%); margin: 0 auto; padding: 10px 11px 8px; border: 1px solid #454548; border-radius: 14px; background: #2a2a2d; box-shadow: 0 10px 28px rgba(0,0,0,.18); }.manus-composer:focus-within { border-color: #69696e; }.manus-composer.waiting { border-color: #706047; } textarea { display: block; width: 100%; min-height: 44px; max-height: 180px; resize: vertical; border: 0; outline: 0; color: #ececef; background: transparent; font: 13px/1.55 inherit; } textarea::placeholder { color: #8b8b91; }.composer-bottom-row { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin-top: 7px; }.composer-tools,.composer-actions { display: flex; align-items: center; gap: 6px; }.composer-tools > button,.stop-control { display: grid; height: 25px; place-items: center; border: 1px solid transparent; border-radius: 6px; color: #a2a2a8; cursor: pointer; background: transparent; }.composer-tools > button:hover,.stop-control:hover:not(:disabled) { border-color: #4c4c51; color: #ededee; background: #323236; }.composer-tools > button:first-child { width: 25px; }.composer-tools svg,.submit-control svg { width: 14px; height: 14px; fill: none; stroke: currentColor; stroke-linecap: round; stroke-linejoin: round; stroke-width: 1.8; }.model-chip { display: inline-flex !important; width: auto; gap: 5px; padding: 0 7px; font: 10px inherit; }.model-chip svg { width: 11px; height: 11px; }.stop-control { padding: 0 8px; color: #bd9296; font: 10px inherit; }.submit-control { display: grid; width: 27px; height: 27px; place-items: center; border: 0; border-radius: 50%; color: #1e1e20; cursor: pointer; background: #ececee; }.submit-control:hover:not(:disabled) { background: #fff; }.submit-control:disabled,.stop-control:disabled { cursor: not-allowed; opacity: .43; }.composer-disclaimer { margin: 7px 0 0; color: #68686d; font-size: 10px; text-align: center; }
@media (max-width: 680px) { .manus-composer-wrap { padding: 0 13px 9px; }.model-chip span { display: none; } }
</style>
