# Agent 可靠性实测记录（持续更新）

> 用途：每轮「修改 → 真实评测 → 总结记录 → 与历史对比」循环的基线账本。
> 新的评测跑完后，在本文件追加一节，并与下方历史记录对比指标变化。
> 评测入口：`FREE_API_LIVE_MODEL_ID=<模型UUID> FREE_API_LIVE_WRITE_FIXTURE=1 npx wdio run wdio.conf.mjs --spec test/e2e/live-agent-model-regression.e2e.mjs`
> 报告落盘：`agent_evaluations/<evaluation_id>.json`（聚合指标）+ `agent_evaluations/monitor/`（逐步快照）。

## 记录 1：2026-08-29 · glm-5.2 @ Shantung · isolated_write_fixture

- **evaluation_id**: `agent-write-fixture-fa5e5292-9b8e-4898-82c6-07ae7b37a145`（run `fa3e944f`）
- **模型**: glm-5.2（UUID `1304de62-61b7-41f5-8c2b-29797b960a4f`，平台 Shantung / token.sensenova.cn）
- **场景**: 隔离写入 fixture（修复 `summarize-readings.mjs`，hidden assertion 验收，受保护文件不可变性校验）
- **结果**: **通过**（E2E ✓，trace `succeeded`，verdict `passes_observed_guardrails`，0 warnings）

| 指标 | 本次 (glm-5.2) | 历史对照 (sensenova-6.8-flash-lite, 2026-08-26) |
|---|---:|---:|
| LLM 调用 | 9 | 8 |
| 工具步骤 | 10 | 9 |
| 工具失败率 | 0% | 0% |
| JSON 解析成功率 | 100% | 100% |
| 重复读取 / 重复失败 | 0 / 0 | 0 / 0 |
| 缓存命中率 | 81.7% | 29.5% |
| 峰值上下文占用 | 16.2% | 13.3% |
| 观测时长 | 247s | 104s |

**行为亮点**（来自逐步轨迹）：
- 自行设计 11 项边界检查脚本（clamp、空 id、非有限值、输入不可变性、top3 排序 tiebreaker 等），超出可见测试要求——复杂交付门控（ReviewContract）在真实运行中生效的直接证据。
- 交付报告完整披露：修改文件、逐条行为对应、验证命令及结果、已覆盖契约、未验证边界。

**结论**：glm-5.2 @ Shantung 在本场景下可靠；缓存效率显著优于历史 sensenova 基线，代价是更长的思考时间（TTFT ~3.5s，末步思考 20s）。

## 记录 2：2026-08-29 · glm-5.2 @ Shantung · 全能力升级后回归（写 fixture + 诊断）

- **触发**：本轮落地 4 项可靠性修复 + watch 持续监听 + L2 侦察 worker + 附件句柄后，重跑验证无回归。
- **evaluation_id**：
  - 写 fixture：`agent-write-fixture-d3c3b11d-e984-4a31-a07b-8f27f59e09f0`（trace `0c256a2b`）
  - 诊断 fake-action：`agent-diag-fake-action-33b6ffd9-25f0-4ad7-9819-cda78c950ef2`（trace `7d289e2f`）

| 指标 | 写 fixture（本次） | 写 fixture（记录1基线） | 诊断 fake-action（本次） |
|---|---:|---:|---:|
| LLM 调用 | 11 | 9 | 7 |
| JSON 解析错误 | 0 | 0 | 0 |
| 工具步骤 | 12 | 10 | 8 |
| 工具失败 | 1（安全拦截） | 0 | 1（MCP 网络抖动） |
| 重复读取 / 重复失败 | 0 / 0 | 0 / 0 | 0 / 0 |
| 缓存命中率 | 82.4% | 81.7% | 88.1% |
| 观测时长 | 231s | 247s | 189s |
| verdict | review_recommended | passes_observed_guardrails | needs_attention |

**失败步骤归因（重要，均非 agent 逻辑回归）**：
1. 写 fixture 第 0 步：模型尝试 `switch_workspace` 被**宿主安全策略正确拦截**，模型随即改用只读工具在边界内完成任务——写入边界实测有效。
2. 诊断第 3 步：MCP 端点 `mcp.wxhjy.dpdns.org` HTTP 请求失败（网络抖动），非代码问题。

### 诊断 4 场景结果（E2E：1 过 1 挂，挂因集中且精确）

| 场景 | 调用/步骤 | 失败 | 重复 | 缓存 | 时长 | verdict |
|---|---|---:|---|---:|---:|---|
| diag-fake-action | 7 / 8 | 1（MCP 网络抖动） | 0/0 | 88.1% | 189s | needs_attention |
| diag-plugin-removal | 9 / 10 | 0 | 0/0 | 89.9% | 148s | needs_attention |
| diag-false-leak | 8 / 9 | 1 | 0/0 | 88.4% | 120s | review_recommended |
| diag-name-collision | 6 / 7 | 0 | 0/0 | 88.3% | 91s | needs_attention |

全部 trace `succeeded`、0 JSON 解析错误、0 重复读取/重复失败。**唯一系统性失分点（E2E 隐藏断言）**：「最终交付必须明确披露已核验范围、未核验范围或关键假设、以及结论强度」——glm-5.2 在诊断类任务上交付纪律不达标（结论本身未被判定错误，是披露结构缺失）。

