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

      <!-- 🎯 上下文关注域 (Focus Domains / 系统预装材料) -->
      <div class="field wide focus-domain-section">
        <div class="focus-header">
          <span class="field-label">🎯 上下文关注域 (Focus Domains / 自动装配材料)</span>
          <span class="field-hint">勾选该角色需要的专项材料。派发时系统底层自动并发采集并安全截断注入，主 Agent 无需在提示词中手动粘贴大段代码与日志。</span>
        </div>
        <div class="focus-pills-wrap">
          <button
            v-for="opt in focusDomainOptions"
            :key="opt.value"
            type="button"
            class="focus-pill"
            :class="{ active: form.focus_domains.includes(opt.value) }"
            :title="opt.desc"
            @click="toggleFocusDomain(opt.value)"
          >
            <span class="pill-check">{{ form.focus_domains.includes(opt.value) ? '✓' : '+' }}</span>
            <span>{{ opt.label }}</span>
          </button>
        </div>
      </div>

      <!-- 📥 自定义入参需求约束 (Input Contract Schema / 动态业务上下文) -->
      <div class="field wide input-contract-section">
        <div class="contract-header">
          <div class="contract-title-group">
            <span class="field-label">📥 自定义入参需求约束 (Input Contract Schema)</span>
            <span class="field-hint">声明主 Agent 在向该专家派工时，除了系统自动采集的关注材料外，必须或建议在输入参数中额外显式补充的业务字段（如 business_goal、affected_modules 等）。</span>
          </div>
          <button type="button" class="btn btn-sm btn-input-add" @click="addInputField">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 5v14M5 12h14"/></svg>
            添加入参字段
          </button>
        </div>

        <div v-if="!form.input_fields.length" class="contract-empty">
          <span class="empty-hint">暂无特定入参字段约束（主 Agent 按常规说明自由派发）。点击右上角「+ 添加入参字段」可约束派工必须携带的特定业务要素。</span>
        </div>

        <div v-else class="schema-table-wrap input-table-wrap">
          <div class="schema-table-header">
            <div class="th-col col-idx">#</div>
            <div class="th-col col-name">入参键名 (Key) <b class="th-req">*</b></div>
            <div class="th-col col-type">数据类型 (Type)</div>
            <div class="th-col col-req">约束 (Req)</div>
            <div class="th-col col-desc">入参用途说明 (Description)</div>
            <div class="th-col col-enum">可选枚举范围 (Enum)</div>
            <div class="th-col col-action">操作</div>
          </div>
          <div class="schema-table-body">
            <div
              v-for="(field, fIdx) in form.input_fields"
              :key="fIdx"
              class="schema-table-row input-row"
            >
              <div class="td-col col-idx">
                <span class="row-index">{{ fIdx + 1 }}</span>
              </div>
              <div class="td-col col-name">
                <input
                  v-model="field.name"
                  type="text"
                  class="cell-input mono-input"
                  placeholder="例如: business_goal"
                  @blur="sanitizeFieldName(field)"
                />
              </div>
              <div class="td-col col-type">
                <select v-model="field.field_type" class="cell-select">
                  <option value="string">string (文本)</option>
                  <option value="string[]">string[] (列表)</option>
                  <option value="number">number (数字)</option>
                  <option value="boolean">boolean (布尔)</option>
                </select>
              </div>
              <div class="td-col col-req">
                <button
                  type="button"
                  class="req-toggle-pill"
                  :class="{ active: field.required }"
                  @click="field.required = !field.required"
                >
                  {{ field.required ? '必填' : '选填' }}
                </button>
              </div>
              <div class="td-col col-desc">
                <input
                  v-model="field.description"
                  type="text"
                  class="cell-input"
                  placeholder="说明此入参的具体业务诉求与预期目标"
                />
              </div>
              <div class="td-col col-enum">
                <input
                  v-model="field.allowed_values_str"
                  type="text"
                  class="cell-input"
                  placeholder="feature, bugfix... (选填)"
                />
              </div>
              <div class="td-col col-action">
                <button
                  type="button"
                  class="btn-row-del"
                  title="删除此字段"
                  @click="removeInputField(fIdx)"
                >
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M18 6L6 18M6 6l12 12"/></svg>
                </button>
              </div>
            </div>
          </div>
        </div>
      </div>

      <!-- 📋 交付契约约束 (Contract Schema) -->
      <div class="field wide contract-section">
        <div class="contract-header">
          <div class="contract-title-group">
            <span class="field-label">📋 结构化交付契约约束 (Contract Schema)</span>
            <span class="field-hint">声明该专家在输出 JSON 交付物时除常规 findings/confidence 外必须包含的特化字段。调度时自动注入 Prompt，交卷时由 Rust 门禁严格校验。</span>
          </div>
          <button type="button" class="btn btn-sm btn-output-add" @click="addSchemaField">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 5v14M5 12h14"/></svg>
            添加契约字段
          </button>
        </div>

        <div v-if="!form.schema_fields.length" class="contract-empty">
          <span class="empty-hint">暂无特化契约字段（仅输出基线 findings / confidence）。点击右上角「+ 添加契约字段」可定义此专家的专属输出（如 risk_level、cve_refs 等）。</span>
        </div>

        <div v-else class="schema-table-wrap output-table-wrap">
          <div class="schema-table-header">
            <div class="th-col col-idx">#</div>
            <div class="th-col col-name">交付字段键名 (Key) <b class="th-req">*</b></div>
            <div class="th-col col-type">数据类型 (Type)</div>
            <div class="th-col col-req">约束 (Req)</div>
            <div class="th-col col-desc">交付物说明 (Description)</div>
            <div class="th-col col-enum">可选枚举范围 (Enum)</div>
            <div class="th-col col-action">操作</div>
          </div>
          <div class="schema-table-body">
            <div
              v-for="(field, fIdx) in form.schema_fields"
              :key="fIdx"
              class="schema-table-row output-row"
            >
              <div class="td-col col-idx">
                <span class="row-index">{{ fIdx + 1 }}</span>
              </div>
              <div class="td-col col-name">
                <input
                  v-model="field.name"
                  type="text"
                  class="cell-input mono-input"
                  placeholder="例如: risk_level"
                  @blur="sanitizeFieldName(field)"
                />
              </div>
              <div class="td-col col-type">
                <select v-model="field.field_type" class="cell-select">
                  <option value="string">string (文本)</option>
                  <option value="string[]">string[] (列表)</option>
                  <option value="number">number (数字)</option>
                  <option value="boolean">boolean (布尔)</option>
                </select>
              </div>
              <div class="td-col col-req">
                <button
                  type="button"
                  class="req-toggle-pill"
                  :class="{ active: field.required }"
                  @click="field.required = !field.required"
                >
                  {{ field.required ? '必填' : '选填' }}
                </button>
              </div>
              <div class="td-col col-desc">
                <input
                  v-model="field.description"
                  type="text"
                  class="cell-input"
                  placeholder="说明此交付字段的评判准则与格式"
                />
              </div>
              <div class="td-col col-enum">
                <input
                  v-model="field.allowed_values_str"
                  type="text"
                  class="cell-input"
                  placeholder="high, medium, low... (选填)"
                />
              </div>
              <div class="td-col col-action">
                <button
                  type="button"
                  class="btn-row-del"
                  title="删除此字段"
                  @click="removeSchemaField(fIdx)"
                >
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M18 6L6 18M6 6l12 12"/></svg>
                </button>
              </div>
            </div>
          </div>
        </div>
      </div>

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
            <div v-if="getSchemaFieldCount(card) > 0" class="meta-tag contract-tag" :title="`特化契约字段: ${getSchemaFieldNames(card)}`">
              <span class="tag-icon">📋</span>
              <span class="tag-text">契约 {{ getSchemaFieldCount(card) }} 字段</span>
            </div>
            <div v-if="getFocusDomains(card).length > 0" class="meta-tag focus-tag" :title="`系统自动装配: ${getFocusDomainLabels(card)}`">
              <span class="tag-icon">🎯</span>
              <span class="tag-text">{{ getFocusDomains(card).length }} 关注材料</span>
            </div>
            <div v-if="getInputFieldCount(card) > 0" class="meta-tag input-tag" :title="`必填/自定义入参: ${getInputFieldNames(card)}`">
              <span class="tag-icon">📥</span>
              <span class="tag-text">入参 {{ getInputFieldCount(card) }} 字段</span>
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
  custom_schema?: string;
  focus_domains?: string;
  input_schema?: string;
}

