<template>
  <button
    class="theme-toggle"
    type="button"
    :title="isDark ? '切换到浅色模式' : '切换到深色模式'"
    :aria-label="isDark ? '切换到浅色模式' : '切换到深色模式'"
    @click="toggleTheme"
  >
    <span class="theme-toggle-icons" :class="{ 'is-light': !isDark }" aria-hidden="true">
      <svg class="theme-sun" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round">
        <circle cx="12" cy="12" r="4" />
        <path d="M12 2v2m0 16v2M2 12h2m16 0h2M4.93 4.93l1.42 1.42m11.3 11.3 1.42 1.42M4.93 19.07l1.42-1.42m11.3-11.3 1.42-1.42" />
      </svg>
      <svg class="theme-moon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
        <path d="M20.7 13A8.8 8.8 0 0 1 11 3.3 8.8 8.8 0 1 0 20.7 13Z" />
      </svg>
    </span>
  </button>
</template>

<script setup lang="ts">
import { useTheme } from "../../composables/useTheme";

const { isDark, toggleTheme } = useTheme();
</script>

<style scoped>
.theme-toggle {
  display: inline-grid;
  place-items: center;
  flex: 0 0 32px;
  width: 32px;
  height: 32px;
  padding: 0;
  border: 1px solid var(--border-1);
  border-radius: 10px;
  background: var(--surface-1);
  color: var(--text-2);
  cursor: pointer;
  transition: color .16s ease, background-color .16s ease, border-color .16s ease, transform var(--motion-quick) var(--motion-ease);
}
.theme-toggle:hover { color: var(--accent-light); background: var(--surface-3); border-color: var(--border-2); }
.theme-toggle:active { transform: scale(.94); }
.theme-toggle:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
.theme-toggle-icons { position: relative; width: 18px; height: 18px; }
.theme-toggle svg { position: absolute; inset: 0; width: 18px; height: 18px; transition: opacity .3s ease, transform .45s cubic-bezier(.22, 1, .36, 1); }
.theme-sun { opacity: 1; transform: rotate(0deg) scale(1); }
.theme-moon { opacity: 0; transform: rotate(-60deg) scale(.5); }
.is-light .theme-sun { opacity: 0; transform: rotate(60deg) scale(.5); }
.is-light .theme-moon { opacity: 1; transform: rotate(0deg) scale(1); }
@media (prefers-reduced-motion: reduce) { .theme-toggle, .theme-toggle svg { transition: none; } .theme-toggle:active { transform: none; } }
</style>
