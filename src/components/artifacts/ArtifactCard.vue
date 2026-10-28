<template>
  <div class="artifact-card" :class="[`kind-${artifact.kind}`]">
    <!-- 左侧大图标区 -->
    <div class="artifact-icon-wrap" :title="kindLabel">
      <span class="artifact-icon">{{ icon }}</span>
      <span class="artifact-format-badge">{{ artifact.format.toUpperCase() }}</span>
    </div>

    <!-- 中间主信息区 -->
    <div class="artifact-main-info">
      <div class="artifact-title-row">
        <strong class="artifact-name" :title="artifact.name">
          {{ artifact.name }}
        </strong>
      </div>

      <div class="artifact-meta-row">
        <span class="meta-tag kind-tag">{{ kindLabel }}</span>
        <span v-if="formattedSize" class="meta-tag size-tag">{{ formattedSize }}</span>
        <span v-if="artifact.localPath" class="meta-tag path-tag" :title="artifact.localPath">
          本地产物
        </span>
        <span v-if="artifact.description" class="meta-desc" :title="artifact.description">
          {{ artifact.description }}
        </span>
      </div>
    </div>

    <!-- 右侧操作动作区 -->
    <div class="artifact-actions">
      <!-- 预览按钮 (支持预览或包含内容时高亮可用) -->
      <button
        v-if="canPreview"
        type="button"
        class="artifact-action-btn btn-preview"
        title="在工作台或弹窗中预览此交付物"
        @click.stop="handlePreview"
      >
        <svg viewBox="0 0 24 24"><path d="M2 12s3-7 10-7 10 7 10 7-3 7-10 7-10-7-10-7Z"/><circle cx="12" cy="12" r="3"/></svg>
        <span>预览</span>
      </button>

      <!-- 下载 / 另存为按钮 -->
      <button
        v-if="canDownload"
        type="button"
        class="artifact-action-btn btn-download"
        :title="downloaded ? '已成功另存到电脑' : '另存为 / 导出此文件到指定文件夹'"
        @click.stop="handleDownload"
      >
        <svg v-if="!downloaded" viewBox="0 0 24 24"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3"/></svg>
        <svg v-else viewBox="0 0 24 24" class="text-green"><path d="M20 6 9 17l-5-5"/></svg>
        <span>{{ downloaded ? "已保存" : "另存为" }}</span>
      </button>

      <!-- 本地文件原生打开按钮 (唤起系统默认关联程序如 PDF 查看器/Excel/代码编辑器) -->
      <button
        v-if="hasLocalPath"
        type="button"
        class="artifact-action-btn btn-open"
        title="在系统默认应用中打开此文件 (如 PDF阅读器 / Excel / 代码编辑器)"
        @click.stop="handleOpenInSystem"
      >
        <svg viewBox="0 0 24 24"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></svg>
        <span>打开</span>
      </button>

      <!-- 在文件管理器中高亮定位 -->
      <button
        v-if="hasLocalPath"
        type="button"
        class="artifact-action-btn btn-reveal"
        title="在系统文件管理器中定位该文件"
        @click.stop="handleRevealInFolder"
      >
        <svg viewBox="0 0 24 24"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
        <span>定位</span>
      </button>

      <!-- 复制文件与路径按钮 -->
      <button
        v-if="hasLocalPath"
        type="button"
        class="artifact-action-btn btn-copy"
        :title="copied ? '已成功复制文件与路径到剪贴板！' : `复制文件 (支持在微信/桌面/文件夹直接 Ctrl+V 粘贴文件)`"
        @click.stop="copyFile"
      >
        <svg v-if="!copied" viewBox="0 0 24 24"><rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg>
        <svg v-else viewBox="0 0 24 24" class="text-green"><path d="M20 6 9 17l-5-5"/></svg>
        <span>{{ copied ? "已复制" : "复制文件" }}</span>
      </button>

      <!-- 删除受管产物按钮 -->
      <button
        v-if="artifact.artifactId"
        type="button"
        class="artifact-action-btn btn-delete"
        title="删除此受管交付物"
        @click.stop="handleDelete"
      >
        <svg viewBox="0 0 24 24"><path d="M3 6h18M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></svg>
        <span>删除</span>
      </button>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from "vue";
import { save } from "@tauri-apps/plugin-dialog";
import type { ArtifactDescriptor } from "../../types/artifact";
import {
  deleteArtifactById,
  formatFileSize,
  getArtifactIcon,
  getArtifactKindLabel,
  isArtifactPreviewable,
  isSafeArtifactUrl,
  openArtifactById,
  openArtifactInSystem,
  readArtifactById,
  readArtifactFile,
  resolveArtifactAbsolutePath,
  revealArtifactById,
  showArtifactInFolder,
} from "../../utils/artifact";

