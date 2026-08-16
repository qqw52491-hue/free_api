<template>
  <div class="plugin-container">
    <div class="header">
      <div class="title-group">
        <h1>能力管理</h1>
        <p class="subtitle">集中维护 MCP 接入与工作区 Skill 指令；无需为两类能力单独切换页面。</p>
      </div>
      <button class="add-btn" @click="showAddModal = true">
        <span class="icon">+</span> 添加 MCP
      </button>
    </div>
    <section class="capability-summary" aria-label="能力概览">
      <article class="capability-card">
        <div class="capability-card-heading"><span class="capability-kicker">MCP 连接</span><strong>{{ plugins.filter((plugin) => plugin.enabled).length }} 已启用</strong></div>
        <p>通过 stdio 或 HTTP 桥接连接外部工具。修改后会在下一次 Agent 任务中重新加载。</p>
      </article>
      <article class="capability-card">
        <div class="capability-card-heading"><span class="capability-kicker">WORKSPACE SKILLS</span><strong>自动加载</strong></div>
        <p>Agent 会按工作区层级读取 <code>AGENTS.md</code> 与 <code>HUB.md</code>，内容变更会自动刷新，无需重复配置。</p>
      </article>
    </section>

    <section class="instruction-library" aria-label="自定义提示词与 Skill 库">
      <div class="library-heading">
        <div>
          <span class="library-kicker">PROMPTS &amp; SKILLS</span>
          <h2>自定义提示词与 Skill</h2>
          <p>保存常用的工作方式、输出规范或领域流程；启用的内容会在下一次 Agent 任务中作为补充上下文加载。</p>
        </div>
        <button class="library-add-btn" data-testid="custom-library-add" type="button" @click="openInstructionModal()">+ 添加内容</button>
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
          <p v-if="item.description" class="instruction-description">{{ item.description }}</p>
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
          <div class="arg-list">
            <span v-for="(arg, idx) in p.args" :key="idx" class="arg-tag">{{ arg }}</span>
          </div>
          <div class="env-badges" v-if="Object.keys(p.env || {}).length > 0">
            <span v-for="(val, key) in p.env" :key="key" class="badge">🔑 {{ key }}</span>
          </div>
        </div>
        <div class="card-footer">
          <span class="created-at">{{ formatDate(p.created_at) }}</span>
          <button class="delete-btn" @click="confirmDelete(p.name)">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="16" height="16">
              <polyline points="3 6 5 6 21 6"></polyline>
              <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"></path>
            </svg>
            删除
          </button>
        </div>
      </div>
    </div>

    <!-- 空状态 -->
    <div v-else class="empty-state">
      <div class="empty-icon">🔌</div>
      <h3>还没有加载任何 MCP 插件</h3>
      <p>MCP 插件可以让你的 AI Agent 操作 Excel、搜索网页、管理文件等</p>
      <button class="add-btn" @click="showAddModal = true">立即添加第一个</button>
    </div>

    <div v-if="showInstructionModal" class="modal-overlay" @click.self="closeInstructionModal">
      <div class="modal-content instruction-modal">
        <h2>{{ editingInstructionId ? '编辑自定义内容' : '添加自定义内容' }}</h2>
        <p class="modal-hint">自定义内容仅作为补充上下文，不能覆盖内置安全规则、工具契约或当前任务。</p>
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
          <label>说明（可选）</label>
          <input v-model.trim="instructionForm.description" maxlength="240" placeholder="说明适用场景，便于以后查找" />
        </div>
        <div class="form-group">
          <label>内容</label>
          <textarea v-model="instructionForm.content" maxlength="12000" rows="10" :placeholder="instructionForm.kind === 'skill' ? '写清执行步骤、输入与输出要求。' : '写清希望 Agent 遵循的表达、格式或偏好。'"></textarea>
          <p class="form-tip">{{ instructionForm.content.length }}/12000。新任务会自动读取已启用的内容。</p>
        </div>
        <div class="form-actions">
          <button class="cancel-btn" type="button" @click="closeInstructionModal">取消</button>
          <button class="save-btn" type="button" :disabled="!instructionForm.name.trim() || !instructionForm.content.trim()" @click="saveCustomInstruction">保存</button>
        </div>
      </div>
    </div>

    <!-- 添加弹窗 -->
    <div v-if="showAddModal" class="modal-overlay" @click.self="showAddModal = false">
      <div class="modal-content">
        <h2>添加 MCP 插件</h2>
        <p class="modal-hint">填入 MCP Server 的启动配置，保存后 Agent 下次启动时自动加载。</p>
        <section class="http-bridge-preset">
          <div class="preset-heading">
            <strong>远端 HTTP MCP 快速桥接</strong>
            <span>通过 uvx mcp-stdio 转换为本地 stdio，无需改动 Agent 运行时。</span>
          </div>
          <input v-model.trim="form.remoteUrl" type="url" placeholder="https://example.com/mcp" />
          <div class="preset-controls">
            <select v-model="form.httpTransport">
              <option value="streamable-http">Streamable HTTP（推荐）</option>
              <option value="sse">Legacy SSE</option>
            </select>
            <input v-model="form.bearerToken" type="password" autocomplete="off" placeholder="Bearer Token（可选）" />
            <button type="button" class="preset-btn" @click="useHttpBridgePreset">填入桥接配置</button>
          </div>
          <p class="form-tip">若留空 token，桥接器会按远端要求使用 OAuth；填写 token 后仅写入本机忽略的插件配置，不会提交到 Git。</p>
        </section>

        <div class="form-group">
          <label>插件名称</label>
          <input v-model="form.name" placeholder="例如: excel, filesystem, web-search" />
        </div>

        <div class="form-group">
          <label>启动命令</label>
          <input v-model="form.command" placeholder="npx, uvx, /usr/local/bin/go-mcp, python3" />
          <p class="form-tip">建议使用绝对路径，避免 PATH 问题。uvx 会自动下载 Python 包。</p>
        </div>

        <div class="form-group">
          <label>运行参数（每行一个）</label>
          <textarea v-model="form.argsText" placeholder="excel-mcp-server&#10;stdio" rows="4"></textarea>
        </div>

        <div class="form-group">
          <label>环境变量（可选，每行 KEY=VALUE）</label>
          <textarea v-model="form.envText" placeholder="API_KEY=sk-xxx&#10;BASE_URL=https://..." rows="3"></textarea>
        </div>

        <div class="form-actions">
          <button class="cancel-btn" @click="showAddModal = false">取消</button>
          <button class="save-btn" @click="savePlugin" :disabled="!form.name || !form.command">
            💾 保存并激活
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup>
import { ref, onMounted } from 'vue';
import { invoke } from '@tauri-apps/api/core';

