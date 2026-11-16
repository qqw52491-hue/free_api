<template>
  <div ref="wrapperRef" class="wf-select" :class="{ 'is-open': isOpen, 'is-disabled': disabled }">
    <button
      type="button"
      class="wf-trigger"
      :disabled="disabled"
      aria-haspopup="listbox"
      :aria-expanded="isOpen"
      @click="toggle"
      @keydown="onTriggerKey"
    >
      <span class="wf-dot" :class="dotClass(selected?.value ?? modelValue)" aria-hidden="true"></span>
      <span class="wf-value" :class="{ 'is-placeholder': !selected }">{{ selected ? selected.label : placeholder }}</span>
      <span class="wf-chevron-badge" aria-hidden="true">
        <svg class="wf-chevron" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round">
          <path d="m6 9 6 6 6-6" />
        </svg>
      </span>
    </button>

    <select
      ref="selectRef"
      class="form-select workflow-select wf-native-select"
      :value="modelValue"
      :disabled="disabled"
      tabindex="-1"
      aria-hidden="true"
      @change.stop="onNativeChange"
    >
      <option v-for="opt in options" :key="opt.value" :value="opt.value" :disabled="opt.disabled">{{ opt.label }}</option>
    </select>

    <Teleport to="body">
      <Transition name="wf-pop">
        <div
          v-if="isOpen"
          ref="popoverRef"
          class="wf-popover is-teleported"
          role="listbox"
          :style="popoverStyle"
          @click.stop
        >
          <div
            v-for="(opt, idx) in options"
            :key="opt.value"
            class="wf-option"
            role="option"
            :aria-selected="opt.value === modelValue"
            :aria-disabled="opt.disabled || undefined"
            :class="{ 'is-active': opt.value === modelValue, 'is-focus': idx === focusIndex, 'is-disabled': opt.disabled }"
            @mouseenter="!opt.disabled && (focusIndex = idx)"
            @click="choose(opt)"
          >
            <span class="wf-dot" :class="dotClass(opt.value)" aria-hidden="true"></span>
            <span class="wf-option-label">{{ opt.label }}</span>
            <svg v-if="opt.value === modelValue" class="wf-check" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="M20 6 9 17l-5-5" />
            </svg>
          </div>
        </div>
      </Transition>
    </Teleport>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'

export interface WorkflowSelectOption { value: string; label: string; disabled?: boolean }

const props = withDefaults(defineProps<{
  modelValue?: string
  options: WorkflowSelectOption[]
  placeholder?: string
  disabled?: boolean
}>(), { modelValue: '', placeholder: '请选择', disabled: false })

const emit = defineEmits<{
  (e: 'update:modelValue', value: string): void
  (e: 'change', event: Event): void
}>()

const isOpen = ref(false)
const focusIndex = ref(-1)
const wrapperRef = ref<HTMLElement | null>(null)
const popoverRef = ref<HTMLElement | null>(null)
const selectRef = ref<HTMLSelectElement | null>(null)
const popoverStyle = ref<Record<string, string>>({})

const selected = computed(() => props.options.find(o => o.value === props.modelValue) ?? null)

function dotClass(val?: string) {
  if (val === 'on' || val === 'custom' || val === 'allowlist' || val === 'delivery' || val === 'plan_approval' || val === 'cross_review') {
    return 'is-on'
  }
  if (val === 'off') {
    return 'is-off'
  }
  return 'is-auto'
}

