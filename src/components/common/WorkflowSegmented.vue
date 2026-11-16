<template>
  <div
    ref="rootRef"
    class="wf-seg"
    :class="[`is-${variant}`, { 'is-compact': compact, 'is-disabled': disabled }]"
    @click="onRootClick"
  >
    <div
      v-if="variant === 'slider'"
      ref="trackRef"
      class="wf-seg-track"
      :class="{ 'has-thumb': thumbReady }"
      :style="{ '--wf-cols': String(Math.max(1, options.length)) }"
      role="radiogroup"
      @keydown="onGroupKey"
    >
      <span
        class="wf-seg-thumb"
        :class="activeToneClass"
        :style="thumbStyle"
        aria-hidden="true"
      ></span>
      <button
        v-for="(opt, idx) in options"
        :key="opt.value"
        :ref="el => setBtnRef(idx, el)"
        type="button"
        role="radio"
        class="wf-seg-btn"
        :class="[toneClass(opt), { 'is-active': opt.value === modelValue }]"
        :aria-checked="opt.value === modelValue"
        :disabled="disabled || opt.disabled"
        :tabindex="isTabTarget(idx) ? 0 : -1"
        :title="opt.title || opt.optionLabel || opt.label"
        @click="choose($event, opt)"
      >
        <span class="wf-seg-dot" aria-hidden="true"></span>
        <span class="wf-seg-label">{{ opt.label }}</span>
      </button>
    </div>

    <div
      v-else
      class="wf-pills-group"
      role="radiogroup"
      @keydown="onGroupKey"
    >
      <button
        v-for="(opt, idx) in options"
        :key="opt.value"
        :ref="el => setBtnRef(idx, el)"
        type="button"
        role="radio"
        class="wf-pill-btn"
        :class="[toneClass(opt), { 'is-active': opt.value === modelValue }]"
        :aria-checked="opt.value === modelValue"
        :disabled="disabled || opt.disabled"
        :tabindex="isTabTarget(idx) ? 0 : -1"
        :title="opt.title || opt.optionLabel || opt.label"
        @click="choose($event, opt)"
      >
        <span class="wf-pill-dot" aria-hidden="true"></span>
        <span class="wf-pill-label">{{ opt.label }}</span>
      </button>
    </div>

    <select
      ref="selectRef"
      class="wf-native-select"
      :value="modelValue"
      :disabled="disabled"
      tabindex="-1"
      aria-hidden="true"
      @change.stop="onNativeChange"
    >
      <option v-for="opt in options" :key="opt.value" :value="opt.value" :disabled="opt.disabled">
        {{ opt.optionLabel || opt.label }}
      </option>
    </select>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch, type ComponentPublicInstance } from 'vue'

export interface WorkflowSegmentedOption {
  value: string
  label: string
  optionLabel?: string
  title?: string
  tone?: 'auto' | 'on' | 'off' | 'accent'
  disabled?: boolean
}

const props = withDefaults(defineProps<{
  modelValue?: string
  options: WorkflowSegmentedOption[]
  disabled?: boolean
  variant?: 'slider' | 'pills'
  compact?: boolean
}>(), {
  modelValue: '',
  disabled: false,
  variant: 'slider',
  compact: false,
})

const emit = defineEmits<{
  (e: 'update:modelValue', value: string): void
  (e: 'change', event: Event): void
}>()

const rootRef = ref<HTMLElement | null>(null)
const trackRef = ref<HTMLElement | null>(null)
const selectRef = ref<HTMLSelectElement | null>(null)
const btnRefs = ref<(HTMLButtonElement | null)[]>([])
const thumbBox = ref<{ left: number; top: number; width: number; height: number } | null>(null)
let resizeObserver: ResizeObserver | null = null

function setBtnRef(idx: number, el: Element | ComponentPublicInstance | null) {
  btnRefs.value[idx] = el instanceof HTMLButtonElement ? el : null
}

function toneClass(opt: WorkflowSegmentedOption | undefined) {
  return `tone-${opt?.tone || 'auto'}`
}

