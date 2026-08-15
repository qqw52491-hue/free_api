<template>
    <div ref="streamElement" class="process-tree" aria-label="Agent 分层执行过程" @scroll.passive="onStreamScroll">
        <button v-if="isRunning && !followLiveTail" type="button" class="follow-latest" @click="resumeLiveFollow">跟随最新输出</button>
        <section v-if="!currentGoal && !timelineEntries.length && !legacyMessages.length" class="empty-conversation">
            <div class="empty-logo">m</div>
            <h2>有什么可以帮你的？</h2>
            <p>输入一个任务。每个回合会保留自己的过程、成果与总结。</p>
        </section>
        <!-- 旧版本没有过程轨迹的消息仍按原样显示；已覆盖的消息会进入对应回合，不再被整体堆到页面顶部。 -->
        <section v-if="legacyMessages.length" class="conversation-history" aria-label="早期对话记录">
            <article v-for="message in legacyMessages" :key="message.id" class="saved-message" :class="message.role === 'user' ? 'from-user' : 'from-agent'">
                <span class="saved-role">{{ message.role === 'user' ? '你' : 'Agent' }}</span>
                <p>{{ message.content }}</p>
                <time>{{ formatConversationTime(message.created_at) }}</time>
            </article>
        </section>
        <template v-for="entry in timelineEntries" :key="entry.id">
            <article v-if="entry.type === 'user'" class="user-message">{{ entry.content }}</article>
            <article v-else-if="entry.type === 'message'" class="saved-message" :class="entry.role === 'user' ? 'from-user' : 'from-agent'" @click="entry.isHuman && $emit('select-human', entry.id)">
                <span class="saved-role">{{ entry.role === 'user' ? '你' : 'Agent' }}</span>
                <p>{{ entry.content }}</p>
                <time>{{ formatClock(entry.timestamp) }}</time>
            </article>
            <article v-else-if="entry.type === 'plan'" class="plan-node" :class="`status-${entry.status}`">
                <div class="plan-row">
                    <span class="plan-mark"><i></i></span>
                    <span class="plan-title"><small>计划</small>{{ entry.title }}</span>
                    <span class="row-status">{{ statusLabel(entry.status) }}</span>
                    <time>{{ formatClock(entry.timestamp) }}</time>
                </div>
                <div class="plan-children">
                    <article v-for="step in entry.steps" :key="step.id" class="goal-node" :class="`status-${step.status}`">
                        <button class="goal-row" :class="{ selected: step.id === selectedStepId }" type="button" @click="$emit('select-step', step.id)">
                            <span class="tree-mark"><i></i></span>
                            <span class="goal-label">{{ step.description || '执行步骤' }}<small v-if="step.modelTier" class="model-tier">{{ step.modelTier }}</small></span>
                            <span class="row-status">{{ statusLabel(step.status) }}</span>
                            <time>{{ formatClock(step.timestamp) }}</time>
                        </button>
                        <div class="node-children">
                            <button v-if="step.thought" class="child-row thought-child" :class="{ selected: step.id === selectedStepId && selectedSubpart === 'thought' }" type="button" @click="$emit('select-step-thought', step.id)">
                                <span class="child-mark"></span><b>思考</b><span>{{ shortText(step.thought, 110) }}</span><em>已分析</em>
                            </button>
                            <button class="child-row tool-child" :class="{ selected: step.id === selectedStepId && selectedSubpart === 'tool' }" type="button" @click="$emit('select-step', step.id)">
                                <span class="child-mark"></span><b>{{ step.tool || '工具' }}</b><span>{{ shortText(step.action || step.explanation || '执行操作', 110) }}</span><em>{{ step.source }}</em>
                            </button>
                        </div>
                    </article>
                    <article v-if="entry.isThinking" class="active-thought-stream" :class="{ selected: selectedNarrative === 'active-thought' }" role="button" tabindex="0" @click="$emit('select-active-thought')" @keydown.enter="$emit('select-active-thought')">
                        <header><span class="child-mark"></span><b>思考</b><em>思考中</em><time>{{ formatClock(entry.thinkingTimestamp || entry.timestamp) }}</time></header>
                        <pre class="active-thought-content">{{ thought || '正在分析下一步操作…' }}</pre>
                    </article>
                </div>
            </article>
            <article v-else-if="entry.type === 'active-thought'" class="active-thought-stream root-thought-stream" :class="{ selected: selectedNarrative === 'active-thought' }" role="button" tabindex="0" @click="$emit('select-active-thought')" @keydown.enter="$emit('select-active-thought')">
                <header><span class="tree-mark"><i></i></span><b>思考</b><em>思考中</em><time>{{ formatClock(entry.timestamp) }}</time></header>
                <pre class="active-thought-content">{{ thought || '正在分析下一步操作…' }}</pre>
            </article>
            <article v-else-if="entry.type === 'completion'" class="saved-message from-agent final-response" :class="{ failed: entry.failed }">
                <span class="saved-role">Agent</span>
                <div v-if="entry.summary" class="summary-rich" aria-label="Agent 最终回复">
                    <div class="markdown-content" v-html="renderFinalMarkdown(entry.summary)"></div>
                </div>
                <p v-else class="final-fallback">{{ entry.title }}</p>
                <div v-if="entry.artifacts.length" class="final-artifacts">
                    <b>交付物</b>
                    <code v-for="artifact in entry.artifacts" :key="artifact">{{ artifact }}</code>
                </div>
                <time>{{ formatClock(entry.timestamp) }}</time>
            </article>
        </template>
        <p v-if="isRunning && !timelineEntries.length && !legacyMessages.length" class="idle-live"><i></i>{{ stateDescription }}</p>
    </div>
