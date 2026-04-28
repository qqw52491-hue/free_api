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

### 🧩 模块 1：线性缓存优化上下文 (Cache-Optimized Context)
采用了“从静态到动态”的线性追加结构，极大提升了在大模型（如 DeepSeek, Anthropic）上的 **Prompt Caching** 命中率，降低 90% 以上的重复 Token 开销：
-   **静态头部 (Fixed Prefix)**: 系统提示词 + 用户终极目标 (Goal) + 长期事实记忆 (Memories)。
-   **线性增长区 (Append-only History)**: 全局对话历史记录。利用历史回合的线性追加特性，确保过往决策被完美缓存。
-   **动态尾部 (Dynamic Suffix)**: 当前工具手册 + 任务面板 (Todo List) + 环境观测 (Observation)。将变动最频繁的内容置于末尾，彻底避免缓存污染。

### 🔌 模块 2：MCP 插件管家 (Plugin Manager)
负责管理本地 `plugins/` 目录下的所有 YAML 配置。其核心 `McpClient` 具备：
-   **子进程动态生成 (Spawn)**: 以后台方式拉起工具。
-   **JSON-RPC 2.0 通信**: 通过 `stdin`/`stdout` 与插件进行高速标准通信。

### 🚦 模块 3：核心路由调度器 (The Main Loop Router)
担任系统的“信号发生器”，根据 LLM 的意图和上下文状态（如视觉需求、执行错误）决定指令去向：
-   **智能分流**: 自动识别简单任务（Flash 模型）与复杂纠错/规划（Pro 模型）。
-   **内置/外部路由**: 浏览器原生操作与 MCP 插件操作的无缝分发。

---

## 🚀 性能与可靠性：工业级装甲优化

为了应对高频率、长周期的 Agent 任务，系统已实装以下深层优化：

### 1. ⚡️ 全局插件保活 (Stateful MCP)
*   **机制**：`PluginRegistry` 被托管在 Tauri 的全局状态中，插件进程（如 Excel 句柄）**只会启动一次**。后续调用为微秒级热响应，且能保持插件内部上下文。

### 🔄 2. 智能历史截断与归档 (AI-Driven History Flushing)
*   **机制**：当历史接近上限时，系统发出动态预警。AI 会主动将关键信息汇总至 `memories_update` 并触发 `clear_history` 标志，由内核物理清空冗长历史。
*   **价值**：实现了长周期任务的“无损扩容”，确保 Agent 在处理超长任务时永不掉线。

### 🛠️ 3. 进程自愈机制 (Process Self-Healing)
*   **机制**：集成健康检查，发起请求前自动检测子进程状态。若发现崩溃，系统会自动 **静默重启 (Respawn)**，确保执行链路不断裂。

### 📸 4. 视觉感知增强 (Visual Observation)
*   **机制**：集成 `capture_screenshot` 接口。当 DOM 抓取失败或页面复杂时，自动升维至视觉模型处理。

### 🧵 5. 高并发线程安全 (Async Concurrency Safety)
*   使用 `Arc<Mutex<>>` 封装全局注册表，确保多 Agent 会话并发调度时的绝对安全。

---

## 🛠️ 代码工程化分层 (`src/agent/`)

-   `mod.rs`: 总枢纽、主循环与核心路由逻辑。
-   `context.rs`: 线性缓存上下文拼装与历史管理。
-   `mcp.rs`: MCP 客户端协议实现。
-   `browser.rs`: 基于 CDP 的内置自动化内核。
-   `types.rs`: 强类型的指令协议与数据结构。
-   `utils.rs`: JSON 暴力提取与鲁棒性工具集。

---

## 🚀 快速开始

1.  在项目根目录创建 `plugins/` 目录。
2.  放置 YAML 配置文件（如 `excel.yaml`）：
    ```yaml
    name: excel-mcp
    command: uvx
    args: ["mcp-server-excel"]
    ```
3.  通过 Agent 循环调用 `excel-mcp/write_xlsx` 即可。
