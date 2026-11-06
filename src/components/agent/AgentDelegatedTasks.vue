<template>
  <section v-if="tasks.length || loadError" class="delegated-tasks-dock-wrap" aria-label="协同子 Agent 进度" @click.stop>
    <div class="subagent-dock" :class="{ 'is-active': runningCount > 0, 'is-waiting': waitingCount > 0 }">
      <button type="button" class="dock-header" :aria-expanded="dockOpen" @click="toggleDock">
        <div class="dock-header-left">
          <span class="state-dot" aria-hidden="true">
            <span v-if="waitingCount > 0" class="pulse-dot"></span>
            <span v-else-if="runningCount > 0" class="spinner-ring"></span>
            <svg v-else-if="failedCount > 0" class="state-svg is-failed" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><line x1="18" y1="6" x2="6" y2="18" /><line x1="6" y1="6" x2="18" y2="18" /></svg>
            <svg v-else-if="cancelledCount > 0 && deliveredCount === 0 && handoffCount === 0" class="state-svg is-cancelled" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><rect x="6" y="6" width="12" height="12" rx="2" /></svg>
            <svg v-else-if="handoffCount > 0 && deliveredCount === 0" class="state-svg is-handoff" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.3"><polyline points="9 10 4 15 9 20" /><path d="M20 4v7a4 4 0 0 1-4 4H4" /></svg>
            <svg v-else class="state-svg is-done" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><polyline points="20 6 9 17 4 12" /></svg>
          </span>
          <span class="dock-title">协同子 Agent ({{ tasks.length }})</span>
          <div class="dock-pills">
            <span v-if="runningCount" class="dock-pill is-running">{{ runningCount }} 运行中</span>
            <span v-if="waitingCount" class="dock-pill is-waiting">{{ waitingCount }} 待回答</span>
            <span v-if="deliveredCount" class="dock-pill is-done">{{ deliveredCount }} 已交付</span>
            <span v-if="handoffCount" class="dock-pill is-handoff">{{ handoffCount }} 已交接</span>
            <span v-if="cancelledCount" class="dock-pill is-cancelled">{{ cancelledCount }} 已停止</span>
            <span v-if="failedCount" class="dock-pill is-failed">{{ failedCount }} 未完成</span>
          </div>
          <span v-if="!dockOpen && headerPreview" class="dock-ticker-preview">{{ headerPreview }}</span>
        </div>
        <div class="dock-header-right">
          <span class="dock-toggle-label">{{ dockOpen ? '收起' : '展开' }}</span>
          <svg class="dock-chevron" :class="{ 'is-open': dockOpen }" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><polyline points="6 9 12 15 18 9" /></svg>
        </div>
      </button>

      <div v-if="dockOpen" class="dock-tray soft-scrollbar">
        <article
          v-for="task in tasks"
          :key="task.child_run_id"
          class="subagent-item"
          :class="[`state-${stateKey(task)}`, { 'is-expanded': isTaskOpen(task) }]"
          :style="{ marginLeft: `${Math.max(0, (task.depth || 1) - 1) * 14}px` }"
        >
          <div class="subagent-row" role="button" tabindex="0" title="点击在右侧打开该子 Agent 详细执行工作台" @click="inspectTask(task)" @keydown.enter.prevent="inspectTask(task)">
            <div class="subagent-row-left">
              <span v-if="(task.depth || 1) > 1" class="tree-branch">↳</span>
              <span v-if="(task.depth || 1) > 1" class="depth-badge">L{{ task.depth }}</span>
              <span class="state-dot" aria-hidden="true">
                <span v-if="stateKey(task) === 'waiting'" class="pulse-dot"></span>
                <span v-else-if="stateKey(task) === 'running'" class="spinner-ring"></span>
                <svg v-else-if="stateKey(task) === 'done'" class="state-svg is-done" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><polyline points="20 6 9 17 4 12" /></svg>
                <svg v-else-if="stateKey(task) === 'handoff'" class="state-svg is-handoff" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.3"><polyline points="9 10 4 15 9 20" /><path d="M20 4v7a4 4 0 0 1-4 4H4" /></svg>
                <svg v-else-if="stateKey(task) === 'cancelled'" class="state-svg is-cancelled" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><rect x="6" y="6" width="12" height="12" rx="2" /></svg>
                <svg v-else class="state-svg is-failed" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><line x1="18" y1="6" x2="6" y2="18" /><line x1="6" y1="6" x2="18" y2="18" /></svg>
              </span>
              <span class="role-pill" :class="`kind-${task.purpose || 'task'}`">{{ roleBadge(task) }}</span>
              <strong class="task-title" :title="taskTitle(task)">{{ taskTitle(task) }}</strong>
              <span v-if="liveTicker(task)" class="live-activity-ticker" :title="liveTicker(task)">
                <span v-if="task.latest_tool" class="ticker-tool-tag">{{ shortTool(task.latest_tool) }}</span>
                <span class="ticker-step-text">{{ liveTicker(task) }}</span>
              </span>
            </div>
            <div class="subagent-row-right">
              <span v-if="task.queued_messages" class="meta-chip queue-chip" title="待进入上下文的消息数">✉️ {{ task.queued_messages }}</span>
              <span v-if="task.model_id && shortModel(task.model_id)" class="meta-chip model-chip" :title="shortModel(task.model_id)">{{ shortModel(task.model_id) }}</span>
              <span v-if="stepCount(task)" class="meta-chip">{{ stepCount(task) }} 步</span>
              <span v-if="durationLabel(task)" class="meta-chip">{{ durationLabel(task) }}</span>
              <span class="status-badge" :class="`state-${stateKey(task)}`">{{ label(task) }}</span>
              <button
                type="button"
                class="subagent-context-toggle"
                :class="{ 'is-open': isTaskOpen(task) }"
                :aria-expanded="isTaskOpen(task)"
                title="展开/收起子任务背景与结论摘要"
                @click.stop="toggleTask(task)"
              >
                <span>{{ isTaskOpen(task) ? '收起背景' : '背景' }}</span>
                <svg class="dock-chevron" :class="{ 'is-open': isTaskOpen(task) }" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><polyline points="6 9 12 15 18 9" /></svg>
              </button>
            </div>
          </div>

          <div
            v-if="task.recent_steps?.length"
            class="trace-steps-list soft-scrollbar"
            aria-label="最近执行步骤（点击查看详情）"
          >
            <div
              v-for="st in task.recent_steps"
              :key="st.seq"
              class="trace-step-row"
              role="button"
              tabindex="0"
              title="点击在右侧工作台查看该子 Agent 详细步骤与输出"
              @click="inspectTask(task)"
              @keydown.enter.prevent="inspectTask(task)"
            >
              <span class="trace-seq">#{{ st.seq }}</span>
              <span class="trace-tool">{{ shortTool(st.tool) }}</span>
              <span v-if="st.target" class="trace-target" :title="st.target">{{ st.target }}</span>
              <span v-if="st.description" class="trace-desc" :title="st.description">{{ st.description }}</span>
            </div>
          </div>

          <div v-if="isTaskOpen(task)" class="subagent-detail">
            <div class="detail-meta-bar">
              <span>#{{ task.child_run_id.slice(0, 8) }}</span>
              <span>层级 L{{ task.depth || 1 }}</span>
              <span v-if="task.phase">阶段 {{ task.phase }}</span>
              <span v-if="task.updated_at">更新于 {{ time(task.updated_at) }}</span>
              <span v-if="task.queued_messages">{{ task.queued_messages }} 条消息待投递</span>
              <button type="button" class="link-btn" @click.stop="inspectTask(task)">在右侧打开完整工作台 ↗</button>
            </div>

            <div v-if="!fullFor(task) && (isTaskTruncated(task) || isSummaryTruncated(task))" class="truncation-bar">
              <span class="truncation-tag">摘要 · 已截断（目标最多 300 字 / 结论最多 180 字）</span>
              <button type="button" class="link-btn" :disabled="busy[task.child_run_id]" @click.stop="loadFull(task)">查看完整内容</button>
            </div>
            <div v-else-if="fullFor(task)" class="truncation-bar">
              <span class="truncation-tag">{{ task.execution_ended ? '宿主保存的完整记录' : '运行中的目标全文；交付结论产生后将自动更新' }}</span>
              <button v-if="!task.execution_ended" type="button" class="link-btn" :disabled="busy[task.child_run_id]" @click.stop="loadFull(task)">刷新</button>
              <button type="button" class="link-btn" @click.stop="collapseFull(task)">收起</button>
            </div>
            <pre v-if="hasMultiLineGoal(task) || fullFor(task)?.goal" class="task-prompt-box">{{ fullFor(task)?.goal || task.task?.trim() }}</pre>
            <div v-if="fullFor(task)?.summary || task.summary" class="task-summary-box">{{ fullFor(task)?.summary || task.summary }}</div>

            <AgentHumanInterventionCard
              v-if="task.human_request"
              :human-form="form(task)"
              :pause-reason="task.human_request.message"
              :is-sending="busy[task.child_run_id]"
              @submit="answer(task, $event)"
              @cancel="cancel(task)"
            />

            <form v-if="!task.execution_ended" class="subagent-msg-form" @submit.prevent="send(task)">
              <input v-model="drafts[task.child_run_id]" class="subagent-msg-input" aria-label="给子任务补充资料" placeholder="给这个子 Agent 补充上下文或约束…" />
              <button type="submit" class="subagent-btn is-primary" :disabled="busy[task.child_run_id] || !drafts[task.child_run_id]?.trim()">发送补充</button>
              <button type="button" class="subagent-btn is-danger" :disabled="busy[task.child_run_id]" @click="cancel(task)">停止</button>
            </form>

            <p v-if="receipts[task.child_run_id]" class="receipt-notice">
              {{ receipts[task.child_run_id].delivery === 'in_context' ? '✓ 补充已进入子任务上下文' : task.execution_ended ? '任务已结束，该补充尚未进入上下文' : '⏳ 补充已入队，等待下一个安全边界注入' }}
            </p>
            <p v-if="errors[task.child_run_id]" class="error-notice" role="alert">{{ errors[task.child_run_id] }}</p>
          </div>
        </article>
      </div>
    </div>
    <p v-if="loadError" class="error-notice" role="alert">{{ loadError }}</p>
  </section>
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import AgentHumanInterventionCard from './AgentHumanInterventionCard.vue';
import { normalizeHumanForm } from '../../composables/useAgentSession';
import {
  createSerialRefresher, detectAutoOpen, fullCacheIsFresh, isSettled, mergeSnapshot,
  type FullDetailCache,
} from './delegatedTaskState';
type StepTrace = { seq: number; tool: string; target?: string | null; description?: string | null; status?: string };
type Task = {
  child_run_id: string;
  depth?: number;
  parent_session_id?: string;
  task?: string;
  task_truncated?: boolean;
  summary?: string;
  summary_truncated?: boolean;
  purpose?: string;
  role?: string;
  role_label?: string;
  model_id?: string;
  activity?: string;
  execution_status: string;
  execution_ended: boolean;
  delivery_accepted: boolean;
  handoff_accepted?: boolean;
  step?: number;
  total_steps?: number;
  latest_tool?: string;
  latest_step?: string;
  recent_steps?: StepTrace[];
  phase?: string;
  stage?: string;
  started_at?: string;
  updated_at?: string;
  finished_at?: string;
  duration_ms?: number;
  queued_messages?: number;
  runtime_attached?: boolean;
  human_request?: { message?: string; form?: unknown };
};
type Receipt = { message_id: string; delivery: string };

