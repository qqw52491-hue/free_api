<template>
    <div ref="streamElement" class="process-tree" aria-label="Agent 分层执行过程" @scroll.passive="onStreamScroll">
        <button v-if="isRunning && !followLiveTail" type="button" class="follow-latest" @click="resumeLiveFollow">跟随最新输出</button>

        <div class="stream-centered-content">
            <section v-if="!currentGoal && !timelineEntries.length && !legacyMessages.length" class="empty-conversation">
                <div class="empty-logo">m</div>
                <h2>有什么可以帮你的？</h2>
                <p>输入一个任务。每个回合会保留自己的过程、成果与总结。</p>
            </section>

            <!-- 早期对话记录 -->
            <section v-if="legacyMessages.length" class="conversation-history" aria-label="早期对话记录">
                <article v-for="message in legacyMessages" :key="message.id" class="saved-message" :class="message.role === 'user' ? 'from-user' : 'from-agent'">
                    <template v-if="message.role === 'agent' || message.role === 'assistant'">
                        <div class="agent-msg-header">
                            <span class="agent-logo-pill">⚡ Free API</span>
                            <time class="msg-clock">{{ formatConversationTime(message.created_at) }}</time>
                        </div>
                        <div class="agent-text-body markdown-content" v-html="renderFinalMarkdown(message.content)"></div>
                    </template>
                    <template v-else>
                        <p class="user-text-content">{{ message.content }}</p>
                        <time class="msg-clock">{{ formatConversationTime(message.created_at) }}</time>
                    </template>
                </article>
            </section>

            <template v-for="entry in timelineEntries" :key="entry.id">
                <article v-if="entry.type === 'user'" class="user-message">{{ entry.content }}</article>
                <article v-else-if="entry.type === 'message'" class="saved-message" :class="entry.role === 'user' ? 'from-user' : 'from-agent'" @click="entry.isHuman && $emit('select-human', entry.id)">
                    <template v-if="entry.role === 'agent' || entry.role === 'assistant'">
                        <div class="agent-msg-header">
                            <span class="agent-logo-pill">⚡ Free API</span>
                            <time class="msg-clock">{{ formatClock(entry.timestamp) }}</time>
                        </div>
                        <div class="agent-text-body markdown-content" v-html="renderFinalMarkdown(entry.content)"></div>
                    </template>
                    <template v-else>
                        <span v-if="entry.isHuman" class="intervention-indicator">人工干预输入</span>
                        <p class="user-text-content">{{ entry.content }}</p>
                        <time class="msg-clock">{{ formatClock(entry.timestamp) }}</time>
                    </template>
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
                    <div class="agent-msg-header">
                        <span class="agent-logo-pill">⚡ Free API</span>
                        <time class="msg-clock">{{ formatClock(entry.timestamp) }}</time>
                    </div>
                    <div v-if="entry.summary" class="summary-rich markdown-content" aria-label="Agent 最终回复" v-html="renderFinalMarkdown(entry.summary)"></div>
                    <div v-else class="agent-text-body markdown-content" v-html="renderFinalMarkdown(entry.title)"></div>
                    <div v-if="entry.artifacts.length" class="final-artifacts">
                        <b>交付物</b>
                        <code v-for="artifact in entry.artifacts" :key="artifact">{{ artifact }}</code>
                    </div>
                </article>
            </template>
            <p v-if="isRunning && !timelineEntries.length && !legacyMessages.length" class="idle-live"><i></i>{{ stateDescription }}</p>
        </div>
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
.process-tree {
    position: relative;
    display: flex;
    min-height: 0;
    flex: 1;
    flex-direction: column;
    padding: 18px clamp(16px, 4vw, 48px) 24px;
    overflow-y: auto;
    overscroll-behavior: contain;
    background: #141416;
    color: #e4e4e7;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
}

/* 居中阅读流容器 (彻底解决左侧靠边偏离问题) */
.stream-centered-content {
    width: 100%;
    max-width: 820px;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: 12px;
}

