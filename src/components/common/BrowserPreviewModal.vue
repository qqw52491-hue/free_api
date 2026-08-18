<template>
  <Teleport to="body">
    <Transition name="browser-fade">
      <div v-if="isVisible" class="browser-preview-overlay" @click.self="close">
        <div class="browser-preview-window">
          <!-- 浏览器顶栏 (Header & Address Bar) -->
          <div class="browser-header">
            <!-- 窗体控制点 -->
            <div class="window-controls">
              <span class="ctrl-dot close-dot" @click="close" title="关闭 (ESC)"></span>
              <span class="ctrl-dot min-dot" @click="close" title="收起"></span>
              <span class="ctrl-dot max-dot" @click="toggleFullscreen" title="最大化"></span>
            </div>

            <!-- 地址导航栏 -->
            <div class="address-bar-container">
              <div class="nav-actions">
                <button type="button" class="nav-btn" @click="refreshIframe" title="刷新页面">
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2">
                    <path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.67"/>
                  </svg>
                </button>
              </div>

              <div class="address-input-wrapper">
                <span class="security-lock-icon" title="安全连接">
                  <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="#10b981" stroke-width="2.2">
                    <rect x="3" y="11" width="18" height="11" rx="2" ry="2"/>
                    <path d="M7 11V7a5 5 0 0 1 10 0v4"/>
                  </svg>
                </span>
                <input
                  type="text"
                  readonly
                  :value="previewUrl"
                  class="address-input"
                  title="页面 URL"
                  @click="$event.currentTarget && ($event.currentTarget as HTMLInputElement).select()"
                />
                <button type="button" class="copy-url-btn" @click="copyUrl" :title="copied ? '已复制链接' : '复制链接'">
                  <span v-if="copied" class="copy-tip">已复制</span>
                  <svg v-else width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                    <rect x="9" y="9" width="13" height="13" rx="2" ry="2"/>
                    <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>
                  </svg>
                </button>
              </div>
            </div>

            <!-- 右侧操作区：在系统默认浏览器中打开 + 关闭 -->
            <div class="browser-actions">
              <button
                type="button"
                class="action-btn external-btn"
                @click="openInExternalBrowser()"
                title="在系统默认浏览器 (Chrome/Edge/Firefox) 中打开"
              >
                <span>外部打开</span>
                <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2">
                  <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/>
                  <polyline points="15 3 21 3 21 9"/>
                  <line x1="10" y1="14" x2="21" y2="3"/>
                </svg>
              </button>
              <button type="button" class="action-btn close-btn" @click="close" title="关闭窗口">
                <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2">
                  <line x1="18" y1="6" x2="6" y2="18"/>
                  <line x1="6" y1="6" x2="18" y2="18"/>
                </svg>
              </button>
            </div>
          </div>

          <!-- 网页内容区域 (Iframe & Loading / Fallback) -->
          <div class="browser-content">
            <!-- 加载进度条 -->
            <div v-if="isLoading" class="browser-loading-bar"></div>

            <iframe
              v-if="previewUrl"
              ref="iframeRef"
              :src="previewUrl"
              class="browser-iframe"
              sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-downloads"
              @load="onIframeLoad"
              @error="onIframeError"
            ></iframe>

            <!-- 嵌入限制友好提示 (针对拒绝 iframe 嵌入的网站如 Github/Google/Bilibili 等) -->
            <div v-if="showFallbackBanner || loadFailed" class="xframe-fallback-banner">
              <div class="fallback-card">
                <div class="fallback-icon">🛡️</div>
                <div class="fallback-info">
                  <h4>部分网站限制了应用内嵌预览</h4>
                  <p>如果页面显示空白或拒绝连接，这是该网站的安全策略 (X-Frame-Options)。您可以点击下方按钮一键调起系统默认浏览器访问：</p>
                </div>
                <button type="button" class="fallback-launch-btn" @click="openInExternalBrowser()">
                  <span>在系统浏览器中打开</span>
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2">
                    <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/>
                    <polyline points="15 3 21 3 21 9"/>
                    <line x1="10" y1="14" x2="21" y2="3"/>
                  </svg>
                </button>
              </div>
            </div>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<script setup lang="ts">
