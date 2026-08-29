import path from "node:path";
import { fileURLToPath } from "node:url";

const rootDir = path.dirname(fileURLToPath(import.meta.url));

// Linux desktop E2E may run under Wayland while the WebKitGTK test stack uses
// X11 automation. Preserve explicit user choices, otherwise use the same
// compatibility defaults as the project’s `dev:x11` scripts.
if (process.platform === "linux") {
  process.env.GDK_BACKEND ??= "x11";
  process.env.WEBKIT_DISABLE_DMABUF_RENDERER ??= "1";
}
// The cross-platform test build uses `tauri build --no-bundle`, which emits
// this executable on macOS and Linux. Windows keeps its native suffix.
const appBinaryPath = path.join(
  rootDir,
  "src-tauri",
  "target",
  "debug",
  process.platform === "win32" ? "free-api-chat.exe" : "free-api-chat",
);
const defaultMochaTimeoutMs = 180_000;
const requestedMochaTimeoutMs = Number(process.env.FREE_API_WDIO_MOCHA_TIMEOUT_MS);
const mochaTimeoutMs = Number.isFinite(requestedMochaTimeoutMs)
  ? Math.max(defaultMochaTimeoutMs, Math.min(requestedMochaTimeoutMs, 1_200_000))
  : defaultMochaTimeoutMs;

/**
 * 原生 Tauri 窗口 E2E：macOS 使用嵌入式 WebDriver。
 * 仅显式带 `wdio` feature 的测试 App bundle 会加载 Rust 侧驱动；普通发布版不会启动测试端口。
 */
export const config = {
  runner: "local",
  specs: ["./test/e2e/**/*.e2e.mjs"],
  maxInstances: 1,
  services: [["@wdio/tauri-service", {
    appBinaryPath,
    driverProvider: "embedded",
    embeddedPort: 4445,
    commandTimeout: 30_000,
    startTimeout: 180_000,
  }]],
  capabilities: [{
    browserName: "tauri",
    "tauri:options": { application: appBinaryPath },
  }],
  logLevel: "info",
  waitforTimeout: 10_000,
  connectionRetryTimeout: 90_000,
  connectionRetryCount: 1,
  framework: "mocha",
  reporters: ["spec"],
  mochaOpts: {
    ui: "bdd",
    // 默认保持短 E2E 的三分钟上限；长 Agent 回归显式设置环境变量，最大二十分钟。
    timeout: mochaTimeoutMs,
  },
};
