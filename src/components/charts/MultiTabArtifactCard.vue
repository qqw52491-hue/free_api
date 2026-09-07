<template>
  <div class="multi-tab-card">
    <div class="card-top-bar">
      <div class="card-title">
        <span>📑</span>
        <strong>{{ title || '复合交付看板' }}</strong>
      </div>
      <div class="tabs-nav">
        <button
          v-for="tab in tabs"
          :key="tab.id"
          type="button"
          class="tab-nav-btn"
          :class="{ active: currentTabId === tab.id }"
          @click="currentTabId = tab.id"
        >
          <span class="tab-icon">{{ tab.icon || defaultIcon(tab.type) }}</span>
          <span>{{ tab.label }}</span>
        </button>
      </div>
    </div>

    <div class="tab-pane-content">
      <template v-for="tab in tabs" :key="tab.id">
        <div v-show="currentTabId === tab.id" class="pane-wrap">
          <RichContentRenderer v-if="tab.type === 'markdown'" :content="tab.content" />
          <EChartsBlock v-else-if="tab.type === 'echarts'" :code="tab.content" />
          <MermaidBlock v-else-if="tab.type === 'mermaid'" :code="tab.content" />
          <HtmlPreviewBlock v-else-if="tab.type === 'html'" :code="tab.content" />
          <pre v-else class="code-pane"><code>{{ tab.content }}</code></pre>
        </div>
      </template>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, defineAsyncComponent } from "vue";
import RichContentRenderer from "./RichContentRenderer.vue";
import HtmlPreviewBlock from "./HtmlPreviewBlock.vue";

const EChartsBlock = defineAsyncComponent(() => import("./EChartsBlock.vue"));
const MermaidBlock = defineAsyncComponent(() => import("./MermaidBlock.vue"));

export interface TabItem {
  id: string;
  label: string;
  type: "markdown" | "echarts" | "mermaid" | "html" | "code";
  content: string;
  icon?: string;
}

const props = defineProps<{
  title?: string;
  tabs: TabItem[];
}>();

const currentTabId = ref<string>(props.tabs[0]?.id || "");

function defaultIcon(type: string) {
  switch (type) {
    case "markdown": return "📄";
    case "echarts": return "📊";
    case "mermaid": return "📐";
    case "html": return "🌐";
    default: return "💻";
  }
}
</script>

<style scoped>
.multi-tab-card {
  width: 100%;
  margin: 14px 0;
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 10px;
  background: #18181b;
  overflow: hidden;
  box-shadow: 0 4px 20px rgba(0, 0, 0, 0.3);
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
}

.card-top-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 8px 14px;
  background: #1f1f23;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
}

.card-title {
  display: flex;
  align-items: center;
  gap: 6px;
  color: #f4f4f5;
  font-size: 13px;
}

.tabs-nav {
  display: flex;
  align-items: center;
  gap: 4px;
  background: #141416;
  padding: 3px;
  border-radius: 7px;
  border: 1px solid rgba(255, 255, 255, 0.06);
}

.tab-nav-btn {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 4px 10px;
  border: 0;
  border-radius: 5px;
  background: transparent;
  color: #a1a1aa;
  font-size: 12px;
  font-weight: 550;
  cursor: pointer;
  transition: all 0.15s;
}

.tab-nav-btn:hover { color: #fff; }

.tab-nav-btn.active {
  background: #27272a;
  color: #f4f4f5;
  box-shadow: 0 1px 4px rgba(0, 0, 0, 0.3);
}

.tab-pane-content {
  padding: 14px;
}

.pane-wrap {
  width: 100%;
}

.code-pane {
  margin: 0;
  padding: 12px;
  background: #141416;
  border-radius: 6px;
  color: #e4e4e7;
  font-size: 12px;
  overflow: auto;
}
</style>
