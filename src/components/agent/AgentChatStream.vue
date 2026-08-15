<template>
    <div ref="streamElement" class="chat-stream" aria-label="Agent 对话与执行过程">
        <div v-if="!currentGoal && timeline.length === 0" class="chat-empty">
            <div class="assistant-avatar">A</div>
            <h2>从一个任务开始</h2>
            <p>描述目标后，Agent 的规划、调用、恢复和结果会按对话顺序出现在这里。工具操作默认只显示一行摘要，点击可在右侧查看完整输入与输出。</p>
        </div>

        <article v-if="currentGoal" class="chat-message message-user">
            <div class="message-avatar">你</div>
            <div class="message-body">
                <div class="message-meta"><span>你的任务</span><time>已提交</time></div>
                <p>{{ currentGoal }}</p>
            </div>
        </article>

        <template v-for="item in timeline" :key="item.id">
            <button
                v-if="item.tool"
                class="activity-row"
                :class="[`tool-${item.tool.status}`, { selected: item.id === selectedItemId }]"
                type="button"
                @click="$emit('select', item.id)"
            >
                <span class="activity-pulse" aria-hidden="true"></span>
                <span class="activity-label">{{ item.tool.source }}</span>
                <span class="activity-name">{{ item.tool.name }}</span>
                <span class="activity-action">{{ item.tool.action }}</span>
                <span class="activity-status">{{ toolStatusLabel(item.tool.status) }}</span>
                <span class="activity-time">{{ formatClock(item.timestamp) }}</span>
            </button>
            <button
                v-else
                class="chat-message message-agent"
                :class="[`kind-${item.kind}`, { selected: item.id === selectedItemId }]"
                type="button"
                @click="$emit('select', item.id)"
            >
                <span class="assistant-avatar">A</span>
                <span class="message-body">
                    <span class="message-meta">
                        <span>{{ messageTitle(item) }}</span>
                        <time>{{ formatClock(item.timestamp) }}</time>
                    </span>
                    <span class="message-copy">{{ item.detail || item.title }}</span>
                </span>
            </button>
        </template>

        <article v-if="completionMessage || finalSummary" class="chat-message message-final" :class="{ failed: hasError }">
            <div class="assistant-avatar">A</div>
            <div class="message-body">
                <div class="message-meta"><span>{{ hasError ? '任务结束：存在异常' : '任务已完成' }}</span><time>最终结果</time></div>
                <h3>{{ completionMessage || (hasError ? '任务未能完成' : '任务已完成') }}</h3>
                <p v-if="finalSummary">{{ finalSummary }}</p>
                <div v-if="finalArtifacts.length" class="artifact-list">
                    <span>产出</span>
                    <code v-for="artifact in finalArtifacts" :key="artifact">{{ artifact }}</code>
                </div>
            </div>
        </article>
    </div>
</template>

<script setup lang="ts">
import { nextTick, ref, watch } from "vue";
import type { AgentTimelineItem, AgentToolStatus } from "./types";
import { formatClock } from "./format";

const props = defineProps<{
    currentGoal: string;
    timeline: AgentTimelineItem[];
    selectedItemId: string | null;
    completionMessage: string;
    finalSummary: string;
    finalArtifacts: string[];
    hasError: boolean;
}>();

defineEmits<{
    select: [id: string];
}>();

const streamElement = ref<HTMLElement | null>(null);

watch(
    () => props.timeline.length,
    () => nextTick(() => streamElement.value?.scrollTo({ top: streamElement.value.scrollHeight, behavior: "smooth" })),
    { flush: "post" },
);

function toolStatusLabel(status: AgentToolStatus) {
    return { pending: "等待", running: "执行中", succeeded: "完成", failed: "失败" }[status];
}

function messageTitle(item: AgentTimelineItem) {
    return item.kind === "human" ? "你补充了说明" : item.title || "Agent";
}
</script>

