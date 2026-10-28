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
            <label class="config-file-label" :class="{ 'is-disabled': busy }">
              <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M14 3H5v18h14V8zm0 0v5h5M12 17v-5m-2 2 2-2 2 2" /></svg>
              <span><strong>选择 JSON 文件</strong><small>点击选择已导出的配置，自动解析并预览</small></span>
              <span class="config-file-format">.json</span>
              <input type="file" accept=".json,application/json" aria-label="选择 JSON 文件" :disabled="busy" @change="readConfigFile" />
            </label>
            <div class="config-editor">
              <div class="config-editor-header"><span>或粘贴配置内容</span><button type="button" class="config-text-button" :disabled="busy" @click="useExample">填入格式示例</button></div>
              <textarea v-model="inputJson" aria-label="待导入的 JSON 配置" placeholder="在此粘贴 JSON 配置…" spellcheck="false" :disabled="busy" @input="invalidatePreview" />
              <div class="config-editor-footer"><span class="config-note">解析后可勾选要导入的内容</span><button type="button" :disabled="busy || !inputJson.trim()" @click="parseImport">解析并预览</button></div>
            </div>
            <p class="config-note config-import-note">重复项自动跳过，保留已有配置。未包含的 API Key 可稍后补填。</p>
          </template>
          <template v-else>
            <label class="config-key-choice"><input v-model="includeKeys" type="checkbox" :disabled="busy" /><span><strong>包含 API Key</strong><small>默认关闭；开启后以明文导出，仅用于自己的备份或迁移。</small></span></label>
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
                    <input type="checkbox" :checked="group.row.selected && (group.models.length === group.row.models.length || group.models.every(m => m.selected))" :disabled="busy" :indeterminate="group.row.selected && group.models.some(m => m.selected) && group.models.some(m => !m.selected)" @change="togglePlatform(group, $event)" />
                    <svg viewBox="0 0 24 24" class="config-platform-icon" aria-hidden="true"><circle cx="12" cy="12" r="9" /><path d="M3 12h18M12 3a18 18 0 0 1 0 18 18 18 0 0 1 0-18" /></svg>
                    <span class="config-platform-info"><strong>{{ group.row.platform.name }}</strong><span class="config-endpoint">{{ group.row.platform.base_url }}</span></span>
                    <span class="config-count-badge">{{ group.models.length }}</span>
                    <span v-if="mode === 'import'" class="config-key-badge">{{ group.row.platform.api_key?.trim() ? '含 Key' : '未含 Key' }}</span>
                  </label>
                  <label v-for="item in group.models" :key="item.model.name" class="config-model" :class="{ 'is-selected': group.row.selected && item.selected, 'is-muted': !group.row.selected }">
                    <input v-model="item.selected" type="checkbox" :disabled="busy || !group.row.selected" />
                    <span class="config-model-name">{{ item.model.display_name || item.model.name }}<small v-if="item.model.display_name && item.model.display_name !== item.model.name">{{ item.model.name }}</small></span>
                    <small v-if="!item.model.enabled" class="config-disabled-badge">已禁用</small>
                  </label>
                  <small v-if="!group.models.length">仅平台配置，未包含模型</small>
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
import { computed, nextTick, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { save } from '@tauri-apps/plugin-dialog'
import { MODEL_CONFIG_EXAMPLE, selectConfig, selectedConfig, filterConfigPlatforms, toggleConfigMatch, type ModelConfig, type SelectablePlatform, type FilteredConfigPlatform } from '../utils/modelConfigTransfer'

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
button { display: inline-flex; align-items: center; justify-content: center; gap: 7px; min-height: 34px; border: 1px solid rgba(255, 255, 255, 0.12); border-radius: var(--radius-sm, 8px); background: rgba(255, 255, 255, 0.05); color: var(--text-1); padding: 7px 14px; cursor: pointer; font: 500 12.5px/1.4 var(--font); white-space: nowrap; transition: all .18s ease; }
button:hover:not(:disabled) { background: rgba(255, 255, 255, 0.09); border-color: rgba(255, 255, 255, 0.22); transform: translateY(-0.5px); }
button:disabled, .is-disabled { opacity: .45; cursor: default; transform: none; }
button:focus-visible, input:focus-visible, textarea:focus-visible, .config-file-label:focus-within { outline: 2px solid var(--accent-light); outline-offset: 2px; }
svg { width: 16px; height: 16px; flex-shrink: 0; fill: none; stroke: currentColor; stroke-width: 1.8; stroke-linecap: round; stroke-linejoin: round; }
.config-entry-actions { gap: 6px; }
.config-entry-actions button { background: var(--bg-2); }
.config-entry-actions svg { color: var(--text-2); width: 14px; height: 14px; }

/* 弹窗遮罩与主容器 */
.config-transfer-overlay { position: fixed; inset: 0; z-index: 10000; background: rgba(9, 9, 11, 0.75); backdrop-filter: blur(12px); display: flex; align-items: center; justify-content: center; padding: 24px; animation: overlay-fade .2s ease-out; }
.config-transfer-dialog { width: 700px; max-width: 100%; max-height: 88vh; display: flex; flex-direction: column; color: var(--text-1); background: #131316; border: 1px solid rgba(255, 255, 255, 0.14); border-radius: 20px; box-shadow: 0 24px 64px -12px rgba(0, 0, 0, 0.8), 0 0 0 1px rgba(255, 255, 255, 0.06); font: 13px/1.6 var(--font); overflow: hidden; }

@keyframes overlay-fade {
  from { opacity: 0; }
  to { opacity: 1; }
}

header, footer { display: flex; align-items: center; gap: 12px; padding: 13px 20px; flex-shrink: 0; }
header { justify-content: space-between; border-bottom: 1px solid rgba(255, 255, 255, 0.08); background: rgba(255, 255, 255, 0.02); }
.config-heading { display: flex; align-items: center; gap: 10px; min-width: 0; }
.config-heading-icon { width: 34px; height: 34px; display: grid; place-items: center; background: rgba(108, 99, 255, 0.12); border: 1px solid rgba(108, 99, 255, 0.25); border-radius: 10px; color: var(--accent-light, #8b83ff); }
.config-heading-icon svg { width: 18px; height: 18px; }
h3 { margin: 0; font-size: 15px; font-weight: 600; letter-spacing: -.2px; color: #f4f4f5; }
p.config-subtitle { margin: 1px 0 0; color: #a1a1aa; font-size: 11.5px; }
button.config-close { width: 28px; min-height: 28px; padding: 0; border: 0; border-radius: 7px; color: #a1a1aa; background: transparent; transition: all .15s ease; }
button.config-close:hover { background: rgba(255, 255, 255, 0.08); color: #f4f4f5; }

.config-transfer-body { padding: 14px 20px; overflow-y: auto; scrollbar-width: thin; }
p { margin: 8px 0; line-height: 1.5; }
.config-note, small { color: #a1a1aa; font-size: 11.5px; }
label { display: flex; align-items: center; gap: 10px; cursor: pointer; }
input[type=checkbox] { width: 15px; height: 15px; flex-shrink: 0; margin: 0; accent-color: var(--accent, #6c63ff); cursor: pointer; border-radius: 4px; }
input:disabled { cursor: default; }

.config-file-label { position: relative; min-height: 84px; gap: 14px; padding: 18px; border: 1px dashed rgba(255, 255, 255, 0.18); border-radius: 12px; background: rgba(255, 255, 255, 0.02); transition: all .18s ease; }
.config-file-label:hover:not(.is-disabled) { background: rgba(255, 255, 255, 0.05); border-color: var(--accent-light); }
.config-file-label > svg { width: 26px; height: 26px; color: #a1a1aa; }
.config-file-label strong, .config-key-choice strong { font-size: 13px; font-weight: 550; color: #f4f4f5; }
.config-file-label small, .config-key-choice small { display: block; margin-top: 3px; font-weight: 400; color: #a1a1aa; }
.config-file-format { margin-left: auto; padding: 3px 8px; color: #a1a1aa; background: rgba(255, 255, 255, 0.06); border-radius: 5px; font: 11px/1.6 monospace; }
.config-file-label input { position: absolute; inset: 0; opacity: 0; cursor: pointer; width: 100%; }

.config-editor { margin-top: 18px; border: 1px solid rgba(255, 255, 255, 0.1); border-radius: 12px; overflow: hidden; }
.config-editor-header, .config-editor-footer { display: flex; justify-content: space-between; align-items: center; gap: 10px; padding: 10px 14px; background: rgba(255, 255, 255, 0.02); }
.config-editor-header { color: #a1a1aa; font-size: 12px; border-bottom: 1px solid rgba(255, 255, 255, 0.08); }
.config-editor-footer { border-top: 1px solid rgba(255, 255, 255, 0.08); }
textarea { box-sizing: border-box; display: block; width: 100%; min-height: 170px; resize: vertical; margin: 10px 0; padding: 14px; border: 1px solid rgba(255, 255, 255, 0.12); border-radius: 8px; color: #f4f4f5; background: #0c0c0e; font: 12px/1.8 'SFMono-Regular', Consolas, monospace; tab-size: 2; }
.config-editor textarea { margin: 0; border: 0; border-radius: 0; }
textarea::placeholder { color: #71717a; }

/* 导出 Key 紧凑单行条 - 杜绝大面积侵占核心模型列表 */
.config-key-choice {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 7px 12px;
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.02);
  transition: all .15s ease;
}
.config-key-choice:hover { border-color: rgba(255, 255, 255, 0.18); background: rgba(255, 255, 255, 0.04); }
.config-key-choice input { align-self: center; margin: 0; }
.config-key-choice strong { font-size: 12.5px; font-weight: 550; color: #f4f4f5; display: inline-flex; align-items: center; gap: 6px; }
.config-key-choice small { display: inline; margin-left: 8px; margin-top: 0; font-weight: 400; color: #71717a; font-size: 11.5px; }
p.config-import-note { margin-bottom: 0; }

/* 列表选择区域 */
.config-selection { margin-top: 12px; }
.config-selection-header { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-bottom: 8px; }
.config-selection-header strong { font-size: 13px; font-weight: 550; color: #f4f4f5; }

.config-picker { border: 1px solid rgba(255, 255, 255, 0.12); border-radius: 12px; overflow: hidden; background: #0c0c0e; box-shadow: inset 0 1px 3px rgba(0, 0, 0, 0.5); }
.config-search-header { padding: 9px 12px 8px; border-bottom: 1px solid rgba(255, 255, 255, 0.08); background: rgba(255, 255, 255, 0.03); }
.config-search-input-wrap { position: relative; display: flex; align-items: center; }
.config-search-input-wrap > svg { position: absolute; left: 10px; width: 14px; height: 14px; pointer-events: none; color: #71717a; }
.config-search-input-wrap input { width: 100%; min-width: 0; padding: 6px 30px 6px 32px; border: 1px solid rgba(255, 255, 255, 0.12); border-radius: 7px; background: rgba(255, 255, 255, 0.04); color: #f4f4f5; font: 12px/1.5 var(--font); outline: none; transition: all .15s ease; }
.config-search-input-wrap input:focus { background: rgba(255, 255, 255, 0.06); border-color: var(--accent, #6c63ff); box-shadow: 0 0 0 2px var(--accent-glow, rgba(108, 99, 255, 0.25)); }
.config-search-input-wrap input::placeholder { color: #71717a; }
.config-search-input-wrap input::-webkit-search-cancel-button { -webkit-appearance: none; }
button.config-clear-search { position: absolute; right: 5px; padding: 0; width: 22px; min-height: 22px; border: 0; color: #71717a; background: transparent; font-size: 11.5px; }
button.config-clear-search:hover { color: #f4f4f5; }

.config-search-meta { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin-top: 6px; color: #a1a1aa; font-size: 11.5px; }
.config-search-meta .config-transfer-actions { gap: 10px; }
.config-search-meta button.config-text-button { font-size: 11.5px; }

/* 大幅提升模型列表可视区域，避免被过度压缩截断 */
.config-picker-list { max-height: min(440px, 52vh); overflow-y: auto; overscroll-behavior: contain; scrollbar-width: thin; padding: 6px 8px; }
.config-filter-note { padding: 9px 14px; border-top: 1px solid rgba(255, 255, 255, 0.08); color: #a1a1aa; font-size: 11px; background: rgba(255, 255, 255, 0.02); }
.config-search-empty { min-height: 180px; padding: 26px 12px; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 8px; color: #a1a1aa; font-size: 12px; text-align: center; }
.config-search-empty > svg { width: 26px; height: 26px; margin-bottom: 5px; color: #71717a; }
.config-search-empty strong { color: #f4f4f5; font-weight: 500; }
.config-search-empty button { margin-top: 6px; }
button.config-text-button { min-height: 24px; padding: 2px 6px; border: 0; background: transparent; color: #a1a1aa; font-size: 12px; border-radius: 4px; }
button.config-text-button:hover:not(:disabled) { background: rgba(255, 255, 255, 0.06); color: var(--accent-light, #8b83ff); }

/* 平台行与模型卡片精致化 */
.config-platform { padding: 4px 0 8px; }
.config-platform + .config-platform { margin-top: 8px; padding-top: 10px; border-top: 1px solid rgba(255, 255, 255, 0.06); }
.config-platform-heading { padding: 8px 10px; border-radius: 8px; gap: 10px; transition: background .15s ease; }
.config-platform-heading:hover { background: rgba(255, 255, 255, 0.04); }
.config-platform-icon { width: 16px; height: 16px; color: #a1a1aa; }
.config-platform-info { flex: 1; min-width: 0; }
.config-platform-info strong { font-size: 13px; font-weight: 600; color: #f4f4f5; }
.config-endpoint { display: block; margin-top: 1px; color: #71717a; font-size: 11px; font-family: monospace; overflow-wrap: anywhere; }
.config-count-badge, .config-key-badge, .config-disabled-badge { color: #a1a1aa; padding: 2px 8px; background: rgba(255, 255, 255, 0.06); border: 1px solid rgba(255, 255, 255, 0.08); border-radius: 99px; font-size: 11px; font-weight: 500; white-space: nowrap; }

/* 模型条目卡片极致现代重塑：现代微质感卡片布局与精致选中态 */
.config-model {
  position: relative;
  display: flex;
  align-items: center;
  gap: 12px;
  min-height: 44px;
  padding: 8px 14px;
  margin: 4px 6px 5px 28px;
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 10px;
  background: rgba(255, 255, 255, 0.02);
  transition: all 0.18s cubic-bezier(0.16, 1, 0.3, 1);
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.2);
}
.config-model:hover {
  background: rgba(255, 255, 255, 0.05);
  border-color: rgba(255, 255, 255, 0.16);
  transform: translateY(-1px);
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.35);
}
.config-model.is-selected {
  background: linear-gradient(90deg, rgba(108, 99, 255, 0.15) 0%, rgba(108, 99, 255, 0.06) 100%);
  border-color: rgba(108, 99, 255, 0.4);
  box-shadow: 0 2px 8px rgba(108, 99, 255, 0.12);
}
.config-model.is-selected::before {
  content: '';
  position: absolute;
  left: 0;
  top: 6px;
  bottom: 6px;
  width: 3px;
  border-radius: 0 3px 3px 0;
  background: var(--accent, #6c63ff);
}
.config-model.is-selected:hover {
  background: linear-gradient(90deg, rgba(108, 99, 255, 0.22) 0%, rgba(108, 99, 255, 0.1) 100%);
  border-color: rgba(108, 99, 255, 0.55);
  box-shadow: 0 4px 16px rgba(108, 99, 255, 0.2);
}
.config-model.is-muted {
  opacity: 0.35;
  filter: grayscale(0.6);
}
.config-model-name {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
  font-size: 13px;
  font-weight: 550;
  color: #f4f4f5;
  letter-spacing: -0.1px;
}
.config-model-name small {
  display: inline-block;
  font-size: 11px;
  font-weight: 400;
  color: #9ca3af;
  font-family: 'SFMono-Regular', Consolas, monospace;
  line-height: 1.3;
}
.config-platform > small { display: block; padding: 6px 34px; color: #71717a; }

/* 底部栏操作区 */
footer { justify-content: flex-end; border-top: 1px solid rgba(255, 255, 255, 0.08); background: rgba(255, 255, 255, 0.02); gap: 10px; padding: 16px 24px; }
.config-footer-note { margin-right: auto; color: #a1a1aa; font-size: 12px; }
button.config-primary {
  background: var(--accent, #6c63ff);
  border-color: var(--accent, #6c63ff);
  color: #fff;
  font-weight: 600;
  box-shadow: 0 4px 14px rgba(108, 99, 255, 0.35);
}
button.config-primary:hover:not(:disabled) {
  background: var(--accent-dark, #5950e8);
  border-color: var(--accent-dark, #5950e8);
  box-shadow: 0 6px 20px rgba(108, 99, 255, 0.45);
}
.config-error, .config-success { padding: 10px 14px; border-radius: 8px; overflow-wrap: anywhere; font-size: 12px; }
.config-error { color: #f87171; background: rgba(239, 68, 68, 0.1); border: 1px solid rgba(239, 68, 68, 0.25); }
.config-success { color: #4ade80; background: rgba(34, 197, 94, 0.1); border: 1px solid rgba(34, 197, 94, 0.25); }
details { margin-top: 18px; }
summary { cursor: pointer; color: #a1a1aa; font-size: 12px; transition: color .15s; }
summary:hover { color: #f4f4f5; }

@media (max-width: 600px) {
  .config-transfer-overlay { padding: 10px; }
  .config-transfer-dialog { max-height: 95vh; border-radius: 14px; }
  header, footer { padding: 14px 16px; }
  .config-transfer-body { padding: 16px; }
  .config-heading-icon, .config-footer-note { display: none; }
  .config-selection-header, .config-search-meta { flex-wrap: wrap; gap: 6px; }
  .config-file-format, .config-count-badge { display: none; }
  footer { flex-wrap: wrap; }
}
</style>
