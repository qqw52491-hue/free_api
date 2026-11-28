<template>
  <div class="external-agents-page">
    <div class="external-agents-content">
      <button v-if="canReturnToProject" type="button" class="back-link" @click="$emit('return-to-project')">
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="m14 6-6 6 6 6"/></svg>
        返回项目
      </button>
      <header class="page-heading">
        <div>
          <h1>外部 Agent</h1>
          <p>在这里维护连接，回到项目选择要派发的 Agent。</p>
        </div>
        <button v-if="loaded && agents.length" type="button" class="primary-button" :disabled="saving" @click="editAgent()">
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 5v14M5 12h14"/></svg>
          新建连接
        </button>
      </header>

      <p v-if="loading" class="notice" role="status">正在读取外部 Agent…</p>
      <div v-else-if="loadError" class="notice" role="alert">
        <p class="error-text">{{ loadError }}</p>
        <button type="button" class="secondary-button" @click="load">重试</button>
      </div>
      <template v-else-if="loaded">
        <ul v-if="agents.length" class="agent-list" aria-label="已保存的连接">
          <li v-for="agent in agents" :key="agent.id" class="agent-card">
            <div class="agent-icon" aria-hidden="true">
              <svg viewBox="0 0 24 24"><path d="m5 8 4 4-4 4m8 0h6"/></svg>
            </div>
            <div class="agent-identity">
              <div class="agent-name-row">
                <h2>{{ agent.name }}</h2>
                <span class="agent-state" :class="{ enabled: agent.enabled }">{{ agent.enabled ? '已启用' : '已停用' }}</span>
              </div>
              <code :title="agent.command">{{ agent.command }}</code>
            </div>
            <button type="button" class="secondary-button" :disabled="saving" :aria-label="`编辑 ${agent.name}`" @click="editAgent(agent)">
              编辑
            </button>
          </li>
        </ul>
        <p v-if="notice" class="notice success-text" role="status">{{ notice }}</p>

        <form v-if="draft" class="editor" @submit.prevent="save">
          <div class="editor-heading">
            <h2>{{ draft.id ? '编辑连接' : '添加本地 Codex' }}</h2>
            <p>使用这台电脑上的 Codex 及其登录配置。</p>
          </div>
          <label class="field">
            <span>连接名称</span>
            <input ref="nameInput" v-model.trim="draft.name" required :disabled="saving" autocomplete="off" />
          </label>
          <label class="field">
            <span>Codex 可执行文件路径</span>
            <input v-model.trim="draft.command" required :disabled="saving" autocomplete="off" spellcheck="false" placeholder="/opt/homebrew/bin/codex" />
          </label>
          <label class="checkbox-field">
            <input v-model="draft.enabled" type="checkbox" :disabled="saving" />
            <span>允许项目派发任务</span>
          </label>
          <p v-if="saveError" class="error-text" role="alert">{{ saveError }}</p>
          <div class="editor-actions">
            <button v-if="agents.length" type="button" class="secondary-button" :disabled="saving" @click="closeEditor">取消</button>
            <button type="submit" class="primary-button" :disabled="saving || !draft.name || !draft.command">
              {{ saving ? '正在保存…' : '保存连接' }}
            </button>
          </div>
        </form>
      </template>
    </div>
  </div>
</template>