const props = defineProps<{ sessionId: string; active: boolean; allModels?: Array<{ id: string; name: string }> }>();
const emit = defineEmits<{ 'inspect-worker': [childRunId: string] }>();
const tasks = ref<Task[]>([]);
const drafts = ref<Record<string, string>>({});
const busy = ref<Record<string, boolean>>({});
const errors = ref<Record<string, string>>({});
const receipts = ref<Record<string, Receipt>>({});
const fullDetails = ref<Record<string, { goal?: string; summary?: string; cache: FullDetailCache }>>({});
const userDockOpen = ref<boolean | null>(null);
const expandedMap = ref<Record<string, boolean>>({});
const loadError = ref('');

let timer: ReturnType<typeof setInterval> | undefined;
const seenActiveIds = new Set<string>();
const seenWaitingIds = new Set<string>();

const runningCount = computed(() => tasks.value.filter((t) => !t.execution_ended && !t.human_request).length);
const waitingCount = computed(() => tasks.value.filter((t) => !!t.human_request).length);
const deliveredCount = computed(() => tasks.value.filter((t) => t.delivery_accepted === true).length);
const handoffCount = computed(() => tasks.value.filter((t) => !t.delivery_accepted && t.handoff_accepted === true).length);
const cancelledCount = computed(() => tasks.value.filter((t) => t.execution_ended && !t.delivery_accepted && !t.handoff_accepted && t.execution_status === 'cancelled').length);
const failedCount = computed(() => tasks.value.filter((t) => t.execution_ended && !t.delivery_accepted && !t.handoff_accepted && t.execution_status !== 'cancelled').length);

