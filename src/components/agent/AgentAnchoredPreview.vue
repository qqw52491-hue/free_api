<template>
    <Teleport to="body">
        <div v-if="details" class="preview-layer" :class="{ 'is-positioned': layout }">
            <div v-if="layout" class="preview-leader" :style="layout.leader" aria-hidden="true"></div>
            <div
                ref="card" :id="tooltipId" class="preview-card" :class="{ 'is-interactive': interactive }"
                :role="interactive ? 'dialog' : 'tooltip'" :aria-labelledby="interactive ? `${tooltipId}-title` : undefined"
                :style="layout?.card" @pointerenter="holdCard" @pointerleave="leave()" @focusin="holdCard" @focusout="blurCard"
            >
                <header>
                    <strong :id="`${tooltipId}-title`">{{ details.title }}</strong>
                    <span class="preview-state" :data-tone="details.tone">{{ details.status }}</span>
                    <button v-if="interactive" type="button" class="preview-close" aria-label="关闭步骤浮卡" @click="closeCard">×</button>
                </header>
                <div v-if="details.subtitle" class="preview-subtitle">{{ details.subtitle }}</div>
                <p v-if="details.description" class="preview-description" tabindex="0">{{ details.description }}</p>
                <dl v-if="details.rows.length">
                    <div v-for="row in details.rows" :key="row.label">
                        <dt>{{ row.label }}</dt>
                        <dd :class="row.kind && `preview-${row.kind}`" :data-tone="row.tone">{{ row.value }}</dd>
                    </div>
                </dl>
                <div v-if="details.sections?.length" :key="revision" class="preview-sections">
                    <AgentPreviewPayload v-for="section in details.sections" :key="section.id" :section="section" />
                </div>
            </div>
        </div>
    </Teleport>
</template>

<script lang="ts">
import type { AgentPreviewSection } from "./AgentPreviewPayload.vue";

export interface AgentPreviewDetails {
    title: string;
    status: string;
    tone: string;
    subtitle?: string;
    description?: string;
    rows: { label: string; value: string; tone?: string; kind?: "command" | "output" }[];
    sections?: AgentPreviewSection[];
}
</script>

<script setup lang="ts">
import { computed, onBeforeUnmount, ref, shallowRef, useId, watch } from "vue";
import AgentPreviewPayload from "./AgentPreviewPayload.vue";

const props = defineProps<{ interactive?: boolean; describe: (id: string, node: HTMLElement) => AgentPreviewDetails | undefined }>();
const tooltipId = `agent-anchor-preview-${useId()}`;
const card = ref<HTMLElement | null>(null);
const active = shallowRef<HTMLElement | null>(null);
const revision = ref(0);
const details = computed(() => {
    const node = active.value;
    const id = node?.dataset.previewId;
    return node && id ? props.describe(id, node) : undefined;
});
const layout = shallowRef<{
    card: { left: string; top: string; maxHeight?: string };
    leader: Record<string, string>;
    verticalSide?: "above" | "below";
} | null>(null);
type CursorPoint = { x: number; y: number };
let pending: HTMLElement | null = null;
let pendingPoint: CursorPoint | undefined;
let delay = 0;
let closeDelay = 0;
let frame = 0;
let nextPoint: CursorPoint | undefined;
let relativePoint: CursorPoint | undefined;
let restoringFocus: HTMLElement | null = null;

function show(node: HTMLElement, immediate = false, point?: CursorPoint) {
    if (node === restoringFocus) return;
    keepOpen();
    if (active.value === node) {
        cancelPending();
        if (point) move(node, point);
        return;
    }
    if (pending === node && !immediate) {
        if (point) pendingPoint = point;
        return;
    }
    cancelPending();
    pending = node;
    pendingPoint = point;
    const reveal = () => {
        const initialPoint = pendingPoint;
        pending = null;
        pendingPoint = undefined;
        delay = 0;
        if (!node.isConnected) {
            dismiss();
            return;
        }
        keepOpen();
        const focused = document.activeElement;
        clearActive();
        // Retire focus on the old controls before the next anchor owns the card.
        if (props.interactive && focused instanceof HTMLElement && insideCard(focused)) focused.blur();
        nextPoint = initialPoint;
        active.value = node;
        revision.value++;
        node.dataset.preview = "true";
        if (props.interactive) {
            node.setAttribute("aria-expanded", "true");
            node.setAttribute("aria-controls", tooltipId);
        } else node.setAttribute("aria-describedby", tooltipId);
    };
    if (immediate) reveal();
    else delay = window.setTimeout(reveal, 160);
}

