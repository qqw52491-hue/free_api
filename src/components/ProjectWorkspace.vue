<template>
  <div class="project-workspace-view">
    <!-- 顶部状态栏 -->
    <header class="pw-topbar">
      <div class="pw-breadcrumbs">
        <span class="crumb-root">Projects</span>
        <span class="crumb-sep">/</span>
        <span class="crumb-current">{{ project?.name || "加载中…" }}</span>
      </div>

      <div class="pw-top-actions">
        <div class="model-badge-chip">
          <span class="dot"></span>
          <span>{{ currentProModelName }}</span>
        </div>
        <button
          type="button"
          class="btn-icon-subtle"
          title="复制项目根路径"
          @click="copyProjectPath"
        >
          <svg viewBox="0 0 24 24"><path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2"></path><rect x="8" y="2" width="8" height="4" rx="1" ry="1"></rect></svg>
          <span v-if="copiedPath" class="copy-hint">已复制!</span>
        </button>
        <button
          type="button"
          class="btn-icon-subtle danger"
          title="解除项目登记"
          @click="showDeleteConfirm = true"
        >
          <svg viewBox="0 0 24 24"><polyline points="3 6 5 6 21 6"></polyline><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"></path></svg>
        </button>
      </div>
    </header>

    <!-- 主体双栏区域 (Manus Project Dashboard) -->
    <div class="pw-main-grid">
      <!-- 左侧：项目标题、发起任务输入框、历史任务列表 -->
      <div class="pw-center-column">
        <!-- 1. 项目 Hero 头部 -->
        <div class="pw-hero-card">
          <div class="pw-folder-icon">
            <svg viewBox="0 0 24 24">
              <path d="M3 7h7l2 2h9v10H3z"/>
            </svg>
          </div>
          <div class="pw-hero-meta">
            <h1 class="pw-project-title">{{ project?.name }}</h1>
            <div class="pw-project-sub">
              <span>绑定工作区 · <code>{{ project?.root_path }}</code></span>
              <span class="dot-sep">•</span>
              <span>更新于 {{ formatTime(project?.updated_at || project?.created_at) }}</span>
            </div>
          </div>
        </div>

        <!-- 2. 发起任务输入框 ("Start a task in this project" - 复用与 AgentComposer 一致的模型路由选择) -->
        <div class="pw-task-input-box" :class="{ focused: inputFocused }">
          <textarea
            ref="taskInputRef"
            v-model="taskGoal"
            class="pw-task-textarea"
            placeholder="在本项目中发起任务... (自动挂载本项目文件与目录上下文并直接开始执行)"
            rows="3"
            @focus="inputFocused = true"
            @blur="inputFocused = false"
            @keydown.enter.exact.prevent="submitNewTask"
          ></textarea>

          <div class="pw-input-bottom-bar">
            <div class="pw-input-left-actions">
              <button
                type="button"
                class="composer-tool-btn"
                title="添加数据文件到本项目 (Excel, Word, PDF, Code...)"
                @click="promptAddResource"
              >
                <svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></svg>
              </button>

              <!-- 🤖 复用与 AgentComposer 一致的模型路由浮层选择 -->
              <div class="model-picker-wrap" ref="modelPickerRef">
                <button
                  class="model-chip"
                  type="button"
                  :class="{ active: modelMenuOpen }"
                  title="配置模型路由 (推理 / 执行 / 视觉)"
                  @click="modelMenuOpen = !modelMenuOpen"
                >
                  <span class="model-icon">🤖</span>
                  <span>{{ currentProModelName }}</span>
                  <svg viewBox="0 0 24 24" class="chevron-icon"><path d="m8 10 4 4 4-4"/></svg>
                </button>

                <div v-if="modelMenuOpen" class="model-routing-popover">
                  <div class="popover-header">
                    <div class="header-left">
                      <div class="popover-title">Agent 模型路由</div>
                      <div class="popover-sub">规划、工具执行与视觉分析独立配置</div>
                    </div>
                    <button
                      type="button"
                      class="close-popover-btn"
                      title="关闭"
                      @click="modelMenuOpen = false"
                    >
                      <svg viewBox="0 0 24 24"><path d="M18 6 6 18M6 6l12 12"/></svg>
                    </button>
                  </div>

                  <div class="popover-body">
                    <!-- 推理模型 -->
                    <div class="route-item">
                      <div class="route-item-head">
                        <span class="route-badge pro">🧠 推理模型</span>
                        <small>负责长程规划与步骤拆解</small>
                      </div>
                      <ModelSelectDropdown
                        :model-value="modelRouting.pro"
                        :models="allModels || []"
                        placeholder="选择推理模型..."
                        size="sm"
                        @update:model-value="updateRouteValue('pro', $event)"
                      />
                    </div>

                    <!-- 执行模型 -->
                    <div class="route-item">
                      <div class="route-item-head">
                        <span class="route-badge flash">⚡ 执行模型</span>
                        <small>负责快速工具执行与摘要</small>
                      </div>
                      <ModelSelectDropdown
                        :model-value="modelRouting.flash"
                        :models="allModels || []"
                        placeholder="选择执行模型..."
                        size="sm"
                        @update:model-value="updateRouteValue('flash', $event)"
                      />
                    </div>

                    <!-- 视觉模型 -->
                    <div class="route-item">
                      <div class="route-item-head">
                        <span class="route-badge vision">👁️ 视觉模型</span>
                        <small>负责网页与截图理解</small>
                      </div>
                      <ModelSelectDropdown
                        :model-value="modelRouting.vision"
                        :models="allModels || []"
                        placeholder="选择视觉模型..."
                        size="sm"
                        @update:model-value="updateRouteValue('vision', $event)"
                      />
                    </div>
                  </div>
                </div>
              </div>

              <!-- ⚡ 上下文模式与压缩策略选择 -->
              <div class="budget-mode-picker" ref="budgetPickerRef">
                <button
                  class="budget-chip"
                  type="button"
                  :class="{ active: budgetMenuOpen }"
                  :title="currentBudgetTooltip"
                  @click="budgetMenuOpen = !budgetMenuOpen"
                >
                  <span class="budget-icon">{{ currentBudgetIcon }}</span>
                  <span class="budget-label">{{ currentBudgetLabel }}</span>
                  <svg viewBox="0 0 24 24" class="chevron-icon"><path d="m8 10 4 4 4-4"/></svg>
                </button>

                <div v-if="budgetMenuOpen" class="budget-dropdown-menu">
                  <div class="budget-menu-header">上下文与压缩策略</div>
                  <button
                    v-for="opt in budgetModes"
                    :key="opt.id"
                    type="button"
                    class="budget-menu-item"
                    :class="{ selected: opt.id === (modelRouting.context_budget_mode || 'deep_coding') }"
                    @click="selectBudgetMode(opt.id)"
                  >
                    <span class="opt-icon">{{ opt.icon }}</span>
                    <div class="opt-text">
                      <div class="opt-title">
                        <strong>{{ opt.description }}</strong>
                        <span class="opt-tag">{{ opt.tag }}</span>
                      </div>
                      <small class="opt-sub">{{ opt.tooltip }}</small>
                    </div>
                    <svg v-if="opt.id === (modelRouting.context_budget_mode || 'deep_coding')" class="check-icon" viewBox="0 0 24 24"><path d="M20 6 9 17l-5-5"/></svg>
                  </button>
                </div>
              </div>
            </div>

            <div class="pw-input-right-actions">
              <span class="keyboard-hint">Enter ↵ 发送</span>
              <button
                type="button"
                class="pw-send-btn"
                :disabled="!taskGoal.trim() || busy"
                title="在本项目中开启对话并直接执行"
                @click="submitNewTask"
              >
                <svg viewBox="0 0 24 24"><line x1="12" y1="19" x2="12" y2="5"></line><polyline points="5 12 12 5 19 12"></polyline></svg>
              </button>
            </div>
          </div>
        </div>

        <!-- 3. 本项目任务列表 (Tasks Section) -->
        <div class="pw-tasks-section">
          <div class="pw-tasks-header">
            <div>
              <h2 class="pw-section-title">Tasks ({{ sessions.length }})</h2>
              <p class="pw-section-subtitle">本项目专属对话，数据与受管文件在项目内自动继承共享</p>
            </div>
            <button
              type="button"
              class="btn-new-chat-subtle"
              title="在本项目下发起空白对话"
              @click="submitNewTaskDirect"
            >
              <svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></svg>
              <span>新建空白对话</span>
            </button>
          </div>

          <!-- 任务列表卡片 -->
          <div v-if="sessions.length" class="pw-tasks-grid">
            <div
              v-for="s in sessions"
              :key="s.id"
              class="pw-task-card"
              @click="emit('open-session', { id: s.id, title: s.title })"
            >
              <div class="pw-task-card-icon">
                <svg viewBox="0 0 24 24"><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"></path></svg>
              </div>
              <div class="pw-task-card-content">
                <div class="pw-task-card-title-row">
                  <span class="pw-task-card-title">{{ s.title || "未命名对话" }}</span>
                  <span class="pw-task-time">{{ formatTime(s.updated_at || s.created_at) }}</span>
                </div>
                <div class="pw-task-card-stats">
                  <span>{{ s.message_count || 0 }} 条历史记录</span>
                </div>
              </div>
              <button
                type="button"
                class="pw-task-delete-btn"
                title="删除会话"
                @click.stop="deleteSession(s.id)"
              >
                ×
              </button>
            </div>
          </div>

          <!-- 空任务提示 (Manus 风格) -->
          <div v-else class="pw-empty-tasks" @click="focusInput">
            <div class="empty-dashed-box">
              <svg class="empty-icon" viewBox="0 0 24 24">
                <rect x="3" y="3" width="18" height="18" rx="2" stroke-dasharray="4 4" fill="none"/>
                <path d="M12 8v8M8 12h8"/>
              </svg>
              <span class="empty-text">Create a new task to get started</span>
              <span class="empty-sub">在上方输入框键入任务指令，即可开启首个项目 Agent 任务</span>
            </div>
          </div>
        </div>
      </div>

      <!-- 右侧：项目受管文件与数据源 (Manus Right Rail) -->
      <aside class="pw-right-rail">
        <!-- 1. Files & sources (项目受管数据与文件) -->
        <div class="rail-card">
          <div class="rail-card-head">
            <div class="rail-card-title">
              <svg viewBox="0 0 24 24"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"></path><polyline points="14 2 14 8 20 8"></polyline></svg>
              <span>Files & sources ({{ resources.length }})</span>
            </div>
            <button
              type="button"
              class="rail-action-btn"
              title="添加/登记数据文件"
              @click="promptAddResource"
            >
              + Upload
            </button>
          </div>
          <p class="rail-desc">为本项目挂载核心数据（Excel、Word、PDF、代码），Agent 在所有任务中自动感知共享。</p>

          <!-- 资源文件搜索 -->
          <div v-if="resources.length > 3" class="rail-search-bar">
            <svg viewBox="0 0 24 24"><circle cx="11" cy="11" r="8"></circle><line x1="21" y1="21" x2="16.65" y2="16.65"></line></svg>
            <input v-model="resourceFilter" type="text" placeholder="过滤项目文件..." class="rail-search-input" />
          </div>

          <!-- 资源文件列表 -->
          <div v-if="filteredResources.length" class="rail-file-list">
            <div
              v-for="res in filteredResources"
              :key="res.resource_id"
              class="rail-file-item"
              @click="openResourcePreview(res.resource_id)"
            >
              <span class="file-type-badge">{{ kindBadge(res.relative_path) }}</span>
              <div class="file-info-col">
                <span class="file-label" :title="res.label">{{ res.label }}</span>
                <span class="file-sub">{{ res.relative_path }} · {{ formatBytes(res.size_bytes) }}</span>
              </div>
              <button
                type="button"
                class="file-remove-btn"
                title="取消登记"
                @click.stop="removeResource(res.resource_id)"
              >
                ×
              </button>
            </div>
          </div>
          <div v-else class="rail-empty-hint">
            <span>暂无受管数据文件，点击 + Upload 添加</span>
          </div>
        </div>

        <!-- 2. Statistics & Overview (工作区统计) -->
        <div class="rail-card stats-card">
          <div class="rail-card-title">
            <svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"></circle><polyline points="12 6 12 12 16 14"></polyline></svg>
            <span>Overview</span>
          </div>
          <div class="stats-grid">
            <div class="stat-box">
              <span class="stat-num">{{ sessions.length }}</span>
              <span class="stat-label">总对话数</span>
            </div>
            <div class="stat-box">
              <span class="stat-num">{{ resources.length }}</span>
              <span class="stat-label">受管文件</span>
            </div>
            <div class="stat-box">
              <span class="stat-num">{{ formatBytes(totalResourceBytes) }}</span>
              <span class="stat-label">数据总体积</span>
            </div>
          </div>
        </div>
      </aside>
    </div>

    <!-- ==================== 模态弹窗 ==================== -->

    <!-- 1. 解除项目登记确认弹窗 -->
    <transition name="modal-fade">
      <div v-if="showDeleteConfirm" class="modal-overlay" @click.self="showDeleteConfirm = false">
        <div class="modal-card modal-card-danger">
          <div class="modal-head">
            <div class="modal-title-row">
              <span class="text-red">⚠️</span>
              <h3>解除项目登记</h3>
            </div>
            <button type="button" class="modal-close-btn" @click="showDeleteConfirm = false">×</button>
          </div>
          <div class="modal-body">
            <p class="modal-desc">
              确定要解除项目 <strong class="text-white">“{{ project?.name }}”</strong> 的登记吗？
            </p>
            <p class="modal-desc-sub">
              此操作将清除该项目的受管资源索引和会话关联，但<strong>绝不会删除本地磁盘上的任何物理文件</strong>。
            </p>
          </div>
          <div class="modal-footer">
            <button type="button" class="btn-ghost" @click="showDeleteConfirm = false">取消</button>
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

    <!-- 2. 文件预览弹窗 -->
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
              <button type="button" class="btn-subtle" @click="copyPreviewContent">
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
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import ModelSelectDropdown from "./common/ModelSelectDropdown.vue";
import RichContentRenderer from "./charts/RichContentRenderer.vue";
import {
  CONTEXT_BUDGET_MODES,
  type AgentModelOption,
  type ContextBudgetMode,
  type ModelRouting,
} from "./agent/types";

