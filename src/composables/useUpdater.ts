import { ref, shallowRef } from "vue";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { getBundleType } from "@tauri-apps/api/app";

// GitHub Release 提供清单与签名包；检查不自动下载安装。
export const availableUpdate = shallowRef<Update | null>(null);
export const updateChecking = ref(false);
export const updateInstalling = ref(false);
export const updateInstalled = ref(false);
export const updateStatusText = ref("");
export const updateError = ref("");
export const downloadProgress = ref<number | null>(null);
// 当前 CI 的 Linux 主更新包为 AppImage；deb/rpm 使用发布页安装，
// 避免其客户端回退匹配 linux-x86_64 后下载到不同安装形式的包。
export const manualUpdateRequired = ref(false);

let watcherTimer: number | null = null;
let pendingCheck: Promise<Update | null> | null = null;
const POLL_MS = 60 * 60 * 1000;
const errorText = (error: unknown) => error instanceof Error ? error.message : String(error);

export function checkUpdateOnce(): Promise<Update | null> {
  // 手动检查复用后台在途请求，不能把“还没查完”当成“没有新版”。
  if (pendingCheck) return pendingCheck;
  if (updateInstalling.value || updateInstalled.value) return Promise.resolve(availableUpdate.value);
  updateChecking.value = true;
  updateError.value = "";
  updateStatusText.value = "正在检查更新…";
  pendingCheck = (async () => {
    try {
      const [found, bundle] = await Promise.all([check({ timeout: 20_000 }), getBundleType()]);
      manualUpdateRequired.value = bundle === "deb" || bundle === "rpm";
      const previous = availableUpdate.value;
      availableUpdate.value = found;
      if (previous && previous !== found) void previous.close().catch(() => {});
      updateStatusText.value = found ? `发现新版本 ${found.version}` : "已是最新版本";
      return found;
    } catch (error) {
      updateError.value = `无法检查更新：${errorText(error)}。请检查网络及 GitHub 是否已有公开发布的版本。`;
      updateStatusText.value = "";
      throw error;
    } finally {
      updateChecking.value = false;
      pendingCheck = null;
    }
  })();
  return pendingCheck;
}

export async function restartAfterUpdate() {
  try {
    updateError.value = "";
    await relaunch();
  } catch (error) {
    updateError.value = `更新已安装，重启失败：${errorText(error)}。请手动重启应用。`;
  }
}

export async function installAvailableUpdate() {
  if (updateInstalling.value || updateInstalled.value) return;
  // 正在刷新缓存时先等同一个检查完成，避免安装期间关闭其资源句柄。
  if (pendingCheck) {
    try { await pendingCheck; } catch { return; }
  }
  if (updateInstalling.value || updateInstalled.value) return;
  const update = availableUpdate.value;
  if (!update) return;
  if (manualUpdateRequired.value) {
    updateError.value = "当前 deb/rpm 安装请从发布页下载新版安装包。";
    return;
  }
  updateInstalling.value = true;
  updateError.value = "";
  downloadProgress.value = null;
  let downloaded = 0;
  let total = 0;
  updateStatusText.value = `正在下载 ${update.version}…`;
  try {
    await update.downloadAndInstall((event) => {
      if (event.event === "Started") {
        downloaded = 0;
        total = event.data.contentLength ?? 0;
      } else if (event.event === "Progress") {
        downloaded += event.data.chunkLength;
        downloadProgress.value = total > 0 ? Math.min(100, Math.floor(downloaded / total * 100)) : null;
        updateStatusText.value = total > 0
          ? `正在下载 ${update.version} · ${downloadProgress.value}%`
          : `正在下载 ${update.version} · ${(downloaded / 1024 / 1024).toFixed(1)} MB`;
      } else {
        downloadProgress.value = 100;
        updateStatusText.value = "下载完成，正在验证并安装…";
      }
    });
    availableUpdate.value = null;
    updateInstalled.value = true;
    updateStatusText.value = `${update.version} 已安装，即将重启…`;
    await restartAfterUpdate();
  } catch (error) {
    updateStatusText.value = "";
    updateError.value = `更新安装失败：${errorText(error)}`;
  } finally {
    updateInstalling.value = false;
  }
}

export function startUpdateWatcher() {
  if (watcherTimer !== null) return;
  const poll = () => {
    if (!updateInstalling.value && !updateInstalled.value) {
      // 后台错误留给更新面板显示，不弹窗、不产生未处理的拒绝。
      void checkUpdateOnce().catch(() => {});
    }
  };
  poll();
  watcherTimer = window.setInterval(() => {
    if (!document.hidden) poll();
  }, POLL_MS);
}