const plugins = ref([]);
const customInstructions = ref([]);
const showAddModal = ref(false);
const showInstructionModal = ref(false);
const editingInstructionId = ref('');
const instructionForm = ref({ kind: 'prompt', name: '', description: '', content: '', enabled: true });
const form = ref({
  name: '',
  command: '',
  argsText: '',
  envText: '',
  remoteUrl: '',
  httpTransport: 'streamable-http',
  bearerToken: '',
});

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

const closeInstructionModal = () => {
  showInstructionModal.value = false;
  editingInstructionId.value = '';
  instructionForm.value = { kind: 'prompt', name: '', description: '', content: '', enabled: true };
};

const openInstructionModal = (item = null) => {
  editingInstructionId.value = item?.id || '';
  instructionForm.value = item
    ? { kind: item.kind, name: item.name, description: item.description || '', content: item.content, enabled: item.enabled }
    : { kind: 'prompt', name: '', description: '', content: '', enabled: true };
  showInstructionModal.value = true;
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

const useHttpBridgePreset = () => {
  const url = form.value.remoteUrl.trim();
  if (!url) {
    alert('请先填写远端 MCP URL');
    return;
  }
  form.value.name = form.value.name.trim() || 'remote-http-mcp';
  form.value.command = 'uvx';
  form.value.argsText = ['mcp-stdio', url, '--transport', form.value.httpTransport].join('\n');
  const lines = form.value.envText
    .split('\n')
    .filter((line) => !line.trim().startsWith('MCP_BEARER_TOKEN='));
  if (form.value.bearerToken.trim()) {
    lines.push(`MCP_BEARER_TOKEN=${form.value.bearerToken.trim()}`);
  }
  form.value.envText = lines.filter(Boolean).join('\n');
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
    });
    
    showAddModal.value = false;
    form.value = {
      name: '',
      command: '',
      argsText: '',
      envText: '',
      remoteUrl: '',
      httpTransport: 'streamable-http',
      bearerToken: '',
    };
    await loadPlugins();
  } catch (err) {
    alert('保存失败: ' + err);
  }
};

