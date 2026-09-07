<template>
  <div class="echarts-card">
    <div class="echarts-header">
      <div class="echarts-title">
        <span class="chart-type-icon">{{ chartIcon }}</span>
        <span>{{ chartTitle || '数据可视化' }}</span>
      </div>
      <div class="echarts-tools">
        <button type="button" class="tool-btn" title="导出为图片" @click="downloadImage">
          <svg viewBox="0 0 24 24"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3"/></svg>
          <span>导出</span>
        </button>
        <button type="button" class="tool-btn" title="复制代码" @click="copyCode">
          <svg viewBox="0 0 24 24"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
          <span>{{ copied ? '已复制' : '复制' }}</span>
        </button>
      </div>
    </div>

    <div v-if="renderError" class="echarts-error">
      <span class="error-icon">⚠️</span>
      <span>图表配置解析错误: {{ renderError }}</span>
    </div>

    <div ref="chartContainer" class="echarts-body" :style="{ height: `${height}px` }"></div>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import * as echarts from "echarts/core";
import {
  BarChart,
  LineChart,
  PieChart,
  ScatterChart,
  RadarChart,
  TreeChart,
  TreemapChart,
  GraphChart,
  SankeyChart,
} from "echarts/charts";
import {
  TitleComponent,
  TooltipComponent,
  GridComponent,
  LegendComponent,
  ToolboxComponent,
  DataZoomComponent,
  VisualMapComponent,
  MarkLineComponent,
  MarkPointComponent,
  DatasetComponent,
  TransformComponent,
} from "echarts/components";
import { CanvasRenderer } from "echarts/renderers";

// 显式按需注册必需的图表组件与 Canvas 渲染器
echarts.use([
  TitleComponent,
  TooltipComponent,
  GridComponent,
  LegendComponent,
  ToolboxComponent,
  DataZoomComponent,
  VisualMapComponent,
  MarkLineComponent,
  MarkPointComponent,
  DatasetComponent,
  TransformComponent,
  BarChart,
  LineChart,
  PieChart,
  ScatterChart,
  RadarChart,
  TreeChart,
  TreemapChart,
  GraphChart,
  SankeyChart,
  CanvasRenderer,
]);

const props = withDefaults(
  defineProps<{
    code: string;
    height?: number;
  }>(),
  {
    height: 340,
  }
);

const chartContainer = ref<HTMLDivElement | null>(null);
const renderError = ref<string | null>(null);
const copied = ref(false);
const chartTitle = ref<string>("");
const chartType = ref<string>("bar");

let chartInstance: any = null;
let resizeObserver: ResizeObserver | null = null;

const chartIcon = computed(() => {
  switch (chartType.value) {
    case "graph": return "🕸️ 知识图谱";
    case "line": return "📈 折线图";
    case "pie": return "🥧 饼图";
    case "scatter": return "🌌 散点图";
    case "radar": return "🎯 雷达图";
    case "tree":
    case "treemap": return "🌲 树图";
    case "sankey": return "🌊 桑基图";
    default: return "📊 统计图表";
  }
});

// 解析 JSON 或宽松对象字面量
function parseOption(raw: string): any {
  const trimmed = raw.trim();
  try {
    return JSON.parse(trimmed);
  } catch {
    // 容错: 去除注释与包裹的 markdown 标记
    const cleaned = trimmed
      .replace(/^\s*```(?:json|echarts|javascript|js)?/i, "")
      .replace(/```\s*$/, "")
      .replace(/\/\/.*$/gm, "")
      .trim();
    try {
      return JSON.parse(cleaned);
    } catch {
      // 宽松函数求值
      return new Function(`return (${cleaned})`)();
    }
  }
}

