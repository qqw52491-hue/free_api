import { ref } from "vue";
import { check, type Update } from "@tauri-apps/plugin-updater";

// 自托管更新状态（单例）：启动查一次 + 每小时轮询，发现新版则
// availableUpdate 非空——UI 按钮据此亮起"下载并安装 vX"。
// 检查失败（无网络/隧道未开/未发布过）静默当"已最新"，绝不打扰用户。
export const availableUpdate = ref<Update | null>(null);
export const updateChecking = ref(false);
export const updateStatusText = ref("");

let watcherTimer: number | null = null;
const POLL_MS = 60 * 60 * 1000; // 每小时

export async function checkUpdateOnce(): Promise<Update | null> {
  if (updateChecking.value) return availableUpdate.value;
  updateChecking.value = true;
  try {
    const found = await check();
    availableUpdate.value = found;
    if (found) {
      updateStatusText.value = `发现新版本 ${found.version}`;
    }
    return found;
  } catch (err) {
    console.warn("[updater] 检查更新失败:", err);
    return null;
  } finally {
    updateChecking.value = false;
  }
}

export function startUpdateWatcher() {
  if (watcherTimer !== null) return;
  void checkUpdateOnce();
  watcherTimer = window.setInterval(() => {
    if (document.hidden) return; // 后台挂着不白烧
    void checkUpdateOnce();
  }, POLL_MS);
}
