<template>
  <div class="chat-layout">
    <!-- Sidebar: 会话列表 -->
    <aside class="sidebar" :class="{ collapsed: sidebarCollapsed }">
      <div class="sidebar-header">
        <button class="btn btn-primary btn-new-chat" @click="newSession" :disabled="allModels.length === 0">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M12 5v14M5 12h14"/></svg>
          <span v-if="!sidebarCollapsed">新对话</span>
        </button>
        <button class="btn-icon" @click="sidebarCollapsed = !sidebarCollapsed" style="flex-shrink:0">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path v-if="sidebarCollapsed" d="M13 17l5-5-5-5M6 17l5-5-5-5"/>
            <path v-else d="M11 17l-5-5 5-5M18 17l-5-5 5-5"/>
          </svg>
        </button>
      </div>
      <div class="session-list" v-if="!sidebarCollapsed">
        <div v-if="sessions.length === 0" class="sidebar-empty">
          <span style="font-size:28px;opacity:0.4">💬</span>
          <span>暂无对话</span>
        </div>
        <div v-for="s in sessions" :key="s.id" class="session-item" :class="{ active: currentSessionId === s.id }" @click="selectSession(s.id)">
          <div class="session-item-content">
            <div class="session-title" v-if="renamingId !== s.id">{{ s.title }}</div>
            <input v-else class="session-rename-input" v-model="renameText" @keydown.enter="doRename(s.id)" @blur="doRename(s.id)" @click.stop autofocus />
            <div class="session-meta">{{ s.message_count }} 条消息</div>
          </div>
          <div class="session-actions" @click.stop>
            <button class="btn-icon btn-xs" @click="startRename(s)" title="重命名">✏️</button>
            <button class="btn-icon btn-xs btn-del" @click="removeSession(s.id)" title="删除">🗑️</button>
          </div>
        </div>
      </div>
    </aside>

    <!-- Main Chat Area -->
    <section class="chat-main">
      <!-- 顶部工具栏：选择平台 → 选择模型 -->
      <div class="chat-toolbar">
        <div class="toolbar-left">
          <span class="toolbar-title" v-if="currentSession">{{ currentSession.title }}</span>
          <span class="toolbar-title dim" v-else>选择或创建一个对话</span>
        </div>
        <div class="toolbar-right">
          <select class="form-select toolbar-select" v-model="selectedModelId" @change="onModelSwitch">
            <option value="" disabled>选择模型</option>
            <optgroup v-for="group in groupedModels" :key="group.platform" :label="group.platform">
              <option v-for="m in group.models" :key="m.id" :value="m.id">{{ m.name }}</option>
            </optgroup>
          </select>
        </div>
      </div>

      <!-- 消息区域 -->
      <div class="messages-container" ref="messagesContainer">
        <!-- 欢迎页 -->
        <div v-if="!currentSession" class="welcome-screen">
          <div class="welcome-icon">⚡</div>
          <h2 class="welcome-title">Free API Chat</h2>
          <p class="welcome-desc">高性能本地 AI 对话工具</p>
          <div class="welcome-tips">
            <div class="tip-card" @click="allModels.length > 0 ? newSession() : $router.push('/apis')">
              <div class="tip-icon">{{ allModels.length > 0 ? '💬' : '🔌' }}</div>
              <div class="tip-text">{{ allModels.length > 0 ? '开始新对话' : '先添加平台和模型' }}</div>
            </div>
            <div class="tip-card" @click="$router.push('/apis')">
              <div class="tip-icon">⚙️</div>
              <div class="tip-text">管理平台与模型</div>
            </div>
            <div class="tip-card">
              <div class="tip-icon">📎</div>
              <div class="tip-text">支持上传图片附件</div>
            </div>
          </div>
        </div>

        <!-- 消息列表 -->
        <div v-else class="messages-list">
          <div v-for="msg in messages" :key="msg.id" class="message-row" :class="msg.role">
            <div class="message-avatar">
              <span v-if="msg.role === 'user'">👤</span>
              <span v-else>🤖</span>
            </div>
            <div class="message-body">
              <div class="message-header">
                <span class="message-role">{{ msg.role === 'user' ? '你' : 'AI' }}</span>
                <span class="message-time">{{ formatTime(msg.created_at) }}</span>
              </div>
              <div v-if="msg.attachments" class="message-attachments">
                <div v-for="(att, i) in parseAttachments(msg.attachments)" :key="i" class="attach-chip">📎 {{ att.name }}</div>
              </div>
              <div class="message-content md-content" v-html="renderMarkdown(msg.content)"></div>
            </div>
          </div>

          <!-- 流式输出 -->
          <div v-if="streaming || thinkingContent" class="message-row assistant">
            <div class="message-avatar"><span>🤖</span></div>
            <div class="message-body">
              <div class="message-header">
                <span class="message-role">AI</span>
                <span class="message-model pulsing" v-if="streaming">
                  {{ isThinking ? '🧠 思考中…' : '生成中…' }}
                </span>
              </div>

              <!-- 🧠 思考过程展示 -->
              <div v-if="thinkingContent" class="thinking-block" :class="{ collapsed: thinkingCollapsed && !isThinking }">
                <div class="thinking-header" @click="thinkingCollapsed = !thinkingCollapsed">
                  <span class="thinking-icon" :class="{ spinning: isThinking }">🧠</span>
                  <span class="thinking-title">{{ isThinking ? '思考中...' : '思考过程' }}</span>
                  <span class="thinking-toggle">{{ thinkingCollapsed ? '▼ 展开' : '▲ 收起' }}</span>
                </div>
                <div v-show="!thinkingCollapsed || isThinking" class="thinking-body">
                  <div class="thinking-content" v-html="renderMarkdown(thinkingContent)"></div>
                </div>
              </div>

              <div class="message-content md-content" v-if="streamContent" v-html="renderMarkdown(streamContent)"></div>
              <div class="message-content" v-else-if="!thinkingContent">
                <div class="loading-dots"><span></span><span></span><span></span></div>
              </div>
            </div>
          </div>

          <!-- 📊 Token 用量统计栏 -->
          <div v-if="tokenUsage" class="token-usage-bar">
            <div class="token-stats">
              <span class="token-label">📊 Token</span>
              <span class="token-item">⬆️ <strong>{{ tokenUsage.prompt_tokens }}</strong></span>
              <span class="token-sep">·</span>
              <span class="token-item">⬇️ <strong>{{ tokenUsage.completion_tokens }}</strong></span>
              <span class="token-sep">·</span>
              <span class="token-item">∑ <strong>{{ tokenUsage.total_tokens }}</strong></span>
            </div>
            <div class="token-ctx">
              <div class="ctx-bar">
                <div class="ctx-fill" :style="{ width: Math.min(tokenUsage.usage_percent, 100) + '%' }" :class="{ warning: tokenUsage.usage_percent > 70, danger: tokenUsage.usage_percent > 90 }"></div>
              </div>
              <span class="ctx-text">{{ tokenUsage.total_tokens }}/{{ tokenUsage.context_window }} ({{ tokenUsage.usage_percent.toFixed(1) }}%)</span>
            </div>
          </div>
        </div>
      </div>

      <!-- 输入区 -->
      <div class="input-area" v-if="currentSession">
        <div v-if="pendingFiles.length > 0" class="attachments-bar">
          <div v-for="(f, i) in pendingFiles" :key="i" class="attach-preview">
            <img v-if="f.mime_type.startsWith('image/')" :src="'data:' + f.mime_type + ';base64,' + f.data_base64" class="attach-thumb" />
            <span v-else class="attach-file-icon">📄</span>
            <span class="attach-name">{{ f.name }}</span>
            <button class="attach-remove" @click="pendingFiles.splice(i, 1)">×</button>
          </div>
        </div>
        <div class="input-row">
          <button class="btn-icon" @click="pickFile" title="上传附件" :disabled="streaming">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48"/></svg>
          </button>
          <textarea
            ref="inputRef"
            class="chat-input"
            v-model="userInput"
            :placeholder="streaming ? 'AI 回复中…' : '输入消息，Shift+Enter 换行…'"
            :disabled="streaming"
            @keydown="onKeydown"
            rows="1"
          ></textarea>
          <button class="btn-send" @click="sendMessage" :disabled="(!userInput.trim() && pendingFiles.length === 0) || streaming || !selectedModelId">
            <svg v-if="!streaming" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><line x1="22" y1="2" x2="11" y2="13"/><polygon points="22 2 15 22 11 13 2 9 22 2"/></svg>
            <svg v-else width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><rect x="6" y="6" width="12" height="12" rx="2"/></svg>
          </button>
        </div>
        <div class="input-hint">
          <span v-if="chatError" class="input-error">{{ chatError }}</span>
          <span v-else>Enter 发送 · Shift+Enter 换行 · 右上角切换模型</span>
        </div>
      </div>
    </section>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, nextTick, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { marked } from 'marked'