**下一轮优化动作（未实施，待验证）**：在 `prompts/skills/code_diagnosis.md` 与 core_logic.md 复杂交付要求中，显式加入「诊断交付四件套：已核验范围 / 未核验范围或关键假设 / 结论强度（confirmed|supported|unverified）/ 就地证据引用」，改后重跑 diag-name-collision（最快 91s）验证是否转绿。

## 本轮新发现并修复的迁移缺口（2026-08-29）

- 线上库 `agent_child_runs` 缺 `result_json` 列（首版建表后新增、无滚动迁移）→ 老库升级后子任务终态结果**静默不可消费**。已补 `ALTER TABLE`，并新增 `kind` 列（review/worker 分类记账）。测试用全新建库无法暴露此类问题，**老库迁移是后续每个新列的必检项**。

1. `cancel_running_child_runs` 取消后未广播 `notify_job_state_changed` → 挂起在 `wait_for_jobs` 的协程只能等 180s 超时兜底。已修，并加测试证明取消即时唤醒等待者。
2. `run_child_reviewer`（同步路径）timeout/failed/cancelled 终态不写 `result_json` → 结果永远无法被信箱消费，模型对子审查死亡无感知。已补齐 `child_error_result_json`。
3. `poll_completed_child_runs_filtered` 遇到损坏 result_json 行会永久重试。已改为清除并记日志。
4. 进程崩溃后 `agent_child_runs` 孤儿 running 行永不收敛（job 表有恢复、child 表没有）。新增 `recover_orphaned_child_runs_on_session_start`，挂接到主循环会话启动处。
5. 同步路径 child_reviewer 系统提示词 JSON 示例双重转义（复制粘贴残留）。已修。

## 新增能力：持续监听（watch engine）

- `watch_start(kind: file|command, target, interval_secs 5–600)` / `watch_stop(watch_id?)`，每运行最多 3 个并发监听，单监听 100 事件上限自动收敛。
- 实现锚点：`agent_background_jobs` 表 `job_type='watch'` 的 running 行作 durable 状态锚点；变化事件以 `job_type='watch_event'` 进入既有 durable inbox——`wait_jobs`、主循环自动注入、运行结束级联取消全部复用现有机制，零新增消费路径。
- 文件监听含工作区 containment 校验；command 监听单次执行同时产出哈希与输出摘要（30s 超时、200KB 截断）。
- finish 门控已扩展：存在 running 监听时拦截 finish，提示 `watch_stop` 释放。
- 覆盖原始待办「文件 watch + 结果事件流」的持续监听部分（一次性等待由 `wait_jobs`/job_engine 覆盖）。

## 下一轮对比建议

1. 用 glm-5.2 重跑同一 fixture，确认 watch/child_reviewer 修复无回归（指标应保持 0 失败 0 重复）。
2. 跑 `agent-diag-false-leak`（只读诊断）场景对比 glm-5.2 与 sensenova 的根因定位质量。
3. 待 roadmap §6.2 的「根 + 2 只读 worker 跨模块诊断」评测实现后，补多 agent 协作的首组对比数据（重点看：token 总账、墙钟时间、根因完整度）。

## 记录 3：2026-08-29 · 交付披露修复验证 + L3 落地 + 深度检查结论

### 修复动作（诊断交付披露失分）

1. `code_diagnosis` 技能：交付四件套改为无条件模板（已核验范围/未核验假设/结论强度/主根因就地校准），并要求深度排查默认建立 review_contract。
2. `core_logic.md`：诊断/根因排查类任务默认视为复杂任务，必须建契约。
3. finish 门控泛化新原则：「谁宣称主根因，谁就校准」——未建契约的交付只要 summary 宣称主根因，也必须就地给出直接证据 + 结论强度（复用 assessor 的窗口检测，非新增关键词启发式）。

### 验证结果（diag spec，glm-5.2 @ Shantung）

- 第一轮：1/4 场景过（3 个挂在交付披露合规断言）。
- 第二轮：**3/4 过，且合规断言全部通过**；唯一失败是 0 步基础设施抖动（trace 未产生任何步骤，与 agent 逻辑无关）。
- 第三轮（提示词+门控修复后稳定复跑）：**4/4 全部通过**（2026-08-29，run 时长 9m53s）。结论：交付披露失分已修复且稳定收敛。

### 上下文 / 推理 / 约束深检结论（泛化视角）

- **上下文**：压缩引擎成熟（真实校准水位→先一次性脱水→再归档，append-only 保护前缀缓存）；评测缓存命中 82-90%、峰值上下文 ≤16%，无结构性缺口。本轮补上 worker 回执 findings 单条 600 字符上限，堵住子任务异常输出污染主上下文的口子。
- **推理**：路由分层（Pro 规划/救场、Flash 常规）+ 分层 reasoning effort 运行稳定（六场 0 解析错误）；推理能力的天花板已从单上下文转移到 L2/L3 的独立上下文。
- **约束**：finish 门控现覆盖 未闭合 Todo / 审查契约 / 写入验证 / 复杂交付披露（契约版+无契约主根因版）/ 运行中后台任务与监听；写入边界（switch_workspace 拦截、worker 副本 containment）实测有效。

