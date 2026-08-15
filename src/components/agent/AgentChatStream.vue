<template>
    <div ref="streamElement" class="process-tree" aria-label="Agent 分层执行过程">
        <section v-if="!currentGoal && !entries.length && !conversationMessages.length" class="empty-conversation">
            <div class="empty-logo">m</div>
            <h2>有什么可以帮你的？</h2>
            <p>输入一个任务。每个规划步骤会展开其思考和工具动作。</p>
        </section>

        <section v-if="conversationMessages.length" class="conversation-history" aria-label="已保存对话">
            <article v-for="message in conversationMessages" :key="message.id" class="saved-message" :class="message.role === 'user' ? 'from-user' : 'from-agent'">
                <span class="saved-role">{{ message.role === 'user' ? '你' : 'Agent' }}</span>
                <p>{{ message.content }}</p>
                <time>{{ formatConversationTime(message.created_at) }}</time>
            </article>
        </section>
        <article v-if="currentGoal && isRunning" class="user-message">{{ currentGoal }}</article>

        <section v-if="entries.length" class="tree-list">
            <template v-for="entry in entries" :key="entry.id">
                <article v-if="entry.type === 'step'" class="goal-node" :class="`status-${entry.step.status}`">
                    <button class="goal-row" :class="{ selected: entry.step.id === selectedStepId }" type="button" @click="$emit('select-step', entry.step.id)">
                        <span class="tree-mark"><i></i></span>
                        <span class="goal-label">{{ entry.step.description || '执行步骤' }}</span>
                        <span class="row-status">{{ statusLabel(entry.step.status) }}</span>
                        <time>{{ formatClock(entry.step.timestamp) }}</time>
                    </button>
                    <div class="node-children">
                        <button v-if="entry.step.thought" class="child-row thought-child" :class="{ selected: entry.step.id === selectedStepId && selectedSubpart === 'thought' }" type="button" @click="$emit('select-step-thought', entry.step.id)">
                            <span class="child-mark"></span><b>思考</b><span>{{ shortText(entry.step.thought, 110) }}</span><em>已分析</em>
                        </button>
                        <button class="child-row tool-child" :class="{ selected: entry.step.id === selectedStepId && selectedSubpart === 'tool' }" type="button" @click="$emit('select-step', entry.step.id)">
                            <span class="child-mark"></span><b>{{ entry.step.tool || '工具' }}</b><span>{{ shortText(entry.step.action || entry.step.explanation || '执行操作', 110) }}</span><em>{{ entry.step.source }}</em>
                        </button>
                    </div>
                </article>

                <button v-else-if="entry.type === 'human'" class="root-row human-row" :class="{ selected: selectedNarrative === entry.id }" type="button" @click="$emit('select-human', entry.id)">
                    <span class="tree-mark"><i></i></span><b>你补充了说明</b><span>{{ shortText(entry.message, 115) }}</span><em>人工输入</em><time>{{ formatClock(entry.timestamp) }}</time>
                </button>

                <button v-else class="root-row active-thought" :class="{ selected: selectedNarrative === 'active-thought' }" type="button" @click="$emit('select-active-thought')">
                    <span class="tree-mark"><i></i></span><b>思考</b><span>{{ shortText(thought || '正在分析下一步操作…', 115) }}</span><em>思考中</em><time>{{ formatClock(entry.timestamp) }}</time>
                </button>
            </template>
        </section>

        <p v-if="isRunning && !entries.length" class="idle-live"><i></i>{{ stateDescription }}</p>

        <section v-if="(completionMessage || finalSummary) && !(conversationMessages.length && !isRunning)" class="result-line" :class="{ failed: hasError }">
            <span>{{ hasError ? '任务结束：存在异常' : '任务已完成' }}</span><strong>{{ completionMessage || (hasError ? '任务未能完成' : '任务已完成') }}</strong><p v-if="finalSummary">{{ finalSummary }}</p>
        </section>
    </div>
</template>

<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import type { AgentStepItem, AgentToolStatus } from "./types";
import { formatClock, shortText } from "./format";

type TreeEntry =
    | { id: string; type: "step"; timestamp: number; step: AgentStepItem }
    | { id: string; type: "human"; timestamp: number; message: string }
    | { id: "active-thought"; type: "active-thought"; timestamp: number };