interface SchemaFieldFormItem {
  name: string;
  field_type: 'string' | 'string[]' | 'number' | 'boolean';
  required: boolean;
  description: string;
  allowed_values_str: string;
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

const focusDomainOptions = [
  { value: 'git_diff', label: '⚡ 关键代码差异 (git_diff)', desc: '自动提取当前代码变更差异切片（带预算截断）' },
  { value: 'changed_files', label: '📁 变更文件列表 (changed_files)', desc: '自动提取工作区未暂存与已修改的文件树' },
  { value: 'last_verify_log', label: '🧪 最近验证/测试日志 (last_verify_log)', desc: '自动注入最近一次构建或单测的终端执行输出' },
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
  focus_domains: [] as string[],
  input_fields: [] as SchemaFieldFormItem[],
  schema_fields: [] as SchemaFieldFormItem[],
});

const presetTemplates = [
  {
    icon: '🎨',
    name: 'frontend_ui_reviewer',
    label: '🎨 前端美化与交互审查专家',
    scope_hints: '涉及前端 UI、Vue 组件重构、CSS 样式排版及响应式交互优化',
    persona_prompt: '你是一位资深前端 UI/UX 架构师。请从视觉美观度、配色和谐度、响应式适配度、组件内聚性及微交互流畅度等维度进行严谨审查。评估并输出质量得分与具体可执行的修改建议。',
    default_test_command: 'npm run build',
    allowed_kinds: 'reviewer',
    hard_scope: '',
    max_concurrency: 0,
    focus_domains: ['git_diff', 'changed_files'],
    input_fields: [
      {
        name: 'target_viewport',
        field_type: 'string' as const,
        required: false,
        description: '重点审查的目标视口/设备分辨率',
        allowed_values_str: 'desktop, mobile, tablet',
      },
    ],
    schema_fields: [
      {
        name: 'visual_score',
        field_type: 'number' as const,
        required: true,
        description: '视觉与微交互体验得分 (0~100)',
        allowed_values_str: '',
      },
      {
        name: 'a11y_issues',
        field_type: 'string[]' as const,
        required: false,
        description: '无障碍与对比度缺陷清单',
        allowed_values_str: '',
      },
    ],
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
    focus_domains: ['git_diff', 'changed_files', 'last_verify_log'],
    input_fields: [
      {
        name: 'security_level',
        field_type: 'string' as const,
        required: false,
        description: '本次审查的安全敏感级别要求',
        allowed_values_str: 'high, medium, critical',
      },
    ],
    schema_fields: [
      {
        name: 'risk_level',
        field_type: 'string' as const,
        required: true,
        description: '综合安全风险等级',
        allowed_values_str: 'low, medium, high, critical',
      },
      {
        name: 'cve_refs',
        field_type: 'string[]' as const,
        required: false,
        description: '关联的 CVE 漏洞编号清单',
        allowed_values_str: '',
      },
    ],
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
    focus_domains: ['changed_files'],
    input_fields: [
      {
        name: 'search_query',
        field_type: 'string' as const,
        required: true,
        description: '需要探测的目标符号、方法名或技术关键词',
        allowed_values_str: '',
      },
    ],
    schema_fields: [] as SchemaFieldFormItem[],
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
    focus_domains: ['changed_files', 'git_diff'],
    input_fields: [] as SchemaFieldFormItem[],
    schema_fields: [] as SchemaFieldFormItem[],
  },
];

