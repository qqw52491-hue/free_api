# 🤖 SandwichAgent (Free API Agent)

<p align="center">
  <strong>基于 Rust + Tauri 2.0 构建的高性能、高确定性本地原生自主 Web & 系统级 Agent 平台</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/built_with-Rust_1.78+-orange?logo=rust" />
  <img src="https://img.shields.io/badge/Framework-Tauri_2.0_+_Vue_3-blue?logo=tauri" />
  <img src="https://img.shields.io/badge/Architecture-Sandwich_Context_Engine-purple" />
  <img src="https://img.shields.io/badge/Caching-Prefix_Cache_Optimized-brightgreen" />
  <img src="https://img.shields.io/badge/Protocol-MCP_2.0-green" />
  <img src="https://img.shields.io/badge/license-MIT-lightgrey" />
</p>

---

## 📖 目录

- [一、设计哲学与理论背景](#一设计哲学与理论背景)
  - [1. 记忆的闭环：Write–Manage–Read 与三阶段演化](#1-记忆的闭环writemanageread-与三阶段演化)
  - [2. 为什么拒绝「统一存储大杂烩 (God Table)」？](#2-为什么拒绝统一存储大杂烩-god-table)
  - [3. 为什么坚守「写入即定型 (Immutable on Write)」？](#3-为什么坚守写入即定型-immutable-on-write)
  - [4. 前缀缓存的工业级真相与监控指标降级](#4-前缀缓存的工业级真相与监控指标降级)
- [二、真·三明治上下文引擎 (Sandwich Context Engine)](#二真三明治上下文引擎-sandwich-context-engine)
  - [1. 上下文三层拓扑结构](#1-上下文三层拓扑结构)
  - [2. 字节级确定性保证 (Byte-Level Determinism)](#2-字节级确定性保证-byte-level-determinism)
  - [3. 动态安全预算与滞后高低水位压缩 (Lagged Watermark Compaction)](#3-动态安全预算与滞后高低水位压缩-lagged-watermark-compaction)
  - [4. 混合落盘与零冗余存储 (Hybrid Storage & Disk Spilling)](#4-混合落盘与零冗余存储-hybrid-storage--disk-spilling)
  - [5. 核心记忆安检门与双向反馈 (Memory Security Gatekeeper)](#5-核心记忆安检门与双向反馈-memory-security-gatekeeper)
  - [6. 出口级消息角色合并 (Same-Role Consolidation)](#6-出口级消息角色合并-same-role-consolidation)
- [三、三级读路径检索体系与联邦检索 (Federated Retrieval)](#三三级读路径检索体系与联邦检索-federated-retrieval)
  - [1. 检索上限由“写”决定：定长路标 (Signpost)](#1-检索上限由写决定定长路标-signpost)
  - [2. 三级读路径拓扑 (Three-Tier Read Path)](#2-三级读路径拓扑-three-tier-read-path)
  - [3. 统一检索接口设计 `search(...)`](#3-统一检索接口设计-search)
- [四、自我改进闭环：经验蒸馏与淘汰门禁 (Experience Distillation)](#四自我改进闭环经验蒸馏与淘汰门禁-experience-distillation)
  - [1. Sleep-Time Compute（睡眠期异步计算）](#1-sleep-time-compute睡眠期异步计算)
  - [2. Hindsight Lesson 结构化抽取](#2-hindsight-lesson-结构化抽取)
  - [3. 淘汰门禁与防止自我污染](#3-淘汰门禁与防止自我污染)
- [五、双模式人机协同 (Human-in-the-Loop)](#五双模式人机协同-human-in-the-loop)
  - [模式 A：人类主动抢方向盘（异步安全点拦截）](#模式-a人类主动抢方向盘异步安全点拦截)
  - [模式 B：AI 主动挂起求助（真正的阻塞等待）](#模式-bai-主动挂起求助真正的阻塞等待)
- [六、模块结构与代码索引](#六模块结构与代码索引)
- [七、快速上手与运行](#七快速上手与运行)

---

## 一、设计哲学与理论背景

### 1. 记忆的闭环：Write–Manage–Read 与三阶段演化
根据最新的 Agent 记忆体系形式化理论（*Memory for Autonomous LLM Agents, arXiv 2603.07670* 与 *ACL 2026 综述 From Storage to Experience*），自主 Agent 的演化经历三个阶段：
1. **Storage（轨迹保存）**：保存执行流水与产出。
2. **Reflection（轨迹精炼）**：提炼局部摘要与避坑教训。
3. **Experience（轨迹抽象）**：跨任务沉淀可复用的策略与知识。

```
       ┌────────────────────────────────────────────────────────┐
       │                       LLM Agent                        │
       └───────┬────────────────────────────────────────▲───────┘
               │ 1. 产生工具输出 / 事实 / 决策               │ 3. 读取上下文 (Read)
               ▼                                        │    - 命中前缀缓存
       ┌───────────────┐                        ┌───────┴────────┐
       │ 写入 (Write)   │ ────── 2. 管理 ──────► │ 组装消息       │
       │ - 4k 入口硬截断 │    (Manage)           │ - 三明治分层   │
       │ - Head/Tail采样│ - 高低水位自适应压缩    │ - 状态卡置底   │
       │ - 敏感凭据过滤 │ - 超阈值落盘 .log 标记 │ - 出口相邻合并 │
       └───────────────┘ - BTreeMap 字母序对齐  └────────────────┘
```

SandwichAgent 在该闭环上的核心工程定位：
- **上下文内压缩 (In-Context Compaction)**：滞后高低水位线驱动，规则型保底摘要。
- **外部多级存储 (Tiered Storage)**：热数据在 Context、温数据在 SQLite、冷数据/超长原始日志溢出落盘为 `.log` 文件。
- **分层虚拟上下文**：轻量 `output_id` 引用索引 + 按需回查机制。

---

### 2. 为什么拒绝「统一存储大杂烩 (God Table)」？
在初期设计中，曾探讨过是否引入一个 `universal_nodes` 表将规则、工具文档、历史日志、代码片段、事实记忆全部打平存储。我们在深思后**坚决否决了这一方案**：

> **核心判断**：*统一检索接口 ≠ 统一存储表。Rust 强大的类型系统是最大的工程资产，God Table 会把编译期类型安全赶进 SQL 和 JSON 字符串里。*

- **生命周期完全不同**：系统规则是**静态不可变**的；代码是**高频被编辑**的；执行历史是**只追加 (Append-only)** 的；记忆卡是**键值更新**的。强行合并只会导致索引失效和失效级联。
- **避免过期快照污染**：代码是鲜活的，预建代码摘要树会随着文件修改迅速过期（Claude Code 也因此转向实时 JIT 代码检索）。因此代码检索直接走实时 `ripgrep` / AST，历史检索走 SQLite FTS5，规则常驻上下文，绝不强行混库。

---

### 3. 为什么坚守「写入即定型 (Immutable on Write)」？
很多 Agent 框架在每一轮发送 Prompt 前，会重新计算前几轮历史消息的摘要或动态修改早期的消息文本。**这在现代大模型（DeepSeek、Claude、OpenAI）的前缀缓存机制下是毁灭性的！**

- **一旦前缀中哪怕一个标点发生变化，该位置之后的所有 KV Cache 全部击穿**，推理延迟暴增 3~5 倍，Token 计费直接按原价全额收取。
- **SandwichAgent 原则**：任何工具输出在产生的瞬间（`add_step`）完成采样、脱水、截断与结构化。生成不可变的 `ChatMessage` 压入历史队列后，**后续轮次绝不再对其进行动态修改**。

---

### 4. 前缀缓存的工业级真相与监控指标降级

#### (1) 缓存机制的物理限制
- **块对齐 (Block Alignment)**：DeepSeek 按 64 tokens 对齐，OpenAI 按 128 tokens 对齐（且需 $\ge 1024$ tokens 才触发），Anthropic 需显式设置 `cache_control` 断点。前缀末尾不满一块的部分天然存在块级抖动。
- **缓存生命周期 (TTL)**：各厂商缓存通常保留 5~10 分钟。用户离开后再交互出现 Cache Miss 是底层物理机制，无需做误报处理；多节点请求需使用 Sticky Session 锁定同一推理实例。
- **Prefill 节省真相**：缓存命中省掉的是 Prefill 阶段庞大的矩阵乘法（$O(N^2)$ 计算），但 KV Cache 仍需从内存或 SSD 搬运，TTFT 仍包含网络、排队与调度开销。

#### (2) 告别“虚荣命中率”
当上下文堆叠到 128k 时，哪怕浪费严重，命中率也能轻易达到 99.9%（分母效应）。因此我们将监控指标从百分比降级，采用工业级真实 KPI：
$$\text{核心监控} = \text{每步重算的 Miss Token 绝对数量} + \text{每步实际调用单价 (\$) } + \text{TTFT 首字延迟}$$

---

## 二、真·三明治上下文引擎 (Sandwich Context Engine)

### 1. 上下文三层拓扑结构

```
══════════════════════════════════════════════════════════════════════
 🍞 顶层面包：100% 静态前缀 (True Static Prefix)
 ──────────────────────────────────────────────────────────────────
 1. system_prompt        (系统规则基座，会话期内绝对不变)
 2. active_tool_detail   (当前活跃 Skill/工具操作手册，连续同工具调用不变)
 3. user_goal            (用户终极任务目标，单次任务绝对常量)
 👉 稳吃 DeepSeek (64 tok 对齐) / OpenAI (1024 tok 门槛) 前缀缓存
══════════════════════════════════════════════════════════════════════
 🥩 中间夹心：线性历史流水 (Append-Only History Stream)
 ──────────────────────────────────────────────────────────────────
 Step 1: [Action Brief] -> [Sampled Output (output_id=...)]
 Step 2: [Action Brief] -> [Sampled Output (output_id=...)]
 Step N: ...
 👉 纯线性向后追加，无动态插入，完美覆盖主流大模型的增量缓存
══════════════════════════════════════════════════════════════════════
 🍞 底层面包：动态状态卡 + 观测 + 决策 (Dynamic State Cards)
 ──────────────────────────────────────────────────────────────────
 (1) 活动计划 (Plan ID & Title)
 (2) 全局进度与策略摘要 (Progress Summary)
 (3) 避坑指南 (Failed Attempts 失败教训)
 (4) 不可逆操作永久安全台账 (Side-Effects Ledger)
 (5) 核心事实记忆库 (Memories, 严格按 BTreeMap 字母序)
 (6) 任务数据目录 (TaskStore Resource 外部引用)
 (7) 当前任务面板 (Todo List)
 (8) 最新环境观测 (Observation)
 (9) 最终行动约束与决策指令 (Decision Instruction)
 👉 变化点全集中在末尾，绝不污染前面的长缓存；且最靠近输出端，利用 Recency Bias
══════════════════════════════════════════════════════════════════════
```

---

### 2. 字节级确定性保证 (Byte-Level Determinism)
- **从根源消除 Hash 随机性**：Rust 标准库的 `HashMap` 采用 SipHash 带有随机种子，同一结构体在不同进程或不同运行中序列化出的 JSON Key 顺序完全随机，会导致前缀缓存命中率暴跌为 0。
- **全面的 `BTreeMap` 迁移**：所有暴露给 Prompt 序列化的键值对（如 `memories`）统一采用 `BTreeMap<String, String>`，保证内存与序列化层面的绝对字母序。
- **分段指纹压力测试**：内置 `ContextFingerprint`（SHA-256 分段快照），经过 **1,000 次高并发迭代测试**，确保组装出的 Prompt 前缀字节级一致（100% Deterministic）。

---

### 3. 动态安全预算与滞后高低水位压缩 (Lagged Watermark Compaction)

为了杜绝大任务运行 50 步后的爆窗崩溃，系统实现了自适应的滞后水位线机制：

```
总窗口 (例如 32,768 tokens)
┌─────────────────────────────────────────────────────────────────┐
│ 静态前缀 │ 预留输出 │ 底部状态卡 │ 安全余量(15%) │   可分配历史容量   │
└─────────────────────────────────────────────────────────────────┘
                                                ▲                ▲
                                                │                │
                                           低水位线 (50%)   高水位线 (80%)
                                                │                │
                                                └── 触发压缩 ◄────┘
```

- **动态预算推导**：`ContextBudget::derive(window, max_output, static_prefix_tokens)`，全量扣除系统提示、活跃工具手册、终极目标与 15% 绝对安全缓冲区。
- **滞后高低水位**：当历史 Token 触碰 **80% 高水位线** 时触发压缩；压缩引擎一次性计算出需要归档的步数，通过单次 `drain(0..compact_count)` 批量移除早期历史（$O(n)$ 单趟完成，避免循环 `remove(0)` 的 $O(n^2)$ 内存抖动），直至历史降至 **50% 低水位线**。
- **0 依赖硬保底**：内置 `RuleBasedSummarizer`，无需向 LLM 发起异步总结请求，100% 永不失败、永不 Crash。

---

### 4. 混合落盘与零冗余存储 (Hybrid Storage & Disk Spilling)
单步工具执行可能产生数十万字符的原始日志（如庞大的 DOM 树或编译输出）：

1. **入口硬截断 (Ingress Hard-Cap)**：单步输出超过 **12,000 字符 (~4,000 Tokens)** 立即进行安全截断。
2. **Head + Tail 智能采样**：提取头部 300 字符 + 尾部 500 字符 + 关键报错行，注入历史上下文，并附带唯一的 `output_id`。
3. **条件式磁盘溢出落盘**：
   - 输出 > 2,000 字符时，全量写入系统本地文件：`~/.local/share/free-api-chat/outputs/{output_id}.log`。
   - **SQLite 零冗余**：数据库仅存储 `[[spilled_to_disk:/path/...]]` 路径指针，避免大文本在 SQLite 数据库里产生双份数据膨胀。
   - **透明反查**：调用 `get_step_output(output_id)` 时，优先命中本地 `.log` 文件，未溢出的短文本从 SQLite 读取，对上层完全透明。

---

### 5. 核心记忆安检门与双向反馈 (Memory Security Gatekeeper)
为防止大模型幻觉导致安全事故或上下文爆仓，写入记忆卡时必须通过严格安检门：

- **敏感词正则黑名单**：严禁明文包含 `sk-`、`Bearer `、`ghp_`、`BEGIN PRIVATE KEY` 等凭据。
- **Unicode 字符数限制**：Key ≤ 40 字符，Value ≤ 300 字符（使用 `.chars().count()` 正确统计中英文 Unicode 字符，杜绝 Rust 字节长度误判）。
- **双向反馈闭环**：若记忆被拦截，`update_memories` 会返回具体拒绝原因，并在下一轮主动向模型推送友好提示，杜绝模型“以为记下了但没生效”的困惑循环。

---

### 6. 出口级消息角色合并 (Same-Role Consolidation)
部分大模型提供商（特别是 Anthropic Claude 系列）对消息角色的**严格交替**（`system -> user -> assistant -> user`）有硬性校验，连续多条 `user` 角色消息会导致 API 报 422 错误。

SandwichAgent 在 `assemble_messages()` 的最终出口处，通过纯函数 `consolidate_consecutive_messages` 进行单趟线性合并：
- 顶部静态区与底部动态状态卡中生成的多个连续 `user` 区块，会被无损自动合并为单一 `user` 消息，中间以 `\n\n` 优雅隔开。
- 从根本上免疫任何 Provider 的交替校验限制。

---

## 三、三级读路径检索体系与联邦检索 (Federated Retrieval)

### 1. 检索上限由“写”决定：定长路标 (Signpost)
检索的召回率并不取决于搜索算法有多玄妙，而是取决于**写入时是否已经是结构化信息**。
在每步执行完成写入时，系统自动生成定长路标（Signpost，约 30 Tokens）：
```text
[S12] shell_exec · cargo test · FAILED · link_error/openssl · out=89a7 · +2 files
```
定长、可正则检索、字节绝对确定，彻底替代散文化长篇日志。

---

### 2. 三级读路径拓扑 (Three-Tier Read Path)

```
┌─ 第 0 级：常驻区 (0 次网络往返，永远在上下文) ──────────────────┐
│ • Pinned 事实 / Constraints / Side-effects 不可逆台账           │
│ • Todo 状态 + 当前子目标 + 最新 Observation                      │
│ • 【最近路标索引】定长结构化签名                                │
└──────────────────────────────────────────────────────────────┘
                               │ 模型看到路标，决定下钻
┌─ 第 1 级：索引区 (1 次工具调用) ────────────────────────────┐
│ search(kinds=["history"], outcome=failed, query="openssl")   │
│ → 返回匹配的历史路标列表 + output_id (不返回正文)             │
└──────────────────────────────────────────────────────────────┘
                               │
┌─ 第 2 级：归档区 (按需精确切片) ──────────────────────────┐
│ get_step_output(output_id="89a7") 或 grep/切片读取             │
└──────────────────────────────────────────────────────────────┘
```

---

### 3. 统一检索接口设计 `search(...)`
模型只需要面对一个统一入口，但通过强类型参数表达精准意图：

```rust
pub struct SearchQuery {
    pub query: String,                         // 自由文本 / 关键词
    pub kinds: Option<Vec<SearchKind>>,       // ["history", "doc", "code"]
    pub outcome: Option<OutcomeFilter>,       // Failed | Succeeded (找踩坑记录的杀手级参数)
    pub path_prefix: Option<String>,          // "src-tauri/src/"
    pub since_step: Option<usize>,            // 步骤下界
    pub limit: Option<usize>,                 // 返回条数限制
}
```
**内部实现**：通过**联邦检索**并行路由：
- `kinds: ["history"]` → 路由至本地 SQLite FTS5 / BM25 分词索引。
- `kinds: ["code"]` → 路由至实时 `ripgrep` / AST 符号分析（绝不返回过期代码快照）。

---

## 四、自我改进闭环：经验蒸馏与淘汰门禁 (Experience Distillation)

### 1. Sleep-Time Compute（睡眠期异步计算）
避免在 Agent 热路径（Turn-Loop）中进行耗时的反思与记忆重构。在用户阅读回复或空闲的“睡眠期”，后台异步任务触发经验提炼，将主流程开销降为 0。

### 2. Hindsight Lesson 结构化抽取
当检测到执行链路经历 **[Step N 失败] → [Step M 成功修正]** 时，自动蒸馏为定长经验（Lesson）：

```rust
pub struct Lesson {
    pub symptom: String,              // "cargo test 报链接器错误: -lssl not found"
    pub root_cause: String,           // "系统缺少 openssl 开发包"
    pub fix: String,                  // "先执行 pacman -S openssl，再 cargo clean"
    pub scope: LessonScope,           // Session | Project | Global
    pub applied_count: u32,           // 应用计数器
    pub success_when_applied: u32,    // 应用后成功计数器
}
```

### 3. 淘汰门禁与防止自我污染
为防止 Agent 产生错误的自负偏见（“上次改这个文件报错了，以后绝不碰它”），引入硬性统计门禁：
- **淘汰准则**：当 `applied_count >= 5` 且 $\frac{\text{success\_when\_applied}}{\text{applied\_count}} < 0.5$ 时，自动标记为无效经验并淘汰降权。
- **Global 级人工审核**：跨项目级的全局规则必须在桌面 UI 经人类确认后方可常驻。

---

## 五、双模式人机协同 (Human-in-the-Loop)

这是市面上绝大多数 Agent 框架都不具备的工程级人机交互能力。

### 模式 A：人类主动抢方向盘（异步安全点拦截）
AI 运行途中，你可以随时在干预框输入指令（如 *"换个思路，不要用正则提取，用 cheerio 解析"*）：
- 系统**不暴力杀死进程**，而是在 AI 完成当前原子动作后的**下一个安全点 (Safepoint)** 通过 `try_recv()` 检查消息。
- 人类指令作为最高优先级事件注入上下文，AI 会在 `reflection` 中承认收到，并重写规划改道。

### 模式 B：AI 主动挂起求助（真正的阻塞等待）
当 AI 遇到验证码、扫码登录或高风险破坏性动作时，它会主动调用 `ask_human` 工具：
- Rust 后端触发 `rx.recv().await` 真正挂起，**Token 消耗与 CPU 占用立刻归零**。
- 前端弹出醒目的橙红色求助卡片，人类处理完成并输入回复后，AI 带着最新指令被唤醒继续。

---

## 六、模块结构与代码索引

上下文引擎模块全面解耦，按单一职责划分（11 个子文件，平均约 100 行）：

```
src-tauri/src/agent/context/
├── mod.rs          # 模块对外统一 re-export 出口
├── types.rs        # 核心数据结构 (SandwichContext, ChatMessage, FailedAttempt, SideEffectRecord)
├── assembler.rs    # 三明治组装器 (静态前缀 -> 历史流水 -> 动态状态卡 -> 出口角色合并)
├── sampling.rs     # 写入即定型 (4k 硬截断 + 智能 Head/Tail 采样 + output_id 生成)
├── compaction.rs   # 压缩引擎 (80%/50% 滞后高低水位 + drain 批量截断 + Summarizer trait)
├── budget.rs       # 零依赖 Token 估算器与动态 ContextBudget 预算推导
├── determinism.rs  # SHA-256 语义切片指纹快照与 1000 次确定性测试套件
├── memory.rs       # 核心事实记忆安全安检门 (正则拦截 + 长度校验 + BTreeMap 字母序渲染)
├── audit.rs        # 不可逆外部动作安全永久台账 (SideEffectRecord 自动聚合折叠)
├── render.rs       # 避坑指南、TaskStore 目录、Todo 面板与决策指令渲染器
└── cache_policy.rs # Provider 差异化缓存块对齐策略扩展接口 (DeepSeek/OpenAI/Default)
```

---

## 七、快速上手与运行

### 1. 环境依赖
- **Rust** 1.78+ (`cargo`)
- **Node.js** 20+ (`npm` / `pnpm`)
- **Google Chrome**（用于 CDP 浏览器自主控制）

### 2. 本地开发与测试
```bash
# 1. 克隆代码仓库
git clone https://github.com/your-username/free_api.git
cd free_api

# 2. 安装前端依赖
npm install

# 3. 运行全量单元测试（验证 48 个上下文与状态机测试套件）
cargo test --manifest-path src-tauri/Cargo.toml

# 4. 启动本地桌面客户端
npm run tauri dev
```

---

## 📄 License
本项目采用 **MIT License** 开源协议。
欢迎提交 Issue 与 Pull Request 共同完善新一代 Agent 架构！⭐