function move(node: HTMLElement, point: CursorPoint) {
    if (pending === node) pendingPoint = point;
    else if (active.value === node) {
        nextPoint = point;
        schedulePosition();
    }
}

function dismiss(node?: HTMLElement) {
    if (node && node !== pending && node !== active.value) return;
    keepOpen();
    cancelPending();
    clearActive();
}

function cancelPending() {
    if (delay) window.clearTimeout(delay);
    delay = 0;
    pending = null;
    pendingPoint = undefined;
}

function clearActive() {
    if (active.value) {
        delete active.value.dataset.preview;
        if (props.interactive) {
            active.value.setAttribute("aria-expanded", "false");
            active.value.removeAttribute("aria-controls");
        }
        if (active.value.getAttribute("aria-describedby") === tooltipId) {
            active.value.removeAttribute("aria-describedby");
        }
    }
    active.value = null;
    layout.value = null;
    nextPoint = undefined;
    relativePoint = undefined;
}

function keepOpen() {
    if (closeDelay) window.clearTimeout(closeDelay);
    closeDelay = 0;
}

function holdCard() {
    keepOpen();
    cancelPending();
}

function leave(node?: HTMLElement) {
    if (node && node !== active.value && node !== pending) return;
    if (node === pending) cancelPending();
    if (!active.value || props.interactive && insideCard(document.activeElement)) return;
    keepOpen();
    closeDelay = window.setTimeout(() => dismiss(), 200);
}

function insideCard(target: EventTarget | null) {
    return target instanceof Node && Boolean(card.value?.contains(target));
}

function blurAnchor(node: HTMLElement, next: EventTarget | null) {
    if (!insideCard(next)) dismiss(node);
}

function blurCard(event: FocusEvent) {
    if (props.interactive && active.value && insideCard(event.target)
        && event.relatedTarget !== active.value && !insideCard(event.relatedTarget)) dismiss();
}

function closeCard() {
    const anchor = active.value;
    const restore = props.interactive && anchor?.isConnected && insideCard(document.activeElement);
    dismiss();
    if (restore && anchor) {
        restoringFocus = anchor;
        anchor.focus({ preventScroll: true });
        restoringFocus = null;
    }
}

function pointerOutside(event: PointerEvent) {
    const target = event.target;
    if (target instanceof Node && !insideCard(target) && !active.value?.contains(target)) dismiss();
}

const clamp = (value: number, min: number, max: number) => Math.min(Math.max(value, min), Math.max(min, max));
const px = (value: number) => `${Math.round(value)}px`;

