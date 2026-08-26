<template>
  <div id="root-layout">
    <main class="main-content">
      <RouterView />
    </main>
  </div>
</template>

<script setup lang="ts">
import { useGlobalLinkInterceptor } from "./composables/useGlobalLinkInterceptor";

// 全局注册 Markdown 目录大纲定位、本地文件/目录打开以及外部网页独立窗口拦截
useGlobalLinkInterceptor();
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