# 子 Agent 编排体系演进方案（现状盘点 + 目标设计 + 分阶段改造）

本文档是对《Agent 终态审查门禁 (Reviewer Gatekeeper) 与多角色分工架构设计》的姊妹篇。
前者回答"**终审怎么做**"，本文回答"**子 Agent 这台机器本身设计得怎么样、离真正的多 Agent 编队还差什么、怎么分阶段补齐**"。

> 结论先行：当前体系的**运行时（发动机）是重的、建制（编队）是轻的**。
> 隔离、持久化、预算、恢复这些机制层超过多数自研框架；但组织拓扑是平的、角色只是换皮、
> 上下文交接与结果回执两份契约过薄，使它目前更接近"三个专用异步工具"，而非多 Agent 编队。

---

## 目录
1. [现状全景：子 Agent 运行时到底有多重](#一现状全景子-agent-运行时到底有多重)
2. [九维对比：现状 vs 目标](#二九维对比现状-vs-目标)
3. [差距详析（含证据与业界参照）](#三差距详析含证据与业界参照)
4. [分阶段改造方案 P0~P5](#四分阶段改造方案-p0p5)
5. [明确不做的事（Non-Goals）](#五明确不做的事non-goals)
6. [里程碑依赖与验收总表](#六里程碑依赖与验收总表)

---

## 一. 现状全景：子 Agent 运行时到底有多重

先如实盘点已有的东西——很多设计是生产级的，**演进方案必须建立在这些资产之上，而不是推翻它们**。

### 1.1 三档递进的隔离体系（L1/L2/L3）

| 档位 | 承载者 | 隔离方式 | 关键机制 |
|---|---|---|---|
| L1 同步审查 | `child_reviewer`（同步调用） | 只读 + 证据引用上限（`MAX_CHILD_RESOURCE_IDS=2`） | 结论走结构化 `ChildReviewResult` |
| L2 只读侦察 | `child_worker`（异步 spawn） | 独立上下文 + 只读动作面 + 命令白名单 | durable inbox 回传 findings/confidence |
| L3 隔离写入 | `child_writer`（异步 spawn） | **git 副本工作区**，root 仍是真实工作区唯一写入者 | 路径包含性校验防符号链接逃逸（`child_writer.rs:161-187`）；复制预算 4000 文件/200MB；写次数/单文件字节/diff 总预算三重硬顶（`child_writer.rs:19-25`）；命令白名单 + shell 元字符拒绝（`child_writer.rs:245-274`）；测试前 finish 拦截（`child_writer.rs:593-603`） |

**"root 唯一写入者"是整套体系最重要的契约**：writer 在副本上改码跑测试，只回传 diff 回执，
由 root 审阅后用常规写工具落实（派发回执文案明确写了这一点，`loop_runner.rs:929`）。
这保证了真实工作区的每次变更都过 root 的上下文与终审门禁。

### 1.2 durable 生命周期与零 Token 挂起

- 子任务登记在 `agent_child_runs` 表：begin → running → 终态（必带 `result_json`）；
- 结果经 durable inbox 回流，主循环每步自动注入未消费结果（`main_loop.rs:1105-1130`）；
- `wait_jobs` 用 `tokio::watch` 版本号封死"初查与注册等待之间"的竞态窗口（`job_engine.rs:73-85, 276-280`），超时 clamp 1–180s、可被 interrupt 打断、**等待期间零 Token 零 CPU**；
- 崩溃/取消后孤儿任务有 `recover_orphaned` 收敛（`main_loop.rs:1096-1097`），不会留永久僵尸。

### 1.3 角色卡（Role Cards）现状

`agent_role_cards` 表（`db.rs:209-220`）定义：`name / label / model_id / persona_prompt / scope_hints / default_test_command / enabled`。

当前角色卡能改变的三件事：
1. **人设**：`persona_block()` 追加到子 Agent system prompt（`role_manager.rs:230-235`）；
2. **模型**：`model_id` 非空时钉死，覆盖路由档位；
3. **默认值**：`merge_writer_defaults` 把 `default_test_command`、`scope_hints` 合并进派发参数（`role_manager.rs:205-228`）——注意 `scope_hints` 在此转为 `focus_paths`，**只是注入 prompt 的建议性关注路径，不构成写围栏**。

派发点 4 处实时查库（`reviewer_gatekeeper.rs:409`、`loop_runner.rs:743/892/958`），即改即生效。

### 1.4 已核实的边界参数（写方案前先钉死事实）

| 参数 | 现值 | 位置 |
|---|---|---|
| 每 kind 每父 run 子任务数上限 | 2（按 kind 分别计数，三 kind 理论总并发 6；**无全局预算**；常量名 `MAX_CHILD_REVIEWS_PER_PARENT_RUN` 有误导性） | `child_reviewer.rs:15, 251-258` |
| writer 总超时 / 步数 / 单步 LLM 超时 | 300s / `MAX_WRITER_STEPS` / 60s | `child_writer.rs:13-21` |
| 单步指令模型 | **单工具单动作**（`ToolInstruction` 只有单个 `tool/action/params` 字段，`types.rs:236-241`），无批量并行派发 |
| 子 Agent 动作面 | writer = `list_dir/read_file/write_file/run_command/finish`；worker = 只读 + `finish`；**硬编码，角色卡不可裁剪** | `child_writer.rs:355-597`、`child_worker.rs:418` |
| 递归派发 | 不支持（子 Agent 动作面里没有 `child_*`，孙 Agent 无法产生） |
| 追问/续聊 | 不支持（终态落库即终结，root 只能重新派发一个新子 Agent） |

---

## 二. 九维对比：现状 vs 目标

| # | 维度 | 现状 | 目标（本方案） | 差距评级 |
|---|---|---|---|---|
| 1 | 组织拓扑 | 星型静态：root 派 3 种固定 child，child 之间无通信、无递归 | 保持星型（刻意设计），但 root 可编队、可并行、可追问 | 中 |
| 2 | 并发规模 | 每 kind 2 个，无全局预算，一步一令 | 全局预算 + 每 kind 弹性配额 + 单步批量派发 | **高** |
| 3 | 角色定义 | 换皮三件事：persona/模型/默认命令 | 角色卡 v2：+ `allowed_tools`（能力面裁剪）+ `scope`（强制围栏）+ `max_concurrency` + 预算覆盖 | **高** |
| 4 | 上下文交接 | 一段 `task` 字符串（4 行 format!，`child_writer.rs:519-524`） | 自包含纪律条款 + 结构化 `context_excerpts`（todo 切片/记忆/文件摘录） | **高** |
| 5 | 结果回执 | 裸 LCS diff 无行号无 hunk；48k 预算耗尽即占位符，root 无法重建 | 带行号 unified hunk 或 `git apply` 补丁；预算分级降级而非丢弃 | **高** |
| 6 | 生命周期 | 一次性：终态落库即终结 | 支持 resume 追问（复用终态消息历史续开一轮） | 中 |
| 7 | 隔离与安全 | L3 副本 + 三重预算 + 白名单（**强项，业界前列**） | 保持；补副本回收与命令超时两个洞 | 低（修洞即可） |
| 8 | 可观测性 | child_runs 表 + telemetry + 前端事件，但副本成本被丢弃、事件名散落 | 补齐副本规模/交接体积/回执完整度观测 | 低 |
| 9 | 文档 | 文档只写了角色卡与菜单，**运行时一章缺失** | 补"子 Agent 运行时架构"章 | 低（纯写作） |

---

## 三. 差距详析（含证据与业界参照）

### 3.1 交接契约过薄 —— 最大单点

现状：子 Agent 收到的全部世界就是这段文本：

```rust
// child_writer.rs:519-524
json!({"role": "user", "content": format!(
    "副本工作区根目录：{}\n关注路径：{}\n必须执行的测试命令：{}\n\n修改任务：{}",
    copy_root.display(), focus, request.test_command, request.task
)})
```

问题：
- root 写一句"修复那个 bug"派出去，子 Agent 全盲开跑——**没有任何机制拦截模糊派发**；
- 主 Agent 上下文里的 todo 切片、既有结论、相关文件路径，全靠 root 自觉抄进 task 字符串，抄多少漏多少无从校验；
- 对比成熟编排系统的纪律（"子 Agent prompt 必须自包含：背景、目标、验收标准、相关材料"），本系统的级联菜单里没有等价条款。

### 3.2 回执契约过薄 —— 第二单点

- `line_diff`（`child_writer.rs:201-243`）是无行号、无上下文的裸 LCS diff；
- 总预算 48,000 字符耗尽后 diff 直接替换为"（总 diff 预算耗尽，未内联）"占位符（`child_writer.rs:452-458`），此时 root **仅凭文件名无法重建修改内容**；
- writer 异步运行最长 300s，期间 root 可继续写真实工作区；diff 基线是 spawn 时刻快照，**重放即静默覆盖，无冲突检测**；
- 副本目录 `.writer-{child8}` 建在主工作区内，全仓库无清理逻辑（每次最多泄漏 200MB，且污染 `git status`）。

### 3.3 角色是"换皮"不是"编队"

对照成熟多 Agent 系统的角色语义，一个角色应定义**能力面**：
- 能用哪些工具（本系统：三档动作面硬编码，角色卡不可裁剪）；
- 允许碰哪些路径（`scope_hints` 只进 prompt 文案，`ensure_contained` 只校验副本根包含性，不按 scope 拦截）；
- 并发额度（无，per-kind 常量钉死）；
- 上下文/Token 预算（有硬顶但角色不可覆盖）。

现状下 `frontend` 角色卡与 `backend` 角色卡的唯一区别是提示词和模型——**换 persona 不是真分工，裁剪能力面才是**。

### 3.4 编队规模与并行度

- 每 kind 2 个：对一个"重"的多 Agent 系统，同 kind 编队上限 2 偏小（例如"5 个模块并行侦察"直接不可表达）；
- 单步单指令：LLM 一步只能发一个工具调用，无法在一个决策里 fan-out（想并行 3 个侦察要耗 3 步、3 次 LLM 往返）；
- 无全局预算：三 kind 各自 2，加剧本 watch/job 后总并发无人看门。

### 3.5 无追问（resume）

子 Agent 终态落库后就是"死"的。root 看完回执想追问"第二个缺陷的复现路径是什么"，只能新派一个子 Agent、重新付一遍理解成本。成熟系统普遍支持对已跑完的 agent 续发消息。

### 3.6 两个运行时的洞（修 bug 层面，非设计层面）

1. **命令无超时**：`child_writer.rs:21` 定义 `COMMAND_TIMEOUT_SECS=60`、`child_worker.rs:24` 定义 20s，但两处 `run_command` 均用 `std::process::Command::output()`（`child_writer.rs:422-426`、`child_worker.rs:211-215`）——**常量死代码**。挂死的 `npm test`（watch 模式）会让 writer 永远停在观测阶段、终态不落盘、占死 tokio 工作线程。
2. **副本永不回收**：见 3.2。

---

## 四. 分阶段改造方案 P0~P5

> 原则：**不推翻现有机制，只做收敛与加厚**。每个阶段独立可交付、可回滚（全部新字段 `serde default` + SQL `ALTER TABLE ... ADD COLUMN`，旧数据零迁移成本）。

### P0 补文档（零代码，半天）

给《Reviewer Gatekeeper 与多角色分工》补一章"子 Agent 运行时架构"：
- L1/L2/L3 隔离档位表、root 唯一写入者契约、durable inbox 与 wait_jobs 语义、预算体系、孤儿恢复；
- 明确写出"星型拓扑（所有结果只回流 root）是刻意设计"，防止后人误改成 mesh。

**验收**：文档与代码逐条对得上（文件:行号可查）。

### P1 运行时修洞（1 天，含测试）

1. `child_writer/child_worker` 的 `run_command` 改 `tokio::process::Command` + `tokio::time::timeout(COMMAND_TIMEOUT_SECS)`，超时返回观测文本"命令超时（>Ns），已被强制终止"，writer 循环继续可收敛；
2. writer 终态（无论成败）后清理 `.writer-*` 副本目录（`std::fs::remove_dir_all` 包 `let _ =`，失败仅记日志）；
3. **崩溃僵尸清扫**：终态清理覆盖不了进程崩溃——应用启动时扫描工作区 `.writer-*` 目录，与 `agent_child_runs` 表交叉核对（无对应 running 记录、或 mtime 超过 1 小时的）直接删除，防止崩溃累积僵尸副本；
4. 顺手修 `evaluate_finish_diff` 的阻塞 git 调用（同一类病）。

**验收**：新增单测——挂死命令（`sleep 999` 白名单外模拟）在 N 秒内返回超时观测且 run 正常 finish；副本目录在终态后不存在；清扫逻辑对"表内 running 但目录超龄"与"表内无记录"两种僵尸均删除、对活跃 run 的副本不动。

### P2 交接契约升级（2~3 天）

**2a. 级联菜单加纪律条款**（`role_manager.rs:178-201` 的菜单构建尾部追加）：

```markdown
【派发纪律】派发给子 Agent 的 task 必须自包含：
1. 背景：为什么做这件事（一句话）；
2. 目标：要产出什么（可验收的描述）；
3. 相关材料：涉及的文件路径/既有结论/约束。
模糊派发（如"修复那个 bug"）会被 host 打回。
```

配套**派发质量检查（软硬结合，避免误伤）**：外部评审指出硬性长度阈值（如"<24 字符打回"）会误伤合法的短任务（"修复 index.ts 的拼写错误"恰好 20 来个字符），把 LLM 推进自我怀疑的重试循环。因此降一档：
- **硬拦截只保留一条**：`task` 为空或纯空白 → `host_rejection:task_empty`（零争议）；
- **软性警示不做拦截**：task 疑似过薄（无任何路径/文件名/命令痕迹且无换行结构）时，照常派发，但在回执 stdout 前附加一行提示"⚠️ 本次派发任务描述较简略，子 Agent 可能缺乏必要背景，建议派发时携带 context_excerpts"——让 root 在下一步自我修正，而不是被打回后瞎猜怎么改写；

**2b. 结构化交接字段**：派发参数新增可选 `context_excerpts`：

```rust
#[derive(Deserialize, Default)]
pub struct DispatchContext {
    /// root 从自己的上下文里摘的相关材料，逐条注入 user prompt
    pub background: Option<String>,        // 一段背景
    pub todo_slice: Vec<String>,           // 相关 todo 条目
    pub file_hints: Vec<String>,           // 相关文件路径（仅路径+一句说明）
    pub prior_findings: Vec<String>,       // 既有结论（如 reviewer/worker 回执摘要）
}
```

`run_writer_loop` 的首条 user 消息从 4 行 format! 升级为分区渲染（背景/任务/关注路径/测试命令/既有结论），每区缺失则略过。

**兼容性**：参数不传 → 行为与现在完全一致。
**验收**：单测覆盖"空 task 被硬拦截、疑似过薄 task 软警示不拦截""context_excerpts 正确渲染进 prompt""不传时输出与旧格式等价"。

### P3 角色卡 v2：从换皮到编队（3~4 天）

`agent_role_cards` 加列（`db.rs` 迁移）+ `RoleCard` 结构体扩展：

```sql
ALTER TABLE agent_role_cards ADD COLUMN allowed_kinds TEXT NOT NULL DEFAULT '';      -- 逗号分隔: writer,worker,reviewer；空=不限
ALTER TABLE agent_role_cards ADD COLUMN hard_scope   TEXT NOT NULL DEFAULT '';      -- 逗号分隔路径前缀；空=不限制
ALTER TABLE agent_role_cards ADD COLUMN max_concurrency INTEGER NOT NULL DEFAULT 0; -- 0=跟随全局默认
```

语义设计（**默认值全部等于现状**，零破坏）：
- `allowed_kinds`：该角色可被哪几类 child 挂载（`frontend` 角色卡配 `writer`，`scout` 配 `worker`）；
- `hard_scope`：writer 的 `ensure_contained` 增加第二层校验——目标相对路径必须命中 `hard_scope` 前缀之一，未命中返回 `host_rejection:scope_violation`；worker 的读取同步受此围栏。**`scope_hints` 保持原语义（建议），`hard_scope` 是新语义（强制）**，避免破坏现有卡；
- `max_concurrency`：角色级并发额度，与全局 per-kind 上限取 min。

前端 `RoleManagerView.vue` 表单加三个字段（多选 kind、路径标签输入、数字步进器）。

**验收**：单测——scope 越界写入被拒、卡内路径放行；角色并发额度生效；旧卡（三字段为空）行为不变。

### P4 编队规模与批量派发（3~4 天）

1. **常量正名**：`MAX_CHILD_REVIEWS_PER_PARENT_RUN` → `MAX_CHILDREN_PER_KIND_PER_RUN`（保持 2），新增 `MAX_TOTAL_CHILDREN_PER_RUN = 6` 全局看门（begin 时同时校验两道，`child_reviewer.rs:251-258` 处）；
2. **弹性配额**：per-kind 上限可被角色卡 `max_concurrency` 覆盖（P3 已建字段）；
3. **单步批量派发**：`ToolInstruction` 增加可选 `dispatches: Vec<Value>` 数组字段——LLM 一步可同时派多个子任务（限同 kind、总数 ≤ 剩余额度），host 逐个 spawn 后合成一条回执（child_run_id 列表）：

```rust
// types.rs ToolInstruction 增量（serde default 保持单派发兼容）
#[serde(default)]
pub dispatches: Option<Vec<serde_json::Value>>,
```

系统提示词同步说明"需要并行时用 dispatches 数组一次派发多个同 kind 子任务"。

**协议兼容与回执组装（外部评审正确指出的两个坑，设计如下）**：

1. **单发/批发的平滑回退**——中小模型极易在两种格式间摇摆。策略：
   - host 侧**同时接受**两种形态：`tool + action + params`（旧）与 `tool + dispatches[]`（新），任何一步只认其中一种，两者同时出现时以 `dispatches` 为准并在观测里提示"检测到混合格式，已忽略单发字段"；
   - 模型输出批发但只带 1 个元素 → 等价单发，正常处理，不报格式错；
   - 连续 2 次批发格式解析失败 → 后续 3 步在 observation 中注入"批发格式解析失败，本轮请使用单工具派发"的降级指令，之后再恢复接受批发——**降级是暂时的，不是永久的**，防止模型学废。
2. **部分成功的回执组装**——派发 3 个、第 1 成功/第 2 超限/第 3 格式错时，逐条结果不吞并、不打断：
   - 回执 stdout 按序号逐条列出：`[1/3] ✅ 已派发 child_run_id=...`、`[2/3] ❌ 超出并发上限（当前 kind 已满 2），该条未派发，可在回收后重试`、`[3/3] ❌ 参数缺少 task 字段，该条未派发`；
   - `success` 字段取"至少一条成功"，`route` 记 `dispatches:1ok2fail`；被拒条目**不消耗并发名额**；
   - 前端状态卡片：现有 `agent_child_runs` 按行渲染，天然支持多卡片，无需新事件协议——只需 dispatch 事件里带 `batch_index/batch_total` 两个可选字段供排序。

**验收**：单测批量派发 3 个 worker 全部登记、超额第 4 个被全局看门拒绝且回执明确；单派发路径回归不变；混合格式与解析失败降级路径各有单测。

### P5 回执与追问（5~7 天，可按需拆开）

> 外部评审正确指出本阶段有两个硬骨头：**resume 与副本回收的生命周期冲突**、**指纹冲突检测的 Git 底层细节**。以下设计直接给出解法。

#### 5a. 回执升级与冲突检测

**⚠️ 前置修正（评审发现的方案错误）**：初版方案假设"副本本就是 git 工作区"——**这是错的**。`copy_recursive` 复制时明确跳过 `.git`（`child_writer.rs:139`），**任何项目的副本都没有 git 仓库**，直接跑 `git diff` 必然失败。修正为"**spawn 时自建快照基线**"：

- writer 副本创建完成后、进入动作循环前，在副本内执行一次：
  ```
  git init -q && git add -A && git -c user.email=agent@local -c user.name=writer commit -qm "spawn baseline"
  ```
  `-c` 内联身份避免宿主机未配置 git identity 导致 commit 失败；
- 这一个基线提交同时解决三件事：① 回执 diff 有可比对象（`git add -A && git diff --cached --unified=3`，即相对基线的全部变更）；② spawn 时刻文件指纹免费获得（基线提交的 blob，`git ls-tree -r HEAD` 一次取全）；③ `git apply --3way` 所需的 base blob 就在基线提交里，三态冲突的真合并路径可用；
- **降级路径**：宿主无 git 二进制时回退到现有 `line_diff` 裸 diff（保留不删），回执标注"⚠️ git 不可用，回执为无行号简化 diff"。

**Unified hunk 生成**：如上，diff 全部由 git 生成（基线提交 → `git diff --cached --unified=3`），行号、hunk 头、上下文行不再手写，LCS 实现仅在 git 不可用时兜底。

**预算分级降级（不丢弃，只降级）**：
1. 全量 hunk 在 48k 预算内 → 直接内联回执；
2. 超预算 → 先砍上下文行（`--unified=0` 重算），再砍小文件的 hunk（大文件优先保留）；
3. 仍超 → 完整补丁落盘为补丁文件并登记 artifact（`artifact_manager` 现成机制），回执内联每个文件的首个 hunk 头 + `patch_ref`，root 用 `get_step_output` 按需取全文——**任何情况下 root 都有一条重建路径**。

**指纹冲突检测（三态判定，不是简单告警）**：spawn 时记录每个目标文件的内容指纹，finish 时在真实工作区重算，按三态处理：

| 判定 | 含义 | root 侧动作 |
|---|---|---|
| `root_now == spawn_hash` | 主工作区未被第三方改动 | 回执标 ✅ 可直接重放 |
| `root_now == result_hash` | root 已自行改成了相同内容 | 回执标 ✅ 无需操作 |
| 三者互不相等 | **真冲突**（writer 跑的 1~3 分钟里 root 动了同一文件） | 回执标 ⚠️ 禁止直接重放；回执附该文件的 hunk 补丁，root 审阅后手工合并或走 `git apply --3way`（spawn 基线可用 `git hash-object` 重构 base blob） |

实现要点：指纹计算用 `git hash-object`（与 blob 语义一致，免手写 SHA 逻辑）；每文件 spawn/finish/root 三点采样；判断为纯函数抽出，可单测。**把静默覆盖变成显式的三态决策**。

#### 5b. resume 追问与副本生命周期状态机

**核心矛盾**：P1 让副本"终态即删"，P5b 要求 resume 时"副本还在"。解法是把副本生命周期从"跟随子任务"改为"**跟随父 run**"：

| 副本状态 | 进入条件 | 副本处理 | 可 resume？ |
|---|---|---|---|
| 活跃 | spawn | 存在 | ✅ |
| 保活 | child 终态 | 保留（不删），标记 `resumable_until = 父 run 终态` | ✅ |
| 回收 | **父 run 到达终态**（Done/Failed/取消） | 统一 `remove_dir_all` 所有该父 run 的 `.writer-*`；挂载点为父 run 终态落库收口处（`main_loop.rs` 中状态写为 succeeded/failed/cancelled 之后），封装为 `cleanup_child_workspaces(&db, &parent_run_id)` 一个函数 | ❌ |
| 僵尸清扫 | 应用重启（P1 已建） | 交叉核对 `agent_child_runs` + mtime 超龄即删 | ❌ |

这样：磁盘上同时存在的副本数 ≤ 父 run 内 writer 数（≤ per-kind 上限），泄漏上限从"每 writer 200MB 无限累积"变成"单父 run 内有界"；TTL 计时器、后台扫描器都不需要引入——回收锚点是父 run 终态这一个已有事件，P1 的启动清扫兜住崩溃场景。

**上下文膨胀控制**：终态落盘的不是原始消息历史，而是**复用主循环 checkpoint 已有的脱水机制**（`checkpoint.rs` 的 observation 脱水，单条上限 4000 字符）——stdout 日志、大文件读取结果在落盘前先脱水，`message_history_json` 保存的是脱水后的历史。resume 重启时直接以脱水历史为消息数组起步，无需二次压缩；若历史整体仍超预算，沿用主循环 80%/35% 水位压缩同一套逻辑。

**resume 语义**：
- 新增 `resume_child` 工具：`child_run_id` + 一条 user 追问消息 → 以脱水历史起步重启循环，步数上限减半、写预算重计，产出**新 run 记录**（`parent_run_id` 指向当前 root run，新旧 run 用 `resumed_from` 列关联）——追问链本身也是可审计的；
- 仅 `succeeded` 状态、且副本仍处于"保活"状态的 run 可追问（已回收的返回明确错误"该子任务的工作副本已随父运行结束回收，请重新派发"）；
- writer 类 resume 强制携带新测试命令，测试证据门禁原样生效。

**验收**：hunk 格式与降级顺序单测；三态指纹判定纯函数单测（含 `--3way` 路径）；副本状态机单测（父 run 终态触发回收、resume 被拒于已回收副本）；脱水历史 resume 后消息数组正确性单测。

---

## 五. 明确不做的事（Non-Goals）

1. **不做子 Agent 递归**（孙 Agent）：星型拓扑是刻意设计——所有变更与结论必须经 root 汇聚，才能保证"root 唯一写入者 + 终审门禁"两条不变量。开放递归会把上下文契约问题指数化。
2. **不做 child 间直接通信**：理由同上，mesh 拓皮的收益抵不过契约失控的风险。
3. **不做动态生成角色**：角色卡必须人来建（数据库实体），不让 LLM 运行时造角色—— persona 注入链路要保持可审计。
4. **不引入外部多 Agent 框架**：现有 checkpoint/inbox/预算底座是资产，替换成本远大于收益。

---

## 六. 里程碑依赖与验收总表

```
P0 文档 ──┐
P1 修洞 ──┼─→ P2 交接契约 ─→ P3 角色卡v2 ─→ P4 编队规模 ─→ (P5a 回执 ─+─ P5b resume)
          │                                        ↑                    ↑
          └────────────────────────────────────────┘   P3 的 max_concurrency  P5a 的
                                                      被 P4 消费           patch_ref 复用
                                                                           artifact_manager
```

| 阶段 | 工作量 | 交付物 | 核心验收 |
|---|---|---|---|
| P0 | 0.5 天 | 运行时架构文档章 | 文档-代码逐条可查证 |
| P1 | 1~1.5 天 | 命令超时 + 终态回收 + 启动僵尸清扫 + gatekeeper 异步化 | 挂死命令可收敛；副本零残留（含崩溃场景） |
| P2 | 2~3 天 | 派发纪律 + 空派发硬拦截/薄派发软警示 + `context_excerpts` | 空派发被拦、软警示不误伤；结构化交接渲染正确 |
| P3 | 3~4 天 | 角色卡 v2 三字段 + 强制围栏 + 前端表单 | scope 越界被拒；旧卡零影响 |
| P4 | 3~4 天 | 常量正名 + 全局预算 + 批量派发（含单发/批发回退与逐条回执） | 一步 fan-out 多子任务；混合格式/解析失败降级有单测 |
| P5 | 5~7 天 | hunk 回执 + 三态指纹判定 + 副本生命周期状态机 + 脱水 resume | 回执任何情况下可重建；追问复用脱水历史；副本回收锚定父 run 终态 |

全部完成后，九维对比表中 #2/#3/#4/#5 四个"高差距"项降为"已达标"，#1/#6 中差距项补齐；
#7/#8 随 P1/P5 顺手修洞——届时"三个专用异步工具"将真正具备**多 Agent 编队**的完整语义：
root 作为唯一仲裁与写入者，按角色能力面编队、并行派发、结构化交接、可追问、可重建地回执。
