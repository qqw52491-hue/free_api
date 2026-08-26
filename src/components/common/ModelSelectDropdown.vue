<template>
  <div class="model-select-wrapper" ref="wrapperRef" :class="[size, { 'is-open': isOpen, 'is-disabled': disabled }]">
    <!-- Trigger Button -->
    <button
      type="button"
      class="model-select-trigger"
      :disabled="disabled"
      @click="toggleDropdown"
      :title="selectedModel ? `${selectedModel.display_name || selectedModel.name} (${selectedModel.platform_name || '未知平台'})` : placeholder"
    >
      <div class="trigger-left">
        <span class="platform-icon">{{ getPlatformEmoji(selectedModel?.platform_name || selectedModel?.name || '') }}</span>
        <div class="model-names">
          <span class="selected-display-name" v-if="selectedModel">
            {{ selectedModel.display_name || selectedModel.name }}
          </span>
          <span class="placeholder-text" v-else>{{ placeholder }}</span>
          <span class="selected-platform-tag" v-if="selectedModel && selectedModel.platform_name">
            {{ selectedModel.platform_name }}
          </span>
        </div>
      </div>

      <div class="trigger-right">
        <span class="badge-count" v-if="models.length > 0 && !selectedModel">{{ models.length }}</span>
        <svg class="chevron-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="m6 9 6 6 6-6"/>
        </svg>
      </div>
    </button>

    <!-- Dropdown Popover -->
    <Transition name="scale-fade">
      <div
        class="model-dropdown-popover"
        :class="[`placement-${actualPlacement}`, { 'align-right': alignRight }]"
        v-if="isOpen"
      >
        <!-- Search Input Bar -->
        <div class="dropdown-search-bar">
          <div class="search-input-wrap">
            <svg class="search-icon" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2">
              <circle cx="11" cy="11" r="8"/>
              <line x1="21" y1="21" x2="16.65" y2="16.65"/>
            </svg>
            <input
              ref="searchInputRef"
              v-model="searchQuery"
              class="dropdown-search-input"
              placeholder="搜索模型标识、名称或平台 (如 flash, r1, deepseek)..."
              @keydown.esc="closeDropdown"
              @keydown.enter.prevent="selectFirstMatch"
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
          <div class="search-stats-row">
            <span class="stats-text">
              <template v-if="searchQuery.trim()">
                找到 <strong>{{ matchedModelsCount }}</strong> 个匹配项 (共 {{ models.length }} 个)
              </template>
              <template v-else>
                共 <strong>{{ models.length }}</strong> 个可用模型 · {{ groupedFilteredModels.length }} 个平台
              </template>
            </span>
            <span class="shortcut-tip">按 Enter 选择首项</span>
          </div>
        </div>

        <!-- Scrollable Model Groups List -->
        <div class="dropdown-models-list">
          <template v-if="groupedFilteredModels.length > 0">
            <div
              v-for="group in groupedFilteredModels"
              :key="group.platform"
              class="platform-group-block"
            >
              <!-- Platform Header -->
              <div class="group-header">
                <span class="group-platform-icon">{{ getPlatformEmoji(group.platform) }}</span>
                <span class="group-platform-name">{{ group.platform }}</span>
                <span class="group-count-badge">{{ group.models.length }}</span>
              </div>

              <!-- Model Option Rows -->
              <div class="group-models-container">
                <button
                  v-for="m in group.models"
                  :key="m.id"
                  type="button"
                  class="model-option-row"
                  :class="{ 'is-selected': m.id === modelValue }"
                  @click="selectModel(m)"
                >
                  <div class="option-left">
                    <span class="model-status-dot" :class="{ 'is-active': m.id === modelValue }"></span>
                    <div class="option-text-group">
                      <div class="option-title-line">
                        <span class="option-display-name">{{ m.display_name || m.name }}</span>
                        <span v-if="isThinkingModel(m)" class="badge-thinking">🧠 思考</span>
                        <span v-if="m.context_window && m.context_window > 0" class="badge-ctx">
                          {{ formatTok(m.context_window) }}
                        </span>
                      </div>
                      <div class="option-id-line" v-if="m.display_name && m.display_name !== m.name">
                        {{ m.name }}
                      </div>
                    </div>
                  </div>

                  <div class="option-right">
                    <svg v-if="m.id === modelValue" class="check-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5">
                      <polyline points="20 6 9 17 4 12"/>
                    </svg>
                  </div>
                </button>
              </div>
            </div>
          </template>

          <!-- Empty State -->
          <div v-else class="dropdown-empty-state">
            <div class="empty-icon">🔍</div>
            <p class="empty-title">未找到匹配的模型</p>
            <p class="empty-desc">没有模型匹配 "{{ searchQuery }}"</p>
            <button type="button" class="btn-reset-search" @click="searchQuery = ''">
              清空搜索关键词
            </button>
          </div>
        </div>
      </div>
    </Transition>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, watch, nextTick, onMounted, onBeforeUnmount } from 'vue'