### 已知边界（记录，不在本轮处理）

1. L3 回执是 diff 建议稿，落实修改仍需 root 手动应用——未来可做宿主端「审阅后一键应用 + 自动重验证」。
2. writer 副本建在工作区内（`.writer-<id>`），评测文件计数会包含它（不影响受保护文件断言）。
3. 门控挑战循环理论上可能消耗步数至 MAX_STEPS——目前唯一一次 trace failed 为基础设施抖动，需持续观察。

## 记录 4：2026-08-29 · 档位对照实验（核心问题：强模型的钱能不能省？）

**实验设置**：同一批 4 个复杂诊断场景（隔离 fixture + 隐藏断言 + 交付披露合规检查），
三种模型配置各跑一遍。当晚 Shantung 端点在劣化（6.8 崩、6.7 高延迟 7-62s TTFT），数据含噪声但方向明确。

| 配置 | E2E 结果 | 披露合规 | 单场景耗时 | 总耗时 |
|---|---|---|---|---|
| **全强 glm-5.2** | **4/4 过** | **4/4** | 91-190s | **9m53s** |
| 全快 6.7-flash-lite | 1/4 过 | 2/4 | 665-1179s | 1h13m54s（**7.5 倍**） |
| 全快 6.8（崩溃端点） | — | 0/1 | 单场景 1036s / 16 次调用 | 已失控终止 |
| 混合（Pro=glm-5.2 + Flash=6.7，规则路由） | 进行中 | 3 场景：2 True / 1 False | 960-1179s | — |

**结论（数据说话，不再纠结）**：
1. **复杂诊断任务的强模型钱不能省**：全快组质量崩（JSON 合规差、披露纪律守不住）+ 墙钟 7.5 倍——"省流"完全被重试和返工吃掉。
2. **混合路由的价值在韧性而非省钱**：常规步骤走 Flash 时 inherits 端点劣化，但规划/救场由强模型兜底，场景没有像纯快组那样彻底崩。
3. **档位映射基线确定**：planner/reviewer → Pro（质量关口绝不降级）；executor/scout → Flash 跑腿可以，但诊断类任务的 executor 建议也挂 Pro 或依赖升维信号。
4. 端点可用性是波动的（6.8 白天 926ms 在线、晚上崩）——这正是"槽位可换 + 角色挂档位"架构的现实理由。

**落地物**：角色→档位映射（role_models，支持档位别名或钉死具体 UUID）、结构化失败信号（替代 ❌ 字符串匹配）、难度打分路由（routing_mode=scored，透明加权表：契约+20/连续失败×15/观测超长+20，≥60 升维）。282 单测全绿。打分模式的实测验证待端点健康后补跑。

## 记录 5：2026-08-29 · 角色档位配置页面落地

- **AgentComposer 模型路由弹窗**新增：①路由模式切换（规则/打分）；②角色档位表——规划、执行、审查、侦察L2、写入L3 五个固定角色，每角色可选「跟随自动 / 强模型槽 / 快模型槽 / 视觉槽」，值经 role_models 透传后端，钉死优先于自动分流。
- **配置面收敛**：用户只维护 3 个槽位（换模型=换槽），角色默认跟随系统映射表，高级钉死入口保留——与 Claude Code 的档位别名模式同构。
- 打分模式（routing_mode=scored）实弹验证已排队后台执行，结果见后续记录。

---

## 附：评测运维手册（2026-08-29 白屏排查战实录，按症状查表）

### 症状 1：评测启动失败「Embedded WebDriver server did not become ready on port 4445」

**根因（当晚真凶）**：跑过普通 `cargo build`（不带 `wdio` feature）之后，`target/debug/free-api-chat`
被覆盖成无驱动插件的版本——app 能起窗口但前端全白、驱动永不注册。
**诊断命令**：`strings src-tauri/target/debug/free-api-chat | grep -c "wdio-webdriver"`（0 = 二进制坏的）。
**修复**：`npm run test:e2e:build`（带 `--features wdio`）再重试。
**铁律**：评测前必跑 test:e2e:build 并校验 strings；写完 Rust 代码后先 cargo test，评测前必须重新 test:e2e:build。

### 症状 2：app 进程活着、窗口白屏、无障碍树读不到

= app 卡死在启动早期（驱动插件注册之前）。当晚链路：二进制无 wdio feature（症状 1）→ 白屏。
**铁律**：`pkill -9 free-api-chat` 强杀后检查 `~/Library/Application Support/free-api-chat/*.db-wal/-shm`
残留（当晚 WAL 0 字节属正常）；重启前确认 `lsof -i :4445` 为空。

### 症状 3：端口 4445 被残留进程占用（连发启动的竞态）

上一轮的 app/driver 还没退干净就起新一轮 → 新 driver 绑不上端口。
**修复**：`pkill -9 -f "wdio"; pkill -9 -f free-api-chat; sleep 5; lsof -i :4445`（必须为空）再启动。
**铁律**：一轮评测结束后再启动下一轮，绝不并行两个 wdio。

### 症状 4：评测规格静默跳过 / executeAsync 脚本超时

- 规格 `describeLive = modelId ? describe : describe.skip`——**只要 FREE_API_LIVE_MODEL_ID 没设，全部场景静默 skip**；
  加分槽变量（PRO/FLASH/VISION）时门控变量仍必须传。
