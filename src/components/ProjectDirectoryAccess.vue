<template>
  <section class="directory-access" aria-label="项目目录权限">
    <div class="directory-access-heading">
      <div><h3>{{ sessionId ? "当前对话 · 目录权限" : "附加目录权限" }}</h3><p>参考其他项目代码，或授权跨目录修改。</p></div>
      <div class="directory-access-actions">
        <button type="button" :disabled="busy || loading" :aria-expanded="workspacePickerOpen" @click="toggleWorkspacePicker">选择已有工作区</button>
        <button type="button" :disabled="busy || loading" @click="addDirectory">选择本机目录</button>
      </div>
    </div>
    <div v-if="workspacePickerOpen" class="workspace-picker" aria-label="已有工作区">
      <p class="directory-access-note">选择已登记的项目目录，添加后默认只读。</p>
      <p v-if="workspacesLoading" role="status" class="directory-access-note">正在加载已有工作区…</p>
      <p v-else-if="workspacesError" role="alert" class="directory-access-error">{{ workspacesError }} <button type="button" @click="loadWorkspaces">重试</button></p>
      <p v-else-if="!availableWorkspaces.length" class="directory-access-note">暂无其他已登记的工作区，可以使用“选择本机目录”。</p>
      <div v-else class="workspace-picker-list">
        <button v-for="workspace in availableWorkspaces" :key="workspace.id" type="button" class="workspace-picker-option" :disabled="busy || loading || isDirectoryAdded(workspace.root_path)" @click="addWorkspace(workspace)">
          <span class="workspace-picker-title">{{ workspace.name }} <span v-if="isDirectoryAdded(workspace.root_path)">已添加</span></span>
          <code :title="workspace.root_path">{{ workspace.root_path }}</code>
        </button>
      </div>
    </div>
    <div class="directory-access-row primary">
      <code :title="primaryPath">{{ primaryPath }}</code><span>{{ sessionId ? "会话工作区" : "主工作区" }} · 可读写</span>
    </div>
    <div v-for="directory in directories" :key="directory.path" class="directory-access-row">
      <code :title="directory.path">{{ directory.path }}</code>
      <select :value="directory.access" :disabled="busy || loading" :aria-label="`${directory.path} 的权限`" @change="changeAccess(directory.path, $event)">
        <option value="read_only">只读</option><option value="read_write">可读写</option>
      </select>
      <button type="button" :disabled="busy || loading" :aria-label="`移除 ${directory.path} 授权`" @click="removeDirectory(directory.path)">移除</button>
    </div>
    <p v-if="loading" class="directory-access-note">正在加载目录权限…</p>
    <p v-else-if="!directories.length && !error" class="directory-access-note">尚未添加参考目录；新增默认只读。</p>
    <p class="directory-access-note">{{ sessionId ? "仅当前对话有效，续跑或重新打开保留；不带入新对话。" : "按项目保存，" }}运行中可调整。写权限包含读权限；重叠目录以最深规则为准，主工作区权限不变。移除只撤销授权，不删除磁盘文件。</p>
    <p class="directory-access-note">本地文件、搜索、看图按权限校验。Shell 的 cwd 仅接受可写目录；任意脚本与外部 MCP 不是只读沙箱，MCP 需服务端另行授权。</p>
    <p v-if="error" role="alert" class="directory-access-error">{{ error }} <button v-if="!busy && !loading" type="button" @click="loadDirectories">重试加载</button></p>
    <p v-if="notice" role="status" class="directory-access-notice">{{ notice }}</p>
  </section>
</template>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

type DirectoryAccess = "read_only" | "read_write";
interface ProjectDirectory { path: string; access: DirectoryAccess }
interface AgentProject { id: string; name: string; root_path: string }
const props = defineProps<{ projectId?: string; rootPath?: string; sessionId?: string }>();
const primaryPath = ref(props.rootPath || "");
function ownerParams() { return props.sessionId ? { sessionId: props.sessionId } : { projectId: props.projectId }; }
function mutationCommand(action: string) { return props.sessionId ? action.replace("_project_", "_session_") : action; }
const directories = ref<ProjectDirectory[]>([]);
const busy = ref(false);
const loading = ref(false);
const error = ref("");
const notice = ref("");
const workspacePickerOpen = ref(false);
const workspaces = ref<AgentProject[]>([]);
const workspacesLoading = ref(false);
const workspacesError = ref("");
const availableWorkspaces = computed(() => workspaces.value.filter(
  project => project.id !== props.projectId && project.root_path !== primaryPath.value,
));
let generation = 0;
let workspaceRequest = 0;

function isDirectoryAdded(path: string) {
  return directories.value.some(directory => directory.path === path);
}

async function loadWorkspaces() {
  const version = generation;
  const request = ++workspaceRequest;
  workspaces.value = [];
  workspacesLoading.value = true;
  workspacesError.value = "";
  try {
    const result = await invoke<AgentProject[]>("list_agent_projects");
    if (version === generation && request === workspaceRequest) workspaces.value = result;
  } catch (e) {
    if (version === generation && request === workspaceRequest) workspacesError.value = `加载已有工作区失败：${String(e)}`;
  } finally {
    if (version === generation && request === workspaceRequest) workspacesLoading.value = false;
  }
}

async function toggleWorkspacePicker() {
  workspacePickerOpen.value = !workspacePickerOpen.value;
  if (workspacePickerOpen.value) await loadWorkspaces();
}

