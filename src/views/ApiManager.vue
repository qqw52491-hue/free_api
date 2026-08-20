<template>
  <div class="manager-layout">
    <!-- 顶部主标签页切换 -->
    <header class="top-nav-bar">
      <div class="nav-tabs">
        <button
          class="nav-tab-btn"
          :class="{ active: activeTab === 'platforms' }"
          @click="activeTab = 'platforms'"
        >
          <span>🌐 渠道与模型管理</span>
          <span class="tab-badge">{{ platforms.length }} 平台</span>
        </button>
        <button
          class="nav-tab-btn"
          :class="{ active: activeTab === 'specs' }"
          @click="activeTab = 'specs'"
        >
          <span>📚 模型物理规格词典</span>
          <span class="tab-badge">{{ modelSpecs.length }} 预设</span>
        </button>
      </div>
      <div class="nav-right-tip" v-if="activeTab === 'platforms'">
        <span>💡 提示：添加模型时可直接引用【规格词典】，免去重复填写上下文大小</span>
      </div>
      <div class="nav-right-tip" v-else>
        <span>⚡ 物理规格全局统一维护，修改后所有引用该规格的配置均可继承</span>
      </div>
    </header>

    <!-- 主体内容区 -->
    <main class="main-content-body">
      <!-- ==================== 视图 1：渠道与模型管理 ==================== -->
      <div v-if="activeTab === 'platforms'" class="tab-content-wrap platforms-view">
        <!-- 左侧: 平台列表 -->
        <aside class="platform-panel">
          <div class="panel-header">
            <h3 class="panel-title">🌐 AI 平台</h3>
            <button class="btn btn-primary btn-sm" @click="showAddPlatform = true">
              <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M12 5v14M5 12h14"/></svg>
              添加平台
            </button>
          </div>

          <div class="platform-list">
            <div v-if="platforms.length === 0" class="empty-hint">
              <span style="font-size:28px;opacity:0.4">🔗</span>
              <span>点击上方添加平台</span>
            </div>
            <div
              v-for="p in platforms" :key="p.id"
              class="platform-item"
              :class="{ active: selectedPlatformId === p.id }"
              @click="selectPlatform(p.id)"
            >
              <div class="p-left">
                <div class="p-icon">{{ getPlatformEmoji(p.name) }}</div>
                <div class="p-info">
                  <div class="p-name">{{ p.name }}</div>
                  <div class="p-url">{{ p.base_url }}</div>
                </div>
              </div>
              <div class="p-actions">
                <span class="p-model-count">{{ getModelCount(p.id) }} 个模型</span>
                <button class="btn-icon btn-xs" @click.stop="editPlatform(p)" title="编辑">
                  <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z"/></svg>
                </button>
                <button class="btn-icon btn-xs btn-del" @click.stop="confirmDeletePlatform(p)" title="删除">
                  <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2"/></svg>
                </button>
              </div>
            </div>
          </div>
        </aside>

        <!-- 右侧: 模型列表 -->
        <section class="model-panel">
          <div v-if="!selectedPlatformId || !currentPlatform" class="panel-header">
            <div>
              <h3 class="panel-title">模型管理</h3>
              <p class="panel-subtitle">请先从左侧选择一个平台，再管理其模型与连通性。</p>
            </div>
            <div class="model-header-actions">
              <button class="btn btn-primary btn-sm" type="button" data-testid="test-current-platform" disabled>检测当前平台</button>
            </div>
          </div>
          <template v-if="selectedPlatformId && currentPlatform">
            <div class="panel-header">
              <div>
                <h3 class="panel-title">{{ currentPlatform.name }} — 模型管理</h3>
                <p class="panel-subtitle">Key 和请求地址自动继承平台配置，参数可直接套用规格词典</p>
              </div>
              <div class="model-header-actions">
                <span v-if="batchTestSummary && batchTestPlatformId === selectedPlatformId" class="batch-test-summary" data-testid="batch-test-summary" role="status" aria-live="polite">{{ batchTestSummary }}</span>
                <button class="btn btn-primary btn-sm" type="button" data-testid="test-current-platform" :disabled="isTestingCurrentPlatform || models.length === 0" :aria-busy="isTestingCurrentPlatform ? 'true' : 'false'" @click="testCurrentPlatform">
                  {{ isTestingCurrentPlatform ? `检测中 ${batchTestCompleted}/${batchTestTotal}` : '检测当前平台' }}
                </button>
              </div>
            </div>

            <!-- 快速添加模型栏 (含规格库快速代入) -->
            <div class="add-model-container">
              <div class="add-model-bar">
                <div class="spec-picker-wrap">
                  <span class="spec-picker-icon">⚡</span>
                  <select v-model="quickSelectedSpecId" @change="onQuickSpecSelect" class="spec-select-box">
                    <option value="">-- 从规格词典自动代入参数 --</option>
                    <option v-for="spec in modelSpecs" :key="spec.id" :value="spec.id">
                      {{ spec.name }} ({{ formatTok(spec.context_window) }} 上下文 / {{ formatTok(spec.max_tokens) }} 输出)
                    </option>
                  </select>
                </div>

                <div class="add-model-input-wrap">
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 5v14M5 12h14"/></svg>
                  <input
                    class="add-model-input"
                    v-model="newModelName"
                    placeholder="输入平台调用的 API 模型标识，如 deepseek-chat 或 deepseek-ai/DeepSeek-V3"
                    @keydown.enter="addModel"
                  />
                </div>

                <button class="btn btn-primary" @click="addModel" :disabled="!newModelName.trim()">
                  添加模型
                </button>
              </div>

              <!-- 选定规格后的快速属性提示卡 -->
              <div v-if="activeQuickSpec" class="selected-spec-pill-bar">
                <span class="spec-pill-label">已套用规格: <strong>{{ activeQuickSpec.name }}</strong></span>
                <span class="spec-tag">窗口: {{ formatTok(activeQuickSpec.context_window) }}</span>
                <span class="spec-tag">输出: {{ formatTok(activeQuickSpec.max_tokens) }}</span>
                <span v-if="activeQuickSpec.supports_thinking" class="spec-tag thinking">🧠 深度思考</span>
                <span class="spec-tag">推荐温度: {{ activeQuickSpec.default_temperature }}</span>
                <button class="btn-clear-spec" @click="clearQuickSpec" title="清除规格套用">✕</button>
              </div>
            </div>

            <!-- 模型列表 -->
            <div class="model-grid" v-if="models.length > 0">
              <div v-for="m in models" :key="m.id" class="model-card">
                <div class="m-left">
                  <div class="m-icon-box" :class="m.status">
                    <span class="status-dot"></span>
                  </div>
                  <div class="m-info">
                    <div class="m-name">{{ m.name }}</div>
                    <div class="m-meta">
                      <span v-if="m.status === 'online'" class="badge badge-online">在线 {{ m.latency_ms }}ms</span>
                      <span v-else-if="m.status === 'offline'" class="badge badge-offline" :title="m.error_msg || '离线'">离线</span>
                      <span v-else-if="m.status === 'testing'" class="badge badge-testing">检测中…</span>
                      <span v-else class="badge badge-unknown">未检测</span>
                      <button v-if="m.status === 'offline' || m.diagnostics" type="button" class="btn-diag-badge" @click="openDiagnostics(m)" title="查看详细网络与请求头诊断">
                        🔍 诊断详情
                      </button>
                      <span class="m-tokens">输出上限: {{ m.max_tokens }} tok</span>
                      <span class="m-tokens" :style="{ color: m.context_window > 0 ? '#818cf8' : 'var(--text-4)' }">
                        上下文窗口: {{ m.context_window > 0 ? formatTok(m.context_window) : '未配置 (默认32k)' }}
                      </span>
                    </div>
                  </div>
                </div>
                <div class="m-actions">
                  <button class="btn btn-ghost btn-sm" @click="editModel(m)" title="配置">
                    <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z"/></svg>
                    配置
                  </button>
                  <button class="btn btn-success btn-sm" @click="testSingleModel(m)" :disabled="m.status === 'testing'">
                    <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" :class="{ spinning: m.status === 'testing' }">
                      <path d="M21 2v6h-6M3 12a9 9 0 0 1 15-6.7L21 8"/>
                    </svg>
                    检测
                  </button>
                  <button class="btn btn-danger btn-sm" @click="confirmDeleteModel(m)">
                    <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/></svg>
                    删除
                  </button>
                </div>
              </div>
            </div>
            <div v-else class="empty-models">
              <span style="font-size:36px;opacity:0.3">🤖</span>
              <p>此平台下暂无模型</p>
              <p style="font-size:12px;color:var(--text-4)">在上方从规格词典选择或输入模型标识后点击添加</p>
            </div>
          </template>

          <!-- 未选中平台时 -->
          <div v-else class="select-hint">
            <div class="hint-glow">
              <span style="font-size:48px">⚡</span>
            </div>
            <h3>选择一个平台开始管理模型</h3>
            <p>在左侧添加或选择一个 AI 平台（如 OpenRouter、DeepSeek、硅基流动），<br/>然后在右侧快速录入该平台支持的模型与上下文参数</p>
          </div>
        </section>
      </div>

      <!-- ==================== 视图 2：模型物理规格词典 ==================== -->
      <div v-else class="tab-content-wrap specs-view">
        <div class="specs-container">
          <div class="specs-header">
            <div>
              <h2 class="specs-main-title">📚 模型物理规格词典 (Model Specs)</h2>
              <p class="specs-main-desc">
                集中维护常见大模型的物理上下文容量、最大输出限制与思考特性。与 API 渠道完全解耦，配置一次，处处复用。
              </p>
            </div>
            <div class="specs-header-actions">
              <div class="spec-search-wrap">
                <input
                  v-model="specSearchQuery"
                  class="spec-search-input"
                  placeholder="🔍 搜索模型规格..."
                />
              </div>
              <button class="btn btn-primary" @click="openAddSpecModal">
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M12 5v14M5 12h14"/></svg>
                + 添加新规格
              </button>
            </div>
          </div>

          <!-- 规格卡片网格 -->
          <div class="specs-grid">
            <div
              v-for="spec in filteredModelSpecs"
              :key="spec.id"
              class="spec-card"
            >
              <div class="spec-card-top">
                <div class="spec-title-row">
                  <span class="spec-name">{{ spec.name }}</span>
                  <span v-if="spec.supports_thinking" class="badge-thinking">🧠 深度思考</span>
                  <span v-else class="badge-standard">⚡ 标准通用</span>
                </div>
                <p class="spec-desc">{{ spec.description || '暂无描述' }}</p>
              </div>

              <div class="spec-props-grid">
                <div class="spec-prop-item">
                  <span class="prop-label">上下文窗口 (Context)</span>
                  <span class="prop-val highlight">{{ formatTok(spec.context_window) }}</span>
                </div>
                <div class="spec-prop-item">
                  <span class="prop-label">最大单次输出 (Max)</span>
                  <span class="prop-val">{{ formatTok(spec.max_tokens) }}</span>
                </div>
                <div class="spec-prop-item">
                  <span class="prop-label">推荐默认温度</span>
                  <span class="prop-val">{{ spec.default_temperature }}</span>
                </div>
              </div>

              <div class="spec-card-footer">
                <div class="spec-card-btns">
                  <button class="btn btn-ghost btn-sm" @click="editSpec(spec)">
                    编辑规格
                  </button>
                  <button class="btn btn-danger btn-sm" @click="confirmDeleteSpec(spec)">
                    删除
                  </button>
                </div>
                <button
                  v-if="selectedPlatformId && currentPlatform"
                  class="btn btn-primary btn-sm btn-quick-apply"
                  @click="quickApplySpecToPlatform(spec)"
                  title="直接将此规格添加为当前选定平台的模型"
                >
                  + 添加至 {{ currentPlatform.name }}
                </button>
              </div>
            </div>
          </div>
        </div>
      </div>
    </main>

    <!-- ==================== 弹窗区 ==================== -->
    <Teleport to="body">
      <!-- 弹窗: 添加/编辑平台 -->
      <Transition name="fade">
        <div class="modal-overlay" v-if="showAddPlatform || editingPlatform" @click.self="closePlatformModal">
          <div class="modal">
            <div class="modal-header">
              <span class="modal-title">{{ editingPlatform ? '编辑平台' : '添加新平台' }}</span>
              <button class="btn-icon" @click="closePlatformModal">
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M18 6L6 18M6 6l12 12"/></svg>
              </button>
            </div>
            <div class="modal-body">
              <div class="form-group">
                <label class="form-label">平台名称 *</label>
                <input class="form-input" v-model="pForm.name" placeholder="例如：OpenRouter、DeepSeek、硅基流动" />
              </div>
              <div class="form-group">
                <label class="form-label">API 地址 (Base URL) *</label>
                <input class="form-input" v-model="pForm.base_url" placeholder="https://openrouter.ai/api/v1" />
              </div>
              <div class="form-group">
                <label class="form-label">API Key *</label>
                <div class="key-wrap">
                  <input
                    :type="showKey ? 'text' : 'password'"
                    class="form-input"
                    v-model="pForm.api_key"
                    placeholder="sk-..."
                    style="padding-right:44px"
                  />
                  <button class="key-toggle" @click="showKey = !showKey" type="button">
                    <svg v-if="!showKey" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"/><circle cx="12" cy="12" r="3"/></svg>
                    <svg v-else width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8"/><path d="M1 1l22 22"/></svg>
                  </button>
                </div>
              </div>
              <div v-if="formError" class="form-error">⚠️ {{ formError }}</div>
            </div>
            <div class="modal-footer">
              <button class="btn btn-ghost" @click="closePlatformModal">取消</button>
              <button class="btn btn-primary" @click="savePlatform">
                {{ editingPlatform ? '更新' : '添加' }}
              </button>
            </div>
          </div>
        </div>
      </Transition>

      <!-- 编辑模型配置弹窗 -->
      <Transition name="fade">
        <div class="modal-overlay" v-if="editingModel" @click.self="editingModel = null">
          <div class="modal" style="max-width:480px">
            <div class="modal-header">
              <span class="modal-title">配置模型 — {{ editingModel.name }}</span>
              <button class="btn-icon" @click="editingModel = null">
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M18 6L6 18M6 6l12 12"/></svg>
              </button>
            </div>
            <div class="modal-body">
              <!-- 从规格模板套用 -->
              <div class="form-group" style="background:var(--bg-2);padding:10px 12px;border-radius:6px;border:1px solid var(--border-1)">
                <label class="form-label" style="margin-bottom:6px">⚡ 从规格词典一键套用:</label>
                <select @change="onApplySpecToEditModel($event)" class="spec-select-box" style="width:100%">
                  <option value="">-- 选择预设规格以覆盖参数 --</option>
                  <option v-for="spec in modelSpecs" :key="spec.id" :value="spec.id">
                    {{ spec.name }} ({{ formatTok(spec.context_window) }} 上下文 / {{ formatTok(spec.max_tokens) }} 输出)
                  </option>
                </select>
              </div>

              <div class="form-group">
                <label class="form-label">显示名称</label>
                <input class="form-input" v-model="mForm.display_name" placeholder="前端界面显示的别名" />
              </div>

              <div class="form-group">
                <label class="form-label">上下文窗口 (context_window) <span style="color:var(--accent)">*</span></label>
                <input class="form-input" type="number" v-model.number="mForm.context_window"
                  placeholder="如 128000（GPT-4o）、65536（DeepSeek-V3）" min="0" step="1024" />
                <div class="quick-pills">
                  <span class="quick-pill" @click="mForm.context_window = 32768">32k</span>
                  <span class="quick-pill" @click="mForm.context_window = 65536">64k</span>
                  <span class="quick-pill" @click="mForm.context_window = 128000">128k</span>
                  <span class="quick-pill" @click="mForm.context_window = 200000">200k</span>
                  <span class="quick-pill" @click="mForm.context_window = 1048576">1M</span>
                </div>
                <div style="font-size:11px;color:var(--text-4);margin-top:4px">
                  模型能接受的最大输入+输出 Token 总量。Agent 压缩历史时以此为基准。
                </div>
              </div>
              <div class="form-group">
                <label class="form-label">单次最大输出 (max_tokens)</label>
                <input class="form-input" type="number" v-model.number="mForm.max_tokens"
                  placeholder="如 4096、8192" min="256" step="256" />
                <div class="quick-pills">
                  <span class="quick-pill" @click="mForm.max_tokens = 4096">4k</span>
                  <span class="quick-pill" @click="mForm.max_tokens = 8192">8k</span>
                  <span class="quick-pill" @click="mForm.max_tokens = 16384">16k</span>
                </div>
              </div>
            </div>
            <div class="modal-footer">
              <button class="btn btn-ghost" @click="editingModel = null">取消</button>
              <button class="btn btn-primary" @click="saveModelConfig">保存</button>
            </div>
          </div>
        </div>
      </Transition>

      <!-- 弹窗: 添加/编辑模型规格 -->
      <Transition name="fade">
        <div class="modal-overlay" v-if="showAddSpec || editingSpec" @click.self="closeSpecModal">
          <div class="modal" style="max-width:520px">
            <div class="modal-header">
              <span class="modal-title">{{ editingSpec ? '编辑模型规格' : '新建模型规格' }}</span>
              <button class="btn-icon" @click="closeSpecModal">
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M18 6L6 18M6 6l12 12"/></svg>
              </button>
            </div>
            <div class="modal-body">
              <div class="form-group">
                <label class="form-label">规格名称 / 基础型号 *</label>
                <input class="form-input" v-model="specForm.name" placeholder="例如：DeepSeek-V4、Claude-3.7-Sonnet、Qwen-2.5-Coder" />
              </div>

              <div class="form-group">
                <label class="form-label">物理上下文容量 (Context Window, Tokens) *</label>
                <input class="form-input" type="number" v-model.number="specForm.context_window" min="1024" step="1024" />
                <div class="quick-pills">
                  <span class="quick-pill" @click="specForm.context_window = 32768">32k (32,768)</span>
                  <span class="quick-pill" @click="specForm.context_window = 65536">64k (65,536)</span>
                  <span class="quick-pill" @click="specForm.context_window = 128000">128k (128,000)</span>
                  <span class="quick-pill" @click="specForm.context_window = 200000">200k (200,000)</span>
                  <span class="quick-pill" @click="specForm.context_window = 1048576">1M (1,048,576)</span>
                </div>
              </div>

              <div class="form-group">
                <label class="form-label">单次最大输出 (Max Tokens) *</label>
                <input class="form-input" type="number" v-model.number="specForm.max_tokens" min="512" step="512" />
                <div class="quick-pills">
                  <span class="quick-pill" @click="specForm.max_tokens = 4096">4k (4,096)</span>
                  <span class="quick-pill" @click="specForm.max_tokens = 8192">8k (8,192)</span>
                  <span class="quick-pill" @click="specForm.max_tokens = 16384">16k (16,384)</span>
                  <span class="quick-pill" @click="specForm.max_tokens = 32768">32k (32,768)</span>
                </div>
              </div>

              <div class="form-row" style="display:flex;gap:12px">
                <div class="form-group" style="flex:1">
                  <label class="form-label">默认推荐温度</label>
                  <input class="form-input" type="number" v-model.number="specForm.default_temperature" min="0" max="2" step="0.1" />
                </div>
                <div class="form-group" style="flex:1;display:flex;flex-direction:column;justify-content:center">
                  <label class="form-label">特性支持</label>
                  <label style="display:flex;align-items:center;gap:6px;cursor:pointer;margin-top:6px">
                    <input type="checkbox" v-model="specForm.supports_thinking" />
                    <span style="font-size:13px;color:var(--text-1)">🧠 支持深度思考 (Reasoning)</span>
                  </label>
                </div>
              </div>

              <div class="form-group">
                <label class="form-label">规格描述说明</label>
                <textarea class="form-textarea" v-model="specForm.description" placeholder="关于此型号的特点、官方介绍或适用场景备注..." rows="2"></textarea>
              </div>

              <div v-if="specFormError" class="form-error">⚠️ {{ specFormError }}</div>
            </div>
            <div class="modal-footer">
              <button class="btn btn-ghost" @click="closeSpecModal">取消</button>
              <button class="btn btn-primary" @click="saveModelSpec">
                {{ editingSpec ? '更新规格' : '保存规格' }}
              </button>
            </div>
          </div>
        </div>
      </Transition>

      <!-- 确认删除弹窗 (平台 / 模型 / 规格) -->
      <Transition name="fade">
        <div class="modal-overlay" v-if="deletingPlatform || deletingModelItem || deletingSpec" @click.self="clearAllDeleteModals">
          <div class="modal" style="max-width:400px">
            <div class="modal-header">
              <span class="modal-title">⚠️ 确认删除操作</span>
            </div>
            <div class="modal-body">
              <p v-if="deletingPlatform" style="color:var(--text-2)">
                确定要删除平台 <strong style="color:var(--text-1)">{{ deletingPlatform.name }}</strong> 吗？<br/>
                该平台下的所有模型也会被一同删除。
              </p>
              <p v-else-if="deletingModelItem" style="color:var(--text-2)">
                确定要删除模型 <strong style="color:var(--text-1)">{{ deletingModelItem.name }}</strong> 吗？
              </p>
              <p v-else-if="deletingSpec" style="color:var(--text-2)">
                确定要删除模型物理规格 <strong style="color:var(--text-1)">{{ deletingSpec.name }}</strong> 吗？<br/>
                已绑定该规格的现有模型不会受影响，但后续将无法一键代入。
              </p>
            </div>
            <div class="modal-footer">
              <button class="btn btn-ghost" @click="clearAllDeleteModals">取消</button>
              <button class="btn btn-danger" @click="executeConfirmedDelete">确认删除</button>
            </div>
          </div>
        </div>
      </Transition>

      <!-- 请求与连通性诊断详情弹窗 -->
      <Transition name="fade">
        <div class="modal-overlay" v-if="diagModalModel" @click.self="diagModalModel = null">
          <div class="modal diag-modal" style="max-width:680px">
            <div class="modal-header">
              <div style="display:flex;align-items:center;gap:8px">
                <span style="font-size:18px">🔍</span>
                <span class="modal-title">API 请求诊断 — {{ diagModalModel.name }}</span>
              </div>
              <button class="btn-icon btn-xs" @click="diagModalModel = null">
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>
              </button>
            </div>
            <div class="modal-body" style="display:flex;flex-direction:column;gap:14px;max-height:75vh;overflow-y:auto">
              <!-- 概览栏 -->
              <div class="diag-overview-box">
                <div class="diag-row">
                  <span class="diag-label">目标地址:</span>
                  <code class="diag-code">{{ diagModalModel.diagnostics?.endpoint_url || '未知' }}</code>
                </div>
                <div class="diag-row">
                  <span class="diag-label">HTTP 状态:</span>
                  <span :class="diagModalModel.status === 'online' ? 'status-text-online' : 'status-text-offline'">
                    {{ diagModalModel.diagnostics?.http_status ? `HTTP ${diagModalModel.diagnostics.http_status}` : (diagModalModel.status === 'online' ? 'HTTP 200 OK' : '连接失败 (网络/超时/拒绝)') }}
                  </span>
                  <span style="margin-left:auto;font-size:11.5px;color:var(--text-3)">耗时: {{ diagModalModel.latency_ms }} ms</span>
                </div>
              </div>

              <!-- 智能排查建议 -->
              <div class="diag-suggestion-box">
                <div class="diag-suggestion-title">💡 排查分析与建议:</div>
                <ul class="diag-suggestion-list">
                  <li v-if="diagModalModel.diagnostics?.http_status === 401">
                    <strong>401 Unauthorized</strong>: API Key 错误或已过期。请确认中转站填写的令牌是否正确。
                  </li>
                  <li v-else-if="diagModalModel.diagnostics?.http_status === 402 || diagModalModel.diagnostics?.response_body?.includes('insufficient')">
                    <strong>402 Insufficient Balance</strong>: <strong>中转站账户钱包余额不足</strong>。请前往中转站控制台充值。
                  </li>
                  <li v-else-if="diagModalModel.diagnostics?.http_status === 404">
                    <strong>404 Not Found</strong>: 接口路径不存在。请检查平台 Base URL 是否遗漏了 <code>/v1</code>。
                  </li>
                  <li v-else-if="diagModalModel.diagnostics?.http_status === 429">
                    <strong>429 Rate Limit / Quota</strong>: 触发频率限制或中转站额度耗尽。
                  </li>
                  <li v-else-if="diagModalModel.diagnostics?.raw_error?.includes('error sending request') || !diagModalModel.diagnostics?.http_status">
                    <strong>网络连接异常</strong>: 请求未能到达目标服务器，请检查代理网络或域名拼写。
                  </li>
                  <li v-else>
                    请检查下方服务器返回的原始响应，核对模型名称是否与中转站后台一致。
                  </li>
                </ul>
              </div>

              <!-- 请求头与认证 -->
              <div class="diag-section">
                <div class="diag-section-header">📋 发送的 HTTP 请求头 (Headers)</div>
                <pre class="diag-pre"><code>{{ JSON.stringify(diagModalModel.diagnostics?.headers || {}, null, 2) }}</code></pre>
              </div>

              <!-- 请求体 Payload -->
              <div class="diag-section">
                <div class="diag-section-header">📦 发送的请求体 (JSON Body)</div>
                <pre class="diag-pre"><code>{{ JSON.stringify(diagModalModel.diagnostics?.payload || {}, null, 2) }}</code></pre>
              </div>

              <!-- 服务端原始响应 -->
              <div class="diag-section">
                <div class="diag-section-header">💬 服务端返回 / 底层报错详情</div>
                <pre class="diag-pre error-pre"><code>{{ diagModalModel.diagnostics?.response_body || diagModalModel.diagnostics?.raw_error || diagModalModel.error_msg || '无响应内容' }}</code></pre>
              </div>
            </div>
            <div class="modal-footer" style="justify-content:space-between">
              <button class="btn btn-ghost btn-sm" @click="copyDiagnosticsText">
                <span>{{ copiedDiag ? '✅ 已复制完整诊断 JSON' : '📋 复制完整诊断数据' }}</span>
              </button>
              <button class="btn btn-primary btn-sm" @click="diagModalModel = null">关闭</button>
            </div>
          </div>
        </div>
      </Transition>
    </Teleport>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { invoke } from '@tauri-apps/api/core'

