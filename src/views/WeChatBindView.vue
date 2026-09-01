<template>
  <div class="wechat-view-container">
    <div class="wechat-view">
      <header class="view-header">
      <h2>📱 微信绑定与移动控制台</h2>
      <p class="header-sub">
        通过微信或手机浏览器随时向本机 Agent 派发任务、实时接收执行进度与回复交互。
      </p>
    </header>

    <!-- 1. 连接状态监控栏 -->
    <section class="card">
      <div class="card-title-row">
        <h3>连接状态</h3>
        <span class="state-pill" :class="statusClass">{{ statusText }}</span>
      </div>

      <div class="status-grid">
        <div class="status-item">
          <span class="status-label">桥进程</span>
          <span class="status-value">{{ bridgeRunning ? "运行中" : "未启动" }}</span>
        </div>
        <div class="status-item">
          <span class="status-label">微信账号</span>
          <span class="status-value">{{ loggedIn ? "已登录" : "未登录" }}</span>
        </div>
        <div class="status-item">
          <span class="status-label">执行中任务</span>
          <span class="status-value">{{ bridge?.active_runs ?? 0 }}</span>
        </div>
        <div class="status-item">
          <span class="status-label">待你回答</span>
          <span class="status-value">{{ bridge?.pending_questions ?? 0 }}</span>
        </div>
      </div>

      <div class="action-row">
        <button class="btn primary" :disabled="busy || bridgeRunning" @click="startBridge">
          {{ bridgeRunning ? "桥已运行" : "① 启动桥" }}
        </button>
        <button class="btn" :disabled="busy || !bridgeRunning || loggedIn" @click="startLogin">
          ② 扫码登录
        </button>
        <button
          class="btn warn"
          :class="{ armed: reconfirmArmed }"
          :disabled="busy || !bridgeRunning"
          @click="reloginQr"
        >
          {{ reconfirmArmed ? "确认清除当前微信并出新码？" : "重新扫码（切换账号）" }}
        </button>
        <button class="btn danger" :disabled="busy || !bridgeRunning" @click="stopBridge">
          停止桥
        </button>
        <button class="btn ghost" :disabled="busy" @click="refreshAll">刷新</button>
      </div>

      <div v-if="loginError" class="error-line">⚠️ {{ loginError }}</div>
    </section>

    <!-- 2. 双二维码工作区：左侧微信绑定，右侧网页控制台 -->
    <section class="card qr-hub-card">
      <div class="card-title-row">
        <div class="card-title-group">
          <h3>📲 双通道扫码连接中心</h3>
          <span class="hub-pill">两个二维码独立分栏 · 左右并排</span>
        </div>
      </div>

      <div class="dual-qr-grid">
        <!-- 左侧：微信扫码绑定 -->
        <div class="qr-col wechat-col">
          <div class="col-header">
            <div class="col-title-group">
              <span class="badge wechat-badge">🟢 微信通道</span>
              <h4 class="col-title">个人微信扫码绑定</h4>
            </div>
            <span class="col-desc">微信 App 扫码授权，直接在聊天窗口派发任务</span>
          </div>

          <div class="qr-box-wrapper">
            <!-- 已登录状态展示 -->
            <div v-if="loggedIn" class="logged-card">
              <div class="logged-badge-icon">✓</div>
              <div class="logged-info">
                <h5>微信账号已绑定</h5>
                <p>{{ bridge?.account_id ? `已连接: ${bridge.account_id}` : "个人微信账号在线连接中" }}</p>
                <span class="logged-sub">可直接在微信聊天框给此账号发指令</span>
              </div>
              <button class="btn warn sm" :disabled="busy" @click="reloginQr">
                {{ reconfirmArmed ? "确认切换？" : "🔄 重新扫码切换" }}
              </button>
            </div>

            <!-- 未登录：展示微信登录二维码 -->
            <div v-else class="qr-display-content">
              <div class="qr-white-card">
                <img v-if="qrDataUrl" :src="qrDataUrl" alt="微信登录二维码" />
                <div v-else class="qr-loading-inner">
                  <div class="pulse-spinner green"></div>
                  <span>{{ qrHint || (bridgeRunning ? "正在获取微信码…" : "请先启动桥") }}</span>
                </div>
              </div>
              <div class="qr-card-footer">
                <p class="qr-hint-text">打开手机微信 <strong>「扫一扫」</strong> 并在手机端确认</p>
                <div class="qr-btn-group">
                  <button v-if="!bridgeRunning" class="btn primary sm" :disabled="busy" @click="startBridgeAndLogin">
                    启动桥并获取二维码
                  </button>
                  <button v-else-if="!qrVisible" class="btn primary sm" :disabled="busy" @click="startLogin">
                    获取微信登录码
                  </button>
                  <button v-else class="btn ghost sm" :disabled="busy" @click="startLogin">
                    🔄 刷新二维码
                  </button>
                </div>
              </div>
            </div>
          </div>
        </div>

        <!-- 右侧：手机网页控制台二维码 -->
        <div class="qr-col console-col">
          <div class="col-header">
            <div class="col-title-group">
              <span class="badge console-badge">🌐 网页控制台</span>
              <span class="console-source-pill" :class="consoleQrSource">
                {{ consoleQrSource === "custom" ? "穿透域名" : "局域网 Wi-Fi" }}
              </span>
            </div>
            <span class="col-desc">手机扫码直接打开 Web 界面（已内嵌免密 Token）</span>
          </div>

          <div class="qr-box-wrapper">
            <div class="qr-display-content">
              <div class="qr-white-card">
                <div v-if="consoleQrSvg" class="console-svg-render" v-html="consoleQrSvg"></div>
                <div v-else class="qr-loading-inner">
                  <div class="pulse-spinner blue"></div>
                  <span>生成中…</span>
                </div>
              </div>
              <div class="qr-card-footer">
                <p class="qr-hint-text">
                  {{ consoleQrSource === "custom"
                    ? "🚀 穿透模式：手机任意 4G/5G/Wi-Fi 均可直接打开"
                    : "📶 局域网模式：手机需与电脑连接在同一 Wi-Fi" }}
                </p>
              </div>
            </div>

            <!-- 控制台链接与设置 -->
            <div class="console-controls">
              <div class="url-copy-row">
                <input class="url-input" type="text" readonly :value="consoleQrPrimary" :title="consoleQrPrimary" />
                <button class="btn copy-btn" :class="{ copied: copySuccess }" @click="copyConsoleUrl">
                  {{ copySuccess ? "已复制 ✓" : "复制链接" }}
                </button>
              </div>

              <div class="tunnel-row">
                <input
                  v-model="consoleUrlDraft"
                  class="tunnel-input"
                  placeholder="穿透域名（选填，如 https://my-agent.com）"
                  @keydown.enter="saveConsoleUrl"
                />
                <button class="btn primary sm" :disabled="busy || savingUrl" @click="saveConsoleUrl">
                  {{ savingUrl ? "保存中" : "保存域名" }}
                </button>
              </div>
            </div>
          </div>
        </div>
      </div>
    </section>

    <!-- 3. 通道配置 -->
    <section class="card config-card">
      <div class="card-title-row">
        <div class="card-title-group">
          <h3>⚙️ 通道与模型路由配置</h3>
          <span class="config-path" :title="configPath">{{ configPath }}</span>
        </div>
      </div>

      <!-- 🤖 Agent 模型角色档位分工 (Tab 切换卡片) -->
      <div class="role-tiers-card">
        <div class="role-tiers-header">
          <div class="header-left">
            <span class="role-section-badge">🤖 模型分工路由 (Role Tiers)</span>
            <span class="role-section-desc">为 Agent 的规划、执行与视觉分别指定最适配的模型</span>
          </div>
          <button
            type="button"
            class="btn-sync-all"
            title="将 Pro、Flash、Vision 全部一键设为跟随默认模型"
            @click="syncAllRoutesToDefault"
          >
            ⚡ 一键跟随默认
          </button>
        </div>

        <!-- 4 档位 Tab 切换栏 -->
        <div class="role-tab-nav">
          <button
            type="button"
            class="role-tab-btn"
            :class="{ active: activeRoleTab === 'default' }"
            @click="activeRoleTab = 'default'"
          >
            <span class="tab-icon">🌟</span>
            <div class="tab-meta">
              <span class="tab-label">默认基座</span>
              <span class="tab-val-preview">{{ getModelDisplayName(form.model_id) || "未选择" }}</span>
            </div>
          </button>

          <button
            type="button"
            class="role-tab-btn pro"
            :class="{ active: activeRoleTab === 'pro' }"
            @click="activeRoleTab = 'pro'"
          >
            <span class="tab-icon">🧠</span>
            <div class="tab-meta">
              <span class="tab-label">Pro 推理</span>
              <span class="tab-val-preview" :class="{ 'is-follow': !form.pro }">
                {{ form.pro ? getModelDisplayName(form.pro) : "跟随默认" }}
              </span>
            </div>
          </button>

          <button
            type="button"
            class="role-tab-btn flash"
            :class="{ active: activeRoleTab === 'flash' }"
            @click="activeRoleTab = 'flash'"
          >
            <span class="tab-icon">⚡</span>
            <div class="tab-meta">
              <span class="tab-label">Flash 执行</span>
              <span class="tab-val-preview" :class="{ 'is-follow': !form.flash }">
                {{ form.flash ? getModelDisplayName(form.flash) : "跟随默认" }}
              </span>
            </div>
          </button>

          <button
            type="button"
            class="role-tab-btn vision"
            :class="{ active: activeRoleTab === 'vision' }"
            @click="activeRoleTab = 'vision'"
          >
            <span class="tab-icon">👁️</span>
            <div class="tab-meta">
              <span class="tab-label">Vision 视觉</span>
              <span class="tab-val-preview" :class="{ 'is-follow': !form.vision }">
                {{ form.vision ? getModelDisplayName(form.vision) : "跟随默认" }}
              </span>
            </div>
          </button>
        </div>

        <!-- 对应 Tab 的配置面板 -->
        <div class="role-tab-content">
          <!-- 1. 默认创建 -->
          <div v-if="activeRoleTab === 'default'" class="tier-pane default-pane">
            <div class="tier-info-banner">
              <div class="tier-badge-row">
                <span class="tier-tag default">🌟 默认基座模型 (Default Base)</span>
                <span class="tier-hint">任务创建与初始对话的基础会话模型</span>
              </div>
              <p class="tier-desc">
                当微信收到新任务发起会话时使用的首选模型。若后续推理、执行或视觉未特别指定，将自动继承此配置。
              </p>
            </div>

            <div class="tier-select-box">
              <label class="tier-select-label">选择基座模型：</label>
              <ModelSelectDropdown
                :model-value="form.model_id"
                :models="allModels || []"
                placeholder="搜索并选择默认模型..."
                size="md"
                @update:model-value="form.model_id = $event"
              />
            </div>
          </div>

          <!-- 2. Pro 推理 -->
          <div v-else-if="activeRoleTab === 'pro'" class="tier-pane pro-pane">
            <div class="tier-info-banner">
              <div class="tier-badge-row">
                <span class="tier-tag pro">🧠 Pro 推理规划专家 (Reasoning & Planning)</span>
                <span class="tier-hint">负责长程规划、深层反思与工具决策</span>
              </div>
              <p class="tier-desc">
                负责复杂长流程拆解、反思纠错、自愈重试与架构设计。推荐搭配强推理模型（如 DeepSeek-R1、Claude 3.7 Sonnet、Gemini 2.5 Pro）。
              </p>
            </div>

            <div class="tier-select-box">
              <div class="tier-select-header">
                <label class="tier-select-label">选择 Pro 推理模型：</label>
                <button
                  v-if="form.pro"
                  type="button"
                  class="btn-reset-route"
                  @click="form.pro = ''"
                >
                  ↩️ 重置为跟随默认
                </button>
              </div>
              <ModelSelectDropdown
                :model-value="form.pro"
                :models="allModels || []"
                placeholder="跟随默认模型 (或选择特定 Pro 模型)..."
                size="md"
                @update:model-value="form.pro = $event"
              />
            </div>
          </div>

          <!-- 3. Flash 执行 -->
          <div v-else-if="activeRoleTab === 'flash'" class="tier-pane flash-pane">
            <div class="tier-info-banner">
              <div class="tier-badge-row">
                <span class="tier-tag flash">⚡ Flash 极速执行专家 (Fast Execution)</span>
                <span class="tier-hint">负责高频工具调用与快速代码执行</span>
              </div>
              <p class="tier-desc">
                负责常规命令调用、文件读写、快速代码润色等高频单步执行。推荐搭配低延迟、高吞吐的高性价比模型（如 Gemini 2.5 Flash、DeepSeek-V3、Claude 3.5 Haiku）。
              </p>
            </div>

            <div class="tier-select-box">
              <div class="tier-select-header">
                <label class="tier-select-label">选择 Flash 执行模型：</label>
                <button
                  v-if="form.flash"
                  type="button"
                  class="btn-reset-route"
                  @click="form.flash = ''"
                >
                  ↩️ 重置为跟随默认
                </button>
              </div>
              <ModelSelectDropdown
                :model-value="form.flash"
                :models="allModels || []"
                placeholder="跟随默认模型 (或选择特定 Flash 模型)..."
                size="md"
                @update:model-value="form.flash = $event"
              />
            </div>
          </div>

          <!-- 4. Vision 视觉 -->
          <div v-else-if="activeRoleTab === 'vision'" class="tier-pane vision-pane">
            <div class="tier-info-banner">
              <div class="tier-badge-row">
                <span class="tier-tag vision">👁️ Vision 多模态视觉专家 (Multimodal Vision)</span>
                <span class="tier-hint">负责屏幕截图解析、图表识别与视觉审查</span>
              </div>
              <p class="tier-desc">
                负责解析图片附件、网页 UI 截图、图表数据与图像理解。推荐选用具备强大多模态视觉理解能力的模型。
              </p>
            </div>

            <div class="tier-select-box">
              <div class="tier-select-header">
                <label class="tier-select-label">选择 Vision 视觉模型：</label>
                <button
                  v-if="form.vision"
                  type="button"
                  class="btn-reset-route"
                  @click="form.vision = ''"
                >
                  ↩️ 重置为跟随默认
                </button>
              </div>
              <ModelSelectDropdown
                :model-value="form.vision"
                :models="allModels || []"
                placeholder="跟随默认模型 (或选择特定 Vision 模型)..."
                size="md"
                @update:model-value="form.vision = $event"
              />
            </div>
          </div>
        </div>
      </div>

      <!-- 其他基础通道参数 -->
      <div class="form-grid other-params-grid">
        <label class="field">
          <span>临时工作区（未绑定任务）</span>
          <input v-model="form.scratch_dir" type="text" placeholder="留空使用默认目录" />
        </label>
        <label class="field">
          <span>进度心跳间隔（分钟）</span>
          <input v-model.number="form.heartbeat_minutes" type="number" min="1" max="1440" />
        </label>
        <label class="field">
          <span>LLM 请求重试次数（API 不稳定时调高，1–100，留空不改）</span>
          <input
            v-model="form.llm_retries"
            type="number"
            min="1"
            max="100"
            placeholder="系统默认 3"
          />
        </label>
        <div class="field check-field">
          <span>随应用自动启动桥</span>
          <label class="switch-row">
            <input v-model="form.auto_start_bridge" type="checkbox" />
            <em>{{ form.auto_start_bridge ? "应用启动时自动拉起" : "需要手动点「启动桥」" }}</em>
          </label>
        </div>
      </div>

      <label class="field wide">
        <span>首次联系帮助文案</span>
        <textarea v-model="form.help_text" rows="3"></textarea>
      </label>

      <div class="action-row">
        <button class="btn primary" :disabled="saving" @click="saveConfig">
          {{ saving ? "保存中…" : "保存通道配置" }}
        </button>
        <span v-if="saveHint" class="save-hint">{{ saveHint }}</span>
      </div>
    </section>

    <!-- 3. 最近任务 -->
    <section class="card">
      <div class="card-title-row">
        <h3>最近微信任务</h3>
        <span class="header-sub">应用重启后列表会清空；微信里的历史不受影响</span>
      </div>
      <div v-if="!runs.length" class="empty-line">暂无通过微信派发的任务。</div>
      <ul v-else class="run-list">
        <li v-for="run in runs" :key="run.run_id" class="run-item">
          <span class="run-status" :class="run.status">{{ statusLabel(run.status) }}</span>
          <div class="run-body">
            <div class="run-goal">{{ run.goal_preview }}</div>
            <div class="run-meta">
              <span v-if="run.last_progress">最新：{{ run.last_progress }}</span>
              <span v-if="run.error" class="run-error">错误：{{ run.error }}</span>
              <span>{{ run.updated_at.slice(11, 19) }}</span>
            </div>
          </div>
        </li>
      </ul>
    </section>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import ModelSelectDropdown from "../components/common/ModelSelectDropdown.vue";