const props = defineProps<{
  artifact: ArtifactDescriptor;
}>();

const emit = defineEmits<{
  preview: [artifact: ArtifactDescriptor];
  download: [artifact: ArtifactDescriptor];
  deleted: [artifactId: string];
}>();

const icon = computed(() => getArtifactIcon(props.artifact.kind, props.artifact.format));
const kindLabel = computed(() => getArtifactKindLabel(props.artifact.kind, props.artifact.format));
const formattedSize = computed(() => formatFileSize(props.artifact.sizeBytes));
const canPreview = computed(() => isArtifactPreviewable(props.artifact));
const canDownload = computed(() => Boolean(
  isSafeArtifactUrl(props.artifact.downloadUrl) ||
  props.artifact.content ||
  props.artifact.artifactId ||
  props.artifact.localPath
));
const hasLocalPath = computed(() => Boolean(
  (props.artifact.localPath && typeof props.artifact.localPath === "string" && props.artifact.localPath.trim().length > 0 && props.artifact.localPath.trim() !== "[object Object]") ||
  props.artifact.artifactId
));

const copied = ref(false);
const downloaded = ref(false);

async function copyFile() {
  const rawPath = props.artifact.localPath;
  try {
    const absPath = rawPath ? await resolveArtifactAbsolutePath(rawPath) : (props.artifact.artifactId ? await resolveArtifactAbsolutePath(props.artifact.artifactId) : "");
    if (!absPath && !rawPath) return;

    // 1. 写入绝对路径到常规文本剪贴板
    await navigator.clipboard.writeText(absPath || rawPath || "");

    // 2. 调用 Rust 将文件对象自身注入系统文件剪贴板（支持在文件管理器/微信/桌面直接 Ctrl+V 粘贴物理文件）
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("copy_file_to_clipboard", { path: absPath || rawPath });

    copied.value = true;
    setTimeout(() => {
      copied.value = false;
    }, 2000);
  } catch (err) {
    console.warn("注入文件剪贴板失败，降级为常规文本复制", err);
    try {
      if (rawPath) {
        await navigator.clipboard.writeText(rawPath);
        copied.value = true;
        setTimeout(() => {
          copied.value = false;
        }, 2000);
      }
    } catch {
      // ignore
    }
  }
}

async function handlePreview() {
  if (!props.artifact.content) {
    if (props.artifact.artifactId) {
      const text = await readArtifactById(props.artifact.artifactId);
      if (text) props.artifact.content = text;
    } else if (props.artifact.localPath) {
      const text = await readArtifactFile(props.artifact.localPath);
      if (text) props.artifact.content = text;
    }
  }
  emit("preview", props.artifact);
}

async function handleOpenInSystem() {
  if (props.artifact.artifactId) {
    await openArtifactById(props.artifact.artifactId);
    return;
  }
  if (props.artifact.localPath) {
    await openArtifactInSystem(props.artifact.localPath);
  }
}

async function handleRevealInFolder() {
  if (props.artifact.artifactId) {
    await revealArtifactById(props.artifact.artifactId);
    return;
  }
  if (props.artifact.localPath) {
    await showArtifactInFolder(props.artifact.localPath);
  }
}

async function handleDelete() {
  if (!props.artifact.artifactId) return;
  const ok = await deleteArtifactById(props.artifact.artifactId);
  if (ok) {
    emit("deleted", props.artifact.artifactId);
  }
}

