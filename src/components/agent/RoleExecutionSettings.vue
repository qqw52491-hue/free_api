<template>
  <section class="execution-settings" aria-label="默认执行组合">
    <div class="heading">
      <div><h4>默认执行组合</h4><p>为角色选择常用的执行方式。保存后用于新派发的子任务。</p></div>
      <label class="enable"><input v-model="enabled" type="checkbox" @change="publish(true)" />使用角色默认组合</label>
    </div>
    <p v-if="parseError" class="error">原执行配置无法解析，请选择一个推荐组合重新设置。</p>
    <div class="presets">
      <button v-for="preset in presets" :key="preset.id" type="button" :aria-pressed="matches(preset)" :class="{ selected: matches(preset) }" @click="applyPreset(preset)">
        <strong>{{ preset.label }}<span v-if="preset.id === 'general'" class="recommended">推荐</span></strong><span>{{ preset.description }}</span>
      </button>
    </div>
    <template v-if="enabled">
      <div class="controls">
        <label>执行方式<select v-model="profile" @change="changeProfile"><option value="task">通用子任务</option><option value="worker">调查助手</option><option value="writer">实施助手</option><option value="reviewer">专用审查</option></select></label>
        <label>方案安排<select v-model="planMode" :disabled="specialized" @change="publish()"><option value="auto">自动判断</option><option value="cross_review">先讨论并审查方案</option><option value="plan_approval">方案需要人工批准</option></select></label>
        <label>结果如何结束<select v-model="completion" :disabled="specialized" @change="publish()"><option value="delivery">完整交付，按任务验证和审查</option><option value="handoff">交回结果，由父任务集成验收</option></select></label>
      </div>
      <p class="hint">{{ !specialized ? '普通角色使用同一通用执行器，共享可用 Skill、MCP 和工具；自动模式仍按任务需要安排方案。' : '正式审查或显式隔离补丁使用专用结果契约；完整流程选择共享工作区。' }}</p>
      <label v-if="profile === 'writer'" class="tool-mode">工作区方式<select v-model="workspace" @change="changeProfile"><option value="shared">共享父工作区，直接实施</option><option value="isolated">隔离补丁，审阅后 apply</option></select></label>
      <StageSettingsFields v-if="!specialized" v-model="stages" :approval="planMode === 'plan_approval'" @update:model-value="publish()" />
      <p class="hint">结果交接子任务默认关闭重复环节；显式选择方案模式时沿用该模式。自动不代表关闭。</p>
      <details>
        <summary>工具权限</summary>
        <label class="tool-mode">权限范围<select v-model="toolMode" @change="publish()"><option value="inherit">继承父任务可用工具</option><option value="allowlist">仅允许指定工具</option></select></label>
        <label v-if="toolMode === 'allowlist'" class="tool-list">允许的工具或动作<textarea v-model="toolsText" rows="3" placeholder="例如：read_file, list_dir, send_message&#10;通用任务也可填写 local_file.read" @input="publish()" /></label>
        <p class="hint">使用逗号或换行分隔，填写小写名称。列表为空时禁用业务工具；交回结果、方案流程、人工求助和只读通信查询仍可用。派发参数可进一步缩小范围，父任务、角色工具上限与资源授权始终生效。</p>
      </details>
      <div class="summary" aria-live="polite">{{ summary }}</div>
      <p class="hint">每个子任务默认保存独立检查点，无需另行开启。流程可在派发时显式调整；工具权限按本配置限制。</p>
    </template>
    <p v-else class="hint">沿用各派发入口的现有默认值。也可以直接点击上面的推荐组合。</p>
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
const props = defineProps<{ modelValue: string }>();
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
function changeProfile() {
  if (profile.value !== 'writer') workspace.value = 'shared';
  if (specialized.value) { completion.value = 'handoff'; planMode.value = 'auto'; }
  if (planMode.value === 'plan_approval' && stages.value.plan === false) stages.value = { ...stages.value, plan: true };
  publish(true);
}
function applyPreset(preset: Preset) {
  enabled.value = true; stages.value = {}; workspace.value = 'shared'; profile.value = preset.profile; planMode.value = preset.planMode; completion.value = preset.completion;
  toolMode.value = preset.tools ? 'allowlist' : 'inherit'; toolsText.value = preset.tools?.join(', ') || ''; publish(true);
}
function matches(preset: Preset) { return Object.keys(stages.value).length === 0 && enabled.value && workspace.value === 'shared' && profile.value === preset.profile && planMode.value === preset.planMode && completion.value === preset.completion && toolMode.value === 'inherit'; }
watch(() => props.modelValue, raw => {
  if (raw === lastEmitted) return;
  if (!raw) { enabled.value = false; parseError.value = false; return; }
  try {
    const value = JSON.parse(raw);
    if (!['task', 'worker', 'writer', 'reviewer'].includes(value.profile)) throw new Error('profile');
    enabled.value = true; parseError.value = false; profile.value = value.profile; workspace.value = value.execution?.workspace || 'shared';
    stages.value = value.execution?.stages || {};
    planMode.value = value.execution?.workflow?.plan_mode || 'auto'; completion.value = value.execution?.workflow?.completion || (profile.value === 'task' ? 'delivery' : 'handoff');
    toolMode.value = Array.isArray(value.execution?.allowed_tools) ? 'allowlist' : 'inherit'; toolsText.value = value.execution?.allowed_tools?.join(', ') || '';
  } catch { parseError.value = true; enabled.value = false; }
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