/* 浮动跟随最新 */
.follow-latest {
    position: sticky;
    z-index: 4;
    top: 8px;
    align-self: flex-end;
    border: 1px solid rgba(255, 255, 255, 0.15);
    border-radius: 999px;
    padding: 4px 10px;
    color: #d9e7ff;
    cursor: pointer;
    background: rgba(19, 31, 49, 0.94);
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.3);
    font-size: 11px;
    font-weight: 600;
}
.follow-latest:hover {
    border-color: #7aa8e8;
    background: #203552;
}

/* 空状态 */
.empty-conversation {
    display: grid;
    min-height: 60vh;
    align-content: center;
    justify-items: center;
    text-align: center;
    padding: 40px 0;
}
.empty-logo {
    display: grid;
    width: 32px;
    height: 32px;
    place-items: center;
    border-radius: 8px;
    color: #18181a;
    background: #ececef;
    font-family: Georgia, serif;
    font-size: 18px;
    font-weight: 800;
}
.empty-conversation h2 {
    margin: 12px 0 4px;
    color: #f1f1f1;
    font-size: 20px;
    letter-spacing: -0.03em;
}
.empty-conversation p {
    max-width: 440px;
    margin: 0;
    color: #8b8b8f;
    font-size: 12px;
    line-height: 1.6;
}

/* 对话历史 */
.conversation-history {
    display: flex;
    width: 100%;
    flex-direction: column;
    gap: 10px;
    margin-bottom: 4px;
}

.saved-message {
    position: relative;
    display: flex;
    flex-direction: column;
    box-sizing: border-box;
    width: 100%;
}

/* 👤 用户消息气泡 (深色精致圆角气泡) */
.saved-message.from-user, .user-message {
    align-self: flex-end;
    max-width: min(680px, 88%);
    padding: 9px 15px;
    border-radius: 16px 16px 4px 16px;
    background: #27272a;
    border: 1px solid rgba(255, 255, 255, 0.08);
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.2);
    color: #f4f4f5;
    margin: 2px 0;
}

.user-text-content {
    margin: 0;
    color: #f4f4f5;
    font-size: 13px;
    line-height: 1.6;
    white-space: pre-wrap;
    word-break: break-word;
}

/* 🤖 Agent 消息：彻底透明无框排版 */
.saved-message.from-agent {
    align-self: flex-start;
    width: 100%;
    max-width: 100%;
    padding: 4px 0 12px;
    border: none !important;
    border-radius: 0 !important;
    background: transparent !important;
    box-shadow: none !important;
}

.agent-msg-header {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 6px;
}

.agent-logo-pill {
    font-size: 12.5px;
    font-weight: 700;
    color: #f4f4f5;
    letter-spacing: -0.01em;
}

.msg-clock {
    font-size: 10px;
    color: #71717a;
}

.from-user .msg-clock {
    text-align: right;
    margin-top: 3px;
}

.intervention-indicator {
    display: block;
    font-size: 10px;
    font-weight: 700;
    color: var(--orange);
    margin-bottom: 3px;
}

/* ── 📝 Markdown 优雅排版 (标题/段落/列表/表格/代码) ── */
.markdown-content,
.summary-rich,
.agent-text-body {
    width: 100%;
    color: #e4e4e7;
    font-size: 13.5px;
    line-height: 1.78;
    letter-spacing: 0.003em;
    overflow-wrap: anywhere;
}

.markdown-content :deep(h1),
.markdown-content :deep(h2),
.markdown-content :deep(h3),
.markdown-content :deep(h4),
.markdown-content :deep(h5),
.markdown-content :deep(h6) {
    color: #f4f4f5;
    font-weight: 700;
    line-height: 1.35;
    letter-spacing: -0.015em;
    margin: 16px 0 8px;
}
.markdown-content :deep(h1:first-child),
.markdown-content :deep(h2:first-child),
.markdown-content :deep(h3:first-child) {
    margin-top: 2px;
}
.markdown-content :deep(h1) { font-size: 19px; }
.markdown-content :deep(h2) { font-size: 16.5px; }
.markdown-content :deep(h3) { font-size: 14.5px; }
.markdown-content :deep(h4) { font-size: 13.5px; }

.markdown-content :deep(p) {
    margin: 0 0 10px;
    color: inherit;
    font-size: inherit;
    line-height: inherit;
    white-space: normal;
}
.markdown-content :deep(p:last-child) {
    margin-bottom: 0;
}

