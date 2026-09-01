<template>
  <div class="role-manager-view">
    <header class="view-header">
      <div class="header-main">
        <h2>🎭 Agent 专家工坊 (Role Team)</h2>
        <p class="header-sub">
          配置多 Agent 协作角色卡。每个角色拥有独立的 Persona 提示词与专属模型绑定，主调度 Master 会在复杂任务中自动唤醒对应专家角色。
        </p>
      </div>
      <div class="header-actions">
        <button class="btn primary" @click="startCreate">
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 5v14M5 12h14"/></svg>
          新建专家角色
        </button>
      </div>
    </header>

    <!-- 预设模板快速添加栏 -->
    <section class="preset-templates-bar card">
      <div class="preset-header">
        <span class="preset-title">✨ 推荐专家预设模板 (点击一键载入配置)：</span>
      </div>
      <div class="preset-chips">
        <button
          v-for="tpl in presetTemplates"
          :key="tpl.name"
          type="button"
          class="preset-chip"
          @click="applyPreset(tpl)"
        >
          <span class="tpl-icon">{{ tpl.icon }}</span>
          <span class="tpl-label">{{ tpl.label }}</span>
        </button>
      </div>
    </section>

    <!-- 新建 / 编辑弹窗或表单 -->
    <section v-if="isEditing || isCreating" class="card editor-card">
      <div class="card-title-row">
        <h3>{{ isEditing ? `编辑角色：${form.label || form.name}` : "新建协作角色卡" }}</h3>
        <button class="btn ghost btn-sm" @click="cancelEdit">取消</button>
      </div>

      <div class="form-grid">
        <label class="field">
          <span class="field-label">角色唯一标识 (英文 ID，如 frontend_reviewer) <b class="req">*</b></span>
          <input
            v-model="form.name"
            type="text"
            placeholder="例如: frontend_reviewer"
            :disabled="isEditing"
          />
        </label>

        <label class="field">
          <span class="field-label">展示名称 (中文标签) <b class="req">*</b></span>
          <input v-model="form.label" type="text" placeholder="例如: 🎨 前端视觉与交互审查官" />
        </label>

        <div class="field">
          <span class="field-label">绑定大模型 (可选，留空跟随全局路由)</span>
          <ModelSelectDropdown
            v-model="form.model_id"
            :models="models"
            placeholder="跟随全局自动路由"
            size="md"
          />
        </div>

        <label class="field">
          <span class="field-label">验证测试命令 (可选，如 npm test / cargo check)</span>
          <input
            v-model="form.default_test_command"
            type="text"
            placeholder="例如: cargo check 或 npm test"
          />
        </label>

        <label class="field">
          <span class="field-label">角色并发上限 (Max Concurrency)</span>
          <input
            v-model.number="form.max_concurrency"
            type="number"
            min="0"
            max="20"
            placeholder="0 = 跟随全局默认"
          />
        </label>
      </div>

      <div class="field wide">
        <span class="field-label">允许挂载的 Child 类型 (Allowed Kinds) <span class="field-hint">（留空则不限，均可挂载）</span></span>
        <div class="kind-chips">
          <button
            v-for="opt in kindOptions"
            :key="opt.value"
            type="button"
            class="kind-chip"
            :class="{ selected: form.allowed_kinds.includes(opt.value) }"
            @click="toggleKind(opt.value)"
          >
            {{ opt.label }}
          </button>
        </div>
      </div>

      <label class="field wide">
        <span class="field-label">强制物理围栏 (Hard Scope) <span class="field-hint">（超出此范围的写入/读取将被 Rust 宿主层硬拦截，例如: src/, package.json；留空不限）</span></span>
        <input
          v-model="form.hard_scope"
          type="text"
          placeholder="例如: src/, package.json"
        />
      </label>

      <label class="field wide">
        <span class="field-label">适用范围提示 (Scope Hints)</span>
        <input
          v-model="form.scope_hints"
          type="text"
          placeholder="例如: 涉及 Vue/CSS 样式调整、响应式布局及用户交互时优先使用"
        />
      </label>

      <label class="field wide">
        <span class="field-label">专属 Persona 提示词 (定义专家的职责、挑刺准则与打分标准) <b class="req">*</b></span>
        <textarea
          v-model="form.persona_prompt"
          rows="6"
          placeholder="例如: 你是一位资深前端架构师与 UI 评审专家。重点审查 UI 配色、交互过渡、响应式适配度与代码优雅性。请给出结构化打分与改进项..."
        ></textarea>
      </label>

      <div class="action-row">
        <button class="btn primary" :disabled="saving" @click="saveRoleCard">
          {{ saving ? "保存中…" : isEditing ? "保存修改" : "确认创建角色" }}
        </button>
        <span v-if="feedbackMsg" class="feedback-hint" :class="{ error: feedbackIsError }">
          {{ feedbackMsg }}
        </span>
      </div>
    </section>

    <!-- 已有角色卡片网格列表 -->
    <section class="roles-grid-section">
      <div v-if="loading" class="loading-state">
        <div class="spinner"></div>
        <span>正在加载专家角色列表…</span>
      </div>

      <div v-else-if="!cards.length" class="empty-card card">
        <div class="empty-icon">🎭</div>
        <h3>暂未创建任何专家角色</h3>
        <p>可以通过上方的一键预设模板快速生成，或点击右上角「新建专家角色」。</p>
      </div>

      <div v-else class="cards-grid">
        <div
          v-for="card in cards"
          :key="card.name"
          class="role-card-item"
          :class="{ disabled: !card.enabled }"
        >
          <div class="role-card-header">
            <div class="role-name-wrap">
              <span class="role-avatar-icon">{{ extractEmoji(card.label) || '🤖' }}</span>
              <div class="role-title-box">
                <strong class="role-label">{{ card.label }}</strong>
                <span class="role-slug-name">@{{ card.name }}</span>
              </div>
            </div>
            <div class="role-status-toggle">
              <button
                type="button"
                class="switch-pill"
                :class="{ on: card.enabled }"
                :title="card.enabled ? '点击禁用该角色' : '点击启用该角色'"
                @click="toggleRole(card)"
              >
                {{ card.enabled ? '已启用' : '已停用' }}
              </button>
            </div>
          </div>

          <div class="role-meta-row">
            <div class="meta-tag model-tag" :title="getModelName(card.model_id)">
              <span class="tag-icon">🧠</span>
              <span class="tag-text">{{ getModelName(card.model_id) }}</span>
            </div>
            <div v-if="card.default_test_command" class="meta-tag cmd-tag">
              <span class="tag-icon">🧪</span>
              <span class="tag-text">{{ card.default_test_command }}</span>
            </div>
            <div v-if="card.allowed_kinds" class="meta-tag kind-tag" :title="`允许挂载类型: ${card.allowed_kinds}`">
              <span class="tag-icon">🏷️</span>
              <span class="tag-text">{{ card.allowed_kinds }}</span>
            </div>
            <div v-if="card.max_concurrency > 0" class="meta-tag concurrency-tag" :title="`并发上限: ${card.max_concurrency}`">
              <span class="tag-icon">⚡</span>
              <span class="tag-text">并发 {{ card.max_concurrency }}</span>
            </div>
          </div>

          <p v-if="card.scope_hints" class="role-scope-hint" :title="card.scope_hints">
            🎯 {{ card.scope_hints }}
          </p>

          <p v-if="card.hard_scope" class="role-hard-scope" :title="`强制物理围栏: ${card.hard_scope}`">
            🚧 围栏: {{ card.hard_scope }}
          </p>

          <div class="role-prompt-preview">
            <span class="preview-label">Persona 核心指引:</span>
            <p class="prompt-text">{{ card.persona_prompt }}</p>
          </div>

          <div class="role-card-actions">
            <button class="btn btn-xs ghost" @click="editCard(card)">
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 20h9M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4L16.5 3.5z"/></svg>
              编辑
            </button>
            <button class="btn btn-xs danger ghost" @click="deleteCard(card)">
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M3 6h18M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></svg>
              删除
            </button>
          </div>
        </div>
      </div>
    </section>
  </div>
