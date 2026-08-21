# 🤖 SandwichAgent (Free API Agent)

<p align="center">
  <strong>基于 Rust + Tauri 2.0 构建的高性能、高确定性本地原生自主系统级 Agent 平台</strong>
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

## 🚦 系统各模块实现状态 (Implementation Status)

| 架构分层 | 核心模块 | 状态 | 验证与测试 |
|:---|:---|:---:|:---|
| **一、设计哲学与基座** | 理论分层、拒 God Table、确定性前缀 | ✅ **生产就绪** | 1,000 次指纹测试 |
| **二、三明治上下文引擎** | 冻结静态区 + 纯追加流水 + 尾部状态卡 | ✅ **生产就绪** | 49 个单元测试全绿 |
| **三、三级检索体系** | 定长路标 + SQLite FTS5 + 统一 `search` | ✅ **生产就绪** | FTS5 + ripgrep 联邦检索 |
| **四、自我改进闭环** | Sleep-Time Compute + Lesson 抽取与门禁 | 🚧 **设计定稿 (Roadmap RFC)** | 规划中，下一步落地 |
| **五、人机协同 (HITL)** | 异步抢方向盘 (`try_recv`) + AI 阻塞求助 | ✅ **生产就绪** | 状态机测试覆盖 |

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
- [三、🚧【规划中】三级读路径检索体系与联邦检索 (Federated Retrieval RFC)](#三🚧规划中三级读路径检索体系与联邦检索-federated-retrieval-rfc)
  - [1. 检索上限由“写”决定：定长路标 (Signpost)](#1-检索上限由写决定定长路标-signpost)
  - [2. 三级读路径拓扑 (Three-Tier Read Path)](#2-三级读路径拓扑-three-tier-read-path)
  - [3. 统一检索接口设计 `search(...)`](#3-统一检索接口设计-search)
- [四、🚧【规划中】自我改进闭环：经验蒸馏与淘汰门禁 (Experience Distillation RFC)](#四🚧规划中自我改进闭环经验蒸馏与淘汰门禁-experience-distillation-rfc)
  - [1. Sleep-Time Compute（睡眠期异步计算）](#1-sleep-time-compute睡眠期异步计算)
  - [2. Hindsight Lesson 结构化抽取](#2-hindsight-lesson-结构化抽取)
  - [3. 淘汰门禁与防止自我污染](#3-淘汰门禁与防止自我污染)
- [五、双模式人机协同 (Human-in-the-Loop)](#五双模式人机协同-human-in-the-loop)
  - [模式 A：人类主动抢方向盘（异步安全点拦截）](#模式-a人类主动抢方向盘异步安全点拦截)
  - [模式 B：AI 主动挂起求助（真正的阻塞等待）](#模式-bai-主动挂起求助真正的阻塞等待)
- [六、双轮驱动架构与工具体系 (Dual-Engine Architecture: Commands & Agent Tools)](#六双轮驱动架构与工具体系-dual-engine-architecture-commands--agent-tools)
- [七、全代码库目录与文件深度索引 (Exhaustive Codebase & File Index)](#七全代码库目录与文件深度索引-exhaustive-codebase--file-index)
- [八、快速上手与运行](#八快速上手与运行)
- [九、待办与演进路线 (Roadmap & TODO)](#-待办与演进路线-roadmap--todo)


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
- **分段指纹压力测试**：内置 `ContextFingerprint`（SHA-256 分段快照），经过 **1,000 次顺序迭代压力测试**，确保组装出的 Prompt 前缀字节级一致（100% Deterministic）。

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

1. **入口硬截断 (Ingress Hard-Cap)**：单步输出超过 **12,000 字符 (约 4,000~9,000 Tokens)** 立即进行安全截断。
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

## 四、🚧【规划中】自我改进闭环：经验蒸馏与淘汰门禁 (Experience Distillation RFC)

> [!NOTE]
> 本章为系统的**经验提炼与反思闭环设计蓝图 (Design RFC)**。旨在解决长程任务中的自我纠错与经验跨任务复用问题。

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

---

## 六、双轮驱动架构与工具体系 (Dual-Engine Architecture: Commands & Agent Tools)

系统采用现代化桌面 AI 应用的**双轮驱动解耦架构**，将“人类手动操作”与“Agent 自主执行”彻底按单一职责分离，底层 100% 共享 SQLite 数据与业务状态：

```
┌────────────────────────────────────────────────────────┐
│                   Vue 前端 UI 界面                      │
└────────────────────────────────────────────────────────┘
          │                                  ▲
   (用户手动操作 invoke)               (Agent 动态表单交互)
          ▼                                  │
┌──────────────────────┐          ┌──────────────────────┐
│  src/commands/       │          │  src/agent/tools/    │
│  ├─ models.rs (模型) │          │  ├─ model_manager.rs │
│  ├─ mcp.rs    (插件) │          │  ├─ mcp_manager.rs   │
│  ├─ skills.rs (技能) │          │  ├─ skill_manager.rs │
│  ├─ chat.rs   (对话) │          │  └─ ... (原生AI工具)  │
│  └─ tasks.rs  (复盘) │          └──────────────────────┘
└──────────────────────┘                     ▲
          │                                  │
          │         ┌───────────────┐        │ (模型输出 JSON 规划)
          └────────►│ SQLite 数据库 ├────────┘
                    └───────────────┘
```

### 1. 通道分工与设计定位

| 架构层 | 目录位置 | 面向对象 | 职责与触发机制 |
|:---|:---|:---|:---|
| **UI 交互控制器** | `src-tauri/src/commands/` | **人类用户 & Vue 前端** | 响应页面点击、表单提交、测速测试与会话管理，充当桌面端 API Controller。 |
| **Agent 原生工具箱** | `src-tauri/src/agent/tools/` | **大模型 Agent 自身** | 统一实现 `dispatch(db, action, params)` 契约，赋予 AI 自主查改模型、动态接入 MCP 与提纯 Skill 的能力。 |

### 2. 统一工具执行契约 (Universal Tool Contract)

所有内置 Agent 工具均实现标准单一入口契约：

```rust
pub fn dispatch(
    db: &DbState,
    action: &str,
    params: &serde_json::Value,
) -> DispatchResult
```

- **`stdout`**：成功信息（如 `✅ 成功接入 MCP 插件 [excel]，能力卡:...`），自动作为下一轮 **Observation (观察结果)** 喂给模型；
- **`stderr`**：失败原因或破坏性安全拦截（如 `⚠️ 安全保护：必须附带 confirm: true`），引导模型立即纠错或向用户求助；
- **`route`**：用于全链路审计与运行轨迹追踪（Run Trace）。

### 3. 水平扩展新工具标准流程 (3-Step Tool Pipeline)

后续若需增加新的内置工具（如 `db_manager`、`git_manager`）：
1. **定义规范**：在 `src-tauri/prompts/tools/xxx.md` 编写供 Agent 理解的参数与使用说明；
2. **实现分发**：在 `src-tauri/src/agent/tools/xxx.rs` 实现 `pub fn dispatch(...)`；
3. **注册路由**：在 `src-tauri/src/agent/dispatcher.rs` 中添加一行单流分发路由；
4. *(可选)* 如前端需配套可视化按钮，在 `src-tauri/src/commands/` 对应子模块暴露 Tauri Command。

---

## 七、全代码库目录与文件深度索引 (Exhaustive Codebase & File Index)

本节对项目内**每一个核心文件夹与文件**的具体职责、技术实现与设计定位进行详尽说明：

### 1. 后端核心架构 (`src-tauri/src/agent/`)

`agent/` 是整个 SandwichAgent 自主推理、状态机管理、三明治上下文装配与工具调度的核心内核：

| 子目录 / 文件路径 | 模块性质 | 核心职责与设计说明 |
| :--- | :--- | :--- |
| [`checkpoint.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/checkpoint.rs) | 状态容灾 | **幂等断点快照与故障自愈状态机**：负责每一步执行前后持久化快照，通过 `RecoveryDecision` 判定是直接续跑、等待人类确认还是重试未决副作用，确保崩溃后 0 丢失、0 重复执行。 |
| [`control.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/control.rs) | 人机通信 | **人机干预与实时打断控制中心**：维护跨线程 `HumanControl` 通道（Cancel/ReplaceGoal/Answer），支持毫秒级流式打断与写盘事务 1.5s 安全落盘缓冲期。 |
| [`conversation.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/conversation.rs) | 会话流 | **长期对话生命周期与多轮历史流管理**：负责将多轮任务有序归档进 SQLite 会话历史，保证恢复阶段不会重复向模型追加相同轮次。 |
| [`dispatcher.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/dispatcher.rs) | 路由中枢 | **Agent 工具分发中枢与代码安全预算守卫**：统一接收模型的 JSON 决策，内置只读代码预算拦截（超出范围自动注入纠偏），并统一派发至内置或 MCP 工具。 |
| [`lesson.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/lesson.rs) | 避坑经验 | **经验避坑库 (Failed Attempts) 与血缘反馈**：自动收集工具报错并生成精炼教训；在后续步骤检索命中后记录血缘因果，并根据任务成败进行显式打分。 |
| [`loop_runner.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/loop_runner.rs) | 单步执行 | **单步原子流水线 (CallLlm + RunTool)**：封装大模型动态路由规划、流式 Token 计量、工具执行、超时控制、自愈重试与优雅退出逻辑。 |
| [`main_loop.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/main_loop.rs) | 主生命周期 | **任务顶层调度器与运行状态机**：驱动 Agent 跨步流转、前缀缓存预热、死循环硬拦截、状态卡刷新、TaskStore 目录同步与任务完成结算。 |
| [`mcp_catalog.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/mcp_catalog.rs) | 能力提取 | **MCP 工具清单缓存与智能能力卡提纯**：首次扫描或变更时调用选定大模型，将几千行冗长 JSON Schema 提炼为几十 Token 的结构化能力卡，极大缩减 Prompt。 |
| [`mod.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/mod.rs) | 模块根 | **Agent 模块聚合导出与终态指令回归测试**：精简的 Hub 根，集中导出子模块并包含核心终态判定与工具菜单单元测试。 |
| [`project_map.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/project_map.rs) | 项目地图 | **工作区轻量项目地图生成器**：在代码审查或跨文件任务启动时，提取目录拓扑、入口文件与 Git 差异元数据（不读大文件正文），避免上下文膨胀。 |
| [`router.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/router.rs) | 模型路由 | **动态分级路由器**：根据当前任务目标与上下文复杂度（视觉图表、长篇代码、轻量步骤），动态在 Fast / Pro / Vision 模型阶梯间切换。 |
| [`run_state.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/run_state.rs) | 状态快照 | **权威运行状态机 (`AgentRunMachine`)**：维护严格的状态机流转图（Idle → Planning → ToolExecution → Finalizing → Done），向前端推送权威事件。 |
| [`run_trace.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/run_trace.rs) | 历史复盘 | **运行轨迹快照持久化**：独立保存包含思考过程、工具参数、中间结果与 Artifacts 的完整运行轨迹，供历史复盘与离线回放。 |
| [`skill_context.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/skill_context.rs) | 技能装载 | **工作区 Skill 与 AGENTS.md 规范加载器**：自动向上寻找工作区提示词文件，基于文件哈希缓存，避免每步重复读取。 |
| [`task_store.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/task_store.rs) | 结构化数据 | **会话独立任务数据区**：针对抓取/批量处理产生的长篇结构化数据，提供数据集创建、分页查询与键值存储，模型仅按需分页读取，杜绝上下文爆炸。 |
| [`task_summary.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/task_summary.rs) | 交付结算 | **任务成果提纯与交付物汇总**：在任务终态自动汇总产生的文件、图表、数据集与操作结论，生成高质量交付卡片。 |
| [`types.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/types.rs) | 类型定义 | **Agent 核心数据模型**：定义 `AgentInstruction`（模型返回的统一决策结构体）、`DispatchResult`、`TaskResourceRef` 等核心类型。 |
| [`web_search.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/web_search.rs) | 联网搜索 | **原生搜索引擎与网页正文提取器**：集成多源实时搜索，内置 HTML 转 Markdown 清洗流水线，剥离广告与噪声。 |

#### (1) 三明治上下文子引擎 (`src-tauri/src/agent/context/`)
- [`mod.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/context/mod.rs)：上下文聚合入口，定义核心结构体 `SandwichContext`。
- [`assembler.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/context/assembler.rs)：三明治拓扑拼装器（绝对静态前缀 + 纯追加流水 + 动态状态卡）。
- [`audit.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/context/audit.rs)：不可逆副作用安全台账 (`AuditLedger`)，永久记录写入/删除等关键操作。
- [`budget.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/context/budget.rs)：Token 动态预算与多语言/代码稠密度评估算法。
- [`cache_policy.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/context/cache_policy.rs)：前缀缓存物理对齐策略（64 / 128 / 1024 Token 门槛适配）。
- [`compaction.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/context/compaction.rs)：滞后高低水位自适应压缩器，保证历史流水平滑压缩，绝不破坏前缀稳定性。
- [`determinism.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/context/determinism.rs)：字节级确定性保证，提供 SHA256 指纹校验与字典序规范化。
- [`memory.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/context/memory.rs)：核心事实记忆卡（严格按 BTreeMap 字母序对齐，内置明文密钥过滤阻断）。
- [`render.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/context/render.rs)：动态状态卡（Todo、避坑教训、台账、数据目录）Markdown 模板渲染。
- [`sampling.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/context/sampling.rs)：大文本定向证据采样（Head/Tail 智能保留 + 错误堆栈优先提取）。
- [`types.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/context/types.rs)：定义 `ChatMessage`, `TokenBudget`, `MemoryCard`, `UsageMetrics` 等数据结构。

#### (2) Agent 原生内置工具箱 (`src-tauri/src/agent/tools/`)
- [`mod.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/tools/mod.rs)：定义 `BuiltinTool` 统一 Trait（规范 `name`, `dispatch`, `prompt_path`）与派发分流器。
- [`model_manager.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/tools/model_manager.rs)：大模型自主管理工具，支持 AI 自主查询/新增模型平台、调整推理参数与连通性验证。
- [`mcp_manager.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/tools/mcp_manager.rs)：MCP 插件热插拔工具，支持 URL+Token、本地 Stdio、JSON 清单批量导入与能力卡提炼。
- [`skill_manager.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/tools/skill_manager.rs)：Skill 技能管理工具，支持单条/多段 Markdown 导入与触发卡落库。

#### (3) 外部 MCP 协议调度中心 (`src-tauri/src/agent/mcp/`)
- [`mod.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/mcp/mod.rs)：模块聚合与多路复用并行测试。
- [`client.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/mcp/client.rs)：双轨客户端实现（Stdio 异步 Actor 通道 + HTTP/SSE 直连连接池）。
- [`registry.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/mcp/registry.rs)：插件注册中心，负责本地 YAML 目录扫描与 DB 动态插件挂载。
- [`types.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/mcp/types.rs)：JSON-RPC 2.0 请求/响应规范与插件配置定义。

#### (4) 动态检索与工作区搜索 (`src-tauri/src/agent/retrieval/`)
- [`mod.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/retrieval/mod.rs)：检索模块聚合与中英文检索单元测试。
- [`fts_engine.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/retrieval/fts_engine.rs)：SQLite FTS5 / Trigram 全文检索与自愈迁移引擎。
- [`memory_signpost.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/retrieval/memory_signpost.rs)：三层记忆路标召回与 Context 动态关联。
- [`workspace_search.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/retrieval/workspace_search.rs)：代码符号与文件系统联邦搜索。
- [`types.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/retrieval/types.rs)：检索请求、召回结果与元数据模型。

#### (5) 基础工具与流处理流水线 (`src-tauri/src/agent/utils/`)
- [`mod.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/utils/mod.rs)：工具模块聚合与测试。
- [`json_pipeline.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/utils/json_pipeline.rs)：JSON 流式提取、Markdown 围栏剥离、截断/单引号/尾逗号智能容错修复。
- [`llm_client.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/utils/llm_client.rs)：兼容 OpenAI 协议的流式客户端与 Token 计量统计器。
- [`log_sampler.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/utils/log_sampler.rs)：超长日志智能采样器（首尾保留 + 错误堆栈捕获 + 语义去重）。
- [`subprocess.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/utils/subprocess.rs)：跨平台异步子进程派生与超时控制。

#### (6) 复合计划树引擎 (`src-tauri/src/agent/plan_tree/`)
- [`executor.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/plan_tree/executor.rs)：串行 (`seq`) / 并行 (`par`) 计划树调度执行引擎。
- [`scope.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/plan_tree/scope.rs)：计划树变量作用域与依赖输出引用解析。
- [`timeout.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/plan_tree/timeout.rs)：节点级故障隔离与超时保护。
- [`types.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/agent/plan_tree/types.rs)：计划树节点、状态与拓扑模型。

---

### 2. Tauri 前端 API 交互控制器 (`src-tauri/src/commands/`)

`commands/` 专为 Vue 前端提供高性能异步 Tauri Command 路由接口：

| 文件路径 | 交互领域 | 暴露的主要功能 |
| :--- | :--- | :--- |
| [`chat.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/commands/chat.rs) | 对话与运行 | 会话生命周期、消息持久化、实时流式推理通信、原生独立 WebView 窗口创建、人类干预打断。 |
| [`models.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/commands/models.rs) | 模型渠道 | 平台提供商 CRUD、模型规格词典、实时流式连通性测试与延迟诊断。 |
| [`mcp.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/commands/mcp.rs) | MCP 插件 | MCP 插件列表、单条/批量导入、启用/禁用、热插拔探测与大模型能力卡提炼。 |
| [`skills.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/commands/skills.rs) | 技能中心 | 自定义提示词与 Skill CRUD、批量导入与大模型能力卡智能提纯。 |
| [`tasks.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/commands/tasks.rs) | 任务数据 | TaskStore 数据集目录查询、分页读取记录、历史运行轨迹与复盘快照导出。 |
| [`mod.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/commands/mod.rs) | 统一汇聚 | 统一汇聚并对外导出所有命令处理函数。 |

---

### 3. Rust 基座与系统核心 (`src-tauri/src/`)

- [`db.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/db.rs)：SQLite 数据库连接池初始化、数据库表 Schema 迁移与索引管理。
- [`llm.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/llm.rs)：底层模型平台参数、深度思考档位、重试策略与 OpenAI 协议标准封装。
- [`lib.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/lib.rs)：Tauri 应用构建入口、插件注册、状态初始化与 Command 路由挂载。
- [`main.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/main.rs)：桌面应用主程序二进制入口。
- [`task_registry.rs`](file:///Users/wx/Documents/free_api/src-tauri/src/task_registry.rs)：长耗时后台异步任务注册与生命周期追踪。

---

### 4. 提示词工程与工具规范 (`src-tauri/prompts/`)

- [`core_logic.md`](file:///Users/wx/Documents/free_api/src-tauri/prompts/core_logic.md)：Agent 主决策规划系统提示词（思维链、工具调用格式、安全边界与 JSON 协议）。
- [`agent_system_prompt.md`](file:///Users/wx/Documents/free_api/src-tauri/prompts/agent_system_prompt.md)：通用助手基准系统提示词。
- `skills/`:
  - [`chart_visualization.md`](file:///Users/wx/Documents/free_api/src-tauri/prompts/skills/chart_visualization.md)：图表与可视化交付规范（ECharts / Mermaid / HTML 沙箱）。
- `tools/`:
  - [`ask_human.md`](file:///Users/wx/Documents/free_api/src-tauri/prompts/tools/ask_human.md)：人机协同工具说明（确认、动态表单与求助）。
  - [`model_manager.md`](file:///Users/wx/Documents/free_api/src-tauri/prompts/tools/model_manager.md)：大模型平台与模型规格管理工具规范。
  - [`mcp_manager.md`](file:///Users/wx/Documents/free_api/src-tauri/prompts/tools/mcp_manager.md)：MCP 插件接入与能力卡管理工具规范。
  - [`skill_manager.md`](file:///Users/wx/Documents/free_api/src-tauri/prompts/tools/skill_manager.md)：工作区 Skill 与提示词管理工具规范。
  - [`web_search.md`](file:///Users/wx/Documents/free_api/src-tauri/prompts/tools/web_search.md)：全网实时搜索引擎规范。
  - [`web_fetch.md`](file:///Users/wx/Documents/free_api/src-tauri/prompts/tools/web_fetch.md)：网页正文提取工具规范。

---

### 5. Vue 3 前端界面与交互层 (`src/`)

`src/` 采用 Vue 3 + TypeScript + Vite 构建，提供专业级桌面交互体验：

| 目录 / 文件路径 | 分类 | 核心功能 |
| :--- | :--- | :--- |
| [`App.vue`](file:///Users/wx/Documents/free_api/src/App.vue) | 根组件 | 桌面主窗口容器、顶部导航与全局通知管理。 |
| [`main.ts`](file:///Users/wx/Documents/free_api/src/main.ts) | 入口 | Vue 应用初始化、Pinia 状态挂载与样式加载。 |
| [`style.css`](file:///Users/wx/Documents/free_api/src/style.css) | 样式 | 全局设计系统、暗黑/明亮主题与原子化 CSS 变量。 |
| **`views/`** | **页面视图** | |
| [`AgentView.vue`](file:///Users/wx/Documents/free_api/src/views/AgentView.vue) | 主工作台 | Agent 自动化任务核心视图（会话列表 + 执行时间线 + 人机干预 + 深度检查器）。 |
| [`ApiManager.vue`](file:///Users/wx/Documents/free_api/src/views/ApiManager.vue) | 模型管理 | 模型渠道配置、API Key 维护、流式连通性测试与延迟可视化诊断。 |
| [`ChatView.vue`](file:///Users/wx/Documents/free_api/src/views/ChatView.vue) | 纯净对话 | 经典多轮对话视图（支持思维链折叠、Token 速率统计与多模型切换）。 |
| [`PluginView.vue`](file:///Users/wx/Documents/free_api/src/views/PluginView.vue) | 能力中心 | MCP 插件与工作区 Skill 综合管理、批量导入与大模型能力卡智能提炼。 |
| **`components/agent/`** | **Agent 组件** | |
| [`AgentChatStream.vue`](file:///Users/wx/Documents/free_api/src/components/agent/AgentChatStream.vue) | 时间线 | 任务执行流水线组件（展示思考、计划、动作、工具输出折叠与代码高亮）。 |
| [`AgentComposer.vue`](file:///Users/wx/Documents/free_api/src/components/agent/AgentComposer.vue) | 输入框 | 任务输入面板（支持模型路由、深度思考档位、自动清历史开关）。 |
| [`AgentHumanInterventionCard.vue`](file:///Users/wx/Documents/free_api/src/components/agent/AgentHumanInterventionCard.vue) | 人机干预 | 人机交互卡片（AI 阻塞提问、动态表单填写、随时打断抢方向盘）。 |
| [`AgentInspector.vue`](file:///Users/wx/Documents/free_api/src/components/agent/AgentInspector.vue) | 深度检查器 | 运行时检查面板（查看核心记忆卡、不可逆操作台账、避坑教训、Todo 清单与 Token 明细）。 |
| [`AgentRecordPreview.vue`](file:///Users/wx/Documents/free_api/src/components/agent/AgentRecordPreview.vue) | 数据预览 | TaskStore 结构化任务数据区分页表格与 JSON 数据预览抽屉。 |
| [`AgentSettingsDrawer.vue`](file:///Users/wx/Documents/free_api/src/components/agent/AgentSettingsDrawer.vue) | 运行配置 | 全局执行参数抽屉（模型分级阶梯、超时控制、上下文窗口大小）。 |
| **`components/charts/`** | **可视化沙箱** | |
| [`EChartsBlock.vue`](file:///Users/wx/Documents/free_api/src/components/charts/EChartsBlock.vue) | 图表沙箱 | ECharts 图表动态渲染沙箱。 |
| [`HtmlPreviewBlock.vue`](file:///Users/wx/Documents/free_api/src/components/charts/HtmlPreviewBlock.vue) | 网页沙箱 | 原生 HTML/JS/CSS 独立安全沙箱渲染器。 |
| [`MermaidBlock.vue`](file:///Users/wx/Documents/free_api/src/components/charts/MermaidBlock.vue) | 架构图 | Mermaid 流程图、时序图与架构拓扑渲染器。 |
| [`MultiTabArtifactCard.vue`](file:///Users/wx/Documents/free_api/src/components/charts/MultiTabArtifactCard.vue) | 交付卡片 | 多标签页交付物容器（支持代码/图表/网页即时切换）。 |
| [`RichContentRenderer.vue`](file:///Users/wx/Documents/free_api/src/components/charts/RichContentRenderer.vue) | 富媒体 | 混合富媒体渲染器（自动识别 Markdown、数学公式与图表组件）。 |
| **`composables/`** | **组合式 API** | |
| [`useAgentSession.ts`](file:///Users/wx/Documents/free_api/src/composables/useAgentSession.ts) | 会话与事件 | 状态机流转与 Tauri 进度/状态/日志事件监听。 |
| [`useAgentTaskResources.ts`](file:///Users/wx/Documents/free_api/src/composables/useAgentTaskResources.ts) | 任务数据区 | TaskStore 前端状态同步与分页查询。 |
| [`useBrowserPreview.ts`](file:///Users/wx/Documents/free_api/src/composables/useBrowserPreview.ts) | 独立窗口 | 原生独立浏览器窗口与外部页面预览控制。 |
| [`usePersistedAgentConversation.ts`](file:///Users/wx/Documents/free_api/src/composables/usePersistedAgentConversation.ts) | 草稿持久化 | 本地草稿与会话持久化管理。 |

---

### 6. 插件、脚本与测试套件

- `plugins/`:
  - [`excel.yaml`](file:///Users/wx/Documents/free_api/plugins/excel.yaml)：本地 Excel 自动化处理 MCP 插件配置文件。
  - [`remote-hub.yaml.example`](file:///Users/wx/Documents/free_api/plugins/remote-hub.yaml.example)：远程 HTTP/SSE MCP 服务接入范例。
- `scripts/`:
  - [`test-configured-platforms.mjs`](file:///Users/wx/Documents/free_api/scripts/test-configured-platforms.mjs)：全平台模型批量测速与流式连通性诊断脚本。
- `test/e2e/`:
  - [`agent-thinking-policy.e2e.mjs`](file:///Users/wx/Documents/free_api/test/e2e/agent-thinking-policy.e2e.mjs)：Agent 深度思考策略与状态机端到端自动化测试。
  - [`capability-center.e2e.mjs`](file:///Users/wx/Documents/free_api/test/e2e/capability-center.e2e.mjs)：能力中心（MCP + Skill）批量导入与能力卡提炼端到端测试。


---

## 八、快速上手与运行

### 1. 环境依赖
- **Rust** 1.78+ (`cargo`)
- **Node.js** 20+ (`npm` / `pnpm`)

### 2. 本地开发与测试
```bash
# 1. 克隆代码仓库
git clone https://github.com/your-username/free_api.git
cd free_api

# 2. 安装前端依赖
npm install

# 3. 运行全量单元测试（验证 49 个上下文、检索与状态机测试套件）
cargo test --manifest-path src-tauri/Cargo.toml

# 4. 启动本地桌面客户端
npm run tauri dev
```

---

## 🚀 待办与演进路线 (Roadmap & TODO)

### 1. 🛡️ 双向零信任安全护栏引擎 (Bidirectional Zero-Trust Guardrails)
- [ ] **大模型调用前（上下文隔离）**：对 `web_fetch`、外部代码仓库读取等不可信数据自动包裹 `<untrusted_content_boundary>` 隔离沙箱，防止间接提示注入（Indirect Prompt Injection）。
- [ ] **工具物理执行前（物理闸门）**：
  - 拦截破坏性命令（如 `rm -rf /`、`mkfs`、`truncate`）；
  - 拦截特权提权（`sudo`、`chmod 777`）；
  - 工作区路径牢笼约束（`canonicalize` 物理防越界读取 `~/.ssh/`、`/etc/shadow`）；
  - 自动降级为 `ask_human (kind: "confirm")` 弹窗由用户手动授权。
- [ ] **出站渲染脱敏（Anti-Exfiltration）**：过滤模型输出中的 API Key 明文，剥离不可信外部 Markdown 隐蔽外带图片。

### 2. 👥 轻量级子 Agent 调度架构 (Sub-Agent Dispatcher & Scoped Context)
- [ ] **沙箱化子任务派发**：父 Agent 通过 `spawn_subagent` 工具派发长耗时或高 Token 消耗的专项任务（如批量爬取、长篇代码审校、数据清洗）。
- [ ] **上下文无污染隔离**：子 Agent 在独立的会话沙箱和 Token 预算中循环推进，完成后仅向父 Agent 交付精炼结论与 Artifacts，彻底解决父会话上下文爆炸问题。
- [ ] **父子状态联动**：前端树形展示主子任务关联与进度流转。

---

## 📄 License
本项目采用 **MIT License** 开源协议。
欢迎提交 Issue 与 Pull Request 共同完善新一代 Agent 架构！⭐


