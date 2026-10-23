<template>
  <div id="root-layout">
    <main class="main-content">
      <RouterView />
    </main>
  </div>
</template>

<script setup lang="ts">
import { useGlobalLinkInterceptor } from "./composables/useGlobalLinkInterceptor";
import { startUpdateWatcher } from "./composables/useUpdater";

// 全局注册 Markdown 目录大纲定位、本地文件/目录打开以及外部网页独立窗口拦截
useGlobalLinkInterceptor();
// 启动 + 每小时间隔检查 GitHub 更新；发现新版点亮侧栏的下载按钮
startUpdateWatcher();
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
