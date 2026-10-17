# 🤖 SandwichAgent (Free API Agent)

<p align="center">
  <strong>基于 Rust + Tauri 2 / Vue 3 的本地自主 Agent 平台</strong><br/>
  面向复杂、长时任务：规划与讨论、工具执行、多角色协作、证据审查、恢复与交付
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

<a id="current-capabilities"></a>
## 🚦 系统能力总览 (Current Capabilities)

| 架构域 | 当前实现 | 状态 |
|:---|:---|:---:|
| **任务分流与规划** | 入口 triage、直接执行/方案审查/专家讨论等路由；支持执行中带证据修订已批准方案 | ✅ 已实现 |
| **多 Agent 协作与角色卡** | 主 Agent 按需派发 worker / writer / reviewer；角色卡可定义模型、职责范围、入参/产出结构化契约 | ✅ 已实现 |
| **多层质量审查** | 方案反驳、阶段审查、最终 Reviewer；区分 `blocking_issues` 与非阻塞 `nit_issues` | ✅ 已实现 |
| **可追溯验证证据** | `evidence_record/find/inspect/compare`；宿主保存来源、执行输出关联与文件快照 | ✅ 已实现 |
| **冻结验证复用** | Reviewer 打回后，符合严格输入条件的宿主冻结命令回执可独立保留并供再审查；产物/契约改变触发重验 | ✅ 有边界地实现 |
| **完成与交付** | `prepare_finish`/`finish`、ReviewContract、checkpoint、终态事务提交；仍有其他验证路径待优化 | 🔧 持续打磨 |
| **长任务上下文与检索** | 三明治上下文、渐进压缩、SQLite FTS5、ripgrep、`output_id` 原文回查 | ✅ 已实现 |
| **并发与恢复** | child job 池、`wait_jobs`、中断/续跑、检查点与任务步骤持久化 | ✅ 已实现 |
| **执行安全与隔离** | 工作区权限围栏、写副作用审计、隔离写入 `child_writer.apply` 冲突校验 | ✅ 已实现 |
| **人机协同与接入** | `ask_human`、运行中干预、Tauri 桌面端与 HTTP/Mobile Web 通道 | ✅ 已实现 |
| **评测与观测** | eval runner、机械判据、运行轨迹、Token/步骤及审查记录 | ✅ 已实现 |
| **Sleep-Time Compute** | 长期经验蒸馏与策略淘汰设想 | 🚧 规划中 |

**状态说明：**“已实现”表示仓库内已有对应代码、测试或调用链，**不等于**已经证明所有模型、长任务与平台环境下都稳定可靠。具体实测与历史局限见 [`OVERNIGHT_LOOP.md`](./OVERNIGHT_LOOP.md)、[`docs/agent-reliability-records.md`](./docs/agent-reliability-records.md) 及评测记录。**近期关于审查、验证证据复用与多 Agent 收敛的讨论及新会话续接方式**见 [`docs/agent-review-evidence-convergence-notes-2026-10-17.md`](./docs/agent-review-evidence-convergence-notes-2026-10-17.md)。

### 当前执行模型（一眼看懂）

```text
任务请求
   ↓
入口分流 / 需求理解 ──→ 直接执行，或制定方案、邀请 Reviewer/多专家讨论
   ↓
主 Agent 调度工具和按需子 Agent ──→ 执行中新证据可促成方案修订
   ↓
真实工具输出、产物和验证记录 ──→ Checkpoint / Evidence Store
   ↓
阶段审查 / 最终 Reviewer ──→ 找到具体 Blocking → Agent 修复并补充受影响证据
   ↓                                  ↑
证据足够、硬性验收义务满足 ─────────────┘（不满足时返回）
   ↓
prepare_finish / finish → 宿主执行最终状态提交
```

**分工原则：**工具与宿主提供实际执行事实、权限边界与可靠回执；Agent 负责实现和调整计划；Reviewer 结合代码、产物和证据判断质量。**验证结果是审查证据，不等同于最终质量裁决。** 已通过的事实可复用，但不能因有一条旧 PASS 就自动放行新产物。

**实战边界：**已有实际使用者反馈，长会话曾连续执行数百步骤，主要摩擦集中在最后的交付门禁；这属于具体使用经验，不代表经过统计验证的复杂任务成功率。当前已缓解部分**冻结验证重复执行**，但自适应验证、额外配置验证、增量审查和真实模型持续收敛仍需更多实测。避免把“框架能够支持复杂任务”写成“复杂任务已稳定自动完成”。

---

## 📖 目录

