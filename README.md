# 🤖 SandwichAgent (Free API Agent)

<p align="center">
  <strong>基于 Rust + Tauri 2.0 构建的高确定性、低 Token 成本的本地自主 Agent 平台</strong><br/>
  为「又臭又长又容易翻车的重型编码任务」而生
</p>

<p align="center">
  <img src="https://img.shields.io/badge/built_with-Rust-orange?logo=rust" />
  <img src="https://img.shields.io/badge/Framework-Tauri_2.0_+_Vue_3-blue?logo=tauri" />
  <img src="https://img.shields.io/badge/Architecture-Sandwich_Context_Engine-purple" />
  <img src="https://img.shields.io/badge/Caching-Prefix_Cache_Optimized-brightgreen" />
  <img src="https://img.shields.io/badge/Protocol-MCP_+_HTTP-green" />
  <img src="https://img.shields.io/badge/license-MIT-lightgrey" />
</p>

---

## 🚦 系统能力总览 (Current Capabilities)

| 架构域 | 核心机制 | 状态 |
|:---|:---|:---:|
| **三明治上下文引擎** | 冻结静态前缀 + 纯追加流水 + 尾部动态状态卡 | ✅ 生产就绪 |
| **前缀缓存纪律** | 写入即定型、BTreeMap 字母序、真实值水位校准、出口级角色合并 | ✅ 生产就绪 |
| **检索体系** | 定长路标 + SQLite FTS5 + ripgrep 联邦检索 + `output_id` 句柄回查 | ✅ 生产就绪 |
| **检索去重与索引** | 同参检索锚点复用、同路径 signpost 软引导、进程级 SessionSearchIndex（LRU 128 上限 + 删除即清） | ✅ 生产就绪 |
| **门禁与质量对账** | submit_plan 风险预检、方案交叉审查（child_reviewer）、阶段写入审查、finish 终态对账（Git Diff 行级证据） | ✅ 生产就绪 |
| **多智能体并发** | child_worker / child_writer / child_reviewer + job_engine 异步任务池 + wait_jobs 信箱语义 | ✅ 生产就绪 |
| **安全护栏** | 工作区路径牢笼、写副作用门（含 move/rename/remove 命名空间剥离）、破坏性命令拦截、工作区软锁 | ✅ 生产就绪 |
| **死锁逃生** | 连续 8 次门禁拦截 → 确定性降级中断（`consecutive_gate_blocked` 持久化计数） | ✅ 生产就绪 |
| **人机协同 (HITL)** | 异步抢方向盘（安全点拦截）+ AI 阻塞式求助（ask_human） | ✅ 生产就绪 |
| **多通道接入** | 桌面 WebView + HTTP/Mobile Web 远程通道 | ✅ 生产就绪 |
| **评测基础设施** | evaluation_runner 实弹回放、eval_oracle 阶段断言、Token/步数/缓存统计 | ✅ 生产就绪 |
| **自我改进闭环** | Sleep-Time Compute + Lesson 蒸馏门禁 | 🚧 设计定稿（RFC） |
| **工具输出三权分立** | `DispatchResult.view`：模型文本 / 无损存档 / UI 结构化卡片正交投影 | ✅ MVP 已落地 |

变更台账见 [`OVERNIGHT_LOOP.md`](./OVERNIGHT_LOOP.md)——每个阶段的改动、验证证据与实弹评测记录都在其中。

---

## 📖 目录