function position() {
    frame = 0;
    const node = active.value;
    if (!node || !card.value) return;
    const box = node.getBoundingClientRect();
    const scrollBox = node.closest("[data-preview-viewport]")?.getBoundingClientRect();
    const viewport = document.documentElement;
    const width = viewport.clientWidth;
    const height = viewport.clientHeight;
    if (!node.isConnected || !box.width || !box.height || box.bottom <= Math.max(0, scrollBox?.top ?? 0)
        || box.top >= Math.min(height, scrollBox?.bottom ?? height)) {
        dismiss();
        return;
    }
    const { width: cardWidth, height: measuredHeight } = card.value.getBoundingClientRect();
    const margin = 14;
    const gap = 38;
    const point = node.querySelector("[data-preview-point]")?.getBoundingClientRect();
    if (nextPoint) {
        relativePoint = {
            x: clamp((nextPoint.x - box.left) / box.width, 0, 1),
            y: clamp((nextPoint.y - box.top) / box.height, 0, 1),
        };
        nextPoint = undefined;
    }
    const cursor = relativePoint ? { x: box.left + box.width * relativePoint.x, y: box.top + box.height * relativePoint.y } : undefined;
    const center = cursor?.y ?? (point ? point.top + point.height / 2 : box.top + box.height / 2);
    const align = point ?? node.querySelector("[data-preview-align]")?.getBoundingClientRect() ?? box;
    const alignX = cursor?.x ?? align.left + align.width / 2;
    const opensLeft = box.left >= cardWidth + gap + margin;
    const opensRight = width - box.right >= cardWidth + gap + margin;
    const below = height - box.bottom - margin;
    const above = box.top - margin;
    const verticalGap = 22;
    let opensBelow = below >= measuredHeight + verticalGap || below > above;
    let cardHeight = measuredHeight;
    let maxHeight: number | undefined;
    let verticalSide: "above" | "below" | undefined;
    if (props.interactive) {
        const viewportLimit = Math.max(0, Math.min(560, height - margin * 2));
        maxHeight = viewportLimit;
        if (!opensLeft && !opensRight) {
            const previousSide = layout.value?.verticalSide;
            const minimum = Math.min(160, viewportLimit);
            const previousRoom = (previousSide === "below" ? below : above) - verticalGap;
            if (previousSide && previousRoom >= minimum) opensBelow = previousSide === "below";
            verticalSide = opensBelow ? "below" : "above";
            maxHeight = clamp((opensBelow ? below : above) - verticalGap, minimum, viewportLimit);
        }
        cardHeight = Math.min(card.value.scrollHeight + card.value.offsetHeight - card.value.clientHeight, maxHeight);
    }

    let left: number, top: number, startX: number, startY: number, endX: number, endY: number;
    if (opensLeft || opensRight) {
        left = opensLeft ? box.left - gap - cardWidth : box.right + gap;
        top = clamp(center - cardHeight * 0.68, margin, height - cardHeight - margin);
        startX = opensLeft ? left + cardWidth : left;
        startY = top + cardHeight - 22;
        endX = opensLeft ? (point?.left ?? box.left) : (point?.right ?? box.right);
        endY = center;
    } else {
        left = clamp(alignX - cardWidth * 0.72, margin, width - cardWidth - margin);
        const desiredTop = opensBelow ? box.bottom + verticalGap : box.top - verticalGap - cardHeight;
        top = clamp(desiredTop, margin, height - cardHeight - margin);
        startX = clamp(alignX - 22, left + 20, left + cardWidth - 20);
        startY = opensBelow ? top : top + cardHeight;
        endX = alignX;
        endY = opensBelow ? (point?.bottom ?? box.bottom) : (point?.top ?? box.top);
    }
    if (cursor) {
        endX = cursor.x;
        endY = cursor.y;
    }
    const horizontal = endX - startX;
    const vertical = endY - startY;
    const next = {
        card: { left: px(left), top: px(top), maxHeight: maxHeight === undefined ? undefined : px(maxHeight) },
        verticalSide,
        leader: {
            left: `${startX}px`, top: `${startY - .5}px`,
            width: `${Math.hypot(horizontal, vertical)}px`,
            rotate: `${Math.atan2(vertical, horizontal) * 180 / Math.PI}deg`,
        },
    };
    const previous = layout.value;
    if (!previous || Object.entries(next.leader).some(([key, value]) => previous.leader[key] !== value)
        || previous.card.left !== next.card.left || previous.card.top !== next.card.top
        || previous.card.maxHeight !== next.card.maxHeight || previous.verticalSide !== next.verticalSide) layout.value = next;
}

function schedulePosition() {
    if (active.value && !frame) frame = window.requestAnimationFrame(position);
}

function followScroll(event: Event) {
    if (!insideCard(event.target)) schedulePosition();
}

watch([active, card], ([node, element], _, cleanup) => {
    if (!node || !element) return;
    const observer = typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(schedulePosition);
    observer?.observe(node);
    observer?.observe(element);
    const point = node.querySelector("[data-preview-point]");
    if (point) observer?.observe(point);
    window.addEventListener("scroll", followScroll, { capture: true, passive: true });
    window.addEventListener("resize", schedulePosition, { passive: true });
    window.addEventListener("keydown", handleKey, true);
    if (props.interactive) window.addEventListener("pointerdown", pointerOutside, true);
    schedulePosition();
    cleanup(() => {
        observer?.disconnect();
        window.removeEventListener("scroll", followScroll, true);
        window.removeEventListener("resize", schedulePosition);
        window.removeEventListener("keydown", handleKey, true);
        window.removeEventListener("pointerdown", pointerOutside, true);
        if (frame) window.cancelAnimationFrame(frame);
        frame = 0;
    });
}, { flush: "post" });
watch(details, (current) => {
    if (current) schedulePosition();
    else if (active.value) dismiss();
}, { flush: "post" });

function escape(event: KeyboardEvent) {
    if (event.key !== "Escape" || !(active.value || pending)) return;
    event.stopPropagation();
    closeCard();
}

function handleKey(event: KeyboardEvent) {
    if (props.interactive && event.key === "ArrowDown" && active.value === event.target) {
        const summary = card.value?.querySelector<HTMLElement>("summary");
        if (!summary) return;
        event.preventDefault();
        event.stopPropagation();
        summary.focus();
        return;
    }
    escape(event);
}