</template>

<script setup lang="ts">
import { ref, reactive, onMounted } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import ModelSelectDropdown from '../components/common/ModelSelectDropdown.vue';

interface RoleCard {
  id: string;
  name: string;
  label: string;
  model_id: string;
  persona_prompt: string;
  scope_hints: string;
  default_test_command: string;
  allowed_kinds: string;
  hard_scope: string;
  max_concurrency: number;
  enabled: boolean;
}

interface ModelItem {
  id: string;
  name: string;
  platform_id: string;
  platform_name?: string;
}

const kindOptions = [
  { value: 'writer', label: '✍️ 代码修改专家 (Writer)' },
  { value: 'worker', label: '🔍 只读侦察探针 (Worker)' },
  { value: 'reviewer', label: '🛡️ 终态审查专家 (Reviewer)' },
];

const cards = ref<RoleCard[]>([]);
const models = ref<ModelItem[]>([]);
const loading = ref(false);
const saving = ref(false);
const isEditing = ref(false);
const isCreating = ref(false);
const feedbackMsg = ref('');
const feedbackIsError = ref(false);

const form = reactive({
  name: '',
  label: '',
  model_id: '',
  persona_prompt: '',
  scope_hints: '',
  default_test_command: '',
  allowed_kinds: [] as string[],
  hard_scope: '',
  max_concurrency: 0,
  enabled: true,
});

