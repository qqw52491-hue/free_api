<template>
  <div class="html-preview-card" :class="{ 'is-fullscreen': isFullscreen }">
    <!-- 右上角悬浮精致操作坞 (Floating Glass Action Dock) -->
    <div class="floating-dock">
      <!-- 网页标题 / 标志 -->
      <span class="dock-tag">
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><line x1="2" y1="12" x2="22" y2="12"/><path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z"/></svg>
        <span>{{ documentTitle }}</span>
      </span>

      <!-- 预览 / 源码 模式切换小胶囊 -->
      <button
        type="button"
        class="dock-btn mode-switch-btn"
        :title="activeTab === 'preview' ? '切换查看源代码' : '切换回实时预览'"
        @click="activeTab = activeTab === 'preview' ? 'code' : 'preview'"
      >
        <template v-if="activeTab === 'preview'">
          <svg viewBox="0 0 24 24"><polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/></svg>
          <span>源码</span>
        </template>
        <template v-else>
          <svg viewBox="0 0 24 24"><polygon points="5 3 19 12 5 21 5 3"/></svg>
          <span>预览</span>
        </template>
      </button>

      <!-- 重新运行 -->
      <button
        v-if="activeTab === 'preview'"
        type="button"
        class="dock-btn"
        title="刷新运行"
        @click="reloadIframe"
      >
        <svg viewBox="0 0 24 24"><path d="M23 4v6h-6M1 20v-6h6"/><path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15"/></svg>
      </button>

      <!-- 全屏沉浸 -->
      <button
        type="button"
        class="dock-btn"
        :title="isFullscreen ? '退出全屏 (ESC)' : '全屏沉浸预览'"
        @click="toggleFullscreen"
      >
        <svg v-if="!isFullscreen" viewBox="0 0 24 24"><path d="M8 3H5a2 2 0 0 0-2 2v3m18 0V5a2 2 0 0 0-2-2h-3m0 18h3a2 2 0 0 0 2-2v-3M3 16v3a2 2 0 0 0 2 2h3"/></svg>
        <svg v-else viewBox="0 0 24 24"><path d="M8 3v3a2 2 0 0 1-2 2H3m18 0h-3a2 2 0 0 1-2-2V3m0 18v-3a2 2 0 0 1 2-2h3M3 16h3a2 2 0 0 1 2 2v3"/></svg>
      </button>

      <!-- 导出 HTML -->
      <button
        type="button"
        class="dock-btn"
        title="导出为独立 HTML 文件"
        @click="exportHtml"
      >
        <svg viewBox="0 0 24 24"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3"/></svg>
      </button>

      <!-- 复制代码 -->
      <button
        type="button"
        class="dock-btn"
        title="复制代码"
        @click="copyCode"
      >
        <svg viewBox="0 0 24 24"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
        <span v-if="copied" class="copied-badge">已复制</span>
      </button>
    </div>

    <!-- 纯净展示主体 -->
    <div class="card-body" :style="{ height: isFullscreen ? '100vh' : (height === '100%' ? '100%' : (typeof height === 'number' ? `${height}px` : height)), flex: height === '100%' ? '1' : undefined }">
      <!-- 1. 实时预览 (纯净沙箱直接展示) -->
      <div v-show="activeTab === 'preview'" class="preview-container">
        <iframe
          ref="iframeRef"
          class="sandbox-iframe"
          :srcdoc="preparedHtml"
          sandbox="allow-scripts allow-modals allow-forms allow-popups"
          allow="accelerometer; camera; encrypted-media; geolocation; gyroscope; microphone; midi; clipboard-read; clipboard-write"
          frameborder="0"
          @load="handleIframeLoad"
        ></iframe>
      </div>

      <!-- 2. 源代码视图 -->
      <div v-show="activeTab === 'code'" class="code-container">
        <pre><code class="hljs language-html" v-html="highlightedCode"></code></pre>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref } from "vue";
import hljs from "highlight.js/lib/common";

const props = withDefaults(
  defineProps<{
    code: string;
    height?: number | string;
  }>(),
  {
    height: 480,
  }
);

const emit = defineEmits<{
  "agent-submit": [data: any];
  "agent-cancel": [reason: string];
}>();

const activeTab = ref<"preview" | "code">("preview");
const iframeRef = ref<HTMLIFrameElement | null>(null);
const isFullscreen = ref(false);
const copied = ref(false);

