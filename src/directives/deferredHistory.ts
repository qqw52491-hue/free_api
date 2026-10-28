import type { ObjectDirective } from "vue";

type HistoryNode = {
  visibility: string;
  intrinsicSize: string;
  applied: boolean;
};

const nodes = new WeakMap<HTMLElement, HistoryNode>();
const pending = new Set<HTMLElement>();
let timer: number | undefined;
let resizeObserver: ResizeObserver | undefined;
let supported: boolean | undefined;

function schedule() {
  if (timer !== undefined || !pending.size) return;
  timer = window.setTimeout(flush, 32);
}

function flush() {
  timer = undefined;
  const measured: Array<{ element: HTMLElement; width: number; height: number }> = [];
  // 批量读完尺寸再写样式，避免每条消息交替触发布局；分批给输入和绘制留时间。
  for (const element of Array.from(pending).slice(0, 16)) {
    pending.delete(element);
    const state = nodes.get(element);
    if (!state || state.applied || !element.isConnected || !element.clientWidth || !element.clientHeight) continue;
    const style = getComputedStyle(element);
    const rect = element.getBoundingClientRect();
    const pixels = (value: string) => Number.parseFloat(value) || 0;
    measured.push({
      element,
      width: Math.max(0, rect.width - pixels(style.paddingLeft) - pixels(style.paddingRight) - pixels(style.borderLeftWidth) - pixels(style.borderRightWidth)),
      height: Math.max(0, rect.height - pixels(style.paddingTop) - pixels(style.paddingBottom) - pixels(style.borderTopWidth) - pixels(style.borderBottomWidth)),
    });
  }
  for (const { element, width, height } of measured) {
    const state = nodes.get(element);
    if (!state || state.applied) continue;
    // 使用实际内容尺寸而非统一估高；auto 会记住后续可见时的真实尺寸。
    element.style.setProperty("contain-intrinsic-size", `auto ${width}px auto ${height}px`);
    element.style.setProperty("content-visibility", "auto");
    state.applied = true;
    resizeObserver?.unobserve(element);
  }
  schedule();
}

function enable(element: HTMLElement) {
  if (supported === undefined) {
    supported = typeof CSS !== "undefined"
      && CSS.supports("content-visibility", "auto")
      && CSS.supports("contain-intrinsic-size", "auto 1px auto 1px");
  }
  if (!supported || nodes.has(element)) return;
  nodes.set(element, {
    visibility: element.style.getPropertyValue("content-visibility"),
    intrinsicSize: element.style.getPropertyValue("contain-intrinsic-size"),
    applied: false,
  });
  if (!resizeObserver && typeof ResizeObserver !== "undefined") {
    resizeObserver = new ResizeObserver((entries) => {
      for (const entry of entries) {
        const node = entry.target as HTMLElement;
        if (entry.contentRect.width && entry.contentRect.height && nodes.has(node) && !nodes.get(node)?.applied) pending.add(node);
      }
      schedule();
    });
  }
  // v-show 隐藏的会话不先测量，恢复可见后由尺寸通知再处理。
  resizeObserver?.observe(element);
  pending.add(element);
  schedule();
}

function disable(element: HTMLElement) {
  const state = nodes.get(element);
  if (!state) return;
  pending.delete(element);
  resizeObserver?.unobserve(element);
  if (state.applied) {
    if (state.visibility) element.style.setProperty("content-visibility", state.visibility);
    else element.style.removeProperty("content-visibility");
    if (state.intrinsicSize) element.style.setProperty("contain-intrinsic-size", state.intrinsicSize);
    else element.style.removeProperty("contain-intrinsic-size");
  }
  nodes.delete(element);
}

/** 仅用于已完成的历史正文；不卸载 DOM，不修改 Markdown，也不限制可见内容。 */
export const vDeferredHistory: ObjectDirective<HTMLElement, boolean> = {
  mounted(element, binding) { if (binding.value) enable(element); },
  updated(element, binding) {
    if (binding.value) enable(element);
    else disable(element);
  },
  beforeUnmount: disable,
};
