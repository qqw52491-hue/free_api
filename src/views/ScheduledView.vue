<template>
  <div class="scheduled-view">
    <header class="view-header">
      <h2>⏱️ 定时任务中心</h2>
      <p class="header-sub">
        按时间自动派发 Agent 任务；应用需保持运行。完成后若微信桥在线，结果会推送到绑定的微信。
      </p>
    </header>

    <!-- 新建/编辑 -->
    <section class="card">
      <div class="card-title-row">
        <h3>{{ editingId ? "编辑定时任务" : "新建定时任务" }}</h3>
        <button v-if="editingId" class="btn ghost" @click="resetForm">取消编辑</button>
      </div>

      <div class="form-grid">
        <label class="field">
          <span>任务名称</span>
          <input v-model="form.name" type="text" placeholder="例如：每日巡检" />
        </label>
        <label class="field">
          <span>触发方式</span>
          <select v-model="form.kind">
            <option value="daily">每天定时</option>
            <option value="interval">固定间隔</option>
          </select>
        </label>
        <label v-if="form.kind === 'daily'" class="field">
          <span>时间（本地 HH:MM）</span>
          <input v-model="form.time" type="time" />
        </label>
        <label v-if="form.kind === 'interval'" class="field">
          <span>间隔（分钟）</span>
          <input v-model.number="form.minutes" type="number" min="1" max="43200" />
        </label>
        <label class="field">
          <span>工作区项目（可选，留空用临时区）</span>
          <select v-model="form.project_id">
            <option value="">临时工作区</option>
            <option v-for="p in projects" :key="p.id" :value="p.id">{{ p.name }}</option>
          </select>
        </label>
      </div>

      <label class="field wide">
        <span>任务内容（发给 Agent 的目标）</span>
        <textarea v-model="form.goal" rows="3" placeholder="例如：检查项目里的 TODO 与未提交改动，输出一份简报"></textarea>
      </label>

      <div class="action-row">
        <button class="btn primary" :disabled="saving" @click="save">
          {{ saving ? "保存中…" : editingId ? "保存修改" : "创建定时任务" }}
        </button>
        <span v-if="hint" class="hint">{{ hint }}</span>
      </div>
    </section>

    <!-- 列表 -->
    <section class="card">
      <div class="card-title-row">
        <h3>已有计划（{{ schedules.length }}）</h3>
        <span class="config-path" :title="listPath">{{ listPath }}</span>
      </div>
      <div v-if="!schedules.length" class="empty-line">还没有定时任务，先在上面创建一个。</div>
      <ul v-else class="schedule-list">
        <li v-for="item in schedules" :key="item.id" class="schedule-item">
          <div class="schedule-head">
            <span class="sched-name">{{ item.name }}</span>
            <span class="state-pill" :class="item.enabled ? 'on' : 'off'">
              {{ item.enabled ? "启用中" : "已停用" }}
            </span>
          </div>
          <div class="sched-meta">
            <span>
              {{ item.kind === "daily" ? `每天 ${item.time}` : `每 ${item.minutes} 分钟` }}
            </span>
            <span>下次：{{ formatTime(item.next_fired_at) }}</span>
            <span>工作区：{{ projectName(item.project_id) }}</span>
          </div>
          <div class="sched-goal">{{ item.goal }}</div>
          <div class="sched-actions">
            <button class="btn" @click="toggle(item)">{{ item.enabled ? "停用" : "启用" }}</button>
            <button class="btn" @click="edit(item)">编辑</button>
            <button class="btn danger" @click="remove(item)">删除</button>
          </div>
        </li>
      </ul>
    </section>
  </div>
</template>