const documentTitle = computed(() => {
  const match = props.code.match(/<title>([^<]+)<\/title>/i);
  return match ? match[1].trim() : "HTML 交互页面";
});

// 通信桥梁内置脚本 (自动注入 window.Agent SDK)
const BRIDGE_SCRIPT = `
<script>
(function() {
  var agentPort = null;
  window.addEventListener('message', function(e) {
    if (e.data === 'FREE_API_INIT_PORT' && e.ports && e.ports[0]) {
      agentPort = e.ports[0];
      agentPort.onmessage = function(event) {
        if (window.Agent && typeof window.Agent.onMessage === 'function') {
          window.Agent.onMessage(event.data);
        }
      };
    }
  });

  window.Agent = {
    submit: function(data) {
      if (agentPort) {
        agentPort.postMessage({ type: 'SUBMIT', data: data });
      } else {
        window.top.postMessage({ source: 'FREE_API_AGENT_BRIDGE', type: 'SUBMIT', data: data }, '*');
      }
    },
    cancel: function(reason) {
      if (agentPort) {
        agentPort.postMessage({ type: 'CANCEL', data: reason || '用户取消操作' });
      } else {
        window.top.postMessage({ source: 'FREE_API_AGENT_BRIDGE', type: 'CANCEL', data: reason }, '*');
      }
    },
    toast: function(msg) {
      if (agentPort) {
        agentPort.postMessage({ type: 'TOAST', data: msg });
      }
    }
  };
})();
<` + `/script>`;

const preparedHtml = computed(() => {
  let html = props.code.trim();

  if (!html.toLowerCase().includes("<!doctype html>") && !html.toLowerCase().includes("<html")) {
    html = `<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>${documentTitle.value}</title>
  ${BRIDGE_SCRIPT}
  <style>
    body {
      margin: 0;
      padding: 16px;
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
      color: #e4e4e7;
      background: #18181b;
    }
  </style>
</head>
<body>
  ${html}
</body>
</html>`;
  } else {
    // 注入 Bridge SDK 到 <head>
    if (html.toLowerCase().includes("<head>")) {
      html = html.replace(/<head>/i, `<head>\n${BRIDGE_SCRIPT}\n`);
    } else if (html.toLowerCase().includes("<html>")) {
      html = html.replace(/<html>/i, `<html>\n<head>${BRIDGE_SCRIPT}</head>\n`);
    } else {
      html = `${BRIDGE_SCRIPT}\n${html}`;
    }

    // 自动闭合被意外截断的 script/body/html 标签
    const lower = html.toLowerCase();
    const openScriptCount = (lower.match(/<script/g) || []).length;
    const closeScriptCount = (lower.match(/<\/script>/g) || []).length;
    if (openScriptCount > closeScriptCount) {
      html += "\n<" + "/script>";
    }
    if (!lower.includes("</body>")) {
      html += "\n<" + "/body>";
    }
    if (!lower.includes("</html>")) {
      html += "\n<" + "/html>";
    }
  }
  return html;
});

const highlightedCode = computed(() => {
  try {
    return hljs.highlight(props.code, { language: "html" }).value;
  } catch {
    return props.code;
  }
});

let currentChannel: MessageChannel | null = null;

function handleIframeLoad() {
  const iframe = iframeRef.value;
  if (!iframe?.contentWindow) return;

  // 建立全新的 MessageChannel 专线通道
  currentChannel = new MessageChannel();
  currentChannel.port1.onmessage = (event) => {
    const { type, data } = event.data || {};
    if (type === "SUBMIT") {
      emit("agent-submit", data);
    } else if (type === "CANCEL") {
      emit("agent-cancel", data);
    }
  };

  // 把 Port 2 递交给沙箱内部
  iframe.contentWindow.postMessage("FREE_API_INIT_PORT", "*", [currentChannel.port2]);
}

// 兼容全局 window.top.postMessage 兜底通信
function handleGlobalMessage(event: MessageEvent) {
  if (event.data?.source === "FREE_API_AGENT_BRIDGE") {
    const { type, data } = event.data;
    if (type === "SUBMIT") {
      emit("agent-submit", data);
    } else if (type === "CANCEL") {
      emit("agent-cancel", data);
    }
  }
}

onMounted(() => {
  window.addEventListener("message", handleGlobalMessage);
});