<style scoped>
.chat-stream {
    display: flex;
    min-height: 0;
    flex: 1;
    flex-direction: column;
    gap: 14px;
    padding: 26px clamp(18px, 4vw, 64px) 34px;
    overflow-y: auto;
    overscroll-behavior: contain;
}
.chat-empty {
    display: grid;
    max-width: 630px;
    min-height: 100%;
    align-content: center;
    justify-items: start;
    margin: 0 auto;
    color: var(--chat-text-muted);
}
.chat-empty h2 {
    margin: 18px 0 8px;
    color: var(--chat-text);
    font-size: 26px;
    letter-spacing: -0.03em;
}
.chat-empty p {
    max-width: 570px;
    margin: 0;
    font-size: 14px;
    line-height: 1.8;
}
.chat-message {
    display: flex;
    width: min(820px, 100%);
    gap: 12px;
    padding: 12px 14px;
    border: 1px solid transparent;
    border-radius: 14px;
    color: inherit;
    text-align: left;
}
button.chat-message {
    cursor: pointer;
    background: transparent;
}
.chat-message:hover,
.chat-message.selected {
    border-color: var(--chat-line-strong);
    background: var(--chat-hover);
}
.message-user {
    align-self: flex-end;
    max-width: min(700px, 92%);
    border-color: var(--chat-user-line);
    background: var(--chat-user-bg);
}
.message-agent {
    align-self: flex-start;
}
.message-final {
    align-self: flex-start;
    border-color: color-mix(in srgb, var(--chat-success) 38%, transparent);
    background: color-mix(in srgb, var(--chat-success) 7%, transparent);
}
.message-final.failed {
    border-color: color-mix(in srgb, var(--chat-danger) 42%, transparent);
    background: color-mix(in srgb, var(--chat-danger) 7%, transparent);
}
.message-avatar,
.assistant-avatar {
    display: grid;
    width: 28px;
    height: 28px;
    flex: 0 0 auto;
    place-items: center;
    border-radius: 9px;
    font-size: 11px;
    font-weight: 800;
}
.message-avatar {
    color: #0c1220;
    background: #dbeafe;
}
.assistant-avatar {
    color: #e8f0fc;
    background: linear-gradient(145deg, #637cff, #8b5cf6);
}
.message-body {
    min-width: 0;
    flex: 1;
}
.message-meta {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    color: var(--chat-text-muted);
    font-size: 11px;
    font-weight: 650;
}
.message-meta time {
    color: var(--chat-text-faint);
    font-weight: 500;
}
.message-body p,
.message-copy {
    display: block;
    margin: 7px 0 0;
    color: var(--chat-text-soft);
    font-size: 14px;
    line-height: 1.65;
    white-space: pre-wrap;
}
.message-final h3 {
    margin: 8px 0 0;
    color: var(--chat-text);
    font-size: 15px;
}
.activity-row {
    display: grid;
    width: min(820px, 100%);
    grid-template-columns: auto auto minmax(0, auto) minmax(0, 1fr) auto auto;
    align-items: center;
    gap: 9px;
    align-self: flex-start;
    padding: 10px 12px;
    border: 1px solid var(--chat-line);
    border-radius: 11px;
    color: var(--chat-text-soft);
    cursor: pointer;
    background: var(--chat-tool-bg);
    font: inherit;
    font-size: 12px;
    text-align: left;
}
.activity-row:hover,
.activity-row.selected {
    border-color: var(--chat-accent);
    background: var(--chat-hover);
}
.activity-pulse {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--chat-text-faint);
}
.tool-running .activity-pulse { background: var(--chat-warning); box-shadow: 0 0 0 4px color-mix(in srgb, var(--chat-warning) 14%, transparent); }
.tool-succeeded .activity-pulse { background: var(--chat-success); }
.tool-failed .activity-pulse { background: var(--chat-danger); }
.activity-label,
.activity-status {
    color: var(--chat-text-faint);
    font-size: 10px;
    font-weight: 760;
    letter-spacing: 0.06em;
}
.activity-name {
    overflow: hidden;
    color: var(--chat-text);
    font-weight: 720;
    text-overflow: ellipsis;
    white-space: nowrap;
}
.activity-action {
    overflow: hidden;
    color: var(--chat-text-muted);
    text-overflow: ellipsis;
    white-space: nowrap;
}
.activity-time { color: var(--chat-text-faint); font-size: 11px; }
.artifact-list {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 7px;
    margin-top: 12px;
    color: var(--chat-text-muted);
    font-size: 12px;
}
.artifact-list code {
    max-width: 240px;
    overflow: hidden;
    padding: 3px 6px;
    border: 1px solid var(--chat-line);
    border-radius: 6px;
    color: var(--chat-text-soft);
    text-overflow: ellipsis;
    white-space: nowrap;
}
@media (max-width: 700px) {
    .chat-stream { padding: 18px 13px 24px; }
    .activity-row { grid-template-columns: auto auto minmax(0, 1fr) auto; }
    .activity-action,
    .activity-time { display: none; }
}
</style>
