<template>
  <div class="config-transfer-actions config-entry-actions">
    <button type="button" @click="openTransfer('import')"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3v12m-4-4 4 4 4-4M4 16v4h16v-4" /></svg>导入配置</button>
    <button type="button" @click="openTransfer('export')"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 15V3m-4 4 4-4 4 4M4 16v4h16v-4" /></svg>导出配置</button>
  </div>
  <Teleport to="body">
    <div v-if="mode" class="config-transfer-overlay" @click.self="closeTransfer">
      <section class="config-transfer-dialog" role="dialog" aria-modal="true" aria-labelledby="config-transfer-title" :aria-busy="busy" @keydown.esc="closeTransfer">
        <header>
          <div class="config-heading">
            <div class="config-heading-icon"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 4h16v16H4zM8 4v16M12 9h4m-4 5h4" /></svg></div>
            <div><h3 id="config-transfer-title">{{ mode === 'import' ? '导入模型配置' : '导出模型配置' }}</h3><p class="config-subtitle">{{ mode === 'import' ? '从文件或文本中恢复你的平台与模型' : '把平台与模型配置带到另一台设备' }}</p></div>
          </div>
          <button type="button" class="config-close" aria-label="关闭" :disabled="busy" @click="closeTransfer">✕</button>
        </header>
        <div class="config-transfer-body">
          <template v-if="mode === 'import'">
            <label class="config-file-label" :class="{ 'is-disabled': busy, 'is-compact': rows.length > 0 }">
              <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M14 3H5v18h14V8zm0 0v5h5M12 17v-5m-2 2 2-2 2 2" /></svg>
              <span><strong>选择 JSON 文件</strong><small>点击选择已导出的配置，自动解析并预览</small></span>
              <span class="config-file-format">.json</span>
              <input type="file" accept=".json,application/json" aria-label="选择 JSON 文件" :disabled="busy" @change="readConfigFile" />
            </label>
            <div class="config-editor" :class="{ 'has-preview': rows.length > 0 }">
              <div class="config-editor-header"><span>或粘贴配置内容</span><button type="button" class="config-text-button" :disabled="busy" @click="useExample">填入格式示例</button></div>
              <textarea v-model="inputJson" aria-label="待导入的 JSON 配置" placeholder="在此粘贴 JSON 配置…" spellcheck="false" :disabled="busy" @input="invalidatePreview" />
              <div class="config-editor-footer"><span class="config-note">解析后可勾选要导入的内容</span><button type="button" :disabled="busy || !inputJson.trim()" @click="parseImport">解析并预览</button></div>
            </div>
            <p class="config-note config-import-note">重复项自动跳过，保留已有配置。未包含的 API Key 可稍后补填。</p>
          </template>
          <template v-else>
            <label class="config-key-choice" :class="{ 'is-enabled': includeKeys }">
              <input v-model="includeKeys" type="checkbox" :disabled="busy" />
              <span class="config-key-icon"><svg viewBox="0 0 24 24" aria-hidden="true"><rect x="5" y="10" width="14" height="11" rx="3" /><path d="M8 10V7a4 4 0 0 1 8 0v3m-4 5v2" /></svg></span>
              <span class="config-key-copy"><strong>包含 API Key</strong><small>默认不导出；开启后密钥将以明文写入文件。</small></span>
            </label>
          </template>

          <div v-if="rows.length" class="config-selection">
            <div class="config-selection-header">
              <strong>选择{{ mode === 'import' ? '导入' : '导出' }}内容</strong>
              <span class="config-note">已选 {{ selection.platforms.length }} 个平台 · {{ selectedModelCount }} 个模型</span>
            </div>
            <div class="config-picker">
              <div class="config-search-header">
                <div class="config-search-input-wrap">
                  <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="10.5" cy="10.5" r="6.5" /><path d="m16 16 4.5 4.5" /></svg>
                  <input v-model="searchQuery" type="search" aria-label="搜索模型或平台" placeholder="搜索模型名称、标识或平台" :disabled="busy" autocomplete="off" spellcheck="false" />
                  <button v-if="searchQuery" type="button" class="config-clear-search" aria-label="清空搜索" :disabled="busy" @click="searchQuery = ''">✕</button>
                </div>
                <div class="config-search-meta">
                  <span>{{ searchQuery.trim() ? '匹配' : '共' }} {{ filteredRows.length }} 个平台 · {{ visibleModelCount }} 个模型</span>
                  <div class="config-transfer-actions">
                    <button type="button" class="config-text-button" :disabled="busy || !filteredRows.length" @click="selectAll(true)">{{ searchQuery.trim() ? '选择匹配项' : '全选' }}</button>
                    <button type="button" class="config-text-button" :disabled="busy || !filteredRows.length" @click="selectAll(false)">{{ searchQuery.trim() ? '取消匹配项' : '取消全选' }}</button>
                  </div>
                </div>
              </div>
              <div class="config-picker-list">
                <div v-for="group in filteredRows" :key="group.sourceIndex" class="config-platform">
                  <label class="config-platform-heading">
                    <input type="checkbox" :checked="group.row.selected && group.models.every(m => m.selected)" :disabled="busy" :indeterminate="group.row.selected && group.models.some(m => m.selected) && group.models.some(m => !m.selected)" @change="togglePlatform(group, $event)" />
                    <svg viewBox="0 0 24 24" class="config-platform-icon" aria-hidden="true"><circle cx="12" cy="12" r="9" /><path d="M3 12h18M12 3a18 18 0 0 1 0 18 18 18 0 0 1 0-18" /></svg>
                    <span class="config-platform-info"><strong>{{ group.row.platform.name }}</strong><span class="config-endpoint">{{ group.row.platform.base_url }}</span></span>
                    <span v-if="group.models.length" class="config-count-badge">{{ groupSelectedCount(group) }}/{{ group.models.length }} <span>模型</span></span>
                    <span v-if="mode === 'import'" class="config-key-badge">{{ group.row.platform.api_key?.trim() ? '含 Key' : '未含 Key' }}</span>
                  </label>
                  <div v-if="group.models.length" class="config-model-list">
                    <label v-for="item in group.models" :key="item.model.name" class="config-model" :class="{ 'is-selected': group.row.selected && item.selected, 'is-unselected': !(group.row.selected && item.selected) }">
                      <input v-model="item.selected" type="checkbox" :disabled="busy" @change="toggleModel(group.row, item, item.selected)" />
                      <span class="config-model-name">
                        <span class="config-model-title" :title="item.model.display_name || item.model.name">{{ item.model.display_name || item.model.name }}</span>
                        <small v-if="item.model.display_name && item.model.display_name !== item.model.name" :title="item.model.name">{{ item.model.name }}</small>
                      </span>
                      <small v-if="!item.model.enabled" class="config-disabled-badge">已禁用</small>
                    </label>
                  </div>
                  <small v-else class="config-empty-models">仅平台配置，未包含模型</small>
                </div>
                <div v-if="!filteredRows.length" class="config-search-empty" role="status">
                  <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="10.5" cy="10.5" r="6.5" /><path d="m16 16 4.5 4.5" /></svg>
                  <strong>未找到匹配的平台或模型</strong>
                  <span>试试其他名称，或清空搜索查看全部</span>
                  <button type="button" :disabled="busy" @click="searchQuery = ''">清空搜索</button>
                </div>
              </div>
              <div v-if="searchQuery.trim()" class="config-filter-note">搜索不会清除已勾选内容<span v-if="hiddenSelectedModelCount">，另有 {{ hiddenSelectedModelCount }} 个已选模型未显示</span>。</div>
            </div>
          </div>
          <p v-if="busy && !rows.length" class="config-note" role="status">正在处理配置…</p>
          <p v-else-if="mode === 'export' && !rows.length && !busy && !error" class="config-note">暂无可导出的平台，请先添加平台或导入配置。</p>
          <details v-if="mode === 'export' && rows.length">
            <summary>查看将导出的 JSON{{ includeKeys ? '（含明文 Key）' : '' }}</summary>
            <textarea :value="outputJson" readonly aria-label="导出 JSON 预览" spellcheck="false" />
          </details>
          <p v-if="error" class="config-error" role="alert">{{ error }}</p>
          <p v-if="message" class="config-success" role="status">{{ message }}</p>
        </div>
        <footer>
          <span class="config-footer-note">{{ selection.platforms.length ? `已选 ${selection.platforms.length} 个平台 · ${selectedModelCount} 个模型` : '平台与模型配置' }}</span>
          <button type="button" :disabled="busy" @click="closeTransfer">关闭</button>
          <template v-if="mode === 'export'">
            <button type="button" :disabled="busy || !selection.platforms.length" @click="copyExport">复制 JSON</button>
            <button type="button" class="config-primary" :disabled="busy || !selection.platforms.length" @click="saveExport">保存 JSON 文件</button>
          </template>
          <button v-else type="button" class="config-primary" :disabled="busy || !selection.platforms.length" @click="executeImport">{{ busy ? '处理中…' : '导入所选配置' }}</button>
        </footer>
      </section>
    </div>
  </Teleport>
