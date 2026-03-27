# 🤖 Free-API Agent 极客平台 (微内核架构)

基于 Rust + Tauri 的极致轻量、彻底解耦的本地微内核 Agent 平台。

## 💡 核心设计思想：内置特种部队 + 外部包工头

系统采用双执行路径设计，确保在极速网页自动化与通用扩展性之间达到完美平衡：

1.  **极速内置路径 (Browser DOM)**: 针对浏览器操作，内置基于 `CDP` (Chrome DevTools Protocol) 的高性能 DOM 操控。
2.  **标准 MCP 插件路径 (Stdio Subprocess)**: 对于 Excel、数据库等非浏览器操作，通过 **MCP (Model Context Protocol)** 协议“外包”给子进程。
    *   **⚡️ 免环境配置 (Environment-Free)**: 系统直接调用 `uvx` (针对 Python) 或 `npx` (针对 Node.js) 动态拉起独立进程。
    *   **即插即用**: 开发者无需手动安装复杂的 Python 虚拟环境或 Node 依赖。系统会自动在临时隔离环境中运行插件。

---

## 🏗️ 三大核心架构模块

### 🥪 模块 1：三明治上下文引擎 (The Sandwich Context)
实现了层次化的 Prompt 结构，确保模型始终保持长期的目标感与短期的精确落脚点：
-   **上层 (Top Bread)**: 系统提示词 + 用户的终极宏观任务 (Ultimate Goal)。
-   **中层 (Filling)**: 
    *   **抽象任务面板 (To-Do List)**: JSON 格式的任务进度表。
    *   **滑动窗口记忆 (Sliding Window)**: 仅保留最近 5 步的动作摘要，节省 Token。
-   **下层 (Bottom Bread)**: 最新提取的网页 DOM 状态或 MCP 工具执行结果。

### 🔌 模块 2：MCP 插件管家 (Plugin Manager)
负责管理本地 `plugins/` 目录下的所有 YAML 配置。其核心 `McpClient` 具备：
-   **子进程动态生成 (Spawn)**: 以后台方式拉起工具。
-   **JSON-RPC 2.0 通信**: 通过 `stdin`/`stdout` 与插件进行高速标准通信。

### 🚦 模块 3：核心路由调度器 (The Main Loop Router)
担任系统的“信号发生器”，根据 LLM 的意图决定指令去向：
-   **内置白名单路由**: 浏览器相关操作 (Click, Type, Extract) 直接在内核中高效执行。
-   **外部扩展路由**: 无法由内核处理的操作自动转发到已注册的 MCP 插件。

---

## 🛠️ 代码工程化分层 (`src/agent/`)

为了防止代码过度集成导致难以维护，项目已将 Agent 模块彻底拆解：
-   `mod.rs`: 总枢纽与核心路由。
-   `context.rs`: 三明治上下文拼装逻辑。
-   `mcp.rs`: MCP 客户端与插件生命周期管理。
-   `browser.rs`: 基于 `headless_chrome` 的内置自动化逻辑。
-   `types.rs`: 全局共享的结构体定义。
-   `utils.rs`: 健壮的 JSON 暴力提取器与跨平台 Shell 调用。

---

## 🚀 快速开始

1.  在项目根目录创建 `plugins/` 目录。
2.  放置一个 YAML 配置文件（例如 `excel.yaml`）：
    ```yaml
    name: excel-mcp
    command: uvx
    args: ["mcp-server-excel"]
    ```
3.  通过系统的 Agent 循环，呼叫 `excel-mcp/read_sheet` 即可启动跨进程联动。
