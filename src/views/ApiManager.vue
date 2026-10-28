<template>
  <div class="manager-layout">
    <!-- 顶部主标签页切换 -->
    <header class="top-nav-bar">
      <div class="nav-left-group">
        <button class="back-btn" type="button" title="返回对话" @click="goBack">
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
            <line x1="19" y1="12" x2="5" y2="12"></line>
            <polyline points="12 19 5 12 12 5"></polyline>
          </svg>
          <span>返回对话</span>
        </button>
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
      </div>
      <div class="nav-right-group" v-if="activeTab === 'platforms'">
        <ModelConfigTransfer @imported="onConfigImported" />
      </div>
      <div class="nav-right-tip" v-else>
        <span>⚡ 物理规格全局统一维护，修改后所有引用该规格的配置均可继承</span>
      </div>
    </header>

    <!-- 主体内容区 -->
    <main class="main-content-body">
      <p v-if="configRefreshError" class="form-error" role="alert">{{ configRefreshError }}</p>
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
              <div class="p-header-row">
                <div class="p-left">
                  <div class="p-icon">{{ getPlatformEmoji(p.name) }}</div>
                  <div class="p-name-line">
                    <span class="p-name" :title="p.name">{{ p.name }}</span>
                    <span v-if="getKeyBadge(p.api_key).total > 0" class="p-key-badge" :class="{ 'has-disabled': getKeyBadge(p.api_key).disabled > 0 }" :title="getKeyBadge(p.api_key).title">
                      🔑 {{ getKeyBadge(p.api_key).label }}
                    </span>
                  </div>
                </div>
                <div class="p-actions">
                  <button class="btn-icon btn-xs" @click.stop="openFetchRemoteModelsModal(p.id)" title="从接口拉取模型">
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21.5 2v6h-6M2.5 22v-6h6M2 11.5a10 10 0 0 1 18.8-4.3M22 12.5a10 10 0 0 1-18.8 4.2"/></svg>
                  </button>
                  <button class="btn-icon btn-xs" @click.stop="editPlatform(p)" title="编辑">
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z"/></svg>
                  </button>
                  <button class="btn-icon btn-xs btn-del" @click.stop="confirmDeletePlatform(p)" title="删除">
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2"/></svg>
                  </button>
                </div>
              </div>
              <div class="p-meta-row">
                <span class="p-model-count">{{ getModelCount(p.id) }} 个模型</span>
                <span class="p-url" :title="p.base_url">{{ p.base_url }}</span>
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
                <button class="btn btn-accent btn-sm btn-fetch-models" type="button" :disabled="isFetchingRemoteModels" @click="openFetchRemoteModelsModal(selectedPlatformId)" title="通过 GET /v1/models 自动拉取平台所有可用模型">
                  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" :class="{ spinning: isFetchingRemoteModels }">
                    <path d="M21.5 2v6h-6M2.5 22v-6h6M2 11.5a10 10 0 0 1 18.8-4.3M22 12.5a10 10 0 0 1-18.8 4.2"/>
                  </svg>
                  <span>{{ isFetchingRemoteModels ? '正在拉取…' : '🔄 从接口拉取模型' }}</span>
                </button>
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
                  <SpecSelectDropdown
                    v-model="quickSelectedSpecId"
                    :specs="modelSpecs"
                    placeholder="从规格词典自动代入参数"
                    size="md"
                  />
                </div>

                <div class="add-model-input-wrap">
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 5v14M5 12h14"/></svg>
                  <input
                    class="add-model-input"
                    v-model="newModelName"
                    ref="newModelInput"
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
                      <span class="m-tokens" :style="{ color: m.context_window > 0 ? 'var(--accent-light)' : 'var(--text-4)' }">
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
              <p style="font-size:12px;color:var(--text-4)">你可以手动输入模型标识，或者直接点击下方按钮从平台接口自动拉取并写入</p>
              <button class="btn btn-primary" style="margin-top:14px;gap:8px" @click="openFetchRemoteModelsModal(selectedPlatformId)" :disabled="isFetchingRemoteModels">
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" :class="{ spinning: isFetchingRemoteModels }">
                  <path d="M21.5 2v6h-6M2.5 22v-6h6M2 11.5a10 10 0 0 1 18.8-4.3M22 12.5a10 10 0 0 1-18.8 4.2"/>
                </svg>
                <span>{{ isFetchingRemoteModels ? '正在拉取远端模型…' : '🔄 从接口一键拉取全部模型' }}</span>
              </button>
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
                  title="套用参数后填写模型标识，再确认添加"
                >
                  使用此规格
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
                <div class="key-header-row">
                  <label class="form-label" style="margin-bottom:0">API Key 管理 *</label>
                  <div class="key-header-actions">
                    <span v-if="currentKeyItems.length > 0" class="key-count-badge" :class="{ 'has-disabled': disabledKeyCount > 0 }">
                      ⚡ {{ enabledKeyCount }}/{{ currentKeyItems.length }} 启用 {{ disabledKeyCount > 0 ? `(${disabledKeyCount} 个已禁用/跳过)` : '(自动轮换)' }}
                    </span>
                    <button
                      type="button"
                      class="btn-text-action"
                      @click="rawKeyInputMode = !rawKeyInputMode"
                      :title="rawKeyInputMode ? '切换到卡片式开关列表' : '切换到纯文本批量输入'"
                    >
                      {{ rawKeyInputMode ? '📱 开关列表' : '📝 批量编辑' }}
                    </button>
                  </div>
                </div>

                <!-- 模式 A：卡片式列表与开关 (默认当有Key且非批量模式时展示) -->
                <div v-if="!rawKeyInputMode && currentKeyItems.length > 0" class="key-management-box">
                  <div class="key-items-list">
                    <div
                      v-for="(item, idx) in currentKeyItems"
                      :key="idx"
                      class="key-item-card"
                      :class="{ 'is-disabled': !item.enabled }"
                    >
                      <div class="key-item-left">
                        <span class="key-idx">#{{ idx + 1 }}</span>
                        <span class="key-text" :title="item.key">
                          {{ showKey ? item.key : maskKey(item.key) }}
                        </span>
                      </div>
                      <div class="key-item-right">
                        <span class="key-state-tag" :class="{ active: item.enabled }">
                          {{ item.enabled ? '🟢 轮询中' : '⚪ 已跳过' }}
                        </span>
                        <button
                          type="button"
                          class="key-switch-btn"
                          :class="{ active: item.enabled }"
                          :title="item.enabled ? '点击临时禁用该 Key (轮询调度时将跳过)' : '点击重新启用该 Key'"
                          @click="toggleKeyItem(idx)"
                        >
                          <span class="key-switch-thumb"></span>
                        </button>
                        <button
                          type="button"
                          class="btn-icon-xs btn-del-key"
                          title="删除此 Key"
                          @click="removeKeyItem(idx)"
                        >
                          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>
                        </button>
                      </div>
                    </div>
                  </div>

                  <!-- 快捷添加单条 Key -->
                  <div class="add-key-inline">
                    <input
                      v-model="newKeyInput"
                      class="form-input form-input-sm"
                      placeholder="输入新的 API Key (如 sk-... 或 AIzaSy...)"
                      @keydown.enter.prevent="addSingleKey"
                    />
                    <button
                      type="button"
                      class="btn btn-secondary btn-sm"
                      :disabled="!newKeyInput.trim()"
                      @click="addSingleKey"
                    >
                      + 添加 Key
                    </button>
                  </div>
                </div>

                <!-- 模式 B：纯文本多行编辑器 -->
                <div v-else class="key-wrap">
                  <textarea
                    class="form-textarea key-textarea"
                    v-model="pForm.api_key"
                    :style="{ filter: showKey ? 'none' : 'blur(4.5px)', transition: 'filter 0.2s ease' }"
                    placeholder="支持配置多个 API Key (每行一个或逗号/分号隔开)&#10;sk-key1...&#10;sk-key2..."
                    rows="3"
                  ></textarea>
                  <button class="key-toggle" @click="showKey = !showKey" type="button" :title="showKey ? '隐藏 API Key' : '显示 API Key'">
                    <svg v-if="!showKey" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"/><circle cx="12" cy="12" r="3"/></svg>
                    <svg v-else width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8"/><path d="M1 1l22 22"/></svg>
                  </button>
                </div>

                <div class="form-tip">
                  💡 <strong>每个 Key 均带独立开关</strong>：默认全部开启。临时关闭某个 Key 后，在模型调用与轮询时会自动跳过它，无须删除 Key。
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
              <div class="form-group" style="background:rgba(var(--theme-foreground-rgb), 0.02);padding:10px 12px;border-radius:8px;border:1px solid rgba(var(--theme-foreground-rgb), 0.08)">
                <label class="form-label" style="margin-bottom:8px;font-size:12px;color:var(--text-2)">⚡ 从规格词典一键套用:</label>
                <SpecSelectDropdown
                  :specs="modelSpecs"
                  placeholder="-- 选择预设规格以覆盖参数 --"
                  @change="onApplySpecToEditModel"
                  style="width: 100%"
                />
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
              <div class="form-group">
                <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:6px">
                  <label class="form-label" style="margin:0">Provider 能力配置（JSON）</label>
                  <span style="font-size:11px;color:var(--text-3)">标准协议流派模板：</span>
                </div>
                <div class="quick-pills" style="margin-bottom:8px">
                  <span class="quick-pill" @click="applyStandardCapability('kimi_k3')" title="Kimi / 商汤托管推理协议：支持 low/medium/high/max 多档深度调节，自动剔除冲突采样参数">🌙 Kimi / Moonshot 协议</span>
                  <span class="quick-pill" @click="applyStandardCapability('reasoning_effort')" title="OpenAI GPT-5 / o系列新推理协议：支持 reasoning_effort 深度调节，自动映射 max_completion_tokens">🏛️ OpenAI / GPT-5 协议</span>
                  <span class="quick-pill" @click="applyStandardCapability('deepseek_r1')" title="DeepSeek (R系列) / QwQ 开源推理协议：原生 reasoning_content 思考流，纯净免采样">🚀 DeepSeek / 开源推理协议</span>
                  <span class="quick-pill" @click="applyStandardCapability('budget_tokens')" title="Anthropic Claude (4 / 5 系列) Extended Thinking：支持动态 Token 预算控制与消息严格交替">🎭 Anthropic 思考预算协议</span>
                  <span class="quick-pill" @click="applyStandardCapability('google_gemini')" title="Google Gemini (3.x 系列) 协议：高度标准化的通用规约与结构化输出">✨ Google Gemini 协议</span>
                  <span class="quick-pill" @click="applyStandardCapability('json_mode')" title="火山方舟 / 字节协议：强制 JSON 结构化输出规范">📦 字节/火山方舟协议</span>
                  <span class="quick-pill" @click="applyStandardCapability('thinking_bool')" title="支持 thinking: true/false 简单开关协议">🔘 思考布尔开关</span>
                  <span class="quick-pill" @click="applyStandardCapability('standard')" title="无额外字段，最纯粹的通用兼容模式">⚡ 经典标准兼容 ({})</span>
                </div>
                <textarea class="form-input" v-model="mForm.provider_capabilities" rows="10" spellcheck="false"
                  placeholder='{}'></textarea>
                <div style="font-size:11px;color:var(--text-4);margin-top:4px;line-height:1.5">
                  空对象 <code>{}</code> 为纯净标准请求；点击上方标准模板可一键载入参数规约（如思考档位映射、Token 预算控制、自动清除冲突参数等）。
                </div>
              </div>
              <div v-if="modelConfigError" class="form-error">⚠️ {{ modelConfigError }}</div>
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
                <label class="form-label">规格名称 / 容量档位 *</label>
                <input class="form-input" v-model="specForm.name" placeholder="例如：128k 主流标配档、200k 长上下文档、32k 本地轻量档" />
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

      <!-- 弹窗: 从接口拉取与批量导入模型 -->
      <Transition name="fade">
        <div class="modal-overlay" v-if="showRemoteModelsModal" @click.self="showRemoteModelsModal = false">
          <div class="modal remote-models-modal">
            <div class="modal-header">
              <div class="modal-title-wrap">
                <span class="modal-title">🔄 从接口同步可用模型</span>
                <span class="platform-badge" v-if="targetPlatformForFetch">{{ targetPlatformForFetch.name }}</span>
              </div>
              <button class="btn-icon" @click="showRemoteModelsModal = false">
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M18 6L6 18M6 6l12 12"/></svg>
              </button>
            </div>

            <!-- Loading 状态 -->
            <div class="modal-body modal-loading-state" v-if="isFetchingRemoteModels">
              <div class="loading-spinner-large"></div>
              <h4>正在向平台接口获取模型列表…</h4>
              <p class="text-sub">正在请求 <code>{{ targetPlatformForFetch?.base_url }}/models</code></p>
              <p class="text-sub-hint">系统将自动结合本地【规格词典】匹配上下文容量与推荐参数</p>
            </div>

            <!-- 报错状态 -->
            <div class="modal-body modal-error-state" v-else-if="remoteModelFetchError">
              <div class="error-icon">⚠️</div>
              <h4>拉取模型列表失败</h4>
              <p class="error-detail">{{ remoteModelFetchError }}</p>
              <div class="error-actions">
                <button class="btn btn-ghost" @click="showRemoteModelsModal = false">关闭</button>
                <button class="btn btn-primary" @click="retryFetchRemoteModels">重新拉取</button>
              </div>
            </div>

            <!-- 成功获取到模型列表 -->
            <div class="modal-body remote-models-body" v-else-if="remoteFetchResult">
              <div class="fetch-summary-bar">
                <div class="endpoint-indicator">
                  <span class="method-tag">GET</span>
                  <span class="endpoint-url" :title="remoteFetchResult.endpoint_url">{{ remoteFetchResult.endpoint_url }}</span>
                </div>
                <div class="fetch-stats">
                  <span>共检索到 <strong>{{ remoteFetchResult.total }}</strong> 个模型</span>
                  <span class="badge-new-count" v-if="remoteFetchResult.new_count > 0">新模型 +{{ remoteFetchResult.new_count }}</span>
                </div>
              </div>

              <div class="remote-toolbar">
                <div class="remote-search-box">
                  <input
                    v-model="remoteModelSearchQuery"
                    class="form-input remote-search-input"
                    placeholder="🔍 搜索过滤模型标识或名称..."
                  />
                </div>
                <div class="remote-batch-buttons">
                  <button type="button" class="btn-tool" @click="selectAllRemoteModels(true)">全选</button>
                  <button type="button" class="btn-tool" @click="selectAllRemoteModels(false)">全不选</button>
                  <button type="button" class="btn-tool" @click="selectOnlyNewRemoteModels" v-if="remoteFetchResult.new_count > 0">
                    仅选新模型 ({{ remoteFetchResult.new_count }})
                  </button>
                </div>
              </div>

              <!-- 模型列表卡片 -->
              <div class="remote-models-list">
                <div
                  v-for="m in filteredRemoteModels"
                  :key="m.id"
                  class="remote-model-row"
                  :class="{ 'is-selected': m.selected, 'is-existing': m.exists }"
                  @click="m.selected = !m.selected"
                >
                  <div class="remote-col-check">
                    <input type="checkbox" v-model="m.selected" @click.stop class="custom-checkbox" />
                  </div>

                  <div class="remote-col-info">
                    <div class="remote-model-name-row">
                      <span class="remote-model-id">{{ m.name }}</span>
                      <span v-if="!m.exists" class="badge-tag badge-tag-new">✨ 新增</span>
                      <span v-else class="badge-tag badge-tag-exist">✓ 已在库中</span>
                      <span v-if="m.supports_thinking" class="badge-tag badge-tag-thinking">🧠 深度思考</span>
                    </div>
                    <div class="remote-model-sub-row">
                      <span v-if="m.matched_spec_name" class="spec-match-tag">
                        ⚡ 匹配规格: <strong>{{ m.matched_spec_name }}</strong>
                      </span>
                      <span class="param-tag">窗口: {{ formatTok(m.context_window) }}</span>
                      <span class="param-tag">输出: {{ formatTok(m.max_tokens) }}</span>
                      <span class="param-tag">温度: {{ m.temperature }}</span>
                    </div>
                  </div>
                </div>
                <div v-if="filteredRemoteModels.length === 0" class="empty-search-hint">
                  未找到匹配 "{{ remoteModelSearchQuery }}" 的模型
                </div>
              </div>

              <div v-if="importSuccessMsg" class="import-success-toast">
                {{ importSuccessMsg }}
              </div>
            </div>

            <!-- Footer -->
            <div class="modal-footer remote-modal-footer" v-if="!isFetchingRemoteModels && !remoteModelFetchError">
              <div class="footer-left-hint">
                <span>已选中 <strong>{{ selectedRemoteModelsCount }}</strong> / {{ remoteFetchResult?.total || 0 }} 个模型</span>
              </div>
              <div class="footer-actions">
                <button class="btn btn-ghost" @click="showRemoteModelsModal = false">取消</button>
                <button
                  class="btn btn-primary"
                  :disabled="selectedRemoteModelsCount === 0 || isImportingRemoteModels"
                  @click="executeImportRemoteModels"
                >
                  <svg v-if="isImportingRemoteModels" class="spinning" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M21 12a9 9 0 1 1-6.219-8.56"/></svg>
                  <span>{{ isImportingRemoteModels ? '正在写入数据库…' : `导入选中的 ${selectedRemoteModelsCount} 个模型到 SQLite` }}</span>
                </button>
              </div>
            </div>
          </div>
        </div>
      </Transition>
    </Teleport>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, nextTick, onMounted } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import ModelConfigTransfer from '../components/ModelConfigTransfer.vue'
