/**
 * 🌟 换行感知与动力学平滑尾随滚动指令 (v-scroll-tail)
 *
 * 核心特性：
 * 1. 挂载时从开头（0）渲染，当文本超出视口宽度时，平滑动态向右滚动至末尾；
 * 2. 动画速度与紧迫度动态计算：
 *    - 常规平滑滑动：采用平滑阻尼插值（lerp 0.22，保底 3.5px/帧），提供柔和连贯的滚动阅读感知；
 *    - 快速跟进/打断加速：若上一次滚动尚未结束便有新文本块（chunk/token）涌入，自动切换为加速追赶模式（lerp 0.50，保底 15px/帧）；
 *    - 连续打断暴走模式（极速到底）：连续多次打断且未到终点，自动提升至 lerp 0.85，保底 35px/帧，1~2 帧内直达尾部，杜绝严重滞后；
 * 3. 兼容单体内置滚动与父容器溢出双重结构，在滚动偏移 > 6px 时自动附加 has-left-overflow 渐隐遮罩；
 * 4. 完美配合 Vue Transition (ticker-flip)，换行重置与行内追随互不干扰，组件卸载自动清理 rAF 避免内存泄漏。
 */

interface ScrollTailState {
    current: number;
    target: number;
    rafId: number | null;
    isAccelerated: boolean;
    interruptCount: number;
    unmounted: boolean;
    scrollEl: HTMLElement;
}

const scrollStateMap = new WeakMap<HTMLElement, ScrollTailState>();

function resolveScrollElement(el: HTMLElement): HTMLElement {
    if (el.scrollWidth > el.clientWidth) {
        return el;
    }
    const parent = el.parentElement;
    if (parent && parent.scrollWidth > parent.clientWidth) {
        return parent;
    }
    return el;
}

function updateOverflowClasses(targetEl: HTMLElement, hostEl: HTMLElement, pos: number) {
    const hasOverflow = pos > 6;
    targetEl.classList.toggle("has-left-overflow", hasOverflow);
    if (hostEl !== targetEl) {
        hostEl.classList.toggle("has-left-overflow", hasOverflow);
    }
}

export function animateScrollTail(hostEl: HTMLElement) {
    if (!hostEl.isConnected) return;
    let state = scrollStateMap.get(hostEl);
    if (!hostEl.getClientRects().length) {
        if (state?.rafId != null) cancelAnimationFrame(state.rafId);
        if (state) state.rafId = null;
        return;
    }
    if (!state) {
        const scrollEl = resolveScrollElement(hostEl);
        state = {
            current: scrollEl.scrollLeft,
            target: 0,
            rafId: null,
            isAccelerated: false,
            interruptCount: 0,
            unmounted: false,
            scrollEl,
        };
        scrollStateMap.set(hostEl, state);
    }

    if (state.unmounted) return;

    // 重新解析可能的滚动目标（在挂载或内容变更后尺寸可能更新）
    state.scrollEl = resolveScrollElement(hostEl);
    const scrollEl = state.scrollEl;
    const maxScroll = Math.max(0, scrollEl.scrollWidth - scrollEl.clientWidth);

    // 🌟 用户核心诉求：“如果没滚动完成下一个来了就加快速度 直接到底部”
    const remainingBefore = state.target - state.current;
    if (state.rafId !== null && remainingBefore > 4 && maxScroll > state.target) {
        state.isAccelerated = true;
        state.interruptCount = (state.interruptCount || 0) + 1;
    }

    state.target = maxScroll;

    // 如果动画帧正在执行中，由于 state.target、isAccelerated 与 interruptCount 已被更新，
    // 下一个 tick 循环会自动按新目标和加速动力学推进
    if (state.rafId !== null) {
        return;
    }

    // 若已经处于目标位置，直接核销并保持对齐
    if (Math.abs(state.target - state.current) <= 0.8) {
        state.current = state.target;
        scrollEl.scrollLeft = Math.round(state.current);
        updateOverflowClasses(scrollEl, hostEl, state.current);
        state.isAccelerated = false;
        state.interruptCount = 0;
        return;
    }

    function tick() {
        if (!state || state.unmounted || !state.scrollEl.isConnected) {
            if (state) state.rafId = null;
            return;
        }
        if (!hostEl.getClientRects().length) {
            state.rafId = null;
            return;
        }

        const el = state.scrollEl;
        const remaining = state.target - state.current;

        // 已经到终点，或距离极小（<= 0.8px）
        if (Math.abs(remaining) <= 0.8 || state.target === 0) {
            state.current = state.target;
            el.scrollLeft = Math.round(state.current);
            updateOverflowClasses(el, hostEl, state.current);
            state.rafId = null;
            state.isAccelerated = false;
            state.interruptCount = 0;
            return;
        }

        // 🌟 动态计算滚动步长与加速度（限速追赶版）：
        // 1. 常规平滑速率：lerp 0.22，3.5px 起步，封顶 16px/帧——看得见在滚；
        // 2. 打断加速：lerp 0.50，15px 起步，封顶 42px/帧；
        // 3. 连续打断暴走（“没滚完下一个来了就加速”）：lerp 0.85、35px 起步、
        //    封顶 90px/帧——飞快掠过但仍是滚动不是瞬移；
        // 4. >200px 大跨度：提高速度基底
        let lerp = 0.22;
        let minSpeed = 3.5;
        let maxSpeed = 16;

        if (state.interruptCount >= 2) {
            lerp = 0.85;
            minSpeed = 35;
            maxSpeed = 90;
        } else if (state.isAccelerated) {
            lerp = 0.50;
            minSpeed = 15;
            maxSpeed = 42;
        }

        if (remaining > 200) {
            lerp = Math.max(lerp, 0.70);
            minSpeed = Math.max(minSpeed, 25);
            maxSpeed = Math.max(maxSpeed, 60);
        }

        let delta = remaining * lerp;
        if (delta > 0) {
            delta = Math.min(Math.max(delta, minSpeed), maxSpeed);
        } else if (delta < 0) {
            delta = Math.max(Math.min(delta, -minSpeed), -maxSpeed);
        }

        state.current += delta;
        // 防止过冲
        if (remaining > 0 && state.current > state.target) {
            state.current = state.target;
        } else if (remaining < 0 && state.current < state.target) {
            state.current = state.target;
        }

        el.scrollLeft = Math.round(state.current);
        updateOverflowClasses(el, hostEl, state.current);

        if (Math.abs(state.target - state.current) > 0.8) {
            state.rafId = requestAnimationFrame(tick);
        } else {
            state.current = state.target;
            el.scrollLeft = Math.round(state.current);
            state.rafId = null;
            state.isAccelerated = false;
            state.interruptCount = 0;
        }
    }

    state.rafId = requestAnimationFrame(tick);
}

export const vScrollTail = {
    mounted(el: HTMLElement) {
        const scrollEl = resolveScrollElement(el);
        scrollEl.scrollLeft = 0;
        updateOverflowClasses(scrollEl, el, 0);

        scrollStateMap.set(el, {
            current: 0,
            target: 0,
            rafId: null,
            isAccelerated: false,
            interruptCount: 0,
            unmounted: false,
            scrollEl,
        });

        requestAnimationFrame(() => {
            animateScrollTail(el);
        });
    },
    updated(el: HTMLElement) {
        animateScrollTail(el);
    },
    unmounted(el: HTMLElement) {
        const state = scrollStateMap.get(el);
        if (state) {
            state.unmounted = true;
            if (state.rafId !== null) {
                cancelAnimationFrame(state.rafId);
                state.rafId = null;
            }
            scrollStateMap.delete(el);
        }
    },
};