.markdown-content :deep(ul),
.markdown-content :deep(ol) {
    margin: 6px 0 12px;
    padding-left: 20px;
}
.markdown-content :deep(li) {
    margin: 3px 0;
    line-height: 1.68;
}
.markdown-content :deep(li > ul),
.markdown-content :deep(li > ol) {
    margin: 2px 0 4px;
}

/* 引用块 */
.markdown-content :deep(blockquote) {
    margin: 10px 0;
    padding: 6px 14px;
    border-left: 3px solid var(--accent);
    background: rgba(255, 255, 255, 0.025);
    border-radius: 0 6px 6px 0;
    color: #a1a1aa;
}

/* 表格 (精细边框与悬停) */
.markdown-content :deep(table) {
    width: 100%;
    margin: 12px 0 16px;
    border-collapse: separate;
    border-spacing: 0;
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 8px;
    overflow: hidden;
    font-size: 12.5px;
}
.markdown-content :deep(th) {
    padding: 7px 12px;
    background: #1f2025;
    color: #f4f4f5;
    font-weight: 600;
    text-align: left;
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
    border-right: 1px solid rgba(255, 255, 255, 0.06);
}
.markdown-content :deep(th:last-child) {
    border-right: none;
}
.markdown-content :deep(td) {
    padding: 6px 12px;
    background: #161619;
    color: #d4d4d8;
    border-bottom: 1px solid rgba(255, 255, 255, 0.05);
    border-right: 1px solid rgba(255, 255, 255, 0.05);
}
.markdown-content :deep(td:last-child) {
    border-right: none;
}
.markdown-content :deep(tr:last-child td) {
    border-bottom: none;
}
.markdown-content :deep(tr:hover td) {
    background: #1e1e24;
}

/* 行内代码与代码块 */
.markdown-content :deep(code) {
    padding: 2px 5px;
    border-radius: 4px;
    color: #e5c07b;
    background: #1e1e23;
    font: 12px ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}
.markdown-content :deep(.code-block-wrapper) {
    margin: 12px 0 16px;
    border-radius: 8px;
    overflow: hidden;
    border: 1px solid rgba(255, 255, 255, 0.08);
    background: #18181c;
}
.markdown-content :deep(.code-block-header) {
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: 30px;
    padding: 0 12px;
    background: #202026;
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
    color: #8e8e93;
    font-size: 11px;
}
.markdown-content :deep(pre) {
    margin: 0;
    padding: 12px 14px;
    overflow: auto;
    background: transparent;
}
.markdown-content :deep(pre code) {
    padding: 0;
    color: #d9dde5;
    background: transparent;
    font-size: 12.5px;
    line-height: 1.6;
}

/* 链接 */
.markdown-content :deep(a) {
    color: #60a5fa;
    text-decoration: none;
}
.markdown-content :deep(a:hover) {
    text-decoration: underline;
}

/* ── ⚡ 紧凑分层执行树 ── */
.plan-node {
    width: 100%;
    margin: 3px 0;
    background: transparent !important;
    border: none !important;
}

/* 计划大标题行 (极紧凑，高 24px) */
.plan-row {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 24px;
    padding: 2px 4px;
    color: #f4f4f5;
    font-size: 12px;
}

.plan-mark {
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: rgba(255, 255, 255, 0.08);
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 9px;
    flex-shrink: 0;
}

.plan-mark i {
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: #a1a1aa;
}

.plan-title {
    font-weight: 600;
    color: #e4e4e7;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
}

.plan-title small {
    margin-right: 6px;
    color: var(--accent-light);
    font-size: 9px;
    font-weight: 700;
    text-transform: uppercase;
}

.model-tier {
    margin-left: 6px;
    color: #71717a;
    font-size: 9px;
    font-weight: 600;
}

.plan-children {
    margin-left: 7px;
    padding-left: 12px;
    border-left: 1px solid rgba(255, 255, 255, 0.08);
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-top: 2px;
}

.goal-node {
    margin: 0;
    display: flex;
    flex-direction: column;
}

/* 所有交互按钮重置 */
button.goal-row,
button.child-row,
button.root-row {
    -webkit-appearance: none;
    appearance: none;
    background: transparent !important;
    border: none !important;
    outline: none !important;
    box-shadow: none !important;
    cursor: pointer;
    font: inherit;
    text-align: left;
}

