<template>
  <div v-if="shouldShow" class="app-update-control" :class="{ collapsed }">
    <button
      type="button"
      class="update-trigger"
      :class="{ available: availableUpdate || updateInstalled }"
      :title="availableUpdate ? `发现新版本 ${availableUpdate.version}` : '应用更新'"
      aria-label="应用更新"
      :aria-expanded="panelOpen"
      @click="panelOpen = !panelOpen"
    >
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
        <path d="M12 3v12m-4-4 4 4 4-4M5 16v4h14v-4" />
      </svg>
      <span v-if="!collapsed">{{ availableUpdate ? '发现新版' : '应用更新' }}</span>
    </button>
    <section v-if="panelOpen" class="update-panel" role="dialog" aria-label="应用更新详情" @keydown.esc="panelOpen = false">
      <header><strong>应用更新</strong><button type="button" aria-label="关闭更新面板" @click="panelOpen = false">×</button></header>
      <p>当前版本 {{ currentVersion || '—' }} · GitHub Releases</p>
      <p class="update-message" aria-live="polite">{{ updateStatusText || '检查是否有可用的新版本。' }}</p>
      <p v-if="updateError" class="update-error" role="alert">{{ updateError }}</p>
      <progress v-if="updateInstalling" :value="downloadProgress ?? undefined" max="100" aria-label="更新下载进度" />
      <div class="update-actions">
        <button v-if="updateInstalled" type="button" :disabled="updateInstalling" @click="restartAfterUpdate">重启应用</button>
        <button v-else-if="availableUpdate && manualUpdateRequired" type="button" @click="openReleases">下载新版安装包</button>
        <button v-else-if="availableUpdate" type="button" :disabled="updateChecking || updateInstalling" @click="installAvailableUpdate">
          {{ updateInstalling ? '安装中…' : `下载并安装 v${availableUpdate.version}` }}
        </button>
        <button v-else type="button" :disabled="updateChecking || updateInstalling" @click="checkManually">{{ updateChecking ? '检查中…' : '检查更新' }}</button>
        <button type="button" class="secondary" @click="openReleases">查看发布页</button>
      </div>
      <small>{{ manualUpdateRequired ? '当前 deb/rpm 安装通过发布页下载新版安装包。' : '点击安装后会重启应用。' }}</small>
    </section>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  availableUpdate, updateChecking, updateInstalling, updateInstalled,
  updateStatusText, updateError, downloadProgress, checkUpdateOnce,
  installAvailableUpdate, restartAfterUpdate, manualUpdateRequired,
} from "../composables/useUpdater";

defineProps<{ collapsed?: boolean }>();
const panelOpen = ref(false);
const currentVersion = ref("");

// 只有存在可用更新、安装中、安装完成，或用户已手动打开面板时才显示该按钮；无更新时不占位
const shouldShow = computed(() => {
  return Boolean(
    availableUpdate.value ||
    updateInstalling.value ||
    updateInstalled.value ||
    panelOpen.value
  );
});
onMounted(() => { void getVersion().then(value => { currentVersion.value = value; }).catch(() => {}); });
async function checkManually() {
  try { await checkUpdateOnce(); } catch { /* 具体错误由共享状态显示 */ }
}
async function openReleases() {
  try { await openUrl("https://github.com/qqw52491-hue/free_api/releases"); }
  catch { updateError.value = "无法打开发布页，请在浏览器中访问 GitHub 仓库的 Releases。"; }
}
</script>

<style scoped>
.app-update-control {
  margin-left: auto;
}

.app-update-control.collapsed {
  margin-left: 0;
  display: flex;
  justify-content: center;
  width: 100%;
}

.update-trigger {
  display: flex;
  align-items: center;
  gap: 7px;
  padding: 5px;
  border: 0;
  border-radius: 7px;
  background: transparent;
  color: var(--theme-text-secondary, #a1a1aa);
  cursor: pointer;
  font: inherit;
  font-size: 11px;
  transition: all 0.15s ease;
}

.app-update-control.collapsed .update-trigger {
  width: 32px;
  height: 32px;
  padding: 0;
  justify-content: center;
  border-radius: 8px;
}

.update-trigger svg {
  width: 18px;
  height: 18px;
}

.update-trigger:hover {
  background: var(--theme-bg-raised, #27272a);
  color: var(--theme-text-primary, #fafafa);
}

.update-trigger.available {
  color: var(--theme-status-info, #60a5fa);
}
.update-panel { position: fixed; z-index: 100; left: 12px; bottom: 64px; width: min(330px, calc(100vw - 48px)); padding: 16px; border: 1px solid var(--theme-border, #3f3f46); border-radius: 12px; background: var(--theme-bg-panel, #18181b); box-shadow: 0 12px 40px #0006; color: var(--theme-text-primary, #e4e4e7); font-size: 12px; }
.update-panel header { display: flex; align-items: center; justify-content: space-between; }
.update-panel header button { border: 0; background: none; color: var(--theme-text-secondary, #a1a1aa); font-size: 20px; cursor: pointer; }
.update-panel p { margin: 10px 0; line-height: 1.6; overflow-wrap: anywhere; }
.update-panel p:first-of-type, .update-panel small { color: var(--theme-text-secondary, #a1a1aa); }
.update-panel .update-error { color: var(--theme-status-warning, #fbbf24); }
.update-panel progress { width: 100%; accent-color: #60a5fa; }
.update-actions { display: flex; gap: 8px; margin: 14px 0 10px; }
.update-actions button { padding: 7px 10px; border: 1px solid #3b82f6; border-radius: 6px; background: #2563eb; color: white; cursor: pointer; font: inherit; }
.update-actions button.secondary { border-color: var(--theme-border, #3f3f46); background: transparent; color: var(--theme-text-primary, #d4d4d8); }
.update-actions button:disabled { opacity: .55; cursor: wait; }
</style>
