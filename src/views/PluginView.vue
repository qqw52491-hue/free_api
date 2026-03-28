<template>
  <div class="plugin-container">
    <div class="header">
      <div class="title-group">
        <h1>MCP 插件中心</h1>
        <p class="subtitle">让 AI 获得无限外部能力。支持 npx, uvx, 本地二进制等多种运行方式。</p>
      </div>
      <button class="add-btn" @click="showAddModal = true">
        <span class="icon">+</span> 添加插件
      </button>
    </div>

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

    <!-- 添加弹窗 -->
    <div v-if="showAddModal" class="modal-overlay" @click.self="showAddModal = false">
      <div class="modal-content">
        <h2>添加 MCP 插件</h2>
        <p class="modal-hint">填入 MCP Server 的启动配置，保存后 Agent 下次启动时自动加载。</p>

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
const showAddModal = ref(false);
const form = ref({
  name: '',
  command: '',
  argsText: '',
  envText: '',
});

const loadPlugins = async () => {
  try {
    plugins.value = await invoke('get_mcp_plugins');
  } catch (err) {
    console.error('加载插件失败:', err);
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
    });
    
    showAddModal.value = false;
    form.value = { name: '', command: '', argsText: '', envText: '' };
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

onMounted(loadPlugins);
</script>

<style scoped>
.plugin-container {
  padding: 32px 40px;
  max-width: 1200px;
  margin: 0 auto;
  color: #e0e0e0;
  height: 100%;
  overflow-y: auto;
}

.header {
  display: flex;
  justify-content: space-between;
  align-items: flex-end;
  margin-bottom: 40px;
}

h1 {
  font-size: 2.2rem;
  margin: 0 0 8px 0;
  background: linear-gradient(135deg, #fff 0%, #888 100%);
  background-clip: text;
  -webkit-background-clip: text;
  -webkit-text-fill-color: transparent;
}

.subtitle {
  color: #666;
  font-size: 0.95rem;
  margin: 0;
}

.add-btn {
  background: linear-gradient(135deg, #3d5afe, #651fff);
  color: white;
  border: none;
  padding: 12px 24px;
  border-radius: 12px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.3s;
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 0.95rem;
}

.add-btn:hover {
  transform: translateY(-2px);
  box-shadow: 0 6px 24px rgba(61, 90, 254, 0.35);
}

.plugin-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(360px, 1fr));
  gap: 20px;
}

.plugin-card {
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid #2a2a2e;
  border-radius: 16px;
  padding: 20px 24px;
  transition: all 0.3s;
}

.plugin-card:hover {
  border-color: #3d5afe;
  background: rgba(61, 90, 254, 0.04);
  transform: translateY(-2px);
}

.plugin-card.disabled {
  opacity: 0.5;
}

.card-header {
  display: flex;
  align-items: center;
  gap: 14px;
  margin-bottom: 16px;
}

.plugin-icon {
  width: 44px;
  height: 44px;
  background: #222;
  border-radius: 12px;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 1.4rem;
  font-weight: bold;
  color: #555;
  transition: all 0.3s;
}

.plugin-icon.active {
  background: linear-gradient(135deg, #3d5afe, #651fff);
  color: white;
}

.plugin-info {
  flex: 1;
  min-width: 0;
}

.plugin-info h3 {
  margin: 0 0 4px 0;
  font-size: 1.1rem;
}

.cmd-snippet {
  font-family: 'SF Mono', 'Fira Code', monospace;
  font-size: 0.8rem;
  color: #666;
  display: block;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

/* Toggle Switch */
.toggle-switch {
  position: relative;
  width: 44px;
  height: 24px;
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
  background: #333;
  border-radius: 24px;
  transition: 0.3s;
}

.slider::before {
  content: "";
  position: absolute;
  height: 18px;
  width: 18px;
  left: 3px;
  bottom: 3px;
  background: white;
  border-radius: 50%;
  transition: 0.3s;
}

input:checked + .slider {
  background: #3d5afe;
}

input:checked + .slider::before {
  transform: translateX(20px);
}

.card-body {
  margin-bottom: 14px;
}

.arg-list {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin-bottom: 8px;
}

.arg-tag {
  font-size: 0.8rem;
  color: #aaa;
  background: rgba(255, 255, 255, 0.06);
  padding: 3px 10px;
  border-radius: 6px;
  font-family: monospace;
}

.badge {
  font-size: 0.75rem;
  background: rgba(61, 90, 254, 0.1);
  color: #7986cb;
  padding: 2px 10px;
  border-radius: 4px;
}

.card-footer {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding-top: 12px;
  border-top: 1px solid #222;
}

.created-at {
  font-size: 0.8rem;
  color: #555;
}

.delete-btn {
  display: flex;
  align-items: center;
  gap: 4px;
  background: transparent;
  border: 1px solid transparent;
  color: #555;
  cursor: pointer;
  padding: 6px 12px;
  border-radius: 8px;
  font-size: 0.8rem;
  transition: all 0.2s;
}

.delete-btn:hover {
  color: #ff5252;
  border-color: rgba(255, 82, 82, 0.3);
  background: rgba(255, 82, 82, 0.08);
}

/* Modal */
.modal-overlay {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  bottom: 0;
  background: rgba(0, 0, 0, 0.75);
  backdrop-filter: blur(8px);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 1000;
}

.modal-content {
  background: #1a1a1e;
  width: 100%;
  max-width: 520px;
  padding: 32px;
  border-radius: 20px;
  border: 1px solid #333;
}

.modal-content h2 {
  margin: 0 0 8px 0;
  font-size: 1.4rem;
}

.modal-hint {
  color: #666;
  font-size: 0.9rem;
  margin: 0 0 24px 0;
}

.form-group {
  margin-bottom: 20px;
}

.form-group label {
  display: block;
  margin-bottom: 6px;
  font-size: 0.85rem;
  color: #999;
  font-weight: 500;
}

.form-tip {
  font-size: 0.8rem;
  color: #555;
  margin: 6px 0 0 0;
}

input, textarea {
  width: 100%;
  background: #111;
  border: 1px solid #333;
  color: white;
  padding: 10px 14px;
  border-radius: 10px;
  outline: none;
  font-size: 0.9rem;
  font-family: 'SF Mono', 'Fira Code', monospace;
  box-sizing: border-box;
  transition: border-color 0.2s;
}

input:focus, textarea:focus {
  border-color: #3d5afe;
}

textarea {
  resize: vertical;
}

.form-actions {
  display: flex;
  justify-content: flex-end;
  gap: 12px;
  margin-top: 28px;
}

.cancel-btn {
  background: transparent;
  border: 1px solid #333;
  color: #aaa;
  padding: 10px 20px;
  border-radius: 10px;
  cursor: pointer;
  transition: all 0.2s;
}

.cancel-btn:hover {
  border-color: #555;
}

.save-btn {
  background: linear-gradient(135deg, #3d5afe, #651fff);
  color: white;
  border: none;
  padding: 10px 24px;
  border-radius: 10px;
  cursor: pointer;
  font-weight: 600;
  transition: all 0.3s;
}

.save-btn:hover:not(:disabled) {
  box-shadow: 0 4px 16px rgba(61, 90, 254, 0.35);
}

.save-btn:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}

/* Empty State */
.empty-state {
  text-align: center;
  padding: 80px 0;
}

.empty-icon {
  font-size: 4rem;
  margin-bottom: 16px;
}

.empty-state h3 {
  margin: 0 0 8px 0;
  font-size: 1.2rem;
}

.empty-state p {
  color: #666;
  margin: 0 0 24px 0;
}
</style>