interface ModelOption {
  id: string;
  name: string;
  display_name: string;
  platform_id: string;
  platform_name: string;
}

interface BridgeStatus {
  running: boolean;
  logged_in?: boolean;
  account_id?: string | null;
  active_runs?: number;
  pending_questions?: number;
  error?: string;
}

interface ChannelConfigView {
  port: number;
  bridge_port: number;
  scratch_dir: string;
  model_id: string;
  pro: string;
  flash: string;
  vision: string;
  heartbeat_minutes: number;
  help_text: string;
  auto_start_bridge: boolean;
  llm_retries: number | null;
  config_path: string;
}

interface RunRecord {
  run_id: string;
  session_id: string;
  goal_preview: string;
  status: string;
  paused_reason: string | null;
  last_progress: string | null;
  event_count: number;
  started_at: string;
  updated_at: string;
  result: string | null;
  error: string | null;
}

const bridge = ref<BridgeStatus | null>(null);
const allModels = ref<ModelOption[]>([]);
const runs = ref<RunRecord[]>([]);
const busy = ref(false);
const saving = ref(false);
const saveHint = ref("");
const loginError = ref("");
const qrDataUrl = ref<string | null>(null);
const qrHint = ref<string | null>(null);
const showQr = ref(false);
const configPath = ref("");
const reconfirmArmed = ref(false);

