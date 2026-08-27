<template>
  <section class="project-workspace">
    <!-- 顶部导航与操作栏 -->
    <header class="workspace-header">
      <div class="header-info">
        <div class="kicker-row">
          <span class="workspace-kicker">📁 PROJECT WORKSPACE</span>
          <span class="workspace-badge" v-if="projects.length">{{ projects.length }} 个项目</span>
        </div>
        <h1 class="workspace-title">项目与受管资源</h1>
        <p class="workspace-subtitle">
          绑定本地工作区根目录，显式登记关键文件为受管资源；Agent 与子审查员（Child Reviewer）将在对话中精准参考这些上下文。
        </p>
      </div>
      <div class="header-actions">
        <button class="btn-primary-action" type="button" :disabled="busy" @click="createProject">
          <svg class="btn-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <line x1="12" y1="5" x2="12" y2="19"></line>
            <line x1="5" y1="12" x2="19" y2="12"></line>
          </svg>
          <span>{{ busy ? "处理中…" : "新建项目" }}</span>
        </button>
      </div>
    </header>

    <!-- 错误反馈提示条 -->
    <transition name="fade">
      <div v-if="errorMessage" class="error-banner">
        <svg class="error-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <circle cx="12" cy="12" r="10"></circle>
          <line x1="12" y1="8" x2="12" y2="12"></line>
          <line x1="12" y1="16" x2="12.01" y2="16"></line>
        </svg>
        <span class="error-text">{{ errorMessage }}</span>
        <button class="error-close" type="button" @click="errorMessage = ''">×</button>
      </div>
    </transition>

    <!-- 主工作区分栏网格 -->
    <div class="workspace-layout">
      <!-- 左侧：项目列表与检索 -->
      <aside class="project-nav-panel">
        <div class="panel-head">
          <div class="panel-title-wrap">
            <svg class="section-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"></path>
            </svg>
            <h2>项目列表</h2>
          </div>
          <span class="count-pill">{{ filteredProjects.length }}</span>
        </div>

        <div v-if="projects.length > 3" class="search-box">
          <svg class="search-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <circle cx="11" cy="11" r="8"></circle>
            <line x1="21" y1="21" x2="16.65" y2="16.65"></line>
          </svg>
          <input
            v-model="projectFilterQuery"
            type="text"
            placeholder="搜索项目名称或路径..."
            class="search-input"
          />
        </div>

        <div class="project-list-scroll">
          <button
            v-for="project in filteredProjects"
            :key="project.id"
            type="button"
            class="project-item-card"
            :class="{ active: activeProjectId === project.id }"
            @click="selectProject(project.id)"
          >
            <div class="project-item-head">
              <span class="project-item-icon">📂</span>
              <strong class="project-item-name" :title="project.name">{{ project.name }}</strong>
            </div>
            <div class="project-item-meta">
              <span class="project-item-path" :title="project.root_path">{{ project.root_path }}</span>
            </div>
          </button>

          <div v-if="!projects.length" class="empty-nav-state">
            <div class="empty-icon-wrap">📁</div>
            <p class="empty-title">暂无项目</p>
            <p class="empty-hint">点击右上角“新建项目”选择本地代码目录进行登记。</p>
          </div>
        </div>
      </aside>

      <!-- 右侧：活动项目详情工作区 -->
      <main v-if="activeProject" class="project-content-panel">
        <!-- 项目详情头部 -->
        <div class="project-banner">
          <div class="banner-left">
            <div class="banner-title-row">
              <div class="project-avatar">📂</div>
              <div>
                <span class="banner-kicker">ACTIVE PROJECT</span>
                <h2 class="active-project-title">{{ activeProject.name }}</h2>
              </div>
            </div>
            <div class="root-path-badge" :title="activeProject.root_path">
              <svg class="path-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M3 9l9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"></path>
                <polyline points="9 22 9 12 15 12 15 22"></polyline>
              </svg>
              <code>{{ activeProject.root_path }}</code>
            </div>
          </div>
          <div class="banner-right">
            <button class="btn-danger-ghost" type="button" :disabled="busy" @click="deleteProject">
              <svg class="btn-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <polyline points="3 6 5 6 21 6"></polyline>
                <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"></path>
              </svg>
              <span>解除登记</span>
            </button>
          </div>
        </div>

        <!-- 快捷操作栏 -->
        <div class="project-actions-bar">
          <button
            class="btn-sub-action"
            type="button"
            :disabled="busy || !models.length"
            @click="createProjectSession"
          >
            <svg class="btn-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"></path>
            </svg>
            <span>项目内新对话</span>
          </button>
          <button
            class="btn-sub-action"
            type="button"
            :disabled="busy"
            @click="addProjectResource"
          >
            <svg class="btn-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"></path>
              <polyline points="14 2 14 8 20 8"></polyline>
              <line x1="12" y1="18" x2="12" y2="12"></line>
              <line x1="9" y1="15" x2="15" y2="15"></line>
            </svg>
            <span>添加指定文件</span>
          </button>
        </div>

        <!-- 双列面板：对话列表 & 受管文件 -->
        <div class="detail-split-grid">
          <!-- 关联对话列表 -->
          <section class="section-card">
            <div class="section-head">
              <div class="section-title-wrap">
                <svg class="section-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                  <path d="M21 11.5a8.38 8.38 0 0 1-.9 3.8 8.5 8.5 0 0 1-7.6 4.7 8.38 8.38 0 0 1-3.8-.9L3 21l1.9-5.7a8.38 8.38 0 0 1-.9-3.8 8.5 8.5 0 0 1 4.7-7.6 8.38 8.38 0 0 1 3.8-.9h.5a8.48 8.48 0 0 1 8 8v.5z"></path>
                </svg>
                <h3>项目对话</h3>
              </div>
              <span class="count-pill">{{ sessions.length }}</span>
            </div>

            <div class="section-body">
              <div
                v-for="session in sessions"
                :key="session.id"
                class="session-card-row"
                @click="emit('open-session', session)"
              >
                <div class="session-card-info">
                  <span class="session-title">{{ session.title || "未命名对话" }}</span>
                  <span class="session-meta">{{ session.message_count }} 条消息 · {{ formatDate(session.updated_at) }}</span>
                </div>
                <div class="session-arrow">↗</div>
              </div>
              <div v-if="!sessions.length" class="card-empty-hint">
                <span>💬 从此项目创建的对话会自动挂载该项目上下文。</span>
              </div>
            </div>
          </section>

          <!-- 受管文件列表 -->
          <section class="section-card">
            <div class="section-head">
              <div class="section-title-wrap">
                <svg class="section-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                  <path d="M13 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z"></path>
                  <polyline points="13 2 13 9 20 9"></polyline>
                </svg>
                <h3>受管文件</h3>
              </div>
              <span class="count-pill">{{ resources.length }}</span>
            </div>

            <div class="section-body">
              <div
                v-for="resource in resources"
                :key="resource.resource_id"
                class="resource-card-row"
                :class="{ active: preview?.resource.resource_id === resource.resource_id }"
              >
                <button
                  type="button"
                  class="resource-info-btn"
                  title="点击查看资源内容"
                  @click="previewResource(resource.resource_id)"
                >
                  <span class="resource-kind-icon">{{ kindEmoji(resource.kind) }}</span>
                  <div class="resource-text-col">
                    <strong class="resource-label">{{ resource.label }}</strong>
                    <small class="resource-rel-path">{{ resource.relative_path }} · {{ formatBytes(resource.size_bytes) }}</small>
                  </div>
                </button>
                <button
                  type="button"
                  class="resource-delete-btn"
                  title="取消登记此资源"
                  @click.stop="removeResource(resource.resource_id)"
                >
                  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                    <line x1="18" y1="6" x2="6" y2="18"></line>
                    <line x1="6" y1="6" x2="18" y2="18"></line>
                  </svg>
                </button>
              </div>
              <div v-if="!resources.length" class="card-empty-hint">
                <span>📑 显式登记的文件将作为高权重参考资源提供给 Agent。</span>
              </div>
            </div>
          </section>
        </div>

        <!-- 资源内容实时预览面板 -->
        <transition name="slide-up">
          <section v-if="preview" class="resource-preview-panel">
            <div class="preview-header">
              <div class="preview-title-col">
                <div class="preview-title-row">
                  <span class="preview-icon">{{ kindEmoji(preview.resource.kind) }}</span>
                  <h3 class="preview-label">{{ preview.resource.label }}</h3>
                </div>
                <code class="preview-path">{{ preview.resource.relative_path }}</code>
              </div>
              <div class="preview-actions">
                <span v-if="preview.truncated" class="truncated-badge">仅截取前 {{ previewLimitLabel }} 字符</span>
                <button class="preview-btn" type="button" @click="copyPreviewContent">
                  {{ copied ? "✓ 已复制" : "复制全文" }}
                </button>
                <button class="preview-close-btn" type="button" title="关闭预览" @click="preview = null">×</button>
              </div>
            </div>
            <div class="preview-code-wrap">
              <pre class="preview-code">{{ preview.content }}</pre>
            </div>
          </section>
        </transition>
      </main>

      <!-- 未选中项目时的默认引导面板 -->
      <main v-else class="project-content-panel empty-selection-state">
        <div class="empty-hero">
          <div class="empty-hero-icon">📁</div>
          <h2>选择或创建项目</h2>
          <p>
            绑定本地文件夹后，你可以按需添加核心代码、需求文档或 API 规范。系统不会全盘扫描无关代码，保护你的本地安全与 Token 预算。
          </p>
          <button class="btn-primary-action" type="button" :disabled="busy" @click="createProject">
            <svg class="btn-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <line x1="12" y1="5" x2="12" y2="19"></line>
              <line x1="5" y1="12" x2="19" y2="12"></line>
            </svg>
            <span>立即登记新项目</span>
          </button>
        </div>
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
const copied = ref(false);
const projectFilterQuery = ref("");
const previewLimitLabel = "120,000";