</template>

<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { save } from '@tauri-apps/plugin-dialog'
import { MODEL_CONFIG_EXAMPLE, selectConfig, selectedConfig, filterConfigPlatforms, toggleConfigMatch, toggleConfigModel, type ModelConfig, type SelectablePlatform, type FilteredConfigPlatform } from '../utils/modelConfigTransfer'

const emit = defineEmits<{ imported: [] }>()
const mode = ref<'import' | 'export' | null>(null)
const rows = ref<SelectablePlatform[]>([])
const inputJson = ref('')
const includeKeys = ref(false)
const busy = ref(false)
const error = ref('')
const message = ref('')
const searchQuery = ref('')
const selection = computed(() => selectedConfig(rows.value, mode.value === 'import' || includeKeys.value))
const selectedModelCount = computed(() => selection.value.platforms.reduce((sum, p) => sum + p.models.length, 0))
const outputJson = computed(() => JSON.stringify(selection.value, null, 2))
const filteredRows = computed(() => filterConfigPlatforms(rows.value, searchQuery.value))
const visibleModelCount = computed(() => filteredRows.value.reduce((sum, group) => sum + group.models.length, 0))
const hiddenSelectedModelCount = computed(() => selectedModelCount.value - filteredRows.value.reduce((sum, group) =>
  sum + (group.row.selected ? group.models.filter(item => item.selected).length : 0), 0))