import SpecSelectDropdown from '../components/common/SpecSelectDropdown.vue'

const emit = defineEmits<{
  'models-updated': [];
  'back': [];
}>()

const goBack = () => {
  emit('back');
}

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
  provider_capabilities: string;
  error_msg?: string;
  diagnostics?: DiagnosticsData;
}

interface RemoteModelItem {
  id: string;
  name: string;
  display_name: string;
  exists: boolean;
  existing_id: string | null;
  matched_spec_id: string | null;
  matched_spec_name: string | null;
  context_window: number;
  max_tokens: number;
  temperature: number;
  supports_thinking: boolean;
  description?: string | null;
  selected?: boolean;
}

interface FetchRemoteModelsResult {
  endpoint_url: string;
  total: number;
  new_count: number;
  models: RemoteModelItem[];
}

// 响应式数据
const platforms = ref<PlatformData[]>([])
const models = ref<ModelData[]>([])
const modelSpecs = ref<ModelSpecData[]>([])
const specSearchQuery = ref('')
const selectedPlatformId = ref('')

// 远程拉取模型状态
const showRemoteModelsModal = ref(false)
const isFetchingRemoteModels = ref(false)
const isImportingRemoteModels = ref(false)
const remoteFetchPlatformId = ref('')
const remoteFetchResult = ref<FetchRemoteModelsResult | null>(null)
const remoteModelSearchQuery = ref('')
const remoteModelFetchError = ref('')
const importSuccessMsg = ref('')

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
const newModelInput = ref<HTMLInputElement | null>(null)
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
const modelConfigError = ref('')
const configRefreshError = ref('')