const form = reactive({
  model_id: "",
  pro: "",
  flash: "",
  vision: "",
  scratch_dir: "",
  heartbeat_minutes: 30,
  help_text: "",
  auto_start_bridge: true,
  llm_retries: "" as number | "",
});

const activeRoleTab = ref<"default" | "pro" | "flash" | "vision">("default");

function getModelDisplayName(modelId: string): string {
  if (!modelId) return "";
  const found = allModels.value.find((m) => m.id === modelId || m.name === modelId);
  if (found) {
    return found.display_name || found.name;
  }
  return modelId;
}

function syncAllRoutesToDefault() {
  form.pro = "";
  form.flash = "";
  form.vision = "";
  saveHint.value = "已将 Pro / Flash / Vision 路由全部重置为跟随默认模型（记得点击保存）";
}

let statusTimer: number | null = null;
let qrTimer: number | null = null;
let runsTimer: number | null = null;
let reconfirmTimer: number | null = null;
let unlistenRunStarted: UnlistenFn | null = null;
let unlistenRunDone: UnlistenFn | null = null;

const bridgeRunning = computed(() => Boolean(bridge.value?.running));
const loggedIn = computed(() => Boolean(bridge.value?.logged_in));

const statusText = computed(() => {
  if (!bridgeRunning.value) return "未连接";
  if (loggedIn.value) return "已连接微信";
  if (showQr.value) return "等待扫码";
  return "桥已就绪";
});