function initChart() {
  if (!chartContainer.value) return;
  renderError.value = null;

  try {
    const rawOption = parseOption(props.code);
    if (!rawOption || typeof rawOption !== "object") {
      throw new Error("图表配置必须为有效的 JSON 对象");
    }

    if (rawOption.title?.text) {
      chartTitle.value = String(rawOption.title.text);
    }

    // 探测图表类型
    if (Array.isArray(rawOption.series) && rawOption.series[0]?.type) {
      chartType.value = rawOption.series[0].type;
    } else if (rawOption.series?.type) {
      chartType.value = rawOption.series.type;
    }

    if (!chartInstance) {
      chartInstance = echarts.init(chartContainer.value, "dark", {
        renderer: "canvas",
      });
    }

    // 注入 Obsidian 曜石暗色主题与精致调色板
    const obsidianPalette = [
      "#6c63ff",
      "#00d4aa",
      "#ffa43d",
      "#38bdf8",
      "#ff4757",
      "#8b83ff",
      "#2ed573",
      "#fbbf24",
      "#f43f5e",
    ];

    const finalOption: Record<string, any> = {
      backgroundColor: "transparent",
      color: rawOption.color || obsidianPalette,
      textStyle: {
        fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif",
        color: "#d4d4d8",
      },
      tooltip: {
        backgroundColor: "#1f1f23",
        borderColor: "rgba(255, 255, 255, 0.12)",
        textStyle: { color: "#f4f4f5", fontSize: 12 },
        extraCssText: "box-shadow: 0 8px 24px rgba(0, 0, 0, 0.4); border-radius: 8px;",
        ...rawOption.tooltip,
      },
      grid: {
        top: 40,
        left: 45,
        right: 25,
        bottom: 35,
        containLabel: true,
        borderColor: "rgba(255, 255, 255, 0.06)",
        ...rawOption.grid,
      },
      ...rawOption,
    };

    // 特殊优化: 如果是 graph (关系图谱/知识图谱)，自动补全物理力导向与优雅交互
    if (Array.isArray(finalOption.series)) {
      for (const s of finalOption.series) {
        if (s.type === "graph") {
          s.roam = s.roam ?? true;
          s.draggable = s.draggable ?? true;
          s.label = {
            show: true,
            position: "right",
            color: "#f4f4f5",
            fontSize: 11,
            ...(s.label as object),
          };
          s.lineStyle = {
            color: "source",
            curveness: 0.15,
            width: 1.5,
            ...(s.lineStyle as object),
          };
          if (!s.layout) s.layout = "force";
          if (s.layout === "force") {
            s.force = {
              repulsion: 160,
              edgeLength: [60, 140],
              gravity: 0.08,
              ...(s.force as object),
            };
          }
        }
      }
    }

    chartInstance.setOption(finalOption, true);
  } catch (err: any) {
    renderError.value = err?.message || String(err);
  }
}

function downloadImage() {
  if (!chartInstance) return;
  const url = chartInstance.getDataURL({
    type: "png",
    pixelRatio: 2,
    backgroundColor: "#18181b",
  });
  const link = document.createElement("a");
  link.download = `${chartTitle.value || "chart"}.png`;
  link.href = url;
  link.click();
}

function copyCode() {
  navigator.clipboard.writeText(props.code).then(() => {
    copied.value = true;
    setTimeout(() => (copied.value = false), 1800);
  });
}

onMounted(() => {
  nextTick(() => {
    initChart();
    if (chartContainer.value) {
      resizeObserver = new ResizeObserver(() => {
        chartInstance?.resize();
      });
      resizeObserver.observe(chartContainer.value);
    }
  });
});

watch(() => props.code, () => {
  nextTick(initChart);
});

onBeforeUnmount(() => {
  resizeObserver?.disconnect();
  chartInstance?.dispose();
  chartInstance = null;
});
</script>

<style scoped>
.echarts-card {
  width: 100%;
  margin: 14px 0;
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 10px;
  background: #18181b;
  overflow: hidden;
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.25);
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
}

.echarts-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  height: 38px;
  padding: 0 12px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.06);
  background: rgba(255, 255, 255, 0.02);
}

.echarts-title {
  display: flex;
  align-items: center;
  gap: 6px;
  color: #f4f4f5;
  font-size: 12.5px;
  font-weight: 600;
}

.chart-type-icon {
  font-size: 12px;
}

.echarts-tools {
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

.echarts-error {
  padding: 10px 14px;
  color: #fca5a5;
  background: rgba(239, 68, 68, 0.08);
  border-bottom: 1px solid rgba(239, 68, 68, 0.15);
  font-size: 12px;
  display: flex;
  align-items: center;
  gap: 6px;
}

.echarts-body {
  width: 100%;
  min-height: 260px;
}
</style>
