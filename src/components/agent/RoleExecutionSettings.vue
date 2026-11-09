<template>
  <section class="execution-settings" aria-label="角色的可选执行默认值">
    <div class="heading">
      <div><h4>角色的可选执行默认值</h4><p>角色定义职责和交付字段；这里仅为挂载此角色的新任务提供默认值。不选角色也可派发通用子任务。</p></div>
      <label class="enable"><input :checked="enabled" type="checkbox" @change="changeEnabled($event)" />使用角色默认值</label>
    </div>
    <p v-if="parseError" class="error">原执行配置无法解析，请选择一个推荐组合重新设置。</p>
    <details class="preset-details">
      <summary>快捷组合（会同时设置执行方式和流程）</summary>
      <p class="hint">快捷组合是已有设置的组合，不决定工具权限；涉及多项变化时会提示确认。</p>
      <div class="presets">
        <button v-for="preset in presets" :key="preset.id" type="button" :aria-pressed="matches(preset)" :class="{ selected: matches(preset) }" @click="applyPreset(preset)">
          <strong>{{ preset.label }}<span v-if="preset.id === 'general'" class="recommended">推荐</span></strong><span>{{ preset.description }}</span>
        </button>
      </div>
    </details>
    <template v-if="enabled">
      <section class="settings-block" aria-label="执行方式">
        <h5>执行方式</h5>
        <p class="hint">决定如何运行和交回结果，不自动授予工具权限。</p>
        <div class="controls">
          <label>执行方式<select :value="profile" @change="changeProfile($event)"><option value="task">通用子任务</option><option value="worker">调查助手</option><option value="writer">实施助手</option><option value="reviewer">专用审查</option></select></label>
          <label>结果如何结束<select v-model="completion" :disabled="specialized" @change="publish()"><option value="delivery">完整交付，按任务验证和审查</option><option value="handoff">交回结果，由父任务集成验收</option></select></label>
        </div>
        <p class="hint">{{ !specialized ? '普通方式使用同一通用执行器；调查职责不自动保证只读，需要限制时请设置工具权限。' : '正式审查或显式隔离补丁使用专用结果契约；完整流程选择共享工作区。' }}</p>
        <label v-if="profile === 'writer'" class="tool-mode">工作区方式<select :value="workspace" @change="changeWorkspace($event)"><option value="shared">共享父工作区，直接实施</option><option value="isolated">隔离补丁，审阅后 apply</option></select></label>
      </section>
      <section class="settings-block" aria-label="工具权限">
        <h5>工具权限</h5>
        <p class="hint">决定允许调用什么，与流程是否开启分开；不会扩大父任务或宿主权限。</p>
        <label class="tool-mode">权限范围<select :value="toolMode" @change="changeToolMode($event)"><option value="inherit">继承父任务可用工具</option><option value="allowlist">仅允许指定工具</option></select></label>
        <label v-if="toolMode === 'allowlist'" class="tool-list">允许的工具或动作<textarea v-model="toolsText" rows="3" placeholder="例如：read_file, list_dir, send_message&#10;通用任务也可填写 local_file.read" @input="publish()" /></label>
        <p class="hint">使用逗号或换行分隔，填写小写名称。列表为空时禁用业务工具；交回结果、方案流程、人工求助和只读通信查询仍可用。派发参数可进一步缩小范围，父任务、角色工具上限与资源授权继续生效。</p>
      </section>
      <section class="settings-block" aria-label="工作流程">
        <h5>工作流程</h5>
        <p class="hint">决定方案和审查如何安排，不授予或取消工具权限。自动沿用现有宿主策略，不等于由 Agent 完全自主决定。</p>
        <div class="controls"><label>方案安排<select v-model="planMode" :disabled="specialized" @change="publish()"><option value="auto">自动 / 沿用现有策略</option><option value="cross_review">先讨论并审查方案</option><option value="plan_approval">方案需要人工批准</option></select></label></div>
        <StageSettingsFields v-if="!specialized" v-model="stages" :approval="planMode === 'plan_approval'" @update:model-value="publish()" />
        <p class="hint">结果交接子任务默认关闭重复环节；显式选择方案模式时沿用该模式。关闭自动审查不等于禁止主动审查，仍受实际权限约束。</p>
      </section>
      <div class="summary" aria-live="polite">{{ summary }}</div>
      <p class="hint">每个子任务保存独立检查点。切换快捷组合保留工具限制和兼容的工作环节；涉及其他设置变化时会提示确认。</p>
    </template>
    <p v-else class="hint">未启用时沿用派发入口的默认值和父任务权限；角色职责、模型及输入输出字段仍然有效。</p>
  </section>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import StageSettingsFields from './StageSettingsFields.vue';