const statusClass = computed(() => {
  if (!bridgeRunning.value) return "off";
  if (loggedIn.value) return "on";
  return "wait";
});

const qrVisible = computed(() => showQr.value && !loggedIn.value);

function statusLabel(status: string): string {
  switch (status) {
    case "running":
      return "执行中";
    case "waiting_human":
      return "等你回答";
    case "done":
      return "完成";
    case "failed":
      return "失败";
    case "cancelled":
      return "已停止";
    default:
      return status;
  }
}

async function refreshStatus() {
  try {
    const value = await invoke<{ bridge: BridgeStatus; config: ChannelConfigView }>(
      "wechat_bridge_status"
    );
    bridge.value = value.bridge;
    loginError.value = value.bridge?.error || "";
    if (value.bridge?.logged_in) {
      showQr.value = false;
      stopQrPolling();
    }
  } catch {
    bridge.value = { running: false };
  }
}

async function refreshConfig() {
  try {
    const value = await invoke<ChannelConfigView>("wechat_channel_config_get");
    configPath.value = value.config_path;
    form.model_id = value.model_id;
    form.pro = value.pro;
    form.flash = value.flash;
    form.vision = value.vision;
    form.scratch_dir = value.scratch_dir;
    form.heartbeat_minutes = value.heartbeat_minutes;
    form.help_text = value.help_text;
    form.auto_start_bridge = value.auto_start_bridge !== false;
    form.llm_retries = value.llm_retries ?? "";
    consoleUrlDraft.value = ((value as unknown as Record<string, unknown>).console_base_url as string) || "";
    await refreshConsoleQr();
  } catch {
    /* 配置读取失败保持表单原样 */
  }
}

// ---- 网页控制台二维码 ----
const consoleQrSvg = ref("");
const consoleQrPrimary = ref("");
const consoleQrSource = ref<"lan" | "custom">("lan");
const consoleUrlDraft = ref("");
const copySuccess = ref(false);
const savingUrl = ref(false);

async function refreshConsoleQr() {
  try {
    const value = await invoke<{ svg: string; primary: string; source: "lan" | "custom" }>(
      "wechat_channel_console_qr"
    );
    consoleQrSvg.value = value.svg;
    consoleQrPrimary.value = value.primary;
    consoleQrSource.value = value.source;
  } catch {
    consoleQrSvg.value = "";
  }
}

async function copyConsoleUrl() {
  if (!consoleQrPrimary.value) return;
  try {
    await navigator.clipboard.writeText(consoleQrPrimary.value);
    copySuccess.value = true;
    setTimeout(() => {
      copySuccess.value = false;
    }, 2000);
  } catch (err) {
    console.error("复制控制台链接失败:", err);
  }
}

async function saveConsoleUrl() {
  savingUrl.value = true;
  try {
    await invoke("wechat_channel_config_set", {
      config: { console_base_url: consoleUrlDraft.value.trim() },
    });
    await refreshConfig();
  } catch {
    /* 保存失败静默，二维码保持原样 */
  } finally {
    savingUrl.value = false;
  }
}

async function refreshRuns() {
  try {
    runs.value = await invoke<RunRecord[]>("wechat_channel_runs");
  } catch {
    /* 列表失败忽略 */
  }
}

async function loadModels() {
  try {
    allModels.value = await invoke<ModelOption[]>("get_all_models_with_platform");
  } catch {
    allModels.value = [];
  }
}

function startQrPolling() {
  stopQrPolling();
  qrTimer = window.setInterval(async () => {
    try {
      const value = await invoke<{
        state: string;
        qr_data_url: string | null;
        error: string | null;
        hint: string | null;
      }>("wechat_bridge_qr");
      qrDataUrl.value = value.qr_data_url;
      qrHint.value = value.hint;
      loginError.value = value.error || "";
      if (value.state === "logged_in") {
        showQr.value = false;
        stopQrPolling();
        await refreshStatus();
      }
    } catch {
      /* 桥未就绪，继续等待 */
    }
  }, 2000);
}

function stopQrPolling() {
  if (qrTimer !== null) {
    clearInterval(qrTimer);
    qrTimer = null;
  }
}

async function startBridge() {
  busy.value = true;
  try {
    await invoke("wechat_bridge_start");
  } catch (error) {
    loginError.value = String(error);
  } finally {
    await refreshStatus();
    busy.value = false;
  }
}

async function startLogin() {
  busy.value = true;
  try {
    await invoke("wechat_bridge_login_start");
    showQr.value = true;
    qrDataUrl.value = null;
    qrHint.value = null;
    startQrPolling();
  } catch (error) {
    loginError.value = String(error);
  } finally {
    await refreshStatus();
    busy.value = false;
  }
}

