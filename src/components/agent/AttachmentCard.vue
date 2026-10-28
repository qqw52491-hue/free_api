<template>
    <div
        class="attachment-chip-card"
        :class="{ 'is-image': isImage }"
        :title="attachment.upload_failed
            ? '附件落盘失败：仅作为本条消息的内存数据发送，历史记录中无法回显'
            : imageUnavailable ? '无法读取图片，原文件或历史副本不可用'
            : (isImage ? '点击放大预览图片' : '点击预览解析内容')"
        @click.stop="handleClick"
    >
        <!-- ⚠️ 落盘失败标记：如实披露附件未持久化，绝不伪装成已保存 -->
        <span v-if="attachment.upload_failed" class="upload-failed-badge">⚠️</span>
        <!-- 1. 图片缩略图卡片 -->
        <template v-if="isImage">
            <div class="thumb-wrap">
                <img
                    v-if="displayImgSrc"
                    :key="displayImgSrc"
                    :src="displayImgSrc"
                    :alt="attachment.file_name"
                    class="img-thumbnail"
                    :class="{ 'is-ready': imageLoaded }"
                    @load="imageLoaded = true"
                    @error="handleImgError"
                />
                <div v-if="!displayImgSrc || !imageLoaded" class="thumb-placeholder">
                    <FileIcon kind="image" :size="20" />
                </div>
                <div class="thumb-overlay">
                    <svg viewBox="0 0 24 24"><path d="M15 3h6v6M9 21H3v-6M21 3l-7 7M3 21l7-7"/></svg>
                </div>
            </div>
            <div class="attachment-meta">
                <span class="file-name" :title="attachment.file_name">{{ attachment.file_name }}</span>
                <span class="file-size">{{ formattedSize }}</span>
            </div>
        </template>

        <!-- 2. 文档/表格/代码/压缩包卡片 -->
        <template v-else>
            <div class="file-icon-badge" :class="badgeClass">
                <FileIcon :name="attachment.file_name" :size="20" />
            </div>
            <div class="attachment-meta">
                <span class="file-name" :title="attachment.file_name">{{ attachment.file_name }}</span>
                <span class="file-size">{{ formattedSize }}</span>
            </div>
        </template>

        <!-- ⤢ 展开回输入框按钮 (专门给粘贴折叠的长文本附件使用，提供撤回/后悔药体验) -->
        <button
            v-if="removable && attachment.is_pasted_snippet"
            type="button"
            class="unpack-attachment-btn"
            title="展开回输入框 (解包还原为文本)"
            @click.stop="$emit('unpack', attachment)"
        >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor"><path d="M15 3h6v6M9 21H3v-6M21 3l-7 7M3 21l7-7"/></svg>
        </button>

        <!-- 移除附件按钮 (用于输入框就绪托盘) -->
        <button
            v-if="removable"
            type="button"
            class="remove-attachment-btn"
            title="移除此附件"
            @click.stop="$emit('remove')"
        >
            <svg viewBox="0 0 24 24"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>
        </button>
    </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import type { ParsedAttachment } from "./types";
import FileIcon from "./FileIcon.vue";

const props = withDefaults(
    defineProps<{
        attachment: ParsedAttachment;
        removable?: boolean;
        sessionId?: string;
    }>(),
    {
        removable: false,
        sessionId: "",
    }
);

const emit = defineEmits<{
    "preview-image": [attachment: ParsedAttachment];
    "preview-doc": [attachment: ParsedAttachment];
    unpack: [attachment: ParsedAttachment];
    remove: [];
}>();

const asyncImgSrc = ref<string>("");
const imageUnavailable = ref(false);
const imageLoaded = ref(false);
let imageRequest = 0;
let loadingImage = false;
watch([
    () => props.attachment.file_path,
    () => props.attachment.image_data_url,
    () => props.sessionId,
], () => {
    imageRequest++;
    asyncImgSrc.value = "";
    imageUnavailable.value = false;
    imageLoaded.value = false;
    loadingImage = false;
});