const emit = defineEmits<{
  'models-updated': []
}>()

// 标签页状态
const activeTab = ref<'platforms' | 'specs'>('platforms')

interface PlatformData {
  id: string; name: string; base_url: string; api_key: string; created_at: string;
}

interface ModelSpecData {
  id: string;
  name: string;
  context_window: number;
  max_tokens: number;
  supports_thinking: boolean;
  default_temperature: number;
  description: string;
  created_at: string;
}

interface DiagnosticsData {
  endpoint_url: string;
  method: string;
  headers: Record<string, string>;
  payload: any;
  http_status: number | null;
  response_body: string;
  raw_error: string | null;
  latency_ms: number;
}

interface ModelData {
  id: string; platform_id: string; name: string; display_name: string;
  max_tokens: number; temperature: number; enabled: boolean; status: string; latency_ms: number;
  context_window: number;
  error_msg?: string;
  diagnostics?: DiagnosticsData;
}

// 响应式数据
const platforms = ref<PlatformData[]>([])
const models = ref<ModelData[]>([])
const modelSpecs = ref<ModelSpecData[]>([])
const specSearchQuery = ref('')
const selectedPlatformId = ref('')

// 模态弹窗状态
const showAddPlatform = ref(false)
const editingPlatform = ref<PlatformData | null>(null)
const editingModel = ref<ModelData | null>(null)
const showAddSpec = ref(false)
const editingSpec = ref<ModelSpecData | null>(null)