import type { StageSettings } from './types';
const stages = ref<StageSettings>({});
type Profile = 'task' | 'worker' | 'writer' | 'reviewer';
type PlanMode = 'auto' | 'cross_review' | 'plan_approval';
type Completion = 'handoff' | 'delivery';
interface Preset { id: string; label: string; description: string; profile: Profile; planMode: PlanMode; completion: Completion; tools?: string[] }
const props = defineProps<{ modelValue: string; allowedKinds?: string[] }>();
const emit = defineEmits<{ 'update:modelValue': [value: string]; 'profile-change': [profile: Profile] }>();
const enabled = ref(false);
const profile = ref<Profile>('task');
const planMode = ref<PlanMode>('auto');
const completion = ref<Completion>('delivery');
const workspace = ref<'shared' | 'isolated'>('shared');
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
const names: Record<Profile, string> = { task: '通用子任务', worker: '调查助手', writer: '实施助手', reviewer: '专用审查' };
const summary = computed(() => `${names[profile.value]} · ${!specialized.value ? ({ auto: '按固定环节配置', cross_review: '先审查方案', plan_approval: '方案人工批准' }[planMode.value]) + ' · ' : ''}${completion.value === 'delivery' ? '完整交付' : '结果交接'} · ${toolMode.value === 'inherit' ? '继承可用能力' : '限定工具范围'}`);
function toolNames() { return [...new Set(toolsText.value.split(/[,，\n]/).map(s => s.trim()).filter(Boolean))]; }
function publish(includeProfile = false) {
  parseError.value = false;
  if (planMode.value === 'plan_approval' && stages.value.plan === false) stages.value = { ...stages.value, plan: true };
  if (!enabled.value) { lastEmitted = ''; emit('update:modelValue', ''); return; }
  const workflow = !specialized.value ? { plan_mode: planMode.value, completion: completion.value } : { completion: 'handoff' };
  lastEmitted = JSON.stringify({ profile: profile.value, execution: { ...(!specialized.value ? { stages: stages.value } : {}), ...(workspace.value === 'isolated' ? { workspace: 'isolated' } : {}), ...(toolMode.value === 'allowlist' ? { allowed_tools: toolNames() } : {}), workflow } });
  emit('update:modelValue', lastEmitted);
  if (includeProfile) emit('profile-change', profile.value);
}
function changeToolMode(event: Event) {
  const select = event.target as HTMLSelectElement;
  const next = select.value as 'inherit' | 'allowlist';
  if (toolMode.value === 'allowlist' && next === 'inherit' && !window.confirm('切换到继承将移除角色当前的工具列表限制；父任务和宿主权限仍然生效。是否继续？')) {
    select.value = toolMode.value; return;
  }
  toolMode.value = next;
  publish();
}
function kindExpansionWarning(nextProfile: Profile): string[] {
  const allowed = props.allowedKinds || [];
  return allowed.length && !allowed.includes(nextProfile) ? [`把 ${names[nextProfile]} 加入角色允许挂载的类型`] : [];
}
function changeEnabled(event: Event) {
  const input = event.target as HTMLInputElement;
  if (input.checked && parseError.value && !window.confirm('原执行配置无法解析，继续将用新默认值替换，原工具限制可能无法保留。是否继续？')) { input.checked = false; return; }
  if (!input.checked && enabled.value) {
    if (!window.confirm('停用后会移除角色保存的执行默认值，包括工具限制；新任务将按派发入口和父任务权限运行。是否继续？')) { input.checked = true; return; }
  } else if (input.checked && !approveChanges(kindExpansionWarning(profile.value))) {
    input.checked = false; return;
  }
  enabled.value = input.checked;
  publish(true);
  if (!input.checked) resetInputs();
}
function specialChangeWarning(nextProfile: Profile, nextWorkspace: 'shared' | 'isolated'): string[] {
  const changes: string[] = [];
  const nextSpecialized = nextProfile === 'reviewer' || nextWorkspace === 'isolated';
  if (nextSpecialized && !specialized.value) {
    if (Object.keys(stages.value).length) changes.push('清除当前普通工作环节设置');
    if (planMode.value !== 'auto') changes.push('清除当前方案模式');
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
  if (specialized.value) { stages.value = {}; completion.value = 'handoff'; planMode.value = 'auto'; }
  publish(true);
}
function changeWorkspace(event: Event) {
  const select = event.target as HTMLSelectElement;
  const next = select.value as 'shared' | 'isolated';
  if (!approveChanges([...specialChangeWarning(profile.value, next), ...kindExpansionWarning(profile.value)])) { select.value = workspace.value; return; }
  workspace.value = next;
  if (specialized.value) { stages.value = {}; completion.value = 'handoff'; planMode.value = 'auto'; }
  publish(true);
}
function applyPreset(preset: Preset) {
  if (parseError.value && !window.confirm('原执行配置无法解析，继续将用快捷组合替换，原工具限制可能无法保留。是否继续？')) return;
  const changes: string[] = kindExpansionWarning(preset.profile);
  if (enabled.value) {
    if (profile.value !== preset.profile) changes.push('更改执行方式');
    if (workspace.value !== 'shared') changes.push('工作区改为共享');
    if (planMode.value !== preset.planMode) changes.push('更改方案模式');
    if (completion.value !== preset.completion) changes.push('更改结果结束方式');
    changes.push(...specialChangeWarning(preset.profile, 'shared').filter(c => !changes.includes(c)));
  }
  if (!approveChanges(changes)) return;
  enabled.value = true; workspace.value = 'shared'; profile.value = preset.profile; planMode.value = preset.planMode; completion.value = preset.completion;
  if (specialized.value) stages.value = {};
  // Preserve tool restrictions and stage settings supported by the new profile.
  publish(true);
}
function resetInputs() {
  profile.value = 'task'; planMode.value = 'auto'; completion.value = 'delivery';
  workspace.value = 'shared'; stages.value = {}; toolMode.value = 'inherit'; toolsText.value = '';
}
function matches(preset: Preset) { return Object.keys(stages.value).length === 0 && enabled.value && workspace.value === 'shared' && profile.value === preset.profile && planMode.value === preset.planMode && completion.value === preset.completion; }
watch(() => props.modelValue, raw => {
  if (raw === lastEmitted) return;
  if (!raw) { resetInputs(); enabled.value = false; parseError.value = false; return; }
  try {
    const value = JSON.parse(raw);
    if (!['task', 'worker', 'writer', 'reviewer'].includes(value.profile)) throw new Error('profile');
    enabled.value = true; parseError.value = false; profile.value = value.profile; workspace.value = value.execution?.workspace || 'shared';
    stages.value = value.execution?.stages || {};
    planMode.value = value.execution?.workflow?.plan_mode || 'auto'; completion.value = value.execution?.workflow?.completion || (profile.value === 'task' ? 'delivery' : 'handoff');
    toolMode.value = Array.isArray(value.execution?.allowed_tools) ? 'allowlist' : 'inherit'; toolsText.value = value.execution?.allowed_tools?.join(', ') || '';
  } catch { resetInputs(); parseError.value = true; enabled.value = false; }
}, { immediate: true });
</script>

<style scoped>
.execution-settings { padding: 18px; border: 1px solid var(--border-1, #3f3f46); border-radius: 12px; background: var(--bg-1, #18181b); }
.heading { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; flex-wrap: wrap; }
h4 { margin: 0; font-size: 15px; color: var(--theme-text-primary, #f4f4f5); }
p { margin: 7px 0 0; }
.heading p, .hint { color: var(--theme-text-muted, #a1a1aa); font-size: 12px; line-height: 1.65; }
.enable { display: flex; align-items: center; gap: 7px; font-size: 13px; color: var(--theme-text-primary, #e4e4e7); cursor: pointer; }
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