import { ref, watch, onMounted, onUnmounted } from "vue";
import { useBrowserPreview } from "../../composables/useBrowserPreview";

const {
  isVisible,
  previewUrl,
  isLoading,
  loadFailed,
  close,
  openInExternalBrowser,
  onIframeLoad,
  onIframeError,
} = useBrowserPreview();

const iframeRef = ref<HTMLIFrameElement | null>(null);
const copied = ref(false);
const showFallbackBanner = ref(false);
let fallbackTimer: number | null = null;

function refreshIframe() {
  if (iframeRef.value && previewUrl.value) {
    isLoading.value = true;
    const current = previewUrl.value;
    iframeRef.value.src = "about:blank";
    setTimeout(() => {
      if (iframeRef.value) {
        iframeRef.value.src = current;
      }
    }, 50);
  }
}

function copyUrl() {
  if (!previewUrl.value) return;
  navigator.clipboard.writeText(previewUrl.value).then(() => {
    copied.value = true;
    setTimeout(() => {
      copied.value = false;
    }, 1800);
  });
}

function toggleFullscreen(e: MouseEvent) {
  const windowEl = (e.currentTarget as HTMLElement)?.closest(".browser-preview-window") as HTMLElement;
  if (windowEl) {
    windowEl.classList.toggle("fullscreen");
  }
}

// 监听键盘 ESC 快速退出
function handleKeydown(e: KeyboardEvent) {
  if (e.key === "Escape" && isVisible.value) {
    close();
  }
}

watch(isVisible, (val) => {
  if (val) {
    showFallbackBanner.value = false;
    // 如果 2.5 秒后仍未加载完成，或特定大站，显示快捷提示
    if (fallbackTimer) clearTimeout(fallbackTimer);
    fallbackTimer = window.setTimeout(() => {
      showFallbackBanner.value = true;
    }, 3000);
  } else {
    if (fallbackTimer) clearTimeout(fallbackTimer);
    showFallbackBanner.value = false;
  }
});

onMounted(() => {
  window.addEventListener("keydown", handleKeydown);
});

onUnmounted(() => {
  window.removeEventListener("keydown", handleKeydown);
  if (fallbackTimer) clearTimeout(fallbackTimer);
});
</script>

<style scoped>
.browser-preview-overlay {
  position: fixed;
  inset: 0;
  z-index: 99999;
  background: rgba(0, 0, 0, 0.72);
  backdrop-filter: blur(10px);
  -webkit-backdrop-filter: blur(10px);
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
}

.browser-preview-window {
  width: 92vw;
  max-width: 1200px;
  height: 88vh;
  background: #18181c;
  border: 1px solid rgba(255, 255, 255, 0.14);
  border-radius: 14px;
  box-shadow: 0 24px 64px rgba(0, 0, 0, 0.6), 0 0 0 1px rgba(255, 255, 255, 0.05);
  display: flex;
  flex-direction: column;
  overflow: hidden;
  transition: all 0.25s cubic-bezier(0.16, 1, 0.3, 1);
}

.browser-preview-window.fullscreen {
  width: 100vw;
  max-width: 100vw;
  height: 100vh;
  border-radius: 0;
}

/* Header */
.browser-header {
  height: 48px;
  background: #1f1f24;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 16px;
  gap: 16px;
  user-select: none;
  flex-shrink: 0;
}

.window-controls {
  display: flex;
  align-items: center;
  gap: 8px;
}

