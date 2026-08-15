<template>
    <footer class="agent-composer">
        <div v-if="isWaitingForHuman" class="human-banner">
            <span>需要你的输入</span>
            <p>{{ pauseReason || 'Agent 暂停执行，等待确认、选择或补充信息。' }}</p>
        </div>
        <div class="composer-shell" :class="{ running: isRunning, waiting: isWaitingForHuman }">
            <textarea
                :value="draft"
                :disabled="isSending"
                :placeholder="placeholder"
                rows="3"
                @input="$emit('update:draft', ($event.target as HTMLTextAreaElement).value)"
                @keydown.ctrl.enter.prevent="submit"
                @keydown.meta.enter.prevent="submit"
            />
            <div class="composer-actions">
                <span>{{ isRunning ? '⌘ / Ctrl + Enter 发送补充说明' : '描述目标、范围和预期结果' }}</span>
                <div>
                    <button v-if="isRunning" class="stop-button" type="button" :disabled="stopRequested" @click="$emit('stop')">
                        {{ stopRequested ? '已请求停止' : '停止' }}
                    </button>
                    <button class="send-button" type="button" :disabled="!canSubmit || isSending" @click="submit">
                        {{ isSending ? '发送中' : isRunning ? (isWaitingForHuman ? '继续执行' : '发送') : '开始执行' }}
                    </button>
                </div>
            </div>
        </div>
    </footer>
</template>

<script setup lang="ts">
import { computed } from "vue";

const props = defineProps<{
    draft: string;
    isRunning: boolean;
    isWaitingForHuman: boolean;
    isSending: boolean;
    stopRequested: boolean;
    canStart: boolean;
    pauseReason: string;
}>();

const emit = defineEmits<{
    "update:draft": [value: string];
    submit: [];
    stop: [];
}>();

const canSubmit = computed(() => props.isRunning ? Boolean(props.draft.trim()) : props.canStart);
const placeholder = computed(() => props.isRunning
    ? props.isWaitingForHuman ? "输入确认、选择结果或补充信息…" : "向运行中的 Agent 发送补充说明…"
    : "例如：分析这个项目的状态机，并将风险与下一步以结构化摘要说明。",
);

function submit() {
    if (!canSubmit.value || props.isSending) return;
    emit("submit");
}
</script>

<style scoped>
.agent-composer { padding: 0 clamp(18px, 4vw, 64px) 24px; background: linear-gradient(0deg, var(--chat-bg) 76%, transparent); }
.human-banner { display: flex; align-items: baseline; gap: 10px; width: min(820px, 100%); margin: 0 auto 9px; padding: 9px 12px; border: 1px solid color-mix(in srgb, var(--chat-warning) 38%, transparent); border-radius: 10px; color: var(--chat-text-soft); background: color-mix(in srgb, var(--chat-warning) 7%, transparent); }
.human-banner span { flex: 0 0 auto; color: var(--chat-warning); font-size: 11px; font-weight: 800; }
.human-banner p { margin: 0; overflow: hidden; font-size: 12px; line-height: 1.4; text-overflow: ellipsis; white-space: nowrap; }
.composer-shell { width: min(820px, 100%); margin: 0 auto; padding: 11px 12px 9px; border: 1px solid var(--chat-line-strong); border-radius: 15px; background: var(--chat-composer-bg); box-shadow: 0 16px 36px rgba(0,0,0,.16); }
.composer-shell:focus-within { border-color: color-mix(in srgb, var(--chat-accent) 72%, var(--chat-line-strong)); box-shadow: 0 0 0 3px color-mix(in srgb, var(--chat-accent) 11%, transparent), 0 16px 36px rgba(0,0,0,.16); }
.composer-shell.waiting { border-color: color-mix(in srgb, var(--chat-warning) 44%, var(--chat-line-strong)); }
textarea { display: block; width: 100%; min-height: 58px; resize: vertical; border: 0; outline: 0; color: var(--chat-text); background: transparent; font: 14px/1.55 inherit; }
textarea::placeholder { color: var(--chat-text-faint); }
.composer-actions { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-top: 7px; }
.composer-actions > span { color: var(--chat-text-faint); font-size: 10px; }
.composer-actions > div { display: flex; gap: 7px; }
.stop-button, .send-button { border: 1px solid var(--chat-line-strong); border-radius: 8px; padding: 7px 11px; cursor: pointer; font: inherit; font-size: 12px; font-weight: 700; }
.stop-button { color: var(--chat-text-muted); background: transparent; }
.stop-button:hover:not(:disabled) { border-color: var(--chat-danger); color: var(--chat-danger); }
.send-button { border-color: var(--chat-accent); color: #06111f; background: var(--chat-accent); }
.send-button:hover:not(:disabled) { filter: brightness(1.08); }
.stop-button:disabled, .send-button:disabled { cursor: not-allowed; opacity: .45; }
@media (max-width: 700px) { .agent-composer { padding: 0 13px 14px; } .composer-actions > span { display: none; } }
</style>
