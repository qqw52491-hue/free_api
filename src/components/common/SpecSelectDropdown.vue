<template>
  <div
    class="spec-select-wrapper"
    ref="wrapperRef"
    :class="[size, { 'is-open': isOpen, 'is-disabled': disabled }]"
  >
    <!-- 触发按钮 -->
    <button
      type="button"
      class="spec-select-trigger"
      :disabled="disabled"
      @click="toggleDropdown"
      :title="selectedSpec ? `${selectedSpec.name} (${formatTok(selectedSpec.context_window)} 上下文 / ${formatTok(selectedSpec.max_tokens)} 输出)` : placeholder"
    >
      <div class="trigger-left">
        <span class="spec-icon-badge">
          <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round">
            <polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"></polygon>
          </svg>
        </span>
        <div class="spec-names">
          <span class="selected-spec-name" v-if="selectedSpec">
            {{ selectedSpec.name }}
          </span>
          <span class="placeholder-text" v-else>{{ placeholder }}</span>
          <span class="selected-spec-tag" v-if="selectedSpec">
            {{ formatTok(selectedSpec.context_window) }} / {{ formatTok(selectedSpec.max_tokens) }}
          </span>
        </div>
      </div>

      <div class="trigger-right">
        <button
          v-if="allowClear && selectedSpec && !disabled"
          type="button"
          class="btn-trigger-clear"
          @click.stop="clearSelection"
          title="清除规格套用"
        >
          ✕
        </button>
        <svg class="chevron-icon" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2">
          <path d="m6 9 6 6 6-6"/>
        </svg>
      </div>
    </button>

    <!-- 下拉弹出层 (Teleport 到 body 彻底杜绝模态框或滚动容器的截断) -->
    <Teleport to="body">
      <Transition name="scale-fade">
        <div
          ref="popoverRef"
          class="spec-dropdown-popover is-teleported"
          :style="popoverFixedStyle"
          v-if="isOpen"
          @click.stop
        >
          <!-- 搜索与筛选栏 -->
          <div class="dropdown-search-bar" v-if="specs.length > 5">
            <div class="search-input-wrap">
              <svg class="search-icon" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2">
                <circle cx="11" cy="11" r="8"/>
                <line x1="21" y1="21" x2="16.65" y2="16.65"/>
              </svg>
              <input
                ref="searchInputRef"
                v-model="searchQuery"
                class="dropdown-search-input"
                placeholder="搜索物理规格名称或描述…"
                @keydown.esc="closeDropdown"
              />
              <button
                v-if="searchQuery"
                type="button"
                class="btn-clear-search"
                @click="searchQuery = ''"
                title="清空搜索"
              >
                ✕
              </button>
            </div>
          </div>

          <!-- 选项列表 -->
          <div class="dropdown-options-list">
            <!-- 清空/默认选项 -->
            <div
              v-if="allowClear"
              class="spec-option-item is-clear-option"
              :class="{ 'is-active': !selectedSpec }"
              @click="clearSelection"
            >
              <div class="option-main">
                <span class="option-title">-- 不套用规格词典 --</span>
              </div>
            </div>

            <!-- 规格选项 -->
            <div
              v-for="spec in filteredSpecs"
              :key="spec.id"
              class="spec-option-item"
              :class="{ 'is-active': selectedSpec?.id === spec.id }"
              @click="selectSpec(spec)"
            >
              <div class="option-main">
                <div class="option-header">
                  <span class="spec-name-text">{{ spec.name }}</span>
                  <span v-if="spec.supports_thinking" class="thinking-badge">🧠 思考</span>
                </div>
                <div class="option-meta">
                  <span class="meta-tag">窗口: {{ formatTok(spec.context_window) }}</span>
                  <span class="meta-tag">输出: {{ formatTok(spec.max_tokens) }}</span>
                  <span class="meta-tag" v-if="spec.default_temperature !== undefined">温度: {{ spec.default_temperature }}</span>
                </div>
                <div v-if="spec.description" class="option-desc">{{ spec.description }}</div>
              </div>
              <svg v-if="selectedSpec?.id === spec.id" class="check-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5">
                <polyline points="20 6 9 17 4 12"/>
              </svg>
            </div>

            <!-- 搜索无结果 -->
            <div v-if="filteredSpecs.length === 0" class="dropdown-empty">
              未找到匹配的规格预设
            </div>
          </div>
        </div>
      </Transition>
    </Teleport>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, watch, nextTick, onMounted, onUnmounted } from 'vue'

export interface ModelSpecItem {
  id: string
  name: string
  context_window: number
  max_tokens: number
  supports_thinking?: boolean
  default_temperature?: number
  description?: string
}

const props = withDefaults(defineProps<{
  modelValue?: string
  specs: ModelSpecItem[]
  placeholder?: string
  disabled?: boolean
  allowClear?: boolean
  size?: 'sm' | 'md' | 'lg'
}>(), {
  modelValue: '',
  placeholder: '从规格词典自动代入参数',
  disabled: false,
  allowClear: true,
  size: 'md'
})