// 表单对象
const pForm = ref({ name: '', base_url: '', api_key: '' })
const mForm = ref({ display_name: '', max_tokens: 4096, context_window: 0, provider_capabilities: '{}' })
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

interface ApiKeyItem {
  key: string;
  enabled: boolean;
}

const rawKeyInputMode = ref(false)
const newKeyInput = ref('')

function maskKey(key: string): string {
  const clean = (key || '').trim()
  if (clean.length > 12) {
    return clean.slice(0, 6) + '...' + clean.slice(-4)
  }
  return clean ? '******' : ''
}

function parseKeyItems(raw: string): ApiKeyItem[] {
  if (!raw) return []
  const trimmed = raw.trim()
  if (trimmed.startsWith('[') && trimmed.endsWith(']')) {
    try {
      const arr = JSON.parse(trimmed)
      if (Array.isArray(arr)) {
        const list: ApiKeyItem[] = []
        for (const item of arr) {
          if (typeof item === 'string') {
            const clean = item.trim()
            if (clean) list.push({ key: clean, enabled: true })
          } else if (item && typeof item === 'object') {
            const k = String(item.key || item.api_key || item.token || '').trim()
            const enabled = item.enabled !== false && item.disabled !== true
            if (k) list.push({ key: k, enabled })
          }
        }
        if (list.length > 0) return list
      }
    } catch {}
  }
  return trimmed
    .split(/[\n\r,;，；]+/)
    .map(s => s.trim())
    .filter(Boolean)
    .map(s => ({ key: s, enabled: true }))
}

