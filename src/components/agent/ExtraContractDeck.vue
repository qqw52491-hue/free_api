<template>
  <div v-if="hasContent" class="extra-contract-deck">
    <div class="extra-deck-header">
      <span class="extra-deck-icon">🏷️</span>
      <strong class="extra-deck-title">角色特化交付契约 (Custom Schema)</strong>
      <span v-if="activeFields.length > 0" class="extra-deck-count">
        {{ activeFields.length }} 项契约规范
      </span>
      <span v-else class="extra-deck-count">
        {{ dynamicEntries.length }} 项扩展产物
      </span>
    </div>

    <!-- 1. 按 Schema 遍历点货：合同驱动，按类型分流呈现 -->
    <div v-if="activeFields.length > 0" class="contract-fields-grid">
      <div
        v-for="field in activeFields"
        :key="field.name"
        class="contract-field-row"
      >
        <div class="field-meta">
          <span class="field-name-badge">
            <span class="field-type-pill">{{ field.field_type }}</span>
            <span class="field-name-text">{{ field.name }}</span>
            <span v-if="field.required" class="field-required-star" title="必填契约字段">*</span>
          </span>
          <span v-if="field.description" class="field-desc-text" :title="field.description">
            {{ field.description }}
          </span>
        </div>

        <div class="field-value-cell">
          <!-- 字符串标量：显示为徽章或格式化文本 -->
          <template v-if="field.field_type === 'string'">
            <span
              v-if="hasValue(field.name)"
              class="field-val-badge"
              :class="getBadgeClass(getFieldValue(field.name))"
            >
              {{ String(getFieldValue(field.name)) }}
            </span>
            <span v-else class="val-missing">— 未提供 —</span>
          </template>

          <!-- 字符串数组：渲染为标签组 (Chips) -->
          <template v-else-if="field.field_type === 'string[]'">
            <div v-if="isArrayWithItems(field.name)" class="field-chips-group">
              <span
                v-for="(item, idx) in getArrayItems(field.name)"
                :key="idx"
                class="field-chip-tag"
              >
                {{ formatHistoricalVal(item) }}
              </span>
            </div>
            <span v-else-if="hasValue(field.name)" class="val-empty-arr">— 空数组 [] —</span>
            <span v-else class="val-missing">— 未提供 —</span>
          </template>

          <!-- 数字标量：显示为指标值 -->
          <template v-else-if="field.field_type === 'number'">
            <span v-if="hasValue(field.name)" class="field-number-metric">
              {{ Number(getFieldValue(field.name)).toLocaleString() }}
            </span>
            <span v-else class="val-missing">— 未提供 —</span>
          </template>

          <!-- 布尔标量：显示为状态标签 -->
          <template v-else-if="field.field_type === 'boolean'">
            <span
              v-if="hasValue(field.name)"
              class="field-bool-pill"
              :class="getFieldValue(field.name) ? 'bool-true' : 'bool-false'"
            >
              {{ getFieldValue(field.name) ? '✔ 是 (True)' : '✕ 否 (False)' }}
            </span>
            <span v-else class="val-missing">— 未提供 —</span>
          </template>
        </div>
      </div>
    </div>

    <!-- 2. 无 Schema 时的自适应遍历兜底 (保证历史无 Schema 运行的兼容性) -->
    <div v-else-if="dynamicEntries.length > 0" class="contract-fields-grid fallback-mode">
      <div
        v-for="[key, val] in dynamicEntries"
        :key="key"
        class="contract-field-row"
      >
        <div class="field-meta">
          <span class="field-name-badge">
            <span class="field-name-text">{{ key }}</span>
          </span>
        </div>
        <div class="field-value-cell">
          <div v-if="Array.isArray(val)" class="field-chips-group">
            <span v-for="(item, idx) in val" :key="idx" class="field-chip-tag">
              {{ formatHistoricalVal(item) }}
            </span>
            <span v-if="val.length === 0" class="val-empty-arr">— 空 [] —</span>
          </div>
          <span v-else-if="typeof val === 'boolean'" class="field-bool-pill" :class="val ? 'bool-true' : 'bool-false'">
            {{ val ? '✔ 是' : '✕ 否' }}
          </span>
          <span v-else-if="typeof val === 'number'" class="field-number-metric">
            {{ val }}
          </span>
          <span v-else class="field-val-badge">
            {{ formatHistoricalVal(val) }}
          </span>
        </div>
      </div>
    </div>

    <!-- 3. 历史字段容忍区：数据库/结果中有、但当前 Schema 已废弃的旧字段，折叠收起 -->
    <details v-if="historicalEntries.length > 0" class="historical-accordion">
      <summary class="historical-summary">
        <span>📜 历史/未声明扩展字段 ({{ historicalEntries.length }})</span>
      </summary>
      <div class="historical-content">
        <div
          v-for="[key, val] in historicalEntries"
          :key="key"
          class="historical-row"
        >
          <code class="historical-key">{{ key }}</code>:
          <span class="historical-val">{{ formatHistoricalVal(val) }}</span>
        </div>
      </div>
    </details>
  </div>
