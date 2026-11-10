<template>
  <div class="workflow-page">
    <div class="workflow-content">
      <header class="page-heading">
        <div><span class="eyebrow">AGENT SETTINGS</span><h1>子任务默认流程</h1><p>为普通子 Agent 设置共同起点，角色只需配置不同的部分。</p></div>
        <span class="scope-badge">应用级 · 新派发任务</span>
      </header>
      <div v-if="loading" class="notice" role="status">正在读取配置…</div>
      <div v-if="error" class="notice error" role="alert">{{ error }}</div>
      <template v-if="loaded">
        <section class="flow-card">
          <div class="section-heading"><span class="step">01</span><div><h2>结果交给谁验收</h2><p>先确定交付责任，再安排适用的方案和审查环节。</p></div></div>
          <div class="delivery-options" role="radiogroup" aria-label="默认交付方式">
            <button v-for="option in deliveryOptions" :key="option.value" type="button" role="radio" :aria-checked="(config.completion || '') === option.value" :class="{selected:(config.completion || '') === option.value}" @click="config.completion = option.value || null">
              <span class="radio-dot"></span><strong>{{ option.label }}</strong><span>{{ option.description }}</span>
            </button>
          </div>
          <div class="outcome-preview" aria-live="polite"><div><span>通用 task</span><strong>{{ config.completion === 'handoff' ? '交回结果，父任务集成验收' : '完整交付，执行适用验证与审查' }}</strong></div><div><span>调查 / 实施助手</span><strong>{{ config.completion === 'delivery' ? '完整交付，执行适用验证与审查' : '交回结果，父任务集成验收' }}</strong></div></div>
        </section>
        <section class="flow-card">
          <div class="section-heading"><span class="step">02</span><div><h2>方案如何安排</h2><p>日常使用可保留现有策略；需要先讨论或人工批准时再指定。</p></div></div>
          <label class="plan-field"><span>默认方案模式</span><select :value="config.plan_mode || ''" @change="changePlan(($event.target as HTMLSelectElement).value)"><option value="">沿用现有策略</option><option value="cross_review">先讨论并审查方案</option><option value="plan_approval">方案需要人工批准</option></select></label>
          <p v-if="config.plan_mode === 'plan_approval'" class="approval-note">新派发的普通子任务须保留人工批准要求，角色和派发参数不能自行取消。</p>
          <details class="advanced"><summary>单独调整方案与审查环节 <span>可选</span></summary><StageSettingsFields v-model="config.stages" :approval="config.plan_mode === 'plan_approval'" /></details>
          <p class="resolved-note">{{ effectiveSummary }}</p>
        </section>
        <section class="inheritance-card"><h2>这份配置在哪里生效</h2><div class="inheritance-flow"><span>子任务默认流程</span><b>→</b><span>角色特殊设置</span><b>→</b><span>本次派发设置</span></div><p>仅补充角色和本次派发未指定的字段；人工批准约束继续保留。适用于普通 task、调查和实施助手。正式审查、隔离补丁、已有任务恢复及追问沿用各自规则。</p><p>模型、工具权限与主任务流程保持独立。明确的验证要求和已有审查阻塞始终有效。</p></section>
        <footer class="save-bar"><div role="status" :class="{success:message && !dirty}">{{ dirty ? '有未保存的更改' : message || '配置已同步' }}</div><div><button type="button" class="reset-button" :disabled="saving" @click="reset">恢复原有默认</button><button type="button" class="save-button" :disabled="saving || !dirty" @click="save">{{ saving ? '正在保存…' : '保存设置' }}</button></div></footer>
      </template>
      <button v-else-if="!loading" class="reset-button" @click="load">重新读取</button>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import StageSettingsFields from '../components/agent/StageSettingsFields.vue';
import type { StageSettings } from '../components/agent/types';
interface Defaults { completion: string | null; plan_mode: string | null; stages: StageSettings }
const empty = (): Defaults => ({completion:null,plan_mode:null,stages:{}});
const config = ref<Defaults>(empty());
const saved = ref('');
const loading = ref(true), loaded = ref(false), saving = ref(false);
const error = ref(''), message = ref('');
const dirty = computed(() => JSON.stringify(config.value) !== saved.value);
const deliveryOptions = [
  {value:'',label:'沿用现有策略',description:'通用任务完整交付，调查与实施助手交回结果。'},
  {value:'handoff',label:'交给父任务验收',description:'完成子目标后交回，由父任务负责集成和整体交付。'},
  {value:'delivery',label:'子任务完整交付',description:'子任务自己完成适用的验证与独立审查。'},
];
const effectiveSummary = computed(() => config.value.plan_mode || Object.keys(config.value.stages).length
  ? '显式设置优先；自动环节仍按交付方式与任务分流决定，不代表全部开启。'
  : '保留原有安排：完整交付按分流决定适用环节；结果交接默认不重复走方案与审查。');
function changePlan(value: string) {
  config.value.plan_mode = value || null;
  if (value === 'plan_approval' && config.value.stages.plan === false) config.value.stages.plan = true;
  message.value = '';
}
function reset() { config.value = empty(); message.value = ''; }
async function load() {
  loading.value = true; error.value = '';
  try { config.value = await invoke<Defaults>('get_child_workflow_defaults'); saved.value = JSON.stringify(config.value); loaded.value = true; }
  catch (e) { error.value = `无法读取设置：${String(e)}`; }
  finally { loading.value = false; }
}
async function save() {
  saving.value = true; error.value = ''; message.value = '';
  const snapshot = JSON.stringify(config.value);
  try { await invoke('save_child_workflow_defaults',{config:JSON.parse(snapshot)}); saved.value = snapshot; message.value = '已保存，将用于后续新派发的普通子任务'; }
  catch (e) { error.value = `保存失败：${String(e)}`; }
  finally { saving.value = false; }
}
onMounted(load);
</script>

