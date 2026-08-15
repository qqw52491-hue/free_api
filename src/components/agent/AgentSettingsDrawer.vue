<template>
    <div v-if="open" class="settings-backdrop" @click.self="$emit('close')">
        <aside class="settings-drawer" aria-label="Agent 设置">
            <header>
                <div>
                    <span>AGENT SETTINGS</span>
                    <h2>模型与运行配置</h2>
                    <p>运行期间模型路由会锁定，避免中途改变执行策略。</p>
                </div>
                <button type="button" @click="$emit('close')">关闭</button>
            </header>
            <div class="settings-scroll">
                <section>
                    <div class="section-heading"><h3>模型路由</h3><small>{{ isRunning ? '运行中已锁定' : '任务开始前可调整' }}</small></div>
                    <label>推理模型
                        <select :value="modelRouting.pro" :disabled="isRunning" @change="updateRoute('pro', $event)">
                            <option value="" disabled>选择复杂推理模型</option>
                            <optgroup v-for="group in groupedModels" :key="group.platform" :label="group.platform">
                                <option v-for="model in group.models" :key="model.id" :value="model.id">{{ model.display_name || model.name }}</option>
                            </optgroup>
                        </select>
                    </label>
                    <label>执行模型
                        <select :value="modelRouting.flash" :disabled="isRunning" @change="updateRoute('flash', $event)">
                            <option value="" disabled>选择快速执行模型</option>
                            <optgroup v-for="group in groupedModels" :key="group.platform" :label="group.platform">
                                <option v-for="model in group.models" :key="model.id" :value="model.id">{{ model.display_name || model.name }}</option>
                            </optgroup>
                        </select>
                    </label>
                    <label>视觉模型
                        <select :value="modelRouting.vision" :disabled="isRunning" @change="updateRoute('vision', $event)">
                            <option value="" disabled>选择多模态模型</option>
                            <optgroup v-for="group in groupedModels" :key="group.platform" :label="group.platform">
                                <option v-for="model in group.models" :key="model.id" :value="model.id">{{ model.display_name || model.name }}</option>
                            </optgroup>
                        </select>
                    </label>
                </section>
                <section>
                    <label class="context-toggle">
                        <input :checked="logContext" :disabled="isRunning" type="checkbox" @change="$emit('update:log-context', ($event.target as HTMLInputElement).checked)" />
                        <span><strong>记录完整上下文</strong><small>用于任务复盘与调试。</small></span>
                    </label>
                </section>
                <section class="mcp-connection-section">
                    <div class="section-heading"><h3>MCP 连接</h3><small>{{ catalogSummary }}</small></div>
                    <p class="section-description">工具目录会按插件配置缓存；刷新会验证 stdio 连接并重新发现工具。</p>
                    <div v-if="catalogLoading" class="mcp-empty">正在读取已启用连接…</div>
                    <div v-else-if="mcpCatalog.length" class="mcp-connection-list">
                        <article v-for="entry in mcpCatalog" :key="entry.name" class="mcp-connection-card">
                            <div class="mcp-connection-main">
                                <span class="mcp-status-dot" :class="connectionState[entry.name] || (entry.cached ? 'ready' : 'idle')"></span>
                                <div>
                                    <strong>{{ entry.name }}</strong>
                                    <small>{{ entry.tool_count ? `${entry.tool_count} 个工具` : '尚未发现工具' }} · {{ entry.cached ? '目录已缓存' : '等待验证' }}</small>
                                </div>
                            </div>
                            <button type="button" class="mcp-refresh" :disabled="connectionState[entry.name] === 'testing'" @click="refreshConnection(entry.name)">
                                {{ connectionState[entry.name] === 'testing' ? '连接中…' : '刷新连接' }}
                            </button>
                            <p v-if="connectionError[entry.name]" class="mcp-error">{{ connectionError[entry.name] }}</p>
                        </article>
                    </div>
                    <div v-else class="mcp-empty">当前没有已启用的 MCP 连接。</div>
                    <button type="button" class="manage-mcp" @click="$emit('open-plugins')">管理 MCP 连接</button>
                </section>
                <section>
                    <div class="section-heading"><h3>快捷任务</h3><small>一键填充</small></div>
                    <div class="preset-list">
                        <button v-for="preset in presets" :key="preset.label" type="button" :disabled="isRunning" @click="$emit('apply-preset', preset.goal)">
                            <strong>{{ preset.label }}</strong><span>{{ preset.goal }}</span>
                        </button>
                    </div>
                </section>
            </div>
        </aside>
    </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { AGENT_TASK_PRESETS, type AgentModelOption } from "./types";

