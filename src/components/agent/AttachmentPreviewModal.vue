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
                    <div class="dialog-body image-dialog-body">
                        <img
                            v-if="imageSrc && !imageFailed"
                            :key="imageSrc"
                            :src="imageSrc"
                            :alt="imageModal.file_name"
                            class="full-preview-img"
                            :class="{ 'is-ready': imageReady }"
                            @load="imageReady = true"
                            @error="imageFailed = true"
                        />
                        <div v-if="!imageReady || imageFailed" class="image-preview-status" role="status">
                            {{ imageFailed || !imageSrc ? '图片无法加载，请关闭后重试' : '正在加载图片…' }}
                        </div>
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
                            <button
                                v-if="docModal.is_pasted_snippet"
                                type="button"
                                class="btn-action btn-unpack"
                                title="将此文本展开倒回下方输入框"
                                @click="handleUnpack"
                            >
                                ⤢ 展开回输入框
                            </button>
                            <button
                                v-if="docModal.is_pasted_snippet || docModal.content_type.startsWith('text/') || isTextLike(docModal.file_name)"
                                type="button"
                                class="btn-action btn-edit-toggle"
                                :class="{ active: isEditing }"
                                :title="isEditing ? '切回排版预览' : '直接在线编辑修改文本'"
                                @click="toggleEdit"
                            >
                                {{ isEditing ? "👁️ 预览" : "✏️ 编辑文本" }}
                            </button>
                            <button
                                v-if="isEditing"
                                type="button"
                                class="btn-action btn-save"
                                title="保存当前编辑修改"
                                @click="handleSave"
                            >
                                {{ saveNotice ? "✓ 已保存" : "💾 保存修改" }}
                            </button>
                            <button type="button" class="btn-copy-doc" @click="handleCopy">
                                {{ copied ? "✓ 已复制" : "复制" }}
                            </button>
                            <button type="button" class="btn-close-dialog" @click="$emit('close-doc')">×</button>
                        </div>
                    </div>
                    <div class="dialog-body doc-dialog-body">
                        <textarea
                            v-if="isEditing"
                            v-model="editDraft"
                            class="doc-edit-textarea"
                            placeholder="在此编辑文本内容..."
                            spellcheck="false"
                        ></textarea>
                        <RichContentRenderer
                            v-else-if="docModal.prompt_text"
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
import { formatFileSize } from "./formatUtils";
import { computed, ref, watch } from "vue";
import { convertFileSrc } from "@tauri-apps/api/core";
import RichContentRenderer from "../charts/RichContentRenderer.vue";
import type { ParsedAttachment } from "./types";

const props = defineProps<{
    imageModal: ParsedAttachment | null;
    docModal: ParsedAttachment | null;
}>();

const emit = defineEmits<{
    "close-image": [];
    "close-doc": [];
    "unpack-doc": [attachment: ParsedAttachment];
    "update-doc": [attachment: ParsedAttachment, newText: string];
}>();

const copied = ref(false);
const isEditing = ref(false);
const editDraft = ref("");
const saveNotice = ref(false);

watch(
    () => props.docModal,
    (modal) => {
        if (modal) {
            editDraft.value = modal.prompt_text || modal.raw_preview || "";
            isEditing.value = false;
            saveNotice.value = false;
        }
    },
    { immediate: true }
);

function toggleEdit() {
    isEditing.value = !isEditing.value;
}

function handleSave() {
    if (!props.docModal) return;
    emit("update-doc", props.docModal, editDraft.value);
    saveNotice.value = true;
    setTimeout(() => {
        saveNotice.value = false;
    }, 1500);
}

function handleUnpack() {
    if (!props.docModal) return;
    emit("unpack-doc", props.docModal);
    emit("close-doc");
}

function isTextLike(fileName: string): boolean {
    return /\.(txt|md|markdown|json|js|ts|vue|rs|py|html|css|scss|sh|yaml|yml|xml|sql|toml)$/i.test(fileName);
}

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