const activeOption = computed(() => props.options.find(o => o.value === props.modelValue))
const activeToneClass = computed(() => toneClass(activeOption.value))
const thumbReady = computed(() => thumbBox.value !== null && thumbBox.value.width > 0)
const thumbStyle = computed(() => {
  if (!thumbBox.value || thumbBox.value.width <= 0) return { opacity: '0' }
  return {
    width: `${thumbBox.value.width}px`,
    height: `${thumbBox.value.height}px`,
    transform: `translate3d(${thumbBox.value.left}px, ${thumbBox.value.top}px, 0)`,
    opacity: '1',
  }
})

function isTabTarget(idx: number) {
  const current = props.options.findIndex(o => o.value === props.modelValue && !o.disabled)
  if (current >= 0) return idx === current
  const firstEnabled = props.options.findIndex(o => !o.disabled)
  return idx === (firstEnabled >= 0 ? firstEnabled : 0)
}

function syncThumb() {
  if (props.variant !== 'slider' || !trackRef.value) {
    thumbBox.value = null
    return
  }
  const idx = props.options.findIndex(o => o.value === props.modelValue)
  const btn = idx >= 0 ? btnRefs.value[idx] : null
  if (!btn || btn.offsetWidth <= 0 || btn.offsetHeight <= 0) {
    thumbBox.value = null
    return
  }
  thumbBox.value = {
    left: btn.offsetLeft,
    top: btn.offsetTop,
    width: btn.offsetWidth,
    height: btn.offsetHeight,
  }
}

function onRootClick(e: MouseEvent) {
  if (!(e.target as HTMLElement | null)?.closest('button')) {
    e.preventDefault()
  }
}

function choose(e: MouseEvent, opt: WorkflowSegmentedOption) {
  const btn = e.currentTarget as HTMLButtonElement | null
  if (props.disabled || opt.disabled || btn?.matches(':disabled')) return
  if (opt.value === props.modelValue) return
  if (selectRef.value) {
    selectRef.value.value = opt.value
    selectRef.value.dispatchEvent(new Event('change', { bubbles: true }))
  }
}

function onNativeChange(e: Event) {
  const select = e.target as HTMLSelectElement
  const next = select.value
  emit('update:modelValue', next)
  emit('change', e)
  nextTick(() => {
    if (selectRef.value && selectRef.value.value !== (props.modelValue ?? '')) {
      selectRef.value.value = props.modelValue ?? ''
    }
    syncThumb()
  })
}

function onGroupKey(e: KeyboardEvent) {
  const step = (e.key === 'ArrowRight' || e.key === 'ArrowDown') ? 1 : (e.key === 'ArrowLeft' || e.key === 'ArrowUp') ? -1 : 0
  if (!step) return
  const activeEl = document.activeElement
  const currentIdx = btnRefs.value.findIndex(b => b === activeEl)
  if (currentIdx < 0) return
  e.preventDefault()
  const n = props.options.length
  for (let k = 1; k <= n; k++) {
    const nextIdx = (currentIdx + step * k + n) % n
    const nextBtn = btnRefs.value[nextIdx]
    if (nextBtn && !nextBtn.disabled && !nextBtn.matches(':disabled')) {
      nextBtn.focus()
      return
    }
  }
}

watch(
  [() => props.modelValue, () => props.options, () => props.variant],
  async () => {
    await nextTick()
    syncThumb()
  },
  { flush: 'post', deep: true }
)

watch(
  () => props.modelValue,
  val => {
    if (selectRef.value && selectRef.value.value !== (val ?? '')) {
      selectRef.value.value = val ?? ''
    }
  }
)

onMounted(() => {
  syncThumb()
  if (typeof ResizeObserver !== 'undefined' && trackRef.value) {
    resizeObserver = new ResizeObserver(() => syncThumb())
    resizeObserver.observe(trackRef.value)
  }
})

onUnmounted(() => {
  resizeObserver?.disconnect()
  resizeObserver = null
})
</script>

<style scoped>
.wf-seg {
  position: relative;
  display: block;
  width: 100%;
  min-width: 0;
  font-family: var(--font, sans-serif);
}

.wf-seg > .wf-native-select {
  position: absolute;
  inset: 0;
  width: 1px;
  min-width: 0;
  height: 1px;
  min-height: 0;
  padding: 0;
  margin: 0;
  border: 0;
  opacity: 0;
  pointer-events: none;
  overflow: hidden;
  clip: rect(0, 0, 0, 0);
}

