<template>
    <div class="app-layout">
        <!-- 侧边栏：集成对话、LLM 平台管理、插件、定时任务与技能库 -->
        <aside class="app-sidebar" :class="{ collapsed: sidebarCollapsed }">
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

            <nav class="primary-nav">
                <button
                    type="button"
                    class="nav-row new-task"
                    :class="{ 'icon-only': sidebarCollapsed }"
                    title="新建对话"
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
            </nav>

            <div v-if="!sidebarCollapsed" class="sidebar-section">
                <div class="section-title"><span>Projects</span><button type="button" title="新建任务" @click="createNewTask">+</button></div>
                <button type="button" class="project-row" @click="createNewTask">
                    <svg viewBox="0 0 24 24"><path d="M3 7h7l2 2h9v10H3z"/></svg>
                    <span>新建对话</span>
                </button>
            </div>

            <div v-if="!sidebarCollapsed" class="sidebar-section tasks-section">
                <div class="section-title"><span>Tasks</span><span class="list-control">☷</span></div>
                <div class="task-list">
                    <div
                        v-for="tab in tabs"
                        :key="tab.id"
                        class="task-row"
                        :class="{ active: currentNavView === 'agent' && activeTabId === tab.id }"
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
                        <span v-else class="task-label">{{ tab.label }}</span>
                        <button type="button" class="task-close" title="关闭任务" @click.stop="closeTab(tab.id)">×</button>
                    </div>
                </div>
            </div>

            <footer class="sidebar-footer">
                <span class="profile-dot">⚡</span>
                <span v-if="!sidebarCollapsed" class="profile-name">free_api</span>
            </footer>
        </aside>

        <!-- 右侧主工作区 (动态切换 Agent、LLM 管理、插件或技能库) -->
        <main class="app-main-content">
            <!-- 1. 🤖 Agent 对话主面板 -->
            <div v-show="currentNavView === 'agent'" class="full-view-panel">
                <template v-for="tab in tabs" :key="tab.id">
                    <div v-show="activeTabId === tab.id" class="agent-panel-frame">
                        <AgentPanel
                            :ref="(el) => { if (el) panelRefs[tab.id] = el }"
                            :panel-id="tab.id"
                            :all-models="allModels"
                            @new-session="createNewTask"
                            @open-plugins="currentNavView = 'plugins'"
                            @update:running="(val) => { tab.running = val; }"
                        />
                    </div>
                </template>
            </div>

            <!-- 2. ⚙️ LLM 平台与模型管理面板 (原平台管理) -->
            <div v-if="currentNavView === 'llm'" class="full-view-panel">
                <ApiManager @models-updated="loadAllModels" />
            </div>

            <!-- 3. 🔌 插件 / 提示词与 Skill 库 -->
            <div v-if="currentNavView === 'plugins' || currentNavView === 'library'" class="full-view-panel">
                <PluginView @back="currentNavView = 'agent'" />
            </div>

            <!-- 4. ⏱️ Scheduled 定时任务 -->
            <div v-if="currentNavView === 'scheduled'" class="full-view-panel scheduled-view">
                <div class="scheduled-card">
                    <span class="scheduled-icon">⏱️</span>
                    <h3>定时任务中心</h3>
                    <p>支持配置定时巡检、自动化总结或定期 Agent 运行计划。任务会在后台准时触发。</p>
                </div>
            </div>
        </main>
    </div>
</template>