const imageReady = ref(false);
const imageFailed = ref(false);
watch(imageSrc, () => {
    imageReady.value = false;
    imageFailed.value = false;
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
    background: var(--theme-bg-panel, #18181c);
    border: 1px solid rgba(var(--theme-foreground-rgb), 0.16);
    border-radius: 12px;
    box-shadow: var(--theme-shadow, 0 24px 60px rgba(0, 0, 0, 0.9));
    display: flex;
    flex-direction: column;
    overflow: hidden;
}

.doc-preview-dialog {
    width: min(1080px, 94vw);
    height: min(850px, 90vh);
    max-height: 92vh;
    background: var(--theme-bg-panel, #18181c);
    border: 1px solid rgba(var(--theme-foreground-rgb), 0.16);
    border-radius: 12px;
    box-shadow: var(--theme-shadow, 0 24px 60px rgba(0, 0, 0, 0.9));
    display: flex;
    flex-direction: column;
    overflow: hidden;
}

.dialog-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 18px;
    background: var(--theme-bg-panel, #1f1f24);
    border-bottom: 1px solid rgba(var(--theme-foreground-rgb), 0.08);
}

.dialog-title {
    font-size: 13.5px;
    font-weight: 600;
    color: var(--theme-text-primary, #f4f4f5);
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
    color: var(--theme-text-primary, #f4f4f5);
}

.dialog-file-meta small {
    color: var(--theme-text-muted, #71717a);
    font-size: 11.5px;
}

.dialog-actions {
    display: flex;
    align-items: center;
    gap: 10px;
}

.btn-copy-doc {
    background: rgba(var(--theme-foreground-rgb), 0.08);
    border: 1px solid rgba(var(--theme-foreground-rgb), 0.15);
    color: var(--theme-text-primary, #e4e4e7);
    font-size: 12px;
    padding: 5px 12px;
    border-radius: 6px;
    cursor: pointer;
    transition: all 0.15s ease;
}

.btn-copy-doc:hover {
    background: rgba(var(--theme-foreground-rgb), 0.16);
    color: var(--theme-text-primary, #fff);
}

.btn-close-dialog {
    background: transparent;
    border: 0;
    color: var(--theme-text-secondary, #a1a1aa);
    font-size: 20px;
    cursor: pointer;
    padding: 0 4px;
    line-height: 1;
}

.btn-close-dialog:hover {
    color: var(--theme-text-primary, #ffffff);
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
    box-shadow: var(--theme-shadow, 0 4px 20px rgba(0, 0, 0, 0.6));
    opacity: 0;
}

.full-preview-img.is-ready { opacity: 1; }
.image-dialog-body { position: relative; min-height: 120px; min-width: min(280px, 70vw); }
.image-preview-status { position: absolute; inset: 0; display: grid; place-items: center; padding: 20px; color: var(--text-2, #a1a1aa); font-size: 13px; }

.doc-dialog-body {
    flex: 1;
    overflow-y: auto;
    background: var(--paper-bg);
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
    background: var(--paper-bg) !important;
}

.doc-dialog-body :deep(.card-body) {
    flex: 1;
    height: 100% !important;
    min-height: 100% !important;
    background: var(--paper-bg) !important;
}

.doc-dialog-body :deep(.preview-container),
.doc-dialog-body :deep(.sandbox-iframe) {
    width: 100%;
    height: 100% !important;
    min-height: 100% !important;
    border: none !important;
    border-radius: 0 !important;
    background: var(--paper-bg) !important;
    flex: 1;
}

.doc-markdown-content {
    flex: 1;
    color: var(--paper-text);
    background: var(--paper-bg);
    font-size: 13.5px;
    line-height: 1.7;
    padding: 28px 36px;
}

.doc-markdown-content :deep(h1),
.doc-markdown-content :deep(h2),
.doc-markdown-content :deep(h3),
.doc-markdown-content :deep(strong) {
    color: var(--paper-heading);
}

.doc-markdown-content :deep(p),
.doc-markdown-content :deep(li) {
    color: var(--paper-body);
}

.doc-raw-preview {
    margin: 0;
    flex: 1;
    white-space: pre-wrap;
    word-break: break-word;
    font-family: monospace;
    font-size: 12px;
    color: var(--theme-text-primary, #d4d4d8);
    background: var(--theme-bg-deep, #0d0d10);
    padding: 20px;
    border-radius: 0;
    overflow: auto;
}

.doc-empty-hint {
    color: var(--theme-text-muted, #71717a);
    font-size: 13px;
    text-align: center;
    padding: 40px 0;
}

.doc-edit-textarea {
    flex: 1;
    width: 100%;
    height: 100%;
    min-height: 420px;
    background: var(--theme-bg-deep, #0d0d10);
    color: var(--theme-text-primary, #e4e4e7);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
    font-size: 13px;
    line-height: 1.65;
    padding: 24px 28px;
    border: none;
    outline: none;
    resize: none;
    box-sizing: border-box;
    white-space: pre-wrap;
    word-break: break-word;
}

.btn-action {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 5px 12px;
    font-size: 12px;
    font-weight: 500;
    border-radius: 6px;
    border: 1px solid rgba(var(--theme-foreground-rgb), 0.12);
    background: rgba(var(--theme-foreground-rgb), 0.06);
    color: var(--theme-text-primary, #d4d4d8);
    cursor: pointer;
    transition: all 0.15s;
}

.btn-action:hover {
    background: rgba(var(--theme-foreground-rgb), 0.12);
    color: var(--theme-text-primary, #ffffff);
}

.btn-action.btn-unpack {
    background: rgba(59, 130, 246, 0.15);
    border-color: rgba(59, 130, 246, 0.35);
    color: var(--theme-status-info, #93c5fd);
}

.btn-action.btn-unpack:hover {
    background: rgba(59, 130, 246, 0.28);
    color: var(--theme-status-info, #bfdbfe);
    transform: translateY(-1px);
}

.btn-action.btn-save {
    background: rgba(34, 197, 94, 0.18);
    border-color: rgba(34, 197, 94, 0.4);
    color: var(--theme-status-success, #86efac);
}

.btn-action.btn-save:hover {
    background: rgba(34, 197, 94, 0.3);
    color: var(--theme-status-success, #bbf7d0);
}

.btn-action.btn-edit-toggle.active {
    background: rgba(var(--theme-foreground-rgb), 0.18);
    color: var(--theme-text-primary, #ffffff);
    border-color: rgba(var(--theme-foreground-rgb), 0.3);
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