export interface ModelOptionItem {
  id: string;
  name: string;
  display_name?: string;
  platform_name?: string;
  platform_id?: string;
  context_window?: number;
  max_tokens?: number;
  enabled?: boolean;
  [key: string]: any;
}

const props = withDefaults(
  defineProps<{
    modelValue?: string;
    models?: ModelOptionItem[];
    placeholder?: string;
    disabled?: boolean;
    size?: 'sm' | 'md' | 'lg';
    placement?: 'auto' | 'top' | 'bottom';
  }>(),
  {
    modelValue: '',
    models: () => [],
    placeholder: '选择模型...',
    disabled: false,
    size: 'md',
    placement: 'auto',
  }
)

const emit = defineEmits<{
  'update:modelValue': [value: string];
  'change': [value: string, model: ModelOptionItem | null];
}>()

const isOpen = ref(false)
const searchQuery = ref('')
const wrapperRef = ref<HTMLElement | null>(null)
const searchInputRef = ref<HTMLInputElement | null>(null)
const actualPlacement = ref<'top' | 'bottom'>('bottom')
const alignRight = ref(false)

function calculatePlacement() {
  if (!wrapperRef.value) return
  const rect = wrapperRef.value.getBoundingClientRect()
  const spaceBelow = window.innerHeight - rect.bottom
  const spaceAbove = rect.top

  if (props.placement === 'top') {
    actualPlacement.value = 'top'
  } else if (props.placement === 'bottom') {
    actualPlacement.value = 'bottom'
  } else {
    // 弹窗高度大约 360-400px，若下方空间不足且上方空间更大则自适应向上弹出
    if (spaceBelow < 380 && spaceAbove > spaceBelow) {
      actualPlacement.value = 'top'
    } else {
      actualPlacement.value = 'bottom'
    }
  }

  // 水平右对齐自适应：若右侧距离屏幕边缘较近，则贴右对齐防止溢出
  if (rect.left + 380 > window.innerWidth && rect.right >= 340) {
    alignRight.value = true
  } else {
    alignRight.value = false
  }
}

const selectedModel = computed(() => {
  if (!props.modelValue) return null
  return props.models.find(m => m.id === props.modelValue) || null
})

function getPlatformEmoji(name: string) {
  const n = (name || '').toLowerCase()
  if (n.includes('openrouter')) return '🌈'
  if (n.includes('deepseek')) return '🐋'
  if (n.includes('openai') || n.includes('chatgpt') || n.includes('gpt')) return '🟢'
  if (n.includes('claude') || n.includes('anthropic')) return '🟠'
  if (n.includes('google') || n.includes('gemini')) return '🔵'
  if (n.includes('硅基') || n.includes('siliconflow')) return '💎'
  if (n.includes('商汤') || n.includes('sensenova')) return '⚡'
  if (n.includes('ollama') || n.includes('local') || n.includes('11434')) return '🦙'
  if (n.includes('qwen') || n.includes('通义')) return '🟣'
  return '🤖'
}

function isThinkingModel(m: ModelOptionItem): boolean {
  const str = `${m.name} ${m.display_name || ''}`.toLowerCase()
  return str.includes('r1') || str.includes('reasoner') || str.includes('thinking') || str.includes('claude-3-7') || str.includes('o1') || str.includes('o3')
}