async function startBridgeAndLogin() {
  await startBridge();
  if (bridgeRunning.value && !loggedIn.value) {
    await startLogin();
  }
}

async function stopBridge() {
  busy.value = true;
  try {
    await invoke("wechat_bridge_stop");
    showQr.value = false;
    stopQrPolling();
    qrDataUrl.value = null;
    qrHint.value = null;
  } catch (error) {
    loginError.value = String(error);
  } finally {
    await refreshStatus();
    busy.value = false;
  }
}

async function reloginQr() {
  // 两步确认：第一次点击进入待确认态（4 秒后自动复位），第二次才真正执行
  if (!reconfirmArmed.value) {
    reconfirmArmed.value = true;
    if (reconfirmTimer !== null) clearTimeout(reconfirmTimer);
    reconfirmTimer = window.setTimeout(() => {
      reconfirmArmed.value = false;
      reconfirmTimer = null;
    }, 4000);
    return;
  }
  reconfirmArmed.value = false;
  if (reconfirmTimer !== null) {
    clearTimeout(reconfirmTimer);
    reconfirmTimer = null;
  }
  busy.value = true;
  try {
    await invoke("wechat_bridge_relogin");
    showQr.value = true;
    qrDataUrl.value = null;
    qrHint.value = null;
    startQrPolling();
  } catch (error) {
    loginError.value = String(error);
  } finally {
    await refreshStatus();
    busy.value = false;
  }
}

async function saveConfig() {
  saving.value = true;
  saveHint.value = "";
  try {
    await invoke("wechat_channel_config_set", {
      config: {
        port: undefined,
        bridge_port: undefined,
        scratch_dir: form.scratch_dir,
        model_id: form.model_id,
        pro: form.pro,
        flash: form.flash,
        vision: form.vision,
        heartbeat_minutes: form.heartbeat_minutes,
        help_text: form.help_text,
        auto_start_bridge: form.auto_start_bridge,
        llm_retries: form.llm_retries === "" ? undefined : Number(form.llm_retries),
      },
    });
    saveHint.value = "已保存 ✓（端口变更需重启应用生效）";
  } catch (error) {
    saveHint.value = `保存失败：${error}`;
  }
  saving.value = false;
}

async function refreshAll() {
  await Promise.all([refreshStatus(), refreshConfig(), refreshRuns(), loadModels()]);
}

onMounted(async () => {
  await refreshAll();
  if (bridgeRunning.value && !loggedIn.value && !showQr.value) {
    await startLogin();
  }
  statusTimer = window.setInterval(refreshStatus, 5000);
  runsTimer = window.setInterval(refreshRuns, 10000);

  // 监听来自 Rust 的 任务启动 / 完成 事件，手机和微信发起任务后桌面列表秒刷新无需等 10s
  unlistenRunStarted = await listen("channel-run-started", () => {
    refreshRuns();
    refreshStatus();
  });
  unlistenRunDone = await listen("channel-run-done", () => {
    refreshRuns();
    refreshStatus();
  });
});

onUnmounted(() => {
  if (statusTimer !== null) clearInterval(statusTimer);
  if (qrTimer !== null) clearInterval(qrTimer);
  if (runsTimer !== null) clearInterval(runsTimer);
  if (reconfirmTimer !== null) clearTimeout(reconfirmTimer);
  unlistenRunStarted?.();
  unlistenRunDone?.();
});
</script>

<style scoped>
.wechat-view-container {
  width: 100%;
  height: 100%;
  overflow-y: auto;
  overflow-x: hidden;
  box-sizing: border-box;
}

.wechat-view-container::-webkit-scrollbar {
  width: 6px;
}

.wechat-view-container::-webkit-scrollbar-track {
  background: transparent;
}

.wechat-view-container::-webkit-scrollbar-thumb {
  background: rgba(255, 255, 255, 0.16);
  border-radius: 4px;
}

.wechat-view-container::-webkit-scrollbar-thumb:hover {
  background: rgba(255, 255, 255, 0.3);
}

.wechat-view {
  display: flex;
  flex-direction: column;
  gap: 16px;
  max-width: 960px;
  margin: 0 auto;
  padding: 18px 24px 60px;
  color: #f4f4f5;
  user-select: none;
  box-sizing: border-box;
}

.view-header {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.view-header h2 {
  margin: 0;
  font-size: 20px;
  font-weight: 700;
  color: #f4f4f5;
  letter-spacing: -0.02em;
}

.header-sub {
  margin: 0;
  font-size: 12px;
  color: #a1a1aa;
  line-height: 1.5;
}

.card {
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 12px;
  padding: 16px 18px;
  background: #141419;
  box-shadow: 0 4px 20px rgba(0, 0, 0, 0.35);
  display: flex;
  flex-direction: column;
  gap: 14px;
  transition: border-color 0.2s ease;
}

.card:hover {
  border-color: rgba(255, 255, 255, 0.12);
}

.card-title-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding-bottom: 8px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.06);
}

.card-title-row h3 {
  margin: 0;
  font-size: 13.5px;
  font-weight: 700;
  color: #f4f4f5;
}

.config-path {
  margin-left: auto;
  font-size: 11px;
  font-family: ui-monospace, monospace;
  color: #71717a;
  background: rgba(255, 255, 255, 0.04);
  padding: 2px 8px;
  border-radius: 4px;
  max-width: 50%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.state-pill {
  font-size: 11px;
  font-weight: 600;
  padding: 2px 10px;
  border-radius: 999px;
  display: inline-flex;
  align-items: center;
  gap: 4px;
}

.state-pill.on {
  background: rgba(52, 211, 153, 0.15);
  border: 1px solid rgba(52, 211, 153, 0.35);
  color: #34d399;
}

.state-pill.wait {
  background: rgba(251, 191, 36, 0.15);
  border: 1px solid rgba(251, 191, 36, 0.35);
  color: #fbbf24;
}

.state-pill.off {
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.1);
  color: #71717a;
}

.status-grid {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 10px;
}

.status-item {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 10px 12px;
  border-radius: 10px;
  background: #1a1a24;
  border: 1px solid rgba(255, 255, 255, 0.06);
  transition: all 0.18s ease;
}

.status-item:hover {
  background: #1e1e2b;
  border-color: rgba(255, 255, 255, 0.12);
}