- `browser.executeAsync` 的函数体在浏览器上下文执行，**模块级变量不会带进去**——新增参数必须同步改
  函数签名和调用实参列表，否则 `done` 被顶掉 → 30s 脚本超时（当晚 fake-action 全军覆没的根因）。
- 判断依据：报告 mtime + 报告内 routing id 是否为本轮模型；`agent_llm_calls` 有无新增行。

### 症状 5：评测「卡住」——先分清端点慢还是真死锁

查 `agent_llm_calls` 最近几行：`ttft_ms` 飙到几十秒 = 端点劣化（当晚 6.8 崩、6.7 TTFT 7-62s），
`outcome=parse_error` 反复出现 = 小模型 JSON 合规问题在烧重试。后台评测传输错误最多 20 次有界退避，不会死锁，只会长跑。

### 监控速查（本轮验证有效）

```bash
# 运行状态与步骤数
sqlite3 "$DB" "SELECT id,status,started_at FROM agent_run_traces ORDER BY started_at DESC LIMIT 3;"
sqlite3 "$DB" "SELECT COUNT(*) FROM agent_step_logs WHERE session_id='$SESSION';"
# 逐调用延迟/解析/重试（区分端点慢 vs 死锁）
sqlite3 "$DB" "SELECT session_call_index,outcome,request_try,transport_attempt_count,total_duration_ms,ttft_ms FROM agent_llm_calls WHERE session_id='$SESSION' ORDER BY session_call_index DESC LIMIT 6;"
# 子任务（reviewer/worker/writer 按 kind）
sqlite3 "$DB" "SELECT kind,status,error_kind FROM agent_child_runs WHERE started_at>'$T';"
```
$DB = `~/Library/Application Support/free-api-chat/data_v2.db`；报告自述路由模型（routing.reasoning/execution_model_id），
跨轮核对报告归属以 mtime + routing id 双重确认。

---

# 夜战汇总：多组合循环测试矩阵（2026-08-29 深夜，8+ 轮真实评测）

> 背景：Shantung 端点整夜劣化（6.8 崩溃、6.7 高延迟波动）——所有含 Flash 档的组合都带噪声，
> 但结论方向一致且互相印证。全部报告自述路由模型，归属可靠。

## 测试矩阵

| # | 组合 | 场景 | E2E | 披露合规 | 关键数字 |
|---|---|---|---|---|---|
| 1 | 全强 glm-5.2（基线） | diag×4 | **4/4** | 4/4 | 6-9 调用/场景，91-190s，总 9m53s |
| 2 | 全快 6.7 | diag×4 | 1/4 | 2/4 | 6-12 调用，665-1179s，总 74 分钟 |
| 3 | 全快 6.8（端点崩） | diag×1 | 失控 | 0 | 16 调用/1036s 单场景 |
| 4 | 混合+规则（Pro=5.2/Flash=6.7） | diag×4 | 未及汇总 | 3/4 | plugin-removal 不合规 |
| 5 | **打分模式**（混合，未钉角色） | diag×4 | 1/4 | 1/4 | 与规则模式相当 |
| 6 | **角色钉死**（planner/reviewer→Pro）+规则 | diag×4 | **3/4** | 3/4 | 混合组最佳单轮 |
| 7 | 混合+写fixture | write×1 | 超时取消 | — | 6 步/20 分钟，4 次解析错误耗尽时限 |
| 8 | **打分+角色钉死**（含质询升维调优） | diag×4 | **4/4 全过** | **4/4** | plugin-removal 三连过（11/20/21 调用），总 63m36s |
| 9 | 打分+钉死+长程 24 阶段（35 分钟预算） | 长程×1 | 第 47 步失败 | — | 并行分支 4>2 越界警告 + 尾段解析错误 |

## 核心调优结论

1. **「打分 + 角色钉死」是当前最优混合配置**（#8）：追平全强模型 4/4 的质量，同时执行步骤享受快模型成本。
2. **质询升维调优生效**：finish 被质询视为交付失败 → 重写步升维。plugin-removal 此前在所有含 Flash 组里 100% 失败，调优后三连合规通过。
3. **长程混合运行的新调优线索（#9）**：a) 并行分支越界（4>2）需要宿主在 spawn 层硬拒绝而非仅警告；b) 长程尾段（第 45+ 步）建议自动升维——收官阶段的精度价值最高；c) 24 阶段 fixture 在混合档位下建议 35+ 分钟预算。
4. **劣化端点下混合路由会吹掉固定时限**（#7）：时间预算应与端点健康度联动，或直接提高长任务预算。
5. 角色钉死本身（#6）提升有限但稳定；与打分模式叠加后才突破——**打分负责"何时升维"，钉死负责"谁固定守关口"**，两者正交。

## 运维新增教训

- 普通构建覆盖评测二进制的坑一夜复发了两次——已写入手册（症状 1）；永久解法是给评测构建配独立 CARGO_TARGET_DIR（待办）。
- 评测规格的 executeAsync 闭包变量必须全部走参数传递（三份规格统一重构完成）。
- 评测时长已参数化：FREE_API_LIVE_MAX_DURATION_MINUTES（长程场景建议 35+）。

---

