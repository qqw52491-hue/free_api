<template>
    <footer class="manus-composer-wrap">
        <div v-if="isWaitingForHuman" class="waiting-note"><span></span><p>{{ pauseReason || 'Agent 正在等待你的确认或补充信息。' }}</p></div>
        <div class="manus-composer" :class="{ waiting: isWaitingForHuman }">
            <section v-if="isWaitingForHuman && humanForm" class="human-form-card" aria-live="polite">
                <strong>{{ humanForm.title }}</strong>
                <p v-if="humanForm.description">{{ humanForm.description }}</p>
                <div v-if="humanForm.kind === 'confirm'" class="human-form-options confirm-options">
                    <button type="button" :class="{ selected: confirmValue }" @click="confirmValue = true">确认</button>
                    <button type="button" :class="{ selected: !confirmValue }" @click="confirmValue = false">取消</button>
                </div>
                <div v-else-if="humanForm.kind === 'single_select'" class="human-form-options">
                    <label v-for="option in humanForm.options" :key="option.id" :class="{ selected: singleValue === option.id }">
                        <input v-model="singleValue" type="radio" :name="`human-form-${humanForm.title}`" :value="option.id" />
                        <span><b>{{ option.label }}</b><small v-if="option.description">{{ option.description }}</small></span>
                    </label>
                </div>
                <div v-else-if="humanForm.kind === 'multi_select'" class="human-form-options">
                    <label v-for="option in humanForm.options" :key="option.id" :class="{ selected: multiValue.includes(option.id) }">
                        <input v-model="multiValue" type="checkbox" :value="option.id" />
                        <span><b>{{ option.label }}</b><small v-if="option.description">{{ option.description }}</small></span>
                    </label>
                </div>
                <textarea v-else v-model="textValue" class="human-form-text" :placeholder="humanForm.placeholder || '请输入你的回答…'" rows="3" />
                <button type="button" class="human-form-submit" :disabled="!canSubmitForm || isSending" @click="submitForm">提交选择</button>
            </section>
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
import { computed, ref, watch } from "vue";
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
    "submit-form": [value: string | string[] | boolean];
    stop: [];
    "open-settings": [];
}>();

const singleValue = ref("");
const multiValue = ref<string[]>([]);
const confirmValue = ref(false);
const textValue = ref("");
watch(() => props.humanForm, (form) => {
    singleValue.value = typeof form?.defaultValue === "string" ? form.defaultValue : "";
    multiValue.value = Array.isArray(form?.defaultValue) ? form.defaultValue : [];
    confirmValue.value = typeof form?.defaultValue === "boolean" ? form.defaultValue : false;
    textValue.value = typeof form?.defaultValue === "string" && form?.kind === "text" ? form.defaultValue : "";
}, { immediate: true, deep: true });
const canSubmitForm = computed(() => {
    const form = props.humanForm;
    if (!form) return false;
    if (form.kind === "single_select") return Boolean(singleValue.value);
    if (form.kind === "multi_select") return multiValue.value.length > 0;
    if (form.kind === "text") return Boolean(textValue.value.trim());
    return true;
});
function submitForm() {
    const form = props.humanForm;
    if (!form || !canSubmitForm.value) return;
    const value = form.kind === "single_select" ? singleValue.value
        : form.kind === "multi_select" ? multiValue.value
        : form.kind === "confirm" ? confirmValue.value
        : textValue.value.trim();
    emit("submit-form", value);
}
const currentModelName = computed(() => props.allModels.find((model) => model.id === props.currentModelId)?.display_name || "Manus Desktop");
const placeholderText = computed(() => props.isRunning
    ? props.isWaitingForHuman
        ? "回答当前问题；内容会写入这一步，不会新增对话消息…"
        : "输入新任务；旧模型/工具会在安全边界停止…"
    : "想让 Agent 帮你做什么？",
);
const canSubmit = computed(() => props.isRunning ? Boolean(props.draft.trim()) : props.canStart && Boolean(props.draft.trim()));
function submit() { if (canSubmit.value && !props.isSending) emit("submit"); }
</script>