const dockOpen = computed(() => {
  if (userDockOpen.value !== null) return userDockOpen.value;
  return waitingCount.value > 0 || runningCount.value > 0;
});

const headerPreview = computed(() => {
  const active = tasks.value.find((t) => !!t.human_request) || tasks.value.find((t) => !t.execution_ended) || tasks.value[tasks.value.length - 1];
  if (!active) return '';
  const step = liveTicker(active);
  return step ? `${taskTitle(active)} · ${step}` : taskTitle(active);
});

function toggleDock() {
  userDockOpen.value = !dockOpen.value;
}

function stateKey(t: Task): 'waiting' | 'running' | 'done' | 'handoff' | 'cancelled' | 'failed' {
  if (t.human_request) return 'waiting';
  if (t.delivery_accepted) return 'done';
  if (t.handoff_accepted) return 'handoff';
  if (t.execution_status === 'cancelled') return 'cancelled';
  if (t.execution_ended) return 'failed';
  return 'running';
}

function isTaskOpen(t: Task): boolean {
  if (t.child_run_id in expandedMap.value) return expandedMap.value[t.child_run_id];
  return !!t.human_request;
}

function toggleTask(t: Task) {
  expandedMap.value[t.child_run_id] = !isTaskOpen(t);
}

function inspectTask(t: Task) {
  emit('inspect-worker', t.child_run_id);
}