// 删除确认
const deletingPlatform = ref<PlatformData | null>(null)
const deletingModelItem = ref<ModelData | null>(null)
const deletingSpec = ref<ModelSpecData | null>(null)

// 诊断模态
const diagModalModel = ref<ModelData | null>(null)
const copiedDiag = ref(false)

// 快速添加模型
const newModelName = ref('')
const quickSelectedSpecId = ref('')
const showKey = ref(false)
const formError = ref('')
const specFormError = ref('')

// 批量测试
const isTestingCurrentPlatform = ref(false)
const batchTestCompleted = ref(0)
const batchTestTotal = ref(0)
const batchTestSummary = ref('')
const batchTestPlatformId = ref('')

// 表单对象
const pForm = ref({ name: '', base_url: '', api_key: '' })
const mForm = ref({ display_name: '', max_tokens: 4096, context_window: 0 })
const specForm = ref({
  name: '',
  context_window: 128000,
  max_tokens: 8192,
  supports_thinking: false,
  default_temperature: 0.7,
  description: '',
})

// 计算属性
const currentPlatform = computed(() => platforms.value.find(p => p.id === selectedPlatformId.value))
const activeQuickSpec = computed(() => modelSpecs.value.find(s => s.id === quickSelectedSpecId.value))

const filteredModelSpecs = computed(() => {
  if (!specSearchQuery.value.trim()) return modelSpecs.value
  const q = specSearchQuery.value.toLowerCase()
  return modelSpecs.value.filter(
    s => s.name.toLowerCase().includes(q) || s.description.toLowerCase().includes(q)
  )
})