let previousFocus: HTMLElement | null = null

watch(rows, (currentRows) => {
  for (const row of currentRows) {
    if (row.models.length > 0) {
      if (row.models.some(m => m.selected)) {
        row.selected = true
      } else {
        row.selected = false
      }
    }
  }
}, { deep: true })

function errorText(value: unknown) { return value instanceof Error ? value.message : String(value) }

async function openTransfer(value: 'import' | 'export') {
  if (busy.value || mode.value) return
  previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null
  mode.value = value
  rows.value = []
  inputJson.value = ''
  includeKeys.value = false
  error.value = ''
  message.value = ''
  searchQuery.value = ''
  await nextTick()
  document.querySelector<HTMLElement>('.config-transfer-dialog button')?.focus()
  if (value === 'export') {
    busy.value = true
    try {
      // Keep one snapshot; the explicit checkbox controls serialization, not a second read.
      const config = await invoke<ModelConfig>('export_model_config', { includeApiKeys: true })
      rows.value = selectConfig(config)
    } catch (e) { error.value = '读取配置失败：' + errorText(e) }
    finally { busy.value = false }
  }
}

function closeTransfer() {
  if (busy.value) return
  mode.value = null
  rows.value = []
  inputJson.value = ''
  includeKeys.value = false
  searchQuery.value = ''
  previousFocus?.focus()
}

function invalidatePreview() {
  rows.value = []
  error.value = ''
  message.value = ''
  searchQuery.value = ''
}

function useExample() {
  invalidatePreview()
  inputJson.value = MODEL_CONFIG_EXAMPLE
}

async function readConfigFile(event: Event) {
  const input = event.target as HTMLInputElement
  const file = input.files?.[0]
  input.value = ''
  if (!file || busy.value) return
  busy.value = true
  invalidatePreview()
  try {
    inputJson.value = await file.text()
    rows.value = selectConfig(await invoke<ModelConfig>('preview_model_config', { json: inputJson.value }))
  } catch (e) { error.value = '读取或解析失败：' + errorText(e) }
  finally { busy.value = false }
}

