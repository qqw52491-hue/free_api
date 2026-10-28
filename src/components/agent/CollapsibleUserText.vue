<script setup lang="ts">
import { computed, ref } from "vue";

/**
 * 用户消息正文：默认折叠（max-height + 底部渐隐 + 底部居中 chevron），
 * 点击展开；展开后仍按字符上限截断，超长指令不会把整条时间线撑爆。
 */
const props = defineProps<{ text: string }>();

const COLLAPSE_CHARS = 550; // 字符数阈值（约 9-10 行）
const COLLAPSE_LINES = 9; // 折叠态换行数阈值（超 9 行即折叠）
const EXPANDED_MAX_CHARS = 6000; // 展开态仍截断的硬上限

const expanded = ref(false);

const isLong = computed(
    () =>
        props.text.length > COLLAPSE_CHARS ||
        (props.text.match(/\n/g)?.length ?? 0) >= COLLAPSE_LINES,
);
const truncated = computed(() => props.text.length > EXPANDED_MAX_CHARS);
const shown = computed(() =>
    truncated.value ? props.text.slice(0, EXPANDED_MAX_CHARS) : props.text,
);

function toggle() {
    expanded.value = !expanded.value;
}
</script>

<template>
    <div class="user-text-wrap">
        <div
            class="user-text-clip"
            :class="{ collapsed: isLong && !expanded }"
            :title="isLong && !expanded ? '点击展开全文' : undefined"
            @click="isLong && !expanded && toggle()"
        >
            <p class="user-text-content">{{ shown }}</p>
            <div v-if="isLong && !expanded" class="user-text-fade" aria-hidden="true" />
        </div>
        <p v-if="expanded && truncated" class="user-text-trunc-note">
            内容过长，仅展示前 {{ EXPANDED_MAX_CHARS }} 字符
        </p>
        <div
            v-if="isLong"
            class="user-text-expandbar"
            role="button"
            tabindex="0"
            :title="expanded ? '收起' : '展开全文'"
            @click.stop="toggle"
            @keydown.enter.prevent="toggle"
            @keydown.space.prevent="toggle"
        >
            <svg
                class="chev"
                :class="{ up: expanded }"
                width="14"
                height="14"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2.4"
                stroke-linecap="round"
                stroke-linejoin="round"
            >
                <polyline points="6 9 12 15 18 9" />
            </svg>
        </div>
    </div>
</template>

<style scoped>
.user-text-wrap {
    width: 100%;
    box-sizing: border-box;
}

.user-text-content {
    margin: 0;
    color: var(--theme-text-primary, #f4f4f5);
    font-size: 13px;
    line-height: 1.6;
    white-space: pre-wrap;
    word-break: break-word;
    overflow-wrap: anywhere;
}

/* 用户长消息折叠态：约 9 行 max-height 钳制 + 底部渐隐 + 底部居中 chevron 展开条 */
.user-text-clip {
    position: relative;
}

.user-text-clip.collapsed {
    cursor: pointer;
}

.user-text-clip.collapsed .user-text-content {
    max-height: 14.5em; /* ≈9 行 × line-height 1.6 */
    overflow: hidden;
}

.user-text-fade {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    height: 3.2em;
    background: linear-gradient(to bottom, rgba(var(--theme-rgb-raised, 39, 39, 42), 0), var(--theme-bg-raised, #27272a));
    pointer-events: none;
}

/* 底部居中 chevron 展开条：只占一条细带，不抢视觉，也不造成历史时间戳错位 */
.user-text-expandbar {
    display: flex;
    align-items: center;
    justify-content: center;
    margin-top: 4px;
    padding: 2px 0;
    color: var(--theme-text-muted, #71717a);
    cursor: pointer;
    border-radius: 6px;
    transition: color 0.15s ease, background 0.15s ease;
    user-select: none;
    outline: none;
}

.user-text-expandbar:hover,
.user-text-expandbar:focus-visible {
    color: var(--theme-text-secondary, #a1a1aa);
    background: rgba(var(--theme-foreground-rgb), 0.06);
}

.user-text-expandbar .chev {
    transition: transform 0.18s ease;
}

.user-text-expandbar .chev.up {
    transform: rotate(180deg);
}

.user-text-trunc-note {
    margin: 4px 0 0;
    color: var(--theme-text-secondary, #a1a1aa);
    font-size: 11px;
    font-style: italic;
}
</style>
