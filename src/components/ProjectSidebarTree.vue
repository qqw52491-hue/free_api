<template>
  <div class="project-sidebar-tree">
    <!-- Projects 标题行 -->
    <div class="section-title">
      <span>Projects ({{ projects.length }})</span>
      <div class="title-actions">
        <button
          type="button"
          class="title-btn"
          title="新建工程项目 (绑定本地目录)"
          @click="promptCreateProject"
        >
          <svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></svg>
        </button>
      </div>
    </div>

    <!-- 项目树列表 -->
    <div class="project-tree-list">
      <div
        v-for="project in projects"
        :key="project.id"
        class="project-tree-item"
      >
        <!-- 一级项目节点 (样式与原有侧边栏行完全一致) -->
        <div
          class="project-row"
          :class="{ expanded: isExpanded(project.id), active: activeProjectId === project.id }"
          @click="handleProjectRowClick(project)"
        >
          <button
            type="button"
            class="tree-arrow"
            :class="{ rotated: isExpanded(project.id) }"
            @click.stop="toggleExpand(project.id)"
          >
            <svg viewBox="0 0 24 24"><polyline points="9 18 15 12 9 6"></polyline></svg>
          </button>
          <svg class="project-icon-svg" viewBox="0 0 24 24">
            <path d="M3 7h7l2 2h9v10H3z"/>
          </svg>
          <span class="project-name" :title="`${project.name} (${project.root_path})`">{{ project.name }}</span>

          <!-- 悬浮操作 -->
          <div class="project-hover-actions" @click.stop>
            <button
              type="button"
              class="tree-act-btn"
              title="在本项目下发起新对话 (共享当前项目文件与上下文)"
              @click="quickCreateSession(project)"
            >
              <svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></svg>
            </button>
            <button
              type="button"
              class="tree-act-btn"
              title="添加数据文件 (Excel, Word, MD, Code...)"
              @click="promptAddResource(project)"
            >
              <svg viewBox="0 0 24 24"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"></path><polyline points="14 2 14 8 20 8"></polyline><line x1="12" y1="18" x2="12" y2="12"></line><line x1="9" y1="15" x2="15" y2="15"></line></svg>
            </button>
            <button
              type="button"
              class="tree-act-btn danger"
              title="解除项目登记"
              @click="promptDeleteProject(project)"
            >
              <svg viewBox="0 0 24 24"><polyline points="3 6 5 6 21 6"></polyline><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"></path></svg>
            </button>
          </div>
        </div>

        <!-- 二级展开项 (项目内会话与受管数据) -->
        <div v-if="isExpanded(project.id)" class="project-children">
          <!-- 1. 项目内对话列表 (包括未持久化的草稿态对话和已持久化对话) -->
          <div
            v-for="session in projectSessionList(project.id)"
            :key="session.id"
            class="child-leaf-row session-leaf"
            :class="{ active: activeSessionId === session.id, 'is-draft': session.isDraft }"
            @click="emit('open-session', session)"
            @dblclick="startRename(session)"
          >
            <svg class="child-icon-svg" viewBox="0 0 24 24">
              <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"></path>
            </svg>
            <input
              v-if="renamingId === session.id"
              v-model="renameText"
              class="leaf-rename-box"
              @blur="doRename(session.id)"
              @keydown.enter.prevent="doRename(session.id)"
              @click.stop
            />
            <span v-else class="leaf-label" :title="session.title">{{ session.title || "新对话" }}</span>
            <button
              type="button"
              class="leaf-close-btn"
              title="关闭/删除对话"
              @click.stop="closeSession(project.id, session)"
            >×</button>
          </div>

          <!-- 2. 项目内数据与文件列表 (Excel, Word, Code, MD...) -->
          <div
            v-for="res in projectResourcesMap[project.id] || []"
            :key="res.resource_id"
            class="child-leaf-row resource-leaf"
            title="点击预览文件内容"
            @click="openResourcePreview(res.resource_id)"
          >
            <svg class="child-icon-svg" viewBox="0 0 24 24">
              <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"></path>
              <polyline points="14 2 14 8 20 8"></polyline>
            </svg>
            <span class="leaf-label resource-code-label" :title="`${res.label} (${res.relative_path})`">{{ res.label }}</span>
            <button
              type="button"
              class="leaf-close-btn"
              title="取消受管"
              @click.stop="removeResource(project.id, res.resource_id)"
            >×</button>
          </div>

          <!-- 空子项提示 -->
          <div
            v-if="!projectSessionList(project.id).length && !(projectResourcesMap[project.id] || []).length"
            class="empty-children-hint"
          >
            <span>点击右侧 + 开启首个项目对话</span>
          </div>
        </div>
      </div>

      <!-- 无项目引导提示 -->
      <div v-if="!projects.length" class="no-projects-hint">
        <button type="button" class="btn-create-project-hint" @click="promptCreateProject">
          <span>+ 绑定本地项目目录</span>
        </button>
      </div>
    </div>

    <!-- ==================== 模态弹窗系统 ==================== -->

    <!-- 1. 新建项目弹窗 -->
    <transition name="modal-fade">
      <div v-if="createProjectModal.visible" class="modal-overlay" @click.self="createProjectModal.visible = false">
        <div class="modal-card">
          <div class="modal-head">
            <div class="modal-title-row">
              <svg class="modal-head-svg" viewBox="0 0 24 24"><path d="M3 7h7l2 2h9v10H3z"/></svg>
              <h3>新建项目工作区</h3>
            </div>
            <button type="button" class="modal-close-btn" @click="createProjectModal.visible = false">×</button>
          </div>
          <div class="modal-body">
            <div class="form-group">
              <label class="form-label">已选根目录路径:</label>
              <div class="path-display-box" :title="createProjectModal.rootPath">
                <code>{{ createProjectModal.rootPath }}</code>
              </div>
            </div>
            <div class="form-group">
              <label class="form-label" for="tree-project-name">项目名称:</label>
              <input
                id="tree-project-name"
                ref="projectNameInputRef"
                v-model="createProjectModal.name"
                type="text"
                class="form-input"
                placeholder="例如: 财务分析系统 / 电商后端"
                @keydown.enter="submitCreateProject"
              />
            </div>
          </div>
          <div class="modal-footer">
            <button type="button" class="btn-ghost" @click="createProjectModal.visible = false">取消</button>
            <button
              type="button"
              class="btn-solid"
              :disabled="!createProjectModal.name.trim() || busy"
              @click="submitCreateProject"
            >
              {{ busy ? "创建中…" : "确认创建" }}
            </button>
          </div>
        </div>
      </div>
    </transition>

    <!-- 2. 解除项目登记确认弹窗 -->
    <transition name="modal-fade">
      <div v-if="deleteProjectModal.visible" class="modal-overlay" @click.self="deleteProjectModal.visible = false">
        <div class="modal-card modal-card-danger">
          <div class="modal-head">
            <div class="modal-title-row">
              <span class="text-red">⚠️</span>
              <h3>解除项目登记</h3>
            </div>
            <button type="button" class="modal-close-btn" @click="deleteProjectModal.visible = false">×</button>
          </div>
          <div class="modal-body">
            <p class="modal-desc">
              确定要解除项目 <strong class="text-white">“{{ deleteProjectModal.project?.name }}”</strong> 的登记吗？
            </p>
            <p class="modal-desc-sub">
              此操作将清除该项目的受管资源索引和会话关联，但<strong>绝不会删除本地磁盘上的任何物理文件</strong>。
            </p>
          </div>
          <div class="modal-footer">
            <button type="button" class="btn-ghost" @click="deleteProjectModal.visible = false">取消</button>
            <button
              type="button"
              class="btn-danger-solid"
              :disabled="busy"
              @click="submitDeleteProject"
            >
              {{ busy ? "处理中…" : "确认解除" }}
            </button>
          </div>
        </div>
      </div>
    </transition>

    <!-- 3. 资源数据实时预览弹窗 -->
    <transition name="modal-fade">
      <div v-if="previewModal.visible" class="modal-overlay" @click.self="previewModal.visible = false">
        <div class="modal-card modal-card-preview">
          <div class="modal-head">
            <div class="modal-title-row">
              <svg class="modal-head-svg" viewBox="0 0 24 24"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"></path><polyline points="14 2 14 8 20 8"></polyline></svg>
              <div class="preview-head-text">
                <h3>{{ previewModal.resource?.label }}</h3>
                <code>{{ previewModal.resource?.relative_path }}</code>
              </div>
            </div>
            <div class="preview-head-actions">
              <span v-if="previewModal.truncated" class="truncated-tag">仅截取前 120,000 字符</span>
              <button type="button" class="btn-subtle btn-sm" @click="copyResourcePreview">
                {{ previewModal.copied ? "✓ 已复制" : "复制全文" }}
              </button>
              <button type="button" class="modal-close-btn" @click="previewModal.visible = false">×</button>
            </div>
          </div>
          <div class="modal-body preview-body">
            <RichContentRenderer
              v-if="previewModal.content && isMarkdownOrDoc(previewModal.resource?.relative_path)"
              :content="previewModal.content"
              class="preview-rich-markdown"
            />
            <pre v-else class="code-preview-block">{{ previewModal.content }}</pre>
          </div>
        </div>
      </div>
    </transition>
  </div>
