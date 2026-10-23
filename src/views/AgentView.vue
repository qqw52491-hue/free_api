<template>
    <div class="app-layout">
        <!-- 移动端抽屉遮罩 -->
        <div
            v-if="mobileSidebarOpen"
            class="mobile-sidebar-backdrop"
            @click="mobileSidebarOpen = false"
        ></div>

        <!-- 侧边栏：集成对话、项目树、LLM 平台管理、插件、定时任务与技能库 -->
        <aside class="app-sidebar" :class="{ collapsed: sidebarCollapsed, 'mobile-open': mobileSidebarOpen }">
            <header class="sidebar-brand-row">
                <div v-if="!sidebarCollapsed" class="brand-lockup">
                    <span class="brand-symbol">⚡</span>
                    <strong>free api</strong>
                </div>
                <button
                    v-if="!sidebarCollapsed"
                    class="brand-toggle-btn"
                    type="button"
                    title="收起侧栏"
                    @click="sidebarCollapsed = true"
                >
                    <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
                        <rect x="3" y="4" width="18" height="16" rx="2"/>
                        <path d="M9 4v16"/>
                    </svg>
                </button>
                <button
                    v-else
                    class="brand-collapsed-logo"
                    type="button"
                    title="展开侧边栏"
                    @click="sidebarCollapsed = false"
                >
                    <span class="brand-symbol">⚡</span>
                    <span class="expand-icon-wrap" aria-label="展开侧边栏">
                        <svg viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16"/></svg>
                    </span>
                </button>
            </header>

            <!-- 顶部主功能导航 -->
            <nav class="primary-nav">
                <button
                    type="button"
                    class="nav-row new-task"
                    :class="{ 'icon-only': sidebarCollapsed }"
                    title="新建独立对话 (输入指令后自动持久化)"
                    @click="createNewTask"
                >
                    <svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></svg>
                    <span v-if="!sidebarCollapsed">新建对话</span>
                </button>
                <button
                    type="button"
                    class="nav-row"
                    :class="{ active: currentNavView === 'agent', 'icon-only': sidebarCollapsed }"
                    title="Agent 对话"
                    @click="currentNavView = 'agent'"
                >
                    <svg viewBox="0 0 24 24"><rect x="4" y="4" width="16" height="16" rx="4"/><circle cx="9" cy="10" r="1.5" fill="currentColor"/><circle cx="15" cy="10" r="1.5" fill="currentColor"/><path d="M9 15h6"/></svg>
                    <span v-if="!sidebarCollapsed">Agent</span>
                </button>
                <button
                    type="button"
                    class="nav-row"
                    :class="{ active: currentNavView === 'llm', 'icon-only': sidebarCollapsed }"
                    title="平台与模型管理 (LLM 管理)"
                    @click="currentNavView = 'llm'"
                >
                    <svg viewBox="0 0 24 24"><path d="M12 2v4M12 18v4M4.93 4.93l2.83 2.83M16.24 16.24l2.83 2.83M2 12h4M18 12h4M4.93 19.07l2.83-2.83M16.24 7.76l2.83-2.83"/><circle cx="12" cy="12" r="4"/></svg>
                    <span v-if="!sidebarCollapsed">LLM 管理</span>
                </button>
                <button
                    type="button"
                    class="nav-row"
                    :class="{ active: currentNavView === 'roles', 'icon-only': sidebarCollapsed }"
                    title="Agent 角色工坊与专家团队"
                    @click="currentNavView = 'roles'"
                >
                    <svg viewBox="0 0 24 24"><path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M23 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.13a4 4 0 0 1 0 7.75"/></svg>
                    <span v-if="!sidebarCollapsed">Roles 角色</span>
                </button>
                <button
                    type="button"
                    class="nav-row"
                    :class="{ active: currentNavView === 'plugins', 'icon-only': sidebarCollapsed }"
                    title="能力与 MCP 插件"
                    @click="currentNavView = 'plugins'"
                >
                    <svg viewBox="0 0 24 24"><path d="M9 2v4M15 2v4M9 18v4M15 18v4"/><rect x="5" y="6" width="14" height="12" rx="3"/></svg>
                    <span v-if="!sidebarCollapsed">Plugins</span>
                </button>
                <button
                    type="button"
                    class="nav-row"
                    :class="{ active: currentNavView === 'scheduled', 'icon-only': sidebarCollapsed }"
                    title="定时任务"
                    @click="currentNavView = 'scheduled'"
                >
                    <svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/></svg>
                    <span v-if="!sidebarCollapsed">Scheduled</span>
                </button>
                <button
                    type="button"
                    class="nav-row"
                    :class="{ active: currentNavView === 'library', 'icon-only': sidebarCollapsed }"
                    title="提示词与技能库"
                    @click="currentNavView = 'library'"
                >
                    <svg viewBox="0 0 24 24"><path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20"/><path d="M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z"/></svg>
                    <span v-if="!sidebarCollapsed">Library</span>
                </button>
                <button
                    type="button"
                    class="nav-row"
                    :class="{ active: currentNavView === 'wechat', 'icon-only': sidebarCollapsed }"
                    title="微信绑定与通道配置"
                    @click="currentNavView = 'wechat'"
                >
                    <svg viewBox="0 0 24 24"><path d="M8.5 3C5 3 2 5.6 2 8.9c0 1.9 1 3.5 2.5 4.6l-.7 2.3 2.6-1.3c.7.2 1.4.3 2.1.3h.4A5.7 5.7 0 0 1 8.5 13c0-3.3 3-5.9 6.7-5.9h.3C14.8 4.6 11.9 3 8.5 3z"/><path d="M22 15.4c0-2.8-2.6-5-5.8-5s-5.8 2.2-5.8 5 2.6 5 5.8 5c.7 0 1.4-.1 2-.3l2.3 1.2-.6-2.1A5 5 0 0 0 22 15.4z"/></svg>
                    <span v-if="!sidebarCollapsed">微信绑定</span>
                </button>
            </nav>

            <!-- 📁 & 💬 下方工作内容统一平滑滚动容器 (Projects + Tasks) -->
            <div v-if="!sidebarCollapsed" class="sidebar-scrollable-body">
                <!-- 1. 📁 项目树形区域 (Projects 树形折叠展开) -->
                <div class="sidebar-section projects-tree-section">
                    <ProjectSidebarTree
                        ref="projectTreeRef"
                        :all-models="allModels"
                        :active-session-id="activeTabId"
                        :active-project-id="currentNavView === 'project' ? selectedProjectId : undefined"
                        :drafts="tabs"
                        @select-project="handleSelectProject"
                        @open-session="openTreeSession"
                        @new-project-session="handleNewProjectSession"
                        @session-deleted="handleSessionDeleted"
                        @fork-session="(s) => forkSessionDirectly(s.id)"
                    />
                </div>

                <!-- 2. 💬 独立任务与对话列表 (Tasks 扁平列表) -->
                <div class="sidebar-section tasks-section">
                    <div class="section-title">
                        <span>Tasks ({{ independentTabs.length }})</span>
                        <button v-if="taskSearchQuery" type="button" class="clear-search-btn" title="清空搜索" @click="taskSearchQuery = ''">✕</button>
                        <span v-else class="list-control">☷</span>
                    </div>
                    <div v-if="tabs.length > 2 || taskSearchQuery" class="task-search-bar">
                        <svg viewBox="0 0 24 24"><circle cx="11" cy="11" r="8"></circle><line x1="21" y1="21" x2="16.65" y2="16.65"></line></svg>
                        <input v-model="taskSearchQuery" type="text" placeholder="搜索历史任务..." class="task-search-input" />
                    </div>
                    <div class="task-list">
                        <div
                            v-for="tab in independentTabs"
                            :key="tab.id"
                            class="task-row"
                            :class="{ active: currentNavView === 'agent' && activeTabId === tab.id, 'is-draft': tab.isDraft }"
                            @click="selectTask(tab.id)"
                            @dblclick="startRename(tab)"
                        >
                            <span class="task-state" :class="{ running: tab.running }"></span>
                            <input
                                v-if="renamingId === tab.id"
                                v-model="renameText"
                                class="task-rename-box"
                                @blur="doRename(tab.id)"
                                @keydown.enter.prevent="doRename(tab.id)"
                                @click.stop
                            />
                            <span v-else class="task-label" :title="tab.label">{{ tab.label }}</span>
                            <button
                                v-if="!tab.isDraft"
                                type="button"
                                class="task-action-btn fork-btn"
                                title="分叉此任务（完整克隆全部历史与状态为新会话）"
                                @click.stop="forkSessionDirectly(tab.id)"
                            >
                                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                                    <circle cx="6" cy="6" r="2.5" /><circle cx="18" cy="6" r="2.5" /><circle cx="6" cy="18" r="2.5" />
                                    <path d="M6 8.5v7" /><path d="M18 8.5c0 4-4 4-8.5 5.5" />
                                </svg>
                            </button>
                            <button type="button" class="task-close" title="关闭任务" @click.stop="closeTab(tab.id)">×</button>
                        </div>
                    </div>
                </div>
            </div>

            <footer class="sidebar-footer">
                <span class="profile-dot">⚡</span>
                <span v-if="!sidebarCollapsed" class="profile-name">free_api</span>
                <AppUpdateControl :collapsed="sidebarCollapsed" />
            </footer>
        </aside>

        <!-- 右侧主工作区 (动态切换 Agent 对话、Project 工作区、LLM 管理、插件或技能库) -->
        <main class="app-main-content">
            <!-- 📱 移动端极简顶部栏 (汉堡菜单呼出与当前标签指示) -->
            <header class="mobile-top-bar mobile-only">
                <button type="button" class="mobile-menu-trigger" @click="mobileSidebarOpen = true" title="打开菜单">
                    <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                        <line x1="3" y1="12" x2="21" y2="12"></line>
                        <line x1="3" y1="6" x2="21" y2="6"></line>
                        <line x1="3" y1="18" x2="21" y2="18"></line>
                    </svg>
                </button>
                <div class="mobile-brand-title">
                    <span class="brand-dot">⚡</span>
                    <strong>free api</strong>
                </div>
                <button type="button" class="mobile-new-chat-btn" @click="createNewTask" title="新建对话">
                    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                        <path d="M12 5v14M5 12h14" />
                    </svg>
                </button>
            </header>
            <!-- 1. 🤖 Agent 对话主面板 -->
            <div v-show="currentNavView === 'agent'" class="full-view-panel">
                <template v-for="tab in tabs" :key="tab.uid">
                    <div v-if="activatedPanelIds.has(tab.id)" v-show="activeTabId === tab.id" class="agent-panel-frame">
                        <AgentPanel
                            :ref="(el) => { if (el) panelRefs[tab.id] = el }"
                            :panel-id="tab.id"
                            :project-id="tab.projectId"
                            :all-models="allModels"
                            @new-session="createNewTask"
                            @open-plugins="currentNavView = 'plugins'"
                            @open-roles="currentNavView = 'roles'"
                            @update:running="(val) => { tab.running = val; }"
                            @update:title="(title) => { updateTabTitle(tab.id, title); }"
                            @promoted="handleSessionPromoted"
                            @forked="handleForked"
                            @show-toast="(text, type) => showToast(text, type)"
                            :initial-draft="forkDrafts[tab.id]"
                        />
                    </div>
                </template>
            </div>

            <!-- 2. 📁 Project 工作区面板 (类似 Manus Project Dashboard) -->
            <div v-if="currentNavView === 'project' && selectedProjectId" class="full-view-panel">
                <ProjectWorkspace
                    ref="projectWorkspaceRef"
                    :project-id="selectedProjectId"
                    :all-models="allModels"
                    @open-session="openTreeSession"
                    @start-task="handleStartProjectTask"
                    @project-deleted="handleProjectDeleted"
                    @session-deleted="handleSessionDeleted"
                    @refresh-projects="projectTreeRef?.refreshProjects()"
                />
            </div>

            <!-- 3. ⚙️ LLM 平台与模型管理面板 -->
            <div v-if="currentNavView === 'llm'" class="full-view-panel">
                <ApiManager @models-updated="loadAllModels" @back="currentNavView = 'agent'" />
            </div>

            <!-- 4. 🎭 专家工坊与角色卡配置 -->
            <div v-if="currentNavView === 'roles'" class="full-view-panel">
                <RoleManagerView />
            </div>

            <!-- 5. 🔌 插件 / 提示词与 Skill 库 -->
            <div v-if="currentNavView === 'plugins' || currentNavView === 'library'" class="full-view-panel">
                <PluginView @back="currentNavView = 'agent'" />
            </div>

            <!-- 6. ⏱️ Scheduled 定时任务 -->
            <div v-if="currentNavView === 'scheduled'" class="full-view-panel">
                <ScheduledView />
            </div>

            <!-- 7. 📱 微信绑定与通道配置 -->
            <div v-if="currentNavView === 'wechat'" class="full-view-panel">
                <WeChatBindView />
            </div>
        </main>

        <!-- 🌟 全屏智能拖拽悬停毛玻璃蒙层 -->
        <transition name="fade-drop">
            <div v-if="isDraggingOver" class="global-drag-overlay" :class="`view-${currentNavView}`">
                <div class="drag-glass-card">
                    <div class="drag-icon-pulse">
                        <span v-if="currentNavView === 'project'">📁</span>
                        <span v-else>📥</span>
                    </div>
                    <div class="drag-text-group">
                        <h3 v-if="currentNavView === 'project'">松开将数据文件添加至项目受管资源</h3>
                        <h3 v-else>松开将文件附加至当前会话</h3>
                        <p>
                            <span class="hint-highlight">💡 无论何处，拖入文件夹将自动识别并创建/导入本地新项目</span>
                        </p>
                        <div class="drag-badge-tags">
                            <span>🖼️ 图片视觉预览</span>
                            <span>📊 Excel / Word / PDF</span>
                            <span>💻 代码工程与日志</span>
                            <span>📁 本地项目文件夹</span>
                        </div>
                    </div>
                </div>
            </div>
        </transition>

        <!-- 🌟 全局轻量浮动操作 Toast -->
        <transition name="toast-slide">
            <div v-if="floatingToast" class="global-floating-toast" :class="`is-${floatingToast.type}`">
                <span class="toast-icon">
                    <template v-if="floatingToast.type === 'success'">✓</template>
                    <template v-else-if="floatingToast.type === 'warning'">⚠️</template>
                    <template v-else>ℹ️</template>
                </span>
                <span class="toast-text">{{ floatingToast.text }}</span>
            </div>
        </transition>
    </div>
