<template>
  <div class="manager-layout">
    <!-- 左侧: 平台列表 -->
    <aside class="platform-panel">
      <div class="panel-header">
        <h3 class="panel-title">🌐 AI 平台</h3>
        <button class="btn btn-primary btn-sm" @click="showAddPlatform = true">
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M12 5v14M5 12h14"/></svg>
          添加平台
        </button>
      </div>

      <div class="platform-list">
        <div v-if="platforms.length === 0" class="empty-hint">
          <span style="font-size:28px;opacity:0.4">🔗</span>
          <span>点击上方添加平台</span>
        </div>
        <div
          v-for="p in platforms" :key="p.id"
          class="platform-item"
          :class="{ active: selectedPlatformId === p.id }"
          @click="selectPlatform(p.id)"
        >
          <div class="p-left">
            <div class="p-icon">{{ getPlatformEmoji(p.name) }}</div>
            <div class="p-info">
              <div class="p-name">{{ p.name }}</div>
              <div class="p-url">{{ p.base_url }}</div>
            </div>
          </div>
          <div class="p-actions">
            <span class="p-model-count">{{ getModelCount(p.id) }} 个模型</span>
            <button class="btn-icon btn-xs" @click.stop="editPlatform(p)" title="编辑">
              <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z"/></svg>
            </button>
            <button class="btn-icon btn-xs btn-del" @click.stop="confirmDeletePlatform(p)" title="删除">
              <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2"/></svg>
            </button>
          </div>
        </div>
      </div>
    </aside>

    <!-- 右侧: 模型列表 -->
    <section class="model-panel">
      <template v-if="selectedPlatformId && currentPlatform">
        <div class="panel-header">
          <div>
            <h3 class="panel-title">{{ currentPlatform.name }} — 模型管理</h3>
            <p class="panel-subtitle">添加模型名称即可，Key 和地址会自动继承平台配置</p>
          </div>
        </div>

        <!-- 快速添加模型 -->
        <div class="add-model-bar">
          <div class="add-model-input-wrap">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 5v14M5 12h14"/></svg>
            <input
              class="add-model-input"
              v-model="newModelName"
              placeholder="输入模型标识，如 google/gemini-2.0-flash 或 openai/gpt-4o"
              @keydown.enter="addModel"
            />
          </div>
          <button class="btn btn-primary" @click="addModel" :disabled="!newModelName.trim()">添加模型</button>
        </div>

        <!-- 模型列表 -->
        <div class="model-grid" v-if="models.length > 0">
          <div v-for="m in models" :key="m.id" class="model-card">
            <div class="m-left">
              <div class="m-icon-box" :class="m.status">
                <span class="status-dot"></span>
              </div>
              <div class="m-info">
                <div class="m-name">{{ m.name }}</div>
                <div class="m-meta">
                  <span v-if="m.status === 'online'" class="badge badge-online">在线 {{ m.latency_ms }}ms</span>
                  <span v-else-if="m.status === 'offline'" class="badge badge-offline">离线</span>
                  <span v-else-if="m.status === 'testing'" class="badge badge-testing">检测中…</span>
                  <span v-else class="badge badge-unknown">未检测</span>
                  <span class="m-tokens">{{ m.max_tokens }} tokens</span>
                </div>
              </div>
            </div>
            <div class="m-actions">
              <button class="btn btn-success btn-sm" @click="testSingleModel(m)" :disabled="m.status === 'testing'">
                <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" :class="{ spinning: m.status === 'testing' }">
                  <path d="M21 2v6h-6M3 12a9 9 0 0 1 15-6.7L21 8"/>
                </svg>
                检测
              </button>
              <button class="btn btn-danger btn-sm" @click="deleteModel(m.id)">
                <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/></svg>
                删除
              </button>
            </div>
          </div>
        </div>
        <div v-else class="empty-models">
          <span style="font-size:36px;opacity:0.3">🤖</span>
          <p>此平台下暂无模型</p>
          <p style="font-size:12px;color:var(--text-4)">在上方输入模型标识并回车即可添加</p>
        </div>
      </template>

      <!-- 未选中平台时 -->
      <div v-else class="select-hint">
        <div class="hint-glow">
          <span style="font-size:48px">⚡</span>
        </div>
        <h3>选择一个平台开始管理模型</h3>
        <p>在左侧添加或选择一个 AI 平台（如 OpenRouter、DeepSeek），<br/>然后在右侧快速录入该平台支持的模型</p>
      </div>
    </section>

    <!-- 弹窗: 添加/编辑平台 -->
    <Teleport to="body">
      <Transition name="fade">
        <div class="modal-overlay" v-if="showAddPlatform || editingPlatform" @click.self="closePlatformModal">
          <div class="modal">
            <div class="modal-header">
              <span class="modal-title">{{ editingPlatform ? '编辑平台' : '添加新平台' }}</span>
              <button class="btn-icon" @click="closePlatformModal">
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M18 6L6 18M6 6l12 12"/></svg>
              </button>
            </div>
            <div class="modal-body">
              <div class="form-group">
                <label class="form-label">平台名称 *</label>
                <input class="form-input" v-model="pForm.name" placeholder="例如：OpenRouter、DeepSeek、硅基流动" />
              </div>
              <div class="form-group">
                <label class="form-label">API 地址 (Base URL) *</label>
                <input class="form-input" v-model="pForm.base_url" placeholder="https://openrouter.ai/api/v1" />
              </div>
              <div class="form-group">
                <label class="form-label">API Key *</label>
                <div class="key-wrap">
                  <input
                    :type="showKey ? 'text' : 'password'"
                    class="form-input"
                    v-model="pForm.api_key"
                    placeholder="sk-..."
                    style="padding-right:44px"
                  />
                  <button class="key-toggle" @click="showKey = !showKey" type="button">
                    <svg v-if="!showKey" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"/><circle cx="12" cy="12" r="3"/></svg>
                    <svg v-else width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8"/><path d="M1 1l22 22"/></svg>
                  </button>
                </div>
              </div>
              <div v-if="formError" class="form-error">⚠️ {{ formError }}</div>
            </div>
            <div class="modal-footer">
              <button class="btn btn-ghost" @click="closePlatformModal">取消</button>
              <button class="btn btn-primary" @click="savePlatform">
                {{ editingPlatform ? '更新' : '添加' }}
              </button>
            </div>
          </div>
        </div>
      </Transition>

      <!-- 删除确认弹窗 -->
      <Transition name="fade">
        <div class="modal-overlay" v-if="deletingPlatform" @click.self="deletingPlatform = null">
          <div class="modal" style="max-width:400px">
            <div class="modal-header">
              <span class="modal-title">确认删除</span>
            </div>
            <div class="modal-body">
              <p style="color:var(--text-2)">确定要删除平台 <strong style="color:var(--text-1)">{{ deletingPlatform.name }}</strong> 吗？<br/>该平台下的所有模型也会被删除。</p>
            </div>
            <div class="modal-footer">
              <button class="btn btn-ghost" @click="deletingPlatform = null">取消</button>
              <button class="btn btn-danger" @click="doDeletePlatform">确认删除</button>
            </div>
          </div>
        </div>
      </Transition>
    </Teleport>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { invoke } from '@tauri-apps/api/core'