function isTaskTruncated(t: Task): boolean {
  return t.task_truncated === true || (t.task || '').length >= 300;
}

function isSummaryTruncated(t: Task): boolean {
  return t.summary_truncated === true || (t.summary || '').length >= 180;
}

function fullFor(t: Task): { goal?: string; summary?: string } | undefined {
  const entry = fullDetails.value[t.child_run_id];
  return entry && fullCacheIsFresh(entry.cache, t) ? entry : undefined;
}

function roleBadge(t: Task): string {
  if (t.role_label) return t.role_label;
  if (t.purpose === 'worker') return '侦察探针';
  if (t.purpose === 'writer') return '代码写入';
  if (t.purpose === 'reviewer') return '独立审查';
  return '子任务';
}

function taskTitle(t: Task): string {
  const raw = (t.task || '').split(/\r?\n/).map((s) => s.trim()).find(Boolean) || '协同子任务';
  return raw.length > 56 ? `${raw.slice(0, 55)}…` : raw;
}

function hasMultiLineGoal(t: Task): boolean {
  const raw = (t.task || '').trim();
  return raw.includes('\n') || raw.length > 56;
}

function liveTicker(t: Task): string {
  if (t.human_request?.message) return t.human_request.message.split(/\r?\n/)[0];
  if (!t.execution_ended) {
    if (t.latest_step) return t.latest_step.split(/\r?\n/)[0];
    if (t.stage) return t.stage.replace(/^child_task:/, '');
    return '正在思考与推进…';
  }
  if (t.summary) return t.summary.split(/\r?\n/)[0];
  return t.latest_step ? t.latest_step.split(/\r?\n/)[0] : '';
}