</template>

<script setup lang="ts">
import { computed } from "vue";
import type { ContractFieldDef, ContractSchema } from "./types";

const props = defineProps<{
  extra?: Record<string, any>;
  schema?: ContractSchema;
}>();

const activeFields = computed<ContractFieldDef[]>(() => {
  return props.schema?.fields || [];
});

const declaredNameSet = computed<Set<string>>(() => {
  return new Set(activeFields.value.map((f) => f.name));
});

const dynamicEntries = computed<[string, any][]>(() => {
  if (!props.extra) return [];
  return Object.entries(props.extra);
});

const historicalEntries = computed<[string, any][]>(() => {
  if (!props.extra || activeFields.value.length === 0) return [];
  const decl = declaredNameSet.value;
  return Object.entries(props.extra).filter(([k]) => !decl.has(k));
});

const hasContent = computed<boolean>(() => {
  if (activeFields.value.length > 0) return true;
  return !!(props.extra && Object.keys(props.extra).length > 0);
});

function hasValue(name: string): boolean {
  if (!props.extra) return false;
  const val = props.extra[name];
  return val !== undefined && val !== null;
}

function getFieldValue(name: string): any {
  return props.extra ? props.extra[name] : undefined;
}

function isArrayWithItems(name: string): boolean {
  if (!props.extra) return false;
  const val = props.extra[name];
  return Array.isArray(val) && val.length > 0;
}

function getArrayItems(name: string): any[] {
  if (!props.extra) return [];
  const val = props.extra[name];
  return Array.isArray(val) ? val : [];
}

function getBadgeClass(val: any): string {
  const s = String(val).toLowerCase();
  if (s.includes("crit") || s.includes("fatal") || s.includes("fail") || s.includes("block")) {
    return "badge-danger";
  }
  if (s.includes("high") || s.includes("warn")) {
    return "badge-warning";
  }
  if (s.includes("pass") || s.includes("success") || s.includes("low") || s.includes("confirm")) {
    return "badge-success";
  }
  return "badge-info";
}

function formatHistoricalVal(val: any): string {
  if (typeof val === "string") return val;
  try {
    return JSON.stringify(val);
  } catch {
    return String(val);
  }
}
</script>

<style scoped>
.extra-contract-deck {
  background: linear-gradient(135deg, rgba(168, 85, 247, 0.08), rgba(var(--theme-rgb-panel, 24, 24, 32), 0.95));
  border: 1px solid rgba(168, 85, 247, 0.28);
  border-radius: 8px;
  padding: 8px 10px;
  display: flex;
  flex-direction: column;
  gap: 8px;
  box-shadow: 0 4px 14px rgba(0, 0, 0, 0.28);
}

.extra-deck-header {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 10px;
}

.extra-deck-icon {
  font-size: 13px;
}