async function addWorkspace(workspace: AgentProject) {
  // 已授权的目录不能被默认只读的快捷添加意外降权。
  if (busy.value || loading.value || isDirectoryAdded(workspace.root_path)) return;
  if (await mutate("set_agent_project_directory", { path: workspace.root_path, access: "read_only" })) {
    workspacePickerOpen.value = false;
  }
}

async function loadDirectories() {
  const version = ++generation;
  const owner = ownerParams();
  primaryPath.value = props.rootPath || "";
  ++workspaceRequest;
  workspacePickerOpen.value = false;
  workspaces.value = [];
  workspacesLoading.value = false;
  workspacesError.value = "";
  directories.value = [];
  error.value = "";
  notice.value = "";
  loading.value = true;
  try {
    if (props.sessionId) {
      const config = await invoke<{ root_path: string; directories: ProjectDirectory[] }>("get_agent_session_directory_config", owner);
      if (version === generation) { primaryPath.value = config.root_path; directories.value = config.directories; }
    } else {
      const result = await invoke<ProjectDirectory[]>("list_agent_project_directories", owner);
      if (version === generation) directories.value = result;
    }
  } catch (e) {
    if (version === generation) error.value = `加载目录权限失败：${String(e)}`;
  } finally {
    if (version === generation) loading.value = false;
  }
}

async function mutate(action: string, params: Record<string, unknown>) {
  const version = generation;
  const owner = ownerParams();
  busy.value = true;
  error.value = "";
  notice.value = "";
  try {
    const result = await invoke<ProjectDirectory[]>(mutationCommand(action), { ...owner, ...params });
    if (version === generation) {
      directories.value = result;
      notice.value = "已保存。后续受控工具调用使用新权限，Agent 下一步骤刷新目录清单；不重置原任务和审查。已开始的操作不被中断。";
      return true;
    }
  } catch (e) {
    if (version === generation) error.value = `保存目录权限失败：${String(e)}`;
  } finally { busy.value = false; }
  return false;
}

async function addDirectory() {
  const version = generation;
  busy.value = true;
  error.value = "";
  try {
    const path = await open({ directory: true, multiple: false, title: "添加参考目录（默认只读）", defaultPath: props.sessionId ? undefined : primaryPath.value || undefined });
    if (typeof path === "string" && version === generation) {
      await mutate("set_agent_project_directory", { path, access: "read_only" });
    }
  } catch (e) {
    if (version === generation) error.value = `选择目录失败：${String(e)}`;
  } finally { busy.value = false; }
}

async function changeAccess(path: string, event: Event) {
  const select = event.target as HTMLSelectElement;
  const access = select.value as DirectoryAccess;
  await mutate("set_agent_project_directory", { path, access });
  // 原生 select 已提前改变值，保存失败时恢复服务端最后确认的权限。
  const saved = directories.value.find(directory => directory.path === path);
  if (saved) select.value = saved.access;
}
async function removeDirectory(path: string) { await mutate("remove_agent_project_directory", { path }); }
watch(() => [props.projectId, props.sessionId], loadDirectories, { immediate: true });
</script>

<style scoped>
.directory-access { padding: 18px; border: 1px solid var(--border-color, #30333b); border-radius: 12px; background: var(--bg-secondary, #1b1d23); margin-bottom: 18px; }
.directory-access-heading { display: flex; align-items: center; justify-content: space-between; flex-wrap: wrap; gap: 12px; margin-bottom: 12px; }
.directory-access-actions { display: flex; flex-wrap: wrap; gap: 8px; }
.workspace-picker { padding: 10px; margin-bottom: 12px; border: 1px solid var(--border-color, #30333b); border-radius: 8px; }
.workspace-picker-list { display: flex; flex-direction: column; gap: 6px; margin-top: 8px; max-height: 240px; overflow-y: auto; }
.workspace-picker-option { display: flex; flex-direction: column; gap: 4px; text-align: left; }
.workspace-picker-option:not(:disabled):hover { border-color: var(--text-secondary, #a1a5b3); }
.workspace-picker-option code { overflow-wrap: anywhere; color: var(--text-secondary, #a1a5b3); }
.workspace-picker-title { display: flex; justify-content: space-between; gap: 10px; }
.workspace-picker-title > span { color: var(--text-secondary, #a1a5b3); white-space: nowrap; }
h3 { margin: 0 0 5px; font-size: 14px; } p { margin: 0; }
.directory-access-heading p, .directory-access-note { font-size: 12px; color: var(--text-secondary, #a1a5b3); line-height: 1.6; }
.directory-access-row { display: flex; align-items: center; gap: 10px; padding: 8px 0; border-top: 1px solid var(--border-color, #30333b); font-size: 12px; }
.directory-access-row code { flex: 1; min-width: 0; overflow-wrap: anywhere; }
.primary span { color: var(--text-secondary, #a1a5b3); white-space: nowrap; }
button, select { border: 1px solid var(--border-color, #454955); border-radius: 6px; background: var(--bg-primary, #23262e); color: inherit; font-size: 12px; padding: 6px 9px; cursor: pointer; }
button:disabled, select:disabled { opacity: .5; cursor: default; }
.directory-access-note { margin-top: 8px; }
.directory-access-error { margin-top: 10px; font-size: 12px; color: #f28b82; }
.directory-access-notice { margin-top: 10px; font-size: 12px; color: #70cba6; }
@media (max-width: 600px) { .directory-access-row { flex-wrap: wrap; } .directory-access-row code { flex-basis: 100%; } }
</style>