const props = defineProps<{
    open: boolean;
    allModels: AgentModelOption[];
    modelRouting: { pro: string; flash: string; vision: string };
    logContext: boolean;
    isRunning: boolean;
}>();

const emit = defineEmits<{
    close: [];
    "update:model-routing": [value: { pro: string; flash: string; vision: string }];
    "update:log-context": [value: boolean];
    "apply-preset": [goal: string];
    "open-plugins": [];
}>();

const presets = AGENT_TASK_PRESETS;
type McpConnectionState = "idle" | "testing" | "ready" | "error";
interface McpCatalogEntry {
    name: string;
    tool_count: number;
    cached: boolean;
}
const mcpCatalog = ref<McpCatalogEntry[]>([]);
const catalogLoading = ref(false);
const connectionState = ref<Record<string, McpConnectionState>>({});
const connectionError = ref<Record<string, string>>({});
const catalogSummary = computed(() => {
    if (catalogLoading.value) return "读取中";
    return mcpCatalog.value.length ? `${mcpCatalog.value.length} 个已启用连接` : "未配置";
});
async function loadMcpCatalog() {
    catalogLoading.value = true;
    try {
        mcpCatalog.value = await invoke<McpCatalogEntry[]>("get_mcp_tool_catalog", { name: null });
    } catch (error) {
        mcpCatalog.value = [];
        console.warn("无法读取 MCP 工具目录", error);
    } finally {
        catalogLoading.value = false;
    }
}
async function refreshConnection(name: string) {
    connectionState.value = { ...connectionState.value, [name]: "testing" };
    connectionError.value = { ...connectionError.value, [name]: "" };
    try {
        await invoke("refresh_mcp_plugin_connection", { name });
        connectionState.value = { ...connectionState.value, [name]: "ready" };
        await loadMcpCatalog();
    } catch (error) {
        connectionState.value = { ...connectionState.value, [name]: "error" };
        connectionError.value = { ...connectionError.value, [name]: String(error) };
    }
}
watch(() => props.open, (open) => {
    if (open) void loadMcpCatalog();
}, { immediate: true });
const groupedModels = computed(() => {
    const groups = new Map<string, AgentModelOption[]>();
    for (const model of props.allModels) {
        const platform = model.platform_name || "未分类平台";
        groups.set(platform, [...(groups.get(platform) || []), model]);
    }
    return [...groups.entries()].map(([platform, models]) => ({ platform, models }));
});

function updateRoute(key: "pro" | "flash" | "vision", event: Event) {
    const value = (event.target as HTMLSelectElement).value;
    emit("update:model-routing", { ...props.modelRouting, [key]: value });
}
</script>

