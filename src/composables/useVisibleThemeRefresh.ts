import { onBeforeUnmount, onMounted, watch, type Ref } from "vue";
import { useTheme } from "./useTheme";

// 切换颜色后先让界面恢复交互；昂贵图表逐个刷新，不同时重建整段历史。
const pending = new Map<symbol, () => void | Promise<void>>();
let scheduled = false;

function scheduleNext() {
  if (scheduled || pending.size === 0) return;
  scheduled = true;
  window.setTimeout(() => {
    const next = pending.entries().next();
    if (next.done) {
      scheduled = false;
      return;
    }
    const [id, refresh] = next.value;
    pending.delete(id);
    Promise.resolve().then(refresh).catch((error) => {
      console.warn("图表主题刷新失败:", error);
    }).finally(() => {
      scheduled = false;
      scheduleNext();
    });
  }, 80);
}

export function useVisibleThemeRefresh(
  element: Ref<HTMLElement | null>,
  refresh: () => void | Promise<void>,
) {
  const { theme } = useTheme();
  const id = Symbol("chart-theme");
  let renderedTheme = theme.value;
  let visible = false;
  let disposed = false;
  let observer: IntersectionObserver | undefined;

  function enqueue() {
    if (disposed || !visible || renderedTheme === theme.value) return;
    pending.set(id, async () => {
      const node = element.value;
      if (disposed || !visible || !node || renderedTheme === theme.value) return;
      // 等待期间可能已滚出视口或切换会话；v-show 隐藏的面板也不重建。
      const rect = node.getBoundingClientRect();
      if (rect.width === 0 || rect.height === 0 || rect.bottom <= 0 || rect.top >= innerHeight) return;
      const target = theme.value;
      await refresh();
      renderedTheme = target;
      enqueue();
    });
    scheduleNext();
  }

  watch(theme, enqueue, { flush: "post" });
  onMounted(() => {
    if (!element.value) return;
    if (typeof IntersectionObserver === "undefined") {
      visible = true;
      enqueue();
      return;
    }
    observer = new IntersectionObserver((entries) => {
      visible = entries.some((entry) => entry.isIntersecting);
      if (visible) enqueue();
      else pending.delete(id);
    });
    observer.observe(element.value);
  });
  onBeforeUnmount(() => {
    disposed = true;
    observer?.disconnect();
    pending.delete(id);
  });
}
