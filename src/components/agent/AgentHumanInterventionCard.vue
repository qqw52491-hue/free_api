<template>
  <section class="human-intervention-card" aria-label="需要用户协助与确认">
    <div class="card-top-row">
      <div class="card-badge">
        <span class="pulse-dot"></span>
        <span>需要人类协助</span>
      </div>
      <span class="card-prompt-type">{{ formTypeLabel }}</span>
    </div>

    <h3 class="intervention-title">{{ formTitle }}</h3>
    <p v-if="formDescription" class="intervention-desc">{{ formDescription }}</p>

    <!-- 1. 二次确认 (Confirm) -->
    <div v-if="formKind === 'confirm'" class="options-container confirm-box">
      <button
        type="button"
        class="choice-btn confirm-btn"
        :class="{ active: confirmValue === true }"
        @click="confirmValue = true"
      >
        <span class="choice-icon">✓</span>
        <span>确认执行</span>
      </button>
      <button
        type="button"
        class="choice-btn cancel-btn"
        :class="{ active: confirmValue === false }"
        @click="confirmValue = false"
      >
        <span class="choice-icon">✕</span>
        <span>取消并终止</span>
      </button>
    </div>

    <!-- 2. 单选 (Single Select) -->
    <div v-else-if="formKind === 'single_select'" class="options-container list-box">
      <div
        v-for="opt in formOptions"
        :key="opt.id"
        class="option-item"
        :class="{ selected: singleSelectedId === opt.id }"
        @click="singleSelectedId = opt.id"
      >
        <div class="radio-circle">
          <div class="radio-inner" v-if="singleSelectedId === opt.id"></div>
        </div>
        <div class="option-text-wrap">
          <span class="option-label">{{ opt.label }}</span>
          <small v-if="opt.description" class="option-sub">{{ opt.description }}</small>
        </div>
      </div>
    </div>

    <!-- 3. 多选 (Multi Select) -->
    <div v-else-if="formKind === 'multi_select'" class="options-container list-box">
      <div class="multi-select-toolbar">
        <span>已选 {{ multiSelectedIds.length }}/{{ formOptions.length }} 项</span>
        <div class="toolbar-actions">
          <button type="button" class="mini-btn" @click="selectAll">全选</button>
          <button type="button" class="mini-btn" @click="clearAll">清空</button>
        </div>
      </div>
      <div
        v-for="opt in formOptions"
        :key="opt.id"
        class="option-item"
        :class="{ selected: multiSelectedIds.includes(opt.id) }"
        @click="toggleMultiOption(opt.id)"
      >
        <div class="checkbox-box" :class="{ checked: multiSelectedIds.includes(opt.id) }">
          <svg v-if="multiSelectedIds.includes(opt.id)" viewBox="0 0 24 24"><polyline points="20 6 9 17 4 12"/></svg>
        </div>
        <div class="option-text-wrap">
          <span class="option-label">{{ opt.label }}</span>
          <small v-if="opt.description" class="option-sub">{{ opt.description }}</small>
        </div>
      </div>
    </div>

    <!-- 4. 本地文件/文件夹选择器 (File Picker) -->
    <div v-else-if="formKind === 'file_picker'" class="options-container file-box">
      <div class="file-picker-trigger" @click="handlePickFile">
        <span class="picker-icon">📁</span>
        <div class="picker-hint">
          <strong>点击选择本地文件或目录</strong>
          <span>支持 CSV, JSON, TXT, Excel, 代码文件等</span>
        </div>
        <button type="button" class="btn-picker-action">浏览文件…</button>
      </div>
      <div v-if="selectedFilePaths.length" class="selected-files-list">
        <div v-for="(path, idx) in selectedFilePaths" :key="idx" class="file-chip">
          <span class="file-chip-name">{{ path }}</span>
          <button type="button" class="file-chip-del" @click.stop="removeFile(idx)">×</button>
        </div>
      </div>
    </div>

    <!-- 5. 极致灵活自定义 HTML 交互微应用 (Custom HTML Mini-App) -->
    <div v-else-if="formKind === 'custom_html' || customHtmlCode" class="options-container custom-html-box">
      <HtmlPreviewBlock
        :code="customHtmlCode"
        :height="360"
        @agent-submit="handleCustomHtmlSubmit"
        @agent-cancel="handleCustomHtmlCancel"
      />
    </div>

    <!-- 6. 兜底/附加补充输入框 (Write-in input) -->
    <div v-if="formKind !== 'custom_html' && !customHtmlCode" class="write-in-wrap">
      <label class="write-in-label">
        <span>补充说明 / 自定义要求（可选）:</span>
      </label>
      <input
        v-model="customWriteInText"
        type="text"
        class="write-in-input"
        placeholder="如果以上选项不符合，可在此输入补充意见后提交…"
        @keydown.enter.prevent="handleFormSubmit"
      />
    </div>

    <!-- 提交动作栏 (非自定义 HTML 时展示标准提交按钮) -->
    <div v-if="formKind !== 'custom_html' && !customHtmlCode" class="intervention-actions">
      <button
        type="button"
        class="submit-intervention-btn"
        :disabled="!canSubmit || isSending"
        @click="handleFormSubmit"
      >
        <span>{{ isSending ? '提交中…' : '提交反馈' }}</span>
        <svg viewBox="0 0 24 24"><path d="M5 12h14M12 5l7 7-7 7"/></svg>
      </button>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed, ref } from "vue";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import type { AgentHumanForm } from "./types";