function serializeKeyItems(items: ApiKeyItem[]): string {
  if (items.length === 0) return ''
  const hasDisabled = items.some(i => !i.enabled)
  if (hasDisabled) {
    return JSON.stringify(items.map(i => ({ key: i.key.trim(), enabled: i.enabled })))
  }
  if (items.length === 1) {
    return items[0].key.trim()
  }
  return items.map(i => i.key.trim()).join('\n')
}

const currentKeyItems = computed(() => parseKeyItems(pForm.value.api_key || ''))
const enabledKeyCount = computed(() => currentKeyItems.value.filter(i => i.enabled).length)
const disabledKeyCount = computed(() => currentKeyItems.value.length - enabledKeyCount.value)


function getKeyBadge(rawKey: string) {
  const items = parseKeyItems(rawKey)
  const total = items.length
  const enabled = items.filter(i => i.enabled).length
  const disabled = total - enabled
  let label = ''
  let title = ''
  if (total === 0) {
    label = '0 Key'
    title = '未配置 API Key'
  } else if (disabled === 0) {
    label = `${total} ${total === 1 ? 'Key' : 'Keys'}`
    title = `已配置 ${total} 个 API Key (全部启用，自动故障转移/轮换)`
  } else {
    label = `${enabled}/${total} Keys`
    title = `已配置 ${total} 个 API Key (${enabled} 个启用，${disabled} 个已禁用/跳过)`
  }
  return { total, enabled, disabled, label, title }
}

