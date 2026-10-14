<template>
  <section class="directory-access" aria-label="项目目录权限">
    <div class="directory-access-heading">
      <div><h3>附加目录权限</h3><p>参考其他项目代码，或授权跨目录修改。</p></div>
      <button type="button" :disabled="busy || loading" @click="addDirectory">＋ 添加目录</button>
    </div>
    <div class="directory-access-row primary">
      <code :title="rootPath">{{ rootPath }}</code><span>主工作区 · 可读写</span>
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
    <p class="directory-access-note">按项目保存，运行中可调整。写权限包含读权限；重叠目录以最深规则为准，主工作区权限不变。移除只撤销授权，不删除磁盘文件。</p>
    <p class="directory-access-note">本地文件、搜索、看图按权限校验。Shell 的 cwd 仅接受可写目录；任意脚本与外部 MCP 不是只读沙箱，MCP 需服务端另行授权。</p>
    <p v-if="error" role="alert" class="directory-access-error">{{ error }} <button v-if="!busy && !loading" type="button" @click="loadDirectories">重试加载</button></p>
    <p v-if="notice" role="status" class="directory-access-notice">{{ notice }}</p>
  </section>
</template>

<script setup lang="ts">
import { ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

type DirectoryAccess = "read_only" | "read_write";
interface ProjectDirectory { path: string; access: DirectoryAccess }
const props = defineProps<{ projectId: string; rootPath: string }>();
const directories = ref<ProjectDirectory[]>([]);
const busy = ref(false);
const loading = ref(false);
const error = ref("");
const notice = ref("");
let generation = 0;

async function loadDirectories() {
  const version = ++generation;
  const projectId = props.projectId;
  directories.value = [];
  error.value = "";
  notice.value = "";
  loading.value = true;
  try {
    const result = await invoke<ProjectDirectory[]>("list_agent_project_directories", { projectId });
    if (version === generation) directories.value = result;
  } catch (e) {
    if (version === generation) error.value = `加载目录权限失败：${String(e)}`;
  } finally {
    if (version === generation) loading.value = false;
  }
}

async function mutate(action: string, params: Record<string, unknown>) {
  const version = generation;
  const projectId = props.projectId;
  busy.value = true;
  error.value = "";
  notice.value = "";
  try {
    const result = await invoke<ProjectDirectory[]>(action, { projectId, ...params });
    if (version === generation) {
      directories.value = result;
      notice.value = "已保存。后续受控工具调用使用新权限，Agent 下一步骤刷新目录清单；不重置原任务和审查。已开始的操作不被中断。";
    }
  } catch (e) {
    if (version === generation) error.value = `保存目录权限失败：${String(e)}`;
  } finally { busy.value = false; }
}

async function addDirectory() {
  const version = generation;
  busy.value = true;
  error.value = "";
  try {
    const path = await open({ directory: true, multiple: false, title: "添加参考目录（默认只读）", defaultPath: props.rootPath });
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
watch(() => props.projectId, loadDirectories, { immediate: true });
</script>

<style scoped>
.directory-access { padding: 18px; border: 1px solid var(--border-color, #30333b); border-radius: 12px; background: var(--bg-secondary, #1b1d23); margin-bottom: 18px; }
.directory-access-heading { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-bottom: 12px; }
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
