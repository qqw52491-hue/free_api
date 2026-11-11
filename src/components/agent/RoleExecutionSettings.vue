<template>
  <section class="execution-settings" aria-label="角色的独立配置">
    <div class="heading"><div><h4>角色的独立配置</h4><p>公共流程在「子任务默认流程」统一管理。这里仅设置这个角色的例外，各区域独立保存。</p></div><button type="button" class="workflow-link" @click="emit('open-workflow')">查看公共默认流程 →</button></div>
    <p v-if="parseError" class="error">原配置无法解析，尚未修改。<button type="button" @click="repairConfig">重新设置</button></p>
    <fieldset class="independent-fields" :disabled="parseError">
      <section class="settings-block" aria-label="工具权限">
        <h5>工具权限</h5>
        <label class="tool-mode">可选工具范围
          <select :value="toolMode" @change="changeToolMode($event)">
            <option value="inherit">由 AI 按需选择（默认）</option>
            <option value="allowlist">仅允许指定工具</option>
          </select>
        </label>
        <p v-if="toolMode === 'inherit'" class="hint">继承父任务可用工具，AI 自行决定每次用哪些。</p>
        <template v-else>
          <label class="tool-list">允许的工具或动作
            <textarea v-model="toolsText" rows="3" placeholder="例如 local_file.read, search, my_plugin/lookup" @input="publish()" />
          </label>
          <p class="hint">AI 从列表中按需选择，父任务和宿主权限仍然生效。留空会禁用业务工具，交付、人工求助和只读通信查询仍可用。</p>
        </template>
        <p v-if="isReviewer" class="hint">正式审查可使用内置只读取证工具。外部 MCP 需先在「能力管理」的插件卡片上开启「审查工具」授权。</p>
      </section>
      <section class="settings-block" aria-label="工作流程">
        <h5>工作流程 <span class="section-tag">{{ flowMode === 'custom' && !specialized ? '角色自定义' : specialized ? '专用结果契约' : '继承公共默认' }}</span></h5>
        <p class="hint">只决定方案、审查和交付安排，不改变上面的工具权限。</p>
        <label class="tool-mode">流程来源<select :value="flowMode" :disabled="specialized" @change="changeFlowMode($event)"><option value="inherit">继承子任务默认流程</option><option value="custom">为这个角色自定义</option></select></label>
        <template v-if="flowMode === 'custom' && !specialized">
          <div class="controls">
            <label>方案安排<select v-model="planMode" @change="publish()"><option value="">继承公共默认</option><option value="auto">自动 / 原有策略</option><option value="cross_review">先讨论并审查方案</option><option value="plan_approval">方案需要人工批准</option></select></label>
            <label>结果如何结束<select v-model="completion" @change="publish()"><option value="">继承公共默认</option><option value="delivery">完整交付，执行适用验证和审查</option><option value="handoff">交回结果，由父任务集成验收</option></select></label>
          </div>
          <StageSettingsFields v-model="stages" :approval="planMode === 'plan_approval'" @update:model-value="publish()" />
        </template>
        <p v-else class="hint">{{ specialized ? '专用审查或隔离补丁按专用结果契约执行。' : '角色不附加流程覆盖，沿用公共默认及本次派发设置。' }}</p>
      </section>
      <section class="settings-block" aria-label="执行方式">
        <h5>执行方式 <span class="section-tag">高级选项</span></h5>
        <label class="tool-mode">默认执行方式<select :value="profile" @change="changeProfile($event)"><option value="">沿用派发入口</option><option value="task">通用子任务</option><option value="worker">调查助手</option><option value="writer">实施助手</option><option value="reviewer">专用审查</option></select></label>
        <p v-if="workspace === 'isolated'" class="hint">此角色仍有历史隔离配置。请先改为共享工作区再派发新任务。<button type="button" class="workflow-link" @click="changeWorkspace()">改为共享工作区</button></p>
        <p class="hint">普通方式共用通用执行器；调查名称不自动保证只读。切到专用方式时会说明不兼容的流程变化。</p>
        <details class="preset-details"><summary>快捷组合（同时调整执行方式与流程）</summary><div class="presets"><button v-for="preset in presets" :key="preset.id" type="button" :aria-pressed="matches(preset)" :class="{selected:matches(preset)}" @click="applyPreset(preset)"><strong>{{ preset.label }}</strong><span>{{ preset.description }}</span></button></div></details>
      </section>
    </fieldset>
    <div class="summary" aria-live="polite">{{ summary }}</div>
    <p class="hint">修改后点击角色页面的保存按钮生效。角色模型、职责和字段契约独立保留，已登记任务不会被直接改写。</p>
  </section>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import StageSettingsFields from './StageSettingsFields.vue';