.status-label {
  font-size: 11px;
  color: #71717a;
  font-weight: 500;
}

.status-value {
  font-size: 14px;
  font-weight: 700;
  color: #f4f4f5;
}

.action-row {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
  padding-top: 2px;
}

.btn {
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 8px;
  padding: 6px 14px;
  font-size: 12px;
  font-weight: 600;
  background: #1f1f2a;
  color: #e4e4e7;
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  transition: all 0.15s ease;
}

.btn:hover:not(:disabled) {
  border-color: rgba(255, 255, 255, 0.22);
  background: #272736;
  color: #f4f4f5;
  transform: translateY(-1px);
}

.btn:active:not(:disabled) {
  transform: translateY(0);
}

.btn:disabled {
  opacity: 0.45;
  cursor: not-allowed;
  transform: none;
}

.btn.primary {
  background: linear-gradient(135deg, #2563eb, #38bdf8);
  border-color: rgba(56, 189, 248, 0.4);
  color: #fff;
  box-shadow: 0 2px 10px rgba(37, 99, 235, 0.3);
}

.btn.primary:hover:not(:disabled) {
  background: linear-gradient(135deg, #1d4ed8, #0ea5e9);
  box-shadow: 0 4px 14px rgba(37, 99, 235, 0.45);
}

.btn.danger {
  color: #f87171;
  background: rgba(239, 68, 68, 0.1);
  border-color: rgba(239, 68, 68, 0.25);
}

.btn.danger:hover:not(:disabled) {
  background: rgba(239, 68, 68, 0.2);
  border-color: rgba(239, 68, 68, 0.4);
  color: #fca5a5;
}

.btn.warn {
  color: #fbbf24;
  background: rgba(251, 191, 36, 0.1);
  border-color: rgba(251, 191, 36, 0.25);
}

.btn.warn:hover:not(:disabled) {
  background: rgba(251, 191, 36, 0.2);
  border-color: rgba(251, 191, 36, 0.4);
}

.btn.warn.armed {
  background: linear-gradient(135deg, #d97706, #f59e0b);
  border-color: rgba(245, 158, 11, 0.5);
  color: #0f172a;
  font-weight: 700;
  box-shadow: 0 0 12px rgba(245, 158, 11, 0.35);
}

.btn.ghost {
  background: transparent;
  border-color: rgba(255, 255, 255, 0.08);
  color: #a1a1aa;
}

.btn.ghost:hover:not(:disabled) {
  background: rgba(255, 255, 255, 0.05);
  color: #f4f4f5;
}

.error-line {
  color: #f87171;
  font-size: 11.5px;
  background: rgba(239, 68, 68, 0.1);
  border: 1px solid rgba(239, 68, 68, 0.2);
  border-radius: 6px;
  padding: 6px 10px;
}



.form-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px 16px;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 6px;
  font-size: 11.5px;
  font-weight: 600;
  color: #d4d4d8;
}

.field.wide {
  grid-column: 1 / -1;
}

.field.check-field .switch-row {
  display: flex;
  align-items: center;
  gap: 8px;
  cursor: pointer;
  padding: 6px 0;
}

.field.check-field .switch-row em {
  font-style: normal;
  font-size: 12px;
  color: #e4e4e7;
}

.field.check-field input[type="checkbox"] {
  width: 16px;
  height: 16px;
  accent-color: #38bdf8;
  cursor: pointer;
}

.field input,
.field select,
.field textarea {
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 8px;
  background: #1a1a24;
  color: #f4f4f5;
  padding: 8px 12px;
  font-size: 12px;
  font-family: inherit;
  outline: none;
  transition: all 0.18s ease;
  box-shadow: inset 0 1px 2px rgba(0, 0, 0, 0.2);
}

.field select {
  -webkit-appearance: none;
  -moz-appearance: none;
  appearance: none;
  padding-right: 32px;
  background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='12' height='12' viewBox='0 0 24 24' fill='none' stroke='%2338bdf8' stroke-width='2.2'%3E%3Cpath d='M6 9l6 6 6-6'/%3E%3C/svg%3E");
  background-repeat: no-repeat;
  background-position: right 12px center;
  cursor: pointer;
}

.field select option {
  background-color: #16161f;
  color: #f4f4f5;
}

.field input:hover,
.field select:hover,
.field textarea:hover {
  border-color: rgba(255, 255, 255, 0.2);
  background: #1e1e2b;
}

.field input:focus,
.field select:focus,
.field textarea:focus {
  border-color: #38bdf8;
  background: #1f1f2e;
  box-shadow: 0 0 0 3px rgba(56, 189, 248, 0.15);
}

.save-hint {
  font-size: 11.5px;
  color: #38bdf8;
  font-weight: 500;
}

.empty-line {
  color: #71717a;
  font-size: 12px;
  text-align: center;
  padding: 16px 0;
  border: 1px dashed rgba(255, 255, 255, 0.08);
  border-radius: 8px;
}

.run-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.run-item {
  display: flex;
  gap: 10px;
  align-items: flex-start;
  padding: 10px 12px;
  border-radius: 10px;
  background: #191924;
  border: 1px solid rgba(255, 255, 255, 0.06);
  transition: all 0.18s ease;
}

.run-item:hover {
  border-color: rgba(255, 255, 255, 0.15);
  background: #1e1e2b;
}

.run-status {
  flex-shrink: 0;
  font-size: 10.5px;
  font-weight: 600;
  padding: 2px 8px;
  border-radius: 999px;
  margin-top: 1px;
}

.run-status.running {
  background: rgba(56, 189, 248, 0.15);
  border: 1px solid rgba(56, 189, 248, 0.35);
  color: #38bdf8;
}

.run-status.waiting_human {
  background: rgba(251, 191, 36, 0.15);
  border: 1px solid rgba(251, 191, 36, 0.35);
  color: #fbbf24;
}

.run-status.done {
  background: rgba(52, 211, 153, 0.15);
  border: 1px solid rgba(52, 211, 153, 0.35);
  color: #34d399;
}

.run-status.failed {
  background: rgba(239, 68, 68, 0.15);
  border: 1px solid rgba(239, 68, 68, 0.35);
  color: #f87171;
}

.run-status.cancelled {
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.1);
  color: #71717a;
}

.run-body {
  display: flex;
  flex-direction: column;
  gap: 3px;
  min-width: 0;
}

.run-goal {
  font-size: 12px;
  font-weight: 500;
  color: #f4f4f5;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.run-meta {
  display: flex;
  gap: 12px;
  font-size: 11px;
  color: #71717a;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.run-error {
  color: #f87171;
  font-size: 11px;
}

/* 双二维码工作区与分栏卡片 */
.qr-hub-card {
  border: 1px solid rgba(255, 255, 255, 0.1);
  background: #14141a;
}

.card-title-group {
  display: flex;
  align-items: center;
  gap: 10px;
}

.hub-pill {
  font-size: 11px;
  font-weight: 600;
  padding: 2px 10px;
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.06);
  border: 1px solid rgba(255, 255, 255, 0.12);
  color: #a1a1aa;
}

