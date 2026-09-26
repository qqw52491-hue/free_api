<template>
    <div v-if="open" class="settings-backdrop" @click.self="$emit('close')">
        <aside class="settings-drawer" data-testid="agent-settings-drawer" aria-label="Agent 设置">
            <header>
                <div>
                    <span>AGENT SETTINGS</span>
                    <h2>系统与运行配置</h2>
                    <p>管理 MCP 连接、工具发现与调试记录。</p>
                </div>
                <button type="button" @click="$emit('close')">关闭</button>
            </header>
            <div class="settings-scroll">
                <section class="mcp-connection-section">
                    <div class="section-heading"><h3>MCP 连接</h3><small>{{ catalogSummary }}</small></div>
                    <p class="section-description">工具目录会按插件配置缓存；刷新会验证 stdio 连接并重新发现工具。</p>
                    <div v-if="catalogLoading" class="mcp-empty">正在读取已启用连接…</div>
                    <div v-else-if="mcpCatalog.length" class="mcp-connection-list">
                        <article v-for="entry in mcpCatalog" :key="entry.name" class="mcp-connection-card">
                            <div class="mcp-connection-main">
                                <span class="mcp-status-dot" :class="connectionState[entry.name] || (entry.last_error ? 'error' : (entry.cached || entry.last_ok_at ? 'ready' : 'idle'))"></span>
                                <div>
                                    <strong>{{ entry.name }}</strong>
                                    <small v-if="entry.last_error" class="mcp-last-err">上次失败：{{ entry.last_error.slice(0, 60) }}</small>
                                    <small v-else-if="entry.last_ok_at">上次握手 {{ entry.last_ok_at.slice(5, 16) }} · {{ entry.tool_count || '—' }} 工具</small>
                                    <small v-else>{{ entry.tool_count ? `${entry.tool_count} 个工具` : '尚未发现工具' }} · {{ entry.cached ? '目录已缓存' : '等待验证' }}</small>
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
                <section class="retry-config-section">
                    <div class="section-heading"><h3>模型请求重试</h3><small>{{ isRunning ? '运行中已锁定' : '可自定义输入 (0 ~ 100)' }}</small></div>
                    <p class="section-description">当 API 遇到 429 速率限制、网络连接超时或上游抖动时自动退避重试。</p>
                    <div class="retry-input-row">
                        <label for="llm-retry-input"><strong>最大重试次数</strong></label>
                        <div class="retry-control-box">
                            <input
                                id="llm-retry-input"
                                type="number"
                                min="0"
                                max="100"
                                class="retry-number-input"
                                :value="retryAttempts"
                                :disabled="isRunning"
                                @input="onRetryInput(($event.target as HTMLInputElement).value)"
                            />
                            <span class="unit-label">次</span>
                        </div>
                    </div>
                    <div class="retry-presets">
                        <button
                            v-for="preset in [0, 1, 3, 5, 10, 20, 50]"
                            :key="preset"
                            type="button"
                            class="preset-chip"
                            :class="{ active: retryAttempts === preset }"
                            :disabled="isRunning"
                            @click="$emit('update:retry-attempts', preset)"
                        >
                            {{ preset === 0 ? '0 (不重试)' : preset === 3 ? '3 (默认)' : `${preset} 次` }}
                        </button>
                    </div>
                    <p class="retry-policy-note">
                        ⏳ <strong>指数退避机制</strong>：重试间隔按 1s → 2s → 4s → 8s 递增（单次封顶 15s）。使用共享/公共免费高并发接口时，可调大重试次数（如 20~50 次）持续排队直到可用。
                    </p>
                </section>
                <section class="role-cards-section">
                    <div class="section-heading"><h3>专精角色池</h3><small>{{ roleCards.length ? `${roleCards.length} 张角色卡` : '未创建' }}</small></div>
                    <p class="section-description">角色卡 = 人设提示词 + 钉死模型 + 作用域。Agent 派发子任务时按角色标识挂载；上限 8 张。</p>
                    <div v-if="roleCardsLoading" class="mcp-empty">正在读取角色池…</div>
                    <div v-else-if="roleCards.length" class="role-card-list">
                        <article v-for="card in roleCards" :key="card.id" class="role-card-item" :class="{ disabled: !card.enabled }">
                            <div class="role-card-main">
                                <strong>{{ card.label || card.name }} <small>[{{ card.name }}]</small></strong>
                                <small>{{ roleModelLabel(card.model_id) }} · {{ card.enabled ? '启用' : '已禁用' }}<template v-if="card.default_test_command"> · {{ card.default_test_command }}</template></small>
                                <p v-if="card.persona_prompt">{{ card.persona_prompt.length > 90 ? card.persona_prompt.slice(0, 90) + '…' : card.persona_prompt }}</p>
                            </div>
                            <div class="role-card-actions">
                                <button type="button" @click="editRoleCard(card)">编辑</button>
                                <button type="button" @click="toggleRoleCard(card.name)">{{ card.enabled ? '禁用' : '启用' }}</button>
                                <button type="button" class="danger" @click="deleteRoleCard(card.name)">删除</button>
                            </div>
                        </article>
                    </div>
                    <div v-else class="mcp-empty">角色池为空——创建第一张角色卡（如"前端审美大师"绑定擅长 UI 的模型）。</div>
                    <button type="button" class="manage-mcp" @click="roleFormOpen = !roleFormOpen">{{ roleFormOpen ? '收起表单' : '+ 新增角色卡' }}</button>
                    <div v-if="roleFormOpen" class="role-form">
                        <label>角色标识 (ASCII)*<input v-model="roleForm.name" :disabled="!!roleForm.editingId" placeholder="frontend_designer" /></label>
                        <label>显示名<input v-model="roleForm.label" placeholder="前端审美大师" /></label>
                        <div class="role-form-field">
                            <span class="role-field-label">钉死模型（留空跟随档位路由）</span>
                            <ModelSelectDropdown
                                :model-value="roleForm.model_id"
                                :models="roleModelOptions || []"
                                placeholder="跟随档位路由 (可搜索指定模型)..."
                                size="sm"
                                @update:model-value="roleForm.model_id = $event"
                            />
                        </div>
                        <label>人设提示词*<textarea v-model="roleForm.persona_prompt" rows="4" placeholder="专精 Tailwind、暗黑玻璃拟态、微交互与响应式排版……"></textarea></label>
                        <label>作用域提示（逗号分隔，可选）<input v-model="roleForm.scope_hints" placeholder="src/components, src/assets" /></label>
                        <label>默认测试命令（可选）<input v-model="roleForm.default_test_command" placeholder="npm run build" /></label>
                        <p v-if="roleFormError" class="mcp-error">{{ roleFormError }}</p>
                        <div class="role-form-actions">
                            <button type="button" class="manage-mcp" :disabled="roleSaving" @click="saveRoleCard">{{ roleSaving ? '保存中…' : (roleForm.editingId ? '保存修改' : '创建角色卡') }}</button>
                            <button type="button" @click="roleFormOpen = false">取消</button>
                        </div>
                    </div>
                </section>
                <section>
                    <div class="section-heading"><h3>调试配置</h3><small>{{ isRunning ? '运行中已锁定' : '可随时切换' }}</small></div>
                    <label class="context-toggle">
                        <input :checked="logContext" :disabled="isRunning" type="checkbox" @change="$emit('update:log-context', ($event.target as HTMLInputElement).checked)" />
                        <span><strong>记录完整上下文</strong><small>用于任务复盘与调试。</small></span>
                    </label>
                </section>
            </div>
        </aside>
    </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import ModelSelectDropdown from "../common/ModelSelectDropdown.vue";