/* 具体执行步骤行 (极紧凑，高 22px) */
.goal-row {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 22px;
    padding: 2px 6px;
    border-radius: 4px;
    color: #d4d4d8;
    font-size: 12px;
    width: 100%;
    transition: background 0.12s;
}

.goal-row:hover {
    background: rgba(255, 255, 255, 0.04) !important;
}

.goal-row.selected {
    background: rgba(108, 99, 255, 0.12) !important;
    color: #ffffff !important;
}

.tree-mark {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: rgba(255, 255, 255, 0.06);
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
}

.tree-mark i {
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: #a1a1aa;
}

.status-succeeded .tree-mark i { background: #55d6a2; }
.status-running .tree-mark i { background: var(--orange); animation: work-pulse 1s ease-in-out infinite; }
.status-failed .tree-mark i { background: var(--red-light); }

.goal-label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 500;
}

.row-status {
    margin-left: auto;
    font-size: 10px;
    color: #71717a;
    font-style: normal;
    flex-shrink: 0;
}

.goal-row time,
.plan-row time,
.child-row time {
    font-size: 9.5px;
    color: #52525b;
    font-family: ui-monospace, monospace;
    flex-shrink: 0;
    margin-left: 6px;
}

/* 子步骤与思考 (极紧凑，高 20px) */
.node-children {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 1px;
    margin-left: 6px;
    padding-left: 10px;
    border-left: 1px solid rgba(255, 255, 255, 0.06);
    margin-bottom: 2px;
}

.child-row {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 20px;
    padding: 1px 6px;
    border-radius: 3px;
    font-size: 11px;
    color: #8e8e93;
    width: 100%;
    transition: background 0.12s;
}

.child-row:hover {
    background: rgba(255, 255, 255, 0.03) !important;
    color: #d4d4d8 !important;
}

.child-row.selected {
    background: rgba(108, 99, 255, 0.1) !important;
    color: #ffffff !important;
}

.child-mark {
    width: 3px;
    height: 3px;
    border-radius: 50%;
    background: #71717a;
    flex-shrink: 0;
}

.child-row b {
    font-weight: 600;
    color: #b4b4b9;
    flex-shrink: 0;
}

.child-row span:not(.child-mark) {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
}

.child-row em {
    color: #636368;
    font-size: 9.5px;
    font-style: normal;
    margin-left: auto;
    flex-shrink: 0;
}

/* 实时思考流 */
.active-thought-stream {
    margin: 2px 0 2px 6px;
    border-radius: 4px;
    cursor: pointer;
    background: rgba(255, 255, 255, 0.02);
    border: 1px dashed rgba(255, 255, 255, 0.08);
}

.active-thought-stream header {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 2px 8px;
    font-size: 11px;
    color: #cfcfd4;
}

.active-thought-stream header b { color: var(--accent-light); }
.active-thought-stream header em { color: #94949a; font-size: 10px; font-style: normal; margin-left: auto; }
.active-thought-stream header time { color: #73737a; font-size: 10px; }

.active-thought-content {
    max-height: 90px;
    margin: 0;
    padding: 4px 8px 6px;
    overflow: auto;
    color: #a9a9af;
    font: 11px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace;
    white-space: pre-wrap;
}

/* 交付物与总结 */
.final-response {
    width: 100%;
    max-width: 100%;
    padding: 4px 0 16px;
    border: 0;
    background: transparent;
}

.final-artifacts {
    display: flex;
    margin-top: 8px;
    gap: 4px;
    flex-direction: column;
}

.final-artifacts b { color: #bdbdc4; font-size: 10px; }
.final-artifacts code {
    color: #88888f;
    font: 10px ui-monospace, monospace;
    background: #1c1c20;
    padding: 2px 6px;
    border-radius: 4px;
}

.idle-live {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0;
    padding: 2px 4px;
    color: #85858b;
    font-size: 11px;
}

.idle-live i {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: #eee;
    animation: work-pulse 1s ease-in-out infinite;
}

@keyframes work-pulse {
    0%, 100% { opacity: 0.35; transform: scale(0.8); }
    50% { opacity: 1; transform: scale(1); }
}
</style>