<style scoped>
.manus-composer-wrap { padding: 0 clamp(22px, 5vw, 76px) 12px; background: linear-gradient(0deg, #202022 78%, transparent); }.waiting-note { display: flex; width: min(805px, 100%); align-items: center; gap: 8px; margin: 0 auto 8px; padding: 8px 10px; border: 1px solid #565048; border-radius: 7px; color: #b9ab95; background: rgba(165,131,77,.08); font-size: 11px; }.waiting-note span { width: 6px; height: 6px; border-radius: 50%; background: #c6a56a; }.waiting-note p { margin: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.manus-composer { width: min(805px, 100%); margin: 0 auto; padding: 10px 11px 8px; border: 1px solid #454548; border-radius: 14px; background: #2a2a2d; box-shadow: 0 10px 28px rgba(0,0,0,.18); }.manus-composer:focus-within { border-color: #69696e; }.manus-composer.waiting { border-color: #706047; } textarea { display: block; width: 100%; min-height: 44px; max-height: 180px; resize: vertical; border: 0; outline: 0; color: #ececef; background: transparent; font: 13px/1.55 inherit; } textarea::placeholder { color: #8b8b91; }.composer-bottom-row { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin-top: 7px; }.composer-tools,.composer-actions { display: flex; align-items: center; gap: 6px; }.composer-tools > button,.stop-control { display: grid; height: 25px; place-items: center; border: 1px solid transparent; border-radius: 6px; color: #a2a2a8; cursor: pointer; background: transparent; }.composer-tools > button:hover,.stop-control:hover:not(:disabled) { border-color: #4c4c51; color: #ededee; background: #323236; }.composer-tools > button:first-child { width: 25px; }.composer-tools svg,.submit-control svg { width: 14px; height: 14px; fill: none; stroke: currentColor; stroke-linecap: round; stroke-linejoin: round; stroke-width: 1.8; }.model-chip { display: inline-flex !important; width: auto; gap: 5px; padding: 0 7px; font: 10px inherit; }.model-chip svg { width: 11px; height: 11px; }.stop-control { padding: 0 8px; color: #bd9296; font: 10px inherit; }.submit-control { display: grid; width: 27px; height: 27px; place-items: center; border: 0; border-radius: 50%; color: #1e1e20; cursor: pointer; background: #ececee; }.submit-control:hover:not(:disabled) { background: #fff; }.submit-control:disabled,.stop-control:disabled { cursor: not-allowed; opacity: .43; }.composer-disclaimer { margin: 7px 0 0; color: #68686d; font-size: 10px; text-align: center; }
@media (max-width: 680px) { .manus-composer-wrap { padding: 0 13px 9px; }.model-chip span { display: none; } }
.human-form-card { margin: 0 0 10px; padding: 11px; border: 1px solid #6f6048; border-radius: 10px; background: rgba(156,128,73,.08); }.human-form-card > strong { display: block; color: #efe6d5; font-size: 13px; }.human-form-card > p { margin: 5px 0 9px; color: #b9b2a8; font-size: 12px; line-height: 1.5; }.human-form-options { display: grid; gap: 6px; }.human-form-options label, .confirm-options button { display: flex; align-items: flex-start; gap: 8px; padding: 8px 9px; border: 1px solid #4d4c50; border-radius: 7px; color: #d8d8dc; background: #303034; text-align: left; cursor: pointer; }.human-form-options label.selected, .confirm-options button.selected { border-color: #9a8052; background: rgba(163,129,72,.18); }.human-form-options input { margin: 2px 0 0; accent-color: #c09b60; }.human-form-options span { display: grid; gap: 2px; }.human-form-options b { font-size: 12px; font-weight: 600; }.human-form-options small { color: #a3a3a9; font-size: 11px; line-height: 1.35; }.confirm-options { grid-template-columns: repeat(2, minmax(0, 1fr)); }.confirm-options button { justify-content: center; align-items: center; font: inherit; font-size: 12px; }.human-form-text { width: 100%; min-height: 72px; padding: 8px; border: 1px solid #4d4c50; border-radius: 7px; color: #e7e7ea; background: #252528; resize: vertical; font: 12px/1.5 inherit; }.human-form-submit { margin-top: 9px; padding: 7px 12px; border: 1px solid #9a8052; border-radius: 7px; color: #f5ead7; background: rgba(163,129,72,.2); font: 12px/1 inherit; cursor: pointer; }.human-form-submit:disabled { opacity: .45; cursor: not-allowed; }
</style>