function shortTool(tool: string): string {
  return tool.includes('/') ? (tool.split('/').pop() || tool) : tool;
}

function shortModel(model: string): string {
  const matched = props.allModels?.find(m => m.id === model);
  const resolved = matched?.name || model;
  if (/^[0-9a-f]{8}-[0-9a-f]{4}-/i.test(resolved)) return '';
  const clean = resolved.includes('/') ? (resolved.split('/').pop() || resolved) : resolved;
  return clean.replace(/-\d{8}$/, '');
}

function stepCount(t: Task): number {
  return t.total_steps || (t.step !== undefined ? t.step + 1 : 0);
}

function durationLabel(t: Task): string {
  if (typeof t.duration_ms === 'number' && t.duration_ms > 0) {
    return `${(t.duration_ms / 1000).toFixed(1)}s`;
  }
  if (t.started_at) {
    const start = Date.parse(t.started_at);
    const end = t.finished_at ? Date.parse(t.finished_at) : Date.now();
    if (!Number.isNaN(start) && !Number.isNaN(end) && end >= start) {
      const sec = Math.floor((end - start) / 1000);
      return sec >= 60 ? `${Math.floor(sec / 60)}m${sec % 60}s` : `${sec}s`;
    }
  }
  return '';
}

function call<T>(action: string, params: unknown = {}) {
  return invoke<T>('control_child_task', { sessionId: props.sessionId, action, params });
}

function label(t: Task) {
  if (t.human_request) return '等待回答';
  if (t.delivery_accepted) return '子目标已交付';
  if (t.handoff_accepted) return '已交接';
  if (t.execution_status === 'cancelled') return '已停止';
  if (t.execution_ended) return '未完成';
  return t.phase === 'done' ? '正在收尾' : t.runtime_attached === false ? '等待核对状态' : '执行中';
}

async function loadFull(t: Task) {
  const session = props.sessionId;
  busy.value[t.child_run_id] = true;
  errors.value[t.child_run_id] = '';
  try {
    // 宿主返回完整保存记录（不经模型侧 2000/8000 字投影）。
    const res = await call<Record<string, any>>('result', { child_run_id: t.child_run_id });
    if (session !== props.sessionId) return;
    const goal = typeof res?.goal === 'string' ? res.goal.split('【宿主提供')[0].trim() : t.task;
    const summary = typeof res?.delivery?.summary === 'string' ? res.delivery.summary.trim() : undefined;
    fullDetails.value[t.child_run_id] = {
      goal, summary,
      // 以读取结果自身的执行状态为准；任务结算后失效重读。
      cache: { settled: isSettled(res as Task) },
    };
  } catch (error) {
    if (session === props.sessionId) errors.value[t.child_run_id] = `完整内容加载失败：${String(error)}`;
  } finally {
    busy.value[t.child_run_id] = false;
  }
}

function collapseFull(t: Task) {
  delete fullDetails.value[t.child_run_id];
}