interface ModelOption {
  id: string; name: string; display_name: string; platform_id: string; platform_name: string;
}
interface ChatSession {
  id: string; title: string; model_id: string; created_at: string; updated_at: string; message_count: number;
}
interface ChatMessage {
  id: string; session_id: string; role: string; content: string; attachments: string | null; created_at: string; model_id: string | null;
}
interface FileAttachment {
  name: string; mime_type: string; data_base64: string;
}

const allModels = ref<ModelOption[]>([])
const sessions = ref<ChatSession[]>([])
const messages = ref<ChatMessage[]>([])
const currentSessionId = ref('')
const selectedModelId = ref('')

const userInput = ref('')
const streaming = ref(false)
const streamContent = ref('')
const chatError = ref('')
const pendingFiles = ref<FileAttachment[]>([])
const sidebarCollapsed = ref(false)
const renamingId = ref('')
const renameText = ref('')

interface TokenUsageInfo {
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  context_window: number;
  usage_percent: number;
}
const tokenUsage = ref<TokenUsageInfo | null>(null)
const thinkingContent = ref('')
const isThinking = ref(false)
const thinkingCollapsed = ref(false)

const messagesContainer = ref<HTMLElement>()
const inputRef = ref<HTMLTextAreaElement>()

const currentSession = computed(() => sessions.value.find(s => s.id === currentSessionId.value))