import HtmlPreviewBlock from "../charts/HtmlPreviewBlock.vue";

const props = defineProps<{
  humanForm: AgentHumanForm | null;
  pauseReason?: string;
  isSending?: boolean;
}>();

const emit = defineEmits<{
  submit: [answer: string];
  cancel: [];
}>();

const formKind = computed(() => props.humanForm?.kind || (props.humanForm?.html ? "custom_html" : "single_select"));
const formTitle = computed(() => props.humanForm?.title || props.pauseReason || "请提供反馈或确认下一步");
const formDescription = computed(() => props.humanForm?.description || "");
const formOptions = computed(() => props.humanForm?.options || []);
const customHtmlCode = computed(() => props.humanForm?.html || (props.humanForm?.kind === "custom_html" ? props.humanForm.description || "" : ""));

const formTypeLabel = computed(() => {
  switch (formKind.value) {
    case "confirm": return "操作确认";
    case "single_select": return "单项选择";
    case "multi_select": return "多项选择";
    case "file_picker": return "文件上传";
    case "custom_html": return "自定义交互应用";
    default: return "问答输入";
  }
});

// 状态
const confirmValue = ref<boolean | null>(
  typeof props.humanForm?.defaultValue === "boolean" ? props.humanForm.defaultValue : true
);
const singleSelectedId = ref<string>(
  typeof props.humanForm?.defaultValue === "string"
    ? props.humanForm.defaultValue
    : formOptions.value[0]?.id || ""
);
const multiSelectedIds = ref<string[]>(
  Array.isArray(props.humanForm?.defaultValue) ? props.humanForm.defaultValue : []
);
const selectedFilePaths = ref<string[]>([]);
const customWriteInText = ref("");

function toggleMultiOption(id: string) {
  const index = multiSelectedIds.value.indexOf(id);
  if (index >= 0) {
    multiSelectedIds.value.splice(index, 1);
  } else {
    multiSelectedIds.value.push(id);
  }
}

function selectAll() {
  multiSelectedIds.value = formOptions.value.map((o) => o.id);
}

function clearAll() {
  multiSelectedIds.value = [];
}

async function handlePickFile() {
  try {
    const selected = await openDialog({
      multiple: true,
      directory: false,
      title: "请选择要提供给 Agent 的文件",
    });
    if (selected) {
      if (Array.isArray(selected)) {
        selectedFilePaths.value = [...new Set([...selectedFilePaths.value, ...selected])];
      } else {
        selectedFilePaths.value = [...new Set([...selectedFilePaths.value, selected])];
      }
    }
  } catch (e) {
    console.error("打开文件选择器失败:", e);
  }
}

function removeFile(index: number) {
  selectedFilePaths.value.splice(index, 1);
}