function toggleKeyItem(index: number) {
  const list = parseKeyItems(pForm.value.api_key || '')
  if (index >= 0 && index < list.length) {
    list[index].enabled = !list[index].enabled
    pForm.value.api_key = serializeKeyItems(list)
  }
}

function removeKeyItem(index: number) {
  const list = parseKeyItems(pForm.value.api_key || '')
  if (index >= 0 && index < list.length) {
    list.splice(index, 1)
    pForm.value.api_key = serializeKeyItems(list)
  }
}

function addSingleKey() {
  const raw = newKeyInput.value.trim()
  if (!raw) return
  const newItems = parseKeyItems(raw)
  const list = parseKeyItems(pForm.value.api_key || '')
  for (const item of newItems) {
    if (!list.some(existing => existing.key === item.key)) {
      list.push(item)
    }
  }
  pForm.value.api_key = serializeKeyItems(list)
  newKeyInput.value = ''
}

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

const platformModelCounts = ref<Record<string, number>>({})

async function refreshModelCounts() {
  try {
    const all = await invoke<ModelData[]>('get_models')
    const counts: Record<string, number> = {}
    for (const m of all) {
      counts[m.platform_id] = (counts[m.platform_id] || 0) + 1
    }
    platformModelCounts.value = counts
  } catch (e) {
    console.error('Failed to refresh model counts', e)
  }
}