onBeforeUnmount(() => dismiss());
defineExpose({ show, move, leave, dismiss, escape, blurAnchor });
</script>

<style scoped>
.preview-layer {
    --preview-accent: var(--theme-status-info, #62baf4);
    --preview-edge: color-mix(in srgb, var(--preview-accent) 42%, var(--theme-border, #272d37));
    position: fixed;
    inset: 0;
    z-index: 45;
    pointer-events: none;
    visibility: hidden;
}
.preview-layer.is-positioned { visibility: visible; }
.preview-card {
    position: absolute;
    pointer-events: auto;
    box-sizing: border-box;
    width: min(306px, calc(100vw - 28px));
    max-height: calc(100vh - 28px);
    overflow: hidden;
    padding: 13px 14px 12px;
    border: 1px solid var(--preview-edge);
    border-radius: 9px;
    background: linear-gradient(145deg, color-mix(in srgb, var(--preview-accent) 8%, transparent), transparent 57%),
        var(--theme-bg-panel, #171a20);
    color: var(--theme-text-primary, #f3f4f6);
    box-shadow: 0 14px 34px #080b1461, inset 0 1px 0 #ffffff0d;
    font-size: 12px;
}
.preview-card header { display: flex; align-items: flex-start; gap: 8px; justify-content: space-between; }
.preview-card strong { display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 3; overflow: hidden; font-size: 13px; line-height: 1.42; overflow-wrap: anywhere; }
.preview-card.is-interactive {
    width: min(560px, calc(100vw - 28px));
    overflow: auto;
    overscroll-behavior: contain;
}
.preview-card.is-interactive strong { flex: 1; }
.preview-description {
    max-height: min(144px, 24vh);
    margin: 9px 0 0;
    padding: 8px 9px;
    overflow: auto;
    overscroll-behavior: contain;
    border-inline-start: 2px solid var(--preview-edge);
    background: color-mix(in srgb, var(--preview-accent) 5%, transparent);
    line-height: 1.55;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
}
.preview-close {
    flex: none;
    width: 20px;
    min-height: 20px;
    padding: 0;
    border: 0;
    border-radius: 4px;
    color: var(--theme-text-secondary, #a4a9b5);
    background: transparent;
    cursor: pointer;
    font-size: 16px;
    line-height: 1;
}
.preview-close:hover { background: rgba(var(--theme-foreground-rgb), 0.1); }
.preview-sections { display: grid; gap: 6px; margin-top: 10px; }
.preview-state {
    flex: none;
    padding: 2px 6px;
    border: 1px solid color-mix(in srgb, currentColor 32%, transparent);
    border-radius: 5px;
    font-size: 10px;
    line-height: 1.4;
    color: var(--theme-text-secondary, #a4a9b5);
}
.preview-state[data-tone="running"] { color: var(--preview-accent); }
[data-tone="waiting"], [data-tone="pending"] { color: var(--theme-status-warning, #d9a44d); }
[data-tone="succeeded"], [data-tone="collected"] { color: var(--theme-status-success, #36c295); }
[data-tone="failed"] { color: var(--theme-status-danger, #e77879); }
.preview-subtitle { margin-top: 3px; color: var(--theme-text-secondary, #a4a9b5); font-size: 10px; overflow-wrap: anywhere; }
.preview-card dl { display: grid; gap: 7px; margin: 10px 0 0; padding-top: 9px; border-top: 1px solid var(--theme-border, #272d37); }
.preview-card dl > div { display: grid; grid-template-columns: 64px minmax(0, 1fr); gap: 7px; line-height: 1.45; }
.preview-card dt { color: var(--theme-text-secondary, #a4a9b5); }
.preview-card dd { display: -webkit-box; -webkit-line-clamp: 3; -webkit-box-orient: vertical; overflow: hidden; margin: 0; overflow-wrap: anywhere; }
.preview-card dd.preview-command { -webkit-line-clamp: 7; white-space: pre-wrap; }
.preview-card dd.preview-output { -webkit-line-clamp: 10; white-space: pre-wrap; }
.preview-leader {
    position: absolute;
    height: 1px;
    background: linear-gradient(90deg, var(--preview-edge), var(--preview-accent));
    transform-origin: 0 50%;
}
.preview-leader::before, .preview-leader::after {
    content: "";
    position: absolute;
    top: 50%;
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--preview-accent);
    translate: -50% -50%;
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--preview-accent) 20%, transparent);
}
.preview-leader::before { left: 0; }
.preview-leader::after { left: 100%; }
</style>