const canSubmit = computed(() => {
  if (customWriteInText.value.trim()) return true;
  if (formKind.value === "confirm") return confirmValue.value !== null;
  if (formKind.value === "single_select") return !!singleSelectedId.value;
  if (formKind.value === "multi_select") return multiSelectedIds.value.length > 0;
  if (formKind.value === "file_picker") return selectedFilePaths.value.length > 0;
  return true;
});

function handleFormSubmit() {
  if (!canSubmit.value || props.isSending) return;

  const parts: string[] = [];

  if (formKind.value === "confirm") {
    parts.push(`【确认状态】: ${confirmValue.value ? "确认执行" : "取消操作"}`);
  } else if (formKind.value === "single_select") {
    const matched = formOptions.value.find((o) => o.id === singleSelectedId.value);
    parts.push(`【选择项】: ${matched?.label || singleSelectedId.value}`);
  } else if (formKind.value === "multi_select") {
    const labels = multiSelectedIds.value.map((id) => {
      const opt = formOptions.value.find((o) => o.id === id);
      return opt?.label || id;
    });
    parts.push(`【已选项】: ${labels.join(", ")}`);
  } else if (formKind.value === "file_picker") {
    parts.push(`【已选文件路径】:\n${selectedFilePaths.value.map((p) => `- ${p}`).join("\n")}`);
  }

  if (customWriteInText.value.trim()) {
    parts.push(`【补充说明】: ${customWriteInText.value.trim()}`);
  }

  emit("submit", parts.join("\n"));
}

function handleCustomHtmlSubmit(data: any) {
  let text = "";
  if (typeof data === "string") {
    text = data;
  } else {
    try {
      text = JSON.stringify(data, null, 2);
    } catch {
      text = String(data);
    }
  }
  emit("submit", text);
}

function handleCustomHtmlCancel(reason: string) {
  emit("submit", `【用户取消了操作】: ${reason}`);
}
</script>

<style scoped>
.human-intervention-card {
  margin: 12px 0;
  padding: 16px 18px;
  border: 1px solid rgba(251, 191, 36, 0.3);
  border-radius: 12px;
  background: #18181b;
  box-shadow: 0 4px 24px rgba(0, 0, 0, 0.4);
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
}

.card-top-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 10px;
}

.card-badge {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 2px 8px;
  border-radius: 99px;
  background: rgba(251, 191, 36, 0.12);
  border: 1px solid rgba(251, 191, 36, 0.25);
  color: #fbbf24;
  font-size: 11px;
  font-weight: 600;
}

.pulse-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: #fbbf24;
  box-shadow: 0 0 8px #fbbf24;
}

.card-prompt-type {
  color: #8e8e93;
  font-size: 11px;
}

.intervention-title {
  margin: 0 0 4px 0;
  color: #f4f4f5;
  font-size: 14.5px;
  font-weight: 650;
  line-height: 1.4;
}

.intervention-desc {
  margin: 0 0 12px 0;
  color: #a1a1aa;
  font-size: 12.5px;
  line-height: 1.5;
}

.options-container {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-bottom: 12px;
}

/* 确认按钮组 */
.confirm-box {
  flex-direction: row;
  gap: 10px;
}

.choice-btn {
  flex: 1;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  padding: 10px 16px;
  border-radius: 8px;
  border: 1px solid rgba(255, 255, 255, 0.1);
  background: #1f1f23;
  color: #a1a1aa;
  font-size: 13px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.15s;
}

.confirm-btn.active {
  background: rgba(46, 213, 115, 0.15);
  border-color: #2ed573;
  color: #2ed573;
}

.cancel-btn.active {
  background: rgba(255, 71, 87, 0.15);
  border-color: #ff4757;
  color: #ff4757;
}

/* 选项列表 */
.list-box {
  gap: 6px;
}

.multi-select-toolbar {
  display: flex;
  justify-content: space-between;
  align-items: center;
  color: #8e8e93;
  font-size: 11.5px;
  margin-bottom: 2px;
}

