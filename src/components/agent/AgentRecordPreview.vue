<template>
    <div v-if="resource" class="record-backdrop" @click.self="$emit('close')">
        <section class="record-dialog" role="dialog" aria-modal="true" :aria-label="`${resource.label} 的任务记录`">
            <header class="record-header">
                <div>
                    <span>持久化记录</span>
                    <h2>{{ resource.label }}</h2>
                    <p>{{ resource.resourceId }}</p>
                </div>
                <button type="button" @click="$emit('close')">关闭</button>
            </header>
            <div v-if="loading" class="record-loading">正在读取记录…</div>
            <p v-else-if="error" class="record-error">{{ error }}</p>
            <div v-else-if="page.records.length === 0" class="record-loading">当前资源还没有可展示的记录。</div>
            <ol v-else class="record-list">
                <li v-for="record in page.records" :key="record.key">
                    <header><strong>{{ record.key }}</strong><time>{{ record.updatedAt || '未知更新时间' }}</time></header>
                    <pre>{{ formatPayload(record.data) }}</pre>
                </li>
            </ol>
            <footer class="record-footer">
                <span>{{ rangeLabel }}</span>
                <div>
                    <button type="button" :disabled="loading || page.offset === 0" @click="$emit('previous')">上一页</button>
                    <button type="button" :disabled="loading || !page.hasMore" @click="$emit('next')">下一页</button>
                </div>
            </footer>
        </section>
    </div>
</template>

<script setup lang="ts">
import { computed } from "vue";
import type { AgentTaskRecordPage, AgentTaskResource } from "./types";
import { formatPayload } from "./format";

const props = defineProps<{
    resource: AgentTaskResource | null;
    page: AgentTaskRecordPage;
    loading: boolean;
    error: string;
}>();

defineEmits<{
    close: [];
    previous: [];
    next: [];
}>();

const rangeLabel = computed(() => {
    if (props.page.total === 0) return "0 条记录";
    const start = props.page.offset + 1;
    const end = props.page.offset + props.page.records.length;
    return `第 ${start}–${end} 条，共 ${props.page.total} 条`;
});
</script>

<style scoped>
.record-backdrop { position: fixed; z-index: 50; inset: 0; display: grid; place-items: center; padding: 24px; background: rgba(3, 7, 15, .72); backdrop-filter: blur(5px); }
.record-dialog { display: grid; width: min(900px, 100%); max-height: min(760px, calc(100vh - 48px)); grid-template-rows: auto minmax(0, 1fr) auto; overflow: hidden; border: 1px solid var(--chat-line-strong); border-radius: 16px; background: #101722; box-shadow: 0 28px 80px rgba(0,0,0,.45); }
.record-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; padding: 18px 20px; border-bottom: 1px solid var(--chat-line); }
.record-header span { color: var(--chat-accent); font-size: 10px; font-weight: 800; letter-spacing: .1em; }
.record-header h2 { margin: 5px 0 3px; color: var(--chat-text); font-size: 18px; }
.record-header p { max-width: 600px; margin: 0; overflow: hidden; color: var(--chat-text-faint); font: 11px/1.4 ui-monospace, SFMono-Regular, Menlo, monospace; text-overflow: ellipsis; white-space: nowrap; }
.record-header button, .record-footer button { border: 1px solid var(--chat-line-strong); border-radius: 7px; padding: 6px 9px; color: var(--chat-text-soft); cursor: pointer; background: var(--chat-tool-bg); font: inherit; font-size: 12px; }
.record-header button:hover, .record-footer button:hover:not(:disabled) { border-color: var(--chat-accent); color: var(--chat-text); }
.record-header button:disabled, .record-footer button:disabled { cursor: not-allowed; opacity: .45; }
.record-loading, .record-error { display: grid; min-height: 190px; place-items: center; margin: 0; padding: 28px; color: var(--chat-text-muted); font-size: 13px; }
.record-error { color: var(--chat-danger); }
.record-list { min-height: 0; margin: 0; padding: 14px 20px; overflow-y: auto; list-style: none; }
.record-list li { margin-bottom: 10px; overflow: hidden; border: 1px solid var(--chat-line); border-radius: 9px; background: rgba(13,20,31,.72); }
.record-list header { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 9px 11px; border-bottom: 1px solid var(--chat-line); color: var(--chat-text-soft); font-size: 12px; }
.record-list time { color: var(--chat-text-faint); font-size: 10px; }
.record-list pre { max-height: 280px; margin: 0; padding: 11px; overflow: auto; color: #cbd8e8; font: 11px/1.6 ui-monospace, SFMono-Regular, Menlo, monospace; white-space: pre-wrap; word-break: break-word; }
.record-footer { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 13px 20px; border-top: 1px solid var(--chat-line); color: var(--chat-text-faint); font-size: 12px; }
.record-footer div { display: flex; gap: 8px; }
@media (max-width: 620px) { .record-backdrop { padding: 10px; } .record-dialog { max-height: calc(100vh - 20px); } .record-header, .record-footer { align-items: flex-start; flex-direction: column; } }
</style>
