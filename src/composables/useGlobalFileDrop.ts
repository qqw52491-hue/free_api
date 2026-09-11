import { ref, onMounted, onUnmounted } from "vue";

export interface DragDropOptions {
  onDropPaths: (paths: string[]) => void | Promise<void>;
  onHoverChange?: (isOver: boolean, position?: { x: number; y: number }) => void;
}

export interface DraggedPathItem {
  path: string;
  name: string;
  is_dir: boolean;
  size_bytes: number;
  exists: boolean;
}

/**
 * 全局桌面端与浏览器端统一文件拖拽捕获 Composable
 * 
 * 核心特性：
 * 1. 接管 Tauri 2 原生 `getCurrentWebview().onDragDropEvent`，获取操作系统真实绝对路径；
 * 2. 彻底拦截全局浏览器的默认 `dragover` / `drop` 跳转行为，防止 Webview 误当成 URL 打开；
 * 3. 兼容浏览器测试环境降级，自动清理事件监听。
 */
export function useGlobalFileDrop(options: DragDropOptions) {
  const isDraggingOver = ref(false);
  const hoverPosition = ref<{ x: number; y: number } | null>(null);

  let unlistenTauri: (() => void) | null = null;
  let dragLeaveTimer: ReturnType<typeof setTimeout> | null = null;

  const preventWindowDefaults = (e: DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
  };

  const handleWindowDragOver = (e: DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (e.dataTransfer) {
      e.dataTransfer.dropEffect = "copy";
    }
  };

  onMounted(async () => {
    // 1. 全局拦截浏览器默认导航行为
    window.addEventListener("dragover", handleWindowDragOver, false);
    window.addEventListener("drop", preventWindowDefaults, false);

    // 2. 尝试挂载 Tauri 2 原生拖拽事件
    try {
      const { getCurrentWebview } = await import("@tauri-apps/api/webview");
      const webview = getCurrentWebview();
      if (webview && typeof webview.onDragDropEvent === "function") {
        unlistenTauri = await webview.onDragDropEvent((event) => {
          if (dragLeaveTimer) {
            clearTimeout(dragLeaveTimer);
            dragLeaveTimer = null;
          }

          if (event.payload.type === "enter" || event.payload.type === "over") {
            isDraggingOver.value = true;
            if ("position" in event.payload && event.payload.position) {
              hoverPosition.value = {
                x: event.payload.position.x,
                y: event.payload.position.y,
              };
            }
            options.onHoverChange?.(true, hoverPosition.value ?? undefined);
          } else if (event.payload.type === "leave") {
            // 防抖 80ms 避免快速滑过时闪烁
            dragLeaveTimer = setTimeout(() => {
              isDraggingOver.value = false;
              hoverPosition.value = null;
              options.onHoverChange?.(false);
            }, 80);
          } else if (event.payload.type === "drop") {
            isDraggingOver.value = false;
            hoverPosition.value = null;
            options.onHoverChange?.(false);
            const paths = event.payload.paths || [];
            if (paths.length > 0) {
              options.onDropPaths(paths);
            }
          }
        });
        return;
      }
    } catch (e) {
      console.debug("Tauri webview onDragDropEvent not available, fallback to HTML5 drop", e);
    }

    // 3. 浏览器降级监听 (当在纯 Web 环境开发测试时)
    const handleHtml5DragEnter = (e: DragEvent) => {
      e.preventDefault();
      if (e.dataTransfer && Array.from(e.dataTransfer.types).includes("Files")) {
        isDraggingOver.value = true;
        options.onHoverChange?.(true);
      }
    };

    const handleHtml5DragLeave = (e: DragEvent) => {
      e.preventDefault();
      // 如果离开到窗口外
      if (e.relatedTarget === null || (e.clientX <= 0 && e.clientY <= 0)) {
        isDraggingOver.value = false;
        options.onHoverChange?.(false);
      }
    };

    const handleHtml5Drop = (e: DragEvent) => {
      e.preventDefault();
      e.stopPropagation();
      isDraggingOver.value = false;
      options.onHoverChange?.(false);
      // HTML5 环境下通常拿不到真实系统路径，仅能作为开发降级通知
      const files = e.dataTransfer?.files;
      if (files && files.length > 0) {
        console.warn("浏览器环境无法读取绝对系统路径，请在 Tauri 桌面端运行测试拖拽");
      }
    };

    window.addEventListener("dragenter", handleHtml5DragEnter);
    window.addEventListener("dragleave", handleHtml5DragLeave);
    window.addEventListener("drop", handleHtml5Drop);

    unlistenTauri = () => {
      window.removeEventListener("dragenter", handleHtml5DragEnter);
      window.removeEventListener("dragleave", handleHtml5DragLeave);
      window.removeEventListener("drop", handleHtml5Drop);
    };
  });

  onUnmounted(() => {
    window.removeEventListener("dragover", handleWindowDragOver);
    window.removeEventListener("drop", preventWindowDefaults);
    if (dragLeaveTimer) {
      clearTimeout(dragLeaveTimer);
    }
    if (unlistenTauri) {
      unlistenTauri();
      unlistenTauri = null;
    }
  });

  return {
    isDraggingOver,
    hoverPosition,
  };
}
