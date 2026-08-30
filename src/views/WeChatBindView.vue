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
        <div class="field">
          <span>默认模型（会话创建）</span>
          <ModelSelectDropdown
            :model-value="form.model_id"
            :models="allModels || []"
            placeholder="搜索并选择默认模型..."
            size="sm"
            @update:model-value="form.model_id = $event"
          />
        </div>
        <div class="field">
          <span>Pro 路由（推理）</span>
          <ModelSelectDropdown
            :model-value="form.pro"
            :models="allModels || []"
            placeholder="跟随默认模型 (或选择 Pro)..."
            size="sm"
            @update:model-value="form.pro = $event"
          />
        </div>
        <div class="field">
          <span>Flash 路由（执行）</span>
          <ModelSelectDropdown
            :model-value="form.flash"
            :models="allModels || []"
            placeholder="跟随默认模型 (或选择 Flash)..."
            size="sm"
            @update:model-value="form.flash = $event"
          />
        </div>
        <div class="field">
          <span>Vision 路由（视觉）</span>
          <ModelSelectDropdown
            :model-value="form.vision"
            :models="allModels || []"
            placeholder="跟随默认模型 (或选择 Vision)..."
            size="sm"
            @update:model-value="form.vision = $event"
          />
        </div>
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
  display: flex;
  flex-direction: column;
  gap: 16px;
  max-width: 900px;
  margin: 0 auto;
  padding: 16px 20px 40px;
  color: #f4f4f5;
  user-select: none;
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

.qr-area {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 10px;
  padding: 16px;
  border: 1px dashed rgba(56, 189, 248, 0.35);
  border-radius: 12px;
  background: rgba(56, 189, 248, 0.03);
}

.qr-img {
  width: 220px;
  height: 220px;
  border-radius: 10px;
  background: #fff;
  padding: 10px;
  box-shadow: 0 4px 20px rgba(0, 0, 0, 0.4);
}

.qr-waiting {
  width: 220px;
  height: 180px;
  display: flex;
  align-items: center;
  justify-content: center;
  color: #71717a;
  font-size: 12px;
  text-align: center;
}

.qr-tip {
  margin: 0;
  font-size: 11px;
  color: #a1a1aa;
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
</style>