function formatTok(tokens: number) {
  if (!tokens || tokens <= 0) return '0'
  if (tokens >= 1000000) return (tokens / 1000000).toFixed(tokens % 1000000 === 0 ? 0 : 1) + 'M'
  if (tokens >= 1000) return (tokens / 1000).toFixed(tokens % 1000 === 0 ? 0 : 0) + 'k'
  return tokens.toLocaleString()
}

function getPlatformEmoji(name: string) {
  const n = name.toLowerCase()
  if (n.includes('openrouter')) return '🌈'
  if (n.includes('deepseek')) return '🐋'
  if (n.includes('openai') || n.includes('chatgpt')) return '🟢'
  if (n.includes('claude') || n.includes('anthropic')) return '🟠'
  if (n.includes('google') || n.includes('gemini')) return '🔵'
  if (n.includes('硅基') || n.includes('siliconflow')) return '💎'
  return '🌐'
}

function getModelCount(platformId: string) {
  return models.value.filter(m => m.platform_id === platformId).length
}

// ==================== 数据加载 ====================
async function loadPlatforms() {
  platforms.value = await invoke<PlatformData[]>('get_platforms')
}

async function loadModelSpecs() {
  modelSpecs.value = await invoke<ModelSpecData[]>('get_model_specs')
}