interface PlatformData {
  id: string; name: string; base_url: string; api_key: string; created_at: string;
}
interface ModelData {
  id: string; platform_id: string; name: string; display_name: string;
  max_tokens: number; temperature: number; enabled: boolean; status: string; latency_ms: number;
}

const platforms = ref<PlatformData[]>([])
const models = ref<ModelData[]>([])
const selectedPlatformId = ref('')
const showAddPlatform = ref(false)
const editingPlatform = ref<PlatformData | null>(null)
const deletingPlatform = ref<PlatformData | null>(null)
const newModelName = ref('')
const showKey = ref(false)
const formError = ref('')

const pForm = ref({ name: '', base_url: '', api_key: '' })
const currentPlatform = computed(() => platforms.value.find(p => p.id === selectedPlatformId.value))

function getPlatformEmoji(name: string) {
  const n = name.toLowerCase()
  if (n.includes('openrouter')) return '🌈'
  if (n.includes('deepseek')) return '🐋'
  if (n.includes('openai') || n.includes('chatgpt')) return '🟢'
  if (n.includes('claude') || n.includes('anthropic')) return '🟠'
  if (n.includes('google') || n.includes('gemini')) return '🔵'
  if (n.includes('硅基') || n.includes('siliconflow')) return '💎'
  return '🌐'
}

function getModelCount(platformId: string) {
  return models.value.filter(m => m.platform_id === platformId).length
}

// ---- 平台 ----
async function loadPlatforms() {
  platforms.value = await invoke<PlatformData[]>('get_platforms')
}

async function selectPlatform(id: string) {
  selectedPlatformId.value = id
  models.value = await invoke<ModelData[]>('get_models', { platformId: id })
}