const togglePlugin = async (name, enabled) => {
  try {
    await invoke('toggle_mcp_plugin', { name, enabled });
    await loadPlugins();
  } catch (err) {
    alert('切换失败: ' + err);
  }
};

const confirmDelete = async (name) => {
  if (confirm(`确定要删除插件「${name}」吗？同时会删除对应的 yaml 配置文件。`)) {
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
  const d = new Date(dateStr);
  return `${d.getMonth()+1}/${d.getDate()} ${d.getHours()}:${String(d.getMinutes()).padStart(2, '0')}`;
};

onMounted(() => {
  loadPlugins();
  loadCustomInstructions();
});
</script>

<style scoped>
.plugin-container {
  padding: 24px clamp(20px, 4vw, 48px);
  max-width: 1040px;
  margin: 0 auto;
  color: #e4e4e7;
  height: 100%;
  overflow-y: auto;
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
}

.header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 24px;
  padding-bottom: 16px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
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
.capability-card code { color: #f0d7a1; background: #121214; padding: 1px 4px; border-radius: 4px; font-size: 11px; font-family: ui-monospace, monospace; }

@media (max-width: 760px) { .capability-summary { grid-template-columns: 1fr; } }

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
.instruction-description { margin: 6px 0 0; color: #a1a1aa; font-size: 12px; line-height: 1.4; }
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
  padding: 2px 6px;
  border-radius: 4px;
  color: var(--accent-light);
  cursor: pointer;
  background: transparent;
  font-size: 11px;
}
.text-btn:hover { background: rgba(255, 255, 255, 0.06); }
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

.plugin-info {
  flex: 1;
  min-width: 0;
}

.plugin-info h3 {
  margin: 0 0 2px 0;
  font-size: 13.5px;
  font-weight: 600;
  color: #f4f4f5;
}

.cmd-snippet {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 11px;
  color: #71717a;
  display: block;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

/* Toggle Switch (小巧精致黑灰开关) */
.toggle-switch {
  position: relative;
  width: 36px;
  height: 20px;
  flex-shrink: 0;
}

.toggle-switch input {
  opacity: 0;
  width: 0;
  height: 0;
}

.slider {
  position: absolute;
  cursor: pointer;
  inset: 0;
  background: #27272a;
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 20px;
  transition: 0.2s;
}

.slider::before {
  content: "";
  position: absolute;
  height: 14px;
  width: 14px;
  left: 2px;
  bottom: 2px;
  background: #a1a1aa;
  border-radius: 50%;
  transition: 0.2s;
}

input:checked + .slider {
  background: var(--accent);
  border-color: var(--accent);
}

input:checked + .slider::before {
  background: #ffffff;
  transform: translateX(16px);
}

.card-body {
  margin-bottom: 10px;
}

.arg-list {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
  margin-bottom: 6px;
}

.arg-tag {
  font-size: 10.5px;
  color: #a1a1aa;
  background: #141416;
  border: 1px solid rgba(255, 255, 255, 0.06);
  padding: 2px 8px;
  border-radius: 4px;
  font-family: ui-monospace, monospace;
}

.badge {
  font-size: 10px;
  background: rgba(108, 99, 255, 0.12);
  color: var(--accent-light);
  padding: 2px 8px;
  border-radius: 4px;
}

.card-footer {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding-top: 8px;
  border-top: 1px solid rgba(255, 255, 255, 0.06);
}

.created-at {
  font-size: 10.5px;
  color: #71717a;
}

.delete-btn {
  display: flex;
  align-items: center;
  gap: 4px;
  background: transparent;
  border: 1px solid transparent;
  color: #71717a;
  cursor: pointer;
  padding: 3px 8px;
  border-radius: 4px;
  font-size: 11px;
  transition: all 0.15s;
}

.delete-btn:hover {
  color: var(--red-light);
  background: rgba(255, 71, 87, 0.1);
}

/* Modal (高级磨砂 Obsidian 质感弹窗) */
.modal-overlay {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  bottom: 0;
  background: rgba(0, 0, 0, 0.65);
  backdrop-filter: blur(10px);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 1000;
}

.modal-content {
  background: #18181b;
  width: 100%;
  max-width: 480px;
  padding: 24px;
  border-radius: 12px;
  border: 1px solid rgba(255, 255, 255, 0.12);
  box-shadow: 0 20px 50px rgba(0, 0, 0, 0.5);
}

.modal-content h2 {
  margin: 0 0 4px 0;
  font-size: 16px;
  font-weight: 600;
  color: #f4f4f5;
}

.modal-hint {
  color: #8e8e93;
  font-size: 12px;
  margin: 0 0 18px 0;
}

.form-group {
  margin-bottom: 14px;
}

.form-group label {
  display: block;
  margin-bottom: 4px;
  font-size: 11.5px;
  color: #a1a1aa;
  font-weight: 500;
}

.form-tip {
  font-size: 11px;
  color: #71717a;
  margin: 4px 0 0 0;
}

.http-bridge-preset {
  margin: 0 0 16px;
  padding: 12px;
  border: 1px solid rgba(108, 99, 255, 0.25);
  border-radius: 8px;
  background: rgba(108, 99, 255, 0.05);
}
.preset-heading strong {
  color: #f4f4f5;
  font-size: 12.5px;
}
.preset-heading span {
  color: #8e8e93;
  font-size: 11px;
  line-height: 1.4;
}
.preset-controls {
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(0, 1.2fr) auto;
  gap: 6px;
  margin-top: 8px;
}
select, input, textarea {
  width: 100%;
  background: #121214;
  border: 1px solid rgba(255, 255, 255, 0.12);
  color: #f4f4f5;
  padding: 7px 10px;
  border-radius: 6px;
  outline: none;
  font-size: 12.5px;
  box-sizing: border-box;
  transition: border-color 0.15s;
}
select:focus, input:focus, textarea:focus {
  border-color: var(--accent);
}
.preset-btn {
  border: 1px solid var(--accent);
  border-radius: 6px;
  padding: 0 10px;
  color: #ffffff;
  background: var(--accent);
  font-size: 12px;
  font-weight: 500;
  cursor: pointer;
  white-space: nowrap;
}
.preset-btn:hover {
  background: var(--accent-dark);
}

.form-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 20px;
}

.cancel-btn {
  background: transparent;
  border: 1px solid rgba(255, 255, 255, 0.12);
  color: #a1a1aa;
  padding: 6px 14px;
  border-radius: 6px;
  font-size: 12.5px;
  cursor: pointer;
  transition: all 0.15s;
}
.cancel-btn:hover { border-color: rgba(255, 255, 255, 0.25); color: #f4f4f5; }

.save-btn {
  background: var(--accent);
  color: #ffffff;
  border: none;
  padding: 6px 16px;
  border-radius: 6px;
  font-size: 12.5px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.15s;
}
.save-btn:hover:not(:disabled) {
  background: var(--accent-dark);
}
.save-btn:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}

/* Empty State */
.empty-state {
  text-align: center;
  padding: 60px 0;
}
.empty-icon { font-size: 3rem; margin-bottom: 12px; }
.empty-state h3 { margin: 0 0 6px 0; font-size: 15px; color: #f4f4f5; }
.empty-state p { color: #8e8e93; font-size: 12.5px; margin: 0 0 18px 0; }

.instruction-modal { max-width: 540px; }
.instruction-modal textarea { min-height: 140px; }
</style>