function isMarkdownOrDoc(path?: string): boolean {
  if (!path) return true;
  const ext = path.split('.').pop()?.toLowerCase() || '';
  return ['md', 'markdown', 'mdown', 'mkdn', 'docx', 'doc', 'xlsx', 'xls', 'pdf', 'txt', 'csv', 'json'].includes(ext);
}

interface ChatSessionRow { id: string; title: string; model_id: string; created_at: string; updated_at: string; message_count: number; }

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

const MODEL_ROUTING_STORAGE_KEY = "free-api.agent.model-routing.v1";

const props = defineProps<{
  projectId: string;
  allModels?: AgentModelOption[];
}>();

const emit = defineEmits<{
  (event: "open-session", session: { id: string; title: string }): void;
  (event: "start-task", projectId: string, goal: string): void;
  (event: "project-deleted", projectId: string): void;
  (event: "refresh-projects"): void;
}>();

const project = ref<AgentProject | null>(null);
const sessions = ref<ChatSessionRow[]>([]);
const resources = ref<AgentProjectResource[]>([]);
const taskGoal = ref("");
const taskInputRef = ref<HTMLTextAreaElement | null>(null);
const inputFocused = ref(false);
const busy = ref(false);
const copiedPath = ref(false);
const showDeleteConfirm = ref(false);
const resourceFilter = ref("");
const modelMenuOpen = ref(false);
const budgetMenuOpen = ref(false);
const modelPickerRef = ref<HTMLElement | null>(null);
const budgetPickerRef = ref<HTMLElement | null>(null);