function time(value: string) {
  return new Date(value).toLocaleTimeString();
}

function form(t: Task) {
  return normalizeHumanForm(t.human_request?.form, t.human_request?.message || '子任务需要你的回答');
}

async function doRefresh() {
  const session = props.sessionId;
  if (!session) return;
  try {
    const result = await call<{ tasks: Task[] }>('list');
    if (session !== props.sessionId) return;
    const previous = new Map(tasks.value.map((t) => [t.child_run_id, t]));
    const merged = result.tasks.map((t) => mergeSnapshot(previous.get(t.child_run_id), t));
    for (const task of merged) {
      if (task.human_request && !seenWaitingIds.has(task.child_run_id)) delete expandedMap.value[task.child_run_id];
    }
    // 仅新任务启动或新出现人工请求才自动展开；持续存在的等待项不会重置手动收起。
    if (detectAutoOpen(seenActiveIds, seenWaitingIds, merged)) userDockOpen.value = null;
    tasks.value = merged;
    loadError.value = '';
    if (typeof window !== 'undefined') {
      window.dispatchEvent(new CustomEvent('agent:child-tasks-synced', {
        detail: { sessionId: session, tasks: merged },
      }));
    }
    for (const task of merged) {
      const receipt = receipts.value[task.child_run_id];
      if (receipt?.delivery === 'queued') {
        receipts.value[task.child_run_id] = await call<Receipt>('receipt', {
          child_run_id: task.child_run_id,
          message_id: receipt.message_id,
        });
      }
    }
  } catch (error) {
    if (session === props.sessionId) loadError.value = `子任务进度读取失败：${String(error)}`;
  }
}

// 串行：进行中的查询收到新请求（含切换会话）会在结束后补跑一次。
const refresher = createSerialRefresher(doRefresh);
function refresh() { return refresher.request(); }

async function perform(t: Task, action: string, message?: string) {
  busy.value[t.child_run_id] = true;
  errors.value[t.child_run_id] = '';
  try {
    const result = await call<Receipt>(action, { child_run_id: t.child_run_id, message });
    if (action === 'send') {
      receipts.value[t.child_run_id] = result;
      drafts.value[t.child_run_id] = '';
    }
    await refresh();
  } catch (error) {
    errors.value[t.child_run_id] = String(error);
  } finally {
    busy.value[t.child_run_id] = false;
  }
}

function send(t: Task) { return perform(t, 'send', drafts.value[t.child_run_id]); }
function answer(t: Task, message: string) { return perform(t, 'answer', message); }
function cancel(t: Task) { return perform(t, 'cancel'); }

function onProgressEvent(e: Event) {
  const detail = (e as CustomEvent).detail;
  if (!detail || detail.sessionId !== props.sessionId) return;
  const target = tasks.value.find((t) => t.child_run_id === detail.childRunId);
  if (target && detail.event && !isSettled(target)) {
    if (detail.event.tool || detail.event.action) target.latest_tool = String(detail.event.tool || detail.event.action);
    if (detail.event.description && detail.event.description !== '执行操作') target.latest_step = String(detail.event.description);
  }
  void refresh();
}

watch(() => props.sessionId, () => {
  tasks.value = [];
  receipts.value = {};
  errors.value = {};
  fullDetails.value = {};
  expandedMap.value = {};
  seenActiveIds.clear();
  seenWaitingIds.clear();
  userDockOpen.value = null;
  void refresh();
});

onMounted(() => {
  void refresh();
  window.addEventListener('agent:child-task-progress', onProgressEvent);
  timer = setInterval(() => {
    if (props.active || tasks.value.some((t) => !t.execution_ended)) void refresh();
  }, 2500);
});

onUnmounted(() => {
  window.removeEventListener('agent:child-task-progress', onProgressEvent);
  if (timer) clearInterval(timer);
});
</script>

<style scoped src="./agent-delegated-tasks.css"></style>
