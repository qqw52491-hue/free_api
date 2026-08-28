<template>
    <Teleport to="body">
        <!-- 🖼️ 图片放大全屏预览弹窗 -->
        <Transition name="fade">
            <div v-if="imageModal" class="image-preview-backdrop" @click="$emit('close-image')">
                <div class="image-preview-dialog" @click.stop>
                    <div class="dialog-header">
                        <span class="dialog-title">{{ imageModal.file_name }}</span>
                        <button type="button" class="btn-close-dialog" @click="$emit('close-image')">×</button>
                    </div>
                    <div class="dialog-body">
                        <img
                            :src="imageSrc"
                            :alt="imageModal.file_name"
                            class="full-preview-img"
                        />
                    </div>
                </div>
            </div>
        </Transition>

        <!-- 📄 文档/表格/PDF 富文本在线预览弹窗 -->
        <Transition name="fade">
            <div v-if="docModal" class="doc-preview-backdrop" @click="$emit('close-doc')">
                <div class="doc-preview-dialog" @click.stop>
                    <div class="dialog-header">
                        <div class="dialog-title-row">
                            <span class="dialog-file-icon">{{ getFileBadgeIcon(docModal.file_name) }}</span>
                            <div class="dialog-file-meta">
                                <h3>{{ docModal.file_name }}</h3>
                                <small>{{ docModal.content_type }} · {{ formatFileSize(docModal.size_bytes) }}</small>
                            </div>
                        </div>
                        <div class="dialog-actions">
                            <button type="button" class="btn-copy-doc" @click="handleCopy">
                                {{ copied ? "✓ 已复制" : "复制解析内容" }}
                            </button>
                            <button type="button" class="btn-close-dialog" @click="$emit('close-doc')">×</button>
                        </div>
                    </div>
                    <div class="dialog-body doc-dialog-body">
                        <RichContentRenderer
                            v-if="docModal.prompt_text"
                            :content="docModal.prompt_text"
                            class="doc-markdown-content"
                        />
                        <pre v-else-if="docModal.raw_preview" class="doc-raw-preview">{{ docModal.raw_preview }}</pre>
                        <div v-else class="doc-empty-hint">暂无提取内容</div>
                    </div>
                </div>
            </div>
        </Transition>
    </Teleport>
</template>

<script setup lang="ts">
import { computed, ref } from "vue";
import { convertFileSrc } from "@tauri-apps/api/core";
import RichContentRenderer from "../charts/RichContentRenderer.vue";
import type { ParsedAttachment } from "./types";

const props = defineProps<{
    imageModal: ParsedAttachment | null;
    docModal: ParsedAttachment | null;
}>();

defineEmits<{
    "close-image": [];
    "close-doc": [];
}>();

const copied = ref(false);

const imageSrc = computed(() => {
    if (!props.imageModal) return "";
    if (props.imageModal.image_data_url) return props.imageModal.image_data_url;
    if (props.imageModal.file_path) {
        if (props.imageModal.file_path.startsWith("data:")) return props.imageModal.file_path;
        try {
            return convertFileSrc(props.imageModal.file_path);
        } catch {
            return "";
        }
    }
    return "";
});

function getFileBadgeIcon(fileName: string): string {
    const ext = fileName.split(".").pop()?.toLowerCase() || "";
    if (["xlsx", "xls", "csv", "ods"].includes(ext)) return "📊";
    if (["pdf"].includes(ext)) return "📄";
    if (["docx", "doc"].includes(ext)) return "📝";
    if (["zip", "tar", "gz", "rar", "7z"].includes(ext)) return "📦";
    if (["rs", "ts", "js", "py", "go", "java", "c", "cpp", "html", "css", "json", "sql", "sh"].includes(ext)) return "💻";
    return "📎";
}