const presetTemplates = [
  {
    icon: '🎨',
    name: 'frontend_ui_reviewer',
    label: '🎨 前端美化与交互审查专家',
    scope_hints: '涉及前端 UI、Vue 组件重构、CSS 样式排版及响应式交互优化',
    persona_prompt: '你是一位资深前端 UI/UX 架构师。请从视觉美观度、配色和谐度、响应式适配度、组件内聚性及微交互流畅度等维度进行严谨审查。评估并输出 0~100 的质量得分以及具体可执行的修改建议。',
    default_test_command: 'npm run build',
    allowed_kinds: 'reviewer',
    hard_scope: '',
    max_concurrency: 0,
  },
  {
    icon: '🛡️',
    name: 'logic_security_reviewer',
    label: '🛡️ 架构逻辑与安全审计官',
    scope_hints: '涉及后端核心状态流转、并发安全、数据库事务与边界异常处理',
    persona_prompt: '你是一位资深系统架构与安全评审专家。重点审查核心逻辑的边界条件、状态一致性、死锁与竞态条件风险、错误处理完整度以及 SQL 注入等安全漏洞。给出严密的诊断打分与加固清单。',
    default_test_command: 'cargo test',
    allowed_kinds: 'reviewer',
    hard_scope: '',
    max_concurrency: 0,
  },
  {
    icon: '🔍',
    name: 'codebase_scout',
    label: '🔍 全局代码侦察探针 (Scout)',
    scope_hints: '用于大范围跨文件代码搜索、调用链路追踪与依赖定位',
    persona_prompt: '你是一个高效率代码调研探针。在只读约束下，使用搜索与检索工具迅速定位关键接口、定义处及所有调用方，提炼关键上下文，为后续开发提供精准线索。',
    default_test_command: '',
    allowed_kinds: 'worker',
    hard_scope: '',
    max_concurrency: 0,
  },
  {
    icon: '📝',
    name: 'doc_writer',
    label: '📝 技术文档与规范编撰专家',
    scope_hints: '涉及 API 文档、架构说明、设计规范及交付报告的编写与美化',
    persona_prompt: '你是一位专业的技术写作专家。善于使用清晰易懂的 Markdown 结构、表格、时序图与警示块（Alerts），将复杂的代码逻辑与交付成果梳理为极其优雅专业的技术文档。',
    default_test_command: '',
    allowed_kinds: 'writer',
    hard_scope: 'docs/',
    max_concurrency: 0,
  },
];



function extractEmoji(label: string) {
  const match = label.match(/(\p{Emoji_Presentation}|\p{Extended_Pictographic})/u);
  return match ? match[0] : '';
}

function getModelName(modelId: string) {
  if (!modelId) return '跟随全局路由';
  const found = models.value.find((m) => m.id === modelId);
  return found ? found.name : modelId.slice(0, 10);
}

function toggleKind(kind: string) {
  const idx = form.allowed_kinds.indexOf(kind);
  if (idx >= 0) {
    form.allowed_kinds.splice(idx, 1);
  } else {
    form.allowed_kinds.push(kind);
  }
}