# 同源对照实验：glm-5.3-flash 同模型，free_api agent vs ZCode（2026-08-30）

> 实验设计：同一官方 API 模型（glm-5.3-flash @ 智谱BigModel，重试 20），同一批字节级一致的 fixture
>（4 个诊断 + 1 个写修复），两个 agent 躯壳各自独立作业，同一套隐藏断言评分。
> 本轮修复：FREE_API_DB_PATH 隔离（评测实例独立 DB，根治与日常实例的锁争抢）。

## 结果矩阵

| 场景 | free_api agent（scored+钉死） | ZCode（同模型） |
|---|---|---|
| fake_action | ✅ 合规，5 调用/224s | ✅ 过隐藏断言 |
| plugin_removal | ✅ **passes_observed_guardrails**，5 调用/230s | ✅ |
| false_leak | ✅ 合规（评分器修正后） | ✅ |
| name_collision | ✅ **passes_observed_guardrails**，6 调用/159s | ✅ |
| write_fixture | ✅ E2E PASS，9 调用/367s | ✅ 可见测试+隐藏契约 PASS |
| **合计** | **5/5 任务正确**，diag 总 16m44s | **5/5 任务正确** |

过程指标（diag）：0 解析错误、5-10 调用/场景、159-230s/场景、2 个工具失败（均为宿主越界拒绝，行为正确）。

## 三个关键发现

1. **此前"混合档位劣势"主要是端点质量，不是架构。** 同一套 scored+钉死配置，换到健康的官方 API
   （glm-5.3-flash）后：调用数从 12-21 降到 5-10，单场景时长从 665-1179s 降到 159-230s，解析错误归零，
   合规 4/4。架构本身经得起健康端点的检验。
2. **评分器自身也需要被测。** false_leak 场景你的 agent 写出了完全正确的诊断（并显式排除了泄漏假设），
   却因否定词"不成立"不在断言的否定词表里而被误判 FAIL。已修（补不成立/不存在/并无/并未）。
   教训：**隐藏断言的关键词覆盖面是评估基础设施的一部分，会制造假阴性。**
3. **FREE_API_DB_PATH 隔离根治了评测启动挂死。** 之前"driver 起不来/白屏"反复出现的深层原因之一：
   评测实例与日常使用实例共抢同一 SQLite。现在评测用独立 DB 副本，与日常使用物理隔离。

## 两臂差异的本质（同名模型下的 harness 差距）

- **正确性打平**：同一模型在两套 harness 下都能 5/5——模型能力是天花板，两套 harness 都够到了。
- **过程差异是哲学差异**：free_api 是无人值守流水线（固定编排开销换审计性与可复现），ZCode 是交互式协作
  （人即调度器，省掉固定开销）。墙钟差（diag 16 分钟 vs 交互式几分钟）主要是流水线的结构成本，不是缺陷。
- **可提升点**（保持无人值守优势的前提下逼近交互式效率）：
  a) 端点健康探测与快慢槽自动切换（昨晚 6.8 崩溃拖垮全链的教训）；
  b) 评分器否定词表这类评估基础设施的自测（避免假阴性误导调优方向）；
  c) 并行分支 spawn 层硬拒绝（仍在待办）。

---

# 24 阶段长程双臂对照（第二轮）：free_api agent vs ZCode（2026-08-30 午）

> 实验设计：与首次同源对照同思路，但升级为 **24 阶段长程任务双臂对照**。
> 同一份字节级一致的 progression fixture（stages 数组程序化提取自
> `test/e2e/live-agent-long-convergence.e2e.mjs`，生成器 `test-artifacts/compare-zcode/gen-long-fixture.mjs`），
> 同源模型（GLM-5.3-Flash），各自独立作业，同一套隐藏断言评分
> （`test-artifacts/compare-zcode/hidden-assert.mjs`：受保护文件完整性 + 24 transform 全量重放 + 输入不可变）。

## 结果矩阵

| 维度 | free_api agent（scored+钉死） | ZCode（交互式） |
|---|---|---|
| 隐藏断言 | **24/24 PASS** | **24/24 PASS** |
| 受保护文件 | 零改写 ✓ | 零改写 ✓ |
| 实质完成 24 阶段用时 | **13m38s**（03:11:23→03:25:01） | **6m56s**（11:19:20→11:26:16） |
| LLM 调用 / 轮次 | 79 次 | ~54 轮 |
| 工具动作 | 78 步（读 28 + 写 23 + 命令 26 + 并行批 1） | ~77（24 读 + 24 编辑 + 26 bash，3 次浪费） |
| 工具失败 / 解析错误 | 0 / 1（98.7% 解析成功） | 0 / 0 |
| 重复读取 | **0 次**（28 读全唯一，拦截器零触发=威慑充分） | 0（预取并行读） |
| 最大并行分支 | 2（合规） | —（交互式无编排） |
| 缓存命中 | 88.7%（重复结构对 KV cache 友好） | —（宿主侧未计） |
| trace 状态 | cancelled（见下：20 分钟上限 bug） | —（不经 trace） |

## 本轮两个工程发现