function getModelCount(platformId: string) {
  return platformModelCounts.value[platformId] ?? 0
}

// ==================== 数据加载 ====================
async function onConfigImported() {
  configRefreshError.value = ''
  emit('models-updated')
  try {
    await loadPlatforms()
    const id = selectedPlatformId.value || platforms.value[0]?.id
    if (id) await selectPlatform(id)
  } catch (e) {
    configRefreshError.value = '配置已导入，刷新列表失败，请重新打开模型管理：' + String(e)
  }
}

async function loadPlatforms() {
  platforms.value = await invoke<PlatformData[]>('get_platforms')
  await refreshModelCounts()
}

async function loadModelSpecs() {
  modelSpecs.value = await invoke<ModelSpecData[]>('get_model_specs')
}

async function selectPlatform(id: string) {
  selectedPlatformId.value = id
  models.value = await invoke<ModelData[]>('get_models', { platformId: id })
  platformModelCounts.value[id] = models.value.length
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
function clearQuickSpec() {
  quickSelectedSpecId.value = ''
}

async function addModel() {
  if (!newModelName.value.trim() || !selectedPlatformId.value) return

  const spec = activeQuickSpec.value
  const ctx = spec ? spec.context_window : 0
  const maxTok = spec ? spec.max_tokens : 4096
  const temp = spec ? spec.default_temperature : 0.7
  const dispName = newModelName.value.trim()

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
  await refreshModelCounts()
  emit('models-updated')
}

function editModel(m: ModelData) {
  editingModel.value = m
  mForm.value = {
    display_name: m.display_name || m.name,
    max_tokens: m.max_tokens,
    context_window: m.context_window ?? 0,
    provider_capabilities: m.provider_capabilities || '{}',
  }
  modelConfigError.value = ''
}

function onApplySpecToEditModel(specId: string, spec: any) {
  if (spec) {
    mForm.value.context_window = spec.context_window || 0
    mForm.value.max_tokens = spec.max_tokens || 4096
  } else if (specId) {
    const found = modelSpecs.value.find(s => s.id === specId)
    if (found) {
      mForm.value.context_window = found.context_window || 0
      mForm.value.max_tokens = found.max_tokens || 4096
    }
  }
}

function applyStandardCapability(type: 'standard' | 'google_gemini' | 'reasoning_effort' | 'budget_tokens' | 'thinking_bool' | 'json_mode' | 'kimi_k3' | 'deepseek_r1') {
  if (type === 'standard' || type === 'google_gemini') {
    mForm.value.provider_capabilities = '{}'
  } else if (type === 'kimi_k3') {
    mForm.value.provider_capabilities = JSON.stringify({
      thinking: {
        force_enabled: true,
        remove_sampling_when_enabled: true,
        effort: {
          field: "reasoning_effort",
          map: {
            off: "low",
            low: "low",
            medium: "high",
            high: "max",
            max: "max",
            auto: "max"
          },
          default: "max"
        }
      },
      response_format: { type: "json_object" }
    }, null, 2)
  } else if (type === 'deepseek_r1') {
    mForm.value.provider_capabilities = JSON.stringify({
      thinking: {
        force_enabled: true,
        remove_sampling_when_enabled: true
      },
      response_format: { type: "json_object" }
    }, null, 2)
  } else if (type === 'reasoning_effort') {
    mForm.value.provider_capabilities = JSON.stringify({
      thinking: {
        field: "reasoning_effort",
        enabled_value: "medium",
        disabled_value: "none",
        effort: {
          field: "reasoning_effort",
          map: {
            off: "none",
            low: "low",
            medium: "medium",
            high: "high"
          }
        },
        remove_sampling_when_enabled: true
      }
    }, null, 2)
  } else if (type === 'budget_tokens') {
    mForm.value.provider_capabilities = JSON.stringify({
      thinking: {
        field: "thinking",
        enabled_value: { type: "enabled" },
        disabled_value: { type: "disabled" },
        effort: {
          field: "budget_tokens",
          map: {
            low: 2048,
            medium: 8192,
            high: 32768
          }
        },
        remove_sampling_when_enabled: true
      }
    }, null, 2)
  } else if (type === 'thinking_bool') {
    mForm.value.provider_capabilities = JSON.stringify({
      thinking: {
        field: "thinking",
        enabled_value: true,
        disabled_value: false
      }
    }, null, 2)
  } else if (type === 'json_mode') {
    mForm.value.provider_capabilities = JSON.stringify({
      response_format: { type: "json_object" }
    }, null, 2)
  }
}

async function saveModelConfig() {
  if (!editingModel.value) return
  const providerCapabilities = mForm.value.provider_capabilities.trim() || '{}'
  try {
    const parsed = JSON.parse(providerCapabilities)
    if (!parsed || Array.isArray(parsed) || typeof parsed !== 'object') {
      throw new Error('必须是 JSON 对象')
    }
  } catch (error: any) {
    modelConfigError.value = `Provider 能力配置无效：${error?.message || error}`
    return
  }
  modelConfigError.value = ''
  try {
    await invoke('update_model', {
      id: editingModel.value.id,
      maxTokens: mForm.value.max_tokens,
      contextWindow: mForm.value.context_window,
      displayName: mForm.value.display_name,
      providerCapabilities,
    })
    const idx = models.value.findIndex(x => x.id === editingModel.value!.id)
    if (idx >= 0) {
      models.value[idx].display_name = mForm.value.display_name
      models.value[idx].max_tokens = mForm.value.max_tokens
      models.value[idx].context_window = mForm.value.context_window
      models.value[idx].provider_capabilities = providerCapabilities
    }
    editingModel.value = null
    emit('models-updated')
  } catch (error: any) {
    modelConfigError.value = `保存失败：${error?.message || error}`
  }
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
  quickSelectedSpecId.value = spec.id
  activeTab.value = 'platforms'
  await nextTick()
  newModelInput.value?.focus()
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
    await refreshModelCounts()
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
  } finally {
    isTestingCurrentPlatform.value = false
    if (selectedPlatformId.value === platformId) await selectPlatform(platformId)
  }
}

