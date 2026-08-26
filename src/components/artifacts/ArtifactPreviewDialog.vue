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
              v-if="canOpenInExternalWindow"
              type="button"
              class="action-btn"
              title="在独立窗口中打开"
              @click="openInExternalWindow"
            >
              <svg viewBox="0 0 24 24"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></svg>
              <span>独立窗口</span>
            </button>

            <!-- 本地文件原生打开按钮 (唤起系统默认关联程序如 PDF 查看器/Excel/代码编辑器) -->
            <button
              v-if="hasLocalPath"
              type="button"
              class="action-btn"
              title="在系统默认程序中打开此文件"
              @click="handleOpenInSystem"
            >
              <svg viewBox="0 0 24 24"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></svg>
              <span>系统打开</span>
            </button>

            <!-- 在系统文件管理器中定位 -->
            <button
              v-if="hasLocalPath"
              type="button"
              class="action-btn"
              title="在系统文件管理器中定位该文件"
              @click="handleRevealInFolder"
            >
              <svg viewBox="0 0 24 24"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
              <span>定位文件</span>
            </button>

            <!-- 复制文件与路径 -->
            <button
              v-if="hasLocalPath"
              type="button"
              class="action-btn"
              :title="copied ? '已成功复制文件与路径到剪贴板！' : `复制文件 (支持在微信/桌面/文件夹直接 Ctrl+V 粘贴文件)`"
              @click="copyFile"
            >
              <svg v-if="!copied" viewBox="0 0 24 24"><rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg>
              <svg v-else viewBox="0 0 24 24" class="text-green"><path d="M20 6 9 17l-5-5"/></svg>
              <span>{{ copied ? "已复制" : "复制文件" }}</span>
            </button>

            <!-- 导出为 Word -->
            <button
              v-if="isMarkdownFormat || isHtmlPreview"
              type="button"
              class="action-btn"
              :title="wordExported ? '已成功导出 Word' : '将当前文档导出为 Word (.doc)'"
              @click="exportToWord"
            >
              <svg v-if="!wordExported" viewBox="0 0 24 24"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="16" y1="13" x2="8" y2="13"/><line x1="16" y1="17" x2="8" y2="17"/><polyline points="10 9 9 9 8 9"/></svg>
              <svg v-else viewBox="0 0 24 24" class="text-green"><path d="M20 6 9 17l-5-5"/></svg>
              <span>{{ wordExported ? "已导出Word" : "导出Word" }}</span>
            </button>

            <!-- 导出为 PDF / 打印 -->
            <button
              v-if="isMarkdownFormat || isHtmlPreview"
              type="button"
              class="action-btn"
              title="将当前文档打印或另存为 PDF"
              @click="exportToPdf"
            >
              <svg viewBox="0 0 24 24"><polyline points="6 9 6 2 18 2 18 9"/><path d="M6 18H4a2 2 0 0 1-2-2v-5a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2v5a2 2 0 0 1-2 2h-2"/><rect width="12" height="8" x="6" y="14"/></svg>
              <span>导出PDF</span>
            </button>

            <!-- 另存为 / 导出源文件 -->
            <button
              v-if="canDownload"
              type="button"
              class="action-btn btn-primary"
              :title="downloaded ? '已成功另存到电脑' : '另存为 / 导出此文件到指定文件夹'"
              @click="handleDownload"
            >
              <svg v-if="!downloaded" viewBox="0 0 24 24"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3"/></svg>
              <svg v-else viewBox="0 0 24 24" class="text-green"><path d="M20 6 9 17l-5-5"/></svg>
              <span>{{ downloaded ? "已保存" : "另存为" }}</span>
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
          <template v-else-if="artifact.kind === 'image' && (isSafeArtifactUrl(artifact.previewUrl) || displayContent)">
            <div class="image-preview-container">
              <img :src="isSafeArtifactUrl(artifact.previewUrl) ? artifact.previewUrl : displayContent" :alt="artifact.name" class="preview-image" />
            </div>
          </template>

          <!-- 3. Markdown / 代码 / 富文本 / 图表产物 -->
          <template v-else-if="displayContent">
            <div class="markdown-preview-container">
              <div
                v-if="renderedMarkdown"
                class="artifact-markdown-rendered custom-scroll"
                v-html="renderedMarkdown"
              />
              <pre v-else class="raw-code-block custom-scroll"><code>{{ displayContent }}</code></pre>
            </div>
          </template>

          <!-- 4. 空状态 / 加载中 -->
          <div v-else class="empty-preview">
            <span class="empty-icon">⏳</span>
            <p>正在读取交付物内容...</p>
          </div>
        </main>
      </div>
    </div>
  </Teleport>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { save } from "@tauri-apps/plugin-dialog";
