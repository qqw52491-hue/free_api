<template>
  <div class="mermaid-card">
    <div class="mermaid-header">
      <div class="mermaid-title">
        <span class="chart-type-icon">📐</span>
        <span>{{ diagramTitle }}</span>
      </div>
      <div class="mermaid-tools">
        <button type="button" class="tool-btn" title="导出为 SVG" @click="downloadSvg">
          <svg viewBox="0 0 24 24"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3"/></svg>
          <span>导出 SVG</span>
        </button>
        <button type="button" class="tool-btn" title="复制代码" @click="copyCode">
          <svg viewBox="0 0 24 24"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
          <span>{{ copied ? '已复制' : '复制' }}</span>
        </button>
      </div>
    </div>

    <div v-if="renderError" class="mermaid-error">
      <span class="error-icon">⚠️</span>
      <span>Mermaid 语法错误: {{ renderError }}</span>
    </div>

    <div ref="containerRef" class="mermaid-body" v-html="svgContent"></div>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";
import mermaid from "mermaid";

const props = defineProps<{
  code: string;
}>();

const containerRef = ref<HTMLDivElement | null>(null);
const svgContent = ref<string>("");
const renderError = ref<string | null>(null);
const copied = ref(false);

const uniqueId = `mermaid-${Math.random().toString(36).substring(2, 9)}`;

const diagramTitle = computed(() => {
  const trimmed = props.code.trim().toLowerCase();
  if (trimmed.startsWith("graph") || trimmed.startsWith("flowchart")) return "流程架构图";
  if (trimmed.startsWith("sequencediagram")) return "时序交互图";
  if (trimmed.startsWith("classdiagram")) return "类关系图";
  if (trimmed.startsWith("statediagram")) return "状态机图";
  if (trimmed.startsWith("gantt")) return "甘特进度图";
  if (trimmed.startsWith("mindmap")) return "思维导图";
  if (trimmed.startsWith("erdiagram")) return "实体关系图 (ER)";
  return "拓扑图表";
});

async function renderDiagram() {
  renderError.value = null;
  const raw = props.code.trim();
  if (!raw) return;

  try {
    mermaid.initialize({
      startOnLoad: false,
      theme: "dark",
      securityLevel: "loose",
      themeVariables: {
        darkMode: true,
        background: "#18181b",
        mainBkg: "#1f1f23",
        primaryColor: "#6c63ff",
        primaryTextColor: "#f4f4f5",
        primaryBorderColor: "rgba(255, 255, 255, 0.15)",
        lineColor: "#8e8e93",
        secondaryColor: "#00d4aa",
        tertiaryColor: "#ffa43d",
        edgeLabelBackground: "#18181b",
        fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif",
        fontSize: "13px",
      },
    });

    const { svg } = await mermaid.render(uniqueId, raw);
    svgContent.value = svg;
  } catch (err: any) {
    renderError.value = err?.message || String(err);
    svgContent.value = "";
  }
}

function downloadSvg() {
  if (!svgContent.value) return;
  const blob = new Blob([svgContent.value], { type: "image/svg+xml" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.download = `${diagramTitle.value}.svg`;
  link.href = url;
  link.click();
  URL.revokeObjectURL(url);
}

function copyCode() {
  navigator.clipboard.writeText(props.code).then(() => {
    copied.value = true;
    setTimeout(() => (copied.value = false), 1800);
  });
}

onMounted(() => {
  nextTick(renderDiagram);
});

watch(() => props.code, () => {
  nextTick(renderDiagram);
});
</script>

<style scoped>
.mermaid-card {
  width: 100%;
  margin: 14px 0;
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 10px;
  background: #18181b;
  overflow: hidden;
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.25);
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
}

.mermaid-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  height: 38px;
  padding: 0 12px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.06);
  background: rgba(255, 255, 255, 0.02);
}

.mermaid-title {
  display: flex;
  align-items: center;
  gap: 6px;
  color: #f4f4f5;
  font-size: 12.5px;
  font-weight: 600;
}

.chart-type-icon { font-size: 12px; }

.mermaid-tools {
  display: flex;
  align-items: center;
  gap: 6px;
}

.tool-btn {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 3px 8px;
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 5px;
  color: #a1a1aa;
  background: transparent;
  cursor: pointer;
  font-size: 11px;
  transition: all 0.15s;
}

.tool-btn svg {
  width: 12px;
  height: 12px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
}

.tool-btn:hover {
  color: #ffffff;
  background: rgba(255, 255, 255, 0.08);
  border-color: rgba(255, 255, 255, 0.16);
}

.mermaid-error {
  padding: 10px 14px;
  color: #fca5a5;
  background: rgba(239, 68, 68, 0.08);
  border-bottom: 1px solid rgba(239, 68, 68, 0.15);
  font-size: 12px;
  display: flex;
  align-items: center;
  gap: 6px;
}

.mermaid-body {
  width: 100%;
  padding: 16px;
  display: flex;
  justify-content: center;
  overflow-x: auto;
  background: #141416;
}

.mermaid-body :deep(svg) {
  max-width: 100%;
  height: auto;
}
</style>
