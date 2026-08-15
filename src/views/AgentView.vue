<template>
    <div class="manus-app-layout">
        <aside class="manus-sidebar" :class="{ collapsed: sidebarCollapsed }">
            <header class="sidebar-brand-row">
                <div class="brand-lockup">
                    <span class="brand-symbol">m</span>
                    <strong v-if="!sidebarCollapsed">manus</strong>
                </div>
                <div v-if="!sidebarCollapsed" class="brand-controls">
                    <button type="button" title="搜索"><svg viewBox="0 0 24 24"><circle cx="11" cy="11" r="7"/><path d="m20 20-4-4"/></svg></button>
                    <button type="button" title="收起侧栏" @click="sidebarCollapsed = true"><svg viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16"/></svg></button>
                </div>
                <button v-else class="expand-control" type="button" title="展开侧栏" @click="sidebarCollapsed = false"><svg viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16"/></svg></button>
            </header>

            <nav class="primary-nav">
                <button type="button" class="nav-row new-task" title="新建对话" @click="addTab"><svg viewBox="0 0 24 24"><path d="M5 3h11l3 3v15H5z"/><path d="M8 13h8M12 9v8"/></svg><span v-if="!sidebarCollapsed">新建对话</span></button>
                <button type="button" class="nav-row active" title="Agent"><svg viewBox="0 0 24 24"><rect x="4" y="5" width="16" height="14" rx="3"/><path d="M8 11h.01M16 11h.01M8 15h8"/></svg><span v-if="!sidebarCollapsed">Agent</span></button>
                <button type="button" class="nav-row" title="Plugins" @click="$router.push('/plugins')"><svg viewBox="0 0 24 24"><path d="M8 3v6M16 3v6M8 15v6M16 15v6"/><rect x="4" y="8" width="16" height="8" rx="2"/></svg><span v-if="!sidebarCollapsed">Plugins</span></button>
                <button type="button" class="nav-row" title="Scheduled"><svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="8"/><path d="M12 7v5l3 2"/></svg><span v-if="!sidebarCollapsed">Scheduled</span></button>
                <button type="button" class="nav-row" title="Library"><svg viewBox="0 0 24 24"><path d="M5 4h10a4 4 0 0 1 4 4v12H9a4 4 0 0 0-4 0z"/><path d="M9 4v16"/></svg><span v-if="!sidebarCollapsed">Library</span></button>
            </nav>

            <div v-if="!sidebarCollapsed" class="sidebar-section">
                <div class="section-title"><span>Projects</span><button type="button" @click="addTab">+</button></div>
                <button type="button" class="project-row" @click="addTab"><svg viewBox="0 0 24 24"><path d="M3 7h7l2 2h9v10H3z"/></svg><span>新建对话</span></button>
            </div>

            <div v-if="!sidebarCollapsed" class="sidebar-section tasks-section">
                <div class="section-title"><span>Tasks</span><span class="list-control">☷</span></div>
                <div class="task-list">
                    <div v-for="tab in tabs" :key="tab.id" class="task-row" :class="{ active: activeTabId === tab.id }" @click="activeTabId = tab.id" @dblclick="startRename(tab)">
                        <span class="task-state" :class="{ running: tab.running }"></span>
                        <input v-if="renamingId === tab.id" v-model="renameText" class="task-rename-box" @blur="doRename(tab.id)" @keydown.enter.prevent="doRename(tab.id)" @click.stop />
                        <span v-else class="task-label">{{ tab.label }}</span>
                        <button type="button" class="task-close" title="关闭" @click.stop="closeTab(tab.id)">×</button>
                    </div>
                </div>
            </div>

            <footer v-if="!sidebarCollapsed" class="sidebar-footer"><span class="profile-dot">f</span><span>free_api</span><button type="button">···</button></footer>
        </aside>

        <main class="agent-main-area">
            <template v-for="tab in tabs" :key="tab.id">
                <div v-show="activeTabId === tab.id" class="agent-panel-frame">
                    <AgentPanel
                        :ref="(el) => { if (el) panelRefs[tab.id] = el }"
                        :panel-id="tab.id"
                        :all-models="allModels"
                        @new-session="addTab"
                    />
                </div>
            </template>
        </main>
    </div>
</template>

