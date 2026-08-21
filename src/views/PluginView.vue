<template>
  <div class="plugin-container">
    <div class="header">
      <div class="header-left">
        <button class="back-btn" type="button" title="返回对话" @click="goBack">
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <line x1="19" y1="12" x2="5" y2="12"></line>
            <polyline points="12 19 5 12 12 5"></polyline>
          </svg>
          <span>返回对话</span>
        </button>
        <div class="title-group">
          <h1>能力管理</h1>
          <p class="subtitle">维护 MCP 外部工具与工作区 Skill 指令；支持单个/批量极速接入与大模型自动提纯能力卡并写入 SQLite。</p>
        </div>
      </div>
      <div class="header-actions">
        <button class="add-btn" @click="openAddModal">
          <span class="icon">+</span> 接入 MCP
        </button>
      </div>
    </div>
    <section class="capability-summary" aria-label="能力概览">
      <article class="capability-card">
        <div class="capability-card-heading"><span class="capability-kicker">MCP 连接</span><strong>{{ plugins.filter((plugin) => plugin.enabled).length }} 已启用</strong></div>
        <p>通过 Streamable HTTP、SSE 或本地 stdio 连接。支持单条或批量 JSON/清单导入，能力卡保存在 SQLite 中。</p>
      </article>
      <article class="capability-card">
        <div class="capability-card-heading"><span class="capability-kicker">WORKSPACE SKILLS</span><strong>自动加载</strong></div>
        <p>自定义 Skill 库支持批量提炼与单篇配置，提纯后的触发卡作为核心摘要直接注入系统提示词。</p>
      </article>
    </section>

    <section class="instruction-library" aria-label="自定义提示词与 Skill 库">
      <div class="library-heading">
        <div>
          <span class="library-kicker">PROMPTS &amp; SKILLS</span>
          <h2>自定义提示词与 Skill</h2>
          <p>保存常用的工作方式、输出规范或领域流程；启用的内容会在下一次 Agent 任务中作为补充上下文加载。</p>
        </div>
        <div class="library-actions">
          <button class="library-add-btn" data-testid="custom-library-add" type="button" @click="openInstructionModal()">+ 添加 / 批量导入</button>
        </div>
      </div>
      <div v-if="customInstructions.length" class="instruction-grid" data-testid="custom-instruction-list">
        <article v-for="item in customInstructions" :key="item.id" class="instruction-card" :class="{ disabled: !item.enabled }">
          <div class="instruction-card-head">
            <div>
              <span class="instruction-kind">{{ item.kind === 'skill' ? 'CUSTOM SKILL' : 'CUSTOM PROMPT' }}</span>
              <h3>{{ item.name }}</h3>
            </div>
            <label class="toggle-switch" :title="item.enabled ? '停用此条目' : '启用此条目'">
              <input type="checkbox" :checked="item.enabled" @change="toggleCustomInstruction(item, !item.enabled)">
              <span class="slider"></span>
            </label>
          </div>
          <div v-if="item.description" class="capability-card-badge">
            <span class="card-kicker">💡 技能卡:</span>
            <p class="card-desc">{{ item.description }}</p>
          </div>
          <pre class="instruction-preview">{{ instructionPreview(item.content) }}</pre>
          <div class="instruction-card-footer">
            <time>{{ formatDate(item.updated_at) }}</time>
            <div class="instruction-actions">
              <button type="button" class="text-btn" @click="openInstructionModal(item)">编辑</button>
              <button type="button" class="text-btn danger" @click="confirmDeleteCustomInstruction(item)">删除</button>
            </div>
          </div>
        </article>
      </div>
      <div v-else class="instruction-empty">
        <strong>还没有自定义内容</strong>
        <span>可添加“回答风格”等提示词，或添加“代码审查流程”等可复用 Skill。</span>
      </div>
    </section>

    <!-- 插件卡片网格 -->
    <div class="plugin-grid" v-if="plugins.length > 0">
      <div v-for="p in plugins" :key="p.id" class="plugin-card" :class="{ disabled: !p.enabled }">
        <div class="card-header">
          <div class="plugin-icon" :class="{ active: p.enabled }">{{ p.name.charAt(0).toUpperCase() }}</div>
          <div class="plugin-info">
            <h3>{{ p.name }}</h3>
            <code class="cmd-snippet">{{ p.command }} {{ p.args.join(' ') }}</code>
          </div>
          <label class="toggle-switch">
            <input type="checkbox" :checked="p.enabled" @change="togglePlugin(p.name, !p.enabled)">
            <span class="slider"></span>
          </label>
        </div>
        <div class="card-body">
          <div v-if="p.capability_card" class="capability-card-badge">
            <span class="card-kicker">💡 能力卡 (SQLite 已持久化):</span>
            <p class="card-desc">{{ p.capability_card }}</p>
          </div>
          <div v-else class="capability-card-badge empty">
            <span class="card-kicker">💡 暂无能力卡 (点击编辑/提炼生成)</span>
          </div>
          <div class="arg-list">
            <span v-for="(arg, idx) in p.args" :key="idx" class="arg-tag">{{ arg }}</span>
          </div>
          <div class="env-badges" v-if="Object.keys(p.env || {}).length > 0">
            <span v-for="(val, key) in p.env" :key="key" class="badge">🔑 {{ key }}</span>
          </div>
        </div>
        <div class="card-footer">
          <span class="created-at">{{ formatDate(p.created_at) }}</span>
          <div class="footer-actions">
            <button class="text-btn" @click="openEditPlugin(p)">编辑/提炼</button>
            <button class="delete-btn" @click="confirmDelete(p.name)">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="14" height="14">
                <polyline points="3 6 5 6 21 6"></polyline>
                <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"></path>
              </svg>
              删除
            </button>
          </div>
        </div>
      </div>
    </div>

    <!-- 空状态 -->
    <div v-else class="empty-state">
      <div class="empty-icon">🔌</div>
      <h3>还没有加载任何 MCP 插件</h3>
      <p>MCP 插件可以让你的 AI Agent 操作 Excel、搜索网页、管理文件等</p>
      <button class="add-btn" @click="openAddModal">立即接入第一个</button>
    </div>

    <!-- 自定义 Skill / 提示词弹窗 (支持单个与批量) -->
    <div v-if="showInstructionModal" class="modal-overlay" @click.self="closeInstructionModal">
      <div class="modal-content instruction-modal">
        <div class="modal-header-tabs">
          <button 
            type="button" 
            class="tab-btn" 
            :class="{ active: skillTab === 'single' }" 
            @click="skillTab = 'single'"
          >
            {{ editingInstructionId ? '编辑内容' : '📝 单个添加' }}
          </button>
          <button 
            v-if="!editingInstructionId" 
            type="button" 
            class="tab-btn" 
            :class="{ active: skillTab === 'batch' }" 
            @click="skillTab = 'batch'"
          >
            📋 批量导入 Skill
          </button>
        </div>

        <!-- 单个添加/编辑 -->
        <div v-if="skillTab === 'single'">
          <p class="modal-hint">自定义内容保存在本地 SQLite 中，启动任务时自动拼接入系统上下文。</p>
          <div class="form-group">
            <label>类型</label>
            <select v-model="instructionForm.kind">
              <option value="prompt">自定义提示词</option>
              <option value="skill">自定义 Skill</option>
            </select>
          </div>
          <div class="form-group">
            <label>名称</label>
            <input v-model.trim="instructionForm.name" maxlength="80" :placeholder="instructionForm.kind === 'skill' ? '例如：前端代码审查' : '例如：回答风格'" />
          </div>
          <div class="form-group">
            <div class="label-with-action">
              <label>技能触发卡 / 说明（保存至 SQLite，进系统提示词）</label>
              <div class="ai-gen-toolbar" v-if="instructionForm.kind === 'skill'">
                <select v-model="instructionForm.selectedModelId" class="compact-model-select">
                  <option value="">默认模型</option>
                  <option v-for="m in allModels" :key="m.id" :value="m.id">{{ m.display_name || m.name }}</option>
                </select>
                <button type="button" class="ai-gen-btn" :disabled="generatingSkillCard || !instructionForm.name.trim() || !instructionForm.content.trim()" @click="generateSkillCard">
                  {{ generatingSkillCard ? '提炼中…' : '🤖 AI 提炼技能卡' }}
                </button>
              </div>
            </div>
            <textarea v-model.trim="instructionForm.description" maxlength="240" rows="2" placeholder="说明适用场景与何时触发，支持手动输入或上方 AI 自动提炼" />
          </div>
          <div class="form-group">
            <label>完整内容</label>
            <textarea v-model="instructionForm.content" maxlength="12000" rows="8" :placeholder="instructionForm.kind === 'skill' ? '写清执行步骤、输入与输出要求。' : '写清希望 Agent 遵循的表达、格式或偏好。'"></textarea>
            <p class="form-tip">{{ instructionForm.content.length }}/12000。新任务会自动读取已启用的内容。</p>
          </div>
          <div class="form-actions">
            <button class="cancel-btn" type="button" @click="closeInstructionModal">取消</button>
            <button class="save-btn" type="button" :disabled="!instructionForm.name.trim() || !instructionForm.content.trim()" @click="saveCustomInstruction">保存</button>
          </div>
        </div>

        <!-- 批量导入 Skill -->
        <div v-else class="batch-panel">
          <p class="modal-hint">支持粘贴 JSON 列表 <code>[{"name": "...", "content": "..."}]</code> 或用 <code>### 技能名</code> 分隔的多篇 Markdown 文档。</p>
          <div class="form-group">
            <div class="label-with-action">
              <label>提炼所用大模型</label>
              <select v-model="batchSkillModelId" class="compact-model-select">
                <option value="">默认模型</option>
                <option v-for="m in allModels" :key="m.id" :value="m.id">{{ m.display_name || m.name }}</option>
              </select>
            </div>
            <textarea v-model="batchSkillText" rows="10" placeholder="示例 Markdown 批量格式：&#10;### 代码审查规范&#10;这是代码审查步骤和检查清单...&#10;&#10;### 提交信息格式化&#10;这是 Git 提交规范..."></textarea>
          </div>
          <div v-if="batchSkillResults.length" class="batch-results-list">
            <h4>导入结果 ({{ batchSkillResults.filter(r => r.success).length }}/{{ batchSkillResults.length }} 成功):</h4>
            <div v-for="(r, idx) in batchSkillResults" :key="idx" class="batch-result-item" :class="{ success: r.success, fail: !r.success }">
              <div class="result-head">
                <strong>{{ r.name }}</strong>
                <span class="status-badge">{{ r.success ? '✅ 成功' : '❌ 失败' }}</span>
              </div>
              <p v-if="r.description" class="result-card">💡 {{ r.description }}</p>
              <p v-if="r.error" class="result-err">{{ r.error }}</p>
            </div>
          </div>
          <div class="form-actions">
            <button class="cancel-btn" type="button" @click="closeInstructionModal">关闭</button>
            <button class="save-btn" type="button" :disabled="batchSkillLoading || !batchSkillText.trim()" @click="handleBatchImportSkills">
              {{ batchSkillLoading ? '正在提炼入库…' : '🚀 开始批量提炼入库' }}
            </button>
          </div>
        </div>
      </div>
    </div>

    <!-- MCP 接入与导入弹窗 (支持极速直连、标准配置与批量导入) -->
    <div v-if="showAddModal" class="modal-overlay" @click.self="showAddModal = false">
      <div class="modal-content plugin-modal">
        <div class="modal-header-tabs">
          <button 
            type="button" 
            class="tab-btn" 
            :class="{ active: mcpTab === 'quick' }" 
            @click="mcpTab = 'quick'"
          >
            ⚡️ 极速直连 (URL+Token)
          </button>
          <button 
            type="button" 
            class="tab-btn" 
            :class="{ active: mcpTab === 'custom' }" 
            @click="mcpTab = 'custom'"
          >
            ⚙️ 命令行 stdio
          </button>
          <button 
            v-if="!editingPluginName" 
            type="button" 
            class="tab-btn" 
            :class="{ active: mcpTab === 'batch' }" 
            @click="mcpTab = 'batch'"
          >
            📋 批量导入 (JSON/清单)
          </button>
        </div>

        <!-- 极速直连 Tab -->
        <div v-if="mcpTab === 'quick'">
          <p class="modal-hint">只需填入远端 MCP 服务的 URL 与 Bearer Token，系统自动连接、调用选定大模型提纯能力卡并写入 SQLite。</p>
          <div class="form-group">
            <label>插件标识名称（可选，留空将自动根据 URL 命名）</label>
            <input v-model.trim="quickForm.name" placeholder="例如: remote-http-mcp, filesystem, db-tools" />
          </div>
          <div class="form-group">
            <label>远端 MCP 服务 URL（必填）</label>
            <input v-model.trim="quickForm.remoteUrl" type="url" placeholder="https://mcp.example.com/mcp" />
          </div>
          <div class="form-group">
            <label>Bearer Token（可选）</label>
            <input v-model.trim="quickForm.bearerToken" type="password" placeholder="sk-xxx 或自定义密钥" />
          </div>
          <div class="form-row">
            <div class="form-group half">
              <label>协议类型</label>
              <select v-model="quickForm.httpTransport">
                <option value="streamable-http">Streamable HTTP（推荐）</option>
                <option value="sse">Legacy SSE</option>
              </select>
            </div>
            <div class="form-group half">
              <label>提炼能力卡所选大模型</label>
              <select v-model="quickForm.selectedModelId">
                <option value="">默认模型</option>
                <option v-for="m in allModels" :key="m.id" :value="m.id">{{ m.display_name || m.name }}</option>
              </select>
            </div>
          </div>
          <div class="form-actions">
            <button class="cancel-btn" @click="showAddModal = false">取消</button>
            <button class="save-btn" :disabled="!quickForm.remoteUrl.trim() || quickConnecting" @click="handleQuickConnectMcp">
              {{ quickConnecting ? '⏳ 正在连接并提炼能力卡…' : '⚡️ 一键连接提炼并保存' }}
            </button>
          </div>
        </div>

        <!-- 本地 stdio 命令行 Tab -->
        <div v-else-if="mcpTab === 'custom'">
          <p class="modal-hint">配置本地可执行命令与环境变量，支持手动编辑或点击 AI 提炼能力卡。</p>
          <div class="form-group">
            <label>插件名称</label>
            <input v-model.trim="form.name" :disabled="Boolean(editingPluginName)" placeholder="例如: excel, filesystem" />
          </div>
          <div class="form-group">
            <label>启动命令</label>
            <input v-model.trim="form.command" placeholder="npx, uvx, /usr/local/bin/go-mcp, python3" />
          </div>
          <div class="form-group">
            <label>运行参数（每行一个）</label>
            <textarea v-model="form.argsText" placeholder="excel-mcp-server&#10;stdio" rows="3"></textarea>
          </div>
          <div class="form-group">
            <label>环境变量（可选，每行 KEY=VALUE）</label>
            <textarea v-model="form.envText" placeholder="API_KEY=sk-xxx&#10;BASE_URL=https://..." rows="2"></textarea>
          </div>
          <div class="form-group capability-card-field">
            <div class="label-with-action">
              <label>💡 智能能力卡（保存至 SQLite，直接进系统提示词）</label>
              <div class="ai-gen-toolbar">
                <select v-model="form.selectedModelId" class="compact-model-select">
                  <option value="">默认模型</option>
                  <option v-for="m in allModels" :key="m.id" :value="m.id">{{ m.display_name || m.name }}</option>
                </select>
                <button type="button" class="ai-gen-btn" :disabled="generatingMcpCard || !form.name.trim() || !form.command.trim()" @click="generateMcpCard">
                  {{ generatingMcpCard ? '正在生成…' : '🤖 AI 提炼能力卡' }}
                </button>
              </div>
            </div>
            <textarea v-model.trim="form.capabilityCard" placeholder="点击右上角「AI 提炼能力卡」自动生成，或直接手动输入简明核心定位与适用场景..." rows="3"></textarea>
          </div>
          <div class="form-actions">
            <button class="cancel-btn" @click="showAddModal = false">取消</button>
            <button class="save-btn" @click="savePlugin" :disabled="!form.name || !form.command || generatingMcpCard">
              💾 保存并激活
            </button>
          </div>
        </div>

        <!-- 批量导入 Tab -->
        <div v-else class="batch-panel">
          <p class="modal-hint">支持粘贴 <strong>Cursor / Claude Desktop 标准 JSON</strong>（含 <code>mcpServers</code> 对象）或多行 <code>https://... [Token]</code> 格式清单。</p>
          <div class="form-group">
            <div class="label-with-action">
              <label>提炼能力卡所用大模型</label>
              <select v-model="batchMcpModelId" class="compact-model-select">
                <option value="">默认模型</option>
                <option v-for="m in allModels" :key="m.id" :value="m.id">{{ m.display_name || m.name }}</option>
              </select>
            </div>
            <textarea v-model="batchMcpText" rows="9" placeholder='示例 1 (Cursor / Claude JSON 格式)：&#10;{&#10;  "mcpServers": {&#10;    "excel": { "command": "uvx", "args": ["excel-mcp-server", "stdio"] }&#10;  }&#10;}&#10;&#10;示例 2 (多行 HTTP URL 格式)：&#10;https://mcp.example.com/mcp sk-my-token-123 my-custom-mcp'></textarea>
          </div>
          <div v-if="batchMcpResults.length" class="batch-results-list">
            <h4>导入结果 ({{ batchMcpResults.filter(r => r.success).length }}/{{ batchMcpResults.length }} 成功):</h4>
            <div v-for="(r, idx) in batchMcpResults" :key="idx" class="batch-result-item" :class="{ success: r.success, fail: !r.success }">
              <div class="result-head">
                <strong>{{ r.name }} ({{ r.tool_count }} 个工具)</strong>
                <span class="status-badge">{{ r.success ? '✅ 成功' : '❌ 失败' }}</span>
              </div>
              <p v-if="r.capability_card" class="result-card">💡 {{ r.capability_card }}</p>
              <p v-if="r.error" class="result-err">{{ r.error }}</p>
            </div>
          </div>
          <div class="form-actions">
            <button class="cancel-btn" @click="showAddModal = false">关闭</button>
            <button class="save-btn" :disabled="batchMcpLoading || !batchMcpText.trim()" @click="handleBatchImportMcp">
              {{ batchMcpLoading ? '⏳ 批量连接提炼中…' : '🚀 开始批量导入' }}
            </button>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup>