const budgetModes = CONTEXT_BUDGET_MODES;
const currentBudgetModeObj = computed(() => {
  const mode = modelRouting.value.context_budget_mode || "deep_coding";
  return budgetModes.find((item) => item.id === mode) || budgetModes[0];
});
const currentBudgetIcon = computed(() => currentBudgetModeObj.value.icon);
const currentBudgetLabel = computed(() => currentBudgetModeObj.value.label);
const currentBudgetTooltip = computed(() => currentBudgetModeObj.value.tooltip);

function selectBudgetMode(mode: ContextBudgetMode) {
  modelRouting.value.context_budget_mode = mode;
  try {
    localStorage.setItem(MODEL_ROUTING_STORAGE_KEY, JSON.stringify(modelRouting.value));
  } catch {
    // 忽略
  }
  budgetMenuOpen.value = false;
}

// 模型路由
const modelRouting = ref<ModelRouting>(loadModelRouting());

function loadModelRouting(): ModelRouting {
  try {
    const raw = localStorage.getItem(MODEL_ROUTING_STORAGE_KEY);
    if (raw) return JSON.parse(raw);
  } catch {
    // 忽略
  }
  return {
    reasoning_effort_version: 2,
    pro: "",
    flash: "",
    vision: "",
    pro_reasoning_effort: "auto",
    flash_reasoning_effort: "auto",
    vision_reasoning_effort: "auto",
    context_budget_mode: "deep_coding",
  };
}