const isImage = computed(() => {
    const name = props.attachment.file_name || "";
    const type = props.attachment.content_type || "";
    return Boolean(
        props.attachment.image_data_url ||
        type.startsWith("image/") ||
        /\.(png|jpg|jpeg|webp|gif|bmp|svg)$/i.test(name)
    );
});

const displayImgSrc = computed(() => {
    if (imageUnavailable.value) return "";
    if (props.attachment.image_data_url) {
        return props.attachment.image_data_url;
    }
    if (asyncImgSrc.value) {
        return asyncImgSrc.value;
    }
    if (props.attachment.file_path) {
        if (props.attachment.file_path.startsWith("data:")) {
            return props.attachment.file_path;
        }
        try {
            return convertFileSrc(props.attachment.file_path);
        } catch {
            return "";
        }
    }
    return "";
});

async function handleImgError() {
    imageLoaded.value = false;
    if (loadingImage) return;
    if (props.attachment.file_path && !asyncImgSrc.value) {
        loadingImage = true;
        const request = ++imageRequest;
        try {
            const parsed = await invoke<ParsedAttachment>(props.sessionId ? "parse_session_attachment" : "parse_attachment_file", {
                path: props.attachment.file_path,
                sessionId: props.sessionId,
            });
            if (request !== imageRequest) return;
            if (parsed?.image_data_url) {
                asyncImgSrc.value = parsed.image_data_url;
            } else imageUnavailable.value = true;
        } catch (e) {
            if (request !== imageRequest) return;
            imageUnavailable.value = true;
            console.warn("读取缩略图失败:", e);
        } finally {
            if (request === imageRequest) loadingImage = false;
        }
    } else imageUnavailable.value = true;
}

const badgeClass = computed(() => {
    const ext = props.attachment.file_name.split(".").pop()?.toLowerCase() || "";
    if (["xlsx", "xls", "csv"].includes(ext)) return "badge-excel";
    if (["pdf"].includes(ext)) return "badge-pdf";
    if (["docx", "doc"].includes(ext)) return "badge-word";
    if (["zip", "tar", "gz", "rar", "7z"].includes(ext)) return "badge-zip";
    return "badge-generic";
});