import { ref, onMounted } from 'vue';
import { useRouter, useRoute } from 'vue-router';
import { invoke } from '@tauri-apps/api/core';

const emit = defineEmits(['back']);
const router = useRouter();
const route = useRoute();

const goBack = () => {
  emit('back');
  if (route.path === '/plugins') {
    router.push('/agent');
  }
};

const plugins = ref([]);
const customInstructions = ref([]);
const allModels = ref([]);

// MCP Modal
const showAddModal = ref(false);
const mcpTab = ref('quick'); // 'quick' | 'custom' | 'batch'
const editingPluginName = ref('');
const quickConnecting = ref(false);
const generatingMcpCard = ref(false);

const quickForm = ref({
  name: '',
  remoteUrl: '',
  bearerToken: '',
  httpTransport: 'streamable-http',
  selectedModelId: '',
});

const form = ref({
  name: '',
  command: '',
  argsText: '',
  envText: '',
  capabilityCard: '',
  selectedModelId: '',
});

// MCP Batch
const batchMcpText = ref('');
const batchMcpModelId = ref('');
const batchMcpLoading = ref(false);
const batchMcpResults = ref([]);

// Skill Modal
const showInstructionModal = ref(false);
const skillTab = ref('single'); // 'single' | 'batch'
const editingInstructionId = ref('');
const generatingSkillCard = ref(false);