<script setup lang="ts">
import { onMounted, reactive, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";

interface ScheduleItem {
  id: string;
  name: string;
  enabled: boolean;
  kind: string;
  time: string | null;
  minutes: number | null;
  project_id: string | null;
  goal: string;
  session_id: string | null;
  last_fired_at: string | null;
  next_fired_at: string | null;
}

interface AgentProjectLite {
  id: string;
  name: string;
}

const schedules = ref<ScheduleItem[]>([]);
const projects = ref<AgentProjectLite[]>([]);
const listPath = ref("");
const saving = ref(false);
const hint = ref("");
const editingId = ref("");

const form = reactive({
  name: "",
  kind: "daily",
  time: "09:00",
  minutes: 60,
  project_id: "",
  goal: "",
});

async function load() {
  try {
    const value = await invoke<{ schedules: ScheduleItem[]; path: string }>(
      "wechat_schedule_list"
    );
    schedules.value = value.schedules;
    listPath.value = value.path;
  } catch (error) {
    hint.value = `加载失败：${error}`;
  }
  try {
    projects.value = await invoke<AgentProjectLite[]>("list_agent_projects");
  } catch {
    projects.value = [];
  }
}

function resetForm() {
  editingId.value = "";
  form.name = "";
  form.kind = "daily";
  form.time = "09:00";
  form.minutes = 60;
  form.project_id = "";
  form.goal = "";
}

async function save() {
  saving.value = true;
  hint.value = "";
  try {
    await invoke("wechat_schedule_save", {
      item: {
        id: editingId.value,
        name: form.name,
        enabled: true,
        kind: form.kind,
        time: form.kind === "daily" ? form.time : null,
        minutes: form.kind === "interval" ? form.minutes : null,
        project_id: form.project_id || null,
        goal: form.goal,
        session_id: null,
        last_fired_at: null,
        next_fired_at: null,
      },
    });
    hint.value = "已保存 ✓";
    resetForm();
    await load();
  } catch (error) {
    hint.value = `保存失败：${error}`;
  }
  saving.value = false;
}

async function toggle(item: ScheduleItem) {
  try {
    await invoke("wechat_schedule_toggle", { id: item.id, enabled: !item.enabled });
    await load();
  } catch (error) {
    hint.value = `操作失败：${error}`;
  }
}

async function remove(item: ScheduleItem) {
  try {
    await invoke("wechat_schedule_delete", { id: item.id });
    await load();
  } catch (error) {
    hint.value = `删除失败：${error}`;
  }
}

function edit(item: ScheduleItem) {
  editingId.value = item.id;
  form.name = item.name;
  form.kind = item.kind;
  form.time = item.time || "09:00";
  form.minutes = item.minutes || 60;
  form.project_id = item.project_id || "";
  form.goal = item.goal;
  window.scrollTo?.(0, 0);
}

function projectName(projectId: string | null): string {
  if (!projectId) return "临时工作区";
  return projects.value.find((p) => p.id === projectId)?.name || "已删除项目";
}

function formatTime(ts: string | null): string {
  if (!ts) return "—";
  const date = new Date(ts);
  if (Number.isNaN(date.getTime())) return "—";
  return date.toLocaleString("zh-CN", { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" });
}

onMounted(load);
</script>

<style scoped>
.scheduled-view {
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

.action-row {
  display: flex;
  align-items: center;
  gap: 8px;
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

.btn.ghost {
  background: transparent;
}

.hint {
  font-size: 12px;
  color: var(--text-secondary, #9aa0a6);
}

.empty-line {
  color: var(--text-secondary, #9aa0a6);
  font-size: 12px;
}

.schedule-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.schedule-item {
  padding: 10px 12px;
  border-radius: 8px;
  background: var(--bg-secondary, #22252b);
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.schedule-head {
  display: flex;
  align-items: center;
  gap: 10px;
}

.sched-name {
  font-size: 13px;
  font-weight: 600;
}

.state-pill {
  font-size: 11px;
  padding: 1px 8px;
  border-radius: 999px;
  border: 1px solid currentColor;
}

.state-pill.on {
  color: #4ade80;
}

.state-pill.off {
  color: #9aa0a6;
}

.sched-meta {
  display: flex;
  gap: 14px;
  font-size: 11px;
  color: var(--text-secondary, #9aa0a6);
  flex-wrap: wrap;
}

.sched-goal {
  font-size: 12px;
  color: var(--text-secondary, #d0d4da);
}

.sched-actions {
  display: flex;
  gap: 8px;
}
</style>