const props = withDefaults(defineProps<{
    open: boolean;
    logContext: boolean;
    retryAttempts?: number;
    isRunning: boolean;
}>(), {
    retryAttempts: 3,
});

const emit = defineEmits<{
    close: [];
    "update:log-context": [value: boolean];
    "update:retry-attempts": [value: number];
    "open-plugins": [];
}>();

function onRetryInput(raw: string) {
    const parsed = parseInt(raw, 10);
    if (!Number.isNaN(parsed)) {
        const clamped = Math.max(0, Math.min(100, parsed));
        emit("update:retry-attempts", clamped);
    }
}

type McpConnectionState = "idle" | "testing" | "ready" | "error";
interface McpCatalogEntry {
    name: string;
    tool_count: number;
    cached: boolean;
    last_ok_at?: string;
    last_error?: string;
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
        const entries = await invoke<McpCatalogEntry[]>("get_mcp_tool_catalog", { name: null });
        mcpCatalog.value = entries;
        // 自动探测未缓存/新挂载的插件连接状态，无需用户手动逐个点击刷新
        for (const entry of entries) {
            if (!entry.cached || entry.tool_count === 0) {
                void refreshConnection(entry.name, false);
            } else {
                connectionState.value[entry.name] = "ready";
            }
        }
    } catch (error) {
        mcpCatalog.value = [];
        console.warn("无法读取 MCP 工具目录", error);
    } finally {
        catalogLoading.value = false;
    }
}
async function refreshConnection(name: string, reloadAfter = true) {
    connectionState.value = { ...connectionState.value, [name]: "testing" };
    connectionError.value = { ...connectionError.value, [name]: "" };
    try {
        const updated = await invoke<McpCatalogEntry>("refresh_mcp_plugin_connection", { name });
        connectionState.value = { ...connectionState.value, [name]: "ready" };
        const idx = mcpCatalog.value.findIndex(e => e.name === name);
        if (idx >= 0) {
            mcpCatalog.value[idx] = updated;
        } else if (reloadAfter) {
            const entries = await invoke<McpCatalogEntry[]>("get_mcp_tool_catalog", { name: null });
            mcpCatalog.value = entries;
        }
    } catch (error) {
        connectionState.value = { ...connectionState.value, [name]: "error" };
        connectionError.value = { ...connectionError.value, [name]: String(error) };
    }
}
interface RoleCard {
    id: string;
    name: string;
    label: string;
    model_id: string;
    persona_prompt: string;
    scope_hints: string;
    default_test_command: string;
    enabled: boolean;
}
interface RoleModelOption { id: string; name: string; display_name: string; platform_name: string; }
const roleCards = ref<RoleCard[]>([]);
const roleCardsLoading = ref(false);
const roleFormOpen = ref(false);
const roleSaving = ref(false);
const roleFormError = ref("");
const roleModelOptions = ref<RoleModelOption[]>([]);
const emptyRoleForm = { id: "", name: "", label: "", model_id: "", persona_prompt: "", scope_hints: "", default_test_command: "", editingId: "" };
const roleForm = ref({ ...emptyRoleForm });

