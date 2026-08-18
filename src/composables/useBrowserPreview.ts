import { invoke } from "@tauri-apps/api/core";
import { openUrl as tauriOpenUrl } from "@tauri-apps/plugin-opener";

/**
 * 微信模式：新建一个专属的 Tauri 原生独立 WebView 预览窗口
 * 1. 独立窗口，随意拖拽/缩放/置顶；
 * 2. 作为顶级窗口 (Top-Level Webview)，100% 突破 X-Frame-Options 与 CSP 限制；
 * 3. Google、B站、GitHub、百度等任何网站均可完美浏览、交互与播放。
 */
export async function openWebviewWindow(url: string, title?: string) {
  if (!url) return;
  try {
    if (/^https?:\/\//i.test(url)) {
      await invoke("open_in_webview_window", {
        url,
        title: title || "网页预览",
      });
      return;
    }
    // mailto / file / 本地路径等使用原生 opener
    await tauriOpenUrl(url);
  } catch (err) {
    console.warn("调用 open_in_webview_window 异常，降级通过系统打开:", err);
    try {
      await tauriOpenUrl(url);
    } catch {
      window.open(url, "_blank", "noopener,noreferrer");
    }
  }
}
