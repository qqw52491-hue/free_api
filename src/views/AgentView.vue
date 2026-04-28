<template>
    <div class="agent-tabs-container">
        <!-- 标签栏 -->
        <div class="tab-bar">
            <div
                v-for="tab in tabs"
                :key="tab.id"
                class="tab-item"
                :class="{ active: activeTabId === tab.id, running: tab.running }"
                @click="activeTabId = tab.id"
            >
                <span class="tab-status">{{ tab.running ? '⟳' : '🤖' }}</span>
                <span class="tab-label">{{ tab.label }}</span>
                <button
                    v-if="tabs.length > 1"
                    class="tab-close"
                    @click.stop="closeTab(tab.id)"
                    title="关闭此标签"
                >×</button>
            </div>
            <button class="tab-add" @click="addTab" title="新开并发 Agent">
                <span>＋</span>
            </button>
        </div>

        <!-- 面板区域 -->
        <div class="tab-panels">
            <div
                v-for="tab in tabs"
                :key="tab.id"
                class="tab-panel"
                :class="{ active: activeTabId === tab.id }"
            >
                <AgentPanel
                    :ref="(el: any) => { if (el) panelRefs[tab.id] = el; }"
                    :panel-id="tab.id"
                    :all-models="allModels"
                />
            </div>
        </div>
    </div>
</template>

<script setup lang="ts">
import { ref, reactive, onMounted, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import AgentPanel from "../components/AgentPanel.vue";

interface ModelOption {
    id: string;
    name: string;
    display_name: string;
    platform_id: string;
    platform_name: string;
}
interface TabInfo {
    id: string;
    label: string;
    running: boolean;
}

const allModels = ref<ModelOption[]>([]);

// 生成唯一面板 ID
function genId() {
    return Math.random().toString(36).substring(2, 10);
}

// 标签页管理
const firstId = genId();
const tabs = ref<TabInfo[]>([
    { id: firstId, label: "Agent #1", running: false },
]);
const activeTabId = ref(firstId);
const panelRefs = reactive<Record<string, any>>({});

let tabCounter = 1;

function addTab() {
    tabCounter++;
    const newId = genId();
    tabs.value.push({ id: newId, label: `Agent #${tabCounter}`, running: false });
    activeTabId.value = newId;
}

function closeTab(id: string) {
    const idx = tabs.value.findIndex(t => t.id === id);
    if (idx === -1 || tabs.value.length <= 1) return;

    // 如果关闭的是当前激活的标签，切到相邻标签
    if (activeTabId.value === id) {
        const nextIdx = idx > 0 ? idx - 1 : idx + 1;
        activeTabId.value = tabs.value[nextIdx].id;
    }

    tabs.value.splice(idx, 1);
    delete panelRefs[id];
}

// 监听子面板的 isRunning 状态，同步到标签页指示器
watch(panelRefs, () => {
    for (const tab of tabs.value) {
        const panel = panelRefs[tab.id];
        if (panel) {
            tab.running = panel.isRunning;
        }
    }
}, { deep: true });

onMounted(async () => {
    allModels.value = await invoke<ModelOption[]>("get_all_models_with_platform");
});
</script>

<style scoped>
.agent-tabs-container {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
}

/* ── 标签栏 ── */
.tab-bar {
    display: flex;
    align-items: center;
    background: var(--bg-1);
    border-bottom: 1px solid var(--border-1);
    padding: 0 8px;
    gap: 2px;
    flex-shrink: 0;
    height: 38px;
    overflow-x: auto;
}

.tab-item {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 14px;
    font-size: 12px;
    font-weight: 500;
    color: var(--text-3);
    cursor: pointer;
    border-radius: 6px 6px 0 0;
    border: 1px solid transparent;
    border-bottom: none;
    transition: all 0.2s;
    white-space: nowrap;
    position: relative;
    user-select: none;
}

.tab-item:hover {
    background: var(--surface-1);
    color: var(--text-2);
}

.tab-item.active {
    background: var(--bg-0);
    color: var(--text-1);
    border-color: var(--border-1);
    font-weight: 600;
}

.tab-item.active::after {
    content: '';
    position: absolute;
    bottom: -1px;
    left: 0;
    right: 0;
    height: 1px;
    background: var(--bg-0);
}

.tab-item.running .tab-status {
    animation: spin 1s linear infinite;
    color: var(--accent-light);
}

.tab-status {
    font-size: 13px;
}

.tab-label {
    max-width: 120px;
    overflow: hidden;
    text-overflow: ellipsis;
}

.tab-close {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    border: none;
    background: transparent;
    color: var(--text-4);
    font-size: 13px;
    cursor: pointer;
    line-height: 1;
    padding: 0;
    margin-left: 2px;
    transition: all 0.15s;
}

.tab-close:hover {
    background: rgba(255, 71, 87, 0.15);
    color: var(--red-light);
}

.tab-add {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    border-radius: 6px;
    border: 1px dashed var(--border-2);
    background: transparent;
    color: var(--text-4);
    font-size: 16px;
    cursor: pointer;
    margin-left: 4px;
    flex-shrink: 0;
    transition: all 0.2s;
}

.tab-add:hover {
    border-color: var(--accent);
    color: var(--accent-light);
    background: rgba(108, 99, 255, 0.06);
}

/* ── 面板区域 ── */
.tab-panels {
    flex: 1;
    overflow: hidden;
    position: relative;
}

.tab-panel {
    position: absolute;
    inset: 0;
    display: none;
}

.tab-panel.active {
    display: block;
}

/* ── 旋转动画 ── */
@keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
}
</style>