function editPlatform(p: PlatformData) {
  editingPlatform.value = p
  pForm.value = { name: p.name, base_url: p.base_url, api_key: p.api_key }
  showKey.value = false
  formError.value = ''
}

function closePlatformModal() {
  showAddPlatform.value = false
  editingPlatform.value = null
  pForm.value = { name: '', base_url: '', api_key: '' }
  formError.value = ''
}

async function savePlatform() {
  formError.value = ''
  if (!pForm.value.name.trim()) { formError.value = '平台名称不能为空'; return }
  if (!pForm.value.base_url.trim()) { formError.value = 'API 地址不能为空'; return }
  if (!pForm.value.api_key.trim()) { formError.value = 'API Key 不能为空'; return }

  try {
    if (editingPlatform.value) {
      await invoke('update_platform', {
        id: editingPlatform.value.id,
        name: pForm.value.name,
        baseUrl: pForm.value.base_url,
        apiKey: pForm.value.api_key,
      })
    } else {
      await invoke('add_platform', {
        name: pForm.value.name,
        baseUrl: pForm.value.base_url,
        apiKey: pForm.value.api_key,
      })
    }
    closePlatformModal()
    await loadPlatforms()
  } catch (e: any) {
    formError.value = '保存失败: ' + (e?.message || e)
  }
}

function confirmDeletePlatform(p: PlatformData) {
  deletingPlatform.value = p
}

async function doDeletePlatform() {
  if (!deletingPlatform.value) return
  await invoke('delete_platform', { id: deletingPlatform.value.id })
  if (selectedPlatformId.value === deletingPlatform.value.id) {
    selectedPlatformId.value = ''
    models.value = []
  }
  deletingPlatform.value = null
  await loadPlatforms()
}

// ---- 模型 ----
async function addModel() {
  if (!newModelName.value.trim() || !selectedPlatformId.value) return
  await invoke('add_model', {
    platformId: selectedPlatformId.value,
    name: newModelName.value.trim(),
    displayName: newModelName.value.trim(),
  })
  newModelName.value = ''
  await selectPlatform(selectedPlatformId.value)
}

async function deleteModel(id: string) {
  await invoke('delete_model', { id })
  await selectPlatform(selectedPlatformId.value)
}

async function testSingleModel(m: ModelData) {
  const idx = models.value.findIndex(x => x.id === m.id)
  if (idx >= 0) models.value[idx].status = 'testing'
  try {
    const result = await invoke<{ status: string; latency_ms: number; message: string }>('test_model', { modelId: m.id })
    if (idx >= 0) {
      models.value[idx].status = result.status
      models.value[idx].latency_ms = result.latency_ms
    }
  } catch {
    if (idx >= 0) models.value[idx].status = 'offline'
  }
}

onMounted(loadPlatforms)
</script>

<style scoped>
.manager-layout {
  display: flex;
  height: 100%;
  background: var(--bg-0);
}

/* ---- 左侧平台面板 ---- */
.platform-panel {
  width: 320px;
  min-width: 320px;
  border-right: 1px solid var(--border-1);
  background: var(--bg-1);
  display: flex;
  flex-direction: column;
}

.panel-header {
  padding: 18px 16px;
  border-bottom: 1px solid var(--border-1);
  display: flex;
  justify-content: space-between;
  align-items: center;
  flex-shrink: 0;
}

.panel-title {
  font-size: 15px;
  font-weight: 600;
  color: var(--text-1);
}

.panel-subtitle {
  font-size: 12px;
  color: var(--text-3);
  margin-top: 3px;
}

.platform-list {
  flex: 1;
  overflow-y: auto;
  padding: 8px;
}

.platform-item {
  padding: 12px 14px;
  border-radius: var(--radius-md);
  cursor: pointer;
  margin-bottom: 4px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  transition: all 0.18s;
  border: 1px solid transparent;
}

.platform-item:hover {
  background: var(--surface-2);
}

.platform-item.active {
  background: rgba(108, 99, 255, 0.1);
  border-color: rgba(108, 99, 255, 0.2);
}

.p-left {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
  flex: 1;
}

.p-icon {
  font-size: 22px;
  flex-shrink: 0;
}

.p-info { min-width: 0; }