const instructionForm = ref({
  kind: 'prompt',
  name: '',
  description: '',
  content: '',
  enabled: true,
  selectedModelId: '',
});

// Skill Batch
const batchSkillText = ref('');
const batchSkillModelId = ref('');
const batchSkillLoading = ref(false);
const batchSkillResults = ref([]);

const loadAllModels = async () => {
  try {
    allModels.value = await invoke('get_all_models_with_platform');
  } catch (err) {
    console.error('加载模型列表失败:', err);
  }
};

const loadPlugins = async () => {
  try {
    plugins.value = await invoke('get_mcp_plugins');
  } catch (err) {
    console.error('加载插件失败:', err);
  }
};

const loadCustomInstructions = async () => {
  try {
    customInstructions.value = await invoke('get_agent_custom_instructions');
  } catch (err) {
    console.error('加载自定义提示词与 Skill 失败:', err);
  }
};

onMounted(() => {
  loadPlugins();
  loadCustomInstructions();
  loadAllModels();
});

const openAddModal = () => {
  editingPluginName.value = '';
  mcpTab.value = 'quick';
  quickForm.value = {
    name: '',
    remoteUrl: '',
    bearerToken: '',
    httpTransport: 'streamable-http',
    selectedModelId: allModels.value[0]?.id || '',
  };
  form.value = {
    name: '',
    command: '',
    argsText: '',
    envText: '',
    capabilityCard: '',
    selectedModelId: allModels.value[0]?.id || '',
  };
  batchMcpText.value = '';
  batchMcpResults.value = [];
  batchMcpModelId.value = allModels.value[0]?.id || '';
  showAddModal.value = true;
};

