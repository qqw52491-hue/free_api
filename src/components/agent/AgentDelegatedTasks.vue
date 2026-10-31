<template>
  <section v-if="tasks.length || loadError" class="delegated-tasks" aria-label="通用子任务进度">
    <details v-for="task in tasks" :key="task.child_run_id" :open="!!task.human_request">
      <summary>
        <strong>{{ task.task?.split('\n')[0] || '通用子任务' }}</strong>
        <span>{{ label(task) }}</span>
      </summary>
      <p>任务 {{ task.child_run_id.slice(0, 8) }} · <span v-if="task.step !== undefined">当前步骤 {{ task.step + 1 }} · </span>{{ task.updated_at ? `最近活动 ${time(task.updated_at)}` : '正在启动' }}</p>
      <p v-if="task.queued_messages">{{ task.queued_messages }} 条消息尚未进入任务上下文<span v-if="task.execution_ended">，可在后续追问时补充</span>。</p>
      <AgentHumanInterventionCard v-if="task.human_request" :human-form="form(task)" :pause-reason="task.human_request.message" :is-sending="busy[task.child_run_id]" @submit="answer(task, $event)" @cancel="cancel(task)" />
      <form v-if="!task.execution_ended" class="message-row" @submit.prevent="send(task)">
        <input v-model="drafts[task.child_run_id]" aria-label="给子任务补充资料" placeholder="给这个子任务补充资料或约束…" />
        <button :disabled="busy[task.child_run_id] || !drafts[task.child_run_id]?.trim()">发送补充</button>
        <button type="button" :disabled="busy[task.child_run_id]" @click="cancel(task)">停止子任务</button>
      </form>
      <p v-if="receipts[task.child_run_id]">{{ receipts[task.child_run_id].delivery === 'in_context' ? '补充已进入任务上下文' : task.execution_ended ? '任务已结束，这条补充尚未进入上下文' : '补充已入队，等待下一个安全边界' }}</p>
      <p v-if="errors[task.child_run_id]" role="alert">{{ errors[task.child_run_id] }}</p>
    </details>
    <p v-if="loadError" role="alert">{{ loadError }}</p>
  </section>
</template>

<script setup lang="ts">
import { onMounted, onUnmounted, ref, watch } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import AgentHumanInterventionCard from './AgentHumanInterventionCard.vue';
import { normalizeHumanForm } from '../../composables/useAgentSession';
type Task = { child_run_id: string; task?: string; activity?: string; execution_status: string; execution_ended: boolean; delivery_accepted: boolean; step?: number; phase?: string; updated_at?: string; queued_messages?: number; runtime_attached?: boolean; human_request?: {message?: string; form?: unknown} };
type Receipt = {message_id: string; delivery: string};
const props = defineProps<{ sessionId: string; active: boolean }>();
const tasks = ref<Task[]>([]), drafts = ref<Record<string,string>>({}), busy = ref<Record<string,boolean>>({}), errors = ref<Record<string,string>>({}), receipts = ref<Record<string,Receipt>>({});
const loadError = ref('');
let timer: ReturnType<typeof setInterval> | undefined;
let loading = false;
function call<T>(action: string, params: unknown = {}) { return invoke<T>('control_child_task', {sessionId: props.sessionId, action, params}); }
function label(t: Task) { if (t.human_request) return '等待你回答'; if (t.delivery_accepted) return '子目标已交付'; if (t.execution_status === 'cancelled') return '已停止'; if (t.execution_ended) return '未完成'; return t.phase === 'done' ? '正在收尾' : t.runtime_attached === false ? '执行器未连接，等待核对状态' : '执行中'; }
function time(value: string) { return new Date(value).toLocaleTimeString(); }
function form(t: Task) { return normalizeHumanForm(t.human_request?.form, t.human_request?.message || '子任务需要你的回答'); }
async function refresh() {
  if (!props.sessionId || loading) return;
  const session = props.sessionId; loading = true;
  try {
    const result = await call<{tasks: Task[]}>('list');
    if (session !== props.sessionId) return;
    tasks.value = result.tasks; loadError.value = '';
    for (const task of result.tasks) {
      const receipt = receipts.value[task.child_run_id];
      if (receipt?.delivery === 'queued') receipts.value[task.child_run_id] = await call<Receipt>('receipt', {child_run_id: task.child_run_id, message_id: receipt.message_id});
    }
  } catch (error) { if (session === props.sessionId) loadError.value = `子任务进度读取失败：${String(error)}`; }
  finally { loading = false; }
}
async function perform(t: Task, action: string, message?: string) {
  busy.value[t.child_run_id] = true; errors.value[t.child_run_id] = '';
  try {
    const result = await call<Receipt>(action, {child_run_id:t.child_run_id, message});
    if (action === 'send') { receipts.value[t.child_run_id] = result; drafts.value[t.child_run_id] = ''; }
    await refresh();
  } catch (error) { errors.value[t.child_run_id] = String(error); }
  finally { busy.value[t.child_run_id] = false; }
}
function send(t: Task) { return perform(t, 'send', drafts.value[t.child_run_id]); }
function answer(t: Task, message: string) { return perform(t, 'answer', message); }
function cancel(t: Task) { return perform(t, 'cancel'); }
watch(() => props.sessionId, () => {tasks.value=[]; receipts.value={}; errors.value={}; void refresh();});
onMounted(() => {void refresh(); timer = setInterval(() => {if (props.active || tasks.value.some(t => !t.execution_ended)) void refresh();}, 3000);});
onUnmounted(() => {if(timer) clearInterval(timer);});
</script>

<style scoped>
.delegated-tasks { max-height: 38vh; overflow: auto; padding: 0 18px 8px; font-size: 12px; }
details { border: 1px solid var(--border-color, #d7dce2); border-radius: 9px; padding: 9px 12px; margin: 5px 0; }
summary { cursor: pointer; display: flex; gap: 10px; justify-content: space-between; }
summary strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
summary span { white-space: nowrap; color: var(--text-secondary, #64748b); }
p { margin: 8px 0; color: var(--text-secondary, #64748b); }
.message-row { display: flex; gap: 7px; flex-wrap: wrap; }
input { flex: 1; min-width: 160px; padding: 7px; border: 1px solid var(--border-color, #d7dce2); border-radius: 6px; color: inherit; background: transparent; }
button { padding: 6px 9px; border: 1px solid var(--border-color, #d7dce2); border-radius: 6px; cursor: pointer; color: inherit; background: transparent; }
button:disabled { opacity: .5; cursor: default; }
[role=alert] { color: #b45309; }
</style>