</template>

<script setup lang="ts">
import { computed, defineAsyncComponent, nextTick, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import AgentPanel from "../components/AgentPanel.vue";
import AppUpdateControl from "../components/AppUpdateControl.vue";
import ProjectSidebarTree, { type AgentProject, type TreeSessionItem } from "../components/ProjectSidebarTree.vue";
import ProjectWorkspace from "../components/ProjectWorkspace.vue";
import { useGlobalFileDrop, type DraggedPathItem } from "../composables/useGlobalFileDrop";

const ApiManager = defineAsyncComponent(() => import("./ApiManager.vue"));
const PluginView = defineAsyncComponent(() => import("./PluginView.vue"));
const WeChatBindView = defineAsyncComponent(() => import("./WeChatBindView.vue"));
const ScheduledView = defineAsyncComponent(() => import("./ScheduledView.vue"));
const RoleManagerView = defineAsyncComponent(() => import("./RoleManagerView.vue"));

interface ModelOption { id: string; name: string; display_name: string; platform_id: string; platform_name: string; }
interface ChatSessionRow { id: string; title: string; model_id: string; created_at: string; updated_at: string; message_count: number; }
interface TabInfo { uid: string; id: string; label: string; running: boolean; projectId?: string; isDraft?: boolean; }
interface FloatingToast { id: number; text: string; type: "success" | "info" | "warning"; }

const currentNavView = ref<"agent" | "project" | "llm" | "roles" | "plugins" | "scheduled" | "library" | "wechat">("agent");
const selectedProjectId = ref<string>("");
const allModels = ref<ModelOption[]>([]);
const sidebarCollapsed = ref(false);
const mobileSidebarOpen = ref(false);
const renamingId = ref<string | null>(null);
const renameText = ref("");
const tabs = ref<TabInfo[]>([]);
const activeTabId = ref("");
const taskSearchQuery = ref("");
const panelRefs = reactive<Record<string, any>>({});
// AgentPanel 懒挂载：只有被激活过的 tab 才渲染面板。每个 panel 挂载即拉取历史消息、
// 注册事件监听——全量挂载数百条会话会拖垮启动。watch activeTabId 覆盖所有激活路径。
const activatedPanelIds = reactive(new Set<string>());
watch(activeTabId, (id) => { if (id) activatedPanelIds.add(id); }, { immediate: true });
const projectTreeRef = ref<InstanceType<typeof ProjectSidebarTree> | null>(null);
const projectWorkspaceRef = ref<InstanceType<typeof ProjectWorkspace> | null>(null);

const floatingToast = ref<FloatingToast | null>(null);
let toastTimer: ReturnType<typeof setTimeout> | null = null;

function showToast(text: string, type: "success" | "info" | "warning" = "success") {
    if (toastTimer) clearTimeout(toastTimer);
    floatingToast.value = { id: Date.now(), text, type };
    toastTimer = setTimeout(() => {
        floatingToast.value = null;
    }, 3500);
}

async function handleGlobalDrop(paths: string[]) {
    if (!paths || paths.length === 0) return;
    try {
        const items = await invoke<DraggedPathItem[]>("inspect_dragged_paths", { paths });
        if (!items || items.length === 0) return;

        // 1. 优先检查是否包含文件夹 (Directory)
        const dirItem = items.find((i) => i.is_dir);
        if (dirItem) {
            // 无论当前在哪个页面，拖入文件夹一律触发创建/导入本地项目
            sidebarCollapsed.value = false;
            projectTreeRef.value?.promptCreateProjectWithDir(dirItem.path);
            showToast(`已识别本地项目目录: ${dirItem.name}，请确认导入`, "info");
            return;
        }

        // 2. 全为文件：提取有效路径
        const validFilePaths = items.filter((i) => !i.is_dir && i.exists).map((i) => i.path);
        if (validFilePaths.length === 0) {
            showToast("拖入的文件不存在或已被移除", "warning");
            return;
        }

        // 2a. 场景 A: 当前在项目详情工作区 (Project Workspace)
        if (currentNavView.value === "project" && selectedProjectId.value) {
            const count = await projectWorkspaceRef.value?.addResourcesByPaths(validFilePaths);
            const prj = projectWorkspaceRef.value?.project;
            const prjName = prj?.name || "当前项目";
            showToast(`已成功为【${prjName}】导入 ${count || validFilePaths.length} 个数据文件`, "success");
            return;
        }

        // 2b. 场景 B: 当前在 Agent 对话主面板
        if (currentNavView.value === "agent" && activeTabId.value) {
            const panel = panelRefs[activeTabId.value];
            if (panel && typeof panel.attachFiles === "function") {
                const count = await panel.attachFiles(validFilePaths);
                showToast(`已为当前会话附加 ${count || validFilePaths.length} 个文件`, "success");
                return;
            }
        }

        // 2c. 场景 C: 其他视图或无活跃会话，自动切回对话并附加
        currentNavView.value = "agent";
        if (!activeTabId.value) {
            createNewTask();
        }
        await nextTick();
        setTimeout(async () => {
            const panel = panelRefs[activeTabId.value];
            if (panel && typeof panel.attachFiles === "function") {
                const count = await panel.attachFiles(validFilePaths);
                showToast(`已切换至会话并附加 ${count || validFilePaths.length} 个文件`, "success");
            }
        }, 150);
    } catch (err) {
        console.error("处理拖拽文件失败:", err);
        showToast("处理拖拽文件失败: " + String(err), "warning");
    }
}

const { isDraggingOver } = useGlobalFileDrop({
    onDropPaths: handleGlobalDrop,
});

// 独立会话列表过滤：只保留未与任何项目绑定的独立任务
const independentTabs = computed(() => {
    const projectSessionIds = new Set<string>();
    if (projectTreeRef.value?.projectSessionsMap) {
        for (const sessions of Object.values(projectTreeRef.value.projectSessionsMap)) {
            if (Array.isArray(sessions)) {
                for (const s of sessions) {
                    projectSessionIds.add(s.id);
                }
            }
        }
    }
    const query = taskSearchQuery.value.trim().toLowerCase();
    return tabs.value.filter((tab) => {
        if (tab.projectId || projectSessionIds.has(tab.id)) return false;
        if (!query) return true;
        return tab.label.toLowerCase().includes(query) || tab.id.toLowerCase().includes(query);
    });
});

function asTab(session: ChatSessionRow, projectId?: string): TabInfo {
    return {
        uid: `tab-${session.id}`,
        id: session.id,
        label: session.title || "未命名对话",
        running: false,
        projectId,
        isDraft: false
    };
}

// 点击项目节点：切换到 Manus 风格的 Project 工作区主面板
function handleSelectProject(project: AgentProject) {
    selectedProjectId.value = project.id;
    currentNavView.value = "project";
}

function handleProjectDeleted(deletedProjectId: string) {
    if (selectedProjectId.value === deletedProjectId) {
        selectedProjectId.value = "";
        currentNavView.value = "agent";
    }
    projectTreeRef.value?.refreshProjects();
}

// 在 Project 工作区直接发起任务
async function handleStartProjectTask(projectId: string, goal: string) {
    const draftId = `draft-${Date.now()}`;
    tabs.value.unshift({
        uid: `tab-${draftId}`,
        id: draftId,
        label: "新对话",
        running: false,
        projectId: projectId,
        isDraft: true,
    });
    activeTabId.value = draftId;
    currentNavView.value = "agent";

    if (goal.trim()) {
        await nextTick();
        setTimeout(() => {
            const panel = panelRefs[draftId];
            if (panel) {
                if (typeof panel.runAgent === "function") {
                    void panel.runAgent(goal.trim());
                } else if (panel.session?.runAgent) {
                    void panel.session.runAgent(goal.trim());
                }
            }
        }, 50);
    }
}

// 新建独立对话：未发消息前仅在前端保留草稿，不写数据库
function createNewTask() {
    currentNavView.value = "agent";
    const existingDraft = tabs.value.find((t) => !t.projectId && t.isDraft && !t.running);
    if (existingDraft) {
        activeTabId.value = existingDraft.id;
        return;
    }
    const draftId = `draft-${Date.now()}`;
    tabs.value.unshift({
        uid: `tab-${draftId}`,
        id: draftId,
        label: "新对话",
        running: false,
        isDraft: true,
    });
    activeTabId.value = draftId;
}

// 在项目下新建对话：未发消息前仅在前端保留草稿，绑定该项目
function handleNewProjectSession(project: AgentProject) {
    currentNavView.value = "agent";
    const existingDraft = tabs.value.find((t) => t.projectId === project.id && t.isDraft && !t.running);
    if (existingDraft) {
        activeTabId.value = existingDraft.id;
        return;
    }
    const draftId = `draft-${Date.now()}`;
    tabs.value.unshift({
        uid: `tab-${draftId}`,
        id: draftId,
        label: "新对话",
        running: false,
        projectId: project.id,
        isDraft: true,
    });
    activeTabId.value = draftId;
}

// 打开树形菜单中的会话
function openTreeSession(session: TreeSessionItem) {
    const existing = tabs.value.find((tab) => tab.id === session.id);
    if (!existing) {
        tabs.value.unshift({
            uid: `tab-${session.id}`,
            id: session.id,
            label: session.title || "未命名对话",
            running: false,
            isDraft: session.isDraft,
            projectId: session.projectId,
        });
    } else {
        // 已存在的 tab 提到最前——增量合并后新会话沉在列表末尾，
        // 打开（含 fork 落地）必须保证其在 Tasks 顶部可见
        const idx = tabs.value.indexOf(existing);
        if (idx > 0) {
            tabs.value.splice(idx, 1);
            tabs.value.unshift(existing);
        }
        if (session.projectId && !existing.projectId) {
            existing.projectId = session.projectId;
        }
        if (session.title && (existing.label === "未命名对话" || !existing.label)) {
            existing.label = session.title;
        }
    }
    activeTabId.value = session.id;
    currentNavView.value = "agent";
}

// ⑂ 分叉产物：打开新会话 tab，并把分叉点消息正文预填进它的输入框
interface ForkReport {
    new_session_id: string;
    title: string;
    prompt_seed?: string | null;
}
const forkDrafts = ref<Record<string, string>>({});
async function handleForked(payload: { sessionId: string; title: string; promptSeed?: string | null }) {
    if (payload.promptSeed) {
        forkDrafts.value[payload.sessionId] = payload.promptSeed;
    }
    await loadSessions();
    openTreeSession({ id: payload.sessionId, title: payload.title, isDraft: false });
    projectTreeRef.value?.refreshProjects();
    showToast(payload.promptSeed ? "已成功在此分叉新会话" : "已成功克隆并打开新会话", "success");
}

async function forkSessionDirectly(sessionId: string) {
    if (sessionId.startsWith("draft-")) return;
    const tab = tabs.value.find((t) => t.id === sessionId);
    if (tab?.running) {
        showToast("该会话正在执行任务，请等待完成或停止后再分叉", "warning");
        return;
    }
    try {
        const report = await invoke<ForkReport>("fork_agent_session", {
            sessionId,
            beforeMessageId: null,
            beforeTimestampMs: null,
            upToMessageId: null,
            upToTimestampMs: null,
        });
        await handleForked({
            sessionId: report.new_session_id,
            title: report.title,
            promptSeed: report.prompt_seed,
        });
    } catch (e) {
        console.error("[forkSessionDirectly] failed:", e);
        showToast("分叉会话失败: " + String(e), "warning");
    }
}

// 草稿会话在用户发送第一条消息后成功持久化，升级为正式数据库会话
function handleSessionPromoted(oldId: string, newId: string, title: string, projectId?: string) {
    const tab = tabs.value.find((t) => t.id === oldId);
    if (tab) {
        tab.id = newId;
        tab.label = title;
        tab.isDraft = false;
        if (projectId) tab.projectId = projectId;
        activatedPanelIds.delete(oldId);
        activatedPanelIds.add(newId);
    }
    if (activeTabId.value === oldId) {
        activeTabId.value = newId;
    }
    if (panelRefs[oldId]) {
        panelRefs[newId] = panelRefs[oldId];
        delete panelRefs[oldId];
    }
    projectTreeRef.value?.refreshProjects();
}

function handleSessionDeleted(sessionId: string) {
    const index = tabs.value.findIndex((t) => t.id === sessionId);
    if (index >= 0) {
        tabs.value.splice(index, 1);
        activatedPanelIds.delete(sessionId);
        delete panelRefs[sessionId];
        if (activeTabId.value === sessionId) {
            activeTabId.value = tabs.value[0]?.id || "";
        }
    }
}

function selectTask(tabId: string) {
    activeTabId.value = tabId;
    currentNavView.value = "agent";
}

async function loadAllModels() {
    try {
        allModels.value = await invoke<ModelOption[]>("get_all_models_with_platform");
    } catch {
        allModels.value = [];
    }
}

function extractSmartTitle(text: string): string {
    const trimmed = text.trim();
    if (!trimmed) return "";
    const clean = trimmed
        .replace(/^[\{\[\"\'\s#\->`]+/, "")
        .replace(/^(你好[呀啊吗~！!，,]*|请问[一下]*|请帮我[一下]*|帮我[一下]*|我想[要]*|请[您]*)+/i, "")
        .replace(/[\r\n]+/g, " ")
        .trim();
    if (!clean) return trimmed.slice(0, 18);
    if (clean.length <= 20) return clean;
    return `${clean.slice(0, 19)}…`;
}

async function backfillSessionTitles(sessions: ChatSessionRow[]) {
    for (const session of sessions) {
        if (!session.title || session.title.startsWith("Agent 任务") || session.title.startsWith("未命名") || session.title.includes("#")) {
            try {
                const traces = await invoke<any[]>("get_session_run_traces", { sessionId: session.id });
                if (traces && traces.length > 0) {
                    const firstGoal = traces[0]?.goal || "";
                    if (firstGoal) {
                        const smartTitle = extractSmartTitle(firstGoal);
                        if (smartTitle && smartTitle !== session.title) {
                            session.title = smartTitle;
                            await invoke("rename_session", { id: session.id, title: smartTitle });
                            const tab = tabs.value.find((t) => t.id === session.id);
                            if (tab) tab.label = smartTitle;
                            continue;
                        }
                    }
                }
                const msgs = await invoke<any[]>("get_messages", { sessionId: session.id });
                if (msgs && msgs.length > 0) {
                    const userMsg = msgs.find((m) => m.role === "user");
                    if (userMsg && userMsg.content) {
                        const smartTitle = extractSmartTitle(userMsg.content);
                        if (smartTitle && smartTitle !== session.title) {
                            session.title = smartTitle;
                            await invoke("rename_session", { id: session.id, title: smartTitle });
                            const tab = tabs.value.find((t) => t.id === session.id);
                            if (tab) tab.label = smartTitle;
                        }
                    }
                }
            } catch {
                // 忽略单个读取错误
            }
        }
    }
}

async function loadSessions() {
    try {
        const sessions = await invoke<ChatSessionRow[]>("get_sessions");
        
        // 增量合并现有 tabs：草稿与已激活(用户打开过)的 tab 永久保留——项目会话掉出
        // get_sessions 的 LIMIT 窗口也绝不被轮询踢掉并重置 activeTabId；
        // 仅驱逐"从未激活且已不在会话列表中"的纯列表条目（已删除会话的墓碑）
        const sessionIds = new Set(sessions.map((s) => s.id));
        const existingTabs = tabs.value.filter(
            (t) => t.isDraft || activatedPanelIds.has(t.id) || sessionIds.has(t.id)
        );
        const existingMap = new Map(existingTabs.map((t) => [t.id, t]));
        
        for (const s of sessions) {
            const existing = existingMap.get(s.id);
            if (existing) {
                if (s.title && s.title !== existing.label && !existing.label.includes("…")) {
                    existing.label = s.title;
                }
            } else {
                const newTab = asTab(s);
                existingTabs.push(newTab);
                existingMap.set(s.id, newTab);
            }
        }
        tabs.value = existingTabs;
        if (!tabs.value.length) {
            createNewTask();
            return;
        }
        if (!activeTabId.value || !tabs.value.some((tab) => tab.id === activeTabId.value)) {
            activeTabId.value = tabs.value[0].id;
        }
        void backfillSessionTitles(sessions);
    } catch {}
}

async function closeTab(id: string) {
    const index = tabs.value.findIndex((tab) => tab.id === id);
    if (index < 0) return;
    if (!id.startsWith("draft-")) {
        await invoke("delete_session", { id });
    }
    tabs.value.splice(index, 1);
    activatedPanelIds.delete(id);
    delete panelRefs[id];
    if (!tabs.value.length) { createNewTask(); return; }
    if (activeTabId.value === id) activeTabId.value = tabs.value[Math.max(0, index - 1)].id;
    projectTreeRef.value?.refreshProjects();
}

function startRename(tab: TabInfo) {
    if (tab.isDraft) return;
    renamingId.value = tab.id;
    renameText.value = tab.label;
    nextTick(() => (document.querySelector(".task-rename-box") as HTMLInputElement | null)?.focus());
}

async function doRename(id: string) {
    if (renamingId.value !== id) return;
    const title = renameText.value.trim();
    renamingId.value = null;
    if (!title) return;
    await invoke("rename_session", { id, title });
    const tab = tabs.value.find((item) => item.id === id);
    if (tab) tab.label = title;
    projectTreeRef.value?.refreshProjects();
}

async function updateTabTitle(id: string, newTitle: string) {
    const tab = tabs.value.find((item) => item.id === id);
    if (!tab) return;
    const cleanTitle = newTitle.trim();
    if (!cleanTitle || tab.label === cleanTitle) return;
    tab.label = cleanTitle;
    if (!id.startsWith("draft-")) {
        await invoke("rename_session", { id, title: cleanTitle });
    }
    projectTreeRef.value?.refreshProjects();
}

let unlistenRunStarted: UnlistenFn | null = null;
let unlistenRunDone: UnlistenFn | null = null;
let sidebarPollTimer: number | null = null;

onMounted(async () => {
    await loadAllModels();
    try {
        await loadSessions();
    } catch {
        tabs.value = [];
        createNewTask();
    }

    // 监听手机 / 微信 / 远程发起的任务启动和结束，实时刷新桌面端侧边栏任务与项目列表
    try {
        unlistenRunStarted = await listen<{ session_id: string; run_id: string }>("channel-run-started", async (event) => {
            await loadSessions();
            projectTreeRef.value?.refreshProjects();
            if (event.payload?.session_id) {
                const tab = tabs.value.find((t) => t.id === event.payload.session_id);
                if (tab) tab.running = true;
            }
        });
        unlistenRunDone = await listen<{ session_id: string }>("channel-run-done", async (event) => {
            await loadSessions();
            projectTreeRef.value?.refreshProjects();
            if (event.payload?.session_id) {
                const tab = tabs.value.find((t) => t.id === event.payload.session_id);
                if (tab) tab.running = false;
            }
        });
    } catch {}

    // 3 秒轻量轮询，确保在任意远程操作后桌面侧栏列表秒级自动同步
    sidebarPollTimer = window.setInterval(async () => {
        if (document.hidden) return;
        await loadSessions();
        projectTreeRef.value?.refreshProjects();
    }, 3000);
});

onUnmounted(() => {
    if (sidebarPollTimer !== null) clearInterval(sidebarPollTimer);
    unlistenRunStarted?.();
    unlistenRunDone?.();
});
</script>

<style scoped>
.app-layout {
    display: flex;
    width: 100vw;
    height: 100vh;
    overflow: hidden;
    color: #e4e4e7;
    background: #141416;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
}

/* ── 侧边栏 ── */
.app-sidebar {
    display: flex;
    width: 254px;
    flex: 0 0 254px;
    flex-direction: column;
    border-right: 1px solid rgba(255, 255, 255, 0.08);
    background: #18181b;
    transition: width 0.18s cubic-bezier(0.4, 0, 0.2, 1), flex-basis 0.18s cubic-bezier(0.4, 0, 0.2, 1);
    z-index: 10;
}

.app-sidebar.collapsed {
    width: 64px;
    flex: 0 0 64px;
}

.sidebar-brand-row {
    display: flex;
    height: 52px;
    align-items: center;
    justify-content: space-between;
    padding: 0 14px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
}

.app-sidebar.collapsed .sidebar-brand-row {
    justify-content: center;
    padding: 0;
}

.brand-lockup {
    display: flex;
    align-items: center;
    gap: 10px;
}

.brand-symbol {
    display: grid;
    width: 28px;
    height: 28px;
    place-items: center;
    border-radius: 7px;
    color: #fbbf24;
    background: rgba(251, 191, 36, 0.1);
    border: 1px solid rgba(251, 191, 36, 0.22);
    font-size: 14px;
    transition: all 0.15s;
}

.brand-lockup strong {
    color: #f4f4f5;
    font-size: 15.5px;
    font-weight: 750;
    letter-spacing: -0.02em;
}

.brand-toggle-btn {
    display: grid;
    width: 30px;
    height: 30px;
    place-items: center;
    border: 0;
    border-radius: 6px;
    color: #a1a1aa;
    cursor: pointer;
    background: transparent;
    transition: all 0.15s;
}
.brand-toggle-btn:hover {
    color: #ffffff;
    background: rgba(255, 255, 255, 0.08);
}
.brand-toggle-btn svg {
    width: 17px;
    height: 17px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.8;
}

.brand-collapsed-logo {
    position: relative;
    display: grid;
    width: 42px;
    height: 42px;
    place-items: center;
    border: 0;
    border-radius: 9px;
    cursor: pointer;
    background: transparent;
    transition: all 0.15s;
}

.brand-collapsed-logo .expand-icon-wrap {
    display: none;
    position: absolute;
    inset: 0;
    align-items: center;
    justify-content: center;
    background: #27272a;
    border-radius: 9px;
    border: 1px solid rgba(255, 255, 255, 0.12);
    color: #ffffff;
}

.brand-collapsed-logo .expand-icon-wrap svg {
    width: 18px;
    height: 18px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.8;
}

.brand-collapsed-logo:hover .brand-symbol {
    opacity: 0;
}

.brand-collapsed-logo:hover .expand-icon-wrap {
    display: flex;
}

/* 导航项：舒适舒展吸顶，恢复 31px 优雅高度与自然呼吸感 */
.primary-nav {
    display: flex;
    flex-direction: column;
    gap: 2.5px;
    padding: 8px 8px 8px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
    flex-shrink: 0;
}

.app-sidebar.collapsed .primary-nav {
    padding: 10px 0;
    align-items: center;
    gap: 4px;
}

.nav-row {
    display: flex;
    height: 31px; /* 优雅舒适高度，再也不局促拥挤 */
    align-items: center;
    gap: 10px;
    padding: 0 11px;
    border: 1px solid transparent;
    border-radius: 6px;
    color: #a1a1aa;
    cursor: pointer;
    background: transparent;
    font-size: 13px;
    font-weight: 500;
    text-align: left;
    transition: all 0.12s;
}

.nav-row svg {
    width: 15.5px;
    height: 15.5px;
    flex-shrink: 0;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.8;
}

.nav-row:hover {
    color: #f4f4f5;
    background: rgba(255, 255, 255, 0.05);
}

.nav-row.active {
    color: #ffffff;
    background: #27272a;
    border-color: rgba(255, 255, 255, 0.08);
    font-weight: 600;
}

.nav-row.new-task {
    height: 33px;
    margin-bottom: 3px;
    color: #f4f4f5;
    border: 1px solid rgba(255, 255, 255, 0.12);
    background: rgba(255, 255, 255, 0.05);
    font-weight: 600;
}
.nav-row.new-task:hover {
    background: rgba(255, 255, 255, 0.1);
}

.nav-row.icon-only {
    width: 40px;
    height: 40px;
    padding: 0;
    justify-content: center;
    border-radius: 9px;
    margin: 0 auto;
}

.nav-row.icon-only svg {
    width: 19px;
    height: 19px;
}

/* 📁 & 💬 下方工作内容统一平滑滚动容器 (Projects + Tasks) */
.sidebar-scrollable-body {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    overflow-x: hidden;
    display: flex;
    flex-direction: column;
}

/* 统一细窄平滑半透明暗黑滚动条 */
.sidebar-scrollable-body::-webkit-scrollbar {
    width: 4px;
}

.sidebar-scrollable-body::-webkit-scrollbar-track {
    background: transparent;
}

.sidebar-scrollable-body::-webkit-scrollbar-thumb {
    background: rgba(255, 255, 255, 0.12);
    border-radius: 4px;
}

.sidebar-scrollable-body::-webkit-scrollbar-thumb:hover {
    background: rgba(255, 255, 255, 0.28);
}

/* 侧边栏项目和任务 */
.sidebar-section {
    padding: 6px 8px 0;
}

.projects-tree-section {
    padding-top: 8px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
    flex-shrink: 0;
}

.section-title {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 8px 6px;
    color: #71717a;
    font-size: 11px;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    flex-shrink: 0;
}

.list-control { color: #71717a; }

.tasks-section {
    padding-top: 8px;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
}

.task-list {
    display: flex;
    flex-direction: column;
    gap: 2px;
}

.task-search-bar {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0 4px 8px;
    padding: 4px 8px;
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 6px;
}

.task-search-bar svg {
    width: 12px;
    height: 12px;
    stroke: #71717a;
    stroke-width: 2;
    fill: none;
    flex-shrink: 0;
}

.task-search-input {
    width: 100%;
    border: none;
    outline: none;
    background: transparent;
    color: #f4f4f5;
    font-size: 11.5px;
}

.clear-search-btn {
    border: none;
    background: transparent;
    color: #71717a;
    cursor: pointer;
    font-size: 11px;
    padding: 0 4px;
}

.task-list {
    display: flex;
    flex-direction: column;
    gap: 2px;
}

.task-row {
    display: flex;
    height: 32px;
    align-items: center;
    gap: 8px;
    padding: 0 10px;
    border-radius: 7px;
    color: #a1a1aa;
    cursor: pointer;
    transition: all 0.12s;
}

.task-row:hover {
    color: #f4f4f5;
    background: rgba(255, 255, 255, 0.04);
}

.task-row.active {
    color: #ffffff;
    background: #27272a;
    font-weight: 500;
}

.task-row.is-draft {
    color: #a1a1aa;
    font-style: italic;
}

.task-state {
    width: 6px;
    height: 6px;
    flex: 0 0 auto;
    border-radius: 50%;
    background: #52525b;
    transition: all 0.2s ease;
}

.task-state.running {
    background: #38bdf8;
    box-shadow: 0 0 6px rgba(56, 189, 248, 0.8);
    animation: task-pulse 1.2s ease-in-out infinite;
}

@keyframes task-pulse {
    0%, 100% { transform: scale(0.85); opacity: 0.6; }
    50% { transform: scale(1.2); opacity: 1; }
}

.task-label {
    min-width: 0;
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12.5px;
}

.task-close,
.task-action-btn {
    display: none;
    width: 16px;
    height: 16px;
    padding: 0;
    border: 0;
    border-radius: 4px;
    color: #71717a;
    cursor: pointer;
    background: transparent;
    align-items: center;
    justify-content: center;
    font-size: 13px;
    line-height: 1;
    transition: all 0.15s;
}

.task-row:hover .task-close { display: block; }
.task-row:hover .task-action-btn { display: inline-flex; }
.task-close:hover { color: #fff; background: rgba(255, 255, 255, 0.15); }
.task-action-btn:hover { color: #a5b4fc; background: rgba(129, 140, 248, 0.15); }

.task-rename-box {
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

/* 底部状态 */
.sidebar-footer {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px 14px;
    border-top: 1px solid rgba(255, 255, 255, 0.06);
    color: #a1a1aa;
    font-size: 12px;
}

.app-sidebar.collapsed .sidebar-footer {
    flex-direction: column;
    justify-content: center;
    padding: 12px 0;
}

.profile-dot {
    display: grid;
    width: 22px;
    height: 22px;
    place-items: center;
    border-radius: 6px;
    color: #fbbf24;
    background: rgba(251, 191, 36, 0.1);
    border: 1px solid rgba(251, 191, 36, 0.2);
    font-size: 11px;
    font-weight: 800;
}

.profile-name {
    font-weight: 500;
}

/* 主内容区域 */
.app-main-content {
    min-width: 0;
    min-height: 0;
    flex: 1;
    height: 100%;
    overflow: hidden;
    background: #141416;
}

.full-view-panel {
    width: 100%;
    height: 100%;
    overflow: hidden;
    animation: panel-enter 0.24s cubic-bezier(0.16, 1, 0.3, 1);
}

.agent-panel-frame {
    width: 100%;
    height: 100%;
    animation: panel-enter 0.24s cubic-bezier(0.16, 1, 0.3, 1);
}

@keyframes panel-enter {
    from {
        opacity: 0;
        transform: translateY(4px);
    }
    to {
        opacity: 1;
        transform: translateY(0);
    }
}

/* 定时任务占位 */
.scheduled-view {
    display: grid;
    height: 100%;
    place-items: center;
    padding: 30px;
}
.scheduled-card {
    display: grid;
    gap: 10px;
    max-width: 420px;
    padding: 28px;
    border-radius: 12px;
    background: #18181b;
    border: 1px solid rgba(255, 255, 255, 0.08);
    text-align: center;
    justify-items: center;
}
.scheduled-icon { font-size: 32px; }
.scheduled-card h3 { color: #f4f4f5; font-size: 16px; margin: 0; }
.scheduled-card p { color: #8e8e93; font-size: 13px; line-height: 1.6; margin: 0; }

@media (max-width: 768px) {
    .app-layout {
        position: relative;
        width: 100vw;
        height: 100vh;
        overflow: hidden;
        padding-top: var(--safe-area-top);
        padding-bottom: var(--safe-area-bottom);
    }
    /* 手机端彻底隐藏桌面常驻侧边栏，释放 100% 屏幕宽度给工作区 */
    .app-sidebar {
        position: fixed;
        top: 0;
        left: -280px;
        bottom: 0;
        width: 280px !important;
        flex-basis: 280px !important;
        z-index: 100;
        background: #141416 !important;
        box-shadow: 16px 0 36px rgba(0, 0, 0, 0.7);
        transition: transform 0.28s cubic-bezier(0.16, 1, 0.3, 1);
    }
    .app-sidebar.mobile-open {
        transform: translateX(280px);
    }
    .app-main-content {
        width: 100% !important;
        flex: 1 !important;
        min-width: 0 !important;
        padding-top: 0 !important;
    }
    .mobile-sidebar-backdrop {
        position: fixed;
        inset: 0;
        background: rgba(0, 0, 0, 0.65);
        backdrop-filter: blur(4px);
        z-index: 95;
    }
    .mobile-top-bar {
        display: flex;
        align-items: center;
        justify-content: space-between;
        padding: 8px 14px;
        background: rgba(20, 20, 22, 0.95);
        backdrop-filter: blur(16px);
        border-bottom: 1px solid rgba(255, 255, 255, 0.08);
        z-index: 20;
        flex-shrink: 0;
    }
    .mobile-menu-trigger, .mobile-new-chat-btn {
        display: flex;
        align-items: center;
        justify-content: center;
        width: 36px;
        height: 36px;
        border-radius: 8px;
        background: rgba(255, 255, 255, 0.05);
        border: 1px solid rgba(255, 255, 255, 0.1);
        color: #f4f4f5;
        cursor: pointer;
    }
    .mobile-brand-title {
        display: flex;
        align-items: center;
        gap: 6px;
        font-size: 15px;
        font-weight: 700;
        letter-spacing: -0.02em;
        color: #f4f4f5;
    }
}

/* 全屏智能拖拽悬停毛玻璃蒙层 */
.global-drag-overlay {
    position: fixed;
    inset: 0;
    z-index: 9999;
    background: rgba(10, 14, 23, 0.78);
    backdrop-filter: blur(14px) saturate(180%);
    display: flex;
    align-items: center;
    justify-content: center;
    pointer-events: none;
    padding: 32px;
}

.drag-glass-card {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    padding: 36px 48px;
    background: linear-gradient(135deg, rgba(30, 41, 59, 0.85), rgba(15, 23, 42, 0.92));
    border: 2px dashed rgba(99, 102, 241, 0.65);
    border-radius: 20px;
    box-shadow: 0 20px 50px rgba(0, 0, 0, 0.6), 0 0 30px rgba(99, 102, 241, 0.2);
    animation: dragPulseBorder 2s ease-in-out infinite alternate;
    max-width: 520px;
}

@keyframes dragPulseBorder {
    0% {
        border-color: rgba(99, 102, 241, 0.5);
        box-shadow: 0 20px 50px rgba(0, 0, 0, 0.6), 0 0 20px rgba(99, 102, 241, 0.15);
    }
    100% {
        border-color: rgba(129, 140, 248, 0.9);
        box-shadow: 0 20px 50px rgba(0, 0, 0, 0.6), 0 0 40px rgba(129, 140, 248, 0.35);
    }
}

.drag-icon-pulse {
    font-size: 48px;
    line-height: 1;
    margin-bottom: 16px;
    filter: drop-shadow(0 4px 12px rgba(99, 102, 241, 0.5));
}

.drag-text-group h3 {
    margin: 0 0 8px 0;
    font-size: 19px;
    font-weight: 700;
    color: #ffffff;
    letter-spacing: -0.01em;
}

.drag-text-group p {
    margin: 0 0 16px 0;
    font-size: 13px;
    color: #94a3b8;
}

.hint-highlight {
    display: inline-block;
    color: #38bdf8;
    background: rgba(56, 189, 248, 0.1);
    padding: 3px 10px;
    border-radius: 12px;
    border: 1px solid rgba(56, 189, 248, 0.25);
    font-weight: 500;
}

.drag-badge-tags {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    justify-content: center;
}

.drag-badge-tags span {
    font-size: 11px;
    padding: 3px 9px;
    border-radius: 6px;
    background: rgba(255, 255, 255, 0.07);
    color: #cbd5e1;
    border: 1px solid rgba(255, 255, 255, 0.08);
}

/* 全局飘窗 Toast */
.global-floating-toast {
    position: fixed;
    top: 20px;
    left: 50%;
    transform: translateX(-50%);
    z-index: 99999;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 18px;
    border-radius: 12px;
    background: rgba(15, 23, 42, 0.92);
    border: 1px solid rgba(255, 255, 255, 0.12);
    backdrop-filter: blur(16px);
    box-shadow: 0 10px 30px rgba(0, 0, 0, 0.45);
    color: #f8fafc;
    font-size: 13.5px;
    font-weight: 500;
    pointer-events: none;
}

.global-floating-toast.is-success {
    border-color: rgba(34, 197, 94, 0.35);
    background: linear-gradient(135deg, rgba(20, 83, 45, 0.9), rgba(15, 23, 42, 0.95));
}

.global-floating-toast.is-warning {
    border-color: rgba(245, 158, 11, 0.35);
    background: linear-gradient(135deg, rgba(120, 53, 15, 0.9), rgba(15, 23, 42, 0.95));
}

.global-floating-toast.is-info {
    border-color: rgba(56, 189, 248, 0.35);
    background: linear-gradient(135deg, rgba(12, 74, 110, 0.9), rgba(15, 23, 42, 0.95));
}

.global-floating-toast .toast-icon {
    font-weight: 700;
}

/* 动效 */
.fade-drop-enter-active,
.fade-drop-leave-active {
    transition: opacity 0.2s ease, transform 0.2s ease;
}
.fade-drop-enter-from,
.fade-drop-leave-to {
    opacity: 0;
    transform: scale(0.98);
}

.toast-slide-enter-active,
.toast-slide-leave-active {
    transition: all 0.25s cubic-bezier(0.16, 1, 0.3, 1);
}
.toast-slide-enter-from,
.toast-slide-leave-to {
    opacity: 0;
    transform: translate(-50%, -16px);
}
</style>