onUnmounted(() => {
  window.removeEventListener("message", handleGlobalMessage);
  if (currentChannel) {
    currentChannel.port1.close();
    currentChannel = null;
  }
});

function reloadIframe() {
  if (iframeRef.value) {
    const prev = iframeRef.value.srcdoc;
    iframeRef.value.srcdoc = "";
    nextTick(() => {
      if (iframeRef.value) {
        iframeRef.value.srcdoc = prev;
      }
    });
  }
}

function toggleFullscreen() {
  isFullscreen.value = !isFullscreen.value;
}

async function copyCode() {
  try {
    await navigator.clipboard.writeText(props.code);
    copied.value = true;
    setTimeout(() => (copied.value = false), 2000);
  } catch (e) {
    console.error("复制失败:", e);
  }
}

function exportHtml() {
  try {
    const blob = new Blob([preparedHtml.value], { type: "text/html;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${documentTitle.value.replace(/[^a-zA-Z0-9_\u4e00-\u9fa5]/g, "_") || "export"}.html`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  } catch (e) {
    console.error("导出失败:", e);
  }
}
</script>

<style scoped>
.html-preview-card {
  position: relative;
  width: 100%;
  border-radius: 12px;
  overflow: hidden;
  border: 1px solid rgba(var(--theme-foreground-rgb), 0.1);
  background: var(--theme-bg-base, #141416);
  box-shadow: var(--theme-shadow, 0 4px 20px rgba(0, 0, 0, 0.3));
  margin: 10px 0;
  transition: all 0.25s ease;
}

/* 全屏沉浸模式 */
.html-preview-card.is-fullscreen {
  position: fixed;
  top: 0;
  left: 0;
  width: 100vw;
  height: 100vh;
  z-index: 9999;
  border-radius: 0;
  margin: 0;
  border: none;
}

/* 右上角极简半透明毛玻璃悬浮操作坞 */
.floating-dock {
  position: absolute;
  top: 10px;
  right: 12px;
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 8px;
  border-radius: 20px;
  background: rgba(var(--theme-rgb-panel, 24, 24, 27), 0.75);
  backdrop-filter: blur(12px);
  -webkit-backdrop-filter: blur(12px);
  border: 1px solid rgba(var(--theme-foreground-rgb), 0.12);
  box-shadow: var(--theme-shadow, 0 4px 16px rgba(0, 0, 0, 0.4));
  z-index: 100;
  transition: opacity 0.2s ease, transform 0.2s ease;
}
.html-preview-card:hover .floating-dock {
  opacity: 1;
}

.dock-tag {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  color: var(--theme-status-warning, #fbbf24);
  font-size: 11px;
  font-weight: 600;
  padding-right: 6px;
  border-right: 1px solid rgba(var(--theme-foreground-rgb), 0.1);
  max-width: 140px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.dock-btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 4px;
  padding: 4px 7px;
  border-radius: 6px;
  border: none;
  background: transparent;
  color: var(--theme-text-secondary, #a1a1aa);
  cursor: pointer;
  font-size: 11px;
  font-weight: 550;
  transition: all 0.15s ease;
}
.dock-btn:hover {
  color: var(--theme-text-primary, #ffffff);
  background: rgba(var(--theme-foreground-rgb), 0.1);
}
.dock-btn svg {
  width: 13px;
  height: 13px;
  fill: none;
  stroke: currentColor;
  stroke-width: 2;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.mode-switch-btn {
  color: var(--theme-status-info, #38bdf8);
  background: rgba(56, 189, 248, 0.08);
}
.mode-switch-btn:hover {
  color: var(--theme-text-primary, #ffffff);
  background: rgba(56, 189, 248, 0.2);
}

.copied-badge {
  font-size: 10px;
  color: var(--theme-status-success, #4ade80);
  font-weight: 600;
}

/* 主体内容 */
.card-body {
  width: 100%;
  position: relative;
  background: var(--theme-bg-base, #141416);
}

.preview-container,
.code-container {
  width: 100%;
  height: 100%;
}

.sandbox-iframe {
  width: 100%;
  height: 100%;
  border: none;
  background: var(--theme-bg-base, #141416);
}

.code-container {
  overflow: auto;
  padding: 16px;
  background: var(--theme-bg-deep, #101012);
}
.code-container pre {
  margin: 0;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  font-size: 12px;
  line-height: 1.6;
}
</style>