function startCreate() {
  isEditing.value = false;
  isCreating.value = true;
  Object.assign(form, {
    name: '',
    label: '',
    model_id: '',
    persona_prompt: '',
    scope_hints: '',
    default_test_command: '',
    allowed_kinds: [],
    hard_scope: '',
    max_concurrency: 0,
    enabled: true,
  });
  feedbackMsg.value = '';
}

function cancelEdit() {
  isEditing.value = false;
  isCreating.value = false;
  feedbackMsg.value = '';
}

function applyPreset(tpl: typeof presetTemplates[0]) {
  isCreating.value = true;
  isEditing.value = false;
  form.name = tpl.name;
  form.label = tpl.label;
  form.scope_hints = tpl.scope_hints;
  form.persona_prompt = tpl.persona_prompt;
  form.default_test_command = tpl.default_test_command;
  form.allowed_kinds = tpl.allowed_kinds ? tpl.allowed_kinds.split(',').map((s) => s.trim()).filter(Boolean) : [];
  form.hard_scope = tpl.hard_scope || '';
  form.max_concurrency = tpl.max_concurrency || 0;
  form.enabled = true;
  feedbackMsg.value = `已载入「${tpl.label}」预设，可根据需要调整后点击保存。`;
  feedbackIsError.value = false;
}

function editCard(card: RoleCard) {
  isCreating.value = false;
  isEditing.value = true;
  Object.assign(form, {
    name: card.name,
    label: card.label,
    model_id: card.model_id || '',
    persona_prompt: card.persona_prompt,
    scope_hints: card.scope_hints || '',
    default_test_command: card.default_test_command || '',
    allowed_kinds: card.allowed_kinds ? card.allowed_kinds.split(',').map((s) => s.trim()).filter(Boolean) : [],
    hard_scope: card.hard_scope || '',
    max_concurrency: card.max_concurrency || 0,
    enabled: card.enabled,
  });
  feedbackMsg.value = '';
}

async function loadCards() {
  loading.value = true;
  try {
    cards.value = await invoke<RoleCard[]>('list_role_cards');
  } catch (err: any) {
    feedbackMsg.value = `加载角色失败: ${err?.message || err}`;
    feedbackIsError.value = true;
  } finally {
    loading.value = false;
  }
}

async function loadModels() {
  try {
    const raw = await invoke<any[]>('get_all_models_with_platform');
    models.value = raw.map((item) => ({
      id: item.id,
      name: item.name,
      platform_id: item.platform_id,
      platform_name: item.platform_name || item.platform_id,
    }));
  } catch (err) {
    console.error('加载模型列表失败', err);
  }
}

async function saveRoleCard() {
  if (!form.name.trim() || !form.label.trim() || !form.persona_prompt.trim()) {
    feedbackMsg.value = '请填写角色 ID、展示名称与 Persona 提示词！';
    feedbackIsError.value = true;
    return;
  }

  saving.value = true;
  feedbackMsg.value = '';
  try {
    await invoke('upsert_role_card', {
      card: {
        name: form.name.trim(),
        label: form.label.trim(),
        model_id: form.model_id || '',
        persona_prompt: form.persona_prompt.trim(),
        scope_hints: form.scope_hints.trim(),
        default_test_command: form.default_test_command.trim(),
        allowed_kinds: form.allowed_kinds.join(','),
        hard_scope: form.hard_scope.trim(),
        max_concurrency: Number(form.max_concurrency) || 0,
        enabled: form.enabled,
      },
    });
    feedbackMsg.value = '角色卡已成功保存！';
    feedbackIsError.value = false;
    await loadCards();
    setTimeout(() => {
      cancelEdit();
    }, 1200);
  } catch (err: any) {
    feedbackMsg.value = `保存失败: ${err?.message || err}`;
    feedbackIsError.value = true;
  } finally {
    saving.value = false;
  }
}

async function toggleRole(card: RoleCard) {
  try {
    await invoke('toggle_role_card', { name: card.name });
    card.enabled = !card.enabled;
  } catch (err: any) {
    alert(`切换状态失败: ${err?.message || err}`);
  }
}

async function deleteCard(card: RoleCard) {
  if (!confirm(`确定要删除角色「${card.label}」吗？`)) return;
  try {
    await invoke('delete_role_card', { name: card.name });
    await loadCards();
  } catch (err: any) {
    alert(`删除失败: ${err?.message || err}`);
  }
}