async function selectPlatform(id: string) {
  selectedPlatformId.value = id
  models.value = await invoke<ModelData[]>('get_models', { platformId: id })
}

// ==================== 平台增删改 ====================
function editPlatform(p: PlatformData) {
  editingPlatform.value = p
  pForm.value = { name: p.name, base_url: p.base_url, api_key: p.api_key }
  showKey.value = false
  formError.value = ''
}

function closePlatformModal() {
  showAddPlatform.value = false
  editingPlatform.value = null
  pForm.value = { name: '', base_url: '', api_key: '' }
  formError.value = ''
}

async function savePlatform() {
  formError.value = ''
  if (!pForm.value.name.trim()) { formError.value = '平台名称不能为空'; return }
  if (!pForm.value.base_url.trim()) { formError.value = 'API 地址不能为空'; return }
  if (!pForm.value.api_key.trim()) { formError.value = 'API Key 不能为空'; return }

  try {
    if (editingPlatform.value) {
      await invoke('update_platform', {
        id: editingPlatform.value.id,
        name: pForm.value.name,
        baseUrl: pForm.value.base_url,
        apiKey: pForm.value.api_key,
      })
    } else {
      await invoke('add_platform', {
        name: pForm.value.name,
        baseUrl: pForm.value.base_url,
        apiKey: pForm.value.api_key,
      })
    }
    closePlatformModal()
    await loadPlatforms()
    emit('models-updated')
  } catch (e: any) {
    formError.value = '保存失败: ' + (e?.message || e)
  }
}

function confirmDeletePlatform(p: PlatformData) {
  deletingPlatform.value = p
}

// ==================== 模型增删改 ====================
function onQuickSpecSelect() {
  if (activeQuickSpec.value) {
    if (!newModelName.value.trim()) {
      newModelName.value = activeQuickSpec.value.name.toLowerCase()
    }
  }
}

function clearQuickSpec() {
  quickSelectedSpecId.value = ''
}

async function addModel() {
  if (!newModelName.value.trim() || !selectedPlatformId.value) return

  const spec = activeQuickSpec.value
  const ctx = spec ? spec.context_window : 0
  const maxTok = spec ? spec.max_tokens : 4096
  const temp = spec ? spec.default_temperature : 0.7
  const dispName = spec ? spec.name : newModelName.value.trim()

  await invoke('add_model_with_spec', {
    platformId: selectedPlatformId.value,
    name: newModelName.value.trim(),
    displayName: dispName,
    maxTokens: maxTok,
    temperature: temp,
    contextWindow: ctx,
  })

  newModelName.value = ''
  quickSelectedSpecId.value = ''
  await selectPlatform(selectedPlatformId.value)
  emit('models-updated')
}

function editModel(m: ModelData) {
  editingModel.value = m
  mForm.value = {
    display_name: m.display_name || m.name,
    max_tokens: m.max_tokens,
    context_window: m.context_window ?? 0,
  }
}

function onApplySpecToEditModel(e: Event) {
  const specId = (e.target as HTMLSelectElement).value
  const spec = modelSpecs.value.find(s => s.id === specId)
  if (spec) {
    mForm.value.context_window = spec.context_window
    mForm.value.max_tokens = spec.max_tokens
    if (!mForm.value.display_name || mForm.value.display_name === editingModel.value?.name) {
      mForm.value.display_name = spec.name
    }
  }
}

async function saveModelConfig() {
  if (!editingModel.value) return
  await invoke('update_model', {
    id: editingModel.value.id,
    maxTokens: mForm.value.max_tokens,
    contextWindow: mForm.value.context_window,
    displayName: mForm.value.display_name,
  })
  const idx = models.value.findIndex(x => x.id === editingModel.value!.id)
  if (idx >= 0) {
    models.value[idx].display_name = mForm.value.display_name
    models.value[idx].max_tokens = mForm.value.max_tokens
    models.value[idx].context_window = mForm.value.context_window
  }
  editingModel.value = null
  emit('models-updated')
}

function confirmDeleteModel(m: ModelData) {
  deletingModelItem.value = m
}

// ==================== 规格词典增删改 ====================
function openAddSpecModal() {
  editingSpec.value = null
  specForm.value = {
    name: '',
    context_window: 128000,
    max_tokens: 8192,
    supports_thinking: false,
    default_temperature: 0.7,
    description: '',
  }
  specFormError.value = ''
  showAddSpec.value = true
}

