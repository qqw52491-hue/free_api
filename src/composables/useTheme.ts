import { computed, readonly, ref } from "vue";

type Theme = "dark" | "light";
const storageKey = "free-api-theme";
const theme = ref<Theme>("dark");
let initialized = false;

function applyTheme(value: Theme) {
  document.documentElement.dataset.theme = value;
  theme.value = value;
}

// 在 Vue 挂载前应用选择，避免启动时先显示另一套颜色。
export function initializeTheme() {
  if (initialized) return;
  initialized = true;
  let saved: string | null = null;
  try { saved = localStorage.getItem(storageKey); } catch { /* 存储不可用时仍可切换。 */ }
  applyTheme(saved === "light" ? "light" : "dark");
  window.addEventListener("storage", (event) => {
    if (event.key === storageKey && (event.newValue === "dark" || event.newValue === "light")) {
      applyTheme(event.newValue);
    }
  });
}

function toggleTheme() {
  const target: Theme = theme.value === "dark" ? "light" : "dark";
  // 只切换根节点变量，不截取整个聊天页面，也不等待渲染器的动画 Promise。
  applyTheme(target);
  try { localStorage.setItem(storageKey, target); } catch { /* 本次选择仍然生效。 */ }
}

export function useTheme() {
  return {
    theme: readonly(theme),
    isDark: computed(() => theme.value === "dark"),
    toggleTheme,
  };
}

export function themeColor(token: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(token).trim();
}