function formatFileSize(bytes?: number): string {
    if (!bytes || bytes <= 0) return "";
    if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${bytes} B`;
}

async function handleCopy() {
    if (!props.docModal) return;
    const text = props.docModal.prompt_text || props.docModal.raw_preview || "";
    try {
        await navigator.clipboard.writeText(text);
        copied.value = true;
        setTimeout(() => {
            copied.value = false;
        }, 2000);
    } catch {
        // ignore
    }
}
</script>

<style scoped>
/* 全局弹窗遮罩 */
.image-preview-backdrop,
.doc-preview-backdrop {
    position: fixed;
    inset: 0;
    width: 100vw;
    height: 100vh;
    background: rgba(0, 0, 0, 0.82);
    backdrop-filter: blur(10px);
    -webkit-backdrop-filter: blur(10px);
    z-index: 99999;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 24px;
    box-sizing: border-box;
}

.image-preview-dialog {
    max-width: min(1000px, 92vw);
    max-height: 90vh;
    background: #18181c;
    border: 1px solid rgba(255, 255, 255, 0.16);
    border-radius: 12px;
    box-shadow: 0 24px 60px rgba(0, 0, 0, 0.9);
    display: flex;
    flex-direction: column;
    overflow: hidden;
}

.doc-preview-dialog {
    width: min(1080px, 94vw);
    height: min(850px, 90vh);
    max-height: 92vh;
    background: #18181c;
    border: 1px solid rgba(255, 255, 255, 0.16);
    border-radius: 12px;
    box-shadow: 0 24px 60px rgba(0, 0, 0, 0.9);
    display: flex;
    flex-direction: column;
    overflow: hidden;
}

.dialog-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 18px;
    background: #1f1f24;
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
}

.dialog-title {
    font-size: 13.5px;
    font-weight: 600;
    color: #f4f4f5;
}

.dialog-title-row {
    display: flex;
    align-items: center;
    gap: 12px;
}

.dialog-file-icon {
    font-size: 24px;
}

.dialog-file-meta h3 {
    margin: 0;
    font-size: 13.5px;
    font-weight: 600;
    color: #f4f4f5;
}

.dialog-file-meta small {
    color: #71717a;
    font-size: 11.5px;
}

.dialog-actions {
    display: flex;
    align-items: center;
    gap: 10px;
}

.btn-copy-doc {
    background: rgba(255, 255, 255, 0.08);
    border: 1px solid rgba(255, 255, 255, 0.15);
    color: #e4e4e7;
    font-size: 12px;
    padding: 5px 12px;
    border-radius: 6px;
    cursor: pointer;
    transition: all 0.15s ease;
}

.btn-copy-doc:hover {
    background: rgba(255, 255, 255, 0.16);
    color: #fff;
}

.btn-close-dialog {
    background: transparent;
    border: 0;
    color: #a1a1aa;
    font-size: 20px;
    cursor: pointer;
    padding: 0 4px;
    line-height: 1;
}

.btn-close-dialog:hover {
    color: #ffffff;
}

.dialog-body {
    padding: 16px;
    display: grid;
    place-items: center;
    overflow: auto;
}

.full-preview-img {
    max-width: 100%;
    max-height: calc(85vh - 70px);
    object-fit: contain;
    border-radius: 6px;
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.6);
}

.doc-dialog-body {
    flex: 1;
    overflow-y: auto;
    background: #ffffff;
    padding: 0;
    display: flex !important;
    flex-direction: column;
    height: 100%;
    min-height: 0;
}

.doc-dialog-body :deep(.rich-content-stream) {
    flex: 1;
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
}

.doc-dialog-body :deep(.html-preview-card) {
    flex: 1;
    height: 100% !important;
    min-height: 100% !important;
    margin: 0 !important;
    display: flex;
    flex-direction: column;
    border: none !important;
    border-radius: 0 !important;
    box-shadow: none !important;
    background: #ffffff !important;
}

.doc-dialog-body :deep(.card-body) {
    flex: 1;
    height: 100% !important;
    min-height: 100% !important;
    background: #ffffff !important;
}

.doc-dialog-body :deep(.preview-container),
.doc-dialog-body :deep(.sandbox-iframe) {
    width: 100%;
    height: 100% !important;
    min-height: 100% !important;
    border: none !important;
    border-radius: 0 !important;
    background: #ffffff !important;
    flex: 1;
}

.doc-markdown-content {
    flex: 1;
    color: #18181b;
    background: #ffffff;
    font-size: 13.5px;
    line-height: 1.7;
    padding: 28px 36px;
}

.doc-markdown-content :deep(h1),
.doc-markdown-content :deep(h2),
.doc-markdown-content :deep(h3),
.doc-markdown-content :deep(strong) {
    color: #09090b;
}

.doc-markdown-content :deep(p),
.doc-markdown-content :deep(li) {
    color: #27272a;
}

.doc-raw-preview {
    margin: 0;
    flex: 1;
    white-space: pre-wrap;
    word-break: break-word;
    font-family: monospace;
    font-size: 12px;
    color: #d4d4d8;
    background: #0d0d10;
    padding: 20px;
    border-radius: 0;
    overflow: auto;
}

.doc-empty-hint {
    color: #71717a;
    font-size: 13px;
    text-align: center;
    padding: 40px 0;
}

/* 渐变过渡动画 */
.fade-enter-active,
.fade-leave-active {
    transition: opacity 0.18s ease;
}

.fade-enter-from,
.fade-leave-to {
    opacity: 0;
}
</style>