function editSpec(spec: ModelSpecData) {
  editingSpec.value = spec
  specForm.value = {
    name: spec.name,
    context_window: spec.context_window,
    max_tokens: spec.max_tokens,
    supports_thinking: spec.supports_thinking,
    default_temperature: spec.default_temperature,
    description: spec.description,
  }
  specFormError.value = ''
  showAddSpec.value = true
}

function closeSpecModal() {
  showAddSpec.value = false
  editingSpec.value = null
  specFormError.value = ''
}

async function saveModelSpec() {
  specFormError.value = ''
  if (!specForm.value.name.trim()) {
    specFormError.value = '规格名称不能为空'
    return
  }
  if (specForm.value.context_window <= 0) {
    specFormError.value = '上下文窗口必须大于 0'
    return
  }

  try {
    await invoke('upsert_model_spec', {
      id: editingSpec.value ? editingSpec.value.id : null,
      name: specForm.value.name.trim(),
      contextWindow: specForm.value.context_window,
      maxTokens: specForm.value.max_tokens,
      supportsThinking: specForm.value.supports_thinking,
      defaultTemperature: specForm.value.default_temperature,
      description: specForm.value.description,
    })
    closeSpecModal()
    await loadModelSpecs()
  } catch (e: any) {
    specFormError.value = '保存失败: ' + (e?.message || e)
  }
}

function confirmDeleteSpec(spec: ModelSpecData) {
  deletingSpec.value = spec
}

async function quickApplySpecToPlatform(spec: ModelSpecData) {
  if (!selectedPlatformId.value) return
  await invoke('add_model_with_spec', {
    platformId: selectedPlatformId.value,
    name: spec.name.toLowerCase(),
    displayName: spec.name,
    maxTokens: spec.max_tokens,
    temperature: spec.default_temperature,
    contextWindow: spec.context_window,
  })
  await selectPlatform(selectedPlatformId.value)
  activeTab.value = 'platforms'
  emit('models-updated')
}

// ==================== 统一确认删除处理器 ====================
function clearAllDeleteModals() {
  deletingPlatform.value = null
  deletingModelItem.value = null
  deletingSpec.value = null
}

async function executeConfirmedDelete() {
  if (deletingPlatform.value) {
    await invoke('delete_platform', { id: deletingPlatform.value.id })
    if (selectedPlatformId.value === deletingPlatform.value.id) {
      selectedPlatformId.value = ''
      models.value = []
    }
    deletingPlatform.value = null
    await loadPlatforms()
    emit('models-updated')
  } else if (deletingModelItem.value) {
    await invoke('delete_model', { id: deletingModelItem.value.id })
    deletingModelItem.value = null
    if (selectedPlatformId.value) {
      await selectPlatform(selectedPlatformId.value)
    }
    emit('models-updated')
  } else if (deletingSpec.value) {
    await invoke('delete_model_spec', { idOrName: deletingSpec.value.id })
    deletingSpec.value = null
    await loadModelSpecs()
  }
}

// ==================== 诊断与测试 ====================
function openDiagnostics(m: ModelData) {
  diagModalModel.value = m
  copiedDiag.value = false
}

function copyDiagnosticsText() {
  if (!diagModalModel.value) return
  const data = {
    model: diagModalModel.value.name,
    status: diagModalModel.value.status,
    latency_ms: diagModalModel.value.latency_ms,
    error_msg: diagModalModel.value.error_msg,
    diagnostics: diagModalModel.value.diagnostics
  }
  navigator.clipboard.writeText(JSON.stringify(data, null, 2)).then(() => {
    copiedDiag.value = true
    setTimeout(() => { copiedDiag.value = false }, 2000)
  })
}

async function testSingleModel(m: ModelData): Promise<boolean> {
  const idx = models.value.findIndex(x => x.id === m.id)
  if (idx >= 0) {
    models.value[idx].status = 'testing'
    models.value[idx].error_msg = ''
  }
  try {
    const result = await invoke<{ status: string; latency_ms: number; message: string; diagnostics?: DiagnosticsData }>('test_model', { modelId: m.id })
    if (idx >= 0) {
      models.value[idx].status = result.status
      models.value[idx].latency_ms = result.latency_ms
      models.value[idx].error_msg = result.status === 'offline' ? result.message : ''
      models.value[idx].diagnostics = result.diagnostics
    }
    return result.status === 'online'
  } catch (err: any) {
    if (idx >= 0) {
      models.value[idx].status = 'offline'
      models.value[idx].error_msg = String(err)
    }
    return false
  }
}

async function testCurrentPlatform() {
  if (isTestingCurrentPlatform.value || !selectedPlatformId.value) return
  const platformId = selectedPlatformId.value
  const targets = [...models.value]
  isTestingCurrentPlatform.value = true
  batchTestCompleted.value = 0
  batchTestTotal.value = targets.length
  batchTestSummary.value = ''
  batchTestPlatformId.value = platformId
  let onlineCount = 0
  try {
    for (const model of targets) {
      if (await testSingleModel(model)) onlineCount += 1
      batchTestCompleted.value += 1
    }
    batchTestSummary.value = `已检测 ${targets.length} 个：${onlineCount} 可用，${targets.length - onlineCount} 不可用`
  } finally {
    isTestingCurrentPlatform.value = false
    if (selectedPlatformId.value === platformId) await selectPlatform(platformId)
  }
}

onMounted(async () => {
  await Promise.all([loadPlatforms(), loadModelSpecs()])
})
</script>

<style scoped>
.manager-layout {
  display: flex;
  flex-direction: column;
  height: 100%;
  background: var(--bg-0);
}

/* ---- 顶部主标签页切换 ---- */
.top-nav-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 8px 18px;
  background: var(--bg-1);
  border-bottom: 1px solid var(--border-1);
  flex-shrink: 0;
}

.nav-tabs {
  display: flex;
  gap: 8px;
}

.nav-tab-btn {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 8px 16px;
  border-radius: 8px;
  font-size: 13.5px;
  font-weight: 550;
  color: var(--text-3);
  background: transparent;
  border: 1px solid transparent;
  cursor: pointer;
  transition: all 0.18s;
}

.nav-tab-btn:hover {
  background: var(--surface-1);
  color: var(--text-1);
}

.nav-tab-btn.active {
  background: var(--surface-2);
  color: var(--text-1);
  border-color: rgba(255, 255, 255, 0.12);
  box-shadow: var(--shadow-sm);
}

.tab-badge {
  font-size: 11px;
  padding: 1px 6px;
  border-radius: 99px;
  background: rgba(255, 255, 255, 0.08);
  color: var(--text-3);
}

.nav-right-tip {
  font-size: 12px;
  color: var(--text-4);
}

/* ---- 主体内容容器 ---- */
.main-content-body {
  flex: 1;
  display: flex;
  overflow: hidden;
}

.tab-content-wrap {
  width: 100%;
  height: 100%;
}

.platforms-view {
  display: flex;
}

/* ---- 左侧平台面板 ---- */
.platform-panel {
  width: 320px;
  min-width: 320px;
  border-right: 1px solid var(--border-1);
  background: var(--bg-1);
  display: flex;
  flex-direction: column;
}

