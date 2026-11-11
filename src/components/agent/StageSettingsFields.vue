<template>
  <fieldset class="stage-fields" :disabled="disabled">
    <legend>固定环节</legend>
    <label v-for="field in fields" :key="field.key">
      <span>{{ field.label }}</span>
      <select :value="mode(field.key)" :disabled="field.key === 'plan' && approval" @change="change(field.key, ($event.target as HTMLSelectElement).value)">
        <option value="auto">自动 / 沿用默认</option><option value="on">开启</option><option value="off" :disabled="field.key === 'plan' && requiresPlan">关闭</option>
      </select>
    </label>
    <p>关闭后不自动触发。正式方案关闭时方案审查也关闭；人工批准仍要求正式方案。最终审查开启后仍按材料适用条件派审。已有阻塞、权限和明确验证要求继续生效。</p>
    <p v-if="approval && modelValue?.plan_review === true">同时开启人工批准和方案审查：先由 Agent 审方案，再将方案及审查结果交给你批准或打回。</p>
  </fieldset>
</template>
<script setup lang="ts">
import type { StageSettings } from './types';
const props = defineProps<{ modelValue?: StageSettings; disabled?: boolean; approval?: boolean; requiresPlan?: boolean; legacyStageReviewDisabled?: boolean }>();
const emit = defineEmits<{ 'update:modelValue': [value: StageSettings]; 'stage-change': [key: keyof StageSettings, mode: string, value: StageSettings] }>();
const fields: { key: keyof StageSettings; label: string }[] = [
  { key: 'plan', label: '正式方案' }, { key: 'plan_review', label: '方案审查' },
  { key: 'stage_review', label: '阶段审查' }, { key: 'final_review', label: '最终审查' },
];
function mode(key: keyof StageSettings) {
  if (key === 'plan' && props.approval) return 'on';
  if (key === 'stage_review' && props.modelValue?.stage_review == null && props.legacyStageReviewDisabled) return 'off';
  return props.modelValue?.[key] === true ? 'on' : props.modelValue?.[key] === false ? 'off' : 'auto';
}
function change(key: keyof StageSettings, mode: string) {
  if (key === 'plan' && mode === 'off' && (props.approval || props.requiresPlan)) return;
  const value = { ...props.modelValue };
  if (mode === 'auto') delete value[key]; else value[key] = mode === 'on';
  if (key === 'plan' && mode === 'off') value.plan_review = false;
  if (key === 'plan_review' && mode === 'on') value.plan = true;
  emit('update:modelValue', value);
  emit('stage-change', key, mode, value);
}
</script>
<style scoped>
.stage-fields { margin: 14px 0; padding: 12px; border: 1px solid var(--border-1, #7775); border-radius: 8px; display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; }
legend, label { font-size: 12px; color: var(--theme-text-primary); }
label { display: flex; flex-direction: column; gap: 6px; }
select { padding: 7px; border-radius: 6px; background: var(--theme-inset-bg); color: var(--theme-text-primary); border: 1px solid var(--border-1, #7775); }
p { grid-column: 1 / -1; margin: 0; font-size: 12px; line-height: 1.6; color: var(--theme-text-muted); }
</style>