<script setup lang="ts">
import { nextTick, onMounted, reactive, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import AgentPanel from "../components/AgentPanel.vue";
import ApiManager from "./ApiManager.vue";
import PluginView from "./PluginView.vue";

interface ModelOption { id: string; name: string; display_name: string; platform_id: string; platform_name: string; }
interface ChatSessionRow { id: string; title: string; model_id: string; created_at: string; updated_at: string; message_count: number; }
interface TabInfo { id: string; label: string; running: boolean; }

const currentNavView = ref<"agent" | "llm" | "plugins" | "scheduled" | "library">("agent");
const allModels = ref<ModelOption[]>([]);
const sidebarCollapsed = ref(false);
const renamingId = ref<string | null>(null);
const renameText = ref("");
const tabs = ref<TabInfo[]>([]);
const activeTabId = ref("");
const panelRefs = reactive<Record<string, any>>({});

function asTab(session: ChatSessionRow): TabInfo {
    return { id: session.id, label: session.title || "未命名对话", running: false };
}

function defaultTitle() {
    return `Agent 任务 #${tabs.value.length + 1}`;
}

async function addTab() {
    const created = await invoke<ChatSessionRow>("create_session", {
        title: defaultTitle(),
        modelId: allModels.value[0]?.id || "",
    });
    tabs.value.unshift(asTab(created));
    activeTabId.value = created.id;
}

async function createNewTask() {
    currentNavView.value = "agent";
    await addTab();
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

async function loadSessions() {
    const sessions = await invoke<ChatSessionRow[]>("get_sessions");
    tabs.value = sessions.map(asTab);
    if (!tabs.value.length) { await addTab(); return; }
    if (!tabs.value.some((tab) => tab.id === activeTabId.value)) activeTabId.value = tabs.value[0].id;
}

async function closeTab(id: string) {
    const index = tabs.value.findIndex((tab) => tab.id === id);
    if (index < 0) return;
    await invoke("delete_session", { id });
    tabs.value.splice(index, 1);
    delete panelRefs[id];
    if (!tabs.value.length) { await addTab(); return; }
    if (activeTabId.value === id) activeTabId.value = tabs.value[Math.max(0, index - 1)].id;
}

function startRename(tab: TabInfo) {
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
}

watch(panelRefs, () => {
    for (const tab of tabs.value) {
        const panel = panelRefs[tab.id];
        if (panel) tab.running = panel.isRunning;
    }
}, { deep: true });

onMounted(async () => {
    await loadAllModels();
    try {
        await loadSessions();
    } catch {
        tabs.value = [];
        await addTab();
    }
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

/* 折叠状态更宽敞、更舒适 (64px) */
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
    stroke-linecap: round;
    stroke-linejoin: round;
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

/* 导航项 */
.primary-nav {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 10px 8px 14px;
}

.app-sidebar.collapsed .primary-nav {
    padding: 10px 0;
    align-items: center;
    gap: 4px;
}

.nav-row {
    display: flex;
    height: 36px;
    align-items: center;
    gap: 11px;
    padding: 0 12px;
    border: 1px solid transparent;
    border-radius: 8px;
    color: #a1a1aa;
    cursor: pointer;
    background: transparent;
    font-size: 13.5px;
    font-weight: 500;
    text-align: left;
    transition: all 0.15s;
}

.nav-row svg {
    width: 18px;
    height: 18px;
    flex-shrink: 0;
    fill: none;
    stroke: currentColor;
    stroke-linecap: round;
    stroke-linejoin: round;
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
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.2);
}

.nav-row.new-task {
    margin-bottom: 6px;
    color: #f4f4f5;
    border: 1px solid rgba(255, 255, 255, 0.12);
    background: rgba(255, 255, 255, 0.05);
    font-weight: 600;
}
.nav-row.new-task:hover {
    background: rgba(255, 255, 255, 0.1);
}

/* 折叠状态下的单图标模式 */
.nav-row.icon-only {
    width: 44px;
    height: 44px;
    padding: 0;
    justify-content: center;
    border-radius: 10px;
    margin: 0 auto;
}

.nav-row.icon-only svg {
    width: 20px;
    height: 20px;
}

.nav-row.icon-only.new-task {
    margin-bottom: 6px;
}

/* 侧边栏项目和任务 */
.sidebar-section {
    padding: 6px 8px 0;
}

.section-title {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 8px 6px;
    color: #71717a;
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
}
.section-title button {
    border: 0;
    color: #a1a1aa;
    cursor: pointer;
    background: transparent;
    font-size: 16px;
    line-height: 1;
}

.list-control { color: #71717a; }

.project-row {
    display: flex;
    width: 100%;
    align-items: center;
    gap: 9px;
    padding: 8px 10px;
    border: 0;
    border-radius: 7px;
    color: #a1a1aa;
    cursor: pointer;
    background: transparent;
    font-size: 12.5px;
    text-align: left;
    transition: all 0.15s;
}
.project-row svg {
    width: 16px;
    height: 16px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.8;
}
.project-row:hover {
    background: rgba(255, 255, 255, 0.04);
    color: #f4f4f5;
}

.tasks-section {
    min-height: 0;
    flex: 1;
    padding-top: 12px;
    overflow-y: auto;
}

.task-list {
    display: flex;
    flex-direction: column;
    gap: 2px;
}

.task-row {
    display: flex;
    height: 34px;
    align-items: center;
    gap: 8px;
    padding: 0 10px;
    border-radius: 7px;
    color: #a1a1aa;
    cursor: pointer;
    transition: all 0.15s;
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

.task-state {
    width: 7px;
    height: 7px;
    flex: 0 0 auto;
    border-radius: 50%;
    background: #52525b;
    transition: all 0.2s ease;
}
.task-state.running {
    background: #38bdf8;
    box-shadow: 0 0 8px rgba(56, 189, 248, 0.8);
    animation: task-pulse 1.2s ease-in-out infinite;
}

@keyframes task-pulse {
    0%, 100% { transform: scale(0.85); opacity: 0.6; }
    50% { transform: scale(1.2); opacity: 1; box-shadow: 0 0 10px rgba(56, 189, 248, 1); }
}

.task-label {
    min-width: 0;
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12.5px;
}

.task-close {
    display: none;
    width: 18px;
    height: 18px;
    padding: 0;
    border: 0;
    border-radius: 4px;
    color: #71717a;
    cursor: pointer;
    background: transparent;
    font-size: 14px;
    line-height: 1;
}
.task-row:hover .task-close { display: block; }
.task-close:hover { color: #fff; background: rgba(255, 255, 255, 0.15); }

.task-rename-box {
    min-width: 0;
    flex: 1;
    border: 1px solid rgba(255, 255, 255, 0.2);
    border-radius: 5px;
    outline: 0;
    padding: 3px 6px;
    color: #f4f4f5;
    background: #18181b;
    font-size: 12px;
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

@media (max-width: 760px) {
    .app-sidebar { width: 64px; flex-basis: 64px; }
    .app-sidebar .brand-lockup,
    .app-sidebar .brand-toggle-btn,
    .app-sidebar .nav-row span,
    .app-sidebar .sidebar-section,
    .app-sidebar .profile-name { display: none; }
    .app-sidebar .sidebar-brand-row { justify-content: center; padding: 0; }
    .app-sidebar .primary-nav { padding: 10px 0; align-items: center; }
    .app-sidebar .sidebar-footer { justify-content: center; padding: 12px 0; }
}
</style>