.mini-btn {
  border: 0;
  color: #a1a1aa;
  background: transparent;
  cursor: pointer;
  padding: 2px 6px;
  font-size: 11.5px;
  border-radius: 4px;
}
.mini-btn:hover { color: #fff; background: rgba(255, 255, 255, 0.08); }

.option-item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 12px;
  border-radius: 8px;
  border: 1px solid rgba(255, 255, 255, 0.08);
  background: #1f1f23;
  cursor: pointer;
  transition: all 0.12s;
}

.option-item:hover {
  background: #27272a;
  border-color: rgba(255, 255, 255, 0.15);
}

.option-item.selected {
  background: rgba(108, 99, 255, 0.12);
  border-color: var(--accent);
}

.radio-circle {
  width: 16px;
  height: 16px;
  border-radius: 50%;
  border: 1px solid rgba(255, 255, 255, 0.3);
  display: grid;
  place-items: center;
}
.option-item.selected .radio-circle {
  border-color: var(--accent);
}
.radio-inner {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--accent);
}

.checkbox-box {
  width: 16px;
  height: 16px;
  border-radius: 4px;
  border: 1px solid rgba(255, 255, 255, 0.3);
  display: grid;
  place-items: center;
}
.checkbox-box.checked {
  background: var(--accent);
  border-color: var(--accent);
}
.checkbox-box svg {
  width: 11px;
  height: 11px;
  stroke: #fff;
  stroke-width: 2.5;
  fill: none;
}

.option-text-wrap {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.option-label {
  color: #f4f4f5;
  font-size: 13px;
  font-weight: 550;
}
.option-sub {
  color: #8e8e93;
  font-size: 11.5px;
}

/* 文件选择器 */
.file-picker-trigger {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px 14px;
  border: 1px dashed rgba(255, 255, 255, 0.2);
  border-radius: 8px;
  background: #141416;
  cursor: pointer;
  transition: all 0.15s;
}
.file-picker-trigger:hover {
  border-color: var(--accent);
  background: rgba(108, 99, 255, 0.04);
}
.picker-icon { font-size: 20px; }
.picker-hint { flex: 1; display: flex; flex-direction: column; gap: 2px; }
.picker-hint strong { color: #f4f4f5; font-size: 12.5px; }
.picker-hint span { color: #71717a; font-size: 11px; }
.btn-picker-action {
  border: 1px solid rgba(255, 255, 255, 0.15);
  border-radius: 6px;
  padding: 5px 12px;
  color: #f4f4f5;
  background: #27272a;
  font-size: 12px;
  cursor: pointer;
}

.selected-files-list {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin-top: 6px;
}
.file-chip {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 3px 8px;
  background: #1f1f23;
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 6px;
  font-size: 11.5px;
  color: #e4e4e7;
}
.file-chip-name {
  max-width: 320px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.file-chip-del {
  border: 0;
  background: transparent;
  color: #71717a;
  cursor: pointer;
  font-size: 14px;
  line-height: 1;
}
.file-chip-del:hover { color: #ff4757; }

/* 自定义补充输入 */
.write-in-wrap {
  margin-top: 10px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.write-in-label {
  color: #8e8e93;
  font-size: 11px;
}
.write-in-input {
  width: 100%;
  padding: 7px 10px;
  border-radius: 6px;
  border: 1px solid rgba(255, 255, 255, 0.1);
  background: #121214;
  color: #f4f4f5;
  font-size: 12.5px;
  outline: none;
  transition: border-color 0.15s;
}
.write-in-input:focus {
  border-color: rgba(255, 255, 255, 0.3);
}

/* 提交按钮 */
.intervention-actions {
  display: flex;
  justify-content: flex-end;
  margin-top: 14px;
}

.submit-intervention-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 6px 16px;
  border-radius: 6px;
  border: 0;
  background: #f4f4f5;
  color: #18181b;
  font-size: 12.5px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.15s;
}
.submit-intervention-btn svg {
  width: 14px;
  height: 14px;
  stroke: currentColor;
  stroke-width: 2;
  fill: none;
}
.submit-intervention-btn:hover:not(:disabled) {
  background: #ffffff;
  transform: translateY(-1px);
}
.submit-intervention-btn:disabled {
  opacity: 0.45;
  cursor: not-allowed;
}
</style>
