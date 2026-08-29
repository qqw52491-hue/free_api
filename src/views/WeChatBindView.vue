<template>
  <div class="wechat-view">
    <header class="view-header">
      <h2>📱 微信绑定</h2>
      <p class="header-sub">
        个人微信扫码连接 ClawBot 通道，之后在微信里即可给本机 Agent 派任务、收进度。
      </p>
    </header>

    <!-- 1. 连接状态 -->
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

      <!-- 二维码区域 -->
      <div v-if="qrVisible" class="qr-area">
        <img v-if="qrDataUrl" :src="qrDataUrl" alt="微信扫码二维码" class="qr-img" />
        <div v-else class="qr-waiting">
          <span v-if="qrHint">{{ qrHint }}</span>
          <span v-else>正在获取二维码…</span>
        </div>
        <p class="qr-tip">请使用微信扫描（二维码过期会自动刷新，扫码后在微信中确认连接）</p>
      </div>
    </section>

    <!-- 2. 通道配置 -->
    <section class="card">
      <div class="card-title-row">
        <h3>通道配置</h3>
        <span class="config-path" :title="configPath">{{ configPath }}</span>
      </div>

      <div class="form-grid">
        <label class="field">
          <span>默认模型（会话创建）</span>
          <select v-model="form.model_id">
            <option value="">请选择</option>
            <option v-for="m in allModels" :key="m.id" :value="m.id">
              {{ m.display_name || m.name }}（{{ m.platform_name }}）
            </option>
          </select>
        </label>
        <label class="field">
          <span>Pro 路由（推理）</span>
          <select v-model="form.pro">
            <option value="">跟随默认模型</option>
            <option v-for="m in allModels" :key="m.id" :value="m.id">
              {{ m.display_name || m.name }}
            </option>
          </select>
        </label>
        <label class="field">
          <span>Flash 路由（执行）</span>
          <select v-model="form.flash">
            <option value="">跟随默认模型</option>
            <option v-for="m in allModels" :key="m.id" :value="m.id">
              {{ m.display_name || m.name }}
            </option>
          </select>
        </label>
        <label class="field">
          <span>Vision 路由（视觉）</span>
          <select v-model="form.vision">
            <option value="">跟随默认模型</option>
            <option v-for="m in allModels" :key="m.id" :value="m.id">
              {{ m.display_name || m.name }}
            </option>
          </select>
        </label>
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
        <textarea v-model="form.help_text" rows="4"></textarea>
      </label>

      <div class="action-row">
        <button class="btn primary" :disabled="saving" @click="saveConfig">
          {{ saving ? "保存中…" : "保存配置" }}
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
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";

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

let statusTimer: number | null = null;
let qrTimer: number | null = null;
let runsTimer: number | null = null;
let reconfirmTimer: number | null = null;

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
  } catch {
    /* 配置读取失败保持表单原样 */
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
  }
  await refreshStatus();
  busy.value = false;
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
  }
  await refreshStatus();
  busy.value = false;
}

async function stopBridge() {
  busy.value = true;
  try {
    await invoke("wechat_bridge_stop");
    showQr.value = false;
    stopQrPolling();
  } catch (error) {
    loginError.value = String(error);
  }
  await refreshStatus();
  busy.value = false;
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
  }
  await refreshStatus();
  busy.value = false;
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
  statusTimer = window.setInterval(refreshStatus, 5000);
  runsTimer = window.setInterval(refreshRuns, 10000);
});

onUnmounted(() => {
  if (statusTimer !== null) clearInterval(statusTimer);
  if (qrTimer !== null) clearInterval(qrTimer);
  if (runsTimer !== null) clearInterval(runsTimer);
  if (reconfirmTimer !== null) clearTimeout(reconfirmTimer);
});
</script>