- [当前能力和执行模型](#current-capabilities)
- [一、设计哲学](#一设计哲学与理论背景)
- [二、三明治上下文引擎](#二真三明治上下文引擎-sandwich-context-engine)
- [三、检索体系：脱水不失忆](#三检索体系脱水不失忆)
- [四、门禁、审查与证据复用](#四门禁审查与证据复用)
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

## 四、门禁、审查与证据复用

核心边界：**宿主守住可机械证明的事实，Agent / Reviewer 负责有上下文的语义判断。** 不以增加门禁次数代替质量，也不把模型自述当作宿主实测。

### 1. 规划与方案反驳（写前）

- 入口 `triage` 可以分流到 `direct`、`outline_review`、`technical_plan`、`expert_panel` 或 `user_clarification`，不是所有任务都强制多 Agent 讨论。
- `submit_plan` 与 `child_reviewer` 负责方案审查、反驳与修订；专家会审按任务和角色需要进行，并受预算及审批约束。自动、交叉审查和人工先批准模式由实际配置决定。
- **执行中可推翻原先假设**：发现新证据时，通过 `request_retriage` 或带 `revision_reason` 的方案修订重新评估受影响部分；之前的先验不是不可修改的事实。
- `agent_policy.rs`、路径范围和写入门保留确定性安全约束。方案审查给出的建议不天然等于事实。

### 2. 阶段审查与反馈（写中）

`stage_review/` 在任务执行中对实际修改进行审查；最终 `reviewer_gatekeeper/` 可取证检查代码、产物、命令记录及登记的历史证据。Reviewer 输出 **`blocking_issues`（交付阻塞）** 与 **`nit_issues`（非阻塞建议）**，并提供具体修复要求。Agent 负责修复，Reviewer 负责核对修复是否成立；打回不应自动抹去所有此前有效的执行事实。

### 3. 真实验证记录与适用性

`task_store` 暴露四个证据动作：`evidence_record`、`evidence_find`、`evidence_inspect`、`evidence_compare`。可记录验证目的、命令、原始 `output_id` 关联、时间和文件快照；Reviewer 可以检索、阅读并与当前文件作比较。

- **执行事实 ≠ 当前有效性 ≠ 质量结论**：宿主执行成功只证明当时命令退出成功；是否覆盖现在的交付，由 Reviewer 根据产物版本、执行环境、验证范围和后续修改判断。
- 证据里的 Agent `reported_outcome` / `command` 属于声明，原始命令结果应核对 `output_id`。`evidence_find` 命中的是候选记录，不会自动授予 PASS。
- 小文件可取得全文件哈希；较大文件采用首/中/尾有界采样（最多约 12 MiB），**抽样比较不等于完整性证明**，也不能替代业务验证。

### 4. `prepare_finish` / `finish`（收尾）

`prepare_finish` 和 `finish` 会调用同一套完成度评估：检查 TODO、验收契约、实际改动范围、相关后台任务及必要的验证，再进入最终 Reviewer。准备阶段不是纯粹的“查看状态”，可能执行检查；`finish` 通过后将完成状态、运行轨迹与交付消息作原子提交。

**最新改进：冻结验证与终审判决分别保留。** 如果上一轮 Reviewer 只要求补充说明或证据，在同运行、同工作区、同材料/范围/契约及副作用条件匹配时，宿主可以复用先前真实成功的冻结命令回执，将其交给 Reviewer 再判断，而不直接再次执行。旧回执失效、产物或契约变化、原命令失败时仍走相应验证；Reviewer 有具体疑点可要求重验，其实际失败结果不能被旧 PASS 覆盖。

```text
首次验证成功 → Reviewer 打回说明 → 仅修改说明 → Reviewer 重新判断（冻结命令可不重跑）
                                     ↘ 修改产物/验收契约 → 重新执行相关冻结验证
```

**仍有边界：**这项复用目前重点覆盖**冻结义务命令**，不是所有自适应验证、通道额外命令或全部审查项目的逐项缓存。冻结输入指纹没有变化，也不能机械证明外部服务、依赖和环境从未变化；最终适用性仍需审查判断。系统还保留 `consecutive_gate_blocked` 等异常收尾保护，具体触发以现行代码为准。

### 5. 写副作用门（执行面）

`dispatcher/`（`is_write_or_side_effect_tool`）同时按**工具名与 action** 判定写入副作用；MCP/插件命名空间也受相应权限校验。工作区路径围栏、不可逆操作保护和工作区软锁用于限制真实越权行为，而不是代替 Reviewer 判断实现是否正确。

---

## 五、多智能体并发架构

**主 Agent 是执行与分派中心，角色是按需调用的能力，不要求每项任务都维持固定常驻团队。** 入口分流可以建议需要的专家与审查方式，主 Agent 结合任务与证据安排协作。

**角色卡 (`tools/role_manager/`)** 支持为不同领域的 worker / writer / reviewer 配置模型、人设、允许的角色类型、关注领域、路径范围、技能，以及结构化**入参契约 (`input_schema`)** 和**产出契约 (`custom_schema`)**。这些约束有助于协作结果可消费、可检查，但不意味着角色的语义判断总是正确。

**执行中修订**：方案和分流结论是可修正先验。发现与原方案冲突的新证据时，Agent 可以说明理由发起分流复核或方案修订；不必为纠正局部假设抹去此前已完成的有效工作。

宏观任务级并发（比单步多工具并发更难的部分）：

- **`child_worker`**：侦察兵/执行员，独立沙箱上下文与 Token 预算，完成只回精炼结论——父会话上下文不爆炸；
- **`child_writer`**：隔离的写入执行员——在物理隔离副本中改码跑测试，产出结构化 diff 回执（`changed_files` + 三态指纹 + `patch_ref`）；主 Agent 审查后调 **`child_writer.apply`** 由宿主机械合入：优先副本字节搬运、patch artifact 兜底、逐文件指纹冲突则**整体拒绝**（宁可不合并也不盲覆盖），合入后须在真实工作区重跑验证；
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

### 3. 三权分立（模型观察、存档与 UI 视图）

同一事实分别向模型、界面与存档投影：

- **模型**看 `stdout`（高信噪比采样文本，省 Token）；
- **UI** 看 `view`（结构化字段 → 类型化卡片，零正则猜测）；
- **存档**走 `output_id` + step json（无损可回放）。

`view.kind` 支持类型化展示，原始回执与模型观察文本独立保存。**扩展新外观 = 工具返回 `view:{kind:...}` + 前端对应渲染分支**；未提供 `view` 的工具继续使用通用文本回退，不强制每个工具都接入 UI 卡片。

### 4. 新增内置工具四步

1. `prompts/tools/xxx.md` 写 Agent 可读的使用说明；
2. `agent/tools/xxx.rs` 实现 `dispatch()`；
3. `dispatcher/` 注册一行路由；
4.（可选）`commands/` 暴露配套 Tauri Command。

---

## 八、代码库目录索引

### 1. Agent 内核（`src-tauri/src/agent/`，按域分目录）

**运行主线（一次任务的生命周期链路）**

| 模块 | 职责 |
|:---|:---|
| `main_loop/` | 任务顶层状态机：`mod`（主循环体：步循环、门禁拦截、P3 死锁逃生）、`entry`（入口装配）、`helpers`（前置辅助）、`auto_verify`（写入验证自愈）、`precheck`（prepare_finish 终态预检）、`finalize`（终态提交事务）、`post_step`（步尾收尾：todo 证据回填/lesson 血缘/副作用台账） |
| `loop_runner/` | 单步原子流水线：`mod`（派发壳 + `DispatchCtx` 共享上下文）、`dispatch_*` 八条工具域派发臂（child_writer / child_reviewer / child_worker / resume_child / jobs / watch / ask_human / local） |
| `call_llm_phase.rs` | LLM 调用相：流式推理、Token 计量、中断信号接入 |
| `dispatcher/` | 工具分发中枢：`shell_safety`（命令安全谓词：段切分/重定向/危险命令）、`tool_events`（前端事件）、`project_dispatch`（项目资源派发）、`command_exec`（命令执行内核）、`mod`（写副作用门 + `run_agent_step` 编排） |
| `run_state.rs` / `run_trace.rs` | 权威运行状态机 → 前端事件 / 运行轨迹持久化（`agent_run_steps` 行式存储 + 事务化 append） |
| `conversation.rs` / `control.rs` | 会话生命周期 / 人机干预通道 |

**子 Agent 体系**

| 模块 | 职责 |
|:---|:---|
| `child_writer/` | 隔离写入执行员：`workspace`（副本基线/worktree）、`actions`（写动作拦截）、`fingerprint`（三态指纹 CleanReplay/AlreadyApplied/Conflict）、`apply`（机械合入真实工作区）、`cleanup`（孤儿副本回收） |
| `child_reviewer/` | 独立审查员：`types`（数据结构）、`parse`（裁决解析）、`lifecycle`（spawn/监控）、`poll`（结果轮询） |
| `child_worker.rs` | 侦察兵子 Agent：只读沙箱上下文、独立 Token 预算、回精炼结论 |
| `job_engine.rs` | 异步任务池：子任务派发、生命周期、wait_jobs 信箱语义 |
| `session_fork/` | 会话分叉（子会话继承上下文派生） |
| `triage.rs` | 任务分诊：复杂度评估、角色路由表、取证任务单 |
| `plan_tree/` + `plan_tree_dispatch.rs` | 计划树执行引擎（seq/par）+ 受控并行分发（单批≤2） |

**门禁与终态（质量护城河）**

| 模块 | 职责 |
|:---|:---|
| `plan_gate/` | 方案审批结论解析与前置写入门禁 |
| `stage_review/` | 阶段级写入审查（批准→收尾的中段缺口） |
| `reviewer_gatekeeper/` | 终审：`baseline`（任务起点材料）、`evidence`（当前产物及历史证据读取）、`finish_diff`（Reviewer 取证与判决、按需冻结命令补验）、`verdict_parse`（结构化裁决） |
| `finish_guard/` | finish 终态质询：`diff`（行级对账/漂移地图）、`state_updates`（todo 守卫）+ 按主题拆分的 6 个测试文件 + `testkit` 夹具 |
| `finalization.rs` + `finalization/evaluate.rs` | 完成度评估、终审与冻结验证执行回执分开复用；`FinalizationReceipt` 签发及原子提交 |
| `agent_policy.rs` / `gate_applicability.rs` / `gate_messages.rs` | Policy-as-Code 策略引擎 / 门禁适用性判定 / 驳回话术 |
| `impact_pack.rs` / `review_contract.rs` / `review_tools.rs` | 影响面证据包（漏问防线）/ 审查契约 / 审查工具 |

**上下文与检索**

| 模块 | 职责 |
|:---|:---|
| `context/` | 三明治引擎：`assembler`（组装）、`budget`（水位）、`compaction/`（压缩摘要）、`audit/`（不可逆台账 + `shell_readonly` 命令白名单）、`determinism`/`memory`/`render`/`sampling`/`no_progress`/`resource_scope`/`zero_hit`/`types/` |
| `retrieval/` | 联邦检索：`fts_engine`（SQLite FTS5）、`workspace_search`（ripgrep 实时代码检索）、`memory_signpost`（路标召回）、`types` |
| `checkpoint/` | 断点快照：`types`（数据结构）、`durable`（持久化）、`plan_ops`/`plan_batch`（计划操作）、`inquiry_ops`（问询操作）；含 `RecoveryDecision` 自愈与 `consecutive_gate_blocked` 持久化 |
| `task_store.rs` / `evidence_store.rs` / `task_summary.rs` / `artifact_manager.rs` | 结构化数据区 / 验证证据的登记、检索、详情与比较 / 交付物汇总与工件管理 |
| `lesson.rs` / `skill_context.rs` | 避坑经验血缘（召回记录 + 因果反馈打分）/ 技能装载 |

**通道与基础设施**

| 模块 | 职责 |
|:---|:---|
| `channel/` | 多通道接入：`http/`（mod 路由壳 + session_ops / run_spawn / run_control / data_queries / views / upload / router + `mobile_web/` 资产）、`bridge`/`config`/`runtime` |
| `mcp/` + `mcp_catalog.rs` | MCP 协议：stdio Actor + HTTP/SSE 客户端、注册表、会话引导、插件目录 |
| `model_filter/` | 模型族检测/normalizer/healer（provider 差异适配） |
| `utils/` | `json_pipeline`（指令解析修复）、`llm_client`（provider 归一化）、`log_sampler`、`subprocess` |
| `protocol/` / `types/` | 协议动作定义（finish/submit_plan/prepare_finish 判别）/ 核心类型（AgentInstruction/DispatchResult/TodoItem） |
| `watch_engine.rs` / `scheduler.rs` / `workspace_lock.rs` | 持续监听引擎 / 定时触发器 / 工作区软锁 |
| `evaluation*.rs` / `eval_oracle.rs` | 实弹评测：回放器、阶段断言、Token/步数统计、分析 |
| `tools/` | 原生工具箱：local_file/、local_shell、mcp_manager、model_manager、role_manager/（含结构化角色契约）、project_manager、schedule_manager、skill_manager、system_info、wechat_manager |
| `prompt_assembly.rs` / `tool_manual.rs` / `context_collector.rs` | 提示词组装 / 工具手册按需加载 / 上下文采集 |
| `host_verify.rs` / `change_verification.rs` / `path_guard.rs` / `target_block.rs` / `project_map.rs` / `telemetry.rs` / `uv_runtime.rs` / `web_search.rs` / `file_parser.rs` / `plan_submit.rs` / `router.rs` | 宿主验证 / 变更核验 / 路径牢笼 / 目标区块 / 项目地图 / 遥测 / uv 运行时 / 联网搜索 / 文件解析 / 方案提交 / 模型路由 |

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

- `src-tauri/src/db/`：`init`（schema 迁移 + 连接池）、`maintenance`（会话级联清理/VACUUM）、`model_specs`（模型规格 CRUD + uvx 迁移）；
- `src-tauri/src/commands/`：`chat/`（sessions/messages/send/misc/artifacts 五域命令）、models/mcp/mcp_registry/projects/skills/tasks/roles；
- `src-tauri/src/llm/`：`mod`（ModelConfig）、`api_keys`（多 Key 轮询/脱敏/可轮换判定）、`sse`（流式事件解析）、`stream`（单次请求）、`retry`（退避/限流/调用总预算）；
- `src-tauri/src/task_registry.rs` / `lib.rs` / `main.rs`；
- `src-tauri/prompts/`：core_logic、agent_system_prompt、skills/、tools/；
- `plugins/`：MCP 插件 YAML 目录；`scripts/eval_runner.mjs`：实弹评测入口。

---

## 九、快速上手与运行

### 环境依赖

- **Node.js 20+ / npm**，以及项目 Rust 依赖所需的 **Rust stable / Cargo**。
- 桌面端请先安装 [Tauri 2 的平台依赖](https://v2.tauri.app/start/prerequisites/)（macOS、Windows、Linux 要求不同）。
- 使用真实模型前，需在应用内配置可用的模型通道/密钥；MCP 插件、角色卡和评测环境按实际任务配置。

### 开发

```bash
git clone https://github.com/qqw52491-hue/free_api.git
cd free_api
npm install

# Rust 静态检查和库测试
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --lib --manifest-path src-tauri/Cargo.toml

# 前端类型检查 + 构建
npm run build          # vue-tsc --noEmit && vite build

# 桌面客户端
npm run tauri dev      # Linux Wayland 可按需使用 npm run dev:wayland
```

> 本地 2026-10-17 的一次 Rust 库回归：`1155 passed / 0 failed / 10 ignored`（另过滤 1 项剪贴板测试）。这只是**代码层测试结果，不是复杂任务端到端成功率**。实际执行结果以当前平台重新测试为准。

### 实弹评测

```bash
npm run eval:real       # scripts/eval_runner.mjs 驱动真实会话回放
npm run eval:real:dry   # 干跑校验配置
```

---

## 十、演进路线 (Roadmap)

### 已实现（详见 OVERNIGHT_LOOP.md 与相关实现）

- [x] 入口任务分流、方案审查/反驳、按需多角色协作及执行中带理由的方案修订。
- [x] 角色卡的结构化输入/输出契约、模型分配、范围与技能声明。
- [x] 子 Agent 并发体系（worker/writer/reviewer + job_engine + dispatches）、隔离 writer 的冲突安全合入。
- [x] 阶段审查、最终 Reviewer 的 Blocking/Nit 区分及可取证审查工具。
- [x] 证据存储与查找/详情/比较；`output_id` 原始记录关联；有界大文件采样与来源标识。
- [x] 冻结验证宿主回执跨终审打回复用（在严格输入条件匹配时），并由 Reviewer 核对适用性。
- [x] `prepare_finish` / `finish` 交付事务、权限边界与异常收尾保护。
- [x] 检索去重、行式步骤持久化、checkpoint 恢复、结构化 UI 视图与多通道接入。
- [x] `SESSION_SCOPED_TABLES` 驱动会话级联清理，并通过 schema 反射测试核对表归属。

### 进行中 / 需要实测的方向

- [ ] **交付摩擦**：用真实长任务定位重复验证、无效 Blocking 和反复 `finish` 的成本。
- [ ] **验证复用覆盖**：评估自适应验证与额外配置验证是否需要独立回执；不无差别缓存 PASS。
- [ ] **Reviewer 反馈质量**：观察真实缺陷命中、修复要求可执行性与打回后的闭合率。
- [ ] **多 Agent 收敛**：对比有无方案讨论/阶段审查时的实际质量、成本和完成率。
- [ ] **渲染与边界**：继续完善不可信内容隔离、脱敏、工具视图与跨平台恢复。
- [ ] **Sleep-Time Compute**：睡眠期经验蒸馏、Lesson 淘汰等长期研究方向。

---

## 📄 License

本项目采用 **MIT License** 开源协议。