<script setup lang="ts">
import { nextTick, onMounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import type { ExternalAgent } from "../types/externalAgent";

defineProps<{ canReturnToProject: boolean }>();
defineEmits<{ (event: "return-to-project"): void }>();

type AgentDraft = Omit<ExternalAgent, "id"> & { id?: string };
const agents = ref<ExternalAgent[]>([]);
const draft = ref<AgentDraft | null>(null);
const nameInput = ref<HTMLInputElement | null>(null);
const loading = ref(true);
const loaded = ref(false);
const saving = ref(false);
const loadError = ref("");
const saveError = ref("");
const notice = ref("");

function newCodexDraft(): AgentDraft {
  return { name: "本地 Codex", command: "/opt/homebrew/bin/codex", enabled: true };
}

async function load() {
  loading.value = true;
  loadError.value = "";
  try {
    agents.value = await invoke<ExternalAgent[]>("list_external_agents");
    loaded.value = true;
    if (!agents.value.length) draft.value = newCodexDraft();
  } catch (error) {
    loadError.value = `读取连接失败：${String(error)}`;
  } finally {
    loading.value = false;
  }
}

function editAgent(agent?: ExternalAgent) {
  draft.value = agent ? { ...agent } : newCodexDraft();
  saveError.value = "";
  notice.value = "";
  void nextTick(() => nameInput.value?.focus());
}

function closeEditor() {
  draft.value = null;
  saveError.value = "";
}

async function save() {
  if (!draft.value || !draft.value.name || !draft.value.command || saving.value) return;
  saving.value = true;
  saveError.value = "";
  notice.value = "";
  try {
    const agent = await invoke<ExternalAgent>("save_external_agent", { agent: draft.value });
    const index = agents.value.findIndex(item => item.id === agent.id);
    if (index < 0) agents.value.push(agent);
    else agents.value[index] = agent;
    closeEditor();
    notice.value = agent.enabled ? `已保存 ${agent.name}，可以在项目中选择。` : `已停用 ${agent.name}，新任务将无法派发到此连接。`;
  } catch (error) {
    saveError.value = `保存连接失败：${String(error)}`;
  } finally {
    saving.value = false;
  }
}

onMounted(load);
</script>

<style scoped>
.external-agents-page {
  height: 100%;
  overflow: auto;
  background: var(--theme-bg-base, #111318);
  color: var(--theme-text-primary, #e6e8ef);
}
.external-agents-content {
  max-width: 860px;
  margin: 0 auto;
  padding: 42px 36px;
}
.page-heading, .agent-card, .agent-name-row, .editor-actions, .checkbox-field {
  display: flex;
  align-items: center;
}
.page-heading { justify-content: space-between; gap: 20px; margin-bottom: 24px; }
h1, h2, p { margin: 0; }
h1 { font-size: 28px; line-height: 1.25; }
h2 { font-size: 15px; line-height: 1.35; }
p, code { color: var(--theme-text-muted, #9399a8); font-size: 13px; line-height: 1.5; }
.page-heading p, .editor-heading p { margin-top: 7px; }
.agent-list { display: grid; gap: 10px; padding: 0; margin: 0; list-style: none; }
.agent-card, .notice, .editor {
  background: var(--theme-bg-panel, #191c23);
  border: 1px solid rgba(var(--theme-foreground-rgb, 255, 255, 255), .13);
  border-radius: 12px;
}
.agent-card { gap: 12px; padding: 13px; }
.agent-icon {
  display: grid;
  place-items: center;
  flex: 0 0 34px;
  height: 34px;
  border-radius: 8px;
  background: rgba(129, 140, 248, .16);
  color: var(--theme-status-accent, #b6bcff);
}
svg { width: 16px; height: 16px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
.agent-identity { display: grid; gap: 3px; min-width: 0; flex: 1; }
.agent-name-row { gap: 8px; flex-wrap: wrap; }
.agent-name-row h2 { overflow-wrap: anywhere; }
.agent-identity code { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; font-size: 12px; }
.agent-state { color: var(--theme-text-muted, #9399a8); font-size: 11px; white-space: nowrap; }
.agent-state.enabled, .success-text { color: var(--theme-status-success, #7acba8); }
.notice { margin: 14px 0; padding: 12px 15px; }
.notice button { margin-top: 9px; }
.editor { display: grid; gap: 16px; margin-top: 16px; padding: 20px; }
.field { display: grid; gap: 6px; font-size: 13px; }
.field input {
  min-width: 0;
  color: inherit;
  font: inherit;
  padding: 9px 11px;
  background: var(--theme-bg-base, #111318);
  border: 1px solid rgba(var(--theme-foreground-rgb, 255, 255, 255), .19);
  border-radius: 8px;
}
.checkbox-field { gap: 7px; width: fit-content; font-size: 13px; }
.checkbox-field input { accent-color: #7265df; }
.editor-actions { justify-content: flex-end; gap: 9px; }
button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 5px;
  min-height: 36px;
  padding: 6px 12px;
  border: 1px solid transparent;
  border-radius: 8px;
  font: inherit;
  font-size: 13px;
  white-space: nowrap;
  cursor: pointer;
}
.primary-button { background: #5948cf; color: #fff; }
.primary-button:hover:not(:disabled) { background: #4d3abf; }
.secondary-button {
  background: rgba(var(--theme-foreground-rgb, 255, 255, 255), .05);
  border-color: rgba(var(--theme-foreground-rgb, 255, 255, 255), .14);
  color: inherit;
}
.secondary-button:hover:not(:disabled) { background: rgba(var(--theme-foreground-rgb, 255, 255, 255), .1); }
.back-link { padding-left: 0; background: transparent; color: var(--theme-text-secondary, #b8bdce); margin-bottom: 12px; }
button:disabled { opacity: .5; cursor: default; }
button:focus-visible, input:focus-visible { outline: 2px solid #818cf8; outline-offset: 2px; }
.error-text { color: var(--theme-status-danger, #f2a2a2); overflow-wrap: anywhere; }
@media (max-width: 600px) {
  .external-agents-content { padding: 22px 16px; }
  .page-heading { align-items: flex-start; gap: 12px; }
  h1 { font-size: 23px; }
  .editor { padding: 15px; }
}
</style>
