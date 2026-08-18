<template>
  <div id="root-layout">
    <main class="main-content">
      <RouterView />
    </main>

    <!-- 全局应用内浏览器与链接预览窗口 -->
    <BrowserPreviewModal />
  </div>
</template>

<script setup lang="ts">
import { onMounted, onUnmounted } from "vue";
import BrowserPreviewModal from "./components/common/BrowserPreviewModal.vue";
import { useBrowserPreview } from "./composables/useBrowserPreview";

const browserPreview = useBrowserPreview();

function handleGlobalLinkClick(e: MouseEvent) {
  // 向上查找最近的 <a> 链接元素
  const target = (e.target as HTMLElement)?.closest("a");
  if (!target) return;

  const href = target.getAttribute("href") || target.getAttribute("data-url");
  if (!href) return;

  // 1. 强制阻止 WebKit 原生重定向，100% 根除 "The URL can't be shown" 报错
  e.preventDefault();
  e.stopPropagation();

  // 2. HTTP / HTTPS 网页链接：呼起内置浏览器预览窗口
  if (/^https?:\/\//i.test(href)) {
    const title = target.getAttribute("title") || target.textContent?.trim() || href;
    browserPreview.open(href, title);
    return;
  }

  // 3. mailto: 邮件链接：通过 Tauri Opener 调用系统默认邮件客户端
  if (/^mailto:/i.test(href)) {
    browserPreview.openInExternalBrowser(href);
    return;
  }

  // 4. file:/// 或系统路径：通过外部应用打开
  if (/^file:\/\//i.test(href) || href.startsWith("/")) {
    browserPreview.openInExternalBrowser(href);
    return;
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