.panel-header {
  padding: 18px 16px;
  border-bottom: 1px solid var(--border-1);
  display: flex;
  justify-content: space-between;
  align-items: center;
  flex-shrink: 0;
}

.model-header-actions {
  display: flex;
  align-items: center;
  gap: 10px;
}
.batch-test-summary {
  color: var(--text-2);
  font-size: 12px;
  white-space: nowrap;
}
.panel-title {
  font-size: 15px;
  font-weight: 600;
  color: var(--text-1);
}

.panel-subtitle {
  font-size: 12px;
  color: var(--text-3);
  margin-top: 3px;
}

.platform-list {
  flex: 1;
  overflow-y: auto;
  padding: 8px;
}

.platform-item {
  padding: 12px 14px;
  border-radius: var(--radius-md);
  cursor: pointer;
  margin-bottom: 4px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  transition: all 0.18s;
  border: 1px solid transparent;
}

.platform-item:hover {
  background: var(--surface-2);
}

.platform-item.active {
  background: #27272a;
  border-color: rgba(255, 255, 255, 0.12);
}

.p-left {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
  flex: 1;
}

.p-icon {
  font-size: 22px;
  flex-shrink: 0;
}

.p-info { min-width: 0; }

.p-name {
  font-size: 14px;
  font-weight: 600;
  color: var(--text-1);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.p-url {
  font-size: 10px;
  color: var(--text-4);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  margin-top: 2px;
  font-family: monospace;
}

.p-actions {
  display: flex;
  align-items: center;
  gap: 4px;
  flex-shrink: 0;
}

.p-model-count {
  font-size: 10px;
  color: var(--text-4);
  background: var(--surface-1);
  border: 1px solid var(--border-1);
  padding: 2px 6px;
  border-radius: 99px;
  margin-right: 4px;
}

.p-actions .btn-icon {
  opacity: 0;
  transition: opacity 0.15s;
}

.platform-item:hover .p-actions .btn-icon {
  opacity: 1;
}

.btn-xs { padding: 4px; border-radius: 4px; }
.btn-del:hover { color: var(--red-light) !important; background: rgba(255,71,87,0.1) !important; }

/* ---- 右侧模型面板 ---- */
.model-panel {
  flex: 1;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  padding: 24px;
  gap: 20px;
  overflow-y: auto;
}

/* 快速添加栏 */
.add-model-container {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.add-model-bar {
  display: flex;
  gap: 10px;
  align-items: center;
}

.spec-picker-wrap {
  position: relative;
  display: flex;
  align-items: center;
  background: var(--bg-2);
  border: 1px solid var(--border-2);
  border-radius: var(--radius-sm);
  padding: 0 10px;
}

.spec-picker-icon {
  font-size: 14px;
  margin-right: 4px;
}

.spec-select-box {
  background: transparent;
  border: none;
  color: var(--text-2);
  font-size: 12.5px;
  padding: 9px 0;
  outline: none;
  cursor: pointer;
  max-width: 260px;
}

.spec-select-box option {
  background: var(--bg-1);
  color: var(--text-1);
}

.add-model-input-wrap {
  flex: 1;
  display: flex;
  align-items: center;
  gap: 8px;
  background: var(--bg-2);
  border: 1px solid var(--border-2);
  border-radius: var(--radius-sm);
  padding: 0 12px;
  transition: border-color 0.18s, box-shadow 0.18s;
  color: var(--text-3);
}

.add-model-input-wrap:focus-within {
  border-color: rgba(255, 255, 255, 0.25);
  box-shadow: 0 0 0 2px rgba(255, 255, 255, 0.06);
}

.add-model-input {
  flex: 1;
  background: none;
  border: none;
  outline: none;
  color: var(--text-1);
  font-family: var(--font);
  font-size: 14px;
  padding: 10px 0;
}

.add-model-input::placeholder { color: var(--text-4); }

.selected-spec-pill-bar {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 12px;
  background: rgba(129, 140, 248, 0.08);
  border: 1px solid rgba(129, 140, 248, 0.2);
  border-radius: 6px;
  font-size: 12px;
}

.spec-pill-label {
  color: #c7d2fe;
}

.spec-tag {
  background: rgba(255, 255, 255, 0.08);
  padding: 2px 7px;
  border-radius: 4px;
  font-size: 11px;
  color: var(--text-2);
}

.spec-tag.thinking {
  background: rgba(168, 85, 247, 0.18);
  color: #d8b4fe;
  border: 1px solid rgba(168, 85, 247, 0.3);
}

.btn-clear-spec {
  margin-left: auto;
  background: none;
  border: none;
  color: var(--text-4);
  cursor: pointer;
  font-size: 12px;
  padding: 2px 6px;
}
.btn-clear-spec:hover { color: var(--text-1); }

/* 模型列表 */
.model-grid {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.model-card {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 14px 18px;
  background: var(--bg-1);
  border: 1px solid var(--border-1);
  border-radius: var(--radius-md);
  transition: border-color 0.18s, box-shadow 0.18s, transform 0.18s;
}

.model-card:hover {
  border-color: var(--border-3);
  box-shadow: var(--shadow-sm);
  transform: translateX(2px);
}

.m-left {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
}

.m-icon-box {
  width: 10px;
  height: 10px;
  border-radius: 50%;
  flex-shrink: 0;
}

.m-icon-box.online .status-dot { background: var(--green); box-shadow: 0 0 8px var(--green); }
.m-icon-box.offline .status-dot { background: var(--red); }
.m-icon-box.testing .status-dot { background: var(--orange); animation: pulse 1s ease infinite; }
.m-icon-box.unknown .status-dot { background: var(--text-4); }

.status-dot {
  display: block;
  width: 10px;
  height: 10px;
  border-radius: 50%;
}

.m-info { min-width: 0; }

.m-name {
  font-size: 14px;
  font-weight: 600;
  color: var(--text-1);
  font-family: 'JetBrains Mono', monospace;
}

.m-meta {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 3px;
}

.m-tokens {
  font-size: 11px;
  color: var(--text-4);
}

.m-actions {
  display: flex;
  gap: 6px;
  flex-shrink: 0;
}

.empty-models {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 8px;
  color: var(--text-3);
  font-size: 14px;
}

.empty-hint {
  display: flex;
  flex-direction: column;
  align-items: center;
  padding: 40px 16px;
  gap: 10px;
  color: var(--text-4);
  font-size: 13px;
}

.select-hint {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  text-align: center;
  gap: 14px;
  color: var(--text-3);
}

.select-hint h3 {
  font-size: 18px;
  font-weight: 600;
  color: var(--text-2);
}

.select-hint p {
  font-size: 13px;
  color: var(--text-4);
  line-height: 1.8;
}

.hint-glow {
  width: 80px;
  height: 80px;
  background: linear-gradient(135deg, rgba(108,99,255,0.1), rgba(0,212,170,0.1));
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  box-shadow: 0 0 40px var(--accent-glow);
}

/* ==================== 规格词典视图样式 ==================== */
.specs-view {
  overflow-y: auto;
  padding: 24px;
}

.specs-container {
  max-width: 1180px;
  margin: 0 auto;
  display: flex;
  flex-direction: column;
  gap: 20px;
}

.specs-header {
  display: flex;
  justify-content: space-between;
  align-items: flex-end;
  border-bottom: 1px solid var(--border-1);
  padding-bottom: 16px;
}

.specs-main-title {
  font-size: 20px;
  font-weight: 700;
  color: var(--text-1);
}

.specs-main-desc {
  font-size: 13px;
  color: var(--text-3);
  margin-top: 6px;
}

.specs-header-actions {
  display: flex;
  align-items: center;
  gap: 12px;
}

.spec-search-wrap {
  position: relative;
}

.spec-search-input {
  background: var(--bg-1);
  border: 1px solid var(--border-2);
  border-radius: 6px;
  padding: 7px 12px;
  font-size: 13px;
  color: var(--text-1);
  outline: none;
  width: 200px;
}
.spec-search-input:focus { border-color: rgba(255, 255, 255, 0.3); }

.specs-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(340px, 1fr));
  gap: 16px;
}

.spec-card {
  background: var(--bg-1);
  border: 1px solid var(--border-1);
  border-radius: var(--radius-md);
  padding: 18px;
  display: flex;
  flex-direction: column;
  justify-content: space-between;
  gap: 16px;
  transition: all 0.2s ease;
}

.spec-card:hover {
  border-color: var(--border-3);
  box-shadow: var(--shadow-sm);
  transform: translateY(-2px);
}

.spec-card-top {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.spec-title-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}

.spec-name {
  font-size: 16px;
  font-weight: 700;
  color: #f4f4f5;
  font-family: 'JetBrains Mono', monospace;
}

.badge-thinking {
  font-size: 11px;
  background: rgba(168, 85, 247, 0.18);
  border: 1px solid rgba(168, 85, 247, 0.35);
  color: #d8b4fe;
  padding: 2px 8px;
  border-radius: 99px;
  font-weight: 600;
}

.badge-standard {
  font-size: 11px;
  background: rgba(59, 130, 246, 0.14);
  border: 1px solid rgba(59, 130, 246, 0.3);
  color: #93c5fd;
  padding: 2px 8px;
  border-radius: 99px;
}

.spec-desc {
  font-size: 12.5px;
  color: var(--text-3);
  line-height: 1.5;
}

.spec-props-grid {
  display: grid;
  grid-template-columns: 1fr 1fr 1fr;
  gap: 8px;
  background: var(--bg-2);
  padding: 10px 12px;
  border-radius: 6px;
  border: 1px solid var(--border-1);
}

.spec-prop-item {
  display: flex;
  flex-direction: column;
  gap: 3px;
}

.prop-label {
  font-size: 10.5px;
  color: var(--text-4);
}

.prop-val {
  font-size: 13px;
  font-weight: 600;
  color: var(--text-2);
}

.prop-val.highlight {
  color: #818cf8;
}

.spec-card-footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  border-top: 1px solid var(--border-1);
  padding-top: 12px;
}