function updateRouteValue(route: "pro" | "flash" | "vision", modelId: string) {
  modelRouting.value = {
    ...modelRouting.value,
    [route]: modelId,
  };
  try {
    localStorage.setItem(MODEL_ROUTING_STORAGE_KEY, JSON.stringify(modelRouting.value));
  } catch {
    // 忽略
  }
}

const currentProModelName = computed(() => {
  const currentId = modelRouting.value.pro;
  if (currentId && props.allModels) {
    const m = props.allModels.find((item) => item.id === currentId);
    if (m) return m.display_name || m.name;
  }
  if (props.allModels && props.allModels.length > 0) {
    return props.allModels[0].display_name || props.allModels[0].name;
  }
  return "配置模型路由";
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

const filteredResources = computed(() => {
  const q = resourceFilter.value.trim().toLowerCase();
  if (!q) return resources.value;
  return resources.value.filter((r) => r.label.toLowerCase().includes(q) || r.relative_path.toLowerCase().includes(q));
});

const totalResourceBytes = computed(() => {
  return resources.value.reduce((acc, r) => acc + (r.size_bytes || 0), 0);
});

async function loadProject() {
  if (!props.projectId) return;
  try {
    const all = await invoke<AgentProject[]>("list_agent_projects");
    project.value = all.find((p) => p.id === props.projectId) || null;
    if (project.value) {
      const [sList, rList] = await Promise.all([
        invoke<ChatSessionRow[]>("list_agent_project_sessions", { projectId: props.projectId }),
        invoke<AgentProjectResource[]>("list_agent_project_resources", { projectId: props.projectId }),
      ]);
      sessions.value = sList;
      resources.value = rList;
    }
  } catch (e) {
    console.error("加载项目详情失败:", e);
  }
}

function focusInput() {
  taskInputRef.value?.focus();
}

function submitNewTask() {
  const goal = taskGoal.value.trim();
  if (!goal || !props.projectId) return;
  taskGoal.value = "";
  emit("start-task", props.projectId, goal);
}

function submitNewTaskDirect() {
  if (!props.projectId) return;
  emit("start-task", props.projectId, "");
}

async function promptAddResource() {
  if (!project.value) return;
  try {
    const selected = await openDialog({
      multiple: false,
      directory: false,
      title: `在 ${project.value.name} 中选择数据文件 (Excel, Word, PDF, Code...)`,
      defaultPath: project.value.root_path,
    });
    if (typeof selected !== "string" || !selected) return;
    busy.value = true;
    await invoke("add_agent_project_resource", {
      projectId: project.value.id,
      path: selected,
    });
    await loadProject();
    emit("refresh-projects");
  } catch (e) {
    console.error("添加资源失败:", e);
  } finally {
    busy.value = false;
  }
}

async function removeResource(resourceId: string) {
  if (!props.projectId) return;
  try {
    await invoke("remove_agent_project_resource", { projectId: props.projectId, resourceId });
    await loadProject();
    emit("refresh-projects");
  } catch (e) {
    console.error("移除资源失败:", e);
  }
}

async function deleteSession(sessionId: string) {
  try {
    await invoke("delete_session", { id: sessionId });
    await loadProject();
    emit("refresh-projects");
  } catch (e) {
    console.error("删除会话失败:", e);
  }
}

async function submitDeleteProject() {
  if (!project.value) return;
  busy.value = true;
  try {
    await invoke("delete_agent_project", { projectId: project.value.id });
    showDeleteConfirm.value = false;
    emit("project-deleted", project.value.id);
  } catch (e) {
    console.error("删除项目失败:", e);
  } finally {
    busy.value = false;
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

async function copyPreviewContent() {
  if (!previewModal.content) return;
  try {
    await navigator.clipboard.writeText(previewModal.content);
    previewModal.copied = true;
    setTimeout(() => { previewModal.copied = false; }, 2000);
  } catch {
    // 忽略
  }
}

async function copyProjectPath() {
  if (!project.value?.root_path) return;
  try {
    await navigator.clipboard.writeText(project.value.root_path);
    copiedPath.value = true;
    setTimeout(() => { copiedPath.value = false; }, 2000);
  } catch {
    // 忽略
  }
}

function kindBadge(path: string): string {
  const ext = (path.split(".").pop() || "").toLowerCase();
  if (["xlsx", "xls", "csv"].includes(ext)) return "XLS";
  if (["docx", "doc"].includes(ext)) return "DOC";
  if (ext === "pdf") return "PDF";
  if (["ts", "js", "vue", "py", "rs", "go", "java", "sql"].includes(ext)) return "CODE";
  if (["md", "txt"].includes(ext)) return "DOC";
  return "FILE";
}

function formatBytes(bytes?: number): string {
  if (!bytes || bytes <= 0) return "0 B";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function formatTime(iso?: string): string {
  if (!iso) return "刚刚";
  try {
    const d = new Date(iso);
    return `${d.getMonth() + 1}月${d.getDate()}日 ${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  } catch {
    return iso;
  }
}

function handleClickOutside(e: MouseEvent) {
  const target = e.target as Node;
  // 如果点击的是被 Teleport 挂载到 body 上的模型下拉选择浮层，不要关闭外部的模型配置弹窗
  if (target instanceof Element && (target.closest('.model-dropdown-popover') || target.closest('.is-teleported'))) {
    return;
  }
  if (budgetPickerRef.value && !budgetPickerRef.value.contains(target)) {
    budgetMenuOpen.value = false;
  }
  if (modelPickerRef.value && !modelPickerRef.value.contains(target)) {
    modelMenuOpen.value = false;
  }
}

function handleKeyDown(e: KeyboardEvent) {
  if (e.key === "Escape") {
    if (modelMenuOpen.value) modelMenuOpen.value = false;
    if (budgetMenuOpen.value) budgetMenuOpen.value = false;
  }
}

watch(() => props.projectId, () => {
  loadProject();
});

onMounted(() => {
  loadProject();
  document.addEventListener("click", handleClickOutside, true);
  window.addEventListener("keydown", handleKeyDown);
});

onBeforeUnmount(() => {
  document.removeEventListener("click", handleClickOutside, true);
  window.removeEventListener("keydown", handleKeyDown);
});
</script>

<style scoped>
.project-workspace-view {
  display: flex;
  flex-direction: column;
  width: 100%;
  height: 100%;
  overflow: hidden;
  background: #141416;
  color: #e4e4e7;
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
}

/* 顶部状态栏 */
.pw-topbar {
  display: flex;
  height: 48px;
  align-items: center;
  justify-content: space-between;
  padding: 0 24px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.06);
  background: #18181b;
}

.pw-breadcrumbs {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
}

.crumb-root { color: #71717a; font-weight: 500; }
.crumb-sep { color: #52525b; font-size: 12px; }
.crumb-current { color: #f4f4f5; font-weight: 650; }

.pw-top-actions {
  display: flex;
  align-items: center;
  gap: 10px;
}

.model-badge-chip {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 3px 10px;
  border-radius: 20px;
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.1);
  font-size: 11.5px;
  color: #a1a1aa;
}

.model-badge-chip .dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: #10b981;
  box-shadow: 0 0 6px rgba(16, 185, 129, 0.6);
}

.btn-icon-subtle {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 5px 8px;
  border-radius: 6px;
  border: 1px solid transparent;
  background: transparent;
  color: #a1a1aa;
  cursor: pointer;
  font-size: 12px;
  transition: all 0.12s;
}

.btn-icon-subtle:hover {
  background: rgba(255, 255, 255, 0.08);
  color: #ffffff;
}

.btn-icon-subtle.danger:hover {
  background: rgba(239, 68, 68, 0.15);
  color: #f87171;
}

.btn-icon-subtle svg {
  width: 14px;
  height: 14px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
}

.copy-hint {
  font-size: 11px;
  color: #10b981;
}

/* 主体网格 (左宽中栏 + 右侧栏) */
.pw-main-grid {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 340px;
  height: calc(100% - 48px);
  overflow: hidden;
}

/* 左侧主体内容 */
.pw-center-column {
  padding: 32px 40px;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 28px;
}

/* 1. Hero 头部 */
.pw-hero-card {
  display: flex;
  align-items: center;
  gap: 16px;
}

.pw-folder-icon {
  display: grid;
  place-items: center;
  width: 48px;
  height: 48px;
  border-radius: 12px;
  background: rgba(255, 255, 255, 0.06);
  border: 1px solid rgba(255, 255, 255, 0.1);
  color: #f4f4f5;
}

.pw-folder-icon svg {
  width: 24px;
  height: 24px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
}

.pw-hero-meta {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.pw-project-title {
  margin: 0;
  font-size: 22px;
  font-weight: 750;
  letter-spacing: -0.02em;
  color: #ffffff;
}

.pw-project-sub {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12.5px;
  color: #71717a;
}

.pw-project-sub code {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  color: #a1a1aa;
}

.dot-sep { color: #52525b; }

/* 2. 发起任务输入框 */
.pw-task-input-box {
  display: flex;
  flex-direction: column;
  border-radius: 14px;
  background: #18181b;
  border: 1px solid rgba(255, 255, 255, 0.1);
  box-shadow: 0 4px 20px rgba(0, 0, 0, 0.3);
  transition: all 0.15s ease;
  overflow: visible;
  position: relative;
}

.pw-task-input-box.focused {
  border-color: rgba(255, 255, 255, 0.25);
  box-shadow: 0 6px 28px rgba(0, 0, 0, 0.45);
}

.pw-task-textarea {
  width: 100%;
  padding: 16px 18px 8px;
  background: transparent;
  border: none;
  outline: none;
  color: #f4f4f5;
  font-size: 14px;
  line-height: 1.5;
  resize: none;
  box-sizing: border-box;
}

.pw-task-textarea::placeholder {
  color: #52525b;
}

.pw-input-bottom-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 8px 14px 12px;
}

.pw-input-left-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  position: relative;
}

.composer-tool-btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  padding: 0;
  border-radius: 7px;
  border: 1px solid rgba(255, 255, 255, 0.08);
  background: rgba(255, 255, 255, 0.04);
  color: #a1a1aa;
  cursor: pointer;
  transition: all 0.12s ease;
}

.composer-tool-btn:hover {
  background: rgba(255, 255, 255, 0.08);
  color: #ffffff;
}

.composer-tool-btn svg {
  width: 15px;
  height: 15px;
  fill: none;
  stroke: currentColor;
  stroke-width: 2;
}

/* 🤖 模型路由选择浮层 (与 AgentComposer 风格一致) */
.model-picker-wrap {
  position: relative;
}

.model-chip {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 5px 10px;
  background: rgba(255, 255, 255, 0.04);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 8px;
  color: #e4e4e7;
  font-size: 12px;
  font-weight: 550;
  cursor: pointer;
  transition: all 0.12s ease;
}

.model-chip:hover {
  background: rgba(255, 255, 255, 0.08);
  border-color: rgba(255, 255, 255, 0.2);
}

.model-chip.active {
  background: rgba(255, 255, 255, 0.1);
  border-color: rgba(255, 255, 255, 0.25);
  color: #ffffff;
}

.model-icon { font-size: 12px; }

.chevron-icon {
  width: 13px;
  height: 13px;
  stroke: #71717a;
  stroke-width: 2;
  fill: none;
}

.model-routing-popover {
  position: absolute;
  top: calc(100% + 8px);
  bottom: auto;
  left: 0;
  width: 320px;
  max-height: min(480px, calc(100vh - 200px));
  overflow-y: auto;
  background: #1c1c20;
  border: 1px solid rgba(255, 255, 255, 0.14);
  border-radius: 12px;
  box-shadow: 0 16px 40px rgba(0, 0, 0, 0.7);
  z-index: 1200;
  display: flex;
  flex-direction: column;
}

.budget-mode-picker {
  position: relative;
  display: inline-flex;
  align-items: center;
}

.budget-chip {
  width: auto;
  height: 28px;
  padding: 0 9px;
  display: inline-flex;
  align-items: center;
  gap: 5px;
  font-size: 12px;
  font-weight: 500;
  color: #e4e4e7;
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 6px;
  cursor: pointer;
  transition: all 0.15s ease;
}

.budget-chip:hover {
  background: rgba(255, 255, 255, 0.09);
  border-color: rgba(255, 255, 255, 0.18);
}

.budget-chip.active {
  background: rgba(59, 130, 246, 0.15);
  border-color: rgba(59, 130, 246, 0.4);
  color: #93c5fd;
}

.budget-dropdown-menu {
  position: absolute;
  top: calc(100% + 8px);
  bottom: auto;
  left: 0;
  width: 260px;
  max-height: min(400px, calc(100vh - 200px));
  overflow-y: auto;
  background: #1e1e24;
  border: 1px solid rgba(255, 255, 255, 0.12);
  border-radius: 10px;
  box-shadow: 0 16px 36px rgba(0, 0, 0, 0.6);
  padding: 6px;
  z-index: 1200;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.budget-menu-header {
  font-size: 11px;
  font-weight: 600;
  color: #71717a;
  padding: 4px 8px 6px;
  text-transform: uppercase;
}

.budget-menu-item {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  padding: 8px;
  border: 1px solid transparent;
  border-radius: 6px;
  background: transparent;
  color: #d4d4d8;
  cursor: pointer;
  text-align: left;
  transition: all 0.12s ease;
}

.budget-menu-item:hover {
  background: rgba(255, 255, 255, 0.06);
  color: #ffffff;
}

.budget-menu-item.selected {
  background: rgba(59, 130, 246, 0.12);
  border-color: rgba(59, 130, 246, 0.3);
  color: #93c5fd;
}

.budget-menu-item .opt-icon { font-size: 14px; margin-top: 1px; }
.budget-menu-item .opt-text { display: flex; flex-direction: column; gap: 2px; flex: 1; min-width: 0; }
.budget-menu-item .opt-title { display: flex; align-items: center; gap: 6px; font-size: 12px; }
.budget-menu-item .opt-tag { font-size: 10px; padding: 1px 4px; border-radius: 3px; background: rgba(255, 255, 255, 0.08); color: #a1a1aa; }
.budget-menu-item .opt-sub { font-size: 10.5px; color: #71717a; line-height: 1.3; }
.budget-menu-item .check-icon { width: 14px; height: 14px; stroke: #3b82f6; stroke-width: 2.2; fill: none; margin-top: 2px; }

.popover-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 10px 14px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.06);
}

.popover-title {
  font-size: 12.5px;
  font-weight: 700;
  color: #f4f4f5;
}

.popover-sub {
  font-size: 10.5px;
  color: #71717a;
}

.close-popover-btn {
  background: transparent;
  border: none;
  color: #71717a;
  cursor: pointer;
  padding: 2px;
}

.close-popover-btn svg {
  width: 14px;
  height: 14px;
  stroke: currentColor;
  stroke-width: 2;
  fill: none;
}

.popover-body {
  padding: 12px 14px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.route-item {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.route-item-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.route-badge {
  font-size: 11px;
  font-weight: 650;
}

.route-badge.pro { color: #818cf8; }
.route-badge.flash { color: #38bdf8; }
.route-badge.vision { color: #f472b6; }

.route-item-head small {
  font-size: 10px;
  color: #71717a;
}

.pw-input-right-actions {
  display: flex;
  align-items: center;
  gap: 10px;
}

.keyboard-hint {
  font-size: 11.5px;
  color: #52525b;
}

.pw-send-btn {
  display: grid;
  place-items: center;
  width: 32px;
  height: 32px;
  border-radius: 8px;
  border: none;
  background: #f4f4f5;
  color: #141416;
  cursor: pointer;
  transition: all 0.12s ease;
}

.pw-send-btn:hover:not(:disabled) {
  background: #ffffff;
  transform: translateY(-1px);
}

.pw-send-btn:disabled {
  opacity: 0.3;
  cursor: not-allowed;
}

.pw-send-btn svg {
  width: 16px;
  height: 16px;
  fill: none;
  stroke: currentColor;
  stroke-width: 2.2;
}

/* 3. Tasks 列表区 */
.pw-tasks-section {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.pw-tasks-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.pw-section-title {
  margin: 0;
  font-size: 15px;
  font-weight: 700;
  color: #f4f4f5;
}

.pw-section-subtitle {
  margin: 2px 0 0;
  font-size: 12px;
  color: #71717a;
}

.btn-new-chat-subtle {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 5px 10px;
  border-radius: 6px;
  border: 1px solid rgba(255, 255, 255, 0.08);
  background: rgba(255, 255, 255, 0.04);
  color: #a1a1aa;
  font-size: 12px;
  font-weight: 550;
  cursor: pointer;
  transition: all 0.12s;
}

.btn-new-chat-subtle:hover {
  background: rgba(255, 255, 255, 0.08);
  color: #f4f4f5;
}

.btn-new-chat-subtle svg {
  width: 13px;
  height: 13px;
  fill: none;
  stroke: currentColor;
  stroke-width: 2;
}

.pw-tasks-grid {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.pw-task-card {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px 14px;
  border-radius: 10px;
  background: #18181b;
  border: 1px solid rgba(255, 255, 255, 0.06);
  cursor: pointer;
  transition: all 0.12s ease;
}

.pw-task-card:hover {
  background: #202024;
  border-color: rgba(255, 255, 255, 0.14);
}

.pw-task-card-icon {
  display: grid;
  place-items: center;
  width: 32px;
  height: 32px;
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.04);
  color: #a1a1aa;
  flex-shrink: 0;
}

.pw-task-card-icon svg {
  width: 16px;
  height: 16px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
}

.pw-task-card-content {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.pw-task-card-title-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.pw-task-card-title {
  font-size: 13.5px;
  font-weight: 600;
  color: #f4f4f5;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.pw-task-time {
  font-size: 11.5px;
  color: #71717a;
}

.pw-task-card-stats {
  font-size: 11.5px;
  color: #71717a;
}

.pw-task-delete-btn {
  display: none;
  width: 20px;
  height: 20px;
  border: none;
  border-radius: 4px;
  background: transparent;
  color: #71717a;
  font-size: 15px;
  cursor: pointer;
}

.pw-task-card:hover .pw-task-delete-btn {
  display: block;
}

.pw-task-delete-btn:hover {
  color: #f87171;
  background: rgba(239, 68, 68, 0.15);
}

/* 空任务提示 (Manus 风格) */
.pw-empty-tasks {
  cursor: pointer;
}

.empty-dashed-box {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 8px;
  padding: 40px 20px;
  border-radius: 12px;
  border: 1px dashed rgba(255, 255, 255, 0.12);
  background: rgba(255, 255, 255, 0.01);
  text-align: center;
  transition: all 0.15s;
}

.empty-dashed-box:hover {
  border-color: rgba(255, 255, 255, 0.22);
  background: rgba(255, 255, 255, 0.02);
}

.empty-icon {
  width: 32px;
  height: 32px;
  stroke: #52525b;
  stroke-width: 1.6;
}

.empty-text {
  font-size: 14px;
  font-weight: 600;
  color: #a1a1aa;
}

.empty-sub {
  font-size: 12px;
  color: #71717a;
}

/* 右侧栏 (Manus Right Rail) */
.pw-right-rail {
  border-left: 1px solid rgba(255, 255, 255, 0.06);
  background: #161619;
  padding: 24px 20px;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 20px;
}

.rail-card {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 16px;
  border-radius: 12px;
  background: #1c1c20;
  border: 1px solid rgba(255, 255, 255, 0.06);
}

.rail-card-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.rail-card-title {
  display: flex;
  align-items: center;
  gap: 7px;
  font-size: 13px;
  font-weight: 700;
  color: #f4f4f5;
}

.rail-card-title svg {
  width: 15px;
  height: 15px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
}

.rail-action-btn {
  padding: 3px 8px;
  border-radius: 5px;
  border: 1px solid rgba(255, 255, 255, 0.1);
  background: rgba(255, 255, 255, 0.05);
  color: #a1a1aa;
  font-size: 11.5px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.12s;
}

.rail-action-btn:hover {
  background: rgba(255, 255, 255, 0.1);
  color: #ffffff;
}

.rail-desc {
  margin: 0;
  font-size: 11.5px;
  color: #71717a;
  line-height: 1.45;
}

.rail-search-bar {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 8px;
  background: rgba(0, 0, 0, 0.2);
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 6px;
}

.rail-search-bar svg {
  width: 12px;
  height: 12px;
  stroke: #71717a;
  stroke-width: 2;
  fill: none;
}

.rail-search-input {
  width: 100%;
  border: none;
  outline: none;
  background: transparent;
  color: #f4f4f5;
  font-size: 11.5px;
}

.rail-file-list {
  display: flex;
  flex-direction: column;
  gap: 4px;
  max-height: 280px;
  overflow-y: auto;
}

.rail-file-item {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 8px;
  border-radius: 6px;
  background: rgba(255, 255, 255, 0.02);
  cursor: pointer;
  transition: all 0.12s;
}

.rail-file-item:hover {
  background: rgba(255, 255, 255, 0.06);
}

.file-type-badge {
  font-size: 9.5px;
  font-weight: 800;
  padding: 2px 4px;
  border-radius: 4px;
  background: rgba(255, 255, 255, 0.08);
  color: #fbbf24;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

.file-info-col {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 1px;
}

.file-label {
  font-size: 12px;
  font-weight: 550;
  color: #e4e4e7;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.file-sub {
  font-size: 10.5px;
  color: #71717a;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

.file-remove-btn {
  display: none;
  width: 16px;
  height: 16px;
  border: none;
  border-radius: 3px;
  background: transparent;
  color: #71717a;
  font-size: 13px;
  cursor: pointer;
}

.rail-file-item:hover .file-remove-btn {
  display: block;
}

.file-remove-btn:hover {
  color: #f87171;
  background: rgba(239, 68, 68, 0.15);
}

.rail-empty-hint {
  padding: 10px;
  text-align: center;
  font-size: 11px;
  color: #52525b;
}

/* Stats */
.stats-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 6px;
}

.stat-box {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 2px;
  padding: 8px 4px;
  border-radius: 6px;
  background: rgba(0, 0, 0, 0.2);
}

.stat-num {
  font-size: 15px;
  font-weight: 750;
  color: #f4f4f5;
}

.stat-label {
  font-size: 10.5px;
  color: #71717a;
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
