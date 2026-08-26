<template>
  <Teleport to="body">
    <div v-if="open && artifact" class="preview-backdrop" @click="handleBackdropClick">
      <div class="preview-dialog" @click.stop>
        <!-- 弹窗顶部栏 -->
        <header class="dialog-header">
          <div class="dialog-header-left">
            <span class="dialog-icon">{{ icon }}</span>
            <div class="dialog-titles">
              <strong class="dialog-name" :title="artifact.name">{{ artifact.name }}</strong>
              <div class="dialog-tags">
                <span class="dialog-tag">{{ kindLabel }}</span>
                <span v-if="formattedSize" class="dialog-tag">{{ formattedSize }}</span>
                <span v-if="artifact.localPath" class="dialog-path" :title="artifact.localPath">
                  {{ artifact.localPath }}
                </span>
              </div>
            </div>
          </div>

          <div class="dialog-header-actions">
            <!-- 外部独立 Webview 打开 (如果是网页或 URL) -->
            <button
              v-if="artifact.kind === 'web' || artifact.previewUrl"
              type="button"
              class="action-btn"
              title="在独立窗口中打开"
              @click="openInExternalWindow"
            >
              <svg viewBox="0 0 24 24"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></svg>
              <span>独立窗口</span>
            </button>

            <!-- 下载文件 -->
            <button
              v-if="canDownload"
              type="button"
              class="action-btn btn-primary"
              title="下载保存此文件"
              @click="handleDownload"
            >
              <svg viewBox="0 0 24 24"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3"/></svg>
              <span>下载</span>
            </button>

            <!-- 关闭弹窗 -->
            <button
              type="button"
              class="action-btn close-btn"
              title="关闭 (Esc)"
              @click="$emit('close')"
            >
              <svg viewBox="0 0 24 24"><path d="M18 6 6 18M6 6l12 12"/></svg>
            </button>
          </div>
        </header>

        <!-- 弹窗内容主体区 -->
        <main class="dialog-body">
          <!-- 1. 网页 / HTML 产物：嵌入 HtmlPreviewBlock 沙箱 -->
          <template v-if="isHtmlPreview">
            <HtmlPreviewBlock
              :code="previewHtmlContent"
              class="dialog-html-preview"
            />
          </template>

          <!-- 2. 图片产物：高清图展示 -->
          <template v-else-if="artifact.kind === 'image' && (artifact.previewUrl || artifact.content)">
            <div class="image-preview-container">
              <img :src="artifact.previewUrl || artifact.content" :alt="artifact.name" class="preview-image" />
            </div>
          </template>

          <!-- 3. Markdown / 富文本 / 图表产物 -->
          <template v-else-if="artifact.content">
            <div class="markdown-preview-container">
              <RichContentRenderer
                :content="artifact.content"
                :compact-html="false"
              />
            </div>
          </template>

          <!-- 4. 本地文件或无直接文本的产物：展示文件卡片与操作引导 -->
          <template v-else>
            <div class="empty-preview-state">
              <div class="state-icon">{{ icon }}</div>
              <h3>{{ artifact.name }}</h3>
              <p class="state-kind">{{ kindLabel }}</p>
              <p v-if="artifact.localPath" class="state-path">
                文件已生成于本地系统路径：<br />
                <code>{{ artifact.localPath }}</code>
              </p>
              <div class="state-actions">
                <button
                  v-if="artifact.localPath"
                  type="button"
                  class="action-btn"
                  @click="copyPath"
                >
                  <svg viewBox="0 0 24 24"><rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg>
                  <span>{{ copied ? "已复制路径" : "复制完整路径" }}</span>
                </button>
              </div>
            </div>
          </template>
        </main>
      </div>
    </div>
  </Teleport>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import type { ArtifactDescriptor } from "../../types/artifact";
import { formatFileSize, getArtifactIcon, getArtifactKindLabel } from "../../utils/artifact";
import HtmlPreviewBlock from "../charts/HtmlPreviewBlock.vue";
import RichContentRenderer from "../charts/RichContentRenderer.vue";
import { openWebviewWindow } from "../../composables/useBrowserPreview";

const props = defineProps<{
  open: boolean;
  artifact: ArtifactDescriptor | null;
}>();

const emit = defineEmits<{
  close: [];
  download: [artifact: ArtifactDescriptor];
}>();

const icon = computed(() => (props.artifact ? getArtifactIcon(props.artifact.kind, props.artifact.format) : "📦"));
const kindLabel = computed(() => (props.artifact ? getArtifactKindLabel(props.artifact.kind, props.artifact.format) : ""));
const formattedSize = computed(() => (props.artifact ? formatFileSize(props.artifact.sizeBytes) : ""));
const canDownload = computed(() => Boolean(props.artifact?.downloadUrl || props.artifact?.content));

const isHtmlPreview = computed(() => {
  if (!props.artifact) return false;
  return (
    props.artifact.format === "html" ||
    props.artifact.format === "htm" ||
    (props.artifact.content && props.artifact.content.includes("<!DOCTYPE html>")) ||
    (props.artifact.content && props.artifact.content.includes("```html"))
  );
});

const previewHtmlContent = computed(() => {
  if (!props.artifact) return "";
  const content = props.artifact.content || "";
  const match = content.match(/```html[\s\r\n]+([\s\S]*?)```/i);
  if (match) return match[1].trim();
  return content;
});

const copied = ref(false);

async function copyPath() {
  if (!props.artifact?.localPath) return;
  try {
    await navigator.clipboard.writeText(props.artifact.localPath);
    copied.value = true;
    setTimeout(() => {
      copied.value = false;
    }, 2000);
  } catch (err) {
    console.warn("无法复制路径", err);
  }
}