function addSchemaField() {
  form.schema_fields.push({
    name: '',
    field_type: 'string',
    required: true,
    description: '',
    allowed_values_str: '',
  });
}

function removeSchemaField(idx: number) {
  form.schema_fields.splice(idx, 1);
}

function addInputField() {
  form.input_fields.push({
    name: '',
    field_type: 'string',
    required: true,
    description: '',
    allowed_values_str: '',
  });
}

function removeInputField(idx: number) {
  form.input_fields.splice(idx, 1);
}

function sanitizeFieldName(field: SchemaFieldFormItem) {
  field.name = field.name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9_]/g, '_')
    .replace(/^_+|_+$/g, '');
}

function getSchemaFieldCount(card: RoleCard): number {
  if (!card.custom_schema) return 0;
  try {
    const parsed = JSON.parse(card.custom_schema);
    return Array.isArray(parsed?.fields) ? parsed.fields.length : 0;
  } catch {
    return 0;
  }
}

function getSchemaFieldNames(card: RoleCard): string {
  if (!card.custom_schema) return '';
  try {
    const parsed = JSON.parse(card.custom_schema);
    if (Array.isArray(parsed?.fields)) {
      return parsed.fields.map((f: any) => f.name).join(', ');
    }
  } catch {}
  return '';
}