import DOMPurify from "dompurify";
import { marked } from "marked";
import { openWebviewWindow } from "../../composables/useBrowserPreview";
import type { ArtifactDescriptor } from "../../types/artifact";
import {
  formatFileSize,
  getArtifactIcon,
  getArtifactKindLabel,
  isSafeArtifactUrl,
  openArtifactInSystem,
  readArtifactFile,
  resolveArtifactAbsolutePath,
  showArtifactInFolder,
} from "../../utils/artifact";
import HtmlPreviewBlock from "../charts/HtmlPreviewBlock.vue";

const props = defineProps<{
  open: boolean;
  artifact: ArtifactDescriptor | null;
}>();

const emit = defineEmits<{
  close: [];
  download: [artifact: ArtifactDescriptor];
}>();

const localContent = ref<string>("");

watch(
  () => [props.open, props.artifact] as const,
  async ([isOpen, current]) => {
    if (!isOpen || !current) {
      localContent.value = "";
      return;
    }
    if (current.content) {
      localContent.value = current.content;
      return;
    }
    if (current.localPath) {
      const text = await readArtifactFile(current.localPath);
      if (text) {
        current.content = text;
        localContent.value = text;
      }
    }
  },
  { immediate: true }
);

const displayContent = computed(() => localContent.value || props.artifact?.content || "");

const icon = computed(() => (props.artifact ? getArtifactIcon(props.artifact.kind, props.artifact.format) : "📦"));
const kindLabel = computed(() => (props.artifact ? getArtifactKindLabel(props.artifact.kind, props.artifact.format) : ""));
const formattedSize = computed(() => (props.artifact ? formatFileSize(props.artifact.sizeBytes) : ""));
const canDownload = computed(() => Boolean(isSafeArtifactUrl(props.artifact?.downloadUrl) || displayContent.value || props.artifact?.localPath || props.artifact?.artifactId));
const hasLocalPath = computed(() => Boolean(props.artifact?.localPath && typeof props.artifact.localPath === "string" && props.artifact.localPath.trim().length > 0 && props.artifact.localPath.trim() !== "[object Object]"));
const canOpenInExternalWindow = computed(() => {
  if (!props.artifact) return false;
  return isSafeArtifactUrl(props.artifact.previewUrl) || (props.artifact.kind === "web" && Boolean(displayContent.value));
});

const isHtmlPreview = computed(() => {
  if (!props.artifact) return false;
  const content = displayContent.value;
  return (
    props.artifact.format === "html" ||
    props.artifact.format === "htm" ||
    content.includes("<!DOCTYPE html>") ||
    content.includes("```html")
  );
});

const isMarkdownFormat = computed(() => {
  const fmt = props.artifact?.format?.toLowerCase();
  const name = props.artifact?.name?.toLowerCase() || "";
  return fmt === "md" || fmt === "markdown" || name.endsWith(".md");
});

const renderedMarkdown = computed(() => {
  if (!isMarkdownFormat.value) return null;
  return DOMPurify.sanitize(marked.parse(displayContent.value) as string);
});

const previewHtmlContent = computed(() => {
  const content = displayContent.value;
  if (!content) return "";
  const match = content.match(/```html[\s\r\n]+([\s\S]*?)```/i);
  if (match) return match[1].trim();
  return content;
});

const copied = ref(false);
const downloaded = ref(false);
const wordExported = ref(false);