async function handleDownload() {
  if (isSafeArtifactUrl(props.artifact.downloadUrl)) {
    window.open(props.artifact.downloadUrl, "_blank", "noopener,noreferrer");
    return;
  }

  try {
    // 1. 弹出操作系统原生另存为对话框
    const targetPath = await save({
      defaultPath: props.artifact.name,
      filters: props.artifact.format
        ? [{ name: props.artifact.format.toUpperCase(), extensions: [props.artifact.format] }]
        : undefined,
    });

    if (!targetPath) return; // 用户取消

    // 2. 调用 Rust 原生保存到目标路径
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("export_artifact_to_file", {
      artifactId: props.artifact.artifactId,
      sourcePath: props.artifact.localPath,
      content: props.artifact.content,
      targetPath,
    });

    downloaded.value = true;
    setTimeout(() => {
      downloaded.value = false;
    }, 2500);
    emit("download", props.artifact);
  } catch (err) {
    console.warn("系统另存为失败，降级使用 Blob 下载:", err);
    let content = props.artifact.content;
    if (!content) {
      if (props.artifact.artifactId) {
        content = (await readArtifactById(props.artifact.artifactId)) || undefined;
      } else if (props.artifact.localPath) {
        content = (await readArtifactFile(props.artifact.localPath)) || undefined;
      }
    }

    if (content) {
      props.artifact.content = content;
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
</script>

<style scoped>
.artifact-card {
  display: flex;
  align-items: center;
  gap: 12px;
  background: rgba(var(--theme-rgb-panel, 24, 24, 28), 0.75);
  border: 1px solid rgba(var(--theme-foreground-rgb), 0.09);
  border-radius: 10px;
  padding: 10px 14px;
  transition: all 0.18s cubic-bezier(0.16, 1, 0.3, 1);
  backdrop-filter: blur(12px);
  position: relative;
  overflow: hidden;
  width: 100%;
}

.artifact-card:hover {
  background: rgba(var(--theme-rgb-panel, 30, 30, 36), 0.9);
  border-color: rgba(var(--theme-foreground-rgb), 0.18);
  box-shadow: var(--theme-shadow, 0 4px 20px rgba(0, 0, 0, 0.35));
  transform: translateY(-1px);
}

.artifact-icon-wrap {
  position: relative;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  width: 42px;
  height: 42px;
  border-radius: 8px;
  background: rgba(var(--theme-foreground-rgb), 0.05);
  border: 1px solid rgba(var(--theme-foreground-rgb), 0.08);
  flex-shrink: 0;
}

.artifact-icon {
  font-size: 20px;
  line-height: 1;
}

.artifact-format-badge {
  position: absolute;
  bottom: -4px;
  font-size: 8.5px;
  font-weight: 800;
  letter-spacing: 0.3px;
  color: #a5b4fc;
  background: #1e1b4b;
  border: 1px solid rgba(165, 180, 252, 0.3);
  border-radius: 4px;
  padding: 0 3px;
  line-height: 1.2;
}

.artifact-main-info {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 3px;
}

.artifact-title-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.artifact-name {
  font-size: 13px;
  font-weight: 600;
  color: var(--theme-text-primary, #f4f4f5);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 100%;
}

.artifact-meta-row {
  display: flex;
  align-items: center;
  flex-wrap: nowrap;
  gap: 6px;
  font-size: 11px;
  overflow: hidden;
  max-width: 100%;
}

.meta-tag {
  display: inline-flex;
  align-items: center;
  padding: 1px 6px;
  border-radius: 4px;
  font-size: 10.5px;
  font-weight: 500;
  white-space: nowrap;
  flex-shrink: 0;
}

.kind-tag {
  background: rgba(99, 102, 241, 0.12);
  color: var(--theme-status-accent, #a5b4fc);
  border: 1px solid rgba(99, 102, 241, 0.2);
}

.size-tag {
  background: rgba(var(--theme-foreground-rgb), 0.06);
  color: var(--theme-text-secondary, #a1a1aa);
}

.path-tag {
  background: rgba(16, 185, 129, 0.12);
  color: var(--theme-status-success, #6ee7b7);
  border: 1px solid rgba(16, 185, 129, 0.2);
}

.meta-desc {
  color: var(--theme-text-muted, #71717a);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  flex: 1;
  min-width: 0;
}

.artifact-actions {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-shrink: 0;
}

.artifact-action-btn {
  display: inline-flex !important;
  align-items: center !important;
  gap: 4px !important;
  height: 28px !important;
  padding: 0 10px !important;
  border-radius: 6px !important;
  font-size: 11.5px !important;
  font-weight: 500 !important;
  cursor: pointer;
  transition: all 0.15s ease;
  border: 1px solid rgba(var(--theme-foreground-rgb), 0.1) !important;
  background: rgba(var(--theme-foreground-rgb), 0.04) !important;
  color: var(--theme-text-primary, #e4e4e7) !important;
}

.artifact-action-btn svg {
  width: 13px;
  height: 13px;
  stroke: currentColor;
  stroke-width: 2;
  fill: none;
}

.artifact-action-btn:hover {
  background: rgba(var(--theme-foreground-rgb), 0.1) !important;
  border-color: rgba(var(--theme-foreground-rgb), 0.25) !important;
  color: var(--theme-text-primary, #ffffff) !important;
}

.btn-preview {
  background: rgba(99, 102, 241, 0.16) !important;
  border-color: rgba(99, 102, 241, 0.35) !important;
  color: var(--theme-status-accent, #c7d2fe) !important;
}

.btn-preview:hover {
  background: rgba(99, 102, 241, 0.3) !important;
  border-color: rgba(99, 102, 241, 0.6) !important;
  color: var(--theme-text-primary, #ffffff) !important;
}

.text-green {
  color: var(--theme-status-success, #34d399) !important;
}
</style>