const formattedSize = computed(() => {
    const bytes = props.attachment.size_bytes;
    if (!bytes || bytes === 0) return "";
    if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${bytes} B`;
});

function handleClick() {
    if (isImage.value) {
        const imageDataUrl = props.attachment.image_data_url || asyncImgSrc.value;
        emit("preview-image", imageDataUrl
            ? { ...props.attachment, image_data_url: imageDataUrl }
            : props.attachment);
    } else {
        emit("preview-doc", props.attachment);
    }
}
</script>

<style scoped>
.attachment-chip-card {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 4px 8px 4px 5px;
    background: rgba(var(--theme-foreground-rgb), 0.05);
    border: 1px solid rgba(var(--theme-foreground-rgb), 0.09);
    border-radius: 8px;
    max-width: 260px;
    transition: all 0.15s ease;
    user-select: none;
    box-sizing: border-box;
    cursor: pointer;
}

.upload-failed-badge {
    position: absolute;
    top: -6px;
    left: -6px;
    font-size: 12px;
    line-height: 1;
    filter: drop-shadow(0 1px 2px rgba(0, 0, 0, 0.6));
    z-index: 2;
}

.attachment-chip-card:hover {
    background: rgba(var(--theme-foreground-rgb), 0.09);
    border-color: rgba(var(--theme-foreground-rgb), 0.18);
    transform: translateY(-1px);
    box-shadow: var(--theme-shadow, 0 4px 12px rgba(0, 0, 0, 0.3));
}

.attachment-chip-card.is-image {
    padding: 3px 6px 3px 3px;
}

/* 图片缩略图容器 */
.thumb-wrap {
    position: relative;
    width: 38px;
    height: 38px;
    border-radius: 6px;
    overflow: hidden;
    cursor: pointer;
    flex-shrink: 0;
    background: var(--theme-bg-deep, #000);
}

.img-thumbnail {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
    opacity: 0;
}

.img-thumbnail.is-ready { opacity: 1; }

.thumb-placeholder {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    display: grid;
    place-items: center;
    font-size: 16px;
    background: rgba(var(--theme-foreground-rgb), 0.06);
}

.thumb-overlay {
    position: absolute;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    display: grid;
    place-items: center;
    opacity: 0;
    transition: opacity 0.15s ease;
}

.thumb-wrap:hover .thumb-overlay {
    opacity: 1;
}

.thumb-overlay svg {
    width: 14px;
    height: 14px;
    stroke: var(--on-accent);
    stroke-width: 2;
    fill: none;
}

.file-icon-badge {
    width: 32px;
    height: 32px;
    border-radius: 6px;
    display: grid;
    place-items: center;
    font-size: 15px;
    flex-shrink: 0;
    background: rgba(var(--theme-foreground-rgb), 0.07);
    border: 1px solid rgba(var(--theme-foreground-rgb), 0.08);
}

.file-icon-badge.badge-excel { background: rgba(16, 185, 129, 0.15); border-color: rgba(16, 185, 129, 0.3); }
.file-icon-badge.badge-pdf   { background: rgba(239, 68, 68, 0.15);  border-color: rgba(239, 68, 68, 0.3); }
.file-icon-badge.badge-word  { background: rgba(59, 130, 246, 0.15); border-color: rgba(59, 130, 246, 0.3); }
.file-icon-badge.badge-zip   { background: rgba(245, 158, 11, 0.15); border-color: rgba(245, 158, 11, 0.3); }
.file-icon-badge.badge-generic { background: rgba(var(--theme-foreground-rgb), 0.07); border-color: rgba(var(--theme-foreground-rgb), 0.08); }

.attachment-meta {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
    flex: 1;
    cursor: pointer;
}

.attachment-meta .file-name {
    font-size: 11.5px;
    font-weight: 500;
    color: var(--theme-text-primary, #f4f4f5);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    line-height: 1.2;
}

.attachment-meta .file-size {
    font-size: 10px;
    color: var(--theme-text-muted, #71717a);
}

.unpack-attachment-btn {
    display: grid !important;
    place-items: center !important;
    width: 18px !important;
    height: 18px !important;
    border-radius: 50% !important;
    border: 0 !important;
    background: rgba(59, 130, 246, 0.15) !important;
    color: var(--theme-status-info, #60a5fa) !important;
    cursor: pointer;
    padding: 0 !important;
    flex-shrink: 0;
    transition: all 0.12s;
}

.unpack-attachment-btn:hover {
    background: rgba(59, 130, 246, 0.35) !important;
    color: var(--theme-status-info, #93c5fd) !important;
    transform: scale(1.08);
}

.unpack-attachment-btn svg {
    width: 10px;
    height: 10px;
    stroke: currentColor;
    stroke-width: 2.5;
}

.remove-attachment-btn {
    display: grid !important;
    place-items: center !important;
    width: 18px !important;
    height: 18px !important;
    border-radius: 50% !important;
    border: 0 !important;
    background: rgba(var(--theme-foreground-rgb), 0.08) !important;
    color: var(--theme-text-secondary, #a1a1aa) !important;
    cursor: pointer;
    padding: 0 !important;
    flex-shrink: 0;
    transition: all 0.12s;
}

.remove-attachment-btn:hover {
    background: rgba(239, 68, 68, 0.25) !important;
    color: var(--theme-status-danger, #fca5a5) !important;
}

.remove-attachment-btn svg {
    width: 10px;
    height: 10px;
    stroke: currentColor;
    stroke-width: 2.5;
}
</style>