</template>
<script setup lang="ts">
import DOMPurify from "dompurify";
import { computed, nextTick, ref, watch } from "vue";
import type { AgentStepItem, AgentToolStatus } from "./types";
import { formatClock, renderMarkdown, shortText } from "./format";
type HistoryMessage = { id: string; role: string; content: string; created_at: string; timestamp: number };
type HistoryTurn = { id: string; goal: string; status: string; steps: AgentStepItem[]; messages: HistoryMessage[]; summary: string; artifacts: string[]; startedAt: number; completedAt: number };
type PlanEntry = { id: string; type: "plan"; timestamp: number; title: string; status: AgentToolStatus; steps: AgentStepItem[]; isThinking: boolean; thinkingTimestamp?: number };
type TimelineEntry =
    | PlanEntry
    | { id: string; type: "user"; timestamp: number; content: string }
    | { id: string; type: "message"; timestamp: number; role: string; content: string; isHuman: boolean }
    | { id: string; type: "completion"; timestamp: number; title: string; summary: string; artifacts: string[]; failed: boolean }
    | { id: "active-thought"; type: "active-thought"; timestamp: number };
const props = defineProps<{
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
    isRunning: boolean;
    completionMessage: string;
    finalSummary: string;
    finalArtifacts: string[];
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
const followLiveTail = ref(true);
const BOTTOM_THRESHOLD_PX = 56;
function isNearBottom(element: HTMLElement) { return element.scrollHeight - element.scrollTop - element.clientHeight <= BOTTOM_THRESHOLD_PX; }
function followLatest(smooth = true) { if (followLiveTail.value) nextTick(() => streamElement.value?.scrollTo({ top: streamElement.value.scrollHeight, behavior: smooth ? "smooth" : "auto" })); }
function onStreamScroll(event: Event) { followLiveTail.value = isNearBottom(event.currentTarget as HTMLElement); }
function resumeLiveFollow() { followLiveTail.value = true; followLatest(false); }
function followThoughtTail() { nextTick(() => Array.from(document.querySelectorAll<HTMLElement>(".active-thought-content")).forEach((element) => { element.scrollTop = element.scrollHeight; })); }
watch(() => props.isThinking, (running, wasRunning) => { if (running && !wasRunning) activeThoughtStartedAt.value = Date.now(); if (!running && wasRunning) activeThoughtStartedAt.value = null; }, { immediate: true });
function planStatus(steps: AgentStepItem[]): AgentToolStatus { if (steps.some((step) => step.status === "running" || step.status === "pending")) return "running"; if (steps.some((step) => step.status === "failed")) return "failed"; return "succeeded"; }
function planEntries(steps: AgentStepItem[], prefix: string, active = false): PlanEntry[] {
    const groups = new Map<string, PlanEntry>();
    for (const step of steps.filter((item) => Boolean(item.description || item.tool || item.action))) {
        const key = step.planId || "plan-default";
        const existing = groups.get(key) || { id: `${prefix}-plan-${key}`, type: "plan" as const, timestamp: step.timestamp, title: step.planTitle || "执行任务", status: step.status, steps: [], isThinking: false };
        existing.steps.push(step);
        existing.timestamp = Math.min(existing.timestamp, step.timestamp);
        if (step.planTitle) existing.title = step.planTitle;
        groups.set(key, existing);
    }
    const plans = [...groups.values()].map((plan) => ({ ...plan, status: planStatus(plan.steps) }));
    if (active && activeThoughtStartedAt.value) {
        const last = plans[plans.length - 1];
        if (last) { last.isThinking = true; last.thinkingTimestamp = activeThoughtStartedAt.value; }
    }
    return plans;
}
function completionArtifacts(value: unknown): string[] { return Array.isArray(value) ? value.map((item) => String(item || "").trim()).filter(Boolean) : []; }
function historyTimeline(): TimelineEntry[] {
    const timeline: TimelineEntry[] = [];
    for (const turn of [...props.historyTurns].sort((a, b) => a.startedAt - b.startedAt)) {
        timeline.push({ id: `history-user-${turn.id}`, type: "user", timestamp: turn.startedAt, content: turn.goal });
        const middle: TimelineEntry[] = [
            ...planEntries(turn.steps, `history-${turn.id}`),
            ...turn.messages.map((message) => ({ id: `history-message-${turn.id}-${message.id}`, type: "message" as const, timestamp: message.timestamp, role: message.role, content: message.content, isHuman: message.role === "user" })),
        ].sort((a, b) => a.timestamp - b.timestamp || a.id.localeCompare(b.id));
        timeline.push(...middle);
        if (turn.summary || turn.artifacts.length || ["succeeded", "failed", "cancelled"].includes(turn.status)) {
            const failed = turn.status === "failed";
            timeline.push({ id: `history-completion-${turn.id}`, type: "completion", timestamp: turn.completedAt, title: failed ? "任务未能顺利完成" : "任务已由 AI 标记完成", summary: turn.summary, artifacts: completionArtifacts(turn.artifacts), failed });
        }
    }
    return timeline;
}
function currentTimeline(): TimelineEntry[] {
    const timeline: TimelineEntry[] = [];
    // 后端已写入同目标的终态回合时，绝不再同时渲染残留实时状态，避免“结束后再折叠/重复”。
    const alreadyPersisted = !props.isRunning && props.currentGoal && props.historyTurns.some((turn) =>
        turn.goal.trim() === props.currentGoal.trim() && ["succeeded", "failed", "cancelled"].includes(turn.status),
    );
    if (alreadyPersisted) return timeline;
    const plans = planEntries(props.steps, "live", props.isThinking);
    const firstTimestamp = plans[0]?.timestamp || Date.now();
    if (props.currentGoal) timeline.push({ id: "live-user-goal", type: "user", timestamp: firstTimestamp - 1, content: props.currentGoal });
    const middle: TimelineEntry[] = [
        ...plans,
        ...props.humanInterventions.map((item, index) => ({ id: `live-human-${item.timestamp}-${index}`, type: "message" as const, timestamp: item.timestamp, role: "user", content: item.message, isHuman: true })),
    ].sort((a, b) => a.timestamp - b.timestamp || a.id.localeCompare(b.id));
    timeline.push(...middle);
    if (props.isThinking && activeThoughtStartedAt.value && !plans.length) timeline.push({ id: "active-thought", type: "active-thought", timestamp: activeThoughtStartedAt.value });
    if (props.completionMessage || props.finalSummary || props.finalArtifacts.length) {
        timeline.push({ id: "live-completion", type: "completion", timestamp: Date.now(), title: props.completionMessage || (props.hasError ? "任务未能完成" : "任务已完成"), summary: props.finalSummary, artifacts: props.finalArtifacts, failed: props.hasError });
    }
    return timeline;
}
const timelineEntries = computed<TimelineEntry[]>(() => [...historyTimeline(), ...currentTimeline()]);
function renderFinalMarkdown(content: string): string {
    return DOMPurify.sanitize(renderMarkdown(content), {
        FORBID_TAGS: ["button", "form", "iframe", "object", "embed", "script", "style"],
        FORBID_ATTR: ["style"],
    });
}
function statusLabel(status: AgentToolStatus) { return { pending: "等待", running: "执行中", succeeded: "完成", failed: "失败" }[status]; }
function formatConversationTime(value: string) { const date = new Date(value); return Number.isNaN(date.getTime()) ? "" : date.toLocaleString("zh-CN", { hour: "2-digit", minute: "2-digit", month: "numeric", day: "numeric" }); }
watch(() => [timelineEntries.value.length, props.thought, props.completionMessage, props.historyTurns.length], () => { followThoughtTail(); followLatest(); }, { flush: "post" });
watch(() => props.isRunning, (running, wasRunning) => { if (running && !wasRunning) { followLiveTail.value = true; followLatest(false); } if (!running && wasRunning) followLatest(); });
</script>
<style scoped>
.process-tree { position: relative; display: flex; min-height: 0; flex: 1; flex-direction: column; gap: 10px; padding: 18px clamp(22px, 4vw, 58px) 22px; overflow-y: auto; overscroll-behavior: contain; }.follow-latest { position: sticky; z-index: 4; top: 8px; align-self: flex-end; border: 1px solid #4a566b; border-radius: 999px; padding: 6px 10px; color: #d9e7ff; cursor: pointer; background: rgba(19, 31, 49, .94); box-shadow: 0 4px 16px rgba(0,0,0,.22); font: 600 11px inherit; }.follow-latest:hover { border-color: #7aa8e8; background: #203552; }.empty-conversation { display: grid; min-height: 100%; align-content: center; justify-items: center; text-align: center; }.empty-logo { display: grid; width: 34px; height: 34px; place-items: center; border-radius: 10px; color: #18181a; background: #ececef; font-family: Georgia, serif; font-size: 20px; font-weight: 800; }.empty-conversation h2 { margin: 14px 0 6px; color: #f1f1f1; font-size: 23px; letter-spacing: -.035em; }.empty-conversation p { max-width: 480px; margin: 0; color: #8b8b8f; font-size: 12px; line-height: 1.7; }
.conversation-history { display: flex; width: min(820px, 100%); flex-direction: column; gap: 12px; align-self: flex-start; margin-bottom: 6px; }.saved-message { position: relative; max-width: min(720px, calc(100% - 12px)); padding: 13px 15px 29px; border: 1px solid #3a3b43; border-radius: 12px; box-shadow: 0 8px 22px rgba(0, 0, 0, 0.16); }.saved-message.from-user { align-self: flex-end; border-color: #414149; border-bottom-right-radius: 5px; background: linear-gradient(135deg, #303035, #29292e); }.saved-message.from-agent { align-self: flex-start; border-bottom-left-radius: 5px; background: linear-gradient(135deg, #303238, #27282d); }.saved-role { display: inline-flex; align-items: center; min-height: 16px; color: #c2c4ce; font-size: 10px; font-weight: 750; letter-spacing: 0.03em; }.saved-message p { margin: 6px 0 0; color: #f0f0f2; font-size: 12px; line-height: 1.7; white-space: pre-wrap; }.saved-message time { position: absolute; right: 13px; bottom: 9px; color: #9496a1; font-size: 9px; letter-spacing: 0.02em; }.user-message { align-self: flex-end; max-width: min(720px, 90%); padding: 9px 12px; border-radius: 10px 10px 3px 10px; color: #eeeeef; background: #2b2b2f; font-size: 13px; line-height: 1.6; white-space: pre-wrap; }.tree-list { display: flex; width: min(920px, 100%); flex-direction: column; align-self: flex-start; gap: 8px; }.plan-node { overflow: hidden; border: 1px solid #35353a; border-radius: 8px; background: #1f2023; }.plan-row { display: grid; grid-template-columns: 17px minmax(0,1fr) auto auto; gap: 8px; align-items: center; min-height: 33px; padding: 0 9px; color: #e1e1e4; background: #27282c; font-size: 12px; }.plan-mark { display: grid; width: 14px; height: 14px; place-items: center; border: 1px solid #74747a; border-radius: 4px; }.plan-mark i { width: 5px; height: 5px; border-radius: 50%; background: #c6c6cb; }.plan-title { overflow: hidden; font-weight: 700; text-overflow: ellipsis; white-space: nowrap; }.plan-title small,.model-tier { margin-right: 7px; color: #9292a0; font-size: 9px; font-weight: 700; letter-spacing: .06em; text-transform: uppercase; }.model-tier { margin-left: 7px; }.plan-children { padding: 4px 8px 7px 20px; }.goal-node { margin: 0; }.goal-row,.root-row,.child-row { display: grid; width: 100%; align-items: center; border: 1px solid transparent; color: #a9a9ae; cursor: pointer; background: transparent; font: inherit; text-align: left; }.goal-row,.root-row { grid-template-columns: 17px minmax(0,1fr) auto auto; gap: 8px; min-height: 29px; padding: 4px 8px; border-radius: 5px; font-size: 12px; }.goal-row:hover,.goal-row.selected,.root-row:hover,.root-row.selected,.child-row:hover,.child-row.selected { border-color: #3b3b40; background: #29292d; }.tree-mark { display: grid; width: 15px; height: 15px; place-items: center; border: 1px solid #65656a; border-radius: 4px; }.tree-mark i { width: 5px; height: 5px; border-radius: 50%; background: #b5b5ba; }.goal-label { overflow: hidden; color: #e4e4e6; font-weight: 630; text-overflow: ellipsis; white-space: nowrap; }.row-status,.root-row em { color: #8e8e94; font-size: 10px; font-style: normal; }.goal-row time,.root-row time { color: #6d6d73; font-size: 10px; }.status-running .tree-mark,.active-thought .tree-mark { border-color: #dfdfe2; }.status-running .tree-mark i,.active-thought .tree-mark i { background: #f1f1f2; animation: work-pulse 1s ease-in-out infinite; }.status-failed .tree-mark { border-color: #b76e73; }.status-failed .tree-mark i { background: #dc858a; }
.node-children { position: relative; display: flex; margin: 0 0 2px 15px; padding: 1px 0 1px 14px; flex-direction: column; gap: 1px; }.node-children::before { position: absolute; top: 0; bottom: 0; left: 1px; width: 1px; background: #3b3b40; content: ""; }.child-row { grid-template-columns: 10px 74px minmax(0,1fr) auto; gap: 7px; min-height: 25px; padding: 3px 7px; border-radius: 4px; font-size: 11px; }.child-mark { width: 6px; height: 6px; border-radius: 50%; background: #737378; }.thought-child .child-mark { background: #bebec3; }.child-row b { overflow: hidden; color: #c7c7cb; font-weight: 600; text-overflow: ellipsis; white-space: nowrap; }.child-row span:not(.child-mark) { overflow: hidden; color: #87878d; text-overflow: ellipsis; white-space: nowrap; }.child-row em { color: #74747a; font-size: 10px; font-style: normal; }.human-row { grid-template-columns: 17px minmax(100px,auto) minmax(0,1fr) auto auto; }.human-row b { color: #d8d3d3; font-weight: 600; }.human-row > span:not(.tree-mark) { overflow: hidden; color: #8d8989; text-overflow: ellipsis; white-space: nowrap; }.human-row .tree-mark { border-radius: 50%; }.human-row .tree-mark i { background: #d0c5c5; }.active-thought { grid-template-columns: 17px 74px minmax(0,1fr) auto auto; }.active-thought b { color: #e5e5e7; font-weight: 600; }.active-thought > span:not(.tree-mark) { overflow: hidden; color: #a5a5aa; text-overflow: ellipsis; white-space: nowrap; }.active-thought em { color: #e4e4e7; font-size: 10px; font-style: normal; }
.idle-live { display: flex; width: min(900px,100%); align-items: center; gap: 8px; margin: 0; padding: 4px 8px; color: #85858b; font-size: 11px; }.idle-live i { width: 6px; height: 6px; border-radius: 50%; background: #eee; animation: work-pulse 1s ease-in-out infinite; }.result-line { width: min(900px,100%); align-self: flex-start; padding: 7px 9px; border-left: 2px solid #85858a; color: #aaaab0; }.result-line.failed { border-color: #d47e83; }.result-line > span { color: #85858b; font-size: 10px; font-weight: 800; letter-spacing: .05em; }.result-line strong { display: block; margin-top: 3px; color: #ececee; font-size: 13px; }.result-line p { margin: 5px 0 0; font-size: 12px; line-height: 1.6; }
@keyframes work-pulse { 0%,100% { opacity:.35; transform:scale(.8); } 50% { opacity:1; transform:scale(1); } }
@media (max-width:680px) { .process-tree { padding: 16px 12px 20px; }.goal-row,.root-row { grid-template-columns: 17px minmax(0,1fr) auto; }.goal-row time,.root-row time,.child-row em { display:none; }.child-row { grid-template-columns: 10px 62px minmax(0,1fr); }.human-row,.active-thought { grid-template-columns:17px minmax(0,1fr) auto; } }
.active-thought-stream { margin: 4px 0 4px 15px; border: 1px solid #3c3c42; border-radius: 6px; cursor: pointer; background: #242428; }.active-thought-stream.selected,.active-thought-stream:hover { border-color: #61616a; background: #29292e; }.active-thought-stream header { display: grid; grid-template-columns: 10px auto minmax(0,1fr) auto; gap: 7px; align-items: center; min-height: 25px; padding: 0 8px; color: #cfcfd4; font-size: 11px; }.active-thought-stream header em { color: #94949a; font-size: 10px; font-style: normal; }.active-thought-stream header time { color: #73737a; font-size: 10px; }.active-thought-content { max-height: 124px; margin: 0; padding: 6px 8px 8px 25px; overflow: auto; color: #a9a9af; font: 11px/1.6 ui-monospace, SFMono-Regular, Menlo, monospace; white-space: pre-wrap; scrollbar-width: thin; }.root-thought-stream { margin-left: 0; }.final-artifacts { display: flex; margin-top: 8px; gap: 5px; flex-direction: column; }.final-artifacts b { color: #bdbdc4; font-size: 10px; }.final-artifacts code { overflow: hidden; color: #88888f; font: 10px ui-monospace, SFMono-Regular, Menlo, monospace; text-overflow: ellipsis; white-space: nowrap; }.final-artifacts code.markdown { color: #c8d5e6; }

.summary-rich { display: flex; margin-top: 9px; gap: 5px; flex-direction: column; color: #d6d6da; }.summary-rich h3 { margin: 3px 0 1px; color: #ececef; font-size: 13px; }.summary-rich p,.summary-rich li,.summary-rich blockquote { margin: 0; color: #b9b9bf; font-size: 12px; line-height: 1.68; white-space: pre-wrap; }.summary-rich li { margin-left: 18px; }.summary-rich blockquote { border-left: 2px solid #63636b; padding-left: 9px; color: #c7c7cc; }.result-line strong + .summary-rich { margin-top: 7px; }

/* 窄窗口中外框保持收敛，内部网格保持稳定最小宽度并在卡片内横向滚动，绝不再把文字挤成碎片。 */
.process-tree { padding: 14px clamp(12px, 2.4vw, 30px) 20px; gap: 8px; }
.tree-list { width: min(760px, 100%); min-width: 0; }
.plan-node { width: min(760px, 100%); overflow-x: auto; scrollbar-width: thin; }
.plan-row,.plan-children { min-width: 510px; }
.goal-row { min-width: 486px; }
.child-row { min-width: 460px; }
.plan-row,.goal-row,.child-row { box-sizing: border-box; }
.plan-title,.goal-label,.child-row > span:not(.child-mark) { min-width: 0; }
.result-line { width: min(760px, 100%); box-sizing: border-box; }
.summary-rule { width: 100%; border: 0; border-top: 1px solid #44444a; margin: 7px 0; }
.summary-table { max-width: 100%; margin: 2px 0; overflow-x: auto; color: #aeb0b8; font: 10px/1.55 ui-monospace, SFMono-Regular, Menlo, monospace; white-space: pre; }
@media (max-width: 580px) { .plan-row,.plan-children { min-width: 476px; } .goal-row { min-width: 454px; } .child-row { min-width: 430px; } }


/* 直接子计划卡是当前模板的真实布局节点：默认较窄、宽度稳定，内容过长在卡片内部滚动，绝不压成一条线。 */
.process-tree > .plan-node { align-self: flex-start; width: min(720px, 100%); min-width: min(480px, 100%); flex: 0 0 auto; overflow: hidden; }
.process-tree > .plan-node .plan-row { min-width: 0; grid-template-columns: 17px minmax(260px, 1fr) max-content max-content; }
.process-tree > .plan-node .plan-children { min-width: 0; overflow-x: auto; overflow-y: visible; }
.process-tree > .plan-node .goal-row { min-width: 440px; }
.process-tree > .plan-node .child-row { min-width: 416px; }
.process-tree > .plan-node .goal-node { min-width: 440px; }
.summary-rule { display: none; }
@media (max-width: 540px) { .process-tree > .plan-node { min-width: 100%; } .process-tree > .plan-node .goal-row,.process-tree > .plan-node .goal-node { min-width: 410px; } .process-tree > .plan-node .child-row { min-width: 386px; } }


/* 任务总结 Markdown：使用结构化节点渲染，表格与行内代码均不依赖危险的 HTML 注入。 */
.summary-rich code { padding: 1px 4px; border: 1px solid #44444a; border-radius: 4px; color: #d9d9de; background: #28282c; font: 10px/1.45 ui-monospace, SFMono-Regular, Menlo, monospace; }
.summary-rich .markdown-h1 { margin-top: 3px; font-size: 15px; }.summary-rich .markdown-h2 { margin-top: 10px; font-size: 13px; }.summary-rich .markdown-h3,.summary-rich .markdown-h4,.summary-rich .markdown-h5,.summary-rich .markdown-h6 { margin-top: 8px; font-size: 12px; }
.summary-table-wrap { max-width: 100%; overflow-x: auto; margin: 4px 0 8px; border: 1px solid #414147; border-radius: 6px; }
table.summary-table { width: 100%; min-width: 340px; border-collapse: collapse; color: #c9c9ce; font-size: 11px; line-height: 1.5; }
.summary-table th,.summary-table td { padding: 6px 8px; border-right: 1px solid #3c3c42; border-bottom: 1px solid #3c3c42; text-align: left; vertical-align: top; white-space: nowrap; }
.summary-table th:last-child,.summary-table td:last-child { border-right: 0; }.summary-table tr:last-child td { border-bottom: 0; }.summary-table th { color: #eeeeef; background: #2b2b2f; font-weight: 700; }.summary-table td { background: #222225; }


/* 最终回复与普通 Agent 消息一致，只保留结果本身，不再突出固定“任务总结”系统面板。 */
.final-response { width: min(720px, 92%); padding-bottom: 20px; }.final-response .summary-rich { margin-top: 5px; }.final-response .final-fallback { margin: 5px 0 0; color: #dedee1; font-size: 12px; line-height: 1.62; white-space: pre-wrap; }.final-response.failed { border-color: #76484c; }.final-response .final-artifacts { margin-top: 10px; }

.final-response {
  width: min(860px, 100%);
  max-width: 100%;
  padding: 9px 0 23px;
  border: 0;
  border-radius: 0;
  background: transparent;
}
.final-response .saved-role {
  display: block;
  margin: 0 0 7px;
  color: #b1b5bd;
  font-size: 11px;
  letter-spacing: .02em;
}
.final-response > time { right: 0; bottom: 4px; }
.summary-rich {
  margin: 0;
  color: #e6e7eb;
  font-size: 15px;
  line-height: 1.82;
  letter-spacing: .003em;
  overflow-wrap: anywhere;
}
.summary-rich :deep(h1),
.summary-rich :deep(h2),
.summary-rich :deep(h3),
.summary-rich :deep(h4),
.summary-rich :deep(h5),
.summary-rich :deep(h6) {
  color: #f6f7fa;
  line-height: 1.28;
  letter-spacing: -.018em;
}
.summary-rich :deep(h1) { margin: 24px 0 12px; font-size: 25px; }
.summary-rich :deep(h2) { margin: 22px 0 10px; font-size: 21px; }
.summary-rich :deep(h3) { margin: 19px 0 8px; font-size: 18px; }
.summary-rich :deep(h4) { margin: 17px 0 7px; font-size: 16px; }
.summary-rich :deep(h5), .summary-rich :deep(h6) { margin: 15px 0 6px; font-size: 15px; }
.summary-rich :deep(h1:first-child), .summary-rich :deep(h2:first-child), .summary-rich :deep(h3:first-child) { margin-top: 2px; }
.summary-rich :deep(p) { margin: 0 0 13px; color: inherit; font-size: inherit; line-height: inherit; white-space: normal; }
.summary-rich :deep(ul), .summary-rich :deep(ol) { margin: 7px 0 14px; padding-left: 1.65em; }
.summary-rich :deep(li) { margin: 4px 0; padding-left: 3px; }
.summary-rich :deep(li > ul), .summary-rich :deep(li > ol) { margin: 5px 0 3px; }
.summary-rich :deep(input[type="checkbox"]) { width: 14px; height: 14px; margin: 0 7px 0 0; vertical-align: -2px; accent-color: #84b8ff; }
.summary-rich :deep(blockquote) { margin: 14px 0; padding: 5px 0 5px 15px; border-left: 3px solid #6186b8; color: #b9c5d5; }
.summary-rich :deep(blockquote p:last-child) { margin-bottom: 0; }
.summary-rich :deep(hr) { margin: 21px 0; border: 0; border-top: 1px solid #3a3d44; }
.summary-rich :deep(a) { color: #9dcbff; text-decoration: none; }
.summary-rich :deep(a:hover) { text-decoration: underline; }
.summary-rich :deep(code) { padding: .12em .38em; border-radius: 5px; color: #f0d7a1; background: #202126; font: .88em ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; }
.summary-rich :deep(.code-block-wrapper) { margin: 15px 0 18px; overflow: hidden; border: 1px solid #373942; border-radius: 10px; background: #17181c; }
.summary-rich :deep(.code-block-header) { display: flex; align-items: center; min-height: 30px; padding: 0 11px; border-bottom: 1px solid #30323a; color: #aab2bd; background: #202126; font: 600 11px ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; }
.summary-rich :deep(pre) { margin: 0; padding: 14px 16px; overflow: auto; background: transparent; }
.summary-rich :deep(pre code) { display: block; padding: 0; color: #d9dde5; background: transparent; font-size: 13px; line-height: 1.66; }
.summary-rich :deep(table) { display: block; width: max-content; min-width: min(100%, 520px); max-width: 100%; margin: 15px 0 18px; overflow-x: auto; border-collapse: separate; border-spacing: 0; border: 1px solid #3a3d45; border-radius: 9px; }
.summary-rich :deep(th), .summary-rich :deep(td) { padding: 9px 12px; border-right: 1px solid #353840; border-bottom: 1px solid #353840; text-align: left; vertical-align: top; white-space: nowrap; }
.summary-rich :deep(th) { color: #eef3fa; background: #25282f; font-weight: 700; }
.summary-rich :deep(tr:last-child td) { border-bottom: 0; }
.summary-rich :deep(th:last-child), .summary-rich :deep(td:last-child) { border-right: 0; }
@media (max-width: 640px) {
  .final-response { width: 100%; }
  .summary-rich { font-size: 14px; }
  .summary-rich :deep(h1) { font-size: 22px; }
  .summary-rich :deep(h2) { font-size: 19px; }
  .summary-rich :deep(h3) { font-size: 17px; }
  .summary-rich :deep(.code-block-wrapper) { border-radius: 8px; }
  .summary-rich :deep(pre) { padding: 12px; }
}
</style>