function getInputFieldCount(card: RoleCard): number {
  if (!card.input_schema) return 0;
  try {
    const parsed = JSON.parse(card.input_schema);
    return Array.isArray(parsed?.fields) ? parsed.fields.length : 0;
  } catch {
    return 0;
  }
}

function getInputFieldNames(card: RoleCard): string {
  if (!card.input_schema) return '';
  try {
    const parsed = JSON.parse(card.input_schema);
    if (Array.isArray(parsed?.fields)) {
      return parsed.fields.map((f: any) => f.name).join(', ');
    }
  } catch {}
  return '';
}

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

function toggleFocusDomain(domain: string) {
  const idx = form.focus_domains.indexOf(domain);
  if (idx >= 0) {
    form.focus_domains.splice(idx, 1);
  } else {
    form.focus_domains.push(domain);
  }
}

function getFocusDomains(card: RoleCard): string[] {
  if (!card.focus_domains) return [];
  try {
    const parsed = JSON.parse(card.focus_domains);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return card.focus_domains.split(/[,，]/).map((s) => s.trim()).filter(Boolean);
  }
}

function getFocusDomainLabels(card: RoleCard): string {
  const list = getFocusDomains(card);
  const labelMap: Record<string, string> = {
    git_diff: '代码差异 (git_diff)',
    changed_files: '变更文件 (changed_files)',
    last_verify_log: '测试日志 (last_verify_log)',
  };
  return list.map((item) => labelMap[item] || item).join(', ');
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
    focus_domains: [],
    input_fields: [],
    schema_fields: [],
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
  form.focus_domains = tpl.focus_domains ? [...tpl.focus_domains] : [];
  if (tpl.input_fields && tpl.input_fields.length) {
    form.input_fields = tpl.input_fields.map((f: any) => ({ ...f }));
  } else {
    form.input_fields = [];
  }
  if (tpl.schema_fields && tpl.schema_fields.length) {
    form.schema_fields = tpl.schema_fields.map((f: any) => ({ ...f }));
  } else {
    form.schema_fields = [];
  }
  feedbackMsg.value = `已载入「${tpl.label}」预设，可根据需要调整后点击保存。`;
  feedbackIsError.value = false;
}