const openEditPlugin = (p) => {
  editingPluginName.value = p.name;
  mcpTab.value = 'custom';
  const envLines = Object.entries(p.env || {})
    .map(([k, v]) => `${k}=${v}`)
    .join('\n');
  form.value = {
    name: p.name,
    command: p.command,
    argsText: (p.args || []).join('\n'),
    envText: envLines,
    capabilityCard: p.capability_card || '',
    selectedModelId: allModels.value[0]?.id || '',
  };
  showAddModal.value = true;
};

const handleQuickConnectMcp = async () => {
  if (!quickForm.value.remoteUrl.trim()) {
    alert('请填写远端 MCP URL');
    return;
  }
  quickConnecting.value = true;
  try {
    await invoke('quick_connect_and_save_mcp', {
      name: quickForm.value.name.trim() || null,
      remoteUrl: quickForm.value.remoteUrl.trim(),
      bearerToken: quickForm.value.bearerToken.trim() || null,
      httpTransport: quickForm.value.httpTransport,
      modelId: quickForm.value.selectedModelId || null,
    });
    showAddModal.value = false;
    await loadPlugins();
  } catch (err) {
    alert('极速连接与提炼失败: ' + err);
  } finally {
    quickConnecting.value = false;
  }
};

const handleBatchImportMcp = async () => {
  if (!batchMcpText.value.trim()) {
    alert('请先输入要导入的 JSON 或清单文本');
    return;
  }
  batchMcpLoading.value = true;
  batchMcpResults.value = [];
  try {
    const results = await invoke('batch_import_mcp_plugins', {
      inputText: batchMcpText.value.trim(),
      modelId: batchMcpModelId.value || null,
    });
    batchMcpResults.value = results;
    await loadPlugins();
  } catch (err) {
    alert('批量导入失败: ' + err);
  } finally {
    batchMcpLoading.value = false;
  }
};