1. **评测时长上限 20 分钟硬夹（已修，bdf3167）**：`MAX_EVALUATION_DURATION_MINUTES=20`
   把协议传入的 60 分钟夹掉；agent 完工后自查阶段再花 5.5 分钟即在 20:00 整点被
   `supervise_worker → cancel_agent_run` 砍掉，trace 完成语是通用的「用户请求停止当前任务」，
   极易误判为真人停止（本次排查即被误导）。实质：24 阶段已全部实现并通过官方 npm test
   （事后对 fixture 复跑 exit=0 + 隐藏断言 24/24）。**教训：guardrail 的取消文案必须能区分
   「系统时限」与「真人请求」。**
2. **并行分支 spawn 层硬拒绝（已修，c5d15ff）**：plan_tree 复合树指令此前绕过
   `validate_controlled_parallel_batch`，executor 对并行组扇出无上限（4>2 越界的真实入口）。
   现双层设防：`run_plan_tree_sync` 执行前整树递归校验（超限/空组零执行）+ executor spawn 点
   不变量兜底（超限直接 Err、零 spawn）。本轮 trace 中 max_parallel_branches=2，合规。

## 两臂差异的定性结论（与首轮对照一致并加粗）

- **正确性等价**：同一模型两套 harness 都 24/24——模型能力仍是天花板。
- **效率差是结构性的**：free_api 每阶段 3 调用（read_multiple_files 取 CURRENT_STAGE+目标文件 →
  write_file → npm test），ZCode 每阶段 2 轮（Edit ∥ 预取下一文件 → npm test，解锁消息直接内含
  下一义务无需重读）。墙钟 2 倍差 ≈ 固定编排开销 + 逐阶段重读，非模型缺陷。
- **验证税**：free_api 完工后自发生成边界自查套件多跑 5.5 分钟（期间发现自己自查脚本的
  预期值笔误并修正，非产品码问题）。无人值守审计性换来该开销——是哲学选择不是浪费；
  ZCode 交互式无此税，但也无审计轨迹。
- **重复读取拦截生效**：上轮 16 连读的循环本轮 0 复现（28 读全唯一），硬拦截未被迫触发。

## 待办

- [ ] 时限修复后重跑一轮拿正式 `succeeded` trace（本轮被并行会话的在途编辑阻塞编译，见下）
- [ ] 评测窗口防误触/最小化选项（wdio 起的 webkit 是用户桌面真实窗口）

## 第二轮重跑补充（2026-08-30 13:05）

- 60 分钟上限修复生效：第一轮 19.4 分钟本会被砍，第二轮 28.5 分钟才到第 24 阶段——旧上限下根本到不了终点。
- 第二轮实质工作再次全部完成（12:35 验证 fixture `npm test` exit=0），但 trace 仍非 succeeded：
  **spec 轮询期限（3600s）与运行上限（3600s）相等**，12:57:23 轮询先到期 → wdio 断言失败并拆实例 →
  watchdog 的取消（12:59）+ 报告落盘序列被打断，最终报告未写出（仅 monitor 快照 `5c3c0238`）。
  已修：spec 侧轮询期限强制 = max(传入值, 运行上限 + 5 分钟缓冲)。
- **第三个可靠性发现：单次 LLM 调用无总预算**。step 74（40ms 的 npm test）之后，
  下一次模型请求在端点劣化下重试挂起 21+ 分钟（12:26→12:59 被 watchdog 掐断）。
  重试 20 次的策略没有 per-call 死线，单点挂起能吃光整个运行预算——这正是交接单
  「端点健康探测与快慢槽自动切换」要解的问题，建议加 per-call 总时限（如 3 分钟）+ 端点熔断降级。
- 当日 glm-5.3-flash 官方端点健康度：首轮 60-80s/调用，二轮 60-100s/调用且出现 61s 解析错误
  与 21 分钟挂起（对比昨日 diag 159-230s/场景）——对照实验结论受端点健康度调制，跨日对比需谨慎。

## 行为调优（2026-08-30 下午，a657e7f）

- **并行扇出分级**：纯只读批次（read/search/list 等无副作用工具）上限 2→4；含写入/执行或未知工具维持 2。
  三层校验（单批 / 整树预校验 / executor spawn 点）统一按批分级，分析器按同口径报警（parallel_cap_exceeded_count），
  core_logic.md 提示词同步。分类保守：未知工具一律按可变处理。
- **LLM 单次调用总预算**：240s（FREE_API_LLM_CALL_BUDGET_SECONDS 可调），覆盖全部重试与退避。
  直接封堵"单请求挂起 21 分钟吃光运行预算"；ask_human 等人不走 LLM 调用，天然不受限。
- 323 单测全绿。

## 槽位熔断 + 行为规则落地（2026-08-30 傍晚，c66292e）

- **llm_breaker**：平台×模型槽位级健康熔断。连续 3 次失败 → 快速失败（冷却 60s 起，
  半开探测失败翻倍封顶 10min）；冷却期满放行真实调用作半开探测（成功恢复清账，失败翻倍）。
  恢复检测零成本：用真实工作当探针，不发合成 ping。用户中断不计失败。`FREE_API_LLM_BREAKER=0` 可关。
- **core_logic.md §8 新增**：①分阶段任务循环节奏——解锁消息含下一义务时用 `commands` 批合并读取，
  禁止重读已知信息（3 轮/阶段 → 2 轮/阶段的落地载体）；②补充自查校准——检查器先自检、
  自查最多 3 步、官方关卡与自查冲突时信官方。