/* ---- Slider Variant ---- */
.wf-seg-track {
  position: relative;
  display: grid;
  grid-template-columns: repeat(var(--wf-cols, 2), minmax(0, 1fr));
  gap: 3px;
  padding: 3px;
  border-radius: 10px;
  background: var(--theme-inset-bg, rgba(0, 0, 0, 0.3));
  border: 1px solid rgba(var(--theme-foreground-rgb, 255, 255, 255), 0.1);
  box-shadow: inset 0 1px 3px rgba(0, 0, 0, 0.22);
}

.wf-seg-thumb {
  position: absolute;
  top: 0;
  left: 0;
  border-radius: 7px;
  border: 1px solid transparent;
  pointer-events: none;
  transition:
    transform var(--motion-normal, 180ms) var(--motion-ease, cubic-bezier(.2, .8, .2, 1)),
    width var(--motion-normal, 180ms) var(--motion-ease, cubic-bezier(.2, .8, .2, 1)),
    height var(--motion-normal, 180ms) var(--motion-ease, cubic-bezier(.2, .8, .2, 1)),
    background-color .18s ease,
    border-color .18s ease,
    box-shadow .18s ease,
    opacity .12s ease;
  box-sizing: border-box;
}

.wf-seg-thumb.tone-auto {
  background: linear-gradient(135deg, rgba(148, 163, 184, 0.2) 0%, rgba(129, 140, 248, 0.14) 100%);
  border-color: rgba(148, 163, 184, 0.36);
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.18);
}

.wf-seg-thumb.tone-accent {
  background: linear-gradient(135deg, rgba(99, 102, 241, 0.28) 0%, rgba(124, 58, 237, 0.16) 100%);
  border-color: rgba(129, 140, 248, 0.56);
  box-shadow: 0 2px 12px rgba(99, 102, 241, 0.22);
}

.wf-seg-thumb.tone-on {
  background: linear-gradient(135deg, rgba(16, 185, 129, 0.26) 0%, rgba(52, 211, 153, 0.14) 100%);
  border-color: rgba(52, 211, 153, 0.52);
  box-shadow: 0 2px 10px rgba(16, 185, 129, 0.18);
}

.wf-seg-thumb.tone-off {
  background: linear-gradient(135deg, rgba(245, 158, 11, 0.24) 0%, rgba(251, 191, 36, 0.12) 100%);
  border-color: rgba(245, 158, 11, 0.5);
  box-shadow: 0 2px 10px rgba(245, 158, 11, 0.16);
}

