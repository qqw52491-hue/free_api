<template>
  <section class="project-workspace">
    <header class="workspace-header">
      <div>
        <p class="eyebrow">PROJECT RESOURCES</p>
        <h1>项目与资源</h1>
        <p>项目只登记一个本地根目录；只有该目录内、由你显式添加的文件才会成为项目资源。</p>
      </div>
      <button class="primary-action" type="button" :disabled="busy" @click="createProject">
        {{ busy ? "处理中…" : "新建项目" }}
      </button>
    </header>

    <p v-if="errorMessage" class="error-message">{{ errorMessage }}</p>

    <div class="workspace-grid">
      <aside class="project-list-panel">
        <div class="panel-title-row">
          <h2>项目</h2>
          <span>{{ projects.length }}</span>
        </div>
        <button
          v-for="project in projects"
          :key="project.id"
          type="button"
          class="project-card"
          :class="{ active: activeProjectId === project.id }"
          @click="selectProject(project.id)"
        >
          <strong>{{ project.name }}</strong>
          <small :title="project.root_path">{{ project.root_path }}</small>
        </button>
        <p v-if="!projects.length" class="empty-copy">选择一个本地文件夹创建项目。不会自动扫描、上传或索引其中的文件。</p>
      </aside>

      <main v-if="activeProject" class="project-detail-panel">
        <div class="project-detail-header">
          <div>
            <p class="eyebrow">ACTIVE PROJECT</p>
            <h2>{{ activeProject.name }}</h2>
            <p class="root-path" :title="activeProject.root_path">{{ activeProject.root_path }}</p>
          </div>
          <button class="ghost-danger" type="button" :disabled="busy" @click="deleteProject">删除项目登记</button>
        </div>

        <div class="action-row">
          <button class="secondary-action" type="button" :disabled="busy || !models.length" @click="createProjectSession">项目内新对话</button>
          <button class="secondary-action" type="button" :disabled="busy" @click="addProjectResource">添加指定文件</button>
        </div>

        <div class="detail-columns">
          <section class="detail-section">
            <div class="panel-title-row"><h3>项目对话</h3><span>{{ sessions.length }}</span></div>
            <button
              v-for="session in sessions"
              :key="session.id"
              class="session-row"
              type="button"
              @click="emit('open-session', session)"
            >
              <span>{{ session.title || "未命名对话" }}</span>
              <small>{{ session.message_count }} 条消息</small>
            </button>
            <p v-if="!sessions.length" class="empty-copy">从此项目创建的对话会自动绑定该项目根目录。</p>
          </section>

          <section class="detail-section">
            <div class="panel-title-row"><h3>受管文件</h3><span>{{ resources.length }}</span></div>
            <article v-for="resource in resources" :key="resource.resource_id" class="resource-row">
              <button type="button" class="resource-main" @click="previewResource(resource.resource_id)">
                <strong>{{ resource.label }}</strong>
                <small>{{ resource.relative_path }} · {{ formatBytes(resource.size_bytes) }}</small>
              </button>
              <button type="button" class="resource-remove" title="取消登记" @click="removeResource(resource.resource_id)">×</button>
            </article>
            <p v-if="!resources.length" class="empty-copy">文件必须由你明确选择，且物理路径必须位于项目根目录内。</p>
          </section>
        </div>

        <section v-if="preview" class="preview-panel">
          <div class="panel-title-row">
            <div><h3>{{ preview.resource.label }}</h3><small>{{ preview.resource.relative_path }}</small></div>
            <span v-if="preview.truncated">仅显示前 {{ previewLimitLabel }} 个字符</span>
          </div>
          <pre>{{ preview.content }}</pre>
        </section>
      </main>

      <main v-else class="project-detail-panel empty-detail">
        <h2>选择或创建项目</h2>
        <p>一个项目可以包含多个对话和多个明确登记的文件；删除项目登记不会删除磁盘上的任何文件或其中的对话历史。</p>
      </main>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

interface AgentProject {
  id: string;
  name: string;
  root_path: string;
  created_at: string;
  updated_at: string;
}

interface AgentProjectResource {
  resource_id: string;
  project_id: string;
  label: string;
  relative_path: string;
  canonical_path: string;
  kind: string;
  size_bytes: number;
  created_at: string;
}

interface ProjectResourcePreview {
  resource: AgentProjectResource;
  content: string;
  truncated: boolean;
}

