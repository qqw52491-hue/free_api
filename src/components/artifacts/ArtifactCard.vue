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
        @click.stop="$emit('preview', artifact)"
      >
        <svg viewBox="0 0 24 24"><path d="M2 12s3-7 10-7 10 7 10 7-3 7-10 7-10-7-10-7Z"/><circle cx="12" cy="12" r="3"/></svg>
        <span>预览</span>
      </button>

      <!-- 下载 / 导出按钮 -->
      <button
        v-if="canDownload"
        type="button"
        class="artifact-action-btn btn-download"
        title="下载保存此文件"
        @click.stop="handleDownload"
      >
        <svg viewBox="0 0 24 24"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3"/></svg>
        <span>下载</span>
      </button>

      <!-- 复制本地路径按钮 (对于本地文件诚实提供复制) -->
      <button
        v-if="artifact.localPath"
        type="button"
        class="artifact-action-btn btn-copy"
        :title="copied ? '已复制路径' : `复制路径: ${artifact.localPath}`"
        @click.stop="copyPath"
      >
        <svg v-if="!copied" viewBox="0 0 24 24"><rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg>
        <svg v-else viewBox="0 0 24 24" class="text-green"><path d="M20 6 9 17l-5-5"/></svg>
        <span>{{ copied ? "已复制" : "复制路径" }}</span>
      </button>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from "vue";
import type { ArtifactDescriptor } from "../../types/artifact";
import {
  formatFileSize,
  getArtifactIcon,
  getArtifactKindLabel,
  isArtifactPreviewable,
  isSafeArtifactUrl,
} from "../../utils/artifact";

const props = defineProps<{
  artifact: ArtifactDescriptor;
}>();

const emit = defineEmits<{
  preview: [artifact: ArtifactDescriptor];
  download: [artifact: ArtifactDescriptor];
}>();

const icon = computed(() => getArtifactIcon(props.artifact.kind, props.artifact.format));
const kindLabel = computed(() => getArtifactKindLabel(props.artifact.kind, props.artifact.format));
const formattedSize = computed(() => formatFileSize(props.artifact.sizeBytes));
const canPreview = computed(() => isArtifactPreviewable(props.artifact));
const canDownload = computed(() => Boolean(isSafeArtifactUrl(props.artifact.downloadUrl) || props.artifact.content));

const copied = ref(false);

async function copyPath() {
  if (!props.artifact.localPath) return;
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
  if (isSafeArtifactUrl(props.artifact.downloadUrl)) {
    window.open(props.artifact.downloadUrl, "_blank", "noopener,noreferrer");
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
</script>

<style scoped>
.artifact-card {
  display: flex;
  align-items: center;
  gap: 12px;
  background: rgba(24, 24, 28, 0.75);
  border: 1px solid rgba(255, 255, 255, 0.09);
  border-radius: 10px;
  padding: 10px 14px;
  transition: all 0.18s cubic-bezier(0.16, 1, 0.3, 1);
  backdrop-filter: blur(12px);
  position: relative;
  overflow: hidden;
}

.artifact-card:hover {
  background: rgba(30, 30, 36, 0.9);
  border-color: rgba(255, 255, 255, 0.18);
  box-shadow: 0 4px 20px rgba(0, 0, 0, 0.35);
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
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.08);
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
  color: #f4f4f5;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 100%;
}

.artifact-meta-row {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 6px;
  font-size: 11px;
}

.meta-tag {
  display: inline-flex;
  align-items: center;
  padding: 1px 6px;
  border-radius: 4px;
  font-size: 10.5px;
  font-weight: 500;
}

.kind-tag {
  background: rgba(99, 102, 241, 0.12);
  color: #a5b4fc;
  border: 1px solid rgba(99, 102, 241, 0.2);
}

.size-tag {
  background: rgba(255, 255, 255, 0.06);
  color: #a1a1aa;
}

.path-tag {
  background: rgba(16, 185, 129, 0.12);
  color: #6ee7b7;
  border: 1px solid rgba(16, 185, 129, 0.2);
}

.meta-desc {
  color: #71717a;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 200px;
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
  border: 1px solid rgba(255, 255, 255, 0.1) !important;
  background: rgba(255, 255, 255, 0.04) !important;
  color: #e4e4e7 !important;
}

.artifact-action-btn svg {
  width: 13px;
  height: 13px;
  stroke: currentColor;
  stroke-width: 2;
  fill: none;
}

.artifact-action-btn:hover {
  background: rgba(255, 255, 255, 0.1) !important;
  border-color: rgba(255, 255, 255, 0.25) !important;
  color: #ffffff !important;
}

.btn-preview {
  background: rgba(99, 102, 241, 0.16) !important;
  border-color: rgba(99, 102, 241, 0.35) !important;
  color: #c7d2fe !important;
}

.btn-preview:hover {
  background: rgba(99, 102, 241, 0.3) !important;
  border-color: rgba(99, 102, 241, 0.6) !important;
  color: #ffffff !important;
}

.text-green {
  color: #34d399 !important;
}
</style>