async function parseImport() {
  if (busy.value) return
  busy.value = true
  invalidatePreview()
  try { rows.value = selectConfig(await invoke<ModelConfig>('preview_model_config', { json: inputJson.value })) }
  catch (e) { error.value = errorText(e) }
  finally { busy.value = false }
}

function selectAll(selected: boolean) {
  filteredRows.value.forEach(group => toggleConfigMatch(group, selected))
}

function togglePlatform(group: FilteredConfigPlatform, event: Event) {
  toggleConfigMatch(group, (event.target as HTMLInputElement).checked)
}

function toggleModel(row: SelectablePlatform, item: SelectablePlatform['models'][number], selected: boolean) {
  toggleConfigModel(row, item, Boolean(selected))
}

function groupSelectedCount(group: FilteredConfigPlatform) {
  return group.row.selected ? group.models.filter(m => m.selected).length : 0
}

async function executeImport() {
  if (busy.value || !selection.value.platforms.length) return
  busy.value = true
  error.value = ''
  message.value = ''
  try {
    const result = await invoke<{ added_platforms: number; reused_platforms: number; added_models: number; skipped_models: number; platforms_without_key: number }>('import_model_config', { config: selection.value })
    rows.value = []
    inputJson.value = ''
    message.value = `已新增 ${result.added_platforms} 个平台、${result.added_models} 个模型；复用 ${result.reused_platforms} 个平台，跳过 ${result.skipped_models} 个已有模型。`
      + (result.platforms_without_key ? ` ${result.platforms_without_key} 个平台未配置 Key，需要鉴权的平台请在使用前补填。` : '')
    emit('imported')
  } catch (e) { error.value = '导入失败：' + errorText(e) }
  finally { busy.value = false }
}

async function copyExport() {
  error.value = ''
  message.value = ''
  try { await navigator.clipboard.writeText(outputJson.value); message.value = '已复制 JSON' }
  catch { error.value = '复制失败，请保存 JSON 文件，或展开预览手动复制。' }
}

async function saveExport() {
  if (busy.value) return
  busy.value = true
  error.value = ''
  message.value = ''
  try {
    const targetPath = await save({ defaultPath: 'free-api-model-config.json', filters: [{ name: 'JSON 配置', extensions: ['json'] }] })
    if (!targetPath) return
    await invoke('save_model_config_file', { config: selection.value, targetPath })
    message.value = 'JSON 配置文件已保存'
  } catch (e) { error.value = '保存失败：' + errorText(e) }
  finally { busy.value = false }
}
</script>