const props = defineProps<{
    currentGoal: string;
    conversationMessages: { id: string; role: string; content: string; created_at: string }[];
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
    hasError: boolean;
}>();

defineEmits<{
    "select-step": [id: string];
    "select-step-thought": [id: string];
    "select-human": [id: string];
    "select-active-thought": [];
}>();

const streamElement = ref<HTMLElement | null>(null);
const activeThoughtStartedAt = ref<number | null>(null);

watch(() => props.isThinking, (running, wasRunning) => {
    if (running && !wasRunning) activeThoughtStartedAt.value = Date.now();
    if (!running && wasRunning) activeThoughtStartedAt.value = null;
}, { immediate: true });

const entries = computed<TreeEntry[]>(() => {
    const steps = props.steps.map((step) => ({ id: `step-${step.id}`, type: "step" as const, timestamp: step.timestamp, step }));
    const humans = props.humanInterventions.map((item, index) => ({ id: `human-${item.timestamp}-${index}`, type: "human" as const, timestamp: item.timestamp, message: item.message }));
    const active = props.isThinking && activeThoughtStartedAt.value
        ? [{ id: "active-thought" as const, type: "active-thought" as const, timestamp: activeThoughtStartedAt.value }]
        : [];
    return [...steps, ...humans, ...active].sort((a, b) => a.timestamp - b.timestamp || a.id.localeCompare(b.id));
});

watch(() => [entries.value.length, props.thought, props.completionMessage, props.conversationMessages.length], () => {
    nextTick(() => streamElement.value?.scrollTo({ top: streamElement.value.scrollHeight, behavior: "smooth" }));
}, { flush: "post" });

function statusLabel(status: AgentToolStatus) { return { pending: "等待", running: "执行中", succeeded: "完成", failed: "失败" }[status]; }
function formatConversationTime(value: string) {
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? "" : date.toLocaleString("zh-CN", { hour: "2-digit", minute: "2-digit", month: "numeric", day: "numeric" });
}
</script>