.dual-qr-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;
  align-items: stretch;
}

.qr-col {
  background: #181824;
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 12px;
  padding: 16px;
  display: flex;
  flex-direction: column;
  gap: 14px;
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.25);
  transition: all 0.2s ease;
}

.qr-col.wechat-col {
  border-color: rgba(16, 185, 129, 0.25);
  background: linear-gradient(180deg, rgba(16, 185, 129, 0.04) 0%, #161622 100%);
}

.qr-col.console-col {
  border-color: rgba(56, 189, 248, 0.25);
  background: linear-gradient(180deg, rgba(56, 189, 248, 0.04) 0%, #161622 100%);
}

.col-header {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding-bottom: 10px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.06);
}

.col-title-group {
  display: flex;
  align-items: center;
  gap: 8px;
}

.col-title {
  margin: 0;
  font-size: 13.5px;
  font-weight: 700;
  color: #f4f4f5;
}

.col-desc {
  font-size: 11px;
  color: #a1a1aa;
}

.badge {
  font-size: 10.5px;
  font-weight: 700;
  padding: 2px 8px;
  border-radius: 999px;
}

.badge.wechat-badge {
  background: rgba(16, 185, 129, 0.15);
  border: 1px solid rgba(16, 185, 129, 0.35);
  color: #34d399;
}

.badge.console-badge {
  background: rgba(56, 189, 248, 0.15);
  border: 1px solid rgba(56, 189, 248, 0.35);
  color: #38bdf8;
}

.console-source-pill {
  font-size: 10.5px;
  font-weight: 600;
  padding: 2px 8px;
  border-radius: 999px;
  background: rgba(56, 189, 248, 0.12);
  border: 1px solid rgba(56, 189, 248, 0.3);
  color: #38bdf8;
  margin-left: auto;
}

.console-source-pill.custom {
  background: rgba(168, 85, 247, 0.15);
  border-color: rgba(168, 85, 247, 0.35);
  color: #c084fc;
}

.qr-box-wrapper {
  display: flex;
  flex-direction: column;
  flex: 1;
  justify-content: space-between;
  gap: 14px;
}

.qr-display-content {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 10px;
}

.qr-white-card {
  width: 180px;
  height: 180px;
  background: #ffffff;
  border-radius: 12px;
  padding: 8px;
  box-shadow: 0 4px 20px rgba(0, 0, 0, 0.45);
  display: flex;
  align-items: center;
  justify-content: center;
  box-sizing: border-box;
  overflow: hidden;
  margin: 0 auto;
}

.qr-white-card img {
  width: 100%;
  height: 100%;
  object-fit: contain;
  display: block;
}

.console-svg-render {
  width: 100%;
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  overflow: hidden;
}

.console-svg-render :deep(svg) {
  width: 100% !important;
  height: 100% !important;
  max-width: 100% !important;
  max-height: 100% !important;
  display: block !important;
}

.qr-loading-inner {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 8px;
  color: #71717a;
  font-size: 11px;
  text-align: center;
  padding: 8px;
}

.pulse-spinner {
  width: 24px;
  height: 24px;
  border: 2.5px solid rgba(0, 0, 0, 0.1);
  border-top-color: #07c160;
  border-radius: 50%;
  animation: spin 0.8s linear infinite;
}

.pulse-spinner.green {
  border-top-color: #10b981;
}

.pulse-spinner.blue {
  border-top-color: #0284c7;
}

@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}

.qr-card-footer {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  text-align: center;
}

.qr-hint-text {
  margin: 0;
  font-size: 11.5px;
  color: #a1a1aa;
  line-height: 1.4;
}

.qr-hint-text strong {
  color: #f4f4f5;
}

.qr-btn-group {
  display: flex;
  gap: 8px;
}

.btn.sm {
  padding: 4px 10px;
  font-size: 11px;
  border-radius: 6px;
}

.logged-card {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  padding: 24px 16px;
  background: rgba(16, 185, 129, 0.08);
  border: 1px dashed rgba(16, 185, 129, 0.3);
  border-radius: 10px;
  text-align: center;
  flex: 1;
}

.logged-badge-icon {
  width: 42px;
  height: 42px;
  border-radius: 50%;
  background: rgba(16, 185, 129, 0.2);
  border: 2px solid #10b981;
  color: #10b981;
  font-size: 20px;
  font-weight: 700;
  display: flex;
  align-items: center;
  justify-content: center;
}

.logged-info h5 {
  margin: 0;
  font-size: 14px;
  color: #f4f4f5;
  font-weight: 700;
}

.logged-info p {
  margin: 4px 0 2px;
  font-size: 12px;
  color: #34d399;
  font-weight: 600;
}

.logged-sub {
  font-size: 11px;
  color: #a1a1aa;
}

.console-controls {
  display: flex;
  flex-direction: column;
  gap: 8px;
  width: 100%;
}

.url-copy-row,
.tunnel-row {
  display: flex;
  gap: 6px;
}

.url-input {
  flex: 1;
  min-width: 0;
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 6px;
  background: #12121a;
  color: #38bdf8;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 11px;
  padding: 5px 8px;
  outline: none;
  text-overflow: ellipsis;
  box-shadow: inset 0 1px 2px rgba(0, 0, 0, 0.2);
}

