<template>
  <section class="project-agent" aria-labelledby="project-external-agent-heading">
    <div class="project-agent-row">
      <div class="project-agent-copy">
        <h2 id="project-external-agent-heading">外部 Agent</h2>
        <p>为本项目选择可派发只读子任务的 Agent。</p>
      </div>
      <div class="project-agent-actions">
        <select
          v-if="loaded && (enabledAgents.length || selectedId)"
          v-model="selectedId"
          aria-label="本项目的外部 Agent"
          :disabled="saving"
          @change="save"
        >
          <option value="">不使用外部 Agent</option>
          <option v-if="unavailableSelection" :value="selectedId" disabled>{{ unavailableSelection }}</option>
          <option v-for="agent in enabledAgents" :key="agent.id" :value="agent.id">{{ agent.name }}</option>
        </select>
        <button type="button" :disabled="saving" @click="$emit('manage')">管理连接</button>
      </div>
    </div>
    <p v-if="loading" class="project-agent-message" role="status">正在读取项目选择…</p>
    <div v-else-if="error" class="project-agent-error" role="alert">
      <p>{{ error }}</p>
      <button v-if="!loaded" type="button" @click="load">重试</button>
    </div>
    <p v-else-if="saving || saved" class="project-agent-message" role="status">{{ saving ? '正在保存选择…' : '已保存项目选择。' }}</p>
    <p v-else-if="loaded && !enabledAgents.length && !selectedId" class="project-agent-message">暂无可用连接，可以在「管理连接」里添加或启用。</p>
  </section>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import type { ExternalAgent } from "../types/externalAgent";

const props = defineProps<{ projectId: string; viewActive: boolean }>();
defineEmits<{ (event: "manage"): void }>();
const saving = defineModel<boolean>("saving", { required: true });
const agents = ref<ExternalAgent[]>([]);
const selectedId = ref("");
const persistedId = ref("");
const loading = ref(true);
const loaded = ref(false);
const saved = ref(false);
const error = ref("");
let loadGeneration = 0;

const enabledAgents = computed(() => agents.value.filter(agent => agent.enabled));
const unavailableSelection = computed(() => {
  if (!selectedId.value || enabledAgents.value.some(agent => agent.id === selectedId.value)) return "";
  const configured = agents.value.find(agent => agent.id === selectedId.value);
  return configured ? `${configured.name}（已停用）` : "已选连接不可用";
});

async function load() {
  const generation = ++loadGeneration;
  loading.value = true;
  loaded.value = false;
  error.value = "";
  saved.value = false;
  try {
    const [list, id] = await Promise.all([
      invoke<ExternalAgent[]>("list_external_agents"),
      invoke<string | null>("get_project_external_agent", { projectId: props.projectId }),
    ]);
    if (generation !== loadGeneration) return;
    agents.value = list;
    persistedId.value = id ?? "";
    selectedId.value = persistedId.value;
    loaded.value = true;
  } catch (cause) {
    if (generation === loadGeneration) error.value = `读取项目选择失败：${String(cause)}`;
  } finally {
    if (generation === loadGeneration) loading.value = false;
  }
}

async function save() {
  if (saving.value || selectedId.value === persistedId.value) return;
  const id = selectedId.value;
  saving.value = true;
  error.value = "";
  saved.value = false;
  try {
    await invoke("set_project_external_agent", { projectId: props.projectId, agentId: id || null });
    persistedId.value = id;
    saved.value = true;
  } catch (cause) {
    selectedId.value = persistedId.value;
    error.value = `保存项目选择失败：${String(cause)}`;
  } finally {
    saving.value = false;
  }
}

watch(() => props.viewActive, active => { if (active) void load(); }, { immediate: true });
onBeforeUnmount(() => {
  ++loadGeneration;
  if (saving.value) saving.value = false;
});
</script>

<style scoped>
.project-agent {
  padding: 12px 14px;
  border: 1px solid rgba(var(--theme-foreground-rgb, 255, 255, 255), .10);
  border-radius: 12px;
  background: var(--theme-bg-panel, #191c23);
  color: var(--theme-text-primary, #e6e8ef);
}
.project-agent-row, .project-agent-actions, .project-agent-error { display: flex; align-items: center; gap: 8px; }
.project-agent-row { justify-content: space-between; flex-wrap: wrap; gap: 10px 14px; }
.project-agent-copy { flex: 1 1 auto; }
.project-agent-actions { max-width: 100%; flex: 0 0 auto; flex-wrap: wrap; }
.project-agent-copy h2 { margin: 0 0 4px; font-size: 13px; }
.project-agent p { margin: 0; color: var(--theme-text-muted, #9399a8); font-size: 12px; line-height: 1.45; }
.project-agent select, .project-agent button {
  min-height: 34px;
  max-width: min(220px, 100%);
  padding: 5px 8px;
  color: inherit;
  font: inherit;
  font-size: 12px;
  white-space: nowrap;
  background: var(--theme-bg-base, #111318);
  border: 1px solid rgba(var(--theme-foreground-rgb, 255, 255, 255), .17);
  border-radius: 8px;
  cursor: pointer;
}
.project-agent button:hover:not(:disabled) { border-color: var(--theme-status-accent, #a5b4fc); }
.project-agent select:focus-visible, .project-agent button:focus-visible { outline: 2px solid #818cf8; outline-offset: 2px; }
.project-agent :disabled { opacity: .55; cursor: default; }
.project-agent p.project-agent-message, .project-agent-error { margin-top: 8px; }
.project-agent-error p { color: var(--theme-status-danger, #f2a2a2); overflow-wrap: anywhere; }
</style>