function handleDownload() {
  if (!props.artifact) return;
  if (props.artifact.downloadUrl) {
    window.open(props.artifact.downloadUrl, "_blank");
    return;
  }
  if (props.artifact.content) {
    const mime = props.artifact.mimeType || "text/plain;charset=utf-8";
    const blob = new Blob([props.artifact.content], { type: mime });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = props.artifact.name;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
    emit("download", props.artifact);
  }
}

function openInExternalWindow() {
  if (!props.artifact) return;
  if (props.artifact.previewUrl) {
    void openWebviewWindow(props.artifact.previewUrl, props.artifact.name);
    return;
  }
  if (props.artifact.content) {
    // 创建本地 Blob URL 并在新窗口打开
    const blob = new Blob([props.artifact.content], { type: "text/html;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    void openWebviewWindow(url, props.artifact.name);
  }
}

function handleBackdropClick() {
  emit("close");
}

function handleKeyDown(e: KeyboardEvent) {
  if (e.key === "Escape" && props.open) {
    emit("close");
  }
}

watch(
  () => props.open,
  (isOpen) => {
    if (isOpen) {
      window.addEventListener("keydown", handleKeyDown);
    } else {
      window.removeEventListener("keydown", handleKeyDown);
    }
  }
);

onBeforeUnmount(() => {
  window.removeEventListener("keydown", handleKeyDown);
});
</script>

<style scoped>
.preview-backdrop {
  position: fixed;
  inset: 0;
  z-index: 1200;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(4, 6, 12, 0.68);
  backdrop-filter: blur(8px);
  padding: 24px;
  animation: fadeIn 0.15s ease-out;
}

.preview-dialog {
  display: flex;
  flex-direction: column;
  width: min(1000px, 94vw);
  height: min(780px, 90vh);
  background: #141418;
  border: 1px solid rgba(255, 255, 255, 0.15);
  border-radius: 14px;
  box-shadow: 0 24px 60px rgba(0, 0, 0, 0.7), 0 0 0 1px rgba(255, 255, 255, 0.08);
  overflow: hidden;
}

.dialog-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px 18px;
  background: #1a1a20;
  border-bottom: 1px solid rgba(255, 255, 255, 0.09);
  flex-shrink: 0;
}

.dialog-header-left {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
}

.dialog-icon {
  font-size: 24px;
  line-height: 1;
}

.dialog-titles {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.dialog-name {
  font-size: 14px;
  font-weight: 600;
  color: #ffffff;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.dialog-tags {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 11px;
}

.dialog-tag {
  background: rgba(99, 102, 241, 0.15);
  color: #a5b4fc;
  border: 1px solid rgba(99, 102, 241, 0.3);
  padding: 1px 6px;
  border-radius: 4px;
  font-weight: 500;
}

.dialog-path {
  color: #71717a;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 380px;
}

.dialog-header-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
}

.action-btn {
  display: inline-flex !important;
  align-items: center !important;
  gap: 5px !important;
  height: 28px !important;
  padding: 0 10px !important;
  border-radius: 6px !important;
  font-size: 12px !important;
  font-weight: 500 !important;
  cursor: pointer;
  border: 1px solid rgba(255, 255, 255, 0.1) !important;
  background: rgba(255, 255, 255, 0.05) !important;
  color: #e4e4e7 !important;
  transition: all 0.15s ease;
}

.action-btn svg {
  width: 13px;
  height: 13px;
  stroke: currentColor;
  stroke-width: 2;
  fill: none;
}

.action-btn:hover {
  background: rgba(255, 255, 255, 0.12) !important;
  border-color: rgba(255, 255, 255, 0.25) !important;
  color: #ffffff !important;
}

.action-btn.btn-primary {
  background: rgba(99, 102, 241, 0.25) !important;
  border-color: rgba(99, 102, 241, 0.5) !important;
  color: #ffffff !important;
}

.action-btn.btn-primary:hover {
  background: rgba(99, 102, 241, 0.4) !important;
}

.close-btn {
  padding: 0 7px !important;
}

.dialog-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  background: #101014;
  position: relative;
  display: flex;
  flex-direction: column;
}

.dialog-html-preview {
  width: 100%;
  height: 100%;
  border: none !important;
}

.markdown-preview-container {
  padding: 24px;
  max-width: 860px;
  margin: 0 auto;
  width: 100%;
}

.image-preview-container {
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
  width: 100%;
  height: 100%;
  overflow: auto;
}

.preview-image {
  max-width: 100%;
  max-height: 100%;
  border-radius: 8px;
  box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
}

.empty-preview-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  text-align: center;
  height: 100%;
  padding: 40px;
}

.state-icon {
  font-size: 48px;
  margin-bottom: 12px;
}

.empty-preview-state h3 {
  margin: 0 0 6px;
  color: #f4f4f5;
  font-size: 18px;
}

.state-kind {
  color: #a5b4fc;
  font-size: 13px;
  margin: 0 0 16px;
}

.state-path {
  font-size: 12px;
  color: #71717a;
  max-width: 480px;
  margin-bottom: 20px;
}

.state-path code {
  display: inline-block;
  margin-top: 6px;
  padding: 4px 8px;
  background: rgba(0, 0, 0, 0.4);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 6px;
  color: #34d399;
  word-break: break-all;
}

@keyframes fadeIn {
  from {
    opacity: 0;
  }
  to {
    opacity: 1;
  }
}
</style>