.tunnel-input {
  flex: 1;
  min-width: 0;
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 6px;
  background: #12121a;
  color: #f4f4f5;
  font-size: 11px;
  padding: 5px 8px;
  outline: none;
}

.tunnel-input:focus {
  border-color: #38bdf8;
}

.copy-btn {
  flex-shrink: 0;
  padding: 4px 10px;
  font-size: 11px;
}

.copy-btn.copied {
  background: rgba(52, 211, 153, 0.18);
  border-color: rgba(52, 211, 153, 0.4);
  color: #34d399;
}

/* ---- 🤖 角色档位卡片与 Tab 切换器 ---- */
.role-tiers-card {
  display: flex;
  flex-direction: column;
  gap: 12px;
  background: rgba(255, 255, 255, 0.025);
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 12px;
  padding: 14px 16px 16px;
  margin-bottom: 6px;
}

.role-tiers-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.role-section-badge {
  font-size: 13px;
  font-weight: 700;
  color: #f4f4f5;
  margin-right: 8px;
}

.role-section-desc {
  font-size: 12px;
  color: #71717a;
}

.btn-sync-all {
  padding: 3px 10px;
  font-size: 11px;
  font-weight: 600;
  border-radius: 6px;
  border: 1px solid rgba(255, 255, 255, 0.1);
  background: rgba(255, 255, 255, 0.04);
  color: #a1a1aa;
  cursor: pointer;
  transition: all 0.15s ease;
}

.btn-sync-all:hover {
  background: rgba(99, 102, 241, 0.15);
  border-color: rgba(99, 102, 241, 0.35);
  color: #c7d2fe;
}

/* 4 档位 Tab 切换器 */
.role-tab-nav {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 8px;
}

.role-tab-btn {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
  background: #181820;
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 8px;
  cursor: pointer;
  text-align: left;
  transition: all 0.18s cubic-bezier(0.16, 1, 0.3, 1);
  user-select: none;
}

.role-tab-btn:hover {
  background: #20202c;
  border-color: rgba(255, 255, 255, 0.16);
  transform: translateY(-1px);
}

.role-tab-btn .tab-icon {
  font-size: 18px;
  flex-shrink: 0;
}

.role-tab-btn .tab-meta {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
  overflow: hidden;
}

.role-tab-btn .tab-label {
  font-size: 12px;
  font-weight: 600;
  color: #d4d4d8;
}

.role-tab-btn .tab-val-preview {
  font-size: 11px;
  color: #a1a1aa;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.role-tab-btn .tab-val-preview.is-follow {
  color: #71717a;
  font-style: italic;
}

/* Tab 激活状态与专属高亮色 */
.role-tab-btn.active {
  background: #222233;
  border-color: rgba(99, 102, 241, 0.5);
  box-shadow: 0 0 0 1px rgba(99, 102, 241, 0.3), 0 4px 12px rgba(0, 0, 0, 0.25);
}

.role-tab-btn.active .tab-label {
  color: #ffffff;
}

.role-tab-btn.pro.active {
  border-color: rgba(129, 140, 248, 0.6);
  box-shadow: 0 0 0 1px rgba(129, 140, 248, 0.4), 0 4px 12px rgba(99, 102, 241, 0.15);
}

.role-tab-btn.flash.active {
  border-color: rgba(245, 158, 11, 0.6);
  box-shadow: 0 0 0 1px rgba(245, 158, 11, 0.4), 0 4px 12px rgba(245, 158, 11, 0.12);
}

.role-tab-btn.vision.active {
  border-color: rgba(16, 185, 129, 0.6);
  box-shadow: 0 0 0 1px rgba(16, 185, 129, 0.4), 0 4px 12px rgba(16, 185, 129, 0.12);
}

/* Tab 面板内容容器 */
.role-tab-content {
  background: #16161e;
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 10px;
  padding: 14px 16px;
}

.tier-pane {
  display: flex;
  flex-direction: column;
  gap: 12px;
  animation: tab-fade-in 0.2s cubic-bezier(0.16, 1, 0.3, 1);
}

@keyframes tab-fade-in {
  from {
    opacity: 0;
    transform: translateY(4px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

.tier-info-banner {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.tier-badge-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.tier-tag {
  font-size: 11px;
  font-weight: 700;
  padding: 2px 8px;
  border-radius: 4px;
  letter-spacing: 0.02em;
}

.tier-tag.default {
  background: rgba(255, 255, 255, 0.1);
  color: #f4f4f5;
  border: 1px solid rgba(255, 255, 255, 0.15);
}

.tier-tag.pro {
  background: rgba(129, 140, 248, 0.15);
  color: #a5b4fc;
  border: 1px solid rgba(129, 140, 248, 0.35);
}

.tier-tag.flash {
  background: rgba(245, 158, 11, 0.15);
  color: #fcd34d;
  border: 1px solid rgba(245, 158, 11, 0.35);
}

.tier-tag.vision {
  background: rgba(16, 185, 129, 0.15);
  color: #6ee7b7;
  border: 1px solid rgba(16, 185, 129, 0.35);
}

.tier-hint {
  font-size: 11px;
  color: #71717a;
}

.tier-desc {
  margin: 0;
  font-size: 12px;
  line-height: 1.5;
  color: #a1a1aa;
}

.tier-select-box {
  display: flex;
  flex-direction: column;
  gap: 6px;
  background: rgba(0, 0, 0, 0.2);
  border: 1px solid rgba(255, 255, 255, 0.06);
  border-radius: 8px;
  padding: 10px 12px;
}

.tier-select-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.tier-select-label {
  font-size: 12px;
  font-weight: 600;
  color: #e4e4e7;
}

.btn-reset-route {
  background: none;
  border: none;
  color: #38bdf8;
  font-size: 11px;
  cursor: pointer;
  padding: 0;
  transition: opacity 0.15s;
}

.btn-reset-route:hover {
  text-decoration: underline;
  opacity: 0.85;
}

.other-params-grid {
  margin-top: 4px;
}

@media (max-width: 768px) {
  .dual-qr-grid {
    grid-template-columns: 1fr;
  }
  .role-tab-nav {
    grid-template-columns: repeat(2, 1fr);
  }
}
</style>