function editCard(card: RoleCard) {
  isCreating.value = false;
  isEditing.value = true;
  let parsedFields: SchemaFieldFormItem[] = [];
  if (card.custom_schema) {
    try {
      const parsed = JSON.parse(card.custom_schema);
      if (Array.isArray(parsed?.fields)) {
        parsedFields = parsed.fields.map((f: any) => ({
          name: f.name || '',
          field_type: f.field_type || 'string',
          required: f.required !== false,
          description: f.description || '',
          allowed_values_str: Array.isArray(f.allowed_values) ? f.allowed_values.join(', ') : '',
        }));
      }
    } catch {
      parsedFields = [];
    }
  }

  let parsedInputFields: SchemaFieldFormItem[] = [];
  if (card.input_schema) {
    try {
      const parsed = JSON.parse(card.input_schema);
      if (Array.isArray(parsed?.fields)) {
        parsedInputFields = parsed.fields.map((f: any) => ({
          name: f.name || '',
          field_type: f.field_type || 'string',
          required: f.required !== false,
          description: f.description || '',
          allowed_values_str: Array.isArray(f.allowed_values) ? f.allowed_values.join(', ') : '',
        }));
      }
    } catch {
      parsedInputFields = [];
    }
  }

  let parsedDomains: string[] = [];
  if (card.focus_domains) {
    try {
      const parsed = JSON.parse(card.focus_domains);
      if (Array.isArray(parsed)) {
        parsedDomains = parsed;
      }
    } catch {
      parsedDomains = card.focus_domains.split(/[,，]/).map((s) => s.trim()).filter(Boolean);
    }
  }

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
    focus_domains: parsedDomains,
    input_fields: parsedInputFields,
    schema_fields: parsedFields,
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

  // 格式化与校验特化契约字段
  let custom_schema = '';
  const validFields = form.schema_fields.filter((f) => f.name.trim());
  if (validFields.length > 0) {
    const reserved = [
      'findings', 'confidence', 'evidence_refs', 'missing_evidence',
      'changed_files', 'test_status', 'action_log', 'status', 'summary',
    ];
    for (const f of validFields) {
      const fieldName = f.name.trim().toLowerCase();
      if (reserved.includes(fieldName)) {
        feedbackMsg.value = `契约字段名「${fieldName}」为系统基线保留字，禁止重复声明！`;
        feedbackIsError.value = true;
        return;
      }
    }
    const fields = validFields.map((f) => {
      const allowed = f.allowed_values_str.trim()
        ? f.allowed_values_str.split(/[,，]/).map((s) => s.trim()).filter(Boolean)
        : undefined;
      return {
        name: f.name.trim().toLowerCase(),
        field_type: f.field_type,
        required: !!f.required,
        description: f.description.trim(),
        allowed_values: allowed && allowed.length ? allowed : undefined,
      };
    });
    custom_schema = JSON.stringify({
      fields,
      strict_closed_set: true,
    });
  }

  // 格式化与校验特化入参需求字段
  let input_schema = '';
  const validInFields = form.input_fields.filter((f) => f.name.trim());
  if (validInFields.length > 0) {
    const reservedIn = [
      'task', 'focus_domains', 'specialized_context', 'prompt',
    ];
    for (const f of validInFields) {
      const fieldName = f.name.trim().toLowerCase();
      if (reservedIn.includes(fieldName)) {
        feedbackMsg.value = `入参字段名「${fieldName}」为系统调度保留字，禁止重复声明！`;
        feedbackIsError.value = true;
        return;
      }
    }
    const inFields = validInFields.map((f) => {
      const allowed = f.allowed_values_str.trim()
        ? f.allowed_values_str.split(/[,，]/).map((s) => s.trim()).filter(Boolean)
        : undefined;
      return {
        name: f.name.trim().toLowerCase(),
        field_type: f.field_type,
        required: !!f.required,
        description: f.description.trim(),
        allowed_values: allowed && allowed.length ? allowed : undefined,
      };
    });
    input_schema = JSON.stringify({
      fields: inFields,
      strict_closed_set: false,
    });
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
        custom_schema,
        focus_domains: JSON.stringify(form.focus_domains),
        input_schema,
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

/* 📋 交付契约与入参约束 (Contract & Input Schema) */
.contract-section {
  background: rgba(16, 185, 129, 0.02);
  border: 1px solid rgba(16, 185, 129, 0.18);
  border-radius: 10px;
  padding: 16px;
  margin-bottom: 18px;
  transition: border-color 0.2s ease;
}

.input-contract-section {
  background: rgba(6, 182, 212, 0.02);
  border: 1px solid rgba(6, 182, 212, 0.18);
  border-radius: 10px;
  padding: 16px;
  margin-bottom: 18px;
  transition: border-color 0.2s ease;
}

.contract-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 14px;
}

.contract-title-group {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.contract-title-group .field-label {
  font-size: 13.5px;
  font-weight: 600;
  color: #f4f4f5;
  display: flex;
  align-items: center;
  gap: 6px;
}

.contract-title-group .field-hint {
  font-size: 11.5px;
  color: var(--text-3, #a1a1aa);
  line-height: 1.45;
}

.btn-input-add {
  background: rgba(6, 182, 212, 0.12);
  border: 1px solid rgba(6, 182, 212, 0.35);
  color: #67e8f9;
  font-weight: 500;
  display: inline-flex;
  align-items: center;
  gap: 6px;
  white-space: nowrap;
  flex-shrink: 0;
  transition: all 0.15s ease;
}

.btn-input-add:hover {
  background: rgba(6, 182, 212, 0.22);
  border-color: rgba(6, 182, 212, 0.6);
  color: #fff;
  box-shadow: 0 0 12px rgba(6, 182, 212, 0.25);
}

.btn-output-add {
  background: rgba(16, 185, 129, 0.12);
  border: 1px solid rgba(16, 185, 129, 0.35);
  color: #6ee7b7;
  font-weight: 500;
  display: inline-flex;
  align-items: center;
  gap: 6px;
  white-space: nowrap;
  flex-shrink: 0;
  transition: all 0.15s ease;
}

.btn-output-add:hover {
  background: rgba(16, 185, 129, 0.22);
  border-color: rgba(16, 185, 129, 0.6);
  color: #fff;
  box-shadow: 0 0 12px rgba(16, 185, 129, 0.25);
}

.contract-empty {
  padding: 22px 18px;
  border: 1px dashed var(--border-1, #3f3f46);
  border-radius: 8px;
  background: rgba(0, 0, 0, 0.2);
  text-align: center;
}

.input-contract-section .contract-empty {
  border-color: rgba(6, 182, 212, 0.25);
}

.contract-section .contract-empty {
  border-color: rgba(16, 185, 129, 0.25);
}

.empty-hint {
  font-size: 12px;
  color: var(--text-3, #a1a1aa);
}

/* 现代精炼表格体系 (Sleek Schema Table) */
.schema-table-wrap {
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 9px;
  overflow: hidden;
  background: rgba(18, 18, 22, 0.5);
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.2);
}

.input-table-wrap {
  border-color: rgba(6, 182, 212, 0.22);
}

.output-table-wrap {
  border-color: rgba(16, 185, 129, 0.22);
}

.schema-table-header {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 9px 12px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.3px;
  user-select: none;
}

.input-table-wrap .schema-table-header {
  background: rgba(6, 182, 212, 0.08);
  border-bottom-color: rgba(6, 182, 212, 0.2);
  color: #67e8f9;
}

.output-table-wrap .schema-table-header {
  background: rgba(16, 185, 129, 0.08);
  border-bottom-color: rgba(16, 185, 129, 0.2);
  color: #6ee7b7;
}

.th-req {
  color: #ef4444;
  font-weight: bold;
}

.schema-table-body {
  display: flex;
  flex-direction: column;
}

.schema-table-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 6px 12px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.04);
  background: rgba(24, 24, 27, 0.5);
  transition: background 0.15s ease;
}

.schema-table-row:last-child {
  border-bottom: none;
}

.schema-table-row:hover {
  background: rgba(255, 255, 255, 0.035);
}

/* 单元格列宽与对齐 */
.th-col,
.td-col {
  display: flex;
  align-items: center;
}

.col-idx {
  width: 22px;
  justify-content: center;
  flex-shrink: 0;
}

.row-index {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  font-size: 11px;
  color: #71717a;
  font-weight: 600;
}

.col-name {
  flex: 1.4;
  min-width: 140px;
}

.col-type {
  width: 130px;
  flex-shrink: 0;
}

.col-req {
  width: 58px;
  flex-shrink: 0;
  justify-content: center;
}

.col-desc {
  flex: 2;
  min-width: 170px;
}

.col-enum {
  flex: 1.4;
  min-width: 130px;
}

.col-action {
  width: 28px;
  flex-shrink: 0;
  justify-content: center;
}

/* 单元格控件精修 */
.cell-input {
  width: 100%;
  height: 32px;
  padding: 0 10px;
  font-size: 12px;
  border-radius: 6px;
  background: rgba(0, 0, 0, 0.35);
  border: 1px solid rgba(255, 255, 255, 0.08);
  color: #f4f4f5;
  transition: all 0.15s ease;
}

.cell-input:focus {
  background: rgba(0, 0, 0, 0.65);
  outline: none;
}

.input-table-wrap .cell-input:focus,
.input-table-wrap .cell-select:focus {
  border-color: #06b6d4;
  box-shadow: 0 0 0 2px rgba(6, 182, 212, 0.2);
}

.output-table-wrap .cell-input:focus,
.output-table-wrap .cell-select:focus {
  border-color: #10b981;
  box-shadow: 0 0 0 2px rgba(16, 185, 129, 0.2);
}

.mono-input {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  font-size: 12px;
  letter-spacing: 0.2px;
}

.input-table-wrap .mono-input {
  color: #a5f3fc;
}

.output-table-wrap .mono-input {
  color: #a7f3d0;
}

.cell-select {
  width: 100%;
  height: 32px;
  padding: 0 8px;
  font-size: 11.5px;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  border-radius: 6px;
  background: rgba(0, 0, 0, 0.35);
  border: 1px solid rgba(255, 255, 255, 0.08);
  color: #e4e4e7;
  cursor: pointer;
  outline: none;
  transition: all 0.15s ease;
}

.req-toggle-pill {
  width: 100%;
  height: 26px;
  padding: 0 8px;
  border-radius: 13px;
  font-size: 11px;
  font-weight: 600;
  cursor: pointer;
  border: 1px solid rgba(255, 255, 255, 0.08);
  background: rgba(255, 255, 255, 0.04);
  color: #71717a;
  transition: all 0.15s ease;
  text-align: center;
  display: inline-flex;
  align-items: center;
  justify-content: center;
}

.req-toggle-pill.active {
  background: rgba(239, 68, 68, 0.16);
  border-color: rgba(239, 68, 68, 0.45);
  color: #f87171;
  box-shadow: 0 0 8px rgba(239, 68, 68, 0.2);
}

.btn-row-del {
  width: 26px;
  height: 26px;
  border-radius: 6px;
  border: 1px solid transparent;
  background: transparent;
  color: #71717a;
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  transition: all 0.15s ease;
}

.btn-row-del:hover {
  background: rgba(239, 68, 68, 0.18);
  color: #ef4444;
  border-color: rgba(239, 68, 68, 0.35);
}

/* 角色列表徽章样式 */
.meta-tag.contract-tag {
  background: rgba(16, 185, 129, 0.12);
  color: #10b981;
  border-color: rgba(16, 185, 129, 0.25);
}

.meta-tag.focus-tag {
  background: rgba(245, 158, 11, 0.12);
  color: #f59e0b;
  border-color: rgba(245, 158, 11, 0.25);
}

.meta-tag.input-tag {
  background: rgba(6, 182, 212, 0.12);
  color: #06b6d4;
  border-color: rgba(6, 182, 212, 0.25);
}

.focus-domain-section {
  background: rgba(245, 158, 11, 0.03);
  border: 1px solid rgba(245, 158, 11, 0.15);
  border-radius: 8px;
  padding: 14px;
}

.focus-header {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin-bottom: 10px;
}

.focus-pills-wrap {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.focus-pill {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 8px 14px;
  border-radius: 8px;
  border: 1px solid var(--border-1, #3f3f46);
  background: var(--bg-1, #18181b);
  color: var(--text-2, #a1a1aa);
  font-size: 12.5px;
  cursor: pointer;
  transition: all 0.15s ease;
  user-select: none;
}

.focus-pill:hover {
  border-color: #f59e0b;
  color: #f4f4f5;
  background: rgba(245, 158, 11, 0.05);
}

.focus-pill.active {
  background: rgba(245, 158, 11, 0.15);
  border-color: #f59e0b;
  color: #fbbf24;
  font-weight: 600;
  box-shadow: 0 0 10px rgba(245, 158, 11, 0.15);
}

.pill-check {
  font-weight: bold;
  font-size: 13px;
}
</style>

