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

  // 1. 如果带有 download 属性（如导出下载文件）或 blob/javascript/锚点，直接放行
  if (target.hasAttribute("download")) return;

  const href = target.getAttribute("href") || target.getAttribute("data-url");
  if (!href || href === "#" || href.startsWith("#") || href.startsWith("javascript:") || href.startsWith("blob:") || href.startsWith("data:")) {
    return;
  }

  // 2. 只有普通的外部网络链接才拦截并以微信模式独立窗口打开
  if (/^https?:\/\//i.test(href) || /^mailto:/i.test(href)) {
    e.preventDefault();
    e.stopPropagation();
    const title = target.getAttribute("title") || target.textContent?.trim() || "网页预览";
    openWebviewWindow(href, title);
  }
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