</template>

<script setup lang="ts">
import { nextTick, onMounted, reactive, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import RichContentRenderer from "./charts/RichContentRenderer.vue";

function isMarkdownOrDoc(path?: string): boolean {
  if (!path) return true;
  const ext = path.split('.').pop()?.toLowerCase() || '';
  return ['md', 'markdown', 'mdown', 'mkdn', 'docx', 'doc', 'xlsx', 'xls', 'pdf', 'txt', 'csv', 'json'].includes(ext);
}

interface ModelOption { id: string; name: string; display_name: string; platform_id: string; platform_name: string; }
interface ChatSessionRow { id: string; title: string; model_id: string; created_at: string; updated_at: string; message_count: number; }

export interface AgentProject {
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

export interface TreeSessionItem {
  id: string;
  title: string;
  isDraft?: boolean;
}

const props = defineProps<{
  allModels: ModelOption[];
  activeSessionId?: string;
  activeProjectId?: string;
  drafts?: Array<{ id: string; label: string; projectId?: string; isDraft?: boolean }>;
}>();

const emit = defineEmits<{
  (event: "open-session", session: TreeSessionItem): void;
  (event: "select-project", project: AgentProject): void;
  (event: "new-project-session", project: AgentProject): void;
  (event: "session-deleted", sessionId: string): void;
}>();

const projects = ref<AgentProject[]>([]);
const projectSessionsMap = reactive<Record<string, ChatSessionRow[]>>({});
const projectResourcesMap = reactive<Record<string, AgentProjectResource[]>>({});
const expandedProjectIds = ref<Set<string>>(new Set());
const busy = ref(false);

function handleProjectRowClick(project: AgentProject) {
  loadProjectData(project.id);
  emit("select-project", project);
}

const renamingId = ref<string | null>(null);
const renameText = ref("");

// 弹窗状态
const projectNameInputRef = ref<HTMLInputElement | null>(null);

const createProjectModal = reactive({
  visible: false,
  rootPath: "",
  name: "",
});

const deleteProjectModal = reactive<{
  visible: boolean;
  project: AgentProject | null;
}>({
  visible: false,
  project: null,
});

const previewModal = reactive<{
  visible: boolean;
  resource: AgentProjectResource | null;
  content: string;
  truncated: boolean;
  copied: boolean;
}>({
  visible: false,
  resource: null,
  content: "",
  truncated: false,
  copied: false,
});

function isExpanded(projectId: string): boolean {
  return expandedProjectIds.value.has(projectId);
}

function projectSessionList(projectId: string): TreeSessionItem[] {
  const list: TreeSessionItem[] = [];
  if (props.drafts) {
    for (const d of props.drafts) {
      if (d.projectId === projectId && d.isDraft) {
        list.push({ id: d.id, title: d.label || "新对话", isDraft: true });
      }
    }
  }
  const persisted = projectSessionsMap[projectId] || [];
  for (const p of persisted) {
    list.push({ id: p.id, title: p.title || "未命名对话", isDraft: false });
  }
  return list;
}

async function toggleExpand(projectId: string) {
  if (expandedProjectIds.value.has(projectId)) {
    expandedProjectIds.value.delete(projectId);
  } else {
    expandedProjectIds.value.add(projectId);
    await loadProjectData(projectId);
  }
}

async function loadProjectData(projectId: string) {
  try {
    const [sessions, resources] = await Promise.all([
      invoke<ChatSessionRow[]>("list_agent_project_sessions", { projectId }),
      invoke<AgentProjectResource[]>("list_agent_project_resources", { projectId }),
    ]);
    projectSessionsMap[projectId] = sessions;
    projectResourcesMap[projectId] = resources;
  } catch (e) {
    console.error("加载项目数据失败:", e);
  }
}

async function refreshProjects() {
  try {
    projects.value = await invoke<AgentProject[]>("list_agent_projects");
    await Promise.all(projects.value.map((p) => loadProjectData(p.id)));
    // 保持用户当前的折叠/展开状态，默认全部关闭，绝不强制展开
  } catch (e) {
    console.error("加载项目列表失败:", e);
  }
}

// 新建项目
async function promptCreateProject() {
  try {
    const selected = await openDialog({
      directory: true,
      multiple: false,
      title: "选择本地项目根目录",
    });
    if (typeof selected !== "string" || !selected) return;
    createProjectModal.rootPath = selected;
    createProjectModal.name = selected.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || "未命名项目";
    createProjectModal.visible = true;
    nextTick(() => {
      projectNameInputRef.value?.focus();
      projectNameInputRef.value?.select();
    });
  } catch (e) {
    console.error(e);
  }
}

// 拖拽文件夹快速导入/新建项目
function promptCreateProjectWithDir(dirPath: string) {
  if (!dirPath) return;
  createProjectModal.rootPath = dirPath;
  createProjectModal.name = dirPath.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || "未命名项目";
  createProjectModal.visible = true;
  nextTick(() => {
    projectNameInputRef.value?.focus();
    projectNameInputRef.value?.select();
  });
}


async function submitCreateProject() {
  if (!createProjectModal.name.trim() || !createProjectModal.rootPath) return;
  busy.value = true;
  try {
    const project = await invoke<AgentProject>("create_agent_project", {
      name: createProjectModal.name.trim(),
      rootPath: createProjectModal.rootPath,
    });
    createProjectModal.visible = false;
    await refreshProjects();
    expandedProjectIds.value.add(project.id);
  } catch (e) {
    console.error("创建项目失败:", e);
  } finally {
    busy.value = false;
  }
}

// 一键在项目下发起新对话 (通过 emit 触发外层在前端生成草稿会话，未发消息前不写数据库)
function quickCreateSession(project: AgentProject) {
  expandedProjectIds.value.add(project.id);
  emit("new-project-session", project);
}

// 删除项目
function promptDeleteProject(project: AgentProject) {
  deleteProjectModal.project = project;
  deleteProjectModal.visible = true;
}

async function submitDeleteProject() {
  if (!deleteProjectModal.project) return;
  busy.value = true;
  try {
    await invoke("delete_agent_project", { projectId: deleteProjectModal.project.id });
    deleteProjectModal.visible = false;
    await refreshProjects();
  } catch (e) {
    console.error("删除项目失败:", e);
  } finally {
    busy.value = false;
  }
}

// 登记数据与文件
async function promptAddResource(project: AgentProject) {
  try {
    const selected = await openDialog({
      multiple: false,
      directory: false,
      title: `在 ${project.name} 中选择数据文件 (Excel, Word, PDF, MD, Code...)`,
      defaultPath: project.root_path,
    });
    if (typeof selected !== "string" || !selected) return;
    busy.value = true;
    await invoke("add_agent_project_resource", {
      projectId: project.id,
      path: selected,
    });
    await loadProjectData(project.id);
    expandedProjectIds.value.add(project.id);
  } catch (e) {
    console.error("添加数据文件失败:", e);
  } finally {
    busy.value = false;
  }
}

async function removeResource(projectId: string, resourceId: string) {
  try {
    await invoke("remove_agent_project_resource", { projectId, resourceId });
    await loadProjectData(projectId);
  } catch (e) {
    console.error("移除资源失败:", e);
  }
}

async function closeSession(projectId: string, session: TreeSessionItem) {
  try {
    if (!session.isDraft) {
      await invoke("delete_session", { id: session.id });
      await loadProjectData(projectId);
    }
    emit("session-deleted", session.id);
  } catch (e) {
    console.error("删除会话失败:", e);
  }
}

function startRename(session: TreeSessionItem) {
  if (session.isDraft) return;
  renamingId.value = session.id;
  renameText.value = session.title || "";
  nextTick(() => (document.querySelector(".leaf-rename-box") as HTMLInputElement | null)?.focus());
}

async function doRename(sessionId: string) {
  if (renamingId.value !== sessionId) return;
  const title = renameText.value.trim();
  renamingId.value = null;
  if (!title) return;
  await invoke("rename_session", { id: sessionId, title });
  for (const p of projects.value) {
    const s = (projectSessionsMap[p.id] || []).find((item) => item.id === sessionId);
    if (s) s.title = title;
  }
}

async function openResourcePreview(resourceId: string) {
  try {
    const data = await invoke<ProjectResourcePreview>("read_agent_project_resource", { resourceId });
    previewModal.resource = data.resource;
    previewModal.content = data.content;
    previewModal.truncated = data.truncated;
    previewModal.copied = false;
    previewModal.visible = true;
  } catch (e) {
    console.error("读取资源预览失败:", e);
  }
}

async function copyResourcePreview() {
  if (!previewModal.content) return;
  try {
    await navigator.clipboard.writeText(previewModal.content);
    previewModal.copied = true;
    setTimeout(() => { previewModal.copied = false; }, 2000);
  } catch {
    // 忽略
  }
}

defineExpose({
  refreshProjects,
  loadProjectData,
  projects,
  projectSessionsMap,
  promptCreateProjectWithDir,
});

onMounted(() => {
  refreshProjects();
});
</script>

<style scoped>
.project-sidebar-tree {
  display: flex;
  flex-direction: column;
  padding: 0 0 6px 0;
}

.section-title {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 8px 6px;
  color: #71717a;
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0.05em;
  text-transform: uppercase;
}

.title-actions {
  display: flex;
  align-items: center;
  gap: 4px;
}

.title-btn {
  display: grid;
  place-items: center;
  width: 20px;
  height: 20px;
  border-radius: 4px;
  border: none;
  background: transparent;
  color: #a1a1aa;
  cursor: pointer;
  transition: all 0.12s;
  padding: 0;
}

.title-btn:hover {
  background: rgba(255, 255, 255, 0.08);
  color: #ffffff;
}

.title-btn svg {
  width: 13px;
  height: 13px;
  fill: none;
  stroke: currentColor;
  stroke-width: 2;
}

.project-tree-list {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.project-tree-item {
  display: flex;
  flex-direction: column;
  border-radius: 7px;
  overflow: hidden;
}

/* 项目行：与侧边栏 task-row 高度与样式完全一致 */
.project-row {
  display: flex;
  height: 32px;
  align-items: center;
  gap: 7px;
  padding: 0 8px;
  border-radius: 7px;
  color: #a1a1aa;
  cursor: pointer;
  font-size: 12.5px;
  user-select: none;
  transition: all 0.12s ease;
}

.project-row:hover {
  background: rgba(255, 255, 255, 0.04);
  color: #f4f4f5;
}

.project-row.expanded {
  color: #f4f4f5;
  font-weight: 600;
}

.tree-arrow {
  display: grid;
  place-items: center;
  width: 14px;
  height: 14px;
  border: none;
  background: transparent;
  color: #71717a;
  cursor: pointer;
  transition: transform 0.15s ease, color 0.12s;
  padding: 0;
}

.tree-arrow svg {
  width: 10px;
  height: 10px;
  fill: none;
  stroke: currentColor;
  stroke-width: 2.2;
}

.tree-arrow.rotated {
  transform: rotate(90deg);
  color: #f4f4f5;
}

.project-icon-svg {
  width: 15px;
  height: 15px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
  flex-shrink: 0;
}

.project-name {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.project-hover-actions {
  display: none;
  align-items: center;
  gap: 2px;
}

.project-row:hover .project-hover-actions {
  display: flex;
}

.tree-act-btn {
  display: grid;
  place-items: center;
  width: 18px;
  height: 18px;
  border-radius: 4px;
  border: none;
  background: transparent;
  color: #a1a1aa;
  cursor: pointer;
  transition: all 0.12s;
  padding: 0;
}

.tree-act-btn:hover {
  background: rgba(255, 255, 255, 0.12);
  color: #ffffff;
}

.tree-act-btn.danger:hover {
  background: rgba(239, 68, 68, 0.18);
  color: #f87171;
}

.tree-act-btn svg {
  width: 11px;
  height: 11px;
  fill: none;
  stroke: currentColor;
  stroke-width: 2;
}

/* 子项展开区域 (二级菜单) */
.project-children {
  display: flex;
  flex-direction: column;
  gap: 1px;
  padding: 2px 4px 4px 10px;
  border-left: 1px dashed rgba(255, 255, 255, 0.08);
  margin-left: 14px;
}

.child-leaf-row {
  display: flex;
  height: 28px;
  align-items: center;
  gap: 7px;
  padding: 0 8px;
  border-radius: 6px;
  color: #a1a1aa;
  cursor: pointer;
  font-size: 12px;
  transition: all 0.12s ease;
}

.child-leaf-row:hover {
  color: #f4f4f5;
  background: rgba(255, 255, 255, 0.04);
}

.child-leaf-row.active {
  color: #ffffff;
  background: #27272a;
  font-weight: 550;
}

.child-leaf-row.is-draft {
  color: #a1a1aa;
  font-style: italic;
}

.child-icon-svg {
  width: 13px;
  height: 13px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
  flex-shrink: 0;
}

.leaf-label {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.resource-code-label {
  color: #8e8e93;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  font-size: 11px;
}

.child-leaf-row:hover .resource-code-label {
  color: #f4f4f5;
}

.leaf-close-btn {
  display: none;
  width: 16px;
  height: 16px;
  padding: 0;
  border: 0;
  border-radius: 4px;
  color: #71717a;
  cursor: pointer;
  background: transparent;
  font-size: 13px;
  line-height: 1;
}

.child-leaf-row:hover .leaf-close-btn {
  display: block;
}

.leaf-close-btn:hover {
  color: #fff;
  background: rgba(255, 255, 255, 0.15);
}

.empty-children-hint {
  padding: 4px 6px;
  font-size: 11px;
  color: #71717a;
}

.leaf-rename-box {
  min-width: 0;
  flex: 1;
  border: 1px solid rgba(255, 255, 255, 0.2);
  border-radius: 4px;
  outline: 0;
  padding: 2px 4px;
  color: #f4f4f5;
  background: #18181b;
  font-size: 11.5px;
}

.no-projects-hint {
  padding: 2px 8px;
}

.btn-create-project-hint {
  width: 100%;
  padding: 6px 10px;
  border-radius: 6px;
  border: 1px dashed rgba(255, 255, 255, 0.12);
  background: transparent;
  color: #71717a;
  font-size: 11.5px;
  cursor: pointer;
  transition: all 0.12s;
  text-align: left;
}

.btn-create-project-hint:hover {
  border-color: rgba(255, 255, 255, 0.25);
  color: #e4e4e7;
  background: rgba(255, 255, 255, 0.03);
}

/* 模态弹窗 */
.modal-overlay {
  position: fixed;
  inset: 0;
  z-index: 999;
  display: grid;
  place-items: center;
  background: rgba(0, 0, 0, 0.7);
  backdrop-filter: blur(8px);
  padding: 20px;
}

.modal-card {
  width: 100%;
  max-width: 460px;
  border-radius: 14px;
  background: #1c1c20;
  border: 1px solid rgba(255, 255, 255, 0.12);
  box-shadow: 0 16px 48px rgba(0, 0, 0, 0.6);
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.modal-card-danger {
  max-width: 420px;
  border-color: rgba(239, 68, 68, 0.3);
}

.modal-card-preview {
  max-width: 760px;
  max-height: 80vh;
}

.modal-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 14px 18px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.07);
}

.modal-title-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.modal-head-svg {
  width: 16px;
  height: 16px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
}

.modal-head h3 {
  margin: 0;
  font-size: 14.5px;
  font-weight: 700;
  color: #f4f4f5;
}

.modal-close-btn {
  background: none;
  border: none;
  color: #71717a;
  font-size: 18px;
  cursor: pointer;
  padding: 0 4px;
  line-height: 1;
}

.modal-close-btn:hover {
  color: #f4f4f5;
}

.modal-body {
  padding: 18px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.form-group {
  display: flex;
  flex-direction: column;
  gap: 5px;
}

.form-label {
  font-size: 12px;
  font-weight: 600;
  color: #a1a1aa;
}

.path-display-box {
  padding: 7px 10px;
  border-radius: 6px;
  background: rgba(0, 0, 0, 0.3);
  border: 1px solid rgba(255, 255, 255, 0.06);
  font-size: 11.5px;
  color: #8e8e93;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.path-display-box code {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

.form-input {
  width: 100%;
  padding: 8px 11px;
  border-radius: 7px;
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.1);
  color: #f4f4f5;
  font-size: 13px;
  outline: none;
  box-sizing: border-box;
}

.form-input:focus {
  background: rgba(255, 255, 255, 0.08);
  border-color: rgba(255, 255, 255, 0.25);
}

.modal-desc {
  margin: 0;
  font-size: 13.5px;
  color: #e4e4e7;
  line-height: 1.5;
}

.modal-desc-sub {
  margin: 0;
  font-size: 12px;
  color: #8e8e93;
  line-height: 1.5;
}

.text-white { color: #ffffff; }
.text-red { color: #f87171; }

.modal-footer {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 8px;
  padding: 12px 18px;
  background: rgba(0, 0, 0, 0.2);
  border-top: 1px solid rgba(255, 255, 255, 0.06);
}

.btn-ghost {
  padding: 6px 12px;
  border-radius: 6px;
  border: 1px solid transparent;
  background: transparent;
  color: #a1a1aa;
  font-size: 12.5px;
  font-weight: 600;
  cursor: pointer;
}

.btn-ghost:hover {
  background: rgba(255, 255, 255, 0.06);
  color: #f4f4f5;
}

.btn-solid {
  padding: 6px 14px;
  border-radius: 6px;
  border: 1px solid rgba(255, 255, 255, 0.2);
  background: #f4f4f5;
  color: #121214;
  font-size: 12.5px;
  font-weight: 650;
  cursor: pointer;
}

.btn-solid:hover:not(:disabled) {
  background: #ffffff;
  transform: translateY(-1px);
}

.btn-solid:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}

.btn-danger-solid {
  padding: 6px 14px;
  border-radius: 6px;
  border: 1px solid rgba(239, 68, 68, 0.3);
  background: #dc2626;
  color: #ffffff;
  font-size: 12.5px;
  font-weight: 650;
  cursor: pointer;
}

.btn-danger-solid:hover:not(:disabled) {
  background: #ef4444;
}

.preview-head-text {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.preview-head-text code {
  font-size: 11px;
  color: #8e8e93;
}

.preview-head-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.truncated-tag {
  font-size: 10.5px;
  padding: 2px 6px;
  border-radius: 4px;
  background: rgba(251, 191, 36, 0.1);
  color: #fbbf24;
  border: 1px solid rgba(251, 191, 36, 0.2);
}

.btn-subtle {
  padding: 4px 9px;
  border-radius: 5px;
  border: 1px solid rgba(255, 255, 255, 0.08);
  background: rgba(255, 255, 255, 0.06);
  color: #f4f4f5;
  font-size: 11.5px;
  cursor: pointer;
}

.btn-subtle:hover {
  background: rgba(255, 255, 255, 0.12);
}

.preview-body {
  padding: 0;
  overflow: auto;
  max-height: 580px;
  background: #131316;
}

.preview-rich-markdown {
  padding: 18px 24px;
  color: #e4e4e7;
  font-size: 13.5px;
  line-height: 1.65;
}

.code-preview-block {
  height: 100%;
  margin: 0;
  padding: 16px 20px;
  overflow: auto;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  font-size: 12px;
  line-height: 1.6;
  color: #e4e4e7;
  background: #131316;
}

.modal-fade-enter-active,
.modal-fade-leave-active {
  transition: opacity 0.15s ease, transform 0.15s ease;
}

.modal-fade-enter-from,
.modal-fade-leave-to {
  opacity: 0;
  transform: scale(0.97);
}
</style>