const emit = defineEmits<{
  (e: 'update:modelValue', val: string): void
  (e: 'change', val: string, spec: ModelSpecItem | null): void
}>()

const isOpen = ref(false)
const searchQuery = ref('')
const wrapperRef = ref<HTMLElement | null>(null)
const popoverRef = ref<HTMLElement | null>(null)
const searchInputRef = ref<HTMLInputElement | null>(null)
const popoverFixedStyle = ref<Record<string, string>>({})

const selectedSpec = computed(() => {
  return props.specs.find(s => s.id === props.modelValue) || null
})

const filteredSpecs = computed(() => {
  const q = searchQuery.value.trim().toLowerCase()
  if (!q) return props.specs
  return props.specs.filter(s =>
    s.name.toLowerCase().includes(q) || (s.description && s.description.toLowerCase().includes(q))
  )
})

function formatTok(tokens: number) {
  if (!tokens || tokens <= 0) return '0'
  if (tokens >= 1000000) return (tokens / 1000000).toFixed(tokens % 1000000 === 0 ? 0 : 1) + 'M'
  if (tokens >= 1000) return (tokens / 1000).toFixed(tokens % 1000 === 0 ? 0 : 0) + 'k'
  return tokens.toLocaleString()
}

function toggleDropdown() {
  if (props.disabled) return
  isOpen.value = !isOpen.value
}

function closeDropdown() {
  isOpen.value = false
  searchQuery.value = ''
}

function clearSelection() {
  emit('update:modelValue', '')
  emit('change', '', null)
  closeDropdown()
}

function selectSpec(spec: ModelSpecItem) {
  emit('update:modelValue', spec.id)
  emit('change', spec.id, spec)
  closeDropdown()
}

function updatePlacement() {
  if (!isOpen.value || !wrapperRef.value) return
  const rect = wrapperRef.value.getBoundingClientRect()
  const viewportHeight = window.innerHeight
  const estimatedHeight = 320
  const spaceBelow = viewportHeight - rect.bottom
  const spaceAbove = rect.top

  let top = rect.bottom + 6
  if (spaceBelow < estimatedHeight && spaceAbove > spaceBelow) {
    top = Math.max(8, rect.top - estimatedHeight - 6)
  }

  popoverFixedStyle.value = {
    position: 'fixed',
    top: `${Math.round(top)}px`,
    left: `${Math.round(rect.left)}px`,
    width: `${Math.max(280, Math.round(rect.width))}px`,
    zIndex: '2200'
  }
}

watch(isOpen, async (val) => {
  if (val) {
    searchQuery.value = ''
    updatePlacement()
    await nextTick()
    searchInputRef.value?.focus()
  }
})

function handleClickOutside(event: MouseEvent) {
  if (!isOpen.value) return
  const target = event.target as Node
  if (wrapperRef.value && !wrapperRef.value.contains(target) &&
      popoverRef.value && !popoverRef.value.contains(target)) {
    closeDropdown()
  }
}

function handleScrollOrResize() {
  if (isOpen.value) {
    updatePlacement()
  }
}

onMounted(() => {
  document.addEventListener('click', handleClickOutside, true)
  window.addEventListener('scroll', handleScrollOrResize, true)
  window.addEventListener('resize', handleScrollOrResize)
})

onUnmounted(() => {
  document.removeEventListener('click', handleClickOutside, true)
  window.removeEventListener('scroll', handleScrollOrResize, true)
  window.removeEventListener('resize', handleScrollOrResize)
})
</script>

<style scoped>
.spec-select-wrapper {
  position: relative;
  display: inline-block;
  width: auto;
  font-family: var(--font, sans-serif);
}