interface ChatSessionRow {
  id: string;
  title: string;
  model_id: string;
  created_at: string;
  updated_at: string;
  message_count: number;
}

const props = defineProps<{ models: Array<{ id: string }> }>();
const emit = defineEmits<{ (event: "open-session", session: ChatSessionRow): void }>();

const projects = ref<AgentProject[]>([]);
const sessions = ref<ChatSessionRow[]>([]);
const resources = ref<AgentProjectResource[]>([]);
const activeProjectId = ref<string | null>(null);
const preview = ref<ProjectResourcePreview | null>(null);
const busy = ref(false);
const errorMessage = ref("");
const previewLimitLabel = "120,000";

const activeProject = computed(() => projects.value.find((project) => project.id === activeProjectId.value) || null);

function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

function formatBytes(value: number) {
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / (1024 * 1024)).toFixed(1)} MB`;
}

function nameFromPath(path: string) {
  const parts = path.replace(/[\\/]+$/, "").split(/[\\/]/);
  return parts[parts.length - 1] || "未命名项目";
}

async function refreshProjects(preferredId?: string) {
  projects.value = await invoke<AgentProject[]>("list_agent_projects");
  const nextId = preferredId || activeProjectId.value;
  activeProjectId.value = projects.value.some((project) => project.id === nextId)
    ? nextId || null
    : projects.value[0]?.id || null;
  await refreshActiveProject();
}

async function refreshActiveProject() {
  preview.value = null;
  if (!activeProjectId.value) {
    sessions.value = [];
    resources.value = [];
    return;
  }
  const projectId = activeProjectId.value;
  [sessions.value, resources.value] = await Promise.all([
    invoke<ChatSessionRow[]>("list_agent_project_sessions", { projectId }),
    invoke<AgentProjectResource[]>("list_agent_project_resources", { projectId }),
  ]);
}

async function selectProject(projectId: string) {
  activeProjectId.value = projectId;
  errorMessage.value = "";
  try {
    await refreshActiveProject();
  } catch (error) {
    errorMessage.value = errorText(error);
  }
}

async function createProject() {
  const rootPath = await open({ directory: true, multiple: false, title: "选择项目根目录" });
  if (typeof rootPath !== "string") return;
  const requestedName = window.prompt("项目名称", nameFromPath(rootPath));
  if (requestedName === null) return;
  busy.value = true;
  errorMessage.value = "";
  try {
    const project = await invoke<AgentProject>("create_agent_project", { name: requestedName, rootPath });
    await refreshProjects(project.id);
  } catch (error) {
    errorMessage.value = errorText(error);
  } finally {
    busy.value = false;
  }
}

async function deleteProject() {
  if (!activeProject.value || !window.confirm(`删除“${activeProject.value.name}”的项目登记？不会删除磁盘文件或对话历史。`)) return;
  busy.value = true;
  errorMessage.value = "";
  try {
    await invoke("delete_agent_project", { projectId: activeProject.value.id });
    await refreshProjects();
  } catch (error) {
    errorMessage.value = errorText(error);
  } finally {
    busy.value = false;
  }
}

async function createProjectSession() {
  if (!activeProject.value || !props.models[0]) return;
  const title = window.prompt("对话名称", `${activeProject.value.name} · 新任务`);
  if (title === null) return;
  busy.value = true;
  errorMessage.value = "";
  try {
    const session = await invoke<ChatSessionRow>("create_agent_project_session", {
      projectId: activeProject.value.id,
      title,
      modelId: props.models[0].id,
    });
    await refreshActiveProject();
    emit("open-session", session);
  } catch (error) {
    errorMessage.value = errorText(error);
  } finally {
    busy.value = false;
  }
}

async function addProjectResource() {
  if (!activeProject.value) return;
  const path = await open({ directory: false, multiple: false, title: "选择项目内文件" });
  if (typeof path !== "string") return;
  busy.value = true;
  errorMessage.value = "";
  try {
    await invoke("add_agent_project_resource", { projectId: activeProject.value.id, path });
    await refreshActiveProject();
  } catch (error) {
    errorMessage.value = errorText(error);
  } finally {
    busy.value = false;
  }
}

async function removeResource(resourceId: string) {
  if (!activeProject.value) return;
  busy.value = true;
  errorMessage.value = "";
  try {
    await invoke("remove_agent_project_resource", { projectId: activeProject.value.id, resourceId });
    await refreshActiveProject();
  } catch (error) {
    errorMessage.value = errorText(error);
  } finally {
    busy.value = false;
  }
}

async function previewResource(resourceId: string) {
  if (!activeProject.value) return;
  busy.value = true;
  errorMessage.value = "";
  try {
    preview.value = await invoke<ProjectResourcePreview>("read_agent_project_resource", {
      projectId: activeProject.value.id,
      resourceId,
    });
  } catch (error) {
    errorMessage.value = errorText(error);
  } finally {
    busy.value = false;
  }
}

onMounted(async () => {
  try {
    await refreshProjects();
  } catch (error) {
    errorMessage.value = errorText(error);
  }
});
</script>

<style scoped>
.project-workspace { min-height: 100%; padding: 32px; background: #f8fafc; color: #162033; }
.workspace-header, .project-detail-header, .panel-title-row, .action-row { display: flex; align-items: center; justify-content: space-between; gap: 16px; }
.workspace-header { margin-bottom: 22px; }
h1, h2, h3, p { margin: 0; }
h1 { font-size: 26px; letter-spacing: -.03em; }
h2 { font-size: 19px; }
h3 { font-size: 14px; }
.workspace-header > div > p:last-child, .root-path, .empty-copy, small { color: #667085; font-size: 13px; line-height: 1.5; }
.eyebrow { margin-bottom: 6px; color: #4768c7; font-size: 11px; font-weight: 700; letter-spacing: .08em; }
.primary-action, .secondary-action { border: 0; border-radius: 9px; padding: 10px 14px; color: white; background: #3559c7; cursor: pointer; font-weight: 650; }
.secondary-action { color: #2f4da6; background: #e9efff; }
.primary-action:disabled, .secondary-action:disabled { opacity: .5; cursor: not-allowed; }
.workspace-grid { display: grid; grid-template-columns: minmax(210px, .7fr) minmax(0, 2fr); gap: 18px; }
.project-list-panel, .project-detail-panel, .detail-section, .preview-panel { border: 1px solid #e4e8f0; border-radius: 12px; background: white; }
.project-list-panel { padding: 14px; min-height: 460px; }
.project-detail-panel { padding: 22px; }
.project-card, .session-row, .resource-main { width: 100%; border: 0; border-radius: 8px; padding: 11px; background: transparent; text-align: left; cursor: pointer; }
.project-card { display: grid; gap: 3px; margin-top: 8px; }
.project-card:hover, .project-card.active, .session-row:hover, .resource-main:hover { background: #eef3ff; }
.project-card strong, .resource-main strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.project-card small, .resource-main small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.project-detail-header { align-items: flex-start; padding-bottom: 18px; border-bottom: 1px solid #edf0f5; }
.root-path { margin-top: 6px; word-break: break-all; }
.ghost-danger, .resource-remove { border: 0; color: #b42318; background: transparent; cursor: pointer; }
.action-row { justify-content: flex-start; margin: 18px 0; }
.detail-columns { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; }
.detail-section { padding: 14px; min-height: 190px; }
.panel-title-row { margin-bottom: 9px; }
.panel-title-row > span { color: #6373a5; font-size: 12px; font-weight: 700; }
.session-row { display: flex; justify-content: space-between; gap: 12px; }
.resource-row { display: flex; align-items: center; border-radius: 8px; }
.resource-main { display: grid; gap: 3px; min-width: 0; }
.resource-remove { padding: 8px 10px; font-size: 19px; }
.preview-panel { margin-top: 16px; padding: 14px; }
.preview-panel pre { max-height: 280px; overflow: auto; margin: 12px 0 0; padding: 12px; border-radius: 8px; background: #101828; color: #e7edf9; font: 12px/1.55 ui-monospace, SFMono-Regular, Menlo, monospace; white-space: pre-wrap; word-break: break-word; }
.empty-detail { display: grid; place-content: center; min-height: 460px; text-align: center; gap: 10px; }
.empty-copy { margin: 14px 4px; }
.error-message { margin: 0 0 16px; padding: 10px 12px; border-radius: 8px; color: #b42318; background: #fff1f0; font-size: 13px; }
@media (max-width: 900px) { .workspace-grid, .detail-columns { grid-template-columns: 1fr; } .workspace-header { align-items: flex-start; flex-direction: column; } }
</style>