async function exportToWord() {
  if (!props.artifact) return;
  const title = props.artifact.name.replace(/\.[^/.]+$/, "");
  const contentHtml = renderedMarkdown.value || previewHtmlContent.value || `<pre>${displayContent.value}</pre>`;

  const wordDocContent = `<!DOCTYPE html>
<html xmlns:o='urn:schemas-microsoft-com:office:office' xmlns:w='urn:schemas-microsoft-com:office:word' xmlns='http://www.w3.org/TR/REC-html40'>
<head>
<meta charset='utf-8'>
<title>${title}</title>
<!--[if gte mso 9]>
<xml>
<w:WordDocument>
<w:View>Print</w:View>
<w:Zoom>100</w:Zoom>
<w:DoNotOptimizeForBrowser/>
</w:WordDocument>
</xml>
<![endif]-->
<style>
  body {
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "PingFang SC", "Microsoft YaHei", sans-serif;
    font-size: 11pt;
    line-height: 1.6;
    color: #1e293b;
    padding: 20px;
  }
  h1 { font-size: 20pt; font-weight: bold; color: #0f172a; border-bottom: 2px solid #6366f1; padding-bottom: 6px; margin-bottom: 16px; }
  h2 { font-size: 15pt; font-weight: bold; color: #1e293b; margin-top: 20px; margin-bottom: 10px; border-bottom: 1px solid #e2e8f0; padding-bottom: 4px; }
  h3 { font-size: 13pt; font-weight: 600; color: #334155; margin-top: 14px; margin-bottom: 8px; }
  p { margin: 8px 0; }
  ul, ol { padding-left: 24px; margin: 8px 0; }
  li { margin: 4px 0; }
  code { font-family: Consolas, Monaco, "Courier New", monospace; font-size: 10pt; background: #f1f5f9; padding: 2px 5px; border-radius: 4px; color: #475569; }
  pre { background: #0f172a; color: #f8fafc; padding: 14px; border-radius: 6px; font-family: Consolas, monospace; font-size: 9.5pt; line-height: 1.4; overflow-x: auto; }
  pre code { background: transparent; color: inherit; padding: 0; }
  table { border-collapse: collapse; width: 100%; margin: 16px 0; font-size: 10pt; }
  th, td { border: 1px solid #cbd5e1; padding: 8px 12px; text-align: left; }
  th { background-color: #f8fafc; font-weight: 600; color: #334155; }
  tr:nth-child(even) { background-color: #f8fafc; }
  blockquote { border-left: 4px solid #6366f1; margin: 12px 0; padding: 8px 16px; background: #f8fafc; color: #64748b; }
  hr { border: none; border-top: 1px solid #e2e8f0; margin: 20px 0; }
</style>
</head>
<body>
  <h1>${title}</h1>
  ${contentHtml}
</body>
</html>`;

  try {
    const targetPath = await save({
      defaultPath: `${title}.doc`,
      filters: [{ name: "Word Document (*.doc)", extensions: ["doc"] }],
    });
    if (!targetPath) return;

    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("export_artifact_to_file", {
      artifactId: null,
      sourcePath: null,
      content: wordDocContent,
      targetPath,
    });

    wordExported.value = true;
    setTimeout(() => {
      wordExported.value = false;
    }, 2500);
  } catch (err) {
    console.warn("导出 Word 失败:", err);
  }
}

