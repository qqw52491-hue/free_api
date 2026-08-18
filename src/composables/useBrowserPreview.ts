import { ref } from "vue";
import { openUrl as tauriOpenUrl } from "@tauri-apps/plugin-opener";

const isVisible = ref(false);
const previewUrl = ref("");
const previewTitle = ref("");
const isLoading = ref(false);
const loadFailed = ref(false);

export async function openExternalUrl(target: string) {
  if (!target) return;
  try {
    await tauriOpenUrl(target);
  } catch (err) {
    console.error("无法在系统浏览器中打开链接:", err);
    // Fallback: 尝试原生 window.open
    window.open(target, "_blank", "noopener,noreferrer");
  }
}

export function useBrowserPreview() {
  function open(url: string, title?: string) {
    if (!url) return;
    previewUrl.value = url;
    previewTitle.value = title || url;
    isVisible.value = true;
    isLoading.value = true;
    loadFailed.value = false;
  }

  function close() {
    isVisible.value = false;
    previewUrl.value = "";
    previewTitle.value = "";
    isLoading.value = false;
    loadFailed.value = false;
  }

  async function openInExternalBrowser(urlToOpen?: string) {
    const target = urlToOpen || previewUrl.value;
    await openExternalUrl(target);
  }

  function onIframeLoad() {
    isLoading.value = false;
  }

  function onIframeError() {
    isLoading.value = false;
    loadFailed.value = true;
  }

  return {
    isVisible,
    previewUrl,
    previewTitle,
    isLoading,
    loadFailed,
    open,
    close,
    openInExternalBrowser,
    onIframeLoad,
    onIframeError,
  };
}