<style scoped>
.config-transfer-actions { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
button { display: inline-flex; align-items: center; justify-content: center; gap: 7px; min-height: 34px; border: 1px solid var(--border-2); border-radius: 8px; background: var(--theme-bg-panel); color: var(--text-1); padding: 7px 13px; cursor: pointer; font: 500 12px/1.4 var(--font); white-space: nowrap; transition: background .15s, border-color .15s, color .15s; }
button:hover:not(:disabled) { background: var(--surface-2); border-color: var(--border-3); }
button:disabled, .is-disabled { opacity: .45; cursor: default; }
button:focus-visible, input:focus-visible, textarea:focus-visible, summary:focus-visible, .config-file-label:focus-within { outline: 2px solid var(--accent-light); outline-offset: 3px; }
svg { width: 16px; height: 16px; flex-shrink: 0; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
.config-entry-actions { gap: 6px; }
.config-entry-actions button { background: var(--bg-2); }
.config-entry-actions svg { color: var(--text-2); width: 14px; height: 14px; }

.config-transfer-overlay { position: fixed; inset: 0; z-index: 10000; background: rgba(8, 10, 18, .48); backdrop-filter: blur(8px); display: flex; align-items: center; justify-content: center; padding: 24px; animation: config-fade .16s ease-out; }
.config-transfer-dialog { width: 720px; max-width: 100%; max-height: min(880px, 90vh); display: flex; flex-direction: column; color: var(--text-1); background: var(--theme-bg-panel); border: 1px solid var(--border-2); border-radius: 18px; box-shadow: var(--shadow-lg); font: 13px/1.6 var(--font); overflow: hidden; animation: config-panel-enter .18s var(--motion-ease, ease-out); }
@keyframes config-fade { from { opacity: 0; } to { opacity: 1; } }
@keyframes config-panel-enter { from { opacity: 0; transform: translateY(8px); } to { opacity: 1; transform: translateY(0); } }
header, footer { display: flex; align-items: center; gap: 12px; flex-shrink: 0; }
header { justify-content: space-between; padding: 22px 24px 20px; border-bottom: 1px solid var(--border-1); }
.config-heading { display: flex; align-items: center; gap: 12px; min-width: 0; }
.config-heading-icon { width: 40px; height: 40px; display: grid; place-items: center; flex-shrink: 0; background: var(--accent-glow); border-radius: 12px; color: var(--accent-light); }
.config-heading-icon svg { width: 21px; height: 21px; }
h3 { margin: 0; font-size: 17px; line-height: 1.5; font-weight: 600; letter-spacing: -.3px; }
p.config-subtitle { margin: 3px 0 0; color: var(--text-3); font-size: 12px; }
button.config-close { width: 30px; min-height: 30px; padding: 0; border: 0; color: var(--text-3); background: transparent; }
button.config-close:hover { background: var(--surface-2); color: var(--text-1); }
.config-transfer-body { min-height: 0; padding: 20px 24px; overflow-y: auto; scrollbar-width: thin; scrollbar-color: var(--border-3) transparent; }
p { margin: 10px 0; line-height: 1.6; }
.config-note, small { color: var(--text-3); font-size: 11.5px; }
label { display: flex; align-items: center; gap: 10px; cursor: pointer; }
input[type=checkbox] { appearance: none; -webkit-appearance: none; position: relative; box-sizing: border-box; width: 16px; height: 16px; flex-shrink: 0; margin: 0; border: 1px solid var(--border-3); border-radius: 5px; background: var(--theme-bg-panel); cursor: pointer; transition: background .12s, border-color .12s; }
input[type=checkbox]:checked, input[type=checkbox]:indeterminate { background: var(--accent); border-color: var(--accent); }
input[type=checkbox]:checked::after { content: ''; position: absolute; width: 4px; height: 7px; left: 5px; top: 2px; border: solid var(--on-accent, white); border-width: 0 1.5px 1.5px 0; transform: rotate(45deg); }
input[type=checkbox]:indeterminate::after { content: ''; position: absolute; left: 3px; right: 3px; top: 6px; height: 2px; background: var(--on-accent, white); border: 0; transform: none; }
input:disabled { cursor: default; opacity: .5; }

.config-file-label { position: relative; min-height: 62px; gap: 12px; padding: 12px 16px; border: 1px dashed var(--border-3); border-radius: 11px; background: var(--surface-1); transition: border-color .15s, background .15s; }
.config-file-label:hover:not(.is-disabled) { background: var(--accent-glow); border-color: var(--accent); }
.config-file-label > svg { width: 24px; height: 24px; color: var(--accent-light); }
.config-file-label strong { font-size: 13px; font-weight: 550; }
.config-file-label small { display: block; margin-top: 2px; }
.config-file-format { margin-left: auto; padding: 3px 7px; color: var(--text-3); border: 1px solid var(--border-1); border-radius: 5px; font: 10px/1.5 monospace; }
.config-file-label input { position: absolute; inset: 0; opacity: 0; cursor: pointer; width: 100%; }
.config-file-label.is-compact { min-height: 44px; padding: 8px 12px; }
.config-file-label.is-compact > svg { width: 20px; height: 20px; }
.config-editor { margin-top: 14px; border: 1px solid var(--border-2); border-radius: 10px; overflow: hidden; }
.config-editor-header, .config-editor-footer { display: flex; justify-content: space-between; align-items: center; gap: 10px; padding: 9px 12px; }
.config-editor-header { color: var(--text-2); font-size: 12px; border-bottom: 1px solid var(--border-1); background: var(--surface-1); }
.config-editor-footer { border-top: 1px solid var(--border-1); }
textarea { box-sizing: border-box; display: block; width: 100%; min-height: 112px; resize: vertical; margin: 10px 0; padding: 12px 14px; border: 1px solid var(--border-2); border-radius: 8px; color: var(--text-1); background: var(--theme-bg-base); font: 11.5px/1.7 'SFMono-Regular', Consolas, monospace; tab-size: 2; }
.config-editor textarea { margin: 0; border: 0; border-radius: 0; }
.config-editor.has-preview { margin-top: 10px; }
.config-editor.has-preview textarea { min-height: 52px; max-height: 72px; padding: 8px 12px; }
textarea::placeholder { color: var(--text-4); }
p.config-import-note { margin-bottom: 0; font-size: 11px; }

.config-key-choice { display: flex; gap: 11px; padding: 12px 14px; border: 1px solid var(--border-1); border-radius: 11px; background: var(--surface-1); }
.config-key-choice:hover { border-color: var(--border-2); }
.config-key-choice.is-enabled { border-color: color-mix(in srgb, var(--accent) 35%, transparent); background: var(--accent-glow); }
.config-key-choice input { order: 3; margin-left: auto; }
.config-key-icon { width: 30px; height: 32px; display: grid; place-items: center; color: var(--text-3); flex-shrink: 0; }
.config-key-icon svg { width: 19px; height: 19px; }
.config-key-copy { min-width: 0; }
.config-key-copy strong { display: block; font-size: 12.5px; font-weight: 550; color: var(--text-1); }
.config-key-copy small { display: block; margin-top: 2px; font-size: 11px; }

.config-selection { margin-top: 20px; }
.config-selection-header { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin-bottom: 10px; }
.config-selection-header strong { font-size: 12px; font-weight: 600; color: var(--text-2); }
.config-selection-header .config-note { font-size: 11px; }
.config-picker { border: 1px solid var(--border-2); border-radius: 12px; overflow: hidden; background: var(--theme-bg-base); }
.config-search-header { padding: 12px; border-bottom: 1px solid var(--border-1); background: var(--theme-bg-panel); }
.config-search-input-wrap { position: relative; display: flex; align-items: center; }
.config-search-input-wrap > svg { position: absolute; left: 12px; width: 15px; height: 15px; pointer-events: none; color: var(--text-3); }
.config-search-input-wrap input { box-sizing: border-box; width: 100%; min-width: 0; height: 36px; padding: 8px 32px 8px 36px; border: 1px solid var(--border-1); border-radius: 8px; background: var(--theme-bg-base); color: var(--text-1); font: 12px/1.4 var(--font); outline: none; transition: border-color .15s, box-shadow .15s; }
.config-search-input-wrap input:focus { border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-glow); }
.config-search-input-wrap input::placeholder { color: var(--text-4); }
.config-search-input-wrap input::-webkit-search-cancel-button { -webkit-appearance: none; }
button.config-clear-search { position: absolute; right: 7px; width: 23px; min-height: 23px; padding: 0; border: 0; color: var(--text-3); background: transparent; font-size: 10px; }
.config-search-meta { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin-top: 9px; color: var(--text-3); font-size: 11px; }
.config-search-meta .config-transfer-actions { gap: 2px; }
button.config-text-button { min-height: 24px; padding: 3px 7px; border: 0; background: transparent; color: var(--text-2); font-size: 11px; border-radius: 5px; }
button.config-text-button:hover:not(:disabled) { background: var(--surface-2); color: var(--accent-light); }
.config-picker-list { max-height: min(400px, 45vh); overflow-y: auto; overscroll-behavior: contain; scrollbar-width: thin; scrollbar-color: var(--border-3) transparent; padding: 10px; }
.config-platform { overflow: hidden; border: 1px solid var(--border-1); border-radius: 9px; background: var(--theme-bg-panel); }
.config-platform + .config-platform { margin-top: 10px; }
.config-platform-heading { gap: 10px; padding: 11px 12px; background: var(--surface-1); }
.config-platform-heading:hover { background: var(--surface-2); }
.config-platform-icon { box-sizing: content-box; width: 16px; height: 16px; padding: 7px; background: var(--accent-glow); border-radius: 8px; color: var(--accent-light); }
.config-platform-info { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 1px; }
.config-platform-info strong { font-size: 12.5px; font-weight: 600; color: var(--text-1); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.config-endpoint { display: block; color: var(--text-3); font: 10.5px/1.6 'SFMono-Regular', Consolas, monospace; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.config-count-badge, .config-key-badge, .config-disabled-badge { color: var(--text-3); padding: 2px 7px; background: var(--surface-2); border: 1px solid var(--border-1); border-radius: 5px; font-size: 10px; font-weight: 400; white-space: nowrap; }
.config-count-badge { color: var(--text-2); font-variant-numeric: tabular-nums; }
.config-count-badge > span { color: var(--text-3); margin-left: 2px; }
.config-model-list { padding: 4px 7px 7px; border-top: 1px solid var(--border-1); }
.config-model { position: relative; display: flex; align-items: center; gap: 11px; min-height: 42px; padding: 5px 12px; margin: 2px 0 0; border-radius: 6px; transition: background .12s; }
.config-model:hover { background: var(--surface-2); }
.config-model.is-selected { background: color-mix(in srgb, var(--accent) 7%, transparent); }
.config-model.is-selected:hover { background: color-mix(in srgb, var(--accent) 12%, transparent); }
.config-model-name { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 1px; }
.config-model-title { font-size: 12.5px; line-height: 1.6; font-weight: 500; color: var(--text-1); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.config-model-name small { color: var(--text-3); font: 10.5px/1.5 'SFMono-Regular', Consolas, monospace; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.config-disabled-badge { font-size: 10px; flex-shrink: 0; }
.config-empty-models { display: block; padding: 10px 16px; color: var(--text-3); font-size: 11px; }
.config-filter-note { padding: 9px 13px; border-top: 1px solid var(--border-1); color: var(--text-3); font-size: 11px; background: var(--theme-bg-panel); }
.config-search-empty { min-height: 144px; padding: 20px 12px; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 7px; color: var(--text-3); font-size: 12px; text-align: center; }
.config-search-empty > svg { width: 24px; height: 24px; margin-bottom: 4px; }
.config-search-empty strong { color: var(--text-2); font-weight: 500; }
.config-search-empty button { margin-top: 5px; }

footer { justify-content: flex-end; padding: 15px 24px; border-top: 1px solid var(--border-1); background: var(--surface-1); gap: 8px; }
.config-footer-note { margin-right: auto; color: var(--text-3); font-size: 11px; }
button.config-primary { background: var(--accent); border-color: var(--accent); color: var(--on-accent); box-shadow: var(--shadow-accent); }
button.config-primary:hover:not(:disabled) { background: var(--accent-dark); border-color: var(--accent-dark); }
.config-error, .config-success { padding: 10px 13px; border-radius: 8px; overflow-wrap: anywhere; font-size: 12px; }
.config-error { color: var(--theme-status-danger); background: color-mix(in srgb, var(--theme-status-danger) 8%, transparent); border: 1px solid color-mix(in srgb, var(--theme-status-danger) 20%, transparent); }
.config-success { color: var(--theme-status-success); background: color-mix(in srgb, var(--theme-status-success) 8%, transparent); border: 1px solid color-mix(in srgb, var(--theme-status-success) 20%, transparent); }
details { margin-top: 14px; }
summary { cursor: pointer; color: var(--text-3); font-size: 11.5px; padding: 3px 0; }
summary:hover { color: var(--text-1); }

@media (max-width: 600px) {
  .config-transfer-overlay { padding: 10px; }
  .config-transfer-dialog { max-height: 95dvh; border-radius: 14px; }
  header { padding: 18px 16px; }
  h3 { font-size: 16px; }
  p.config-subtitle { font-size: 11px; }
  .config-heading-icon { width: 34px; height: 34px; border-radius: 10px; }
  .config-transfer-body { padding: 16px; }
  .config-selection-header, .config-search-meta { flex-wrap: wrap; gap: 6px; }
  .config-count-badge > span, .config-key-badge, .config-footer-note { display: none; }
  .config-platform-heading { gap: 8px; padding: 10px; }
  .config-platform-icon { display: none; }
  .config-picker-list { padding: 7px; }
  .config-model { padding: 7px 9px; }
  footer { padding: 12px 16px; flex-wrap: wrap; }
  footer button { flex: 1; padding: 8px 10px; }
}
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after { animation: none !important; transition: none !important; }
}
@media (forced-colors: active) {
  input[type=checkbox] { appearance: auto; -webkit-appearance: auto; }
  input[type=checkbox]::after { display: none; }
}
</style>