- 行为规则属概率性引导，效果需跑 eval 验证：llm_call_count 79 → ~55 即生效；
  熔断可用"指向死端点"的故障注入验证开闸/半开恢复/trace 记录。
- 327 单测全绿（+4 熔断状态机）。

## 第三轮长程：正式 succeeded ✅（2026-08-30 15:22，c72e7f8 修复后）

- **evaluation_id**: `agent-write-fixture-afffeec4`（run `7502c431`）· **trace_status: succeeded** · **wdio 1 passing (18m57s)**
- 全修复首次联合生效：60min 上限、spec 轮询缓冲、调用预算 240s、熔断（未触发——端点健康）、分级并行（合规 ≤2）
- 指标：83 调用（含规范 `finish` 交付，三次长跑首次正经收尾）/ 81 工具步 / 0 重复读取（第三次连续 0）/
  缓存命中 91.1%（↑2.4pp）/ 观测 18 分钟（远低于 60min 上限）
- **过程亮点**：阶段 21 首次实现错误（返回字符串而非数组），agent 重读源码定位、修复、通过——
  真实错误下的自恢复路径被完整走通。调用 82 曾漏写 `tool` 字段被宿主指令校验拦截，下调用自行修正。
- **诚实记录**：循环节奏规则（commands 批合并读取）**未改变调用结构**——仍是每阶段 读→写→测 3 调用，
  llm_call_count 83 ≈ 79（含 finish 差异为噪声）。提示词级行为引导如预期是概率性的；
  若要确定生效，需在 loop_runner 层做结构化支持（受"宿主不改写"原则约束，需权衡）。
- spec 侧 TDZ 引用错误（c72e7f8）：evaluationTimeoutMs 在 maxDurationMinutes 声明前使用，
  spec 加载即崩，第三轮首跑 1 秒失败暴露——教训：改 spec 后必须 `node --check` 再提交。
- ZCode 臂 v3 同 fixture 对照：24/24 隐藏断言 PASS、受保护文件零改写、实质 8m46s（51 轮、零浪费调用）。

## 第四轮（示例族提示词）：行为泛化验证 ✅（2026-08-30 17:00，db3684f）

- **4c: trace succeeded，wdio 1 passing (34m29s)**（evaluation `321731dc` / run `5ed10365`）；4 与 4b 因端点劣化
  快速失败（16:09 / 16:23 两个劣化窗口，240s 预算耗尽 → 按设计终止，未挂起一分钟）。
- **行为泛化超出预期**：第一次测试即采用单命令串联 `npm test 2>&1; cat CURRENT_STAGE.md`；
  自行发现并使用 hub 原生 `run_sequence`（首次带前缀报错后，下一步自诊断改为无前缀 + read→edit→test 顺序）；
  edit_file 替代 write_file（diff 式编辑）；新增 directory_tree/git_diff 探索；4b 中用过 parallel:3（纯只读，分级上限内合法）。
- **诚实记录：调用数未降（89 vs 83）**。每阶段仍 3 调用，但结构变了：工单读取已合并进测试命令（省 1 次），
  却把原先 read_multiple_files 批量读拆成 24 次独立 read_text_file（多花 1 次）——左手省右手花。
  结论：**提示词示例改变行为形状，不保证净成本下降**；净收益需要结构化支持
  （then_read 字段或工具层合并交付），见后续计划。
- **韧性大考通过**：6 次解析错误（端点全天不稳）+ 34 分钟长跑，预算+重试扛住全程，零挂起，规范 finish 收尾。
- 隐藏断言 24/24（wdio passing 内含）；受保护文件零改写。

## MCP Hub 侧修复（2026-08-30 傍晚，hub 仓库 48142da）

- 定位到 run_sequence 首用失败的根因（模型自诊断"带 remote-http-mcp/ 前缀导致查表失败"）：
  hub 的 run_sequence 按子服务裸名匹配（tools/list t.name === stepTool），而 agent 侧工具目录
  全是"插件名/工具名"全名——命名空间边界bug，模型首次组合调用必然踩坑。
- 修复：steps.tool 按最后一个 / 截断取裸名后再匹配，全名/裸名都收；
  not-found 报错附带 requested as 原名；schema 描述同步；+1 测试（normalizeStepToolName 9 断言）。
- 已提交 hub 仓库（48142da）并重启上线（本地 8000，MCP 握手验证通过）。
- 顺带确认：shell_exec 走 shell 解释（串联命令可用）；edit_file 为 diff 式（oldText/newText，
  需先知内容——这就是每阶段多出的那次读取的来源）。
- 待做（提示词层）：write_file 免读指南 + "cat 输出可作 oldText 基线"，落地 3→2 调用/阶段。

## 第五轮 + hub 第二修复（2026-08-30 晚，hub 仓库 1895eae）

- **效率突破确认（第五轮前 10 阶段实测）**：write_file/cat 复用规则 + 串联示例后，
  bootstrap 从 3-4 次读取压缩为 1 条命令（cat README+package.json+工单 && find tree），
  阶段验证一条命令全包（cat 目标文件 + cat 检查器 + npm test）。28 调用/10.5 阶段 ≈ 2.7 次/阶段
  （此前 3.3-3.7），read_intent_count=0（读取全并入 shell）。**3→2 调用/阶段落地。**