function formatTok(tokens?: number) {
  if (!tokens || tokens <= 0) return ''
  if (tokens >= 1000000) return (tokens / 1000000).toFixed(tokens % 1000000 === 0 ? 0 : 1) + 'M'
  if (tokens >= 1000) return Math.round(tokens / 1000) + 'k'
  return tokens.toString()
}

// 过滤后的模型分组
const groupedFilteredModels = computed(() => {
  const query = searchQuery.value.trim().toLowerCase()
  const map = new Map<string, ModelOptionItem[]>()

  for (const m of props.models) {
    const platform = m.platform_name || '其他平台'
    const nameMatch = m.name.toLowerCase().includes(query)
    const displayMatch = (m.display_name || '').toLowerCase().includes(query)
    const platformMatch = platform.toLowerCase().includes(query)

    if (!query || nameMatch || displayMatch || platformMatch) {
      if (!map.has(platform)) {
        map.set(platform, [])
      }
      map.get(platform)!.push(m)
    }
  }

  const result: { platform: string; models: ModelOptionItem[] }[] = []
  for (const [platform, modelsList] of map.entries()) {
    result.push({ platform, models: modelsList })
  }
  return result
})

const matchedModelsCount = computed(() => {
  return groupedFilteredModels.value.reduce((acc, g) => acc + g.models.length, 0)
})

function toggleDropdown() {
  if (props.disabled) return
  isOpen.value = !isOpen.value
}

function closeDropdown() {
  isOpen.value = false
  searchQuery.value = ''
}

function selectModel(m: ModelOptionItem) {
  emit('update:modelValue', m.id)
  emit('change', m.id, m)
  closeDropdown()
}

function selectFirstMatch() {
  for (const group of groupedFilteredModels.value) {
    if (group.models.length > 0) {
      selectModel(group.models[0])
      return
    }
  }
}

watch(isOpen, async (val) => {
  if (val) {
    calculatePlacement()
    searchQuery.value = ''
    await nextTick()
    searchInputRef.value?.focus()
  }
})

// 点击外部区域自动关闭
function handleClickOutside(event: MouseEvent) {
  if (wrapperRef.value && !wrapperRef.value.contains(event.target as Node)) {
    closeDropdown()
  }
}

onMounted(() => {
  document.addEventListener('click', handleClickOutside)
})

onBeforeUnmount(() => {
  document.removeEventListener('click', handleClickOutside)
})
</script>

<style scoped>
.model-select-wrapper {
  position: relative;
  display: inline-block;
  user-select: none;
  font-family: var(--font, -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif);
}

/* ---- Trigger Button ---- */
.model-select-trigger {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  background: var(--bg-2, #1f1f23);
  border: 1px solid var(--border-2, rgba(255, 255, 255, 0.14));
  border-radius: var(--radius-sm, 8px);
  color: var(--text-1, #f4f4f5);
  cursor: pointer;
  transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1);
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.2);
  width: 100%;
}

.model-select-wrapper.sm .model-select-trigger {
  padding: 4px 10px;
  min-height: 30px;
  font-size: 12px;
}

.model-select-wrapper.md .model-select-trigger {
  padding: 6px 12px;
  min-height: 36px;
  font-size: 13px;
}

.model-select-wrapper.lg .model-select-trigger {
  padding: 8px 14px;
  min-height: 42px;
  font-size: 14px;
}

.model-select-trigger:hover:not(:disabled) {
  background: var(--bg-3, #27272a);
  border-color: rgba(255, 255, 255, 0.25);
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.3);
}