.wf-seg-btn {
  position: relative;
  z-index: 1;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  min-width: 0;
  min-height: 32px;
  padding: 6px 10px;
  border: 1px solid transparent;
  border-radius: 7px;
  background: transparent;
  color: var(--theme-text-muted, #9ca3af);
  font: inherit;
  font-size: 12.5px;
  font-weight: 500;
  line-height: 1.3;
  text-align: center;
  cursor: pointer;
  outline: none;
  user-select: none;
  transition: color .16s ease, background-color .16s ease, border-color .16s ease;
}

.wf-seg.is-compact .wf-seg-btn {
  min-height: 29px;
  padding: 5px 6px;
  gap: 5px;
  font-size: 12px;
}

.wf-seg-btn:hover:not(:disabled):not(.is-active) {
  color: var(--theme-text-primary, #f4f4f5);
  background: rgba(var(--theme-foreground-rgb, 255, 255, 255), 0.05);
}

.wf-seg-btn.is-active {
  color: var(--theme-text-primary, #f4f4f5);
  font-weight: 600;
}

/* Fallback active fill when thumb hasn't measured yet */
.wf-seg-track:not(.has-thumb) .wf-seg-btn.is-active.tone-auto {
  background: rgba(148, 163, 184, 0.18);
  border-color: rgba(148, 163, 184, 0.35);
}
.wf-seg-track:not(.has-thumb) .wf-seg-btn.is-active.tone-accent {
  background: rgba(99, 102, 241, 0.24);
  border-color: rgba(129, 140, 248, 0.5);
}
.wf-seg-track:not(.has-thumb) .wf-seg-btn.is-active.tone-on {
  background: rgba(16, 185, 129, 0.22);
  border-color: rgba(52, 211, 153, 0.45);
}
.wf-seg-track:not(.has-thumb) .wf-seg-btn.is-active.tone-off {
  background: rgba(245, 158, 11, 0.22);
  border-color: rgba(245, 158, 11, 0.45);
}

.wf-seg-btn:focus-visible {
  box-shadow: 0 0 0 2px var(--accent-glow, rgba(108, 99, 255, 0.35));
}

.wf-seg-btn:disabled {
  opacity: .42;
  cursor: default;
}

.wf-seg-dot,
.wf-pill-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  flex-shrink: 0;
  background: rgba(var(--theme-foreground-rgb, 255, 255, 255), 0.26);
  transition: background-color .16s ease, box-shadow .16s ease, transform .16s ease;
}

.is-active.tone-auto .wf-seg-dot,
.is-active.tone-auto .wf-pill-dot {
  background: rgba(var(--theme-foreground-rgb, 255, 255, 255), 0.65);
}
.is-active.tone-accent .wf-seg-dot,
.is-active.tone-accent .wf-pill-dot {
  background: var(--accent-light, #a5b4fc);
  box-shadow: 0 0 8px rgba(129, 140, 248, 0.65);
}
.is-active.tone-on .wf-seg-dot,
.is-active.tone-on .wf-pill-dot {
  background: var(--theme-status-success, #34d399);
  box-shadow: 0 0 8px rgba(52, 211, 153, 0.6);
}
.is-active.tone-off .wf-seg-dot,
.is-active.tone-off .wf-pill-dot {
  background: var(--theme-status-warning, #f59e0b);
  box-shadow: 0 0 8px rgba(245, 158, 11, 0.55);
}

.wf-seg-label {
  min-width: 0;
  word-break: keep-all;
}

/* ---- Pills Variant ---- */
.wf-pills-group {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.wf-pill-btn {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  padding: 7px 13px;
  border-radius: 999px;
  background: rgba(var(--theme-foreground-rgb, 255, 255, 255), 0.03);
  border: 1px solid rgba(var(--theme-foreground-rgb, 255, 255, 255), 0.11);
  color: var(--theme-text-secondary, #a1a1aa);
  font: inherit;
  font-size: 12.5px;
  font-weight: 500;
  line-height: 1.3;
  cursor: pointer;
  outline: none;
  user-select: none;
  transition: background-color .16s ease, border-color .16s ease, color .16s ease, box-shadow .16s ease, transform .16s ease;
}

.wf-pill-btn:hover:not(:disabled):not(.is-active) {
  background: rgba(var(--theme-foreground-rgb, 255, 255, 255), 0.07);
  border-color: rgba(129, 140, 248, 0.42);
  color: var(--theme-text-primary, #f4f4f5);
  transform: translateY(-1px);
}

.wf-pill-btn.is-active {
  color: var(--theme-text-primary, #f4f4f5);
  font-weight: 600;
}

.wf-pill-btn.is-active.tone-auto {
  background: linear-gradient(135deg, rgba(148, 163, 184, 0.2) 0%, rgba(129, 140, 248, 0.12) 100%);
  border-color: rgba(148, 163, 184, 0.45);
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.16);
}

.wf-pill-btn.is-active.tone-accent {
  background: linear-gradient(135deg, rgba(99, 102, 241, 0.24) 0%, rgba(124, 58, 237, 0.14) 100%);
  border-color: rgba(129, 140, 248, 0.6);
  box-shadow: 0 3px 12px rgba(99, 102, 241, 0.2);
}

.wf-pill-btn:focus-visible {
  box-shadow: 0 0 0 3px var(--accent-glow, rgba(108, 99, 255, 0.25));
}

.wf-pill-btn:disabled {
  opacity: .45;
  cursor: default;
}

@media (max-width: 440px) {
  .wf-seg-btn {
    padding: 6px 6px;
    gap: 4px;
    font-size: 11.5px;
  }
}

@media (prefers-reduced-motion: reduce) {
  .wf-seg-thumb,
  .wf-seg-btn,
  .wf-pill-btn {
    transition: none;
  }
}
</style>