const targetPlatformForFetch = computed(() => {
  const pId = remoteFetchPlatformId.value || selectedPlatformId.value
  return platforms.value.find(p => p.id === pId)
})

const filteredRemoteModels = computed(() => {
  if (!remoteFetchResult.value) return []
  if (!remoteModelSearchQuery.value.trim()) return remoteFetchResult.value.models
  const q = remoteModelSearchQuery.value.toLowerCase()
  return remoteFetchResult.value.models.filter(m =>
    m.name.toLowerCase().includes(q) ||
    m.display_name.toLowerCase().includes(q) ||
    (m.matched_spec_name && m.matched_spec_name.toLowerCase().includes(q))
  )
})

const selectedRemoteModelsCount = computed(() => {
  if (!remoteFetchResult.value) return 0
  return remoteFetchResult.value.models.filter(m => m.selected).length
})

async function openFetchRemoteModelsModal(targetPlatformId?: string) {
  const pId = targetPlatformId || selectedPlatformId.value
  if (!pId) return

  remoteFetchPlatformId.value = pId
  showRemoteModelsModal.value = true
  isFetchingRemoteModels.value = true
  remoteModelFetchError.value = ''
  remoteFetchResult.value = null
  importSuccessMsg.value = ''
  remoteModelSearchQuery.value = ''

  try {
    const res = await invoke<FetchRemoteModelsResult>('fetch_remote_models', {
      platformId: pId,
      baseUrl: null,
      apiKey: null,
    })
    const hasNew = res.models.some(m => !m.exists)
    res.models.forEach(m => {
      m.selected = hasNew ? !m.exists : true
    })
    remoteFetchResult.value = res
  } catch (err: any) {
    remoteModelFetchError.value = typeof err === 'string' ? err : (err?.message || JSON.stringify(err))
  } finally {
    isFetchingRemoteModels.value = false
  }
}