<style scoped>
.process-tree { display: flex; min-height: 0; flex: 1; flex-direction: column; gap: 10px; padding: 18px clamp(22px, 4vw, 58px) 22px; overflow-y: auto; overscroll-behavior: contain; }.empty-conversation { display: grid; min-height: 100%; align-content: center; justify-items: center; text-align: center; }.empty-logo { display: grid; width: 34px; height: 34px; place-items: center; border-radius: 10px; color: #18181a; background: #ececef; font-family: Georgia, serif; font-size: 20px; font-weight: 800; }.empty-conversation h2 { margin: 14px 0 6px; color: #f1f1f1; font-size: 23px; letter-spacing: -.035em; }.empty-conversation p { max-width: 480px; margin: 0; color: #8b8b8f; font-size: 12px; line-height: 1.7; }
.conversation-history { display: flex; width: min(820px, 100%); flex-direction: column; gap: 8px; align-self: flex-start; margin-bottom: 2px; }.saved-message { position: relative; max-width: min(720px, 92%); padding: 8px 10px 17px; border-radius: 8px; border: 1px solid #343438; background: #252527; }.saved-message.from-user { align-self: flex-end; border-bottom-right-radius: 3px; background: #2b2b2f; }.saved-message.from-agent { align-self: flex-start; border-bottom-left-radius: 3px; background: #242426; }.saved-role { color: #a6a6ab; font-size: 10px; font-weight: 700; }.saved-message p { margin: 4px 0 0; color: #dedee1; font-size: 12px; line-height: 1.62; white-space: pre-wrap; }.saved-message time { position: absolute; right: 8px; bottom: 5px; color: #727278; font-size: 9px; }.user-message { align-self: flex-end; max-width: min(720px, 90%); padding: 9px 12px; border-radius: 10px 10px 3px 10px; color: #eeeeef; background: #2b2b2f; font-size: 13px; line-height: 1.6; white-space: pre-wrap; }.tree-list { display: flex; width: min(920px, 100%); flex-direction: column; align-self: flex-start; gap: 2px; }.goal-node { margin: 0; }.goal-row,.root-row,.child-row { display: grid; width: 100%; align-items: center; border: 1px solid transparent; color: #a9a9ae; cursor: pointer; background: transparent; font: inherit; text-align: left; }.goal-row,.root-row { grid-template-columns: 17px minmax(0,1fr) auto auto; gap: 8px; min-height: 29px; padding: 4px 8px; border-radius: 5px; font-size: 12px; }.goal-row:hover,.goal-row.selected,.root-row:hover,.root-row.selected,.child-row:hover,.child-row.selected { border-color: #3b3b40; background: #29292d; }.tree-mark { display: grid; width: 15px; height: 15px; place-items: center; border: 1px solid #65656a; border-radius: 4px; }.tree-mark i { width: 5px; height: 5px; border-radius: 50%; background: #b5b5ba; }.goal-label { overflow: hidden; color: #e4e4e6; font-weight: 630; text-overflow: ellipsis; white-space: nowrap; }.row-status,.root-row em { color: #8e8e94; font-size: 10px; font-style: normal; }.goal-row time,.root-row time { color: #6d6d73; font-size: 10px; }.status-running .tree-mark,.active-thought .tree-mark { border-color: #dfdfe2; }.status-running .tree-mark i,.active-thought .tree-mark i { background: #f1f1f2; animation: work-pulse 1s ease-in-out infinite; }.status-failed .tree-mark { border-color: #b76e73; }.status-failed .tree-mark i { background: #dc858a; }
.node-children { position: relative; display: flex; margin: 0 0 2px 15px; padding: 1px 0 1px 14px; flex-direction: column; gap: 1px; }.node-children::before { position: absolute; top: 0; bottom: 0; left: 1px; width: 1px; background: #3b3b40; content: ""; }.child-row { grid-template-columns: 10px 74px minmax(0,1fr) auto; gap: 7px; min-height: 25px; padding: 3px 7px; border-radius: 4px; font-size: 11px; }.child-mark { width: 6px; height: 6px; border-radius: 50%; background: #737378; }.thought-child .child-mark { background: #bebec3; }.child-row b { overflow: hidden; color: #c7c7cb; font-weight: 600; text-overflow: ellipsis; white-space: nowrap; }.child-row span:not(.child-mark) { overflow: hidden; color: #87878d; text-overflow: ellipsis; white-space: nowrap; }.child-row em { color: #74747a; font-size: 10px; font-style: normal; }.human-row { grid-template-columns: 17px minmax(100px,auto) minmax(0,1fr) auto auto; }.human-row b { color: #d8d3d3; font-weight: 600; }.human-row > span:not(.tree-mark) { overflow: hidden; color: #8d8989; text-overflow: ellipsis; white-space: nowrap; }.human-row .tree-mark { border-radius: 50%; }.human-row .tree-mark i { background: #d0c5c5; }.active-thought { grid-template-columns: 17px 74px minmax(0,1fr) auto auto; }.active-thought b { color: #e5e5e7; font-weight: 600; }.active-thought > span:not(.tree-mark) { overflow: hidden; color: #a5a5aa; text-overflow: ellipsis; white-space: nowrap; }.active-thought em { color: #e4e4e7; font-size: 10px; font-style: normal; }
.idle-live { display: flex; width: min(900px,100%); align-items: center; gap: 8px; margin: 0; padding: 4px 8px; color: #85858b; font-size: 11px; }.idle-live i { width: 6px; height: 6px; border-radius: 50%; background: #eee; animation: work-pulse 1s ease-in-out infinite; }.result-line { width: min(900px,100%); align-self: flex-start; padding: 7px 9px; border-left: 2px solid #85858a; color: #aaaab0; }.result-line.failed { border-color: #d47e83; }.result-line > span { color: #85858b; font-size: 10px; font-weight: 800; letter-spacing: .05em; }.result-line strong { display: block; margin-top: 3px; color: #ececee; font-size: 13px; }.result-line p { margin: 5px 0 0; font-size: 12px; line-height: 1.6; }
@keyframes work-pulse { 0%,100% { opacity:.35; transform:scale(.8); } 50% { opacity:1; transform:scale(1); } }
@media (max-width:680px) { .process-tree { padding: 16px 12px 20px; }.goal-row,.root-row { grid-template-columns: 17px minmax(0,1fr) auto; }.goal-row time,.root-row time,.child-row em { display:none; }.child-row { grid-template-columns: 10px 62px minmax(0,1fr); }.human-row,.active-thought { grid-template-columns:17px minmax(0,1fr) auto; } }
</style>