function toggle(e?: MouseEvent) {
  if (props.disabled || (e?.currentTarget as HTMLButtonElement | null)?.matches(':disabled')) return
  isOpen.value ? close() : open()
}
function open() {
  const current = props.options.findIndex(o => o.value === props.modelValue && !o.disabled)
  focusIndex.value = current >= 0 ? current : props.options.findIndex(o => !o.disabled)
  isOpen.value = true
}
function close() { isOpen.value = false }
function choose(opt: WorkflowSelectOption) {
  if (opt.disabled) return
  close()
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
  })
}
function move(step: 1 | -1) {
  const n = props.options.length
  if (!n) return
  let i = focusIndex.value
  for (let k = 0; k < n; k++) {
    i = (i + step + n) % n
    if (!props.options[i].disabled) { focusIndex.value = i; return }
  }
}
function onTriggerKey(e: KeyboardEvent) {
  if (props.disabled || (e.currentTarget as HTMLButtonElement | null)?.matches(':disabled')) return
  if (e.key === 'Escape' && isOpen.value) { e.preventDefault(); e.stopPropagation(); close(); return }
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    e.preventDefault()
    if (!isOpen.value) open(); else move(e.key === 'ArrowDown' ? 1 : -1)
    return
  }
  if ((e.key === 'Enter' || e.key === ' ') && isOpen.value) {
    e.preventDefault()
    const opt = props.options[focusIndex.value]
    if (opt) choose(opt); else close()
  }
}

function place() {
  if (!isOpen.value || !wrapperRef.value) return
  const rect = wrapperRef.value.getBoundingClientRect()
  const estimated = Math.min(props.options.length * 40 + 14, 280)
  const below = window.innerHeight - rect.bottom
  let top = rect.bottom + 6
  if (below < estimated + 8 && rect.top > below) top = Math.max(8, rect.top - estimated - 6)
  popoverStyle.value = {
    position: 'fixed',
    top: `${Math.round(top)}px`,
    left: `${Math.round(rect.left)}px`,
    minWidth: `${Math.round(rect.width)}px`,
    maxWidth: `${Math.max(rect.width, 420)}px`,
    zIndex: '100100',
  }
}
watch(isOpen, async v => { if (v) { place(); await nextTick(); place() } })
watch(() => props.disabled, d => { if (d) close() })
watch(() => props.modelValue, val => {
  if (selectRef.value && selectRef.value.value !== (val ?? '')) {
    selectRef.value.value = val ?? ''
  }
})

function onDocClick(e: MouseEvent) {
  if (!isOpen.value) return
  const t = e.target as Node
  if (wrapperRef.value?.contains(t) || popoverRef.value?.contains(t)) return
  close()
}
function onViewport() { if (isOpen.value) place() }
onMounted(() => {
  document.addEventListener('click', onDocClick, true)
  window.addEventListener('scroll', onViewport, true)
  window.addEventListener('resize', onViewport)
})
onUnmounted(() => {
  document.removeEventListener('click', onDocClick, true)
  window.removeEventListener('scroll', onViewport, true)
  window.removeEventListener('resize', onViewport)
})
</script>

<style scoped>
.wf-select { position: relative; display: block; width: 100%; min-width: 0; font-family: var(--font, sans-serif); }