.p-name {
  font-size: 14px;
  font-weight: 600;
  color: var(--text-1);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.p-url {
  font-size: 10px;
  color: var(--text-4);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  margin-top: 2px;
  font-family: monospace;
}

.p-actions {
  display: flex;
  align-items: center;
  gap: 4px;
  flex-shrink: 0;
}

.p-model-count {
  font-size: 10px;
  color: var(--text-4);
  background: var(--surface-1);
  border: 1px solid var(--border-1);
  padding: 2px 6px;
  border-radius: 99px;
  margin-right: 4px;
}

.p-actions .btn-icon {
  opacity: 0;
  transition: opacity 0.15s;
}

.platform-item:hover .p-actions .btn-icon {
  opacity: 1;
}

.btn-xs { padding: 4px; border-radius: 4px; }
.btn-del:hover { color: var(--red-light) !important; background: rgba(255,71,87,0.1) !important; }

/* ---- 右侧模型面板 ---- */
.model-panel {
  flex: 1;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  padding: 24px;
  gap: 20px;
  overflow-y: auto;
}

.add-model-bar {
  display: flex;
  gap: 10px;
  align-items: center;
}

.add-model-input-wrap {
  flex: 1;
  display: flex;
  align-items: center;
  gap: 8px;
  background: var(--bg-2);
  border: 1px solid var(--border-2);
  border-radius: var(--radius-sm);
  padding: 0 12px;
  transition: border-color 0.18s, box-shadow 0.18s;
  color: var(--text-3);
}

.add-model-input-wrap:focus-within {
  border-color: var(--accent);
  box-shadow: 0 0 0 3px var(--accent-glow);
}

.add-model-input {
  flex: 1;
  background: none;
  border: none;
  outline: none;
  color: var(--text-1);
  font-family: var(--font);
  font-size: 14px;
  padding: 10px 0;
}

.add-model-input::placeholder { color: var(--text-4); }

.model-grid {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.model-card {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 14px 18px;
  background: var(--bg-1);
  border: 1px solid var(--border-1);
  border-radius: var(--radius-md);
  transition: border-color 0.18s, box-shadow 0.18s, transform 0.18s;
}

.model-card:hover {
  border-color: var(--border-3);
  box-shadow: var(--shadow-sm);
  transform: translateX(2px);
}

.m-left {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
}

.m-icon-box {
  width: 10px;
  height: 10px;
  border-radius: 50%;
  flex-shrink: 0;
}

.m-icon-box.online .status-dot { background: var(--green); box-shadow: 0 0 8px var(--green); }
.m-icon-box.offline .status-dot { background: var(--red); }
.m-icon-box.testing .status-dot { background: var(--orange); animation: pulse 1s ease infinite; }
.m-icon-box.unknown .status-dot { background: var(--text-4); }

.status-dot {
  display: block;
  width: 10px;
  height: 10px;
  border-radius: 50%;
}

.m-info { min-width: 0; }

.m-name {
  font-size: 14px;
  font-weight: 600;
  color: var(--text-1);
  font-family: 'JetBrains Mono', monospace;
}

.m-meta {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 3px;
}

.m-tokens {
  font-size: 11px;
  color: var(--text-4);
}

.m-actions {
  display: flex;
  gap: 6px;
  flex-shrink: 0;
}

.empty-models {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 8px;
  color: var(--text-3);
  font-size: 14px;
}

.empty-hint {
  display: flex;
  flex-direction: column;
  align-items: center;
  padding: 40px 16px;
  gap: 10px;
  color: var(--text-4);
  font-size: 13px;
}

.select-hint {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  text-align: center;
  gap: 14px;
  color: var(--text-3);
}

.select-hint h3 {
  font-size: 18px;
  font-weight: 600;
  color: var(--text-2);
}

.select-hint p {
  font-size: 13px;
  color: var(--text-4);
  line-height: 1.8;
}

.hint-glow {
  width: 80px;
  height: 80px;
  background: linear-gradient(135deg, rgba(108,99,255,0.1), rgba(0,212,170,0.1));
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  box-shadow: 0 0 40px var(--accent-glow);
}

/* ---- Key toggle ---- */
.key-wrap { position: relative; }

.key-toggle {
  position: absolute;
  right: 10px;
  top: 50%;
  transform: translateY(-50%);
  background: none;
  border: none;
  cursor: pointer;
  color: var(--text-3);
  padding: 4px;
  display: flex;
  align-items: center;
}
.key-toggle:hover { color: var(--text-1); }

.form-error {
  font-size: 13px;
  color: var(--red-light);
  background: rgba(255,71,87,0.08);
  border: 1px solid rgba(255,71,87,0.2);
  border-radius: var(--radius-sm);
  padding: 8px 12px;
}

.btn-sm { padding: 5px 11px; font-size: 12px; }
</style>