.ctrl-dot {
  width: 12px;
  height: 12px;
  border-radius: 50%;
  cursor: pointer;
  transition: transform 0.15s ease, opacity 0.15s ease;
}
.ctrl-dot:hover {
  transform: scale(1.15);
  opacity: 0.85;
}
.close-dot { background: #ef4444; }
.min-dot { background: #f59e0b; }
.max-dot { background: #10b981; }

/* Address Bar */
.address-bar-container {
  flex: 1;
  display: flex;
  align-items: center;
  gap: 8px;
  max-width: 760px;
}

.nav-actions {
  display: flex;
  align-items: center;
}

.nav-btn {
  background: transparent;
  border: none;
  color: #a1a1aa;
  width: 28px;
  height: 28px;
  border-radius: 6px;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  transition: all 0.15s ease;
}
.nav-btn:hover {
  background: rgba(255, 255, 255, 0.08);
  color: #ffffff;
}

.address-input-wrapper {
  flex: 1;
  height: 32px;
  background: #121215;
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 8px;
  display: flex;
  align-items: center;
  padding: 0 10px;
  gap: 8px;
}

.security-lock-icon {
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
}

.address-input {
  flex: 1;
  background: transparent;
  border: none;
  color: #d4d4d8;
  font-size: 12.5px;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  outline: none;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.copy-url-btn {
  background: transparent;
  border: none;
  color: #71717a;
  cursor: pointer;
  padding: 4px;
  border-radius: 4px;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: all 0.15s;
}
.copy-url-btn:hover {
  color: #e4e4e7;
  background: rgba(255, 255, 255, 0.08);
}
.copy-tip {
  font-size: 11px;
  color: #10b981;
  font-weight: 600;
}

/* Actions */
.browser-actions {
  display: flex;
  align-items: center;
  gap: 10px;
}

.action-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  font-weight: 600;
  padding: 6px 12px;
  border-radius: 6px;
  cursor: pointer;
  transition: all 0.15s ease;
}

.external-btn {
  background: rgba(108, 99, 255, 0.15);
  border: 1px solid rgba(108, 99, 255, 0.35);
  color: #a5b4fc;
}
.external-btn:hover {
  background: rgba(108, 99, 255, 0.28);
  border-color: rgba(108, 99, 255, 0.6);
  color: #ffffff;
}

.close-btn {
  background: transparent;
  border: none;
  color: #a1a1aa;
  padding: 6px;
}
.close-btn:hover {
  background: rgba(239, 68, 68, 0.18);
  color: #ef4444;
}

/* Content & Iframe */
.browser-content {
  flex: 1;
  position: relative;
  background: #ffffff;
  overflow: hidden;
}

.browser-iframe {
  width: 100%;
  height: 100%;
  border: none;
  background: #ffffff;
}

/* Loading Bar */
.browser-loading-bar {
  position: absolute;
  top: 0;
  left: 0;
  right: 0;
  height: 3px;
  background: linear-gradient(90deg, #6c63ff, #38bdf8, #10b981);
  background-size: 200% 100%;
  animation: loading-bar-anim 1.2s infinite ease-in-out;
  z-index: 10;
}

@keyframes loading-bar-anim {
  0% { background-position: 100% 0; }
  100% { background-position: -100% 0; }
}

/* Fallback Banner */
.xframe-fallback-banner {
  position: absolute;
  bottom: 24px;
  left: 50%;
  transform: translateX(-50%);
  z-index: 20;
  width: 90%;
  max-width: 600px;
}

.fallback-card {
  background: rgba(24, 24, 28, 0.95);
  border: 1px solid rgba(255, 255, 255, 0.16);
  border-radius: 12px;
  box-shadow: 0 16px 36px rgba(0, 0, 0, 0.5);
  backdrop-filter: blur(12px);
  padding: 14px 18px;
  display: flex;
  align-items: center;
  gap: 16px;
}

.fallback-icon {
  font-size: 24px;
  flex-shrink: 0;
}

.fallback-info {
  flex: 1;
  min-width: 0;
}

.fallback-info h4 {
  margin: 0 0 4px;
  font-size: 13px;
  font-weight: 650;
  color: #f4f4f5;
}

.fallback-info p {
  margin: 0;
  font-size: 11.5px;
  color: #a1a1aa;
  line-height: 1.45;
}

.fallback-launch-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  background: #6c63ff;
  border: none;
  color: #ffffff;
  padding: 8px 14px;
  border-radius: 8px;
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  white-space: nowrap;
  transition: all 0.15s ease;
  flex-shrink: 0;
}
.fallback-launch-btn:hover {
  background: #5b52e0;
  box-shadow: 0 4px 12px rgba(108, 99, 255, 0.35);
}

/* Fade animation */
.browser-fade-enter-active,
.browser-fade-leave-active {
  transition: opacity 0.2s ease, transform 0.2s ease;
}

.browser-fade-enter-from,
.browser-fade-leave-to {
  opacity: 0;
  transform: scale(0.97);
}
</style>