// 按平台分组
const groupedModels = computed(() => {
  const groups: { platform: string; models: ModelOption[] }[] = []
  const map = new Map<string, ModelOption[]>()
  for (const m of allModels.value) {
    if (!map.has(m.platform_name)) map.set(m.platform_name, [])
    map.get(m.platform_name)!.push(m)
  }
  for (const [platform, models] of map) {
    groups.push({ platform, models })
  }
  return groups
})

onMounted(async () => {
  allModels.value = await invoke<ModelOption[]>('get_all_models_with_platform')
  sessions.value = await invoke<ChatSession[]>('get_sessions')
  if (allModels.value.length > 0) selectedModelId.value = allModels.value[0].id

  listen<{ session_id: string; content: string; done: boolean }>('chat-stream', (event) => {
    if (event.payload.done) { /* done handled by invoke promise */ }
    else {
      streamContent.value += event.payload.content
      scrollToBottom()
    }
  })

  // 监听思考过程
  listen<{ session_id: string; content?: string; status: string; full_thinking?: string }>('chat-thinking', (event) => {
    const { status, content } = event.payload
    if (status === 'start') {
      isThinking.value = true
      thinkingCollapsed.value = false
      thinkingContent.value = ''
    } else if (status === 'streaming' && content) {
      thinkingContent.value += content
      scrollToBottom()
    } else if (status === 'done') {
      isThinking.value = false
      thinkingCollapsed.value = true  // 思考完成后自动收起
    }
  })

  // 监听 Token 用量统计
  listen<TokenUsageInfo & { session_id: string }>('chat-token-usage', (event) => {
    tokenUsage.value = {
      prompt_tokens: event.payload.prompt_tokens,
      completion_tokens: event.payload.completion_tokens,
      total_tokens: event.payload.total_tokens,
      context_window: event.payload.context_window,
      usage_percent: event.payload.usage_percent,
    }
  })
})

