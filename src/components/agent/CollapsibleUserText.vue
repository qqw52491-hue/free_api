<script setup lang="ts">
import { computed, ref } from "vue";

/**
 * 用户消息正文：默认折叠（max-height + 底部渐隐 + 底部居中 chevron），
 * 点击展开；展开后仍按字符上限截断，超长指令不会把整条时间线撑爆。
 */
const props = defineProps<{ text: string }>();

const COLLAPSE_CHARS = 240; // 字符数阈值（粗略快速路径）
const COLLAPSE_LINES = 5; // 折叠态约可见行数（配合 CSS max-height）
const EXPANDED_MAX_CHARS = 6000; // 展开态仍截断的硬上限

const expanded = ref(false);

const isLong = computed(
    () =>
        props.text.length > COLLAPSE_CHARS ||
        (props.text.match(/\n/g)?.length ?? 0) >= COLLAPSE_LINES,
);
const truncated = computed(() => props.text.length > EXPANDED_MAX_CHARS);
const shown = computed(() =>
    expanded.value && truncated.value
        ? props.text.slice(0, EXPANDED_MAX_CHARS)
        : props.text,
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
            :title="expanded ? '收起' : '展开全文'"
            @click.stop="toggle"
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