- 第五轮本身 failed：新形态故障——模型在超长输出（spilled_to_disk）后把 command 双重编码成
  JSON 字符串，hub 原样透传，子服务 Zod 报天书错误 "expected record, received string"，
  模型看不懂连犯 17 次同型失败直到任务终止（观察 4 次相同失败重犯告警生效）。
- **hub 第二修复（1895eae）**：tools/call 入口对 string 型 arguments 自动 JSON.parse 还原
  （审计记 args_coerce），不可解析时返回指明"双重编码"的可执行报错。7/7 测试过，已重启上线。
- 待验证：下轮 eval 确认 ①净调用数稳定在 ~65-70 ②双重编码出现时模型凭新报错一次自愈。

## 第五轮 b/c：效率突破坐实 + host 第三修复（2026-08-30 晚，883c293）

- **5b（0f998bf2）：trace succeeded，wdio 1 passing (35m32s)，llm_call_count 62（79/89 → 62，-22%）**，
  read_intent 28→2（读取层几乎全并入 shell 串联），write_file 31 次（免读规则生效），
  parallel:2 + parallel:3 混用，0 解析错误。隐藏断言 24/24 独立复核 PASS。
- **5c（dd8975cf）：llm_call_count 60（新低）**，24/24 隐藏断言 PASS，最后一次 npm test exit 0
  ——但发 finish 时端点再次突发故障，预算耗尽任务终止。**实质五轮全部完成，失败只剩端点打点。**
- **host 第三修复（883c293）**：commands 批子步骤原样透传导致顶层 tool 路由键被 hub 参数契约拒绝
  （9 次失败全源于此且模型全部自愈）——现在只透传 command 载荷。
- 调用数演进：79（r3）→ 89（r4c）→ **62（r5b）→ 60（r5c）**；缓存命中随命令多样性下降
  （92% → 67-88%），属于把重复结构换成合并动作的预期交换。
- **唯一遗留缺口**：硬 LLM 失败（端点突发故障）= 任务立即失败。五轮实质全部完成，
  失败全是"端点故障打在尾部"。下一步：熔断冷却期后步骤级重试（任务预算内等待槽位半开再试一次），
  而非直接终止。

## 不同任务形态复测：诊断类双臂对照（2026-08-30 晚，改动后最终确认）

- 场景：4 个源码级诊断 fixture（fake_action / plugin_removal / false_leak / name_collision），
  与 24 阶段修复完全不同的任务形态（根因分析、假设排除、结构化交付），验证今日全部改动无回归。
- **free_api：wdio 3/4 场景通过**。唯一失败 plugin_removal 是**交付格式断言**
  （finish 摘要缺"已核验/未核验/结论强度"披露），其 DIAGNOSIS.json 隐藏断言独立补验 PASS，
  且比 ZCode 臂多识别一个细节（catalog::CACHE thread_local 只清删除线程）。
- **ZCode 臂：4/4 PASS**（grade-diag.mjs 独立评分，_v2 全新 fixture）。
- 对照结论：诊断正确性两臂等效（free_api 甚至更细），失败源于 finish 交付披露结构的轮次方差
  （今早同场景曾通过）——属第 8 节交付格式纪律的执行稳定性问题，非能力缺失。
- 今晨基线（改动前）同套件 5/5；今日改动后 3/4+1 格式失败：无回归证据，方差一例。

## 第六轮启动：端点韧性重试上线（2026-08-30 晚，b652216）

- stream_with_resilient_retry：硬失败后等待 max(熔断冷却,60s)（封顶 180s）重试当前逻辑调用 ×2，
  等待可被用户取消打断；熔断开启时自动等到半开窗口。
- 从此任务死亡策略改为：三次恢复窗口（约 14 分钟）都试过仍失败才判死，且全程受任务看门狗约束。
- 本轮验证目标：①正常路径无回归（调用数维持 ~60 级别）②若端点再抽风，观察韧性窗口是否救回。

## TinyFish 免费搜索接入（2026-08-30 深夜）

- TinyFish 官方 onboarding 已跑完：Codex + Cursor 连接验证通过（无需浏览器登录）；
  Claude Code/OpenClaw/OpenCode 因 CLI 不在 PATH 跳过；TINYFISH_API_KEY 已写入 shell profile。
- **free_api 原生接入**：web_search.rs 回退链新增 search_tinyfish（X-API-Key + GET api.search.tinyfish.ai），
  置顶优先于 Bing 免费抓取；实况测试通过（HN 查询返回 TinyFish 来源）。
- **部署注意**：app 进程需能读到 TINYFISH_API_KEY 环境变量（shell 启动即可；GUI 启动需后续把 key
  收进应用设置存储——待办）。
- API 契约（从官方 SDK 挖出）：Search=GET api.search.tinyfish.ai/?query=；Fetch=POST api.fetch.tinyfish.ai/
  body {urls:[...]}, 认证统一 X-API-Key 头。
- 密钥策略（用户决策）：DEFAULT_TINYFISH_API_KEY 内置随仓库分发（私有仓库），env 可覆盖；
  零配置开箱即用，无环境变量实况验证通过。
