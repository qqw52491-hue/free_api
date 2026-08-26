import { onMounted, onUnmounted } from "vue";
import { openPath as tauriOpenPath } from "@tauri-apps/plugin-opener";
import { openWebviewWindow } from "./useBrowserPreview";

/**
 * 规范化本地路径：剥离 file:// 协议头、处理 Windows 盘符、URL 解码、去除锚点行号
 */
export function normalizeLocalFilePath(rawPath: string): string {
  let cleaned = rawPath.trim();
  if (cleaned.startsWith("file://")) {
    cleaned = cleaned.replace(/^file:\/\//i, "");
  }
  // Windows 路径修正：如 /C:/Users/... -> C:/Users/...
  if (/^\/[A-Za-z]:[\\/]/.test(cleaned)) {
    cleaned = cleaned.slice(1);
  }
  // 剥离可能携带的行号锚点（如 #L10-L20 或 #15）
  cleaned = cleaned.split("#")[0];
  try {
    return decodeURIComponent(cleaned);
  } catch {
    return cleaned;
  }
}

/**
 * 全局链接点击事件处理器
 */
export async function handleGlobalLinkClick(e: MouseEvent): Promise<void> {
  const target = (e.target as HTMLElement)?.closest("a");
  if (!target) return;

  // 导出下载文件等带有 download 属性的原生放行
  if (target.hasAttribute("download")) return;

  const href = target.getAttribute("href") || target.getAttribute("data-local-path") || target.getAttribute("data-url");
  if (!href || href === "javascript:" || href.startsWith("blob:") || href.startsWith("data:")) {
    return;
  }

  // 1. 文档大纲/目录锚点跳转 (TOC / #heading)
  if (href.startsWith("#")) {
    e.preventDefault();
    e.stopPropagation();
    const rawId = href.slice(1);
    if (!rawId) return;

    const decodedId = decodeURIComponent(rawId);
    const slugId = decodedId.toLowerCase().replace(/[^\w\u4e00-\u9fa5]+/g, "-");

    // 优先匹配标准 ID、原始解码 ID 或标准化 Slug ID
    const targetEl =
      document.getElementById(rawId) ||
      document.getElementById(decodedId) ||
      document.getElementById(slugId) ||
      document.querySelector(`[name="${rawId}"]`) ||
      document.querySelector(`[name="${decodedId}"]`);

    if (targetEl) {
      targetEl.scrollIntoView({ behavior: "smooth", block: "start" });
    }
    return;
  }

  // 2. 本地文件与目录跳转 (file:// 或带有 .markdown-local-link 标记)
  const isLocal =
    href.startsWith("file://") ||
    target.classList.contains("markdown-local-link") ||
    target.hasAttribute("data-local-path") ||
    /^\/?[A-Za-z]:[\\/]/.test(href);

  if (isLocal) {
    e.preventDefault();
    e.stopPropagation();
    const localPath = normalizeLocalFilePath(href);
    if (!localPath) return;

    try {
      // 优先调用 Tauri 2 官方 opener 插件在系统资源管理器/默认程序中打开
      await tauriOpenPath(localPath);
    } catch (err) {
      console.warn("调用 tauriOpenPath 异常，尝试后端安全命令打开:", err);
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        await invoke("open_artifact_in_system", { path: localPath });
      } catch (invokeErr) {
        console.error("在系统中打开本地文件/目录失败:", invokeErr);
      }
    }
    return;
  }

  // 3. 外部网络网页链接 (HTTP / HTTPS / MAILTO)
  if (/^https?:\/\//i.test(href) || /^mailto:/i.test(href)) {
    e.preventDefault();
    e.stopPropagation();
    const title = target.getAttribute("title") || target.textContent?.trim() || "网页预览";
    openWebviewWindow(href, title);
  }
}

/**
 * 组合式 API：注册全应用级别的统一链接拦截器
 */
export function useGlobalLinkInterceptor() {
  onMounted(() => {
    // 在捕获阶段优先捕获动态渲染的 Markdown 链接点击
    window.addEventListener("click", handleGlobalLinkClick, true);
  });

  onUnmounted(() => {
    window.removeEventListener("click", handleGlobalLinkClick, true);
  });

  return {
    handleGlobalLinkClick,
    normalizeLocalFilePath,
  };
}