.extra-deck-title {
  color: var(--theme-status-accent, #c084fc);
  font-weight: 700;
  letter-spacing: 0.2px;
}

.extra-deck-count {
  font-size: 8.5px;
  color: var(--theme-text-secondary, #a1a1aa);
  background: rgba(var(--theme-foreground-rgb), 0.08);
  padding: 1px 5px;
  border-radius: 4px;
}

.contract-fields-grid {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.contract-field-row {
  display: flex;
  flex-direction: column;
  gap: 3px;
  padding: 5px 7px;
  background: rgba(var(--theme-foreground-rgb), 0.03);
  border: 1px solid rgba(var(--theme-foreground-rgb), 0.06);
  border-radius: 6px;
  transition: background 0.15s ease;
}

.contract-field-row:hover {
  background: rgba(var(--theme-foreground-rgb), 0.06);
}

.field-meta {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 6px;
}

.field-name-badge {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 9.5px;
  font-weight: 600;
}

.field-type-pill {
  font-size: 7.5px;
  font-family: monospace;
  padding: 1px 4px;
  border-radius: 3px;
  background: rgba(168, 85, 247, 0.2);
  color: var(--theme-status-accent, #d8b4fe);
  border: 1px solid rgba(168, 85, 247, 0.35);
}

.field-name-text {
  color: var(--theme-text-primary, #f4f4f5);
  font-family: monospace;
}

.field-required-star {
  color: var(--theme-status-danger, #f43f5e);
  font-weight: bold;
}

.field-desc-text {
  font-size: 8.5px;
  color: var(--theme-text-secondary, #a1a1aa);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 60%;
}

.field-value-cell {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 4px;
  padding-left: 2px;
}

.field-val-badge {
  font-size: 9px;
  font-weight: 600;
  padding: 2px 7px;
  border-radius: 4px;
}

.badge-info {
  background: rgba(56, 189, 248, 0.15);
  color: var(--theme-status-info, #38bdf8);
  border: 1px solid rgba(56, 189, 248, 0.3);
}

.badge-success {
  background: rgba(52, 211, 153, 0.15);
  color: var(--theme-status-success, #34d399);
  border: 1px solid rgba(52, 211, 153, 0.3);
}

.badge-warning {
  background: rgba(251, 191, 36, 0.15);
  color: var(--theme-status-warning, #fbbf24);
  border: 1px solid rgba(251, 191, 36, 0.3);
}

.badge-danger {
  background: rgba(244, 63, 94, 0.15);
  color: var(--theme-status-danger, #f43f5e);
  border: 1px solid rgba(244, 63, 94, 0.3);
}

.field-chips-group {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
}

.field-chip-tag {
  font-size: 8.5px;
  font-family: monospace;
  padding: 1px 6px;
  border-radius: 3px;
  background: rgba(99, 102, 241, 0.18);
  color: var(--theme-status-accent, #a5b4fc);
  border: 1px solid rgba(99, 102, 241, 0.35);
  letter-spacing: 0.1px;
}

.field-number-metric {
  font-size: 11px;
  font-weight: 700;
  font-family: monospace;
  color: var(--theme-status-info, #38bdf8);
  background: rgba(56, 189, 248, 0.1);
  padding: 1px 6px;
  border-radius: 4px;
}

.field-bool-pill {
  font-size: 8.5px;
  font-weight: 600;
  padding: 1px 6px;
  border-radius: 3px;
}

.bool-true {
  background: rgba(52, 211, 153, 0.15);
  color: var(--theme-status-success, #34d399);
}

.bool-false {
  background: rgba(244, 63, 94, 0.15);
  color: var(--theme-status-danger, #f43f5e);
}

.val-missing {
  font-size: 8.5px;
  color: var(--theme-text-muted, #71717a);
  font-style: italic;
}

.val-empty-arr {
  font-size: 8.5px;
  color: var(--theme-text-muted, #71717a);
  font-family: monospace;
}

.historical-accordion {
  margin-top: 4px;
  border-top: 1px dashed rgba(var(--theme-foreground-rgb), 0.1);
  padding-top: 6px;
}

.historical-summary {
  font-size: 8.5px;
  color: var(--theme-text-muted, #71717a);
  cursor: pointer;
  user-select: none;
}

.historical-summary:hover {
  color: var(--theme-text-secondary, #a1a1aa);
}

.historical-content {
  margin-top: 4px;
  padding: 4px 6px;
  background: var(--theme-inset-bg, rgba(0, 0, 0, 0.25));
  border-radius: 4px;
  display: flex;
  flex-direction: column;
  gap: 3px;
}

.historical-row {
  font-size: 8.5px;
  color: var(--theme-text-secondary, #a1a1aa);
}

.historical-key {
  color: var(--theme-text-primary, #d4d4d8);
  font-family: monospace;
}

.historical-val {
  color: var(--theme-text-muted, #71717a);
  margin-left: 4px;
}
</style>