import type { StageSettings } from './types';
const stages = ref<StageSettings>({});
type Profile = '' | 'task' | 'worker' | 'writer' | 'reviewer';
type PlanMode = '' | 'auto' | 'cross_review' | 'plan_approval';
type Completion = '' | 'handoff' | 'delivery';
interface Preset { id: string; label: string; description: string; profile: Profile; planMode: PlanMode; completion: Completion; tools?: string[] }
const props = defineProps<{ modelValue: string; allowedKinds?: string[] }>();
const emit = defineEmits<{ 'update:modelValue': [value: string]; 'profile-change': [profile: Profile]; 'open-workflow': [] }>();
const flowMode = ref<'inherit' | 'custom'>('inherit');
const profile = ref<Profile>('');
const planMode = ref<PlanMode>('');
const completion = ref<Completion>('');
const workspace = ref<'shared' | 'isolated'>('shared');
const isReviewer = computed(() => profile.value === 'reviewer' || (
  !profile.value && props.allowedKinds?.length === 1 && props.allowedKinds[0] === 'reviewer'
));
const specialized = computed(() => profile.value === 'reviewer' || workspace.value === 'isolated');
const toolMode = ref<'inherit' | 'allowlist'>('inherit');
const toolsText = ref('');
const parseError = ref(false);
let lastEmitted: string | undefined;
const presets: Preset[] = [
  { id: 'general', label: '通用执行', description: '自动安排方案，完成子目标交付', profile: 'task', planMode: 'auto', completion: 'delivery' },
  { id: 'discussion', label: '先讨论方案', description: '方案交叉审查后执行并交付', profile: 'task', planMode: 'cross_review', completion: 'delivery' },
  { id: 'handoff', label: '结果交接', description: '完成子目标材料，交父任务验收', profile: 'task', planMode: 'auto', completion: 'handoff' },
  { id: 'investigate', label: '调查助手', description: '通用执行器，按调查职责返回证据', profile: 'worker', planMode: 'auto', completion: 'handoff' },
  { id: 'write', label: '实施助手', description: '通用执行器，共享工作区实施与验证', profile: 'writer', planMode: 'auto', completion: 'handoff' },
  { id: 'review', label: '专用审查', description: '按审查契约返回裁决与发现', profile: 'reviewer', planMode: 'auto', completion: 'handoff' },
];
const names: Record<Profile, string> = { '': '沿用派发入口', task: '通用子任务', worker: '调查助手', writer: '实施助手', reviewer: '专用审查' };
const summary = computed(() => `${names[profile.value]} · ${specialized.value ? '专用结果契约' : flowMode.value === 'inherit' ? '继承公共流程' : '角色自定义流程'} · ${toolMode.value === 'inherit' ? 'AI 按需选择工具' : '限定工具范围'}`);
function toolNames() { return [...new Set(toolsText.value.split(/[,，\n]/).map(s => s.trim()).filter(Boolean))]; }
function publish(includeProfile = false) {
  if (planMode.value === 'plan_approval' && stages.value.plan === false) stages.value = { ...stages.value, plan: true };
  if (parseError.value) return;
  const workflow = !specialized.value ? (flowMode.value === 'custom' ? { ...(planMode.value ? { plan_mode: planMode.value } : {}), ...(completion.value ? { completion: completion.value } : {}) } : {}) : { completion: 'handoff' };
  lastEmitted = JSON.stringify({ ...(profile.value ? { profile: profile.value } : {}), execution: { ...(!specialized.value && flowMode.value === 'custom' ? { stages: stages.value } : {}), ...(workspace.value === 'isolated' ? { workspace: 'isolated' } : {}), ...(toolMode.value === 'allowlist' ? { allowed_tools: toolNames() } : {}), workflow } });
  emit('update:modelValue', lastEmitted);
  if (includeProfile && profile.value) emit('profile-change', profile.value);
}
function changeToolMode(event: Event) {
  const select = event.target as HTMLSelectElement;
  const next = select.value as 'inherit' | 'allowlist';
  if (toolMode.value === 'allowlist' && next === 'inherit' && !window.confirm('切换后，AI 可从父任务和宿主允许的工具中按需选择。是否移除这个角色的工具列表限制？')) {
    select.value = toolMode.value; return;
  }
  toolMode.value = next;
  publish();
}
function kindExpansionWarning(nextProfile: Profile): string[] {
  const allowed = props.allowedKinds || [];
  return nextProfile && allowed.length && !allowed.includes(nextProfile) ? [`把 ${names[nextProfile]} 加入角色允许挂载的类型`] : [];
}
function changeFlowMode(event: Event) {
  const select = event.target as HTMLSelectElement;
  const next = select.value as 'inherit' | 'custom';
  if (next === 'inherit' && flowMode.value === 'custom' && !window.confirm('将撤下这个角色的方案、审查和交付覆盖，改用公共默认及派发设置。工具限制与执行方式保留。是否继续？')) { select.value = flowMode.value; return; }
  flowMode.value = next;
  publish();
}
function repairConfig() {
  if (!window.confirm('原配置无法解析，重新设置会替换无法读取的原配置（可能包含工具限制）。是否继续？')) return;
  resetInputs(); parseError.value = false; publish();
}
function specialChangeWarning(nextProfile: Profile, nextWorkspace: 'shared' | 'isolated'): string[] {
  const changes: string[] = [];
  const nextSpecialized = nextProfile === 'reviewer' || nextWorkspace === 'isolated';
  if (nextSpecialized && !specialized.value) {
    if (Object.keys(stages.value).length) changes.push('清除当前普通工作环节设置');
    if (flowMode.value === 'custom' && planMode.value) changes.push('清除当前方案模式');
    if (completion.value !== 'handoff') changes.push('结果改为交接');
  }
  return changes;
}
function approveChanges(changes: string[]): boolean {
  return changes.length === 0 || window.confirm(`此操作将${changes.join('、')}。工具限制保留，已派发任务不受影响。是否继续？`);
}
function changeProfile(event: Event) {
  const select = event.target as HTMLSelectElement;
  const next = select.value as Profile;
  const nextWorkspace = next === 'writer' ? workspace.value : 'shared';
  const changes = [...specialChangeWarning(next, nextWorkspace), ...kindExpansionWarning(next)];
  if (nextWorkspace !== workspace.value) changes.push('工作区改为共享');
  if (!approveChanges(changes)) { select.value = profile.value; return; }
  profile.value = next; workspace.value = nextWorkspace;
  if (specialized.value) { stages.value = {}; completion.value = 'handoff'; planMode.value = ''; flowMode.value = 'inherit'; }
  publish(true);
}
function changeWorkspace() {
  if (!approveChanges(['将后续任务改为直接修改共享父工作区'])) return;
  workspace.value = 'shared';
  publish();
}
function applyPreset(preset: Preset) {
  if (parseError.value && !window.confirm('原执行配置无法解析，继续将用快捷组合替换，原工具限制可能无法保留。是否继续？')) return;
  const changes: string[] = kindExpansionWarning(preset.profile);
  {
    if (profile.value !== preset.profile) changes.push('更改执行方式');
    if (workspace.value !== 'shared') changes.push('工作区改为共享');
    if (planMode.value !== preset.planMode) changes.push('更改方案模式');
    if (completion.value !== preset.completion) changes.push('更改结果结束方式');
    changes.push(...specialChangeWarning(preset.profile, 'shared').filter(c => !changes.includes(c)));
  }
  if (!approveChanges(changes)) return;
  flowMode.value = 'custom'; workspace.value = 'shared'; profile.value = preset.profile; planMode.value = preset.planMode; completion.value = preset.completion;
  if (specialized.value) stages.value = {};
  // Preserve tool restrictions and stage settings supported by the new profile.
  publish(true);
}
function resetInputs() {
  profile.value = ''; planMode.value = ''; completion.value = ''; flowMode.value = 'inherit';
  workspace.value = 'shared'; stages.value = {}; toolMode.value = 'inherit'; toolsText.value = '';
}
function matches(preset: Preset) { return Object.keys(stages.value).length === 0 && flowMode.value === 'custom' && workspace.value === 'shared' && profile.value === preset.profile && planMode.value === preset.planMode && completion.value === preset.completion; }
watch(() => props.modelValue, raw => {
  if (raw === lastEmitted) return;
  if (!raw) { resetInputs(); parseError.value = false; return; }
  try {
    const value = JSON.parse(raw);
    if (!value || typeof value !== 'object' || (value.profile != null && !['task', 'worker', 'writer', 'reviewer'].includes(value.profile))) throw new Error('profile');
    parseError.value = false; profile.value = value.profile || '';  workspace.value = value.execution?.workspace || 'shared';
    stages.value = value.execution?.stages || {};
    planMode.value = value.execution?.workflow?.plan_mode || ''; completion.value = value.execution?.workflow?.completion || '';
    flowMode.value = planMode.value || completion.value || Object.keys(stages.value).length ? 'custom' : 'inherit';
    toolMode.value = Array.isArray(value.execution?.allowed_tools) ? 'allowlist' : 'inherit'; toolsText.value = value.execution?.allowed_tools?.join(', ') || '';
  } catch { resetInputs(); parseError.value = true; }
}, { immediate: true });
</script>