const generateMcpCard = async () => {
  if (!form.value.name.trim() || !form.value.command.trim()) {
    alert('请先填写插件名称与启动命令');
    return;
  }
  generatingMcpCard.value = true;
  try {
    const args = form.value.argsText.split('\n').map(a => a.trim()).filter(Boolean);
    const env = {};
    form.value.envText.split('\n').forEach(line => {
      const idx = line.indexOf('=');
      if (idx > 0) env[line.substring(0, idx).trim()] = line.substring(idx + 1).trim();
    });
    const card = await invoke('generate_mcp_capability_card', {
      modelId: form.value.selectedModelId || null,
      name: form.value.name,
      command: form.value.command,
      args: args.length ? args : null,
      env: Object.keys(env).length ? env : null,
    });
    form.value.capabilityCard = card;
  } catch (err) {
    alert('提炼能力卡失败: ' + err);
  } finally {
    generatingMcpCard.value = false;
  }
};

const savePlugin = async () => {
  try {
    const args = form.value.argsText.split('\n').map(a => a.trim()).filter(a => a.length > 0);
    const env = {};
    form.value.envText.split('\n').forEach(line => {
      const idx = line.indexOf('=');
      if (idx > 0) {
        env[line.substring(0, idx).trim()] = line.substring(idx + 1).trim();
      }
    });
    
    await invoke('save_mcp_plugin', {
      name: form.value.name,
      command: form.value.command,
      args,
      env: Object.keys(env).length > 0 ? env : null,
      capabilityCard: form.value.capabilityCard || null,
    });
    
    showAddModal.value = false;
    await loadPlugins();
  } catch (err) {
    alert('保存失败: ' + err);
  }
};

const openInstructionModal = (item = null) => {
  editingInstructionId.value = item?.id || '';
  skillTab.value = 'single';
  batchSkillText.value = '';
  batchSkillResults.value = [];
  batchSkillModelId.value = allModels.value[0]?.id || '';
  instructionForm.value = item
    ? {
        kind: item.kind,
        name: item.name,
        description: item.description || '',
        content: item.content,
        enabled: item.enabled,
        selectedModelId: allModels.value[0]?.id || '',
      }
    : {
        kind: 'skill',
        name: '',
        description: '',
        content: '',
        enabled: true,
        selectedModelId: allModels.value[0]?.id || '',
      };
  showInstructionModal.value = true;
};

const closeInstructionModal = () => {
  showInstructionModal.value = false;
  editingInstructionId.value = '';
};

const generateSkillCard = async () => {
  if (!instructionForm.value.name.trim() || !instructionForm.value.content.trim()) {
    alert('请先填写 Skill 名称与完整内容');
    return;
  }
  generatingSkillCard.value = true;
  try {
    const card = await invoke('generate_skill_capability_card', {
      modelId: instructionForm.value.selectedModelId || null,
      name: instructionForm.value.name,
      content: instructionForm.value.content,
    });
    instructionForm.value.description = card;
  } catch (err) {
    alert('提炼技能卡失败: ' + err);
  } finally {
    generatingSkillCard.value = false;
  }
};

const saveCustomInstruction = async () => {
  try {
    await invoke('save_agent_custom_instruction', {
      id: editingInstructionId.value || null,
      kind: instructionForm.value.kind,
      name: instructionForm.value.name,
      description: instructionForm.value.description || null,
      content: instructionForm.value.content,
      enabled: instructionForm.value.enabled,
    });
    closeInstructionModal();
    await loadCustomInstructions();
  } catch (err) {
    alert('保存自定义内容失败: ' + err);
  }
};

