# 🤖 SandwichAgent — 真正的人机协同 Web Agent 平台

<p align="center">
  <strong>基于 Rust + Tauri 构建的本地原生自主 Web Agent，支持实时人类干预与 AI 主动挂起求助</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/built_with-Rust-orange?logo=rust" />
  <img src="https://img.shields.io/badge/UI-Tauri_+_Vue_3-blue?logo=tauri" />
  <img src="https://img.shields.io/badge/Browser-Chrome_CDP-yellow?logo=googlechrome" />
  <img src="https://img.shields.io/badge/Protocol-MCP_2.0-green" />
  <img src="https://img.shields.io/badge/license-MIT-lightgrey" />
</p>

---

## ✨ 它能干什么？（10 秒了解核心能力）

| 能力 | 描述 |
|------|------|
| 🌐 **全自动浏览器操控** | AI 自主打开网页、点击、输入、滚动、抓取 DOM，就像有个机器人坐在你电脑前 |
| 👑 **人类随时强行接管** | 任务运行中，你可以随时输入指令打断 AI，它会立刻在下一步改道 |
| 🛑 **AI 主动挂起求助** | 遇到扫码登录、验证码时，AI 会主动停下来并告诉你需要做什么，你处理完后它继续 |
| 🔌 **MCP 插件扩展** | 通过 `uvx` / `npx` 免环境动态加载 Excel、数据库等本地工具，即插即用 |
| 🧠 **长任务不失忆** | AI 自驱的记忆归档与历史压缩，任务跑 100 步也不会上下文溢出 |
| 🆘 **Kimi 场外救援** | 执行失败后，自动打开第二个浏览器窗口向 Kimi 求救并获取修复建议 |

---

## 🤝 Human-in-the-Loop：真正的人机协同

这是市面上绝大多数 Agent 框架都没有认真实现的能力。

### 模式 A：人类主动抢方向盘（异步干预注入）

AI 运行途中，你随时可以在干预框里输入新指令，比如：
> "放弃百度，去 Bing 重新搜一遍"
> "这个页面不对，退出去找另一家网站"

系统不会暴力杀死 AI，而是在 AI 完成当前原子动作后的**下一个安全点**，将你的指令作为最高优先级消息注入 AI 的上下文。AI 会在 reflection 中承认收到，并立刻重写计划。

```
人类点击"强行干预" → Vue invoke → Rust 通道 → 大循环开头 try_recv()
→ 注入 turns_history → AI 下一步彻底改道
```

### 模式 B：AI 主动挂起请求协助

当 AI 遇到扫码登录、滑块验证码等无法自动化处理的操作时，它会主动调用 `ask_human` 工具：
> "老板，页面出现了微信扫码登录！请扫码后告诉我一声。"

系统会**彻底冻结执行循环**，前端变为红色警告面板，Token 消耗归零。直到你处理完毕并在面板输入回复，AI 才会被唤醒继续。

```
AI 调用 ask_human → Rust 拦截 recv().await 真正阻塞
→ 前端红色唤醒面板 → 人类输入 → 唤醒 → AI 带着你的回复继续
```

---

## 🏗️ 核心架构设计

```
┌─────────────────────────────────────────────────────────────────┐
│                          大循环 (max 50步)                        │
│                                                                 │
│  ① 安全点检查 (try_recv) ←── 人类干预消息通道 ←── 前端发送        │
│  ↓                                                              │
│  ② 调用大模型规划 (Pro/Flash 动态路由)                            │
│  ↓                                                              │
│  ③ ask_human 特判 ──────────────────────→ 阻塞等待人类唤醒       │
│  ↓ (普通工具)                                                    │
│  ④ 执行工具 (CDP 浏览器 / MCP 插件)                              │
│  ↓                                                              │
│  ⑤ 结果写入上下文 → Emit 前端 → 回到①                            │
└─────────────────────────────────────────────────────────────────┘
```

### 线性缓存优化上下文（Cache-Optimized Context）

采用"从静态到动态"的四层结构，将 Prompt Caching 命中率最大化：

```
[System Prompt]  ← 完全静态，永久缓存
[用户目标 + 长期记忆]  ← 极低频变动
[历史对话轨迹]  ← Append-only，线性缓存完美覆盖
[Todo面板 + 当前观测]  ← 高频变动，放末尾避免污染前缀缓存
```

---

## 🔌 MCP 插件：零配置扩展任何能力

在 `plugins/` 目录下放一个 YAML 文件，AI 即可自动发现并使用：

```yaml
# plugins/excel.yaml
name: excel-mcp
command: uvx
args: ["mcp-server-excel"]
```

系统通过 `uvx`（Python）或 `npx`（Node.js）动态拉起隔离进程，**你不需要安装任何依赖**。

---

## 🚀 工程亮点

| 特性 | 实现方式 |
|------|---------|
| **真·阻塞挂起** | `rx.recv().await` 死等，不消耗任何 CPU/Token |
| **Safepoint 干预** | `try_recv()` 非阻塞检查，绝不撕裂原子动作 |
| **插件全局保活** | `PluginRegistry` 托管于 Tauri State，进程只启动一次 |
| **AI 自驱历史归档** | AI 主动 `clear_history: true` 触发物理清空，配合 `memories_update` 永不失忆 |
| **视觉感知升级** | DOM 解析失败时自动截图，交给多模态大模型看图操作 |
| **Kimi 场外救援** | 执行失败后动态调用网页版 Kimi 获取急救建议 |
| **多会话并发安全** | `Arc<Mutex<HashMap<SessionId, Sender>>>` 保证多 Agent 并发隔离 |

---

## 🛠️ 快速开始

### 环境要求

- Rust 1.78+
- Node.js 20+
- Google Chrome（用于 CDP 控制）

### 运行

```bash
git clone https://github.com/your-username/free_api.git
cd free_api
npm install
npm run tauri dev
```

### 添加 MCP 插件

```bash
mkdir plugins
# 新建一个 YAML 配置文件即可，AI 自动发现
```

---

## 📁 代码结构

```
src-tauri/src/agent/
├── mod.rs        # 主循环、双模式 HITL 干预、路由调度
├── context.rs    # 四层线性缓存上下文拼装与历史管理
├── mcp.rs        # MCP 客户端 JSON-RPC 2.0 实现
├── browser.rs    # CDP 内置自动化内核
├── router.rs     # Pro/Flash/Vision 三模型动态路由
├── types.rs      # 强类型指令协议与数据结构
└── utils.rs      # JSON 暴力提取与鲁棒性工具集

src-tauri/prompts/
├── core_logic.md        # AI 行为总纲（含 HITL 规范）
├── tools/ask_human.md   # AI 主动挂起工具手册（热拔插加载）
├── tools/browser_dom.md # 浏览器操控工具手册
└── rescue.md            # Kimi 场外救援模板
```

---

## 📄 License

MIT — 随便用，随便改，欢迎 PR 和 Star ⭐