- [一、设计哲学](#一设计哲学与理论背景)
- [二、三明治上下文引擎](#二真三明治上下文引擎-sandwich-context-engine)
- [三、检索体系：脱水不失忆](#三检索体系脱水不失忆)
- [四、门禁与质量对账](#四门禁与质量对账体系)
- [五、多智能体并发](#五多智能体并发架构)
- [六、人机协同](#六双模式人机协同-human-in-the-loop)
- [七、工具体系与输出三权分立](#七工具体系与输出三权分立)
- [八、代码库目录索引](#八代码库目录索引)
- [九、快速上手](#九快速上手与运行)
- [十、演进路线](#十演进路线-roadmap)

---

## 一、设计哲学与理论背景

### 1. 记忆的闭环：Write–Manage–Read

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
       │ - 敏感凭据过滤 │ - 45k 脱水低水位线     │ - 出口相邻合并 │
       └───────────────┘ - BTreeMap 字母序对齐  └────────────────┘
```

- **上下文内压缩**：滞后高低水位驱动，规则型保底摘要（零 LLM 调用、永不失败）。
- **外部多级存储**：热数据在 Context、温数据在 SQLite、超长原始日志溢出落盘 `.log` 文件。
- **分层虚拟上下文**：`output_id` 引用句柄 + `task_store.search_step_output` 按需回查。

### 2. 为什么拒绝「统一存储大杂烩 (God Table)」？

> *统一检索接口 ≠ 统一存储表。Rust 的类型系统是最大的工程资产，God Table 会把编译期类型安全赶进 SQL 和 JSON 字符串里。*

系统规则是静态的、代码是高频编辑的、执行历史是只追加的、记忆卡是键值更新的——生命周期完全不同的数据绝不混库。代码检索走实时 `ripgrep`（不缓存过期快照），历史检索走 SQLite FTS5，规则常驻上下文。

### 3. 为什么坚守「写入即定型 (Immutable on Write)」？

现代大模型的前缀缓存机制下，**前缀中哪怕一个标点变化，其后所有 KV Cache 全部击穿**，延迟暴增 3~5 倍、Token 全额计费。因此任何工具输出在 `add_step` 瞬间即完成采样、脱水、截断与结构化，压入历史后**永不回改**。脱水降级采用**批量阈值**而非逐轮滑动——滑动每步都会击碎缓存前缀。

### 4. 前缀缓存的工业级真相

- **块对齐**：DeepSeek 64 tokens、OpenAI 128 tokens（≥1024 才触发）、Anthropic 需显式 `cache_control`。
- **TTL**：各厂商缓存 5~10 分钟，长间隔交互的 Miss 是物理机制不是故障。
- **告别虚荣命中率**：128k 上下文下 99.9% 命中率是分母效应。真实 KPI = **每步 Miss Token 绝对量 + 每步单价 + TTFT**。

---

## 二、真·三明治上下文引擎 (Sandwich Context Engine)

### 1. 三层拓扑

```
══════════════════════════════════════════════════════════════════════
 🍞 顶层面包：100% 静态前缀
 ──────────────────────────────────────────────────────────────────
 1. system_prompt / active_tool_detail / user_goal
 👉 稳吃 DeepSeek(64) / OpenAI(1024) 前缀缓存
══════════════════════════════════════════════════════════════════════
 🥩 中间夹心：纯追加历史流水
 ──────────────────────────────────────────────────────────────────
 Step 1..N: [Action Brief] -> [Sampled Output (output_id=...)]
 👉 无动态插入，完美覆盖增量缓存
══════════════════════════════════════════════════════════════════════
 🍞 底层面包：动态状态卡（变化点全集中尾部）
 ──────────────────────────────────────────────────────────────────
 计划 / 进度摘要(版本化指针) / 避坑台账 / 不可逆台账 / 记忆(BTreeMap)
 / TaskStore 目录 / Todo / 最新观测 / 决策指令
 👉 利用 Recency Bias，且不污染前面长缓存
══════════════════════════════════════════════════════════════════════
```

### 2. 字节级确定性

- `HashMap` 的 SipHash 随机种子会让同一结构体序列化出随机 key 序——**全面迁移 `BTreeMap`** 保证绝对字母序。
- `ContextFingerprint`（SHA-256 分段快照）千次迭代压测，前缀字节级一致。

### 3. 滞后高低水位压缩

- `ContextBudget::derive` 全量扣除静态前缀、工具手册、输出预留与 15% 安全余量；
- 历史触 **80% 高水位** → 单次 `drain(0..n)` 批量归档至 **50% 低水位**（O(n) 单趟）；
- **真实值校准**：本地估算对中文/JSON 按字节超计 40-60%，曾把真实占用 16-26% 误判为高水位。现以**服务端回传的真实 prompt tokens** 为锚成对持久化，缺失时回退本地估算；
- 45k 脱水低水位线把上下文控制在 1.5万~4.5万 的健康锯齿波内。

### 4. 混合落盘零冗余

- 入口硬截断 12,000 字符；Head 300 + Tail 500 + 错误行智能采样入史，附 `output_id`；
- 输出 >2,000 字符全量落盘 `~/.local/share/free-api-chat/outputs/{output_id}.log`，SQLite 只存路径指针；
- `get_step_output(output_id)` 透明反查：先命中 `.log`，短文本走 SQLite。

### 5. 记忆安检门

- 正则黑名单拦截 `sk-`/`Bearer `/`ghp_`/私钥等明文凭据；
- Key ≤40 / Value ≤300 Unicode 字符；被拦截时主动向模型反馈原因，杜绝"以为记下了"。

### 6. 出口级角色合并

Anthropic 系对 `user`/`assistant` 严格交替有硬校验。组装器出口保持原始序列，**归并下沉到 provider 适配层**（OpenAI 兼容不合并 / Anthropic 系归并），历史尾部跨轮字节稳定——实测缓存命中 84.76%→90.13%。

---

## 三、检索体系：脱水不失忆

### 1. 定长路标 (Signpost)

检索召回率取决于写入时是否已结构化。每步执行完成生成约 30 Token 的定长路标：

```text
[S12] shell_exec · cargo test · FAILED · link_error/openssl · out=89a7 · +2 files
```

### 2. 三级读路径

```
┌─ 第 0 级：常驻区（0 网络往返）── Todo/台账/最近路标索引 ────────┐
├─ 第 1 级：索引区（1 次工具调用）─ search(kinds=["history"]) ───┤
│   → 返回路标 + output_id（不返回正文）                          │
├─ 第 2 级：归档区（按需切片）── get_step_output(output_id) ─────┘
```

### 3. 联邦检索 `search(...)`

```rust
pub struct SearchQuery {
    pub query: String,
    pub kinds: Option<Vec<SearchKind>>,       // history | doc | code
    pub outcome: Option<OutcomeFilter>,       // Failed | Succeeded（找踩坑记录）
    pub path_prefix: Option<String>,
    pub since_step: Option<usize>,
    pub limit: Option<usize>,
}
```

`history` → SQLite FTS5/BM25；`code` → 实时 ripgrep/AST。

### 4. 检索经济学（长任务实测病灶修复）

- **同参检索去重**：完全相同的检索参数命中进程级 `SessionSearchIndex`，返回紧凑锚点（`search-cache` 卡片 + `output_id`），不再重复执行与重复入史；
- **同路径 signpost**：同一路径重复读取时注入软引导而非沉默放行；
- **索引治理**：LRU 容量 128 会话、按 `Instant` 淘汰、`delete_session` 显式清理内存索引；
- **写失效**：写类步骤使相关缓存条目失效，防止旧检索结果遮蔽新事实。

### 5. `agent_run_steps` 行式步存储

长任务曾存在 O(n²) 病灶：每步把整个 `steps_json` 反序列化→追加→重序列化。现为行式表 `agent_run_steps(run_id, seq, step_json)` 每步单插一行；`append_step` 与 trace 元数据更新在**同一 SQLite 事务**内提交；读取行优先、缺失回退 legacy `steps_json`（兼容旧快照与测试种子）。

---

## 四、门禁与质量对账体系

这是本项目区别于通用 Agent 框架的护城河：**门禁不止于权限，更对模型的"声称"做物理对账**。

### 1. 方案门禁（写前）

- `submit_plan` 是独立协议动作（禁止与工具调用混合提交），进入审批流程；
- **风险预检**：静态防御设计检查（路径校验、错误处理、回滚方案等），缺项当场驳回并要求补强；
- **交叉审查**：需独立 `child_reviewer` 出具评分卡（correctness/safety 分维度），评分异常偏低（<60）触发人工核验；审查轮次有预算上限，防无限打回空转；
- **安全策略引擎**（`agent_policy.rs`）：Policy-as-Code 门禁，危险模式强力阻断或要求专家联合会审。

### 2. 阶段级写入审查（写中）

`stage_review.rs` 补齐「方案批准 → 收尾审查」中段的独立审查缺口，双触发先到先审。

### 3. finish 终态对账（收尾）

模型说"完成了"不算数，需过 `finish_guard` 两阶段交付：

1. `prepare_finish` **物理对账**：Todo 勾选、Git Diff 实际修改行（行号漂移地图）、验证命令是否真的跑过——对不上当场打回并附缺失清单；
2. 通过签发 `FinalizationReceipt`，正式 `finish` 毫秒级放行；
3. **P3 死锁逃生口**：连续 **8 次**门禁拦截且无产出（`consecutive_gate_blocked` 持久化计数，重启不丢）→ 重写为 `RUN_INTERRUPTED_ROUTE`，走 `finish_interrupted_run` 安全收尾：标记失败、取消子任务、发停止事件——宁可降级交卷，绝不无限空转。普通工具失败不计数（那是正常反馈）。

### 4. 写副作用门（执行面）

`dispatcher.rs::is_write_or_side_effect_tool` 同时按**工具名与 action** 双重判定，剥离 MCP/插件命名空间后匹配 `write/edit/delete/save/move/rename/remove`——`remote-hub/move_file` 之类照样被拦截；`read_text_file`/`list_directory`/`search_graph`/`release_lock` 等只读工具不误伤。

---

## 五、多智能体并发架构

宏观任务级并发（比单步多工具并发更难的部分）：

- **`child_worker`**：侦察兵/执行员，独立沙箱上下文与 Token 预算，完成只回精炼结论——父会话上下文不爆炸；
- **`child_writer`**：隔离的写入执行员；
- **`child_reviewer`**：独立审查员（交叉审查/阶段审查的执行体）；
- **`job_engine`**：异步任务池，`dispatches: [...]` 一步批量扇出多个子任务并行推进；
- **`wait_jobs` 信箱语义**：精确区分「在途任务」「待回收结论」「已消费」「未派发」四种状态，杜绝空轮询烧 Token；
- **`watch_engine`**：持续监听目标变化，变化事件自动注入上下文（免轮询）；收尾前必须 `watch_stop` 否则 finish 拦截；
- **`scheduler`**：定时触发器（daily/interval），让 Agent 具备主动发起能力；
- **`workspace_lock`**：同项目目录同时只允许一个 run，防并发写踩踏；僵尸锁自愈、同会话自动协同打断；
- **`interrupt_rx`（tokio watch 广播）**：取消信号贯通 LLM 流式推理、子任务、审查、终态结算——点停止即全链掐断，无孤儿后台任务。

---

## 六、双模式人机协同 (Human-in-the-Loop)

### 模式 A：人类抢方向盘（异步安全点）

运行途中输入指令 → **不杀进程**，当前原子动作完成后的安全点 `try_recv()` 拦截 → 人类指令作为最高优先级事件注入 → 模型在 reflection 中确认并改道。

### 模式 B：AI 挂起求助（真阻塞）

`ask_human` 触发 `rx.recv().await` 真挂起——**Token/CPU 消耗归零**，前端弹求助卡片，人类答复后唤醒继续。连续求助有冷却防滥用；无应答超时走降级路径而非死等。

---

## 七、工具体系与输出三权分立

### 1. 双轮驱动

`commands/`（人类/Vue 前端的 Tauri API）与 `agent/tools/`（模型自用的原生工具箱）彻底分离，底层共享同一 SQLite。

### 2. 统一工具契约

```rust
pub fn dispatch(db: &DbState, action: &str, params: &serde_json::Value) -> DispatchResult

pub struct DispatchResult {
    pub stdout: String,          // 给模型看的观察文本（采样后）
    pub stderr: String,          // 失败原因/拦截说明 → 引导模型纠错
    pub success: bool,
    pub route: String,           // 全链路审计路由标记
    pub view: Option<Value>,     // 给 UI 看的结构化视图（可选）
}
```

### 3. 三权分立（借鉴 DeepSeek Harness 思路）

同一事实两路正交投影：

- **模型**看 `stdout`（高信噪比采样文本，省 Token）；
- **UI** 看 `view`（结构化字段 → 类型化卡片，零正则猜测）；
- **存档**走 `output_id` + step json（无损可回放）。

已接入 `kind:"search"`（命中列表卡）与 `kind:"search-cache"`（去重锚点卡）。**扩展新外观 = 工具返回塞 `view:{kind:...}` + 前端加一个 kind 分支**，无 view 自动回退文本启发式，渐进不死。

### 4. 新增内置工具四步

1. `prompts/tools/xxx.md` 写 Agent 可读的使用说明；
2. `agent/tools/xxx.rs` 实现 `dispatch()`；
3. `dispatcher.rs` 注册一行路由；
4.（可选）`commands/` 暴露配套 Tauri Command。

---

## 八、代码库目录索引

### 1. Agent 内核（`src-tauri/src/agent/`，112 个 Rust 文件）

| 模块 | 职责 |
|:---|:---|
| `main_loop.rs` | 任务顶层状态机：跨步流转、门禁拦截、P3 死锁逃生口、TaskStore 同步、完成结算 |
| `loop_runner.rs` | 单步原子流水线：协议动作解析（submit_plan/prepare_finish/finish）、工具执行、事件发射 |
| `call_llm_phase.rs` | LLM 调用相：流式推理、Token 计量、中断信号接入 |
| `dispatcher.rs` | 工具分发中枢：写副作用门、只读预算拦截、内置/MCP 分流 |
| `plan_gate.rs` | 方案审批结论解析与前置写入门禁 |
| `stage_review.rs` | 阶段级写入审查（批准→收尾的中段缺口） |
| `reviewer_gatekeeper.rs` | 审查员结果解析、评分卡裁决、预算管控 |
| `review_contract.rs` | 审查输入/输出契约定义 |
| `finish_guard/` | finish 终态质询：Diff 物理对账、行号漂移地图、Receipt 签发 |
| `finalization.rs` | 终态收敛与交付结算 |
| `agent_policy.rs` | Policy-as-Code 安全策略引擎（危险模式阻断/联合会审升级） |
| `gate_messages.rs` | 门禁驳回话术集中管理 |
| `job_engine.rs` | 异步任务池：子任务派发、生命周期、结果信箱 |
| `child_worker.rs` / `child_writer.rs` / `child_reviewer.rs` | 三类子 Agent 角色（侦察/写入/审查） |
| `wait_jobs`（内嵌） | 信箱语义：在途/待回收/已消费/未派发 四态区分 |
| `watch_engine.rs` | 持续监听引擎：变化事件自动注入 |
| `scheduler.rs` | 定时触发器（daily/interval 主动任务） |
| `workspace_lock.rs` | 工作区软锁：单 run 排他、僵尸自愈、防踩踏释放 |
| `checkpoint.rs` | 幂等断点快照 + 故障自愈（`RecoveryDecision`）、`consecutive_gate_blocked` 持久化 |
| `run_state.rs` | 权威运行状态机 `AgentRunMachine` → 前端事件 |
| `run_trace.rs` | 运行轨迹持久化：`agent_run_steps` 行式存储 + 事务化 append |
| `conversation.rs` / `control.rs` | 会话生命周期 / 人机干预通道 |
| `retrieval/` | 联邦检索：FTS5 引擎、路标召回、SessionSearchIndex（LRU）、`search`/`view` 产出 |
| `context/` | 三明治引擎：assembler/budget/compaction/determinism/memory/render/sampling/audit/no_progress |
| `mcp/` | MCP 协议：stdio Actor + HTTP/SSE 客户端、注册表、会话引导 |
| `model_filter/` | 模型族检测/normalizer/healer（provider 差异适配） |
| `plan_tree/` + `plan_tree_dispatch.rs` | 计划树执行引擎（seq/par）、受控并行分发（单批≤2） |
| `channel/` | 多通道接入：HTTP 服务、mobile_web、bridge |
| `evaluation*.rs` / `eval_oracle*.rs` | 实弹评测：回放器、阶段断言、Token/步数统计、分析 |
| `tools/` | 原生工具箱：local_file、local_shell、mcp_manager、model_manager、role_manager、skill_manager、project_manager、schedule_manager、system_info、wechat_manager |
| `host_verify.rs` / `change_verification.rs` / `target_block.rs` | 宿主侧验证、变更核验、目标区块 |
| `task_store.rs` / `task_summary.rs` / `artifact_manager.rs` | 结构化数据区 / 交付物汇总 / 工件管理 |
| `telemetry.rs` / `tool_manual.rs` / `protocol.rs` | 遥测 / 工具手册 / 协议定义 |
| `skill_context.rs` / `lesson.rs` / `router.rs` / `project_map.rs` / `web_search.rs` / `file_parser.rs` / `context_collector.rs` / `prompt_assembly.rs` | 技能装载 / 避坑经验 / 模型路由 / 项目地图 / 联网搜索 / 文件解析 / 上下文采集 / 提示词组装 |

### 2. 前端（`src/`，Vue 3 + TS + Vite）

| 位置 | 职责 |
|:---|:---|
| `views/AgentView.vue` | Agent 主工作台（时间线 + 干预 + 检查器） |
| `components/agent/AgentChatStream.vue` | 执行流水线渲染：通用工具行 + 文件操作卡 + 协同胶囊 + **`view.kind` 类型化卡片** |
| `components/agent/types.ts` | `AgentStepItem.view: ToolView` 判别联合 |
| `composables/useAgentSession.ts` | 事件监听与 step 归一化（live + trace 回放双路 view） |
| `composables/workerUtils.ts` | 文本启发式回退（无 view 步骤的渲染兜底） |
| `components/charts/` | ECharts/Mermaid/HTML 沙箱、多标签交付卡 |
| `views/ApiManager.vue` / `PluginView.vue` / `ChatView.vue` | 模型渠道 / 能力中心 / 纯净对话 |

### 3. 基座与命令层

- `src-tauri/src/db.rs`：SQLite 连接池 + schema 迁移（含 `agent_run_steps`）；
- `src-tauri/src/commands/`：chat/models/mcp/skills/tasks 五个交互控制器；
- `src-tauri/src/llm.rs` / `task_registry.rs` / `lib.rs` / `main.rs`；
- `src-tauri/prompts/`：core_logic、agent_system_prompt、skills/、tools/；
- `plugins/`：MCP 插件 YAML 目录；`scripts/eval_runner.mjs`：实弹评测入口。

---

## 九、快速上手与运行

### 环境依赖
- **Rust** 1.78+、**Node.js** 20+

### 开发

```bash
git clone <repo> && cd free_api
npm install

# 后端单元测试（580+ 项）
cargo test --lib --manifest-path src-tauri/Cargo.toml

# 前端类型检查 + 构建
npm run build          # vue-tsc --noEmit && vite build

# 桌面客户端
npm run tauri dev      # Linux Wayland 环境用 npm run dev:wayland
```

### 实弹评测

```bash
npm run eval:real       # scripts/eval_runner.mjs 驱动真实会话回放
npm run eval:real:dry   # 干跑校验配置
```

---

## 十、演进路线 (Roadmap)

### 已完成（详见 OVERNIGHT_LOOP.md）
- [x] 子 Agent 并发体系（worker/writer/reviewer + job_engine + dispatches）
- [x] 写副作用门（含 move/rename/remove + 命名空间剥离）
- [x] finish 两阶段对账 + P3 死锁降级逃生口
- [x] 检索去重锚点 + 进程级索引治理
- [x] `agent_run_steps` 行式存储（消除 O(n²) 步持久化）
- [x] 工具输出三权分立（`DispatchResult.view`）
- [x] 工作区软锁、HTTP/Mobile Web 通道、watch 监听引擎、定时调度器

### 进行中 / 规划
- [ ] **Sleep-Time Compute**：睡眠期异步经验蒸馏 + Lesson 淘汰门禁（RFC 定稿）
- [ ] **零信任渲染面**：不可信内容边界包裹、出站渲染脱敏完善
- [ ] **会话表注册表化**：`SESSION_SCOPED_TABLES` 驱动级联删除 + schema 反射测试（新增表忘登记 = 测试红）
- [ ] **view 接入面扩展**：`local_file.read`/`edit`/`local_shell` 等高频工具的类型化卡片
- [ ] **P3 实弹验证**：无人类死锁场景下 8 连拒 → 降级中断的实跑观测
- [ ] **步持久化耗时测量**：长会话 `append_step` 前后延迟对比

---

## 📄 License

本项目采用 **MIT License** 开源协议。