function roleModelLabel(modelId: string): string {
    if (!modelId) return "跟随档位路由";
    const found = roleModelOptions.value.find((m) => m.id === modelId);
    return found ? `钉死: ${found.display_name || found.name}` : "钉死专属模型";
}

async function loadRoleCards() {
    roleCardsLoading.value = true;
    try {
        roleCards.value = await invoke<RoleCard[]>("list_role_cards");
    } catch (error) {
        console.warn("无法读取角色池", error);
        roleCards.value = [];
    } finally {
        roleCardsLoading.value = false;
    }
    try {
        roleModelOptions.value = await invoke<RoleModelOption[]>("get_all_models_with_platform");
    } catch {
        roleModelOptions.value = [];
    }
}

function openRoleFormDefaults() {
    roleForm.value = { ...emptyRoleForm };
    roleFormError.value = "";
}

function editRoleCard(card: RoleCard) {
    roleForm.value = { ...card, editingId: card.id };
    roleFormError.value = "";
    roleFormOpen.value = true;
}

async function saveRoleCard() {
    roleSaving.value = true;
    roleFormError.value = "";
    try {
        await invoke("upsert_role_card", { card: { ...roleForm.value, id: roleForm.value.editingId || undefined } });
        roleFormOpen.value = false;
        openRoleFormDefaults();
        await loadRoleCards();
    } catch (error) {
        roleFormError.value = String(error);
    } finally {
        roleSaving.value = false;
    }
}

async function toggleRoleCard(name: string) {
    try {
        await invoke("toggle_role_card", { name });
        await loadRoleCards();
    } catch (error) {
        console.warn("切换角色卡失败", error);
    }
}

async function deleteRoleCard(name: string) {
    try {
        await invoke("delete_role_card", { name });
        await loadRoleCards();
    } catch (error) {
        console.warn("删除角色卡失败", error);
    }
}