async function retryFetchRemoteModels() {
  if (remoteFetchPlatformId.value) {
    await openFetchRemoteModelsModal(remoteFetchPlatformId.value)
  }
}

function selectAllRemoteModels(select: boolean) {
  if (!remoteFetchResult.value) return
  filteredRemoteModels.value.forEach(m => {
    m.selected = select
  })
}

function selectOnlyNewRemoteModels() {
  if (!remoteFetchResult.value) return
  remoteFetchResult.value.models.forEach(m => {
    m.selected = !m.exists
  })
}

async function executeImportRemoteModels() {
  const pId = remoteFetchPlatformId.value || selectedPlatformId.value
  if (!remoteFetchResult.value || !pId) return
  const selected = remoteFetchResult.value.models.filter(m => m.selected)
  if (selected.length === 0) return

  isImportingRemoteModels.value = true
  remoteModelFetchError.value = ''
  importSuccessMsg.value = ''

  try {
    const payload = selected.map(m => ({
      name: m.name,
      display_name: m.display_name,
      context_window: m.context_window,
      max_tokens: m.max_tokens,
      temperature: m.temperature,
      provider_capabilities: '{}',
    }))

    const res = await invoke<{ added_count: number; updated_count: number; total_processed: number }>('import_remote_models', {
      platformId: pId,
      models: payload,
    })

    importSuccessMsg.value = `🎉 成功新增 ${res.added_count} 个模型，更新 ${res.updated_count} 个已有模型！已保存至 SQLite 数据库`

    if (selectedPlatformId.value === pId) {
      await selectPlatform(pId)
    }
    await refreshModelCounts()
    emit('models-updated')

    setTimeout(() => {
      showRemoteModelsModal.value = false
    }, 1200)
  } catch (err: any) {
    remoteModelFetchError.value = `导入失败: ${typeof err === 'string' ? err : (err?.message || JSON.stringify(err))}`
  } finally {
    isImportingRemoteModels.value = false
  }
}

onMounted(async () => {
  await Promise.all([loadPlatforms(), loadModelSpecs()])
  if (platforms.value.length > 0 && !selectedPlatformId.value) {
    await selectPlatform(platforms.value[0].id)
  }
})
</script>

<style scoped src="./api-manager.css"></style>