function exportToPdf() {
  if (!props.artifact) return;
  const title = props.artifact.name.replace(/\.[^/.]+$/, "");
  const contentHtml = renderedMarkdown.value || previewHtmlContent.value || `<pre>${displayContent.value}</pre>`;

  const printWindow = window.open("", "_blank");
  if (!printWindow) {
    window.print();
    return;
  }

  printWindow.document.write(`<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>${title}</title>
<style>
  @page { margin: 20mm 15mm; size: A4; }
  body {
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "PingFang SC", "Microsoft YaHei", sans-serif;
    font-size: 11pt;
    line-height: 1.6;
    color: #111827;
    margin: 0;
    padding: 0;
  }
  h1 { font-size: 22pt; font-weight: bold; color: #111827; border-bottom: 2px solid #4f46e5; padding-bottom: 8px; margin-bottom: 20px; }
  h2 { font-size: 16pt; font-weight: bold; color: #1f2937; margin-top: 24px; margin-bottom: 12px; border-bottom: 1px solid #e5e7eb; padding-bottom: 4px; }
  h3 { font-size: 13pt; font-weight: 600; color: #374151; margin-top: 16px; margin-bottom: 8px; }
  p { margin: 8px 0; }
  ul, ol { padding-left: 24px; margin: 8px 0; }
  code { font-family: Consolas, monospace; font-size: 9.5pt; background: #f3f4f6; padding: 2px 5px; border-radius: 4px; }
  pre { background: #f8fafc; border: 1px solid #e2e8f0; color: #0f172a; padding: 12px; border-radius: 6px; font-family: Consolas, monospace; font-size: 9pt; page-break-inside: avoid; }
  pre code { background: transparent; padding: 0; }
  table { border-collapse: collapse; width: 100%; margin: 16px 0; font-size: 9.5pt; page-break-inside: avoid; }
  th, td { border: 1px solid #d1d5db; padding: 8px 12px; text-align: left; }
  th { background-color: #f9fafb; font-weight: 600; }
  blockquote { border-left: 4px solid #4f46e5; margin: 12px 0; padding: 6px 14px; background: #f9fafb; color: #4b5563; }
</style>
</head>
<body>
  <h1>${title}</h1>
  ${contentHtml}
</body>
</html>`);
  printWindow.document.close();
  printWindow.focus();
  setTimeout(() => {
    printWindow.print();
    printWindow.close();
  }, 250);
}

async function copyFile() {
  const rawPath = props.artifact?.localPath;
  if (!rawPath) return;
  try {
    const absPath = await resolveArtifactAbsolutePath(rawPath);
    await navigator.clipboard.writeText(absPath || rawPath);

    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("copy_file_to_clipboard", { path: absPath || rawPath });

    copied.value = true;
    setTimeout(() => {
      copied.value = false;
    }, 2000);
  } catch (err) {
    console.warn("无法复制文件剪贴板", err);
    try {
      await navigator.clipboard.writeText(rawPath);
      copied.value = true;
      setTimeout(() => {
        copied.value = false;
      }, 2000);
    } catch {
      // ignore
    }
  }
}

async function handleOpenInSystem() {
  if (!props.artifact?.localPath) return;
  await openArtifactInSystem(props.artifact.localPath);
}

async function handleRevealInFolder() {
  if (!props.artifact?.localPath) return;
  await showArtifactInFolder(props.artifact.localPath);
}

async function handleDownload() {
  if (!props.artifact) return;
  if (isSafeArtifactUrl(props.artifact.downloadUrl)) {
    window.open(props.artifact.downloadUrl, "_blank", "noopener,noreferrer");
    return;
  }

  try {
    const targetPath = await save({
      defaultPath: props.artifact.name,
      filters: props.artifact.format
        ? [{ name: props.artifact.format.toUpperCase(), extensions: [props.artifact.format] }]
        : undefined,
    });

    if (!targetPath) return;

    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("export_artifact_to_file", {
      artifactId: props.artifact.artifactId,
      sourcePath: props.artifact.localPath,
      content: displayContent.value || props.artifact.content,
      targetPath,
    });

    downloaded.value = true;
    setTimeout(() => {
      downloaded.value = false;
    }, 2500);
    emit("download", props.artifact);
  } catch (err) {
    console.warn("系统另存为失败，降级使用 Blob 下载:", err);
    const content = displayContent.value || props.artifact.content;
    if (content) {
      const mime = props.artifact.mimeType || "text/plain;charset=utf-8";
      const blob = new Blob([content], { type: mime });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = props.artifact.name;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
      downloaded.value = true;
      setTimeout(() => {
        downloaded.value = false;
      }, 2500);
      emit("download", props.artifact);
    }
  }
}

function openInExternalWindow() {
  if (!props.artifact) return;
  const previewUrl = props.artifact.previewUrl;
  if (previewUrl && isSafeArtifactUrl(previewUrl)) {
    void openWebviewWindow(previewUrl, props.artifact.name);
    return;
  }
  if (props.artifact.kind === "web" && props.artifact.content) {
    // HTML 内容创建本地 Blob URL 并在新窗口打开
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