<style scoped>
.settings-backdrop { position: fixed; z-index: 45; inset: 0; display: flex; justify-content: flex-end; background: rgba(3,7,15,.48); backdrop-filter: blur(2px); }
.settings-drawer { display: grid; width: min(420px, 100%); height: 100%; grid-template-rows: auto minmax(0,1fr); border-left: 1px solid var(--chat-line-strong); background: #101722; box-shadow: -20px 0 60px rgba(0,0,0,.26); }
.settings-drawer header { display: flex; justify-content: space-between; gap: 16px; padding: 21px 20px 17px; border-bottom: 1px solid var(--chat-line); }
.settings-drawer header span { display: block; color: var(--chat-accent); font-size: 10px; font-weight: 800; letter-spacing: .1em; }
.settings-drawer h2 { margin: 6px 0 4px; color: var(--chat-text); font-size: 18px; }
.settings-drawer header p { margin: 0; color: var(--chat-text-muted); font-size: 11px; line-height: 1.55; }
.settings-drawer header button { align-self: flex-start; border: 1px solid var(--chat-line-strong); border-radius: 7px; padding: 6px 8px; color: var(--chat-text-soft); cursor: pointer; background: var(--chat-tool-bg); font: inherit; font-size: 11px; }
.settings-scroll { min-height: 0; overflow-y: auto; }
.settings-scroll section { padding: 18px 20px; border-bottom: 1px solid var(--chat-line); }
.section-heading { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; margin-bottom: 12px; }
.section-heading h3 { margin: 0; color: var(--chat-text); font-size: 13px; }
.section-heading small { color: var(--chat-text-faint); font-size: 10px; }
.settings-scroll label { display: grid; gap: 6px; margin-top: 11px; color: var(--chat-text-muted); font-size: 11px; font-weight: 650; }
.settings-scroll select { width: 100%; padding: 9px 10px; border: 1px solid var(--chat-line-strong); border-radius: 8px; outline: 0; color: var(--chat-text); background: #0d1420; font: 12px inherit; }
.settings-scroll select:focus { border-color: var(--chat-accent); }
.context-toggle { grid-template-columns: auto 1fr; align-items: center; gap: 10px !important; margin: 0 !important; cursor: pointer; }
.context-toggle input { width: 15px; height: 15px; accent-color: var(--chat-accent); }
.context-toggle span { display: grid; gap: 3px; }
.context-toggle strong { color: var(--chat-text); font-size: 12px; }
.context-toggle small { color: var(--chat-text-faint); font-size: 11px; font-weight: 500; }
.preset-list { display: grid; gap: 7px; }
.preset-list button { display: grid; gap: 4px; padding: 10px; border: 1px solid var(--chat-line); border-radius: 9px; color: inherit; cursor: pointer; background: var(--chat-tool-bg); text-align: left; }
.preset-list button:hover:not(:disabled) { border-color: var(--chat-accent); background: var(--chat-hover); }
.preset-list button:disabled { cursor: not-allowed; opacity: .5; }
.preset-list strong { color: var(--chat-text-soft); font-size: 12px; }
.preset-list span { overflow: hidden; color: var(--chat-text-faint); font-size: 10px; line-height: 1.4; text-overflow: ellipsis; white-space: nowrap; }
.mcp-connection-section { display: grid; gap: 10px; }
.section-description { margin: -4px 0 1px; color: var(--chat-text-faint); font-size: 11px; line-height: 1.55; }
.mcp-connection-list { display: grid; gap: 7px; }
.mcp-connection-card { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 8px 10px; padding: 9px; border: 1px solid var(--chat-line); border-radius: 9px; background: var(--chat-tool-bg); }
.mcp-connection-main { display: flex; min-width: 0; align-items: flex-start; gap: 8px; }
.mcp-connection-main > div { display: grid; min-width: 0; gap: 3px; }
.mcp-connection-main strong { overflow: hidden; color: var(--chat-text); font-size: 12px; text-overflow: ellipsis; white-space: nowrap; }
.mcp-connection-main small { color: var(--chat-text-faint); font-size: 10px; }
.mcp-status-dot { width: 7px; height: 7px; margin-top: 4px; flex: 0 0 auto; border-radius: 50%; background: #6f7785; }
.mcp-status-dot.ready { background: #61c096; box-shadow: 0 0 0 3px rgba(97,192,150,.12); }
.mcp-status-dot.testing { background: var(--chat-accent); animation: mcp-pulse 1.05s ease-in-out infinite; }
.mcp-status-dot.error { background: #df7b80; }
.mcp-refresh,.manage-mcp { border: 1px solid var(--chat-line-strong); border-radius: 7px; color: var(--chat-text-soft); cursor: pointer; background: #0d1420; font: 600 10px inherit; }
.mcp-refresh { align-self: start; padding: 6px 7px; }
.mcp-refresh:hover:not(:disabled),.manage-mcp:hover { border-color: var(--chat-accent); color: var(--chat-text); background: var(--chat-hover); }
.mcp-refresh:disabled { cursor: wait; opacity: .65; }
.mcp-error { grid-column: 1 / -1; margin: 0; color: #df8b91; font-size: 10px; line-height: 1.45; word-break: break-word; }
.mcp-empty { padding: 9px; border: 1px dashed var(--chat-line-strong); border-radius: 8px; color: var(--chat-text-faint); font-size: 11px; }
.manage-mcp { width: 100%; padding: 8px; }
@keyframes mcp-pulse { 50% { opacity: .35; transform: scale(.82); } }

</style>