<style scoped>
.wechat-view {
  height: 100%;
  overflow-y: auto;
  padding: 20px 24px;
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.view-header h2 {
  margin: 0 0 4px;
  font-size: 18px;
}

.header-sub {
  margin: 0;
  color: var(--text-secondary, #9aa0a6);
  font-size: 12px;
}

.card {
  border: 1px solid var(--border-color, #2c2f36);
  border-radius: 10px;
  padding: 14px 16px;
  background: var(--panel-bg, #1a1c21);
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.card-title-row {
  display: flex;
  align-items: center;
  gap: 10px;
}

.card-title-row h3 {
  margin: 0;
  font-size: 14px;
}

.config-path {
  margin-left: auto;
  font-size: 11px;
  color: var(--text-secondary, #9aa0a6);
  max-width: 50%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.state-pill {
  font-size: 11px;
  padding: 2px 10px;
  border-radius: 999px;
  border: 1px solid currentColor;
}

.state-pill.on {
  color: #4ade80;
}

.state-pill.wait {
  color: #fbbf24;
}

.state-pill.off {
  color: #9aa0a6;
}

.status-grid {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 8px;
}

.status-item {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 8px 10px;
  border-radius: 8px;
  background: var(--bg-secondary, #22252b);
}

.status-label {
  font-size: 11px;
  color: var(--text-secondary, #9aa0a6);
}

.status-value {
  font-size: 13px;
  font-weight: 600;
}

.action-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.btn {
  border: 1px solid var(--border-color, #2c2f36);
  border-radius: 8px;
  padding: 6px 14px;
  font-size: 12px;
  background: var(--bg-secondary, #22252b);
  color: inherit;
  cursor: pointer;
}

.btn:disabled {
  opacity: 0.45;
  cursor: not-allowed;
}

.btn.primary {
  background: var(--primary-color, #3b82f6);
  border-color: transparent;
  color: #fff;
}

.btn.danger {
  color: #f87171;
}

.btn.warn {
  color: #fbbf24;
}

.btn.warn.armed {
  background: #fbbf24;
  border-color: transparent;
  color: #1a1c21;
}

.btn.ghost {
  background: transparent;
}

.error-line {
  color: #f87171;
  font-size: 12px;
}

.qr-area {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  padding: 12px;
  border: 1px dashed var(--border-color, #2c2f36);
  border-radius: 10px;
}

.qr-img {
  width: 220px;
  height: 220px;
  border-radius: 8px;
  background: #fff;
  padding: 8px;
}

.qr-waiting {
  width: 220px;
  height: 180px;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--text-secondary, #9aa0a6);
  font-size: 12px;
  text-align: center;
}

.qr-tip {
  margin: 0;
  font-size: 11px;
  color: var(--text-secondary, #9aa0a6);
}

.form-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px 14px;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 12px;
  color: var(--text-secondary, #9aa0a6);
}

.field.wide {
  grid-column: 1 / -1;
}

.field.check-field .switch-row {
  display: flex;
  align-items: center;
  gap: 8px;
  cursor: pointer;
}

.field.check-field .switch-row em {
  font-style: normal;
  font-size: 12px;
}

.field.check-field input[type="checkbox"] {
  width: 15px;
  height: 15px;
  accent-color: var(--primary-color, #3b82f6);
}

.field input,
.field select,
.field textarea {
  border: 1px solid var(--border-color, #2c2f36);
  border-radius: 8px;
  background: var(--bg-secondary, #22252b);
  color: inherit;
  padding: 6px 10px;
  font-size: 12px;
  font-family: inherit;
  resize: vertical;
}

.save-hint {
  font-size: 12px;
  color: var(--text-secondary, #9aa0a6);
}

.empty-line {
  color: var(--text-secondary, #9aa0a6);
  font-size: 12px;
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
  padding: 8px 10px;
  border-radius: 8px;
  background: var(--bg-secondary, #22252b);
}

.run-status {
  flex-shrink: 0;
  font-size: 11px;
  padding: 2px 8px;
  border-radius: 999px;
  border: 1px solid currentColor;
  margin-top: 1px;
}

.run-status.running {
  color: #60a5fa;
}

.run-status.waiting_human {
  color: #fbbf24;
}

.run-status.done {
  color: #4ade80;
}

.run-status.failed {
  color: #f87171;
}

.run-status.cancelled {
  color: #9aa0a6;
}

.run-body {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.run-goal {
  font-size: 12px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.run-meta {
  display: flex;
  gap: 12px;
  font-size: 11px;
  color: var(--text-secondary, #9aa0a6);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.run-error {
  color: #f87171;
}
</style>