onMounted(() => {
  loadCards();
  loadModels();
});
</script>

<style scoped>
.role-manager-view {
  padding: 24px 32px;
  height: 100%;
  overflow-y: auto;
  color: var(--text-1, #e4e4e7);
  background: var(--bg-0, #09090b);
}

.view-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 20px;
  gap: 16px;
}

.header-main h2 {
  font-size: 20px;
  font-weight: 700;
  margin: 0 0 6px 0;
  color: #fff;
}

.header-sub {
  font-size: 13px;
  color: var(--text-3, #71717a);
  margin: 0;
  line-height: 1.5;
}

.card {
  background: var(--bg-1, #18181b);
  border: 1px solid var(--border-1, #27272a);
  border-radius: 12px;
  padding: 18px 20px;
  margin-bottom: 20px;
}

/* 预设栏 */
.preset-templates-bar {
  display: flex;
  flex-direction: column;
  gap: 10px;
  background: linear-gradient(180deg, rgba(39, 39, 42, 0.4) 0%, rgba(24, 24, 27, 0.6) 100%);
  border-color: rgba(255, 255, 255, 0.08);
}

.preset-title {
  font-size: 12.5px;
  font-weight: 600;
  color: var(--text-2, #a1a1aa);
}

.preset-chips {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.preset-chip {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 6px 12px;
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.1);
  color: #e4e4e7;
  font-size: 12.5px;
  cursor: pointer;
  transition: all 0.15s ease;
}

.preset-chip:hover {
  background: rgba(99, 102, 241, 0.15);
  border-color: rgba(99, 102, 241, 0.4);
  color: #a5b4fc;
  transform: translateY(-1px);
}

/* 表单网格 */
.editor-card {
  border-color: rgba(99, 102, 241, 0.35);
  box-shadow: 0 4px 20px rgba(0, 0, 0, 0.3);
}

.card-title-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 16px;
}

.card-title-row h3 {
  font-size: 15px;
  margin: 0;
  color: #f4f4f5;
}

.form-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
  gap: 14px;
  margin-bottom: 14px;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.field.wide {
  margin-bottom: 14px;
}

.field-label {
  font-size: 12px;
  color: var(--text-3, #a1a1aa);
  font-weight: 500;
}

.req {
  color: #f43f5e;
}

input[type="text"],
select,
textarea {
  background: var(--bg-2, #27272a);
  border: 1px solid var(--border-1, #3f3f46);
  border-radius: 8px;
  color: #f4f4f5;
  padding: 8px 12px;
  font-size: 13px;
  outline: none;
  transition: border-color 0.15s;
  font-family: inherit;
}

input:focus,
select:focus,
textarea:focus {
  border-color: #6366f1;
}

textarea {
  resize: vertical;
  line-height: 1.5;
}

.action-row {
  display: flex;
  align-items: center;
  gap: 12px;
}

.feedback-hint {
  font-size: 13px;
  color: #10b981;
}

.feedback-hint.error {
  color: #f43f5e;
}

/* 角色卡片网格 */
.cards-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
  gap: 16px;
}

.role-card-item {
  background: var(--bg-1, #18181b);
  border: 1px solid var(--border-1, #27272a);
  border-radius: 12px;
  padding: 16px;
  display: flex;
  flex-direction: column;
  gap: 12px;
  transition: all 0.18s ease;
  position: relative;
}

.role-card-item:hover {
  border-color: rgba(255, 255, 255, 0.16);
  transform: translateY(-2px);
  box-shadow: 0 6px 18px rgba(0, 0, 0, 0.25);
}

.role-card-item.disabled {
  opacity: 0.65;
  filter: grayscale(0.2);
}

.role-card-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 8px;
}

.role-name-wrap {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
}

.role-avatar-icon {
  font-size: 24px;
  line-height: 1;
  flex-shrink: 0;
}

.role-title-box {
  display: flex;
  flex-direction: column;
  min-width: 0;
}

.role-label {
  font-size: 14px;
  color: #f4f4f5;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.role-slug-name {
  font-size: 11px;
  color: var(--text-4, #71717a);
  font-family: monospace;
}

.switch-pill {
  font-size: 11px;
  padding: 2px 8px;
  border-radius: 99px;
  cursor: pointer;
  border: 1px solid var(--border-1, #3f3f46);
  background: rgba(255, 255, 255, 0.05);
  color: var(--text-3, #71717a);
  transition: all 0.15s;
}

.switch-pill.on {
  background: rgba(16, 185, 129, 0.12);
  color: #10b981;
  border-color: rgba(16, 185, 129, 0.3);
  font-weight: 600;
}

.role-meta-row {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.meta-tag {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 11.5px;
  padding: 3px 8px;
  border-radius: 6px;
  background: var(--bg-2, #27272a);
  border: 1px solid rgba(255, 255, 255, 0.06);
  color: var(--text-2, #d4d4d8);
  max-width: 100%;
}

.tag-text {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.role-scope-hint {
  font-size: 12px;
  color: #a1a1aa;
  margin: 0;
  line-height: 1.45;
  background: rgba(255, 255, 255, 0.03);
  padding: 6px 8px;
  border-radius: 6px;
}

.role-prompt-preview {
  background: rgba(0, 0, 0, 0.2);
  border-radius: 6px;
  padding: 8px 10px;
  border: 1px solid rgba(255, 255, 255, 0.04);
}

.preview-label {
  font-size: 11px;
  color: #71717a;
  display: block;
  margin-bottom: 4px;
}

.prompt-text {
  font-size: 12px;
  color: #d4d4d8;
  margin: 0;
  line-height: 1.45;
  display: -webkit-box;
  -webkit-line-clamp: 3;
  -webkit-box-orient: vertical;
  overflow: hidden;
}

.role-card-actions {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 6px;
  margin-top: auto;
  padding-top: 8px;
  border-top: 1px solid rgba(255, 255, 255, 0.05);
}

/* 按钮风格 */
.btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 7px 14px;
  border-radius: 8px;
  font-size: 13px;
  font-weight: 500;
  cursor: pointer;
  border: 1px solid transparent;
  background: rgba(255, 255, 255, 0.08);
  color: #f4f4f5;
  transition: all 0.15s;
}

.btn:hover {
  background: rgba(255, 255, 255, 0.12);
}

.btn.primary {
  background: #6366f1;
  color: #fff;
}

.btn.primary:hover {
  background: #4f46e5;
}

.btn.ghost {
  background: transparent;
  border-color: var(--border-1, #3f3f46);
}

.btn.ghost:hover {
  background: rgba(255, 255, 255, 0.06);
}

.btn.danger:hover {
  color: #f43f5e;
  border-color: rgba(244, 63, 94, 0.4);
  background: rgba(244, 63, 94, 0.1);
}

.btn-sm {
  padding: 4px 10px;
  font-size: 12px;
}

.btn-xs {
  padding: 3px 8px;
  font-size: 11.5px;
}

.loading-state,
.empty-card {
  text-align: center;
  padding: 40px;
  color: var(--text-3, #71717a);
}

.empty-icon {
  font-size: 36px;
  margin-bottom: 10px;
}

.field-hint {
  font-size: 11px;
  color: var(--text-3, #71717a);
  font-weight: normal;
  margin-left: 4px;
}

.kind-chips {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-top: 4px;
}

.kind-chip {
  background: var(--bg-2, #27272a);
  border: 1px solid var(--border-1, #3f3f46);
  border-radius: 6px;
  color: var(--text-2, #d4d4d8);
  padding: 6px 12px;
  font-size: 12px;
  cursor: pointer;
  transition: all 0.15s ease;
}

.kind-chip:hover {
  border-color: #6366f1;
  color: #fff;
}

.kind-chip.selected {
  background: rgba(99, 102, 241, 0.15);
  border-color: #6366f1;
  color: #818cf8;
  font-weight: 500;
}

.meta-tag.kind-tag {
  background: rgba(99, 102, 241, 0.12);
  color: #818cf8;
}

.meta-tag.concurrency-tag {
  background: rgba(234, 179, 8, 0.12);
  color: #eab308;
}

.role-hard-scope {
  font-size: 11px;
  color: #f59e0b;
  margin: 6px 0 0 0;
  line-height: 1.4;
  background: rgba(245, 158, 11, 0.08);
  border: 1px dashed rgba(245, 158, 11, 0.25);
  padding: 4px 8px;
  border-radius: 6px;
}
</style>