.spec-select-trigger {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  width: 100%;
  height: 38px;
  padding: 0 12px;
  background: rgba(255, 255, 255, 0.04);
  border: 1px solid rgba(255, 255, 255, 0.12);
  border-radius: var(--radius-sm, 8px);
  color: var(--text-1, #f4f4f5);
  font-size: 13px;
  cursor: pointer;
  transition: all 0.18s ease;
  outline: none;
  user-select: none;
}

.spec-select-trigger:hover:not(:disabled) {
  background: rgba(255, 255, 255, 0.07);
  border-color: rgba(255, 255, 255, 0.22);
}

.spec-select-wrapper.is-open .spec-select-trigger {
  border-color: var(--accent, #6c63ff);
  box-shadow: 0 0 0 2px var(--accent-glow, rgba(108, 99, 255, 0.25));
  background: rgba(255, 255, 255, 0.08);
}

.spec-select-trigger:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.trigger-left {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
  overflow: hidden;
}

.spec-icon-badge {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 22px;
  height: 22px;
  border-radius: 6px;
  background: rgba(245, 158, 11, 0.15);
  color: #fbbf24;
  flex-shrink: 0;
}

.spec-names {
  display: flex;
  align-items: center;
  gap: 6px;
  overflow: hidden;
  white-space: nowrap;
}

.selected-spec-name {
  font-weight: 550;
  color: var(--text-1, #f4f4f5);
  overflow: hidden;
  text-overflow: ellipsis;
}

.placeholder-text {
  color: var(--text-3, #71717a);
}

.selected-spec-tag {
  font-size: 11px;
  padding: 1px 6px;
  border-radius: 4px;
  background: rgba(108, 99, 255, 0.15);
  color: var(--accent-light, #8b83ff);
  font-family: monospace;
  flex-shrink: 0;
}

.trigger-right {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-shrink: 0;
}

.btn-trigger-clear {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  height: 18px;
  padding: 0;
  border-radius: 50%;
  border: none;
  background: rgba(255, 255, 255, 0.1);
  color: var(--text-3, #71717a);
  font-size: 11px;
  line-height: 1;
  cursor: pointer;
  transition: all 0.15s ease;
}

.btn-trigger-clear:hover {
  background: rgba(239, 68, 68, 0.2);
  color: #f87171;
}

.chevron-icon {
  color: var(--text-3, #71717a);
  transition: transform 0.2s ease;
}

.spec-select-wrapper.is-open .chevron-icon {
  transform: rotate(180deg);
  color: var(--accent-light, #8b83ff);
}

/* ---- Popover 下拉弹出卡片 ---- */
.spec-dropdown-popover {
  background: #18181b;
  border: 1px solid rgba(255, 255, 255, 0.16);
  border-radius: var(--radius-md, 12px);
  box-shadow: 0 16px 40px rgba(0, 0, 0, 0.7), 0 0 0 1px rgba(255, 255, 255, 0.08);
  backdrop-filter: blur(16px);
  display: flex;
  flex-direction: column;
  overflow: hidden;
  font-family: var(--font, sans-serif);
}

.dropdown-search-bar {
  padding: 8px 10px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
  background: rgba(255, 255, 255, 0.02);
}

.search-input-wrap {
  position: relative;
  display: flex;
  align-items: center;
}

.search-icon {
  position: absolute;
  left: 9px;
  color: var(--text-3, #71717a);
  pointer-events: none;
}

.dropdown-search-input {
  width: 100%;
  height: 30px;
  padding: 0 28px 0 30px;
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 6px;
  color: var(--text-1, #f4f4f5);
  font-size: 12px;
  outline: none;
  transition: border-color 0.15s;
}

.dropdown-search-input:focus {
  border-color: var(--accent, #6c63ff);
}

.btn-clear-search {
  position: absolute;
  right: 6px;
  padding: 0;
  width: 18px;
  height: 18px;
  border: none;
  background: transparent;
  color: var(--text-3, #71717a);
  font-size: 11px;
  cursor: pointer;
}

.dropdown-options-list {
  max-height: 280px;
  overflow-y: auto;
  padding: 6px;
  scrollbar-width: thin;
}

.spec-option-item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  padding: 8px 10px;
  border-radius: 8px;
  cursor: pointer;
  transition: background 0.15s ease, border-color 0.15s ease;
  border: 1px solid transparent;
}

.spec-option-item:hover {
  background: rgba(255, 255, 255, 0.06);
}

.spec-option-item.is-active {
  background: rgba(108, 99, 255, 0.14);
  border-color: rgba(108, 99, 255, 0.3);
}

.spec-option-item.is-clear-option {
  border-bottom: 1px solid rgba(255, 255, 255, 0.06);
  margin-bottom: 4px;
  border-radius: 6px;
}

.option-main {
  flex: 1;
  min-width: 0;
}

.option-header {
  display: flex;
  align-items: center;
  gap: 6px;
}

.spec-name-text {
  font-size: 13px;
  font-weight: 550;
  color: var(--text-1, #f4f4f5);
}

.thinking-badge {
  font-size: 10px;
  padding: 1px 5px;
  border-radius: 4px;
  background: rgba(168, 85, 247, 0.15);
  color: #c084fc;
}

.option-meta {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-top: 3px;
}

.meta-tag {
  font-size: 11px;
  color: var(--text-3, #a1a1aa);
  background: rgba(255, 255, 255, 0.05);
  padding: 1px 5px;
  border-radius: 3px;
  font-family: monospace;
}

.option-desc {
  font-size: 11px;
  color: var(--text-3, #71717a);
  margin-top: 3px;
  line-height: 1.4;
}

.check-icon {
  color: var(--accent-light, #8b83ff);
  flex-shrink: 0;
}

.dropdown-empty {
  padding: 20px 10px;
  text-align: center;
  font-size: 12px;
  color: var(--text-3, #71717a);
}

/* 动画 */
.scale-fade-enter-active,
.scale-fade-leave-active {
  transition: opacity 0.15s ease, transform 0.15s ease;
}

.scale-fade-enter-from,
.scale-fade-leave-to {
  opacity: 0;
  transform: translateY(-4px) scale(0.98);
}
</style>