watch(() => props.open, (open) => {
    if (open) {
        void loadMcpCatalog();
        void loadRoleCards();
    }
}, { immediate: true });
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
.settings-scroll select {
    -webkit-appearance: none;
    -moz-appearance: none;
    appearance: none;
    width: 100%;
    padding: 9px 32px 9px 12px;
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 8px;
    outline: none;
    color: #f4f4f5;
    background-color: #0d1420;
    background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='12' height='12' viewBox='0 0 24 24' fill='none' stroke='%238f9bb3' stroke-width='2.2'%3E%3Cpath d='M6 9l6 6 6-6'/%3E%3C/svg%3E");
    background-repeat: no-repeat;
    background-position: right 12px center;
    font: 12px inherit;
    transition: all 0.15s ease;
    cursor: pointer;
}
.settings-scroll select:hover:not(:disabled) {
    border-color: rgba(255, 255, 255, 0.22);
    background-color: #111a28;
}
.settings-scroll select:focus {
    border-color: var(--chat-accent);
    box-shadow: 0 0 0 3px rgba(100, 165, 255, .15);
}
.settings-scroll select option, .settings-scroll select optgroup {
    background-color: #101722;
    color: #f4f4f5;
}
.routing-grid { display: grid; gap: 10px; margin-top: 12px; }
.routing-card { display: grid; gap: 8px; padding: 12px; border: 1px solid var(--chat-line); border-radius: 10px; background: linear-gradient(135deg, rgba(255,255,255,.025), rgba(255,255,255,.01)); }
.routing-card:hover { border-color: var(--chat-line-strong); }
.routing-card-heading { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; }
.routing-card-heading strong { color: var(--chat-text); font-size: 12px; }
.routing-card-heading small { color: var(--chat-text-faint); font-size: 10px; }
.routing-card label { margin: 0; }
.routing-card label > span { color: var(--chat-text-faint); font-size: 10px; font-weight: 700; letter-spacing: .04em; text-transform: uppercase; }
.routing-card select { min-height: 38px; padding: 8px 32px 8px 10px; background-color: rgba(7, 11, 18, .85); }
.routing-note { margin: 12px 0 0; color: var(--chat-text-faint); font-size: 10px; line-height: 1.55; }
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
.mcp-last-err { color:#df7b80; font-size:10px; }
.mcp-refresh,.manage-mcp { border: 1px solid var(--chat-line-strong); border-radius: 7px; color: var(--chat-text-soft); cursor: pointer; background: #0d1420; font: 600 10px inherit; }
.mcp-refresh { align-self: start; padding: 6px 7px; }
.mcp-refresh:hover:not(:disabled),.manage-mcp:hover { border-color: var(--chat-accent); color: var(--chat-text); background: var(--chat-hover); }
.mcp-refresh:disabled { cursor: wait; opacity: .65; }
.mcp-error { grid-column: 1 / -1; margin: 0; color: #df8b91; font-size: 10px; line-height: 1.45; word-break: break-word; }
.mcp-empty { padding: 9px; border: 1px dashed var(--chat-line-strong); border-radius: 8px; color: var(--chat-text-faint); font-size: 11px; }
.manage-mcp { width: 100%; padding: 8px; }
@keyframes mcp-pulse { 50% { opacity: .35; transform: scale(.82); } }

/* 重试配置样式 */
.retry-config-section { display: grid; gap: 10px; }
.retry-input-row { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-top: 4px; }
.retry-input-row label { margin: 0; color: var(--chat-text); font-size: 12px; }
.retry-control-box { display: flex; align-items: center; gap: 8px; }
.retry-number-input {
    width: 70px;
    padding: 7px 10px;
    border: 1px solid var(--chat-line-strong);
    border-radius: 7px;
    background: #0d1420;
    color: #f4f4f5;
    font: 600 13px inherit;
    text-align: center;
    outline: none;
    transition: all 0.15s ease;
}
.retry-number-input:focus {
    border-color: var(--chat-accent);
    box-shadow: 0 0 0 3px rgba(100, 165, 255, .15);
}
.unit-label { font-size: 11px; color: var(--chat-text-faint); }
.retry-presets { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 4px; }
.preset-chip {
    padding: 4px 9px;
    border: 1px solid var(--chat-line);
    border-radius: 6px;
    background: var(--chat-tool-bg);
    color: var(--chat-text-muted);
    font: 500 10.5px inherit;
    cursor: pointer;
    transition: all 0.15s ease;
}
.preset-chip:hover:not(:disabled) {
    border-color: var(--chat-line-strong);
    color: var(--chat-text);
    background: var(--chat-hover);
}
.preset-chip.active {
    border-color: var(--chat-accent);
    background: rgba(100, 165, 255, 0.15);
    color: var(--chat-accent);
    font-weight: 700;
}
.preset-chip:disabled { opacity: 0.5; cursor: not-allowed; }
.retry-policy-note {
    margin: 4px 0 0;
    padding: 8px 10px;
    border-radius: 7px;
    background: rgba(255, 255, 255, 0.02);
    border: 1px solid var(--chat-line);
    color: var(--chat-text-faint);
    font-size: 10.5px;
    line-height: 1.5;
}
.retry-policy-note strong { color: var(--chat-text-muted); }

/* 🎭 专精角色池 */
.role-card-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-bottom: 10px;
}

.role-card-item {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 12px;
    padding: 10px 12px;
    border-radius: 10px;
    background: #181822;
    border: 1px solid rgba(255, 255, 255, 0.08);
    transition: all 0.18s ease;
}

.role-card-item:hover {
    border-color: rgba(255, 255, 255, 0.18);
    background: #1d1d2b;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.25);
}

.role-card-item.disabled {
    opacity: 0.5;
    background: rgba(255, 255, 255, 0.02);
}

.role-card-main {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
}

.role-card-main strong {
    font-size: 12px;
    color: #f4f4f5;
    display: inline-flex;
    align-items: center;
    gap: 6px;
}

.role-card-main strong small {
    color: #38bdf8;
    background: rgba(56, 189, 248, 0.12);
    padding: 1px 5px;
    border-radius: 4px;
    font-size: 9.5px;
    font-family: ui-monospace, monospace;
}

.role-card-main small {
    color: #a1a1aa;
    font-size: 10.5px;
}

.role-card-main p {
    margin: 2px 0 0;
    font-size: 11px;
    color: #71717a;
    line-height: 1.45;
}

.role-card-actions {
    display: flex;
    flex-direction: column;
    gap: 4px;
    flex-shrink: 0;
}

.role-card-actions button {
    font-size: 10.5px;
    font-weight: 600;
    padding: 4px 10px;
    border-radius: 6px;
    border: 1px solid rgba(255, 255, 255, 0.1);
    background: #20202c;
    color: #e4e4e7;
    cursor: pointer;
    transition: all 0.15s ease;
}

.role-card-actions button:hover {
    border-color: rgba(255, 255, 255, 0.22);
    background: #282838;
    color: #f4f4f5;
}

.role-card-actions button.danger {
    color: #f87171;
    background: rgba(239, 68, 68, 0.1);
    border-color: rgba(239, 68, 68, 0.25);
}

.role-card-actions button.danger:hover {
    background: rgba(239, 68, 68, 0.2);
    border-color: rgba(239, 68, 68, 0.4);
}

.role-form {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 14px;
    border-radius: 10px;
    background: #14141d;
    border: 1px solid rgba(255, 255, 255, 0.1);
    margin-top: 8px;
}

.role-form label,
.role-form-field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 11px;
    font-weight: 600;
    color: #d4d4d8;
}

.role-field-label {
    font-size: 11px;
    font-weight: 600;
    color: #d4d4d8;
}

.role-form input,
.role-form textarea,
.role-form select {
    border-radius: 7px;
    border: 1px solid rgba(255, 255, 255, 0.12);
    background: #1c1c28;
    color: #f4f4f5;
    padding: 7px 10px;
    font-size: 11.5px;
    outline: none;
    transition: all 0.15s ease;
}

.role-form select {
    -webkit-appearance: none;
    -moz-appearance: none;
    appearance: none;
    padding-right: 28px;
    background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='12' height='12' viewBox='0 0 24 24' fill='none' stroke='%2338bdf8' stroke-width='2.2'%3E%3Cpath d='M6 9l6 6 6-6'/%3E%3C/svg%3E");
    background-repeat: no-repeat;
    background-position: right 10px center;
    cursor: pointer;
}

.role-form select option {
    background-color: #161622;
    color: #f4f4f5;
}

.role-form input:hover,
.role-form textarea:hover,
.role-form select:hover {
    border-color: rgba(255, 255, 255, 0.22);
    background: #20202e;
}

.role-form input:focus,
.role-form textarea:focus,
.role-form select:focus {
    border-color: #38bdf8;
    background: #222232;
    box-shadow: 0 0 0 3px rgba(56, 189, 248, 0.15);
}

.role-form-actions {
    display: flex;
    gap: 8px;
    padding-top: 4px;
}

.role-form-actions button {
    border-radius: 7px;
    padding: 6px 14px;
    font-size: 11.5px;
    font-weight: 600;
    cursor: pointer;
    transition: all 0.15s ease;
}
</style>
