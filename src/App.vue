<template>
  <div id="root-layout">
    <main class="main-content">
      <RouterView />
    </main>
  </div>
</template>

<script setup lang="ts">
import { onMounted, onUnmounted } from "vue";
import { openWebviewWindow } from "./composables/useBrowserPreview";

function handleGlobalLinkClick(e: MouseEvent) {
  // 向上查找最近的 <a> 链接元素
  const target = (e.target as HTMLElement)?.closest("a");
  if (!target) return;

  const href = target.getAttribute("href") || target.getAttribute("data-url");
  if (!href) return;

  // 1. 强制阻止 WebKit 原生重定向，100% 根除 "The URL can't be shown" 报错
  e.preventDefault();
  e.stopPropagation();

  // 2. 微信模式：新建专属独立原生 WebView 窗口打开网页
  const title = target.getAttribute("title") || target.textContent?.trim() || "网页预览";
  openWebviewWindow(href, title);
}

onMounted(() => {
  // 使用捕获阶段捕获所有动态渲染的富文本 markdown 链接
  window.addEventListener("click", handleGlobalLinkClick, true);
});

onUnmounted(() => {
  window.removeEventListener("click", handleGlobalLinkClick, true);
});
</script>

<style scoped>
#root-layout {
  display: flex;
  flex-direction: column;
  height: 100vh;
  width: 100vw;
  overflow: hidden;
}

.main-content {
  flex: 1;
  width: 100%;
  height: 100%;
  overflow: hidden;
  display: flex;
  flex-direction: column;
}
</style>