.wf-select > .wf-native-select {
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

.wf-trigger {
  display: flex; align-items: center; justify-content: space-between; gap: 10px;
  width: 100%; min-height: 38px; padding: 0 10px 0 12px;
  background: linear-gradient(180deg, rgba(var(--theme-foreground-rgb, 255, 255, 255), 0.055) 0%, rgba(var(--theme-foreground-rgb, 255, 255, 255), 0.025) 100%);
  border: 1px solid rgba(var(--theme-foreground-rgb, 255, 255, 255), 0.13);
  border-radius: var(--radius-sm, 8px);
  box-shadow: inset 0 1px 0 rgba(var(--theme-foreground-rgb, 255, 255, 255), 0.06), 0 1px 2px rgba(0, 0, 0, 0.2);
  color: var(--theme-text-primary, #f4f4f5);
  font: inherit; font-size: 13px; font-weight: 500; text-align: left;
  cursor: pointer; outline: none; user-select: none;
  transition: background .18s ease, border-color .18s ease, box-shadow .18s ease;
}
.wf-trigger:hover:not(:disabled) {
  border-color: rgba(129, 140, 248, 0.48);
  background: linear-gradient(180deg, rgba(99, 102, 241, 0.11) 0%, rgba(var(--theme-foreground-rgb, 255, 255, 255), 0.04) 100%);
}
.wf-trigger:focus-visible,
.wf-select.is-open .wf-trigger {
  border-color: var(--accent, #6c63ff);
  box-shadow: 0 0 0 3px var(--accent-glow, rgba(108, 99, 255, 0.25));
}
.wf-trigger:disabled {
  opacity: .5;
  cursor: default;
  box-shadow: none;
}

.wf-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  flex-shrink: 0;
  transition: background-color .18s ease, box-shadow .18s ease;
}
.wf-dot.is-auto {
  background: rgba(var(--theme-foreground-rgb, 255, 255, 255), 0.35);
}
.wf-dot.is-on {
  background: var(--theme-status-success, #34d399);
  box-shadow: 0 0 8px rgba(52, 211, 153, 0.5);
}
.wf-dot.is-off {
  background: var(--theme-status-warning, #f59e0b);
  box-shadow: 0 0 7px rgba(245, 158, 11, 0.45);
}

.wf-value { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.wf-value.is-placeholder { color: var(--theme-text-muted, #71717a); font-weight: 400; }

.wf-chevron-badge {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
  border-radius: 6px;
  background: rgba(var(--theme-foreground-rgb, 255, 255, 255), 0.05);
  border: 1px solid rgba(var(--theme-foreground-rgb, 255, 255, 255), 0.06);
  flex-shrink: 0;
  transition: background-color .18s ease, border-color .18s ease;
}
.wf-select.is-open .wf-chevron-badge {
  background: rgba(108, 99, 255, 0.16);
  border-color: rgba(129, 140, 248, 0.35);
}
.wf-chevron { color: var(--theme-text-muted, #71717a); transition: transform .2s ease, color .2s ease; }
.wf-select.is-open .wf-chevron { transform: rotate(180deg); color: var(--accent-light, #8b83ff); }

.wf-popover {
  box-sizing: border-box; padding: 6px; max-height: 280px; overflow-y: auto; scrollbar-width: thin;
  background: var(--theme-bg-panel, #18181b);
  border: 1px solid rgba(var(--theme-foreground-rgb, 255, 255, 255), 0.16);
  border-radius: var(--radius-md, 12px);
  box-shadow: var(--theme-shadow, 0 16px 40px rgba(0, 0, 0, 0.7), 0 0 0 1px rgba(255, 255, 255, 0.08));
  backdrop-filter: blur(16px);
  font-family: var(--font, sans-serif);
}
.wf-option {
  display: flex; align-items: center; justify-content: space-between; gap: 10px;
  padding: 8px 10px; border: 1px solid transparent; border-radius: 8px;
  color: var(--theme-text-primary, #f4f4f5); font-size: 13px; line-height: 1.45; cursor: pointer;
  transition: background .15s ease, border-color .15s ease;
}
.wf-option + .wf-option { margin-top: 2px; }
.wf-option.is-focus { background: rgba(var(--theme-foreground-rgb, 255, 255, 255), 0.06); }
.wf-option.is-active {
  background: linear-gradient(135deg, rgba(108, 99, 255, 0.18) 0%, rgba(129, 140, 248, 0.09) 100%);
  border-color: rgba(108, 99, 255, 0.36);
  font-weight: 550;
}
.wf-option.is-disabled { opacity: .4; cursor: not-allowed; }
.wf-option-label { flex: 1; min-width: 0; }
.wf-check { flex-shrink: 0; color: var(--accent-light, #8b83ff); }

.wf-pop-enter-active, .wf-pop-leave-active { transition: opacity var(--motion-normal, 180ms) ease, transform var(--motion-normal, 180ms) var(--motion-ease, cubic-bezier(.2, .8, .2, 1)); }
.wf-pop-leave-active { transition-duration: var(--motion-quick, 120ms); }
.wf-pop-enter-from, .wf-pop-leave-to { opacity: 0; transform: translateY(-3px) scale(.99); }
@media (prefers-reduced-motion: reduce) {
  .wf-pop-enter-active, .wf-pop-leave-active { transition: none; }
  .wf-pop-enter-from, .wf-pop-leave-to { transform: none; }
}
</style>