const activeProject = computed(() =>
  projects.value.find((project) => project.id === activeProjectId.value) || null
);

const filteredProjects = computed(() => {
  const query = projectFilterQuery.value.trim().toLowerCase();
  if (!query) return projects.value;
  return projects.value.filter(
    (p) =>
      p.name.toLowerCase().includes(query) ||
      p.root_path.toLowerCase().includes(query)
  );
});

function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

function formatBytes(value: number) {
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / (1024 * 1024)).toFixed(1)} MB`;
}

function formatDate(isoStr: string) {
  if (!isoStr) return "";
  try {
    const d = new Date(isoStr);
    return `${d.getMonth() + 1}/${d.getDate()} ${d.getHours().toString().padStart(2, "0")}:${d.getMinutes().toString().padStart(2, "0")}`;
  } catch {
    return isoStr;
  }
}

function kindEmoji(kind: string) {
  switch (kind) {
    case "code":
      return "💻";
    case "text":
      return "📝";
    case "pdf":
      return "📕";
    case "image":
      return "🖼️";
    default:
      return "📄";
  }
}

function nameFromPath(path: string) {
  const parts = path.replace(/[\\/]+$/, "").split(/[\\/]/);
  return parts[parts.length - 1] || "未命名项目";
}

async function copyPreviewContent() {
  if (!preview.value?.content) return;
  try {
    await navigator.clipboard.writeText(preview.value.content);
    copied.value = true;
    setTimeout(() => {
      copied.value = false;
    }, 2000);
  } catch {
    // 忽略复制失败
  }
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
  if (!activeProject.value || !window.confirm(`确认解除“${activeProject.value.name}”的项目登记？\n此操作不会删除磁盘上的任何物理文件或对话历史。`)) return;
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
  const path = await open({ directory: false, multiple: false, title: "选择项目内文件 (必须在项目根目录内)" });
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
/* 整个工作区容器：采用 Obsidian Dark 极客色系 */
.project-workspace {
  display: flex;
  flex-direction: column;
  height: 100%;
  width: 100%;
  padding: 24px 28px;
  background: var(--bg-0, #141416);
  color: var(--text-1, #f4f4f5);
  font-family: var(--font, -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif);
  overflow-y: auto;
  box-sizing: border-box;
}

/* 顶部信息与操作栏 */
.workspace-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 20px;
  margin-bottom: 20px;
  padding-bottom: 18px;
  border-bottom: 1px solid var(--border-1, rgba(255, 255, 255, 0.08));
}

.kicker-row {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 6px;
}

.workspace-kicker {
  color: var(--accent-light, #8b83ff);
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

.workspace-badge {
  font-size: 11px;
  padding: 1px 7px;
  border-radius: 99px;
  background: rgba(108, 99, 255, 0.15);
  color: var(--accent-light, #8b83ff);
  border: 1px solid rgba(108, 99, 255, 0.3);
}

.workspace-title {
  margin: 0 0 6px;
  font-size: 22px;
  font-weight: 700;
  color: var(--text-1, #f4f4f5);
  letter-spacing: -0.02em;
}

.workspace-subtitle {
  margin: 0;
  color: var(--text-2, #a1a1aa);
  font-size: 13px;
  line-height: 1.55;
  max-width: 820px;
}

/* 按钮通用 */
.btn-primary-action {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  border: 0;
  border-radius: var(--radius-sm, 8px);
  padding: 8px 16px;
  color: #ffffff;
  background: linear-gradient(135deg, var(--accent, #6c63ff), var(--accent-dark, #584fea));
  box-shadow: 0 2px 10px rgba(108, 99, 255, 0.3);
  font-size: 13px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.15s ease;
  white-space: nowrap;
}

.btn-primary-action:hover:not(:disabled) {
  transform: translateY(-1px);
  box-shadow: 0 4px 14px rgba(108, 99, 255, 0.45);
}

.btn-primary-action:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.btn-icon {
  width: 15px;
  height: 15px;
}

/* 错误提示横幅 */
.error-banner {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 16px;
  padding: 10px 14px;
  border-radius: var(--radius-sm, 8px);
  border: 1px solid rgba(255, 71, 87, 0.3);
  background: rgba(255, 71, 87, 0.12);
  color: var(--red-light, #ff6b81);
  font-size: 13px;
}

.error-icon {
  width: 16px;
  height: 16px;
  flex-shrink: 0;
}

.error-text {
  flex: 1;
}

.error-close {
  background: transparent;
  border: 0;
  color: inherit;
  font-size: 16px;
  cursor: pointer;
  padding: 0 4px;
}

/* 主栅格布局 */
.workspace-layout {
  display: grid;
  grid-template-columns: 280px minmax(0, 1fr);
  gap: 18px;
  flex: 1;
  min-height: 0;
}

/* 左侧：项目导航面板 */
.project-nav-panel {
  display: flex;
  flex-direction: column;
  background: var(--bg-1, #18181b);
  border: 1px solid var(--border-1, rgba(255, 255, 255, 0.08));
  border-radius: var(--radius-md, 12px);
  padding: 14px;
  min-height: 480px;
}

.panel-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 12px;
}

.panel-title-wrap {
  display: flex;
  align-items: center;
  gap: 8px;
}

.section-icon {
  width: 16px;
  height: 16px;
  color: var(--accent-light, #8b83ff);
}

.panel-head h2 {
  font-size: 14px;
  font-weight: 650;
  color: var(--text-1, #f4f4f5);
  margin: 0;
}

.count-pill {
  font-size: 11px;
  font-weight: 600;
  color: var(--text-3, #71717a);
  background: var(--surface-2, rgba(255, 255, 255, 0.06));
  padding: 1px 7px;
  border-radius: 99px;
}

.search-box {
  position: relative;
  margin-bottom: 10px;
}

.search-icon {
  position: absolute;
  left: 9px;
  top: 50%;
  transform: translateY(-50%);
  width: 13px;
  height: 13px;
  color: var(--text-3, #71717a);
}

.search-input {
  width: 100%;
  background: var(--bg-2, #1f1f23);
  border: 1px solid var(--border-1, rgba(255, 255, 255, 0.08));
  border-radius: var(--radius-sm, 8px);
  padding: 6px 10px 6px 28px;
  font-size: 12px;
  color: var(--text-1, #f4f4f5);
  outline: none;
  transition: border-color 0.15s ease;
}

.search-input:focus {
  border-color: var(--accent, #6c63ff);
}

.project-list-scroll {
  display: flex;
  flex-direction: column;
  gap: 6px;
  overflow-y: auto;
  flex: 1;
}

.project-item-card {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 10px 12px;
  border-radius: var(--radius-sm, 8px);
  border: 1px solid transparent;
  background: transparent;
  text-align: left;
  cursor: pointer;
  transition: all 0.15s ease;
}

.project-item-card:hover {
  background: var(--surface-2, rgba(255, 255, 255, 0.06));
}

.project-item-card.active {
  background: rgba(108, 99, 255, 0.12);
  border-color: rgba(108, 99, 255, 0.35);
}

.project-item-head {
  display: flex;
  align-items: center;
  gap: 8px;
}

.project-item-icon {
  font-size: 14px;
}

.project-item-name {
  font-size: 13px;
  font-weight: 600;
  color: var(--text-1, #f4f4f5);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.project-item-card.active .project-item-name {
  color: var(--accent-light, #8b83ff);
}

.project-item-meta {
  display: flex;
  align-items: center;
}

.project-item-path {
  font-size: 11.5px;
  color: var(--text-3, #71717a);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.empty-nav-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 40px 16px;
  text-align: center;
}

.empty-icon-wrap {
  font-size: 32px;
  margin-bottom: 10px;
  opacity: 0.6;
}

.empty-title {
  margin: 0 0 6px;
  font-size: 13px;
  font-weight: 600;
  color: var(--text-2, #a1a1aa);
}

.empty-hint {
  margin: 0;
  font-size: 12px;
  color: var(--text-3, #71717a);
  line-height: 1.45;
}

/* 右侧：活动项目详情区 */
.project-content-panel {
  display: flex;
  flex-direction: column;
  gap: 16px;
  background: var(--bg-1, #18181b);
  border: 1px solid var(--border-1, rgba(255, 255, 255, 0.08));
  border-radius: var(--radius-md, 12px);
  padding: 20px;
  min-height: 480px;
}

/* 详情横幅 */
.project-banner {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
  padding-bottom: 16px;
  border-bottom: 1px solid var(--border-1, rgba(255, 255, 255, 0.08));
}

.banner-left {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.banner-title-row {
  display: flex;
  align-items: center;
  gap: 10px;
}

.project-avatar {
  display: grid;
  place-items: center;
  width: 38px;
  height: 38px;
  border-radius: 10px;
  background: rgba(108, 99, 255, 0.15);
  border: 1px solid rgba(108, 99, 255, 0.3);
  font-size: 18px;
}

.banner-kicker {
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.07em;
  color: var(--accent-light, #8b83ff);
}

.active-project-title {
  margin: 0;
  font-size: 18px;
  font-weight: 700;
  color: var(--text-1, #f4f4f5);
}

.root-path-badge {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 4px 10px;
  border-radius: var(--radius-sm, 8px);
  background: var(--bg-2, #1f1f23);
  border: 1px solid var(--border-1, rgba(255, 255, 255, 0.08));
  max-width: 580px;
}

.path-icon {
  width: 13px;
  height: 13px;
  color: var(--text-3, #71717a);
  flex-shrink: 0;
}

.root-path-badge code {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 12px;
  color: var(--text-2, #a1a1aa);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.btn-danger-ghost {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  border: 1px solid rgba(255, 71, 87, 0.2);
  border-radius: var(--radius-sm, 8px);
  padding: 6px 12px;
  background: transparent;
  color: var(--red-light, #ff6b81);
  font-size: 12px;
  font-weight: 500;
  cursor: pointer;
  transition: all 0.15s ease;
}

.btn-danger-ghost:hover:not(:disabled) {
  background: rgba(255, 71, 87, 0.12);
  border-color: rgba(255, 71, 87, 0.4);
}

/* 操作快捷栏 */
.project-actions-bar {
  display: flex;
  align-items: center;
  gap: 10px;
}

.btn-sub-action {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  border: 1px solid var(--border-2, rgba(255, 255, 255, 0.14));
  border-radius: var(--radius-sm, 8px);
  padding: 7px 14px;
  background: var(--bg-2, #1f1f23);
  color: var(--text-1, #f4f4f5);
  font-size: 12.5px;
  font-weight: 550;
  cursor: pointer;
  transition: all 0.15s ease;
}

.btn-sub-action:hover:not(:disabled) {
  background: var(--surface-3, rgba(255, 255, 255, 0.1));
  border-color: var(--accent-light, #8b83ff);
  color: #ffffff;
}

.btn-sub-action:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

/* 双列网格 */
.detail-split-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 16px;
}

.section-card {
  display: flex;
  flex-direction: column;
  border: 1px solid var(--border-1, rgba(255, 255, 255, 0.08));
  border-radius: var(--radius-sm, 8px);
  background: var(--bg-2, #1f1f23);
  padding: 14px;
  min-height: 200px;
}

.section-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 12px;
}

.section-head h3 {
  margin: 0;
  font-size: 13.5px;
  font-weight: 650;
  color: var(--text-1, #f4f4f5);
}

.section-body {
  display: flex;
  flex-direction: column;
  gap: 6px;
  overflow-y: auto;
  max-height: 240px;
}

/* 会话卡片行 */
.session-card-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 8px 10px;
  border-radius: 6px;
  background: var(--surface-1, rgba(255, 255, 255, 0.03));
  border: 1px solid transparent;
  cursor: pointer;
  transition: all 0.15s ease;
}

.session-card-row:hover {
  background: var(--surface-2, rgba(255, 255, 255, 0.06));
  border-color: var(--border-1, rgba(255, 255, 255, 0.08));
}

.session-card-info {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.session-title {
  font-size: 12.5px;
  font-weight: 550;
  color: var(--text-1, #f4f4f5);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.session-meta {
  font-size: 11px;
  color: var(--text-3, #71717a);
}

.session-arrow {
  color: var(--accent-light, #8b83ff);
  font-size: 13px;
  font-weight: 700;
  opacity: 0.6;
  transition: opacity 0.15s;
}

.session-card-row:hover .session-arrow {
  opacity: 1;
}

/* 资源卡片行 */
.resource-card-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  padding: 6px 8px;
  border-radius: 6px;
  background: var(--surface-1, rgba(255, 255, 255, 0.03));
  border: 1px solid transparent;
  transition: all 0.15s ease;
}

.resource-card-row:hover,
.resource-card-row.active {
  background: var(--surface-2, rgba(255, 255, 255, 0.06));
  border-color: rgba(108, 99, 255, 0.3);
}

.resource-info-btn {
  display: flex;
  align-items: center;
  gap: 8px;
  flex: 1;
  min-width: 0;
  border: 0;
  background: transparent;
  text-align: left;
  cursor: pointer;
  padding: 0;
}

.resource-kind-icon {
  font-size: 14px;
  flex-shrink: 0;
}

.resource-text-col {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.resource-label {
  font-size: 12.5px;
  font-weight: 600;
  color: var(--text-1, #f4f4f5);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.resource-rel-path {
  font-size: 11px;
  color: var(--text-3, #71717a);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.resource-delete-btn {
  display: grid;
  place-items: center;
  width: 22px;
  height: 22px;
  border: 0;
  border-radius: 4px;
  background: transparent;
  color: var(--text-3, #71717a);
  cursor: pointer;
  transition: all 0.15s ease;
}

.resource-delete-btn svg {
  width: 13px;
  height: 13px;
}

.resource-delete-btn:hover {
  background: rgba(255, 71, 87, 0.15);
  color: var(--red-light, #ff6b81);
}

.card-empty-hint {
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px 10px;
  text-align: center;
  color: var(--text-3, #71717a);
  font-size: 12px;
}

/* 资源实时预览区 */
.resource-preview-panel {
  display: flex;
  flex-direction: column;
  border: 1px solid var(--border-2, rgba(255, 255, 255, 0.14));
  border-radius: var(--radius-sm, 8px);
  background: var(--bg-0, #141416);
  padding: 14px 16px;
  margin-top: 4px;
}

.preview-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding-bottom: 10px;
  border-bottom: 1px solid var(--border-1, rgba(255, 255, 255, 0.08));
}

.preview-title-col {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.preview-title-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.preview-icon {
  font-size: 14px;
}

.preview-label {
  margin: 0;
  font-size: 13.5px;
  font-weight: 650;
  color: var(--text-1, #f4f4f5);
}

.preview-path {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 11.5px;
  color: var(--text-3, #71717a);
}

.preview-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.truncated-badge {
  font-size: 11px;
  color: var(--orange, #ffa43d);
  background: rgba(255, 164, 61, 0.12);
  padding: 2px 7px;
  border-radius: 4px;
  border: 1px solid rgba(255, 164, 61, 0.25);
}

.preview-btn {
  border: 1px solid var(--border-2, rgba(255, 255, 255, 0.14));
  border-radius: 6px;
  padding: 4px 10px;
  background: var(--bg-2, #1f1f23);
  color: var(--text-1, #f4f4f5);
  font-size: 11.5px;
  cursor: pointer;
  transition: all 0.15s ease;
}

.preview-btn:hover {
  background: var(--surface-3, rgba(255, 255, 255, 0.1));
  border-color: var(--accent-light, #8b83ff);
}

.preview-close-btn {
  border: 0;
  background: transparent;
  color: var(--text-3, #71717a);
  font-size: 18px;
  line-height: 1;
  padding: 0 4px;
  cursor: pointer;
  transition: color 0.15s ease;
}

.preview-close-btn:hover {
  color: var(--text-1, #f4f4f5);
}

.preview-code-wrap {
  margin-top: 10px;
  max-height: 320px;
  overflow-y: auto;
  border-radius: 6px;
  background: #0d0d0f;
  border: 1px solid var(--border-1, rgba(255, 255, 255, 0.08));
}

.preview-code {
  margin: 0;
  padding: 12px 14px;
  color: #e4e4e7;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  font-size: 12px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-word;
}

/* 空选择态 */
.empty-selection-state {
  display: grid;
  place-items: center;
  text-align: center;
}

.empty-hero {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  max-width: 440px;
  padding: 40px 20px;
}

.empty-hero-icon {
  font-size: 48px;
  opacity: 0.8;
}

.empty-hero h2 {
  margin: 0;
  font-size: 18px;
  font-weight: 700;
  color: var(--text-1, #f4f4f5);
}

.empty-hero p {
  margin: 0 0 8px;
  font-size: 13px;
  line-height: 1.6;
  color: var(--text-3, #71717a);
}

/* 动效 */
.fade-enter-active, .fade-leave-active { transition: opacity 0.2s ease; }
.fade-enter-from, .fade-leave-to { opacity: 0; }

.slide-up-enter-active, .slide-up-leave-active { transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1); }
.slide-up-enter-from, .slide-up-leave-to { opacity: 0; transform: translateY(8px); }

/* 响应式 */
@media (max-width: 960px) {
  .workspace-layout {
    grid-template-columns: 1fr;
  }
  .detail-split-grid {
    grid-template-columns: 1fr;
  }
  .workspace-header {
    flex-direction: column;
    align-items: stretch;
  }
}
</style>