const handleBatchImportSkills = async () => {
  const text = batchSkillText.value.trim();
  if (!text) {
    alert('请先输入要批量导入的 Skill 内容');
    return;
  }
  batchSkillLoading.value = true;
  batchSkillResults.value = [];

  let items = [];
  try {
    if (text.startsWith('[') && text.endsWith(']')) {
      items = JSON.parse(text);
    } else {
      // Markdown 解析
      const sections = text.split(/(?:^|\n)###\s+/).filter(Boolean);
      for (const sec of sections) {
        const lines = sec.trim().split('\n');
        const title = lines[0].trim();
        const content = lines.slice(1).join('\n').trim();
        if (title && content) {
          items.push({ name: title, content });
        }
      }
    }
  } catch (e) {
    alert('解析输入格式失败: ' + e);
    batchSkillLoading.value = false;
    return;
  }

  if (!items.length) {
    alert('未能识别到有效的 Skill 项目，请使用 ### 标题 分隔每段内容');
    batchSkillLoading.value = false;
    return;
  }

  try {
    const results = await invoke('batch_import_skills', {
      skills: items,
      modelId: batchSkillModelId.value || null,
    });
    batchSkillResults.value = results;
    await loadCustomInstructions();
  } catch (err) {
    alert('批量导入失败: ' + err);
  } finally {
    batchSkillLoading.value = false;
  }
};

const toggleCustomInstruction = async (item, enabled) => {
  try {
    await invoke('toggle_agent_custom_instruction', { id: item.id, enabled });
    await loadCustomInstructions();
  } catch (err) {
    alert('更新自定义内容失败: ' + err);
  }
};

const confirmDeleteCustomInstruction = async (item) => {
  if (!confirm(`确定要删除「${item.name}」吗？`)) return;
  try {
    await invoke('delete_agent_custom_instruction', { id: item.id });
    await loadCustomInstructions();
  } catch (err) {
    alert('删除自定义内容失败: ' + err);
  }
};

const instructionPreview = (content) => content.length > 220 ? `${content.slice(0, 220)}…` : content;

const togglePlugin = async (name, enabled) => {
  try {
    await invoke('toggle_mcp_plugin', { name, enabled });
    await loadPlugins();
  } catch (err) {
    alert('切换失败: ' + err);
  }
};

const confirmDelete = async (name) => {
  if (confirm(`确定要删除插件「${name}」吗？`)) {
    try {
      await invoke('delete_mcp_plugin', { name });
      await loadPlugins();
    } catch (err) {
      alert('删除失败: ' + err);
    }
  }
};

const formatDate = (dateStr) => {
  if (!dateStr) return '';
  const date = new Date(dateStr);
  if (isNaN(date.getTime())) return dateStr;
  return date.toLocaleDateString('zh-CN', {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
};
</script>

<style scoped>
.plugin-container {
  padding: 24px;
  max-width: 1080px;
  margin: 0 auto;
  min-height: 100vh;
  box-sizing: border-box;
}

.header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 24px;
}

.header-left {
  display: flex;
  align-items: center;
  gap: 14px;
}

.back-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  background: rgba(255, 255, 255, 0.06);
  border: 1px solid rgba(255, 255, 255, 0.12);
  color: #e4e4e7;
  padding: 6px 12px;
  border-radius: 6px;
  font-size: 12.5px;
  font-weight: 500;
  cursor: pointer;
  transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1);
  white-space: nowrap;
}

.back-btn:hover {
  background: rgba(255, 255, 255, 0.14);
  border-color: rgba(255, 255, 255, 0.25);
  color: #ffffff;
  transform: translateX(-2px);
}

h1 {
  font-size: 20px;
  font-weight: 700;
  color: #f4f4f5;
  margin: 0 0 4px 0;
  letter-spacing: -0.02em;
}

.subtitle {
  color: #8e8e93;
  font-size: 12.5px;
  margin: 0;
}

.add-btn {
  background: #f4f4f5;
  color: #18181b;
  border: none;
  padding: 6px 14px;
  border-radius: 6px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.15s;
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12.5px;
}

.add-btn:hover {
  background: #ffffff;
  transform: translateY(-1px);
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.2);
}