async function selectSession(id: string) {
  currentSessionId.value = id
  messages.value = await invoke<ChatMessage[]>('get_messages', { sessionId: id })
  const s = sessions.value.find(s => s.id === id)
  if (s?.model_id) selectedModelId.value = s.model_id
  await nextTick()
  scrollToBottom()
}

async function newSession() {
  if (!selectedModelId.value && allModels.value.length > 0) selectedModelId.value = allModels.value[0].id
  if (!selectedModelId.value) return
  const session = await invoke<ChatSession>('create_session', { title: '新对话', modelId: selectedModelId.value })
  sessions.value.unshift(session)
  currentSessionId.value = session.id
  messages.value = []
  chatError.value = ''
  await nextTick()
  inputRef.value?.focus()
}

async function removeSession(id: string) {
  await invoke('delete_session', { id })
  sessions.value = sessions.value.filter(s => s.id !== id)
  if (currentSessionId.value === id) { currentSessionId.value = ''; messages.value = [] }
}

function startRename(s: ChatSession) { renamingId.value = s.id; renameText.value = s.title }
async function doRename(id: string) {
  if (renameText.value.trim()) {
    await invoke('rename_session', { id, title: renameText.value.trim() })
    const s = sessions.value.find(x => x.id === id)
    if (s) s.title = renameText.value.trim()
  }
  renamingId.value = ''
}

function onModelSwitch() { chatError.value = '' }

async function sendMessage() {
  if ((!userInput.value.trim() && pendingFiles.value.length === 0) || streaming.value || !selectedModelId.value) return
  chatError.value = ''
  const text = userInput.value.trim()
  userInput.value = ''

  const attachJson = pendingFiles.value.length > 0 ? JSON.stringify(pendingFiles.value.map(f => ({ name: f.name, mime_type: f.mime_type }))) : null
  const userMsg = await invoke<ChatMessage>('save_message', {
    sessionId: currentSessionId.value, role: 'user', content: text || '(附件)', attachments: attachJson, modelId: selectedModelId.value
  })
  messages.value.push(userMsg)

  const sess = sessions.value.find(s => s.id === currentSessionId.value)
  if (sess && sess.message_count === 0) {
    const title = text.slice(0, 30) || '新对话'
    await invoke('rename_session', { id: sess.id, title })
    sess.title = title
  }
  if (sess) sess.message_count++
  scrollToBottom()

  const apiMsgs = messages.value.filter(m => m.role === 'user' || m.role === 'assistant').map(m => ({ role: m.role, content: m.content }))
  const filesToSend = [...pendingFiles.value]
  pendingFiles.value = []
  streaming.value = true
  streamContent.value = ''
  tokenUsage.value = null
  thinkingContent.value = ''
  isThinking.value = false
  thinkingCollapsed.value = false

  try {
    const fullResp = await invoke<string>('send_chat', {
      sessionId: currentSessionId.value, modelId: selectedModelId.value,
      messages: apiMsgs, attachments: filesToSend.length > 0 ? filesToSend : null,
    })
    const aiMsg = await invoke<ChatMessage>('save_message', {
      sessionId: currentSessionId.value, role: 'assistant', content: fullResp, attachments: null, modelId: selectedModelId.value
    })
    messages.value.push(aiMsg)
    if (sess) sess.message_count++
  } catch (e: any) {
    chatError.value = typeof e === 'string' ? e : (e?.message || '发送失败')
  } finally {
    streaming.value = false
    streamContent.value = ''
    scrollToBottom()
  }
}