<script setup lang="ts">
import { nextTick, onMounted, reactive, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import AgentPanel from "../components/AgentPanel.vue";
interface ModelOption { id: string; name: string; display_name: string; platform_id: string; platform_name: string; }
interface ChatSessionRow { id: string; title: string; model_id: string; created_at: string; updated_at: string; message_count: number; }
interface TabInfo { id: string; label: string; running: boolean; }
const allModels = ref<ModelOption[]>([]);
const sidebarCollapsed = ref(false);
const renamingId = ref<string | null>(null);
const renameText = ref("");
const tabs = ref<TabInfo[]>([]);
const activeTabId = ref("");
const panelRefs = reactive<Record<string, any>>({});

function asTab(session: ChatSessionRow): TabInfo { return { id: session.id, label: session.title || "未命名对话", running: false }; }
function defaultTitle() { return `Agent 任务 #${tabs.value.length + 1}`; }
async function addTab() {
    const created = await invoke<ChatSessionRow>("create_session", {
        title: defaultTitle(),
        modelId: allModels.value[0]?.id || "",
    });
    tabs.value.unshift(asTab(created));
    activeTabId.value = created.id;
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
watch(panelRefs, () => { for (const tab of tabs.value) { const panel = panelRefs[tab.id]; if (panel) tab.running = panel.isRunning; } }, { deep: true });
onMounted(async () => {
    try { allModels.value = await invoke<ModelOption[]>("get_all_models_with_platform"); } catch { allModels.value = []; }
    try { await loadSessions(); } catch { tabs.value = []; await addTab(); }
});
</script>

<style scoped>
.manus-app-layout { display: flex; width: 100%; height: 100%; overflow: hidden; color: #e7e7e9; background: #202022; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }.manus-sidebar { display: flex; width: 244px; flex: 0 0 244px; flex-direction: column; border-right: 1px solid #353537; background: #1b1b1d; transition: width .16s ease, flex-basis .16s ease; }.manus-sidebar.collapsed { width: 52px; flex-basis: 52px; }.sidebar-brand-row { display: flex; height: 54px; align-items: center; justify-content: space-between; padding: 0 14px; }.brand-lockup { display: flex; align-items: center; gap: 8px; }.brand-symbol { display: grid; width: 21px; height: 21px; place-items: center; border-radius: 7px; color: #1b1b1d; background: #efeff0; font-family: Georgia, serif; font-size: 14px; font-weight: 900; }.brand-lockup strong { color: #f0f0f1; font-family: Georgia, serif; font-size: 17px; letter-spacing: -.04em; }.brand-controls { display: flex; gap: 3px; }.brand-controls button,.expand-control { display: grid; width: 27px; height: 27px; place-items: center; border: 0; border-radius: 6px; color: #a2a2a8; cursor: pointer; background: transparent; }.brand-controls button:hover,.expand-control:hover { color: #eeeef0; background: #29292c; }.brand-controls svg,.expand-control svg,.nav-row svg,.project-row svg { width: 15px; height: 15px; fill: none; stroke: currentColor; stroke-linecap: round; stroke-linejoin: round; stroke-width: 1.7; }.primary-nav { display: grid; gap: 2px; padding: 7px 9px 14px; }.nav-row { display: flex; height: 34px; align-items: center; gap: 11px; padding: 0 9px; border: 0; border-radius: 7px; color: #a5a5ab; cursor: pointer; background: transparent; font: 13px inherit; text-align: left; }.nav-row:hover,.nav-row.active { color: #ededee; background: #2a2a2d; }.nav-row.new-task { margin-bottom: 4px; color: #e3e3e5; border: 1px solid #3b3b3f; background: #262629; }.manus-sidebar.collapsed .nav-row { justify-content: center; padding: 0; }.sidebar-section { padding: 7px 9px 0; }.section-title { display: flex; align-items: center; justify-content: space-between; padding: 0 7px 7px; color: #77777c; font-size: 11px; }.section-title button { border: 0; color: #9c9ca2; cursor: pointer; background: transparent; font-size: 17px; line-height: 1; }.list-control { color: #76767c; }.project-row { display: flex; width: 100%; align-items: center; gap: 9px; padding: 8px 7px; border: 0; border-radius: 6px; color: #adadb3; cursor: pointer; background: transparent; font: 12px inherit; text-align: left; }.project-row:hover { background: #27272a; color: #eeeeef; }.tasks-section { min-height: 0; flex: 1; padding-top: 20px; overflow: auto; }.task-list { display: grid; gap: 2px; }.task-row { display: flex; height: 33px; align-items: center; gap: 7px; padding: 0 7px; border-radius: 6px; color: #9b9ba1; cursor: pointer; }.task-row:hover,.task-row.active { color: #e7e7e9; background: #303033; }.task-state { width: 7px; height: 7px; flex: 0 0 auto; border-radius: 50%; background: #69696f; }.task-state.running { background: #e5e5e8; box-shadow: 0 0 0 3px rgba(229,229,232,.1); }.task-label { min-width: 0; flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 12px; }.task-close { display: none; width: 18px; height: 18px; padding: 0; border: 0; border-radius: 4px; color: #a5a5ab; cursor: pointer; background: transparent; font-size: 15px; }.task-row:hover .task-close { display: block; }.task-close:hover { color: #fff; background: #444449; }.task-rename-box { min-width: 0; flex: 1; border: 1px solid #5a5a60; border-radius: 4px; outline: 0; padding: 3px 5px; color: #e8e8ea; background: #1d1d1f; font: 12px inherit; }.sidebar-footer { display: flex; align-items: center; gap: 8px; padding: 14px; border-top: 1px solid #343436; color: #a5a5aa; font-size: 12px; }.profile-dot { display: grid; width: 21px; height: 21px; place-items: center; border-radius: 50%; color: #1c1c1e; background: #d9d9dc; font-size: 11px; font-weight: 800; }.sidebar-footer button { margin-left: auto; border: 0; color: #87878d; background: transparent; }.agent-main-area { min-width: 0; min-height: 0; flex: 1; overflow: hidden; }.agent-panel-frame { width: 100%; height: 100%; }.manus-sidebar.collapsed .sidebar-brand-row { justify-content: center; padding: 0; }
@media (max-width: 760px) { .manus-sidebar { width: 52px; flex-basis: 52px; }.manus-sidebar .brand-controls,.manus-sidebar .brand-lockup strong,.manus-sidebar .nav-row span,.manus-sidebar .sidebar-section,.manus-sidebar .sidebar-footer { display: none; }.manus-sidebar .nav-row { justify-content: center; padding: 0; }.sidebar-brand-row { justify-content: center; padding: 0; } }
</style>