.spec-card-btns {
  display: flex;
  gap: 6px;
}

.btn-quick-apply {
  font-size: 11.5px;
  padding: 5px 10px;
}

/* ---- Quick Pills ---- */
.quick-pills {
  display: flex;
  gap: 6px;
  margin-top: 6px;
}

.quick-pill {
  font-size: 11px;
  background: var(--surface-1);
  border: 1px solid var(--border-2);
  color: var(--text-3);
  padding: 2px 7px;
  border-radius: 4px;
  cursor: pointer;
  transition: all 0.15s;
}

.quick-pill:hover {
  background: var(--surface-2);
  color: var(--text-1);
  border-color: rgba(255, 255, 255, 0.2);
}

.form-textarea {
  width: 100%;
  background: var(--bg-2);
  border: 1px solid var(--border-2);
  border-radius: var(--radius-sm);
  padding: 8px 12px;
  color: var(--text-1);
  font-family: var(--font);
  font-size: 13px;
  resize: vertical;
  outline: none;
}
.form-textarea:focus { border-color: rgba(255, 255, 255, 0.25); }

/* ---- Key toggle ---- */
.key-wrap { position: relative; }

.key-toggle {
  position: absolute;
  right: 10px;
  top: 50%;
  transform: translateY(-50%);
  background: none;
  border: none;
  cursor: pointer;
  color: var(--text-3);
  padding: 4px;
  display: flex;
  align-items: center;
}
.key-toggle:hover { color: var(--text-1); }

.form-error {
  font-size: 13px;
  color: var(--red-light);
  background: rgba(255,71,87,0.08);
  border: 1px solid rgba(255,71,87,0.2);
  border-radius: var(--radius-sm);
  padding: 8px 12px;
}

.btn-sm { padding: 5px 11px; font-size: 12px; }

/* ---- Diagnostics Badge & Modal Styles ---- */
.btn-diag-badge {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 1px 7px;
  border-radius: 4px;
  background: rgba(108, 99, 255, 0.16);
  border: 1px solid rgba(108, 99, 255, 0.35);
  color: #a5b4fc;
  font-size: 11px;
  font-weight: 550;
  cursor: pointer;
  transition: all 0.15s ease;
}
.btn-diag-badge:hover {
  background: rgba(108, 99, 255, 0.3);
  color: #ffffff;
  border-color: #818cf8;
}

.diag-modal {
  background: #18181c;
  border: 1px solid rgba(255, 255, 255, 0.14);
}

.diag-overview-box {
  background: #1f1f26;
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 8px;
  padding: 10px 14px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.diag-row {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12.5px;
}

.diag-label {
  color: var(--text-3);
  font-size: 12px;
  min-width: 65px;
}

.diag-code {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, monospace;
  color: #e4e4e7;
  font-size: 12px;
  word-break: break-all;
}

.status-text-online {
  color: #10b981;
  font-weight: 600;
  font-size: 12.5px;
}

.status-text-offline {
  color: #f87171;
  font-weight: 600;
  font-size: 12.5px;
}

.diag-suggestion-box {
  background: rgba(251, 191, 36, 0.08);
  border: 1px solid rgba(251, 191, 36, 0.25);
  border-radius: 8px;
  padding: 10px 14px;
}

.diag-suggestion-title {
  font-size: 12.5px;
  font-weight: 650;
  color: #fbbf24;
  margin-bottom: 6px;
}

.diag-suggestion-list {
  margin: 0;
  padding-left: 18px;
  font-size: 12px;
  color: #e4e4e7;
  line-height: 1.6;
}

.diag-section {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.diag-section-header {
  font-size: 12px;
  font-weight: 600;
  color: var(--text-2);
}

.diag-pre {
  background: #131316;
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 6px;
  padding: 10px 12px;
  margin: 0;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, monospace;
  font-size: 11.5px;
  color: #d4d4d8;
  max-height: 180px;
  overflow-y: auto;
  white-space: pre-wrap;
  word-break: break-all;
}

.error-pre {
  color: #fca5a5;
  background: rgba(239, 68, 68, 0.06);
  border-color: rgba(239, 68, 68, 0.2);
}
</style>