<style scoped>
.independent-fields {border:0;padding:0;margin:0;min-width:0}.workflow-link {background:transparent;border:0;color:var(--theme-text-secondary);cursor:pointer;font-size:12px}.section-tag {font-size:11px;font-weight:400;color:var(--theme-text-muted);margin-left:8px}
.execution-settings { padding: 18px; border: 1px solid var(--border-1, #3f3f46); border-radius: 12px; background: var(--bg-1, #18181b); }
.heading { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; flex-wrap: wrap; }
h4 { margin: 0; font-size: 15px; color: var(--theme-text-primary, #f4f4f5); }
p { margin: 7px 0 0; }
.heading p, .hint { color: var(--theme-text-muted, #a1a1aa); font-size: 12px; line-height: 1.65; }
.workflow-link { display: flex; align-items: center; gap: 7px; font-size: 13px; color: var(--theme-text-primary, #e4e4e7); cursor: pointer; }
.presets { display: grid; grid-template-columns: repeat(auto-fit, minmax(175px, 1fr)); gap: 9px; margin: 16px 0; }
.presets button { padding: 12px; border: 1px solid var(--border-1, #3f3f46); border-radius: 9px; background: transparent; color: var(--theme-text-primary, #e4e4e7); text-align: left; cursor: pointer; }
.presets button:hover, .presets button.selected { border-color: var(--theme-status-info, #38bdf8); background: rgba(56, 189, 248, .07); }
.presets strong { display: flex; align-items: center; gap: 7px; font-size: 13px; }
.presets button > span { display: block; margin-top: 6px; font-size: 12px; color: var(--theme-text-muted, #a1a1aa); line-height: 1.5; }
.recommended { padding: 2px 5px; border-radius: 4px; font-size: 10px; color: var(--theme-status-info, #38bdf8); background: rgba(56, 189, 248, .12); }
.settings-block { margin-top: 16px; padding: 14px; border: 1px solid var(--border-1, #3f3f46); border-radius: 9px; }
.settings-block h5 { margin: 0; font-size: 13px; color: var(--theme-text-primary, #e4e4e7); }
.settings-block .controls { margin-top: 12px; }
.controls { display: grid; grid-template-columns: repeat(auto-fit, minmax(175px, 1fr)); gap: 12px; }
.controls label, .tool-list, .tool-mode { min-width: 0; display: flex; flex-direction: column; gap: 7px; font-size: 12px; color: var(--theme-text-primary, #e4e4e7); }
select, textarea { width: 100%; box-sizing: border-box; padding: 9px; border: 1px solid var(--border-1, #3f3f46); border-radius: 7px; background: var(--theme-inset-bg, #202024); color: var(--theme-text-primary, #e4e4e7); font: inherit; }
select:disabled { opacity: .55; }
textarea { resize: vertical; }
details { margin-top: 14px; border-top: 1px solid var(--border-1, #3f3f46); padding-top: 12px; }
summary { cursor: pointer; font-size: 13px; color: var(--theme-text-primary, #e4e4e7); }
.tool-mode, .tool-list { margin-top: 12px; }
.summary { margin-top: 14px; padding: 10px 12px; border-radius: 7px; background: rgba(56, 189, 248, .07); color: var(--theme-status-info, #38bdf8); font-size: 12px; line-height: 1.6; }
.error { color: var(--theme-status-danger, #f87171); font-size: 12px; }
</style>
