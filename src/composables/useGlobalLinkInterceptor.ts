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
 * 查找元素所属的最近独立滚动容器（如 .canvas-scroll-content 或 overflow-y 为 auto/scroll 的容器）
 */
export function findScrollContainer(el: HTMLElement): HTMLElement | null {
  let parent = el.parentElement;
  while (parent && parent !== document.body && parent !== document.documentElement) {
    const style = window.getComputedStyle(parent);
    const overflowY = style.overflowY;
    if (overflowY === "auto" || overflowY === "scroll") {
      return parent;
    }
    parent = parent.parentElement;
  }
  return null;
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
    const cleanSlugId = slugId.replace(/^-+|-+$/g, "");

    // 优先从触发点击的当前就近容器中寻找锚点（如右侧画布 .canvas-scroll-content 或独立段落），
    // 避免因为左右分栏存在重复文档 ID 而错误定位到左侧对话流中
    const localScope =
      target.closest(".canvas-scroll-content") ||
      target.closest(".manus-inspector") ||
      target.closest(".rich-content-stream") ||
      target.closest(".markdown-segment");

    const findTargetIn = (scope: ParentNode): HTMLElement | null => {
      // 1.1 精准属性与 slug 匹配
      const candidates = [rawId, decodedId, slugId, cleanSlugId].filter(Boolean);
      for (const cand of candidates) {
        try {
          const el = scope.querySelector<HTMLElement>(`[id="${CSS.escape(cand)}"], [name="${CSS.escape(cand)}"]`);
          if (el) return el;
        } catch {
          // ignore selector escape errors
        }
      }

      // 1.2 标题文字容错兜底（针对模型生成 TOC 时的细微标点/空格差异）
      const headings = scope.querySelectorAll<HTMLElement>("h1, h2, h3, h4, h5, h6");
      for (const h of headings) {
        const text = (h.getAttribute("data-heading-text") || h.textContent || "").trim();
        if (!text) continue;
        if (text === decodedId || text === rawId) return h;
        const hSlug = text.toLowerCase().replace(/[^\w\u4e00-\u9fa5]+/g, "-").replace(/^-+|-+$/g, "");
        if (hSlug && (hSlug === cleanSlugId || hSlug === slugId)) return h;
      }
      return null;
    };

    const targetEl = (localScope ? findTargetIn(localScope) : null) || findTargetIn(document);

    if (targetEl) {
      const scrollParent = findScrollContainer(targetEl);
      if (scrollParent && scrollParent !== document.body && scrollParent !== document.documentElement) {
        const parentRect = scrollParent.getBoundingClientRect();
        const elRect = targetEl.getBoundingClientRect();
        const top = scrollParent.scrollTop + (elRect.top - parentRect.top);
        scrollParent.scrollTo({ top: Math.max(0, top - 12), behavior: "smooth" });
      } else {
        targetEl.scrollIntoView({ behavior: "smooth", block: "start" });
      }
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