async function pickFile() {
  const input = document.createElement('input')
  input.type = 'file'
  input.accept = 'image/*,.pdf,.txt,.md,.json,.csv'
  input.multiple = true
  input.onchange = async () => {
    if (!input.files) return
    for (const file of Array.from(input.files)) {
      const reader = new FileReader()
      reader.onload = () => {
        const base64 = (reader.result as string).split(',')[1]
        pendingFiles.value.push({ name: file.name, mime_type: file.type || 'application/octet-stream', data_base64: base64 })
      }
      reader.readAsDataURL(file)
    }
  }
  input.click()
}

function onKeydown(e: KeyboardEvent) { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); sendMessage() } }
function scrollToBottom() { nextTick(() => { const el = messagesContainer.value; if (el) el.scrollTop = el.scrollHeight }) }
function formatTime(iso: string) { try { return new Date(iso).toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' }) } catch { return '' } }
function renderMarkdown(text: string) { if (!text) return ''; try { return marked.parse(text, { breaks: true }) } catch { return text.replace(/\n/g, '<br>') } }
function parseAttachments(json: string | null): { name: string }[] { if (!json) return []; try { return JSON.parse(json) } catch { return [] } }

watch(userInput, () => { nextTick(() => { const el = inputRef.value; if (el) { el.style.height = 'auto'; el.style.height = Math.min(el.scrollHeight, 150) + 'px' } }) })
</script>

<style scoped>
.chat-layout { display: flex; flex: 1; overflow: hidden; height: 100%; }

.sidebar { width: 280px; min-width: 280px; background: var(--bg-1); border-right: 1px solid var(--border-1); display: flex; flex-direction: column; transition: all 0.2s; }
.sidebar.collapsed { width: 56px; min-width: 56px; }
.sidebar-header { display: flex; gap: 8px; padding: 14px; align-items: center; }
.btn-new-chat { flex: 1; justify-content: center; }
.sidebar.collapsed .btn-new-chat { padding: 8px; }
.session-list { flex: 1; overflow-y: auto; padding: 4px 8px; display: flex; flex-direction: column; gap: 2px; }
.sidebar-empty { display: flex; flex-direction: column; align-items: center; gap: 8px; padding: 32px 16px; color: var(--text-4); font-size: 13px; }
.session-item { display: flex; align-items: center; gap: 4px; padding: 10px 12px; border-radius: var(--radius-sm); cursor: pointer; transition: all 0.15s; }
.session-item:hover { background: var(--surface-2); }
.session-item.active { background: rgba(108,99,255,0.1); border: 1px solid rgba(108,99,255,0.15); }
.session-item-content { flex: 1; min-width: 0; }
.session-title { font-size: 13px; font-weight: 500; color: var(--text-1); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.session-rename-input { background: var(--bg-2); border: 1px solid var(--accent); border-radius: 4px; color: var(--text-1); font-size: 13px; padding: 2px 6px; width: 100%; outline: none; font-family: var(--font); }
.session-meta { font-size: 11px; color: var(--text-4); margin-top: 2px; }
.session-actions { display: flex; gap: 2px; opacity: 0; transition: opacity 0.15s; }
.session-item:hover .session-actions { opacity: 1; }
.btn-xs { padding: 4px; border-radius: 4px; background: none; border: none; cursor: pointer; }
.btn-del:hover { color: var(--red-light) !important; }

.chat-main { flex: 1; display: flex; flex-direction: column; overflow: hidden; background: var(--bg-0); }
.chat-toolbar { display: flex; align-items: center; justify-content: space-between; padding: 10px 20px; border-bottom: 1px solid var(--border-1); background: var(--bg-1); gap: 16px; flex-shrink: 0; }
.toolbar-title { font-size: 14px; font-weight: 600; color: var(--text-1); }
.toolbar-title.dim { color: var(--text-4); font-weight: 400; }
.toolbar-right { display: flex; align-items: center; gap: 8px; }
.toolbar-select { width: auto; min-width: 260px; padding: 6px 32px 6px 12px; font-size: 12px; }

.messages-container { flex: 1; overflow-y: auto; }
.welcome-screen { display: flex; flex-direction: column; align-items: center; justify-content: center; height: 100%; gap: 12px; padding: 32px; }
.welcome-icon { font-size: 56px; filter: drop-shadow(0 0 24px var(--accent-glow)); animation: pulse 3s ease infinite; }
.welcome-title { font-size: 28px; font-weight: 700; background: linear-gradient(135deg, var(--text-1), var(--accent-light)); -webkit-background-clip: text; background-clip: text; -webkit-text-fill-color: transparent; }
.welcome-desc { color: var(--text-3); font-size: 14px; }
.welcome-tips { display: flex; gap: 14px; margin-top: 28px; }
.tip-card { display: flex; flex-direction: column; align-items: center; gap: 10px; padding: 20px 28px; background: var(--bg-1); border: 1px solid var(--border-1); border-radius: var(--radius-lg); cursor: pointer; transition: all 0.18s; min-width: 140px; }
.tip-card:hover { border-color: var(--accent); background: rgba(108,99,255,0.06); transform: translateY(-3px); box-shadow: var(--shadow-accent); }
.tip-icon { font-size: 24px; }
.tip-text { font-size: 13px; color: var(--text-2); text-align: center; }

.messages-list { padding: 20px 0; }
.message-row { display: flex; gap: 14px; padding: 16px 28px; transition: background 0.15s; }
.message-row:hover { background: var(--surface-1); }
.message-row.assistant { background: rgba(108,99,255,0.03); }
.message-row.assistant:hover { background: rgba(108,99,255,0.06); }
.message-avatar { width: 32px; height: 32px; border-radius: var(--radius-sm); background: var(--bg-2); border: 1px solid var(--border-2); display: flex; align-items: center; justify-content: center; font-size: 15px; flex-shrink: 0; }
.message-row.assistant .message-avatar { background: linear-gradient(135deg, var(--bg-3), var(--bg-4)); border-color: var(--accent-glow); }
.message-body { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 6px; }
.message-header { display: flex; align-items: center; gap: 8px; }
.message-role { font-size: 12px; font-weight: 600; color: var(--text-2); }
.message-time { font-size: 11px; color: var(--text-4); }
.message-model { font-size: 10px; color: var(--accent-light); background: rgba(108,99,255,0.1); padding: 1px 6px; border-radius: 99px; }
.message-attachments { display: flex; flex-wrap: wrap; gap: 6px; }
.attach-chip { display: inline-flex; align-items: center; gap: 4px; padding: 3px 10px; background: var(--surface-2); border: 1px solid var(--border-2); border-radius: 6px; font-size: 11px; color: var(--text-2); }
.message-content { font-size: 14px; line-height: 1.7; }

.input-area { padding: 12px 20px 16px; border-top: 1px solid var(--border-1); background: var(--bg-1); flex-shrink: 0; }
.attachments-bar { display: flex; flex-wrap: wrap; gap: 8px; padding-bottom: 10px; }
.attach-preview { display: flex; align-items: center; gap: 6px; background: var(--bg-2); border: 1px solid var(--border-2); border-radius: var(--radius-sm); padding: 6px 10px; font-size: 12px; color: var(--text-2); }
.attach-thumb { width: 32px; height: 32px; border-radius: 4px; object-fit: cover; }
.attach-file-icon { font-size: 18px; }
.attach-name { max-width: 120px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.attach-remove { background: none; border: none; color: var(--text-4); cursor: pointer; font-size: 16px; }
.attach-remove:hover { color: var(--red-light); }
.input-row { display: flex; align-items: flex-end; gap: 10px; background: var(--bg-2); border: 1px solid var(--border-2); border-radius: var(--radius-lg); padding: 6px 6px 6px 4px; transition: border-color 0.18s, box-shadow 0.18s; }
.input-row:focus-within { border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-glow); }
.chat-input { flex: 1; background: transparent; border: none; outline: none; color: var(--text-1); font-family: var(--font); font-size: 14px; padding: 8px 10px; resize: none; max-height: 150px; line-height: 1.5; }
.chat-input::placeholder { color: var(--text-4); }
.chat-input:disabled { opacity: 0.5; }
.btn-send { width: 38px; height: 38px; border-radius: 10px; background: linear-gradient(135deg, var(--accent), var(--accent-dark)); border: none; color: #fff; cursor: pointer; display: flex; align-items: center; justify-content: center; flex-shrink: 0; transition: all 0.18s; box-shadow: 0 2px 12px var(--accent-glow); }
.btn-send:hover:not(:disabled) { transform: scale(1.06); box-shadow: 0 4px 20px var(--accent-glow); }
.btn-send:disabled { opacity: 0.35; cursor: not-allowed; }
.input-hint { font-size: 11px; color: var(--text-4); padding: 6px 4px 0; }
.input-error { color: var(--red-light); }

/* 📊 Token 用量统计栏 */
.token-usage-bar {
  margin: 0 28px 8px;
  padding: 10px 16px;
  background: var(--bg-1);
  border: 1px solid var(--border-1);
  border-radius: var(--radius-sm);
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  animation: fadeSlideUp 0.3s ease;
}
@keyframes fadeSlideUp {
  from { opacity: 0; transform: translateY(8px); }
  to { opacity: 1; transform: translateY(0); }
}
.token-stats {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 11px;
  color: var(--text-3);
}
.token-label {
  font-weight: 700;
  color: var(--text-2);
  margin-right: 4px;
}
.token-item strong {
  color: var(--text-1);
  font-weight: 600;
}
.token-sep {
  color: var(--border-2);
  font-size: 10px;
}
.token-ctx {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-shrink: 0;
}
.ctx-bar {
  width: 100px;
  height: 6px;
  background: var(--bg-3);
  border-radius: 99px;
  overflow: hidden;
}
.ctx-fill {
  height: 100%;
  border-radius: 99px;
  background: var(--accent);
  transition: width 0.5s ease, background 0.3s ease;
}
.ctx-fill.warning {
  background: var(--orange, #ffa43d);
}
.ctx-fill.danger {
  background: var(--red, #ff4757);
}
.ctx-text {
  font-size: 10px;
  color: var(--text-4);
  white-space: nowrap;
  font-family: 'JetBrains Mono', 'Fira Code', monospace;
}

/* 🧠 思考过程展示 */
.thinking-block {
  background: rgba(108, 99, 255, 0.04);
  border: 1px solid rgba(108, 99, 255, 0.15);
  border-left: 3px solid var(--accent);
  border-radius: 8px;
  overflow: hidden;
  transition: all 0.3s ease;
  margin-bottom: 8px;
}
.thinking-block.collapsed {
  border-color: rgba(108, 99, 255, 0.08);
  background: rgba(108, 99, 255, 0.02);
}
.thinking-header {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
  cursor: pointer;
  user-select: none;
  transition: background 0.15s;
}
.thinking-header:hover {
  background: rgba(108, 99, 255, 0.06);
}
.thinking-icon {
  font-size: 14px;
  transition: transform 0.3s;
}
.thinking-icon.spinning {
  animation: spin 1.5s linear infinite;
}
@keyframes spin {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
}
.thinking-title {
  font-size: 11px;
  font-weight: 600;
  color: var(--accent-light);
  flex: 1;
}
.thinking-toggle {
  font-size: 10px;
  color: var(--text-4);
}
.thinking-body {
  padding: 0 12px 10px;
  max-height: 300px;
  overflow-y: auto;
  transition: max-height 0.3s ease;
}
.thinking-content {
  font-size: 12px;
  line-height: 1.6;
  color: var(--text-3);
  font-style: italic;
  white-space: pre-wrap;
  word-break: break-word;
}
.thinking-content :deep(p) {
  margin: 4px 0;
}
</style>
