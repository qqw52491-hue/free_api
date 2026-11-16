<template>
  <fieldset class="stage-fields" :disabled="disabled">
    <legend>固定环节</legend>
    <label v-for="field in fields" :key="field.key">
      <span @click.prevent>{{ field.label }}</span>
      <WorkflowSegmented
        compact
        :model-value="mode(field.key)"
        :disabled="disabled || (field.key === 'plan' && approval)"
        :options="stageOptions(field.key)"
        @change="change(field.key, ($event.target as HTMLSelectElement).value)"
      />
    </label>
    <p>关闭后不自动触发。正式方案关闭时方案审查也关闭；人工批准仍要求正式方案。最终审查开启后仍按材料适用条件派审。已有阻塞、权限和明确验证要求继续生效。</p>
    <p v-if="approval && modelValue?.plan_review === true">同时开启人工批准和方案审查：先由 Agent 审方案，再将方案及审查结果交给你批准或打回。</p>
  </fieldset>
</template>
<script setup lang="ts">
import WorkflowSegmented, { type WorkflowSegmentedOption } from '../common/WorkflowSegmented.vue';
import type { StageSettings } from './types';
const props = defineProps<{ modelValue?: StageSettings; disabled?: boolean; approval?: boolean; requiresPlan?: boolean; legacyStageReviewDisabled?: boolean }>();
const emit = defineEmits<{ 'update:modelValue': [value: StageSettings]; 'stage-change': [key: keyof StageSettings, mode: string, value: StageSettings] }>();
const fields: { key: keyof StageSettings; label: string }[] = [
  { key: 'plan', label: '正式方案' }, { key: 'plan_review', label: '方案审查' },
  { key: 'stage_review', label: '阶段审查' }, { key: 'final_review', label: '最终审查' },
];
const stageOptions = (key: keyof StageSettings): WorkflowSegmentedOption[] => [
  { value: 'auto', label: '自动', optionLabel: '自动 / 沿用默认', title: '自动 / 沿用默认', tone: 'auto' },
  { value: 'on', label: '开启', tone: 'on' },
  { value: 'off', label: '关闭', tone: 'off', disabled: key === 'plan' && props.requiresPlan },
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
.stage-fields {
  margin: 14px 0;
  padding: 14px;
  background: rgba(var(--theme-foreground-rgb, 255, 255, 255), 0.018);
  border: 1px solid rgba(129, 140, 248, 0.16);
  border-radius: 12px;
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px;
}
legend {
  padding: 2px 10px;
  border-radius: 999px;
  background: rgba(99, 102, 241, 0.14);
  border: 1px solid rgba(129, 140, 248, 0.28);
  color: var(--theme-status-accent, #c7d2fe);
  font-size: 11.5px;
  font-weight: 600;
}
label {
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 7px;
  padding: 10px 12px;
  border-radius: 10px;
  background: rgba(var(--theme-foreground-rgb, 255, 255, 255), 0.025);
  border: 1px solid rgba(var(--theme-foreground-rgb, 255, 255, 255), 0.07);
  font-size: 12px;
  font-weight: 500;
  color: var(--theme-text-primary, #f4f4f5);
}
p { grid-column: 1 / -1; margin: 0; font-size: 12px; line-height: 1.6; color: var(--theme-text-muted, #a1a1aa); }
@media (max-width: 520px) {
  .stage-fields { grid-template-columns: 1fr; }
}
</style>