<style scoped>
.workflow-page{height:100%;overflow:auto;background:var(--theme-bg-base,#111318);color:var(--theme-text-primary,#e6e8ef)}
.workflow-content{max-width:1000px;margin:auto;padding:42px 36px 30px}.page-heading{display:flex;justify-content:space-between;gap:24px;align-items:center;margin-bottom:30px}.eyebrow{font-size:11px;letter-spacing:.14em;color:var(--theme-text-muted,#9399a8)}h1{font-size:28px;letter-spacing:-.03em;margin:10px 0}p{color:var(--theme-text-muted,#9399a8);font-size:13px;line-height:1.8;margin:6px 0}.scope-badge{white-space:nowrap;border:1px solid var(--border-1,#ffffff18);border-radius:20px;padding:7px 12px;font-size:11px;color:var(--theme-text-muted,#9399a8)}
.flow-card{padding:26px;margin-bottom:18px;background:var(--theme-bg-panel,#191c23);border:1px solid var(--border-1,#ffffff13);border-radius:16px}.section-heading{display:flex;gap:14px;align-items:flex-start;margin-bottom:22px}.step{font-size:12px;color:#88a6f8;background:#668bf218;padding:8px;border-radius:9px}h2{font-size:16px;margin:2px 0 4px;font-weight:600}.delivery-options{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:12px}.delivery-options button{position:relative;text-align:left;padding:19px 16px;background:transparent;color:inherit;border:1px solid var(--border-1,#ffffff18);border-radius:11px;cursor:pointer;transition:background .15s,border-color .15s}.delivery-options button:hover{border-color:#789bf2}.delivery-options button.selected{border-color:#789bf2;background:#668bf20d}.radio-dot{display:block;width:12px;height:12px;border:1px solid var(--theme-text-muted,#9399a8);border-radius:50%;margin-bottom:16px}.selected .radio-dot{border:4px solid #789bf2;width:6px;height:6px}.delivery-options strong{display:block;font-size:13px;margin-bottom:9px}.delivery-options button>span:last-child{display:block;font-size:12px;line-height:1.7;color:var(--theme-text-muted,#9399a8)}.outcome-preview{display:grid;grid-template-columns:1fr 1fr;gap:16px;margin-top:20px;padding:16px;background:var(--theme-bg-base,#111318);border-radius:9px}.outcome-preview span{display:block;font-size:11px;color:var(--theme-text-muted,#9399a8);margin-bottom:6px}.outcome-preview strong{font-size:12px;font-weight:500}.plan-field{display:flex;align-items:center;justify-content:space-between;gap:20px;font-size:13px}.plan-field select{width:55%;padding:11px;border:1px solid var(--border-1,#ffffff18);border-radius:8px;background:var(--theme-bg-base,#111318);color:inherit}.advanced{margin-top:24px;padding-top:18px;border-top:1px solid var(--border-1,#ffffff13)}summary{cursor:pointer;font-size:13px}summary span{font-size:11px;margin-left:8px;color:var(--theme-text-muted,#9399a8)}.advanced :deep(.stage-fields){margin-bottom:0;border:0;padding:12px 0 0;gap:16px}.advanced :deep(legend){display:none}.advanced :deep(select){background:var(--theme-bg-base,#111318);padding:10px}.resolved-note{font-size:12px;margin-top:18px}.approval-note{color:var(--theme-text-secondary,#c3b2f1);margin-top:14px}.inheritance-card{padding:18px 4px}.inheritance-card h2{font-size:13px}.inheritance-flow{display:flex;gap:12px;align-items:center;margin:16px 0;font-size:12px}.inheritance-flow span{padding:7px 11px;border-radius:6px;background:var(--theme-bg-panel,#191c23)}.inheritance-flow b{font-weight:400;color:var(--theme-text-muted,#9399a8)}.inheritance-card p{font-size:12px}.save-bar{position:sticky;bottom:0;display:flex;align-items:center;justify-content:space-between;gap:16px;padding:18px 0;background:var(--theme-bg-base,#111318);border-top:1px solid var(--border-1,#ffffff13);margin-top:20px;font-size:12px;color:var(--theme-text-muted,#9399a8)}.save-bar>div:last-child{display:flex;gap:10px}.reset-button,.save-button{padding:10px 17px;border-radius:8px;font-size:12px;cursor:pointer}.reset-button{border:1px solid var(--border-1,#ffffff20);background:transparent;color:var(--theme-text-primary,#e6e8ef)}.save-button{border:1px solid #789bf2;background:#789bf2;color:#10192f;font-weight:600}button:disabled{opacity:.45;cursor:default}button:focus-visible,select:focus-visible,summary:focus-visible{outline:2px solid #a6bfff;outline-offset:3px}.success{color:var(--theme-status-success,#7acba8)}.notice{padding:16px;margin-bottom:18px;border-radius:9px;background:var(--theme-bg-panel,#191c23);font-size:13px}.error{color:#f2a2a2}
@media(max-width:700px){.workflow-content{padding:24px 18px}.page-heading{display:block}.scope-badge{display:inline-block;margin-top:12px}.delivery-options,.outcome-preview{grid-template-columns:1fr}.flow-card{padding:20px}.delivery-options button{padding:14px}.radio-dot{float:right;margin:0}.save-bar{align-items:flex-start;flex-direction:column}.plan-field{align-items:flex-start;flex-direction:column}.plan-field select{width:100%}.inheritance-flow{gap:6px;flex-wrap:wrap}}
</style>