.model-select-wrapper.is-open .model-select-trigger {
  border-color: var(--accent, #6c63ff);
  box-shadow: 0 0 0 2px rgba(108, 99, 255, 0.2);
}

.model-select-wrapper.is-disabled .model-select-trigger {
  opacity: 0.5;
  cursor: not-allowed;
}

.trigger-left {
  display: flex;
  align-items: center;
  gap: 8px;
  overflow: hidden;
  text-align: left;
}

.platform-icon {
  font-size: 14px;
  flex-shrink: 0;
}

.model-names {
  display: flex;
  align-items: center;
  gap: 6px;
  overflow: hidden;
  white-space: nowrap;
}

.selected-display-name {
  font-weight: 550;
  color: var(--text-1, #f4f4f5);
  overflow: hidden;
  text-overflow: ellipsis;
}

.placeholder-text {
  color: var(--text-3, #71717a);
}

.selected-platform-tag {
  font-size: 11px;
  padding: 1px 6px;
  border-radius: 4px;
  background: rgba(255, 255, 255, 0.08);
  color: var(--text-3, #71717a);
  font-weight: 500;
  flex-shrink: 0;
}

.trigger-right {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-shrink: 0;
}

.badge-count {
  font-size: 11px;
  padding: 1px 6px;
  border-radius: 99px;
  background: rgba(255, 255, 255, 0.08);
  color: var(--text-3, #71717a);
}

.chevron-icon {
  color: var(--text-3, #71717a);
  transition: transform 0.2s ease;
}

.model-select-wrapper.is-open .chevron-icon {
  transform: rotate(180deg);
  color: var(--accent-light, #8b83ff);
}

/* ---- Popover Dropdown ---- */
.model-dropdown-popover {
  position: absolute;
  top: calc(100% + 6px);
  left: 0;
  min-width: 320px;
  max-width: min(440px, calc(100vw - 32px));
  width: max-content;
  background: #18181b;
  border: 1px solid rgba(255, 255, 255, 0.16);
  border-radius: var(--radius-md, 12px);
  box-shadow: 0 16px 40px rgba(0, 0, 0, 0.7), 0 0 0 1px rgba(255, 255, 255, 0.08);
  z-index: 1000;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  backdrop-filter: blur(16px);
  animation: popoverFadeIn 0.18s cubic-bezier(0.16, 1, 0.3, 1);
}

.model-dropdown-popover.align-right {
  left: auto;
  right: 0;
}

.model-dropdown-popover.placement-top {
  top: auto;
  bottom: calc(100% + 6px);
  animation: popoverFadeInUp 0.18s cubic-bezier(0.16, 1, 0.3, 1);
}

@keyframes popoverFadeIn {
  from {
    opacity: 0;
    transform: translateY(-4px) scale(0.98);
  }
  to {
    opacity: 1;
    transform: translateY(0) scale(1);
  }
}

@keyframes popoverFadeInUp {
  from {
    opacity: 0;
    transform: translateY(4px) scale(0.98);
  }
  to {
    opacity: 1;
    transform: translateY(0) scale(1);
  }
}

/* Search Header */
.dropdown-search-bar {
  padding: 10px 12px;
  background: #1c1c20;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.search-input-wrap {
  position: relative;
  display: flex;
  align-items: center;
}

.search-icon {
  position: absolute;
  left: 10px;
  color: var(--text-3, #71717a);
  pointer-events: none;
}

.dropdown-search-input {
  width: 100%;
  padding: 7px 28px 7px 32px;
  background: #121214;
  border: 1px solid rgba(255, 255, 255, 0.12);
  border-radius: 6px;
  color: #fff;
  font-size: 12.5px;
  outline: none;
  transition: all 0.15s ease;
}

.dropdown-search-input:focus {
  border-color: var(--accent, #6c63ff);
  background: #09090b;
  box-shadow: 0 0 0 2px rgba(108, 99, 255, 0.15);
}

.dropdown-search-input::placeholder {
  color: var(--text-4, #52525b);
}

.btn-clear-search {
  position: absolute;
  right: 8px;
  background: transparent;
  border: none;
  color: var(--text-3, #71717a);
  cursor: pointer;
  font-size: 12px;
  padding: 2px 4px;
}

.btn-clear-search:hover {
  color: #fff;
}

.search-stats-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  font-size: 11px;
  color: var(--text-3, #71717a);
  padding: 0 2px;
}

.search-stats-row strong {
  color: var(--text-2, #a1a1aa);
}

.shortcut-tip {
  color: var(--text-4, #52525b);
}

/* Models List Area */
.dropdown-models-list {
  max-height: min(340px, 42vh);
  overflow-y: auto;
  padding: 6px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.platform-group-block {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.group-header {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 8px 4px;
  font-size: 11.5px;
  font-weight: 600;
  color: var(--text-3, #71717a);
  border-bottom: 1px solid rgba(255, 255, 255, 0.04);
}

.group-platform-icon {
  font-size: 13px;
}

.group-platform-name {
  flex: 1;
  text-transform: uppercase;
  letter-spacing: 0.5px;
}

.group-count-badge {
  font-size: 10px;
  padding: 1px 5px;
  border-radius: 99px;
  background: rgba(255, 255, 255, 0.06);
  color: var(--text-4, #52525b);
}

.group-models-container {
  display: flex;
  flex-direction: column;
  gap: 2px;
  margin-top: 2px;
}

.model-option-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 7px 10px;
  border-radius: 6px;
  background: transparent;
  border: 1px solid transparent;
  color: var(--text-1, #f4f4f5);
  cursor: pointer;
  text-align: left;
  transition: all 0.12s ease;
  width: 100%;
}

.model-option-row:hover {
  background: rgba(255, 255, 255, 0.06);
  border-color: rgba(255, 255, 255, 0.08);
}

.model-option-row.is-selected {
  background: rgba(108, 99, 255, 0.14);
  border-color: rgba(108, 99, 255, 0.35);
}

.option-left {
  display: flex;
  align-items: center;
  gap: 8px;
  overflow: hidden;
  flex: 1;
}

.model-status-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: rgba(255, 255, 255, 0.2);
  flex-shrink: 0;
}

.model-status-dot.is-active {
  background: var(--accent-light, #8b83ff);
  box-shadow: 0 0 8px var(--accent, #6c63ff);
}

.option-text-group {
  display: flex;
  flex-direction: column;
  gap: 1px;
  overflow: hidden;
}

.option-title-line {
  display: flex;
  align-items: center;
  gap: 6px;
  overflow: hidden;
}

.option-display-name {
  font-size: 12.5px;
  font-weight: 500;
  color: #fff;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.badge-thinking {
  font-size: 10px;
  padding: 1px 5px;
  border-radius: 4px;
  background: rgba(168, 85, 247, 0.15);
  color: #c084fc;
  border: 1px solid rgba(168, 85, 247, 0.3);
  font-weight: 600;
  flex-shrink: 0;
}

.badge-ctx {
  font-size: 10px;
  padding: 1px 5px;
  border-radius: 4px;
  background: rgba(255, 255, 255, 0.06);
  color: var(--text-3, #71717a);
  flex-shrink: 0;
}

.option-id-line {
  font-size: 11px;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, monospace;
  color: var(--text-3, #71717a);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.option-right {
  display: flex;
  align-items: center;
  margin-left: 8px;
  flex-shrink: 0;
}

.check-icon {
  color: var(--accent-light, #8b83ff);
}

/* Empty Search State */
.dropdown-empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 32px 16px;
  text-align: center;
}

.empty-icon {
  font-size: 24px;
  margin-bottom: 8px;
  opacity: 0.6;
}

.empty-title {
  font-size: 13px;
  font-weight: 600;
  color: var(--text-2, #a1a1aa);
  margin-bottom: 2px;
}

.empty-desc {
  font-size: 11.5px;
  color: var(--text-4, #52525b);
  margin-bottom: 12px;
  max-width: 240px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.btn-reset-search {
  background: var(--surface-2, rgba(255, 255, 255, 0.06));
  border: 1px solid var(--border-2, rgba(255, 255, 255, 0.14));
  color: #fff;
  padding: 4px 12px;
  border-radius: 6px;
  font-size: 11.5px;
  cursor: pointer;
  transition: all 0.15s ease;
}

.btn-reset-search:hover {
  background: var(--surface-3, rgba(255, 255, 255, 0.1));
}

/* Transitions */
.scale-fade-enter-active,
.scale-fade-leave-active {
  transition: all 0.15s cubic-bezier(0.16, 1, 0.3, 1);
}

.scale-fade-enter-from,
.scale-fade-leave-to {
  opacity: 0;
  transform: translateY(-4px) scale(0.98);
}
</style>
