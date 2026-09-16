<script setup lang="ts">
import { computed, ref } from "vue";

/**
 * 用户消息正文：默认折叠展示（行钳制+底部渐隐），点击展开；
 * 展开后仍按字符上限截断，超长指令不会把整条时间线撑爆。
 */
const props = defineProps<{ text: string }>();

const COLLAPSE_CHARS = 240; // 字符数阈值（粗略快速路径）
const COLLAPSE_LINES = 5; // 折叠态可见行数
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
</script>

<template>
    <div class="user-text-wrap">
        <div
            class="user-text-clip"
            :class="{ collapsed: isLong && !expanded }"
            :title="isLong && !expanded ? '点击展开全文' : undefined"
            @click="isLong && !expanded && (expanded = true)"
        >
            <p class="user-text-content">{{ shown }}</p>
            <div v-if="isLong && !expanded" class="user-text-fade" aria-hidden="true" />
        </div>
        <p v-if="expanded && truncated" class="user-text-trunc-note">
            内容过长，仅展示前 {{ EXPANDED_MAX_CHARS }} 字符
        </p>
        <button
            v-if="isLong"
            type="button"
            class="user-text-toggle"
            @click.stop="expanded = !expanded"
        >
            {{ expanded ? "收起" : "展开全文" }}
        </button>
    </div>
</template>