.capability-summary {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px;
  margin-bottom: 24px;
}
.capability-card {
  padding: 14px 16px;
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 10px;
  background: #18181b;
}
.capability-card-heading {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
}
.capability-card-heading strong { color: var(--accent-light); font-size: 11px; font-weight: 600; }
.capability-kicker { color: #a1a1aa; font-size: 10px; font-weight: 700; letter-spacing: 0.06em; text-transform: uppercase; }
.capability-card p { margin: 6px 0 0; color: #8e8e93; font-size: 12px; line-height: 1.5; }

/* 提示词与 Skill 库 */
.instruction-library {
  margin-bottom: 24px;
  padding: 16px;
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 10px;
  background: #18181b;
}
.library-heading {
  display: flex;
  justify-content: space-between;
  gap: 16px;
  align-items: flex-start;
  margin-bottom: 14px;
}
.library-kicker, .instruction-kind {
  color: var(--accent-light);
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}
.library-heading h2 {
  margin: 3px 0 4px;
  color: #f4f4f5;
  font-size: 15px;
  font-weight: 600;
}
.library-heading p {
  margin: 0;
  color: #8e8e93;
  font-size: 12px;
  line-height: 1.5;
}
.library-add-btn {
  border: 1px solid rgba(255, 255, 255, 0.12);
  border-radius: 6px;
  padding: 6px 12px;
  color: #f4f4f5;
  cursor: pointer;
  background: rgba(255, 255, 255, 0.05);
  font-size: 12px;
  font-weight: 500;
  transition: all 0.12s;
  white-space: nowrap;
}
.library-add-btn:hover {
  background: rgba(255, 255, 255, 0.1);
}

.instruction-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
  gap: 10px;
}
.instruction-card {
  display: flex;
  flex-direction: column;
  padding: 12px 14px;
  border: 1px solid rgba(255, 255, 255, 0.07);
  border-radius: 8px;
  background: #1f1f23;
  transition: all 0.15s;
}
.instruction-card:hover {
  border-color: rgba(255, 255, 255, 0.14);
}
.instruction-card.disabled { opacity: 0.45; }
.instruction-card-head { display: flex; justify-content: space-between; gap: 8px; }
.instruction-card h3 { margin: 3px 0 0; color: #f4f4f5; font-size: 13.5px; font-weight: 600; }
.instruction-preview {
  display: -webkit-box;
  overflow: hidden;
  margin: 8px 0;
  padding: 8px 10px;
  border-radius: 6px;
  background: #141416;
  border: 1px solid rgba(255, 255, 255, 0.05);
  color: #c9cad1;
  font: 11px/1.55 ui-monospace, SFMono-Regular, Menlo, monospace;
  white-space: pre-wrap;
  -webkit-box-orient: vertical;
  -webkit-line-clamp: 4;
}
.instruction-card-footer {
  display: flex;
  justify-content: space-between;
  gap: 8px;
  align-items: center;
  margin-top: auto;
  padding-top: 8px;
  border-top: 1px solid rgba(255, 255, 255, 0.06);
  color: #71717a;
  font-size: 10.5px;
}
.instruction-actions { display: flex; gap: 4px; }
.text-btn {
  border: 0;
  padding: 3px 8px;
  border-radius: 4px;
  color: var(--accent-light);
  cursor: pointer;
  background: rgba(255, 255, 255, 0.04);
  font-size: 11px;
}
.text-btn:hover { background: rgba(255, 255, 255, 0.1); }
.text-btn.danger { color: var(--red-light); }
.text-btn.danger:hover { background: rgba(255, 71, 87, 0.1); }

.instruction-empty {
  display: grid;
  gap: 4px;
  padding: 20px;
  border: 1px dashed rgba(255, 255, 255, 0.1);
  border-radius: 8px;
  color: #8e8e93;
  font-size: 12px;
  text-align: center;
}
.instruction-empty strong { color: #f4f4f5; }

/* 插件卡片网格 */
.plugin-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
  gap: 12px;
}
.plugin-card {
  background: #18181b;
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 10px;
  padding: 14px 16px;
  transition: border-color 0.15s;
}
.plugin-card:hover {
  border-color: rgba(255, 255, 255, 0.16);
}
.plugin-card.disabled { opacity: 0.5; }
.card-header {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 12px;
}
.plugin-icon {
  width: 34px;
  height: 34px;
  background: #27272a;
  border-radius: 8px;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 14px;
  font-weight: 700;
  color: #71717a;
  transition: all 0.15s;
}
.plugin-icon.active {
  background: linear-gradient(135deg, var(--accent), var(--accent-dark));
  color: #ffffff;
}
.plugin-info { flex: 1; min-width: 0; }
.plugin-info h3 { margin: 0 0 2px 0; font-size: 13.5px; font-weight: 600; color: #f4f4f5; }
.cmd-snippet { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11px; color: #71717a; display: block; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }

/* Toggle Switch */
.toggle-switch { position: relative; width: 36px; height: 20px; flex-shrink: 0; }
.toggle-switch input { opacity: 0; width: 0; height: 0; }
.slider { position: absolute; cursor: pointer; inset: 0; background: #27272a; border: 1px solid rgba(255, 255, 255, 0.1); border-radius: 20px; transition: 0.2s; }
.slider::before { content: ""; position: absolute; height: 14px; width: 14px; left: 2px; bottom: 2px; background: #a1a1aa; border-radius: 50%; transition: 0.2s; }
input:checked + .slider { background: var(--accent); border-color: var(--accent); }
input:checked + .slider::before { background: #ffffff; transform: translateX(16px); }

.card-body { margin-bottom: 10px; }
.capability-card-badge {
  background: rgba(108, 99, 255, 0.08);
  border: 1px solid rgba(108, 99, 255, 0.2);
  border-radius: 6px;
  padding: 8px 10px;
  margin-bottom: 8px;
}
.capability-card-badge.empty {
  background: rgba(255, 255, 255, 0.02);
  border-color: rgba(255, 255, 255, 0.06);
}
.card-kicker { color: var(--accent-light); font-size: 10.5px; font-weight: 600; display: block; margin-bottom: 2px; }
.capability-card-badge.empty .card-kicker { color: #71717a; }
.card-desc { margin: 0; color: #e4e4e7; font-size: 11.5px; line-height: 1.45; }

.arg-list { display: flex; flex-wrap: wrap; gap: 4px; margin-bottom: 6px; }
.arg-tag { font-size: 10.5px; color: #a1a1aa; background: #141416; border: 1px solid rgba(255, 255, 255, 0.06); padding: 2px 8px; border-radius: 4px; font-family: ui-monospace, monospace; }
.badge { font-size: 10px; background: rgba(108, 99, 255, 0.12); color: var(--accent-light); padding: 2px 8px; border-radius: 4px; }
.card-footer { display: flex; justify-content: space-between; align-items: center; padding-top: 8px; border-top: 1px solid rgba(255, 255, 255, 0.06); }
.footer-actions { display: flex; gap: 6px; align-items: center; }
.created-at { font-size: 10.5px; color: #71717a; }
.delete-btn { display: flex; align-items: center; gap: 4px; background: transparent; border: 1px solid transparent; color: #71717a; cursor: pointer; padding: 3px 8px; border-radius: 4px; font-size: 11px; transition: all 0.15s; }
.delete-btn:hover { color: var(--red-light); background: rgba(255, 71, 87, 0.1); }

/* Modal */
.modal-overlay { position: fixed; top: 0; left: 0; right: 0; bottom: 0; background: rgba(0, 0, 0, 0.65); backdrop-filter: blur(10px); display: flex; align-items: center; justify-content: center; z-index: 1000; }
.modal-content { background: #18181b; width: 100%; max-width: 520px; max-height: 90vh; overflow-y: auto; padding: 24px; border-radius: 12px; border: 1px solid rgba(255, 255, 255, 0.12); box-shadow: 0 20px 50px rgba(0, 0, 0, 0.5); }
.modal-content h2 { margin: 0 0 4px 0; font-size: 16px; font-weight: 600; color: #f4f4f5; }
.modal-hint { color: #8e8e93; font-size: 12px; margin: 0 0 18px 0; }
.form-group { margin-bottom: 14px; }
.form-group label { display: block; margin-bottom: 4px; font-size: 11.5px; color: #a1a1aa; font-weight: 500; }
.label-with-action { display: flex; justify-content: space-between; align-items: center; margin-bottom: 6px; }
.ai-gen-toolbar { display: flex; gap: 6px; align-items: center; }
.compact-model-select { width: auto; padding: 3px 8px; font-size: 11px; border-radius: 4px; }
.ai-gen-btn { background: linear-gradient(135deg, var(--accent), var(--accent-dark)); color: #ffffff; border: none; padding: 3px 10px; border-radius: 4px; font-size: 11px; font-weight: 600; cursor: pointer; white-space: nowrap; transition: all 0.15s; }
.ai-gen-btn:hover:not(:disabled) { filter: brightness(1.1); transform: translateY(-1px); }
.ai-gen-btn:disabled { opacity: 0.45; cursor: not-allowed; }
.form-tip { font-size: 11px; color: #71717a; margin: 4px 0 0 0; }
.http-bridge-preset { margin: 0 0 16px; padding: 12px; border: 1px solid rgba(108, 99, 255, 0.25); border-radius: 8px; background: rgba(108, 99, 255, 0.05); }
.preset-heading strong { color: #f4f4f5; font-size: 12.5px; }
.preset-heading span { color: #8e8e93; font-size: 11px; line-height: 1.4; }
.preset-controls { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1.2fr) auto; gap: 6px; margin-top: 8px; }
select, input, textarea { width: 100%; background: #121214; border: 1px solid rgba(255, 255, 255, 0.12); color: #f4f4f5; padding: 7px 10px; border-radius: 6px; outline: none; font-size: 12.5px; box-sizing: border-box; }
.preset-btn { border: 1px solid var(--accent); border-radius: 6px; padding: 0 10px; color: #ffffff; background: var(--accent); font-size: 12px; font-weight: 500; cursor: pointer; white-space: nowrap; }
.preset-btn:hover { background: var(--accent-dark); }
.form-actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 20px; }
.cancel-btn { background: transparent; border: 1px solid rgba(255, 255, 255, 0.12); color: #a1a1aa; padding: 6px 14px; border-radius: 6px; font-size: 12.5px; cursor: pointer; }
.save-btn { background: var(--accent); color: #ffffff; border: none; padding: 6px 16px; border-radius: 6px; font-size: 12.5px; font-weight: 600; cursor: pointer; }
.save-btn:disabled { opacity: 0.4; cursor: not-allowed; }
.empty-state { text-align: center; padding: 60px 0; }
.empty-icon { font-size: 3rem; margin-bottom: 12px; }
.empty-state h3 { margin: 0 0 6px 0; font-size: 15px; color: #f4f4f5; }
.modal-header-tabs {
  display: flex;
  gap: 8px;
  margin-bottom: 16px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
  padding-bottom: 8px;
}
.tab-btn {
  background: transparent;
  border: 1px solid transparent;
  color: #8e8e93;
  padding: 6px 12px;
  border-radius: 6px;
  font-size: 12.5px;
  font-weight: 500;
  cursor: pointer;
  transition: all 0.15s;
}
.tab-btn:hover {
  color: #f4f4f5;
  background: rgba(255, 255, 255, 0.04);
}
.tab-btn.active {
  color: #ffffff;
  background: rgba(108, 99, 255, 0.15);
  border-color: rgba(108, 99, 255, 0.35);
  font-weight: 600;
}

.form-row {
  display: flex;
  gap: 12px;
}
.form-row .half {
  flex: 1;
}

.batch-panel {
  display: flex;
  flex-direction: column;
}
.batch-results-list {
  margin-top: 14px;
  display: flex;
  flex-direction: column;
  gap: 8px;
  max-height: 200px;
  overflow-y: auto;
  padding-right: 4px;
}
.batch-results-list h4 {
  margin: 0 0 6px;
  font-size: 12px;
  color: #a1a1aa;
}
.batch-result-item {
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 6px;
  padding: 8px 10px;
}
.batch-result-item.success {
  border-color: rgba(46, 213, 115, 0.25);
  background: rgba(46, 213, 115, 0.03);
}
.batch-result-item.fail {
  border-color: rgba(255, 71, 87, 0.25);
  background: rgba(255, 71, 87, 0.03);
}
.result-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 4px;
}
.result-head strong {
  font-size: 12px;
  color: #f4f4f5;
}
.status-badge {
  font-size: 10.5px;
  font-weight: 600;
}
.result-card {
  margin: 0;
  font-size: 11px;
  color: #a1a1aa;
  line-height: 1.4;
}
.result-err {
  margin: 0;
  font-size: 11px;
  color: var(--red-light);
}

.instruction-modal { max-width: 580px; }
.instruction-modal textarea { min-height: 120px; }
.plugin-modal { max-width: 580px; }
</style>
